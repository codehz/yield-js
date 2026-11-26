//! Microtask job, combine, operation, and host task validation and loading.

use std::collections::{BTreeMap, BTreeSet};

use crate::binding::{Bindings, CpsBoundary, ExportDecl, NativeTarget, Recovery};
use crate::builtin::is_callable_object;
use crate::error::VmError;
use crate::heap::GeneratorStatus;
use crate::heap::{
    AbortStep, CombineKind, Heap, Object, PromiseCallback, PromiseState, ReactionJob,
};
use crate::runtime::{HOST_TASK_ID_LIMIT, HostInstance, HostTask, HostTaskState, PendingOperation};
use crate::value::{ObjectId, ValueRef};

use crate::snapshot::record::*;

pub(super) fn validate_jobs(heap: &Heap) -> Result<(), VmError> {
    let mut adopts: BTreeMap<ObjectId, usize> = BTreeMap::new();
    let mut closes: BTreeMap<ObjectId, Vec<(ObjectId, ObjectId)>> = BTreeMap::new();
    let mut disposals = BTreeMap::new();
    let mut array_jobs = BTreeSet::new();
    for (id, object) in heap.iter() {
        match object {
            Object::Promise(promise) => {
                if let Some(state) = promise.disposal
                    && (!matches!(promise.state, PromiseState::Pending)
                        || promise.adopting.is_some()
                        || !matches!(heap.get(state), Ok(Object::DisposeState(state)) if state.result == Some(id)))
                {
                    return Err(VmError::InvalidSnapshot(format!(
                        "invalid disposal backlink on Promise {id}"
                    )));
                }
                if let Some(active) = promise.body_read {
                    let valid = matches!(promise.state, PromiseState::Pending)
                        && matches!(heap.get(active), Ok(Object::Promise(read))
                            if matches!(read.state, PromiseState::Pending)
                                && read.reactions.iter().filter(|job| {
                                    matches!(job, ReactionJob::FetchBody(body)
                                        if body.active == active && body.promise == id)
                                }).count() == 1);
                    if !valid {
                        return Err(VmError::InvalidSnapshot(format!(
                            "Promise {id} has an invalid body read link"
                        )));
                    }
                }
                for job in &promise.reactions {
                    match job {
                        ReactionJob::Await(_) => {}
                        ReactionJob::Script {
                            resolve, reject, ..
                        } => {
                            for callback in [resolve, reject] {
                                if !matches!(callback.view(), ValueRef::Object(id) if heap.get(id).is_ok_and(crate::builtin::is_callable_object))
                                {
                                    return Err(VmError::InvalidSnapshot(format!(
                                        "Promise {id} reaction capability is not callable"
                                    )));
                                }
                            }
                        }
                        ReactionJob::Adopt { result } => {
                            require_promise(heap, *result, &format!("Promise {id} reaction"))?;
                        }
                        ReactionJob::GeneratorClose { generator, request } => {
                            require_promise(heap, *request, &format!("Promise {id} reaction"))?;
                            closes.entry(*generator).or_default().push((id, *request));
                        }
                        ReactionJob::DisposeNext { state, result } => {
                            if !matches!(promise.state, PromiseState::Pending)
                                || disposals.insert(*state, (id, *result)).is_some()
                            {
                                return Err(VmError::InvalidSnapshot(format!(
                                    "invalid disposal reaction on Promise {id}"
                                )));
                            }
                        }
                        ReactionJob::ArrayFromAsync(job) => {
                            if !matches!(promise.state, PromiseState::Pending)
                                || !job
                                    .validate(heap)
                                    .is_some_and(|target| array_jobs.insert(target))
                            {
                                return Err(VmError::InvalidSnapshot(format!(
                                    "invalid Array.fromAsync reaction on Promise {id}"
                                )));
                            }
                        }
                        ReactionJob::Streams(job) => {
                            if !crate::builtin::streams::validate_job(heap, id, *job) {
                                return Err(VmError::InvalidSnapshot(format!(
                                    "invalid stream reaction on Promise {id}"
                                )));
                            }
                        }
                        ReactionJob::FetchBody(job) => {
                            if !matches!(promise.state, PromiseState::Pending)
                                || !matches!(
                                    heap.get(job.promise),
                                    Ok(Object::Promise(promise)) if matches!(promise.state, PromiseState::Pending)
                                )
                                || !matches!(heap.get(job.reader), Ok(Object::ReadableReader(_)))
                                || !matches!(heap.get(job.active), Ok(Object::Promise(read))
                                    if matches!(read.state, PromiseState::Pending))
                                || job.total
                                    != job
                                        .chunks
                                        .iter()
                                        .map(|chunk| chunk.len() as u64)
                                        .sum::<u64>()
                            {
                                return Err(VmError::InvalidSnapshot(format!(
                                    "invalid body reaction on Promise {id}"
                                )));
                            }
                        }
                        ReactionJob::FetchUpload(_) => {}
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
            Object::DisposableStack {
                async_,
                disposed,
                records,
                ..
            } if (*disposed && !records.is_empty())
                || !records
                    .iter()
                    .all(|record| valid_disposal_record(heap, record, Some(*async_))) =>
            {
                return Err(VmError::InvalidSnapshot(format!(
                    "invalid disposable stack {id}"
                )));
            }
            _ => {}
        }
    }
    for (id, object) in heap.iter() {
        if let Object::DisposeState(state) = object {
            let reaction = disposals.remove(&id);
            let valid_links = match (state.result, state.waiting) {
                (None, None) => reaction.is_none(),
                (Some(result), Some(source)) => {
                    reaction == Some((source, result))
                        && matches!(heap.get(result), Ok(Object::Promise(promise)) if promise.disposal == Some(id))
                }
                _ => false,
            };
            if !valid_links
                || !state
                    .records
                    .iter()
                    .all(|record| valid_disposal_record(heap, record, state.result.map(|_| true)))
            {
                return Err(VmError::InvalidSnapshot(format!(
                    "invalid disposal state {id}"
                )));
            }
        }
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
    if !disposals.is_empty() {
        return Err(VmError::InvalidSnapshot(
            "disposal reaction has no disposal state".to_owned(),
        ));
    }
    for (generator, waits) in &closes {
        let record = heap.generator(*generator).ok_or_else(|| {
            VmError::InvalidSnapshot(format!(
                "Promise reaction targets missing generator {generator}"
            ))
        })?;
        let Some(head) = record.requests.front() else {
            return Err(VmError::InvalidSnapshot(format!(
                "generator {generator} has a close reaction without a request"
            )));
        };
        if waits.len() != 1 || waits[0].1 != head.promise {
            return Err(VmError::InvalidSnapshot(format!(
                "generator {generator} must own exactly one close reaction for its head request"
            )));
        }
        if record.status != GeneratorStatus::Closing {
            return Err(VmError::InvalidSnapshot(format!(
                "generator {generator} has a close reaction without closing"
            )));
        }
    }
    for (id, generator) in &heap.generators {
        if generator.status == GeneratorStatus::Closing && !closes.contains_key(id) {
            return Err(VmError::InvalidSnapshot(format!(
                "generator {id} is closing without a close reaction"
            )));
        }
    }
    Ok(())
}

pub(super) fn validate_combines(heap: &Heap) -> Result<(), VmError> {
    for (id, state) in &heap.combines {
        if !matches!(heap.get(*id), Ok(Object::Plain(_))) {
            return Err(VmError::InvalidSnapshot(format!(
                "combine key {id} is not a plain object"
            )));
        }
        if state.kind == CombineKind::Race {
            return Err(VmError::InvalidSnapshot(format!(
                "combine {id} has invalid Race kind"
            )));
        }
        if !matches!(state.capability.view(), ValueRef::Object(target) if heap.get(target).is_ok_and(is_callable_object))
        {
            return Err(VmError::InvalidSnapshot(format!(
                "combine {id} capability is not callable"
            )));
        }
        let uncalled = state.called.iter().filter(|&&called| !called).count();
        // An abrupt iteration leaves the sentinel in place after suspension.
        if state.results.len() != state.called.len()
            || !matches!(state.remaining.checked_sub(uncalled), Some(0 | 1))
        {
            return Err(VmError::InvalidSnapshot(format!(
                "combine {id} has inconsistent results, called flags, or remaining count"
            )));
        }
    }
    for (id, object) in heap.iter() {
        let Object::PromiseCallback {
            callback:
                PromiseCallback::CombineElement {
                    state,
                    index,
                    rejected,
                },
            ..
        } = object
        else {
            continue;
        };
        let combine = heap.combines.get(state).ok_or_else(|| {
            VmError::InvalidSnapshot(format!(
                "combine element callback {id} has missing state {state}"
            ))
        })?;
        if *index >= combine.called.len() {
            return Err(VmError::InvalidSnapshot(format!(
                "combine element callback {id} index {index} is out of bounds for state {state}"
            )));
        }
        let direction_ok = match combine.kind {
            CombineKind::All => !*rejected,
            CombineKind::AllSettled => true,
            CombineKind::Any => *rejected,
            CombineKind::Race => false,
        };
        if !direction_ok {
            return Err(VmError::InvalidSnapshot(format!(
                "combine element callback {id} has invalid direction for state {state}"
            )));
        }
    }
    Ok(())
}

pub(super) fn require_promise(heap: &Heap, id: ObjectId, what: &str) -> Result<(), VmError> {
    if heap.promise(id).is_err() {
        return Err(VmError::InvalidSnapshot(format!(
            "{what} references non-Promise object {id}"
        )));
    }
    Ok(())
}

pub(super) fn load_operations(
    bindings: &Bindings,
    has_timer: bool,
    heap: &Heap,
    elapsed: std::time::Duration,
    records: Vec<PendingOperationRecord>,
    fetch_state: &crate::builtin::fetch::transaction::FetchState,
) -> Result<BTreeMap<u64, PendingOperation>, VmError> {
    let mut operations = BTreeMap::new();
    let mut operation_promises = BTreeSet::new();
    for record in records {
        if record.id == 0 || record.id == u64::MAX || operations.contains_key(&record.id) {
            return Err(VmError::InvalidSnapshot(format!(
                "duplicate or invalid host operation id {}",
                record.id
            )));
        }
        let (declaration, recovery, rejection) = if let DeliveryTarget::Timeout {
            deadline, ..
        } = record.target
        {
            if record.name != TIMEOUT_OPERATION_NAME
                || record.recovery != Recovery::Reissue
                || !record.state.is_empty()
                || record.signal.is_some()
                // A timeout is created at or before the snapshot cut with a u64 delay.
                || deadline.saturating_sub(elapsed) > std::time::Duration::from_millis(u64::MAX)
            {
                return Err(VmError::InvalidSnapshot(format!(
                    "timeout operation {} has invalid metadata",
                    record.id
                )));
            }
            (None, Recovery::Reissue, TIMEOUT_OPERATION_REJECTION)
        } else {
            let (declaration, recovery, rejection) =
                bindings.operation(&record.name).ok_or_else(|| {
                    VmError::InvalidSnapshot(format!("operation {} is not declared", record.name))
                })?;
            if recovery != record.recovery {
                return Err(VmError::InvalidSnapshot(format!(
                    "operation {} recovery mismatch",
                    record.name
                )));
            }
            bindings
                .decode_operation(&record.name, &record.state)
                .map_err(|error| {
                    VmError::InvalidSnapshot(format!("operation {} state: {error}", record.name))
                })?;
            (Some(declaration), recovery, rejection)
        };
        if let DeliveryTarget::Promise(promise) = record.target {
            let pending = matches!(heap.get(promise), Ok(Object::Promise(p)) if matches!(p.state, PromiseState::Pending));
            if !pending {
                return Err(VmError::InvalidSnapshot(format!(
                    "operation {:?} references non-pending Promise {promise}",
                    record.name
                )));
            }
            if !operation_promises.insert(promise) {
                return Err(VmError::InvalidSnapshot(format!(
                    "Promise {promise} is associated with more than one host operation"
                )));
            }
        }
        operations.insert(
            record.id,
            PendingOperation {
                name: record.name,
                recovery,
                rejection,
                state: record.state,
                target: record.target,
                signal: record.signal,
                declaration,
                live: None,
            },
        );
    }
    let mut linked = BTreeSet::new();
    for (signal_id, object) in heap.iter() {
        let Object::AbortSignal { signal, .. } = object else {
            continue;
        };
        for step in &signal.steps {
            let AbortStep::Operation(id) = step else {
                continue;
            };
            if signal.reason.is_some()
                || !linked.insert(*id)
                || !(operations.get(id).is_some_and(|operation| {
                    operation.signal == Some(signal_id)
                        && matches!(operation.target, DeliveryTarget::Promise(_))
                }) || fetch_state
                    .transactions
                    .get(id)
                    .is_some_and(|transaction| transaction.signal == Some(signal_id)))
            {
                return Err(VmError::InvalidSnapshot(format!(
                    "AbortSignal {signal_id} has invalid operation link {id}"
                )));
            }
        }
    }
    for (id, transaction) in &fetch_state.transactions {
        if transaction.signal.is_some() && !linked.contains(id) {
            return Err(VmError::InvalidSnapshot(format!(
                "fetch {id} is missing its reciprocal AbortSignal link"
            )));
        }
    }
    for (id, operation) in &operations {
        if operation.signal.is_some() && !linked.contains(id) {
            return Err(VmError::InvalidSnapshot(format!(
                "operation {id} is missing its reciprocal AbortSignal link"
            )));
        }
    }
    if operations
        .values()
        .any(|op| matches!(op.target, DeliveryTarget::Timeout { .. }))
        && !has_timer
    {
        return Err(VmError::InvalidSnapshot(
            "pending timeout operations require the TimerService".to_owned(),
        ));
    }
    Ok(operations)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn validate_timers(
    has_timer: bool,
    heap: &Heap,
    timers: &BTreeMap<i32, ScriptTimer>,
    tasks: &[OrdinaryTask],
    elapsed: std::time::Duration,
    next_timer_id: i32,
    next_operation_id: u64,
    operations: &BTreeMap<u64, PendingOperation>,
) -> Result<(), VmError> {
    fn invalid() -> VmError {
        VmError::InvalidSnapshot("inconsistent timer records, registrations, or tasks".into())
    }
    fn valid_signal(heap: &Heap, id: ObjectId) -> bool {
        matches!(heap.get(id), Ok(Object::AbortSignal { signal, .. }) if signal.reason.is_none() && !signal.dependent)
    }
    if next_timer_id <= 0
        || next_operation_id == 0
        || operations
            .last_key_value()
            .is_some_and(|(&id, _)| id >= next_operation_id)
    {
        return Err(invalid());
    }
    let mut signals = BTreeSet::new();
    let mut sequences = BTreeSet::new();
    let mut matched = BTreeSet::new();
    for (&id, timer) in timers {
        if id <= 0
            || timer.sequence == 0
            || timer.sequence >= next_operation_id
            || !sequences.insert(timer.sequence)
            || timer.nesting == 0
            || timer.period > i32::MAX as u32
            || !matches!(timer.callback.view(), ValueRef::Object(id) if heap.get(id).is_ok_and(is_callable_object))
            || timer.arguments.iter().any(|value| match value.view() {
                ValueRef::Object(id) => heap.get(id).is_err(),
                ValueRef::Symbol(id) => !heap.symbols.contains_key(&id),
                _ => false,
            })
        {
            return Err(invalid());
        }
    }
    for (&id, op) in operations {
        let DeliveryTarget::Timeout {
            target,
            deadline,
            sequence,
        } = op.target
        else {
            continue;
        };
        if sequence == 0 || sequence > id {
            return Err(invalid());
        }
        match target {
            TimerTarget::Abort(signal) => {
                if !valid_signal(heap, signal)
                    || !signals.insert(signal)
                    || !sequences.insert(sequence)
                {
                    return Err(invalid());
                }
            }
            TimerTarget::Script(key) => {
                let Some(timer) = timers.get(&key.id) else {
                    return Err(invalid());
                };
                let delay = if timer.nesting > 6 {
                    timer.period.max(4)
                } else {
                    timer.period
                };
                if key.sequence != sequence
                    || timer.sequence != sequence
                    || timer.state != TimerState::Armed(id)
                    || !matched.insert(key.id)
                    || deadline.saturating_sub(elapsed)
                        > std::time::Duration::from_millis(u64::from(delay))
                {
                    return Err(invalid());
                }
            }
        }
    }
    for task in tasks {
        let valid = match *task {
            OrdinaryTask::AbortTimeout(id) => valid_signal(heap, id) && signals.insert(id),
            OrdinaryTask::ScriptTimer(key) => {
                timers.get(&key.id).is_some_and(|timer| {
                    timer.sequence == key.sequence && timer.state == TimerState::Queued
                }) && matched.insert(key.id)
            }
            _ => true,
        };
        if !valid {
            return Err(invalid());
        }
    }
    if matched.len() != timers.len() {
        return Err(invalid());
    }
    if timers.values().any(|timer| timer.repeat) && !has_timer {
        return Err(VmError::InvalidSnapshot(
            "restored intervals require the TimerService".into(),
        ));
    }
    Ok(())
}

pub(super) fn load_host_tasks(
    bindings: &Bindings,
    heap: &Heap,
    instances: &BTreeMap<ObjectId, HostInstance>,
    operations: &BTreeMap<u64, PendingOperation>,
    records: Vec<HostTaskRecord>,
    next_id: u64,
) -> Result<BTreeMap<u64, HostTask>, VmError> {
    if !(1..=HOST_TASK_ID_LIMIT).contains(&next_id) {
        return Err(VmError::InvalidSnapshot(format!(
            "invalid next host task id {next_id}"
        )));
    }
    let mut tasks = BTreeMap::new();
    for record in records {
        let id = record.id;
        let invalid =
            |message: &str| VmError::InvalidSnapshot(format!("host task {id}: {message}"));
        if !(1..next_id).contains(&id) || tasks.contains_key(&id) {
            return Err(invalid("duplicate id or id outside its high-water mark"));
        }
        match operations.get(&record.waiting_on) {
            Some(operation) if operation.target == DeliveryTarget::Task(id) => {}
            _ => return Err(invalid("waiting operation does not target this task")),
        }
        match record.owner {
            NativeTarget::Export(export) if matches!(bindings.export_decl(export), Some(ExportDecl::Function(f)) if !f.constructable) =>
                {}
            NativeTarget::Method(method) if bindings.method(method).is_some() => {
                let valid_receiver = match record.receiver.view() {
                    ValueRef::Object(object) => instances.get(&object).is_some_and(|instance| {
                        bindings.class_is_a(instance.class, method.class as usize)
                    }),
                    _ => false,
                };
                if !valid_receiver {
                    return Err(invalid("receiver is not an instance of the method's class"));
                }
            }
            _ => return Err(invalid("invalid CPS owner")),
        }
        let stage = bindings
            .cps(record.owner)
            .and_then(|stages| stages.get(record.stage as usize))
            .filter(|stage| matches!(stage.kind, CpsBoundary::External))
            .ok_or_else(|| invalid("stage is not an external CPS boundary"))?;
        if record.saved.len() != stage.slots.len() {
            return Err(invalid("saved value count does not match CPS stage"));
        }
        require_value_objects(heap, std::iter::once(&record.receiver).chain(&record.saved))?;
        tasks.insert(
            id,
            HostTask {
                owner: record.owner,
                stage: record.stage,
                receiver: record.receiver,
                saved: record.saved,
                state: HostTaskState::Waiting(record.waiting_on),
            },
        );
    }
    for (&id, operation) in operations {
        if let DeliveryTarget::Task(task) = operation.target {
            let state = tasks.get(&task).map(|task| task.state);
            if state != Some(HostTaskState::Waiting(id)) {
                return Err(VmError::InvalidSnapshot(format!(
                    "operation {id} targets host task {task}, which does not wait on it"
                )));
            }
        }
    }
    Ok(tasks)
}

use super::{continuations::valid_disposal_record, load_modules::require_value_objects};
