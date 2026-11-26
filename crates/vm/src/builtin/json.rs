//! JSON parsing and serialization for the VM's JSON builtin.

use std::collections::HashMap;
use std::ops::Range;

use crate::error::{ErrorKind, VmError};
use crate::gc::{ActiveRoots, TraceRoots, Tracer};
use crate::heap::{Attributes, BoxedPrimitive, DescriptorUpdate, Object};
use crate::host::{Host, PlatformServices};
use crate::runtime::{Arguments, Invocation};
use crate::value::{JsString, ObjectId, PropertyKey, PropertyKeyRef, Value, ValueRef};
use crate::vm::VmCore;

#[yield_binding_macros::builtin_global(name = "JSON", root = "json")]
pub(crate) mod namespace {
    use super::{ActiveRoots, Host, Invocation, PlatformServices, Value, VmCore, VmError};

    #[function(name = "parse", length = 2)]
    fn parse<P: PlatformServices>(
        vm: &mut VmCore,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        vm.json_parse(host, roots, args)
    }

    #[function(name = "stringify", length = 3)]
    fn stringify<P: PlatformServices>(
        vm: &mut VmCore,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        vm.json_stringify(host, roots, args)
    }

    #[function(name = "rawJSON", length = 1)]
    fn raw_json<P: PlatformServices>(
        vm: &mut VmCore,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        vm.json_raw_json(host, roots, args)
    }

    #[function(name = "isRawJSON", length = 1)]
    fn is_raw_json<P: PlatformServices>(
        vm: &mut VmCore,
        _host: &Host<P>,
        _roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let branded = match args.first().map(Value::view) {
            Some(super::ValueRef::Object(id)) => vm.heap.raw_json_text(id)?.is_some(),
            _ => false,
        };
        Ok(Invocation::Value(Value::boolean(branded)))
    }
}

const MAX_DEPTH: usize = 256;

#[derive(Debug)]
enum JsonValue {
    Null,
    Bool(bool),
    Number(f64),
    String(JsString),
    Raw(JsString),
    Array(Vec<JsonValue>),
    Object(Vec<(JsString, JsonValue)>),
}

enum Parsed {
    Primitive(Value, Range<usize>),
    Array(Vec<Parsed>),
    Object(Vec<(JsString, Parsed)>),
}

struct ParseRecord {
    value: Value,
    children: Children,
}

enum Children {
    None,
    Source(Range<usize>),
    Array(Vec<ParseRecord>),
    Object(HashMap<JsString, ParseRecord>),
}

impl TraceRoots for ParseRecord {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError> {
        self.value.trace(tracer)?;
        match &self.children {
            Children::Array(elements) => elements.trace(tracer)?,
            Children::Object(entries) => {
                for entry in entries.values() {
                    entry.trace(tracer)?;
                }
            }
            Children::None | Children::Source(_) => {}
        }
        Ok(())
    }
}

struct Reviver<'a> {
    function: &'a Value,
    text: &'a JsString,
    depth: usize,
}

