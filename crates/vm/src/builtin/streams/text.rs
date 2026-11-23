//! Encoding Standard streams backed by one native transform per instance.

use super::install;
use super::readable::{converted, illegal_constructor, interface};
use crate::VmError;
use crate::builtin::binary::ARRAY_BUFFER_PROTOTYPE;
use crate::builtin::binary::text::{
    Source, buffer_source, decoder_config, encoding_value, source_bytes,
};
use crate::builtin::event::new_target_prototype;
use crate::builtin::globals::Owner;
use crate::builtin::roots::{RootKind, RootSpec};
use crate::builtin::{FUNCTION_PARENT, OBJECT_PARENT};
use crate::gc::ActiveRoots;
use crate::heap::{
    ArrayBufferObject, Attributes, ElementKind, Heap, Object, PlainObject, ReadableStreamObject,
    TextDecoderConfig, TextEncoding, TransformAlgorithms, TransformStage, TypedArrayObject,
    WritableState, WritableStreamObject,
};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::Invocation;
use crate::value::{JsString, ObjectId, Value, ValueRef};
use crate::vm::Vm;

const ENCODER_PROTO: &str = "textEncoderStream.prototype";
const DECODER_PROTO: &str = "textDecoderStream.prototype";

interface!(ENCODER_OWNER, "textEncoderStream", "TextEncoderStream", ENCODER_PROTO,
    KEY => IntrinsicDef::constructor_with_target(KEY, 0, illegal_constructor, construct_encoder),
    PrototypeGetter "readable" 0 => |vm, _, roots, _, receiver| {
        endpoint(vm, roots, receiver, false, true)
    },
    PrototypeGetter "writable" 0 => |vm, _, roots, _, receiver| {
        endpoint(vm, roots, receiver, false, false)
    },
    PrototypeGetter "encoding" 0 => |vm, _, roots, _, receiver| {
        match text_stream(vm, receiver, false)? {
            Some(_) => Ok(Invocation::Value(encoding_value(TextEncoding::Utf8))),
            None => incompatible(vm, roots),
        }
    },
);

interface!(DECODER_OWNER, "textDecoderStream", "TextDecoderStream", DECODER_PROTO,
    KEY => IntrinsicDef::constructor_with_target(KEY, 0, illegal_constructor, construct_decoder),
    PrototypeGetter "readable" 0 => |vm, _, roots, _, receiver| {
        endpoint(vm, roots, receiver, true, true)
    },
    PrototypeGetter "writable" 0 => |vm, _, roots, _, receiver| {
        endpoint(vm, roots, receiver, true, false)
    },
    PrototypeGetter "encoding" 0 => |vm, _, roots, _, receiver| {
        decoder_property(vm, roots, receiver, |config| encoding_value(config.encoding))
    },
    PrototypeGetter "fatal" 0 => |vm, _, roots, _, receiver| {
        decoder_property(vm, roots, receiver, |config| Value::boolean(config.fatal))
    },
    PrototypeGetter "ignoreBOM" 0 => |vm, _, roots, _, receiver| {
        decoder_property(vm, roots, receiver, |config| Value::boolean(config.ignore_bom))
    },
);

fn construct_encoder(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    target: &Value,
) -> Result<Invocation, VmError> {
    let prototype = converted!(new_target_prototype(
        vm,
        host,
        &roots.with(target),
        target,
        ENCODER_PROTO
    ));
    construct(vm, prototype, None)
}

fn construct_decoder(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    target: &Value,
) -> Result<Invocation, VmError> {
    let roots = &roots.with(&args);
    let roots = &roots.with(target);
    let config = converted!(decoder_config(vm, host, roots, &args));
    let prototype = converted!(new_target_prototype(vm, host, roots, target, DECODER_PROTO));
    construct(vm, prototype, Some(config))
}

fn construct(
    vm: &mut Vm,
    prototype: ObjectId,
    decoder: Option<TextDecoderConfig>,
) -> Result<Invocation, VmError> {
    let algorithms = match decoder {
        Some(config) => TransformAlgorithms::TextDecoder(config, Default::default()),
        None => TransformAlgorithms::TextEncoder(Default::default()),
    };
    let transform = vm.create_native_transform(algorithms)?;
    let wrapper = vm.heap.alloc(Object::TextStream {
        object: PlainObject::new(Some(prototype)),
        transform,
        decoder,
    });
    Ok(Invocation::Value(Value::object(wrapper)))
}

fn text_stream(
    vm: &Vm,
    receiver: &Value,
    decoder_brand: bool,
) -> Result<Option<(ObjectId, Option<TextDecoderConfig>)>, VmError> {
    let ValueRef::Object(id) = receiver.view() else {
        return Ok(None);
    };
    match vm.heap.get(id)? {
        Object::TextStream {
            transform, decoder, ..
        } if decoder.is_some() == decoder_brand => Ok(Some((*transform, *decoder))),
        _ => Ok(None),
    }
}

fn incompatible(vm: &mut Vm, roots: &ActiveRoots<'_>) -> Result<Invocation, VmError> {
    vm.throw_type_error(
        roots,
        "Text stream getter called on an incompatible receiver",
    )
}

fn endpoint(
    vm: &mut Vm,
    roots: &ActiveRoots<'_>,
    receiver: &Value,
    decoder_brand: bool,
    readable: bool,
) -> Result<Invocation, VmError> {
    let Some((transform, _)) = text_stream(vm, receiver, decoder_brand)? else {
        return incompatible(vm, roots);
    };
    let stream = vm.transform(transform)?;
    let id = if readable {
        stream.readable
    } else {
        stream.writable
    };
    Ok(Invocation::Value(Value::object(id)))
}

