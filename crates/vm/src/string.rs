//! UTF-16 string operations.

use crate::JsString;
use crate::error::VmError;
use crate::heap::{
    BoxedPrimitive, MATCH_ALL_SYMBOL, MATCH_SYMBOL, Object, REPLACE_SYMBOL, SEARCH_SYMBOL,
    SPLIT_SYMBOL,
};
use crate::host::Host;
use crate::regexp_runtime::RegExpMethod;
use crate::runtime::Invocation;
use crate::value::{PropertyKeyRef, SymbolId, Value};
use crate::vm::Vm;

const MAX_LENGTH: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy)]
enum StringMethod {
    CharAt,
    CharCodeAt,
    CodePointAt,
    At,
    IndexOf,
    LastIndexOf,
    Includes,
    StartsWith,
    EndsWith,
    Slice,
    Substring,
    ToUpperCase,
    ToLowerCase,
    Trim,
    TrimStart,
    TrimEnd,
    Split,
    Concat,
    Repeat,
    PadStart,
    PadEnd,
    Replace,
    ReplaceAll,
    MatchAll,
    Match,
    Search,
    ToString,
    ValueOf,
}

#[derive(Clone, Copy)]
enum StringStatic {
    FromCharCode,
    FromCodePoint,
    Raw,
}

#[yield_binding_macros::builtin_global(name = "String", root = "string", kind = "constructor")]
pub(crate) mod members {
    use super::{Host, Invocation, StringMethod, StringStatic, Value, Vm, VmError};

    #[methods(
        charAt = StringMethod::CharAt,
        charCodeAt = StringMethod::CharCodeAt,
        codePointAt = StringMethod::CodePointAt,
        at = StringMethod::At,
        indexOf = StringMethod::IndexOf,
        lastIndexOf = StringMethod::LastIndexOf,
        includes = StringMethod::Includes,
        startsWith = StringMethod::StartsWith,
        endsWith = StringMethod::EndsWith,
        slice = StringMethod::Slice,
        substring = StringMethod::Substring,
        toUpperCase = StringMethod::ToUpperCase,
        toLowerCase = StringMethod::ToLowerCase,
        trim = StringMethod::Trim,
        trimStart = StringMethod::TrimStart,
        trimEnd = StringMethod::TrimEnd,
        split = StringMethod::Split,
        concat = StringMethod::Concat,
        repeat = StringMethod::Repeat,
        padStart = StringMethod::PadStart,
        padEnd = StringMethod::PadEnd,
        replace = StringMethod::Replace,
        replaceAll = StringMethod::ReplaceAll,
        matchAll = StringMethod::MatchAll,
        r#match = StringMethod::Match,
        search = StringMethod::Search,
        toString = StringMethod::ToString,
        valueOf = StringMethod::ValueOf,
    )]
    fn call_method(
        vm: &mut Vm,
        host: &mut dyn Host,
        key: StringMethod,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        vm.call_string_method(host, key, receiver, args)
    }

    #[functions(
        fromCharCode = StringStatic::FromCharCode,
        fromCodePoint = StringStatic::FromCodePoint,
        raw = StringStatic::Raw,
    )]
    fn call_static(
        vm: &mut Vm,
        host: &mut dyn Host,
        key: StringStatic,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        vm.call_string_static(host, key, args)
    }
}

impl Vm {
    pub(crate) fn call_string(
        &mut self,
        host: &mut dyn Host,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let Some(value) = args.first() else {
            return Ok(Invocation::Value(Value::String(JsString::default())));
        };
        self.string_value(host, value)
    }

