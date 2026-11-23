use std::borrow::Borrow;
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ops::Range;

use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::object::ReactionJob;
use crate::builtin::string::utf16_unit_string;
use crate::error::VmError;
use crate::ops::canonical_index_key;
use crate::regexp::{RegExpFlags, compile_pattern};
use crate::value::{
    BindingId, JsBigInt, JsString, ObjectId, PropertyKey, PropertyKeyRef, SymbolId, Value, ValueRef,
};

/// Reserved well-known symbols. Ordinary symbols allocate after these IDs and
/// are never interned by description; only the registry interns by key.
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
pub(crate) const HAS_INSTANCE_SYMBOL: SymbolId = 8;
pub(crate) const HAS_INSTANCE_DESCRIPTION: &str = "Symbol.hasInstance";
pub(crate) const IS_CONCAT_SPREADABLE_SYMBOL: SymbolId = 9;
pub(crate) const IS_CONCAT_SPREADABLE_DESCRIPTION: &str = "Symbol.isConcatSpreadable";
pub(crate) const ASYNC_ITERATOR_SYMBOL: SymbolId = 10;
pub(crate) const ASYNC_ITERATOR_DESCRIPTION: &str = "Symbol.asyncIterator";
pub(crate) const TO_STRING_TAG_SYMBOL: SymbolId = 11;
pub(crate) const TO_STRING_TAG_DESCRIPTION: &str = "Symbol.toStringTag";
pub(crate) const DISPOSE_SYMBOL: SymbolId = 12;
pub(crate) const DISPOSE_DESCRIPTION: &str = "Symbol.dispose";
pub(crate) const ASYNC_DISPOSE_SYMBOL: SymbolId = 13;
pub(crate) const ASYNC_DISPOSE_DESCRIPTION: &str = "Symbol.asyncDispose";
pub(crate) const UNSCOPABLES_SYMBOL: SymbolId = 14;
pub(crate) const UNSCOPABLES_DESCRIPTION: &str = "Symbol.unscopables";
/// Bound for ordinary symbols. Every reserved well-known symbol stays below it.
pub(crate) const NEXT_SYMBOL_ID: SymbolId = UNSCOPABLES_SYMBOL + 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromiseState {
    Pending,
    Fulfilled(Value),
    Rejected(Value),
}

/// Reaction observation and host notification state, separate from settlement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectionStatus {
    Unobserved,
    Handled,
    PendingReport,
    Reported,
    PendingHandled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CombineKind {
    All,
    AllSettled,
    Race,
    Any,
}

/// Shared element state keyed by an internal object, not the constructor result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CombineState {
    pub kind: CombineKind,
    pub remaining: usize,
    pub results: Vec<Value>,
    pub called: Vec<bool>,
    pub capability: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromiseObject {
    pub state: PromiseState,
    pub rejection_status: RejectionStatus,
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
    pub(super) extensible: bool,
    /// Private disposal state driving this pending async stack result.
    pub disposal: Option<ObjectId>,
    /// Keeps a pending stream operation alive when only its result is reachable.
    pub stream_dependency: Option<ObjectId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposeHint {
    Sync,
    Async,
    AsyncFromSync,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableResource {
    Method {
        value: Value,
        method: Value,
        hint: DisposeHint,
    },
    Adopt {
        value: Value,
        method: Value,
        hint: DisposeHint,
    },
    Defer {
        method: Value,
        hint: DisposeHint,
    },
    AsyncEmpty,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DisposeState {
    pub records: Vec<DisposableResource>,
    /// The pending throw completion; `Some(undefined)` is still a throw.
    pub error: Option<Value>,
    pub needs_await: bool,
    pub has_awaited: bool,
    /// The async stack result, or `None` for a compiler-owned scope.
    pub result: Option<ObjectId>,
    /// The pending source promise carrying the reciprocal `DisposeNext` reaction.
    pub waiting: Option<ObjectId>,
}

/// Where a generator currently is.
///
/// `Executing` and the two waiting states are transient frames of one call and
/// never survive a snapshot boundary unchanged: while `Awaiting` the frame is
/// in a continuation, and while `Closing` the return argument is in a Promise
/// reaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeneratorStatus {
    /// Parameters are initialized; the body has not run yet.
    Start,
    /// Suspended at generator entry `index` of the function's resume table.
    Suspended(u32),
    /// An async generator's body suspended at `await`. The frame lives in the
    /// single continuation that targets this generator.
    Awaiting,
    /// The body cannot run any more and the queue head's `return` argument is
    /// being assimilated before the request settles with `done: true`.
    Closing,
    /// A resume is running. Reentrant resumes must throw a script TypeError.
    Executing,
    /// The body finished, threw, or was closed. The frame is gone.
    Completed,
}

/// The completion kind an async generator request carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AsyncCompletion {
    Next,
    Throw,
    Return,
}

/// One queued async generator request. The request Promise is settled exactly
/// once, by the step that consumes the request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AsyncRequest {
    pub completion: AsyncCompletion,
    pub argument: Value,
    pub promise: ObjectId,
}

