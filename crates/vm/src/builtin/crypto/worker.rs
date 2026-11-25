//! VM-owned SubtleCrypto transactions. Only primitive inputs and outputs cross
//! the worker boundary; promises and their settlement policy stay on the VM.

use std::collections::VecDeque;
use std::hint::black_box;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};

use crate::builtin::binary::{ARRAY_BUFFER_PROTOTYPE, MAX_BYTE_LENGTH};
use crate::error::VmError;
use crate::heap::{ArrayBufferObject, CryptoKeyAlgorithm, CryptoKeyUsage, Object, PlainObject};
use crate::host::{SubtleCryptoRequest, SubtleCryptoService};
use crate::runtime::Settlement;
use crate::value::{ObjectId, Value};
use crate::vm::VmCore;

// Bound all accepted transactions, including work that completed but has not
// yet been delivered. Both channels are therefore bounded without ever making
// the VM wait for room or for a result.
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

struct Worker {
    requests: SyncSender<SubtleCryptoRequest>,
    results: Receiver<Result<Vec<u8>, ()>>,
    closed: Arc<AtomicBool>,
}

impl Worker {
    fn spawn(service: Arc<dyn SubtleCryptoService>) -> std::io::Result<Self> {
        let (requests, incoming) = mpsc::sync_channel(MAX_PENDING);
        let (outgoing, results) = mpsc::sync_channel(MAX_PENDING);
        let closed = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::clone(&closed);
        std::thread::Builder::new()
            .name("yield-subtle-crypto".into())
            .spawn(move || {
                while let Ok(request) = incoming.recv() {
                    if cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    // Neither a HostFault nor a panic payload may reach script.
                    let result = catch_unwind(AssertUnwindSafe(|| service.execute(request)))
                        .map_err(|_| ())
                        .and_then(|result| result.map_err(|_| ()));
                    if outgoing.try_send(result).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            requests,
            results,
            closed,
        })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Detach rather than joining a possibly slow host service call. Once
        // that call returns, no queued requests are executed for the dead VM.
        self.closed.store(true, Ordering::Release);
    }
}

#[derive(Default)]
pub(crate) struct SubtleState {
    worker: Option<Worker>,
    pending: VecDeque<(ObjectId, Output)>,
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
        self.pending.iter().map(|&(promise, _)| promise)
    }
}

impl VmCore {
    /// Whether accepted SubtleCrypto work still needs VM-side delivery.
    /// Call [`Self::run_until_idle`] again to poll completions without waiting.
    pub fn has_pending_subtle_operations(&self) -> bool {
        !self.subtle_state.pending.is_empty()
    }

    pub(crate) fn start_subtle_request(
        &mut self,
        host: &crate::host::Host,
        promise: ObjectId,
        request: SubtleCryptoRequest,
        result: Output,
    ) -> Result<(), VmError> {
        let state = &mut self.subtle_state;
        if state.worker.is_none() {
            let Some(service) = host.services().get::<Arc<dyn SubtleCryptoService>>() else {
                let reason = self.alloc_dom_exception(
                    "NotSupportedError".into(),
                    "SubtleCrypto service unavailable".into(),
                );
                self.queue_adopt(promise, Settlement::Rejected(reason));
                return Ok(());
            };
            state.worker = Worker::spawn(Arc::clone(service)).ok();
        }
        if state.pending.len() < MAX_PENDING
            && let Some(worker) = &state.worker
            && worker.requests.try_send(request).is_ok()
        {
            // One FIFO worker means delivery order also identifies the VM-side
            // transaction, without sending object IDs to the service.
            state.pending.push_back((promise, result));
        } else {
            // A failed send with no pending work cannot be a full queue.
            if state.pending.is_empty() {
                state.worker = None;
            }
            let settlement = self.subtle_failure();
            self.queue_adopt(promise, settlement);
        }
        Ok(())
    }

    /// Stage one completed transaction after a microtask checkpoint, then let
    /// its settlement and reactions run before delivering another transaction.
    pub(crate) fn pump_subtle_completion(&mut self) -> bool {
        if !self.has_pending_subtle_operations() {
            return false;
        }
        let result = self
            .subtle_state
            .worker
            .as_ref()
            .map_or(Err(TryRecvError::Disconnected), |worker| {
                worker.results.try_recv()
            });
        match result {
            Ok(result) => {
                let (promise, output) = self.subtle_state.pending.pop_front().unwrap();
                let settlement = match result {
                    Ok(bytes) => self.subtle_output(output, bytes),
                    Err(()) => self.subtle_failure(),
                };
                self.queue_adopt(promise, settlement);
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                // Reject every accepted request that no longer has a producer.
                // Keep the closed worker until all its transactions are drained.
                let (promise, _) = self.subtle_state.pending.pop_front().unwrap();
                let settlement = self.subtle_failure();
                self.queue_adopt(promise, settlement);
                if !self.has_pending_subtle_operations() {
                    self.subtle_state.worker = None;
                }
                true
            }
        }
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
