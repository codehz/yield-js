//! Web IDL installation and argument conversion shared by the stream interfaces.

pub(crate) mod compression;
pub(crate) mod iteration;
pub(crate) mod pipe;
pub(super) mod readable;
pub(super) mod strategy;
pub(crate) mod tee;
pub(super) mod text;
pub(crate) mod transfer;
pub(super) mod transform;
mod validate;
pub(super) mod writable;
pub(crate) use validate::{validate_composition, validate_job, validate_record};

use crate::VmError;
use crate::builtin::{define_builtin_data, link_constructor};
use crate::gc::ActiveRoots;
use crate::heap::{Attributes, TO_STRING_TAG_SYMBOL};
use crate::host::{Host, PlatformServices};
use crate::intrinsic::{IntrinsicDef, Slot};
use crate::runtime::Invocation;
use crate::value::{PropertyKey, PropertyKeyRef, Value};
use crate::vm::VmCore;

use crate::heap::ReactionJob;
use crate::runtime::Settlement;
use crate::value::ObjectId;
use serde::{Deserialize, Serialize};

/// The target's heap record owns all durable state outside the job identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamJob {
    Transfer(ObjectId, transfer::TransferStage),
    ReadableStart(ObjectId),
    ReadablePull(ObjectId),
    ReadableCancel(ObjectId),
    WritableStart(ObjectId),
    WritableFlight(ObjectId),
    Transform(ObjectId, crate::heap::TransformStage),
    Tee(ObjectId, tee::TeeJob),
    Pipe {
        target: ObjectId,
        stage: pipe::PipeStage,
    },
    Iteration {
        target: ObjectId,
        stage: iteration::IterationStage,
    },
}

impl StreamJob {
    pub(crate) fn target(self) -> ObjectId {
        match self {
            Self::ReadableStart(id)
            | Self::Transfer(id, _)
            | Self::ReadablePull(id)
            | Self::ReadableCancel(id)
            | Self::WritableStart(id)
            | Self::WritableFlight(id)
            | Self::Transform(id, _)
            | Self::Tee(id, _)
            | Self::Pipe { target: id, .. }
            | Self::Iteration { target: id, .. } => id,
        }
    }
}

impl VmCore {
    pub(crate) fn resume_stream_job<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        job: StreamJob,
        settlement: &Settlement,
    ) -> Result<(), VmError> {
        match job {
            StreamJob::WritableStart(_) | StreamJob::WritableFlight(_) => {
                self.resume_writable_job(host, roots, job, settlement)
            }
            StreamJob::Transform(id, stage) => {
                self.resume_transform_job(host, roots, id, stage, settlement)
            }
            StreamJob::Tee(id, stage) => self.resume_tee_job(host, roots, id, stage, settlement),
            StreamJob::Transfer(id, stage) => {
                self.resume_transfer(host, roots, id, stage, settlement)
            }
            StreamJob::Pipe { target, stage } => {
                self.run_pipe_job(host, roots, target, stage, settlement)
            }
            StreamJob::Iteration { target, stage } => {
                self.resume_iteration_job(host, roots, target, stage, settlement)
            }
            _ => self.resume_readable_job(host, roots, job, settlement),
        }
    }

    fn observe_stream_job(&mut self, promise: ObjectId, job: StreamJob) -> Result<(), VmError> {
        self.react(promise, ReactionJob::Streams(job))
    }

    fn stream_callback_promise<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        value: Value,
    ) -> Result<ObjectId, VmError> {
        let roots = &roots.with(&value);
        match self.promise_identity(host, roots, &value)? {
            Ok(Some(id)) => Ok(id),
            Ok(None) => {
                let promise = self.heap.alloc_promise();
                self.resolve_promise(host, roots, promise, value.clone())?;
                Ok(promise)
            }
            Err(error) => {
                let promise = self.heap.alloc_promise();
                self.settle(promise, Settlement::Rejected(error))?;
                Ok(promise)
            }
        }
    }
}

fn install<P: PlatformServices>(
    vm: &mut VmCore,
    name: &str,
    root: &'static str,
    prototype: &'static str,
    members: &[IntrinsicDef<P>],
) {
    let constructor = vm.builtins.get(root);
    let prototype = vm.builtins.get(prototype);
    link_constructor(vm, constructor, prototype);
    for def in members {
        let key = def.key;
        match key.slot {
            Slot::Static | Slot::Prototype => {
                let function = vm.alloc_intrinsic(key);
                let target = if key.slot == Slot::Static {
                    constructor
                } else {
                    prototype
                };
                define_builtin_data(
                    vm,
                    target,
                    key.member,
                    Value::object(function),
                    Attributes::ORDINARY,
                );
            }
            Slot::PrototypeGetter => {
                let getter = vm.alloc_intrinsic(key);
                super::event::install_accessor(vm, prototype, key.member, getter, None, true)
                    .expect("stream interface attribute");
            }
            _ => {}
        }
    }
    define_builtin_data(
        vm,
        prototype,
        PropertyKey::Symbol(TO_STRING_TAG_SYMBOL),
        Value::string(name.into()),
        Attributes::FUNCTION_METADATA,
    );
}

fn dictionary_member<P: PlatformServices>(
    vm: &mut VmCore,
    host: &Host<P>,
    roots: &ActiveRoots<'_>,
    dictionary: &Value,
    member: &str,
) -> Result<Invocation, VmError> {
    use crate::value::ValueRef;
    match dictionary.view() {
        ValueRef::Null | ValueRef::Undefined => Ok(Invocation::Value(Value::UNDEFINED)),
        ValueRef::Object(_) => {
            vm.read_property(host, roots, dictionary, PropertyKeyRef::String(member))
        }
        _ => vm.throw_type_error(roots, "Options must be an object"),
    }
}
