use crate::JsString;
use crate::binding::{
    Action, Bindings, CallCx, CpsBoundary, CpsStage, ExportCall, ExportDecl, ExportId,
    GuestOutcome, HostFault, HostStep, JsError, JsValue, LiveRegistration, ModuleDispatch,
    NativeTarget, OperationToken, Resume, ResumeCall,
};
use crate::builtin::BuiltinRoots;
use crate::bytecode::{Build, ImportDecl, ModuleTarget, ParamTarget};
use crate::error::VmError;
use crate::frame::Frame;
use crate::gc::ActiveRoots;
use crate::heap::EventPayload;
use crate::heap::HostClassRecord;
use crate::heap::{Attributes, Heap, PromiseState, ReactionJob, RejectionStatus};
use crate::host::{Completion, Host, TimerKey};
use crate::runtime::{
    Arguments, ContinuationTarget, ExecOutcome, HOST_TASK_ID_LIMIT, HostInstance, HostTask,
    HostTaskState, Invocation, Microtask, PendingOperation, Settlement,
};
use crate::snapshot::{DeliveryTarget, OrdinaryTask, ScriptTimer, TimerTarget};
use crate::value::{ContinuationId, ObjectId, PropertyKey, PropertyKeyRef, Value, ValueRef};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

#[path = "timer_runtime.rs"]
mod timer_runtime;

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

pub(crate) fn next_epoch() -> u32 {
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
pub(crate) const IMPORT_EXPORT_ATTRIBUTES: crate::heap::Attributes = crate::heap::Attributes {
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
    pub recovery: crate::binding::Recovery,
    pub state: Vec<u8>,
}

/// State assembled by restore before publication.
#[derive(Clone, Copy)]
pub(crate) struct HostConstruction {
    pub(crate) prototype: Option<ObjectId>,
}

enum HostEntry {
    Call(ExportCall),
    Resume(ResumeCall, Invocation),
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
    pub(crate) next_operation_id: u64,
    pub(crate) script_timers: BTreeMap<i32, ScriptTimer>,
    pub(crate) console: crate::console::ConsoleState,
    pub(crate) next_timer_id: i32,
    pub(crate) next_stream_message: u64,
    pub(crate) clock: crate::runtime::VmClock,
    pub(crate) base_url: Option<url::Url>,
    pub(crate) fetch_environment: crate::fetch::FetchEnvironment,
    pub(crate) fetch_state: crate::builtin::fetch::transaction::FetchState,
    pub(crate) object_urls: crate::builtin::fetch::object_url::ObjectUrls,
    pub(crate) host_classes: Vec<HostClassRecord>,
    pub(crate) host_instances: BTreeMap<ObjectId, HostInstance>,
    pub(crate) module_states: Vec<crate::runtime::ModuleState>,
    pub(crate) globals: BTreeMap<JsString, Value>,
    pub(crate) builtins: BuiltinRoots,
    pub(crate) templates: Vec<ObjectId>,
    pub(crate) module_envs: Vec<Option<ObjectId>>,
    pub(crate) module_namespaces: Vec<ObjectId>,
}

/// Picks the `wintertc.time` service from the linked bindings, falling back to
/// the deterministic [`crate::host::DummyTimeService`].
pub(crate) fn time_source(bindings: &crate::binding::Bindings) -> TimeSource {
    TimeSource(
        bindings
            .service::<crate::host::TimeKey>()
            .cloned()
            .unwrap_or_else(|| Arc::new(crate::host::DummyTimeService)),
    )
}

/// Wall-clock source linked into a VM. Owns the `wintertc.time` service or the
/// deterministic dummy fallback; `Debug` hides the trait object.
pub(crate) struct TimeSource(Arc<dyn crate::host::TimeService>);

impl std::fmt::Debug for TimeSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TimeSource")
    }
}

/// Runtime-only metadata indexed by the original, validated function table.
#[derive(Debug)]
struct FunctionTable {
    indices: FunctionIndices,
    calls: Vec<CallMetadata>,
    module_environments: Vec<bool>,
}

#[derive(Debug)]
enum FunctionIndices {
    /// `func_id` to function index; `usize::MAX` marks a missing ID.
    Dense(Vec<usize>),
    /// Sorted by `func_id`; used when IDs are too sparse for a dense table.
    Sparse(Vec<(u32, usize)>),
}

