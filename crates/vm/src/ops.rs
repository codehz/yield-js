//! Scalar JavaScript operations. Identity and heap layout stay out of this module.

use crate::bytecode::{BinaryOp, UnaryOp};
use crate::error::VmError;
use crate::value::{JsString, PropertyKeyRef, Value};

pub(crate) fn unary_coerces(operator: UnaryOp) -> bool {
    matches!(operator, UnaryOp::Neg | UnaryOp::Pos | UnaryOp::BitNot)
}

pub(crate) const SYMBOL_TO_NUMBER: &str = "Cannot convert a Symbol value to a number";
pub(crate) const SYMBOL_TO_STRING: &str = "Cannot convert a Symbol value to a string";
const SYMBOL_TO_PRIMITIVE: &str = "Cannot convert a Symbol value to a string or a number";

pub(crate) enum BinaryError {
    TypeError(&'static str),
    Vm(VmError),
}

impl From<VmError> for BinaryError {
    fn from(error: VmError) -> Self {
        Self::Vm(error)
    }
}

/// Coercing operands must be rejected before the lossy helpers run; the error
/// is a script `TypeError` so `try`/`catch` can capture it.
fn check_no_symbol(message: &'static str, values: &[&Value]) -> Result<(), BinaryError> {
    if values.iter().any(|value| matches!(value, Value::Symbol(_))) {
        return Err(BinaryError::TypeError(message));
    }
    Ok(())
}

pub(crate) fn binary_op(operator: BinaryOp, lhs: Value, rhs: Value) -> Result<Value, BinaryError> {
    Ok(match operator {
        BinaryOp::Add => {
            if matches!(&lhs, Value::String(_)) || matches!(&rhs, Value::String(_)) {
                check_no_symbol(SYMBOL_TO_STRING, &[&lhs, &rhs])?;
                let mut text = match lhs {
                    Value::String(text) => text,
                    other => to_js_string(&other),
                };
                match rhs {
                    Value::String(right) => text.push_str(&right),
                    other => text.push_str(&to_js_string(&other)),
                }
                Value::String(text)
            } else {
                check_no_symbol(SYMBOL_TO_NUMBER, &[&lhs, &rhs])?;
                Value::Number(to_number(&lhs)? + to_number(&rhs)?)
            }
        }
        BinaryOp::Sub => {
            check_no_symbol(SYMBOL_TO_NUMBER, &[&lhs, &rhs])?;
            Value::Number(to_number(&lhs)? - to_number(&rhs)?)
        }
        BinaryOp::Mul => {
            check_no_symbol(SYMBOL_TO_NUMBER, &[&lhs, &rhs])?;
            Value::Number(to_number(&lhs)? * to_number(&rhs)?)
        }
        BinaryOp::Div => {
            check_no_symbol(SYMBOL_TO_NUMBER, &[&lhs, &rhs])?;
            Value::Number(to_number(&lhs)? / to_number(&rhs)?)
        }
        BinaryOp::Rem => {
            check_no_symbol(SYMBOL_TO_NUMBER, &[&lhs, &rhs])?;
            Value::Number(to_number(&lhs)? % to_number(&rhs)?)
        }
        BinaryOp::Exp => {
            check_no_symbol(SYMBOL_TO_NUMBER, &[&lhs, &rhs])?;
            Value::Number(to_number(&lhs)?.powf(to_number(&rhs)?))
        }
        BinaryOp::Lt => relational(&lhs, &rhs, |ordering| ordering.is_lt())?,
        BinaryOp::Le => relational(&lhs, &rhs, |ordering| ordering.is_le())?,
        BinaryOp::Gt => relational(&lhs, &rhs, |ordering| ordering.is_gt())?,
        BinaryOp::Ge => relational(&lhs, &rhs, |ordering| ordering.is_ge())?,
        BinaryOp::Eq => Value::Bool(loose_equal(&lhs, &rhs)?),
        BinaryOp::Ne => Value::Bool(!loose_equal(&lhs, &rhs)?),
        BinaryOp::StrictEq => Value::Bool(strict_equal(&lhs, &rhs)),
        BinaryOp::StrictNe => Value::Bool(!strict_equal(&lhs, &rhs)),
        BinaryOp::BitAnd => {
            check_no_symbol(SYMBOL_TO_NUMBER, &[&lhs, &rhs])?;
            Value::Number((to_int32(&lhs)? & to_int32(&rhs)?) as f64)
        }
        BinaryOp::BitOr => {
            check_no_symbol(SYMBOL_TO_NUMBER, &[&lhs, &rhs])?;
            Value::Number((to_int32(&lhs)? | to_int32(&rhs)?) as f64)
        }
        BinaryOp::BitXor => {
            check_no_symbol(SYMBOL_TO_NUMBER, &[&lhs, &rhs])?;
            Value::Number((to_int32(&lhs)? ^ to_int32(&rhs)?) as f64)
        }
        BinaryOp::Shl => {
            check_no_symbol(SYMBOL_TO_NUMBER, &[&lhs, &rhs])?;
            Value::Number((to_int32(&lhs)?).wrapping_shl(to_uint32(&rhs)? & 0x1f) as f64)
        }
        BinaryOp::Shr => {
            check_no_symbol(SYMBOL_TO_NUMBER, &[&lhs, &rhs])?;
            Value::Number((to_int32(&lhs)?).wrapping_shr(to_uint32(&rhs)? & 0x1f) as f64)
        }
        BinaryOp::UShr => {
            check_no_symbol(SYMBOL_TO_NUMBER, &[&lhs, &rhs])?;
            Value::Number((to_uint32(&lhs)?).wrapping_shr(to_uint32(&rhs)? & 0x1f) as f64)
        }
    })
}

pub(crate) fn unary_op(operator: UnaryOp, value: Value) -> Value {
    match operator {
        UnaryOp::Neg => Value::Number(-to_number_lossy(&value)),
        UnaryOp::Pos => Value::Number(to_number_lossy(&value)),
        UnaryOp::Not => Value::Bool(!value.truthy()),
        UnaryOp::BitNot => Value::Number((!to_int32_lossy(&value)) as f64),
        UnaryOp::TypeOf => Value::String(value.type_of().into()),
        UnaryOp::Void => Value::Undefined,
    }
}

pub(crate) fn canonical_index(key: &str) -> Option<usize> {
    if key == "0" {
        return Some(0);
    }
    if !matches!(key.as_bytes().first(), Some(b'1'..=b'9')) {
        return None;
    }
    key.parse().ok()
}

pub(crate) fn canonical_index_key(key: PropertyKeyRef<'_>) -> Option<usize> {
    let text = match key {
        PropertyKeyRef::String(text) => return canonical_index(text),
        PropertyKeyRef::JsString(text) => text,
        PropertyKeyRef::Symbol(_) => return None,
    };
    let mut units = text.units();
    match units.next()? {
        48 if units.len() == 0 => Some(0),
        first @ 49..=57 => units.try_fold(usize::from(first - 48), |n, unit| {
            (48..=57).contains(&unit).then_some(())?;
            n.checked_mul(10)?.checked_add(usize::from(unit - 48))
        }),
        _ => None,
    }
}

fn relational(
    lhs: &Value,
    rhs: &Value,
    predicate: impl Fn(std::cmp::Ordering) -> bool,
) -> Result<Value, BinaryError> {
    check_no_symbol(SYMBOL_TO_PRIMITIVE, &[lhs, rhs])?;
    if let (Value::String(lhs), Value::String(rhs)) = (lhs, rhs) {
        return Ok(Value::Bool(predicate(lhs.cmp(rhs))));
    }
    let lhs = to_number_lossy(lhs);
    let rhs = to_number_lossy(rhs);
    Ok(Value::Bool(lhs.partial_cmp(&rhs).is_some_and(predicate)))
}

pub(crate) fn strict_equal(lhs: &Value, rhs: &Value) -> bool {
    match (lhs, rhs) {
        (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
        (Value::Bool(lhs), Value::Bool(rhs)) => lhs == rhs,
        (Value::Number(lhs), Value::Number(rhs)) => lhs == rhs,
        (Value::String(lhs), Value::String(rhs)) => lhs == rhs,
        (Value::Symbol(lhs), Value::Symbol(rhs)) => lhs == rhs,
        (Value::Object(lhs), Value::Object(rhs)) => lhs == rhs,
        _ => false,
    }
}

pub(crate) fn same_value(lhs: &Value, rhs: &Value) -> bool {
    match (lhs, rhs) {
        (Value::Number(lhs), Value::Number(rhs)) => {
            lhs.to_bits() == rhs.to_bits() || lhs.is_nan() && rhs.is_nan()
        }
        _ => strict_equal(lhs, rhs),
    }
}

fn loose_equal(lhs: &Value, rhs: &Value) -> Result<bool, VmError> {
    if strict_equal(lhs, rhs) {
        return Ok(true);
    }
    match (lhs, rhs) {
        (Value::Null, Value::Undefined) | (Value::Undefined, Value::Null) => Ok(true),
        (Value::Number(_), Value::String(_)) => Ok(to_number(lhs)? == to_number(rhs)?),
        (Value::String(_), Value::Number(_)) => Ok(to_number(lhs)? == to_number(rhs)?),
        (Value::Bool(_), _) => loose_equal(&Value::Number(to_number(lhs)?), rhs),
        (_, Value::Bool(_)) => loose_equal(lhs, &Value::Number(to_number(rhs)?)),
        _ => Ok(false),
    }
}

pub(crate) fn to_number(value: &Value) -> Result<f64, VmError> {
    match value {
        Value::Undefined => Ok(f64::NAN),
        Value::Null => Ok(0.0),
        Value::Bool(value) => Ok(if *value { 1.0 } else { 0.0 }),
        Value::Number(value) => Ok(*value),
        Value::String(value) => match value.to_utf8() {
            Ok(text) => parse_js_number(&text),
            Err(_) => Ok(f64::NAN),
        },
        Value::Symbol(_) => Err(VmError::InvalidState(
            "numeric conversion of a Symbol is not implemented in this PoC".to_owned(),
        )),
        Value::Object(_) => Err(VmError::InvalidState(
            "numeric conversion of objects is not implemented in this PoC".to_owned(),
        )),
    }
}

fn to_number_lossy(value: &Value) -> f64 {
    to_number(value).unwrap_or(f64::NAN)
}

pub(crate) fn is_js_whitespace(ch: char) -> bool {
    matches!(ch, '\u{0009}'..='\u{000d}' | ' ' | '\u{00a0}' | '\u{1680}'
        | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}'
        | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

fn parse_js_number(value: &str) -> Result<f64, VmError> {
    let trimmed = value.trim_matches(is_js_whitespace);
    if trimmed.is_empty() {
        return Ok(0.0);
    }
    if trimmed == "Infinity" || trimmed == "+Infinity" {
        return Ok(f64::INFINITY);
    }
    if trimmed == "-Infinity" {
        return Ok(f64::NEG_INFINITY);
    }
    let radix = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .map(|digits| (16, digits))
        .or_else(|| {
            trimmed
                .strip_prefix("0b")
                .or_else(|| trimmed.strip_prefix("0B"))
                .map(|digits| (2, digits))
        })
        .or_else(|| {
            trimmed
                .strip_prefix("0o")
                .or_else(|| trimmed.strip_prefix("0O"))
                .map(|digits| (8, digits))
        });
    if let Some((base, digits)) = radix {
        if digits.is_empty() || !digits.chars().all(|digit| digit.is_digit(base)) {
            return Ok(f64::NAN);
        }
        return Ok(radix_digits_to_f64(digits, base));
    }
    // Rust's parser also accepts aliases such as `inf`; JS does not.
    if !trimmed
        .bytes()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'.' | b'+' | b'-' | b'e' | b'E'))
    {
        return Ok(f64::NAN);
    }
    Ok(trimmed.parse().unwrap_or(f64::NAN))
}

/// Convert a validated, unsigned digit sequence with one binary64 rounding.
pub(crate) fn radix_digits_to_f64(digits: &str, radix: u32) -> f64 {
    let radix = u64::from(radix);
    let mut digits = digits
        .chars()
        .map(|digit| u64::from(digit.to_digit(radix as u32).unwrap()));
    let mut small = 0u64;
    let first_wide = loop {
        let Some(digit) = digits.next() else {
            return small as f64;
        };
        match small.checked_mul(radix).and_then(|n| n.checked_add(digit)) {
            Some(next) => small = next,
            None => break digit,
        }
    };

    // Integers above 1024 bits cannot round to a finite binary64.
    let mut limbs = [0u32; 32];
    limbs[0] = small as u32;
    limbs[1] = (small >> 32) as u32;
    let mut len = 2;
    for digit in std::iter::once(first_wide).chain(digits) {
        let mut carry = digit;
        for limb in &mut limbs[..len] {
            let next = u64::from(*limb) * radix + carry;
            *limb = next as u32;
            carry = next >> 32;
        }
        if carry != 0 {
            if len == limbs.len() {
                return f64::INFINITY;
            }
            limbs[len] = carry as u32;
            len += 1;
        }
    }

    let bit_len = len * 32 - limbs[len - 1].leading_zeros() as usize;
    let bit = |pos: usize| u64::from((limbs[pos / 32] >> (pos % 32)) & 1);
    let shift = bit_len - 53;
    let mut significand = 0;
    for pos in (shift..bit_len).rev() {
        significand = (significand << 1) | bit(pos);
    }
    let guard = shift - 1;
    let sticky = limbs[..guard / 32].iter().any(|&limb| limb != 0)
        || limbs[guard / 32] & ((1 << (guard % 32)) - 1) != 0;
    if bit(guard) != 0 && (sticky || significand & 1 != 0) {
        significand += 1;
    }
    // The implicit leading bit adds one to the biased exponent; a carry
    // out of the significand can therefore produce infinity at the limit.
    f64::from_bits((((bit_len as u64 - 1) + 1022) << 52) + significand)
}

fn to_int32(value: &Value) -> Result<i32, VmError> {
    Ok(to_int32_lossy(value))
}

fn to_int32_lossy(value: &Value) -> i32 {
    number_to_uint32(to_number_lossy(value)) as i32
}

pub(crate) fn number_to_uint32(number: f64) -> u32 {
    if !number.is_finite() || number == 0.0 {
        return 0;
    }
    number.trunc().rem_euclid(4_294_967_296.0) as u32
}

fn to_uint32(value: &Value) -> Result<u32, VmError> {
    Ok(to_int32(value)? as u32)
}

pub(crate) fn to_js_string(value: &Value) -> JsString {
    match value {
        Value::Undefined => "undefined".into(),
        Value::Null => "null".into(),
        Value::Bool(value) => if *value { "true" } else { "false" }.into(),
        Value::Number(value) => ryu_js::Buffer::new().format(*value).into(),
        Value::String(value) => value.clone(),
        Value::Symbol(_) => "Symbol()".into(),
        Value::Object(_) => "[object Object]".into(),
    }
}
