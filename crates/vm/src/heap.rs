//! Object table. Every heap edge is listed by [`Object::write_edges`] plus
//! combine-state edges appended by [`Heap::write_edges`]. Snapshot capture and
//! restore consume that combined list instead of re-matching object kinds.
//!
//! Script property tables store data or accessor properties. Only a missing
//! key walks the prototype chain; environments and native handles are opaque.

use std::borrow::Borrow;
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::VmError;
use crate::ops::array_index;
use crate::value::{
    BindingId, ContinuationId, NativeKey, ObjectId, PropertyKey, PropertyKeyRef, SymbolId, Value,
};

/// Reserved well-known symbols. Ordinary symbols allocate from 2 and are never
/// interned by description; only the registry interns by key.
pub(crate) const ITERATOR_SYMBOL: SymbolId = 0;
pub(crate) const ITERATOR_DESCRIPTION: &str = "Symbol.iterator";
pub(crate) const TO_PRIMITIVE_SYMBOL: SymbolId = 1;
pub(crate) const TO_PRIMITIVE_DESCRIPTION: &str = "Symbol.toPrimitive";

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
    pub props: PropertyTable<String, Property>,
    /// Symbol-keyed own properties, kept beside the string table.
    pub symbol_props: PropertyTable<SymbolId, Property>,
    /// Internal prototype. Promise instances share one identity.
    pub prototype: Option<ObjectId>,
    extensible: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NativeObject {
    pub key: NativeKey,
}

/// Lookup stays keyed; the ordinal records creation order, not write order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropertyTable<K: Ord, V>(BTreeMap<K, OrderedProperty<V>>);

type PropertyTables<'a> = (
    &'a PropertyTable<String, Property>,
    &'a PropertyTable<SymbolId, Property>,
);
type PropertyTablesMut<'a> = (
    &'a mut PropertyTable<String, Property>,
    &'a mut PropertyTable<SymbolId, Property>,
);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct OrderedProperty<V> {
    ordinal: usize,
    value: V,
    attributes: Attributes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Attributes {
    pub writable: bool,
    pub enumerable: bool,
    pub configurable: bool,
}

impl Attributes {
    pub const ORDINARY: Self = Self {
        writable: true,
        enumerable: true,
        configurable: true,
    };
    pub const METHOD: Self = Self {
        enumerable: false,
        ..Self::ORDINARY
    };
    pub const FIXED: Self = Self {
        writable: false,
        enumerable: false,
        configurable: false,
    };
    pub const FUNCTION_PROTOTYPE: Self = Self {
        writable: true,
        ..Self::FIXED
    };
    const ACCESSOR: Self = Self {
        writable: false,
        ..Self::ORDINARY
    };
}

impl<K: Ord, V> PropertyTable<K, V> {
    fn new() -> Self {
        Self(BTreeMap::new())
    }

    pub fn get<Q: Ord + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        self.0.get(key).map(|property| &property.value)
    }

    fn entry<Q: Ord + ?Sized>(&self, key: &Q) -> Option<&OrderedProperty<V>>
    where
        K: Borrow<Q>,
    {
        self.0.get(key)
    }

    pub fn get_mut<Q: Ord + ?Sized>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
    {
        self.0.get_mut(key).map(|property| &mut property.value)
    }

    /// Storage order, for internal traversal rather than script enumeration.
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.0.iter().map(|(key, property)| (key, &property.value))
    }

    fn update(
        &mut self,
        key: K,
        attributes: Option<Attributes>,
        value: impl FnOnce(Option<&V>) -> V,
    ) {
        let ordinal = self.0.len();
        match self.0.entry(key) {
            Entry::Occupied(mut entry) => {
                let property = entry.get_mut();
                property.value = value(Some(&property.value));
                if let Some(attributes) = attributes {
                    property.attributes = attributes;
                }
            }
            Entry::Vacant(entry) => {
                entry.insert(OrderedProperty {
                    ordinal,
                    value: value(None),
                    attributes: attributes.unwrap_or(Attributes::ORDINARY),
                });
            }
        }
    }

    fn remove_configurable<Q: Ord + ?Sized>(&mut self, key: &Q) -> bool
    where
        K: Borrow<Q>,
    {
        let Some(property) = self.0.get(key) else {
            return true;
        };
        if !property.attributes.configurable {
            return false;
        }
        let ordinal = property.ordinal;
        self.0.remove(key);
        // Keep ordinals dense so reinsertion appends and snapshots keep the
        // same validation rules, without adding an ordering counter.
        if ordinal < self.0.len() {
            for property in self.0.values_mut() {
                if property.ordinal > ordinal {
                    property.ordinal -= 1;
                }
            }
        }
        true
    }

    fn valid_order(&self) -> bool {
        let mut seen = vec![false; self.0.len()];
        self.0.values().all(|property| {
            seen.get_mut(property.ordinal)
                .is_some_and(|seen| !std::mem::replace(seen, true))
        })
    }
}

