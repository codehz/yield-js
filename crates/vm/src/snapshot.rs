use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::bytecode::{BYTECODE_VERSION, Build};
use crate::error::VmError;
use crate::heap::{CombineState, EnvRole, Heap, Object, OwnedEdge, PromiseState, ReactionJob};
use crate::host::{Host, PendingOperationMeta};
use crate::runtime::{Continuation, PendingOperation};
use crate::value::{ContinuationId, ObjectId, Value};
use crate::vm::Vm;

pub const SNAPSHOT_FORMAT_VERSION: u32 = 4;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectRecord {
    pub id: ObjectId,
    pub object: Object,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContinuationRecord {
    pub id: ContinuationId,
    /// Async-function result Promise.
    pub async_promise: ObjectId,
    pub func_id: u32,
    pub resume_id: u32,
    /// Promise the continuation waits on. Only recoverable, unsettled promises
    /// are persisted, so this is required for every restored continuation.
    pub waiting_on: ObjectId,
    /// Invocation environment of the suspended frame. Required exactly when
    /// [`crate::bytecode::Function::needs_environment`] is true.
    pub env: Option<ObjectId>,
    /// Values in `ResumeEntry::saved_slots` order.
    pub values: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingOperationRecord {
    pub id: String,
    pub import: String,
    pub promise: ObjectId,
}

/// Host-held script roots live outside the VM. The snapshot deliberately does
/// not contain native pointers, host callbacks, or pending microtasks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub format_version: u32,
    pub bytecode_version: u32,
    pub build_id: String,
    pub objects: Vec<ObjectRecord>,
    pub continuations: Vec<ContinuationRecord>,
    pub operations: Vec<PendingOperationRecord>,
    pub globals: BTreeMap<String, Value>,
    pub combines: BTreeMap<ObjectId, CombineState>,
}

impl Snapshot {
    pub fn new(build_id: String) -> Self {
        Self {
            format_version: SNAPSHOT_FORMAT_VERSION,
            bytecode_version: BYTECODE_VERSION,
            build_id,
            objects: Vec::new(),
            continuations: Vec::new(),
            operations: Vec::new(),
            globals: BTreeMap::new(),
            combines: BTreeMap::new(),
        }
    }

    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|error| error.to_string())
    }

    pub fn from_json(source: &str) -> Result<Self, String> {
        serde_json::from_str(source).map_err(|error| error.to_string())
    }
}

impl Vm {
    pub fn snapshot(&self, roots: &[Value]) -> Result<Snapshot, VmError> {
        if !self.microtasks.is_empty() {
            return Err(VmError::InvalidState(
                "snapshot requires a drained microtask queue".to_owned(),
            ));
        }

        let mut tracer = Tracer::new(&self.heap, &self.continuations);
        for root in roots {
            tracer.mark_value(root)?;
        }
        for value in self.globals.values() {
            tracer.mark_value(value)?;
        }
        for operation in self.operations.values() {
            tracer.mark_object(operation.promise)?;
        }
        tracer.drain()?;
        if self
            .heap
            .resolutions
            .iter()
            .any(|id| !tracer.objects.contains(id))
        {
            return Err(VmError::InvalidState(
                "snapshot during pending thenable assimilation".to_owned(),
            ));
        }

        let mut snapshot = Snapshot::new(self.build.build_id.clone());
        snapshot.globals = self.globals.clone();
        for id in &tracer.objects {
            snapshot.objects.push(ObjectRecord {
                id: *id,
                object: self.heap.cloned(*id)?,
            });
        }
        for id in tracer.continuations_seen {
            let continuation = self.continuations.get(&id).ok_or_else(|| {
                VmError::InvalidState(format!("continuation {id} does not exist"))
            })?;
            let waiting_on = continuation.waiting_on.ok_or_else(|| {
                VmError::InvalidState(format!(
                    "continuation {id} waits on a plain value and cannot be snapshotted"
                ))
            })?;
            snapshot.continuations.push(ContinuationRecord {
                id,
                async_promise: continuation.async_promise,
                func_id: continuation.func_id,
                resume_id: continuation.resume_id,
                waiting_on,
                env: continuation.env,
                values: continuation.values.clone(),
            });
        }
        for operation in self.operations.values() {
            snapshot.operations.push(PendingOperationRecord {
                id: operation.meta.id.clone(),
                import: operation.meta.import.clone(),
                promise: operation.promise,
            });
        }
        for (id, state) in &self.heap.combines {
            if tracer.objects.contains(id) {
                snapshot.combines.insert(*id, state.clone());
            }
        }
        Ok(snapshot)
    }

