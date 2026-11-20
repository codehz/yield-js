//! Number constructors, static functions, and prototype methods.

use std::fmt::Write as _;

use crate::builtin::globals::Owner;
use crate::builtin::{
    ROOT_FUNCTION_PROTOTYPE, ROOT_NUMBER_PROTOTYPE, ROOT_OBJECT_PROTOTYPE, RootKind, RootSpec,
};
use crate::error::VmError;
use crate::heap::{Attributes, BoxedPrimitive, Object};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::ops;
use crate::runtime::Invocation;
use crate::value::{ObjectId, Value};
use crate::vm::Vm;

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

#[yield_binding_macros::builtin_global(name = "Number", root = "number", kind = "constructor")]
pub(crate) mod members {
    use super::{Host, Invocation, Value, Vm, VmError};

    #[functions(isFinite, isInteger, isNaN, isSafeInteger, parseFloat, parseInt)]
    fn call_static(
        vm: &mut Vm,
        host: &mut dyn Host,
        key: &str,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        vm.call_number_static(host, key, args)
    }

    #[methods(toString, valueOf, toFixed, toExponential, toPrecision, toLocaleString)]
    fn call_method(
        vm: &mut Vm,
        host: &mut dyn Host,
        key: &str,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        vm.call_number_method(host, key, receiver, args)
    }

    #[constant(name = "EPSILON")]
    const EPSILON: f64 = f64::EPSILON;
    #[constant(name = "MAX_SAFE_INTEGER")]
    const MAX_SAFE_INTEGER: f64 = super::MAX_SAFE_INTEGER;
    #[constant(name = "MIN_SAFE_INTEGER")]
    const MIN_SAFE_INTEGER: f64 = -MAX_SAFE_INTEGER;
    #[constant(name = "MAX_VALUE")]
    const MAX_VALUE: f64 = f64::MAX;
    #[constant(name = "MIN_VALUE")]
    const MIN_VALUE: f64 = f64::from_bits(1);
    #[constant(name = "POSITIVE_INFINITY")]
    const POSITIVE_INFINITY: f64 = f64::INFINITY;
    #[constant(name = "NEGATIVE_INFINITY")]
    const NEGATIVE_INFINITY: f64 = f64::NEG_INFINITY;
    #[constant(name = "NaN")]
    const NAN: f64 = f64::NAN;
}

const CONSTRUCTOR: IntrinsicKey = IntrinsicKey::new("Number", Slot::Global, "");

pub(crate) const NUMBER_INTRINSICS: &[IntrinsicDef] = &[IntrinsicDef::constructor(
    CONSTRUCTOR,
    |vm, host, args, _| vm.call_number(host, &args),
    |vm, host, args, prototype| {
        let result = match vm.call_number(host, &args)? {
            Invocation::Value(value) => vm.construct_object(&[value])?,
            thrown => thrown,
        };
        vm.with_result_prototype(result, prototype)
    },
)];

pub(crate) const OWNER: Owner = Owner::new(
    members::ROOT,
    NUMBER_ROOTS,
    &[NUMBER_INTRINSICS, members::INTRINSICS],
    Some(|vm, _| install(vm)),
);

pub(crate) const NUMBER_ROOTS: &[RootSpec] = &[
    RootSpec::new(
        members::ROOT,
        RootKind::Native(CONSTRUCTOR),
        Some(ROOT_FUNCTION_PROTOTYPE),
    )
    .fixed(ROOT_NUMBER_PROTOTYPE)
    .backlink(None),
    RootSpec::new(
        ROOT_NUMBER_PROTOTYPE,
        RootKind::Plain,
        Some(ROOT_OBJECT_PROTOTYPE),
    ),
];

fn install(vm: &mut Vm) {
    vm.init_number_constructor(
        vm.builtins.get(members::ROOT),
        vm.builtins.get(ROOT_NUMBER_PROTOTYPE),
    );
}

impl Vm {
    fn init_number_constructor(&mut self, constructor: ObjectId, prototype: ObjectId) {
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
        members::install_members(self, constructor, prototype);
    }

    fn call_number(&mut self, host: &mut dyn Host, args: &[Value]) -> Result<Invocation, VmError> {
        Ok(match args.first() {
            None => Invocation::Value(Value::Number(0.0)),
            Some(arg) => match self.coerce_number(host, arg)? {
                Ok(number) => Invocation::Value(Value::Number(number)),
                Err(error) => Invocation::Thrown(error),
            },
        })
    }