/// Script-visible generator state. The object itself is an ordinary object with
/// a plain property table and prototype; this record holds the suspended frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeneratorObject {
    pub func_id: u32,
    pub status: GeneratorStatus,
    /// Invocation environment held by the suspended frame.
    pub env: Option<ObjectId>,
    /// Values in the resume entry's `saved_slots` order. Empty once completed.
    pub values: Vec<Value>,
    /// Async generators only: FIFO of requests, head first. Synchronous
    /// generators never push here.
    pub requests: VecDeque<AsyncRequest>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum NativeObject {
    Host {
        target: crate::binding::NativeTarget,
        constructable: bool,
        object: PlainObject,
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
/// Small tables keep a sorted vector and skip the tree; large tables use a
/// `BTreeMap`. `layout` changes only when a key is inserted or removed, so a
/// property site can cache a small-table slot and still observe value updates.
#[derive(Debug, Clone)]
pub struct PropertyTable<K: Ord, V> {
    storage: PropertyStorage<K, V>,
    layout: u64,
}

#[derive(Debug, Clone)]
enum PropertyStorage<K, V> {
    Small(Vec<(K, OrderedProperty<V>)>),
    Large(BTreeMap<K, OrderedProperty<V>>),
}

const SMALL_TABLE_LIMIT: usize = 8;

impl<K: Ord, V: PartialEq> PartialEq for PropertyTable<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len()
            && self.ordered_iter().zip(other.ordered_iter()).all(
                |((left_key, left), (right_key, right))| left_key == right_key && left == right,
            )
    }
}

pub(super) type PropertyTables<'a> = (
    &'a PropertyTable<JsString, Property>,
    &'a PropertyTable<SymbolId, Property>,
);
pub(super) type PropertyTablesMut<'a> = (
    &'a mut PropertyTable<JsString, Property>,
    &'a mut PropertyTable<SymbolId, Property>,
);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct OrderedProperty<V> {
    ordinal: usize,
    pub(super) value: V,
    pub(super) attributes: Attributes,
}

pub(super) fn array_index(key: PropertyKeyRef<'_>) -> Option<usize> {
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
    pub(crate) const FUNCTION_METADATA: Self = Self {
        configurable: true,
        ..Self::FIXED
    };
    pub const FUNCTION_PROTOTYPE: Self = Self {
        writable: true,
        ..Self::FIXED
    };
    pub(super) const ACCESSOR: Self = Self {
        writable: false,
        ..Self::ORDINARY
    };
}

pub(super) enum PropertyIter<'a, K: Ord, V> {
    Small(std::slice::Iter<'a, (K, OrderedProperty<V>)>),
    Large(std::collections::btree_map::Iter<'a, K, OrderedProperty<V>>),
}

impl<'a, K: Ord, V> Iterator for PropertyIter<'a, K, V> {
    type Item = (&'a K, &'a OrderedProperty<V>);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Small(iter) => iter.next().map(|(key, property)| (key, property)),
            Self::Large(iter) => iter.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::Small(iter) => iter.size_hint(),
            Self::Large(iter) => iter.size_hint(),
        }
    }
}

mod property_pairs {
    use super::{Property, PropertyTable};
    use crate::value::JsString;

    pub(super) fn serialize<S: serde::Serializer>(
        table: &PropertyTable<JsString, Property>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(table.ordered_iter())
    }

    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<PropertyTable<JsString, Property>, D::Error> {
        crate::snapshot::ordered_pairs::deserialize(deserializer)
            .map(PropertyTable::from_ordered_pairs)
    }
}

impl<K: Ord + Serialize, V: Serialize> Serialize for PropertyTable<K, V> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.ordered_iter())
    }
}

impl<'de, K: Ord + Deserialize<'de>, V: Deserialize<'de>> Deserialize<'de> for PropertyTable<K, V> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        BTreeMap::deserialize(deserializer).map(Self::from_ordered_pairs)
    }
}

impl<K: Ord, V> PropertyTable<K, V> {
    pub(super) fn new() -> Self {
        Self {
            storage: PropertyStorage::Small(Vec::new()),
            layout: 0,
        }
    }

    fn from_ordered_pairs(pairs: BTreeMap<K, OrderedProperty<V>>) -> Self {
        if pairs.len() <= SMALL_TABLE_LIMIT {
            Self {
                storage: PropertyStorage::Small(pairs.into_iter().collect()),
                layout: 0,
            }
        } else {
            Self {
                storage: PropertyStorage::Large(pairs),
                layout: 0,
            }
        }
    }

    fn len(&self) -> usize {
        match &self.storage {
            PropertyStorage::Small(entries) => entries.len(),
            PropertyStorage::Large(index) => index.len(),
        }
    }