    pub fn restore(snapshot: Snapshot, build: Build, host: &mut dyn Host) -> Result<Self, VmError> {
        check_versions(&snapshot)?;
        build.validate().map_err(VmError::InvalidBuild)?;
        if snapshot.build_id != build.build_id {
            return Err(VmError::InvalidSnapshot(format!(
                "build id mismatch: snapshot {}, build {}",
                snapshot.build_id, build.build_id
            )));
        }
        Self::check_imports(&build, host)?;

        let mut heap = Heap::new();
        for record in &snapshot.objects {
            heap.insert_restore(record.id, record.object.clone())?;
        }
        check_heap_edges(&heap, host)?;
        require_value_objects(&heap, snapshot.globals.values())?;
        let continuations = load_continuations(&build, &heap, &snapshot.continuations)?;
        for continuation in continuations.values() {
            require_value_objects(&heap, continuation.values.iter())?;
        }
        let operations = load_operations(&build, &heap, &snapshot.operations, host)?;
        heap.combines = snapshot.combines.clone();
        check_heap_edges(&heap, host)?;
        validate_jobs(&heap)?;
        validate_combines(&heap)?;
        Ok(Self::restored(
            build,
            heap,
            continuations,
            operations,
            snapshot.globals,
        ))
    }
}

struct Tracer<'a> {
    heap: &'a Heap,
    continuations: &'a BTreeMap<ContinuationId, Continuation>,
    objects: BTreeSet<ObjectId>,
    continuations_seen: BTreeSet<ContinuationId>,
    object_queue: VecDeque<ObjectId>,
    continuation_queue: VecDeque<ContinuationId>,
    edges: Vec<OwnedEdge>,
}

impl<'a> Tracer<'a> {
    fn new(heap: &'a Heap, continuations: &'a BTreeMap<ContinuationId, Continuation>) -> Self {
        Self {
            heap,
            continuations,
            objects: BTreeSet::new(),
            continuations_seen: BTreeSet::new(),
            object_queue: VecDeque::new(),
            continuation_queue: VecDeque::new(),
            edges: Vec::new(),
        }
    }

    fn mark_value(&mut self, value: &Value) -> Result<(), VmError> {
        if let Value::Object(id) = value {
            self.mark_object(*id)?;
        }
        Ok(())
    }

    fn mark_object(&mut self, id: ObjectId) -> Result<(), VmError> {
        if !self.heap.contains(id) {
            return Err(VmError::InvalidState(format!(
                "snapshot root references missing object {id}"
            )));
        }
        if self.objects.insert(id) {
            self.object_queue.push_back(id);
        }
        Ok(())
    }

    fn mark_continuation(&mut self, promise: ObjectId, id: ContinuationId) -> Result<(), VmError> {
        if !self.continuations.contains_key(&id) {
            return Err(VmError::InvalidState(format!(
                "Promise {promise} references missing continuation {id}"
            )));
        }
        if self.continuations_seen.insert(id) {
            self.continuation_queue.push_back(id);
        }
        Ok(())
    }

    fn drain(&mut self) -> Result<(), VmError> {
        loop {
            let mut progressed = false;
            while let Some(id) = self.object_queue.pop_front() {
                progressed = true;
                self.walk_object(id)?;
            }
            while let Some(id) = self.continuation_queue.pop_front() {
                progressed = true;
                self.walk_continuation(id)?;
            }
            if !progressed {
                break;
            }
        }
        Ok(())
    }

    fn walk_object(&mut self, id: ObjectId) -> Result<(), VmError> {
        let mut edges = std::mem::take(&mut self.edges);
        self.heap.write_edges(id, &mut edges)?;
        for edge in &edges {
            match *edge {
                OwnedEdge::Object(target) | OwnedEdge::Environment { id: target, .. } => {
                    self.mark_object(target)?;
                }
                OwnedEdge::Continuation(continuation) => {
                    self.mark_continuation(id, continuation)?;
                }
            }
        }
        edges.clear();
        self.edges = edges;
        Ok(())
    }

