//! Scalar JavaScript operations. Identity and heap layout stay out of this module.

use std::cmp::Ordering;

use num_bigint::BigInt;
use num_traits::{FromPrimitive, Pow, ToPrimitive, Zero};

use crate::bytecode::{BinaryOp, UnaryOp};
use crate::error::VmError;
pub(crate) use crate::value::is_js_whitespace;
use crate::value::{JsBigInt, JsString, PropertyKeyRef, Value, ValueRef};

pub(crate) fn unary_coerces(operator: UnaryOp) -> bool {
    matches!(
        operator,
        UnaryOp::Neg | UnaryOp::Pos | UnaryOp::BitNot | UnaryOp::ToNumeric
    )
}

pub(crate) const SYMBOL_TO_NUMBER: &str = "Cannot convert a Symbol value to a number";
pub(crate) const SYMBOL_TO_STRING: &str = "Cannot convert a Symbol value to a string";
pub(crate) const BIGINT_TO_NUMBER: &str = "Cannot convert a BigInt value to a number";
const SYMBOL_TO_PRIMITIVE: &str = "Cannot convert a Symbol value to a string or a number";
const MIXED_BIGINT: &str = "Cannot mix BigInt and other types, use explicit conversions";
const BIGINT_UNSIGNED_SHIFT: &str = "BigInts have no unsigned right shift, use >> instead";
const BIGINT_DIVISION_BY_ZERO: &str = "Division by zero";
const BIGINT_NEGATIVE_EXPONENT: &str = "Exponent must be non-negative";

