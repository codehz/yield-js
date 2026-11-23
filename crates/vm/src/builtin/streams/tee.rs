//! Durable default and byte stream tee algorithms. The source reader belongs
//! to the tee for its entire lifetime, including after either branch closes.

use super::StreamJob;
use super::readable::bytes::{byob_read, byte_close, byte_enqueue};
use super::readable::stream_id;
use crate::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{
    ByteStreamRange, ElementKind, Heap, Object, OwnedEdge, PromiseState, ReactionJob,
    ReadableState, push_value,
};
use crate::host::Host;
use crate::runtime::{Invocation, Settlement};
use crate::value::{ObjectId, PropertyKeyRef, Value, ValueRef};
use crate::vm::Vm;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TeeJob {
    Read,
    ReaderClosed(u64),
    Cancel,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TeeFlight {
    pub(crate) promise: Option<ObjectId>,
    pub(crate) byob_branch: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeeRecord {
    pub(crate) source: ObjectId,
    pub(crate) branches: [ObjectId; 2],
    pub(crate) reader: ObjectId,
    pub(crate) reader_generation: u64,
    pub(crate) byte: bool,
    pub(crate) flight: Option<TeeFlight>,
    pub(crate) read_again: [bool; 2],
    pub(crate) reasons: [Option<Value>; 2],
    pub(crate) cancel_promise: ObjectId,
    pub(crate) cancel_wait: Option<ObjectId>,
    pub(crate) finished: bool,
}

impl TeeRecord {
    pub(crate) fn write_edges(&self, out: &mut Vec<OwnedEdge>) {
        out.extend(
            [
                self.source,
                self.branches[0],
                self.branches[1],
                self.reader,
                self.cancel_promise,
            ]
            .into_iter()
            .map(OwnedEdge::Object),
        );
        out.extend(
            self.flight
                .and_then(|flight| flight.promise)
                .into_iter()
                .map(OwnedEdge::Object),
        );
        out.extend(self.cancel_wait.into_iter().map(OwnedEdge::Object));
        for reason in self.reasons.iter().flatten() {
            push_value(out, reason);
        }
    }

    fn branch_index(&self, branch: ObjectId) -> Result<usize, VmError> {
        self.branches
            .iter()
            .position(|&id| id == branch)
            .ok_or_else(|| invalid("Unknown tee branch"))
    }

    pub(crate) fn validate(&self, heap: &Heap, id: ObjectId) -> bool {
        let (Ok(Object::ReadableStream(source)), Ok(Object::ReadableReader(reader))) =
            (heap.get(self.source), heap.get(self.reader))
        else {
            return false;
        };
        let branches = self.branches;
        branches[0] != branches[1]
            && reader.stream == Some(self.source)
            && reader.internal
            && reader.byob == self.byte
            && source.reader == Some(self.reader)
            && branches.iter().all(|&branch| {
                matches!(heap.get(branch),
                Ok(Object::ReadableStream(s)) if s.byte.is_some() == self.byte
                    && matches!(s.source.view(), ValueRef::Object(owner) if owner == id))
            })
            && self.flight.is_none_or(|flight| {
                flight.promise.is_some() && flight.byob_branch.is_none_or(|index| index < 2)
            })
            && self
                .cancel_wait
                .is_none_or(|wait| has_tee_job(heap, wait, id, TeeJob::Cancel))
    }
}

fn has_tee_job(heap: &Heap, source: ObjectId, id: ObjectId, job: TeeJob) -> bool {
    heap.promise(source).is_ok_and(|promise| {
        matches!(promise.state, PromiseState::Pending)
            && promise
                .reactions
                .iter()
                .filter(|r| **r == ReactionJob::Streams(StreamJob::Tee(id, job)))
                .count()
                == 1
    })
}

pub(super) fn validate_job(heap: &Heap, source: ObjectId, id: ObjectId) -> bool {
    let Ok(Object::Tee(tee)) = heap.get(id) else {
        return false;
    };
    tee.flight
        .is_some_and(|flight| flight.promise == Some(source))
        || tee.cancel_wait == Some(source)
        || tee.reader_generation == 0
            && matches!(heap.get(tee.reader), Ok(Object::ReadableReader(r)) if r.closed == source && has_tee_job(heap, source, id, TeeJob::ReaderClosed(tee.reader_generation)))
}

fn invalid(message: &'static str) -> VmError {
    VmError::InvalidState(message.into())
}

fn invocation_id(result: Invocation) -> Result<ObjectId, VmError> {
    match result {
        Invocation::Value(value) => match value.view() {
            ValueRef::Object(id) => Ok(id),
            _ => Err(invalid("Expected an object from a tee operation")),
        },
        Invocation::Thrown(_) => Err(invalid("Unexpected exception from a tee operation")),
    }
}

pub(super) fn tee(
    vm: &mut Vm,
    _: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    let Some(source) = stream_id(vm, receiver)? else {
        return vm.throw_type_error(roots, "Expected a ReadableStream");
    };
    let roots = &roots.with(&source);
    let record = vm.readable(source)?;
    if record.reader.is_some() {
        return vm.throw_type_error(roots, "ReadableStream is already locked");
    }
    let byte = record.byte.is_some();
    let reader = vm.acquire_tee_reader(roots, source, false)?;
    let cancel_promise = vm.heap.alloc_promise();
    let id = vm.heap.alloc(Object::Tee(TeeRecord {
        source,
        branches: [source; 2], // Filled before either start reaction can run.
        reader,
        reader_generation: 0,
        byte,
        flight: None,
        read_again: [false; 2],
        reasons: [None, None],
        cancel_promise,
        cancel_wait: None,
        finished: false,
    }));
    vm.heap.promise_mut(cancel_promise)?.stream_dependency = Some(id);
    let start = vm.readable_settled_promise(undefined())?;
    let hwm = if byte { 0.0 } else { 1.0 };
    let first = vm.create_readable_internal(Value::object(id), hwm, None, byte, start)?;
    let second = vm.create_readable_internal(Value::object(id), hwm, None, byte, start)?;
    vm.tee_record_mut(id)?.branches = [first, second];
    vm.watch_tee_reader(id)?;
    let result = vm
        .heap
        .alloc_array(vec![Value::object(first), Value::object(second)]);
    Ok(Invocation::Value(Value::object(result)))
}

fn undefined() -> Settlement {
    Settlement::Fulfilled(Value::UNDEFINED)
}

impl Vm {
    fn tee_record(&self, id: ObjectId) -> Result<&TeeRecord, VmError> {
        match self.heap.get(id)? {
            Object::Tee(record) => Ok(record),
            _ => Err(invalid("Expected a tee record")),
        }
    }

    fn tee_record_mut(&mut self, id: ObjectId) -> Result<&mut TeeRecord, VmError> {
        match self.heap.get_mut(id)? {
            Object::Tee(record) => Ok(record),
            _ => Err(invalid("Expected a tee record")),
        }
    }

    fn acquire_tee_reader(
        &mut self,
        roots: &ActiveRoots<'_>,
        source: ObjectId,
        byob: bool,
    ) -> Result<ObjectId, VmError> {
        let prototype = self.builtins.get(if byob {
            "readableStreamBYOBReader.prototype"
        } else {
            "readableStreamDefaultReader.prototype"
        });
        let reader = invocation_id(self.acquire_reader(roots, source, prototype, byob)?)?;
        self.readable_reader_mut(reader)?.internal = true;
        Ok(reader)
    }

    fn watch_tee_reader(&mut self, id: ObjectId) -> Result<(), VmError> {
        let tee = self.tee_record(id)?;
        let closed = self.readable_reader(tee.reader)?.closed;
        self.observe_stream_job(
            closed,
            StreamJob::Tee(id, TeeJob::ReaderClosed(tee.reader_generation)),
        )
    }

    pub(super) fn tee_pull(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        branch: ObjectId,
    ) -> Result<ObjectId, VmError> {
        let index = self.tee_record(id)?.branch_index(branch)?;
        self.start_tee_read(host, &roots.with(&id), id, index)?;
        self.readable_settled_promise(undefined())
    }

    fn start_tee_read(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        index: usize,
    ) -> Result<(), VmError> {
        let tee = self.tee_record_mut(id)?;
        if tee.finished || tee.reasons[index].is_some() {
            return Ok(());
        }
        if tee.flight.is_some() {
            tee.read_again[index] = true;
            return Ok(());
        }
        let (byte, source, branch, mut reader) =
            (tee.byte, tee.source, tee.branches[index], tee.reader);
        let range = if byte {
            self.byte(branch)?
                .pending
                .front()
                .map(|head| ByteStreamRange {
                    offset: head.range.offset + head.bytes_filled,
                    length: head.range.length - head.bytes_filled,
                    ..head.range
                })
        } else {
            None
        };
        let byob = range.is_some();
        self.tee_record_mut(id)?.flight = Some(TeeFlight {
            promise: None,
            byob_branch: byob.then_some(index),
        });
        if self.readable_reader(reader)?.byob != byob {
            self.release_readable_reader(roots, reader)?;
            reader = self.acquire_tee_reader(roots, source, byob)?;
            let tee = self.tee_record_mut(id)?;
            tee.reader = reader;
            tee.reader_generation = tee
                .reader_generation
                .checked_add(1)
                .ok_or_else(|| invalid("Tee reader generation overflow"))?;
            self.watch_tee_reader(id)?;
        }
        let promise = if let Some(range) = range {
            let view = self.make_byte_view(range, Some(ElementKind::Uint8));
            invocation_id(byob_read(
                self,
                host,
                roots,
                vec![view],
                &Value::object(reader),
            )?)?
        } else {
            invocation_id(self.read_readable(host, roots, reader)?)?
        };
        // Source pull may synchronously cancel both branches.
        if let Some(flight) = &mut self.tee_record_mut(id)?.flight {
            flight.promise = Some(promise);
        }
        self.observe_stream_job(promise, StreamJob::Tee(id, TeeJob::Read))
    }

    pub(super) fn tee_cancel(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        branch: ObjectId,
        reason: &Value,
    ) -> Result<ObjectId, VmError> {
        let tee = self.tee_record_mut(id)?;
        let index = tee.branch_index(branch)?;
        tee.reasons[index] = Some(reason.clone());
        let promise = tee.cancel_promise;
        if !tee.finished
            && let [Some(first), Some(second)] = &tee.reasons
        {
            let reasons = vec![first.clone(), second.clone()];
            let composite = Value::object(self.heap.alloc_array(reasons));
            self.cancel_tee_source(host, &roots.with(&id), id, &composite)?;
        }
        Ok(promise)
    }

    fn cancel_tee_source(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        reason: &Value,
    ) -> Result<(), VmError> {
        let source = self.finish_tee(id)?.0;
        let wait = invocation_id(self.cancel_readable(host, roots, source, reason)?)?;
        self.tee_record_mut(id)?.cancel_wait = Some(wait);
        self.observe_stream_job(wait, StreamJob::Tee(id, TeeJob::Cancel))
    }

    fn finish_tee(&mut self, id: ObjectId) -> Result<(ObjectId, ObjectId), VmError> {
        let tee = self.tee_record_mut(id)?;
        tee.finished = true;
        tee.flight = None;
        tee.read_again = [false; 2];
        tee.reasons = [None, None];
        Ok((tee.source, tee.cancel_promise))
    }

    fn error_tee_branches(&mut self, id: ObjectId, error: &Value) -> Result<(), VmError> {
        for branch in self.tee_record(id)?.branches {
            self.error_readable(branch, error)?;
        }
        Ok(())
    }

    fn fail_tee(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        error: &Value,
        cancel_source: bool,
    ) -> Result<(), VmError> {
        self.error_tee_branches(id, error)?;
        if cancel_source {
            self.cancel_tee_source(host, roots, id, error)
        } else {
            let (_, promise) = self.finish_tee(id)?;
            self.settle_readable(promise, undefined())
        }
    }

    pub(super) fn resume_tee_job(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        job: TeeJob,
        settlement: &Settlement,
    ) -> Result<(), VmError> {
        let roots = &roots.with(&id);
        if job == TeeJob::Cancel {
            let tee = self.tee_record_mut(id)?;
            tee.cancel_wait = None;
            let promise = tee.cancel_promise;
            return self.settle_readable(promise, settlement.clone());
        }
        let tee = self.tee_record(id)?;
        if tee.finished {
            return Ok(());
        }
        match (job, settlement) {
            (TeeJob::ReaderClosed(generation), Settlement::Rejected(error))
                if generation == tee.reader_generation =>
            {
                self.fail_tee(host, roots, id, error, false)
            }
            (TeeJob::ReaderClosed(_), _) => Ok(()),
            (TeeJob::Read, Settlement::Rejected(error)) => {
                self.fail_tee(host, roots, id, error, false)
            }
            (TeeJob::Read, Settlement::Fulfilled(result)) => {
                // A source can error after satisfying its read but before tee
                // distributes the chunk. Never deliver through that error.
                if let ReadableState::Errored(error) = &self.readable(tee.source)?.state {
                    let error = error.clone();
                    return self.fail_tee(host, roots, id, &error, false);
                }
                let flight = tee
                    .flight
                    .ok_or_else(|| invalid("Tee read has no flight"))?;
                let byte = tee.byte;
                let ValueRef::Object(result) = result.view() else {
                    return Err(invalid("Expected a tee read result"));
                };
                let result = self.heap.get(result)?;
                let field = |name| {
                    result
                        .own_property(PropertyKeyRef::String(name))
                        .and_then(|property| property.data_value().cloned())
                        .ok_or_else(|| invalid("Invalid tee read result"))
                };
                let done = field("done")? == Value::boolean(true);
                let chunk = field("value")?;
                let roots = &roots.with(&chunk);
                if done {
                    return self.close_tee(host, roots, id, flight.byob_branch, &chunk);
                }
                // Only demand arising during delivery needs another read;
                // this chunk serves all earlier demand on both branches.
                self.tee_record_mut(id)?.read_again = [false; 2];
                if byte {
                    self.distribute_tee_bytes(host, roots, id, flight.byob_branch, &chunk)?;
                } else {
                    for index in 0..2 {
                        if let Some(branch) = self.live_tee_branch(id, index)? {
                            self.enqueue_readable(host, roots, branch, &chunk)?;
                        }
                    }
                }
                let tee = self.tee_record_mut(id)?;
                tee.flight = None;
                if let Some(index) =
                    (0..2).find(|&index| tee.read_again[index] && tee.reasons[index].is_none())
                {
                    return self.start_tee_read(host, roots, id, index);
                }
                Ok(())
            }
            (TeeJob::Cancel, _) => unreachable!(),
        }
    }

    fn live_tee_branch(&self, id: ObjectId, index: usize) -> Result<Option<ObjectId>, VmError> {
        let tee = self.tee_record(id)?;
        Ok((!tee.finished && tee.reasons[index].is_none()).then_some(tee.branches[index]))
    }

    fn distribute_tee_bytes(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        byob: Option<usize>,
        chunk: &Value,
    ) -> Result<(), VmError> {
        let original = byob.unwrap_or(0);
        let other = 1 - original;
        let original_live = self.live_tee_branch(id, original)?.is_some();
        let other_live = self.live_tee_branch(id, other)?.is_some();
        let Some((range, _, length)) = self.byte_view_range(chunk)? else {
            return Err(invalid("Expected an attached byte tee chunk"));
        };
        // Clone before either enqueue/respond can expose the original to JS.
        let copy = if other_live && (original_live || byob.is_some()) {
            match self.clone_byte_range(roots, range)? {
                Ok(range) => Some(self.make_byte_view(range, Some(ElementKind::Uint8))),
                Err(error) => return self.fail_tee(host, roots, id, &error, true),
            }
        } else {
            None
        };
        let roots = &roots.with(&copy);
        if let Some(branch) = self.live_tee_branch(id, original)? {
            let result = if byob.is_some() {
                self.respond_byte_request(host, roots, branch, range.length, Some((range, length)))?
            } else {
                self.enqueue_tee_bytes(host, roots, branch, chunk)?
            };
            if let Invocation::Thrown(error) = result {
                return self.fail_tee(host, roots, id, &error, true);
            }
        }
        if let Some(branch) = self.live_tee_branch(id, other)?
            && let Invocation::Thrown(error) =
                self.enqueue_tee_bytes(host, roots, branch, copy.as_ref().unwrap_or(chunk))?
        {
            return self.fail_tee(host, roots, id, &error, true);
        }
        Ok(())
    }

    fn enqueue_tee_bytes(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        branch: ObjectId,
        chunk: &Value,
    ) -> Result<Invocation, VmError> {
        let controller = Value::object(self.readable(branch)?.controller);
        byte_enqueue(self, host, roots, vec![chunk.clone()], &controller)
    }

    fn close_tee(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        byob: Option<usize>,
        chunk: &Value,
    ) -> Result<(), VmError> {
        let branches = [self.live_tee_branch(id, 0)?, self.live_tee_branch(id, 1)?];
        let byte = self.tee_record(id)?.byte;
        let (_, promise) = self.finish_tee(id)?;
        if byte {
            for branch in branches.into_iter().flatten() {
                let controller = Value::object(self.readable(branch)?.controller);
                // Closing an unaligned BYOB descriptor errors that branch.
                // The state check below then skips its zero-byte response.
                let _ = byte_close(self, host, roots, Vec::new(), &controller)?;
            }
            for (index, branch) in branches.into_iter().enumerate() {
                let Some(branch) = branch else { continue };
                if !matches!(self.readable(branch)?.state, ReadableState::Closed)
                    || self.byte(branch)?.pending.is_empty()
                {
                    continue;
                }
                let replacement = if byob == Some(index) {
                    let (range, _, length) = self
                        .byte_view_range(chunk)?
                        .ok_or_else(|| invalid("Missing byte tee EOF view"))?;
                    Some((range, length))
                } else {
                    None
                };
                if let Invocation::Thrown(_) =
                    self.respond_byte_request(host, roots, branch, 0, replacement)?
                {
                    return Err(invalid("Invalid byte tee EOF response"));
                }
            }
        } else {
            for branch in branches.into_iter().flatten() {
                self.request_readable_close(host, roots, branch)?;
            }
        }
        self.settle_readable(promise, undefined())
    }
}
