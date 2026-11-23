//! Shared host payload protocol. The VM owns JavaScript references separately.

use super::{Bindings, CodecError};

/// Append Rust payload data using the bindings of the VM being captured.
pub trait Encode {
    fn encode(&self, out: &mut Vec<u8>, bindings: &Bindings) -> Result<(), CodecError>;
}

/// Read one Rust payload value, advancing the cursor past that value.
pub trait Decode: Sized {
    fn decode(input: &mut &[u8], bindings: &Bindings) -> Result<Self, CodecError>;
}

pub(super) fn decode_exact<T: Decode>(
    mut input: &[u8],
    bindings: &Bindings,
) -> Result<T, CodecError> {
    let value = T::decode(&mut input, bindings)?;
    if !input.is_empty() {
        return Err(CodecError::new("trailing payload bytes"));
    }
    Ok(value)
}

fn take<'a>(input: &mut &'a [u8], len: usize) -> Result<&'a [u8], CodecError> {
    let (bytes, rest) = input
        .split_at_checked(len)
        .ok_or_else(|| CodecError::new("truncated payload"))?;
    *input = rest;
    Ok(bytes)
}

macro_rules! fixed_width {
    ($($ty:ty),* $(,)?) => {$(
        impl Encode for $ty {
            fn encode(&self, out: &mut Vec<u8>, _: &Bindings) -> Result<(), CodecError> {
                out.extend_from_slice(&self.to_le_bytes());
                Ok(())
            }
        }

        impl Decode for $ty {
            fn decode(input: &mut &[u8], _: &Bindings) -> Result<Self, CodecError> {
                let (bytes, rest) = input.split_first_chunk()
                    .ok_or_else(|| CodecError::new("truncated payload"))?;
                *input = rest;
                Ok(Self::from_le_bytes(*bytes))
            }
        }
    )*};
}

fixed_width!(u8, u16, u32, u64, u128, i8, i16, i32, i64, i128, f32, f64);

impl Encode for () {
    fn encode(&self, _: &mut Vec<u8>, _: &Bindings) -> Result<(), CodecError> {
        Ok(())
    }
}

impl Decode for () {
    fn decode(_: &mut &[u8], _: &Bindings) -> Result<Self, CodecError> {
        Ok(())
    }
}

impl Encode for bool {
    fn encode(&self, out: &mut Vec<u8>, bindings: &Bindings) -> Result<(), CodecError> {
        u8::from(*self).encode(out, bindings)
    }
}

impl Decode for bool {
    fn decode(input: &mut &[u8], bindings: &Bindings) -> Result<Self, CodecError> {
        match u8::decode(input, bindings)? {
            0 => Ok(false),
            1 => Ok(true),
            tag => Err(CodecError::new(format!("invalid bool tag {tag}"))),
        }
    }
}

impl<T: Encode> Encode for Option<T> {
    fn encode(&self, out: &mut Vec<u8>, bindings: &Bindings) -> Result<(), CodecError> {
        self.is_some().encode(out, bindings)?;
        if let Some(value) = self {
            value.encode(out, bindings)?;
        }
        Ok(())
    }
}

impl<T: Decode> Decode for Option<T> {
    fn decode(input: &mut &[u8], bindings: &Bindings) -> Result<Self, CodecError> {
        match u8::decode(input, bindings)? {
            0 => Ok(None),
            1 => T::decode(input, bindings).map(Some),
            tag => Err(CodecError::new(format!("invalid Option tag {tag}"))),
        }
    }
}

fn encode_len(len: usize, out: &mut Vec<u8>, bindings: &Bindings) -> Result<(), CodecError> {
    u32::try_from(len)
        .map_err(|_| CodecError::new("payload length exceeds u32"))?
        .encode(out, bindings)
}

impl Encode for String {
    fn encode(&self, out: &mut Vec<u8>, bindings: &Bindings) -> Result<(), CodecError> {
        encode_len(self.len(), out, bindings)?;
        out.extend_from_slice(self.as_bytes());
        Ok(())
    }
}

impl Decode for String {
    fn decode(input: &mut &[u8], bindings: &Bindings) -> Result<Self, CodecError> {
        let len = u32::decode(input, bindings)? as usize;
        let bytes = take(input, len)?;
        let text =
            std::str::from_utf8(bytes).map_err(|_| CodecError::new("invalid UTF-8 in payload"))?;
        Ok(text.to_owned())
    }
}

impl<T: Encode> Encode for Vec<T> {
    fn encode(&self, out: &mut Vec<u8>, bindings: &Bindings) -> Result<(), CodecError> {
        encode_len(self.len(), out, bindings)?;
        for value in self {
            value.encode(out, bindings)?;
        }
        Ok(())
    }
}

impl<T: Decode> Decode for Vec<T> {
    fn decode(input: &mut &[u8], bindings: &Bindings) -> Result<Self, CodecError> {
        let len = u32::decode(input, bindings)?;
        let mut values = Vec::new();
        for _ in 0..len {
            values.push(T::decode(input, bindings)?);
        }
        Ok(values)
    }
}
