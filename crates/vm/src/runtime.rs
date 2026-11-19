//! Suspended work. A script exception inside an async activation is a
//! [`Settlement`], not a VM failure. [`Invocation`] is the same split for a call.

use crate::error::VmError;
use crate::heap::{CombineKind, PromiseState};
use crate::host::PendingOperationMeta;
use crate::value::{ContinuationId, ObjectId, Value};

#[derive(Debug, Clone)]
pub(crate) struct PendingOperation {
    pub(crate) meta: PendingOperationMeta,
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
        result: ObjectId,
        rejected: bool,
    },
    /// Run `finally`, then reapply `settlement` unless the callback rejects
    /// or returns a thenable that rejects.
    Finalize {
        result: ObjectId,
        on_finally: Value,
        settlement: Settlement,
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
    Call { function: Value, args: Vec<Value> },
}
