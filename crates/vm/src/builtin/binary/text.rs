//! `TextEncoder` (UTF-8) and `TextDecoder` (UTF-8 and UTF-16).
//!
//! Both constructors are ordinary branded objects: script properties live on
//! the instance header, and the decoder state (`prefix`, `pending`) is the only
//! part that must survive a snapshot boundary. No host operation and no opcode
//! takes part, so a decode never suspends.
//!
//! The decoder never copies the whole input. An incomplete prefix left by a
//! streaming call is carried in a four-byte stack buffer and completed from the
//! next input; only the fatal-error path copies the bytes it must retain for the
//! following call.

use super::text_codec::{DecoderState, Utf8Encoder, scalar_at};
use super::{Completion, arg};
use crate::builtin::globals::Owner;
use crate::builtin::{
    FUNCTION_PARENT, OBJECT_PARENT, RootKind, RootSpec, define_builtin_data, link_constructor,
};
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{
    Attributes, DescriptorUpdate, Object, PlainObject, TO_STRING_TAG_SYMBOL, TextDecoderConfig,
    TextDecoderObject, TextEncoding, ViewRange,
};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::Invocation;
use crate::value::{JsString, ObjectId, PropertyKey, Value, ValueRef};
use crate::vm::VmCore;

pub(crate) const ENCODER_ROOT: &str = "textEncoder";
pub(crate) const ENCODER_PROTOTYPE: &str = "textEncoder.prototype";
pub(crate) const DECODER_ROOT: &str = "textDecoder";
pub(crate) const DECODER_PROTOTYPE: &str = "textDecoder.prototype";

const ENCODER: IntrinsicKey = IntrinsicKey::new("TextEncoder", Slot::Global, "");
const ENCODING: IntrinsicKey = IntrinsicKey::new("TextEncoder", Slot::PrototypeGetter, "encoding");
const ENCODE: IntrinsicKey = IntrinsicKey::new("TextEncoder", Slot::Prototype, "encode");
const ENCODE_INTO: IntrinsicKey = IntrinsicKey::new("TextEncoder", Slot::Prototype, "encodeInto");

const DECODER: IntrinsicKey = IntrinsicKey::new("TextDecoder", Slot::Global, "");
const DECODER_ENCODING: IntrinsicKey =
    IntrinsicKey::new("TextDecoder", Slot::PrototypeGetter, "encoding");
const FATAL: IntrinsicKey = IntrinsicKey::new("TextDecoder", Slot::PrototypeGetter, "fatal");
const IGNORE_BOM: IntrinsicKey =
    IntrinsicKey::new("TextDecoder", Slot::PrototypeGetter, "ignoreBOM");
const DECODE: IntrinsicKey = IntrinsicKey::new("TextDecoder", Slot::Prototype, "decode");

/// Every label the Encoding Standard maps to UTF-8.
const UTF8_LABELS: &[&str] = &[
    "unicode-1-1-utf-8",
    "unicode11utf8",
    "unicode20utf8",
    "utf-8",
    "utf8",
    "x-unicode20utf8",
];

const ENCODER_INTRINSICS: &[IntrinsicDef] = &[
    IntrinsicDef::constructor(
        ENCODER,
        0,
        |vm, _, roots, _, _| vm.throw_type_error(roots, "TextEncoder constructor requires new"),
        construct_encoder,
    ),
    IntrinsicDef::call(ENCODING, 0, |vm, _, roots, _, receiver| {
        match vm.require_encoder(roots, receiver)? {
            Ok(_) => Ok(Invocation::Value(Value::string("utf-8".into()))),
            Err(error) => Ok(Invocation::Thrown(error)),
        }
    }),
    IntrinsicDef::call(ENCODE, 0, |vm, host, roots, args, receiver| {
        vm.text_encode(host, roots, receiver, &args)
    }),
    IntrinsicDef::call(ENCODE_INTO, 2, |vm, host, roots, args, receiver| {
        vm.text_encode_into(host, roots, receiver, &args)
    }),
];

