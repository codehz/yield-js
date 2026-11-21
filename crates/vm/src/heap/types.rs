use std::borrow::Borrow;
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ops::Range;

use serde::{Deserialize, Serialize};

use super::object::ReactionJob;
use crate::builtin::string::utf16_unit_string;
use crate::error::VmError;
use crate::ops::canonical_index_key;
use crate::regexp::{RegExpFlags, compile_pattern};
use crate::value::{BindingId, JsString, ObjectId, PropertyKey, PropertyKeyRef, SymbolId, Value};

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromiseState {
    Pending,
    Fulfilled(Value),
    Rejected(Value),
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
    pub(super) extensible: bool,
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

mod property_pairs {
    use super::{Property, PropertyTable};
    use crate::value::JsString;

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
    pub const FUNCTION_PROTOTYPE: Self = Self {
        writable: true,
        ..Self::FIXED
    };
    pub(super) const ACCESSOR: Self = Self {
        writable: false,
        ..Self::ORDINARY
    };
}

impl<K: Ord, V> PropertyTable<K, V> {
    pub(super) fn new() -> Self {
        Self(BTreeMap::new())
    }

    pub fn get<Q: Ord + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        self.0.get(key).map(|property| &property.value)
    }

    pub(super) fn entry<Q: Ord + ?Sized>(&self, key: &Q) -> Option<&OrderedProperty<V>>
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

    pub(super) fn update(
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

    pub(super) fn remove_configurable<Q: Ord + ?Sized>(&mut self, key: &Q) -> bool
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

    pub(super) fn valid_order(&self) -> bool {
        let mut seen = vec![false; self.0.len()];
        self.0.values().all(|property| {
            seen.get_mut(property.ordinal)
                .is_some_and(|seen| !std::mem::replace(seen, true))
        })
    }
}

impl<K: Ord> PropertyTable<K, Property> {
    pub(super) fn restrict(&mut self, level: IntegrityLevel) {
        for entry in self.0.values_mut() {
            level.restrict(&entry.value, &mut entry.attributes);
        }
    }

    pub(super) fn satisfies(&self, level: IntegrityLevel) -> bool {
        self.0
            .values()
            .all(|entry| level.allows(&entry.value, entry.attributes))
    }
}

pub(super) fn collect_own_keys<'a, V>(
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
    pub(super) extensible: bool,
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
    pub(crate) has_class_metadata: bool,
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