    pub fn get<Q: Ord + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        self.entry(key).map(|property| &property.value)
    }

    pub(super) fn entry<Q: Ord + ?Sized>(&self, key: &Q) -> Option<&OrderedProperty<V>>
    where
        K: Borrow<Q>,
    {
        match &self.storage {
            PropertyStorage::Small(entries) => entries
                .binary_search_by(|(stored, _)| stored.borrow().cmp(key))
                .ok()
                .map(|index| &entries[index].1),
            PropertyStorage::Large(index) => index.get(key),
        }
    }

    pub fn get_mut<Q: Ord + ?Sized>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
    {
        match &mut self.storage {
            PropertyStorage::Small(entries) => entries
                .binary_search_by(|(stored, _)| stored.borrow().cmp(key))
                .ok()
                .map(move |index| &mut entries[index].1.value),
            PropertyStorage::Large(index) => index.get_mut(key).map(|property| &mut property.value),
        }
    }

    /// Storage order, for internal traversal rather than script enumeration.
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.ordered_iter()
            .map(|(key, property)| (key, &property.value))
    }

    pub(super) fn ordered_iter(&self) -> PropertyIter<'_, K, V> {
        match &self.storage {
            PropertyStorage::Small(entries) => PropertyIter::Small(entries.iter()),
            PropertyStorage::Large(index) => PropertyIter::Large(index.iter()),
        }
    }

    /// Changes only when a key is inserted or removed, never when a value or
    /// attribute is updated. A cache hit still reads the current entry.
    pub(super) fn layout(&self) -> u64 {
        self.layout
    }

    pub(super) fn slot<Q: Ord + ?Sized>(&self, key: &Q) -> Option<usize>
    where
        K: Borrow<Q>,
    {
        match &self.storage {
            PropertyStorage::Small(entries) => entries
                .binary_search_by(|(stored, _)| stored.borrow().cmp(key))
                .ok(),
            PropertyStorage::Large(_) => None,
        }
    }

    pub(super) fn entry_at(&self, slot: usize) -> Option<&OrderedProperty<V>> {
        match &self.storage {
            PropertyStorage::Small(entries) => entries.get(slot).map(|(_, property)| property),
            PropertyStorage::Large(_) => None,
        }
    }

    pub(super) fn entry_at_mut(&mut self, slot: usize) -> Option<&mut OrderedProperty<V>> {
        match &mut self.storage {
            PropertyStorage::Small(entries) => entries.get_mut(slot).map(|(_, property)| property),
            PropertyStorage::Large(_) => None,
        }
    }

    pub(super) fn update(
        &mut self,
        key: K,
        attributes: Option<Attributes>,
        value: impl FnOnce(Option<&V>) -> V,
    ) {
        match &mut self.storage {
            PropertyStorage::Small(entries) => {
                let position = entries.binary_search_by(|(stored, _)| stored.cmp(&key));
                match position {
                    Ok(index) => {
                        let property = &mut entries[index].1;
                        property.value = value(Some(&property.value));
                        if let Some(attributes) = attributes {
                            property.attributes = attributes;
                        }
                    }
                    Err(index) => {
                        let ordinal = entries.len();
                        entries.insert(
                            index,
                            (
                                key,
                                OrderedProperty {
                                    ordinal,
                                    value: value(None),
                                    attributes: attributes.unwrap_or(Attributes::ORDINARY),
                                },
                            ),
                        );
                        self.layout = self.layout.wrapping_add(1);
                        if entries.len() > SMALL_TABLE_LIMIT {
                            self.promote();
                        }
                    }
                }
            }
            PropertyStorage::Large(index) => {
                let ordinal = index.len();
                match index.entry(key) {
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
                        self.layout = self.layout.wrapping_add(1);
                    }
                }
            }
        }
    }

    fn promote(&mut self) {
        let storage = std::mem::replace(&mut self.storage, PropertyStorage::Small(Vec::new()));
        if let PropertyStorage::Small(entries) = storage {
            self.storage = PropertyStorage::Large(entries.into_iter().collect());
        }
    }

    pub(super) fn remove_configurable<Q: Ord + ?Sized>(&mut self, key: &Q) -> bool
    where
        K: Borrow<Q>,
    {
        match &mut self.storage {
            PropertyStorage::Small(entries) => {
                let Ok(index) = entries.binary_search_by(|(stored, _)| stored.borrow().cmp(key))
                else {
                    return true;
                };
                if !entries[index].1.attributes.configurable {
                    return false;
                }
                let ordinal = entries[index].1.ordinal;
                entries.remove(index);
                if ordinal < entries.len() {
                    for (_, property) in entries.iter_mut() {
                        if property.ordinal > ordinal {
                            property.ordinal -= 1;
                        }
                    }
                }
            }
            PropertyStorage::Large(index) => {
                let Some(property) = index.get(key) else {
                    return true;
                };
                if !property.attributes.configurable {
                    return false;
                }
                let ordinal = property.ordinal;
                index.remove(key);
                if ordinal < index.len() {
                    for property in index.values_mut() {
                        if property.ordinal > ordinal {
                            property.ordinal -= 1;
                        }
                    }
                }
            }
        }
        self.layout = self.layout.wrapping_add(1);
        true
    }

    pub(super) fn valid_order(&self) -> bool {
        let mut seen = vec![false; self.len()];
        self.ordered_iter().all(|(_, property)| {
            seen.get_mut(property.ordinal)
                .is_some_and(|seen| !std::mem::replace(seen, true))
        })
    }
}

impl<K: Ord> PropertyTable<K, Property> {
    pub(super) fn restrict(&mut self, level: IntegrityLevel) {
        match &mut self.storage {
            PropertyStorage::Small(entries) => {
                for (_, entry) in entries {
                    level.restrict(&entry.value, &mut entry.attributes);
                }
            }
            PropertyStorage::Large(index) => {
                for entry in index.values_mut() {
                    level.restrict(&entry.value, &mut entry.attributes);
                }
            }
        }
    }

    pub(super) fn satisfies(&self, level: IntegrityLevel) -> bool {
        self.ordered_iter()
            .all(|(_, entry)| level.allows(&entry.value, entry.attributes))
    }
}

