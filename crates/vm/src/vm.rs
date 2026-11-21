use crate::JsString;
use crate::binding::{
    Action, Bindings, CallCx, CpsBoundary, CpsStage, ExportDecl, ExportId, GuestOutcome,
    GuestResume, HostCall, HostFault, HostStep, JsError, JsValue, LiveRegistration, NativeTarget,
    OperationToken, Resume,
};
use crate::builtin::BuiltinRoots;
use crate::bytecode::{Build, ImportDecl, ModuleTarget, ParamTarget};
use crate::error::VmError;
use crate::frame::Frame;
use crate::gc::ActiveRoots;
use crate::heap::HostClassRecord;
use crate::heap::{
    Attributes, DescriptorUpdate, Heap, Object, PromiseState, ReactionJob, RejectionStatus,
};
use crate::host::{Completion, Host};
use crate::runtime::{
    ContinuationTarget, ExecOutcome, HOST_TASK_ID_LIMIT, HostInstance, HostTask, HostTaskState,
    Invocation, Microtask, PendingOperation, Settlement,
};
use crate::snapshot::DeliveryTarget;
use crate::value::{ContinuationId, ObjectId, Value, ValueRef};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

pub(crate) fn host_function_metadata(
    bindings: &Bindings,
    target: NativeTarget,
) -> Option<(JsString, usize)> {
    use crate::binding::MemberName;
    let (member, prefix, arity) = match target {
        NativeTarget::Export(id) => match bindings.export_decl(id)? {
            ExportDecl::Function(f) => return Some((f.name.as_str().into(), f.arity)),
            _ => return None,
        },
        NativeTarget::Method(id) => {
            let method = bindings.method(id)?;
            (method.name, "", method.arity)
        }
        NativeTarget::Accessor {
            class,
            accessor,
            set,
        } => {
            let decl = bindings.accessor(class as usize, accessor as usize)?;
            if set {
                (decl.name, "set ", decl.set?.arity())
            } else {
                (decl.name, "get ", decl.get?.arity())
            }
        }
    };
    let name = match member {
        MemberName::Name(name) if prefix.is_empty() => name.into(),
        MemberName::Name(name) => format!("{prefix}{name}").into(),
        MemberName::Symbol(symbol) => format!("{prefix}[{}]", symbol.label()).into(),
    };
    Some((name, arity))
}

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

pub(crate) const MODULE_EXPORT_ATTRIBUTES: Attributes = IMPORT_EXPORT_ATTRIBUTES;
pub(crate) const PRELOAD_EXPORT_ATTRIBUTES: Attributes = MODULE_EXPORT_ATTRIBUTES;

