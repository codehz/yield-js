//! Compression Standard streams backed by durable native transform algorithms.

mod brotli;
mod deflate;

use super::install;
use super::readable::{converted, illegal_constructor, interface};
use crate::VmError;
use crate::builtin::binary::text::{Source, buffer_source, source_bytes};
use crate::builtin::event::new_target_prototype;
use crate::builtin::globals::Owner;
use crate::builtin::roots::{RootKind, RootSpec};
use crate::builtin::{FUNCTION_PARENT, OBJECT_PARENT};
use crate::gc::ActiveRoots;
use crate::heap::{Heap, Object, PlainObject, TransformAlgorithms, TransformStage};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::Invocation;
use crate::value::{ObjectId, Value, ValueRef};
use crate::vm::Vm;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Format {
    Gzip,
    Deflate,
    DeflateRaw,
    Brotli,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Codec(Engine);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum Engine {
    Deflate(deflate::Codec),
    Brotli(brotli::Codec),
}

impl Codec {
    fn new(format: Format, decoding: bool) -> Self {
        Self(match format {
            Format::Brotli => Engine::Brotli(brotli::Codec::new(format, decoding)),
            _ => Engine::Deflate(deflate::Codec::new(format, decoding)),
        })
    }

    pub(super) fn decoding(&self) -> bool {
        match &self.0 {
            Engine::Deflate(codec) => codec.decoding(),
            Engine::Brotli(codec) => codec.decoding(),
        }
    }

    pub(super) fn valid_checkpoint(&self) -> bool {
        match &self.0 {
            Engine::Deflate(codec) => codec.valid_checkpoint(),
            Engine::Brotli(codec) => codec.valid_checkpoint(),
        }
    }

    fn process(&mut self, bytes: &[u8], finish: bool) -> Result<Vec<Vec<u8>>, ()> {
        match &mut self.0 {
            Engine::Deflate(codec) => codec.process(bytes, finish),
            Engine::Brotli(codec) => codec.process(bytes, finish),
        }
    }
}

/// Decodes exactly one HTTP content coding without retaining decoder state.
pub(crate) fn decode_http_encoding(encoding: &[u8], data: &[u8]) -> Result<Vec<u8>, ()> {
    let name = encoding.trim_ascii();
    let format = if name.eq_ignore_ascii_case(b"gzip") || name.eq_ignore_ascii_case(b"x-gzip") {
        Format::Gzip
    } else if name.eq_ignore_ascii_case(b"deflate") {
        Format::Deflate
    } else if name.eq_ignore_ascii_case(b"br") {
        Format::Brotli
    } else {
        return Err(());
    };
    let mut codec = Codec::new(format, true);
    let mut output = Vec::new();
    for chunk in codec.process(data, true)? {
        output.extend_from_slice(&chunk);
    }
    Ok(output)
}

const ENCODER_PROTO: &str = "compressionStream.prototype";
const DECODER_PROTO: &str = "decompressionStream.prototype";

interface!(ENCODER_OWNER, "compressionStream", "CompressionStream", ENCODER_PROTO,
    KEY => IntrinsicDef::constructor_with_target(KEY, 1, illegal_constructor,
        |vm, host, roots, args, target| construct(vm, host, roots, args, target, false)),
    PrototypeGetter "readable" 0 => |vm, _, roots, _, receiver| {
        endpoint(vm, roots, receiver, false, true)
    },
    PrototypeGetter "writable" 0 => |vm, _, roots, _, receiver| {
        endpoint(vm, roots, receiver, false, false)
    },
);

interface!(DECODER_OWNER, "decompressionStream", "DecompressionStream", DECODER_PROTO,
    KEY => IntrinsicDef::constructor_with_target(KEY, 1, illegal_constructor,
        |vm, host, roots, args, target| construct(vm, host, roots, args, target, true)),
    PrototypeGetter "readable" 0 => |vm, _, roots, _, receiver| {
        endpoint(vm, roots, receiver, true, true)
    },
    PrototypeGetter "writable" 0 => |vm, _, roots, _, receiver| {
        endpoint(vm, roots, receiver, true, false)
    },
);

fn construct(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    target: &Value,
    decoding: bool,
) -> Result<Invocation, VmError> {
    let roots = &roots.with(&args);
    let roots = &roots.with(target);
    let Some(value) = args.first() else {
        return vm.throw_type_error(roots, "Compression format is required");
    };
    let name = converted!(vm.coerce_string(host, roots, value));
    let format = match name {
        name if name == "gzip" => Format::Gzip,
        name if name == "deflate" => Format::Deflate,
        name if name == "deflate-raw" => Format::DeflateRaw,
        name if name == "brotli" => Format::Brotli,
        _ => return vm.throw_type_error(roots, "Invalid compression format"),
    };
    let fallback = if decoding {
        DECODER_PROTO
    } else {
        ENCODER_PROTO
    };
    let prototype = converted!(new_target_prototype(vm, host, roots, target, fallback));
    let transform = vm.create_native_transform(TransformAlgorithms::Compression(Box::new(
        Codec::new(format, decoding),
    )))?;
    let wrapper = vm.heap.alloc(Object::CompressionStream {
        object: PlainObject::new(Some(prototype)),
        transform,
        decoding,
    });
    Ok(Invocation::Value(Value::object(wrapper)))
}

fn endpoint(
    vm: &mut Vm,
    roots: &ActiveRoots<'_>,
    receiver: &Value,
    decoder_brand: bool,
    readable: bool,
) -> Result<Invocation, VmError> {
    if let ValueRef::Object(id) = receiver.view()
        && let Object::CompressionStream {
            transform,
            decoding,
            ..
        } = vm.heap.get(id)?
        && *decoding == decoder_brand
    {
        let stream = vm.transform(*transform)?;
        let id = if readable {
            stream.readable
        } else {
            stream.writable
        };
        return Ok(Invocation::Value(Value::object(id)));
    }
    vm.throw_type_error(
        roots,
        "Compression stream getter called on an incompatible receiver",
    )
}

impl Vm {
    pub(super) fn invoke_compression_transform(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
        stage: TransformStage,
        chunk: &Value,
    ) -> Result<Invocation, VmError> {
        let live = (stream, chunk);
        let roots = &roots.with(&live);
        let source = match stage {
            TransformStage::Transform => {
                let source = converted!(buffer_source(self, roots, chunk));
                if matches!(source, Source::Empty) {
                    return self.throw_type_error(
                        roots,
                        "Compression stream chunk must be a buffer source",
                    );
                }
                source
            }
            TransformStage::Flush => Source::Empty,
            TransformStage::SinkAbort | TransformStage::SourceCancel => {
                return Ok(Invocation::Value(Value::UNDEFINED));
            }
            TransformStage::Backpressure => unreachable!("backpressure is not a callback"),
        };
        // Nothing can run guest code or trigger GC while the codec is taken
        // out of the stream. This avoids copying the input view or codec state.
        let Some(TransformAlgorithms::Compression(mut codec)) =
            self.transform_mut(stream)?.algorithms.take()
        else {
            unreachable!("compression dispatch requires a compression checkpoint");
        };
        let result = source_bytes(self, source)
            .map(|bytes| codec.process(bytes, stage == TransformStage::Flush));
        self.transform_mut(stream)?.algorithms = Some(TransformAlgorithms::Compression(codec));
        let Ok(output) = result? else {
            return self.throw_type_error(roots, "Compression codec failed");
        };
        for bytes in output.into_iter().filter(|bytes| !bytes.is_empty()) {
            let value = self.stream_bytes(bytes);
            if let thrown @ Invocation::Thrown(_) =
                self.transform_enqueue(host, roots, stream, &value)?
            {
                return Ok(thrown);
            }
        }
        Ok(Invocation::Value(Value::UNDEFINED))
    }
}

pub(super) fn validate_wrapper(heap: &Heap, transform: ObjectId, decoding: bool) -> bool {
    matches!(heap.get(transform), Ok(Object::TransformStream(stream))
        if stream.compression_brand == Some(decoding))
}
