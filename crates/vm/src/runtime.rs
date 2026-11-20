//! Suspended work. A script exception inside an async activation is a
//! [`Settlement`], not a VM failure. [`Invocation`] is the same split for a call.

use std::any::Any;

use crate::binding::{ClassId, Recovery};
use crate::error::VmError;
use crate::heap::{CombineKind, PromiseState};
use crate::value::{ContinuationId, ObjectId, Value};

#[derive(Debug, Clone)]
pub(crate) enum HostSlot {
    Js(Value),
    JsList(Vec<Value>),
}

pub(crate) struct HostInstance {
    pub(crate) class: ClassId,
    pub(crate) slots: Vec<HostSlot>,
    pub(crate) payload: Option<Box<dyn Any>>,
}

impl std::fmt::Debug for HostInstance {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostInstance")
            .field("class", &self.class)
            .field("slots", &self.slots)
            .finish_non_exhaustive()
    }
}

pub(crate) struct ModuleState {
    pub(crate) slots: Vec<HostSlot>,
    pub(crate) payload: Box<dyn Any>,
}

impl std::fmt::Debug for ModuleState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModuleState")
            .field("slots", &self.slots)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub(crate) struct PendingOperation {
    pub(crate) name: String,
    pub(crate) schema: u32,
    pub(crate) recovery: Recovery,
    pub(crate) rejection: &'static str,
    pub(crate) state: Vec<u8>,
    pub(crate) promise: ObjectId,
}

#[derive(Debug, Clone)]
pub(crate) struct Continuation {
    pub(crate) async_promise: ObjectId,
    pub(crate) func_id: u32,
    pub(crate) resume_id: u32,
    pub(crate) waiting_on: Option<ObjectId>,
    pub(crate) env: Option<ObjectId>,
    pub(crate) values: Vec<Value>,
}

impl Continuation {
    pub(crate) fn for_each_object(&self, mut visit: impl FnMut(ObjectId)) {
        visit(self.async_promise);
        if let Some(id) = self.waiting_on {
            visit(id);
        }
        if let Some(id) = self.env {
            visit(id);
        }
        for value in &self.values {
            if let Value::Object(id) = value {
                visit(*id);
            }
        }
    }
}

/// Result of a call. Script throws stay here so callers can transfer them;
/// [`VmError`] is reserved for broken VM or host state.
#[derive(Debug)]
pub(crate) enum Invocation {
    Value(Value),
    Thrown(Value),
}

impl Invocation {
    pub(crate) fn into_result(self) -> Result<Value, VmError> {
        match self {
            Self::Value(value) => Ok(value),
            Self::Thrown(value) => Err(VmError::Thrown(value)),
        }
    }
}

/// How one activation finished. The async result Promise is settled from this,
/// whether the activation was a call or a resume.
#[derive(Debug)]
pub(crate) enum ExecOutcome {
    Return(Value),
    Suspended,
    Thrown(Value),
}

/// A settled async result. Pending is the absence of a settlement.
#[derive(Debug, Clone)]
pub(crate) enum Settlement {
    Fulfilled(Value),
    Rejected(Value),
}

impl Settlement {
    pub(crate) fn from_state(state: &PromiseState) -> Option<Self> {
        match state {
            PromiseState::Pending => None,
            PromiseState::Fulfilled(value) => Some(Self::Fulfilled(value.clone())),
            PromiseState::Rejected(value) => Some(Self::Rejected(value.clone())),
        }
    }

    pub(crate) fn to_state(&self) -> PromiseState {
        match self {
            Self::Fulfilled(value) => PromiseState::Fulfilled(value.clone()),
            Self::Rejected(value) => PromiseState::Rejected(value.clone()),
        }
    }
}

#[derive(Debug)]
pub(crate) enum Microtask {
    Resume {
        continuation: ContinuationId,
        settlement: Settlement,
    },
    /// Run one `then` / `catch` handler, or pass the settlement through.
    CallHandler {
        handler: Option<Value>,
        argument: Value,
        capability: (Value, Value),
        rejected: bool,
    },
    Assimilate {
        result: ObjectId,
        receiver: Value,
        then: Value,
    },
    BeginAdopt {
        result: ObjectId,
        source: ObjectId,
    },
    Adopt {
        result: ObjectId,
        settlement: Settlement,
    },
    Combine {
        result: ObjectId,
        index: usize,
        kind: CombineKind,
        settlement: Settlement,
    },
    /// `queueMicrotask`. A throw has no result promise and is not reported:
    /// this slice does not track unhandled rejections.
    Call {
        function: Value,
        args: Vec<Value>,
    },
}
