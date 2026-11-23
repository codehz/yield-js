//! Optional Web Crypto services: synchronous entropy and worker-side subtle
//! primitives. Neither may carry guest values across the boundary.

use std::sync::Arc;

use crate::binding::HostFault;

/// Supplies cryptographically secure random bytes without access to guest values.
/// Implementations must fill the entire slice on success and must not reenter the VM.
pub trait CryptoService: Send + Sync {
    fn fill_random(&self, bytes: &mut [u8]) -> Result<(), HostFault>;
}

/// Optional, synchronous entropy for the Web Crypto random APIs.
pub struct CryptoKey;

impl crate::ServiceKey for CryptoKey {
    type Value = Arc<dyn CryptoService>;
    const ID: &'static str = "wintertc.crypto";
    const VERSION: u32 = 1;
    const IN_FINGERPRINT: bool = false;
}

pub const CRYPTO_SUBTLE_PROTOCOL_VERSION: u32 = 1;

/// An owned request. PBKDF2 output length and GCM tag length are in bits.
/// GCM ciphertext includes the trailing authentication tag.
pub enum SubtleCryptoRequest {
    DigestSha256 {
        data: Vec<u8>,
    },
    Pbkdf2Sha256 {
        password: Vec<u8>,
        salt: Vec<u8>,
        iterations: u32,
        length: u32,
    },
    AesGcmEncrypt {
        key: Vec<u8>,
        iv: Vec<u8>,
        additional_data: Vec<u8>,
        tag_length: u16,
        data: Vec<u8>,
    },
    AesGcmDecrypt {
        key: Vec<u8>,
        iv: Vec<u8>,
        additional_data: Vec<u8>,
        tag_length: u16,
        data: Vec<u8>,
    },
    HmacSha256 {
        key: Vec<u8>,
        data: Vec<u8>,
    },
}

/// Called off the VM thread. Implementations cannot access or reenter the VM.
pub trait SubtleCryptoService: Send + Sync {
    fn execute(&self, request: SubtleCryptoRequest) -> Result<Vec<u8>, HostFault>;
}

/// Optional worker-side Web Crypto primitives, separate from synchronous entropy.
pub struct SubtleCryptoKey;

impl crate::ServiceKey for SubtleCryptoKey {
    type Value = Arc<dyn SubtleCryptoService>;
    const ID: &'static str = "wintertc.crypto.subtle";
    const VERSION: u32 = CRYPTO_SUBTLE_PROTOCOL_VERSION;
    const IN_FINGERPRINT: bool = false;
}