pub(super) fn collect_own_keys<'a, V>(
    keys: &mut Vec<((u8, usize), PropertyKeyRef<'a>)>,
    props: &'a PropertyTable<JsString, V>,
    symbols: &'a PropertyTable<SymbolId, V>,
    enumerable_strings_only: bool,
) {
    if !enumerable_strings_only {
        keys.reserve(props.len() + symbols.len());
    }
    keys.extend(
        props
            .ordered_iter()
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
                .ordered_iter()
                .map(|(id, property)| ((3, property.ordinal), PropertyKeyRef::Symbol(*id))),
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
        let computed_property;
        let (old, attrs) = match current {
            Some(OwnProperty::Stored(property, attrs)) => (Some(property), attrs),
            Some(OwnProperty::Computed(property, attrs)) => {
                computed_property = property;
                (Some(&computed_property), attrs)
            }
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
                _ => &Value::UNDEFINED,
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

    pub(super) fn allows_fixed_string(&self, property: OwnProperty<'_>) -> bool {
        !self.is_accessor()
            && self.writable != Some(true)
            && self.configurable != Some(true)
            && self
                .enumerable
                .is_none_or(|flag| flag == property.attributes().enumerable)
            && self
                .value
                .as_ref()
                .is_none_or(|value| match (property, value.view()) {
                    (OwnProperty::StringLength(length), ValueRef::Number(_)) => {
                        crate::ops::same_value(&Value::number(length as f64), value)
                    }
                    (OwnProperty::StringIndex(unit), ValueRef::String(text)) => {
                        text.unit_len() == 1 && text.unit_at(0) == Some(unit)
                    }
                    _ => false,
                })
    }
}

pub(crate) enum OwnProperty<'a> {
    Stored(&'a Property, Attributes),
    /// A property computed from another object's state, such as a TypedArray
    /// element read from its buffer. It is materialized before it is returned
    /// so no view of the bytes outlives the lookup.
    Computed(Property, Attributes),
    ArrayLength(usize, bool),
    StringLength(usize),
    StringIndex(u16),
}

impl OwnProperty<'_> {
    pub(crate) fn into_owned(self) -> Property {
        match self {
            Self::Computed(property, _) => property,
            Self::Stored(property, _) => property.clone(),
            Self::ArrayLength(length, _) => Property::Data(Value::number(length as f64)),
            Self::StringLength(length) => Property::Data(Value::number(length as f64)),
            Self::StringIndex(unit) => Property::Data(Value::string(utf16_unit_string(unit))),
        }
    }

    pub(crate) fn data_value(&self) -> Option<&Value> {
        match self {
            Self::Stored(Property::Data(value), _) => Some(value),
            Self::Computed(Property::Data(value), _) => Some(value),
            Self::Stored(_, _) | Self::Computed(_, _) => None,
            _ => None,
        }
    }

    pub(crate) fn attributes(&self) -> Attributes {
        match self {
            Self::Stored(_, attributes) | Self::Computed(_, attributes) => *attributes,
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
            Self::Stored(Property::Accessor { set, .. }, _)
            | Self::Computed(Property::Accessor { set, .. }, _) => Some(*set),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PlainObject {
    #[serde(with = "property_pairs")]
    pub props: PropertyTable<JsString, Property>,
    pub symbol_props: PropertyTable<SymbolId, Property>,
    /// Internal prototype.
    pub prototype: Option<ObjectId>,
    pub(super) extensible: bool,
    pub(crate) is_error: bool,
    #[serde(default)]
    pub(crate) module_namespace: Option<u32>,
    #[serde(default)]
    pub(crate) is_raw_json: bool,
}

impl Serialize for PlainObject {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // JSON omits None; positional formats always need the option tag.
        let has_namespace = !serializer.is_human_readable() || self.module_namespace.is_some();
        let has_raw_json = !serializer.is_human_readable() || self.is_raw_json;
        let mut record = serializer.serialize_struct(
            "PlainObject",
            5 + usize::from(has_namespace) + usize::from(has_raw_json),
        )?;
        record.serialize_field("props", &PropertyPairs(&self.props))?;
        record.serialize_field("symbol_props", &self.symbol_props)?;
        record.serialize_field("prototype", &self.prototype)?;
        record.serialize_field("extensible", &self.extensible)?;
        record.serialize_field("is_error", &self.is_error)?;
        if has_namespace {
            record.serialize_field("module_namespace", &self.module_namespace)?;
        }
        if has_raw_json {
            record.serialize_field("is_raw_json", &self.is_raw_json)?;
        }
        record.end()
    }
}

struct PropertyPairs<'a>(&'a PropertyTable<JsString, Property>);

impl Serialize for PropertyPairs<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        property_pairs::serialize(self.0, serializer)
    }
}

impl PlainObject {
    pub(crate) fn new(prototype: Option<ObjectId>) -> Self {
        Self {
            props: PropertyTable::new(),
            symbol_props: PropertyTable::new(),
            prototype,
            extensible: true,
            is_error: false,
            module_namespace: None,
            is_raw_json: false,
        }
    }

    pub(crate) fn callable(prototype: Option<ObjectId>, length: f64, name: JsString) -> Self {
        let mut object = Self::new(prototype);
        object
            .props
            .update("length".into(), Some(Attributes::FUNCTION_METADATA), |_| {
                Property::Data(Value::number(length))
            });
        object
            .props
            .update("name".into(), Some(Attributes::FUNCTION_METADATA), |_| {
                Property::Data(Value::string(name))
            });
        object
    }
}

/// Binary element type of one TypedArray. Element width, numeric domain, and
/// clamping are derived from it; a view never reinterprets its own kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElementKind {
    Int8,
    Uint8,
    Uint8Clamped,
    Int16,
    Uint16,
    Int32,
    Uint32,
    Float16,
    Float32,
    Float64,
    BigInt64,
    BigUint64,
}