const DECODER_INTRINSICS: &[IntrinsicDef] = &[
    IntrinsicDef::constructor(
        DECODER,
        0,
        |vm, _, roots, _, _| vm.throw_type_error(roots, "TextDecoder constructor requires new"),
        construct_decoder,
    ),
    IntrinsicDef::call(DECODER_ENCODING, 0, |vm, _, roots, _, receiver| {
        match vm.text_decoder_state(receiver)? {
            Some(state) => Ok(Invocation::Value(encoding_value(state.encoding))),
            None => vm.throw_type_error(
                roots,
                "TextDecoder method called on an incompatible receiver",
            ),
        }
    }),
    IntrinsicDef::call(FATAL, 0, |vm, _, roots, _, receiver| {
        match vm.text_decoder_state(receiver)? {
            Some(state) => Ok(Invocation::Value(Value::boolean(state.fatal))),
            None => vm.throw_type_error(
                roots,
                "TextDecoder method called on an incompatible receiver",
            ),
        }
    }),
    IntrinsicDef::call(IGNORE_BOM, 0, |vm, _, roots, _, receiver| {
        match vm.text_decoder_state(receiver)? {
            Some(state) => Ok(Invocation::Value(Value::boolean(state.ignore_bom))),
            None => vm.throw_type_error(
                roots,
                "TextDecoder method called on an incompatible receiver",
            ),
        }
    }),
    IntrinsicDef::call(DECODE, 0, |vm, host, roots, args, receiver| {
        vm.text_decode(host, roots, receiver, &args)
    }),
];

pub(crate) const ENCODER_OWNER: Owner = Owner::new(
    ENCODER_ROOT,
    &[
        RootSpec::new(ENCODER_ROOT, RootKind::Native(ENCODER), FUNCTION_PARENT)
            .fixed(ENCODER_PROTOTYPE)
            .backlink(Some(Attributes::METHOD)),
        RootSpec::new(ENCODER_PROTOTYPE, RootKind::Plain, OBJECT_PARENT),
    ],
    &[ENCODER_INTRINSICS],
    Some(install_encoder),
);

pub(crate) const DECODER_OWNER: Owner = Owner::new(
    DECODER_ROOT,
    &[
        RootSpec::new(DECODER_ROOT, RootKind::Native(DECODER), FUNCTION_PARENT)
            .fixed(DECODER_PROTOTYPE)
            .backlink(Some(Attributes::METHOD)),
        RootSpec::new(DECODER_PROTOTYPE, RootKind::Plain, OBJECT_PARENT),
    ],
    &[DECODER_INTRINSICS],
    Some(install_decoder),
);

fn install_encoder(vm: &mut VmCore, _: &Value) {
    let constructor = vm.builtins.get(ENCODER_ROOT);
    let prototype = vm.builtins.get(ENCODER_PROTOTYPE);
    link_constructor(vm, constructor, prototype);
    operation(vm, prototype, "encode", ENCODE);
    operation(vm, prototype, "encodeInto", ENCODE_INTO);
    accessor(vm, prototype, "encoding", ENCODING);
    define_builtin_data(
        vm,
        prototype,
        PropertyKey::Symbol(TO_STRING_TAG_SYMBOL),
        Value::string("TextEncoder".into()),
        Attributes::FUNCTION_METADATA,
    );
}

fn install_decoder(vm: &mut VmCore, _: &Value) {
    let constructor = vm.builtins.get(DECODER_ROOT);
    let prototype = vm.builtins.get(DECODER_PROTOTYPE);
    link_constructor(vm, constructor, prototype);
    operation(vm, prototype, "decode", DECODE);
    for (name, key) in [
        ("encoding", DECODER_ENCODING),
        ("fatal", FATAL),
        ("ignoreBOM", IGNORE_BOM),
    ] {
        accessor(vm, prototype, name, key);
    }
    define_builtin_data(
        vm,
        prototype,
        PropertyKey::Symbol(TO_STRING_TAG_SYMBOL),
        Value::string("TextDecoder".into()),
        Attributes::FUNCTION_METADATA,
    );
}

/// Install a Web IDL operation: writable, enumerable, configurable.
fn operation(vm: &mut VmCore, target: ObjectId, key: &str, intrinsic: IntrinsicKey) {
    let function = vm.alloc_intrinsic(intrinsic);
    define_builtin_data(
        vm,
        target,
        key,
        Value::object(function),
        Attributes::ORDINARY,
    );
}

/// Install a Web IDL readonly attribute: an enumerable, configurable getter.
fn accessor(vm: &mut VmCore, target: ObjectId, key: &str, intrinsic: IntrinsicKey) {
    let getter = vm.alloc_intrinsic(intrinsic);
    assert!(
        vm.heap
            .apply_descriptor(
                target,
                key.into(),
                DescriptorUpdate {
                    get: Some(Some(getter)),
                    enumerable: Some(true),
                    configurable: Some(true),
                    ..DescriptorUpdate::default()
                }
            )
            .expect("text builtin getter")
    );
}