impl FunctionIndices {
    fn new(by_id: &BTreeMap<u32, usize>) -> Self {
        let Some((&maximum, _)) = by_id.last_key_value() else {
            return Self::Dense(Vec::new());
        };
        let limit = by_id.len().saturating_mul(4).max(64);
        if usize::try_from(maximum).is_ok_and(|maximum| maximum < limit) {
            let mut dense = vec![usize::MAX; usize::try_from(maximum).unwrap() + 1];
            for (&func_id, &index) in by_id {
                dense[func_id as usize] = index;
            }
            Self::Dense(dense)
        } else {
            Self::Sparse(by_id.iter().map(|(&id, &index)| (id, index)).collect())
        }
    }

    fn get(&self, func_id: u32) -> Option<usize> {
        match self {
            Self::Dense(indices) => indices
                .get(func_id as usize)
                .copied()
                .filter(|&index| index != usize::MAX),
            Self::Sparse(indices) => indices
                .binary_search_by_key(&func_id, |&(id, _)| id)
                .ok()
                .map(|index| indices[index].1),
        }
    }
}

#[derive(Debug)]
struct CallMetadata {
    needs_environment: bool,
    is_initializer: bool,
}

/// A monomorphic property-site cache. It records the receiver identity, key,
/// table layout and slot; the value and attributes are always read fresh.
#[derive(Clone, Debug)]
pub(crate) struct PropertyCacheEntry {
    pub(crate) receiver: ObjectId,
    pub(crate) key: PropertyKey,
    pub(crate) layout: u64,
    pub(crate) slot: u32,
    /// The previous access at this site named a different receiver or key.
    /// A stable hit clears it; two consecutive mismatches disable the site.
    pub(crate) unstable: bool,
}

impl PropertyCacheEntry {
    pub(crate) fn matches(&self, receiver: ObjectId, key: PropertyKeyRef<'_>) -> bool {
        self.receiver == receiver && self.key.borrowed() == key
    }

    pub(crate) fn symbol(&self) -> bool {
        matches!(self.key, PropertyKey::Symbol(_))
    }
}

/// A site starts empty. One distinct receiver/key fills it; seeing a second
/// one disables the site for the rest of the VM generation so a megamorphic
/// site does not pay a failed lookup on every access.
#[derive(Clone, Debug, Default)]
pub(crate) enum PropertyCache {
    #[default]
    Empty,
    /// The site ran a few times. Repeated accesses may fill a monomorphic
    /// entry; one-shot sites never pay for a slot lookup.
    Seen(u8),
    Entry(PropertyCacheEntry),
    Disabled,
}

pub(crate) fn property_caches(build: &Build) -> Vec<Vec<PropertyCache>> {
    vec![Vec::new(); build.functions.len()]
}

impl FunctionTable {
    fn from_validated(build: &Build) -> Self {
        let by_id: BTreeMap<_, _> = build
            .functions
            .iter()
            .enumerate()
            .map(|(index, function)| (function.func_id, index))
            .collect();
        let mut calls: Vec<_> = build
            .functions
            .iter()
            .map(|function| CallMetadata {
                needs_environment: function.needs_environment(),
                is_initializer: false,
            })
            .collect();
        let module_environments = build
            .modules
            .iter()
            .map(|module| {
                let declaration = by_id[&module.declaration];
                let evaluation = by_id[&module.evaluation];
                calls[declaration].is_initializer = true;
                calls[evaluation].is_initializer = true;
                calls[declaration].needs_environment || calls[evaluation].needs_environment
            })
            .collect();
        Self {
            indices: FunctionIndices::new(&by_id),
            calls,
            module_environments,
        }
    }
}

/// An immutable, validated program and its host-independent execution metadata.
/// Clone this handle to create multiple VMs on the same thread.
#[derive(Clone, Debug)]
pub struct VerifiedBuild(Rc<VerifiedBuildInner>);

#[derive(Debug)]
struct VerifiedBuildInner {
    build: Build,
    functions: FunctionTable,
}

