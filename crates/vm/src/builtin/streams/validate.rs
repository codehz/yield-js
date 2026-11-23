//! Full validation of stream slots and their pending job backlinks.

use super::StreamJob;
use crate::VmError;
use crate::builtin::is_callable_object;
use crate::heap::{
    ElementKind, Heap, Object, PromiseState, PullIntoReader, ReactionJob, ReadableState,
    ReadableStreamObject,
};
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

fn validate_bytes(heap: &Heap, stream: &ReadableStreamObject) -> bool {
    let reader = stream.reader.and_then(|id| match heap.get(id) {
        Ok(Object::ReadableReader(reader)) => Some(reader),
        _ => None,
    });
    let byob = reader.is_some_and(|reader| reader.byob);
    let active = matches!(stream.state, ReadableState::Readable);
    let closed = matches!(stream.state, ReadableState::Closed);
    let Some(byte) = &stream.byte else {
        return !byob
            && (!stream.queue.is_empty() || stream.total_size == 0.0)
            && !(active && stream.close_requested && stream.queue.is_empty());
    };
    if !stream.queue.is_empty()
        || stream.size.is_some()
        || byte.auto_allocate == Some(0)
        || (!active && !byte.queue.is_empty())
        || (active && stream.close_requested && byte.queue.is_empty())
        || (!active && !closed && (!byte.pending.is_empty() || byte.byob_request.is_some()))
        || (!active
            && (stream.pull.is_some()
                || stream.cancel.is_some()
                || !matches!(stream.source.view(), ValueRef::Undefined)))
    {
        return false;
    }
    let mut buffers = std::collections::BTreeSet::new();
    let mut total = 0usize;
    for range in &byte.queue {
        let Some(end) = range.offset.checked_add(range.length) else {
            return false;
        };
        let Some(sum) = total.checked_add(range.length) else {
            return false;
        };
        if range.length == 0
            || !buffers.insert(range.buffer)
            || !matches!(heap.get(range.buffer), Ok(Object::ArrayBuffer(b)) if b.max_byte_length.is_none() && b.byte_length().is_some_and(|len| end <= len))
        {
            return false;
        }
        total = sum;
    }
    if stream.total_size != total as f64 {
        return false;
    }
    let mut requests = reader.into_iter().flat_map(|r| r.requests.iter().copied());
    let expected = if byob {
        PullIntoReader::Byob
    } else {
        PullIntoReader::Default
    };
    for (index, d) in byte.pending.iter().enumerate() {
        let width = d.view_kind.map_or(1, ElementKind::bytes);
        let r = d.range;
        if d.element_size != width
            || r.length == 0
            || r.length % width != 0
            || r.offset % width != 0
            || r.offset
                .checked_add(r.length)
                .is_none_or(|end| end > d.buffer_length)
            || d.minimum_fill == 0
            || d.minimum_fill > r.length
            || d.minimum_fill % width != 0
            || d.bytes_filled >= d.minimum_fill
            || (index != 0 && d.bytes_filled != 0)
            || (closed && d.bytes_filled % width != 0)
            || !buffers.insert(r.buffer)
            || !matches!(heap.get(r.buffer), Ok(Object::ArrayBuffer(b)) if b.max_byte_length.is_none() && match b.byte_length() {
                Some(length) => length == d.buffer_length,
                // Script may transfer an exposed request view's buffer and subsequently
                // resume the request using respondWithNewView.
                None => index == 0 && byte.byob_request.is_some(),
            })
        {
            return false;
        }
        match d.reader_type {
            PullIntoReader::None if index == 0 && d.request.is_none() => (),
            PullIntoReader::Default if closed && d.request.is_none() && !byob => {
                if byte.auto_allocate != Some(r.length)
                    || d.minimum_fill != 1
                    || d.view_kind != Some(ElementKind::Uint8)
                    || r.offset != 0
                    || d.buffer_length != r.length
                {
                    return false;
                }
            }
            kind if kind == expected && d.request.is_some() && d.request == requests.next() => {
                if kind == PullIntoReader::Default
                    && (byte.auto_allocate != Some(r.length)
                        || d.minimum_fill != 1
                        || d.view_kind != Some(ElementKind::Uint8)
                        || r.offset != 0
                        || d.buffer_length != r.length)
                {
                    return false;
                }
            }
            _ => return false,
        }
    }
    if ((byob || byte.auto_allocate.is_some()) && requests.next().is_some())
        || (!byte.queue.is_empty() && reader.is_some_and(|reader| !reader.requests.is_empty()))
    {
        return false;
    }
    byte.byob_request.is_none_or(|request| {
        let Some(head) = byte.pending.front() else { return false };
        let Ok(Object::ReadableByobRequest { controller: Some(controller), view: Some(view), .. }) = heap.get(request) else { return false };
        *controller == stream.controller && matches!(heap.get(*view), Ok(Object::TypedArray(v)) if v.kind == ElementKind::Uint8 && v.buffer == head.range.buffer
            && v.byte_offset == head.range.offset + head.bytes_filled && v.byte_length == Some(head.range.length - head.bytes_filled))
    })
}

pub(crate) fn validate_record(heap: &Heap, id: ObjectId, object: &Object) -> Result<(), VmError> {
    let valid = match object {
        Object::ReadableStream(s) => {
            matches!(heap.get(s.controller), Ok(Object::ReadableController { stream, .. }) if *stream == id)
                && s.reader.is_none_or(|reader| matches!(heap.get(reader), Ok(Object::ReadableReader(r)) if r.stream == Some(id)))
                && !s.high_water_mark.is_nan() && s.high_water_mark >= 0.0
                && !s.total_size.is_nan() && s.total_size >= 0.0
                && validate_bytes(heap, s)
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
        Object::ReadableByobRequest {
            controller, view, ..
        } => match (controller, view) {
            (None, None) => true,
            (Some(controller), Some(_)) => {
                matches!(heap.get(*controller), Ok(Object::ReadableController { stream, .. })
                if matches!(heap.get(*stream), Ok(Object::ReadableStream(s)) if s.controller == *controller
                    && s.byte.as_ref().is_some_and(|b| b.byob_request == Some(id))))
            }
            _ => false,
        },
        Object::ReadableReader(r) => match r.stream {
            Some(stream) => match heap.get(stream) {
                Ok(Object::ReadableStream(s)) if s.reader == Some(id) => {
                    let mut unique = std::collections::BTreeSet::new();
                    match &s.state {
                            ReadableState::Readable => pending_dependency(heap, r.closed, id)
                                && (r.requests.is_empty() || s.disturbed)
                                && r.requests.iter().all(|&p| p != r.closed && unique.insert(p) && pending_dependency(heap, p, id)),
                            ReadableState::Closed => (r.requests.is_empty() || (r.byob && s.disturbed))
                                && r.requests.iter().all(|&p| p != r.closed && unique.insert(p) && pending_dependency(heap, p, id))
                                && heap.promise(r.closed).is_ok_and(|p| matches!(&p.state, PromiseState::Fulfilled(v) if matches!(v.view(), ValueRef::Undefined))),
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