fn collect_own_keys<'a, V>(
    keys: &mut Vec<((u8, usize), PropertyKeyRef<'a>)>,
    props: &'a PropertyTable<String, V>,
    symbols: &'a PropertyTable<SymbolId, V>,
    enumerable_strings_only: bool,
) {
    if !enumerable_strings_only {
        keys.reserve(props.0.len() + symbols.0.len());
    }
    keys.extend(
        props
            .0
            .iter()
            .filter(|(_, property)| !enumerable_strings_only || property.attributes.enumerable)
            .map(|(key, property)| {
                let order = match array_index(key) {
                    Some(index) => (0, index),
                    None => (1, property.ordinal),
                };
                (order, PropertyKeyRef::String(key))
            }),
    );
    if !enumerable_strings_only {
        keys.extend(
            symbols
                .0
                .iter()
                .map(|(&id, property)| ((2, property.ordinal), PropertyKeyRef::Symbol(id))),
        );
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Property {
    Data(Value),
    Accessor {
        get: Option<ObjectId>,
        set: Option<ObjectId>,
    },
}

/// An absent field leaves the current descriptor unchanged. `Some(None)`
/// explicitly clears an accessor handler.
#[derive(Default)]
pub(crate) struct DescriptorUpdate {
    pub value: Option<Value>,
    pub writable: Option<bool>,
    pub get: Option<Option<ObjectId>>,
    pub set: Option<Option<ObjectId>>,
    pub enumerable: Option<bool>,
    pub configurable: Option<bool>,
}

impl DescriptorUpdate {
    pub(crate) fn is_accessor(&self) -> bool {
        self.get.is_some() || self.set.is_some()
    }

    pub(crate) fn is_data(&self) -> bool {
        self.value.is_some() || self.writable.is_some()
    }

    fn complete(
        self,
        extensible: bool,
        current: Option<OwnProperty<'_>>,
    ) -> Option<(Property, Attributes)> {
        if current.is_none() && !extensible {
            return None;
        }
        let length;
        let (old, attrs) = match current {
            Some(OwnProperty::Stored(property, attrs)) => (Some(property), attrs),
            Some(property @ OwnProperty::ArrayLength(..)) => {
                let attrs = property.attributes();
                length = property.into_owned();
                (Some(&length), attrs)
            }
            None => (None, Attributes::FIXED),
        };
        let old_accessor = matches!(old, Some(Property::Accessor { .. }));
        let accessor = self.is_accessor() || !self.is_data() && old_accessor;
        let next = Attributes {
            enumerable: self.enumerable.unwrap_or(attrs.enumerable),
            configurable: self.configurable.unwrap_or(attrs.configurable),
            writable: !accessor && self.writable.unwrap_or(attrs.writable),
        };
        if old.is_some()
            && !attrs.configurable
            && (next.configurable
                || next.enumerable != attrs.enumerable
                || accessor != old_accessor)
        {
            return None;
        }
        let property = if accessor {
            let (get, set) = match old {
                Some(Property::Accessor { get, set }) => (*get, *set),
                _ => (None, None),
            };
            let next_get = self.get.unwrap_or(get);
            let next_set = self.set.unwrap_or(set);
            if old_accessor && !attrs.configurable && (get != next_get || set != next_set) {
                return None;
            }
            Property::Accessor {
                get: next_get,
                set: next_set,
            }
        } else {
            let value = match old {
                Some(Property::Data(value)) => value,
                _ => &Value::Undefined,
            };
            if old.is_some()
                && !attrs.configurable
                && !attrs.writable
                && (next.writable
                    || self
                        .value
                        .as_ref()
                        .is_some_and(|next| !crate::ops::same_value(value, next)))
            {
                return None;
            }
            Property::Data(self.value.unwrap_or_else(|| value.clone()))
        };
        Some((property, next))
    }
}

pub(crate) enum OwnProperty<'a> {
    Stored(&'a Property, Attributes),
    ArrayLength(usize, bool),
}

impl OwnProperty<'_> {
    pub(crate) fn into_owned(self) -> Property {
        match self {
            Self::Stored(property, _) => property.clone(),
            Self::ArrayLength(length, _) => Property::Data(Value::Number(length as f64)),
        }
    }

    pub(crate) fn attributes(&self) -> Attributes {
        match self {
            Self::Stored(_, attributes) => *attributes,
            Self::ArrayLength(_, writable) => Attributes {
                writable: *writable,
                ..Attributes::FIXED
            },
        }
    }

    pub(crate) fn setter(&self) -> Option<Option<ObjectId>> {
        match self {
            Self::Stored(Property::Accessor { set, .. }, _) => Some(*set),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlainObject {
    pub props: PropertyTable<String, Property>,
    pub symbol_props: PropertyTable<SymbolId, Property>,
    /// Internal prototype.
    pub prototype: Option<ObjectId>,
    extensible: bool,
}

impl PlainObject {
    pub(crate) fn new(prototype: Option<ObjectId>) -> Self {
        Self {
            props: PropertyTable::new(),
            symbol_props: PropertyTable::new(),
            prototype,
            extensible: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArrayObject {
    elements: BTreeMap<usize, (Property, Attributes)>,
    pub(crate) length: usize,
    pub(crate) length_writable: bool,
    /// Own properties other than `length` and canonical indices.
    pub props: PropertyTable<String, Property>,
    pub symbol_props: PropertyTable<SymbolId, Property>,
    pub prototype: Option<ObjectId>,
    extensible: bool,
}

impl ArrayObject {
    pub(crate) fn own_index(&self, index: usize) -> Option<OwnProperty<'_>> {
        self.elements
            .get(&index)
            .map(|(property, attributes)| OwnProperty::Stored(property, *attributes))
    }

    pub(crate) fn dense_values(&self) -> Result<Vec<Value>, VmError> {
        let invalid =
            || VmError::InvalidState("spread arguments require a dense data array".to_owned());
        if self.length != self.elements.len() {
            return Err(invalid());
        }
        self.elements
            .iter()
            .enumerate()
            .map(|(expected, (&index, (property, _)))| match property {
                Property::Data(value) if index == expected => Ok(value.clone()),
                _ => Err(invalid()),
            })
            .collect()
    }

    pub(crate) fn append(&mut self, value: Value) -> Result<(), VmError> {
        if self.length >= u32::MAX as usize || !self.length_writable || !self.extensible {
            return Err(VmError::InvalidState(
                "cannot append to array literal".to_owned(),
            ));
        }
        self.elements
            .insert(self.length, (Property::Data(value), Attributes::ORDINARY));
        self.length += 1;
        Ok(())
    }

    /// Delete in descending index order. A fixed element blocks further
    /// deletions, but neither prior deletions nor the writable change roll back.
    fn set_length(&mut self, length: usize, writable: bool) -> bool {
        while let Some((&index, (_, attributes))) = self.elements.last_key_value() {
            if index < length {
                break;
            }
            if !attributes.configurable {
                self.length = index + 1;
                self.length_writable = writable;
                return false;
            }
            self.elements.pop_last();
        }
        self.length = length;
        self.length_writable = writable;
        true
    }
}

/// A function value. `env` is the lexical environment captured at creation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionObject {
    pub func_id: u32,
    pub env: ObjectId,
    pub props: PropertyTable<String, Property>,
    pub symbol_props: PropertyTable<SymbolId, Property>,
    /// Internal prototype. Not the constructable `.prototype` property.
    pub prototype: Option<ObjectId>,
    extensible: bool,
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
    pub props: PropertyTable<String, Property>,
    pub symbol_props: PropertyTable<SymbolId, Property>,
    pub prototype: Option<ObjectId>,
    extensible: bool,
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
    pub object: PlainObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoundObject {
    pub target: ObjectId,
    pub bound_this: Value,
    pub args: Vec<Value>,
    pub object: PlainObject,
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
    StringIterator {
        object: PlainObject,
        string: String,
        /// UTF-8 byte offset, always on a code point boundary.
        byte_offset: usize,
    },
    Function(FunctionObject),
    Bound(BoundObject),
    Environment(EnvironmentObject),
    BuiltinStatic(BuiltinStatic),
    Resolution(Resolution),
}

#[derive(Clone, Copy)]
pub(crate) enum IntegrityLevel {
    Sealed,
    Frozen,
}

impl IntegrityLevel {
    fn freeze_data(self, property: &Property) -> bool {
        matches!(self, Self::Frozen) && matches!(property, Property::Data(_))
    }

    fn restrict(self, property: &Property, attributes: &mut Attributes) {
        attributes.configurable = false;
        if self.freeze_data(property) {
            attributes.writable = false;
        }
    }

    fn allows(self, property: &Property, attributes: Attributes) -> bool {
        !attributes.configurable && !(attributes.writable && self.freeze_data(property))
    }
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
    Accessor(ObjectId),
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

    fn extensible_mut(&mut self) -> Option<&mut bool> {
        Some(match self {
            Self::Plain(o)
            | Self::ArrayIterator { object: o, .. }
            | Self::StringIterator { object: o, .. }
            | Self::Bound(BoundObject { object: o, .. })
            | Self::Resolution(Resolution { object: o, .. }) => &mut o.extensible,
            Self::Array(o) => &mut o.extensible,
            Self::Function(o) => &mut o.extensible,
            Self::Promise(o) => &mut o.extensible,
            Self::BuiltinStatic(o) => &mut o.extensible,
            Self::Native(_) | Self::Environment(_) => return None,
        })
    }

    pub(crate) fn is_extensible(&self) -> bool {
        match self {
            Self::Plain(o)
            | Self::ArrayIterator { object: o, .. }
            | Self::StringIterator { object: o, .. }
            | Self::Bound(BoundObject { object: o, .. })
            | Self::Resolution(Resolution { object: o, .. }) => o.extensible,
            Self::Array(o) => o.extensible,
            Self::Function(o) => o.extensible,
            Self::Promise(o) => o.extensible,
            Self::BuiltinStatic(o) => o.extensible,
            // Opaque native handles have no script property storage.
            Self::Native(_) | Self::Environment(_) => false,
        }
    }

    pub(crate) fn prevent_extensions(&mut self) {
        if let Some(extensible) = self.extensible_mut() {
            *extensible = false;
        }
    }

    pub(crate) fn set_integrity_level(&mut self, level: IntegrityLevel) {
        self.prevent_extensions();
        if let Some((props, symbols)) = self.tables_mut() {
            for entry in props.0.values_mut().chain(symbols.0.values_mut()) {
                level.restrict(&entry.value, &mut entry.attributes);
            }
        }
        if let Self::Array(array) = self {
            for (property, attributes) in array.elements.values_mut() {
                level.restrict(property, attributes);
            }
            if matches!(level, IntegrityLevel::Frozen) {
                array.length_writable = false;
            }
        }
    }

    pub(crate) fn test_integrity_level(&self, level: IntegrityLevel) -> bool {
        if self.is_extensible() {
            return false;
        }
        if let Self::Array(array) = self
            && (matches!(level, IntegrityLevel::Frozen) && array.length_writable
                || !array.elements.values().all(|(p, a)| level.allows(p, *a)))
        {
            return false;
        }
        self.tables().is_none_or(|(props, symbols)| {
            props
                .0
                .values()
                .chain(symbols.0.values())
                .all(|entry| level.allows(&entry.value, entry.attributes))
        })
    }

    fn tables(&self) -> Option<PropertyTables<'_>> {
        Some(match self {
            Self::Plain(o)
            | Self::ArrayIterator { object: o, .. }
            | Self::StringIterator { object: o, .. }
            | Self::Bound(BoundObject { object: o, .. })
            | Self::Resolution(Resolution { object: o, .. }) => (&o.props, &o.symbol_props),
            Self::Array(o) => (&o.props, &o.symbol_props),
            Self::Function(o) => (&o.props, &o.symbol_props),
            Self::Promise(o) => (&o.props, &o.symbol_props),
            Self::BuiltinStatic(o) => (&o.props, &o.symbol_props),
            Self::Native(_) | Self::Environment(_) => return None,
        })
    }

    fn tables_mut(&mut self) -> Option<PropertyTablesMut<'_>> {
        Some(match self {
            Self::Plain(o)
            | Self::ArrayIterator { object: o, .. }
            | Self::StringIterator { object: o, .. }
            | Self::Bound(BoundObject { object: o, .. })
            | Self::Resolution(Resolution { object: o, .. }) => (&mut o.props, &mut o.symbol_props),
            Self::Array(o) => (&mut o.props, &mut o.symbol_props),
            Self::Function(o) => (&mut o.props, &mut o.symbol_props),
            Self::Promise(o) => (&mut o.props, &mut o.symbol_props),
            Self::BuiltinStatic(o) => (&mut o.props, &mut o.symbol_props),
            Self::Native(_) | Self::Environment(_) => return None,
        })
    }

    pub(crate) fn own_property(&self, key: PropertyKeyRef<'_>) -> Option<OwnProperty<'_>> {
        if let (Self::Array(array), PropertyKeyRef::String(key)) = (self, key) {
            if key == "length" {
                return Some(OwnProperty::ArrayLength(
                    array.length,
                    array.length_writable,
                ));
            }
            if let Some(index) = array_index(key) {
                return array.own_index(index);
            }
        }
        let (props, symbols) = self.tables()?;
        lookup_tables(props, symbols, key).map(|p| OwnProperty::Stored(&p.value, p.attributes))
    }

    pub(crate) fn own_keys(
        &self,
        enumerable_strings_only: bool,
    ) -> impl Iterator<Item = PropertyKey> + '_ {
        let mut keys = Vec::new();
        if let Some((props, symbols)) = self.tables() {
            collect_own_keys(&mut keys, props, symbols, enumerable_strings_only);
        }
        keys.sort_unstable_by_key(|&(order, _)| order);
        let array = match self {
            Self::Array(array) => Some(array),
            _ => None,
        };
        array
            .into_iter()
            .flat_map(move |array| {
                array
                    .elements
                    .iter()
                    .filter(move |(_, (_, attributes))| {
                        !enumerable_strings_only || attributes.enumerable
                    })
                    .map(|(index, _)| PropertyKey::String(index.to_string()))
            })
            .chain(
                array
                    .filter(|_| !enumerable_strings_only)
                    .map(|_| "length".into()),
            )
            .chain(keys.into_iter().map(|(_, key)| match key {
                PropertyKeyRef::String(key) => PropertyKey::String(key.to_owned()),
                PropertyKeyRef::Symbol(id) => PropertyKey::Symbol(id),
            }))
    }

    fn valid_property_tables(&self) -> bool {
        if let Some((props, symbols)) = self.tables()
            && (!props.valid_order() || !symbols.valid_order())
        {
            return false;
        }
        match self {
            Self::Array(array) => {
                array.length <= u32::MAX as usize
                    && array
                        .elements
                        .last_key_value()
                        .is_none_or(|(&index, _)| index < array.length)
                    && array
                        .props
                        .0
                        .keys()
                        .all(|key| key != "length" && array_index(key).is_none())
            }
            _ => true,
        }
    }

    pub(crate) fn prototype(&self) -> Option<ObjectId> {
        match self {
            Self::Plain(plain)
            | Self::ArrayIterator { object: plain, .. }
            | Self::StringIterator { object: plain, .. } => plain.prototype,
            Self::Array(array) => array.prototype,
            Self::Function(function) => function.prototype,
            Self::Promise(promise) => promise.prototype,
            Self::BuiltinStatic(builtin) => builtin.prototype,
            Self::Bound(bound) => bound.object.prototype,
            Self::Resolution(resolution) => resolution.object.prototype,
            Self::Native(_) | Self::Environment(_) => None,
        }
    }

    fn prototype_mut(&mut self) -> Option<&mut Option<ObjectId>> {
        Some(match self {
            Self::Plain(o)
            | Self::ArrayIterator { object: o, .. }
            | Self::StringIterator { object: o, .. }
            | Self::Bound(BoundObject { object: o, .. })
            | Self::Resolution(Resolution { object: o, .. }) => &mut o.prototype,
            Self::Array(o) => &mut o.prototype,
            Self::Function(o) => &mut o.prototype,
            Self::Promise(o) => &mut o.prototype,
            Self::BuiltinStatic(o) => &mut o.prototype,
            Self::Native(_) | Self::Environment(_) => return None,
        })
    }

    /// Append every outgoing reference. Callers clear `out` only by relying on
    /// this method, which replaces the buffer contents.
    pub(crate) fn write_edges(&self, out: &mut Vec<OwnedEdge>) {
        out.clear();
        if let Some(id) = self.prototype() {
            out.push(OwnedEdge::Prototype(id));
        }
        if let Some((props, symbols)) = self.tables() {
            for (_, property) in props.iter() {
                push_property(out, property);
            }
            for (key, property) in symbols.iter() {
                out.push(OwnedEdge::Symbol(*key));
                push_property(out, property);
            }
        }
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
            }
            Self::Resolution(resolution) => {
                out.push(OwnedEdge::Object(resolution.promise));
                if resolution.pair != 0 {
                    out.push(OwnedEdge::Object(resolution.pair));
                }
            }
            Self::Native(_)
            | Self::Plain(_)
            | Self::BuiltinStatic(_)
            | Self::StringIterator { .. } => {}
            Self::ArrayIterator { array, .. } => {
                if let Some(id) = array {
                    out.push(OwnedEdge::Object(*id));
                }
            }
            Self::Array(array) => {
                for (property, _) in array.elements.values() {
                    push_property(out, property);
                }
            }
            Self::Function(function) => {
                out.push(OwnedEdge::Environment {
                    id: function.env,
                    role: EnvRole::Closure,
                });
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

fn push_property(out: &mut Vec<OwnedEdge>, property: &Property) {
    match property {
        Property::Data(value) => push_value(out, value),
        Property::Accessor { get, set } => {
            out.extend([*get, *set].into_iter().flatten().map(OwnedEdge::Accessor));
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
    pub(crate) function_prototype: Option<ObjectId>,
    /// Shared internal prototype of every Promise allocated by this heap.
    pub(crate) promise_prototype: Option<ObjectId>,
}

impl Heap {
    pub(crate) fn new() -> Self {
        Self {
            objects: BTreeMap::new(),
            next_id: 1,
            symbols: BTreeMap::from([
                (ITERATOR_SYMBOL, Some(ITERATOR_DESCRIPTION.to_owned())),
                (
                    TO_PRIMITIVE_SYMBOL,
                    Some(TO_PRIMITIVE_DESCRIPTION.to_owned()),
                ),
            ]),
            symbol_registry: BTreeMap::new(),
            next_symbol_id: 2,
            combines: BTreeMap::new(),
            resolutions: BTreeSet::new(),
            object_prototype: None,
            array_prototype: None,
            function_prototype: None,
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
            props: PropertyTable::new(),
            symbol_props: PropertyTable::new(),
            prototype: self.promise_prototype,
            extensible: true,
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
        self.alloc(Object::Plain(PlainObject::new(prototype)))
    }

    pub(crate) fn alloc_array(&mut self, elements: Vec<Value>) -> ObjectId {
        self.alloc(Object::Array(ArrayObject {
            length: elements.len(),
            length_writable: true,
            elements: elements
                .into_iter()
                .enumerate()
                .map(|(index, value)| (index, (Property::Data(value), Attributes::ORDINARY)))
                .collect(),
            props: PropertyTable::new(),
            symbol_props: PropertyTable::new(),
            prototype: self.array_prototype,
            extensible: true,
        }))
    }

    pub(crate) fn alloc_function(&mut self, func_id: u32, env: ObjectId) -> ObjectId {
        self.alloc(Object::Function(FunctionObject {
            func_id,
            env,
            props: PropertyTable::new(),
            symbol_props: PropertyTable::new(),
            prototype: self.function_prototype,
            extensible: true,
        }))
    }

    pub(crate) fn alloc_builtin(&mut self, name: &str, member: Option<&str>) -> ObjectId {
        self.alloc(Object::BuiltinStatic(BuiltinStatic {
            name: name.to_owned(),
            member: member.map(str::to_owned),
            props: PropertyTable::new(),
            symbol_props: PropertyTable::new(),
            prototype: self.function_prototype,
            extensible: true,
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
        if !object.valid_property_tables() {
            return Err(VmError::InvalidSnapshot(format!(
                "object {id} has invalid property tables"
            )));
        }
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

    pub(crate) fn has_property(
        &self,
        id: ObjectId,
        key: PropertyKeyRef<'_>,
    ) -> Result<bool, VmError> {
        Ok(self.find_property(id, key)?.is_some())
    }

    pub(crate) fn find_property(
        &self,
        id: ObjectId,
        key: PropertyKeyRef<'_>,
    ) -> Result<Option<OwnProperty<'_>>, VmError> {
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
            if let Some(property) = object.own_property(key) {
                return Ok(Some(property));
            }
            let Some(next) = object.prototype() else {
                return Ok(None);
            };
            previous = Some(current);
            current = next;
        }
    }

    pub(crate) fn set_prototype(
        &mut self,
        id: ObjectId,
        prototype: Option<ObjectId>,
    ) -> Result<bool, VmError> {
        let object = self.get(id)?;
        if object.prototype() == prototype {
            return Ok(true);
        }
        if self.object_prototype == Some(id) || !object.is_extensible() {
            return Ok(false);
        }
        // All existing links were checked at restore or installed here. A new
        // link can only form a cycle if its chain leads back to the receiver.
        let mut current = prototype;
        while let Some(ancestor) = current {
            if ancestor == id {
                return Ok(false);
            }
            current = self.get(ancestor)?.prototype();
        }
        *self
            .get_mut(id)?
            .prototype_mut()
            .expect("every extensible object has a prototype slot") = prototype;
        Ok(true)
    }

    pub(crate) fn has_own(&self, id: ObjectId, key: &PropertyKey) -> Result<bool, VmError> {
        Ok(self.get(id)?.own_property(key.borrowed()).is_some())
    }

    pub(crate) fn delete_own(
        &mut self,
        id: ObjectId,
        key: PropertyKeyRef<'_>,
    ) -> Result<bool, VmError> {
        let object = self.get_mut(id)?;
        if let (Object::Array(array), PropertyKeyRef::String(name)) = (&mut *object, key) {
            if name == "length" {
                return Ok(false);
            }
            if let Some(index) = array_index(name) {
                if let Entry::Occupied(entry) = array.elements.entry(index) {
                    if !entry.get().1.configurable {
                        return Ok(false);
                    }
                    entry.remove();
                }
                return Ok(true);
            }
        }
        let Some((props, symbols)) = object.tables_mut() else {
            // Opaque objects expose no own script properties.
            return Ok(true);
        };
        Ok(match key {
            PropertyKeyRef::String(key) => props.remove_configurable(key),
            PropertyKeyRef::Symbol(id) => symbols.remove_configurable(&id),
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

    pub(crate) fn define_data(
        &mut self,
        id: ObjectId,
        key: impl Into<PropertyKey>,
        value: Value,
        attributes: Attributes,
    ) -> Result<(), VmError> {
        self.write_own_property(id, key.into(), Property::Data(value), Some(attributes))
    }

    /// Writes an own data property. Does not walk the chain or apply readonly checks.
    pub(crate) fn set_own(
        &mut self,
        id: ObjectId,
        key: PropertyKey,
        value: Value,
    ) -> Result<(), VmError> {
        self.write_own_property(id, key, Property::Data(value), None)
    }

    pub(crate) fn apply_descriptor(
        &mut self,
        id: ObjectId,
        key: PropertyKey,
        update: DescriptorUpdate,
    ) -> Result<bool, VmError> {
        let object = self.get(id)?;
        if object.tables().is_none() {
            return Ok(false);
        }
        if let (Object::Array(array), Some(name)) = (object, key.as_string())
            && !array.length_writable
            && array_index(name).is_some_and(|index| index >= array.length)
        {
            return Ok(false);
        }
        let Some((property, attrs)) =
            update.complete(object.is_extensible(), object.own_property(key.borrowed()))
        else {
            return Ok(false);
        };
        if key.as_string() == Some("length")
            && let Object::Array(array) = self.get_mut(id)?
        {
            let Property::Data(Value::Number(length)) = property else {
                unreachable!("validated array length descriptor")
            };
            return Ok(array.set_length(length as usize, attrs.writable));
        }
        self.write_own_property(id, key, property, Some(attrs))?;
        Ok(true)
    }

    /// Storage only: script callers validate assignment or descriptors first.
    fn write_own_property(
        &mut self,
        id: ObjectId,
        key: PropertyKey,
        property: Property,
        attributes: Option<Attributes>,
    ) -> Result<(), VmError> {
        let object = self.get_mut(id)?;
        if let (Object::Array(array), PropertyKeyRef::String(name)) = (&mut *object, key.borrowed())
            && let Some(index) = array_index(name)
        {
            let entry = array
                .elements
                .entry(index)
                .or_insert((Property::Data(Value::Undefined), Attributes::ORDINARY));
            entry.0 = property;
            if let Some(attributes) = attributes {
                entry.1 = attributes;
            }
            array.length = array.length.max(index + 1);
            return Ok(());
        }
        let (props, symbols) = object.tables_mut().ok_or_else(|| {
            VmError::InvalidState(format!("object {id} does not expose script properties"))
        })?;
        insert_table(props, symbols, key, attributes, |_| property);
        Ok(())
    }

    pub(crate) fn define_property(
        &mut self,
        id: ObjectId,
        key: PropertyKey,
        property: Property,
    ) -> Result<(), VmError> {
        let plain = match self.get_mut(id)? {
            Object::Plain(plain)
            | Object::ArrayIterator { object: plain, .. }
            | Object::StringIterator { object: plain, .. } => plain,
            _ => {
                return Err(VmError::InvalidState(
                    "property definition requires a plain object".to_owned(),
                ));
            }
        };
        if !plain.extensible {
            return Err(VmError::InvalidState(
                "literal definition requires an extensible object".to_owned(),
            ));
        }
        let attributes = match property {
            Property::Data(_) => Attributes::ORDINARY,
            Property::Accessor { .. } => Attributes::ACCESSOR,
        };
        insert_table(
            &mut plain.props,
            &mut plain.symbol_props,
            key,
            Some(attributes),
            |old| {
                let mut property = property;
                if let Property::Accessor { get, set } = &mut property
                    && let Some(Property::Accessor {
                        get: old_get,
                        set: old_set,
                    }) = old
                {
                    *get = get.or(*old_get);
                    *set = set.or(*old_set);
                }
                property
            },
        );
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
    props: &'m PropertyTable<String, V>,
    symbol_props: &'m PropertyTable<SymbolId, V>,
    key: PropertyKeyRef<'_>,
) -> Option<&'m OrderedProperty<V>> {
    match key {
        PropertyKeyRef::String(key) => props.entry(key),
        PropertyKeyRef::Symbol(id) => symbol_props.entry(&id),
    }
}

fn insert_table<V>(
    props: &mut PropertyTable<String, V>,
    symbol_props: &mut PropertyTable<SymbolId, V>,
    key: PropertyKey,
    attributes: Option<Attributes>,
    value: impl FnOnce(Option<&V>) -> V,
) {
    match key {
        PropertyKey::String(key) => props.update(key, attributes, value),
        PropertyKey::Symbol(id) => symbol_props.update(id, attributes, value),
    }
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