impl VerifiedBuild {
    /// Check the bytecode version, identity, and structure before sharing it.
    pub fn new(build: Build) -> Result<Self, VmError> {
        build.validate().map_err(VmError::InvalidBuild)?;
        Ok(Self::from_validated(build))
    }

    pub(crate) fn from_validated(build: Build) -> Self {
        let functions = FunctionTable::from_validated(&build);
        Self(Rc::new(VerifiedBuildInner { build, functions }))
    }
}

impl TryFrom<Build> for VerifiedBuild {
    type Error = VmError;

    fn try_from(build: Build) -> Result<Self, Self::Error> {
        Self::new(build)
    }
}

impl std::ops::Deref for VerifiedBuild {
    type Target = Build;

    fn deref(&self) -> &Build {
        &self.0.build
    }
}

#[derive(Debug)]
pub struct Vm {
    pub(crate) build: VerifiedBuild,
    pub(crate) bindings: Bindings,
    pub(crate) heap: Heap,
    pub(crate) continuations: BTreeMap<ContinuationId, crate::runtime::Continuation>,
    pub(crate) operations: BTreeMap<u64, PendingOperation>,
    pub(crate) host_tasks: BTreeMap<u64, HostTask>,
    pub(crate) next_host_task_id: u64,
    /// Operations loaded by restore whose `Resume` / `Reissue` work has not
    /// been handed to the host yet. Drained by `start_recovered_operations`.
    pub(crate) recovered_operations: BTreeSet<u64>,
    pub(crate) ordinary_tasks: VecDeque<OrdinaryTask>,
    pub(crate) next_stream_message: u64,
    pub(crate) script_timers: BTreeMap<i32, ScriptTimer>,
    pub(crate) console: crate::console::ConsoleState,
    pub(crate) next_timer_id: i32,
    pub(crate) timer_nesting: u32,
    pub(crate) timer_deadlines: BTreeMap<(Duration, u64), u64>,
    pub(crate) microtasks: VecDeque<Microtask>,
    /// Destinations of stack `use` calls currently running a getter. A move
    /// transfers the list; disposal detaches it. These calls never suspend.
    pub(crate) stack_registrations: Vec<Option<ObjectId>>,
    /// Ordered rejection candidates for the next successful checkpoint.
    pub(crate) pending_rejection_notifications: Vec<ObjectId>,
    pub(crate) globals: BTreeMap<JsString, Value>,
    pub(crate) imports: Vec<ImportModule>,
    /// Class tag id to the VM-owned prototype object.
    pub(crate) host_classes: Vec<HostClassRecord>,
    /// Hidden slots and payloads keyed by the ordinary object that owns them.
    pub(crate) host_instances: BTreeMap<ObjectId, HostInstance>,
    /// Per-module state aligned with the sorted module declaration table.
    pub(crate) module_states: Vec<crate::runtime::ModuleState>,
    pub(crate) epoch: u32,
    pub(crate) next_operation_id: u64,
    pub(crate) next_host_call: u64,
    pub(crate) next_continuation_id: ContinuationId,
    /// Synchronous join recursion guard; never live at a snapshot boundary.
    pub(crate) active_array_joins: BTreeSet<ObjectId>,
    /// Synchronous dispatch stack; the shared paths are also active GC roots.
    pub(crate) active_event_paths: Vec<(ObjectId, std::rc::Rc<Vec<ObjectId>>)>,
    /// Prevent recursive error event dispatch, but not nested default reporting.
    pub(crate) reporting_exception: bool,
    pub(crate) builtins: BuiltinRoots,
    /// Frozen tagged-template objects, indexed by `Instr::GetTemplate`.
    pub(crate) templates: Vec<ObjectId>,
    pub(crate) random_state: u64,
    pub(crate) clock: crate::runtime::VmClock,
    /// Wall-clock source for `Date` and file timestamps.
    pub(crate) time: TimeSource,
    pub(crate) base_url: Option<url::Url>,
    pub(crate) fetch_environment: crate::fetch::FetchEnvironment,
    pub(crate) fetch_state: crate::builtin::fetch::transaction::FetchState,
    pub(crate) fetch_live: BTreeMap<u64, Box<dyn crate::fetch::FetchRegistration>>,
    pub(crate) subtle_state: crate::builtin::crypto::worker::SubtleState,
    pub(crate) object_urls: crate::builtin::fetch::object_url::ObjectUrls,
    pub(crate) module_envs: Vec<Option<ObjectId>>,
    pub(crate) module_namespaces: Vec<ObjectId>,
    /// Per-function, per-PC monomorphic property cache. Never serialized.
    pub(crate) property_caches: Vec<Vec<PropertyCache>>,
}