pub(crate) fn module_getter_capture(target: ModuleTarget) -> Option<u32> {
    match target {
        ModuleTarget::Binding { module, .. } => Some(module),
        ModuleTarget::Namespace { .. } | ModuleTarget::Host(_) => None,
    }
}

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
    pub(crate) module_envs: Vec<Option<ObjectId>>,
    pub(crate) module_namespaces: Vec<ObjectId>,
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
    /// FIFO of rejected promises with a pending host notification.
    pub(crate) pending_rejection_notifications: VecDeque<ObjectId>,
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
    pub(crate) module_envs: Vec<Option<ObjectId>>,
    pub(crate) module_namespaces: Vec<ObjectId>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ModuleEvaluation {
    Pending,
    Visiting,
    Done,
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
        vm.instantiate_modules(host)?;
        let mut states = vec![ModuleEvaluation::Pending; vm.build.modules.len()];
        if let Some(preload) = vm.build.preload {
            vm.evaluate_module(host, preload, &mut states)?;
            vm.install_preload_globals(preload)?;
        }
        vm.evaluate_module(host, vm.build.entry, &mut states)?;
        for env in vm.module_envs.iter().flatten() {
            if !vm.heap.environment(*env)?.uninitialized.is_empty() {
                return Err(VmError::InvalidState(
                    "module evaluation left uninitialized bindings".into(),
                ));
            }
        }
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
            pending_rejection_notifications: VecDeque::new(),
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
            module_envs: Vec::new(),
            module_namespaces: Vec::new(),
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
                let (name, arity) =
                    host_function_metadata(bindings, target).expect("method exists");
                let function = self.heap.intern_native(target, false, arity, name);
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
                    Value::object(function),
                    crate::binding::PROTOTYPE_METHOD_ATTRIBUTES,
                )?;
            }
            for accessor in 0..bindings.accessor_count(class) {
                let declaration = bindings.accessor(class, accessor).expect("accessor exists");
                let get = declaration.get.as_ref().map(|_| {
                    let target = NativeTarget::Accessor {
                        class: class as u32,
                        accessor: accessor as u32,
                        set: false,
                    };
                    let (name, arity) =
                        host_function_metadata(bindings, target).expect("getter exists");
                    self.heap.intern_native(target, false, arity, name)
                });
                let set = declaration.set.as_ref().map(|_| {
                    let target = NativeTarget::Accessor {
                        class: class as u32,
                        accessor: accessor as u32,
                        set: true,
                    };
                    let (name, arity) =
                        host_function_metadata(bindings, target).expect("setter exists");
                    self.heap.intern_native(target, false, arity, name)
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
                    crate::binding::SlotKind::Js => crate::runtime::HostSlot::Js(Value::UNDEFINED),
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
                _ => Invocation::Value(Value::number(id as f64)),
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
                    return Ok(Some(Invocation::Value(Value::number(id as f64))));
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
                        receiver: Value::UNDEFINED,
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
                    Invocation::Value(Value::number(removed as f64))
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
        let receiver = std::mem::replace(&mut frame.receiver, Value::UNDEFINED);
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
                let target = NativeTarget::Export(id);
                let (name, arity) = host_function_metadata(bindings, target).ok_or_else(|| {
                    VmError::InvalidState("missing host function declaration".into())
                })?;
                Ok(Value::object(self.heap.intern_native(
                    target,
                    constructable,
                    arity,
                    name,
                )))
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
        let target = self.build.modules[self.build.entry as usize]
            .exports
            .get(name)
            .ok_or_else(|| VmError::InvalidState(format!("unknown export {name:?}")))?
            .target;
        match target {
            ModuleTarget::Binding { module, binding } => self
                .heap
                .get_module_binding(self.module_environment(module)?, binding)?
                .ok_or_else(|| VmError::InvalidState(format!("export {name:?} is uninitialized"))),
            ModuleTarget::Namespace { module } => self.module_namespace(module),
            ModuleTarget::Host(import) => self.read_host_import(import),
        }
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
        self.invoke(host, &ActiveRoots::EMPTY, &callee, args, &Value::UNDEFINED)?
            .into_result()
    }

    pub(crate) fn read_host_import(&self, import: ImportDecl) -> Result<Value, VmError> {
        let module = self.imports.get(import.module as usize).ok_or_else(|| {
            VmError::InvalidState(format!("import module {} does not exist", import.module))
        })?;
        let declared_module = self
            .build
            .imports
            .keys()
            .nth(import.module as usize)
            .ok_or_else(|| {
                VmError::InvalidState(format!("import module {} is not declared", import.module))
            })?;
        if declared_module != &module.module {
            return Err(VmError::InvalidState(format!(
                "import module index {} is inconsistent",
                import.module
            )));
        }
        match import.export {
            None => Ok(Value::object(module.namespace)),
            Some(export) => module.values.get(export as usize).cloned().ok_or_else(|| {
                VmError::InvalidState(format!("import export {export} does not exist"))
            }),
        }
    }

    pub(crate) fn module_environment(&self, module: u32) -> Result<ObjectId, VmError> {
        self.module_envs
            .get(module as usize)
            .copied()
            .flatten()
            .ok_or_else(|| VmError::InvalidState(format!("module {module} has no environment")))
    }

    pub(crate) fn module_namespace(&self, module: u32) -> Result<Value, VmError> {
        self.module_namespaces
            .get(module as usize)
            .copied()
            .map(Value::object)
            .ok_or_else(|| VmError::InvalidState(format!("module {module} has no namespace")))
    }

    fn instantiate_modules(&mut self, host: &mut dyn Host) -> Result<(), VmError> {
        let count = self.build.modules.len();
        self.module_envs.reserve(count);
        self.module_namespaces.reserve(count);
        for index in 0..count {
            let module = index as u32;
            let env = if self.build.module_needs_environment(module) {
                let bindings = self.build.module_bindings(module);
                let env = self
                    .heap
                    .alloc_environment(None, bindings.map(|id| (id, Value::UNDEFINED)).collect());
                let environment = self.heap.environment_mut(env)?;
                environment.module = Some(module);
                environment
                    .uninitialized
                    .extend(environment.bindings.keys().copied());
                Some(env)
            } else {
                None
            };
            self.module_envs.push(env);
            self.module_namespaces
                .push(self.heap.alloc_module_namespace(module));
        }

        let mut getters = BTreeMap::new();
        let mut sentinel = None;
        for (module, &namespace) in self.build.modules.iter().zip(&self.module_namespaces) {
            for (name, export) in &module.exports {
                let getter = match getters.entry(export.target) {
                    std::collections::btree_map::Entry::Occupied(entry) => *entry.get(),
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        let env = match module_getter_capture(export.target) {
                            Some(module) => self.module_environment(module)?,
                            None => match sentinel {
                                Some(env) => env,
                                None => {
                                    let env = self.heap.alloc_environment(None, BTreeMap::new());
                                    self.heap.environment_mut(env)?.module_getter_sentinel = true;
                                    sentinel = Some(env);
                                    env
                                }
                            },
                        };
                        *entry.insert(self.heap.alloc_function(export.getter, env))
                    }
                };
                let descriptor = DescriptorUpdate {
                    get: Some(Some(getter)),
                    set: Some(None),
                    enumerable: Some(MODULE_EXPORT_ATTRIBUTES.enumerable),
                    configurable: Some(MODULE_EXPORT_ATTRIBUTES.configurable),
                    ..DescriptorUpdate::default()
                };
                if !self
                    .heap
                    .apply_descriptor(namespace, name.as_str().into(), descriptor)?
                {
                    return Err(VmError::InvalidState(format!(
                        "cannot install module export {name:?}"
                    )));
                }
            }
            self.heap.get_mut(namespace)?.prevent_extensions();
        }

        for index in 0..count {
            let declaration = self.build.modules[index].declaration;
            self.run_module_phase(host, index, declaration)?;
        }
        Ok(())
    }

    fn install_preload_globals(&mut self, preload: u32) -> Result<(), VmError> {
        let namespace = self.module_namespaces[preload as usize];
        let global = self.builtins.get("globalThis");
        for name in self.build.modules[preload as usize].exports.keys() {
            let Some(crate::heap::OwnProperty::Stored(
                crate::heap::Property::Accessor {
                    get: Some(getter),
                    set: None,
                },
                _,
            )) = self
                .heap
                .get(namespace)?
                .own_property(crate::value::PropertyKeyRef::String(name))
            else {
                return Err(VmError::InvalidState(format!(
                    "missing preload getter {name:?}"
                )));
            };
            let descriptor = DescriptorUpdate {
                get: Some(Some(*getter)),
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

    fn evaluate_module(
        &mut self,
        host: &mut dyn Host,
        root: u32,
        states: &mut [ModuleEvaluation],
    ) -> Result<(), VmError> {
        let root = root as usize;
        if states[root] == ModuleEvaluation::Done {
            return Ok(());
        }
        let mut stack = vec![(root, 0)];
        states[root] = ModuleEvaluation::Visiting;
        while let Some((index, next_dependency)) = stack.last_mut() {
            let module = &self.build.modules[*index];
            if let Some(&dependency) = module.dependencies.get(*next_dependency) {
                *next_dependency += 1;
                let dependency = dependency as usize;
                if states[dependency] == ModuleEvaluation::Pending {
                    states[dependency] = ModuleEvaluation::Visiting;
                    stack.push((dependency, 0));
                }
            } else {
                let index = *index;
                self.run_module_phase(host, index, module.evaluation)?;
                states[index] = ModuleEvaluation::Done;
                stack.pop();
            }
        }
        Ok(())
    }

    fn run_module_phase(
        &mut self,
        host: &mut dyn Host,
        module: usize,
        func_id: u32,
    ) -> Result<(), VmError> {
        let prepared = self.module_envs[module];
        self.activate(
            host,
            &ActiveRoots::EMPTY,
            func_id,
            None,
            prepared,
            Vec::new(),
            (&Value::UNDEFINED, &Value::UNDEFINED, None),
            None,
        )?
        .into_result()
        .map(|_| ())
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
                Microtask::GeneratorClose {
                    generator,
                    request,
                    ref settlement,
                } => self.finish_closing_request(host, roots, generator, request, settlement)?,
                Microtask::Call {
                    ref function,
                    ref args,
                } => self.run_queue_call(host, roots, function, args)?,
            }
        }
        self.deliver_rejection_notifications(host)
    }

    fn deliver_rejection_notifications(&mut self, host: &mut dyn Host) -> Result<(), VmError> {
        while let Some(id) = self.pending_rejection_notifications.pop_front() {
            let object = self.heap.promise_mut(id)?;
            let reason = match (&object.state, object.rejection_status) {
                (PromiseState::Rejected(reason), RejectionStatus::PendingReport) => {
                    object.rejection_status = RejectionStatus::Reported;
                    Some(reason.clone())
                }
                (PromiseState::Rejected(_), RejectionStatus::PendingHandled) => {
                    object.rejection_status = RejectionStatus::Handled;
                    None
                }
                _ => {
                    return Err(VmError::InvalidState(format!(
                        "invalid rejection notification for Promise {id}"
                    )));
                }
            };
            let promise = self.host_value(Value::object(id));
            if let Some(reason) = reason {
                host.unhandled_rejection(promise, self.host_value(reason));
            } else {
                host.rejection_handled(promise);
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
                    Settlement::Rejected(Value::string(rejection.into())),
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
        let ValueRef::Object(id) = value.view() else {
            return Err(VmError::InvalidState("value is not an object".to_owned()));
        };
        Ok(self.heap.promise(id)?.state.clone())
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
            if matches!(settlement, Settlement::Rejected(_))
                && promise_object.rejection_status == RejectionStatus::Unobserved
            {
                promise_object.rejection_status = RejectionStatus::PendingReport;
                self.pending_rejection_notifications.push_back(promise);
            }
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
            None => self.heap.promise_mut(promise)?.reactions.push(job),
            Some(settlement) => self.enqueue_job(job, settlement)?,
        }
        let status = &mut self.heap.promise_mut(promise)?.rejection_status;
        match *status {
            RejectionStatus::Unobserved => *status = RejectionStatus::Handled,
            RejectionStatus::PendingReport => {
                *status = RejectionStatus::Handled;
                if let Some(index) = self
                    .pending_rejection_notifications
                    .iter()
                    .rposition(|&id| id == promise)
                {
                    self.pending_rejection_notifications.remove(index);
                }
            }
            RejectionStatus::Reported => {
                *status = RejectionStatus::PendingHandled;
                self.pending_rejection_notifications.push_back(promise);
            }
            RejectionStatus::Handled | RejectionStatus::PendingHandled => {}
        }
        Ok(())
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
            ReactionJob::GeneratorClose { generator, request } => {
                self.microtasks.push_back(Microtask::GeneratorClose {
                    generator,
                    request,
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
            ExecOutcome::Yielded { .. } => Err(VmError::InvalidState(
                "generator suspended inside an async activation".to_owned(),
            )),
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
        callee: Option<ObjectId>,
    ) -> Result<Invocation, VmError> {
        self.activate(
            host,
            roots,
            func_id,
            parent_env,
            None,
            args,
            (receiver, new_target, home),
            callee,
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
        callee: Option<ObjectId>,
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
        // An async generator's call returns the generator object, so it has no
        // activation result Promise; each request owns its own instead.
        let is_generator = self.build.functions[function_index].is_generator;
        let result_promise = (is_async && !is_generator).then(|| self.heap.alloc_promise());

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
            bind(param, Value::object(home));
        }
        if let Some(param) = arguments_param {
            bind(param, self.create_arguments_object(&args)?);
        }
        let mut args = args.into_iter();
        for param in params {
            bind(param, args.next().unwrap_or(Value::UNDEFINED));
        }
        if let Some(param) = rest_param {
            let rest = self.heap.alloc_array(args.collect());
            bind(param, Value::object(rest));
        }
        let needs_env = self.build.functions[function_index].needs_environment();
        let env = if needs_env || prepared_env.is_some() {
            let env = match prepared_env {
                Some(env) => env,
                None => self.heap.alloc_environment(parent_env, BTreeMap::new()),
            };
            if !bindings.is_empty() {
                self.heap.environment_mut(env)?.bindings.extend(bindings);
            }
            Some(env)
        } else {
            None
        };
        let mut frame = Frame::new(
            function_index,
            &self.build.functions[function_index],
            env,
            result_promise.map(ContinuationTarget::Promise),
        );
        frame.callee = callee;
        for (slot, value) in slot_args {
            frame.write(slot, value)?;
        }
        let outcome = self.execute_frame(host, roots, frame)?;
        match result_promise {
            Some(promise) => {
                self.settle_outcome(host, roots, promise, outcome)?;
                Ok(Invocation::Value(Value::object(promise)))
            }
            None => match outcome {
                ExecOutcome::Suspended => Err(VmError::InvalidState(
                    "synchronous function suspended".to_owned(),
                )),
                ExecOutcome::Yielded { .. } => Err(VmError::InvalidState(
                    "synchronous function yielded outside a generator resume".to_owned(),
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
            module_envs,
            module_namespaces,
        } = state;
        heap.promise_prototype = Some(builtins.get("promise.prototype"));
        heap.generator_prototype = Some(builtins.get("generator.prototype"));
        heap.async_generator_prototype = Some(builtins.get("asyncGenerator.prototype"));
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
            pending_rejection_notifications: VecDeque::new(),
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
            module_envs,
            module_namespaces,
        }
    }
}