struct Parser<'a> {
    text: &'a [u16],
    pos: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while let Some(ch) = self.peek() {
            if matches!(ch, ' ' | '\t' | '\r' | '\n') {
                self.pos += ch.len_utf16();
            } else {
                break;
            }
        }
    }

    fn peek(&self) -> Option<char> {
        self.text
            .get(self.pos)
            .and_then(|&unit| char::from_u32(u32::from(unit)))
    }

    fn take(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.pos += expected.len_utf16();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<Parsed, String> {
        if depth > MAX_DEPTH {
            return Err("JSON nesting exceeds maximum depth".into());
        }
        self.ws();
        match self.peek() {
            Some('[') => self.array(depth + 1),
            Some('{') => self.object(depth + 1),
            _ => {
                let start = self.pos;
                let value = self.primitive()?;
                Ok(Parsed::Primitive(value, start..self.pos))
            }
        }
    }

    fn primitive(&mut self) -> Result<Value, String> {
        match self.peek() {
            Some('n') if self.literal("null") => Ok(Value::NULL),
            Some('t') if self.literal("true") => Ok(Value::boolean(true)),
            Some('f') if self.literal("false") => Ok(Value::boolean(false)),
            Some('"') => self.string().map(Value::string),
            Some('-' | '0'..='9') => self.number().map(Value::number),
            _ => Err("invalid JSON".into()),
        }
    }

    fn literal(&mut self, literal: &str) -> bool {
        if self.text[self.pos..]
            .iter()
            .take(literal.len())
            .copied()
            .eq(literal.bytes().map(u16::from))
        {
            self.pos += literal.len();
            true
        } else {
            false
        }
    }

    fn string(&mut self) -> Result<JsString, String> {
        let mut units = Vec::new();
        self.scan_string(|unit| units.push(unit))?;
        Ok(JsString::from_units(units))
    }

    fn scan_string(&mut self, mut unit_out: impl FnMut(u16)) -> Result<(), String> {
        if !self.take('"') {
            return Err("invalid JSON string".into());
        }
        loop {
            let unit = *self.text.get(self.pos).ok_or("unterminated JSON string")?;
            self.pos += 1;
            if unit == u16::from(b'"') {
                return Ok(());
            }
            if unit == u16::from(b'\\') {
                let escaped_unit = *self
                    .text
                    .get(self.pos)
                    .ok_or("invalid or unterminated JSON escape")?;
                let escaped =
                    char::from_u32(u32::from(escaped_unit)).ok_or("invalid JSON escape")?;
                self.pos += 1;
                unit_out(match escaped {
                    '"' | '\\' | '/' => escaped as u16,
                    'b' => 0x08,
                    'f' => 0x0c,
                    'n' => u16::from(b'\n'),
                    'r' => u16::from(b'\r'),
                    't' => u16::from(b'\t'),
                    'u' => self.hex4()?,
                    _ => return Err("invalid JSON escape".into()),
                });
            } else if unit <= 0x1f {
                return Err("unescaped control character".into());
            } else {
                unit_out(unit);
            }
        }
    }

    fn hex4(&mut self) -> Result<u16, String> {
        let mut value = 0;
        for _ in 0..4 {
            let unit = *self.text.get(self.pos).ok_or("short Unicode escape")?;
            self.pos += 1;
            value = value * 16
                + char::from_u32(u32::from(unit))
                    .and_then(|ch| ch.to_digit(16))
                    .ok_or("invalid Unicode escape")? as u16;
        }
        Ok(value)
    }

    fn number(&mut self) -> Result<f64, String> {
        let start = self.pos;
        self.scan_number()?;
        self.text[start..self.pos]
            .iter()
            .map(|&unit| char::from(unit as u8))
            .collect::<String>()
            .parse::<f64>()
            .map_err(|_| "invalid JSON number".into())
    }

    fn scan_number(&mut self) -> Result<(), String> {
        self.take('-');
        match self.peek() {
            Some('0') => {
                self.pos += 1;
                if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    return Err("leading zero in JSON number".into());
                }
            }
            Some('1'..='9') => {
                while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.pos += 1;
                }
            }
            _ => return Err("invalid JSON number".into()),
        }
        if self.take('.') {
            if !self.peek().is_some_and(|c| c.is_ascii_digit()) {
                return Err("invalid JSON fraction".into());
            }
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if self.peek().is_some_and(|c| matches!(c, 'e' | 'E')) {
            self.pos += 1;
            if self.peek().is_some_and(|c| matches!(c, '+' | '-')) {
                self.pos += 1;
            }
            if !self.peek().is_some_and(|c| c.is_ascii_digit()) {
                return Err("invalid JSON exponent".into());
            }
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        Ok(())
    }

    fn array(&mut self, depth: usize) -> Result<Parsed, String> {
        self.take('[');
        self.ws();
        let mut values = Vec::new();
        if self.take(']') {
            return Ok(Parsed::Array(values));
        }
        loop {
            values.push(self.value(depth)?);
            self.ws();
            if self.take(']') {
                break;
            }
            if !self.take(',') {
                return Err("invalid JSON array".into());
            }
            self.ws();
            if self.peek() == Some(']') {
                return Err("trailing comma".into());
            }
        }
        Ok(Parsed::Array(values))
    }

    fn object(&mut self, depth: usize) -> Result<Parsed, String> {
        self.take('{');
        self.ws();
        let mut values = Vec::new();
        if self.take('}') {
            return Ok(Parsed::Object(values));
        }
        loop {
            self.ws();
            let key = self.string()?;
            self.ws();
            if !self.take(':') {
                return Err("invalid JSON object".into());
            }
            values.push((key, self.value(depth)?));
            self.ws();
            if self.take('}') {
                break;
            }
            if !self.take(',') {
                return Err("invalid JSON object".into());
            }
            self.ws();
            if self.peek() == Some('}') {
                return Err("trailing comma".into());
            }
        }
        Ok(Parsed::Object(values))
    }
}