impl ElementKind {
    pub(crate) const fn bytes(self) -> usize {
        match self {
            Self::Int8 | Self::Uint8 | Self::Uint8Clamped => 1,
            Self::Int16 | Self::Uint16 | Self::Float16 => 2,
            Self::Int32 | Self::Uint32 | Self::Float32 => 4,
            Self::Float64 | Self::BigInt64 | Self::BigUint64 => 8,
        }
    }

    pub(crate) const fn is_bigint(self) -> bool {
        matches!(self, Self::BigInt64 | Self::BigUint64)
    }

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Int8 => "Int8Array",
            Self::Uint8 => "Uint8Array",
            Self::Uint8Clamped => "Uint8ClampedArray",
            Self::Int16 => "Int16Array",
            Self::Uint16 => "Uint16Array",
            Self::Int32 => "Int32Array",
            Self::Uint32 => "Uint32Array",
            Self::Float16 => "Float16Array",
            Self::Float32 => "Float32Array",
            Self::Float64 => "Float64Array",
            Self::BigInt64 => "BigInt64Array",
            Self::BigUint64 => "BigUint64Array",
        }
    }
}

/// A byte buffer. `bytes` is the single owner of the bytes: views name this
/// object and never hold a copy, so aliasing survives restore unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArrayBufferObject {
    pub object: PlainObject,
    /// `None` means detached. A detached buffer keeps its resizable flag.
    #[serde(serialize_with = "serialize_buffer_bytes")]
    pub bytes: Option<Vec<u8>>,
    /// `Some` for a resizable buffer; the byte length may grow up to it.
    pub max_byte_length: Option<usize>,
}

fn serialize_buffer_bytes<S: serde::Serializer>(
    bytes: &Option<Vec<u8>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    if serializer.is_human_readable() {
        return bytes.serialize(serializer);
    }

    // Bincode encodes both a byte slice and a sequence of u8 as the same
    // length prefix and raw bytes. Only the byte slice uses one bulk write.
    struct ByteSlice<'a>(&'a [u8]);
    impl Serialize for ByteSlice<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.serialize_bytes(self.0)
        }
    }

    bytes.as_deref().map(ByteSlice).serialize(serializer)
}

impl ArrayBufferObject {
    pub(crate) fn is_detached(&self) -> bool {
        self.bytes.is_none()
    }

    pub(crate) fn byte_length(&self) -> Option<usize> {
        self.bytes.as_ref().map(Vec::len)
    }

    pub(crate) fn is_resizable(&self) -> bool {
        self.max_byte_length.is_some()
    }
}

/// One view over an `ArrayBuffer`. `byte_length == None` tracks the buffer's
/// byte length, which only a resizable buffer allows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypedArrayObject {
    pub object: PlainObject,
    pub buffer: ObjectId,
    pub byte_offset: usize,
    pub byte_length: Option<usize>,
    pub kind: ElementKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataViewObject {
    pub object: PlainObject,
    pub buffer: ObjectId,
    pub byte_offset: usize,
    pub byte_length: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

impl TextEncoding {
    pub(crate) fn utf16_unit(self, bytes: [u8; 2]) -> u16 {
        match self {
            Self::Utf16Le => u16::from_le_bytes(bytes),
            Self::Utf16Be => u16::from_be_bytes(bytes),
            Self::Utf8 => unreachable!("not a UTF-16 encoding"),
        }
    }
}

/// A `TextDecoder` instance: the ordinary object header plus the decoder
/// state that survives a snapshot boundary.
///
/// `prefix` holds the trailing bytes of an incomplete UTF-8 or UTF-16 scalar;
/// `pending` holds the bytes after a consumed fatal error while the stream is
/// still open. A non-streaming call always clears both.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextDecoderObject {
    pub object: PlainObject,
    pub encoding: TextEncoding,
    pub fatal: bool,
    pub ignore_bom: bool,
    /// The `stream` flag of the most recent `decode` call.
    pub streaming: bool,
    pub bom_seen: bool,
    pub prefix: [u8; 3],
    pub prefix_len: u8,
    pub pending: Vec<u8>,
}

/// Current byte extent of one view, resolved against the live buffer.
///
/// `Detached` and `OutOfBounds` are distinct: a detached buffer makes every
/// access throw, while a fixed view that a resize pushed out of range stays a
/// legal record that currently reads and writes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ViewRange {
    Detached,
    OutOfBounds,
    Bytes { start: usize, len: usize },
}

impl ViewRange {
    pub(crate) fn len(self) -> usize {
        match self {
            Self::Bytes { len, .. } => len,
            Self::Detached | Self::OutOfBounds => 0,
        }
    }
}

