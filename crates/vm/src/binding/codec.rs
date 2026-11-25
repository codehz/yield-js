//! Shared host payload protocol. The VM owns JavaScript references separately.

use super::{CodecError, Services};

/// Append Rust payload data using the services of the VM being captured.
pub trait Encode<Owner = ()> {
    fn encode(&self, out: &mut Vec<u8>, services: &Services<'_, Owner>) -> Result<(), CodecError>;
}

/// Read one Rust payload value, advancing the cursor past that value.
pub trait Decode<Owner = ()>: Sized {
    fn decode(input: &mut &[u8], services: &Services<'_, Owner>) -> Result<Self, CodecError>;
}

pub(super) fn decode_exact<T: Decode<Owner>, Owner>(
    mut input: &[u8],
    services: &Services<'_, Owner>,
) -> Result<T, CodecError> {
    let value = T::decode(&mut input, services)?;
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
        impl<Owner> Encode<Owner> for $ty {
            fn encode(&self, out: &mut Vec<u8>, _: &Services<'_, Owner>) -> Result<(), CodecError> {
                out.extend_from_slice(&self.to_le_bytes());
                Ok(())
            }
        }

        impl<Owner> Decode<Owner> for $ty {
            fn decode(input: &mut &[u8], _: &Services<'_, Owner>) -> Result<Self, CodecError> {
                let (bytes, rest) = input.split_first_chunk()
                    .ok_or_else(|| CodecError::new("truncated payload"))?;
                *input = rest;
                Ok(Self::from_le_bytes(*bytes))
            }
        }
    )*};
}

fixed_width!(u8, u16, u32, u64, u128, i8, i16, i32, i64, i128, f32, f64);

impl<Owner> Encode<Owner> for () {
    fn encode(&self, _: &mut Vec<u8>, _: &Services<'_, Owner>) -> Result<(), CodecError> {
        Ok(())
    }
}

impl<Owner> Decode<Owner> for () {
    fn decode(_: &mut &[u8], _: &Services<'_, Owner>) -> Result<Self, CodecError> {
        Ok(())
    }
}

impl<Owner> Encode<Owner> for bool {
    fn encode(&self, out: &mut Vec<u8>, services: &Services<'_, Owner>) -> Result<(), CodecError> {
        u8::from(*self).encode(out, services)
    }
}

impl<Owner> Decode<Owner> for bool {
    fn decode(input: &mut &[u8], services: &Services<'_, Owner>) -> Result<Self, CodecError> {
        match u8::decode(input, services)? {
            0 => Ok(false),
            1 => Ok(true),
            tag => Err(CodecError::new(format!("invalid bool tag {tag}"))),
        }
    }
}

impl<Owner, T: Encode<Owner>> Encode<Owner> for Option<T> {
    fn encode(&self, out: &mut Vec<u8>, services: &Services<'_, Owner>) -> Result<(), CodecError> {
        self.is_some().encode(out, services)?;
        if let Some(value) = self {
            value.encode(out, services)?;
        }
        Ok(())
    }
}

impl<Owner, T: Decode<Owner>> Decode<Owner> for Option<T> {
    fn decode(input: &mut &[u8], services: &Services<'_, Owner>) -> Result<Self, CodecError> {
        match u8::decode(input, services)? {
            0 => Ok(None),
            1 => T::decode(input, services).map(Some),
            tag => Err(CodecError::new(format!("invalid Option tag {tag}"))),
        }
    }
}

fn encode_len<Owner>(
    len: usize,
    out: &mut Vec<u8>,
    services: &Services<'_, Owner>,
) -> Result<(), CodecError> {
    u32::try_from(len)
        .map_err(|_| CodecError::new("payload length exceeds u32"))?
        .encode(out, services)
}

impl<Owner> Encode<Owner> for String {
    fn encode(&self, out: &mut Vec<u8>, services: &Services<'_, Owner>) -> Result<(), CodecError> {
        encode_len(self.len(), out, services)?;
        out.extend_from_slice(self.as_bytes());
        Ok(())
    }
}

impl<Owner> Decode<Owner> for String {
    fn decode(input: &mut &[u8], services: &Services<'_, Owner>) -> Result<Self, CodecError> {
        let len = u32::decode(input, services)? as usize;
        let bytes = take(input, len)?;
        let text =
            std::str::from_utf8(bytes).map_err(|_| CodecError::new("invalid UTF-8 in payload"))?;
        Ok(text.to_owned())
    }
}

impl<Owner, T: Encode<Owner>> Encode<Owner> for Vec<T> {
    fn encode(&self, out: &mut Vec<u8>, services: &Services<'_, Owner>) -> Result<(), CodecError> {
        encode_len(self.len(), out, services)?;
        for value in self {
            value.encode(out, services)?;
        }
        Ok(())
    }
}

impl<Owner, T: Decode<Owner>> Decode<Owner> for Vec<T> {
    fn decode(input: &mut &[u8], services: &Services<'_, Owner>) -> Result<Self, CodecError> {
        let len = u32::decode(input, services)?;
        let mut values = Vec::new();
        for _ in 0..len {
            values.push(T::decode(input, services)?);
        }
        Ok(values)
    }
}