pub(crate) fn is_raw_json_source(text: &JsString) -> bool {
    let units = text.to_utf16();
    let mut parser = Parser {
        text: &units,
        pos: 0,
    };
    // Unlike full JSON parsing, raw JSON accepts only a primitive token and
    // does not consume surrounding whitespace.
    let valid = match parser.peek() {
        Some('n') => parser.literal("null"),
        Some('t') => parser.literal("true"),
        Some('f') => parser.literal("false"),
        Some('"') => parser.scan_string(|_| {}).is_ok(),
        Some('-' | '0'..='9') => parser.scan_number().is_ok(),
        _ => false,
    };
    valid && parser.pos == units.len()
}

fn quote(text: &JsString, out: &mut Vec<u16>) {
    out.push(u16::from(b'"'));
    for ch in char::decode_utf16(text.units()) {
        match ch {
            Ok('"') => out.extend("\\\"".encode_utf16()),
            Ok('\\') => out.extend("\\\\".encode_utf16()),
            Ok('\u{0008}') => out.extend("\\b".encode_utf16()),
            Ok('\u{000c}') => out.extend("\\f".encode_utf16()),
            Ok('\n') => out.extend("\\n".encode_utf16()),
            Ok('\r') => out.extend("\\r".encode_utf16()),
            Ok('\t') => out.extend("\\t".encode_utf16()),
            Ok(ch) if ch <= '\u{001f}' => unicode_escape(ch as u16, out),
            Err(error) => unicode_escape(error.unpaired_surrogate(), out),
            Ok(ch) => out.extend_from_slice(ch.encode_utf16(&mut [0; 2])),
        }
    }
    out.push(u16::from(b'"'));
}

fn unicode_escape(unit: u16, out: &mut Vec<u16>) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.extend("\\u".encode_utf16());
    for shift in [12, 8, 4, 0] {
        out.push(u16::from(HEX[((unit >> shift) & 0xf) as usize]));
    }
}

fn encode(value: &JsonValue, out: &mut Vec<u16>, gap: &JsString, indent: usize) {
    match value {
        JsonValue::Null => out.extend("null".encode_utf16()),
        JsonValue::Bool(value) => {
            out.extend((if *value { "true" } else { "false" }).encode_utf16())
        }
        JsonValue::Number(value) => out.extend(ryu_js::Buffer::new().format(*value).encode_utf16()),
        JsonValue::String(value) => quote(value, out),
        JsonValue::Raw(value) => out.extend(value.units()),
        JsonValue::Array(values) => {
            encode_list(out, gap, indent, true, values.len(), |out, next| {
                for (index, value) in values.iter().enumerate() {
                    separate(out, gap, next, index);
                    encode(value, out, gap, next);
                }
            })
        }
        JsonValue::Object(values) => {
            encode_list(out, gap, indent, false, values.len(), |out, next| {
                for (index, (key, value)) in values.iter().enumerate() {
                    separate(out, gap, next, index);
                    quote(key, out);
                    out.push(u16::from(b':'));
                    if !gap.is_empty() {
                        out.push(u16::from(b' '));
                    }
                    encode(value, out, gap, next);
                }
            });
        }
    }
}

fn separate(out: &mut Vec<u16>, gap: &JsString, indent: usize, index: usize) {
    if index > 0 {
        out.push(u16::from(b','));
    }
    newline(out, gap, indent);
}

fn newline(out: &mut Vec<u16>, gap: &JsString, indent: usize) {
    if !gap.is_empty() {
        out.push(u16::from(b'\n'));
        for _ in 0..indent {
            out.extend(gap.units());
        }
    }
}

fn encode_list(
    out: &mut Vec<u16>,
    gap: &JsString,
    indent: usize,
    array: bool,
    len: usize,
    body: impl FnOnce(&mut Vec<u16>, usize),
) {
    out.push(u16::from(if array { b'[' } else { b'{' }));
    if len != 0 {
        body(out, indent + 1);
        newline(out, gap, indent);
    }
    out.push(u16::from(if array { b']' } else { b'}' }));
}

