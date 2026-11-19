use crate::value::{HostValue, NativeKey, Value};

/// Metadata the VM persists for a recoverable host operation. Private host
/// state stays with the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingOperationMeta {
    pub id: String,
    pub import: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostReply {
    /// Synchronous completion.
    Value(HostValue),
    /// The host accepted recoverable work. The VM creates the corresponding
    /// Promise and records only the stable operation ID.
    Operation { operation_id: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Completion {
    Fulfilled(HostValue),
    Rejected(HostValue),
}

/// Host adapter contract. The host is authoritative for operations, native
/// objects and timer/event delivery.
pub trait Host {
    fn has_import(&self, name: &str) -> bool;

    /// Invoked synchronously by script code. Must not re-enter the VM.
    fn call(&mut self, import: &str, args: &[HostValue]) -> Result<HostReply, String>;

    /// Called when a microtask callback throws with no promise result.
    /// The host decides whether and how to log the error.
    fn report_error(&mut self, _error: &Value) {}

    /// Called during snapshot restore before execution starts.
    fn has_native(&self, key: &NativeKey) -> bool;

    /// Called during snapshot restore. Returning `false` rejects the whole
    /// restore before the VM is created.
    fn rebuild_operation(&mut self, operation: &PendingOperationMeta) -> bool;
}
