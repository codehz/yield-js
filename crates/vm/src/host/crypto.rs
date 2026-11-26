//! Optional Web Crypto services: synchronous entropy and submitted subtle
//! primitives. Neither may carry guest values across the boundary.

use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

use crate::binding::HostFault;

/// Supplies cryptographically secure random bytes without access to guest values.
/// Implementations must fill the entire slice on success and must not reenter the VM.
pub trait CryptoService: Send + Sync {
    fn fill_random(&self, bytes: &mut [u8]) -> Result<(), HostFault>;
}

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

/// Submits owned primitive work on the VM thread. Implementations must return
/// promptly without blocking or reentering the VM. `Ok(())` accepts the request;
/// `Err` means it was not accepted, even if the handle was already completed.
/// The host may complete immediately or schedule the work itself.
pub trait SubtleCryptoService {
    fn start(
        &self,
        request: SubtleCryptoRequest,
        completion: SubtleCryptoCompletion,
    ) -> Result<(), HostFault>;
}

/// A one-shot result handle for a single request, with no VM references or JS values.
/// It may be sent to another thread. Dropping it without completing an accepted
/// request causes the VM to reject that request when it next polls.
pub struct SubtleCryptoCompletion(SyncSender<Result<Vec<u8>, ()>>);

impl SubtleCryptoCompletion {
    pub(crate) fn channel() -> (Self, Receiver<Result<Vec<u8>, ()>>) {
        let (sender, receiver) = sync_channel(1);
        (Self(sender), receiver)
    }

    /// Submits the result without waiting for VM delivery. Host fault details
    /// are discarded, as is the result if the VM has dropped its receiver.
    pub fn complete(self, result: Result<Vec<u8>, HostFault>) {
        let _ = self.0.try_send(result.map_err(|_| ()));
    }
}
