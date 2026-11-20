use super::store::lookup_tables;
use super::types::{
    BoundObject, BoxedPrimitive, CombineKind, EnvironmentObject, FunctionObject, IntegrityLevel,
    NativeObject, Object, OwnProperty, PromiseCallback, PromiseObject, PromiseState, Property,
    PropertyTables, PropertyTablesMut, Resolution, array_index, collect_own_keys,
};
use crate::ops::canonical_index_key;
use crate::value::{
    ContinuationId, JsString, ObjectId, PropertyKey, PropertyKeyRef, SymbolId, Value,
};
use serde::{Deserialize, Serialize};

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
            props.restrict(level);
            symbols.restrict(level);
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
        self.tables()
            .is_none_or(|(props, symbols)| props.satisfies(level) && symbols.satisfies(level))
    }

    pub(super) fn tables(&self) -> Option<PropertyTables<'_>> {
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

    pub(super) fn tables_mut(&mut self) -> Option<PropertyTablesMut<'_>> {
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

    pub(super) fn string_own_property(
        &self,
        key: PropertyKeyRef<'_>,
    ) -> Option<OwnProperty<'static>> {
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

    pub(super) fn valid_property_tables(&self) -> bool {
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
                    && array.props.iter().all(|(key, _)| {
                        let key = PropertyKeyRef::JsString(key);
                        !key.is_str("length") && array_index(key).is_none()
                    })
            }
            Self::Boxed {
                primitive: BoxedPrimitive::String(text),
                object,
            } => {
                let length = text.unit_len();
                object.props.iter().all(|(key, _)| {
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

    pub(super) fn prototype_mut(&mut self) -> Option<&mut Option<ObjectId>> {
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
                if let Some(home) = function.home {
                    out.push(OwnedEdge::Object(home));
                }
                if let Some(class) = &function.class {
                    out.push(OwnedEdge::Object(class.prototype));
                }
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

pub(super) fn push_state(out: &mut Vec<OwnedEdge>, state: &PromiseState) {
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