    fn call_number_static(
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
                // A lone surrogate ends a numeric prefix; it need not invalidate digits before it.
                let prefix: String = char::decode_utf16(text.units())
                    .map_while(Result::ok)
                    .collect();
                let text = prefix.trim_start_matches(ops::is_js_whitespace);
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

    fn call_number_method(
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
            "toString" => integer_radix(n, digits as u32).into(),
            "toFixed" if n.abs() >= 1e21 => decimal(),
            "toFixed" => ryu_js::Buffer::new()
                .format_to_fixed(n, digits as u8)
                .into(),
            "toExponential" if omitted => shortest_exponential(n).into(),
            "toExponential" | "toPrecision" => {
                let exponential = method == "toExponential";
                let precision = digits + usize::from(exponential);
                let (coefficient, exponent) = rounded_decimal(n.abs(), precision);
                format_decimal(
                    coefficient,
                    exponent,
                    n < 0.0,
                    exponential || exponent < -6 || exponent >= precision as i32,
                )
                .into()
            }
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
        let radix = radix as u32;
        let end = digits
            .bytes()
            .take_while(|&byte| char::from(byte).is_digit(radix))
            .count();
        if end == 0 {
            return f64::NAN;
        }
        ops::radix_digits_to_f64(&digits[..end], radix)
    };
    if text.starts_with('-') { -value } else { value }
}

fn shortest_exponential(n: f64) -> String {
    if n == 0.0 {
        return "0e+0".to_owned();
    }
    let mut buffer = ryu_js::Buffer::new();
    let shortest = buffer.format(n.abs());
    let (mantissa, exponent) = shortest.split_once('e').map_or((shortest, 0), |(m, e)| {
        (m, e.parse::<i32>().expect("decimal exponent"))
    });
    let point = mantissa.find('.').unwrap_or(mantissa.len());
    // Count leading zeros as digits, excluding the decimal point if it was skipped.
    let significant = mantissa.trim_start_matches(['0', '.']);
    let skipped = mantissa.len() - significant.len();
    let leading_zeros = skipped - usize::from(skipped > point);
    let significant = significant.trim_end_matches(['0', '.']);
    let mut coefficient = String::with_capacity(significant.len() + 6);
    coefficient.extend(significant.chars().filter(|&ch| ch != '.'));
    format_decimal(
        coefficient,
        exponent + point as i32 - leading_zeros as i32 - 1,
        n < 0.0,
        true,
    )
}

fn rounded_decimal(n: f64, precision: usize) -> (String, i32) {
    if n == 0.0 {
        return ("0".repeat(precision), 0);
    }

    let bits = n.to_bits();
    let biased_exponent = (bits >> 52) as i32;
    let fraction = bits & ((1 << 52) - 1);
    let (mut significand, mut exponent) = if biased_exponent == 0 {
        (fraction, -1074)
    } else {
        (fraction | (1 << 52), biased_exponent - 1023 - 52)
    };

    if exponent < 0 {
        // Cancel powers of two so they don't become trailing decimal zeros.
        let shift = significand.trailing_zeros().min((-exponent) as u32);
        significand >>= shift;
        exponent += shift as i32;
    }

    // m * 2^e is an integer for e >= 0; otherwise it is m * 5^(-e) / 10^(-e).
    const BASE: u64 = 1_000_000_000;
    let mut limbs = [0; 86]; // 53 bits times 5^1074 takes at most 767 decimal digits.
    limbs[0] = significand % BASE;
    limbs[1] = significand / BASE;
    let mut len = if limbs[1] == 0 { 1 } else { 2 };
    let (factor, scale) = if exponent < 0 { (5, -exponent) } else { (2, 0) };
    for _ in 0..exponent.abs() {
        let mut carry = 0;
        for limb in &mut limbs[..len] {
            let product = *limb * factor + carry;
            *limb = product % BASE;
            carry = product / BASE;
        }
        if carry != 0 {
            limbs[len] = carry;
            len += 1;
        }
    }

    // Only render enough of the exact coefficient to include the rounding digit.
    let mut digits = String::with_capacity(precision + 9);
    write!(digits, "{}", limbs[len - 1]).expect("writing to a String");
    let mut exponent = (digits.len() + (len - 1) * 9) as i32 - scale - 1;
    for limb in limbs[..len - 1].iter().rev() {
        if digits.len() > precision {
            break;
        }
        write!(digits, "{limb:09}").expect("writing to a String");
    }
    let round_up = digits
        .as_bytes()
        .get(precision)
        .is_some_and(|&digit| digit >= b'5');
    digits.truncate(precision);
    if round_up {
        let keep = digits.trim_end_matches('9').len();
        digits.truncate(keep);
        if let Some(digit) = digits.pop() {
            digits.push(char::from(digit as u8 + 1));
        } else {
            digits.push('1');
            exponent += 1;
        }
    }
    digits.extend(std::iter::repeat_n('0', precision - digits.len()));
    (digits, exponent)
}

fn format_decimal(mut digits: String, exponent: i32, negative: bool, scientific: bool) -> String {
    if scientific {
        if digits.len() > 1 {
            digits.insert(1, '.');
        }
        write!(digits, "e{exponent:+}").expect("writing to a String");
    } else if exponent < 0 {
        // Fixed notation is only selected for exponents down to -6.
        digits.insert_str(0, &"0.00000"[..(1 - exponent) as usize]);
    } else {
        let point = exponent as usize + 1;
        if point < digits.len() {
            digits.insert(point, '.');
        }
    }
    if negative {
        digits.insert(0, '-');
    }
    digits
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