struct Serialize<'a> {
    replacer: Option<&'a Value>,
    properties: Option<&'a [JsString]>,
    stack: Vec<ObjectId>,
    path: String,
}

impl VmCore {
    fn json_parse<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let text =
            match self.coerce_string(host, roots, args.first().unwrap_or(&Value::UNDEFINED))? {
                Ok(text) => text,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
        let units = text.to_utf16();
        let mut parser = Parser {
            text: &units,
            pos: 0,
        };
        let parsed = parser.value(0).and_then(|value| {
            parser.ws();
            if parser.pos != text.unit_len() {
                Err("trailing JSON text".into())
            } else {
                Ok(value)
            }
        });
        let parsed = match parsed {
            Ok(parsed) => parsed,
            Err(message) if message.contains("depth") => {
                return self.throw_range_error(roots, &message);
            }
            Err(message) => return self.json_syntax_error(roots, message.into()),
        };
        let function = args.get(1).filter(|value| self.is_callable(value));
        let record = self.materialize_json(parsed, function.is_some())?;
        let Some(function) = function else {
            return Ok(Invocation::Value(record.value));
        };
        let reviver = Reviver {
            function,
            text: &text,
            depth: 0,
        };
        let holder = self.heap.alloc_plain();
        self.heap.define_data(
            holder,
            PropertyKey::String(JsString::new()),
            record.value.clone(),
            Attributes::ORDINARY,
        )?;
        let root = JsString::new();
        // A callback can remove an unvisited object from the live parse
        // result. Keep all original identities rooted for SameValue checks.
        let roots = &roots.with(&record);
        match self.revive(host, roots, &reviver, holder, &root, Some(&record))? {
            Ok(value) => Ok(Invocation::Value(value)),
            Err(error) => Ok(Invocation::Thrown(error)),
        }
    }

    fn json_raw_json<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let text =
            match self.coerce_string(host, roots, args.first().unwrap_or(&Value::UNDEFINED))? {
                Ok(text) => text,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
        if !is_raw_json_source(&text) {
            return self.json_syntax_error(roots, "Invalid raw JSON text".into());
        }
        let raw = self.heap.alloc_raw_json(text)?;
        Ok(Invocation::Value(Value::object(raw)))
    }

    fn json_syntax_error(
        &mut self,
        roots: &ActiveRoots<'_>,
        message: JsString,
    ) -> Result<Invocation, VmError> {
        let error = self.alloc_class_error(roots, ErrorKind::SyntaxError, Some(message))?;
        Ok(Invocation::Thrown(error))
    }

