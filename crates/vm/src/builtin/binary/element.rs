//! Binary element encoding shared by DataView and TypedArray.
//!
//! A conversion produces an 8-byte pattern; the element width selects the low
//! bytes. Every integer conversion wraps modulo the element width instead of
//! saturating, and Float16 rounds a binary64 directly to binary16 so no value
//! is rounded twice through binary32.

use num_bigint::BigInt;
use num_traits::FromPrimitive;

use crate::heap::ElementKind;
use crate::value::{JsBigInt, Value};

/// Low `kind.bytes()` bytes of `bits`, big-endian first if `little` is false.
pub(crate) fn store(bytes: &mut [u8], kind: ElementKind, little: bool, bits: u64) {
    let width = kind.bytes();
    let source = bits.to_le_bytes();
    if little {
        bytes[..width].copy_from_slice(&source[..width]);
    } else {
        for (index, byte) in source[..width].iter().rev().enumerate() {
            bytes[index] = *byte;
        }
    }
}

pub(crate) fn load(bytes: &[u8], kind: ElementKind, little: bool) -> u64 {
    let width = kind.bytes();
    let mut buffer = [0u8; 8];
    if little {
        buffer[..width].copy_from_slice(&bytes[..width]);
    } else {
        for (index, byte) in bytes[..width].iter().rev().enumerate() {
            buffer[index] = *byte;
        }
    }
    u64::from_le_bytes(buffer)
}

/// Decode a stored pattern. Widths shorter than 8 bytes keep their low bits set
/// by [`load`], so narrowing never needs a second mask.
pub(crate) fn decode(kind: ElementKind, bits: u64) -> Value {
    match kind {
        ElementKind::Int8 => Value::number((bits as u8) as i8 as f64),
        ElementKind::Uint8 | ElementKind::Uint8Clamped => Value::number((bits as u8) as f64),
        ElementKind::Int16 => Value::number((bits as u16) as i16 as f64),
        ElementKind::Uint16 => Value::number((bits as u16) as f64),
        ElementKind::Int32 => Value::number((bits as u32) as i32 as f64),
        ElementKind::Uint32 => Value::number((bits as u32) as f64),
        ElementKind::Float16 => Value::number(f16_to_f64(bits as u16)),
        ElementKind::Float32 => Value::number(f32::from_bits(bits as u32) as f64),
        ElementKind::Float64 => Value::number(f64::from_bits(bits)),
        ElementKind::BigInt64 => Value::bigint(JsBigInt::from_i64(bits as i64)),
        ElementKind::BigUint64 => Value::bigint(JsBigInt::from_bigint(BigInt::from(bits))),
    }
}

/// Bit pattern stored for a converted Number-domain element, modulo its width.
pub(crate) fn number_bits(kind: ElementKind, number: f64) -> u64 {
    match kind {
        ElementKind::Int8 => wrapping(number, 8),
        ElementKind::Uint8 => wrapping(number, 8),
        ElementKind::Uint8Clamped => clamped(number),
        ElementKind::Int16 => wrapping(number, 16),
        ElementKind::Uint16 => wrapping(number, 16),
        ElementKind::Int32 => wrapping(number, 32),
        ElementKind::Uint32 => wrapping(number, 32),
        ElementKind::Float16 => u64::from(f64_to_f16_bits(number)),
        ElementKind::Float32 => u64::from((number as f32).to_bits()),
        ElementKind::Float64 => number.to_bits(),
        ElementKind::BigInt64 | ElementKind::BigUint64 => {
            unreachable!("BigInt element types convert through bigint_bits")
        }
    }
}

/// Bit pattern of a BigInt-domain element: the two's complement low 64 bits.
pub(crate) fn bigint_bits(big: &JsBigInt) -> u64 {
    let mask = (BigInt::from(1u8) << 64u32) - 1;
    let residue: BigInt = big.as_bigint() & mask;
    residue.to_u64_digits().1.first().copied().unwrap_or(0)
}

