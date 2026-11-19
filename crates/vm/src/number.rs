//! Number constructors, static functions, and prototype methods.

use crate::error::VmError;
use crate::heap::{Attributes, BoxedPrimitive, Object};
use crate::host::Host;
use crate::ops;
use crate::runtime::Invocation;
use crate::value::{ObjectId, Value};
use crate::vm::Vm;

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
pub(crate) const NUMBER_STATICS: &[&str] = &[
    "isFinite",
    "isInteger",
    "isNaN",
    "isSafeInteger",
    "parseFloat",
    "parseInt",
];
pub(crate) const NUMBER_METHODS: &[&str] = &[
    "toString",
    "valueOf",
    "toFixed",
    "toExponential",
    "toPrecision",
    "toLocaleString",
];

impl Vm {
    pub(crate) fn init_number_constructor(&mut self, prototype: ObjectId) -> ObjectId {
        let constructor = self.heap.alloc_builtin("Number", None);
        self.heap
            .define_data(
                constructor,
                "prototype",
                Value::Object(prototype),
                Attributes::FIXED,
            )
            .expect("Number stores its prototype");
        self.heap
            .define_data(
                prototype,
                "constructor",
                Value::Object(constructor),
                Attributes::METHOD,
            )
            .expect("Number.prototype stores its constructor");
        for (target, names) in [(constructor, NUMBER_STATICS), (prototype, NUMBER_METHODS)] {
            for &name in names {
                let id = self.heap.alloc_builtin("Number", Some(name));
                self.heap
                    .define_data(target, name, Value::Object(id), Attributes::METHOD)
                    .expect("Number stores its builtin methods");
            }
        }
        for (name, number) in [
            ("EPSILON", f64::EPSILON),
            ("MAX_SAFE_INTEGER", MAX_SAFE_INTEGER),
            ("MIN_SAFE_INTEGER", -MAX_SAFE_INTEGER),
            ("MAX_VALUE", f64::MAX),
            ("MIN_VALUE", f64::from_bits(1)),
            ("POSITIVE_INFINITY", f64::INFINITY),
            ("NEGATIVE_INFINITY", f64::NEG_INFINITY),
            ("NaN", f64::NAN),
        ] {
            self.heap
                .define_data(constructor, name, Value::Number(number), Attributes::FIXED)
                .expect("Number stores its constants");
        }
        constructor
    }

    pub(crate) fn call_number(
        &mut self,
        host: &mut dyn Host,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        Ok(match args.first() {
            None => Invocation::Value(Value::Number(0.0)),
            Some(arg) => match self.coerce_number(host, arg)? {
                Ok(number) => Invocation::Value(Value::Number(number)),
                Err(error) => Invocation::Thrown(error),
            },
        })
    }

