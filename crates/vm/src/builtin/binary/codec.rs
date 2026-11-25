//! Uint8Array base64 and hex codecs.
//!
//! Both directions are pure: no user code runs once the input string is
//! extracted, so the byte lengths observed before decoding still hold when the
//! decoded bytes are written back.

use super::{Completion, arg, element};
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{ElementKind, Object, TO_STRING_TAG_SYMBOL};
use crate::host::Host;
use crate::runtime::{Arguments, Invocation};
use crate::value::{JsString, ObjectId, PropertyKeyRef, Value, ValueRef};
use crate::vm::VmCore;

const STANDARD: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Alphabet {
    Base64,
    Url,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LastChunk {
    Loose,
    Strict,
    StopBeforePartial,
}

/// FromBase64 / FromHex result: characters consumed, bytes produced, and the
/// SyntaxError message when decoding stopped early.
struct Decoded {
    read: usize,
    bytes: Vec<u8>,
    error: Option<&'static str>,
}

fn value_of(character: u16) -> Option<u8> {
    STANDARD
        .iter()
        .position(|&value| u16::from(value) == character)
        .map(|index| index as u8)
}

/// Skip TAB, LF, FF, CR, and SPACE between base64 characters.
fn skip_whitespace(units: &JsString, mut index: usize) -> usize {
    while let Some(character) = units.unit_at(index) {
        if !matches!(character, 0x09 | 0x0a | 0x0c | 0x0d | 0x20) {
            break;
        }
        index += 1;
    }
    index
}

fn decode_chunk(chunk: &[u16]) -> Option<[u8; 3]> {
    let mut values = [0u8; 4];
    for (slot, character) in values.iter_mut().zip(chunk) {
        *slot = value_of(*character)?;
    }
    let [a, b, c, d] = values.map(u32::from);
    Some([
        ((a << 2) | (b >> 4)) as u8,
        (((b & 0x0f) << 4) | (c >> 2)) as u8,
        (((c & 0x03) << 6) | d) as u8,
    ])
}

fn decode_final_chunk(chunk: &[u16], throw_on_extra_bits: bool) -> Result<Vec<u8>, ()> {
    let mut padded = chunk.to_vec();
    if padded.len() == 2 {
        padded.extend_from_slice(&[u16::from(b'A'), u16::from(b'A')]);
    } else {
        padded.push(u16::from(b'A'));
    }
    let bytes = decode_chunk(&padded).ok_or(())?;
    if chunk.len() == 2 {
        if throw_on_extra_bits && bytes[1] != 0 {
            return Err(());
        }
        Ok(vec![bytes[0]])
    } else {
        if throw_on_extra_bits && bytes[2] != 0 {
            return Err(());
        }
        Ok(vec![bytes[0], bytes[1]])
    }
}

fn from_base64(
    units: &JsString,
    alphabet: Alphabet,
    last: LastChunk,
    max_length: usize,
) -> Decoded {
    let length = units.unit_len();
    if max_length == 0 {
        return Decoded {
            read: 0,
            bytes: Vec::new(),
            error: None,
        };
    }
    let mut read = 0;
    let mut bytes = Vec::new();
    let mut chunk: Vec<u16> = Vec::new();
    let mut index = 0;
    let error = |read, bytes| Decoded {
        read,
        bytes,
        error: Some("Invalid base64 string"),
    };
    loop {
        index = skip_whitespace(units, index);
        if index == length {
            if !chunk.is_empty() {
                if last == LastChunk::StopBeforePartial {
                    return Decoded {
                        read,
                        bytes,
                        error: None,
                    };
                }
                if last == LastChunk::Strict || chunk.len() == 1 {
                    return error(read, bytes);
                }
                match decode_final_chunk(&chunk, false) {
                    Ok(decoded) => bytes.extend_from_slice(&decoded),
                    Err(()) => return error(read, bytes),
                }
            }
            return Decoded {
                read: length,
                bytes,
                error: None,
            };
        }
        let mut character = units.unit_at(index).expect("index within input");
        index += 1;
        if character == u16::from(b'=') {
            if chunk.len() < 2 {
                return error(read, bytes);
            }
            // Padding closes the input: two characters need a second "=" and
            // three need only the one already consumed.
            index = skip_whitespace(units, index);
            if chunk.len() == 2 {
                if index == length {
                    if last == LastChunk::StopBeforePartial {
                        return Decoded {
                            read,
                            bytes,
                            error: None,
                        };
                    }
                    return error(read, bytes);
                }
                if units.unit_at(index) != Some(u16::from(b'=')) {
                    return error(read, bytes);
                }
                index = skip_whitespace(units, index + 1);
            }
            if index < length {
                return error(read, bytes);
            }
            match decode_final_chunk(&chunk, last == LastChunk::Strict) {
                Ok(decoded) => {
                    bytes.extend_from_slice(&decoded);
                    return Decoded {
                        read: length,
                        bytes,
                        error: None,
                    };
                }
                Err(()) => return error(read, bytes),
            }
        }
        if alphabet == Alphabet::Url {
            if character == u16::from(b'+') || character == u16::from(b'/') {
                return error(read, bytes);
            }
            if character == u16::from(b'-') {
                character = u16::from(b'+');
            } else if character == u16::from(b'_') {
                character = u16::from(b'/');
            }
        }
        if value_of(character).is_none() {
            return error(read, bytes);
        }
        let remaining = max_length.saturating_sub(bytes.len());
        if (remaining == 1 && chunk.len() == 2) || (remaining == 2 && chunk.len() == 3) {
            return Decoded {
                read,
                bytes,
                error: None,
            };
        }
        chunk.push(character);
        if chunk.len() == 4 {
            match decode_chunk(&chunk) {
                Some(decoded) => bytes.extend_from_slice(&decoded),
                None => return error(read, bytes),
            }
            chunk.clear();
            read = index;
        }
        if bytes.len() == max_length {
            return Decoded {
                read,
                bytes,
                error: None,
            };
        }
    }
}

fn hex_value(unit: u16) -> Option<u8> {
    match unit {
        0x30..=0x39 => Some((unit - 0x30) as u8),
        0x41..=0x46 => Some((unit - 0x41 + 10) as u8),
        0x61..=0x66 => Some((unit - 0x61 + 10) as u8),
        _ => None,
    }
}

fn from_hex(units: &[u16], max_length: usize) -> Decoded {
    if !units.len().is_multiple_of(2) {
        return Decoded {
            read: 0,
            bytes: Vec::new(),
            error: Some("Invalid hex string"),
        };
    }
    let mut bytes = Vec::new();
    let mut read = 0;
    let error = |read, bytes| Decoded {
        read,
        bytes,
        error: Some("Invalid hex string"),
    };
    while read < units.len() && bytes.len() < max_length {
        let (Some(high), Some(low)) = (hex_value(units[read]), hex_value(units[read + 1])) else {
            return error(read, bytes);
        };
        read += 2;
        bytes.push((high << 4) | low);
    }
    Decoded {
        read,
        bytes,
        error: None,
    }
}

pub(in crate::builtin) fn decode_html_base64(text: &JsString) -> Option<JsString> {
    let decoded = from_base64(text, Alphabet::Base64, LastChunk::Loose, usize::MAX);
    decoded
        .error
        .is_none()
        .then(|| JsString::from_latin1(decoded.bytes))
}

pub(in crate::builtin) fn encode_html_base64(text: &JsString) -> Option<JsString> {
    if let Some(bytes) = text.latin1_bytes() {
        return Some(encode(bytes, Alphabet::Base64, false));
    }
    let bytes = text
        .units()
        .map(u8::try_from)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    Some(encode(&bytes, Alphabet::Base64, false))
}

fn encode(units: &[u8], alphabet: Alphabet, omit_padding: bool) -> JsString {
    let symbols = if alphabet == Alphabet::Url {
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
    } else {
        STANDARD
    };
    let mut out = Vec::with_capacity(units.len().div_ceil(3) * 4);
    for chunk in units.chunks(3) {
        let first = u32::from(chunk[0]);
        let second = u32::from(*chunk.get(1).unwrap_or(&0));
        let third = u32::from(*chunk.get(2).unwrap_or(&0));
        let triple = (first << 16) | (second << 8) | third;
        let indexes = [
            (triple >> 18) & 0x3f,
            (triple >> 12) & 0x3f,
            (triple >> 6) & 0x3f,
            triple & 0x3f,
        ];
        let kept = chunk.len() + 1;
        for (position, index) in indexes.iter().enumerate() {
            if position < kept {
                out.push(symbols[*index as usize]);
            } else if !omit_padding {
                out.push(b'=');
            }
        }
    }
    JsString::from_latin1(out)
}

impl VmCore {
    /// The `options` argument of the codec methods, or `None` when absent.
    fn options_object(
        &mut self,
        roots: &ActiveRoots<'_>,
        value: &Value,
    ) -> Completion<Option<ObjectId>> {
        match value.view() {
            ValueRef::Undefined => Ok(Ok(None)),
            ValueRef::Object(id) => Ok(Ok(Some(id))),
            _ => Ok(Err(self.type_error(roots, "options must be an object")?)),
        }
    }

    fn string_option(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        options: Option<ObjectId>,
        key: &str,
        allowed: &[&'static str],
        default: &'static str,
    ) -> Completion<&'static str> {
        let Some(options) = options else {
            return Ok(Ok(default));
        };
        let options = Value::object(options);
        let roots = &roots.with(&options);
        let value = match self.read_property(host, roots, &options, PropertyKeyRef::String(key))? {
            Invocation::Value(value) => value,
            Invocation::Thrown(error) => return Ok(Err(error)),
        };
        if matches!(value.view(), ValueRef::Undefined) {
            return Ok(Ok(default));
        }
        let ValueRef::String(text) = value.view() else {
            return Ok(Err(
                self.type_error(roots, "string option requires a string value")?
            ));
        };
        let text = text.to_utf8().unwrap_or_default();
        match allowed.iter().find(|allowed| **allowed == text) {
            Some(allowed) => Ok(Ok(allowed)),
            None => Ok(Err(self.type_error(roots, "unsupported option value")?)),
        }
    }

    pub(super) fn boolean_option(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        options: Option<ObjectId>,
        key: &str,
    ) -> Completion<bool> {
        let Some(options) = options else {
            return Ok(Ok(false));
        };
        let options = Value::object(options);
        let roots = &roots.with(&options);
        match self.read_property(host, roots, &options, PropertyKeyRef::String(key))? {
            Invocation::Value(value) => Ok(Ok(value.truthy())),
            Invocation::Thrown(error) => Ok(Err(error)),
        }
    }

    /// RequireInternalSlot plus the Uint8Array name check.
    pub(super) fn validate_uint8(
        &mut self,
        roots: &ActiveRoots<'_>,
        value: &Value,
    ) -> Completion<ObjectId> {
        let (id, kind) = match self.require_typed(roots, value)? {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        };
        if kind != ElementKind::Uint8 {
            return Ok(Err(self.type_error(
                roots,
                "Uint8Array method called on a different TypedArray",
            )?));
        }
        Ok(Ok(id))
    }

    fn codec_string(&mut self, roots: &ActiveRoots<'_>, value: &Value) -> Completion<JsString> {
        match value.view() {
            ValueRef::String(text) => Ok(Ok(text.clone())),
            _ => Ok(Err(self.type_error(roots, "codec input must be a string")?)),
        }
    }

    /// Allocate a fresh `%Uint8Array%` and fill it with decoded bytes. The
    /// codec always uses the intrinsic constructor, never the receiver's.
    pub(super) fn uint8_from_bytes(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        bytes: &[u8],
    ) -> Completion<Value> {
        let constructor = Value::object(self.builtins.get("uint8Array"));
        let roots = &roots.with(&constructor);
        let created = match self.construct_with(
            host,
            roots,
            &constructor,
            Arguments::Values(&[Value::number(bytes.len() as f64)]),
            &constructor,
        )? {
            Invocation::Value(value) => value,
            Invocation::Thrown(error) => return Ok(Err(error)),
        };
        let ValueRef::Object(target) = created.view() else {
            return Ok(Err(self.type_error(
                roots,
                "Uint8Array construction did not return an object",
            )?));
        };
        if self.heap.typed_array(target)?.is_none() {
            return Ok(Err(self.type_error(
                roots,
                "Uint8Array construction did not return a TypedArray",
            )?));
        }
        self.write_uint8_bytes(target, bytes)?;
        Ok(Ok(created))
    }

    /// SetUint8ArrayBytes: write from the view's start, clipping to its length.
    fn write_uint8_bytes(&mut self, id: ObjectId, bytes: &[u8]) -> Result<(), VmError> {
        let Some(view) = self.heap.typed_array(id)? else {
            return Ok(());
        };
        let Some(buffer) = self.heap.buffer(view.buffer)? else {
            return Ok(());
        };
        let start = view.byte_offset;
        let available = view
            .byte_length
            .unwrap_or(buffer.byte_length().unwrap_or(0));
        let Some(buffer) = Some(view.buffer) else {
            return Ok(());
        };
        let Some(Object::ArrayBuffer(target)) = Some(self.heap.get_mut(buffer)?) else {
            return Ok(());
        };
        let Some(block) = target.bytes.as_mut() else {
            return Ok(());
        };
        let count = bytes.len().min(available);
        if start + count <= block.len() {
            block[start..start + count].copy_from_slice(&bytes[..count]);
        }
        Ok(())
    }

    /// Bytes of a validated Uint8Array, in order.
    fn uint8_bytes(&mut self, roots: &ActiveRoots<'_>, id: ObjectId) -> Completion<Vec<u8>> {
        let length = match self.validate_typed_bounds(roots, id)? {
            Ok(length) => length,
            Err(error) => return Ok(Err(error)),
        };
        let Some(view) = self.heap.typed_array(id)? else {
            unreachable!("receiver was validated")
        };
        let (buffer, start) = (view.buffer, view.byte_offset);
        let Some(bytes) = self.heap.view_bytes(buffer)? else {
            return Ok(Ok(Vec::new()));
        };
        Ok(Ok(bytes[start..start + length].to_vec()))
    }

    pub(super) fn uint8_from_base64(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let text = match self.codec_string(roots, arg(args, 0))? {
            Ok(text) => text,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let options = match self.options_object(roots, arg(args, 1))? {
            Ok(options) => options,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let alphabet = match self.string_option(
            host,
            roots,
            options,
            "alphabet",
            &["base64", "base64url"],
            "base64",
        )? {
            Ok(value) => value,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let last = match self.string_option(
            host,
            roots,
            options,
            "lastChunkHandling",
            &["loose", "strict", "stop-before-partial"],
            "loose",
        )? {
            Ok(value) => value,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let alphabet = if alphabet == "base64url" {
            Alphabet::Url
        } else {
            Alphabet::Base64
        };
        let decoded = from_base64(&text, alphabet, last_chunk(last), super::MAX_BYTE_LENGTH);
        if let Some(message) = decoded.error {
            let error = self.alloc_error(
                roots,
                self.builtins.get("syntaxerror.prototype"),
                "SyntaxError",
                Some(message.into()),
            )?;
            return Ok(Invocation::Thrown(error));
        }
        match self.uint8_from_bytes(host, roots, &decoded.bytes)? {
            Ok(value) => Ok(Invocation::Value(value)),
            Err(error) => Ok(Invocation::Thrown(error)),
        }
    }

    pub(super) fn uint8_from_hex(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let _ = host;
        let roots = &roots.with(&args);
        let text = match self.codec_string(roots, arg(args, 0))? {
            Ok(text) => text,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let decoded = from_hex(&text.to_utf16(), super::MAX_BYTE_LENGTH);
        if let Some(message) = decoded.error {
            let error = self.alloc_error(
                roots,
                self.builtins.get("syntaxerror.prototype"),
                "SyntaxError",
                Some(message.into()),
            )?;
            return Ok(Invocation::Thrown(error));
        }
        match self.uint8_from_bytes(host, roots, &decoded.bytes)? {
            Ok(value) => Ok(Invocation::Value(value)),
            Err(error) => Ok(Invocation::Thrown(error)),
        }
    }

    pub(super) fn uint8_set_from_base64(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let view = (receiver, args);
        let roots = &roots.with(&view);
        let id = match self.validate_uint8(roots, receiver)? {
            Ok(id) => id,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let text = match self.codec_string(roots, arg(args, 0))? {
            Ok(text) => text,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let options = match self.options_object(roots, arg(args, 1))? {
            Ok(options) => options,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let alphabet = match self.string_option(
            host,
            roots,
            options,
            "alphabet",
            &["base64", "base64url"],
            "base64",
        )? {
            Ok(value) => value,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let last = match self.string_option(
            host,
            roots,
            options,
            "lastChunkHandling",
            &["loose", "strict", "stop-before-partial"],
            "loose",
        )? {
            Ok(value) => value,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let length = match self.validate_typed_bounds(roots, id)? {
            Ok(length) => length,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let alphabet = if alphabet == "base64url" {
            Alphabet::Url
        } else {
            Alphabet::Base64
        };
        let decoded = from_base64(&text, alphabet, last_chunk(last), length);
        self.write_uint8_bytes(id, &decoded.bytes)?;
        self.codec_result(roots, decoded.read, decoded.bytes.len(), decoded.error)
    }

    pub(super) fn uint8_set_from_hex(
        &mut self,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let id = match self.validate_uint8(roots, receiver)? {
            Ok(id) => id,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let text = match self.codec_string(roots, arg(args, 0))? {
            Ok(text) => text,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let length = match self.validate_typed_bounds(roots, id)? {
            Ok(length) => length,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let decoded = from_hex(&text.to_utf16(), length);
        self.write_uint8_bytes(id, &decoded.bytes)?;
        self.codec_result(roots, decoded.read, decoded.bytes.len(), decoded.error)
    }

    /// The `{ read, written }` result, or the SyntaxError after any partial write.
    pub(super) fn codec_result(
        &mut self,
        roots: &ActiveRoots<'_>,
        read: usize,
        written: usize,
        error: Option<&'static str>,
    ) -> Result<Invocation, VmError> {
        if let Some(message) = error {
            let error = self.alloc_error(
                roots,
                self.builtins.get("syntaxerror.prototype"),
                "SyntaxError",
                Some(message.into()),
            )?;
            return Ok(Invocation::Thrown(error));
        }
        let result = self.heap.alloc_plain();
        self.heap
            .set_own_string(result, "read", Value::number(read as f64))?;
        self.heap
            .set_own_string(result, "written", Value::number(written as f64))?;
        Ok(Invocation::Value(Value::object(result)))
    }

    pub(super) fn uint8_to_base64(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let view = (receiver, args);
        let roots = &roots.with(&view);
        let id = match self.validate_uint8(roots, receiver)? {
            Ok(id) => id,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let roots = &roots.with(&id);
        let options = match self.options_object(roots, arg(args, 0))? {
            Ok(options) => options,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let alphabet = match self.string_option(
            host,
            roots,
            options,
            "alphabet",
            &["base64", "base64url"],
            "base64",
        )? {
            Ok(value) => value,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let omit_padding = match self.boolean_option(host, roots, options, "omitPadding")? {
            Ok(value) => value,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let bytes = match self.uint8_bytes(roots, id)? {
            Ok(bytes) => bytes,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let alphabet = if alphabet == "base64url" {
            Alphabet::Url
        } else {
            Alphabet::Base64
        };
        Ok(Invocation::Value(Value::string(encode(
            &bytes,
            alphabet,
            omit_padding,
        ))))
    }

    pub(super) fn uint8_to_hex(
        &mut self,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let id = match self.validate_uint8(roots, receiver)? {
            Ok(id) => id,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let bytes = match self.uint8_bytes(roots, id)? {
            Ok(bytes) => bytes,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            out.push(char::from_digit(u32::from(byte >> 4), 16).expect("hex digit"));
            out.push(char::from_digit(u32::from(byte & 0x0f), 16).expect("hex digit"));
        }
        Ok(Invocation::Value(Value::string(out.into())))
    }
}

fn last_chunk(value: &'static str) -> LastChunk {
    match value {
        "strict" => LastChunk::Strict,
        "stop-before-partial" => LastChunk::StopBeforePartial,
        _ => LastChunk::Loose,
    }
}

/// Keep the shared element helpers referenced from the codec paths.
const _: fn() = || {
    let _ = element::number_bits;
    let _ = TO_STRING_TAG_SYMBOL;
};
