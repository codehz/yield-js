//! Full validation of stream slots and their pending job backlinks.

use super::StreamJob;
use crate::VmError;
use crate::builtin::is_callable_object;
use crate::heap::{Heap, Object, PromiseState, ReactionJob, ReadableState};
use crate::value::{ObjectId, ValueRef};

pub(crate) fn validate_job(heap: &Heap, source: ObjectId, job: StreamJob) -> bool {
    if !heap
        .promise(source)
        .is_ok_and(|p| matches!(p.state, PromiseState::Pending))
    {
        return false;
    }
    match job {
        StreamJob::ReadableStart(id) => {
            matches!(heap.get(id), Ok(Object::ReadableStream(s)) if s.start_wait == Some(source))
        }
        StreamJob::ReadablePull(id) => {
            matches!(heap.get(id), Ok(Object::ReadableStream(s)) if s.pull_wait == Some(source))
        }
        StreamJob::ReadableCancel(id) => pending_dependency(heap, id, source),
    }
}

fn pending_dependency(heap: &Heap, promise: ObjectId, target: ObjectId) -> bool {
    heap.promise(promise).is_ok_and(|p| {
        matches!(p.state, PromiseState::Pending)
            && !p.locked
            && p.adopting.is_none()
            && p.stream_dependency == Some(target)
    })
}

fn has_job(heap: &Heap, source: ObjectId, job: StreamJob) -> bool {
    heap.promise(source).is_ok_and(|p| {
        matches!(p.state, PromiseState::Pending)
            && p.reactions
                .iter()
                .filter(|r| **r == ReactionJob::Streams(job))
                .count()
                == 1
    })
}

pub(crate) fn validate_record(heap: &Heap, id: ObjectId, object: &Object) -> Result<(), VmError> {
    let valid = match object {
        Object::ReadableStream(s) => {
            let active = matches!(s.state, ReadableState::Readable);
            matches!(heap.get(s.controller), Ok(Object::ReadableController { stream, .. }) if *stream == id)
                && s.reader.is_none_or(|reader| matches!(heap.get(reader), Ok(Object::ReadableReader(r)) if r.stream == Some(id)))
                && !s.high_water_mark.is_nan() && s.high_water_mark >= 0.0
                && !s.total_size.is_nan() && s.total_size >= 0.0
                && (!s.queue.is_empty() || s.total_size == 0.0)
                && !(active && s.close_requested && s.queue.is_empty())
                && s.queue.iter().all(|(_, size)| size.is_finite() && *size >= 0.0)
                && [&s.pull, &s.cancel, &s.size].into_iter().flatten().all(|v| matches!(v.view(), ValueRef::Object(f) if heap.get(f).is_ok_and(is_callable_object)))
                && (!s.started || s.start_wait.is_none())
                && (s.pulling == s.pull_wait.is_some())
                && (!s.pull_again || s.pulling)
                && s.start_wait.is_none_or(|source| has_job(heap, source, StreamJob::ReadableStart(id)))
                && s.pull_wait.is_none_or(|source| has_job(heap, source, StreamJob::ReadablePull(id)))
        }
        Object::ReadableController { stream, .. } => {
            matches!(heap.get(*stream), Ok(Object::ReadableStream(s)) if s.controller == id)
        }
        Object::ReadableReader(r) => match r.stream {
            Some(stream) => match heap.get(stream) {
                Ok(Object::ReadableStream(s)) if s.reader == Some(id) => {
                    let mut unique = std::collections::BTreeSet::new();
                    match &s.state {
                            ReadableState::Readable => pending_dependency(heap, r.closed, id)
                                && (r.requests.is_empty() || s.disturbed)
                                && r.requests.iter().all(|&p| p != r.closed && unique.insert(p) && pending_dependency(heap, p, id)),
                            ReadableState::Closed => r.requests.is_empty() && heap.promise(r.closed).is_ok_and(|p| matches!(&p.state, PromiseState::Fulfilled(v) if matches!(v.view(), ValueRef::Undefined))),
                            ReadableState::Errored(reason) => r.requests.is_empty() && heap.promise(r.closed).is_ok_and(|p| matches!(&p.state, PromiseState::Rejected(v) if v == reason)),
                        }
                }
                _ => false,
            },
            None => {
                r.requests.is_empty()
                    && heap
                        .promise(r.closed)
                        .is_ok_and(|p| matches!(p.state, PromiseState::Rejected(_)))
            }
        },
        Object::Promise(p) if p.stream_dependency.is_some() => {
            let dependency = p.stream_dependency.expect("checked dependency");
            pending_dependency(heap, id, dependency)
                && match heap.get(dependency) {
                    Ok(Object::ReadableReader(r)) => r.closed == id || r.requests.contains(&id),
                    Ok(Object::Promise(_)) => {
                        has_job(heap, dependency, StreamJob::ReadableCancel(id))
                    }
                    _ => false,
                }
        }
        _ => true,
    };
    if valid {
        Ok(())
    } else {
        Err(VmError::InvalidSnapshot(format!(
            "invalid readable stream record {id}"
        )))
    }
}