fn construct_encoder(
    vm: &mut VmCore,
    _: &Host,
    _: &ActiveRoots<'_>,
    _: Vec<Value>,
    prototype: Option<ObjectId>,
) -> Result<Invocation, VmError> {
    let prototype = prototype.unwrap_or_else(|| vm.builtins.get(ENCODER_PROTOTYPE));
    let object = vm.heap.alloc(Object::TextEncoder {
        object: PlainObject::new(Some(prototype)),
    });
    Ok(Invocation::Value(Value::object(object)))
}

fn construct_decoder(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    prototype: Option<ObjectId>,
) -> Result<Invocation, VmError> {
    let roots = &roots.with(&args);
    let config = match decoder_config(vm, host, roots, &args)? {
        Ok(config) => config,
        Err(error) => return Ok(Invocation::Thrown(error)),
    };
    let prototype = prototype.unwrap_or_else(|| vm.builtins.get(DECODER_PROTOTYPE));
    let object = vm.heap.alloc(Object::TextDecoder(TextDecoderObject {
        object: PlainObject::new(Some(prototype)),
        encoding: config.encoding,
        fatal: config.fatal,
        ignore_bom: config.ignore_bom,
        streaming: false,
        bom_seen: false,
        prefix: [0; 3],
        prefix_len: 0,
        pending: Vec::new(),
    }));
    Ok(Invocation::Value(Value::object(object)))
}

pub(in crate::builtin) fn decoder_config(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    args: &[Value],
) -> Completion<TextDecoderConfig> {
    let label = match arg(args, 0).view() {
        ValueRef::Undefined => "utf-8".to_owned(),
        _ => match vm.coerce_string(host, roots, arg(args, 0))? {
            Ok(text) => text.to_utf8().unwrap_or_default(),
            Err(error) => return Ok(Err(error)),
        },
    };
    let (fatal, ignore_bom) = match decoder_options(vm, host, roots, arg(args, 1))? {
        Ok(values) => values,
        Err(error) => return Ok(Err(error)),
    };
    let Some(encoding) = canonical_label(&label) else {
        let error = vm.make_range_error(roots, "unsupported TextDecoder encoding label")?;
        return Ok(Err(error));
    };
    Ok(Ok(TextDecoderConfig {
        encoding,
        fatal,
        ignore_bom,
    }))
}

pub(in crate::builtin) fn encoding_value(encoding: TextEncoding) -> Value {
    let name = match encoding {
        TextEncoding::Utf8 => "utf-8",
        TextEncoding::Utf16Le => "utf-16le",
        TextEncoding::Utf16Be => "utf-16be",
    };
    Value::string(name.into())
}

/// Strip ASCII whitespace and ASCII-lowercase, then look the label up.
fn canonical_label(label: &str) -> Option<TextEncoding> {
    let trimmed = label.trim_matches(|c: char| matches!(c, '\t' | '\n' | '\u{0c}' | '\r' | ' '));
    let lowered = trimmed.to_ascii_lowercase();
    match lowered.as_str() {
        "unicodefffe" | "utf-16be" => Some(TextEncoding::Utf16Be),
        "csunicode" | "iso-10646-ucs-2" | "ucs-2" | "unicode" | "unicodefeff" | "utf-16"
        | "utf-16le" => Some(TextEncoding::Utf16Le),
        label if UTF8_LABELS.contains(&label) => Some(TextEncoding::Utf8),
        _ => None,
    }
}

/// The `TextDecoderOptions` dictionary: undefined and null are empty, any
/// other non-object is a TypeError. Members are read in lexicographic order.
fn decoder_options(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    options: &Value,
) -> Completion<(bool, bool)> {
    let id = match options_dictionary(vm, roots, options)? {
        Ok(Some(id)) => id,
        Ok(None) => return Ok(Ok((false, false))),
        Err(error) => return Ok(Err(error)),
    };
    let fatal = match vm.boolean_option(host, roots, Some(id), "fatal")? {
        Ok(value) => value,
        Err(error) => return Ok(Err(error)),
    };
    let ignore_bom = match vm.boolean_option(host, roots, Some(id), "ignoreBOM")? {
        Ok(value) => value,
        Err(error) => return Ok(Err(error)),
    };
    Ok(Ok((fatal, ignore_bom)))
}