    pub(crate) fn call_number_static(
        &mut self,
        host: &mut dyn Host,
        method: &str,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let arg = args.first().unwrap_or(&Value::Undefined);
        let value = match method {
            "parseFloat" | "parseInt" => {
                let text = match self.coerce_string(host, arg)? {
                    Ok(text) => text,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                let text = text.trim_start_matches(ops::is_js_whitespace);
                let number = if method == "parseFloat" {
                    parse_float(text)
                } else {
                    let radix = match args.get(1) {
                        None => 0,
                        Some(value) => match self.coerce_number(host, value)? {
                            Ok(number) => ops::number_to_uint32(number) as i32,
                            Err(error) => return Ok(Invocation::Thrown(error)),
                        },
                    };
                    parse_int(text, radix)
                };
                Value::Number(number)
            }
            "isFinite" | "isNaN" | "isInteger" | "isSafeInteger" => {
                Value::Bool(matches!(arg, Value::Number(number) if match method {
                    "isFinite" => number.is_finite(),
                    "isNaN" => number.is_nan(),
                    "isInteger" => number.fract() == 0.0,
                    _ => number.fract() == 0.0 && number.abs() <= MAX_SAFE_INTEGER,
                }))
            }
            _ => unreachable!("registered Number static"),
        };
        Ok(Invocation::Value(value))
    }

    pub(crate) fn call_number_method(
        &mut self,
        host: &mut dyn Host,
        method: &str,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let number = match receiver {
            Value::Number(number) => Some(*number),
            Value::Object(id) => match self.heap.get(*id)? {
                Object::Boxed {
                    primitive: BoxedPrimitive::Number(number),
                    ..
                } => Some(*number),
                _ => None,
            },
            _ => None,
        };
        let Some(number) = number else {
            return self.throw_type_error("Number method called on incompatible receiver");
        };
        if method == "valueOf" {
            return Ok(Invocation::Value(Value::Number(number)));
        }
        let decimal = || ops::to_js_string(&Value::Number(number));
        let string = |text| Ok(Invocation::Value(Value::String(text)));
        let arg = args.first().unwrap_or(&Value::Undefined);
        let omitted = matches!(arg, Value::Undefined);
        if method == "toLocaleString" || (method == "toPrecision" && omitted) {
            return string(decimal());
        }
        let digits = if omitted {
            if method == "toString" { 10.0 } else { 0.0 }
        } else {
            match self.coerce_number(host, arg)? {
                Ok(n) if n.is_nan() => 0.0,
                Ok(n) => n.trunc(),
                Err(error) => return Ok(Invocation::Thrown(error)),
            }
        };
        // Exponential and precision format non-finite numbers before checking bounds.
        if !number.is_finite() && matches!(method, "toExponential" | "toPrecision") {
            return string(decimal());
        }
        let range = match method {
            "toString" => 2.0..=36.0,
            "toPrecision" => 1.0..=100.0,
            _ => 0.0..=100.0,
        };
        if !range.contains(&digits) {
            return self.throw_range_error(if method == "toString" {
                "Number radix must be between 2 and 36"
            } else {
                "Number format digits out of range"
            });
        }
        if !number.is_finite() {
            return string(decimal());
        }
        let digits = digits as usize;
        let n = if number == 0.0 { 0.0 } else { number };
        string(match method {
            "toString" if digits == 10 => decimal(),
            "toString" => integer_radix(n, digits as u32),
            "toFixed" if n.abs() >= 1e21 => exponent_string(format!("{n:e}")),
            "toFixed" => format!("{n:.digits$}"),
            "toExponential" if omitted => exponent_string(format!("{n:e}")),
            "toExponential" => exponent_string(format!("{n:.digits$e}")),
            "toPrecision" => precision_string(n, digits),
            _ => unreachable!("registered Number method"),
        })
    }
}

fn digit_prefix(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count()
}

fn parse_float(text: &str) -> f64 {
    let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
    let sign_len = text.len() - unsigned.len();
    if unsigned.starts_with("Infinity") {
        return if text.starts_with('-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
    }
    let bytes = unsigned.as_bytes();
    let mut end = digit_prefix(bytes);
    if bytes.get(end) == Some(&b'.') {
        end += 1 + digit_prefix(&bytes[end + 1..]);
    }
    if end == 0 || &bytes[..end] == b"." {
        return f64::NAN;
    }
    if matches!(bytes.get(end), Some(b'e' | b'E')) {
        let mut start = end + 1;
        if matches!(bytes.get(start), Some(b'+' | b'-')) {
            start += 1;
        }
        let len = digit_prefix(&bytes[start..]);
        if len > 0 {
            end = start + len;
        }
    }
    // Only the scanned decimal prefix goes to Rust's binary64 converter.
    text[..sign_len + end].parse().unwrap_or(f64::NAN)
}

fn parse_int(text: &str, mut radix: i32) -> f64 {
    let mut digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    if matches!(radix, 0 | 16)
        && let Some(rest) = digits
            .strip_prefix("0x")
            .or_else(|| digits.strip_prefix("0X"))
    {
        radix = 16;
        digits = rest;
    }
    if radix == 0 {
        radix = 10;
    }
    if !(2..=36).contains(&radix) {
        return f64::NAN;
    }
    let value = if radix == 10 {
        let end = digit_prefix(digits.as_bytes());
        digits[..end].parse().unwrap_or(f64::NAN)
    } else {
        let mut values = digits.chars().map_while(|ch| ch.to_digit(radix as u32));
        let Some(first) = values.next() else {
            return f64::NAN;
        };
        values.fold(f64::from(first), |acc, digit| {
            acc * f64::from(radix) + f64::from(digit)
        })
    };
    if text.starts_with('-') { -value } else { value }
}

fn exponent_string(mut text: String) -> String {
    let end = text.find('e').expect("exponential formatting") + 1;
    if !text[end..].starts_with('-') {
        text.insert(end, '+');
    }
    text
}

fn precision_string(n: f64, digits: usize) -> String {
    let formatted = format!("{n:.places$e}", places = digits - 1);
    let (_, exponent) = formatted.split_once('e').expect("exponential formatting");
    let exponent: i32 = exponent.parse().expect("decimal exponent");
    if exponent < -6 || exponent >= digits as i32 {
        exponent_string(formatted)
    } else {
        let places = (digits as i32 - 1 - exponent) as usize;
        format!("{n:.places$}")
    }
}

fn integer_radix(n: f64, radix: u32) -> String {
    let integer = n.abs().trunc();
    if integer == 0.0 {
        return "0".to_owned();
    }
    let bits = integer.to_bits();
    let exponent = ((bits >> 52) as i32 & 0x7ff) - 1023;
    let significand = (bits & ((1 << 52) - 1)) | (1 << 52);
    // Convert the exact integer's bits to little-endian radix digits. No u64
    // saturation or floating remainder is needed for numbers above 2^64.
    let mut digits = Vec::new();
    for pos in (0..=exponent).rev() {
        let index = pos - exponent + 52;
        let mut carry = if index < 0 {
            0
        } else {
            ((significand >> index) & 1) as u32
        };
        for digit in &mut digits {
            let next = *digit * 2 + carry;
            *digit = next % radix;
            carry = next / radix;
        }
        if carry != 0 {
            digits.push(carry);
        }
    }
    let mut result = String::with_capacity(digits.len() + usize::from(n < 0.0));
    if n < 0.0 {
        result.push('-');
    }
    result.extend(
        digits
            .into_iter()
            .rev()
            .map(|digit| char::from_digit(digit, radix).unwrap()),
    );
    result
}
