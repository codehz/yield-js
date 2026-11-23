use std::sync::Arc;

use crate::binding::{Bindings, HostFault, JsValue, LiveRegistration, OperationToken};
use crate::value::Value;

/// Optional one-shot wakeup service for script timers and AbortSignal.timeout.
pub const TIMER_SERVICE: &str = "wintertc.timer";

pub use crate::console::ConsoleEvent;

/// Optional, synchronous console Printer service (version 1).
pub const CONSOLE_SERVICE: &str = "wintertc.console";

/// Receives owned-data notifications by reference. Implementations may clone
/// events but must not reenter the VM.
pub trait ConsoleSink: Send + Sync {
    fn write(&self, event: &ConsoleEvent);
}

/// Optional, synchronous entropy for the Web Crypto random APIs.
pub const CRYPTO_SERVICE: &str = "wintertc.crypto";

/// Supplies cryptographically secure random bytes without access to guest values.
/// Implementations must fill the entire slice on success and must not reenter the VM.
pub trait CryptoService: Send + Sync {
    fn fill_random(&self, bytes: &mut [u8]) -> Result<(), HostFault>;
}

/// Optional worker-side Web Crypto primitives, separate from synchronous entropy.
pub const CRYPTO_SUBTLE_SERVICE: &str = "wintertc.crypto.subtle";
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

/// Arms a platform wakeup without carrying any guest values into its callback.
/// The scheduler calls [`crate::Vm::wake_timer`] on the VM thread, then drains the turn.
pub trait TimerService: Send + Sync {
    fn arm(
        self: Arc<Self>,
        token: OperationToken,
        delay_ms: u64,
    ) -> Result<Box<dyn LiveRegistration>, HostFault>;
}

/// How a host operation completed. Values are produced outside a call, so
/// they must be constants; host code cannot mint object handles after the
/// call that created them.
#[derive(Debug, Clone)]
pub enum Completion {
    Fulfilled(JsValue),
    Rejected(JsValue),
}

/// Host-side resources and delivery endpoint.
///
/// Drivers from [`Bindings`] register external work. The host delivers
/// completions through [`crate::Vm::complete_operation`].
pub trait Host {
    fn bindings(&self) -> Arc<Bindings>;

    /// Current Unix epoch milliseconds, within the ECMAScript Date range.
    fn now_ms(&self) -> i64;

    /// The host's local time zone as a valid IANA identifier.
    fn time_zone(&self) -> &str;

    /// Value exposed as `navigator.userAgent`. `None` selects the runtime
    /// default. This identifies the script runtime for compatibility probes
    /// and is unrelated to the Fetch `User-Agent` request header.
    fn user_agent(&self) -> Option<&str> {
        None
    }

    /// Long-lived same-VM values retained by the host. Read only when a host
    /// binding requests collection, not by `Vm::collect_garbage` or snapshots.
    fn gc_roots(&self) -> &[Value] {
        &[]
    }

    /// Default report for an exception whose global error event was not canceled.
    /// This callback must not re-enter the VM.
    fn report_error(&mut self, error: JsValue);

    /// Default report for a Promise rejection whose notification was not canceled.
    /// This callback must not re-enter the VM.
    fn unhandled_rejection(&mut self, _promise: JsValue, _reason: JsValue) {}
}