/// `TextDecodeOptions`: undefined and null are empty, other non-objects throw.
fn stream_option(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    options: &Value,
) -> Completion<bool> {
    let id = match options_dictionary(vm, roots, options)? {
        Ok(id) => id,
        Err(error) => return Ok(Err(error)),
    };
    vm.boolean_option(host, roots, id, "stream")
}

fn options_dictionary(
    vm: &mut VmCore,
    roots: &ActiveRoots<'_>,
    options: &Value,
) -> Completion<Option<ObjectId>> {
    match options.view() {
        ValueRef::Undefined | ValueRef::Null => Ok(Ok(None)),
        ValueRef::Object(id) => Ok(Ok(Some(id))),
        _ => Ok(Err(
            vm.type_error(roots, "TextDecoder options must be an object")?
        )),
    }
}

/// A buffer source the decoder may read: its bytes are resolved after the
/// `stream` getter, because that getter can detach or resize the buffer.
#[derive(Clone, Copy)]
pub(in crate::builtin) enum Source {
    Empty,
    Buffer(ObjectId),
    View(ObjectId),
}

pub(in crate::builtin) fn buffer_source(
    vm: &mut VmCore,
    roots: &ActiveRoots<'_>,
    value: &Value,
) -> Completion<Source> {
    let ValueRef::Object(id) = value.view() else {
        return match value.view() {
            ValueRef::Undefined => Ok(Ok(Source::Empty)),
            _ => Ok(Err(vm.type_error(
                roots,
                "TextDecoder input must be a buffer source",
            )?)),
        };
    };
    if vm.heap.buffer(id)?.is_some() {
        return Ok(Ok(Source::Buffer(id)));
    }
    if vm.heap.data_view(id)?.is_some() || vm.heap.typed_array(id)?.is_some() {
        return Ok(Ok(Source::View(id)));
    }
    Ok(Err(vm.type_error(
        roots,
        "TextDecoder input must be a buffer source",
    )?))
}

/// The raw byte window of a validated buffer source. A detached or
/// out-of-bounds view contributes no bytes.
pub(in crate::builtin) fn source_bytes(vm: &VmCore, source: Source) -> Result<&[u8], VmError> {
    match source {
        Source::Empty => Ok(&[]),
        Source::Buffer(id) => Ok(vm
            .heap
            .buffer(id)?
            .and_then(|buffer| buffer.bytes.as_deref())
            .unwrap_or(&[])),
        Source::View(id) => {
            let (buffer, range) = if let Some(view) = vm.heap.typed_array(id)? {
                (view.buffer, vm.heap.typed_range(view)?)
            } else {
                let view = vm.heap.data_view(id)?.expect("buffer source was validated");
                (view.buffer, vm.heap.data_view_range(view)?)
            };
            let ViewRange::Bytes { start, len } = range else {
                return Ok(&[]);
            };
            let bytes = vm.heap.view_bytes(buffer)?.unwrap_or(&[]);
            Ok(&bytes[start..start + len])
        }
    }
}

impl VmCore {
    /// The decoder record behind a branded receiver, or `None`.
    fn text_decoder_state(&self, receiver: &Value) -> Result<Option<&TextDecoderObject>, VmError> {
        let ValueRef::Object(id) = receiver.view() else {
            return Ok(None);
        };
        Ok(match self.heap.get(id)? {
            Object::TextDecoder(state) => Some(state),
            _ => None,
        })
    }