/// ToUint8, ToInt8, and every wider integer conversion: truncate, then wrap.
fn wrapping(number: f64, bits: u32) -> u64 {
    let mask = if bits == 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    if !number.is_finite() || number == 0.0 {
        return 0;
    }
    let truncated = number.trunc();
    // Integers below 2^53 convert exactly; wider magnitudes need the low bits
    // of their exact value, which only the significand carries.
    if truncated.abs() < 9_007_199_254_740_992.0 {
        return (truncated as i64) as u64 & mask;
    }
    let modulus = BigInt::from(1u8) << bits;
    match BigInt::from_f64(truncated) {
        Some(exact) => {
            let low: BigInt = exact & (&modulus - 1);
            low.to_u64_digits().1.first().copied().unwrap_or(0) & mask
        }
        None => 0,
    }
}

/// ToUint8Clamp: clamp to the range, then round halves to even.
fn clamped(number: f64) -> u64 {
    if number.is_nan() || number <= 0.0 {
        return 0;
    }
    if number >= 255.0 {
        return 255;
    }
    let floor = number.floor();
    let fraction = number - floor;
    let rounded = if fraction == 0.5 {
        if floor % 2.0 == 0.0 {
            floor
        } else {
            floor + 1.0
        }
    } else {
        number.round()
    };
    rounded as u64
}

/// Round a binary64 to binary16 with one rounding, ties to even.
pub(crate) fn f64_to_f16_bits(value: f64) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 63) as u16) << 15;
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    let mantissa = bits & ((1u64 << 52) - 1);
    if exponent == 0x7ff {
        if mantissa == 0 {
            return sign | 0x7c00;
        }
        // Keep the value a NaN with a nonzero payload.
        return sign | 0x7c00 | ((mantissa >> 42) as u16) | 0x200;
    }
    let (significand, power) = if exponent == 0 {
        (mantissa, -1074)
    } else {
        (mantissa | (1u64 << 52), exponent - 1075)
    };
    if significand == 0 {
        return sign;
    }
    let magnitude = 64 - significand.leading_zeros() as i32;
    let unbiased = power + magnitude - 1;
    // Round to a multiple of 2^quantum: whole units in the subnormal range,
    // ten fractional bits in the normal range.
    let quantum = (unbiased - 10).max(-24);
    let shift = power - quantum;
    let count = if shift >= 0 {
        if shift >= 64 {
            u64::MAX
        } else {
            significand << shift
        }
    } else {
        round_shift_even(significand, -shift)
    };
    if count == 0 {
        return sign;
    }
    let mut biased = quantum + 25;
    let mut count = count;
    if count >= 2048 {
        count >>= 1;
        biased += 1;
    }
    if biased >= 31 {
        return sign | 0x7c00;
    }
    if count < 1024 {
        // Subnormal: biased is 1 here, so the count is the stored mantissa.
        return sign | count as u16;
    }
    sign | ((biased as u16) << 10) | (count as u16 - 1024)
}

fn round_shift_even(value: u64, shift: i32) -> u64 {
    if shift >= 65 {
        return 0;
    }
    let shift = shift as u32;
    let quotient = value >> shift;
    let dropped = value & ((1u64 << shift) - 1);
    let half = 1u64 << (shift - 1);
    if dropped > half || (dropped == half && quotient & 1 == 1) {
        quotient + 1
    } else {
        quotient
    }
}

pub(crate) fn f16_to_f64(bits: u16) -> f64 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = i32::from((bits >> 10) & 0x1f);
    let mantissa = i32::from(bits & 0x3ff);
    if exponent == 0 {
        return sign * f64::from(mantissa) * 2f64.powi(-24);
    }
    if exponent == 0x1f {
        return if mantissa == 0 {
            sign * f64::INFINITY
        } else {
            f64::NAN
        };
    }
    sign * f64::from(1024 + mantissa) * 2f64.powi(exponent - 25)
}