    fn walk_continuation(&mut self, id: ContinuationId) -> Result<(), VmError> {
        let objects = {
            let continuation = self.continuations.get(&id).ok_or_else(|| {
                VmError::InvalidState(format!("continuation {id} does not exist"))
            })?;
            let mut objects = Vec::new();
            continuation.for_each_object(|object| objects.push(object));
            objects
        };
        for object in objects {
            self.mark_object(object)?;
        }
        Ok(())
    }
}

fn check_versions(snapshot: &Snapshot) -> Result<(), VmError> {
    if snapshot.format_version != SNAPSHOT_FORMAT_VERSION {
        return Err(VmError::InvalidSnapshot(format!(
            "unsupported snapshot format {}, expected {}",
            snapshot.format_version, SNAPSHOT_FORMAT_VERSION
        )));
    }
    if snapshot.bytecode_version != BYTECODE_VERSION {
        return Err(VmError::InvalidSnapshot(format!(
            "unsupported bytecode version {}, expected {}",
            snapshot.bytecode_version, BYTECODE_VERSION
        )));
    }
    Ok(())
}

fn check_heap_edges(heap: &Heap, host: &dyn Host) -> Result<(), VmError> {
    let mut edges = Vec::new();
    for id in heap.ids() {
        if let Object::Native(native) = heap.get(id)?
            && !host.has_native(&native.key)
        {
            return Err(VmError::MissingNative(native.key.clone()));
        }
        heap.write_edges(id, &mut edges)?;
        for edge in edges.drain(..) {
            match edge {
                OwnedEdge::Object(target) => {
                    if !heap.contains(target) {
                        return Err(VmError::InvalidSnapshot(format!(
                            "value references missing object {target}"
                        )));
                    }
                }
                OwnedEdge::Environment { id: env, role } => {
                    require_environment(heap, id, env, role)?;
                }
                OwnedEdge::Continuation(_) => {}
            }
        }
    }
    Ok(())
}

fn require_environment(
    heap: &Heap,
    owner: ObjectId,
    env: ObjectId,
    role: EnvRole,
) -> Result<(), VmError> {
    match heap.is_environment(env) {
        Some(true) => Ok(()),
        Some(false) => Err(VmError::InvalidSnapshot(match role {
            EnvRole::Closure => {
                format!("function object {owner} references non-environment {env}")
            }
            EnvRole::Parent => {
                format!("environment object {owner} references non-environment parent {env}")
            }
        })),
        None => Err(VmError::InvalidSnapshot(match role {
            EnvRole::Closure => {
                format!("function object {owner} references missing environment {env}")
            }
            EnvRole::Parent => {
                format!("environment object {owner} references missing parent {env}")
            }
        })),
    }
}

fn require_value_objects<'a>(
    heap: &Heap,
    values: impl IntoIterator<Item = &'a Value>,
) -> Result<(), VmError> {
    for value in values {
        if let Value::Object(id) = value
            && !heap.contains(*id)
        {
            return Err(VmError::InvalidSnapshot(format!(
                "value references missing object {id}"
            )));
        }
    }
    Ok(())
}

fn load_continuations(
    build: &Build,
    heap: &Heap,
    records: &[ContinuationRecord],
) -> Result<BTreeMap<ContinuationId, Continuation>, VmError> {
    let mut indexed = BTreeMap::new();
    for record in records {
        if indexed.insert(record.id, record.clone()).is_some() {
            return Err(VmError::InvalidSnapshot(format!(
                "duplicate continuation id {}",
                record.id
            )));
        }
        validate_continuation(build, heap, record)?;
    }
    for (id, object) in heap.iter() {
        let Object::Promise(promise) = object else {
            continue;
        };
        for job in &promise.reactions {
            let Some(continuation) = job.await_id() else {
                continue;
            };
            let Some(record) = indexed.get(&continuation) else {
                return Err(VmError::InvalidSnapshot(format!(
                    "Promise {id} references missing continuation {continuation}"
                )));
            };
            if record.waiting_on != id {
                return Err(VmError::InvalidSnapshot(format!(
                    "Promise {id} reaction {continuation} waits on {}",
                    record.waiting_on
                )));
            }
        }
    }
    Ok(indexed
        .into_values()
        .map(|record| {
            (
                record.id,
                Continuation {
                    async_promise: record.async_promise,
                    func_id: record.func_id,
                    resume_id: record.resume_id,
                    waiting_on: Some(record.waiting_on),
                    env: record.env,
                    values: record.values,
                },
            )
        })
        .collect())
}

