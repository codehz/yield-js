//! Resumable mini JavaScript VM proof of concept.
//!
//! The VM consumes only a validated bytecode build. It never parses source,
//! imports the compiler, or stores host-native pointers in snapshots.

mod builtins;
pub mod bytecode;
pub mod error;
mod exec;
mod frame;
pub mod heap;
pub mod host;
mod ops;
mod resolve;
mod runtime;
pub mod snapshot;
pub mod value;
mod vm;

pub use bytecode::{
    Build, Constant, ExceptionHandler, Function, Instr, ParamTarget, ResumeEntry, Slot, SlotKind,
};
pub use error::VmError;
pub use heap::{Object, PromiseState};
pub use host::{Completion, Host, HostReply, PendingOperationMeta};
pub use snapshot::{SNAPSHOT_FORMAT_VERSION, Snapshot};
pub use value::{BindingId, ContinuationId, HostValue, NativeKey, ObjectId, Value};
pub use vm::Vm;
