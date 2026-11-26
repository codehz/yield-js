//! Math uses the VM's numeric coercion and owns no script-visible constructor.

use crate::builtin::binary::element::{f16_to_f64, f64_to_f16_bits};
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::ITERATOR_SYMBOL;
use crate::host::{Host, PlatformServices};
use crate::ops::number_to_uint32;
use crate::runtime::Invocation;
use crate::value::{PropertyKeyRef, Value, ValueRef};
use crate::vm::VmCore;

pub(crate) const RANDOM_SEED: u64 = 0x9e37_79b9_7f4a_7c15;

#[yield_binding_macros::builtin_global(name = "Math", root = "math")]
pub(crate) mod namespace {
    use std::f64::consts;

    use super::{
        ActiveRoots, Host, Invocation, PlatformServices, Value, ValueRef, VmCore, VmError,
    };

    #[constant(name = "E")]
    const E: f64 = consts::E;
    #[constant(name = "LN10")]
    const LN10: f64 = consts::LN_10;
    #[constant(name = "LN2")]
    const LN2: f64 = consts::LN_2;
    #[constant(name = "LOG10E")]
    const LOG10E: f64 = consts::LOG10_E;
    #[constant(name = "LOG2E")]
    const LOG2E: f64 = consts::LOG2_E;
    #[constant(name = "PI")]
    const PI: f64 = consts::PI;
    #[constant(name = "SQRT1_2")]
    const SQRT1_2: f64 = consts::FRAC_1_SQRT_2;
    #[constant(name = "SQRT2")]
    const SQRT2: f64 = consts::SQRT_2;

    #[function(name = "abs", length = 1)]
    fn abs<P: PlatformServices>(
        vm: &mut VmCore,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        if let Some(value) = args.first()
            && let ValueRef::Number(number) = value.view()
        {
            return Ok(Invocation::Value(Value::number(number.abs())));
        }
        match vm.coerce_number(host, roots, args.first().unwrap_or(&Value::UNDEFINED))? {
            Ok(number) => Ok(Invocation::Value(Value::number(number.abs()))),
            Err(error) => Ok(Invocation::Thrown(error)),
        }
    }

    #[functions(
        acos: 1, acosh: 1, asin: 1, asinh: 1, atan: 1, atan2: 2, atanh: 1, cbrt: 1,
        ceil: 1, clz32: 1, cos: 1, cosh: 1, exp: 1, expm1: 1, f16round: 1, floor: 1,
        fround: 1, hypot: 2, imul: 2, log: 1, log1p: 1, log10: 1, log2: 1, max: 2,
        min: 2, pow: 2, random: 0, round: 1, sign: 1, sin: 1, sinh: 1, sqrt: 1,
        sumPrecise: 1, tan: 1, tanh: 1, trunc: 1
    )]
    fn dispatch<P: PlatformServices>(
        vm: &mut VmCore,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        member: &'static str,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        vm.call_math_method(host, roots, member, args)
    }
}

