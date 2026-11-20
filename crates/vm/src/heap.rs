//! Object table. Strong edges are listed by [`Object::write_edges`] plus
//! combine-state edges appended by [`Heap::write_edges`]. Snapshot capture and
//! restore handle WeakMap entries separately as ephemerons.
//!
//! Script property tables store data or accessor properties. Only a missing
//! key walks the prototype chain; environments and native handles are opaque.

use std::borrow::Borrow;
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::JsString;
use crate::error::VmError;
use crate::ops::canonical_index_key;
use crate::regexp::{RegExpFlags, compile_pattern};
use crate::string::utf16_unit_string;
use crate::value::{
    BindingId, ContinuationId, ObjectId, PropertyKey, PropertyKeyRef, SymbolId, Value,
};

/// Reserved well-known symbols. Ordinary symbols allocate from 2 and are never
/// interned by description; only the registry interns by key.
pub(crate) const ITERATOR_SYMBOL: SymbolId = 0;
pub(crate) const ITERATOR_DESCRIPTION: &str = "Symbol.iterator";
pub(crate) const TO_PRIMITIVE_SYMBOL: SymbolId = 1;
pub(crate) const TO_PRIMITIVE_DESCRIPTION: &str = "Symbol.toPrimitive";
pub(crate) const SPECIES_SYMBOL: SymbolId = 2;
pub(crate) const SPECIES_DESCRIPTION: &str = "Symbol.species";
pub(crate) const MATCH_SYMBOL: SymbolId = 3;
pub(crate) const MATCH_DESCRIPTION: &str = "Symbol.match";
pub(crate) const MATCH_ALL_SYMBOL: SymbolId = 4;
pub(crate) const MATCH_ALL_DESCRIPTION: &str = "Symbol.matchAll";
pub(crate) const REPLACE_SYMBOL: SymbolId = 5;
pub(crate) const REPLACE_DESCRIPTION: &str = "Symbol.replace";
pub(crate) const SEARCH_SYMBOL: SymbolId = 6;
pub(crate) const SEARCH_DESCRIPTION: &str = "Symbol.search";
pub(crate) const SPLIT_SYMBOL: SymbolId = 7;
pub(crate) const SPLIT_DESCRIPTION: &str = "Symbol.split";

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
        resolve: Value,
        reject: Value,
        on_fulfilled: Option<Value>,
        on_rejected: Option<Value>,
    },
    /// Copy the source settlement onto `result`; `result.adopting` points back here.
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
    #[serde(with = "property_pairs")]
    pub props: PropertyTable<JsString, Property>,
    /// Symbol-keyed own properties, kept beside the string table.
    pub symbol_props: PropertyTable<SymbolId, Property>,
    /// Internal prototype. Promise instances share one identity.
    pub prototype: Option<ObjectId>,
    extensible: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum NativeObject {
    Host {
        target: crate::binding::NativeTarget,
        constructable: bool,
    },
    Intrinsic {
        intrinsic: crate::intrinsic::IntrinsicId,
        object: PlainObject,
    },
}

impl NativeObject {
    pub(crate) fn target(&self) -> crate::intrinsic::CallableTarget {
        match self {
            Self::Host { target, .. } => crate::intrinsic::CallableTarget::Host(*target),
            Self::Intrinsic { intrinsic, .. } => {
                crate::intrinsic::CallableTarget::Intrinsic(*intrinsic)
            }
        }
    }

    pub(crate) fn constructable(&self) -> bool {
        match self {
            Self::Host { constructable, .. } => *constructable,
            Self::Intrinsic { intrinsic, .. } => intrinsic.definition().construct.is_some(),
        }
    }
}

/// Lookup stays keyed; the ordinal records creation order, not write order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropertyTable<K: Ord, V>(BTreeMap<K, OrderedProperty<V>>);

type PropertyTables<'a> = (
    &'a PropertyTable<JsString, Property>,
    &'a PropertyTable<SymbolId, Property>,
);
type PropertyTablesMut<'a> = (
    &'a mut PropertyTable<JsString, Property>,
    &'a mut PropertyTable<SymbolId, Property>,
);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct OrderedProperty<V> {
    ordinal: usize,
    value: V,
    attributes: Attributes,
}

mod property_pairs {
    use super::*;

    pub(super) fn serialize<S: serde::Serializer>(
        table: &PropertyTable<JsString, Property>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        crate::snapshot::ordered_pairs::serialize(&table.0, serializer)
    }

    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<PropertyTable<JsString, Property>, D::Error> {
        crate::snapshot::ordered_pairs::deserialize(deserializer).map(PropertyTable)
    }
}

