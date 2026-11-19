use std::collections::{BTreeMap, VecDeque};

use crate::bytecode::Build;
use crate::error::VmError;
use crate::frame::Frame;
use crate::heap::{Heap, PromiseState};
use crate::host::{Completion, Host};
use crate::runtime::{ExecOutcome, Invocation, Microtask, PendingOperation, Settlement};
use crate::value::{ContinuationId, HostValue, ObjectId, Value};

#[derive(Debug)]
pub struct Vm {
    pub(crate) build: Build,
    pub(crate) heap: Heap,
    pub(crate) continuations: BTreeMap<ContinuationId, crate::runtime::Continuation>,
    pub(crate) operations: BTreeMap<String, PendingOperation>,
    pub(crate) microtasks: VecDeque<Microtask>,
    pub(crate) globals: BTreeMap<String, Value>,
    next_continuation_id: ContinuationId,
}

impl Vm {
    pub fn new(build: Build, host: &dyn Host) -> Result<Self, VmError> {
        build.validate().map_err(VmError::InvalidBuild)?;
        Self::check_imports(&build, host)?;
        Ok(Self::empty(build))
    }

    fn empty(build: Build) -> Self {
        Self {
            build,
            heap: Heap::new(),
            continuations: BTreeMap::new(),
            operations: BTreeMap::new(),
            microtasks: VecDeque::new(),
            globals: BTreeMap::new(),
            next_continuation_id: 1,
        }
    }

    pub(crate) fn check_imports(build: &Build, host: &dyn Host) -> Result<(), VmError> {
        for import in &build.imports {
            if !host.has_import(&import.name) {
                return Err(VmError::MissingImport(import.name.clone()));
            }
        }
        Ok(())
    }

    pub fn build_id(&self) -> &str {
        &self.build.build_id
    }

    pub fn build(&self) -> &Build {
        &self.build
    }

    pub fn start(
        &mut self,
        host: &mut dyn Host,
        entry: &str,
        args: Vec<HostValue>,
    ) -> Result<Value, VmError> {
        let func_id = self
            .build
            .function_named(entry)
            .ok_or_else(|| VmError::InvalidBuild(format!("entry function {entry:?} not found")))?
            .func_id;
        let args = args
            .iter()
            .map(|value| self.host_to_value(value))
            .collect::<Result<Vec<_>, _>>()?;
        self.call_function(host, func_id, None, args)?.into_result()
    }

    /// Runs queued VM microtasks. Host timer/event delivery belongs to the
    /// host; it should call this after completing an operation.
    pub fn run_until_idle(&mut self, host: &mut dyn Host) -> Result<(), VmError> {
        while let Some(task) = self.microtasks.pop_front() {
            match task {
                Microtask::Resume {
                    continuation,
                    settlement,
                } => self.resume_continuation(host, continuation, settlement)?,
            }
        }
        Ok(())
    }

    pub fn complete_operation(
        &mut self,
        operation_id: &str,
        completion: Completion,
    ) -> Result<(), VmError> {
        if !self.operations.contains_key(operation_id) {
            return Err(VmError::UnknownOperation(operation_id.to_owned()));
        }
        let value = match &completion {
            Completion::Fulfilled(value) | Completion::Rejected(value) => {
                self.host_to_value(value)?
            }
        };
        let operation = self
            .operations
            .remove(operation_id)
            .ok_or_else(|| VmError::UnknownOperation(operation_id.to_owned()))?;
        let settlement = match completion {
            Completion::Fulfilled(_) => Settlement::Fulfilled(value),
            Completion::Rejected(_) => Settlement::Rejected(value),
        };
        self.settle(operation.promise, settlement)
    }

    pub fn pending_operation_ids(&self) -> Vec<String> {
        self.operations.keys().cloned().collect()
    }

    pub fn promise_state(&self, value: &Value) -> Result<PromiseState, VmError> {
        let Value::Object(id) = value else {
            return Err(VmError::InvalidState("value is not an object".to_owned()));
        };
        Ok(self.heap.promise(*id)?.state.clone())
    }

    pub fn to_host_value(&self, value: &Value) -> Result<HostValue, VmError> {
        match value {
            Value::Undefined => Ok(HostValue::Undefined),
            Value::Null => Ok(HostValue::Null),
            Value::Bool(value) => Ok(HostValue::Bool(*value)),
            Value::Number(value) => Ok(HostValue::Number(*value)),
            Value::String(value) => Ok(HostValue::String(value.clone())),
            Value::Object(id) => match self.heap.get(*id)? {
                crate::heap::Object::Native(native) => Ok(HostValue::Native(native.key.clone())),
                _ => Err(VmError::InvalidState(format!(
                    "object {id} cannot cross the host boundary"
                ))),
            },
        }
    }

