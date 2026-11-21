use std::sync::Arc;

use crate::binding::{Bindings, JsValue};
use crate::value::Value;

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