fn validate_continuation(
    build: &Build,
    heap: &Heap,
    record: &ContinuationRecord,
) -> Result<(), VmError> {
    let function = build.function(record.func_id).ok_or_else(|| {
        VmError::InvalidSnapshot(format!(
            "continuation {} references missing function {}",
            record.id, record.func_id
        ))
    })?;
    let resume = function
        .resumes
        .get(record.resume_id as usize)
        .ok_or_else(|| {
            VmError::InvalidSnapshot(format!(
                "continuation {} references missing resume {}",
                record.id, record.resume_id
            ))
        })?;
    if resume.saved_slots.len() != record.values.len() {
        return Err(VmError::InvalidSnapshot(format!(
            "continuation {} has {} values, expected {}",
            record.id,
            record.values.len(),
            resume.saved_slots.len()
        )));
    }
    if function.needs_environment() {
        let Some(env) = record.env else {
            return Err(VmError::InvalidSnapshot(format!(
                "continuation {} has no invocation environment",
                record.id
            )));
        };
        if !matches!(heap.is_environment(env), Some(true)) {
            let missing = heap.is_environment(env).is_none();
            return Err(VmError::InvalidSnapshot(if missing {
                format!(
                    "continuation {} references missing environment {env}",
                    record.id
                )
            } else {
                format!(
                    "continuation {} environment {env} is not an environment",
                    record.id
                )
            }));
        }
    } else if let Some(env) = record.env {
        return Err(VmError::InvalidSnapshot(format!(
            "continuation {} does not use an invocation environment, found {env}",
            record.id
        )));
    }
    match heap.get(record.async_promise) {
        Ok(Object::Promise(promise)) if matches!(promise.state, PromiseState::Pending) => {}
        _ => {
            return Err(VmError::InvalidSnapshot(format!(
                "continuation {} async result {} is not a pending Promise",
                record.id, record.async_promise
            )));
        }
    }
    let waiting = match heap.get(record.waiting_on) {
        Ok(Object::Promise(promise)) => promise,
        _ => {
            return Err(VmError::InvalidSnapshot(format!(
                "continuation {} waits on non-Promise object {}",
                record.id, record.waiting_on
            )));
        }
    };
    if !matches!(waiting.state, PromiseState::Pending) {
        return Err(VmError::InvalidSnapshot(format!(
            "continuation {} waits on a settled Promise",
            record.id
        )));
    }
    let occurrences = waiting
        .reactions
        .iter()
        .filter(|job| job.await_id() == Some(record.id))
        .count();
    if occurrences != 1 {
        return Err(VmError::InvalidSnapshot(format!(
            "continuation {} must appear exactly once in Promise {} reactions, found {occurrences}",
            record.id, record.waiting_on
        )));
    }
    Ok(())
}