    fn require_encoder(
        &mut self,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Completion<ObjectId> {
        let ValueRef::Object(id) = receiver.view() else {
            return Ok(Err(self.type_error(
                roots,
                "TextEncoder method called on an incompatible receiver",
            )?));
        };
        if !matches!(self.heap.get(id)?, Object::TextEncoder { .. }) {
            return Ok(Err(self.type_error(
                roots,
                "TextEncoder method called on an incompatible receiver",
            )?));
        }
        Ok(Ok(id))
    }

    fn require_decoder(
        &mut self,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Completion<ObjectId> {
        let ValueRef::Object(id) = receiver.view() else {
            return Ok(Err(self.type_error(
                roots,
                "TextDecoder method called on an incompatible receiver",
            )?));
        };
        if !matches!(self.heap.get(id)?, Object::TextDecoder(_)) {
            return Ok(Err(self.type_error(
                roots,
                "TextDecoder method called on an incompatible receiver",
            )?));
        }
        Ok(Ok(id))
    }

    pub(super) fn text_encode(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let view = (receiver, args);
        let roots = &roots.with(&view);
        match self.require_encoder(roots, receiver)? {
            Ok(_) => {}
            Err(error) => return Ok(Invocation::Thrown(error)),
        }
        let text = match arg(args, 0).view() {
            ValueRef::Undefined => JsString::new(),
            _ => match self.coerce_string(host, roots, arg(args, 0))? {
                Ok(text) => text,
                Err(error) => return Ok(Invocation::Thrown(error)),
            },
        };
        let bytes = Utf8Encoder::default().encode(text.units(), true);
        match self.uint8_from_bytes(host, roots, &bytes)? {
            Ok(value) => Ok(Invocation::Value(value)),
            Err(error) => Ok(Invocation::Thrown(error)),
        }
    }

    pub(super) fn text_encode_into(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let view = (receiver, args);
        let roots = &roots.with(&view);
        match self.require_encoder(roots, receiver)? {
            Ok(_) => {}
            Err(error) => return Ok(Invocation::Thrown(error)),
        }
        if args.len() < 2 {
            return self.throw_type_error(roots, "encodeInto requires a source and a destination");
        }
        let source = match self.coerce_string(host, roots, arg(args, 0))? {
            Ok(text) => text,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let destination = match self.validate_uint8(roots, arg(args, 1))? {
            Ok(id) => id,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let (buffer, start, capacity) = {
            let Some(view) = self.heap.typed_array(destination)? else {
                unreachable!("destination was validated");
            };
            let buffer = view.buffer;
            match self.heap.typed_range(view)? {
                ViewRange::Bytes { start, len } => (buffer, start, len),
                ViewRange::Detached | ViewRange::OutOfBounds => (buffer, 0, 0),
            }
        };
        let mut read = 0usize;
        let mut written = 0usize;
        if capacity > 0
            && let Object::ArrayBuffer(block) = self.heap.get_mut(buffer)?
            && let Some(bytes) = block.bytes.as_mut()
        {
            let capacity = capacity.min(bytes.len().saturating_sub(start));
            let units = source.to_utf16();
            let mut index = 0;
            let mut encoded = [0u8; 4];
            while index < units.len() {
                let (scalar, consumed) = scalar_at(&units, index);
                let len = scalar.encode_utf8(&mut encoded).len();
                if written + len > capacity {
                    break;
                }
                bytes[start + written..start + written + len].copy_from_slice(&encoded[..len]);
                written += len;
                read += consumed;
                index += consumed;
            }
        }
        self.codec_result(roots, read, written, None)
    }

    pub(super) fn text_decode(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let view = (receiver, args);
        let roots = &roots.with(&view);
        let id = match self.require_decoder(roots, receiver)? {
            Ok(id) => id,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        // Validate the shape first, then run the `stream` getter, and only
        // then resolve the bytes: the getter can resize or detach the source.
        let source = match buffer_source(self, roots, arg(args, 0))? {
            Ok(source) => source,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let stream = match stream_option(self, host, roots, arg(args, 1))? {
            Ok(stream) => stream,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };

        // Keep the snapshot record's flat layout. Move any retained bytes
        // into the codec while borrowing the current input from the heap.
        let Object::TextDecoder(record) = self.heap.get_mut(id)? else {
            unreachable!("receiver was validated");
        };
        let (encoding, fatal, ignore_bom) = (record.encoding, record.fatal, record.ignore_bom);
        let mut state = DecoderState {
            streaming: record.streaming,
            bom_seen: record.bom_seen,
            prefix: record.prefix,
            prefix_len: record.prefix_len,
            pending: std::mem::take(&mut record.pending),
        };
        let result = state.decode(
            encoding,
            fatal,
            ignore_bom,
            source_bytes(self, source)?,
            stream,
        );
        let Object::TextDecoder(record) = self.heap.get_mut(id)? else {
            unreachable!("receiver was validated");
        };
        record.streaming = state.streaming;
        record.bom_seen = state.bom_seen;
        record.prefix = state.prefix;
        record.prefix_len = state.prefix_len;
        record.pending = state.pending;
        match result {
            Ok(text) => Ok(Invocation::Value(Value::string(text.into()))),
            Err(_) => self.throw_type_error(roots, "TextDecoder fatal error"),
        }
    }
}
