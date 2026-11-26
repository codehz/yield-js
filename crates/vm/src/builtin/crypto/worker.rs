//! VM-owned SubtleCrypto transactions. Only owned primitive requests and
//! one-shot completion handles cross the host boundary; promises stay on the VM.

use std::collections::VecDeque;
use std::hint::black_box;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{Receiver, TryRecvError};

use crate::builtin::binary::{ARRAY_BUFFER_PROTOTYPE, MAX_BYTE_LENGTH};
use crate::error::VmError;
use crate::heap::{ArrayBufferObject, CryptoKeyAlgorithm, CryptoKeyUsage, Object, PlainObject};
use crate::host::{
    PlatformServices, SubtleCryptoCompletion, SubtleCryptoRequest, SubtleCryptoService,
};
use crate::runtime::Settlement;
use crate::value::{ObjectId, Value};
use crate::vm::VmCore;

// Bound all accepted transactions, including work that completed but has not
// yet been delivered. Each transaction has room for exactly one result.
const MAX_PENDING: usize = 16;
const SHA256_BYTES: usize = 32;

pub(crate) enum Output {
    Bytes(usize),
    Verify(Vec<u8>),
    AesKey {
        usages: Vec<CryptoKeyUsage>,
        length: usize,
    },
}

struct Pending {
    promise: ObjectId,
    output: Output,
    receiver: Receiver<Result<Vec<u8>, ()>>,
}

#[derive(Default)]
pub(crate) struct SubtleState {
    pending: VecDeque<Pending>,
}

impl std::fmt::Debug for SubtleState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubtleState")
            .field("pending", &self.pending.len())
            .finish_non_exhaustive()
    }
}

impl SubtleState {
    pub(crate) fn ids(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.pending.iter().map(|pending| pending.promise)
    }
}

impl VmCore {
    /// Whether accepted SubtleCrypto work still needs VM-side delivery.
    /// Call [`Self::run_until_idle`] again to poll completions without waiting.
    pub fn has_pending_subtle_operations(&self) -> bool {
        !self.subtle_state.pending.is_empty()
    }

    pub(crate) fn start_subtle_request<P: PlatformServices>(
        &mut self,
        host: &crate::host::Host<P>,
        promise: ObjectId,
        request: SubtleCryptoRequest,
        result: Output,
    ) -> Result<(), VmError> {
        let Some(service) = host.platform().subtle_crypto() else {
            let reason = self.alloc_dom_exception(
                "NotSupportedError".into(),
                "SubtleCrypto service unavailable".into(),
            );
            self.queue_adopt(promise, Settlement::Rejected(reason));
            return Ok(());
        };
        if self.subtle_state.pending.len() < MAX_PENDING {
            let (completion, receiver) = SubtleCryptoCompletion::channel();
            // Neither a HostFault nor a panic payload may reach script.
            let accepted = catch_unwind(AssertUnwindSafe(|| service.start(request, completion)));
            if let Ok(Ok(())) = accepted {
                self.subtle_state.pending.push_back(Pending {
                    promise,
                    output: result,
                    receiver,
                });
                return Ok(());
            }
        }
        let settlement = self.subtle_failure();
        self.queue_adopt(promise, settlement);
        Ok(())
    }

    /// Stage one completed transaction after a microtask checkpoint, then let
    /// its settlement and reactions run before delivering another transaction.
    pub(crate) fn pump_subtle_completion(&mut self) -> bool {
        let pending = &mut self.subtle_state.pending;
        let ready =
            pending
                .iter()
                .enumerate()
                .find_map(|(index, entry)| match entry.receiver.try_recv() {
                    Ok(result) => Some((index, result)),
                    Err(TryRecvError::Disconnected) => Some((index, Err(()))),
                    Err(TryRecvError::Empty) => None,
                });
        let Some((index, result)) = ready else {
            return false;
        };
        let Pending {
            promise, output, ..
        } = pending.remove(index).unwrap();
        let settlement = match result {
            Ok(bytes) => self.subtle_output(output, bytes),
            Err(()) => self.subtle_failure(),
        };
        self.queue_adopt(promise, settlement);
        true
    }

    fn subtle_failure(&mut self) -> Settlement {
        Settlement::Rejected(self.alloc_dom_exception(
            "OperationError".into(),
            "SubtleCrypto operation failed".into(),
        ))
    }

    fn subtle_output(&mut self, output: Output, bytes: Vec<u8>) -> Settlement {
        let value = match output {
            Output::Bytes(length) if bytes.len() == length && length <= MAX_BYTE_LENGTH => {
                Value::object(self.heap.alloc(Object::ArrayBuffer(ArrayBufferObject {
                    object: PlainObject::new(Some(self.builtins.get(ARRAY_BUFFER_PROTOTYPE))),
                    bytes: Some(bytes),
                    max_byte_length: None,
                })))
            }
            Output::Verify(signature) if bytes.len() == SHA256_BYTES => {
                Value::boolean(mac_matches(&bytes, &signature))
            }
            Output::AesKey { usages, length }
                if matches!(length, 16 | 24 | 32) && bytes.len() == length =>
            {
                super::subtle::alloc_key(self, CryptoKeyAlgorithm::AesGcm, usages, bytes)
            }
            _ => return self.subtle_failure(),
        };
        Settlement::Fulfilled(value)
    }
}

fn mac_matches(mac: &[u8], signature: &[u8]) -> bool {
    if signature.len() != SHA256_BYTES {
        return false;
    }
    let mut difference = 0;
    for i in 0..SHA256_BYTES {
        // Keep each XOR/OR round observable to the optimizer so it cannot
        // rewrite the reduction into a short-circuiting equality test.
        difference = black_box(difference | (mac[i] ^ signature[i]));
    }
    difference == 0
}