    fn json_stringify<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let value = args.first().cloned().unwrap_or(Value::UNDEFINED);
        let replacer = args.get(1);
        let mut listed = None;
        let function = replacer.filter(|value| self.is_callable(value));
        if function.is_none()
            && let Some(ValueRef::Object(id)) = replacer.map(Value::view)
        {
            let target = match self.proxy_base(roots, id)? {
                Ok(target) => target,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            if matches!(self.heap.get(target)?, Object::Array(_)) {
                listed = match self.json_property_list(host, roots, id)? {
                    Ok(keys) => Some(keys),
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
            }
        }
        let gap = match self.json_gap(host, roots, args.get(2).unwrap_or(&Value::UNDEFINED))? {
            Ok(gap) => gap,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let holder = self.heap.alloc_plain();
        self.heap.define_data(
            holder,
            PropertyKey::String(JsString::new()),
            value,
            Attributes::ORDINARY,
        )?;
        let mut state = Serialize {
            replacer: function,
            properties: listed.as_deref(),
            stack: Vec::new(),
            path: "$".to_owned(),
        };
        match self.serialize_property(host, roots, &mut state, holder, &JsString::new())? {
            Ok(None) => Ok(Invocation::Value(Value::UNDEFINED)),
            Ok(Some(tree)) => {
                let mut output = Vec::new();
                encode(&tree, &mut output, &gap, 0);
                Ok(Invocation::Value(Value::string(JsString::from_units(
                    output,
                ))))
            }
            Err(error) => Ok(Invocation::Thrown(error)),
        }
    }

    fn json_gap<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        space: &Value,
    ) -> Result<Result<JsString, Value>, VmError> {
        let space = match space.view() {
            ValueRef::Object(id)
                if matches!(
                    self.heap.get(id)?,
                    Object::Boxed {
                        primitive: BoxedPrimitive::String(_),
                        ..
                    }
                ) =>
            {
                match self.coerce_string(host, roots, space)? {
                    Ok(text) => Value::string(text),
                    Err(error) => return Ok(Err(error)),
                }
            }
            ValueRef::Object(id)
                if matches!(
                    self.heap.get(id)?,
                    Object::Boxed {
                        primitive: BoxedPrimitive::Number(_),
                        ..
                    }
                ) =>
            {
                match self.coerce_number(host, roots, space)? {
                    Ok(number) => Value::number(number),
                    Err(error) => return Ok(Err(error)),
                }
            }
            _ => space.clone(),
        };
        Ok(Ok(match space.view() {
            ValueRef::Number(number) => JsString::from_units(vec![
                u16::from(b' ');
                number.trunc().clamp(0.0, 10.0)
                    as usize
            ]),
            ValueRef::String(text) => text.slice_units(0..text.unit_len().min(10)),
            _ => JsString::new(),
        }))
    }

    fn json_property_list<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
    ) -> Result<Result<Vec<JsString>, Value>, VmError> {
        let length = match self.length_of_array_like(host, roots, id)? {
            Ok(length) => length,
            Err(error) => return Ok(Err(error)),
        };
        let mut keys = Vec::new();
        for index in 0..length {
            let name = JsString::from(index.to_string());
            let item = match self.read_property(
                host,
                roots,
                &Value::object(id),
                PropertyKeyRef::JsString(&name),
            )? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
            let key = match item.view() {
                ValueRef::String(_) => item.into_string().expect("string item"),
                ValueRef::Number(number) => crate::ops::to_js_string(&Value::number(number)),
                _ => continue,
            };
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        Ok(Ok(keys))
    }

    fn serialize_property<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        state: &mut Serialize<'_>,
        holder: ObjectId,
        key: &JsString,
    ) -> Result<Result<Option<JsonValue>, Value>, VmError> {
        let roots = &roots.with(&holder);
        if state.stack.len() > MAX_DEPTH {
            return self
                .type_error(roots, "Maximum JSON nesting exceeded")
                .map(Err);
        }
        let mut value = match self.read_property(
            host,
            roots,
            &Value::object(holder),
            PropertyKeyRef::JsString(key),
        )? {
            Invocation::Value(value) => value,
            Invocation::Thrown(error) => return Ok(Err(error)),
        };
        if matches!(value.view(), ValueRef::Object(_) | ValueRef::BigInt(_)) {
            let roots = &roots.with(&value);
            let method =
                match self.read_property(host, roots, &value, PropertyKeyRef::String("toJSON"))? {
                    Invocation::Value(method) => method,
                    Invocation::Thrown(error) => return Ok(Err(error)),
                };
            if self.is_callable(&method) {
                value = match self.invoke(
                    host,
                    roots,
                    &method,
                    Arguments::Values(&[Value::string(key.clone())]),
                    &value,
                )? {
                    Invocation::Value(value) => value,
                    Invocation::Thrown(error) => return Ok(Err(error)),
                };
            }
        }
        if let Some(replacer) = state.replacer.cloned() {
            value = match self.invoke(
                host,
                roots,
                &replacer,
                Arguments::Values(&[Value::string(key.clone()), value]),
                &Value::object(holder),
            )? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
        }
        self.serialize_value(host, roots, state, value)
    }

