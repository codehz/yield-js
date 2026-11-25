//! Bounded, script-independent previews of heap values.

use std::collections::BTreeSet;

use super::{ConsoleKey, ConsoleObject, ConsoleProperty, ConsoleValue};
use crate::heap::{Object, Property};
use crate::value::{ObjectId, PropertyKey, Value, ValueRef};
use crate::vm::VmCore;

pub(crate) const MAX_DEPTH: usize = 3;
pub(crate) const MAX_PROPERTIES: usize = 100;

pub(crate) struct Preview<'a> {
    vm: &'a VmCore,
    pub(crate) remaining: usize,
    ancestors: BTreeSet<ObjectId>,
}

impl<'a> Preview<'a> {
    pub(crate) fn new(vm: &'a VmCore) -> Self {
        Self {
            vm,
            remaining: MAX_PROPERTIES,
            ancestors: BTreeSet::new(),
        }
    }

    pub(crate) fn value(&mut self, value: &Value, depth: usize) -> ConsoleValue {
        match value.view() {
            ValueRef::Undefined => ConsoleValue::Undefined,
            ValueRef::Null => ConsoleValue::Null,
            ValueRef::Bool(value) => ConsoleValue::Bool(value),
            ValueRef::Number(value) => ConsoleValue::Number(value),
            ValueRef::BigInt(value) => ConsoleValue::BigInt(value.to_string()),
            ValueRef::String(value) => ConsoleValue::String(value.clone()),
            ValueRef::Symbol(id) => {
                ConsoleValue::Symbol(self.vm.heap.symbols.get(&id).cloned().flatten())
            }
            ValueRef::Object(id) => ConsoleValue::Object(self.object(id, depth)),
        }
    }

    fn object(&mut self, id: ObjectId, depth: usize) -> ConsoleObject {
        let object = self.vm.heap.get(id).expect("live console argument");
        let kind = kind(object);
        let mut result = ConsoleObject {
            kind: kind.into(),
            length: match object {
                Object::Array(array) => Some(array.length),
                _ => None,
            },
            properties: Vec::new(),
            circular: false,
            truncated: false,
        };
        if !matches!(kind, "Object" | "Array") {
            return result;
        }
        if !self.ancestors.insert(id) {
            result.circular = true;
            return result;
        }
        for (key, property) in object.enumerable_own_properties() {
            if self.remaining == 0 || depth >= MAX_DEPTH {
                result.truncated = true;
                break;
            }
            self.remaining -= 1;
            let key = match key {
                PropertyKey::String(key) => ConsoleKey::String(key),
                PropertyKey::Symbol(id) => {
                    ConsoleKey::Symbol(self.vm.heap.symbols.get(&id).cloned().flatten())
                }
            };
            let property = match property.into_owned() {
                Property::Data(value) => ConsoleProperty::Data(self.value(&value, depth + 1)),
                Property::Accessor { .. } => ConsoleProperty::Accessor,
            };
            result.properties.push((key, property));
        }
        self.ancestors.remove(&id);
        result
    }
}

pub(crate) fn is_truncated(value: &ConsoleValue) -> bool {
    let ConsoleValue::Object(object) = value else {
        return false;
    };
    object.truncated
        || object
            .properties
            .iter()
            .any(|(_, property)| match property {
                ConsoleProperty::Data(value) => is_truncated(value),
                ConsoleProperty::Accessor => false,
            })
}

/// Audit the same depth and per-preview property budget used by the builder.
pub(crate) fn valid(value: &ConsoleValue) -> bool {
    fn visit(value: &ConsoleValue, depth: usize, remaining: &mut usize) -> bool {
        match value {
            ConsoleValue::BigInt(text) => {
                let digits = text.strip_prefix('-').unwrap_or(text);
                text == "0"
                    || (!digits.is_empty()
                        && !digits.starts_with('0')
                        && digits.bytes().all(|byte| byte.is_ascii_digit()))
            }
            ConsoleValue::Object(object) => {
                let array = object.kind == "Array";
                if (array && object.length.is_none_or(|len| len > u32::MAX as usize))
                    || (!array && object.length.is_some())
                {
                    return false;
                }
                if !array && object.kind != "Object" {
                    return object.properties.is_empty() && !object.circular && !object.truncated;
                }
                if object.circular {
                    return depth != 0 && object.properties.is_empty() && !object.truncated;
                }
                if depth >= MAX_DEPTH && !object.properties.is_empty() {
                    return false;
                }
                for (_, property) in &object.properties {
                    if *remaining == 0 {
                        return false;
                    }
                    *remaining -= 1;
                    if let ConsoleProperty::Data(value) = property
                        && !visit(value, depth + 1, remaining)
                    {
                        return false;
                    }
                }
                !object.truncated || depth >= MAX_DEPTH || *remaining == 0
            }
            _ => true,
        }
    }
    let mut remaining = MAX_PROPERTIES;
    visit(value, 0, &mut remaining)
}

