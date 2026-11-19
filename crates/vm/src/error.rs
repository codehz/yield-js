use crate::value::NativeKey;
use crate::value::Value;

#[derive(Debug, thiserror::Error)]
pub enum VmError {
    #[error("invalid bytecode build: {0}")]
    InvalidBuild(String),
    #[error("invalid VM snapshot: {0}")]
    InvalidSnapshot(String),
    #[error("missing host import binding: {0}")]
    MissingImport(String),
    #[error("missing native object binding: {0}")]
    MissingNative(NativeKey),
    #[error("missing host operation: {0}")]
    MissingOperation(String),
    #[error("duplicate host operation: {0}")]
    DuplicateOperation(String),
    #[error("unknown host operation: {0}")]
    UnknownOperation(String),
    #[error("host call failed: {0}")]
    Host(String),
    #[error("invalid VM state: {0}")]
    InvalidState(String),
    #[error("script threw: {0:?}")]
    Thrown(Value),
}
