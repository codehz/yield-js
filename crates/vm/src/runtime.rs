//! Suspended work. A script exception inside an async activation is a
//! [`Settlement`], not a VM failure. [`Invocation`] is the same split for a call.

use std::any::Any;

use crate::binding::{ClassId, HostFault, LiveRegistration, NativeTarget, Recovery};
use crate::error::VmError;
use crate::gc::{TraceRoots, Tracer};
use crate::heap::PromiseState;
use crate::snapshot::DeliveryTarget;
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

pub(crate) struct PendingOperation {
    pub(crate) name: String,
    pub(crate) schema: u32,
    pub(crate) recovery: Recovery,
    pub(crate) rejection: &'static str,
    pub(crate) state: Vec<u8>,
    pub(crate) target: DeliveryTarget,
    pub(crate) declaration: usize,
    pub(crate) live: Option<Box<dyn LiveRegistration>>,
}

impl PendingOperation {
    pub(crate) fn checkpoint(&self) -> Result<Vec<u8>, HostFault> {
        let Some(live) = &self.live else {
            return Ok(self.state.clone());
        };
        let mut state = Vec::new();
        live.checkpoint(&self.state, &mut state)?;
        Ok(state)
    }
}

impl std::fmt::Debug for PendingOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingOperation")
            .field("name", &self.name)
            .field("schema", &self.schema)
            .field("recovery", &self.recovery)
            .field("target", &self.target)
            .field("armed", &self.live.is_some())
            .finish_non_exhaustive()
    }
}

pub(crate) const HOST_TASK_ID_LIMIT: u64 = 1 << 53;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostTaskState {
    Waiting(u64),
    Ready,
    Running,
}

#[derive(Debug)]
pub(crate) struct HostTask {
    pub(crate) owner: NativeTarget,
    pub(crate) stage: u32,
    pub(crate) receiver: Value,
    pub(crate) saved: Vec<Value>,
    pub(crate) state: HostTaskState,
}

impl TraceRoots for HostTask {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError> {
        (&self.receiver, &self.saved).trace(tracer)
    }
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
    pub(crate) fn linked_objects(&self) -> impl Iterator<Item = ObjectId> {
        [Some(self.async_promise), self.waiting_on, self.env]
            .into_iter()
            .flatten()
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
    /// A synchronous generator suspended. `delegate` means `value` is already
    /// the delegate's own result object and is handed back unwrapped.
    Yielded {
        value: Value,
        delegate: bool,
    },
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
    ResumeHost {
        task: u64,
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
    /// `queueMicrotask`. A throw is reported separately from Promise rejections.
    Call {
        function: Value,
        args: Vec<Value>,
    },
}

impl TraceRoots for Settlement {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError> {
        match self {
            Self::Fulfilled(value) | Self::Rejected(value) => value.trace(tracer),
        }
    }
}

impl TraceRoots for Microtask {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError> {
        match self {
            Self::Resume {
                continuation,
                settlement,
            } => {
                tracer.mark_pending_continuation(*continuation)?;
                settlement.trace(tracer)
            }
            Self::ResumeHost { settlement, .. } => settlement.trace(tracer),
            Self::CallHandler {
                handler,
                argument,
                capability,
                rejected: _,
            } => (handler, argument, capability).trace(tracer),
            Self::Assimilate {
                result,
                receiver,
                then,
            } => (result, receiver, then).trace(tracer),
            Self::BeginAdopt { result, source } => (result, source).trace(tracer),
            Self::Adopt { result, settlement } => (result, settlement).trace(tracer),
            Self::Call { function, args } => (function, args).trace(tracer),
        }
    }
}