fn decoder_property(
    vm: &mut Vm,
    roots: &ActiveRoots<'_>,
    receiver: &Value,
    property: impl FnOnce(TextDecoderConfig) -> Value,
) -> Result<Invocation, VmError> {
    match text_stream(vm, receiver, true)? {
        Some((_, Some(config))) => Ok(Invocation::Value(property(config))),
        _ => incompatible(vm, roots),
    }
}

impl Vm {
    pub(super) fn invoke_text_transform(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
        stage: TransformStage,
        chunk: &Value,
    ) -> Result<Invocation, VmError> {
        let flush = match stage {
            TransformStage::Transform => false,
            TransformStage::Flush => true,
            TransformStage::SinkAbort | TransformStage::SourceCancel => {
                return Ok(Invocation::Value(Value::UNDEFINED));
            }
            TransformStage::Backpressure => unreachable!("backpressure is not a callback"),
        };
        let live = (stream, chunk);
        let roots = &roots.with(&live);
        let output = if let Some(TransformAlgorithms::TextEncoder(encoder)) =
            &self.transform(stream)?.algorithms
        {
            // ToString can run script and cancel the readable. Preserve the
            // carry for this invocation without removing it from the heap.
            let mut encoder = *encoder;
            let text = if flush {
                JsString::new()
            } else {
                converted!(self.coerce_string(host, roots, chunk))
            };
            let bytes = encoder.encode(text.units(), flush);
            if let Some(TransformAlgorithms::TextEncoder(checkpoint)) =
                &mut self.transform_mut(stream)?.algorithms
            {
                *checkpoint = encoder;
            }
            if bytes.is_empty() {
                return Ok(Invocation::Value(Value::UNDEFINED));
            }
            self.text_stream_bytes(bytes)
        } else {
            let source = if flush {
                Source::Empty
            } else {
                let source = converted!(buffer_source(self, roots, chunk));
                if matches!(source, Source::Empty) {
                    return self.throw_type_error(
                        roots,
                        "TextDecoderStream chunk must be a buffer source",
                    );
                }
                source
            };
            // No script runs while the checkpoint is borrowed from the
            // transform. Decode directly from the live view's byte window.
            let Some(TransformAlgorithms::TextDecoder(config, state)) =
                &mut self.transform_mut(stream)?.algorithms
            else {
                unreachable!("native decoder dispatch requires a decoder checkpoint");
            };
            let config = *config;
            let mut state = std::mem::take(state);
            let decoded = state.decode_stream(config, source_bytes(self, source)?, flush);
            let Some(TransformAlgorithms::TextDecoder(_, checkpoint)) =
                &mut self.transform_mut(stream)?.algorithms
            else {
                unreachable!("decoding does not execute script");
            };
            match decoded {
                Ok(text) => {
                    *checkpoint = state;
                    if text.is_empty() {
                        return Ok(Invocation::Value(Value::UNDEFINED));
                    }
                    Value::string(text.into())
                }
                Err(_) => return self.throw_type_error(roots, "TextDecoderStream fatal error"),
            }
        };
        self.transform_enqueue(host, roots, stream, &output)
    }

    fn text_stream_bytes(&mut self, bytes: Vec<u8>) -> Value {
        // Move the freshly encoded bytes; no constructor or prototype getter
        // is invoked by the native transform.
        let byte_length = bytes.len();
        let buffer = self.heap.alloc(Object::ArrayBuffer(ArrayBufferObject {
            object: PlainObject::new(Some(self.builtins.get(ARRAY_BUFFER_PROTOTYPE))),
            bytes: Some(bytes),
            max_byte_length: None,
        }));
        Value::object(self.heap.alloc(Object::TypedArray(TypedArrayObject {
            object: PlainObject::new(Some(self.builtins.get("uint8Array.prototype"))),
            buffer,
            byte_offset: 0,
            byte_length: Some(byte_length),
            kind: ElementKind::Uint8,
        })))
    }
}

fn endpoint_defaults(readable: &ReadableStreamObject, writable: &WritableStreamObject) -> bool {
    readable.high_water_mark == 0.0
        && writable.high_water_mark == 1.0
        && readable.size.is_none()
        && writable.sink.size.is_none()
}

pub(super) fn validate_native(
    algorithms: &TransformAlgorithms,
    readable: &ReadableStreamObject,
    writable: &WritableStreamObject,
) -> bool {
    endpoint_defaults(readable, writable)
        && match algorithms {
            TransformAlgorithms::TextEncoder(encoder) => encoder.valid_checkpoint(),
            TransformAlgorithms::TextDecoder(config, state) => {
                state.valid_stream_checkpoint(config.encoding, config.ignore_bom)
            }
            TransformAlgorithms::Script { .. } => false,
        }
}

pub(super) fn validate_wrapper(
    heap: &Heap,
    transform: ObjectId,
    decoder: Option<TextDecoderConfig>,
) -> bool {
    let Ok(Object::TransformStream(stream)) = heap.get(transform) else {
        return false;
    };
    let Ok(Object::ReadableStream(readable)) = heap.get(stream.readable) else {
        return false;
    };
    let Ok(Object::WritableStream(writable)) = heap.get(stream.writable) else {
        return false;
    };
    !stream.transfer_proxy
        && endpoint_defaults(readable, writable)
        && match &stream.algorithms {
            Some(TransformAlgorithms::TextEncoder(_)) => decoder.is_none(),
            Some(TransformAlgorithms::TextDecoder(config, _)) => decoder == Some(*config),
            Some(TransformAlgorithms::Script { .. }) => false,
            None => stream.finish.is_some() || !matches!(writable.state, WritableState::Writable),
        }
}
