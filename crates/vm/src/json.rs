//! JSON parsing and serialization for the VM's JSON builtin.
//!
//! Strings are UTF-8, so escaped lone UTF-16 surrogates are rejected as a
//! catchable `SyntaxError` rather than stored.

use crate::error::VmError;
use crate::heap::{Attributes, Object};
use crate::host::Host;
use crate::runtime::Invocation;
use crate::value::{ObjectId, PropertyKey, PropertyKeyRef, Value};
use crate::vm::Vm;

const MAX_DEPTH: usize = 256;

#[derive(Debug)]
enum JsonValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<JsonValue>),
    Object(Vec<(String, JsonValue)>),
}

struct Parser<'a> {
    text: &'a str,
    pos: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while let Some(ch) = self.peek() {
            if matches!(ch, ' ' | '\t' | '\r' | '\n') {
                self.pos += ch.len_utf8();
            } else {
                break;
            }
        }
    }

    fn peek(&self) -> Option<char> {
        self.text.get(self.pos..)?.chars().next()
    }

    fn take(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.pos += expected.len_utf8();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<JsonValue, String> {
        if depth > MAX_DEPTH {
            return Err("JSON nesting exceeds maximum depth".into());
        }
        self.ws();
        match self.peek() {
            Some('n') if self.literal("null") => Ok(JsonValue::Null),
            Some('t') if self.literal("true") => Ok(JsonValue::Bool(true)),
            Some('f') if self.literal("false") => Ok(JsonValue::Bool(false)),
            Some('"') => self.string().map(JsonValue::String),
            Some('[') => self.array(depth + 1),
            Some('{') => self.object(depth + 1),
            Some('-' | '0'..='9') => self.number(),
            _ => Err("invalid JSON".into()),
        }
    }

    fn literal(&mut self, literal: &str) -> bool {
        if self.text[self.pos..].starts_with(literal) {
            self.pos += literal.len();
            true
        } else {
            false
        }
    }

    fn string(&mut self) -> Result<String, String> {
        if !self.take('"') {
            return Err("invalid JSON string".into());
        }
        let mut result = String::new();
        loop {
            let ch = self.peek().ok_or("unterminated JSON string")?;
            self.pos += ch.len_utf8();
            match ch {
                '"' => return Ok(result),
                '\\' => {
                    let escaped = self.peek().ok_or("unterminated JSON escape")?;
                    self.pos += escaped.len_utf8();
                    match escaped {
                        '"' | '\\' | '/' => result.push(escaped),
                        'b' => result.push('\u{0008}'),
                        'f' => result.push('\u{000c}'),
                        'n' => result.push('\n'),
                        'r' => result.push('\r'),
                        't' => result.push('\t'),
                        'u' => {
                            let first = self.hex4()?;
                            let scalar = if (0xd800..=0xdbff).contains(&first) {
                                if !self.take('\\') || !self.take('u') {
                                    return Err("lone surrogate escape".into());
                                }
                                let second = self.hex4()?;
                                if !(0xdc00..=0xdfff).contains(&second) {
                                    return Err("invalid surrogate pair".into());
                                }
                                0x10000 + ((first - 0xd800) << 10) + (second - 0xdc00)
                            } else if (0xdc00..=0xdfff).contains(&first) {
                                return Err("lone surrogate escape".into());
                            } else {
                                first
                            };
                            result.push(char::from_u32(scalar).ok_or("invalid Unicode escape")?);
                        }
                        _ => return Err("invalid JSON escape".into()),
                    }
                }
                ch if ch <= '\u{001f}' => return Err("unescaped control character".into()),
                ch => result.push(ch),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let mut value = 0;
        for _ in 0..4 {
            let ch = self.peek().ok_or("short Unicode escape")?;
            self.pos += ch.len_utf8();
            value = value * 16 + ch.to_digit(16).ok_or("invalid Unicode escape")?;
        }
        Ok(value)
    }

    fn number(&mut self) -> Result<JsonValue, String> {
        let start = self.pos;
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
        let number = self.text[start..self.pos]
            .parse::<f64>()
            .map_err(|_| "invalid JSON number")?;
        if !number.is_finite() {
            return Err("JSON number is not finite".into());
        }
        Ok(JsonValue::Number(number))
    }

    fn array(&mut self, depth: usize) -> Result<JsonValue, String> {
        self.take('[');
        self.ws();
        let mut values = Vec::new();
        if self.take(']') {
            return Ok(JsonValue::Array(values));
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
        Ok(JsonValue::Array(values))
    }

    fn object(&mut self, depth: usize) -> Result<JsonValue, String> {
        self.take('{');
        self.ws();
        let mut values = Vec::new();
        if self.take('}') {
            return Ok(JsonValue::Object(values));
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
        Ok(JsonValue::Object(values))
    }
}

fn quote(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch <= '\u{001f}' => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
}

fn encode(value: &JsonValue, out: &mut String, gap: &str, indent: &str) {
    match value {
        JsonValue::Null => out.push_str("null"),
        JsonValue::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
        JsonValue::Number(value) if *value == 0.0 => out.push('0'),
        JsonValue::Number(value) => out.push_str(&value.to_string()),
        JsonValue::String(value) => quote(value, out),
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
                    out.push(':');
                    if !gap.is_empty() {
                        out.push(' ');
                    }
                    encode(value, out, gap, next);
                }
            });
        }
    }
}