fn array_index(key: PropertyKeyRef<'_>) -> Option<usize> {
    canonical_index_key(key).filter(|&index| index < u32::MAX as usize)
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
    props: &'a PropertyTable<JsString, V>,
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
                let key = PropertyKeyRef::JsString(key);
                let order = match array_index(key) {
                    Some(index) => (0, index),
                    None => (2, property.ordinal),
                };
                (order, key)
            }),
    );
    if !enumerable_strings_only {
        keys.extend(
            symbols
                .0
                .iter()
                .map(|(&id, property)| ((3, property.ordinal), PropertyKeyRef::Symbol(id))),
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

    pub(crate) fn complete(
        self,
        extensible: bool,
        current: Option<OwnProperty<'_>>,
    ) -> Option<(Property, Attributes)> {
        if current.is_none() && !extensible {
            return None;
        }
        let virtual_property;
        let (old, attrs) = match current {
            Some(OwnProperty::Stored(property, attrs)) => (Some(property), attrs),
            Some(property @ OwnProperty::ArrayLength(..)) => {
                let attrs = property.attributes();
                virtual_property = property.into_owned();
                (Some(&virtual_property), attrs)
            }
            Some(OwnProperty::StringIndex(_) | OwnProperty::StringLength(_)) => {
                unreachable!("fixed string descriptors are validated without materializing them")
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

    fn allows_fixed_string(&self, property: OwnProperty<'_>) -> bool {
        !self.is_accessor()
            && self.writable != Some(true)
            && self.configurable != Some(true)
            && self
                .enumerable
                .is_none_or(|flag| flag == property.attributes().enumerable)
            && self
                .value
                .as_ref()
                .is_none_or(|value| match (property, value) {
                    (OwnProperty::StringLength(length), Value::Number(number)) => {
                        crate::ops::same_value(
                            &Value::Number(length as f64),
                            &Value::Number(*number),
                        )
                    }
                    (OwnProperty::StringIndex(unit), Value::String(text)) => {
                        text.unit_len() == 1 && text.unit_at(0) == Some(unit)
                    }
                    _ => false,
                })
    }
}

pub(crate) enum OwnProperty<'a> {
    Stored(&'a Property, Attributes),
    ArrayLength(usize, bool),
    StringLength(usize),
    StringIndex(u16),
}

impl OwnProperty<'_> {
    pub(crate) fn into_owned(self) -> Property {
        match self {
            Self::Stored(property, _) => property.clone(),
            Self::ArrayLength(length, _) => Property::Data(Value::Number(length as f64)),
            Self::StringLength(length) => Property::Data(Value::Number(length as f64)),
            Self::StringIndex(unit) => Property::Data(Value::String(utf16_unit_string(unit))),
        }
    }

    pub(crate) fn data_value(&self) -> Option<&Value> {
        match self {
            Self::Stored(Property::Data(value), _) => Some(value),
            Self::Stored(_, _) => None,
            _ => None,
        }
    }

    pub(crate) fn attributes(&self) -> Attributes {
        match self {
            Self::Stored(_, attributes) => *attributes,
            Self::ArrayLength(_, writable) => Attributes {
                writable: *writable,
                ..Attributes::FIXED
            },
            Self::StringLength(_) => Attributes::FIXED,
            Self::StringIndex(_) => Attributes {
                enumerable: true,
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
    #[serde(with = "property_pairs")]
    pub props: PropertyTable<JsString, Property>,
    pub symbol_props: PropertyTable<SymbolId, Property>,
    /// Internal prototype.
    pub prototype: Option<ObjectId>,
    extensible: bool,
    pub(crate) is_error: bool,
}

impl PlainObject {
    pub(crate) fn new(prototype: Option<ObjectId>) -> Self {
        Self {
            props: PropertyTable::new(),
            symbol_props: PropertyTable::new(),
            prototype,
            extensible: true,
            is_error: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArrayObject {
    elements: BTreeMap<usize, (Property, Attributes)>,
    pub(crate) length: usize,
    pub(crate) length_writable: bool,
    /// Own properties other than `length` and canonical indices.
    #[serde(with = "property_pairs")]
    pub props: PropertyTable<JsString, Property>,
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

    /// Grows `length` by one without creating an element property, forming a
    /// sparse hole.
    pub(crate) fn append_hole(&mut self) -> Result<(), VmError> {
        if self.length >= u32::MAX as usize || !self.length_writable || !self.extensible {
            return Err(VmError::InvalidState(
                "cannot append to array literal".to_owned(),
            ));
        }
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
    #[serde(with = "property_pairs")]
    pub props: PropertyTable<JsString, Property>,
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
    /// Combinator element callbacks fulfill without assimilating their argument.
    pub direct: bool,
    /// Only pre-existing executor/thenable capabilities need the orphan guard.
    pub track_unreachable: bool,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionKind {
    Map,
    Set,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionIteration {
    Keys,
    Values,
    Entries,
}

/// Internal primitive data of a wrapper. A wrapper never contains another object,
/// null, or undefined as its payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoxedPrimitive {
    String(JsString),
    Number(#[serde(with = "crate::value::number_serde")] f64),
    Bool(bool),
    Symbol(SymbolId),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Object {
    Promise(PromiseObject),
    Native(NativeObject),
    Plain(PlainObject),
    Array(ArrayObject),
    Boxed {
        primitive: BoxedPrimitive,
        object: PlainObject,
    },
    #[serde(
        serialize_with = "serialize_regexp",
        deserialize_with = "deserialize_regexp"
    )]
    RegExp {
        source: JsString,
        flags: RegExpFlags,
        compiled: regress::Regex,
        object: PlainObject,
    },
    /// A non-callable proxy over an ordinary object, array, or another proxy.
    Proxy {
        target: ObjectId,
        handler: ObjectId,
        object: PlainObject,
    },
    // Deleted slots remain empty so live cursors never shift.
    Map {
        object: PlainObject,
        entries: Vec<Option<(Value, Value)>>,
    },
    Set {
        object: PlainObject,
        elements: Vec<Option<Value>>,
    },
    WeakMap {
        object: PlainObject,
        entries: BTreeMap<ObjectId, Value>,
    },
    CollectionIterator {
        object: PlainObject,
        kind: CollectionKind,
        iteration: CollectionIteration,
        target: Option<ObjectId>,
        next_index: usize,
    },
    ArrayIterator {
        object: PlainObject,
        array: Option<ObjectId>,
        next_index: usize,
    },
    StringIterator {
        object: PlainObject,
        string: JsString,
        /// UTF-16 code-unit offset, never inside a surrogate pair.
        position: usize,
    },
    /// Cursor for `String.prototype.matchAll`. The regexp's `lastIndex` is the resume point.
    RegExpStringIterator {
        object: PlainObject,
        regexp: ObjectId,
        string: JsString,
        global: bool,
        full_unicode: bool,
        done: bool,
    },
    /// Internal cursor: capture own keys per prototype, then recheck a key's
    /// descriptor immediately before visiting it. The cursor has no properties.
    ForInIterator {
        current: Option<ObjectId>,
        pending: Option<VecDeque<JsString>>,
        visited: BTreeSet<JsString>,
        string_indices: Range<usize>,
    },
    Function(FunctionObject),
    Bound(BoundObject),
    Environment(EnvironmentObject),
    Resolution(Resolution),
    PromiseCallback {
        object: PlainObject,
        callback: PromiseCallback,
    },
}

fn serialize_regexp<S: serde::Serializer>(
    source: &JsString,
    flags: &RegExpFlags,
    _: &regress::Regex,
    object: &PlainObject,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    #[derive(Serialize)]
    struct Record<'a> {
        source: &'a JsString,
        flags: String,
        object: &'a PlainObject,
    }

    Record {
        source,
        flags: flags.as_string(),
        object,
    }
    .serialize(serializer)
}

fn deserialize_regexp<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<(JsString, RegExpFlags, regress::Regex, PlainObject), D::Error> {
    #[derive(Deserialize)]
    struct Record {
        source: JsString,
        flags: String,
        object: PlainObject,
    }

    let Record {
        source,
        flags,
        object,
    } = Record::deserialize(deserializer)?;
    let (flags, compiled) = compile_pattern(&source, &flags).map_err(serde::de::Error::custom)?;
    Ok((source, flags, compiled, object))
}

impl PartialEq for Object {
    fn eq(&self, other: &Self) -> bool {
        use Object::*;
        match (self, other) {
            (Promise(a), Promise(b)) => a == b,
            (Native(a), Native(b)) => a == b,
            (Plain(a), Plain(b)) => a == b,
            (Array(a), Array(b)) => a == b,
            (
                Boxed {
                    primitive: a,
                    object: x,
                },
                Boxed {
                    primitive: b,
                    object: y,
                },
            ) => a == b && x == y,
            // The compiled matcher is derived from the source and flags.
            (
                RegExp {
                    source: a,
                    flags: f,
                    object: x,
                    ..
                },
                RegExp {
                    source: b,
                    flags: g,
                    object: y,
                    ..
                },
            ) => a == b && f == g && x == y,
            (
                Proxy {
                    target: a,
                    handler: h,
                    object: x,
                },
                Proxy {
                    target: b,
                    handler: i,
                    object: y,
                },
            ) => a == b && h == i && x == y,
            (
                Map {
                    object: x,
                    entries: a,
                },
                Map {
                    object: y,
                    entries: b,
                },
            ) => x == y && a == b,
            (
                WeakMap {
                    object: x,
                    entries: a,
                },
                WeakMap {
                    object: y,
                    entries: b,
                },
            ) => x == y && a == b,
            (
                Set {
                    object: x,
                    elements: a,
                },
                Set {
                    object: y,
                    elements: b,
                },
            ) => x == y && a == b,
            (
                CollectionIterator {
                    object: x,
                    kind: k,
                    iteration: i,
                    target: a,
                    next_index: n,
                },
                CollectionIterator {
                    object: y,
                    kind: l,
                    iteration: j,
                    target: b,
                    next_index: m,
                },
            ) => x == y && k == l && i == j && a == b && n == m,
            (
                ArrayIterator {
                    object: x,
                    array: a,
                    next_index: n,
                },
                ArrayIterator {
                    object: y,
                    array: b,
                    next_index: m,
                },
            ) => x == y && a == b && n == m,
            (
                StringIterator {
                    object: x,
                    string: a,
                    position: n,
                },
                StringIterator {
                    object: y,
                    string: b,
                    position: m,
                },
            ) => x == y && a == b && n == m,
            (
                RegExpStringIterator {
                    object: x,
                    regexp: a,
                    string: s,
                    global: g,
                    full_unicode: u,
                    done: d,
                },
                RegExpStringIterator {
                    object: y,
                    regexp: b,
                    string: t,
                    global: h,
                    full_unicode: v,
                    done: e,
                },
            ) => x == y && a == b && s == t && g == h && u == v && d == e,
            (
                ForInIterator {
                    current: a,
                    pending: p,
                    visited: v,
                    string_indices: x,
                },
                ForInIterator {
                    current: b,
                    pending: q,
                    visited: w,
                    string_indices: y,
                },
            ) => a == b && p == q && v == w && x == y,
            (Function(a), Function(b)) => a == b,
            (Bound(a), Bound(b)) => a == b,
            (Environment(a), Environment(b)) => a == b,
            (Resolution(a), Resolution(b)) => a == b,
            (
                PromiseCallback {
                    object: x,
                    callback: a,
                },
                PromiseCallback {
                    object: y,
                    callback: b,
                },
            ) => x == y && a == b,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PromiseCallback {
    Executor(Option<(Value, Value)>),
    Finally {
        handler: Value,
        species: Value,
        rejected: bool,
    },
    Restore {
        value: Value,
        rejected: bool,
    },
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
            | Self::Boxed { object: o, .. }
            | Self::RegExp { object: o, .. }
            | Self::Proxy { object: o, .. }
            | Self::Map { object: o, .. }
            | Self::Set { object: o, .. }
            | Self::WeakMap { object: o, .. }
            | Self::CollectionIterator { object: o, .. }
            | Self::ArrayIterator { object: o, .. }
            | Self::StringIterator { object: o, .. }
            | Self::RegExpStringIterator { object: o, .. }
            | Self::Bound(BoundObject { object: o, .. })
            | Self::PromiseCallback { object: o, .. }
            | Self::Resolution(Resolution { object: o, .. }) => &mut o.extensible,
            Self::Array(o) => &mut o.extensible,
            Self::Function(o) => &mut o.extensible,
            Self::Promise(o) => &mut o.extensible,
            Self::Native(NativeObject::Intrinsic { object, .. }) => &mut object.extensible,
            Self::Native(_) | Self::Environment(_) | Self::ForInIterator { .. } => return None,
        })
    }

    pub(crate) fn is_extensible(&self) -> bool {
        match self {
            Self::Plain(o)
            | Self::Boxed { object: o, .. }
            | Self::RegExp { object: o, .. }
            | Self::Proxy { object: o, .. }
            | Self::Map { object: o, .. }
            | Self::Set { object: o, .. }
            | Self::WeakMap { object: o, .. }
            | Self::CollectionIterator { object: o, .. }
            | Self::ArrayIterator { object: o, .. }
            | Self::StringIterator { object: o, .. }
            | Self::RegExpStringIterator { object: o, .. }
            | Self::Bound(BoundObject { object: o, .. })
            | Self::PromiseCallback { object: o, .. }
            | Self::Resolution(Resolution { object: o, .. }) => o.extensible,
            Self::Array(o) => o.extensible,
            Self::Function(o) => o.extensible,
            Self::Promise(o) => o.extensible,
            Self::Native(NativeObject::Intrinsic { object, .. }) => object.extensible,
            // Opaque native handles have no script property storage.
            Self::Native(_) | Self::Environment(_) | Self::ForInIterator { .. } => false,
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

    pub(crate) fn tables(&self) -> Option<PropertyTables<'_>> {
        Some(match self {
            Self::Plain(o)
            | Self::Boxed { object: o, .. }
            | Self::RegExp { object: o, .. }
            | Self::Proxy { object: o, .. }
            | Self::Map { object: o, .. }
            | Self::Set { object: o, .. }
            | Self::WeakMap { object: o, .. }
            | Self::CollectionIterator { object: o, .. }
            | Self::ArrayIterator { object: o, .. }
            | Self::StringIterator { object: o, .. }
            | Self::RegExpStringIterator { object: o, .. }
            | Self::Bound(BoundObject { object: o, .. })
            | Self::PromiseCallback { object: o, .. }
            | Self::Resolution(Resolution { object: o, .. }) => (&o.props, &o.symbol_props),
            Self::Array(o) => (&o.props, &o.symbol_props),
            Self::Function(o) => (&o.props, &o.symbol_props),
            Self::Promise(o) => (&o.props, &o.symbol_props),
            Self::Native(NativeObject::Intrinsic { object, .. }) => {
                (&object.props, &object.symbol_props)
            }
            Self::Native(_) | Self::Environment(_) | Self::ForInIterator { .. } => return None,
        })
    }

    fn tables_mut(&mut self) -> Option<PropertyTablesMut<'_>> {
        Some(match self {
            Self::Plain(o)
            | Self::Boxed { object: o, .. }
            | Self::RegExp { object: o, .. }
            | Self::Proxy { object: o, .. }
            | Self::Map { object: o, .. }
            | Self::Set { object: o, .. }
            | Self::WeakMap { object: o, .. }
            | Self::CollectionIterator { object: o, .. }
            | Self::ArrayIterator { object: o, .. }
            | Self::StringIterator { object: o, .. }
            | Self::RegExpStringIterator { object: o, .. }
            | Self::Bound(BoundObject { object: o, .. })
            | Self::PromiseCallback { object: o, .. }
            | Self::Resolution(Resolution { object: o, .. }) => (&mut o.props, &mut o.symbol_props),
            Self::Array(o) => (&mut o.props, &mut o.symbol_props),
            Self::Function(o) => (&mut o.props, &mut o.symbol_props),
            Self::Promise(o) => (&mut o.props, &mut o.symbol_props),
            Self::Native(NativeObject::Intrinsic { object, .. }) => {
                (&mut object.props, &mut object.symbol_props)
            }
            Self::Native(_) | Self::Environment(_) | Self::ForInIterator { .. } => return None,
        })
    }

    fn boxed_string(&self) -> Option<&JsString> {
        match self {
            Self::Boxed {
                primitive: BoxedPrimitive::String(text),
                ..
            } => Some(text),
            _ => None,
        }
    }

    fn string_own_property(&self, key: PropertyKeyRef<'_>) -> Option<OwnProperty<'static>> {
        let text = self.boxed_string()?;
        if key.is_str("length") {
            return Some(OwnProperty::StringLength(text.unit_len()));
        }
        text.unit_at(canonical_index_key(key)?)
            .map(OwnProperty::StringIndex)
    }

    pub(crate) fn own_property(&self, key: PropertyKeyRef<'_>) -> Option<OwnProperty<'_>> {
        if let Self::Array(array) = self {
            if key.is_str("length") {
                return Some(OwnProperty::ArrayLength(
                    array.length,
                    array.length_writable,
                ));
            }
            if let Some(index) = array_index(key) {
                return array.own_index(index);
            }
        }
        if let Some(property) = self.string_own_property(key) {
            return Some(property);
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
        let string_length = self.boxed_string().map(JsString::unit_len);
        if string_length.is_some() && !enumerable_strings_only {
            keys.push(((1, 0), PropertyKeyRef::String("length")));
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
                    .map(|(index, _)| PropertyKey::String(index.to_string().into()))
            })
            .chain(
                array
                    .filter(|_| !enumerable_strings_only)
                    .map(|_| "length".into()),
            )
            .chain(
                (0..string_length.unwrap_or(0))
                    .map(|index| PropertyKey::String(index.to_string().into())),
            )
            .chain(keys.into_iter().map(|(_, key)| key.to_owned()))
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
                    && array.props.0.keys().all(|key| {
                        let key = PropertyKeyRef::JsString(key);
                        !key.is_str("length") && array_index(key).is_none()
                    })
            }
            Self::Boxed {
                primitive: BoxedPrimitive::String(text),
                object,
            } => {
                let length = text.unit_len();
                object.props.0.keys().all(|key| {
                    let key = PropertyKeyRef::JsString(key);
                    !key.is_str("length")
                        && canonical_index_key(key).is_none_or(|index| index >= length)
                })
            }
            _ => true,
        }
    }

    pub(crate) fn prototype(&self) -> Option<ObjectId> {
        match self {
            Self::Plain(plain)
            | Self::Boxed { object: plain, .. }
            | Self::RegExp { object: plain, .. }
            | Self::Proxy { object: plain, .. }
            | Self::Map { object: plain, .. }
            | Self::Set { object: plain, .. }
            | Self::WeakMap { object: plain, .. }
            | Self::CollectionIterator { object: plain, .. }
            | Self::ArrayIterator { object: plain, .. }
            | Self::StringIterator { object: plain, .. }
            | Self::RegExpStringIterator { object: plain, .. } => plain.prototype,
            Self::Array(array) => array.prototype,
            Self::Function(function) => function.prototype,
            Self::Promise(promise) => promise.prototype,
            Self::Native(NativeObject::Intrinsic { object, .. }) => object.prototype,
            Self::Bound(bound) => bound.object.prototype,
            Self::Resolution(resolution) => resolution.object.prototype,
            Self::PromiseCallback { object, .. } => object.prototype,
            Self::Native(_) | Self::Environment(_) | Self::ForInIterator { .. } => None,
        }
    }

    fn prototype_mut(&mut self) -> Option<&mut Option<ObjectId>> {
        Some(match self {
            Self::Plain(o)
            | Self::Boxed { object: o, .. }
            | Self::RegExp { object: o, .. }
            | Self::Proxy { object: o, .. }
            | Self::Map { object: o, .. }
            | Self::Set { object: o, .. }
            | Self::WeakMap { object: o, .. }
            | Self::CollectionIterator { object: o, .. }
            | Self::ArrayIterator { object: o, .. }
            | Self::StringIterator { object: o, .. }
            | Self::RegExpStringIterator { object: o, .. }
            | Self::Bound(BoundObject { object: o, .. })
            | Self::PromiseCallback { object: o, .. }
            | Self::Resolution(Resolution { object: o, .. }) => &mut o.prototype,
            Self::Array(o) => &mut o.prototype,
            Self::Function(o) => &mut o.prototype,
            Self::Promise(o) => &mut o.prototype,
            Self::Native(NativeObject::Intrinsic { object, .. }) => &mut object.prototype,
            Self::Native(_) | Self::Environment(_) | Self::ForInIterator { .. } => return None,
        })
    }

    /// Replace `out` with this object's outgoing strong references.
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
            Self::Proxy {
                target, handler, ..
            } => {
                out.push(OwnedEdge::Object(*target));
                out.push(OwnedEdge::Object(*handler));
            }
            Self::Promise(promise) => {
                if let PromiseState::Fulfilled(value) | PromiseState::Rejected(value) =
                    &promise.state
                {
                    push_value(out, value);
                }
                if let Some(id) = promise.adopting {
                    out.push(OwnedEdge::Object(id));
                }
                for job in &promise.reactions {
                    job.write_edges(out);
                }
            }
            Self::PromiseCallback { callback, .. } => match callback {
                PromiseCallback::Executor(Some((resolve, reject))) => {
                    push_value(out, resolve);
                    push_value(out, reject);
                }
                PromiseCallback::Finally {
                    handler, species, ..
                } => {
                    push_value(out, handler);
                    push_value(out, species);
                }
                PromiseCallback::Restore { value, .. } => push_value(out, value),
                PromiseCallback::Executor(None) => {}
            },
            Self::Resolution(resolution) => {
                out.push(OwnedEdge::Object(resolution.promise));
                if resolution.pair != 0 {
                    out.push(OwnedEdge::Object(resolution.pair));
                }
            }
            Self::Boxed { primitive, .. } => {
                if let BoxedPrimitive::Symbol(symbol) = primitive {
                    out.push(OwnedEdge::Symbol(*symbol));
                }
            }
            Self::Native(_)
            | Self::Plain(_)
            | Self::RegExp { .. }
            | Self::WeakMap { .. }
            | Self::StringIterator { .. } => {}
            Self::RegExpStringIterator { regexp, .. } => out.push(OwnedEdge::Object(*regexp)),
            Self::Map { entries, .. } => {
                for (key, value) in entries.iter().flatten() {
                    push_value(out, key);
                    push_value(out, value);
                }
            }
            Self::Set { elements, .. } => {
                for value in elements.iter().flatten() {
                    push_value(out, value);
                }
            }
            Self::CollectionIterator { target, .. } => {
                if let Some(id) = target {
                    out.push(OwnedEdge::Object(*id));
                }
            }
            Self::ArrayIterator { array, .. } => {
                if let Some(id) = array {
                    out.push(OwnedEdge::Object(*id));
                }
            }
            Self::ForInIterator { current, .. } => {
                if let Some(id) = current {
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

pub(crate) fn push_value(out: &mut Vec<OwnedEdge>, value: &Value) {
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
                resolve,
                reject,
                on_fulfilled,
                on_rejected,
            } => {
                push_value(out, resolve);
                push_value(out, reject);
                if let Some(value) = on_fulfilled {
                    push_value(out, value);
                }
                if let Some(value) = on_rejected {
                    push_value(out, value);
                }
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
    pub(crate) symbols: BTreeMap<SymbolId, Option<JsString>>,
    /// `Symbol.for` registry. Keys are registry strings; ordinary symbols are
    /// never interned by description.
    pub(crate) symbol_registry: BTreeMap<JsString, SymbolId>,
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
                (ITERATOR_SYMBOL, Some(ITERATOR_DESCRIPTION.into())),
                (TO_PRIMITIVE_SYMBOL, Some(TO_PRIMITIVE_DESCRIPTION.into())),
                (SPECIES_SYMBOL, Some(SPECIES_DESCRIPTION.into())),
                (MATCH_SYMBOL, Some(MATCH_DESCRIPTION.into())),
                (MATCH_ALL_SYMBOL, Some(MATCH_ALL_DESCRIPTION.into())),
                (REPLACE_SYMBOL, Some(REPLACE_DESCRIPTION.into())),
                (SEARCH_SYMBOL, Some(SEARCH_DESCRIPTION.into())),
                (SPLIT_SYMBOL, Some(SPLIT_DESCRIPTION.into())),
            ]),
            symbol_registry: BTreeMap::new(),
            next_symbol_id: 8,
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
        description: Option<JsString>,
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
    pub(crate) fn intern_symbol(&mut self, key: JsString) -> Result<SymbolId, VmError> {
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
        symbols: BTreeMap<SymbolId, Option<JsString>>,
        registry: BTreeMap<JsString, SymbolId>,
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

    pub(crate) fn alloc_for_in_iterator(&mut self, source: &Value) -> ObjectId {
        let current = match source {
            Value::Undefined | Value::Null => None,
            Value::Object(id) => Some(*id),
            // The primitive wrapper prototypes do not exist in this subset.
            _ => self.object_prototype,
        };
        let string_length = match source {
            Value::String(string) => string.unit_len(),
            _ => 0,
        };
        self.alloc(Object::ForInIterator {
            current,
            pending: None,
            visited: BTreeSet::new(),
            string_indices: 0..string_length,
        })
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

    pub(crate) fn alloc_intrinsic(&mut self, intrinsic: crate::intrinsic::IntrinsicId) -> ObjectId {
        self.alloc(Object::Native(NativeObject::Intrinsic {
            intrinsic,
            object: PlainObject::new(self.function_prototype),
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
                Object::Resolution(resolution)
                    if resolution.track_unreachable && !resolution.called =>
                {
                    Some(*id)
                }
                _ => None,
            })
            .collect();
    }

    pub(crate) fn contains(&self, id: ObjectId) -> bool {
        self.objects.contains_key(&id)
    }

    pub(crate) fn object_count(&self) -> usize {
        self.objects.len()
    }

    pub(crate) fn get(&self, id: ObjectId) -> Result<&Object, VmError> {
        self.objects.get(&id).ok_or_else(|| missing_object(id))
    }

    /// The ultimate target for operations with no Proxy trap.
    pub(crate) fn proxy_target(&self, mut id: ObjectId) -> Result<ObjectId, VmError> {
        let mut remaining = self.objects.len();
        while let Object::Proxy { target, .. } = self.get(id)? {
            if remaining == 0 {
                return Err(VmError::InvalidState(format!(
                    "proxy target chain at object {id} is cyclic"
                )));
            }
            remaining -= 1;
            id = *target;
        }
        Ok(id)
    }

    /// Read an internal prototype with no Proxy trap.
    pub(crate) fn prototype_of(&self, id: ObjectId) -> Result<Option<ObjectId>, VmError> {
        Ok(self.get(self.proxy_target(id)?)?.prototype())
    }

    pub(crate) fn get_mut(&mut self, id: ObjectId) -> Result<&mut Object, VmError> {
        self.objects.get_mut(&id).ok_or_else(|| missing_object(id))
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

    pub(crate) fn intern_native(
        &mut self,
        target: crate::binding::NativeTarget,
        constructable: bool,
    ) -> ObjectId {
        if let Some(id) = self.objects.iter().find_map(|(id, object)| match object {
            Object::Native(NativeObject::Host {
                target: current,
                constructable: flag,
            }) if *current == target && *flag == constructable => Some(*id),
            _ => None,
        }) {
            return id;
        }
        self.alloc(Object::Native(NativeObject::Host {
            target,
            constructable,
        }))
    }

    pub(crate) fn set_prototype(
        &mut self,
        id: ObjectId,
        prototype: Option<ObjectId>,
    ) -> Result<bool, VmError> {
        let id = self.proxy_target(id)?;
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
            let ancestor = self.proxy_target(ancestor)?;
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
        if let Object::Array(array) = &mut *object {
            if key.is_str("length") {
                return Ok(false);
            }
            if let Some(index) = array_index(key) {
                if let Entry::Occupied(entry) = array.elements.entry(index) {
                    if !entry.get().1.configurable {
                        return Ok(false);
                    }
                    entry.remove();
                }
                return Ok(true);
            }
        }
        if object.string_own_property(key).is_some() {
            return Ok(false);
        }
        let Some((props, symbols)) = object.tables_mut() else {
            // Opaque objects expose no own script properties.
            return Ok(true);
        };
        Ok(match key {
            PropertyKeyRef::String(key) => props.remove_configurable(&JsString::from(key)),
            PropertyKeyRef::JsString(key) => props.remove_configurable(key),
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
        self.set_own(id, key.into(), value)
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
        if let Object::Array(array) = object
            && !array.length_writable
            && array_index(key.borrowed()).is_some_and(|index| index >= array.length)
        {
            return Ok(false);
        }
        let current = object.own_property(key.borrowed());
        if let Some(property @ (OwnProperty::StringLength(_) | OwnProperty::StringIndex(_))) =
            current
        {
            return Ok(update.allows_fixed_string(property));
        }
        let Some((property, attrs)) = update.complete(object.is_extensible(), current) else {
            return Ok(false);
        };
        if key.borrowed().is_str("length")
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
        if object.string_own_property(key.borrowed()).is_some() {
            return Err(VmError::InvalidState(format!(
                "object {id} has a fixed virtual string property"
            )));
        }
        if let Object::Array(array) = &mut *object
            && let Some(index) = array_index(key.borrowed())
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
            | Object::Map { object: plain, .. }
            | Object::Set { object: plain, .. }
            | Object::WeakMap { object: plain, .. }
            | Object::CollectionIterator { object: plain, .. }
            | Object::ArrayIterator { object: plain, .. }
            | Object::StringIterator { object: plain, .. }
            | Object::RegExpStringIterator { object: plain, .. } => plain,
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
    props: &'m PropertyTable<JsString, V>,
    symbol_props: &'m PropertyTable<SymbolId, V>,
    key: PropertyKeyRef<'_>,
) -> Option<&'m OrderedProperty<V>> {
    match key {
        PropertyKeyRef::String(key) => props.entry(&JsString::from(key)),
        PropertyKeyRef::JsString(key) => props.entry(key),
        PropertyKeyRef::Symbol(id) => symbol_props.entry(&id),
    }
}

fn insert_table<V>(
    props: &mut PropertyTable<JsString, V>,
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
