use std::sync::Arc;

use crate::binding::{Bindings, HostFault, JsValue, OperationStart};

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
/// Declarations, classes, codecs and services come from [`Bindings`]. The VM
/// owns every instance, prototype, slot and operation record; the host only
/// schedules work and delivers completions through
/// [`crate::Vm::complete_operation`].
pub trait Host {
    fn bindings(&self) -> Arc<Bindings>;

    /// Called synchronously after the VM records a new operation. The host may
    /// hand `start.state` to a platform service. It must not re-enter the VM.
    fn start_operation(&mut self, start: OperationStart<'_>) -> Result<(), HostFault>;

    /// Called during restore for `Resume` and `Reissue` operations, after the
    /// snapshot has been fully validated. Returning an error rejects the whole
    /// restore; the VM is not created.
    fn resume_operation(&mut self, start: OperationStart<'_>) -> Result<(), HostFault> {
        let _ = start;
        Err(HostFault::new(
            "host does not support operation recovery".to_owned(),
        ))
    }

    /// Reports an uncaught microtask error. The host decides whether to log it.
    fn report_error(&mut self, error: JsValue);
}
