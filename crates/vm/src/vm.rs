use crate::JsString;
use crate::binding::{
    Action, Bindings, CallCx, CpsBoundary, CpsStage, ExportDecl, ExportId, GuestOutcome,
    GuestResume, HostCall, HostFault, HostStep, JsError, JsValue, LiveRegistration, NativeTarget,
    OperationToken, Resume,
};
use crate::builtin::BuiltinRoots;
use crate::bytecode::{Build, ParamTarget};
use crate::error::VmError;
use crate::frame::Frame;
use crate::gc::ActiveRoots;
use crate::heap::HostClassRecord;
use crate::heap::{Attributes, DescriptorUpdate, Heap, Object, PromiseState, ReactionJob};
use crate::host::{Completion, Host};
use crate::runtime::{
    ExecOutcome, HOST_TASK_ID_LIMIT, HostInstance, HostTask, HostTaskState, Invocation, Microtask,
    PendingOperation, Settlement,
};
use crate::snapshot::DeliveryTarget;
use crate::value::{ContinuationId, ObjectId, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

static NEXT_EPOCH: AtomicU32 = AtomicU32::new(1);

fn next_epoch() -> u32 {
    loop {
        let epoch = NEXT_EPOCH.fetch_add(1, Ordering::Relaxed);
        if epoch != 0 {
            return epoch;
        }
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResolvedExport {
    Function(ExportId),
    Constant,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedModule {
    pub(crate) module: String,
    pub(crate) exports: Vec<(String, ResolvedExport)>,
}

/// Module namespace exports are visible to enumeration but immutable.
const IMPORT_EXPORT_ATTRIBUTES: crate::heap::Attributes = crate::heap::Attributes {
    writable: false,
    enumerable: true,
    configurable: false,
};

pub(crate) const PRELOAD_EXPORT_ATTRIBUTES: Attributes = IMPORT_EXPORT_ATTRIBUTES;

/// A pending operation as exposed to the host after a call or restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingOperationInfo {
    pub token: crate::binding::OperationToken,
    pub name: String,
    pub schema: u32,
    pub recovery: crate::binding::Recovery,
    pub state: Vec<u8>,
}

/// State assembled by restore before publication.
#[derive(Clone, Copy)]
pub(crate) struct HostConstruction {
    pub(crate) prototype: Option<ObjectId>,
}

enum HostEntry {
    Call(HostCall),
    Resume(GuestResume, Invocation),
}

struct HostActivation<'a> {
    bindings: &'a Bindings,
    owner: NativeTarget,
    receiver: &'a Value,
    construction: Option<HostConstruction>,
    task: &'a mut Option<u64>,
}

pub(crate) struct RestoredState {
    pub(crate) heap: Heap,
    pub(crate) continuations: BTreeMap<ContinuationId, crate::runtime::Continuation>,
    pub(crate) host_tasks: BTreeMap<u64, HostTask>,
    pub(crate) next_host_task_id: u64,
    pub(crate) operations: BTreeMap<u64, PendingOperation>,
    pub(crate) host_classes: Vec<HostClassRecord>,
    pub(crate) host_instances: BTreeMap<ObjectId, HostInstance>,
    pub(crate) module_states: Vec<Option<crate::runtime::ModuleState>>,
    pub(crate) globals: BTreeMap<JsString, Value>,
    pub(crate) builtins: BuiltinRoots,
    pub(crate) script_env: Option<ObjectId>,
}

#[derive(Debug)]
pub struct Vm {
    pub(crate) build: Build,
    pub(crate) bindings: Arc<Bindings>,
    pub(crate) heap: Heap,
    pub(crate) continuations: BTreeMap<ContinuationId, crate::runtime::Continuation>,
    pub(crate) operations: BTreeMap<u64, PendingOperation>,
    pub(crate) host_tasks: BTreeMap<u64, HostTask>,
    pub(crate) next_host_task_id: u64,
    /// Operations loaded by restore whose `Resume` / `Reissue` work has not
    /// been handed to the host yet. Drained by `start_recovered_operations`.
    pub(crate) recovered_operations: BTreeSet<u64>,
    pub(crate) microtasks: VecDeque<Microtask>,
    pub(crate) globals: BTreeMap<JsString, Value>,
    pub(crate) imports: Vec<ImportModule>,
    /// Class tag id to the VM-owned prototype object.
    pub(crate) host_classes: Vec<HostClassRecord>,
    /// Hidden slots and payloads keyed by the ordinary object that owns them.
    pub(crate) host_instances: BTreeMap<ObjectId, HostInstance>,
    /// Per-module state aligned with the sorted module declaration table.
    pub(crate) module_states: Vec<Option<crate::runtime::ModuleState>>,
    pub(crate) epoch: u32,
    next_operation_id: u64,
    next_host_call: u64,
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
        let bindings = host.bindings();
        let resolved = Self::resolve_imports(&build, &bindings)?;
        let mut vm = Self::empty(build, bindings.clone());
        vm.init_builtins();
        vm.install_classes(&bindings)?;
        vm.install_module_states(&bindings);
        vm.install_imports(&bindings, resolved)?;
        if let Some(initializer) = vm.build.preload.as_ref().map(|preload| preload.initializer) {
            let env = vm.run_script(host, initializer)?;
            vm.install_preload_globals(env)?;
        }
        vm.script_env = vm.run_script(host, vm.build.script)?;
        vm.run_until_idle(host)?;
        Ok(vm)
    }

    fn empty(build: Build, bindings: Arc<Bindings>) -> Self {
        let module_states = (0..bindings.module_count()).map(|_| None).collect();
        Self {
            build,
            bindings,
            heap: Heap::new(),
            host_tasks: BTreeMap::new(),
            next_host_task_id: 1,
            continuations: BTreeMap::new(),
            imports: Vec::new(),
            operations: BTreeMap::new(),
            recovered_operations: BTreeSet::new(),
            microtasks: VecDeque::new(),
            globals: BTreeMap::new(),
            host_classes: Vec::new(),
            host_instances: BTreeMap::new(),
            module_states,
            epoch: next_epoch(),
            next_operation_id: 1,
            next_host_call: 1,
            next_continuation_id: 1,
            active_array_joins: BTreeSet::new(),
            builtins: BuiltinRoots::default(),
            random_state: crate::builtin::math::RANDOM_SEED,
            script_env: None,
        }
    }

    pub(crate) fn resolve_imports(
        build: &Build,
        bindings: &Bindings,
    ) -> Result<ResolvedImports, VmError> {
        let mut modules = Vec::with_capacity(build.imports.len());
        for (module, exports) in &build.imports {
            let mut resolved = Vec::with_capacity(exports.len());
            for export in exports {
                let (id, declaration) = bindings
                    .export(module, export)
                    .ok_or_else(|| VmError::MissingImport(format!("{module}:{export}")))?;
                let kind = match declaration {
                    ExportDecl::Function(_) => ResolvedExport::Function(id),
                    ExportDecl::Constant(_) => ResolvedExport::Constant,
                };
                resolved.push((export.clone(), kind));
            }
            modules.push(ResolvedModule {
                module: module.clone(),
                exports: resolved,
            });
        }
        Ok(ResolvedImports { modules })
    }

    fn install_classes(&mut self, bindings: &Bindings) -> Result<(), VmError> {
        for (class, _) in bindings.classes() {
            let prototype = self.heap.alloc_plain_with(None);
            self.host_classes.push(HostClassRecord {
                prototype,
                source: class,
            });
            for method in 0..bindings.method_count(class) {
                let target = NativeTarget::Method(crate::binding::MethodId {
                    class: class as u32,
                    method: method as u32,
                });
                let function = self.heap.intern_native(target, false);
                let name = bindings
                    .method(crate::binding::MethodId {
                        class: class as u32,
                        method: method as u32,
                    })
                    .expect("method exists")
                    .name;
                self.heap.define_data(
                    prototype,
                    name.key(),
                    Value::Object(function),
                    crate::binding::PROTOTYPE_METHOD_ATTRIBUTES,
                )?;
            }
            for accessor in 0..bindings.accessor_count(class) {
                let declaration = bindings.accessor(class, accessor).expect("accessor exists");
                let get = declaration.get.as_ref().map(|_| {
                    self.heap.intern_native(
                        NativeTarget::Accessor {
                            class: class as u32,
                            accessor: accessor as u32,
                            set: false,
                        },
                        false,
                    )
                });
                let set = declaration.set.as_ref().map(|_| {
                    self.heap.intern_native(
                        NativeTarget::Accessor {
                            class: class as u32,
                            accessor: accessor as u32,
                            set: true,
                        },
                        false,
                    )
                });
                self.heap.define_property(
                    prototype,
                    declaration.name.key(),
                    crate::heap::Property::Accessor { get, set },
                )?;
            }
        }
        for (class, _) in bindings.classes() {
            if let Some(parent) = bindings.class_parent(class) {
                self.heap.set_prototype(
                    self.host_classes[class].prototype,
                    Some(self.host_classes[parent].prototype),
                )?;
            }
        }
        Ok(())
    }

    fn install_module_states(&mut self, bindings: &Bindings) {
        for module in bindings.state_modules() {
            let Some(payload) = bindings.state_new(module) else {
                continue;
            };
            let slots = bindings
                .state_slots(module)
                .unwrap_or_default()
                .iter()
                .map(|slot| match slot.kind {
                    crate::binding::SlotKind::Js => crate::runtime::HostSlot::Js(Value::Undefined),
                    crate::binding::SlotKind::JsList => {
                        crate::runtime::HostSlot::JsList(Vec::new())
                    }
                })
                .collect();
            self.module_states[module] = Some(crate::runtime::ModuleState { slots, payload });
        }
    }

    pub(crate) fn call_host(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        target: NativeTarget,
        args: Vec<Value>,
        receiver: &Value,
        construction: Option<HostConstruction>,
    ) -> Result<Invocation, VmError> {
        let bindings = host.bindings();
        let declaration = match target {
            NativeTarget::Export(id) => match bindings.export_decl(id) {
                Some(ExportDecl::Function(function)) => {
                    (function.call, function.constructable, function.name.clone())
                }
                _ => {
                    return Err(VmError::Host(format!(
                        "host function at {id:?} is not declared"
                    )));
                }
            },
            NativeTarget::Method(id) => match bindings.method(id) {
                Some(method) => (method.call, false, method.name.to_string()),
                None => {
                    return Err(VmError::Host(format!(
                        "host method at {id:?} is not declared"
                    )));
                }
            },
            NativeTarget::Accessor {
                class,
                accessor,
                set,
            } => match bindings.accessor(class as usize, accessor as usize) {
                Some(declaration) => {
                    let entry = if set {
                        &declaration.set
                    } else {
                        &declaration.get
                    };
                    let Some(entry) = entry else {
                        return Err(VmError::Host(format!(
                            "host accessor {class}.{accessor} has no {}",
                            if set { "setter" } else { "getter" }
                        )));
                    };
                    (entry.call, false, declaration.name.to_string())
                }
                None => {
                    return Err(VmError::Host(format!(
                        "host accessor {class}.{accessor} is not declared"
                    )));
                }
            },
        };
        let call = declaration.0;
        if construction.is_some() && !declaration.1 {
            return Err(VmError::Host(format!(
                "host function {} is not a constructor",
                declaration.2
            )));
        }
        let mut task = None;
        let result = self.run_host_steps(
            host,
            roots,
            HostActivation {
                bindings: &bindings,
                owner: target,
                receiver,
                construction,
                task: &mut task,
            },
            HostEntry::Call(call),
            args,
        );
        if let Some(id) = task {
            return Ok(match self.finish_host_task(id, result)? {
                Some(Invocation::Thrown(error)) => Invocation::Thrown(error),
                _ => Invocation::Value(Value::Number(id as f64)),
            });
        }
        result?.ok_or_else(|| VmError::InvalidState("ordinary host call suspended".to_owned()))
    }

    fn run_host_steps(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        activation: HostActivation<'_>,
        mut entry: HostEntry,
        mut args: Vec<Value>,
    ) -> Result<Option<Invocation>, VmError> {
        let HostActivation {
            bindings,
            owner,
            receiver,
            construction,
            task,
        } = activation;
        let prototype = construction.and_then(|context| context.prototype);
        let allocation_prototype = match (owner, prototype) {
            (NativeTarget::Export(export), Some(prototype)) => bindings
                .export_class(export)
                .map(|class| (class, prototype)),
            _ => None,
        };
        let context = (receiver, prototype);
        let roots = &roots.with(&context);
        loop {
            if task.is_some_and(|id| !self.host_tasks.contains_key(&id)) {
                return Ok(None);
            }
            let call_id = self.next_host_call;
            self.next_host_call = self.next_host_call.checked_add(1).unwrap_or(1);
            let epoch = self.epoch;
            let wrap = |value| JsValue::handle(value, epoch, call_id);
            let mut cx = CallCx {
                vm: self,
                bindings,
                epoch,
                call: call_id,
                args: args.into_iter().map(wrap).collect(),
                this: wrap(receiver.clone()),
                construct: construction.is_some() && matches!(entry, HostEntry::Call(_)),
                allocation_prototype,
            };
            let step = match entry {
                HostEntry::Resume(resume, outcome) => {
                    let outcome = match outcome {
                        Invocation::Value(value) => GuestOutcome::Returned(wrap(value)),
                        Invocation::Thrown(value) => GuestOutcome::Thrown(wrap(value)),
                    };
                    resume(&mut cx, outcome)
                }
                HostEntry::Call(call) => call(&mut cx),
            };
            let request = match step {
                Ok(HostStep::Return(value)) => {
                    return cx
                        .into_value(value)
                        .map(Invocation::Value)
                        .map(Some)
                        .map_err(|error| VmError::Host(error.0));
                }
                Err(JsError::Throw(value)) => {
                    return cx
                        .into_value(value)
                        .map(Invocation::Thrown)
                        .map(Some)
                        .map_err(|error| VmError::Host(error.0));
                }
                Err(JsError::Fault(fault)) => return Err(VmError::HostFault(fault.0)),
                Ok(HostStep::Suspend(request)) => request,
            };
            let (action, saved, resume, spawn) = request
                .into_parts(cx)
                .map_err(|fault| VmError::HostFault(fault.0))?;
            // A running task can cancel itself, including from a nested guest call.
            if task.is_some_and(|id| !self.host_tasks.contains_key(&id)) {
                return Ok(None);
            }
            if spawn && (task.is_some() || construction.is_some() || bindings.cps(owner).is_none())
            {
                return Err(VmError::HostFault(
                    "spawn requires an ordinary linked host function or method".to_owned(),
                ));
            }
            let boundary = match &action {
                Action::Call { .. } => CpsBoundary::Call,
                Action::Gc => CpsBoundary::Gc,
                Action::External { .. } => CpsBoundary::External,
                Action::FinishedSpawn => {
                    if !spawn || !saved.is_empty() {
                        return Err(VmError::HostFault(
                            "invalid finished spawn request".to_owned(),
                        ));
                    }
                    let id = self.alloc_host_task_id()?;
                    return Ok(Some(Invocation::Value(Value::Number(id as f64))));
                }
            };
            let (linked, resume) = match resume {
                Resume::Linked(stage) => {
                    let resume = Self::linked_stage(bindings, owner, stage, boundary, saved.len())?;
                    (Some(stage), resume.resume)
                }
                Resume::Inline(resume)
                    if !spawn && matches!(boundary, CpsBoundary::Call | CpsBoundary::Gc) =>
                {
                    (None, resume)
                }
                Resume::Inline(_) => {
                    return Err(VmError::HostFault(
                        "background tasks require a linked stage".to_owned(),
                    ));
                }
            };
            if spawn {
                let stage = linked.expect("spawn has a linked stage");
                let id = self.alloc_host_task_id()?;
                self.host_tasks.insert(
                    id,
                    HostTask {
                        owner,
                        stage,
                        receiver: Value::Undefined,
                        saved: Vec::new(),
                        state: HostTaskState::Running,
                    },
                );
                *task = Some(id);
            }
            let outcome = match action {
                Action::Call {
                    callee,
                    receiver,
                    args,
                } => self.invoke(host, &roots.with(&saved), &callee, args, &receiver)?,
                Action::Gc => {
                    let removed = self.collect_active(host.gc_roots(), &roots.with(&saved))?;
                    Invocation::Value(Value::Number(removed as f64))
                }
                Action::External { decl, state } => {
                    let (Some(id), Some(stage)) = (*task, linked) else {
                        return Err(VmError::HostFault(
                            "external await requires a background task".to_owned(),
                        ));
                    };
                    let token = self
                        .register_operation(
                            decl,
                            state,
                            DeliveryTarget::Task(id),
                            |token, bindings, bytes| {
                                bindings.start_encoded_operation(decl, bytes, token)
                            },
                        )
                        .map_err(|fault| VmError::HostFault(fault.0))?;
                    let frame = self.host_tasks.get_mut(&id).expect("running task exists");
                    frame.stage = stage;
                    frame.receiver = receiver.clone();
                    frame.saved = saved;
                    frame.state = HostTaskState::Waiting(token.id);
                    return Ok(None);
                }
                Action::FinishedSpawn => unreachable!("finished spawn handled before dispatch"),
            };
            entry = HostEntry::Resume(resume, outcome);
            args = saved;
        }
    }

    fn alloc_host_task_id(&mut self) -> Result<u64, VmError> {
        let id = self.next_host_task_id;
        if id >= HOST_TASK_ID_LIMIT {
            return Err(VmError::HostFault(
                "host task id space exhausted".to_owned(),
            ));
        }
        self.next_host_task_id += 1;
        Ok(id)
    }

    fn linked_stage(
        bindings: &Bindings,
        owner: NativeTarget,
        stage: u32,
        boundary: CpsBoundary,
        slot_count: usize,
    ) -> Result<CpsStage, VmError> {
        bindings
            .cps(owner)
            .and_then(|stages| stages.get(stage as usize))
            .filter(|entry| entry.kind == boundary && entry.slots.len() == slot_count)
            .copied()
            .ok_or_else(|| {
                VmError::HostFault(format!("invalid linked host stage {stage} for {owner:?}"))
            })
    }

    fn finish_host_task(
        &mut self,
        id: u64,
        result: Result<Option<Invocation>, VmError>,
    ) -> Result<Option<Invocation>, VmError> {
        let Some(frame) = self.host_tasks.get(&id) else {
            // The current activation returned after canceling itself.
            return result.map(|_| None);
        };
        if !matches!(frame.state, HostTaskState::Waiting(_)) || result.is_err() {
            self.cancel_host_task(id);
        }
        result
    }

    pub(crate) fn cancel_host_task(&mut self, id: u64) {
        if let Some(frame) = self.host_tasks.remove(&id)
            && let HostTaskState::Waiting(operation) = frame.state
        {
            self.recovered_operations.remove(&operation);
            self.operations.remove(&operation);
        }
    }

    fn resume_host_task(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: u64,
        settlement: &Settlement,
    ) -> Result<(), VmError> {
        let Some(frame) = self.host_tasks.get_mut(&id) else {
            return Ok(()); // canceled after delivery
        };
        if frame.state != HostTaskState::Ready {
            return Err(VmError::InvalidState(format!(
                "host task {id} is not ready"
            )));
        }
        frame.state = HostTaskState::Running;
        let owner = frame.owner;
        let stage = frame.stage;
        let receiver = std::mem::replace(&mut frame.receiver, Value::Undefined);
        let saved = std::mem::take(&mut frame.saved);
        let bindings = self.bindings.clone();
        let mut task = Some(id);
        let result =
            Self::linked_stage(&bindings, owner, stage, CpsBoundary::External, saved.len())
                .and_then(|entry| {
                    let outcome = match settlement {
                        Settlement::Fulfilled(value) => Invocation::Value(value.clone()),
                        Settlement::Rejected(value) => Invocation::Thrown(value.clone()),
                    };
                    self.run_host_steps(
                        host,
                        roots,
                        HostActivation {
                            bindings: &bindings,
                            owner,
                            receiver: &receiver,
                            construction: None,
                            task: &mut task,
                        },
                        HostEntry::Resume(entry.resume, outcome),
                        saved,
                    )
                });
        if let Some(Invocation::Thrown(error)) = self.finish_host_task(id, result)? {
            host.report_error(self.host_value(error));
        }
        Ok(())
    }

    fn import_export_value(
        &mut self,
        bindings: &Bindings,
        resolved: ResolvedExport,
    ) -> Result<Value, VmError> {
        match resolved {
            ResolvedExport::Constant => {
                // Constants are looked up through the bindings below.
                let _ = bindings;
                Err(VmError::InvalidState(
                    "constant import needs a declaration".to_owned(),
                ))
            }
            ResolvedExport::Function(id) => {
                let constructable = matches!(
                    bindings.export_decl(id),
                    Some(ExportDecl::Function(function)) if function.constructable
                );
                Ok(Value::Object(
                    self.heap
                        .intern_native(NativeTarget::Export(id), constructable),
                ))
            }
        }
    }

    fn install_imports(
        &mut self,
        bindings: &Bindings,
        resolved: ResolvedImports,
    ) -> Result<(), VmError> {
        for module in resolved.modules {
            let namespace = self.heap.alloc_plain_with(None);
            let mut values = Vec::with_capacity(module.exports.len());
            for (name, export) in module.exports {
                let value = match export {
                    ResolvedExport::Constant => {
                        let (_, declaration) =
                            bindings.export(&module.module, &name).ok_or_else(|| {
                                VmError::MissingImport(format!("{}:{name}", module.module))
                            })?;
                        let ExportDecl::Constant(constant) = declaration else {
                            return Err(VmError::MissingImport(format!(
                                "{}:{name}",
                                module.module
                            )));
                        };
                        constant.value.value()
                    }
                    ResolvedExport::Function(_) => self.import_export_value(bindings, export)?,
                };
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
        bindings: &Bindings,
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
                let expected = match export {
                    ResolvedExport::Function(_) => self.import_export_value(bindings, export)?,
                    ResolvedExport::Constant => {
                        let (_, declaration) =
                            bindings.export(&module.module, &name).ok_or_else(|| {
                                VmError::MissingImport(format!("{}:{name}", module.module))
                            })?;
                        let ExportDecl::Constant(constant) = declaration else {
                            return Err(VmError::MissingImport(format!(
                                "{}:{name}",
                                module.module
                            )));
                        };
                        constant.value.value()
                    }
                };
                let key = JsString::from(name.as_str());
                let Some(value) = actual.get(&key) else {
                    return Err(VmError::InvalidSnapshot(format!(
                        "import namespace {} is missing export {name:?}",
                        module.module
                    )));
                };
                if value != &expected {
                    return Err(VmError::InvalidSnapshot(format!(
                        "import namespace {} export {name:?} does not match the bindings",
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
        args: Vec<JsValue>,
    ) -> Result<Value, VmError> {
        let callee = self.get_export(entry)?;
        let args = args
            .into_iter()
            .map(|value| {
                value
                    .into_constant()
                    .map_err(|error| VmError::Host(error.0))
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.invoke(host, &ActiveRoots::EMPTY, &callee, args, &Value::Undefined)?
            .into_result()
    }
    fn install_preload_globals(&mut self, env: Option<ObjectId>) -> Result<(), VmError> {
        let preload = self.build.preload.as_ref().expect("preload metadata");
        if preload.getters.is_empty() {
            return Ok(());
        }
        let env = env.ok_or_else(|| VmError::InvalidState("missing preload environment".into()))?;
        let global = self.builtins.get("globalThis");
        let mut closures = BTreeMap::new();
        for (name, &func_id) in &preload.getters {
            let getter = *closures
                .entry(func_id)
                .or_insert_with(|| self.heap.alloc_function(func_id, env));
            let descriptor = DescriptorUpdate {
                get: Some(Some(getter)),
                set: Some(None),
                enumerable: Some(PRELOAD_EXPORT_ATTRIBUTES.enumerable),
                configurable: Some(PRELOAD_EXPORT_ATTRIBUTES.configurable),
                ..DescriptorUpdate::default()
            };
            if !self
                .heap
                .apply_descriptor(global, name.as_str().into(), descriptor)?
            {
                return Err(VmError::InvalidState(format!(
                    "cannot install preload global {name:?}"
                )));
            }
        }
        Ok(())
    }

    fn run_script(
        &mut self,
        host: &mut dyn Host,
        func_id: u32,
    ) -> Result<Option<ObjectId>, VmError> {
        let index = self.function_index(func_id)?;
        let prepared = self.build.functions[index]
            .needs_environment()
            .then(|| self.heap.alloc_environment(None, BTreeMap::new()));
        self.activate(
            host,
            &ActiveRoots::EMPTY,
            func_id,
            None,
            prepared,
            Vec::new(),
            (&Value::Undefined, &Value::Undefined, None),
        )?
        .into_result()
        .map(|_| prepared)
    }

    /// Runs queued VM microtasks. Host timer/event delivery belongs to the
    /// host; it should call this after completing an operation.
    pub fn run_until_idle(&mut self, host: &mut dyn Host) -> Result<(), VmError> {
        while let Some(task) = self.microtasks.pop_front() {
            let roots = &ActiveRoots::EMPTY.with(&task);
            match task {
                Microtask::Resume {
                    continuation,
                    ref settlement,
                } => {
                    let result = self.resume_continuation(host, roots, continuation, settlement);
                    self.continuations.remove(&continuation);
                    result?;
                }
                Microtask::ResumeHost {
                    task,
                    ref settlement,
                } => {
                    self.resume_host_task(host, roots, task, settlement)?;
                }
                Microtask::CallHandler {
                    ref handler,
                    ref argument,
                    ref capability,
                    rejected,
                } => self.run_call_handler(host, roots, handler, argument, capability, rejected)?,
                Microtask::Assimilate {
                    result,
                    ref receiver,
                    ref then,
                } => self.run_assimilate(host, roots, result, receiver, then)?,
                Microtask::BeginAdopt { result, source } => {
                    self.react(source, ReactionJob::Adopt { result })?
                }
                Microtask::Adopt {
                    result,
                    ref settlement,
                } => self.run_adopt(host, roots, result, settlement)?,
                Microtask::Combine {
                    result,
                    index,
                    kind,
                    settlement,
                } => self.run_combine(result, index, kind, settlement)?,
                Microtask::Call {
                    ref function,
                    ref args,
                } => self.run_queue_call(host, roots, function, args)?,
            }
        }
        Ok(())
    }

    pub fn complete_operation(
        &mut self,
        token: crate::binding::OperationToken,
        completion: Completion,
    ) -> Result<(), VmError> {
        if token.epoch != self.epoch {
            return Err(VmError::UnknownOperation(format!(
                "stale operation token {}",
                token.id()
            )));
        }
        let std::collections::btree_map::Entry::Occupied(operation) =
            self.operations.entry(token.id)
        else {
            return Err(VmError::UnknownOperation(token.id().to_string()));
        };
        let (value, rejected) = match completion {
            Completion::Fulfilled(value) => (value, false),
            Completion::Rejected(value) => (value, true),
        };
        let value = value
            .into_constant()
            .map_err(|error| VmError::Host(error.0))?;
        let operation = operation.remove();
        let target = operation.target;
        self.recovered_operations.remove(&token.id);
        drop(operation);
        let settlement = if rejected {
            Settlement::Rejected(value)
        } else {
            Settlement::Fulfilled(value)
        };
        self.deliver_operation(target, settlement)
    }

    pub(crate) fn deliver_operation(
        &mut self,
        target: DeliveryTarget,
        settlement: Settlement,
    ) -> Result<(), VmError> {
        match target {
            DeliveryTarget::Promise(promise) => self.settle(promise, settlement),
            DeliveryTarget::Task(task) => {
                let frame = self.host_tasks.get_mut(&task).ok_or_else(|| {
                    VmError::InvalidState(format!("operation targets missing host task {task}"))
                })?;
                if !matches!(frame.state, HostTaskState::Waiting(_)) {
                    return Err(VmError::InvalidState(format!(
                        "host task {task} is not waiting"
                    )));
                }
                frame.state = HostTaskState::Ready;
                self.microtasks
                    .push_back(Microtask::ResumeHost { task, settlement });
                Ok(())
            }
        }
    }

    pub fn pending_operations(&self) -> Vec<PendingOperationInfo> {
        self.operations
            .iter()
            .map(|(id, operation)| PendingOperationInfo {
                token: crate::binding::OperationToken {
                    epoch: self.epoch,
                    id: *id,
                },
                name: operation.name.clone(),
                schema: operation.schema,
                recovery: operation.recovery,
                state: operation.state.clone(),
            })
            .collect()
    }

    /// Rearms recovered `Resume` / `Reissue` operations after publication.
    /// Each operation is registered at most once. A driver failure delivers
    /// the declared rejection without discarding the restored VM.
    pub fn start_recovered_operations(&mut self) -> Result<(), VmError> {
        while let Some(id) = self.recovered_operations.pop_first() {
            let Some(operation) = self.operations.get_mut(&id) else {
                continue;
            };
            let token = crate::binding::OperationToken {
                epoch: self.epoch,
                id,
            };
            if let Ok(live) =
                self.bindings
                    .resume_operation(operation.declaration, &operation.state, token)
            {
                operation.live = Some(live);
            } else {
                let target = operation.target;
                let rejection = operation.rejection;
                self.operations.remove(&id);
                self.deliver_operation(
                    target,
                    Settlement::Rejected(Value::String(rejection.into())),
                )?;
            }
        }
        Ok(())
    }

    pub(crate) fn register_operation(
        &mut self,
        decl: usize,
        state: Vec<u8>,
        target: DeliveryTarget,
        arm: impl FnOnce(
            OperationToken,
            &Bindings,
            &[u8],
        ) -> Result<Box<dyn LiveRegistration>, HostFault>,
    ) -> Result<OperationToken, HostFault> {
        let id = self.next_operation_id;
        self.next_operation_id = self
            .next_operation_id
            .checked_add(1)
            .ok_or_else(|| HostFault::new("operation id space exhausted"))?;
        let token = OperationToken {
            epoch: self.epoch,
            id,
        };
        let (name, schema, recovery, rejection) = self.bindings.operation_metadata(decl);
        self.operations.insert(
            id,
            PendingOperation {
                name: name.to_owned(),
                schema,
                recovery,
                rejection,
                state,
                target,
                declaration: decl,
                live: None,
            },
        );
        let operation = self.operations.get_mut(&id).expect("operation inserted");
        match arm(token, &self.bindings, &operation.state) {
            Ok(live) => {
                operation.live = Some(live);
                Ok(token)
            }
            Err(fault) => {
                self.operations.remove(&id);
                Err(fault)
            }
        }
    }

    pub fn promise_state(&self, value: &Value) -> Result<PromiseState, VmError> {
        let Value::Object(id) = value else {
            return Err(VmError::InvalidState("value is not an object".to_owned()));
        };
        Ok(self.heap.promise(*id)?.state.clone())
    }

    pub(crate) fn host_value(&self, value: Value) -> JsValue {
        JsValue::handle(value, self.epoch, 0)
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
        roots: &ActiveRoots<'_>,
        promise: ObjectId,
        outcome: ExecOutcome,
    ) -> Result<(), VmError> {
        match outcome {
            ExecOutcome::Suspended => Ok(()),
            ExecOutcome::Return(value) => self.resolve_promise(host, roots, promise, value),
            ExecOutcome::Thrown(value) => self.settle(promise, Settlement::Rejected(value)),
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Explicit active roots at a guest call boundary"
    )]
    pub(crate) fn call_function(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        func_id: u32,
        parent_env: Option<ObjectId>,
        args: Vec<Value>,
        receiver: &Value,
        (new_target, home): (&Value, Option<ObjectId>),
    ) -> Result<Invocation, VmError> {
        self.activate(
            host,
            roots,
            func_id,
            parent_env,
            None,
            args,
            (receiver, new_target, home),
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Explicit active roots at a guest call boundary"
    )]
    fn activate(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        func_id: u32,
        parent_env: Option<ObjectId>,
        prepared_env: Option<ObjectId>,
        args: Vec<Value>,
        (receiver, new_target, home): (&Value, &Value, Option<ObjectId>),
    ) -> Result<Invocation, VmError> {
        let function_index = self.function_index(func_id)?;
        let is_async = self.build.functions[function_index].is_async;
        let params = self.build.functions[function_index].params.clone();
        let rest_param = self.build.functions[function_index].rest_param.clone();
        let this_param = self.build.functions[function_index].this_param.clone();
        let new_target_param = self.build.functions[function_index]
            .new_target_param
            .clone();
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
        if let Some(param) = new_target_param {
            bind(param, new_target.clone());
        }
        if let Some(param) = self.build.functions[function_index].home_param.clone() {
            let home =
                home.ok_or_else(|| VmError::InvalidState("function has no home object".into()))?;
            bind(param, Value::Object(home));
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
        let outcome = self.execute_frame(host, roots, frame)?;
        match async_promise {
            Some(promise) => {
                self.settle_outcome(host, roots, promise, outcome)?;
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

    pub(crate) fn restored(build: Build, bindings: Arc<Bindings>, state: RestoredState) -> Self {
        let RestoredState {
            mut heap,
            continuations,
            operations,
            host_tasks,
            next_host_task_id,
            host_classes,
            host_instances,
            module_states,
            globals,
            builtins,
            script_env,
        } = state;
        heap.promise_prototype = Some(builtins.get("promise.prototype"));
        heap.object_prototype = Some(builtins.get("object.prototype"));
        heap.array_prototype = Some(builtins.get("array.prototype"));
        heap.function_prototype = Some(builtins.get("function.prototype"));
        heap.finish_restore();
        let next_continuation_id = continuations.keys().next_back().map_or(1, |id| id + 1);
        let next_operation_id = operations.keys().next_back().map_or(1, |id| id + 1);
        let recovered_operations = operations
            .iter()
            .filter(|(_, operation)| {
                matches!(
                    operation.recovery,
                    crate::binding::Recovery::Resume | crate::binding::Recovery::Reissue
                )
            })
            .map(|(id, _)| *id)
            .collect();
        Self {
            build,
            bindings,
            heap,
            continuations,
            operations,
            host_tasks,
            next_host_task_id,
            recovered_operations,
            microtasks: VecDeque::new(),
            globals,
            imports: Vec::new(),
            host_classes,
            host_instances,
            module_states,
            epoch: next_epoch(),
            next_operation_id,
            next_host_call: 1,
            next_continuation_id,
            active_array_joins: BTreeSet::new(),
            builtins,
            random_state: crate::builtin::math::RANDOM_SEED,
            script_env,
        }
    }
}
