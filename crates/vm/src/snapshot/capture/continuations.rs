//! Continuation records, rejection notifications, and disposal records.

use std::collections::{BTreeMap, BTreeSet};

use crate::builtin::is_callable_object;
use crate::bytecode::Build;
use crate::error::VmError;
use crate::heap::GeneratorStatus;
use crate::heap::{DisposableResource, DisposeHint};
use crate::heap::{Heap, Object, PromiseState, RejectionStatus};
use crate::runtime::{Continuation, ContinuationTarget};
use crate::value::{ContinuationId, ObjectId, ValueRef};

use crate::snapshot::record::*;

pub(super) fn load_continuations(
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
    let mut awaiting = BTreeMap::new();
    for record in indexed.values() {
        if let ContinuationTargetRecord::Generator(generator) = record.target {
            *awaiting.entry(generator).or_insert(0usize) += 1;
        }
    }
    for (id, generator) in &heap.generators {
        if matches!(generator.status, GeneratorStatus::Awaiting) && awaiting.get(id) != Some(&1) {
            return Err(VmError::InvalidSnapshot(format!(
                "generator {id} is awaiting but does not own exactly one continuation"
            )));
        }
    }
    Ok(indexed
        .into_values()
        .map(|record| {
            (
                record.id,
                Continuation {
                    target: match record.target {
                        ContinuationTargetRecord::Promise(promise) => {
                            ContinuationTarget::Promise(promise)
                        }
                        ContinuationTargetRecord::Generator(generator) => {
                            ContinuationTarget::Generator(generator)
                        }
                    },
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

pub(super) fn validate_continuation(
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
    if build.is_initializer(record.func_id) {
        return Err(VmError::InvalidSnapshot(format!(
            "continuation {} references the script function",
            record.id
        )));
    }
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
    match record.target {
        ContinuationTargetRecord::Promise(promise) => {
            if !function.is_async || function.is_generator {
                return Err(VmError::InvalidSnapshot(format!(
                    "continuation {} targets a Promise from a non-async function",
                    record.id
                )));
            }
            match heap.get(promise) {
                Ok(Object::Promise(promise)) if matches!(promise.state, PromiseState::Pending) => {}
                _ => {
                    return Err(VmError::InvalidSnapshot(format!(
                        "continuation {} async result {promise} is not a pending Promise",
                        record.id
                    )));
                }
            }
        }
        ContinuationTargetRecord::Generator(generator) => {
            if !(function.is_async && function.is_generator) {
                return Err(VmError::InvalidSnapshot(format!(
                    "continuation {} targets a generator from a non-generator function",
                    record.id
                )));
            }
            let target = heap.generator(generator).ok_or_else(|| {
                VmError::InvalidSnapshot(format!(
                    "continuation {} targets missing generator {generator}",
                    record.id
                ))
            })?;
            if target.func_id != record.func_id {
                return Err(VmError::InvalidSnapshot(format!(
                    "continuation {} targets a generator of another function",
                    record.id
                )));
            }
            if !matches!(target.status, GeneratorStatus::Awaiting) {
                return Err(VmError::InvalidSnapshot(format!(
                    "continuation {} targets generator {generator} that is not awaiting",
                    record.id
                )));
            }
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

pub(super) fn validate_rejection_notifications(
    heap: &Heap,
    queue: &[ObjectId],
    tasks: &[OrdinaryTask],
) -> Result<(), VmError> {
    use RejectionStatus::{Handled, PendingHandled, PendingReport, Reported, Unobserved};

    let mut queued = BTreeSet::new();
    let mut check = |id, expected, allow_handled| {
        if !queued.insert(id) {
            return Err(VmError::InvalidSnapshot(format!(
                "duplicate pending rejection notification for Promise {id}"
            )));
        }
        if !matches!(heap.get(id), Ok(Object::Promise(promise))
            if matches!(promise.state, PromiseState::Rejected(_))
                && (promise.rejection_status == expected || (allow_handled && promise.rejection_status == Handled)))
        {
            return Err(VmError::InvalidSnapshot(format!(
                "pending rejection notification {id} must reference a rejected Promise with a pending notification"
            )));
        }
        Ok(())
    };
    for &id in queue {
        check(id, PendingReport, false)?;
    }
    for task in tasks {
        match task {
            OrdinaryTask::UnhandledBatch(ids) if !ids.is_empty() => {
                for &id in ids {
                    check(id, PendingReport, true)?;
                }
            }
            OrdinaryTask::RejectionHandled(id) => check(*id, PendingHandled, false)?,
            OrdinaryTask::AbortTimeout(_)
            | OrdinaryTask::ScriptTimer(_)
            | OrdinaryTask::StreamMessage(..)
            | OrdinaryTask::FetchEvent(..) => {}
            _ => return Err(VmError::InvalidSnapshot("invalid ordinary task".into())),
        }
    }

    for (id, object) in heap.iter() {
        let Object::Promise(promise) = object else {
            continue;
        };
        let valid = match promise.rejection_status {
            PendingReport | PendingHandled => queued.contains(&id),
            Reported => matches!(promise.state, PromiseState::Rejected(_)),
            Unobserved => match promise.state {
                PromiseState::Pending => promise.reactions.is_empty(),
                PromiseState::Fulfilled(_) => true,
                PromiseState::Rejected(_) => false,
            },
            Handled => true,
        };
        if !valid {
            return Err(VmError::InvalidSnapshot(format!(
                "Promise {id} has inconsistent rejection status {:?}",
                promise.rejection_status
            )));
        }
    }
    Ok(())
}

pub(super) fn valid_disposal_record(
    heap: &Heap,
    record: &DisposableResource,
    async_: Option<bool>,
) -> bool {
    let valid_object = |id| {
        heap.get(id).is_ok_and(|object| {
            !matches!(
                object,
                Object::Environment(_) | Object::ForInIterator { .. } | Object::DisposeState(_)
            )
        })
    };
    let (method, hint) = match record {
        DisposableResource::Method {
            value,
            method,
            hint,
        } => {
            if !matches!(value.view(), ValueRef::Object(id) if valid_object(id)) {
                return false;
            }
            (method, hint)
        }
        DisposableResource::Adopt {
            value,
            method,
            hint,
        } => {
            if *hint == DisposeHint::AsyncFromSync
                || matches!(value.view(), ValueRef::Object(id) if !valid_object(id))
            {
                return false;
            }
            (method, hint)
        }
        DisposableResource::Defer { method, hint } => {
            if *hint == DisposeHint::AsyncFromSync {
                return false;
            }
            (method, hint)
        }
        DisposableResource::AsyncEmpty => return async_ != Some(false),
    };
    async_.is_none_or(|async_| async_ == (*hint != DisposeHint::Sync))
        && matches!(method.view(), ValueRef::Object(id) if heap.get(id).is_ok_and(is_callable_object))
}
