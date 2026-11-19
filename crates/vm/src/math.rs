//! Math uses the VM's numeric coercion and owns no script-visible constructor.

use std::f64::consts;

use crate::error::VmError;
use crate::heap::Attributes;
use crate::host::Host;
use crate::ops::number_to_uint32;
use crate::runtime::Invocation;
use crate::value::{ObjectId, Value};
use crate::vm::Vm;

pub(crate) const RANDOM_SEED: u64 = 0x9e37_79b9_7f4a_7c15;

pub(crate) const MATH_METHODS: &[&str] = &[
    "abs", "acos", "acosh", "asin", "asinh", "atan", "atan2", "atanh", "cbrt", "ceil", "clz32",
    "cos", "cosh", "exp", "expm1", "floor", "fround", "hypot", "imul", "log", "log1p", "log10",
    "log2", "max", "min", "pow", "random", "round", "sign", "sin", "sinh", "sqrt", "tan", "tanh",
    "trunc",
];

impl Vm {
    pub(crate) fn init_math(&mut self) -> ObjectId {
        let math = self.heap.alloc_plain();
        for (name, number) in [
            ("E", consts::E),
            ("LN10", consts::LN_10),
            ("LN2", consts::LN_2),
            ("LOG10E", consts::LOG10_E),
            ("LOG2E", consts::LOG2_E),
            ("PI", consts::PI),
            ("SQRT1_2", consts::FRAC_1_SQRT_2),
            ("SQRT2", consts::SQRT_2),
        ] {
            self.heap
                .define_data(math, name, Value::Number(number), Attributes::FIXED)
                .expect("Math stores its constants");
        }
        for &name in MATH_METHODS {
            let method = self.heap.alloc_builtin("Math", Some(name));
            self.heap
                .define_data(math, name, Value::Object(method), Attributes::METHOD)
                .expect("Math stores its methods");
        }
        math
    }

    pub(crate) fn call_math_method(
        &mut self,
        host: &mut dyn Host,
        method: &str,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let result = match method {
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
                    match self.coerce_number(host, arg)? {
                        Ok(number) if method == "hypot" && !number.is_finite() => {
                            saw_nan |= number.is_nan();
                            saw_infinity |= number.is_infinite();
                        }
                        Ok(number) => result = combine(result, number),
                        Err(error) => return Ok(Invocation::Thrown(error)),
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
                let x = match self.coerce_number(host, args.first().unwrap_or(&Value::Undefined))? {
                    Ok(number) => number,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                match method {
                    "atan2" | "imul" | "pow" => {
                        let y = match self
                            .coerce_number(host, args.get(1).unwrap_or(&Value::Undefined))?
                        {
                            Ok(number) => number,
                            Err(error) => return Ok(Invocation::Thrown(error)),
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
                    "abs" => x.abs(),
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
        Ok(Invocation::Value(Value::Number(result)))
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
