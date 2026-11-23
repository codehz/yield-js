//! Host contract: the driver that owns bindings, host-retained values, and
//! default error reporting. Optional platform services live in submodules;
//! each is keyed by a zero-sized [`crate::ServiceKey`] and is absent or
//! defaulted unless the host registers it.

use std::sync::Arc;

use crate::binding::{Bindings, JsValue};
use crate::value::Value;

mod console;
mod crypto;
mod time;
mod timer;

pub use console::{ConsoleEvent, ConsoleKey, ConsoleSink};
pub use crypto::{
    CRYPTO_SUBTLE_PROTOCOL_VERSION, CryptoKey, CryptoService, SubtleCryptoKey, SubtleCryptoRequest,
    SubtleCryptoService,
};
pub use time::{DummyTimeService, SystemTimeService, TimeKey, TimeService};
pub use timer::{TimerKey, TimerService};

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
