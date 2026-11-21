//! `TextEncoder` and `TextDecoder`, UTF-8 only.
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

use std::str;

use super::{Completion, arg};
use crate::builtin::globals::Owner;
use crate::builtin::{
    FUNCTION_PARENT, OBJECT_PARENT, RootKind, RootSpec, define_builtin_data, link_constructor,
};
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{
    Attributes, DescriptorUpdate, Object, PlainObject, TO_STRING_TAG_SYMBOL, TextDecoderObject,
    ViewRange,
};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::Invocation;
use crate::value::{JsString, ObjectId, PropertyKey, Value, ValueRef};
use crate::vm::Vm;

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

/// Every label the Encoding Standard maps to UTF-8. Nothing else is accepted.
const LABELS: &[&str] = &[
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
        |vm, _, _, _, _| vm.throw_type_error("TextEncoder constructor requires new"),
        construct_encoder,
    ),
    IntrinsicDef::call(ENCODING, 0, |vm, _, _, _, receiver| {
        match vm.require_encoder(receiver)? {
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
        |vm, _, _, _, _| vm.throw_type_error("TextDecoder constructor requires new"),
        construct_decoder,
    ),
    IntrinsicDef::call(DECODER_ENCODING, 0, |vm, _, _, _, receiver| {
        match vm.text_decoder_state(receiver)? {
            Some(_) => Ok(Invocation::Value(Value::string("utf-8".into()))),
            None => vm.throw_type_error("TextDecoder method called on an incompatible receiver"),
        }
    }),
    IntrinsicDef::call(FATAL, 0, |vm, _, _, _, receiver| {
        match vm.text_decoder_state(receiver)? {
            Some(state) => Ok(Invocation::Value(Value::boolean(state.fatal))),
            None => vm.throw_type_error("TextDecoder method called on an incompatible receiver"),
        }
    }),
    IntrinsicDef::call(IGNORE_BOM, 0, |vm, _, _, _, receiver| {
        match vm.text_decoder_state(receiver)? {
            Some(state) => Ok(Invocation::Value(Value::boolean(state.ignore_bom))),
            None => vm.throw_type_error("TextDecoder method called on an incompatible receiver"),
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

fn install_encoder(vm: &mut Vm, _: &Value) {
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

fn install_decoder(vm: &mut Vm, _: &Value) {
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
fn operation(vm: &mut Vm, target: ObjectId, key: &str, intrinsic: IntrinsicKey) {
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
fn accessor(vm: &mut Vm, target: ObjectId, key: &str, intrinsic: IntrinsicKey) {
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
    vm: &mut Vm,
    _: &mut dyn Host,
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
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    prototype: Option<ObjectId>,
) -> Result<Invocation, VmError> {
    let roots = &roots.with(&args);
    let label = match arg(&args, 0).view() {
        ValueRef::Undefined => "utf-8".to_owned(),
        _ => match vm.coerce_string(host, roots, arg(&args, 0))? {
            Ok(text) => text.to_utf8().unwrap_or_default(),
            Err(error) => return Ok(Invocation::Thrown(error)),
        },
    };
    let (fatal, ignore_bom) = match decoder_options(vm, host, roots, arg(&args, 1))? {
        Ok(values) => values,
        Err(error) => return Ok(Invocation::Thrown(error)),
    };
    if canonical_label(&label).is_none() {
        return Ok(Invocation::Thrown(
            vm.make_range_error("unsupported TextDecoder encoding label")?,
        ));
    }
    let prototype = prototype.unwrap_or_else(|| vm.builtins.get(DECODER_PROTOTYPE));
    let object = vm.heap.alloc(Object::TextDecoder(TextDecoderObject {
        object: PlainObject::new(Some(prototype)),
        fatal,
        ignore_bom,
        streaming: false,
        bom_seen: false,
        prefix: [0; 3],
        prefix_len: 0,
        pending: Vec::new(),
    }));
    Ok(Invocation::Value(Value::object(object)))
}

/// Strip ASCII whitespace and ASCII-lowercase, then look the label up.
fn canonical_label(label: &str) -> Option<&'static str> {
    let trimmed = label.trim_matches(|c: char| matches!(c, '\t' | '\n' | '\u{0c}' | '\r' | ' '));
    let lowered = trimmed.to_ascii_lowercase();
    LABELS.iter().copied().find(|label| *label == lowered)
}

/// The `TextDecoderOptions` dictionary: undefined and null are empty, any
/// other non-object is a TypeError. Members are read in lexicographic order.
fn decoder_options(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    options: &Value,
) -> Completion<(bool, bool)> {
    let id = match options_dictionary(vm, options)? {
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
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    options: &Value,
) -> Completion<bool> {
    let id = match options_dictionary(vm, options)? {
        Ok(id) => id,
        Err(error) => return Ok(Err(error)),
    };
    vm.boolean_option(host, roots, id, "stream")
}

fn options_dictionary(vm: &mut Vm, options: &Value) -> Completion<Option<ObjectId>> {
    match options.view() {
        ValueRef::Undefined | ValueRef::Null => Ok(Ok(None)),
        ValueRef::Object(id) => Ok(Ok(Some(id))),
        _ => Ok(Err(vm.type_error("TextDecoder options must be an object")?)),
    }
}

/// A buffer source the decoder may read: its bytes are resolved after the
/// `stream` getter, because that getter can detach or resize the buffer.
#[derive(Clone, Copy)]
enum Source {
    Empty,
    Buffer(ObjectId),
    View(ObjectId),
}

fn buffer_source(vm: &mut Vm, value: &Value) -> Completion<Source> {
    let ValueRef::Object(id) = value.view() else {
        return match value.view() {
            ValueRef::Undefined => Ok(Ok(Source::Empty)),
            _ => Ok(Err(
                vm.type_error("TextDecoder input must be a buffer source")?
            )),
        };
    };
    if vm.heap.buffer(id)?.is_some() {
        return Ok(Ok(Source::Buffer(id)));
    }
    if vm.heap.data_view(id)?.is_some() || vm.heap.typed_array(id)?.is_some() {
        return Ok(Ok(Source::View(id)));
    }
    Ok(Err(
        vm.type_error("TextDecoder input must be a buffer source")?
    ))
}

/// The raw byte window of a validated buffer source. A detached or
/// out-of-bounds view contributes no bytes.
fn source_bytes(vm: &Vm, source: Source) -> Result<&[u8], VmError> {
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

impl Vm {
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

    fn require_encoder(&mut self, receiver: &Value) -> Completion<ObjectId> {
        let ValueRef::Object(id) = receiver.view() else {
            return Ok(Err(self.type_error(
                "TextEncoder method called on an incompatible receiver",
            )?));
        };
        if !matches!(self.heap.get(id)?, Object::TextEncoder { .. }) {
            return Ok(Err(self.type_error(
                "TextEncoder method called on an incompatible receiver",
            )?));
        }
        Ok(Ok(id))
    }

    fn require_decoder(&mut self, receiver: &Value) -> Completion<ObjectId> {
        let ValueRef::Object(id) = receiver.view() else {
            return Ok(Err(self.type_error(
                "TextDecoder method called on an incompatible receiver",
            )?));
        };
        if !matches!(self.heap.get(id)?, Object::TextDecoder(_)) {
            return Ok(Err(self.type_error(
                "TextDecoder method called on an incompatible receiver",
            )?));
        }
        Ok(Ok(id))
    }

    pub(super) fn text_encode(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let view = (receiver, args);
        let roots = &roots.with(&view);
        match self.require_encoder(receiver)? {
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
        let bytes = utf8_bytes(&text);
        match self.uint8_from_bytes(host, roots, &bytes)? {
            Ok(value) => Ok(Invocation::Value(value)),
            Err(error) => Ok(Invocation::Thrown(error)),
        }
    }

    pub(super) fn text_encode_into(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let view = (receiver, args);
        let roots = &roots.with(&view);
        match self.require_encoder(receiver)? {
            Ok(_) => {}
            Err(error) => return Ok(Invocation::Thrown(error)),
        }
        if args.len() < 2 {
            return self.throw_type_error("encodeInto requires a source and a destination");
        }
        let source = match self.coerce_string(host, roots, arg(args, 0))? {
            Ok(text) => text,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let destination = match self.validate_uint8(arg(args, 1))? {
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
        self.codec_result(read, written, None)
    }

    pub(super) fn text_decode(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let view = (receiver, args);
        let roots = &roots.with(&view);
        let id = match self.require_decoder(receiver)? {
            Ok(id) => id,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        // Validate the shape first, then run the `stream` getter, and only
        // then resolve the bytes: the getter can resize or detach the source.
        let source = match buffer_source(self, arg(args, 0))? {
            Ok(source) => source,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let stream = match stream_option(self, host, roots, arg(args, 1))? {
            Ok(stream) => stream,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };

        let mut decoder = {
            let Object::TextDecoder(record) = self.heap.get_mut(id)? else {
                unreachable!("receiver was validated");
            };
            if !record.streaming {
                record.prefix = [0; 3];
                record.prefix_len = 0;
                record.pending.clear();
                record.bom_seen = false;
            }
            record.streaming = stream;
            let pending = std::mem::take(&mut record.pending);
            Utf8Decoder {
                fatal: record.fatal,
                ignore_bom: record.ignore_bom,
                bom_seen: record.bom_seen,
                prefix: record.prefix,
                prefix_len: usize::from(record.prefix_len),
                out: String::new(),
                retained: Vec::new(),
                pending,
            }
        };

        let bom_seen_before = decoder.bom_seen;
        let complete = {
            let bytes = source_bytes(self, source)?;
            decoder.feed(bytes, !stream)
        };

        let Object::TextDecoder(record) = self.heap.get_mut(id)? else {
            unreachable!("receiver was validated");
        };
        // A fatal call never serializes its output, so it must not advance the
        // stream's BOM mark.
        record.bom_seen = if complete {
            decoder.bom_seen
        } else {
            bom_seen_before
        };
        if complete {
            record.prefix = decoder.prefix;
            record.prefix_len = decoder.prefix_len as u8;
            record.pending.clear();
            Ok(Invocation::Value(Value::string(decoder.out.into())))
        } else {
            record.prefix = [0; 3];
            record.prefix_len = 0;
            record.pending = if stream { decoder.retained } else { Vec::new() };
            self.throw_type_error("TextDecoder fatal error")
        }
    }
}

/// One scalar from UTF-16 units: a surrogate pair, a lone surrogate as U+FFFD,
/// or one BMP code unit. The second value is the units consumed.
fn scalar_at(units: &[u16], index: usize) -> (char, usize) {
    let first = units[index];
    if (0xd800..=0xdbff).contains(&first)
        && let Some(&second) = units.get(index + 1)
        && (0xdc00..=0xdfff).contains(&second)
    {
        let code = 0x10000 + ((u32::from(first) - 0xd800) << 10) + (u32::from(second) - 0xdc00);
        return (char::from_u32(code).expect("surrogate pair is a scalar"), 2);
    }
    if (0xd800..=0xdfff).contains(&first) {
        return ('\u{fffd}', 1);
    }
    (
        char::from_u32(u32::from(first)).expect("BMP unit is a scalar"),
        1,
    )
}

fn utf8_bytes(text: &JsString) -> Vec<u8> {
    let units = text.to_utf16();
    let mut out = Vec::with_capacity(units.len() * 3);
    let mut index = 0;
    let mut encoded = [0u8; 4];
    while index < units.len() {
        let (scalar, consumed) = scalar_at(&units, index);
        out.extend_from_slice(scalar.encode_utf8(&mut encoded).as_bytes());
        index += consumed;
    }
    out
}

/// Streaming UTF-8 state for one `decode` call.
struct Utf8Decoder {
    fatal: bool,
    ignore_bom: bool,
    bom_seen: bool,
    prefix: [u8; 3],
    prefix_len: usize,
    out: String,
    /// Bytes after a consumed fatal error, kept only while the stream is open.
    retained: Vec<u8>,
    pending: Vec<u8>,
}

impl Utf8Decoder {
    fn push(&mut self, scalar: char) {
        if !self.ignore_bom && !self.bom_seen {
            self.bom_seen = true;
            if scalar == '\u{feff}' {
                return;
            }
        }
        self.out.push(scalar);
    }

    fn push_str(&mut self, text: &str) {
        for scalar in text.chars() {
            self.push(scalar);
        }
    }

    /// Feed the retained bytes followed by this call's input. Returns false
    /// when a fatal error stopped decoding.
    fn feed(&mut self, input: &[u8], last: bool) -> bool {
        let pending = std::mem::take(&mut self.pending);
        self.out.reserve(pending.len() + input.len());
        let mut cursor = Cursor {
            sources: [&pending, input],
            index: 0,
            at: 0,
        };
        let mut buffer = [0u8; 4];
        let mut len = self.prefix_len;
        buffer[..len].copy_from_slice(&self.prefix[..len]);
        loop {
            if len > 0 {
                while len < 4
                    && let Some(rest) = cursor.rest()
                {
                    let take = (4 - len).min(rest.len());
                    buffer[len..len + take].copy_from_slice(&rest[..take]);
                    cursor.take(take);
                    len += take;
                }
                // The buffer may already hold a complete scalar even when it
                // is shorter than four bytes, and four bytes may hold a valid
                // scalar followed by an incomplete tail.
                match str::from_utf8(&buffer[..len]) {
                    Ok(text) => {
                        self.push_str(text);
                        len = 0;
                        continue;
                    }
                    Err(error) => {
                        let valid = error.valid_up_to();
                        if valid > 0 {
                            self.push_str(
                                str::from_utf8(&buffer[..valid]).expect("valid_up_to prefix"),
                            );
                        }
                        match error.error_len() {
                            Some(bad) => {
                                if self.fatal {
                                    let start = valid + bad;
                                    self.retained.clear();
                                    self.retained.extend_from_slice(&buffer[start..len]);
                                    cursor.copy_remainder(&mut self.retained);
                                    self.prefix_len = 0;
                                    return false;
                                }
                                self.push('\u{fffd}');
                                let start = valid + bad;
                                buffer.copy_within(start..len, 0);
                                len -= start;
                                continue;
                            }
                            None => {
                                let tail = len - valid;
                                buffer.copy_within(valid..len, 0);
                                len = tail;
                                if cursor.rest().is_some() {
                                    continue;
                                }
                                if last {
                                    if self.fatal {
                                        self.prefix_len = 0;
                                        self.retained.clear();
                                        return false;
                                    }
                                    self.push('\u{fffd}');
                                    len = 0;
                                }
                                break;
                            }
                        }
                    }
                }
            }
            let Some(rest) = cursor.rest() else {
                break;
            };
            match str::from_utf8(rest) {
                Ok(text) => {
                    self.push_str(text);
                    cursor.take(text.len());
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    if valid > 0 {
                        self.push_str(str::from_utf8(&rest[..valid]).expect("valid_up_to prefix"));
                        cursor.take(valid);
                    }
                    let rest = &rest[valid..];
                    match error.error_len() {
                        Some(bad) => {
                            if self.fatal {
                                self.retained.clear();
                                self.retained.extend_from_slice(&rest[bad..]);
                                cursor.skip_source();
                                cursor.copy_remainder(&mut self.retained);
                                self.prefix_len = 0;
                                return false;
                            }
                            self.push('\u{fffd}');
                            cursor.take(bad);
                        }
                        None => {
                            // Keep the incomplete suffix; the next iteration
                            // completes it from the following source or flushes.
                            self.prefix[..rest.len()].copy_from_slice(rest);
                            self.prefix_len = rest.len();
                            buffer[..rest.len()].copy_from_slice(rest);
                            len = rest.len();
                            cursor.take(rest.len());
                        }
                    }
                }
            }
        }
        if last {
            self.prefix_len = 0;
        } else {
            self.prefix[..len].copy_from_slice(&buffer[..len]);
            self.prefix_len = len;
        }
        true
    }
}

struct Cursor<'a> {
    sources: [&'a [u8]; 2],
    index: usize,
    at: usize,
}

impl<'a> Cursor<'a> {
    /// The unread remainder of the current source, skipping exhausted ones.
    fn rest(&mut self) -> Option<&'a [u8]> {
        while self.index < self.sources.len() && self.at == self.sources[self.index].len() {
            self.index += 1;
            self.at = 0;
        }
        (self.index < self.sources.len()).then(|| &self.sources[self.index][self.at..])
    }

    fn take(&mut self, count: usize) {
        self.at += count;
    }

    /// Consume the rest of the current source; the caller embeds it explicitly.
    fn skip_source(&mut self) {
        self.at = self.sources[self.index].len();
    }

    /// Append the unread remainder of the current source and every later one.
    fn copy_remainder(&self, out: &mut Vec<u8>) {
        if self.index < self.sources.len() {
            out.extend_from_slice(&self.sources[self.index][self.at..]);
            for source in &self.sources[self.index + 1..] {
                out.extend_from_slice(source);
            }
        }
    }
}
