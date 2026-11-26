//! Cryptographic primitives for harness interoperability tests only.
//! The AES implementation uses secret-indexed lookups and must not be used in production.

use sha2::{Digest, Sha256};
use yield_vm::binding::HostFault;
use yield_vm::host::{SubtleCryptoRequest, SubtleCryptoService};

#[derive(Clone)]
pub struct TestSubtleCrypto;

impl SubtleCryptoService for TestSubtleCrypto {
    fn execute(&self, request: SubtleCryptoRequest) -> Result<Vec<u8>, HostFault> {
        use SubtleCryptoRequest::*;
        match request {
            DigestSha256 { data } => Ok(Sha256::digest(data).to_vec()),
            HmacSha256 { key, data } => Ok(Hmac::new(&key).digest(&data, &[]).to_vec()),
            Pbkdf2Sha256 {
                password,
                salt,
                iterations,
                length,
            } => pbkdf2(&password, &salt, iterations, length),
            AesGcmEncrypt {
                key,
                iv,
                additional_data,
                tag_length,
                data,
            } => {
                let gcm = Gcm::new(&key, &iv, tag_length)?;
                let mut data = data;
                gcm.apply_ctr(&mut data)?;
                let tag = gcm.tag(&additional_data, &data)?;
                data.extend_from_slice(&tag[..gcm.tag_bytes]);
                Ok(data)
            }
            AesGcmDecrypt {
                key,
                iv,
                additional_data,
                tag_length,
                data,
            } => {
                let gcm = Gcm::new(&key, &iv, tag_length)?;
                let mut data = data;
                let ciphertext_len = data
                    .len()
                    .checked_sub(gcm.tag_bytes)
                    .ok_or_else(auth_error)?;
                let (ciphertext, supplied_tag) = data.split_at(ciphertext_len);
                let expected_tag = gcm.tag(&additional_data, ciphertext)?;
                let difference = supplied_tag
                    .iter()
                    .zip(expected_tag)
                    .fold(0, |acc, (&a, b)| acc | (a ^ b));
                if difference != 0 {
                    return Err(auth_error());
                }
                data.truncate(ciphertext_len);
                gcm.apply_ctr(&mut data)?;
                Ok(data)
            }
        }
    }
}

fn auth_error() -> HostFault {
    HostFault("AES-GCM authentication failed".into())
}

struct Hmac {
    inner: Sha256,
    outer: Sha256,
}

impl Hmac {
    fn new(key: &[u8]) -> Self {
        let mut pad = [0; 64];
        if key.len() > pad.len() {
            pad[..32].copy_from_slice(&Sha256::digest(key));
        } else {
            pad[..key.len()].copy_from_slice(key);
        }
        for byte in &mut pad {
            *byte ^= 0x36;
        }
        let mut inner = Sha256::new();
        inner.update(pad);
        for byte in &mut pad {
            *byte ^= 0x36 ^ 0x5c;
        }
        let mut outer = Sha256::new();
        outer.update(pad);
        Self { inner, outer }
    }

    fn digest(&self, first: &[u8], second: &[u8]) -> [u8; 32] {
        let mut inner = self.inner.clone();
        inner.update(first);
        inner.update(second);
        let mut outer = self.outer.clone();
        outer.update(inner.finalize());
        outer.finalize().into()
    }
}

fn pbkdf2(
    password: &[u8],
    salt: &[u8],
    iterations: u32,
    length: u32,
) -> Result<Vec<u8>, HostFault> {
    if iterations == 0 || length == 0 || !length.is_multiple_of(8) {
        return Err(HostFault("invalid PBKDF2 parameters".into()));
    }
    let hmac = Hmac::new(password);
    let mut output = vec![0; (length / 8) as usize];
    for (i, chunk) in output.chunks_mut(32).enumerate() {
        let mut u = hmac.digest(salt, &(i as u32 + 1).to_be_bytes());
        chunk.copy_from_slice(&u[..chunk.len()]);
        for _ in 1..iterations {
            u = hmac.digest(&u, &[]);
            for (byte, next) in chunk.iter_mut().zip(u) {
                *byte ^= next;
            }
        }
    }
    Ok(output)
}

struct Gcm {
    aes: Aes,
    h: u128,
    j0: [u8; 16],
    tag_bytes: usize,
}

impl Gcm {
    fn new(key: &[u8], iv: &[u8], tag_length: u16) -> Result<Self, HostFault> {
        if iv.is_empty() || !matches!(tag_length, 32 | 64 | 96 | 104 | 112 | 120 | 128) {
            return Err(HostFault("invalid AES-GCM IV or tag length".into()));
        }
        let aes = Aes::new(key)?;
        let h = u128::from_be_bytes(aes.encrypt([0; 16]));
        let j0 = if iv.len() == 12 {
            let mut block = [0; 16];
            block[..12].copy_from_slice(iv);
            block[15] = 1;
            block
        } else {
            // GHASH an empty AAD stream followed by the IV and its bit length.
            ghash(h, &[], iv)?.to_be_bytes()
        };
        Ok(Self {
            aes,
            h,
            j0,
            tag_bytes: usize::from(tag_length / 8),
        })
    }

    fn tag(&self, aad: &[u8], ciphertext: &[u8]) -> Result<[u8; 16], HostFault> {
        check_message_length(ciphertext.len())?;
        let mask = u128::from_be_bytes(self.aes.encrypt(self.j0));
        Ok((mask ^ ghash(self.h, aad, ciphertext)?).to_be_bytes())
    }

