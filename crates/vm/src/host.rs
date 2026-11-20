use crate::value::{HostValue, NativeKey, Value};

/// Metadata the VM persists for a recoverable host operation. Private host
/// state stays with the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingOperationMeta {
    pub id: String,
    pub module: String,
    pub export: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostExport {
    Value(HostValue),
    Function,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostReply {
    /// Synchronous completion.
    Value(HostValue),
    /// The export threw a script-visible error.
    Throw(String),
    /// The host accepted recoverable work. The VM creates the corresponding
    /// Promise and records only the stable operation ID.
    Operation { operation_id: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct HostException(pub HostValue);

#[derive(Debug, Clone, PartialEq)]
pub enum Completion {
    Fulfilled(HostValue),
    Rejected(HostValue),
}

/// Host adapter contract. The host is authoritative for operations, native
/// objects and timer/event delivery.
pub trait Host {
    fn resolve_export(&self, module: &str, export: &str) -> Option<HostExport>;

    /// Invoked synchronously by script code. Must not re-enter the VM.
    fn call(&mut self, module: &str, export: &str, args: &[HostValue])
    -> Result<HostReply, String>;

    /// Reads a property from a host-owned native object. Called synchronously.
    fn get_property(&self, _key: &NativeKey, _property: &str) -> Result<HostValue, HostException> {
        Ok(HostValue::Undefined)
    }

    /// Invokes a host-owned native function. Called synchronously.
    fn call_native(
        &mut self,
        key: &NativeKey,
        _args: &[HostValue],
    ) -> Result<HostReply, HostException> {
        Err(HostException(HostValue::String(
            format!("native {key} is not callable").into(),
        )))
    }

    /// Enables read-only own-data projection for matcher-style inspection.
    fn project_arguments(&self, _module: &str, _export: &str) -> bool {
        false
    }
    fn project_native_arguments(&self, _key: &NativeKey) -> bool {
        false
    }
    /// Called when a microtask callback throws with no promise result.
    /// The host decides whether and how to log the error.
    fn report_error(&mut self, _error: &Value) {}

    /// Called during snapshot restore before execution starts.
    fn has_native(&self, key: &NativeKey) -> bool;

    /// Called during snapshot restore. Returning `false` rejects the whole
    /// restore before the VM is created.
    fn rebuild_operation(&mut self, operation: &PendingOperationMeta) -> bool;
}