    fn call_string_method(
        &mut self,
        host: &mut dyn Host,
        method: StringMethod,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        if matches!(method, StringMethod::ToString | StringMethod::ValueOf) {
            return self.string_primitive(receiver);
        }
        if matches!(receiver, Value::Undefined | Value::Null) {
            return self.throw_type_error("String method requires a non-null receiver");
        }
        if matches!(
            method,
            StringMethod::Match
                | StringMethod::MatchAll
                | StringMethod::Search
                | StringMethod::Replace
                | StringMethod::ReplaceAll
                | StringMethod::Split
        ) && let Some(result) = self.string_pattern_method(host, method, receiver, args)?
        {
            return Ok(result);
        }
        let text = match self.coerce_string(host, receiver)? {
            Ok(text) => text,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        match method {
            StringMethod::CharAt => self.char_at(&text, host, arg(args, 0)),
            StringMethod::CharCodeAt => self.char_code_at(&text, host, arg(args, 0)),
            StringMethod::CodePointAt => self.code_point_at(&text, host, arg(args, 0)),
            StringMethod::At => self.string_at(&text, host, arg(args, 0)),
            StringMethod::IndexOf => self.index_of(&text, host, args, false),
            StringMethod::LastIndexOf => self.index_of(&text, host, args, true),
            StringMethod::Includes => self.string_includes(&text, host, args),
            StringMethod::StartsWith => self.string_affix(&text, host, args, true),
            StringMethod::EndsWith => self.string_affix(&text, host, args, false),
            StringMethod::Slice => self.string_slice(&text, host, args),
            StringMethod::Substring => self.string_substring(&text, host, args),
            StringMethod::ToUpperCase => string_case(&text, true),
            StringMethod::ToLowerCase => string_case(&text, false),
            StringMethod::Trim => string_trim(&text, true, true),
            StringMethod::TrimStart => string_trim(&text, true, false),
            StringMethod::TrimEnd => string_trim(&text, false, true),
            StringMethod::Split => self.string_split(&text, host, args),
            StringMethod::Concat => self.string_concat(&text, host, args),
            StringMethod::Repeat => self.string_repeat(&text, host, arg(args, 0)),
            StringMethod::PadStart => self.string_pad(&text, host, args, true),
            StringMethod::PadEnd => self.string_pad(&text, host, args, false),
            StringMethod::Replace => self.string_replace(&text, host, args),
            StringMethod::ReplaceAll => self.string_replace_all(&text, host, args),
            StringMethod::Match
            | StringMethod::MatchAll
            | StringMethod::Search
            | StringMethod::ToString
            | StringMethod::ValueOf => unreachable!("handled before ToString"),
        }
    }

    fn call_string_static(
        &mut self,
        host: &mut dyn Host,
        method: StringStatic,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        match method {
            StringStatic::FromCharCode => self.string_from_char_code(host, args),
            StringStatic::FromCodePoint => self.string_from_code_point(host, args),
            StringStatic::Raw => self.string_raw(host, args),
        }
    }

    fn string_primitive(&mut self, receiver: &Value) -> Result<Invocation, VmError> {
        let text = match receiver {
            Value::String(text) => Some(text),
            Value::Object(id) => match self.heap.get(*id)? {
                Object::Boxed {
                    primitive: BoxedPrimitive::String(text),
                    ..
                } => Some(text),
                _ => None,
            },
            _ => None,
        };
        match text {
            Some(text) => Ok(Invocation::Value(Value::String(text.clone()))),
            None => self.throw_type_error("String method called on incompatible receiver"),
        }
    }

    fn char_at(
        &mut self,
        text: &JsString,
        host: &mut dyn Host,
        index: &Value,
    ) -> Result<Invocation, VmError> {
        let unit = match self.relative_index(host, index, utf16_len(text), false)? {
            Ok(index) => index.and_then(|index| utf16_unit(text, index)),
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        Ok(Invocation::Value(Value::String(
            unit.map(utf16_unit_string).unwrap_or_default(),
        )))
    }

    fn char_code_at(
        &mut self,
        text: &JsString,
        host: &mut dyn Host,
        index: &Value,
    ) -> Result<Invocation, VmError> {
        let index = match self.relative_index(host, index, utf16_len(text), false)? {
            Ok(index) => index,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let code = index.and_then(|index| utf16_unit(text, index));
        Ok(Invocation::Value(Value::Number(
            code.map_or(f64::NAN, f64::from),
        )))
    }

    fn code_point_at(
        &mut self,
        text: &JsString,
        host: &mut dyn Host,
        index: &Value,
    ) -> Result<Invocation, VmError> {
        let index = match self.relative_index(host, index, utf16_len(text), false)? {
            Ok(index) => index,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        Ok(Invocation::Value(
            match index.and_then(|index| code_point_at(text, index)) {
                Some(point) => Value::Number(f64::from(point)),
                None => Value::Undefined,
            },
        ))
    }

    fn string_at(
        &mut self,
        text: &JsString,
        host: &mut dyn Host,
        index: &Value,
    ) -> Result<Invocation, VmError> {
        let index = match self.relative_index(host, index, utf16_len(text), true)? {
            Ok(index) => index,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        Ok(Invocation::Value(
            match index.and_then(|index| utf16_unit(text, index)) {
                Some(unit) => Value::String(utf16_unit_string(unit)),
                None => Value::Undefined,
            },
        ))
    }

    fn index_of(
        &mut self,
        text: &JsString,
        host: &mut dyn Host,
        args: &[Value],
        last: bool,
    ) -> Result<Invocation, VmError> {
        let needle = match self.coerce_string(host, arg(args, 0))? {
            Ok(needle) => needle,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let length = utf16_len(text);
        let start = match args.get(1) {
            Some(value) if !matches!(value, Value::Undefined) => {
                match self.integer_index(host, value, last)? {
                    Ok(value) => value,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
            }
            _ if last => f64::INFINITY,
            _ => 0.0,
        };
        let position = if last {
            find_last(text, &needle, start)
        } else {
            find_first(text, &needle, start, length)
        };
        Ok(Invocation::Value(Value::Number(position)))
    }

    fn string_includes(
        &mut self,
        text: &JsString,
        host: &mut dyn Host,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let needle = match self.coerce_string(host, arg(args, 0))? {
            Ok(needle) => needle,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let start = match self.clamp_index(host, arg(args, 1), utf16_len(text))? {
            Ok(start) => start,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        Ok(Invocation::Value(Value::Bool(
            find_first(text, &needle, start as f64, utf16_len(text)) >= 0.0,
        )))
    }

    fn string_affix(
        &mut self,
        text: &JsString,
        host: &mut dyn Host,
        args: &[Value],
        start: bool,
    ) -> Result<Invocation, VmError> {
        let search = match self.coerce_string(host, arg(args, 0))? {
            Ok(search) => search,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let length = utf16_len(text);
        let end = if start {
            length
        } else {
            match args.get(1) {
                Some(value) if !matches!(value, Value::Undefined) => {
                    match self.clamp_index(host, value, length)? {
                        Ok(end) => end,
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    }
                }
                _ => length,
            }
        };
        let matched = if start {
            let offset = match self.clamp_index(host, arg(args, 1), length)? {
                Ok(offset) => offset,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            search.unit_len() <= length - offset && matches_at(text, &search, offset)
        } else {
            search.unit_len() <= end && matches_at(text, &search, end - search.unit_len())
        };
        Ok(Invocation::Value(Value::Bool(matched)))
    }

    fn string_slice(
        &mut self,
        text: &JsString,
        host: &mut dyn Host,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let length = utf16_len(text) as f64;
        let clamp = |index: f64| {
            (if index < 0.0 {
                (length + index).max(0.0)
            } else {
                index.min(length)
            }) as usize
        };
        let start = match self.integer_index(host, arg(args, 0), false)? {
            Ok(start) => clamp(start),
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let end = match args.get(1) {
            Some(value) if !matches!(value, Value::Undefined) => {
                match self.integer_index(host, value, false)? {
                    Ok(end) => clamp(end),
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
            }
            _ => length as usize,
        };
        Ok(Invocation::Value(Value::String(slice_units(
            text,
            start,
            end.max(start),
        ))))
    }

    fn string_substring(
        &mut self,
        text: &JsString,
        host: &mut dyn Host,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let length = utf16_len(text) as f64;
        let start = match self.integer_index(host, arg(args, 0), false)? {
            Ok(start) => start.clamp(0.0, length) as usize,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let end = match args.get(1) {
            Some(value) if !matches!(value, Value::Undefined) => {
                match self.integer_index(host, value, false)? {
                    Ok(end) => end.clamp(0.0, length) as usize,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
            }
            _ => length as usize,
        };
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        Ok(Invocation::Value(Value::String(slice_units(
            text, start, end,
        ))))
    }

    fn string_split(
        &mut self,
        text: &JsString,
        host: &mut dyn Host,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let limit = match args.get(1) {
            Some(value) if !matches!(value, Value::Undefined) => {
                match self.coerce_number(host, value)? {
                    Ok(number) => crate::ops::number_to_uint32(number) as usize,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
            }
            _ => usize::MAX,
        };
        let mut parts = Vec::new();
        if limit == 0 {
            return self.array_result(parts);
        }
        match args.first() {
            None | Some(Value::Undefined) => parts.push(Value::String(text.to_owned())),
            Some(separator) => {
                let separator = match self.coerce_string(host, separator)? {
                    Ok(separator) => separator,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                if separator.is_empty() {
                    parts.extend(
                        text.units()
                            .take(limit)
                            .map(|unit| Value::String(utf16_unit_string(unit))),
                    );
                } else {
                    let mut cursor = 0;
                    while parts.len() < limit {
                        if let Some(found) = find_from(text, &separator, cursor) {
                            parts.push(Value::String(slice_units(text, cursor, found)));
                            cursor = found + utf16_len(&separator);
                        } else {
                            parts.push(Value::String(slice_units(text, cursor, utf16_len(text))));
                            break;
                        }
                    }
                }
            }
        }
        self.array_result(parts)
    }

    fn string_concat(
        &mut self,
        text: &JsString,
        host: &mut dyn Host,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let mut parts = Vec::with_capacity(args.len() + 1);
        parts.push(text.to_owned());
        let mut length = text.len();
        let mut last_unit = text.units().next_back();
        for arg in args {
            match self.coerce_string(host, arg)? {
                Ok(part) => {
                    let saves_pair = matches!(last_unit, Some(0xd800..=0xdbff))
                        && matches!(part.units().next(), Some(0xdc00..=0xdfff));
                    length = length.saturating_add(part.len()) - if saves_pair { 2 } else { 0 };
                    if length > MAX_LENGTH {
                        return self.range_error("Invalid string length");
                    }
                    if let Some(unit) = part.units().next_back() {
                        last_unit = Some(unit);
                    }
                    parts.push(part);
                }
                Err(error) => return Ok(Invocation::Thrown(error)),
            }
        }
        let unit_len = parts.iter().map(JsString::unit_len).sum();
        if parts.iter().all(JsString::is_latin1) {
            let mut out = Vec::with_capacity(unit_len);
            for part in &parts {
                out.extend(part.units().map(|unit| unit as u8));
            }
            Ok(Invocation::Value(Value::String(JsString::from_latin1(out))))
        } else {
            let mut out = Vec::with_capacity(unit_len);
            for part in &parts {
                out.extend(part.units());
            }
            Ok(Invocation::Value(Value::String(JsString::from_wide(out))))
        }
    }

    fn string_repeat(
        &mut self,
        text: &JsString,
        host: &mut dyn Host,
        count: &Value,
    ) -> Result<Invocation, VmError> {
        let count = match self.integer_index(host, count, false)? {
            Ok(count) => count,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        if count < 0.0 || !count.is_finite() {
            return self.range_error("Invalid count value");
        }
        let count = count as usize;
        if count == 0 || text.is_empty() {
            return Ok(Invocation::Value(Value::String(JsString::default())));
        }
        let first = text.len();
        let extra = first - pair_savings(text, text);
        if first.saturating_add(extra.saturating_mul(count - 1)) > MAX_LENGTH {
            return self.range_error("Invalid string length");
        }
        if text.is_latin1() {
            let mut out = Vec::with_capacity(text.unit_len() * count);
            for _ in 0..count {
                out.extend(text.units().map(|unit| unit as u8));
            }
            Ok(Invocation::Value(Value::String(JsString::from_latin1(out))))
        } else {
            let mut out = Vec::with_capacity(text.unit_len() * count);
            for _ in 0..count {
                out.extend(text.units());
            }
            Ok(Invocation::Value(Value::String(JsString::from_wide(out))))
        }
    }

    fn string_pad(
        &mut self,
        text: &JsString,
        host: &mut dyn Host,
        args: &[Value],
        start: bool,
    ) -> Result<Invocation, VmError> {
        let target = match self.integer_index(host, arg(args, 0), false)? {
            Ok(target) => target.max(0.0) as usize,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let fill = match args.get(1) {
            Some(value) if !matches!(value, Value::Undefined) => {
                match self.coerce_string(host, value)? {
                    Ok(fill) => fill,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
            }
            _ => " ".into(),
        };
        let length = utf16_len(text);
        if target <= length || fill.is_empty() {
            return Ok(Invocation::Value(Value::String(text.to_owned())));
        }
        let pad_len = target - length;
        if target > MAX_LENGTH {
            return self.range_error("Invalid string length");
        }
        let (copies, tail) = (pad_len / fill.unit_len(), pad_len % fill.unit_len());
        let wide = !text.is_latin1() || fill.units().take(pad_len).any(|unit| unit > 0xff);
        if wide {
            let mut out = Vec::with_capacity(target);
            if !start {
                out.extend(text.units());
            }
            for _ in 0..copies {
                out.extend(fill.units());
            }
            out.extend(fill.units().take(tail));
            if start {
                out.extend(text.units());
            }
            let out = JsString::from_wide(out);
            if out.len() > MAX_LENGTH {
                return self.range_error("Invalid string length");
            }
            Ok(Invocation::Value(Value::String(out)))
        } else {
            let mut out = Vec::with_capacity(target);
            if !start {
                out.extend(text.units().map(|unit| unit as u8));
            }
            for _ in 0..copies {
                out.extend(fill.units().map(|unit| unit as u8));
            }
            out.extend(fill.units().take(tail).map(|unit| unit as u8));
            if start {
                out.extend(text.units().map(|unit| unit as u8));
            }
            let out = JsString::from_latin1(out);
            if out.len() > MAX_LENGTH {
                return self.range_error("Invalid string length");
            }
            Ok(Invocation::Value(Value::String(out)))
        }
    }

    fn string_replace(
        &mut self,
        text: &JsString,
        host: &mut dyn Host,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let search = match self.coerce_string(host, arg(args, 0))? {
            Ok(search) => search,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let Some(position) = find_from(text, &search, 0) else {
            return Ok(Invocation::Value(Value::String(text.to_owned())));
        };
        let matched = search;
        let end = position + utf16_len(&matched);
        let replacement = match args.get(1) {
            Some(value) if self.is_callable(value) => {
                match self.invoke(
                    host,
                    value,
                    vec![
                        Value::String(matched),
                        Value::Number(position as f64),
                        Value::String(text.to_owned()),
                    ],
                    &Value::Undefined,
                )? {
                    Invocation::Value(value) => match self.coerce_string(host, &value)? {
                        Ok(value) => value,
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    },
                    thrown @ Invocation::Thrown(_) => return Ok(thrown),
                }
            }
            Some(value) => match self.coerce_string(host, value)? {
                Ok(pattern) => substitute(&pattern, text, &matched, position),
                Err(error) => return Ok(Invocation::Thrown(error)),
            },
            None => "undefined".into(),
        };
        let mut out = slice_units(text, 0, position);
        out.push_str(&replacement);
        out.push_str(&slice_units(text, end, utf16_len(text)));
        Ok(Invocation::Value(Value::String(out)))
    }

    /// Runs the `String.prototype` protocol lookup. `None` means the caller
    /// must fall back to the plain string algorithm.
    fn string_pattern_method(
        &mut self,
        host: &mut dyn Host,
        method: StringMethod,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Option<Invocation>, VmError> {
        let search = args.first().cloned().unwrap_or(Value::Undefined);
        let (symbol, requires_global, builtin) = match method {
            StringMethod::Match => (MATCH_SYMBOL, false, Some(RegExpMethod::Match)),
            StringMethod::MatchAll => (MATCH_ALL_SYMBOL, true, Some(RegExpMethod::MatchAll)),
            StringMethod::Search => (SEARCH_SYMBOL, false, Some(RegExpMethod::Search)),
            StringMethod::Replace => (REPLACE_SYMBOL, false, None),
            StringMethod::ReplaceAll => (REPLACE_SYMBOL, true, None),
            StringMethod::Split => (SPLIT_SYMBOL, false, None),
            _ => return Ok(None),
        };
        if requires_global {
            match self.is_regexp(host, &search)? {
                Ok(true) => {
                    let flags =
                        match self.read_property(host, &search, PropertyKeyRef::String("flags"))? {
                            Invocation::Value(value) => value,
                            Invocation::Thrown(error) => {
                                return Ok(Some(Invocation::Thrown(error)));
                            }
                        };
                    let flags = match self.coerce_string(host, &flags)? {
                        Ok(flags) => flags,
                        Err(error) => return Ok(Some(Invocation::Thrown(error))),
                    };
                    if !flags.units().any(|unit| unit == u16::from(b'g')) {
                        return Ok(Some(self.throw_type_error(
                            "String.prototype method requires a global RegExp argument",
                        )?));
                    }
                }
                Ok(false) => {}
                Err(error) => return Ok(Some(Invocation::Thrown(error))),
            }
        }
        let protocol = if matches!(search, Value::Undefined | Value::Null) {
            Value::Undefined
        } else {
            match self.get_method(host, &search, symbol)? {
                Ok(method) => method,
                Err(error) => return Ok(Some(Invocation::Thrown(error))),
            }
        };
        if !matches!(protocol, Value::Undefined) {
            let this = receiver.clone();
            let call_args = match method {
                StringMethod::Replace | StringMethod::ReplaceAll => {
                    vec![this, args.get(1).cloned().unwrap_or(Value::Undefined)]
                }
                StringMethod::Split => vec![this, args.get(1).cloned().unwrap_or(Value::Undefined)],
                _ => vec![this],
            };
            return Ok(Some(self.invoke(host, &protocol, call_args, &search)?));
        }
        let Some(builtin) = builtin else {
            return Ok(None);
        };
        let flags = if matches!(method, StringMethod::MatchAll) {
            Value::String("g".into())
        } else {
            Value::Undefined
        };
        let created = self.construct_regexp(host, &[search.clone(), flags], None, false)?;
        let Invocation::Value(Value::Object(id)) = created else {
            return Ok(Some(created));
        };
        Ok(Some(self.call_regexp_method(
            host,
            builtin,
            &Value::Object(id),
            std::slice::from_ref(receiver),
        )?))
    }

    fn get_method(
        &mut self,
        host: &mut dyn Host,
        value: &Value,
        symbol: SymbolId,
    ) -> Result<Result<Value, Value>, VmError> {
        match self.read_property(host, value, PropertyKeyRef::Symbol(symbol))? {
            Invocation::Value(Value::Undefined | Value::Null) => Ok(Ok(Value::Undefined)),
            Invocation::Value(method) => Ok(Ok(method)),
            Invocation::Thrown(error) => Ok(Err(error)),
        }
    }

    fn is_regexp(
        &mut self,
        host: &mut dyn Host,
        value: &Value,
    ) -> Result<Result<bool, Value>, VmError> {
        let Value::Object(id) = *value else {
            return Ok(Ok(false));
        };
        let matcher = match self.read_property(host, value, PropertyKeyRef::Symbol(MATCH_SYMBOL))? {
            Invocation::Value(matcher) => matcher,
            Invocation::Thrown(error) => return Ok(Err(error)),
        };
        if !matches!(matcher, Value::Undefined) {
            return Ok(Ok(true));
        }
        Ok(Ok(matches!(self.heap.get(id)?, Object::RegExp { .. })))
    }

    fn string_replace_all(
        &mut self,
        text: &JsString,
        host: &mut dyn Host,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let search = match self.coerce_string(host, arg(args, 0))? {
            Ok(search) => search,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let replacement_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
        let functional = self.is_callable(&replacement_arg);
        let pattern = if functional {
            JsString::default()
        } else {
            match self.coerce_string(host, &replacement_arg)? {
                Ok(pattern) => pattern,
                Err(error) => return Ok(Invocation::Thrown(error)),
            }
        };
        let search_length = utf16_len(&search);
        let Some(mut position) = find_from(text, &search, 0) else {
            return Ok(Invocation::Value(Value::String(text.clone())));
        };
        let has_substitution = !functional && pattern.units().any(|unit| unit == u16::from(b'$'));
        let mut out = Vec::new();
        let mut source = text.units();
        let mut last = 0usize;
        loop {
            out.extend(source.by_ref().take(position - last));
            let replacement = if functional {
                match self.invoke(
                    host,
                    &replacement_arg,
                    vec![
                        Value::String(search.clone()),
                        Value::Number(position as f64),
                        Value::String(text.clone()),
                    ],
                    &Value::Undefined,
                )? {
                    Invocation::Value(value) => match self.coerce_string(host, &value)? {
                        Ok(value) => value,
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    },
                    thrown @ Invocation::Thrown(_) => return Ok(thrown),
                }
            } else if has_substitution {
                substitute(&pattern, text, &search, position)
            } else {
                pattern.clone()
            };
            out.extend(replacement.units());
            if search_length != 0 {
                source.nth(search_length - 1);
            }
            last = position + search_length;
            let cursor = if search_length == 0 {
                position.saturating_add(1)
            } else {
                last
            };
            if cursor > text.unit_len() {
                break;
            }
            let Some(next) = find_from(text, &search, cursor) else {
                break;
            };
            position = next;
        }
        out.extend(source);
        Ok(Invocation::Value(Value::String(JsString::from_units(out))))
    }

    fn string_from_char_code(
        &mut self,
        host: &mut dyn Host,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let mut units = Vec::with_capacity(args.len());
        for arg in args {
            let number = match self.coerce_number(host, arg)? {
                Ok(number) => number,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            units.push(crate::ops::number_to_uint32(number) as u16);
        }
        Ok(Invocation::Value(Value::String(JsString::from_units(
            units,
        ))))
    }

    fn string_from_code_point(
        &mut self,
        host: &mut dyn Host,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let mut out = Vec::with_capacity(args.len());
        for arg in args {
            let number = match self.coerce_number(host, arg)? {
                Ok(number) => number,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            if !(0.0..=0x10_ffff as f64).contains(&number) || number.fract() != 0.0 {
                return self.range_error("Invalid code point");
            }
            let point = number as u32;
            if point <= 0xffff {
                out.push(point as u16);
            } else {
                let offset = point - 0x10000;
                out.extend([
                    0xd800 + (offset >> 10) as u16,
                    0xdc00 + (offset & 0x3ff) as u16,
                ]);
            }
        }
        Ok(Invocation::Value(Value::String(JsString::from_units(out))))
    }

    fn string_raw(&mut self, host: &mut dyn Host, args: &[Value]) -> Result<Invocation, VmError> {
        let template = arg(args, 0);
        let raw = match self.read_member(host, template, &Value::String("raw".into()))? {
            Invocation::Value(value) => value,
            thrown @ Invocation::Thrown(_) => return Ok(thrown),
        };
        let Value::Object(raw_id) = raw else {
            return self.throw_type_error("String.raw requires a raw array");
        };
        let raw_object = Value::Object(raw_id);
        let length = match self.read_member(host, &raw_object, &Value::String("length".into()))? {
            Invocation::Value(value) => value,
            thrown @ Invocation::Thrown(_) => return Ok(thrown),
        };
        let length = match self.coerce_number(host, &length)? {
            Ok(length) => crate::ops::number_to_uint32(length) as usize,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let mut out = JsString::default();
        let mut segment_index = 0;
        while segment_index < length {
            let key = Value::String(segment_index.to_string().into());
            let segment = match self.read_member(host, &raw_object, &key)? {
                Invocation::Value(value) => value,
                thrown @ Invocation::Thrown(_) => return Ok(thrown),
            };
            match self.coerce_string(host, &segment)? {
                Ok(part) => out.push_str(&part),
                Err(error) => return Ok(Invocation::Thrown(error)),
            }
            segment_index += 1;
            if segment_index < length
                && let Some(substitution) = args.get(segment_index)
            {
                match self.coerce_string(host, substitution)? {
                    Ok(part) => out.push_str(&part),
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
            }
        }
        Ok(Invocation::Value(Value::String(out)))
    }

    fn relative_index(
        &mut self,
        host: &mut dyn Host,
        value: &Value,
        length: usize,
        relative: bool,
    ) -> Result<Result<Option<usize>, Value>, VmError> {
        let number = match self.integer_index(host, value, false)? {
            Ok(number) => number,
            Err(thrown) => return Ok(Err(thrown)),
        };
        if !number.is_finite() {
            return Ok(Ok(None));
        }
        let index = if relative && number < 0.0 {
            number + length as f64
        } else {
            number
        };
        Ok(Ok(if index < 0.0 || index >= length as f64 {
            None
        } else {
            Some(index as usize)
        }))
    }

    fn integer_index(
        &mut self,
        host: &mut dyn Host,
        value: &Value,
        infinity: bool,
    ) -> Result<Result<f64, Value>, VmError> {
        let number = match self.coerce_number(host, value)? {
            Ok(number) => number,
            Err(error) => return Ok(Err(error)),
        };
        Ok(Ok(if number.is_nan() {
            if infinity { f64::INFINITY } else { 0.0 }
        } else {
            number.trunc()
        }))
    }

    fn clamp_index(
        &mut self,
        host: &mut dyn Host,
        value: &Value,
        length: usize,
    ) -> Result<Result<usize, Value>, VmError> {
        match self.integer_index(host, value, false)? {
            Ok(index) => Ok(Ok(index.clamp(0.0, length as f64) as usize)),
            Err(error) => Ok(Err(error)),
        }
    }

    pub(crate) fn array_result(&mut self, elements: Vec<Value>) -> Result<Invocation, VmError> {
        Ok(Invocation::Value(Value::Object(
            self.heap.alloc_array(elements),
        )))
    }

    fn range_error(&mut self, message: &str) -> Result<Invocation, VmError> {
        self.alloc_range_error(message).map(Invocation::Thrown)
    }

    fn alloc_range_error(&mut self, message: &str) -> Result<Value, VmError> {
        self.alloc_error(
            self.builtins.rangeerror_prototype,
            "RangeError",
            Some(message.into()),
        )
    }
}

fn arg(args: &[Value], index: usize) -> &Value {
    args.get(index).unwrap_or(&Value::Undefined)
}

pub(crate) fn utf16_len(text: &JsString) -> usize {
    text.unit_len()
}

pub(crate) fn utf16_unit(text: &JsString, index: usize) -> Option<u16> {
    text.unit_at(index)
}

pub(crate) fn utf16_unit_string(unit: u16) -> JsString {
    JsString::from_units(vec![unit])
}

fn pair_savings(left: &JsString, right: &JsString) -> usize {
    if matches!(left.units().next_back(), Some(0xd800..=0xdbff))
        && matches!(right.units().next(), Some(0xdc00..=0xdfff))
    {
        2 // Three WTF-8 bytes per lone surrogate, four for the joined pair.
    } else {
        0
    }
}

fn code_point_at(text: &JsString, index: usize) -> Option<u32> {
    let first = u32::from(utf16_unit(text, index)?);
    if (0xD800..=0xDBFF).contains(&first)
        && let Some(second) = utf16_unit(text, index + 1).map(u32::from)
        && (0xDC00..=0xDFFF).contains(&second)
    {
        return Some(0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00));
    }
    Some(first)
}

fn slice_units(text: &JsString, start: usize, end: usize) -> JsString {
    text.slice_units(start..end)
}

fn matches_at(text: &JsString, needle: &JsString, start: usize) -> bool {
    needle
        .units()
        .enumerate()
        .all(|(offset, unit)| text.unit_at(start + offset) == Some(unit))
}

pub(crate) fn find_from(text: &JsString, needle: &JsString, start: usize) -> Option<usize> {
    let start = start.min(text.unit_len());
    if needle.is_empty() {
        return Some(start);
    }
    if let (Some(text), Some(needle)) = (text.latin1_bytes(), needle.latin1_bytes()) {
        return text
            .get(start..)?
            .windows(needle.len())
            .position(|window| window == needle)
            .map(|position| start + position);
    }
    let last = text.unit_len().checked_sub(needle.unit_len())?;
    (start..=last).find(|&position| matches_at(text, needle, position))
}

fn find_first(text: &JsString, needle: &JsString, start: f64, length: usize) -> f64 {
    find_from(text, needle, start.clamp(0.0, length as f64) as usize)
        .map(|position| position as f64)
        .unwrap_or(-1.0)
}

fn find_last(text: &JsString, needle: &JsString, start: f64) -> f64 {
    let length = text.unit_len();
    let needle_length = needle.unit_len();
    let end = (start as usize).saturating_add(needle_length).min(length);
    if needle.is_empty() {
        return end as f64;
    }
    let Some(last) = end.checked_sub(needle_length) else {
        return -1.0;
    };
    if let (Some(text), Some(needle)) = (text.latin1_bytes(), needle.latin1_bytes()) {
        return text
            .get(..last + needle.len())
            .and_then(|text| {
                text.windows(needle.len())
                    .rposition(|window| window == needle)
            })
            .map(|position| position as f64)
            .unwrap_or(-1.0);
    }
    (0..=last)
        .rev()
        .find(|&position| matches_at(text, needle, position))
        .map(|position| position as f64)
        .unwrap_or(-1.0)
}

fn string_case(text: &JsString, upper: bool) -> Result<Invocation, VmError> {
    // Case whole scalar runs so context-sensitive mappings (e.g. final sigma)
    // still see their neighbors. Lone surrogates delimit runs and survive unchanged.
    let mut run = String::new();
    let mut out = Vec::with_capacity(utf16_len(text));
    let convert = |run: &str| {
        if upper {
            run.to_uppercase()
        } else {
            run.to_lowercase()
        }
    };
    for item in char::decode_utf16(text.units()) {
        match item {
            Ok(ch) => run.push(ch),
            Err(surrogate) => {
                if !run.is_empty() {
                    out.extend(convert(&run).encode_utf16());
                    run.clear();
                }
                out.push(surrogate.unpaired_surrogate());
            }
        }
    }
    if !run.is_empty() {
        out.extend(convert(&run).encode_utf16());
    }
    Ok(Invocation::Value(Value::String(JsString::from_units(out))))
}

fn string_trim(text: &JsString, start: bool, end: bool) -> Result<Invocation, VmError> {
    let length = text.unit_len();
    let whitespace = |unit: &u16| char::from_u32(u32::from(*unit)).is_some_and(is_trim_whitespace);
    let first = if start {
        text.units().take_while(whitespace).count()
    } else {
        0
    };
    let last = if end {
        length
            - text
                .units()
                .rev()
                .take(length - first)
                .take_while(whitespace)
                .count()
    } else {
        length
    };
    Ok(Invocation::Value(Value::String(slice_units(
        text, first, last,
    ))))
}

fn is_trim_whitespace(ch: char) -> bool {
    matches!(
        ch,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{FEFF}'
            | '\u{2028}'
            | '\u{2029}'
    ) || ch.is_whitespace()
}

fn substitute(
    pattern: &JsString,
    text: &JsString,
    matched: &JsString,
    position: usize,
) -> JsString {
    let mut units = pattern.units().peekable();
    let mut out = Vec::new();
    while let Some(unit) = units.next() {
        if unit != u16::from(b'$') {
            out.push(unit);
            continue;
        }
        match units.peek().copied() {
            Some(0x24) => out.push(unit),
            Some(0x26) => out.extend(matched.units()),
            Some(0x60) => out.extend(text.units().take(position)),
            Some(0x27) => out.extend(text.units().skip(position + utf16_len(matched))),
            _ => {
                out.push(unit);
                continue;
            }
        }
        units.next();
    }
    JsString::from_units(out)
}