impl Vm {
    pub(crate) fn monotonic_millis(&self, _: &dyn Host) -> f64 {
        self.clock.now()
    }

    pub(crate) fn call_host(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        target: NativeTarget,
        args: Arguments<'_>,
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
                Some(method) => (
                    ExportCall::Plain(method.call),
                    false,
                    method.name.to_string(),
                ),
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
                    (
                        ExportCall::Plain(entry.call),
                        false,
                        declaration.name.to_string(),
                    )
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
            args.to_vec()?,
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
                roots,
                bindings,
                epoch,
                call: call_id,
                args: args.into_iter().map(wrap).collect(),
                this: wrap(receiver.clone()),
                construct: construction.is_some() && matches!(entry, HostEntry::Call(_)),
                allocation_prototype,
            };
            let (cx, step) = match entry {
                HostEntry::Resume(resume, outcome) => {
                    let outcome = match outcome {
                        Invocation::Value(value) => GuestOutcome::Returned(wrap(value)),
                        Invocation::Thrown(value) => GuestOutcome::Thrown(wrap(value)),
                    };
                    match resume {
                        ResumeCall::Plain(resume) => {
                            let step = resume(&mut cx, outcome);
                            (cx, step)
                        }
                        ResumeCall::Module(resume) => {
                            resume(ModuleDispatch::new(cx, owner), outcome)
                        }
                    }
                }
                HostEntry::Call(ExportCall::Plain(call)) => {
                    let step = call(&mut cx);
                    (cx, step)
                }
                HostEntry::Call(ExportCall::Module(call)) => call(ModuleDispatch::new(cx, owner)),
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
                Err(JsError::TypeError(message)) => {
                    return cx
                        .vm
                        .type_error(cx.roots, &message)
                        .map(Invocation::Thrown)
                        .map(Some);
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
                    (None, ResumeCall::Plain(resume))
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
                } => self.invoke(
                    host,
                    &roots.with(&saved),
                    &callee,
                    Arguments::Values(&args),
                    &receiver,
                )?,
                Action::Gc => {
                    let removed = self.collect_active(host, &roots.with(&saved))?;
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
                            None,
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
            self.remove_operation(operation);
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
            self.report_exception(host, roots, error)?;
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
        self.invoke(
            host,
            &ActiveRoots::EMPTY,
            &callee,
            Arguments::Values(&args),
            &Value::UNDEFINED,
        )?
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

    /// Runs queued VM microtasks and ordinary tasks. External timer/event
    /// delivery belongs to the host; it should call this after completing an operation.
    pub fn run_until_idle(&mut self, host: &mut dyn Host) -> Result<(), VmError> {
        loop {
            self.run_microtask_checkpoint(host)?;
            let delivered_subtle = self.pump_subtle_completion();
            if delivered_subtle {
                self.run_microtask_checkpoint(host)?;
            }
            let Some(task) = self.ordinary_tasks.pop_front() else {
                // A delivered completion can unblock other ready requests.
                if delivered_subtle {
                    continue;
                }
                return Ok(());
            };
            match task {
                OrdinaryTask::StreamMessage(id, sequence) => {
                    self.deliver_stream_message(host, id, sequence)?
                }
                OrdinaryTask::AbortTimeout(signal) => {
                    let reason = self.alloc_dom_exception(
                        "TimeoutError".into(),
                        "The operation timed out".into(),
                    );
                    self.abort_signal(host, &ActiveRoots::EMPTY, signal, reason)?;
                }
                OrdinaryTask::ScriptTimer(key) => self.run_script_timer(host, key)?,
                OrdinaryTask::UnhandledBatch(batch) => self.run_unhandled_batch(host, batch)?,
                OrdinaryTask::RejectionHandled(id) => {
                    self.run_rejection_notification(host, &ActiveRoots::EMPTY, id, true)?
                }
                OrdinaryTask::FetchEvent(id, event) => self.process_fetch_event(host, id, event)?,
            }
        }
    }

    fn run_microtask_checkpoint(&mut self, host: &mut dyn Host) -> Result<(), VmError> {
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
                Microtask::DisposeNext {
                    state,
                    result,
                    ref settlement,
                } => self.resume_disposal(host, roots, state, result, settlement)?,
                Microtask::ArrayFromAsync {
                    ref state,
                    ref settlement,
                } => {
                    self.resume_array_from_async(host, roots, state, settlement)?;
                }
                Microtask::Streams { .. } => {
                    if let Microtask::Streams { job, settlement } = &task {
                        self.resume_stream_job(host, roots, *job, settlement)?;
                    }
                }
                Microtask::FetchBody {
                    ref job,
                    ref settlement,
                } => {
                    self.resume_fetch_body(host, roots, job.clone(), settlement.clone())?;
                }
                Microtask::FetchUpload { id, ref settlement } => {
                    self.resume_fetch_upload(host, id, settlement)?;
                }
                Microtask::Call {
                    ref function,
                    ref args,
                } => self.run_queue_call(host, roots, function, args)?,
            }
        }
        if !self.pending_rejection_notifications.is_empty() {
            self.ordinary_tasks
                .push_back(OrdinaryTask::UnhandledBatch(std::mem::take(
                    &mut self.pending_rejection_notifications,
                )));
        }
        Ok(())
    }

