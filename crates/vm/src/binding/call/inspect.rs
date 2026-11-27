//! Borrowed, read-only inspection of one script value. Never invokes getters
//! or proxy traps and cannot write script-visible state.

use std::cell::RefCell;

use crate::binding::JsResult;
use crate::heap::{Object, Property};
use crate::value::{JsBigInt, JsString, PropertyKey, SymbolId, ValueRef};
use crate::vm::VmCore;

use super::{CallCx, HandleRoots, JsValue};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectKind {
    Undefined,
    Null,
    Bool,
    Number,
    String,
    BigInt,
    Symbol,
    Object,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum InspectKey {
    String(JsString),
    Symbol(SymbolId),
}

#[derive(Debug, Clone)]
pub enum InspectValue {
    Data(JsValue),
    Accessor,
}

/// A borrowed, read-only view over one script value. It never invokes getters
/// or proxy traps and cannot write script-visible state.
pub struct Inspect<'a> {
    vm: &'a VmCore,
    value: JsValue,
    epoch: u32,
    call: u64,
    handles: &'a RefCell<HandleRoots>,
}

impl CallCx<'_> {
    pub fn inspect(&self, value: JsValue) -> JsResult<Inspect<'_>> {
        self.check_value(&value)?;
        Ok(Inspect {
            vm: self.vm,
            value,
            epoch: self.epoch,
            call: self.call,
            handles: &self.handles,
        })
    }
}

impl<'a> Inspect<'a> {
    pub fn kind(&self) -> InspectKind {
        match self.value.value.view() {
            ValueRef::Undefined => InspectKind::Undefined,
            ValueRef::Null => InspectKind::Null,
            ValueRef::Bool(_) => InspectKind::Bool,
            ValueRef::Number(_) => InspectKind::Number,
            ValueRef::String(_) => InspectKind::String,
            ValueRef::BigInt(_) => InspectKind::BigInt,
            ValueRef::Symbol(_) => InspectKind::Symbol,
            ValueRef::Object(_) => InspectKind::Object,
        }
    }

    pub fn value(&self) -> JsValue {
        self.value.clone()
    }

    pub fn bigint(&self) -> Option<&JsBigInt> {
        self.value.as_bigint()
    }

    pub fn identity(&self) -> Option<u64> {
        match self.value.value.view() {
            ValueRef::Object(id) | ValueRef::Symbol(id) => Some(u64::from(id)),
            _ => None,
        }
    }

    pub fn array_length(&self) -> Option<usize> {
        let ValueRef::Object(id) = self.value.value.view() else {
            return None;
        };
        match self.vm.heap.get(id).ok()? {
            Object::Array(array) => Some(array.length),
            _ => None,
        }
    }

    /// Whether this object was created as an Error, independent of its prototype.
    pub fn is_error(&self) -> bool {
        self.object().is_some_and(Object::has_error_brand)
    }

    /// Whether this value is a RegExp object, without following proxies.
    pub fn is_regexp(&self) -> bool {
        matches!(self.object(), Some(Object::RegExp { .. }))
    }

    /// Match from index zero without running methods or reading or writing `lastIndex`.
    pub fn regexp_matches(&self, text: &JsString) -> Option<bool> {
        let Object::RegExp {
            flags, compiled, ..
        } = self.object()?
        else {
            return None;
        };
        let units = crate::regexp::encode_input(text, *flags);
        Some(crate::regexp::find_match(compiled, *flags, &units, 0).is_some())
    }

    /// The stored pattern source and canonical flags, without property access.
    pub fn regexp_source_flags(&self) -> Option<(&JsString, String)> {
        let Object::RegExp { source, flags, .. } = self.object()? else {
            return None;
        };
        Some((source, flags.as_string()))
    }

    fn object(&self) -> Option<&Object> {
        let ValueRef::Object(id) = self.value.value.view() else {
            return None;
        };
        self.vm.heap.get(id).ok()
    }

    pub fn own_keys(&self) -> Vec<InspectKey> {
        let ValueRef::Object(id) = self.value.value.view() else {
            return Vec::new();
        };
        let Ok(object) = self.vm.heap.get(id) else {
            return Vec::new();
        };
        let mut keys = Vec::new();
        for (key, _) in object.enumerable_own_properties() {
            keys.push(match key {
                PropertyKey::String(key) => InspectKey::String(key),
                PropertyKey::Symbol(key) => InspectKey::Symbol(key),
            });
        }
        keys.sort();
        keys
    }

    pub fn get(&self, key: &InspectKey) -> Option<InspectValue> {
        let ValueRef::Object(id) = self.value.value.view() else {
            return None;
        };
        let key = match key {
            InspectKey::String(key) => PropertyKey::String(key.clone()),
            InspectKey::Symbol(key) => PropertyKey::Symbol(*key),
        };
        let object = self.vm.heap.get(id).ok()?;
        match object.own_property(key.borrowed()) {
            Some(property) => match property.into_owned() {
                Property::Data(value) => Some(InspectValue::Data(JsValue::rooted_handle(
                    value,
                    self.epoch,
                    self.call,
                    self.handles,
                ))),
                Property::Accessor { .. } => Some(InspectValue::Accessor),
            },
            None => None,
        }
    }
}

impl InspectValue {
    pub fn data(&self) -> Option<JsValue> {
        match self {
            Self::Data(value) => Some(value.clone()),
            Self::Accessor => None,
        }
    }
}