pub(crate) enum BinaryError {
    TypeError(&'static str),
    RangeError(&'static str),
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
    if values
        .iter()
        .any(|value| matches!(value.view(), ValueRef::Symbol(_)))
    {
        return Err(BinaryError::TypeError(message));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum NumericRef<'a> {
    Number(f64),
    BigInt(&'a BigInt),
}

fn numeric_ref(value: &Value) -> Result<NumericRef<'_>, BinaryError> {
    Ok(match value.view() {
        ValueRef::BigInt(value) => NumericRef::BigInt(value.as_bigint()),
        ValueRef::Symbol(_) => return Err(BinaryError::TypeError(SYMBOL_TO_NUMBER)),
        ValueRef::Object(_) => {
            return Err(BinaryError::Vm(VmError::InvalidState(
                "numeric conversion requires a primitive value".to_owned(),
            )));
        }
        _ => NumericRef::Number(to_number(value)?),
    })
}

fn bigint_value(value: BigInt) -> Value {
    Value::bigint(JsBigInt::from_bigint(value))
}

fn bigint_shift(value: &BigInt, count: &BigInt, left: bool) -> Result<Value, BinaryError> {
    let negative = count < &BigInt::zero();
    let magnitude = if negative { -count } else { count.clone() };
    let shift = magnitude.to_usize().ok_or_else(|| {
        BinaryError::Vm(VmError::InvalidState(
            "BigInt shift count is too large".to_owned(),
        ))
    })?;
    let left = left != negative;
    Ok(bigint_value(if left {
        value << shift
    } else {
        value >> shift
    }))
}

fn arithmetic(operator: BinaryOp, lhs: Value, rhs: Value) -> Result<Value, BinaryError> {
    let left = numeric_ref(&lhs)?;
    let right = numeric_ref(&rhs)?;
    match (left, right) {
        (NumericRef::Number(left), NumericRef::Number(right)) => {
            Ok(Value::number(match operator {
                BinaryOp::Add => left + right,
                BinaryOp::Sub => left - right,
                BinaryOp::Mul => left * right,
                BinaryOp::Div => left / right,
                BinaryOp::Rem => left % right,
                BinaryOp::Exp => left.powf(right),
                _ => unreachable!("arithmetic operator"),
            }))
        }
        (NumericRef::BigInt(left), NumericRef::BigInt(right)) => Ok(bigint_value(match operator {
            BinaryOp::Add => left + right,
            BinaryOp::Sub => left - right,
            BinaryOp::Mul => left * right,
            BinaryOp::Div => {
                if right.is_zero() {
                    return Err(BinaryError::RangeError(BIGINT_DIVISION_BY_ZERO));
                }
                left / right
            }
            BinaryOp::Rem => {
                if right.is_zero() {
                    return Err(BinaryError::RangeError(BIGINT_DIVISION_BY_ZERO));
                }
                left % right
            }
            BinaryOp::Exp => {
                if right < &BigInt::zero() {
                    return Err(BinaryError::RangeError(BIGINT_NEGATIVE_EXPONENT));
                }
                let exponent = right.to_u64().ok_or_else(|| {
                    BinaryError::Vm(VmError::InvalidState(
                        "BigInt exponent is too large".to_owned(),
                    ))
                })?;
                Pow::pow(left, exponent)
            }
            _ => unreachable!("arithmetic operator"),
        })),
        _ => Err(BinaryError::TypeError(MIXED_BIGINT)),
    }
}

fn bitwise(operator: BinaryOp, lhs: Value, rhs: Value) -> Result<Value, BinaryError> {
    let left = numeric_ref(&lhs)?;
    let right = numeric_ref(&rhs)?;
    match (left, right) {
        (NumericRef::Number(left), NumericRef::Number(right)) => {
            let left = number_to_uint32(left) as i32;
            let right = number_to_uint32(right);
            Ok(Value::number(match operator {
                BinaryOp::BitAnd => (left & right as i32) as f64,
                BinaryOp::BitOr => (left | right as i32) as f64,
                BinaryOp::BitXor => (left ^ right as i32) as f64,
                BinaryOp::Shl => left.wrapping_shl(right & 0x1f) as f64,
                BinaryOp::Shr => left.wrapping_shr(right & 0x1f) as f64,
                _ => unreachable!("bitwise operator"),
            }))
        }
        (NumericRef::BigInt(left), NumericRef::BigInt(right)) => match operator {
            BinaryOp::BitAnd => Ok(bigint_value(left & right)),
            BinaryOp::BitOr => Ok(bigint_value(left | right)),
            BinaryOp::BitXor => Ok(bigint_value(left ^ right)),
            BinaryOp::Shl => bigint_shift(left, right, true),
            BinaryOp::Shr => bigint_shift(left, right, false),
            _ => unreachable!("bitwise operator"),
        },
        _ => Err(BinaryError::TypeError(MIXED_BIGINT)),
    }
}

pub(crate) fn binary_op(operator: BinaryOp, lhs: Value, rhs: Value) -> Result<Value, BinaryError> {
    Ok(match operator {
        BinaryOp::Add => {
            if matches!(lhs.view(), ValueRef::String(_))
                || matches!(rhs.view(), ValueRef::String(_))
            {
                check_no_symbol(SYMBOL_TO_STRING, &[&lhs, &rhs])?;
                let mut text = if matches!(lhs.view(), ValueRef::String(_)) {
                    lhs.into_string().expect("string operand")
                } else {
                    to_js_string(&lhs)
                };
                match rhs.view() {
                    ValueRef::String(right) => text.push_str(right),
                    _ => text.push_str(&to_js_string(&rhs)),
                }
                Value::string(text)
            } else {
                arithmetic(operator, lhs, rhs)?
            }
        }
        BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Rem | BinaryOp::Exp => {
            arithmetic(operator, lhs, rhs)?
        }
        BinaryOp::Lt => relational(&lhs, &rhs, |ordering| ordering.is_lt())?,
        BinaryOp::Le => relational(&lhs, &rhs, |ordering| ordering.is_le())?,
        BinaryOp::Gt => relational(&lhs, &rhs, |ordering| ordering.is_gt())?,
        BinaryOp::Ge => relational(&lhs, &rhs, |ordering| ordering.is_ge())?,
        BinaryOp::Eq => Value::boolean(loose_equal(&lhs, &rhs)?),
        BinaryOp::Ne => Value::boolean(!loose_equal(&lhs, &rhs)?),
        BinaryOp::StrictEq => Value::boolean(strict_equal(&lhs, &rhs)),
        BinaryOp::StrictNe => Value::boolean(!strict_equal(&lhs, &rhs)),
        BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor | BinaryOp::Shl | BinaryOp::Shr => {
            bitwise(operator, lhs, rhs)?
        }
        BinaryOp::UShr => {
            if matches!(lhs.view(), ValueRef::BigInt(_))
                || matches!(rhs.view(), ValueRef::BigInt(_))
            {
                return Err(BinaryError::TypeError(BIGINT_UNSIGNED_SHIFT));
            }
            check_no_symbol(SYMBOL_TO_NUMBER, &[&lhs, &rhs])?;
            Value::number((to_uint32(&lhs)?).wrapping_shr(to_uint32(&rhs)? & 0x1f) as f64)
        }
    })
}

pub(crate) fn unary_op(operator: UnaryOp, value: Value) -> Result<Value, BinaryError> {
    Ok(match operator {
        UnaryOp::Neg => match value.view() {
            ValueRef::BigInt(value) => bigint_value(-value.as_bigint()),
            _ => Value::number(-to_number_lossy(&value)),
        },
        UnaryOp::Pos => match value.view() {
            ValueRef::BigInt(_) => return Err(BinaryError::TypeError(BIGINT_TO_NUMBER)),
            _ => Value::number(to_number_lossy(&value)),
        },
        UnaryOp::Not => Value::boolean(!value.truthy()),
        UnaryOp::BitNot => match value.view() {
            ValueRef::BigInt(value) => bigint_value(!value.as_bigint()),
            _ => Value::number((!to_int32_lossy(&value)) as f64),
        },
        UnaryOp::TypeOf => Value::string(value.type_of().into()),
        UnaryOp::Void => Value::UNDEFINED,
        UnaryOp::ToNumeric => match value.view() {
            ValueRef::BigInt(_) => value,
            ValueRef::Symbol(_) => return Err(BinaryError::TypeError(SYMBOL_TO_NUMBER)),
            _ => Value::number(to_number_lossy(&value)),
        },
        UnaryOp::Increment | UnaryOp::Decrement => {
            let delta = if matches!(operator, UnaryOp::Increment) {
                1.0
            } else {
                -1.0
            };
            match value.view() {
                ValueRef::Number(value) => Value::number(value + delta),
                ValueRef::BigInt(value) => bigint_value(if delta > 0.0 {
                    value.as_bigint() + 1
                } else {
                    value.as_bigint() - 1
                }),
                _ => return Err(BinaryError::TypeError(BIGINT_TO_NUMBER)),
            }
        }
    })
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

fn compare_bigint_f64(big: &BigInt, number: f64) -> Option<Ordering> {
    if number.is_nan() {
        return None;
    }
    if number == f64::INFINITY {
        return Some(Ordering::Less);
    }
    if number == f64::NEG_INFINITY {
        return Some(Ordering::Greater);
    }
    let integer = BigInt::from_f64(number.trunc())?;
    let ordering = big.cmp(&integer);
    if ordering != Ordering::Equal {
        return Some(ordering);
    }
    match number.fract().partial_cmp(&0.0)? {
        Ordering::Greater => Some(Ordering::Less),
        Ordering::Less => Some(Ordering::Greater),
        Ordering::Equal => Some(Ordering::Equal),
    }
}

fn bigint_equals_f64(big: &BigInt, number: f64) -> bool {
    !number.is_nan()
        && number.is_finite()
        && number.fract() == 0.0
        && BigInt::from_f64(number).is_some_and(|integer| &integer == big)
}

fn relational(
    lhs: &Value,
    rhs: &Value,
    predicate: impl Fn(Ordering) -> bool,
) -> Result<Value, BinaryError> {
    check_no_symbol(SYMBOL_TO_PRIMITIVE, &[lhs, rhs])?;
    if let (ValueRef::String(lhs), ValueRef::String(rhs)) = (lhs.view(), rhs.view()) {
        return Ok(Value::boolean(predicate(lhs.cmp(rhs))));
    }
    let ordering = match (numeric_ref(lhs)?, numeric_ref(rhs)?) {
        (NumericRef::Number(lhs), NumericRef::Number(rhs)) => lhs.partial_cmp(&rhs),
        (NumericRef::BigInt(lhs), NumericRef::BigInt(rhs)) => Some(lhs.cmp(rhs)),
        (NumericRef::BigInt(lhs), NumericRef::Number(rhs)) => compare_bigint_f64(lhs, rhs),
        (NumericRef::Number(lhs), NumericRef::BigInt(rhs)) => {
            compare_bigint_f64(rhs, lhs).map(Ordering::reverse)
        }
    };
    Ok(Value::boolean(ordering.is_some_and(predicate)))
}

pub(crate) fn strict_equal(lhs: &Value, rhs: &Value) -> bool {
    match (lhs.view(), rhs.view()) {
        (ValueRef::Undefined, ValueRef::Undefined) | (ValueRef::Null, ValueRef::Null) => true,
        (ValueRef::Bool(lhs), ValueRef::Bool(rhs)) => lhs == rhs,
        (ValueRef::Number(lhs), ValueRef::Number(rhs)) => lhs == rhs,
        (ValueRef::String(lhs), ValueRef::String(rhs)) => lhs == rhs,
        (ValueRef::BigInt(lhs), ValueRef::BigInt(rhs)) => lhs == rhs,
        (ValueRef::Symbol(lhs), ValueRef::Symbol(rhs)) => lhs == rhs,
        (ValueRef::Object(lhs), ValueRef::Object(rhs)) => lhs == rhs,
        _ => false,
    }
}

pub(crate) fn same_value(lhs: &Value, rhs: &Value) -> bool {
    match (lhs.view(), rhs.view()) {
        (ValueRef::Number(lhs), ValueRef::Number(rhs)) => {
            lhs.to_bits() == rhs.to_bits() || lhs.is_nan() && rhs.is_nan()
        }
        _ => strict_equal(lhs, rhs),
    }
}

fn bigint_from_string(value: &JsString) -> Option<JsBigInt> {
    value
        .to_utf8()
        .ok()
        .and_then(|text| JsBigInt::from_js_string(&text))
}

fn loose_equal(lhs: &Value, rhs: &Value) -> Result<bool, VmError> {
    if strict_equal(lhs, rhs) {
        return Ok(true);
    }
    match (lhs.view(), rhs.view()) {
        (ValueRef::Null, ValueRef::Undefined) | (ValueRef::Undefined, ValueRef::Null) => Ok(true),
        (ValueRef::BigInt(lhs), ValueRef::Number(rhs))
        | (ValueRef::Number(rhs), ValueRef::BigInt(lhs)) => {
            Ok(bigint_equals_f64(lhs.as_bigint(), rhs))
        }
        (ValueRef::BigInt(lhs), ValueRef::String(rhs)) => {
            Ok(bigint_from_string(rhs).is_some_and(|rhs| lhs.as_bigint() == rhs.as_bigint()))
        }
        (ValueRef::BigInt(lhs), ValueRef::Bool(rhs))
        | (ValueRef::Bool(rhs), ValueRef::BigInt(lhs)) => Ok(bigint_equals_f64(
            lhs.as_bigint(),
            if rhs { 1.0 } else { 0.0 },
        )),
        (ValueRef::Number(_), ValueRef::String(_)) => Ok(to_number(lhs)? == to_number(rhs)?),
        (ValueRef::String(_), ValueRef::Number(_)) => Ok(to_number(lhs)? == to_number(rhs)?),
        (ValueRef::Bool(_), _) => loose_equal(&Value::number(to_number(lhs)?), rhs),
        (_, ValueRef::Bool(_)) => loose_equal(lhs, &Value::number(to_number(rhs)?)),
        _ => Ok(false),
    }
}

pub(crate) fn to_number(value: &Value) -> Result<f64, VmError> {
    match value.view() {
        ValueRef::Undefined => Ok(f64::NAN),
        ValueRef::Null => Ok(0.0),
        ValueRef::Bool(value) => Ok(if value { 1.0 } else { 0.0 }),
        ValueRef::Number(value) => Ok(value),
        ValueRef::BigInt(_) => Err(VmError::InvalidState(BIGINT_TO_NUMBER.to_owned())),
        ValueRef::String(value) => match value.to_utf8() {
            Ok(text) => parse_js_number(&text),
            Err(_) => Ok(f64::NAN),
        },
        ValueRef::Symbol(_) => Err(VmError::InvalidState(
            "numeric conversion of a Symbol is not implemented in this PoC".to_owned(),
        )),
        ValueRef::Object(_) => Err(VmError::InvalidState(
            "numeric conversion of objects is not implemented in this PoC".to_owned(),
        )),
    }
}

fn to_number_lossy(value: &Value) -> f64 {
    to_number(value).unwrap_or(f64::NAN)
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
    match value.view() {
        ValueRef::Undefined => "undefined".into(),
        ValueRef::Null => "null".into(),
        ValueRef::Bool(value) => if value { "true" } else { "false" }.into(),
        ValueRef::Number(value) => ryu_js::Buffer::new().format(value).into(),
        ValueRef::String(value) => value.clone(),
        ValueRef::BigInt(value) => value.to_decimal().into(),
        ValueRef::Symbol(_) => "Symbol()".into(),
        ValueRef::Object(_) => "[object Object]".into(),
    }
}