    pub(crate) fn host_to_value(&mut self, value: &HostValue) -> Result<Value, VmError> {
        match value {
            HostValue::Undefined => Ok(Value::Undefined),
            HostValue::Null => Ok(Value::Null),
            HostValue::Bool(value) => Ok(Value::Bool(*value)),
            HostValue::Number(value) => {
                if !value.is_finite() {
                    return Err(VmError::Host(
                        "non-finite numbers are not supported by this PoC".to_owned(),
                    ));
                }
                Ok(Value::Number(*value))
            }
            HostValue::String(value) => Ok(Value::String(value.clone())),
            HostValue::Native(key) => Ok(Value::Object(self.heap.intern_native(key.clone()))),
        }
    }

    pub(crate) fn function_index(&self, func_id: u32) -> Result<usize, VmError> {
        self.build
            .functions
            .iter()
            .position(|function| function.func_id == func_id)
            .ok_or_else(|| VmError::InvalidState(format!("function {func_id} does not exist")))
    }

    pub(crate) fn alloc_continuation(&mut self) -> ContinuationId {
        let id = self.next_continuation_id;
        self.next_continuation_id = self
            .next_continuation_id
            .checked_add(1)
            .expect("continuation id space exhausted");
        id
    }

    pub(crate) fn enqueue(&mut self, continuation: ContinuationId, settlement: Settlement) {
        self.microtasks.push_back(Microtask::Resume {
            continuation,
            settlement,
        });
    }

    pub(crate) fn settle(
        &mut self,
        promise: ObjectId,
        settlement: Settlement,
    ) -> Result<(), VmError> {
        let reactions = {
            let promise_object = self.heap.promise_mut(promise)?;
            if !matches!(promise_object.state, PromiseState::Pending) {
                return Err(VmError::InvalidState(format!(
                    "Promise {promise} is already settled"
                )));
            }
            promise_object.state = settlement.to_state();
            std::mem::take(&mut promise_object.reactions)
        };
        for continuation in reactions {
            self.enqueue(continuation, settlement.clone());
        }
        Ok(())
    }

    pub(crate) fn settle_outcome(
        &mut self,
        promise: ObjectId,
        outcome: ExecOutcome,
    ) -> Result<(), VmError> {
        match outcome {
            ExecOutcome::Suspended => Ok(()),
            ExecOutcome::Return(value) => self.settle(promise, Settlement::Fulfilled(value)),
            ExecOutcome::Thrown(value) => self.settle(promise, Settlement::Rejected(value)),
        }
    }

    pub(crate) fn call_function(
        &mut self,
        host: &mut dyn Host,
        func_id: u32,
        parent_env: Option<ObjectId>,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let function_index = self.function_index(func_id)?;
        let is_async = self.build.functions[function_index].is_async;
        let param_bindings = self.build.functions[function_index].param_bindings.clone();
        let async_promise = is_async.then(|| self.heap.alloc_promise());

        let mut bindings = BTreeMap::new();
        for (index, binding) in param_bindings.into_iter().enumerate() {
            let value = args.get(index).cloned().unwrap_or(Value::Undefined);
            bindings.insert(binding, value);
        }
        let env = self.heap.alloc_environment(parent_env, bindings);
        let frame = Frame::new(
            function_index,
            &self.build.functions[function_index],
            Some(env),
            async_promise,
        );
        let outcome = self.execute_frame(host, frame)?;
        match async_promise {
            Some(promise) => {
                self.settle_outcome(promise, outcome)?;
                Ok(Invocation::Value(Value::Object(promise)))
            }
            None => match outcome {
                ExecOutcome::Suspended => Err(VmError::InvalidState(
                    "synchronous function suspended".to_owned(),
                )),
                ExecOutcome::Return(value) => Ok(Invocation::Value(value)),
                ExecOutcome::Thrown(value) => Ok(Invocation::Thrown(value)),
            },
        }
    }

    pub(crate) fn restored(
        build: Build,
        mut heap: Heap,
        continuations: BTreeMap<ContinuationId, crate::runtime::Continuation>,
        operations: BTreeMap<String, PendingOperation>,
        globals: BTreeMap<String, Value>,
    ) -> Self {
        heap.finish_restore();
        let next_continuation_id = continuations.keys().next_back().map_or(1, |id| id + 1);
        Self {
            build,
            heap,
            continuations,
            operations,
            microtasks: VecDeque::new(),
            globals,
            next_continuation_id,
        }
    }
}