impl VmCore {
    fn call_math_method<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        method: &str,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let result = match method {
            "sumPrecise" => {
                return self.math_sum_precise(
                    host,
                    roots,
                    args.first().unwrap_or(&Value::UNDEFINED),
                );
            }
            "random" => self.next_random(),
            "max" | "min" | "hypot" => {
                let (mut result, combine): (f64, fn(f64, f64) -> f64) = match method {
                    "max" => (f64::NEG_INFINITY, max),
                    "min" => (f64::INFINITY, |x, y| -max(-x, -y)),
                    _ => (0.0, f64::hypot),
                };
                let mut saw_nan = false;
                let mut saw_infinity = false;
                // Even NaN and infinity cannot short-circuit later coercions.
                for arg in args {
                    let number = if let ValueRef::Number(number) = arg.view() {
                        number
                    } else {
                        match self.coerce_number(host, roots, arg)? {
                            Ok(number) => number,
                            Err(error) => return Ok(Invocation::Thrown(error)),
                        }
                    };
                    if method == "hypot" && !number.is_finite() {
                        saw_nan |= number.is_nan();
                        saw_infinity |= number.is_infinite();
                    } else {
                        result = combine(result, number);
                    }
                }
                if saw_infinity {
                    f64::INFINITY
                } else if saw_nan {
                    f64::NAN
                } else {
                    result
                }
            }
            _ => {
                let x = if let Some(ValueRef::Number(number)) = args.first().map(Value::view) {
                    number
                } else {
                    match self.coerce_number(
                        host,
                        roots,
                        args.first().unwrap_or(&Value::UNDEFINED),
                    )? {
                        Ok(number) => number,
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    }
                };
                match method {
                    "atan2" | "imul" | "pow" => {
                        let y = if let Some(ValueRef::Number(number)) = args.get(1).map(Value::view)
                        {
                            number
                        } else {
                            match self.coerce_number(
                                host,
                                roots,
                                args.get(1).unwrap_or(&Value::UNDEFINED),
                            )? {
                                Ok(number) => number,
                                Err(error) => return Ok(Invocation::Thrown(error)),
                            }
                        };
                        match method {
                            "atan2" => x.atan2(y),
                            "imul" => (number_to_uint32(x).wrapping_mul(number_to_uint32(y))
                                as i32)
                                .into(),
                            // libm has different special cases for 1^NaN and (±1)^±Infinity.
                            _ if y.is_nan() || (x.abs() == 1.0 && y.is_infinite()) => f64::NAN,
                            _ => x.powf(y),
                        }
                    }
                    "acos" => x.acos(),
                    "acosh" => x.acosh(),
                    "asin" => x.asin(),
                    "asinh" => x.asinh(),
                    "atan" => x.atan(),
                    "atanh" => x.atanh(),
                    "cbrt" => x.cbrt(),
                    "ceil" => x.ceil(),
                    "clz32" => number_to_uint32(x).leading_zeros().into(),
                    "cos" => x.cos(),
                    "cosh" => x.cosh(),
                    "exp" => x.exp(),
                    "expm1" => x.exp_m1(),
                    "f16round" => f16_to_f64(f64_to_f16_bits(x)),
                    "floor" => x.floor(),
                    "fround" => (x as f32).into(),
                    "log" => x.ln(),
                    "log1p" => x.ln_1p(),
                    "log10" => x.log10(),
                    "log2" => x.log2(),
                    "round" => round(x),
                    "sign" if x == 0.0 => x,
                    "sign" => x.signum(),
                    "sin" => x.sin(),
                    "sinh" => x.sinh(),
                    "sqrt" => x.sqrt(),
                    "tan" => x.tan(),
                    "tanh" => x.tanh(),
                    "trunc" => x.trunc(),
                    _ => {
                        return Err(VmError::InvalidState(format!(
                            "unknown Math method {method}"
                        )));
                    }
                }
            }
        };
        Ok(Invocation::Value(Value::number(result)))
    }

    fn math_sum_precise<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        items: &Value,
    ) -> Result<Invocation, VmError> {
        let pair = match items.view() {
            ValueRef::Undefined | ValueRef::Null => {
                return self.throw_type_error(roots, "Math.sumPrecise requires an iterable");
            }
            ValueRef::Object(_) | ValueRef::String(_) => self.get_iterator(host, roots, items)?,
            // Other primitives may inherit a custom Symbol.iterator as well.
            _ => {
                let method = match self.read_property(
                    host,
                    roots,
                    items,
                    PropertyKeyRef::Symbol(ITERATOR_SYMBOL),
                )? {
                    Invocation::Value(method) => method,
                    thrown => return Ok(thrown),
                };
                self.get_iterator_from_method(host, roots, items, &method)?
            }
        };
        let pair = match pair {
            Ok(pair) => pair,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let roots = &roots.with(&pair);
        let (iterator, next) = &pair;
        let mut state = SumState::MinusZero;
        let mut positive = Magnitude::default();
        let mut negative = Magnitude::default();
        let mut count = 0_u64;
        loop {
            let item = match self.iterator_step(host, roots, iterator, next)? {
                Ok(Some(item)) => item,
                Ok(None) => break,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            let error = if count == (1_u64 << 53) - 1 {
                self.make_range_error(roots, "Math.sumPrecise input is too long")?
            } else if let ValueRef::Number(number) = item.view() {
                count += 1;
                state = match state {
                    SumState::NaN => SumState::NaN,
                    _ if number.is_nan() => SumState::NaN,
                    SumState::Infinity(sign) if number.is_infinite() => {
                        if sign == number.is_sign_negative() {
                            state
                        } else {
                            SumState::NaN
                        }
                    }
                    _ if number.is_infinite() => SumState::Infinity(number.is_sign_negative()),
                    SumState::Infinity(_) => state,
                    _ if number.to_bits() == (-0.0_f64).to_bits() => state,
                    _ => {
                        if number.is_sign_negative() {
                            negative.add(number);
                        } else {
                            positive.add(number);
                        }
                        SumState::Finite
                    }
                };
                continue;
            } else {
                self.type_error(roots, "Math.sumPrecise accepts only Numbers")?
            };
            // IteratorClose must run, but a throw from return does not replace this error.
            self.close_iterator(host, &roots.with(&error), iterator)?;
            return Ok(Invocation::Thrown(error));
        }
        let result = match state {
            SumState::MinusZero => -0.0,
            SumState::NaN => f64::NAN,
            SumState::Infinity(true) => f64::NEG_INFINITY,
            SumState::Infinity(false) => f64::INFINITY,
            SumState::Finite => match positive.0.iter().rev().cmp(negative.0.iter().rev()) {
                std::cmp::Ordering::Less => -negative.subtract(&positive).rounded(),
                std::cmp::Ordering::Equal => 0.0,
                std::cmp::Ordering::Greater => positive.subtract(&negative).rounded(),
            },
        };
        Ok(Invocation::Value(Value::number(result)))
    }

    fn next_random(&mut self) -> f64 {
        // xorshift64: zero is the only invalid state. Use the high 53 bits to
        // produce an exactly representable fraction strictly below 1.
        let mut state = self.random_state;
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        self.random_state = state;
        (state >> 11) as f64 / (1_u64 << 53) as f64
    }
}

fn max(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        f64::NAN
    } else if y > x || (x == 0.0 && y == 0.0 && y.is_sign_positive()) {
        y
    } else {
        x
    }
}

