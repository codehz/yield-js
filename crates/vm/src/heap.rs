//! Object table. Every heap edge is listed by [`Object::write_edges`] plus
//! combine-state edges appended by [`Heap::write_edges`]. Snapshot capture and
//! restore consume that combined list instead of re-matching object kinds.
//!
//! Script objects store own string data properties and an internal prototype.
//! `Some(undefined)` is an own hit; only a missing key walks the chain. Writes
//! always land on the receiver. Environments are not script objects and cannot
//! be prototypes. Ordinary literals have no internal prototype: this slice does
//! not expose `Object.prototype`.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::VmError;
use crate::ops::{array_index, property_key};
use crate::value::{BindingId, ContinuationId, NativeKey, ObjectId, Value};

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
    /// Internal prototype. Promise instances share one identity.
    pub prototype: Option<ObjectId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NativeObject {
    pub key: NativeKey,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlainObject {
    pub props: BTreeMap<String, Value>,
    /// Internal prototype. Literals use `None`.
    pub prototype: Option<ObjectId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArrayObject {
    pub elements: Vec<Value>,
    /// Own properties other than `length` and canonical indices.
    pub props: BTreeMap<String, Value>,
    pub prototype: Option<ObjectId>,
}

impl ArrayObject {
    /// `Some` is an own hit, including an explicit `undefined`. A missing
    /// non-index key is `None` so lookup can continue.
    fn own(&self, key: &str) -> Option<Value> {
        if key == "length" {
            return Some(Value::Number(self.elements.len() as f64));
        }
        if let Some(index) = array_index(key) {
            return Some(
                self.elements
                    .get(index)
                    .cloned()
                    .unwrap_or(Value::Undefined),
            );
        }
        self.props.get(key).cloned()
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
#[serde(rename_all = "snake_case")]
pub enum Object {
    Promise(PromiseObject),
    Native(NativeObject),
    Plain(PlainObject),
    Array(ArrayObject),
    Function(FunctionObject),
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
    Environment { id: ObjectId, role: EnvRole },
    Continuation(ContinuationId),
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
            }
            Self::BuiltinStatic(builtin) => {
                push_script(out, &builtin.props, builtin.prototype);
            }
            Self::Resolution(resolution) => {
                out.push(OwnedEdge::Object(resolution.promise));
                if resolution.pair != 0 {
                    out.push(OwnedEdge::Object(resolution.pair));
                }
            }
            Self::Native(_) => {}
            Self::Plain(plain) => push_script(out, &plain.props, plain.prototype),
            Self::Array(array) => {
                for value in &array.elements {
                    push_value(out, value);
                }
                push_script(out, &array.props, array.prototype);
            }
            Self::Function(function) => {
                if let Some(id) = function.env {
                    out.push(OwnedEdge::Environment {
                        id,
                        role: EnvRole::Closure,
                    });
                }
                push_script(out, &function.props, function.prototype);
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

fn push_script(
    out: &mut Vec<OwnedEdge>,
    props: &BTreeMap<String, Value>,
    prototype: Option<ObjectId>,
) {
    if let Some(id) = prototype {
        out.push(OwnedEdge::Object(id));
    }
    for value in props.values() {
        push_value(out, value);
    }
}

fn push_value(out: &mut Vec<OwnedEdge>, value: &Value) {
    if let Value::Object(id) = value {
        out.push(OwnedEdge::Object(*id));
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
    pub(crate) combines: BTreeMap<ObjectId, CombineState>,
    /// Uncalled [`Resolution`] objects. Capture rejects any id in this set that
    /// the tracer did not reach: that capability is only held by an in-flight
    /// `then` call, which is not a snapshot root.
    pub(crate) resolutions: BTreeSet<ObjectId>,
    /// Shared internal prototype of every Promise allocated by this heap.
    pub(crate) promise_prototype: Option<ObjectId>,
}

impl Heap {
    pub(crate) fn new() -> Self {
        Self {
            objects: BTreeMap::new(),
            next_id: 1,
            combines: BTreeMap::new(),
            resolutions: BTreeSet::new(),
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

    pub(crate) fn alloc_promise(&mut self) -> ObjectId {
        self.alloc(Object::Promise(PromiseObject {
            state: PromiseState::Pending,
            reactions: Vec::new(),
            locked: false,
            adopting: None,
            finally_settlement: None,
            props: BTreeMap::new(),
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
        self.alloc_plain_with(None)
    }

    pub(crate) fn alloc_plain_with(&mut self, prototype: Option<ObjectId>) -> ObjectId {
        self.alloc(Object::Plain(PlainObject {
            props: BTreeMap::new(),
            prototype,
        }))
    }

    pub(crate) fn alloc_array(&mut self, elements: Vec<Value>) -> ObjectId {
        self.alloc(Object::Array(ArrayObject {
            elements,
            props: BTreeMap::new(),
            prototype: None,
        }))
    }

    pub(crate) fn alloc_function(&mut self, func_id: u32, env: Option<ObjectId>) -> ObjectId {
        self.alloc(Object::Function(FunctionObject {
            func_id,
            env,
            props: BTreeMap::new(),
            prototype: None,
        }))
    }

    pub(crate) fn alloc_builtin(&mut self, name: &str, member: Option<&str>) -> ObjectId {
        self.alloc(Object::BuiltinStatic(BuiltinStatic {
            name: name.to_owned(),
            member: member.map(str::to_owned),
            props: BTreeMap::new(),
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

    /// Own hit is `Some`, including `undefined`. `None` means the key is absent.
    pub(crate) fn own_property(&self, id: ObjectId, key: &str) -> Result<Option<Value>, VmError> {
        Ok(match self.get(id)? {
            Object::Plain(plain) => plain.props.get(key).cloned(),
            Object::Array(array) => array.own(key),
            Object::Function(function) => function.props.get(key).cloned(),
            Object::Promise(promise) => promise.props.get(key).cloned(),
            Object::BuiltinStatic(builtin) => builtin.props.get(key).cloned(),
            Object::Native(_) | Object::Resolution(_) => None,
            Object::Environment(_) => {
                return Err(VmError::InvalidState(format!(
                    "object {id} is an environment and cannot be a prototype"
                )));
            }
        })
    }

    pub(crate) fn internal_prototype(&self, id: ObjectId) -> Result<Option<ObjectId>, VmError> {
        Ok(match self.get(id)? {
            Object::Plain(plain) => plain.prototype,
            Object::Array(array) => array.prototype,
            Object::Function(function) => function.prototype,
            Object::Promise(promise) => promise.prototype,
            Object::BuiltinStatic(builtin) => builtin.prototype,
            Object::Native(_) | Object::Resolution(_) | Object::Environment(_) => None,
        })
    }

    pub(crate) fn get_member(&self, object: &Value, key: &Value) -> Result<Value, VmError> {
        let key = property_key(key)?;
        let id = object_id(object, "property access on a non-object")?;
        self.get_property(id, &key)
    }

    pub(crate) fn get_property(&self, id: ObjectId, key: &str) -> Result<Value, VmError> {
        let mut current = id;
        let mut seen = BTreeSet::new();
        loop {
            if !seen.insert(current) {
                return Err(VmError::InvalidState(format!(
                    "prototype chain of object {id} is cyclic"
                )));
            }
            if let Some(value) = self.own_property(current, key)? {
                return Ok(value);
            }
            match self.internal_prototype(current)? {
                Some(next) => {
                    if !self.contains(next) {
                        return Err(VmError::InvalidState(format!(
                            "object {current} prototype {next} does not exist"
                        )));
                    }
                    if self.is_environment(next) == Some(true) {
                        return Err(VmError::InvalidState(format!(
                            "object {current} prototype {next} is an environment"
                        )));
                    }
                    current = next;
                }
                None => return Ok(Value::Undefined),
            }
        }
    }

    pub(crate) fn set_member(
        &mut self,
        object: &Value,
        key: &Value,
        value: Value,
    ) -> Result<(), VmError> {
        let key = property_key(key)?;
        let id = object_id(object, "property write on a non-object")?;
        self.set_own_key(id, &key, value)
    }

    /// Writes an own data property. Does not walk the chain or apply readonly checks.
    pub(crate) fn set_own(&mut self, id: ObjectId, key: &str, value: Value) -> Result<(), VmError> {
        self.set_own_key(id, key, value)
    }

    fn set_own_key(&mut self, id: ObjectId, key: &str, value: Value) -> Result<(), VmError> {
        match self.get_mut(id)? {
            Object::Plain(plain) => {
                plain.props.insert(key.to_owned(), value);
                Ok(())
            }
            Object::Function(function) => {
                function.props.insert(key.to_owned(), value);
                Ok(())
            }
            Object::Promise(promise) => {
                promise.props.insert(key.to_owned(), value);
                Ok(())
            }
            Object::BuiltinStatic(builtin) => {
                builtin.props.insert(key.to_owned(), value);
                Ok(())
            }
            Object::Array(array) => array.set(key, value),
            _ => Err(VmError::InvalidState(format!(
                "object {id} does not expose script properties"
            ))),
        }
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

fn object_id(value: &Value, what: &str) -> Result<ObjectId, VmError> {
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
