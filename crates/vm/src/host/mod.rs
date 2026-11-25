//! Host contract: the driver that owns bindings, host-retained values, and
//! default error reporting. Optional platform services live in submodules;
//! each is keyed by a zero-sized [`crate::ServiceKey`] and is absent or
//! defaulted unless the host registers it.

use std::rc::Rc;

use crate::binding::{Bindings, JsValue};
use crate::value::{JsString, Value};

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
    fn bindings(&self) -> Rc<Bindings>;

    /// Value exposed as `navigator.userAgent`. `None` selects the runtime
    /// default. This identifies the script runtime for compatibility probes
    /// and is unrelated to the Fetch `User-Agent` request header.
    fn user_agent(&self) -> Option<&str> {
        None
    }

    /// Visits host-retained roots when a host binding requests collection.
    /// Explicit collection and snapshots take their own root slices instead.
    ///
    /// Pass borrowed values directly from host fields, for example:
    /// `self.pending.iter().chain(self.cache.values()).for_each(visit)`.
    /// Report only values from this VM. Duplicates are allowed; do not re-enter
    /// the VM while visiting. The VM handles any marking errors.
    fn visit_gc_roots(&self, _visit: &mut dyn FnMut(&Value)) {}

    /// Default report for an exception whose global error event was not canceled.
    /// This callback must not re-enter the VM.
    fn report_error(&mut self, error: JsValue);

    /// Same report, with an Error stack or message inspected without running script.
    /// Hosts that only need the thrown value can implement `report_error` alone.
    fn report_error_with_diagnostic(&mut self, error: JsValue, _diagnostic: &JsString) {
        self.report_error(error);
    }

    /// Diagnostic for a module initialization exception, before the VM is discarded.
    /// This does not dispatch an event or change the error returned by construction.
    fn report_startup_exception(&mut self, _diagnostic: &JsString) {}

    /// Default report for a Promise rejection whose notification was not canceled.
    /// This callback must not re-enter the VM.
    fn unhandled_rejection(&mut self, _promise: JsValue, _reason: JsValue) {}
}