/// Resolve a view against its buffer bytes. Tracking views round down to whole
/// elements; a legal zero-length result is not the same as an out-of-range view.
pub(crate) fn view_range(
    bytes: Option<&[u8]>,
    byte_offset: usize,
    byte_length: Option<usize>,
    element_size: usize,
) -> ViewRange {
    let Some(bytes) = bytes else {
        return ViewRange::Detached;
    };
    let available = match bytes.len().checked_sub(byte_offset) {
        Some(available) => available,
        None => return ViewRange::OutOfBounds,
    };
    let len = match byte_length {
        Some(len) => {
            if len > available {
                return ViewRange::OutOfBounds;
            }
            len
        }
        None => available / element_size * element_size,
    };
    ViewRange::Bytes {
        start: byte_offset,
        len,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArrayObject {
    pub(super) elements: BTreeMap<usize, (Property, Attributes)>,
    pub(crate) length: usize,
    pub(crate) length_writable: bool,
    /// Own properties other than `length` and canonical indices.
    #[serde(with = "property_pairs")]
    pub props: PropertyTable<JsString, Property>,
    pub symbol_props: PropertyTable<SymbolId, Property>,
    pub prototype: Option<ObjectId>,
    pub(super) extensible: bool,
}

impl ArrayObject {
    pub(crate) fn own_index(&self, index: usize) -> Option<OwnProperty<'_>> {
        if crate::perf::ENABLED {
            crate::perf::record(|counters| counters.property_lookups += 1);
        }
        self.elements
            .get(&index)
            .map(|(property, attributes)| OwnProperty::Stored(property, *attributes))
    }

    /// Writes an existing writable data element. Missing, accessor, and
    /// non-writable elements are left for the ordinary assignment path.
    pub(crate) fn set_existing_index(&mut self, index: usize, value: &Value) -> bool {
        match self.elements.get_mut(&index) {
            Some((Property::Data(slot), attributes)) if attributes.writable => {
                *slot = value.clone();
                true
            }
            _ => false,
        }
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
    pub(super) fn set_length(&mut self, length: usize, writable: bool) -> bool {
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
    pub(crate) home: Option<ObjectId>,
    pub(crate) dynamic_name: Option<JsString>,
    pub(crate) class: Option<ScriptClass>,
    #[serde(with = "property_pairs")]
    pub props: PropertyTable<JsString, Property>,
    pub symbol_props: PropertyTable<SymbolId, Property>,
    /// Internal prototype. Not the constructable `.prototype` property.
    pub prototype: Option<ObjectId>,
    pub(super) extensible: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ClassRecord<S> {
    pub prototype: ObjectId,
    pub source: S,
}

pub(crate) type ScriptClass = ClassRecord<ScriptClassSource>;
pub(crate) type HostClassRecord = ClassRecord<usize>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ScriptClassSource {
    pub derived: bool,
    pub default_constructor: bool,
    pub elements: Vec<ClassElement>,
    pub private_count: u32,
    pub private_binding: Option<BindingId>,
    pub private: BTreeMap<u32, PrivateMember>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct PrivateMember {
    pub is_static: bool,
    pub kind: PrivateMemberKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) enum PrivateMemberKind {
    Field,
    Method(ObjectId),
    Accessor {
        get: Option<ObjectId>,
        set: Option<ObjectId>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum PrivateElement {
    Field(Value),
    Installed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum ClassElement {
    Field {
        key: PropertyKey,
        initializer: Option<ObjectId>,
        is_static: bool,
    },
    PrivateField {
        index: u32,
        initializer: Option<ObjectId>,
    },
    Block {
        body: ObjectId,
    },
}

impl ClassElement {
    pub(crate) fn closure(&self) -> Option<ObjectId> {
        match self {
            Self::Field { initializer, .. } | Self::PrivateField { initializer, .. } => {
                *initializer
            }
            Self::Block { body } => Some(*body),
        }
    }
}

/// One invocation environment. Bindings are addressed by compile-time IDs, so
/// shadowed names and closures share mutable cells without name lookup.
/// Environments are not script objects and cannot be prototypes.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct EnvironmentObject {
    pub parent: Option<ObjectId>,
    pub bindings: BTreeMap<BindingId, Value>,
    #[serde(default)]
    pub module: Option<u32>,
    #[serde(skip)]
    pub(crate) uninitialized: BTreeSet<BindingId>,
    #[serde(default)]
    pub(crate) module_getter_sentinel: bool,
}

impl Serialize for EnvironmentObject {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let has_module = !serializer.is_human_readable() || self.module.is_some();
        let mut record =
            serializer.serialize_struct("EnvironmentObject", 3 + usize::from(has_module))?;
        record.serialize_field("parent", &self.parent)?;
        record.serialize_field("bindings", &self.bindings)?;
        if has_module {
            record.serialize_field("module", &self.module)?;
        }
        record.serialize_field("module_getter_sentinel", &self.module_getter_sentinel)?;
        record.end()
    }
}

impl EnvironmentObject {
    pub fn new(parent: Option<ObjectId>, bindings: BTreeMap<BindingId, Value>) -> Self {
        Self {
            parent,
            bindings,
            module: None,
            uninitialized: BTreeSet::new(),
            module_getter_sentinel: false,
        }
    }
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
    pub(crate) initial_name: JsString,
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
    BigInt(JsBigInt),
    Bool(bool),
    Symbol(SymbolId),
}

/// Identity stored without a strong edge by weak collections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum WeakKey {
    Object(ObjectId),
    Symbol(SymbolId),
}

impl std::fmt::Display for WeakKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Object(id) => write!(f, "o:{id}"),
            Self::Symbol(id) => write!(f, "s:{id}"),
        }
    }
}

impl Serialize for WeakKey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for WeakKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct WeakKeyVisitor;

        impl serde::de::Visitor<'_> for WeakKeyVisitor {
            type Value = WeakKey;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a weak collection key (o:<id> or s:<id>)")
            }

            fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<WeakKey, E> {
                let invalid = || E::custom("invalid weak collection key");
                let (tag, digits) = text.split_once(':').ok_or_else(invalid)?;
                if digits.is_empty()
                    || (digits.len() > 1 && digits.starts_with('0'))
                    || !digits.bytes().all(|byte| byte.is_ascii_digit())
                {
                    return Err(invalid());
                }
                let id = digits.parse().map_err(|_| invalid())?;
                match tag {
                    "o" => Ok(WeakKey::Object(id)),
                    "s" => Ok(WeakKey::Symbol(id)),
                    _ => Err(invalid()),
                }
            }
        }

        deserializer.deserialize_str(WeakKeyVisitor)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum EventPayload {
    #[default]
    Event,
    CustomEvent {
        detail: Value,
    },
    ErrorEvent {
        message: JsString,
        filename: JsString,
        lineno: u32,
        colno: u32,
        error: Value,
    },
    PromiseRejectionEvent {
        promise: ObjectId,
        reason: Value,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EventState {
    pub ty: JsString,
    pub payload: EventPayload,
    pub bubbles: bool,
    pub cancelable: bool,
    pub composed: bool,
    pub time_stamp: f64,
    pub initialized: bool,
    pub trusted: bool,
    pub canceled: bool,
    pub stop: bool,
    pub stop_immediate: bool,
    pub dispatching: bool,
    pub in_passive: bool,
    pub target: Option<ObjectId>,
    pub current_target: Option<ObjectId>,
    pub phase: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventListener {
    pub id: u64,
    pub ty: JsString,
    pub callback: ObjectId,
    pub capture: bool,
    pub once: bool,
    pub passive: bool,
    pub global_handler: bool,
    // The registration retains its signal, but the signal's step is weak.
    pub signal: Option<ObjectId>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventTargetState {
    pub listeners: Vec<EventListener>,
    pub next_id: u64,
    pub global_handlers: [Option<u64>; 3],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AbortStep {
    Listener { target: ObjectId, id: u64 },
    Operation(u64),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AbortSignalState {
    pub reason: Option<Value>,
    pub dependent: bool,
    // Weak links in registration order, flattened to independent sources.
    pub sources: Vec<ObjectId>,
    pub dependents: Vec<ObjectId>,
    pub steps: Vec<AbortStep>,
    // Stable registration ID of the event handler, not a callback identity.
    pub onabort: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadableState {
    Readable,
    Closed,
    Errored(Value),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadableStreamObject {
    pub object: PlainObject,
    pub controller: ObjectId,
    pub reader: Option<ObjectId>,
    pub state: ReadableState,
    pub disturbed: bool,
    pub queue: VecDeque<(Value, f64)>,
    pub byte: Option<ReadableByteState>,
    #[serde(with = "stream_number")]
    pub total_size: f64,
    #[serde(with = "stream_number")]
    pub high_water_mark: f64,
    pub size: Option<Value>,
    pub source: Value,
    pub pull: Option<Value>,
    pub cancel: Option<Value>,
    pub started: bool,
    pub pulling: bool,
    pub pull_again: bool,
    pub close_requested: bool,
    pub start_wait: Option<ObjectId>,
    pub pull_wait: Option<ObjectId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteStreamRange {
    pub buffer: ObjectId,
    pub offset: usize,
    pub length: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PullIntoReader {
    None,
    Default,
    Byob,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullInto {
    pub range: ByteStreamRange,
    pub buffer_length: usize,
    pub bytes_filled: usize,
    pub minimum_fill: usize,
    pub element_size: usize,
    /// None denotes DataView; otherwise the intrinsic typed-array family.
    pub view_kind: Option<ElementKind>,
    pub reader_type: PullIntoReader,
    pub request: Option<ObjectId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadableByteState {
    pub queue: VecDeque<ByteStreamRange>,
    pub pending: VecDeque<PullInto>,
    pub byob_request: Option<ObjectId>,
    pub auto_allocate: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadableReaderObject {
    pub object: PlainObject,
    pub stream: Option<ObjectId>,
    pub closed: ObjectId,
    pub byob: bool,
    pub requests: VecDeque<ObjectId>,
}

mod stream_number {
    use super::*;
    use crate::value::ValueRef;

    pub fn serialize<S: Serializer>(number: &f64, serializer: S) -> Result<S::Ok, S::Error> {
        Value::number(*number).serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
        match Value::deserialize(deserializer)?.view() {
            ValueRef::Number(number) => Ok(number),
            _ => Err(serde::de::Error::custom("expected stream number")),
        }
    }
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
    Date {
        object: PlainObject,
        time: Option<i64>,
    },
    /// The only owner of a byte buffer. Views name it by id.
    ArrayBuffer(ArrayBufferObject),
    TypedArray(TypedArrayObject),
    DataView(DataViewObject),
    /// A branded `TextEncoder` instance. It has no state beyond properties.
    TextEncoder {
        object: PlainObject,
    },
    TextDecoder(TextDecoderObject),
    QueuingStrategy {
        object: PlainObject,
        // Value's Number encoding preserves NaN and infinities in JSON.
        high_water_mark: Value,
        byte_length: bool,
    },
    ReadableStream(ReadableStreamObject),
    ReadableReader(ReadableReaderObject),
    ReadableController {
        object: PlainObject,
        stream: ObjectId,
    },
    ReadableByobRequest {
        object: PlainObject,
        controller: Option<ObjectId>,
        view: Option<ObjectId>,
    },
    Url {
        object: PlainObject,
        url: url::Url,
        params: Option<ObjectId>,
    },
    UrlSearchParams {
        object: PlainObject,
        pairs: Vec<(String, String)>,
        owner: Option<ObjectId>,
    },
    UrlSearchParamsIterator {
        object: PlainObject,
        params: ObjectId,
        index: usize,
        kind: u8,
    },
    UrlPattern {
        object: PlainObject,
        #[serde(with = "crate::builtin::url_api::record::PatternInit")]
        input: urlpattern::UrlPatternInit,
        ignore_case: bool,
        #[serde(skip)]
        cache: crate::builtin::url_api::pattern::PatternCache,
    },
    /// A non-callable proxy over an ordinary object, array, or another proxy.
    Proxy {
        slots: Option<(ObjectId, ObjectId)>, // (target, handler); absent after revocation
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
        entries: BTreeMap<WeakKey, Value>,
    },
    WeakSet {
        object: PlainObject,
        entries: BTreeSet<WeakKey>,
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
        kind: ArrayIterationKind,
        next_index: u64,
    },
    IteratorHelper {
        object: PlainObject,
        state: crate::builtin::iterator::helpers::HelperState,
    },
    IteratorWrapper {
        object: PlainObject,
        record: crate::builtin::iterator::helpers::Record,
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
    DisposableStack {
        object: PlainObject,
        async_: bool,
        disposed: bool,
        records: Vec<DisposableResource>,
    },
    DisposeState(DisposeState),
    ProxyRevoker {
        object: PlainObject,
        proxy: Option<ObjectId>,
    },
    DomException {
        object: PlainObject,
        name: JsString,
        message: JsString,
    },
    Event {
        object: PlainObject,
        event: EventState,
    },
    EventTarget {
        object: PlainObject,
        target: EventTargetState,
    },
    Performance {
        object: PlainObject,
        target: EventTargetState,
    },
    AbortController {
        object: PlainObject,
        signal: ObjectId,
    },
    AbortSignal {
        object: PlainObject,
        target: EventTargetState,
        signal: AbortSignalState,
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
            (ReadableStream(a), ReadableStream(b)) => a == b,
            (ReadableReader(a), ReadableReader(b)) => a == b,
            (
                ReadableController {
                    object: a,
                    stream: x,
                },
                ReadableController {
                    object: b,
                    stream: y,
                },
            ) => a == b && x == y,
            (
                ReadableByobRequest {
                    object: a,
                    controller: x,
                    view: u,
                },
                ReadableByobRequest {
                    object: b,
                    controller: y,
                    view: v,
                },
            ) => a == b && x == y && u == v,
            (
                QueuingStrategy {
                    object: a,
                    high_water_mark: x,
                    byte_length: p,
                },
                QueuingStrategy {
                    object: b,
                    high_water_mark: y,
                    byte_length: q,
                },
            ) => a == b && x == y && p == q,
            (DisposeState(a), DisposeState(b)) => a == b,
            (
                DisposableStack {
                    object: a,
                    async_: x,
                    disposed: p,
                    records: r,
                },
                DisposableStack {
                    object: b,
                    async_: y,
                    disposed: q,
                    records: s,
                },
            ) => a == b && x == y && p == q && r == s,
            (Native(a), Native(b)) => a == b,
            (Plain(a), Plain(b)) => a == b,
            (
                Event {
                    object: a,
                    event: x,
                },
                Event {
                    object: b,
                    event: y,
                },
            ) => a == b && x == y,
            (
                EventTarget {
                    object: a,
                    target: x,
                },
                EventTarget {
                    object: b,
                    target: y,
                },
            ) => a == b && x == y,
            (
                Performance {
                    object: a,
                    target: x,
                },
                Performance {
                    object: b,
                    target: y,
                },
            ) => a == b && x == y,
            (
                AbortController {
                    object: a,
                    signal: x,
                },
                AbortController {
                    object: b,
                    signal: y,
                },
            ) => a == b && x == y,
            (
                AbortSignal {
                    object: a,
                    target: x,
                    signal: s,
                },
                AbortSignal {
                    object: b,
                    target: y,
                    signal: t,
                },
            ) => a == b && x == y && s == t,
            (
                DomException {
                    object: a,
                    name: x,
                    message: m,
                },
                DomException {
                    object: b,
                    name: y,
                    message: n,
                },
            ) => a == b && x == y && m == n,
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
            (Date { object: x, time: a }, Date { object: y, time: b }) => x == y && a == b,
            (
                Proxy {
                    slots: a,
                    object: x,
                },
                Proxy {
                    slots: b,
                    object: y,
                },
            ) => a == b && x == y,
            (
                ProxyRevoker {
                    proxy: a,
                    object: x,
                },
                ProxyRevoker {
                    proxy: b,
                    object: y,
                },
            ) => a == b && x == y,
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
                WeakSet {
                    object: x,
                    entries: a,
                },
                WeakSet {
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
                    kind: k,
                    next_index: n,
                },
                ArrayIterator {
                    object: y,
                    array: b,
                    kind: l,
                    next_index: m,
                },
            ) => x == y && a == b && k == l && n == m,
            (
                IteratorHelper {
                    object: a,
                    state: x,
                },
                IteratorHelper {
                    object: b,
                    state: y,
                },
            ) => a == b && x == y,
            (
                IteratorWrapper {
                    object: a,
                    record: x,
                },
                IteratorWrapper {
                    object: b,
                    record: y,
                },
            ) => a == b && x == y,
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
    CombineElement {
        state: ObjectId,
        index: usize,
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

    pub(super) fn restrict(self, property: &Property, attributes: &mut Attributes) {
        attributes.configurable = false;
        if self.freeze_data(property) {
            attributes.writable = false;
        }
    }

    pub(super) fn allows(self, property: &Property, attributes: Attributes) -> bool {
        !attributes.configurable && !(attributes.writable && self.freeze_data(property))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArrayIterationKind {
    Keys,
    Values,
    Entries,
}
