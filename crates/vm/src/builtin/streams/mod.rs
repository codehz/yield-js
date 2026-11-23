//! Web IDL installation and argument conversion shared by the stream interfaces.

mod readable;
pub(super) use readable::{CONTROLLER_OWNER, READER_OWNER, STREAM_OWNER};
mod strategy;
mod validate;
pub(crate) use validate::{validate_job, validate_record};

pub(super) use strategy::{BYTE_LENGTH_OWNER, COUNT_OWNER};

use crate::VmError;
use crate::builtin::{define_builtin_data, link_constructor};
use crate::gc::ActiveRoots;
use crate::heap::{Attributes, TO_STRING_TAG_SYMBOL};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, Slot};
use crate::runtime::Invocation;
use crate::value::{PropertyKey, PropertyKeyRef, Value};
use crate::vm::Vm;

use crate::heap::ReactionJob;
use crate::runtime::Settlement;
use crate::value::ObjectId;
use serde::{Deserialize, Serialize};

/// The target's heap record owns all durable state outside the job identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamJob {
    ReadableStart(ObjectId),
    ReadablePull(ObjectId),
    ReadableCancel(ObjectId),
}

impl StreamJob {
    pub(crate) fn target(self) -> ObjectId {
        match self {
            Self::ReadableStart(id) | Self::ReadablePull(id) | Self::ReadableCancel(id) => id,
        }
    }
}

impl Vm {
    fn observe_stream_job(&mut self, promise: ObjectId, job: StreamJob) -> Result<(), VmError> {
        self.react(promise, ReactionJob::Streams(job))
    }

    fn stream_callback_promise(
        &mut self,
        host: &mut dyn Host,
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

fn install(
    vm: &mut Vm,
    name: &str,
    root: &'static str,
    prototype: &'static str,
    members: &[IntrinsicDef],
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

fn dictionary_member(
    vm: &mut Vm,
    host: &mut dyn Host,
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
