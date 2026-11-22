//! Binary-array queries that need more than one object.
//!
//! An element is not stored on the view: it lives in the buffer the view names.
//! Resolving it therefore takes the whole heap, and the result is materialized
//! before it is returned so no borrow of the bytes escapes the lookup.

use crate::builtin::binary::element::{decode, load, store};
use crate::error::VmError;
use crate::heap::{
    ArrayBufferObject, DataViewObject, Heap, Object, OwnProperty, Property, TypedArrayObject,
    ViewRange, view_range,
};
use crate::ops;
use crate::value::{ObjectId, PropertyKey, PropertyKeyRef, Value};

/// Outcome of CanonicalNumericIndexString on a TypedArray view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TypedIndex {
    /// The key is not a canonical numeric index string; ordinary lookup applies.
    Ordinary,
    /// A canonical index that addresses no element: reads give `undefined`, and
    /// writes are dropped without creating a property.
    Invalid,
    Valid(u64),
}

pub(crate) enum OwnPropertyLookup<'a> {
    Found(OwnProperty<'a>),
    Missing,
    /// A canonical TypedArray index that must not walk the prototype chain.
    InvalidTypedIndex,
}

/// CanonicalNumericIndexString: `-0` first, then an exact Number round-trip.
pub(crate) fn canonical_numeric_index(text: &str) -> Option<f64> {
    if text == "-0" {
        return Some(-0.0);
    }
    let digits = text.as_bytes();
    if !digits.is_empty() && digits.iter().all(u8::is_ascii_digit) {
        if digits.len() == 1 {
            return Some(f64::from(digits[0] - b'0'));
        }
        // Below 2^53 an integer without a leading zero round-trips to the same
        // digits; longer or zero-prefixed forms need the general check.
        if digits[0] != b'0'
            && let Ok(value) = text.parse::<u64>()
            && value <= 9_007_199_254_740_991
        {
            return Some(value as f64);
        }
    }
    let number = ops::to_number(&Value::string(text.into())).ok()?;
    let formatted = ops::to_js_string(&Value::number(number));
    (formatted.to_utf8().ok()? == text).then_some(number)
}

/// An integer index: a non-negative integer, excluding negative zero.
fn integer_index(number: f64) -> Option<u64> {
    if !number.is_finite()
        || number.fract() != 0.0
        || number < 0.0
        || number == 0.0 && number.is_sign_negative()
    {
        return None;
    }
    Some(number as u64)
}

impl Heap {
    pub(crate) fn buffer(&self, id: ObjectId) -> Result<Option<&ArrayBufferObject>, VmError> {
        Ok(match self.get(id)? {
            Object::ArrayBuffer(buffer) => Some(buffer),
            _ => None,
        })
    }

    pub(crate) fn typed_array(&self, id: ObjectId) -> Result<Option<&TypedArrayObject>, VmError> {
        Ok(match self.get(id)? {
            Object::TypedArray(view) => Some(view),
            _ => None,
        })
    }

    pub(crate) fn data_view(&self, id: ObjectId) -> Result<Option<&DataViewObject>, VmError> {
        Ok(match self.get(id)? {
            Object::DataView(view) => Some(view),
            _ => None,
        })
    }

    /// Bytes of the buffer a view names, or `None` when that buffer is detached.
    pub(crate) fn view_bytes(&self, buffer: ObjectId) -> Result<Option<&[u8]>, VmError> {
        Ok(self
            .buffer(buffer)?
            .and_then(|buffer| buffer.bytes.as_deref()))
    }

    /// Current extent of a TypedArray view, rounded down to whole elements.
    pub(crate) fn typed_range(&self, view: &TypedArrayObject) -> Result<ViewRange, VmError> {
        Ok(self.typed_bytes_and_range(view)?.1)
    }

    fn typed_bytes_and_range(
        &self,
        view: &TypedArrayObject,
    ) -> Result<(Option<&[u8]>, ViewRange), VmError> {
        let bytes = self.view_bytes(view.buffer)?;
        let range = view_range(bytes, view.byte_offset, view.byte_length, view.kind.bytes());
        Ok((bytes, range))
    }

    pub(crate) fn data_view_range(&self, view: &DataViewObject) -> Result<ViewRange, VmError> {
        let bytes = self.view_bytes(view.buffer)?;
        Ok(view_range(bytes, view.byte_offset, view.byte_length, 1))
    }

    /// Number of elements a TypedArray currently exposes.
    pub(crate) fn typed_length(&self, view: &TypedArrayObject) -> Result<usize, VmError> {
        Ok(self.typed_range(view)?.len() / view.kind.bytes())
    }

    /// Classify a key against a TypedArray's integer-indexed exotic behavior.
    pub(crate) fn typed_index(
        &self,
        id: ObjectId,
        key: PropertyKeyRef<'_>,
    ) -> Result<TypedIndex, VmError> {
        let Some(view) = self.typed_array(id)? else {
            return Ok(TypedIndex::Ordinary);
        };
        let Some(number) = numeric_index(key) else {
            return Ok(TypedIndex::Ordinary);
        };
        let Some(index) = integer_index(number) else {
            return Ok(TypedIndex::Invalid);
        };
        let length = self.typed_length(view)? as u64;
        if index >= length {
            return Ok(TypedIndex::Invalid);
        }
        Ok(TypedIndex::Valid(index))
    }