    fn apply_ctr(&self, data: &mut [u8]) -> Result<(), HostFault> {
        check_message_length(data.len())?;
        let mut counter = self.j0;
        let mut low = u32::from_be_bytes(counter[12..].try_into().unwrap());
        for chunk in data.chunks_mut(16) {
            low = low.wrapping_add(1);
            counter[12..].copy_from_slice(&low.to_be_bytes());
            for (byte, mask) in chunk.iter_mut().zip(self.aes.encrypt(counter)) {
                *byte ^= mask;
            }
        }
        Ok(())
    }
}

fn check_message_length(len: usize) -> Result<(), HostFault> {
    if len.div_ceil(16) as u64 > u64::from(u32::MAX - 1) {
        return Err(HostFault("AES-GCM message exceeds counter limit".into()));
    }
    Ok(())
}

fn ghash(h: u128, aad: &[u8], data: &[u8]) -> Result<u128, HostFault> {
    fn bit_len(input: &[u8]) -> Result<u64, HostFault> {
        (input.len() as u64)
            .checked_mul(8)
            .ok_or_else(|| HostFault("AES-GCM bit length overflows".into()))
    }
    let lengths = (u128::from(bit_len(aad)?) << 64) | u128::from(bit_len(data)?);
    let mut acc = 0;
    // Pad each stream independently, not their concatenation.
    for chunk in aad.chunks(16).chain(data.chunks(16)) {
        let mut block = [0; 16];
        block[..chunk.len()].copy_from_slice(chunk);
        acc = gf_mul(acc ^ u128::from_be_bytes(block), h);
    }
    Ok(gf_mul(acc ^ lengths, h))
}

// GHASH represents the constant coefficient in the most significant bit.
fn gf_mul(x: u128, mut y: u128) -> u128 {
    let mut acc = 0;
    for bit in (0..128).rev() {
        acc ^= y & 0u128.wrapping_sub((x >> bit) & 1);
        let reduce = 0u128.wrapping_sub(y & 1);
        y = (y >> 1) ^ (0xe100_0000_0000_0000_0000_0000_0000_0000 & reduce);
    }
    acc
}

struct Aes {
    words: [[u8; 4]; 60],
    rounds: usize,
}

impl Aes {
    fn new(key: &[u8]) -> Result<Self, HostFault> {
        let nk = match key.len() {
            16 => 4,
            24 => 6,
            32 => 8,
            _ => return Err(HostFault("invalid AES key length".into())),
        };
        let rounds = nk + 6;
        let mut words = [[0; 4]; 60];
        for (word, source) in words.iter_mut().zip(key.as_chunks::<4>().0) {
            *word = *source;
        }
        let mut rcon = 1;
        for i in nk..4 * (rounds + 1) {
            let mut t = words[i - 1];
            if i % nk == 0 {
                t.rotate_left(1);
                t = t.map(sub_byte);
                t[0] ^= rcon;
                rcon = xtime(rcon);
            } else if nk == 8 && i % nk == 4 {
                t = t.map(sub_byte);
            }
            for (j, byte) in t.into_iter().enumerate() {
                words[i][j] = words[i - nk][j] ^ byte;
            }
        }
        Ok(Self { words, rounds })
    }

    fn add_round_key(&self, state: &mut [u8; 16], round: usize) {
        let start = round * 4;
        for (byte, key) in state
            .iter_mut()
            .zip(self.words[start..start + 4].iter().flatten())
        {
            *byte ^= key;
        }
    }

    fn encrypt(&self, mut state: [u8; 16]) -> [u8; 16] {
        self.add_round_key(&mut state, 0);
        for round in 1..=self.rounds {
            let previous = state;
            // SubBytes and ShiftRows on a column-major state.
            for col in 0..4 {
                for row in 0..4 {
                    state[4 * col + row] = sub_byte(previous[4 * ((col + row) % 4) + row]);
                }
            }
            if round != self.rounds {
                for col in state.as_chunks_mut::<4>().0 {
                    let [a, b, c, d] = *col;
                    let sum = a ^ b ^ c ^ d;
                    col[0] ^= sum ^ xtime(a ^ b);
                    col[1] ^= sum ^ xtime(b ^ c);
                    col[2] ^= sum ^ xtime(c ^ d);
                    col[3] ^= sum ^ xtime(d ^ a);
                }
            }
            self.add_round_key(&mut state, round);
        }
        state
    }
}

fn xtime(x: u8) -> u8 {
    (x << 1) ^ (0x1b & 0u8.wrapping_sub(x >> 7))
}

fn sub_byte(x: u8) -> u8 {
    // FIPS 197, Table 4: S-box.
    const SBOX: [u8; 256] = [
        0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab,
        0x76, 0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4,
        0x72, 0xc0, 0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71,
        0xd8, 0x31, 0x15, 0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2,
        0xeb, 0x27, 0xb2, 0x75, 0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6,
        0xb3, 0x29, 0xe3, 0x2f, 0x84, 0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb,
        0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf, 0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45,
        0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8, 0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5,
        0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2, 0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44,
        0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73, 0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a,
        0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb, 0xe0, 0x32, 0x3a, 0x0a, 0x49,
        0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79, 0xe7, 0xc8, 0x37, 0x6d,
        0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08, 0xba, 0x78, 0x25,
        0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a, 0x70, 0x3e,
        0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e, 0xe1,
        0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
        0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb,
        0x16,
    ];
    SBOX[usize::from(x)]
}
