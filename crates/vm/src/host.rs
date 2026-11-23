use std::sync::Arc;

use crate::binding::{Bindings, HostFault, JsValue, LiveRegistration, OperationToken};
use crate::value::Value;

/// An optional monotonic clock shared with the VM, in an arbitrary time domain.
pub trait MonotonicClock: std::fmt::Debug + Send + Sync {
    fn millis(&self) -> u64;
}

/// Optional one-shot wakeup service used by AbortSignal.timeout.
pub const TIMER_SERVICE: &str = "wintertc.timer";

/// Arms a platform wakeup without carrying any guest values into its callback.
/// The scheduler delivers the token on the VM thread, then drains the turn.
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

    fn monotonic_clock(&self) -> Option<Arc<dyn MonotonicClock>> {
        None
    }

    /// The host's local time zone as a valid IANA identifier.
    fn time_zone(&self) -> &str;

    /// Long-lived same-VM values retained by the host. Read only when a host
    /// binding requests collection, not by `Vm::collect_garbage` or snapshots.
    fn gc_roots(&self) -> &[Value] {
        &[]
    }

    /// Reports an uncaught microtask error. The host decides whether to log it.
    fn report_error(&mut self, error: JsValue);

    /// Reports a rejection that still has no reaction at the microtask checkpoint.
    /// This callback must not re-enter the VM.
    fn unhandled_rejection(&mut self, _promise: JsValue, _reason: JsValue) {}

    /// Reports the first observation of a previously reported rejection.
    /// This callback must not re-enter the VM.
    fn rejection_handled(&mut self, _promise: JsValue) {}
}
