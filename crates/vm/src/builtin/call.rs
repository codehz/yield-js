//! Shared invocation and object-model helpers behind the `Vm` impl. Dispatch and
//! constructor routing types live here; each submodule owns one concern.

// Reserved keys for the opaque, shared derived invocation cell.
pub(crate) const DERIVED_CLASS: u32 = u32::MAX;
pub(crate) const DERIVED_TARGET: u32 = u32::MAX - 1;
pub(crate) const DERIVED_THIS: u32 = u32::MAX - 2;

use crate::error::VmError;
use crate::gc::{TraceRoots, Tracer};
use crate::heap::{BoundObject, DescriptorUpdate, Object, Property};
use crate::intrinsic::CallableTarget;
use crate::runtime::Invocation;
use crate::value::{ObjectId, Value};

static UNDEFINED_VALUE: Value = Value::UNDEFINED;

pub(crate) enum Dispatch<'a> {
    Script {
        func_id: u32,
        env: ObjectId,
        callee: ObjectId,
        home: Option<ObjectId>,
        class: bool,
    },
    Resolution(ObjectId),
    PromiseCallback(ObjectId),
    ProxyRevoker(ObjectId),
    Bound(&'a BoundObject),
    Native {
        target: CallableTarget,
        constructable: bool,
    },
}

pub(crate) enum Constructor {
    Script { func_id: u32, env: ObjectId },
    Native(CallableTarget),
}

mod descriptors;
mod error_alloc;
mod errors;
mod invoke;
mod iterator;
mod object_statics;
mod promise_combine;
mod promise_runtime;
mod symbol;
mod tostring;

pub(crate) fn is_callable_object(object: &Object) -> bool {
    matches!(
        object,
        Object::Function(_)
            | Object::Resolution(_)
            | Object::Bound(_)
            | Object::PromiseCallback { .. }
            | Object::ProxyRevoker { .. }
            | Object::Native(_)
    )
}

impl TraceRoots for DescriptorUpdate {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError> {
        (&self.value, self.get, self.set).trace(tracer)
    }
}

impl TraceRoots for Property {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError> {
        match self {
            Property::Data(value) => value.trace(tracer),
            Property::Accessor { get, set } => (*get, *set).trace(tracer),
        }
    }
}

impl TraceRoots for Invocation {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError> {
        match self {
            Invocation::Value(value) | Invocation::Thrown(value) => value.trace(tracer),
        }
    }
}