pub(crate) fn kind(object: &Object) -> &'static str {
    match object {
        Object::Plain(plain) if plain.is_error => "Error",
        Object::Plain(plain) if plain.module_namespace.is_some() => "Module",
        Object::Plain(plain) if plain.is_raw_json => "RawJSON",
        Object::Plain(_) => "Object",
        Object::Array(_) => "Array",
        Object::Promise(_) => "Promise",
        Object::Native(_)
        | Object::Function(_)
        | Object::Bound(_)
        | Object::ProxyRevoker { .. }
        | Object::PromiseCallback { .. } => "Function",
        Object::Boxed { primitive, .. } => match primitive {
            crate::heap::BoxedPrimitive::String(_) => "String",
            crate::heap::BoxedPrimitive::Number(_) => "Number",
            crate::heap::BoxedPrimitive::Bool(_) => "Boolean",
            crate::heap::BoxedPrimitive::BigInt(_) => "BigInt",
            crate::heap::BoxedPrimitive::Symbol(_) => "Symbol",
        },
        Object::RegExp { .. } => "RegExp",
        Object::Date { .. } => "Date",
        Object::ArrayBuffer(_) => "ArrayBuffer",
        Object::TypedArray(array) => array.kind.name(),
        Object::DataView(_) => "DataView",
        Object::CryptoKey(_) => "CryptoKey",
        Object::TextEncoder { .. } => "TextEncoder",
        Object::TextDecoder(_) => "TextDecoder",
        Object::TextStream {
            decoder: Some(_), ..
        } => "TextDecoderStream",
        Object::TextStream { .. } => "TextEncoderStream",
        Object::CompressionStream { decoding: true, .. } => "DecompressionStream",
        Object::CompressionStream { .. } => "CompressionStream",
        Object::QueuingStrategy {
            byte_length: true, ..
        } => "ByteLengthQueuingStrategy",
        Object::QueuingStrategy { .. } => "CountQueuingStrategy",
        Object::ReadableStream(_) => "ReadableStream",
        Object::ReadableReader(_) => "ReadableStreamReader",
        Object::WritableStream(_) => "WritableStream",
        Object::WritableWriter(_) => "WritableStreamDefaultWriter",
        Object::WritableController { .. } => "WritableStreamDefaultController",
        Object::TransformStream(_) => "TransformStream",
        Object::TransformController { .. } => "TransformStreamDefaultController",
        Object::ReadableController { .. } => "ReadableStreamController",
        Object::ReadableByobRequest { .. } => "ReadableStreamBYOBRequest",
        Object::Url { .. } => "URL",
        Object::UrlSearchParams { .. } => "URLSearchParams",
        Object::UrlPattern { .. } => "URLPattern",
        Object::Blob { .. } => "Blob",
        Object::File { .. } => "File",
        Object::Request { .. } => "Request",
        Object::Response { .. } => "Response",
        Object::Headers { .. } => "Headers",
        Object::FormData { .. } => "FormData",
        Object::Proxy { .. } => "Proxy",
        Object::Map { .. } => "Map",
        Object::Set { .. } => "Set",
        Object::WeakMap { .. } => "WeakMap",
        Object::WeakSet { .. } => "WeakSet",
        Object::DisposableStack { async_: true, .. } => "AsyncDisposableStack",
        Object::DisposableStack { .. } => "DisposableStack",
        Object::DomException { .. } => "DOMException",
        Object::Event { .. } => "Event",
        Object::EventTarget { .. } => "EventTarget",
        Object::Performance { .. } => "Performance",
        Object::AbortController { .. } => "AbortController",
        Object::AbortSignal { .. } => "AbortSignal",
        Object::ArrayIterator { .. } => "Array Iterator",
        Object::StringIterator { .. } => "String Iterator",
        Object::RegExpStringIterator { .. } => "RegExp String Iterator",
        Object::UrlSearchParamsIterator { .. } => "URLSearchParams Iterator",
        Object::FormDataIterator { .. } => "FormData Iterator",
        Object::HeadersIterator { .. } => "Headers Iterator",
        Object::CollectionIterator { .. }
        | Object::IteratorHelper { .. }
        | Object::IteratorWrapper { .. }
        | Object::StreamIterator(_) => "Iterator",
        Object::ForInIterator { .. }
        | Object::Environment(_)
        | Object::Resolution(_)
        | Object::DisposeState(_)
        | Object::StreamPipe(_)
        | Object::StreamTransfer(_)
        | Object::Tee(_)
        | Object::StreamFromIterator(_) => "Internal",
    }
}
