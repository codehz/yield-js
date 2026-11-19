//! UTF-16 string operations. String results containing a lone surrogate are rejected.

use crate::error::VmError;
use crate::heap::{BoxedPrimitive, Object};
use crate::host::Host;
use crate::runtime::Invocation;
use crate::value::Value;
use crate::vm::Vm;

const MAX_LENGTH: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(crate) enum StringMethod {
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
    ToString,
    ValueOf,
}

#[derive(Clone, Copy)]
pub(crate) enum StringStatic {
    FromCharCode,
    FromCodePoint,
    Raw,
}

impl Vm {
    pub(crate) fn call_string(
        &mut self,
        host: &mut dyn Host,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let Some(value) = args.first() else {
            return Ok(Invocation::Value(Value::String(String::new())));
        };
        self.string_value(host, value)
    }

    pub(crate) fn call_string_method(
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
            StringMethod::ToString | StringMethod::ValueOf => unreachable!("handled above"),
        }
    }

    pub(crate) fn call_string_static(
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
        text: &str,
        host: &mut dyn Host,
        index: &Value,
    ) -> Result<Invocation, VmError> {
        let Some(index) = self.relative_index(host, index, utf16_len(text), false)? else {
            return Ok(Invocation::Value(Value::String(String::new())));
        };
        Ok(match unit_at(text, index) {
            Some(unit) => match string_from_unit(unit) {
                Some(value) => Invocation::Value(Value::String(value)),
                None => {
                    return self.throw_type_error("string index cannot represent a lone surrogate");
                }
            },
            None => Invocation::Value(Value::String(String::new())),
        })
    }

    fn char_code_at(
        &mut self,
        text: &str,
        host: &mut dyn Host,
        index: &Value,
    ) -> Result<Invocation, VmError> {
        let Some(index) = self.relative_index(host, index, utf16_len(text), false)? else {
            return Ok(Invocation::Value(Value::Number(f64::NAN)));
        };
        Ok(Invocation::Value(match unit_at(text, index) {
            Some(unit) => Value::Number(f64::from(unit)),
            None => Value::Number(f64::NAN),
        }))
    }

    fn code_point_at(
        &mut self,
        text: &str,
        host: &mut dyn Host,
        index: &Value,
    ) -> Result<Invocation, VmError> {
        let Some(index) = self.relative_index(host, index, utf16_len(text), false)? else {
            return Ok(Invocation::Value(Value::Undefined));
        };
        Ok(Invocation::Value(match code_point_at(text, index) {
            Some(point) => Value::Number(f64::from(point)),
            None => Value::Undefined,
        }))
    }

    fn string_at(
        &mut self,
        text: &str,
        host: &mut dyn Host,
        index: &Value,
    ) -> Result<Invocation, VmError> {
        let Some(index) = self.relative_index(host, index, utf16_len(text), true)? else {
            return Ok(Invocation::Value(Value::Undefined));
        };
        Ok(match unit_at(text, index) {
            Some(unit) => match string_from_unit(unit) {
                Some(value) => Invocation::Value(Value::String(value)),
                None => {
                    return self.throw_type_error("string index cannot represent a lone surrogate");
                }
            },
            None => Invocation::Value(Value::Undefined),
        })
    }

    fn index_of(
        &mut self,
        text: &str,
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
        text: &str,
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
        text: &str,
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
        let offset = if start {
            match self.clamp_index(host, arg(args, 1), length)? {
                Ok(offset) => offset,
                Err(error) => return Ok(Invocation::Thrown(error)),
            }
        } else {
            end.saturating_sub(utf16_len(&search))
        };
        let matched = if start {
            slice_units(text, offset, offset.saturating_add(utf16_len(&search)))
                .is_some_and(|slice| slice == search)
        } else {
            slice_units(text, offset, end).is_some_and(|slice| slice == search)
        };
        Ok(Invocation::Value(Value::Bool(matched)))
    }

    fn string_slice(
        &mut self,
        text: &str,
        host: &mut dyn Host,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let length = utf16_len(text);
        let start = self
            .relative_index(host, arg(args, 0), length, true)?
            .unwrap_or_default();
        let end = match args.get(1) {
            Some(value) if !matches!(value, Value::Undefined) => {
                self.relative_index(host, value, length, true)?.unwrap_or(0)
            }
            _ => length,
        };
        let end = end.max(start);
        match slice_units(text, start, end) {
            Some(slice) => Ok(Invocation::Value(Value::String(slice))),
            None => self.throw_type_error("string slice cannot represent a lone surrogate"),
        }
    }

    fn string_substring(
        &mut self,
        text: &str,
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
        match slice_units(text, start, end) {
            Some(slice) => Ok(Invocation::Value(Value::String(slice))),
            None => self.throw_type_error("string substring cannot represent a lone surrogate"),
        }
    }

    fn string_split(
        &mut self,
        text: &str,
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
                    for (index, unit) in text.encode_utf16().enumerate() {
                        if index == limit {
                            break;
                        }
                        let Some(part) = string_from_unit(unit) else {
                            return self.throw_type_error(
                                "string split cannot represent a lone surrogate",
                            );
                        };
                        parts.push(Value::String(part));
                    }
                } else {
                    let mut cursor = 0;
                    while parts.len() < limit {
                        match find_from(text, &separator, cursor) {
                            Some(found) if found >= cursor => {
                                let Some(part) = slice_units(text, cursor, found) else {
                                    return self.throw_type_error(
                                        "string split cannot represent a lone surrogate",
                                    );
                                };
                                parts.push(Value::String(part));
                                cursor = found + utf16_len(&separator);
                                if cursor > utf16_len(text) {
                                    break;
                                }
                            }
                            _ => {
                                let Some(part) = slice_units(text, cursor, utf16_len(text)) else {
                                    return self.throw_type_error(
                                        "string split cannot represent a lone surrogate",
                                    );
                                };
                                parts.push(Value::String(part));
                                break;
                            }
                        }
                    }
                }
            }
        }
        self.array_result(parts)
    }

    fn string_concat(
        &mut self,
        text: &str,
        host: &mut dyn Host,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let mut out = text.to_owned();
        for arg in args {
            match self.coerce_string(host, arg)? {
                Ok(part) => {
                    if out.len().saturating_add(part.len()) > MAX_LENGTH {
                        return self.range_error("Invalid string length");
                    }
                    out.push_str(&part);
                }
                Err(error) => return Ok(Invocation::Thrown(error)),
            }
        }
        Ok(Invocation::Value(Value::String(out)))
    }

    fn string_repeat(
        &mut self,
        text: &str,
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
        if text.len().saturating_mul(count) > MAX_LENGTH {
            return self.range_error("Invalid string length");
        }
        Ok(Invocation::Value(Value::String(text.repeat(count))))
    }

    fn string_pad(
        &mut self,
        text: &str,
        host: &mut dyn Host,
        args: &[Value],
        start: bool,
    ) -> Result<Invocation, VmError> {
        let target = match self.integer_index(host, arg(args, 0), false)? {
            Ok(target) if target.is_finite() && target >= 0.0 => target as usize,
            Ok(_) => return self.range_error("Invalid string length"),
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let fill = match args.get(1) {
            Some(value) if !matches!(value, Value::Undefined) => {
                match self.coerce_string(host, value)? {
                    Ok(fill) => fill,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
            }
            _ => " ".to_owned(),
        };
        let length = utf16_len(text);
        if target <= length || fill.is_empty() {
            return Ok(Invocation::Value(Value::String(text.to_owned())));
        }
        let pad_len = target - length;
        if pad_len > MAX_LENGTH {
            return self.range_error("Invalid string length");
        }
        let units: Vec<u16> = fill.encode_utf16().collect();
        let mut filler = Vec::new();
        while filler.len() < pad_len {
            filler.extend_from_slice(&units);
        }
        filler.truncate(pad_len);
        let filler = string_from_units(&filler)
            .ok_or_else(|| VmError::InvalidState("padding produced a lone surrogate".to_owned()));
        let filler = match filler {
            Ok(filler) => filler,
            Err(_) => {
                return self.throw_type_error("string padding cannot represent a lone surrogate");
            }
        };
        let mut out = String::new();
        if start {
            out.push_str(&filler);
            out.push_str(text);
        } else {
            out.push_str(text);
            out.push_str(&filler);
        }
        Ok(Invocation::Value(Value::String(out)))
    }

    fn string_replace(
        &mut self,
        text: &str,
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
        let matched = slice_units(text, position, position + utf16_len(&search))
            .ok_or_else(|| VmError::InvalidState("matched string is not UTF-8".to_owned()))?;
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
                Ok(pattern) => substitute(&pattern, text, &matched, position)?,
                Err(error) => return Ok(Invocation::Thrown(error)),
            },
            None => "undefined".to_owned(),
        };
        let prefix = slice_units(text, 0, position)
            .ok_or_else(|| VmError::InvalidState("replace prefix is not UTF-8".to_owned()))?;
        let suffix = slice_units(text, position + utf16_len(&search), utf16_len(text))
            .ok_or_else(|| VmError::InvalidState("replace suffix is not UTF-8".to_owned()))?;
        Ok(Invocation::Value(Value::String(format!(
            "{prefix}{replacement}{suffix}"
        ))))
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
        match string_from_units(&units) {
            Some(text) => Ok(Invocation::Value(Value::String(text))),
            None => self.throw_type_error("fromCharCode cannot represent a lone surrogate"),
        }
    }

    fn string_from_code_point(
        &mut self,
        host: &mut dyn Host,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let mut out = String::new();
        for arg in args {
            let number = match self.coerce_number(host, arg)? {
                Ok(number) => number,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            if !(0.0..=0x10_ffff as f64).contains(&number) || number.fract() != 0.0 {
                return self.range_error("Invalid code point");
            }
            let Some(ch) = char::from_u32(number as u32) else {
                return self.throw_type_error("fromCodePoint cannot represent a lone surrogate");
            };
            out.push(ch);
        }
        Ok(Invocation::Value(Value::String(out)))
    }

    fn string_raw(&mut self, host: &mut dyn Host, args: &[Value]) -> Result<Invocation, VmError> {
        let template = arg(args, 0);
        let raw = match self.read_member(host, template, &Value::String("raw".to_owned()))? {
            Invocation::Value(value) => value,
            thrown @ Invocation::Thrown(_) => return Ok(thrown),
        };
        let Value::Object(raw_id) = raw else {
            return self.throw_type_error("String.raw requires a raw array");
        };
        let raw_object = Value::Object(raw_id);
        let length =
            match self.read_member(host, &raw_object, &Value::String("length".to_owned()))? {
                Invocation::Value(value) => value,
                thrown @ Invocation::Thrown(_) => return Ok(thrown),
            };
        let length = match self.coerce_number(host, &length)? {
            Ok(length) => crate::ops::number_to_uint32(length) as usize,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let mut out = String::new();
        let mut segment_index = 0;
        while segment_index < length {
            let key = Value::String(segment_index.to_string());
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
    ) -> Result<Option<usize>, VmError> {
        let number = match self.integer_index(host, value, false)? {
            Ok(number) => number,
            Err(thrown) => return Err(VmError::Thrown(thrown)),
        };
        if !number.is_finite() {
            return Ok(if number.is_sign_negative() {
                Some(0)
            } else {
                None
            });
        }
        let index = if relative && number < 0.0 {
            number + length as f64
        } else {
            number
        };
        Ok(if index < 0.0 || index >= length as f64 {
            None
        } else {
            Some(index as usize)
        })
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
            0.0
        } else if infinity && number == f64::INFINITY {
            f64::INFINITY
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

    fn array_result(&mut self, elements: Vec<Value>) -> Result<Invocation, VmError> {
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
            Some(message.to_owned()),
        )
    }
}

fn arg(args: &[Value], index: usize) -> &Value {
    args.get(index).unwrap_or(&Value::Undefined)
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn unit_at(text: &str, index: usize) -> Option<u16> {
    text.encode_utf16().nth(index)
}

fn code_point_at(text: &str, index: usize) -> Option<u32> {
    let mut units = text.encode_utf16().skip(index);
    let first = u32::from(units.next()?);
    if (0xD800..=0xDBFF).contains(&first)
        && let Some(second) = units.next().map(u32::from)
        && (0xDC00..=0xDFFF).contains(&second)
    {
        return Some(0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00));
    }
    Some(first)
}

fn string_from_unit(unit: u16) -> Option<String> {
    char::from_u32(u32::from(unit)).map(|ch| ch.to_string())
}

fn string_from_units(units: &[u16]) -> Option<String> {
    char::decode_utf16(units.iter().copied())
        .collect::<Result<String, _>>()
        .ok()
}

fn slice_units(text: &str, start: usize, end: usize) -> Option<String> {
    if start > end {
        return Some(String::new());
    }
    let units: Vec<u16> = text.encode_utf16().skip(start).take(end - start).collect();
    string_from_units(&units)
}

fn find_from(text: &str, needle: &str, start: usize) -> Option<usize> {
    if utf16_len(needle) == 0 {
        return Some(start.min(utf16_len(text)));
    }
    let text: Vec<u16> = text.encode_utf16().collect();
    let needle: Vec<u16> = needle.encode_utf16().collect();
    text[start.min(text.len())..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|position| start.min(text.len()) + position)
}

fn find_first(text: &str, needle: &str, start: f64, length: usize) -> f64 {
    if start > length as f64 {
        return -1.0;
    }
    find_from(text, needle, start.max(0.0) as usize)
        .map(|position| position as f64)
        .unwrap_or(-1.0)
}

fn find_last(text: &str, needle: &str, start: f64) -> f64 {
    let text: Vec<u16> = text.encode_utf16().collect();
    let needle: Vec<u16> = needle.encode_utf16().collect();
    let end = if start.is_infinite() {
        text.len()
    } else {
        (start.max(0.0) as usize)
            .saturating_add(needle.len())
            .min(text.len())
    };
    if needle.is_empty() {
        return end.min(text.len()) as f64;
    }
    text[..end]
        .windows(needle.len())
        .rposition(|window| window == needle)
        .map(|position| position as f64)
        .unwrap_or(-1.0)
}

fn string_case(text: &str, upper: bool) -> Result<Invocation, VmError> {
    let converted = if upper {
        text.to_uppercase()
    } else {
        text.to_lowercase()
    };
    Ok(Invocation::Value(Value::String(converted)))
}

fn string_trim(text: &str, start: bool, end: bool) -> Result<Invocation, VmError> {
    let mut chars = text.chars();
    let out = if start && end {
        text.trim_matches(is_trim_whitespace).to_owned()
    } else if start {
        text.trim_start_matches(is_trim_whitespace).to_owned()
    } else {
        text.trim_end_matches(is_trim_whitespace).to_owned()
    };
    let _ = chars.next();
    Ok(Invocation::Value(Value::String(out)))
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
    pattern: &str,
    text: &str,
    matched: &str,
    position: usize,
) -> Result<String, VmError> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '$' || index + 1 == chars.len() {
            out.push(chars[index]);
            index += 1;
            continue;
        }
        match chars[index + 1] {
            '$' => out.push('$'),
            '&' => out.push_str(matched),
            '`' => out.push_str(&slice_units(text, 0, position).unwrap_or_default()),
            '\'' => out.push_str(
                &slice_units(text, position + utf16_len(matched), utf16_len(text))
                    .unwrap_or_default(),
            ),
            _ => {
                out.push('$');
                out.push(chars[index + 1]);
            }
        }
        index += 2;
    }
    Ok(out)
}
