//! Object table. Every heap edge is listed by [`Object::write_edges`] plus
//! combine-state edges appended by [`Heap::write_edges`]. Snapshot capture and
//! restore consume that combined list instead of re-matching object kinds.
//!
//! Plain objects store own data or accessor properties. Only a missing key
//! walks the prototype chain; environments are not script objects.
//! Array iterators share plain-object properties but keep their cursor internal.
//! Bound functions have no own properties or internal prototype.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::VmError;
use crate::ops::array_index;
use crate::value::{
    BindingId, ContinuationId, NativeKey, ObjectId, PropertyKey, PropertyKeyRef, SymbolId, Value,
};

/// Reserved `Symbol.iterator`. Ordinary symbols allocate from 1 and are never
/// interned by description; only the registry interns by key.
pub(crate) const ITERATOR_SYMBOL: SymbolId = 0;
pub(crate) const ITERATOR_DESCRIPTION: &str = "Symbol.iterator";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromiseState {
    Pending,
    Fulfilled(Value),
    Rejected(Value),
}

/// One waiter on a promise. `Await` is the original continuation reaction.
/// The other variants are script handlers, adoption, and combinators.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReactionJob {
    Await(ContinuationId),
    /// `then` / `catch`. A missing handler passes the settlement through.
    Script {
        result: ObjectId,
        on_fulfilled: Option<Value>,
        on_rejected: Option<Value>,
    },
    /// `finally`. The saved settlement is captured when the source settles,
    /// not stored on the pending job.
    Finalize {
        result: ObjectId,
        on_finally: Value,
    },
    /// Copy `source`'s settlement onto `result` (thenable promise adoption,
    /// including a `finally` tail). `result.adopting` points back here.
    Adopt {
        result: ObjectId,
    },
    Combine {
        result: ObjectId,
        index: usize,
        kind: CombineKind,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CombineKind {
    All,
    AllSettled,
    Race,
    Any,
}

/// In-progress `Promise.all` / `allSettled` / `race` / `any`. Keyed by the
/// result promise. `elements` keeps each slot promise reachable from that
/// result so a snapshot does not drop a still-pending input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CombineState {
    pub kind: CombineKind,
    pub count: usize,
    pub remaining: usize,
    pub results: Vec<Value>,
    pub states: Vec<PromiseState>,
    pub first: Option<Value>,
    pub elements: Vec<ObjectId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromiseObject {
    pub state: PromiseState,
    pub reactions: Vec<ReactionJob>,
    /// Resolution has started (adoption or a fulfilled value) and must not
    /// start again. Distinct from [`PromiseState::Pending`]: a locked promise
    /// can still be pending while it waits on another promise.
    pub locked: bool,
    /// Promise whose settlement this one is adopting, when `locked`.
    pub adopting: Option<ObjectId>,
    /// When set, an `Adopt` fulfillment reapplies this settlement instead of
    /// the source value. `finally` uses it so a returned thenable only delays
    /// the original settlement, unless that thenable rejects.
    /// Original settlement reapplied after a `finally` thenable. Distinct from
    /// the value currently being adopted.
    pub finally_settlement: Option<PromiseState>,
    pub props: BTreeMap<String, Value>,
    /// Symbol-keyed own properties, kept beside the string table.
    pub symbol_props: BTreeMap<SymbolId, Value>,
    /// Internal prototype. Promise instances share one identity.
    pub prototype: Option<ObjectId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NativeObject {
    pub key: NativeKey,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Property {
    Data(Value),
    Accessor {
        get: Option<ObjectId>,
        set: Option<ObjectId>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlainObject {
    pub props: BTreeMap<String, Property>,
    pub symbol_props: BTreeMap<SymbolId, Property>,
    /// Internal prototype. Literals use `None`.
    pub prototype: Option<ObjectId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArrayObject {
    pub elements: Vec<Value>,
    /// Own properties other than `length` and canonical indices.
    pub props: BTreeMap<String, Value>,
    pub symbol_props: BTreeMap<SymbolId, Value>,
    pub prototype: Option<ObjectId>,
}

impl ArrayObject {
    /// `Some` is an own hit, including an explicit `undefined`.
    fn own(&self, key: PropertyKeyRef<'_>) -> Option<Value> {
        match key {
            PropertyKeyRef::String("length") => Some(Value::Number(self.elements.len() as f64)),
            PropertyKeyRef::String(key) => self.own_value(key).cloned(),
            PropertyKeyRef::Symbol(id) => self.symbol_props.get(&id).cloned(),
        }
    }

    fn own_value(&self, key: &str) -> Option<&Value> {
        if let Some(index) = array_index(key) {
            return self.elements.get(index);
        }
        self.props.get(key)
    }

    fn set(&mut self, key: &str, value: Value) -> Result<(), VmError> {
        if key == "length" {
            let Value::Number(length) = value else {
                return Err(VmError::InvalidState(
                    "array length must be a number".to_owned(),
                ));
            };
            if !length.is_finite() || length < 0.0 || length.fract() != 0.0 {
                return Err(VmError::InvalidState(
                    "array length must be a non-negative integer".to_owned(),
                ));
            }
            self.elements.resize(length as usize, Value::Undefined);
            return Ok(());
        }
        if let Some(index) = array_index(key) {
            if self.elements.len() <= index {
                self.elements.resize(index + 1, Value::Undefined);
            }
            self.elements[index] = value;
            return Ok(());
        }
        self.props.insert(key.to_owned(), value);
        Ok(())
    }
}

/// A function value. `env` is the lexical environment captured at creation;
/// top-level cached functions use `None`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionObject {
    pub func_id: u32,
    pub env: Option<ObjectId>,
    pub props: BTreeMap<String, Value>,
    pub symbol_props: BTreeMap<SymbolId, Value>,
    /// Internal prototype. Not the constructable `.prototype` property.
    pub prototype: Option<ObjectId>,
}

/// One invocation environment. Bindings are addressed by compile-time IDs, so
/// shadowed names and closures share mutable cells without name lookup.
/// Environments are not script objects and cannot be prototypes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentObject {
    pub parent: Option<ObjectId>,
    pub bindings: BTreeMap<BindingId, Value>,
}

/// `Promise`, `queueMicrotask`, a static such as `Promise.resolve`, or one
/// shared instance method such as `then`. The receiver is supplied by the call,
/// not stored on the object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuiltinStatic {
    pub name: String,
    pub member: Option<String>,
    pub props: BTreeMap<String, Value>,
    pub symbol_props: BTreeMap<SymbolId, Value>,
    pub prototype: Option<ObjectId>,
}

/// One-shot `resolve` or `reject` passed to an executor or a thenable `then`.
///
/// The pair shares the once-flag: calling either marks both `called`. An
/// uncalled capability that no root can reach cannot be snapshotted; the VM
/// rejects that capture instead of dropping the in-flight assimilation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Resolution {
    pub promise: ObjectId,
    pub pair: ObjectId,
    pub resolve: bool,
    pub called: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoundObject {
    pub target: ObjectId,
    pub bound_this: Value,
    pub args: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Object {
    Promise(PromiseObject),
    Native(NativeObject),
    Plain(PlainObject),
    Array(ArrayObject),
    ArrayIterator {
        object: PlainObject,
        array: Option<ObjectId>,
        next_index: usize,
    },
    Function(FunctionObject),
    Bound(BoundObject),
    Environment(EnvironmentObject),
    BuiltinStatic(BuiltinStatic),
    Resolution(Resolution),
}

/// Why an object id must be an environment. The role selects the restore error.
#[derive(Clone, Copy)]
pub(crate) enum EnvRole {
    Closure,
    Parent,
}

/// Owned edge so a walker can drop the source object and then query the table.
#[derive(Clone, Copy)]
pub(crate) enum OwnedEdge {
    Object(ObjectId),
    Prototype(ObjectId),
    Environment { id: ObjectId, role: EnvRole },
    Continuation(ContinuationId),
    Symbol(SymbolId),
}

impl Object {
    pub fn as_promise(&self) -> Option<&PromiseObject> {
        match self {
            Self::Promise(promise) => Some(promise),
            _ => None,
        }
    }

    pub fn as_promise_mut(&mut self) -> Option<&mut PromiseObject> {
        match self {
            Self::Promise(promise) => Some(promise),
            _ => None,
        }
    }

    pub fn as_function(&self) -> Option<&FunctionObject> {
        match self {
            Self::Function(function) => Some(function),
            _ => None,
        }
    }

    pub fn as_environment(&self) -> Option<&EnvironmentObject> {
        match self {
            Self::Environment(environment) => Some(environment),
            _ => None,
        }
    }

    pub fn as_environment_mut(&mut self) -> Option<&mut EnvironmentObject> {
        match self {
            Self::Environment(environment) => Some(environment),
            _ => None,
        }
    }

    /// Append every outgoing reference. Callers clear `out` only by relying on
    /// this method, which replaces the buffer contents.
    pub(crate) fn write_edges(&self, out: &mut Vec<OwnedEdge>) {
        out.clear();
        match self {
            Self::Promise(promise) => {
                if let PromiseState::Fulfilled(value) | PromiseState::Rejected(value) =
                    &promise.state
                {
                    push_value(out, value);
                }
                if let Some(id) = promise.adopting {
                    out.push(OwnedEdge::Object(id));
                }
                if let Some(state) = &promise.finally_settlement {
                    push_state(out, state);
                }
                for job in &promise.reactions {
                    job.write_edges(out);
                }
                push_script(out, &promise.props, promise.prototype);
                push_symbol_values(out, &promise.symbol_props);
            }
            Self::BuiltinStatic(builtin) => {
                push_script(out, &builtin.props, builtin.prototype);
                push_symbol_values(out, &builtin.symbol_props);
            }
            Self::Resolution(resolution) => {
                out.push(OwnedEdge::Object(resolution.promise));
                if resolution.pair != 0 {
                    out.push(OwnedEdge::Object(resolution.pair));
                }
            }
            Self::Native(_) => {}
            Self::Plain(plain) => push_plain(out, plain),
            Self::ArrayIterator { object, array, .. } => {
                push_plain(out, object);
                if let Some(id) = array {
                    out.push(OwnedEdge::Object(*id));
                }
            }
            Self::Array(array) => {
                for value in &array.elements {
                    push_value(out, value);
                }
                push_script(out, &array.props, array.prototype);
                push_symbol_values(out, &array.symbol_props);
            }
            Self::Function(function) => {
                if let Some(id) = function.env {
                    out.push(OwnedEdge::Environment {
                        id,
                        role: EnvRole::Closure,
                    });
                }
                push_script(out, &function.props, function.prototype);
                push_symbol_values(out, &function.symbol_props);
            }
            Self::Bound(bound) => {
                out.push(OwnedEdge::Object(bound.target));
                push_value(out, &bound.bound_this);
                for arg in &bound.args {
                    push_value(out, arg);
                }
            }
            Self::Environment(environment) => {
                if let Some(id) = environment.parent {
                    out.push(OwnedEdge::Environment {
                        id,
                        role: EnvRole::Parent,
                    });
                }
                for value in environment.bindings.values() {
                    push_value(out, value);
                }
            }
        }
    }
}

fn push_plain(out: &mut Vec<OwnedEdge>, plain: &PlainObject) {
    if let Some(id) = plain.prototype {
        out.push(OwnedEdge::Prototype(id));
    }
    for property in plain.props.values() {
        push_property(out, property);
    }
    push_symbol_properties(out, &plain.symbol_props);
}

fn push_script(
    out: &mut Vec<OwnedEdge>,
    props: &BTreeMap<String, Value>,
    prototype: Option<ObjectId>,
) {
    if let Some(id) = prototype {
        out.push(OwnedEdge::Prototype(id));
    }
    for value in props.values() {
        push_value(out, value);
    }
}

fn push_symbol_values(out: &mut Vec<OwnedEdge>, table: &BTreeMap<SymbolId, Value>) {
    for (key, value) in table {
        out.push(OwnedEdge::Symbol(*key));
        push_value(out, value);
    }
}

fn push_symbol_properties(out: &mut Vec<OwnedEdge>, table: &BTreeMap<SymbolId, Property>) {
    for (key, property) in table {
        out.push(OwnedEdge::Symbol(*key));
        push_property(out, property);
    }
}

fn push_property(out: &mut Vec<OwnedEdge>, property: &Property) {
    match property {
        Property::Data(value) => push_value(out, value),
        Property::Accessor { get, set } => {
            out.extend([*get, *set].into_iter().flatten().map(OwnedEdge::Object));
        }
    }
}

fn push_value(out: &mut Vec<OwnedEdge>, value: &Value) {
    match value {
        Value::Object(id) => out.push(OwnedEdge::Object(*id)),
        Value::Symbol(id) => out.push(OwnedEdge::Symbol(*id)),
        _ => {}
    }
}

fn push_state(out: &mut Vec<OwnedEdge>, state: &PromiseState) {
    if let PromiseState::Fulfilled(value) | PromiseState::Rejected(value) = state {
        push_value(out, value);
    }
}

impl ReactionJob {
    pub(crate) fn write_edges(&self, out: &mut Vec<OwnedEdge>) {
        match self {
            Self::Await(id) => out.push(OwnedEdge::Continuation(*id)),
            Self::Script {
                result,
                on_fulfilled,
                on_rejected,
            } => {
                out.push(OwnedEdge::Object(*result));
                if let Some(value) = on_fulfilled {
                    push_value(out, value);
                }
                if let Some(value) = on_rejected {
                    push_value(out, value);
                }
            }
            Self::Finalize { result, on_finally } => {
                out.push(OwnedEdge::Object(*result));
                push_value(out, on_finally);
            }
            Self::Adopt { result } | Self::Combine { result, .. } => {
                out.push(OwnedEdge::Object(*result));
            }
        }
    }

    pub(crate) fn await_id(&self) -> Option<ContinuationId> {
        match self {
            Self::Await(id) => Some(*id),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Heap {
    objects: BTreeMap<ObjectId, Object>,
    next_id: ObjectId,
    /// Symbol descriptions. Missing entry and absent description are distinct.
    pub(crate) symbols: BTreeMap<SymbolId, Option<String>>,
    /// `Symbol.for` registry. Keys are registry strings; ordinary symbols are
    /// never interned by description.
    pub(crate) symbol_registry: BTreeMap<String, SymbolId>,
    next_symbol_id: SymbolId,
    pub(crate) combines: BTreeMap<ObjectId, CombineState>,
    /// Uncalled [`Resolution`] objects. Capture rejects any id in this set that
    /// the tracer did not reach: that capability is only held by an in-flight
    /// `then` call, which is not a snapshot root.
    pub(crate) resolutions: BTreeSet<ObjectId>,
    pub(crate) object_prototype: Option<ObjectId>,
    pub(crate) array_prototype: Option<ObjectId>,
    /// Shared internal prototype of every Promise allocated by this heap.
    pub(crate) promise_prototype: Option<ObjectId>,
}

impl Heap {
    pub(crate) fn new() -> Self {
        Self {
            objects: BTreeMap::new(),
            next_id: 1,
            symbols: BTreeMap::from([(ITERATOR_SYMBOL, Some(ITERATOR_DESCRIPTION.to_owned()))]),
            symbol_registry: BTreeMap::new(),
            next_symbol_id: 1,
            combines: BTreeMap::new(),
            resolutions: BTreeSet::new(),
            object_prototype: None,
            array_prototype: None,
            promise_prototype: None,
        }
    }

    pub(crate) fn alloc(&mut self, object: Object) -> ObjectId {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("object id space exhausted");
        self.objects.insert(id, object);
        id
    }

    /// A fresh `Symbol(desc)`. `None` and an empty string stay distinct.
    pub(crate) fn alloc_symbol(
        &mut self,
        description: Option<String>,
    ) -> Result<SymbolId, VmError> {
        let id = self.next_symbol_id;
        self.next_symbol_id = self
            .next_symbol_id
            .checked_add(1)
            .ok_or_else(|| VmError::InvalidState("symbol id space exhausted".to_owned()))?;
        self.symbols.insert(id, description);
        Ok(id)
    }

    /// `Symbol.for(key)`: interns by registry key only.
    pub(crate) fn intern_symbol(&mut self, key: String) -> Result<SymbolId, VmError> {
        if let Some(id) = self.symbol_registry.get(&key) {
            return Ok(*id);
        }
        let id = self.alloc_symbol(Some(key.clone()))?;
        self.symbol_registry.insert(key, id);
        Ok(id)
    }

    pub(crate) fn next_symbol_id(&self) -> SymbolId {
        self.next_symbol_id
    }

    /// Binds a validated snapshot's symbol tables. Callers must reject the
    /// snapshot first when any entry is inconsistent.
    pub(crate) fn restore_symbols(
        &mut self,
        symbols: BTreeMap<SymbolId, Option<String>>,
        registry: BTreeMap<String, SymbolId>,
        next_symbol_id: SymbolId,
    ) {
        self.symbols = symbols;
        self.symbol_registry = registry;
        self.next_symbol_id = next_symbol_id;
    }

    pub(crate) fn alloc_promise(&mut self) -> ObjectId {
        self.alloc(Object::Promise(PromiseObject {
            state: PromiseState::Pending,
            reactions: Vec::new(),
            locked: false,
            adopting: None,
            finally_settlement: None,
            props: BTreeMap::new(),
            symbol_props: BTreeMap::new(),
            prototype: self.promise_prototype,
        }))
    }

    /// Object edges plus combine-state edges. This is the reachability list.
    pub(crate) fn write_edges(
        &self,
        id: ObjectId,
        out: &mut Vec<OwnedEdge>,
    ) -> Result<(), VmError> {
        self.get(id)?.write_edges(out);
        if let Some(state) = self.combines.get(&id) {
            for element in &state.elements {
                out.push(OwnedEdge::Object(*element));
            }
            for value in &state.results {
                push_value(out, value);
            }
            for recorded in &state.states {
                push_state(out, recorded);
            }
            if let Some(value) = &state.first {
                push_value(out, value);
            }
        }
        Ok(())
    }

    pub(crate) fn alloc_plain(&mut self) -> ObjectId {
        self.alloc_plain_with(self.object_prototype)
    }

    pub(crate) fn alloc_plain_with(&mut self, prototype: Option<ObjectId>) -> ObjectId {
        self.alloc(Object::Plain(PlainObject {
            props: BTreeMap::new(),
            symbol_props: BTreeMap::new(),
            prototype,
        }))
    }

    pub(crate) fn alloc_array(&mut self, elements: Vec<Value>) -> ObjectId {
        self.alloc(Object::Array(ArrayObject {
            elements,
            props: BTreeMap::new(),
            symbol_props: BTreeMap::new(),
            prototype: self.array_prototype,
        }))
    }

    pub(crate) fn alloc_function(&mut self, func_id: u32, env: Option<ObjectId>) -> ObjectId {
        self.alloc(Object::Function(FunctionObject {
            func_id,
            env,
            props: BTreeMap::new(),
            symbol_props: BTreeMap::new(),
            prototype: None,
        }))
    }

    pub(crate) fn alloc_builtin(&mut self, name: &str, member: Option<&str>) -> ObjectId {
        self.alloc(Object::BuiltinStatic(BuiltinStatic {
            name: name.to_owned(),
            member: member.map(str::to_owned),
            props: BTreeMap::new(),
            symbol_props: BTreeMap::new(),
            prototype: None,
        }))
    }

    pub(crate) fn alloc_environment(
        &mut self,
        parent: Option<ObjectId>,
        bindings: BTreeMap<BindingId, Value>,
    ) -> ObjectId {
        self.alloc(Object::Environment(EnvironmentObject { parent, bindings }))
    }

    pub(crate) fn insert_restore(&mut self, id: ObjectId, object: Object) -> Result<(), VmError> {
        if self.objects.insert(id, object).is_some() {
            return Err(VmError::InvalidSnapshot(format!(
                "duplicate object id {id}"
            )));
        }
        Ok(())
    }

    pub(crate) fn finish_restore(&mut self) {
        self.next_id = self.objects.keys().next_back().map_or(1, |id| id + 1);
        self.resolutions = self
            .objects
            .iter()
            .filter_map(|(id, object)| match object {
                Object::Resolution(resolution) if !resolution.called => Some(*id),
                _ => None,
            })
            .collect();
    }

    pub(crate) fn contains(&self, id: ObjectId) -> bool {
        self.objects.contains_key(&id)
    }

    pub(crate) fn get(&self, id: ObjectId) -> Result<&Object, VmError> {
        self.objects.get(&id).ok_or_else(|| missing_object(id))
    }

    pub(crate) fn get_mut(&mut self, id: ObjectId) -> Result<&mut Object, VmError> {
        self.objects.get_mut(&id).ok_or_else(|| missing_object(id))
    }

    pub(crate) fn cloned(&self, id: ObjectId) -> Result<Object, VmError> {
        self.get(id).cloned()
    }

    /// `None` if missing, otherwise whether the object is an environment.
    pub(crate) fn is_environment(&self, id: ObjectId) -> Option<bool> {
        self.objects
            .get(&id)
            .map(|object| matches!(object, Object::Environment(_)))
    }

    pub(crate) fn promise(&self, id: ObjectId) -> Result<&PromiseObject, VmError> {
        self.get(id)?.as_promise().ok_or_else(|| not_promise(id))
    }

    pub(crate) fn promise_mut(&mut self, id: ObjectId) -> Result<&mut PromiseObject, VmError> {
        self.get_mut(id)?
            .as_promise_mut()
            .ok_or_else(|| not_promise(id))
    }

    pub(crate) fn environment(&self, id: ObjectId) -> Result<&EnvironmentObject, VmError> {
        self.get(id)?
            .as_environment()
            .ok_or_else(|| not_environment(id))
    }

    pub(crate) fn environment_mut(
        &mut self,
        id: ObjectId,
    ) -> Result<&mut EnvironmentObject, VmError> {
        self.get_mut(id)?
            .as_environment_mut()
            .ok_or_else(|| not_environment(id))
    }

    pub(crate) fn function(&self, id: ObjectId) -> Option<&FunctionObject> {
        self.objects.get(&id).and_then(Object::as_function)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (ObjectId, &Object)> {
        self.objects.iter().map(|(id, object)| (*id, object))
    }

    pub(crate) fn ids(&self) -> Vec<ObjectId> {
        self.objects.keys().copied().collect()
    }

    pub(crate) fn intern_native(&mut self, key: NativeKey) -> ObjectId {
        if let Some(id) = self.objects.iter().find_map(|(id, object)| match object {
            Object::Native(native) if native.key == key => Some(*id),
            _ => None,
        }) {
            return id;
        }
        self.alloc(Object::Native(NativeObject { key }))
    }

    pub(crate) fn lookup_property(
        &self,
        id: ObjectId,
        key: PropertyKeyRef<'_>,
    ) -> Result<Option<Property>, VmError> {
        let mut current = id;
        let mut previous = None;
        let mut remaining = self.objects.len();
        loop {
            let object = self.objects.get(&current).ok_or_else(|| match previous {
                Some(parent) => VmError::InvalidState(format!(
                    "object {parent} prototype {current} does not exist"
                )),
                None => missing_object(current),
            })?;
            if matches!(object, Object::Environment(_)) {
                return Err(VmError::InvalidState(match previous {
                    Some(parent) => {
                        format!("object {parent} prototype {current} is an environment")
                    }
                    None => format!("object {id} is an environment and cannot be a prototype"),
                }));
            }
            if remaining == 0 {
                return Err(VmError::InvalidState(format!(
                    "prototype chain of object {id} is cyclic"
                )));
            }
            remaining -= 1;
            let (own, prototype) = match object {
                Object::Plain(plain) | Object::ArrayIterator { object: plain, .. } => (
                    lookup_tables(&plain.props, &plain.symbol_props, key).cloned(),
                    plain.prototype,
                ),
                Object::Array(array) => (array.own(key).map(Property::Data), array.prototype),
                Object::Function(function) => (
                    lookup_tables(&function.props, &function.symbol_props, key)
                        .cloned()
                        .map(Property::Data),
                    function.prototype,
                ),
                Object::Promise(promise) => (
                    lookup_tables(&promise.props, &promise.symbol_props, key)
                        .cloned()
                        .map(Property::Data),
                    promise.prototype,
                ),
                Object::BuiltinStatic(builtin) => (
                    lookup_tables(&builtin.props, &builtin.symbol_props, key)
                        .cloned()
                        .map(Property::Data),
                    builtin.prototype,
                ),
                Object::Native(_) | Object::Resolution(_) | Object::Bound(_) => (None, None),
                Object::Environment(_) => unreachable!(),
            };
            if let Some(value) = own {
                return Ok(Some(value));
            }
            let Some(next) = prototype else {
                return Ok(None);
            };
            previous = Some(current);
            current = next;
        }
    }

    pub(crate) fn has_own(&self, id: ObjectId, key: &PropertyKey) -> Result<bool, VmError> {
        let key = key.borrowed();
        Ok(match self.get(id)? {
            Object::Plain(plain) | Object::ArrayIterator { object: plain, .. } => {
                lookup_tables(&plain.props, &plain.symbol_props, key).is_some()
            }
            Object::Array(array) => {
                matches!(key, PropertyKeyRef::String("length")) || array.own(key).is_some()
            }
            Object::Function(function) => {
                lookup_tables(&function.props, &function.symbol_props, key).is_some()
            }
            Object::Promise(promise) => {
                lookup_tables(&promise.props, &promise.symbol_props, key).is_some()
            }
            Object::BuiltinStatic(builtin) => {
                lookup_tables(&builtin.props, &builtin.symbol_props, key).is_some()
            }
            Object::Native(_)
            | Object::Resolution(_)
            | Object::Environment(_)
            | Object::Bound(_) => false,
        })
    }

    /// String-keyed convenience for internal builtin setup.
    pub(crate) fn set_own_string(
        &mut self,
        id: ObjectId,
        key: &str,
        value: Value,
    ) -> Result<(), VmError> {
        self.set_own(id, PropertyKey::String(key.to_owned()), value)
    }

    /// Writes an own data property. Does not walk the chain or apply readonly checks.
    pub(crate) fn set_own(
        &mut self,
        id: ObjectId,
        key: PropertyKey,
        value: Value,
    ) -> Result<(), VmError> {
        match self.get_mut(id)? {
            Object::Plain(plain) | Object::ArrayIterator { object: plain, .. } => {
                insert_table(
                    &mut plain.props,
                    &mut plain.symbol_props,
                    key,
                    Property::Data(value),
                );
                Ok(())
            }
            Object::Function(function) => {
                insert_table(&mut function.props, &mut function.symbol_props, key, value);
                Ok(())
            }
            Object::Promise(promise) => {
                insert_table(&mut promise.props, &mut promise.symbol_props, key, value);
                Ok(())
            }
            Object::BuiltinStatic(builtin) => {
                insert_table(&mut builtin.props, &mut builtin.symbol_props, key, value);
                Ok(())
            }
            Object::Array(array) => match key {
                PropertyKey::String(key) => array.set(&key, value),
                PropertyKey::Symbol(id) => {
                    array.symbol_props.insert(id, value);
                    Ok(())
                }
            },
            _ => Err(VmError::InvalidState(format!(
                "object {id} does not expose script properties"
            ))),
        }
    }

    pub(crate) fn define_property(
        &mut self,
        id: ObjectId,
        key: PropertyKey,
        property: Property,
    ) -> Result<(), VmError> {
        let plain = match self.get_mut(id)? {
            Object::Plain(plain) | Object::ArrayIterator { object: plain, .. } => plain,
            _ => {
                return Err(VmError::InvalidState(
                    "property definition requires a plain object".to_owned(),
                ));
            }
        };
        match key {
            PropertyKey::String(key) => define_in_table(plain.props.entry(key.clone()), property),
            PropertyKey::Symbol(id) => {
                define_in_table(plain.symbol_props.entry(id), property);
            }
        }
        Ok(())
    }

    pub(crate) fn get_binding(&self, env: ObjectId, binding: BindingId) -> Result<Value, VmError> {
        let Some(owner) = self.binding_owner(env, binding)? else {
            return Err(VmError::InvalidState(format!(
                "binding {binding} is not initialized"
            )));
        };
        self.environment(owner)?
            .bindings
            .get(&binding)
            .cloned()
            .ok_or_else(|| VmError::InvalidState(format!("binding {binding} is not initialized")))
    }

    pub(crate) fn set_binding(
        &mut self,
        env: ObjectId,
        binding: BindingId,
        value: Value,
    ) -> Result<(), VmError> {
        let Some(owner) = self.binding_owner(env, binding)? else {
            return Err(VmError::InvalidState(format!(
                "binding {binding} is not declared"
            )));
        };
        self.environment_mut(owner)?.bindings.insert(binding, value);
        Ok(())
    }

    pub(crate) fn declare_binding(
        &mut self,
        env: ObjectId,
        binding: BindingId,
    ) -> Result<(), VmError> {
        self.environment_mut(env)?
            .bindings
            .entry(binding)
            .or_insert(Value::Undefined);
        Ok(())
    }

    fn binding_owner(
        &self,
        env: ObjectId,
        binding: BindingId,
    ) -> Result<Option<ObjectId>, VmError> {
        let mut current = Some(env);
        while let Some(id) = current {
            let environment = self.environment(id)?;
            if environment.bindings.contains_key(&binding) {
                return Ok(Some(id));
            }
            current = environment.parent;
        }
        Ok(None)
    }
}

fn lookup_tables<'m, V>(
    props: &'m BTreeMap<String, V>,
    symbol_props: &'m BTreeMap<SymbolId, V>,
    key: PropertyKeyRef<'_>,
) -> Option<&'m V> {
    match key {
        PropertyKeyRef::String(key) => props.get(key),
        PropertyKeyRef::Symbol(id) => symbol_props.get(&id),
    }
}

fn define_in_table<K: Ord>(entry: Entry<'_, K, Property>, mut property: Property) {
    match entry {
        Entry::Occupied(mut own) => {
            if let Property::Accessor { get, set } = &mut property
                && let Property::Accessor {
                    get: old_get,
                    set: old_set,
                } = own.get()
            {
                *get = get.or(*old_get);
                *set = set.or(*old_set);
            }
            own.insert(property);
        }
        Entry::Vacant(own) => {
            own.insert(property);
        }
    }
}

fn insert_table<V>(
    props: &mut BTreeMap<String, V>,
    symbol_props: &mut BTreeMap<SymbolId, V>,
    key: PropertyKey,
    value: V,
) {
    match key {
        PropertyKey::String(key) => props.insert(key, value),
        PropertyKey::Symbol(id) => symbol_props.insert(id, value),
    };
}

pub(crate) fn object_id(value: &Value, what: &str) -> Result<ObjectId, VmError> {
    match value {
        Value::Object(id) => Ok(*id),
        _ => Err(VmError::InvalidState(what.to_owned())),
    }
}

fn missing_object(id: ObjectId) -> VmError {
    VmError::InvalidState(format!("object {id} does not exist"))
}

fn not_promise(id: ObjectId) -> VmError {
    VmError::InvalidState(format!("object {id} is not a Promise"))
}

fn not_environment(id: ObjectId) -> VmError {
    VmError::InvalidState(format!("object {id} is not an environment"))
}
