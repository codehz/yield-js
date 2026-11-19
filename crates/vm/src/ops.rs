//! Scalar JavaScript operations. Identity and heap layout stay out of this module.

use crate::bytecode::{BinaryOp, UnaryOp};
use crate::error::VmError;
use crate::value::Value;

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
        BinaryOp::Add => match (&lhs, &rhs) {
            (Value::String(_), _) | (_, Value::String(_)) => {
                check_no_symbol(SYMBOL_TO_STRING, &[&lhs, &rhs])?;
                Value::String(to_js_string(&lhs) + &to_js_string(&rhs))
            }
            (lhs, rhs) => {
                check_no_symbol(SYMBOL_TO_NUMBER, &[lhs, rhs])?;
                Value::Number(to_number(lhs)? + to_number(rhs)?)
            }
        },
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
        UnaryOp::TypeOf => Value::String(value.type_of().to_owned()),
        UnaryOp::Void => Value::Undefined,
    }
}

pub(crate) fn array_index(key: &str) -> Option<usize> {
    canonical_index(key).filter(|&index| index < u32::MAX as usize)
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

fn strict_equal(lhs: &Value, rhs: &Value) -> bool {
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
        Value::String(value) => parse_js_number(value),
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

fn parse_js_number(value: &str) -> Result<f64, VmError> {
    let trimmed = value.trim();
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
        if digits.is_empty() {
            return Ok(f64::NAN);
        }
        let mut number = 0.0;
        for digit in digits.chars() {
            let Some(value) = digit.to_digit(base) else {
                return Ok(f64::NAN);
            };
            number = number * base as f64 + value as f64;
        }
        return Ok(number);
    }
    Ok(trimmed.parse().unwrap_or(f64::NAN))
}

fn to_int32(value: &Value) -> Result<i32, VmError> {
    Ok(to_int32_lossy(value))
}

fn to_int32_lossy(value: &Value) -> i32 {
    let number = to_number_lossy(value);
    if !number.is_finite() || number == 0.0 {
        return 0;
    }
    let integer = number.trunc().rem_euclid(4_294_967_296.0);
    let unsigned = integer as u32;
    unsigned as i32
}

fn to_uint32(value: &Value) -> Result<u32, VmError> {
    Ok(to_int32(value)? as u32)
}

pub(crate) fn to_js_string(value: &Value) -> String {
    match value {
        Value::Undefined => "undefined".to_owned(),
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => {
            if value.is_nan() {
                "NaN".to_owned()
            } else if *value == f64::INFINITY {
                "Infinity".to_owned()
            } else if *value == f64::NEG_INFINITY {
                "-Infinity".to_owned()
            } else if *value == 0.0 {
                "0".to_owned()
            } else {
                value.to_string()
            }
        }
        Value::String(value) => value.clone(),
        Value::Symbol(_) => "Symbol()".to_owned(),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}