fn round(x: f64) -> f64 {
    if (-0.5..0.0).contains(&x) {
        return -0.0;
    }
    let floor = x.floor();
    // Adding 0.5 to x before flooring can incorrectly round across a tie.
    if x - floor >= 0.5 { floor + 1.0 } else { floor }
}

#[derive(Clone, Copy)]
enum SumState {
    MinusZero,
    Finite,
    Infinity(bool), // true for negative infinity
    NaN,
}

// Every finite binary64 is an integer multiple of 2^-1074. The largest
// needs 2098 bits in those units; summing fewer than 2^53 terms needs at
// most 2151 bits. Separate unsigned totals avoid intermediate overflow
// and cancellation, with no per-item allocation.
struct Magnitude([u64; 34]);

impl Default for Magnitude {
    fn default() -> Self {
        Self([0; 34])
    }
}

impl Magnitude {
    const FRACTION_MASK: u64 = (1 << 52) - 1;

    fn add(&mut self, number: f64) {
        let bits = number.to_bits();
        let exponent = ((bits >> 52) & 0x7ff) as usize;
        let mut significand = bits & Self::FRACTION_MASK;
        if exponent != 0 {
            significand |= 1 << 52;
        }
        let shift = exponent.saturating_sub(1);
        let mut carry = u128::from(significand) << (shift % 64);
        for word in &mut self.0[shift / 64..] {
            if carry == 0 {
                break;
            }
            carry += u128::from(*word);
            *word = carry as u64;
            carry >>= 64;
        }
        debug_assert_eq!(carry, 0);
    }

    fn subtract(&mut self, other: &Self) -> &Self {
        let mut borrow = false;
        for (word, &rhs) in self.0.iter_mut().zip(&other.0) {
            let (difference, first) = word.overflowing_sub(rhs);
            let (difference, second) = difference.overflowing_sub(u64::from(borrow));
            *word = difference;
            borrow = first || second;
        }
        debug_assert!(!borrow);
        self
    }

    fn rounded(&self) -> f64 {
        let Some(high) = self.0.iter().rposition(|&word| word != 0) else {
            return 0.0;
        };
        let width = high * 64 + (64 - self.0[high].leading_zeros()) as usize;
        if width > 2098 {
            return f64::INFINITY;
        }
        let shift = width.saturating_sub(53);
        let index = shift / 64;
        let offset = shift % 64;
        let mut significand = self.0[index] >> offset;
        if offset != 0 {
            significand |= self.0[index + 1] << (64 - offset);
        }
        if shift != 0 {
            let guard_index = (shift - 1) / 64;
            let guard_mask = 1_u64 << ((shift - 1) % 64);
            let guard_word = self.0[guard_index];
            let sticky = guard_word & (guard_mask - 1) != 0
                || self.0[..guard_index].iter().any(|&word| word != 0);
            if guard_word & guard_mask != 0 && (sticky || significand & 1 != 0) {
                significand += 1;
            }
        }
        // The implicit bit supplies the exponent bias for normals. A carry
        // into the next exponent (including infinity) is handled by addition.
        f64::from_bits(((shift as u64) << 52) + significand)
    }
}
