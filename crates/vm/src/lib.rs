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
//! use yield_vm::{Decode, Encode, operation};
//! #[operation(name = "wait")]
//! #[derive(Encode, Decode)]
//! struct Wait;
//! ```
//!
//! Assemble VM resources with [`host!`], or use [`Host::default`] for a VM
//! with no external capabilities.
//! In `host!`, use `name: value` for a built-in platform service and
//! `Type => value` for a custom service. Only custom services participate in
//! `bind_service` and callback dependency resolution.

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
pub mod intrinsics;
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
    ClassCx, ClassId, ClassParent, ConstKind, ConstValue, Decode, Encode, ExportId, FromServices,
    GuestOutcome, HostClass, HostFault, HostModule, HostStep, Instance, JsError, JsResult, JsValue,
    MemberName, MethodId, ModuleCx, ModuleExports, NativeTarget, Operation, OperationDriver,
    OperationToken, ParentRef, Recovery, Registration, Services, SlotDecl, SuspendRequest, This,
    WellKnownSymbol,
};
pub use bytecode::{
    BUILD_FORMAT_VERSION, Build, BuildResource, Constant, ExceptionHandler, Function, Instr,
    ParamTarget, ResumeEntry, Slot, SlotKind,
};
pub use console::ConsoleEvent;
pub use error::VmError;
pub use fetch::{
    CredentialsMode, FetchEnvironment, FetchEvent, FetchFailure, FetchRegistration, FetchService,
    HopRequest, Origin, ResponseHead, UploadBody,
};
pub use heap::{Object, PromiseState};
pub use host::{
    Absent, Completion, ConsoleSink, CryptoService, Diagnostic, Diagnostics, DummyTimeService,
    GcRoots, Host, Platform, PlatformServices, SubtleCryptoCompletion, SubtleCryptoRequest,
    SubtleCryptoService, SystemTimeService, TimeService, TimerService, UserAgent,
};
pub use init::{RestoreOptions, SnapshotValidation, VmOptions};
pub use intrinsics::{IntrinsicClass, MessageErrorClass};
pub use regexp::{RegExpFlags, validate_regexp};
#[doc(hidden)]
pub use runtime::TestClock;
pub use snapshot::{SNAPSHOT_BINARY_VERSION, SNAPSHOT_FORMAT_VERSION, Snapshot};
pub use value::{BindingId, ContinuationId, JsBigInt, JsString, ObjectId, Value, ValueRef};
pub use vm::{PendingOperationInfo, VerifiedBuild, Vm};
pub use yield_binding_macros::{
    Decode, Encode, host, host_class, host_module, methods, module_exports, operation,
};