fn separate(out: &mut String, gap: &str, indent: &str, index: usize) {
    if index > 0 {
        out.push(',');
    }
    if !gap.is_empty() {
        out.push('\n');
        out.push_str(indent);
    }
}

fn encode_list(
    out: &mut String,
    gap: &str,
    indent: &str,
    array: bool,
    len: usize,
    body: impl FnOnce(&mut String, &str),
) {
    out.push(if array { '[' } else { '{' });
    if len == 0 {
        out.push(if array { ']' } else { '}' });
        return;
    }
    let next = format!("{indent}{gap}");
    body(out, &next);
    if !gap.is_empty() {
        out.push('\n');
        out.push_str(indent);
    }
    out.push(if array { ']' } else { '}' });
}

struct Serialize<'a> {
    replacer: Option<&'a Value>,
    properties: Option<&'a [String]>,
    stack: Vec<ObjectId>,
    path: String,
}

impl Vm {
    pub(crate) fn call_json(
        &mut self,
        host: &mut dyn Host,
        method: &str,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        match method {
            "parse" => self.json_parse(host, args),
            "stringify" => self.json_stringify(host, args),
            _ => Err(VmError::InvalidState(format!(
                "unknown JSON method {method}"
            ))),
        }
    }

    fn json_parse(&mut self, host: &mut dyn Host, args: &[Value]) -> Result<Invocation, VmError> {
        let text = match self.coerce_string(host, args.first().unwrap_or(&Value::Undefined))? {
            Ok(text) => text,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let mut parser = Parser {
            text: &text,
            pos: 0,
        };
        let parsed = parser.value(0).and_then(|value| {
            parser.ws();
            if parser.pos != text.len() {
                Err("trailing JSON text".into())
            } else {
                Ok(value)
            }
        });
        let value = match parsed {
            Ok(value) => self.materialize_json(value)?,
            Err(message) if message.contains("depth") => return self.throw_range_error(&message),
            Err(message) => {
                let error = self.alloc_error(
                    self.builtins.syntaxerror_prototype,
                    "SyntaxError",
                    Some(message),
                )?;
                return Ok(Invocation::Thrown(error));
            }
        };
        let Some(reviver) = args.get(1).filter(|value| self.is_callable(value)) else {
            return Ok(Invocation::Value(value));
        };
        let holder = self.heap.alloc_plain();
        self.heap
            .define_data(holder, "", value, Attributes::ORDINARY)?;
        match self.revive(host, reviver, holder, "")? {
            Ok(()) => self.read_data(holder, ""),
            Err(error) => Ok(Invocation::Thrown(error)),
        }
    }

    fn json_stringify(
        &mut self,
        host: &mut dyn Host,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let value = args.first().cloned().unwrap_or(Value::Undefined);
        let replacer = args.get(1);
        let listed = match replacer {
            Some(Value::Object(id)) if matches!(self.heap.get(*id)?, Object::Array(_)) => {
                match self.json_property_list(host, *id)? {
                    Ok(keys) => Some(keys),
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
            }
            _ => None,
        };
        let function = replacer.filter(|value| self.is_callable(value));
        let gap = match self.json_gap(host, args.get(2).unwrap_or(&Value::Undefined))? {
            Ok(gap) => gap,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let holder = self.heap.alloc_plain();
        self.heap
            .define_data(holder, "", value, Attributes::ORDINARY)?;
        let mut state = Serialize {
            replacer: function,
            properties: listed.as_deref(),
            stack: Vec::new(),
            path: "$".to_owned(),
        };
        match self.serialize_property(host, &mut state, holder, "")? {
            Ok(None) => Ok(Invocation::Value(Value::Undefined)),
            Ok(Some(tree)) => {
                let mut output = String::new();
                encode(&tree, &mut output, &gap, "");
                Ok(Invocation::Value(Value::String(output)))
            }
            Err(error) => Ok(Invocation::Thrown(error)),
        }
    }

    fn json_gap(
        &mut self,
        host: &mut dyn Host,
        space: &Value,
    ) -> Result<Result<String, Value>, VmError> {
        let space = match space {
            Value::Object(_) => match self.coerce_number(host, space)? {
                Ok(number) => Value::Number(number),
                Err(error) => return Ok(Err(error)),
            },
            other => other.clone(),
        };
        // Objects with valueOf are numbers; a string object is not boxed here.
        let space = if matches!(space, Value::Object(_)) {
            match self.coerce_string(host, &space)? {
                Ok(text) => Value::String(text),
                Err(error) => return Ok(Err(error)),
            }
        } else {
            space
        };
        Ok(Ok(match space {
            Value::Number(number) => " ".repeat(number.trunc().clamp(0.0, 10.0) as usize),
            Value::String(text) => text.chars().take(10).collect(),
            _ => String::new(),
        }))
    }

    fn json_property_list(
        &mut self,
        host: &mut dyn Host,
        id: ObjectId,
    ) -> Result<Result<Vec<String>, Value>, VmError> {
        let length = match self.heap.get(id)? {
            Object::Array(array) => array.length,
            _ => 0,
        };
        let mut keys = Vec::new();
        for index in 0..length {
            let name = index.to_string();
            let item = match self.read_property(
                host,
                &Value::Object(id),
                PropertyKeyRef::String(&name),
            )? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
            let key = match item {
                Value::String(key) => key,
                Value::Number(number) => crate::ops::to_js_string(&Value::Number(number)),
                _ => continue,
            };
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        Ok(Ok(keys))
    }

    fn serialize_property(
        &mut self,
        host: &mut dyn Host,
        state: &mut Serialize<'_>,
        holder: ObjectId,
        key: &str,
    ) -> Result<Result<Option<JsonValue>, Value>, VmError> {
        if state.stack.len() > MAX_DEPTH {
            return self.type_error("Maximum JSON nesting exceeded").map(Err);
        }
        let mut value =
            match self.read_property(host, &Value::Object(holder), PropertyKeyRef::String(key))? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
        if let Value::Object(id) = &value {
            let receiver = Value::Object(*id);
            let method =
                match self.read_property(host, &receiver, PropertyKeyRef::String("toJSON"))? {
                    Invocation::Value(method) => method,
                    Invocation::Thrown(error) => return Ok(Err(error)),
                };
            if self.is_callable(&method) {
                value = match self.invoke(
                    host,
                    &method,
                    vec![Value::String(key.to_owned())],
                    &receiver,
                )? {
                    Invocation::Value(value) => value,
                    Invocation::Thrown(error) => return Ok(Err(error)),
                };
            }
        }
        if let Some(replacer) = state.replacer.cloned() {
            value = match self.invoke(
                host,
                &replacer,
                vec![Value::String(key.to_owned()), value],
                &Value::Object(holder),
            )? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
        }
        self.serialize_value(host, state, value)
    }

    fn serialize_value(
        &mut self,
        host: &mut dyn Host,
        state: &mut Serialize<'_>,
        value: Value,
    ) -> Result<Result<Option<JsonValue>, Value>, VmError> {
        if matches!(value, Value::Undefined | Value::Symbol(_))
            || matches!(&value, Value::Object(id) if self.is_callable(&Value::Object(*id)))
        {
            return Ok(Ok(None));
        }
        let tree = match value {
            Value::Null => JsonValue::Null,
            Value::Bool(value) => JsonValue::Bool(value),
            Value::Number(value) if value.is_finite() => JsonValue::Number(value),
            Value::Number(_) => JsonValue::Null,
            Value::String(value) => JsonValue::String(value),
            Value::Object(id)
                if matches!(
                    self.heap.get(self.heap.proxy_target(id)?)?,
                    Object::Array(_)
                ) =>
            {
                match self.serialize_array(host, state, id)? {
                    Ok(value) => value,
                    Err(error) => return Ok(Err(error)),
                }
            }
            Value::Object(id) => match self.serialize_object(host, state, id)? {
                Ok(value) => value,
                Err(error) => return Ok(Err(error)),
            },
            Value::Undefined | Value::Symbol(_) => return Ok(Ok(None)),
        };
        Ok(Ok(Some(tree)))
    }

    fn serialize_array(
        &mut self,
        host: &mut dyn Host,
        state: &mut Serialize<'_>,
        id: ObjectId,
    ) -> Result<Result<JsonValue, Value>, VmError> {
        if let Some(error) = self.cycle(state, id)? {
            return Ok(Err(error));
        }
        state.stack.push(id);
        let is_proxy = matches!(self.heap.get(id)?, Object::Proxy { .. });
        let length = match self.heap.get(id)? {
            Object::Array(array) => array.length,
            _ => {
                let length = match self.read_property(
                    host,
                    &Value::Object(id),
                    PropertyKeyRef::String("length"),
                )? {
                    Invocation::Value(value) => value,
                    Invocation::Thrown(error) => return Ok(Err(error)),
                };
                match self.coerce_number(host, &length)? {
                    Ok(length) => length.clamp(0.0, 9_007_199_254_740_991.0) as usize,
                    Err(error) => return Ok(Err(error)),
                }
            }
        };
        let mut values = Vec::with_capacity(if is_proxy { 0 } else { length });
        let saved = state.path.clone();
        for index in 0..length {
            state.path = format!("{saved}[{index}]");
            let key = index.to_string();
            let owned = PropertyKey::String(key.clone());
            if !is_proxy && !self.heap.has_own(id, &owned)? {
                values.push(JsonValue::Null);
                continue;
            }
            match self.serialize_property(host, state, id, &key)? {
                Ok(Some(value)) => values.push(value),
                Ok(None) => values.push(JsonValue::Null),
                Err(error) => return Ok(Err(error)),
            }
        }
        state.path = saved;
        state.stack.pop();
        Ok(Ok(JsonValue::Array(values)))
    }

    fn serialize_object(
        &mut self,
        host: &mut dyn Host,
        state: &mut Serialize<'_>,
        id: ObjectId,
    ) -> Result<Result<JsonValue, Value>, VmError> {
        if let Some(error) = self.cycle(state, id)? {
            return Ok(Err(error));
        }
        state.stack.push(id);
        let keys = if let Some(properties) = state.properties {
            properties.to_vec()
        } else {
            match self.enumerable_string_keys(host, id)? {
                Ok(keys) => keys,
                Err(error) => return Ok(Err(error)),
            }
        };
        let mut values = Vec::new();
        let saved = state.path.clone();
        for key in keys {
            state.path = format!("{saved}.{key}");
            match self.serialize_property(host, state, id, &key)? {
                Ok(Some(value)) => values.push((key, value)),
                Ok(None) => {}
                Err(error) => return Ok(Err(error)),
            }
        }
        state.path = saved;
        state.stack.pop();
        Ok(Ok(JsonValue::Object(values)))
    }

    fn cycle(&mut self, state: &Serialize<'_>, id: ObjectId) -> Result<Option<Value>, VmError> {
        if state.stack.contains(&id) {
            self.type_error(&format!(
                "Converting circular structure to JSON at {}",
                state.path
            ))
            .map(Some)
        } else {
            Ok(None)
        }
    }

    fn revive(
        &mut self,
        host: &mut dyn Host,
        reviver: &Value,
        holder: ObjectId,
        key: &str,
    ) -> Result<Result<(), Value>, VmError> {
        if self.heap.object_count() > MAX_DEPTH.saturating_mul(8) && key.is_empty() {
            // Depth is checked while walking, not by object count.
        }
        let initial = match self.read_data(holder, key)? {
            Invocation::Value(value) => value,
            Invocation::Thrown(error) => return Ok(Err(error)),
        };
        if let Value::Object(id) = initial {
            let nested = match self.heap.get(id)? {
                Object::Array(array) => (0..array.length).map(|index| index.to_string()).collect(),
                _ => self
                    .heap
                    .get(id)?
                    .own_keys(true)
                    .filter_map(|key| match key {
                        PropertyKey::String(key) => Some(key),
                        PropertyKey::Symbol(_) => None,
                    })
                    .collect::<Vec<_>>(),
            };
            if nested.len() > MAX_DEPTH {
                return self.type_error("Maximum JSON nesting exceeded").map(Err);
            }
            for name in nested {
                if let Err(error) = self.revive(host, reviver, id, &name)? {
                    return Ok(Err(error));
                }
            }
        }
        let value = match self.read_data(holder, key)? {
            Invocation::Value(value) => value,
            Invocation::Thrown(error) => return Ok(Err(error)),
        };
        let replaced = match self.invoke(
            host,
            reviver,
            vec![Value::String(key.to_owned()), value],
            &Value::Object(holder),
        )? {
            Invocation::Value(value) => value,
            Invocation::Thrown(error) => return Ok(Err(error)),
        };
        if matches!(replaced, Value::Undefined) {
            self.heap.delete_own(holder, PropertyKeyRef::String(key))?;
        } else {
            self.heap
                .set_own(holder, PropertyKey::String(key.to_owned()), replaced)?;
        }
        Ok(Ok(()))
    }

    fn read_data(&mut self, id: ObjectId, key: &str) -> Result<Invocation, VmError> {
        let property = self
            .heap
            .get(id)?
            .own_property(PropertyKeyRef::String(key))
            .map(crate::heap::OwnProperty::into_owned);
        Ok(Invocation::Value(match property {
            Some(crate::heap::Property::Data(value)) => value,
            _ => Value::Undefined,
        }))
    }

    fn materialize_json(&mut self, value: JsonValue) -> Result<Value, VmError> {
        match value {
            JsonValue::Null => Ok(Value::Null),
            JsonValue::Bool(value) => Ok(Value::Bool(value)),
            JsonValue::Number(value) => Ok(Value::Number(value)),
            JsonValue::String(value) => Ok(Value::String(value)),
            JsonValue::Array(values) => {
                let id = self.heap.alloc_array(Vec::new());
                for (index, value) in values.into_iter().enumerate() {
                    let value = self.materialize_json(value)?;
                    self.heap.define_data(
                        id,
                        PropertyKey::String(index.to_string()),
                        value,
                        Attributes::ORDINARY,
                    )?;
                }
                Ok(Value::Object(id))
            }
            JsonValue::Object(values) => {
                let id = self.heap.alloc_plain();
                for (key, value) in values {
                    let value = self.materialize_json(value)?;
                    self.heap.define_data(
                        id,
                        PropertyKey::String(key),
                        value,
                        Attributes::ORDINARY,
                    )?;
                }
                Ok(Value::Object(id))
            }
        }
    }
}