    /// Own property including the integer-indexed elements of a TypedArray.
    pub(crate) fn own_property(
        &self,
        id: ObjectId,
        key: PropertyKeyRef<'_>,
    ) -> Result<Option<OwnProperty<'_>>, VmError> {
        Ok(match self.lookup_own_property(self.get(id)?, key)? {
            OwnPropertyLookup::Found(property) => Some(property),
            OwnPropertyLookup::Missing | OwnPropertyLookup::InvalidTypedIndex => None,
        })
    }

    pub(crate) fn lookup_own_property<'a>(
        &'a self,
        object: &'a Object,
        key: PropertyKeyRef<'_>,
    ) -> Result<OwnPropertyLookup<'a>, VmError> {
        if let Object::TypedArray(view) = object
            && let Some(number) = numeric_index(key)
        {
            if crate::perf::ENABLED {
                crate::perf::record(|counters| counters.property_lookups += 1);
            }
            let Some(index) = integer_index(number) else {
                return Ok(OwnPropertyLookup::InvalidTypedIndex);
            };
            return Ok(match self.typed_element_from_view(view, index)? {
                Some(value) => OwnPropertyLookup::Found(OwnProperty::Computed(
                    Property::Data(value),
                    crate::heap::Attributes::ORDINARY,
                )),
                None => OwnPropertyLookup::InvalidTypedIndex,
            });
        }
        Ok(match object.own_property(key) {
            Some(property) => OwnPropertyLookup::Found(property),
            None => OwnPropertyLookup::Missing,
        })
    }

    /// TypedArray [[OwnPropertyKeys]]: element indices, then every other own
    /// string key, then symbols. A view that currently addresses nothing
    /// contributes no indices.
    pub(crate) fn typed_own_keys(
        &self,
        id: ObjectId,
        enumerable_only: bool,
    ) -> Result<Option<Vec<PropertyKey>>, VmError> {
        let Some(view) = self.typed_array(id)? else {
            return Ok(None);
        };
        let mut keys = Vec::new();
        if !matches!(
            self.typed_range(view)?,
            ViewRange::Detached | ViewRange::OutOfBounds
        ) {
            for index in 0..self.typed_length(view)? {
                keys.push(PropertyKey::String(index.to_string().into()));
            }
        }
        let stored = self
            .get(id)?
            .own_keys(enumerable_only)
            .filter(|key| match key {
                PropertyKey::String(name) => !is_canonical_index(PropertyKeyRef::JsString(name)),
                PropertyKey::Symbol(_) => true,
            });
        keys.extend(stored);
        Ok(Some(keys))
    }

    /// Element at a canonical index, or `None` when the view is out of range.
    pub(crate) fn typed_element(&self, id: ObjectId, index: u64) -> Result<Option<Value>, VmError> {
        let Some(view) = self.typed_array(id)? else {
            return Ok(None);
        };
        self.typed_element_from_view(view, index)
    }

    fn typed_element_from_view(
        &self,
        view: &TypedArrayObject,
        index: u64,
    ) -> Result<Option<Value>, VmError> {
        let kind = view.kind;
        let (Some(bytes), ViewRange::Bytes { start, len }) = self.typed_bytes_and_range(view)?
        else {
            return Ok(None);
        };
        if index >= (len / kind.bytes()) as u64 {
            return Ok(None);
        }
        let position = index as usize * kind.bytes();
        Ok(Some(decode(
            kind,
            load(&bytes[start + position..], kind, true),
        )))
    }

    /// Store a pre-converted element. An out-of-range view is not written.
    pub(crate) fn store_typed_element(
        &mut self,
        id: ObjectId,
        index: u64,
        bits: u64,
    ) -> Result<(), VmError> {
        let Some(view) = self.typed_array(id)? else {
            return Ok(());
        };
        let (buffer, kind) = (view.buffer, view.kind);
        let ViewRange::Bytes { start, len } = self.typed_range(view)? else {
            return Ok(());
        };
        let width = kind.bytes();
        let Some(position) = usize::try_from(index)
            .ok()
            .and_then(|index| index.checked_mul(width))
        else {
            return Ok(());
        };
        if position + width > len {
            return Ok(());
        }
        let Some(Object::ArrayBuffer(buffer)) = Some(self.get_mut(buffer)?) else {
            return Ok(());
        };
        let Some(bytes) = buffer.bytes.as_mut() else {
            return Ok(());
        };
        store(&mut bytes[start + position..], kind, true, bits);
        Ok(())
    }
}

/// Whether a key is a canonical numeric index string, which a TypedArray never
/// stores as an ordinary property.
pub(crate) fn is_canonical_index(key: PropertyKeyRef<'_>) -> bool {
    numeric_index(key).is_some()
}

fn numeric_index(key: PropertyKeyRef<'_>) -> Option<f64> {
    match key {
        PropertyKeyRef::String(text) => canonical_numeric_index(text),
        PropertyKeyRef::JsString(text) => canonical_numeric_index(&text.to_utf8().ok()?),
        PropertyKeyRef::Symbol(_) => None,
    }
}
