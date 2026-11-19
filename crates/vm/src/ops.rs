//! Scalar JavaScript operations. Identity and heap layout stay out of this module.

use crate::bytecode::{BinaryOp, UnaryOp};
use crate::error::VmError;
use crate::value::{PropertyKey, Value};

pub(crate) fn unary_coerces(operator: UnaryOp) -> bool {
    matches!(operator, UnaryOp::Neg | UnaryOp::Pos | UnaryOp::BitNot)
}

pub(crate) fn symbol_conversion_error(what: &str) -> VmError {
    VmError::Thrown(crate::builtins::type_error(&format!(
        "Cannot convert a Symbol value to {what}"
    )))
}

/// Coercing operands must be rejected before the lossy helpers run; the error
/// is a script `TypeError` so `try`/`catch` can capture it.
fn check_no_symbol(operator_kind: &str, values: &[&Value]) -> Result<(), VmError> {
    if values.iter().any(|value| matches!(value, Value::Symbol(_))) {
        return Err(symbol_conversion_error(operator_kind));
    }
    Ok(())
}

pub(crate) fn binary_op(operator: BinaryOp, lhs: Value, rhs: Value) -> Result<Value, VmError> {
    Ok(match operator {
        BinaryOp::Add => match (&lhs, &rhs) {
            (Value::String(_), _) | (_, Value::String(_)) => {
                if matches!(lhs, Value::Symbol(_)) || matches!(rhs, Value::Symbol(_)) {
                    return Err(symbol_conversion_error("a string"));
                }
                Value::String(to_js_string(&lhs) + &to_js_string(&rhs))
            }
            (lhs, rhs) => {
                check_no_symbol("a number", &[lhs, rhs])?;
                Value::Number(to_number(lhs)? + to_number(rhs)?)
            }
        },
        BinaryOp::Sub => {
            check_no_symbol("a number", &[&lhs, &rhs])?;
            Value::Number(to_number(&lhs)? - to_number(&rhs)?)
        }
        BinaryOp::Mul => {
            check_no_symbol("a number", &[&lhs, &rhs])?;
            Value::Number(to_number(&lhs)? * to_number(&rhs)?)
        }
        BinaryOp::Div => {
            check_no_symbol("a number", &[&lhs, &rhs])?;
            Value::Number(to_number(&lhs)? / to_number(&rhs)?)
        }
        BinaryOp::Rem => {
            check_no_symbol("a number", &[&lhs, &rhs])?;
            Value::Number(to_number(&lhs)? % to_number(&rhs)?)
        }
        BinaryOp::Exp => {
            check_no_symbol("a number", &[&lhs, &rhs])?;
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
            check_no_symbol("a number", &[&lhs, &rhs])?;
            Value::Number((to_int32(&lhs)? & to_int32(&rhs)?) as f64)
        }
        BinaryOp::BitOr => {
            check_no_symbol("a number", &[&lhs, &rhs])?;
            Value::Number((to_int32(&lhs)? | to_int32(&rhs)?) as f64)
        }
        BinaryOp::BitXor => {
            check_no_symbol("a number", &[&lhs, &rhs])?;
            Value::Number((to_int32(&lhs)? ^ to_int32(&rhs)?) as f64)
        }
        BinaryOp::Shl => {
            check_no_symbol("a number", &[&lhs, &rhs])?;
            Value::Number((to_int32(&lhs)?).wrapping_shl(to_uint32(&rhs)? & 0x1f) as f64)
        }
        BinaryOp::Shr => {
            check_no_symbol("a number", &[&lhs, &rhs])?;
            Value::Number((to_int32(&lhs)?).wrapping_shr(to_uint32(&rhs)? & 0x1f) as f64)
        }
        BinaryOp::UShr => {
            check_no_symbol("a number", &[&lhs, &rhs])?;
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

pub(crate) fn property_key(value: &Value) -> Result<PropertyKey, VmError> {
    match value {
        Value::Symbol(id) => Ok(PropertyKey::Symbol(*id)),
        _ => Ok(PropertyKey::String(to_js_string(value))),
    }
}

pub(crate) fn array_index(key: &str) -> Option<usize> {
    if key == "0" {
        return Some(0);
    }
    if key.starts_with('0') {
        return None;
    }
    key.parse().ok()
}

fn relational(
    lhs: &Value,
    rhs: &Value,
    predicate: impl Fn(std::cmp::Ordering) -> bool,
) -> Result<Value, VmError> {
    check_no_symbol("a string or a number", &[lhs, rhs])?;
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

fn to_number(value: &Value) -> Result<f64, VmError> {
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
    trimmed.parse().map_err(|_| {
        VmError::InvalidState(format!("cannot convert {value:?} to number in this PoC"))
    })
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
            } else {
                value.to_string()
            }
        }
        Value::String(value) => value.clone(),
        Value::Symbol(_) => "Symbol()".to_owned(),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}
