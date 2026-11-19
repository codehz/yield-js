use crate::builtins::BuiltinRoots;
use crate::bytecode::{Build, ParamTarget};
use crate::error::VmError;
use crate::frame::Frame;
use crate::heap::{Heap, PromiseState, ReactionJob};
use crate::host::{Completion, Host};
use crate::runtime::{ExecOutcome, Invocation, Microtask, PendingOperation, Settlement};
use crate::value::{ContinuationId, HostValue, ObjectId, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Debug)]
pub struct Vm {
    pub(crate) build: Build,
    pub(crate) heap: Heap,
    pub(crate) continuations: BTreeMap<ContinuationId, crate::runtime::Continuation>,
    pub(crate) operations: BTreeMap<String, PendingOperation>,
    pub(crate) microtasks: VecDeque<Microtask>,
    pub(crate) globals: BTreeMap<String, Value>,
    next_continuation_id: ContinuationId,
    /// Synchronous join recursion guard; never live at a snapshot boundary.
    pub(crate) active_array_joins: BTreeSet<ObjectId>,
    pub(crate) builtins: BuiltinRoots,
    /// Live environment of the script, present exactly when the script needs one.
    pub(crate) script_env: Option<ObjectId>,
}

impl Vm {
    pub fn new(build: Build, host: &mut dyn Host) -> Result<Self, VmError> {
        build.validate().map_err(VmError::InvalidBuild)?;
        Self::check_imports(&build, host)?;
        let mut vm = Self::empty(build);
        vm.init_builtins();
        vm.run_script(host)?;
        vm.run_until_idle(host)?;
        Ok(vm)
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
            active_array_joins: BTreeSet::new(),
            builtins: BuiltinRoots::default(),
            script_env: None,
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

    /// Reads the current value of a public module binding.
    pub fn get_export(&self, name: &str) -> Result<Value, VmError> {
        let binding = *self
            .build
            .exports
            .get(name)
            .ok_or_else(|| VmError::InvalidState(format!("unknown export {name:?}")))?;
        let env = self
            .script_env
            .ok_or_else(|| VmError::InvalidState("script has no environment".to_owned()))?;
        self.heap.get_binding(env, binding)
    }

    pub fn start(
        &mut self,
        host: &mut dyn Host,
        entry: &str,
        args: Vec<HostValue>,
    ) -> Result<Value, VmError> {
        let callee = self.get_export(entry)?;
        let args = args
            .iter()
            .map(|value| self.host_to_value(value))
            .collect::<Result<Vec<_>, _>>()?;
        self.invoke(host, &callee, args, &Value::Undefined)?
            .into_result()
    }
    fn run_script(&mut self, host: &mut dyn Host) -> Result<(), VmError> {
        let func_id = self.build.script;
        let index = self.function_index(func_id)?;
        let prepared = self.build.functions[index]
            .needs_environment()
            .then(|| self.heap.alloc_environment(None, BTreeMap::new()));
        self.script_env = prepared;
        self.activate(host, func_id, None, prepared, Vec::new(), &Value::Undefined)?
            .into_result()
            .map(|_| ())
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
                Microtask::CallHandler {
                    handler,
                    argument,
                    result,
                    rejected,
                } => self.run_call_handler(host, handler, argument, result, rejected)?,
                Microtask::Finalize {
                    result,
                    on_finally,
                    settlement,
                } => self.run_finalize(host, result, on_finally, settlement)?,
                Microtask::Assimilate {
                    result,
                    receiver,
                    then,
                } => self.run_assimilate(host, result, receiver, then)?,
                Microtask::BeginAdopt { result, source } => {
                    self.react(source, ReactionJob::Adopt { result })?
                }
                Microtask::Adopt { result, settlement } => {
                    self.run_adopt(host, result, settlement)?
                }
                Microtask::Combine {
                    result,
                    index,
                    kind,
                    settlement,
                } => self.run_combine(result, index, kind, settlement)?,
                Microtask::Call { function, args } => self.run_queue_call(host, function, args)?,
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
            Value::Symbol(_) => Err(VmError::InvalidState(
                "Symbol values cannot cross the host boundary".to_owned(),
            )),
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
        for job in reactions {
            self.enqueue_job(job, settlement.clone())?;
        }
        Ok(())
    }

    pub(crate) fn react(&mut self, promise: ObjectId, job: ReactionJob) -> Result<(), VmError> {
        let settlement = Settlement::from_state(&self.heap.promise(promise)?.state);
        match settlement {
            None => {
                self.heap.promise_mut(promise)?.reactions.push(job);
                Ok(())
            }
            Some(settlement) => self.enqueue_job(job, settlement),
        }
    }

    pub(crate) fn enqueue_job(
        &mut self,
        job: ReactionJob,
        settlement: Settlement,
    ) -> Result<(), VmError> {
        match job {
            ReactionJob::Await(continuation) => {
                self.enqueue(continuation, settlement);
            }
            ReactionJob::Script {
                result,
                on_fulfilled,
                on_rejected,
            } => {
                let (handler, argument, rejected) = match settlement {
                    Settlement::Fulfilled(value) => (on_fulfilled, value, false),
                    Settlement::Rejected(value) => (on_rejected, value, true),
                };
                self.microtasks.push_back(Microtask::CallHandler {
                    handler,
                    argument,
                    result,
                    rejected,
                });
            }
            ReactionJob::Finalize { result, on_finally } => {
                self.microtasks.push_back(Microtask::Finalize {
                    result,
                    on_finally,
                    settlement,
                });
            }
            ReactionJob::Adopt { result } => {
                self.microtasks
                    .push_back(Microtask::Adopt { result, settlement });
            }
            ReactionJob::Combine {
                result,
                index,
                kind,
            } => {
                self.microtasks.push_back(Microtask::Combine {
                    result,
                    index,
                    kind,
                    settlement,
                });
            }
        }
        Ok(())
    }

    pub(crate) fn settle_outcome(
        &mut self,
        host: &mut dyn Host,
        promise: ObjectId,
        outcome: ExecOutcome,
    ) -> Result<(), VmError> {
        match outcome {
            ExecOutcome::Suspended => Ok(()),
            ExecOutcome::Return(value) => self.resolve_promise(host, promise, value),
            ExecOutcome::Thrown(value) => self.settle(promise, Settlement::Rejected(value)),
        }
    }

    pub(crate) fn call_function(
        &mut self,
        host: &mut dyn Host,
        func_id: u32,
        parent_env: Option<ObjectId>,
        args: Vec<Value>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        self.activate(host, func_id, parent_env, None, args, receiver)
    }

    fn activate(
        &mut self,
        host: &mut dyn Host,
        func_id: u32,
        parent_env: Option<ObjectId>,
        prepared_env: Option<ObjectId>,
        args: Vec<Value>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let function_index = self.function_index(func_id)?;
        let is_async = self.build.functions[function_index].is_async;
        let params = self.build.functions[function_index].params.clone();
        let this_param = self.build.functions[function_index].this_param.clone();
        let async_promise = is_async.then(|| self.heap.alloc_promise());

        let mut bindings = BTreeMap::new();
        let mut slot_args = Vec::new();
        if let Some(param) = this_param {
            match param {
                ParamTarget::Binding(binding) => {
                    bindings.insert(binding, receiver.clone());
                }
                ParamTarget::Slot(slot) => slot_args.push((slot, receiver.clone())),
            }
        }
        for (index, param) in params.into_iter().enumerate() {
            let value = args.get(index).cloned().unwrap_or(Value::Undefined);
            match param {
                ParamTarget::Binding(binding) => {
                    bindings.insert(binding, value);
                }
                ParamTarget::Slot(slot) => slot_args.push((slot, value)),
            }
        }
        let needs_env = self.build.functions[function_index].needs_environment();
        let env = if needs_env {
            let env = match prepared_env {
                Some(env) => env,
                None => self.heap.alloc_environment(parent_env, BTreeMap::new()),
            };
            if !bindings.is_empty() {
                self.heap.environment_mut(env)?.bindings.extend(bindings);
            }
            Some(env)
        } else if prepared_env.is_some() {
            return Err(VmError::InvalidState(
                "prepared environment for a function that has none".to_owned(),
            ));
        } else {
            None
        };
        let mut frame = Frame::new(
            function_index,
            &self.build.functions[function_index],
            env,
            async_promise,
        );
        for (slot, value) in slot_args {
            frame.write(slot, value)?;
        }
        let outcome = self.execute_frame(host, frame)?;
        match async_promise {
            Some(promise) => {
                self.settle_outcome(host, promise, outcome)?;
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
        builtins: BuiltinRoots,
        script_env: Option<ObjectId>,
    ) -> Self {
        heap.promise_prototype = Some(builtins.promise_prototype);
        heap.object_prototype = Some(builtins.object_prototype);
        heap.array_prototype = Some(builtins.array_prototype);
        heap.function_prototype = Some(builtins.function_prototype);
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
            active_array_joins: BTreeSet::new(),
            builtins,
            script_env,
        }
    }
}
