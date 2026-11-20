use crate::JsString;
use crate::builtins::BuiltinRoots;
use crate::bytecode::{Build, ParamTarget};
use crate::error::VmError;
use crate::frame::Frame;
use crate::heap::{Heap, Object, PromiseState, ReactionJob};
use crate::host::{Completion, Host, HostReply};
use crate::runtime::{ExecOutcome, Invocation, Microtask, PendingOperation, Settlement};
use crate::value::{ContinuationId, HostValue, ObjectId, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Debug, Clone)]
pub(crate) struct ImportModule {
    pub(crate) module: String,
    pub(crate) namespace: ObjectId,
    pub(crate) values: Vec<Value>,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedImports {
    pub(crate) modules: Vec<ResolvedModule>,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedModule {
    pub(crate) module: String,
    pub(crate) exports: Vec<(String, crate::host::HostExport)>,
}

/// Module namespace exports are visible to enumeration but immutable.
const IMPORT_EXPORT_ATTRIBUTES: crate::heap::Attributes = crate::heap::Attributes {
    writable: false,
    enumerable: true,
    configurable: false,
};

#[derive(Debug)]
pub struct Vm {
    pub(crate) build: Build,
    pub(crate) heap: Heap,
    pub(crate) continuations: BTreeMap<ContinuationId, crate::runtime::Continuation>,
    pub(crate) operations: BTreeMap<String, PendingOperation>,
    pub(crate) microtasks: VecDeque<Microtask>,
    pub(crate) globals: BTreeMap<JsString, Value>,
    pub(crate) imports: Vec<ImportModule>,
    next_continuation_id: ContinuationId,
    /// Synchronous join recursion guard; never live at a snapshot boundary.
    pub(crate) active_array_joins: BTreeSet<ObjectId>,
    pub(crate) builtins: BuiltinRoots,
    pub(crate) random_state: u64,
    /// Live environment of the script, present exactly when the script needs one.
    pub(crate) script_env: Option<ObjectId>,
}

impl Vm {
    pub fn new(build: Build, host: &mut dyn Host) -> Result<Self, VmError> {
        build.validate().map_err(VmError::InvalidBuild)?;
        let resolved = Self::resolve_imports(&build, host)?;
        let mut vm = Self::empty(build);
        vm.init_builtins();
        vm.install_imports(resolved)?;
        vm.run_script(host)?;
        vm.run_until_idle(host)?;
        Ok(vm)
    }

    fn empty(build: Build) -> Self {
        Self {
            build,
            heap: Heap::new(),
            continuations: BTreeMap::new(),
            imports: Vec::new(),
            operations: BTreeMap::new(),
            microtasks: VecDeque::new(),
            globals: BTreeMap::new(),
            next_continuation_id: 1,
            active_array_joins: BTreeSet::new(),
            builtins: BuiltinRoots::default(),
            random_state: crate::math::RANDOM_SEED,
            script_env: None,
        }
    }

    pub(crate) fn resolve_imports(
        build: &Build,
        host: &dyn Host,
    ) -> Result<ResolvedImports, VmError> {
        let mut modules = Vec::with_capacity(build.imports.len());
        for (module, exports) in &build.imports {
            let mut resolved = Vec::with_capacity(exports.len());
            for export in exports {
                let value = host
                    .resolve_export(module, export)
                    .ok_or_else(|| VmError::MissingImport(format!("{module}:{export}")))?;
                resolved.push((export.clone(), value));
            }
            modules.push(ResolvedModule {
                module: module.clone(),
                exports: resolved,
            });
        }
        Ok(ResolvedImports { modules })
    }

    fn is_module_export(&self, key: &crate::value::NativeKey) -> bool {
        self.build
            .imports
            .get(&key.type_name)
            .is_some_and(|exports| exports.contains(&key.id))
    }

    pub(crate) fn call_native(
        &mut self,
        host: &mut dyn Host,
        key: &crate::value::NativeKey,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let is_export = self.is_module_export(key);
        let project = is_export && host.project_arguments(&key.type_name, &key.id)
            || host.project_native_arguments(key);
        let host_args = args
            .iter()
            .map(|arg| {
                if project {
                    self.project_argument(arg)
                } else {
                    self.to_host_value(arg)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let reply = if is_export {
            host.call(&key.type_name, &key.id, &host_args)
                .map_err(VmError::Host)?
        } else {
            match host.call_native(key, &host_args) {
                Ok(reply) => reply,
                Err(crate::host::HostException(value)) => {
                    return Ok(Invocation::Thrown(self.host_to_value(&value)?));
                }
            }
        };
        match reply {
            HostReply::Value(value) => Ok(Invocation::Value(self.host_to_value(&value)?)),
            HostReply::Throw(message) => Ok(Invocation::Thrown(Value::String(message.into()))),
            HostReply::Operation { operation_id } => {
                if !is_export {
                    return Err(VmError::Host(
                        "host operation requires a module export".to_owned(),
                    ));
                }
                Ok(Invocation::Value(self.track_operation(
                    key.type_name.clone(),
                    key.id.clone(),
                    operation_id,
                )?))
            }
        }
    }

    fn import_export_value(
        &mut self,
        module: &str,
        name: &str,
        export: crate::host::HostExport,
    ) -> Result<Value, VmError> {
        match export {
            crate::host::HostExport::Value(value) => self.host_to_value(&value),
            crate::host::HostExport::Function => Ok(Value::Object(self.heap.intern_native(
                crate::value::NativeKey {
                    type_name: module.to_owned(),
                    id: name.to_owned(),
                },
                true,
            ))),
        }
    }

    fn install_imports(&mut self, resolved: ResolvedImports) -> Result<(), VmError> {
        for module in resolved.modules {
            let namespace = self.heap.alloc_plain_with(None);
            let mut values = Vec::with_capacity(module.exports.len());
            for (name, export) in module.exports {
                let value = self.import_export_value(&module.module, &name, export)?;
                self.heap.define_data(
                    namespace,
                    name.as_str(),
                    value.clone(),
                    IMPORT_EXPORT_ATTRIBUTES,
                )?;
                values.push(value);
            }
            self.heap.get_mut(namespace)?.prevent_extensions();
            self.imports.push(ImportModule {
                module: module.module,
                namespace,
                values,
            });
        }
        Ok(())
    }

    pub(crate) fn restore_imports(
        &mut self,
        namespaces: Vec<ObjectId>,
        resolved: ResolvedImports,
    ) -> Result<(), VmError> {
        if namespaces.len() != resolved.modules.len() {
            return Err(VmError::InvalidSnapshot(
                "import namespace count mismatch".to_owned(),
            ));
        }
        for (namespace, module) in namespaces.into_iter().zip(resolved.modules) {
            let object = self.heap.get(namespace)?;
            if !matches!(object, Object::Plain(_))
                || object.prototype().is_some()
                || object.is_extensible()
            {
                return Err(VmError::InvalidSnapshot(format!(
                    "import namespace {} is not frozen with a null prototype",
                    module.module
                )));
            }
            let mut actual = BTreeMap::new();
            for key in object.own_keys(false) {
                let crate::value::PropertyKey::String(name) = key else {
                    return Err(VmError::InvalidSnapshot(format!(
                        "import namespace {} has a symbol key",
                        module.module
                    )));
                };
                let Some(property) =
                    object.own_property(crate::value::PropertyKeyRef::JsString(&name))
                else {
                    return Err(VmError::InvalidSnapshot(format!(
                        "import namespace {} lost key {:?}",
                        module.module, name
                    )));
                };
                if property.attributes() != IMPORT_EXPORT_ATTRIBUTES {
                    return Err(VmError::InvalidSnapshot(format!(
                        "import namespace {} export {name:?} is not frozen",
                        module.module
                    )));
                }
                let Some(value) = property.data_value().cloned() else {
                    return Err(VmError::InvalidSnapshot(format!(
                        "import namespace {} export {name:?} is not a data property",
                        module.module
                    )));
                };
                if actual.insert(name, value).is_some() {
                    return Err(VmError::InvalidSnapshot(format!(
                        "import namespace {} has a duplicate key",
                        module.module
                    )));
                }
            }
            if actual.len() != module.exports.len() {
                return Err(VmError::InvalidSnapshot(format!(
                    "import namespace {} does not have exactly the resolved exports",
                    module.module
                )));
            }
            let mut values = Vec::with_capacity(module.exports.len());
            for (name, export) in module.exports {
                let expected = self.import_export_value(&module.module, &name, export)?;
                let key = JsString::from(name.as_str());
                let Some(value) = actual.get(&key) else {
                    return Err(VmError::InvalidSnapshot(format!(
                        "import namespace {} is missing export {name:?}",
                        module.module
                    )));
                };
                if value != &expected {
                    return Err(VmError::InvalidSnapshot(format!(
                        "import namespace {} export {name:?} does not match the host",
                        module.module
                    )));
                }
                values.push(value.clone());
            }
            self.imports.push(ImportModule {
                module: module.module,
                namespace,
                values,
            });
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
                    capability,
                    rejected,
                } => self.run_call_handler(host, handler, argument, capability, rejected)?,
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
                crate::heap::Object::Native(native) => Ok(if native.function {
                    HostValue::NativeFunction(native.key.clone())
                } else {
                    HostValue::Native(native.key.clone())
                }),
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
            HostValue::Native(key) => {
                Ok(Value::Object(self.heap.intern_native(key.clone(), false)))
            }
            HostValue::NativeFunction(key) => {
                Ok(Value::Object(self.heap.intern_native(key.clone(), true)))
            }
            HostValue::Data(_) => Err(VmError::Host(
                "host data projections cannot be converted into script values".to_owned(),
            )),
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
                resolve,
                reject,
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
                    capability: (resolve, reject),
                    rejected,
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
        let rest_param = self.build.functions[function_index].rest_param.clone();
        let this_param = self.build.functions[function_index].this_param.clone();
        let arguments_param = self.build.functions[function_index].arguments_param.clone();
        let async_promise = is_async.then(|| self.heap.alloc_promise());

        let mut bindings = BTreeMap::new();
        let mut slot_args = Vec::new();
        let mut bind = |param, value| match param {
            ParamTarget::Binding(binding) => {
                bindings.insert(binding, value);
            }
            ParamTarget::Slot(slot) => slot_args.push((slot, value)),
        };
        if let Some(param) = this_param {
            bind(param, receiver.clone());
        }
        if let Some(param) = arguments_param {
            bind(param, self.create_arguments_object(&args)?);
        }
        let mut args = args.into_iter();
        for param in params {
            bind(param, args.next().unwrap_or(Value::Undefined));
        }
        if let Some(param) = rest_param {
            let rest = self.heap.alloc_array(args.collect());
            bind(param, Value::Object(rest));
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
        globals: BTreeMap<JsString, Value>,
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
            imports: Vec::new(),
            next_continuation_id,
            active_array_joins: BTreeSet::new(),
            builtins,
            random_state: crate::math::RANDOM_SEED,
            script_env,
        }
    }
}