    fn run_unhandled_batch(
        &mut self,
        host: &mut dyn Host,
        mut batch: Vec<ObjectId>,
    ) -> Result<(), VmError> {
        for (index, &id) in batch.iter().enumerate() {
            let remaining = &batch[index..];
            let roots = &ActiveRoots::EMPTY.with(&remaining);
            if let Err(error) = self.run_rejection_notification(host, roots, id, false) {
                // Preserve unstarted entries ahead of any work queued by the listener.
                if index + 1 < batch.len() {
                    self.ordinary_tasks
                        .push_front(OrdinaryTask::UnhandledBatch(batch.split_off(index + 1)));
                }
                return Err(error);
            }
        }
        Ok(())
    }

    fn run_rejection_notification(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        handled: bool,
    ) -> Result<(), VmError> {
        let promise = self.heap.promise_mut(id)?;
        let reason = match (&promise.state, promise.rejection_status, handled) {
            (PromiseState::Rejected(reason), RejectionStatus::PendingReport, false) => {
                reason.clone()
            }
            (PromiseState::Rejected(_), RejectionStatus::Handled, false) => return Ok(()),
            (PromiseState::Rejected(reason), RejectionStatus::PendingHandled, true) => {
                promise.rejection_status = RejectionStatus::Handled;
                reason.clone()
            }
            _ => {
                return Err(VmError::InvalidState(format!(
                    "invalid rejection notification for Promise {id}"
                )));
            }
        };
        let ty = if handled {
            "rejectionhandled"
        } else {
            "unhandledrejection"
        };
        let live = (id, &reason);
        let roots = &roots.with(&live);
        let result = self
            .new_global_notification(
                host,
                ty,
                !handled,
                EventPayload::PromiseRejectionEvent {
                    promise: id,
                    reason: reason.clone(),
                },
            )
            .and_then(|event| self.dispatch_global_notification(host, roots, event));
        if !handled {
            // A synchronous observation during dispatch wins even if the event
            // was not canceled. Do not leave a consumed candidate pending on a fault.
            let status = &mut self.heap.promise_mut(id)?.rejection_status;
            if *status == RejectionStatus::PendingReport {
                *status = RejectionStatus::Reported;
            }
        }
        if result? && !handled {
            host.unhandled_rejection(self.host_value(Value::object(id)), self.host_value(reason));
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
        let operation = self
            .operations
            .get(&token.id)
            .ok_or_else(|| VmError::UnknownOperation(token.id().to_string()))?;
        if matches!(operation.target, DeliveryTarget::Timeout { .. }) {
            return Err(VmError::InvalidState(
                "timer token requires wake_timer".into(),
            ));
        }
        let (value, rejected) = match completion {
            Completion::Fulfilled(value) => (value, false),
            Completion::Rejected(value) => (value, true),
        };
        let value = value
            .into_constant()
            .map_err(|error| VmError::Host(error.0))?;
        let operation = self.remove_operation(token.id).expect("operation exists");
        let target = operation.target;
        drop(operation);
        let settlement = if rejected {
            Settlement::Rejected(value)
        } else {
            Settlement::Fulfilled(value)
        };
        self.deliver_operation(target, settlement)
    }

    pub(crate) fn abort_operation(&mut self, id: u64, reason: Value) -> Result<(), VmError> {
        let Some(operation) = self.remove_operation(id) else {
            self.abort_fetch(id, reason)?;
            return Ok(());
        };
        let target = operation.target;
        drop(operation);
        self.deliver_operation(target, Settlement::Rejected(reason))
    }

    fn remove_operation(&mut self, id: u64) -> Option<PendingOperation> {
        let operation = self.operations.remove(&id)?;
        self.recovered_operations.remove(&id);
        if let DeliveryTarget::Timeout {
            deadline, sequence, ..
        } = operation.target
        {
            self.timer_deadlines.remove(&(deadline, sequence));
        }
        self.unlink_operation_signal(id, operation.signal);
        Some(operation)
    }

    pub(crate) fn unlink_operation_signal(&mut self, id: u64, signal: Option<ObjectId>) {
        if let Some(signal) = signal {
            self.abort_signal_state_mut(signal)
                .expect("operation signal exists")
                .expect("operation signal has AbortSignal brand")
                .steps
                .retain(|step| {
                    !matches!(step, crate::heap::AbortStep::Operation(linked) if *linked == id)
                });
        }
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
            DeliveryTarget::Timeout { .. } => Err(VmError::InvalidState(
                "timeout cannot receive a settlement".into(),
            )),
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
                recovery: operation.recovery,
                state: operation.state.clone(),
            })
            .collect()
    }

    /// Rearms recovered `Resume` / `Reissue` operations after publication.
    /// Driver failures deliver their declared rejection; a timer service fault
    /// is returned to the host without consuming other recovered operations.
    pub fn start_recovered_operations(&mut self) -> Result<(), VmError> {
        if self.recovered_operations.is_empty() {
            return Ok(());
        }
        let elapsed = self.clock.elapsed();
        self.queue_due_timers(elapsed);
        while let Some(id) = self.recovered_operations.pop_first() {
            let Some(operation) = self.operations.get_mut(&id) else {
                continue;
            };
            let token = OperationToken {
                epoch: self.epoch,
                id,
            };
            if let DeliveryTarget::Timeout {
                target, deadline, ..
            } = operation.target
            {
                let remaining = Self::timeout_remaining(deadline, elapsed);
                match Self::arm_timeout(&self.bindings, token, remaining) {
                    Ok(live) => operation.live = Some(live),
                    Err(fault) => {
                        self.remove_operation(id);
                        self.discard_timer(target);
                        return Err(VmError::HostFault(fault.0));
                    }
                }
                continue;
            }
            let decl = operation
                .declaration
                .expect("driver operation has a declaration");
            if let Ok(live) = self
                .bindings
                .resume_operation(decl, &operation.state, token)
            {
                operation.live = Some(live);
            } else {
                let target = operation.target;
                let rejection = operation.rejection;
                self.remove_operation(id);
                self.deliver_operation(
                    target,
                    Settlement::Rejected(Value::string(rejection.into())),
                )?;
            }
        }
        Ok(())
    }

    pub(crate) fn schedule_timeout(
        &mut self,
        signal: ObjectId,
        delay_ms: u64,
    ) -> Result<(), HostFault> {
        let deadline = self
            .clock
            .elapsed()
            .saturating_add(std::time::Duration::from_millis(delay_ms));
        let sequence = self.next_operation_id;
        self.arm_new_timeout(TimerTarget::Abort(signal), deadline, delay_ms, sequence)?;
        Ok(())
    }

    fn timeout_remaining(deadline: std::time::Duration, elapsed: std::time::Duration) -> u64 {
        let remaining = deadline.saturating_sub(elapsed);
        u64::try_from(remaining.as_nanos().div_ceil(1_000_000))
            .expect("timeout remaining delay fits u64")
    }

    fn arm_timeout(
        bindings: &Bindings,
        token: OperationToken,
        delay_ms: u64,
    ) -> Result<Box<dyn LiveRegistration>, HostFault> {
        bindings
            .service::<TimerKey>()
            .ok_or_else(|| HostFault::new("timer service is unavailable"))?
            .clone()
            .arm(token, delay_ms)
    }

    fn allocate_operation_token(&mut self) -> Result<OperationToken, HostFault> {
        let id = self.next_operation_id;
        self.next_operation_id = id
            .checked_add(1)
            .ok_or_else(|| HostFault::new("operation id space exhausted"))?;
        Ok(OperationToken {
            epoch: self.epoch,
            id,
        })
    }

    pub(crate) fn register_operation(
        &mut self,
        decl: usize,
        state: Vec<u8>,
        target: DeliveryTarget,
        signal: Option<ObjectId>,
        arm: impl FnOnce(
            OperationToken,
            &Bindings,
            &[u8],
        ) -> Result<Box<dyn LiveRegistration>, HostFault>,
    ) -> Result<OperationToken, HostFault> {
        let token = self.allocate_operation_token()?;
        let id = token.id;
        let (name, recovery, rejection) = self.bindings.operation_metadata(decl);
        self.operations.insert(
            id,
            PendingOperation {
                name: name.to_owned(),
                recovery,
                rejection,
                state,
                target,
                signal,
                declaration: Some(decl),
                live: None,
            },
        );
        if let Some(signal) = signal {
            self.abort_signal_state_mut(signal)
                .expect("operation signal exists")
                .expect("operation signal has AbortSignal brand")
                .steps
                .push(crate::heap::AbortStep::Operation(id));
        }
        let operation = self.operations.get_mut(&id).expect("operation inserted");
        match arm(token, &self.bindings, &operation.state) {
            Ok(live) => {
                operation.live = Some(live);
                Ok(token)
            }
            Err(fault) => {
                self.remove_operation(id);
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
            .0
            .functions
            .indices
            .get(func_id)
            .ok_or_else(|| VmError::InvalidState(format!("function {func_id} does not exist")))
    }

    pub(crate) fn is_initializer(&self, function_index: usize) -> bool {
        self.build.0.functions.calls[function_index].is_initializer
    }

    pub(crate) fn module_needs_environment(&self, module_index: usize) -> bool {
        self.build.0.functions.module_environments[module_index]
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
            promise_object.stream_dependency = None;
            if matches!(settlement, Settlement::Rejected(_))
                && promise_object.rejection_status == RejectionStatus::Unobserved
            {
                promise_object.rejection_status = RejectionStatus::PendingReport;
                self.pending_rejection_notifications.push(promise);
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
        self.mark_promise_handled(promise)
    }

    pub(crate) fn mark_promise_handled(&mut self, promise: ObjectId) -> Result<(), VmError> {
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
                self.ordinary_tasks
                    .push_back(OrdinaryTask::RejectionHandled(promise));
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
            ReactionJob::DisposeNext { state, result } => {
                self.microtasks.push_back(Microtask::DisposeNext {
                    state,
                    result,
                    settlement,
                });
            }
            ReactionJob::ArrayFromAsync(state) => {
                self.microtasks
                    .push_back(Microtask::ArrayFromAsync { state, settlement });
            }
            ReactionJob::Streams(job) => {
                self.microtasks
                    .push_back(Microtask::Streams { job, settlement });
            }
            ReactionJob::FetchBody(job) => {
                self.microtasks
                    .push_back(Microtask::FetchBody { job, settlement });
            }
            ReactionJob::FetchUpload(id) => {
                self.microtasks
                    .push_back(Microtask::FetchUpload { id, settlement });
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
        args: Arguments<'_>,
        receiver: &Value,
        (new_target, home): (&Value, Option<ObjectId>),
        callee: Option<ObjectId>,
    ) -> Result<Invocation, VmError> {
        let function_index = self.function_index(func_id)?;
        if self.is_initializer(function_index) {
            return Err(VmError::InvalidState(
                "script function is not callable".to_owned(),
            ));
        }
        self.activate(
            host,
            roots,
            function_index,
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
    pub(crate) fn activate(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        function_index: usize,
        parent_env: Option<ObjectId>,
        prepared_env: Option<ObjectId>,
        args: Arguments<'_>,
        (receiver, new_target, home): (&Value, &Value, Option<ObjectId>),
        callee: Option<ObjectId>,
    ) -> Result<Invocation, VmError> {
        let function = &self.build.functions[function_index];
        // An async generator's call returns the generator object, so it has no
        // activation result Promise; each request owns its own instead.
        let result_promise =
            (function.is_async && !function.is_generator).then(|| self.heap.alloc_promise());

        let mut frame = Frame::new(
            function_index,
            function,
            None,
            result_promise.map(ContinuationTarget::Promise),
        );
        frame.callee = callee;

        // Entry initialization only mutates the frame and heap; it cannot run
        // guest code or collect before execute_frame takes over rooting.
        let mut bindings = BTreeMap::new();
        let mut bind = |param: &ParamTarget, value| match *param {
            ParamTarget::Binding(binding) => {
                bindings.insert(binding, value);
                Ok(())
            }
            ParamTarget::Slot(slot) => frame.write(slot, value),
        };
        if let Some(param) = &function.this_param {
            bind(param, receiver.clone())?;
        }
        if let Some(param) = &function.new_target_param {
            bind(param, new_target.clone())?;
        }
        if let Some(param) = &function.home_param {
            let home =
                home.ok_or_else(|| VmError::InvalidState("function has no home object".into()))?;
            bind(param, Value::object(home))?;
        }
        if let Some(param) = &function.arguments_param {
            let values = args.to_vec()?;
            let iterator = self.builtins.get("array.values");
            bind(
                param,
                Self::create_arguments_object(&mut self.heap, iterator, &values)?,
            )?;
        }
        for (index, param) in function.params.iter().enumerate() {
            bind(param, args.get(index)?.clone())?;
        }
        if let Some(param) = &function.rest_param {
            let rest = self.heap.alloc_array(args.tail(function.params.len())?);
            bind(param, Value::object(rest))?;
        }
        frame.env = if let Some(env) = prepared_env {
            if !bindings.is_empty() {
                self.heap.environment_mut(env)?.bindings.extend(bindings);
            }
            Some(env)
        } else if self.build.0.functions.calls[function_index].needs_environment {
            Some(self.heap.alloc_environment(parent_env, bindings))
        } else {
            None
        };
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

    /// The wall-clock source linked for this VM.
    pub(crate) fn time(&self) -> &dyn crate::host::TimeService {
        &*self.time.0
    }

    pub(crate) fn restored(build: VerifiedBuild, bindings: Bindings, state: RestoredState) -> Self {
        let RestoredState {
            mut heap,
            continuations,
            operations,
            next_operation_id,
            script_timers,
            console,
            next_timer_id,
            next_stream_message,
            clock,
            base_url,
            fetch_environment,
            fetch_state,
            object_urls,
            host_tasks,
            next_host_task_id,
            host_classes,
            host_instances,
            module_states,
            globals,
            builtins,
            templates,
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
        let timer_deadlines = operations
            .iter()
            .filter_map(|(&id, op)| match op.target {
                DeliveryTarget::Timeout {
                    deadline, sequence, ..
                } => Some(((deadline, sequence), id)),
                _ => None,
            })
            .collect();
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
        let caches = property_caches(&build);
        let time = time_source(&bindings);
        Self {
            property_caches: caches,
            build,
            bindings,
            heap,
            continuations,
            operations,
            host_tasks,
            next_host_task_id,
            recovered_operations,
            ordinary_tasks: VecDeque::new(),
            next_stream_message,
            script_timers,
            console,
            next_timer_id,
            timer_nesting: 0,
            timer_deadlines,
            microtasks: VecDeque::new(),
            stack_registrations: Vec::new(),
            pending_rejection_notifications: Vec::new(),
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
            active_event_paths: Vec::new(),
            reporting_exception: false,
            builtins,
            templates,
            random_state: crate::builtin::math::RANDOM_SEED,
            clock,
            time,
            base_url,
            fetch_environment,
            fetch_state,
            fetch_live: Default::default(),
            subtle_state: Default::default(),
            object_urls,
            module_envs,
            module_namespaces,
        }
    }
}