fn validate_jobs(heap: &Heap) -> Result<(), VmError> {
    let mut adopts: BTreeMap<ObjectId, usize> = BTreeMap::new();
    for (id, object) in heap.iter() {
        match object {
            Object::Promise(promise) => {
                for job in &promise.reactions {
                    match job {
                        ReactionJob::Await(_) => {}
                        ReactionJob::Script { result, .. }
                        | ReactionJob::Finalize { result, .. }
                        | ReactionJob::Adopt { result }
                        | ReactionJob::Combine { result, .. } => {
                            require_promise(heap, *result, &format!("Promise {id} reaction"))?;
                        }
                    }
                    if let ReactionJob::Adopt { result } = job {
                        *adopts.entry(*result).or_default() += 1;
                    }
                }
            }
            Object::Resolution(resolution) => {
                require_promise(heap, resolution.promise, &format!("Resolution {id}"))?;
                if !matches!(heap.get(resolution.pair), Ok(Object::Resolution(_))) {
                    return Err(VmError::InvalidSnapshot(format!(
                        "Resolution {id} pair {} is not a resolution function",
                        resolution.pair
                    )));
                }
            }
            Object::BuiltinMethod(method) => {
                require_promise(heap, method.receiver, &format!("builtin method {id}"))?;
            }
            _ => {}
        }
    }
    for (id, object) in heap.iter() {
        let Object::Promise(promise) = object else {
            continue;
        };
        if promise.locked && matches!(promise.state, PromiseState::Pending) {
            let count = adopts.get(&id).copied().unwrap_or(0);
            if count != 1 {
                return Err(VmError::InvalidSnapshot(format!(
                    "locked Promise {id} must have exactly one Adopt reaction, found {count}"
                )));
            }
        }
        if let Some(source) = promise.adopting {
            require_promise(heap, source, &format!("Promise {id} adopting"))?;
            let linked = heap
                .promise(source)?
                .reactions
                .iter()
                .any(|job| matches!(job, ReactionJob::Adopt { result } if *result == id));
            if !linked && matches!(heap.promise(source)?.state, PromiseState::Pending) {
                return Err(VmError::InvalidSnapshot(format!(
                    "Promise {id} adopts pending Promise {source} without an Adopt reaction"
                )));
            }
        }
    }
    Ok(())
}

fn validate_combines(heap: &Heap) -> Result<(), VmError> {
    for (id, state) in &heap.combines {
        let promise = heap.promise(*id).map_err(|_| {
            VmError::InvalidSnapshot(format!("combine key {id} is not a pending Promise"))
        })?;
        if !matches!(promise.state, PromiseState::Pending) {
            return Err(VmError::InvalidSnapshot(format!(
                "combine key {id} is not a pending Promise"
            )));
        }
        if state.remaining > state.count
            || state.results.len() != state.count
            || state.elements.len() != state.count
        {
            return Err(VmError::InvalidSnapshot(format!(
                "combine {id} has inconsistent remaining/count/results"
            )));
        }
        let states_ok = match state.kind {
            crate::heap::CombineKind::AllSettled => state.states.len() == state.count,
            _ => state.states.is_empty(),
        };
        if !states_ok {
            return Err(VmError::InvalidSnapshot(format!(
                "combine {id} has inconsistent state records"
            )));
        }
        for element in &state.elements {
            require_promise(heap, *element, &format!("combine {id} element"))?;
        }
    }
    Ok(())
}

fn require_promise(heap: &Heap, id: ObjectId, what: &str) -> Result<(), VmError> {
    if heap.promise(id).is_err() {
        return Err(VmError::InvalidSnapshot(format!(
            "{what} references non-Promise object {id}"
        )));
    }
    Ok(())
}

fn load_operations(
    build: &Build,
    heap: &Heap,
    records: &[PendingOperationRecord],
    host: &mut dyn Host,
) -> Result<BTreeMap<String, PendingOperation>, VmError> {
    let mut operations = BTreeMap::new();
    let mut operation_promises = BTreeSet::new();
    for record in records {
        if operations.contains_key(&record.id) {
            return Err(VmError::InvalidSnapshot(format!(
                "duplicate host operation {:?}",
                record.id
            )));
        }
        if !build
            .imports
            .iter()
            .any(|import| import.name == record.import)
        {
            return Err(VmError::InvalidSnapshot(format!(
                "operation {:?} references missing import {:?}",
                record.id, record.import
            )));
        }
        match heap.get(record.promise) {
            Ok(Object::Promise(promise)) if matches!(promise.state, PromiseState::Pending) => {}
            _ => {
                return Err(VmError::InvalidSnapshot(format!(
                    "operation {:?} references non-pending Promise {}",
                    record.id, record.promise
                )));
            }
        }
        if !operation_promises.insert(record.promise) {
            return Err(VmError::InvalidSnapshot(format!(
                "Promise {} is associated with more than one host operation",
                record.promise
            )));
        }
        let meta = PendingOperationMeta {
            id: record.id.clone(),
            import: record.import.clone(),
        };
        if !host.rebuild_operation(&meta) {
            return Err(VmError::MissingOperation(record.id.clone()));
        }
        operations.insert(
            record.id.clone(),
            PendingOperation {
                meta,
                promise: record.promise,
            },
        );
    }
    Ok(operations)
}
