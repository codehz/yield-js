//! Resumable mini JavaScript VM proof of concept.
//!
//! The VM consumes only a validated bytecode build. It never parses source,
//! imports the compiler, or stores host-native pointers in snapshots.
//!
//! A prototype member takes a typed `This` for the declared class:
//!
//! ```compile_fail
//! use yield_vm::{JsResult, JsValue, host_class, methods};
//! use yield_vm::binding::CallCx;
//! #[host_class(tag = "Wrong", methods)]
//! struct Wrong;
//! #[methods(Wrong)]
//! impl Wrong {
//!     #[method(name = "value")]
//!     fn value(_: &mut CallCx<'_>, _: JsValue) -> JsResult<()> { Ok(()) }
//! }
//! ```
//!
//! An accessor can only declare one getter for a given key:
//!
//! ```compile_fail
//! use yield_vm::{JsResult, JsValue, This, host_class, methods};
//! use yield_vm::binding::CallCx;
//! #[host_class(tag = "Duplicate", methods)]
//! struct Duplicate;
//! #[methods(Duplicate)]
//! impl Duplicate {
//!     #[get(name = "value")]
//!     fn first(_: &mut CallCx<'_>, _: This<Duplicate>) -> JsResult<JsValue> {
//!         Ok(JsValue::undefined())
//!     }
//!     #[get(name = "value")]
//!     fn second(_: &mut CallCx<'_>, _: This<Duplicate>) -> JsResult<JsValue> {
//!         Ok(JsValue::undefined())
//!     }
//! }
//! ```
//!
//! Operations must declare a recovery strategy:
//!
//! ```compile_fail
//! use yield_vm::{Bindings, operation};
//! use yield_vm::binding::CodecError;
//! #[operation(name = "wait", schema = 1, encode = write, decode = read)]
//! struct Wait;
//! fn write(_: &Wait, _: &mut Vec<u8>) -> Result<(), CodecError> { Ok(()) }
//! fn read(_: &[u8], _: &Bindings) -> Result<Wait, CodecError> { Ok(Wait) }
//! ```
//!
//! Register bindings with [`host_bindings!`]; there is no public builder:
//!
//! ```compile_fail,E0599
//! let _ = yield_vm::Bindings::builder();
//! ```

extern crate self as yield_vm;

pub mod binding;
mod builtin;
use builtin as builtins;
pub mod bytecode;
pub mod console;
mod digest;
pub mod error;
mod exec;
pub mod fetch;
mod frame;
mod gc;
pub mod heap;
pub mod host;
mod init;
mod intrinsic;
mod ops;
#[doc(hidden)]
pub mod perf;
mod private;
mod regexp;
mod resolve;
mod runtime;
pub mod snapshot;
pub mod value;
mod vm;

#[doc(hidden)]
pub use binding::__macro_support;

pub use binding::{
    Bindings, ClassId, ConstKind, ConstValue, ExportId, GuestOutcome, HostClass, HostFault,
    HostModule, HostModuleState, HostStep, Instance, JsError, JsResult, JsValue, MemberName,
    MethodId, ModuleCx, ModuleExports, NativeTarget, Operation, OperationDriver, OperationToken,
    Recovery, Registration, ServiceKey, Services, SlotDecl, SuspendRequest, This, WellKnownSymbol,
};
pub use bytecode::{
    BUILD_FORMAT_VERSION, Build, Constant, ExceptionHandler, Function, Instr, ParamTarget,
    ResumeEntry, Slot, SlotKind,
};
pub use console::ConsoleEvent;
pub use error::VmError;
pub use fetch::{
    CredentialsMode, FETCH_PROTOCOL_VERSION, FetchEnvironment, FetchEvent, FetchFailure, FetchKey,
    FetchRegistration, FetchService, HopRequest, Origin, ResponseHead, UploadBody,
};
pub use heap::{Object, PromiseState};
pub use host::{
    CRYPTO_SUBTLE_PROTOCOL_VERSION, Completion, ConsoleKey, ConsoleSink, CryptoKey, CryptoService,
    DummyTimeService, Host, SubtleCryptoKey, SubtleCryptoRequest, SubtleCryptoService,
    SystemTimeService, TimeKey, TimeService, TimerKey,
};
pub use init::{RestoreOptions, SnapshotValidation, VmOptions};
pub use regexp::{RegExpFlags, validate_regexp};
#[doc(hidden)]
pub use runtime::TestClock;
pub use snapshot::{SNAPSHOT_BINARY_VERSION, SNAPSHOT_FORMAT_VERSION, Snapshot};
pub use value::{BindingId, ContinuationId, JsBigInt, JsString, ObjectId, Value, ValueRef};
pub use vm::{PendingOperationInfo, VerifiedBuild, Vm};
pub use yield_binding_macros::{
    host_bindings, host_class, host_module, methods, module_exports, operation,
};