    fn serialize_value<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        state: &mut Serialize<'_>,
        value: Value,
    ) -> Result<Result<Option<JsonValue>, Value>, VmError> {
        if let ValueRef::Object(id) = value.view()
            && let Some(text) = self.heap.raw_json_text(id)?
        {
            return Ok(Ok(Some(JsonValue::Raw(text.clone()))));
        }
        let tree = match value.view() {
            ValueRef::Null => JsonValue::Null,
            ValueRef::Bool(value) => JsonValue::Bool(value),
            ValueRef::Number(value) if value.is_finite() => JsonValue::Number(value),
            ValueRef::Number(_) => JsonValue::Null,
            ValueRef::String(_) => JsonValue::String(value.into_string().expect("string value")),
            ValueRef::BigInt(_) => {
                return Ok(Err(
                    self.type_error(roots, "Do not know how to serialize a BigInt")?
                ));
            }
            ValueRef::Object(_) if self.is_callable(&value) => return Ok(Ok(None)),
            ValueRef::Object(id) => {
                let roots = &roots.with(&id);
                let target = match self.proxy_base(roots, id)? {
                    Ok(target) => target,
                    Err(error) => return Ok(Err(error)),
                };
                let result = if matches!(self.heap.get(target)?, Object::Array(_)) {
                    self.serialize_array(host, roots, state, id)?
                } else {
                    self.serialize_object(host, roots, state, id)?
                };
                match result {
                    Ok(tree) => tree,
                    Err(error) => return Ok(Err(error)),
                }
            }
            ValueRef::Undefined | ValueRef::Symbol(_) => return Ok(Ok(None)),
        };
        Ok(Ok(Some(tree)))
    }

    fn serialize_array<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        state: &mut Serialize<'_>,
        id: ObjectId,
    ) -> Result<Result<JsonValue, Value>, VmError> {
        let roots = &roots.with(&id);
        if let Some(error) = self.cycle(roots, state, id)? {
            return Ok(Err(error));
        }
        state.stack.push(id);
        let length = match self.length_of_array_like(host, roots, id)? {
            Ok(length) => length,
            Err(error) => return Ok(Err(error)),
        };
        let mut values = Vec::new();
        let saved = state.path.clone();
        for index in 0..length {
            state.path = format!("{saved}[{index}]");
            let key = JsString::from(index.to_string());
            match self.serialize_property(host, roots, state, id, &key)? {
                Ok(Some(value)) => values.push(value),
                Ok(None) => values.push(JsonValue::Null),
                Err(error) => return Ok(Err(error)),
            }
        }
        state.path = saved;
        state.stack.pop();
        Ok(Ok(JsonValue::Array(values)))
    }

    fn serialize_object<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        state: &mut Serialize<'_>,
        id: ObjectId,
    ) -> Result<Result<JsonValue, Value>, VmError> {
        let roots = &roots.with(&id);
        if let Some(error) = self.cycle(roots, state, id)? {
            return Ok(Err(error));
        }
        state.stack.push(id);
        let keys = if let Some(properties) = state.properties {
            properties.to_vec()
        } else {
            match self.enumerable_string_keys(host, roots, id)? {
                Ok(keys) => keys,
                Err(error) => return Ok(Err(error)),
            }
        };
        let mut values = Vec::new();
        let saved = state.path.clone();
        for key in keys {
            state.path = format!(
                "{saved}.{}",
                key.to_utf8().unwrap_or_else(|_| "<surrogate>".into())
            );
            match self.serialize_property(host, roots, state, id, &key)? {
                Ok(Some(value)) => values.push((key, value)),
                Ok(None) => {}
                Err(error) => return Ok(Err(error)),
            }
        }
        state.path = saved;
        state.stack.pop();
        Ok(Ok(JsonValue::Object(values)))
    }

    fn cycle(
        &mut self,
        roots: &ActiveRoots<'_>,
        state: &Serialize<'_>,
        id: ObjectId,
    ) -> Result<Option<Value>, VmError> {
        if state.stack.contains(&id) {
            self.type_error(
                roots,
                &format!("Converting circular structure to JSON at {}", state.path),
            )
            .map(Some)
        } else {
            Ok(None)
        }
    }

    fn revive<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        reviver: &Reviver<'_>,
        holder: ObjectId,
        key: &JsString,
        record: Option<&ParseRecord>,
    ) -> Result<Result<Value, Value>, VmError> {
        let roots = &roots.with(&holder);
        if reviver.depth > MAX_DEPTH {
            return self
                .type_error(roots, "Maximum JSON nesting exceeded")
                .map(Err);
        }
        let receiver = Value::object(holder);
        let value =
            match self.read_property(host, roots, &receiver, PropertyKeyRef::JsString(key))? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
        let children = record
            .filter(|record| crate::ops::same_value(&record.value, &value))
            .map(|record| &record.children);
        if let ValueRef::Object(id) = value.view() {
            let roots = &roots.with(&id);
            let next = Reviver {
                depth: reviver.depth + 1,
                ..*reviver
            };
            let target = match self.proxy_base(roots, id)? {
                Ok(target) => target,
                Err(error) => return Ok(Err(error)),
            };
            if matches!(self.heap.get(target)?, Object::Array(_)) {
                let length = match self.length_of_array_like(host, roots, id)? {
                    Ok(length) => length,
                    Err(error) => return Ok(Err(error)),
                };
                for index in 0..length {
                    let record = match children {
                        Some(Children::Array(elements)) => usize::try_from(index)
                            .ok()
                            .and_then(|index| elements.get(index)),
                        _ => None,
                    };
                    let name = JsString::from(index.to_string());
                    let result = self.revive(host, roots, &next, id, &name, record)?;
                    if let Err(error) = self.apply_reviver_result(host, roots, id, name, result)? {
                        return Ok(Err(error));
                    }
                }
            } else {
                let keys = match self.enumerable_string_keys(host, roots, id)? {
                    Ok(keys) => keys,
                    Err(error) => return Ok(Err(error)),
                };
                for name in keys {
                    let record = match children {
                        Some(Children::Object(entries)) => entries.get(&name),
                        _ => None,
                    };
                    let result = self.revive(host, roots, &next, id, &name, record)?;
                    if let Err(error) = self.apply_reviver_result(host, roots, id, name, result)? {
                        return Ok(Err(error));
                    }
                }
            }
        }
        let context = self.heap.alloc_plain();
        if let Some(Children::Source(span)) = children {
            self.heap.define_data(
                context,
                "source",
                Value::string(reviver.text.slice_units(span.clone())),
                Attributes::ORDINARY,
            )?;
        }
        Ok(
            match self.invoke(
                host,
                roots,
                reviver.function,
                Arguments::Values(&[Value::string(key.clone()), value, Value::object(context)]),
                &receiver,
            )? {
                Invocation::Value(value) => Ok(value),
                Invocation::Thrown(error) => Err(error),
            },
        )
    }

    fn apply_reviver_result<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        holder: ObjectId,
        key: JsString,
        result: Result<Value, Value>,
    ) -> Result<Result<(), Value>, VmError> {
        let value = match result {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        };
        if matches!(value.view(), ValueRef::Undefined) {
            if let Invocation::Thrown(error) =
                self.delete_property(host, roots, &Value::object(holder), &Value::string(key))?
            {
                return Ok(Err(error));
            }
        } else {
            let update = DescriptorUpdate {
                value: Some(value),
                writable: Some(true),
                enumerable: Some(true),
                configurable: Some(true),
                ..Default::default()
            };
            if let Err(error) = self.try_apply_property_update(
                host,
                roots,
                holder,
                PropertyKey::String(key),
                update,
            )? {
                return Ok(Err(error));
            }
        }
        Ok(Ok(()))
    }

    fn materialize_json(&mut self, parsed: Parsed, track: bool) -> Result<ParseRecord, VmError> {
        let (value, children) = match parsed {
            Parsed::Primitive(value, span) => (
                value,
                if track {
                    Children::Source(span)
                } else {
                    Children::None
                },
            ),
            Parsed::Array(values) => {
                let id = self.heap.alloc_array(Vec::new());
                let mut elements = Vec::new();
                if track {
                    elements.reserve(values.len());
                }
                for (index, parsed) in values.into_iter().enumerate() {
                    let record = self.materialize_json(parsed, track)?;
                    let key = PropertyKey::String(index.to_string().into());
                    if track {
                        self.heap.define_data(
                            id,
                            key,
                            record.value.clone(),
                            Attributes::ORDINARY,
                        )?;
                        elements.push(record);
                    } else {
                        self.heap
                            .define_data(id, key, record.value, Attributes::ORDINARY)?;
                    }
                }
                (Value::object(id), Children::Array(elements))
            }
            Parsed::Object(values) => {
                let id = self.heap.alloc_plain();
                let mut entries = HashMap::new();
                for (key, parsed) in values {
                    let record = self.materialize_json(parsed, track)?;
                    if track {
                        self.heap.define_data(
                            id,
                            PropertyKey::String(key.clone()),
                            record.value.clone(),
                            Attributes::ORDINARY,
                        )?;
                        // The final spelling/value for duplicate decoded keys wins.
                        entries.insert(key, record);
                    } else {
                        self.heap.define_data(
                            id,
                            PropertyKey::String(key),
                            record.value,
                            Attributes::ORDINARY,
                        )?;
                    }
                }
                (Value::object(id), Children::Object(entries))
            }
        };
        Ok(ParseRecord { value, children })
    }
}
