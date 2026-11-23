//! Private same-VM transport. Each envelope has a corresponding ordinary task;
//! reactions observe native stream operations but never deliver across the link.
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::builtin::structured_clone::clone_message;
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{
    Heap, Object, OwnedEdge, PromiseState, Property, ReactionJob, ReadableState, WritableFlight,
    WritableSink, push_value,
};
use crate::host::Host;
use crate::runtime::{Invocation, Settlement};
use crate::snapshot::OrdinaryTask;
use crate::value::{ObjectId, PropertyKeyRef, Value, ValueRef};
use crate::vm::Vm;

use super::StreamJob;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferStage {
    Data,
    Finish,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum Command {
    Pull,
    Write(Value),
    Close,
    Stop(Value),
    Invalid(Value),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum Reply {
    Ack,
    Chunk(Value),
    Closed,
    Error(Value),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum Message {
    Request(TransferStage, Command),
    Response(TransferStage, Reply),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Phase {
    Sending,
    Waiting(ObjectId),
    Returning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Flight {
    result: ObjectId,
    phase: Phase,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransferRecord {
    original: ObjectId,
    lock: ObjectId,
    proxy: ObjectId,
    readable: bool,
    closed: Option<ObjectId>,
    data: Option<Flight>,
    finish: Option<Flight>,
    ended: bool,
    queue: VecDeque<(u64, Message)>,
}

impl TransferRecord {
    fn flight(&self, stage: TransferStage) -> Option<Flight> {
        match stage {
            TransferStage::Data => self.data,
            TransferStage::Finish => self.finish,
            TransferStage::Closed => None,
        }
    }

    fn slot(&mut self, stage: TransferStage) -> &mut Option<Flight> {
        match stage {
            TransferStage::Data => &mut self.data,
            TransferStage::Finish => &mut self.finish,
            TransferStage::Closed => unreachable!(),
        }
    }

    pub(crate) fn write_edges(&self, out: &mut Vec<OwnedEdge>) {
        out.extend([self.original, self.lock, self.proxy].map(OwnedEdge::Object));
        out.extend(self.closed.map(OwnedEdge::Object));
        for flight in [self.data, self.finish].into_iter().flatten() {
            out.push(OwnedEdge::Object(flight.result));
            if let Phase::Waiting(wait) = flight.phase {
                out.push(OwnedEdge::Object(wait));
            }
        }
        for (_, message) in &self.queue {
            match message {
                Message::Request(
                    _,
                    Command::Write(value) | Command::Stop(value) | Command::Invalid(value),
                )
                | Message::Response(_, Reply::Chunk(value) | Reply::Error(value)) => {
                    push_value(out, value)
                }
                _ => {}
            }
        }
    }

    pub(super) fn owns_result(&self, id: ObjectId) -> bool {
        [self.data, self.finish]
            .into_iter()
            .flatten()
            .any(|flight| flight.result == id)
    }

    pub(super) fn owns_proxy(&self, id: ObjectId, readable: bool) -> bool {
        self.proxy == id && self.readable == readable && !self.ended
    }

    pub(super) fn wait(&self, stage: TransferStage) -> Option<ObjectId> {
        if stage == TransferStage::Closed {
            return self.closed;
        }
        match self.flight(stage)?.phase {
            Phase::Waiting(id) => Some(id),
            _ => None,
        }
    }

    pub(super) fn validate(&self, heap: &Heap, id: ObjectId) -> bool {
        let pending = |p: ObjectId| {
            heap.promise(p)
                .is_ok_and(|p| matches!(p.state, PromiseState::Pending))
        };
        let owner = Value::object(id);
        let (backlink, lock_closed, pull, sink_wait) = if self.readable {
            let (
                Ok(Object::ReadableStream(src)),
                Ok(Object::ReadableReader(lock)),
                Ok(Object::ReadableStream(proxy)),
            ) = (
                heap.get(self.original),
                heap.get(self.lock),
                heap.get(self.proxy),
            )
            else {
                return false;
            };
            if src.reader != Some(self.lock)
                || lock.stream != Some(self.original)
                || !lock.internal
                || lock.byob
                || proxy.byte.is_some()
                || proxy.high_water_mark != 0.0
                || proxy.pull.is_some()
                || proxy.cancel.is_some()
                || proxy.size.is_some()
            {
                return false;
            }
            (proxy.source == owner, lock.closed, proxy.pull_wait, None)
        } else {
            let (
                Ok(Object::WritableStream(src)),
                Ok(Object::WritableWriter(lock)),
                Ok(Object::WritableStream(proxy)),
            ) = (
                heap.get(self.original),
                heap.get(self.lock),
                heap.get(self.proxy),
            )
            else {
                return false;
            };
            if src.writer != Some(self.lock)
                || lock.stream != Some(self.original)
                || proxy.high_water_mark != 1.0
            {
                return false;
            }
            if proxy.sink.receiver == owner
                && proxy.sink
                    != (WritableSink {
                        receiver: owner.clone(),
                        ..WritableSink::default()
                    })
            {
                return false;
            }
            (
                proxy.sink.receiver == owner,
                lock.closed,
                None,
                proxy.flight_wait,
            )
        };
        if self.original == self.proxy
            || (self.ended && (backlink || self.closed.is_some()))
            || (!self.ended && !backlink && self.finish.is_none())
        {
            return false;
        }
        if self.closed.is_some_and(|p| p != lock_closed) {
            return false;
        }
        if self
            .data
            .zip(self.finish)
            .is_some_and(|(a, b)| a.result == b.result)
            || (!self.readable && self.data.is_some() && self.finish.is_some())
        {
            return false;
        }
        let stages = [
            TransferStage::Data,
            TransferStage::Finish,
            TransferStage::Closed,
        ];
        for stage in stages {
            if let Some(wait) = self.wait(stage) {
                let job = ReactionJob::Streams(StreamJob::Transfer(id, stage));
                if !pending(wait)
                    || !heap
                        .promise(wait)
                        .is_ok_and(|p| p.reactions.iter().filter(|r| **r == job).count() == 1)
                {
                    return false;
                }
                let expected = match (self.readable, stage) {
                    (true, TransferStage::Data) => Some(self.lock),
                    (false, TransferStage::Data | TransferStage::Finish) => Some(self.original),
                    _ => None,
                };
                if let Some(owner) = expected
                    && !heap
                        .promise(wait)
                        .is_ok_and(|p| p.stream_dependency == Some(owner))
                {
                    return false;
                }
            }
            let mut messages = self.queue.iter().filter_map(|(_, m)| match m {
                Message::Request(s, _) | Message::Response(s, _) if *s == stage => Some(m),
                _ => None,
            });
            if let Some(flight) = self.flight(stage) {
                if !heap.promise(flight.result).is_ok_and(|p| {
                    matches!(p.state, PromiseState::Pending) && p.stream_dependency == Some(id)
                }) {
                    return false;
                }
                if stage == TransferStage::Data && self.readable && pull != Some(flight.result) {
                    return false;
                }
                if !self.readable && sink_wait != Some(flight.result) {
                    return false;
                }
                match (flight.phase, messages.next()) {
                    (Phase::Sending, Some(Message::Request(_, cmd)))
                        if matches!(
                            (self.readable, stage, cmd),
                            (true, TransferStage::Data, Command::Pull)
                                | (
                                    false,
                                    TransferStage::Data,
                                    Command::Write(_) | Command::Invalid(_),
                                )
                                | (
                                    false,
                                    TransferStage::Finish,
                                    Command::Close | Command::Stop(_) | Command::Invalid(_),
                                )
                                | (
                                    true,
                                    TransferStage::Finish,
                                    Command::Stop(_) | Command::Invalid(_),
                                )
                        ) => {}
                    (Phase::Waiting(_), None) => {}
                    (Phase::Returning, Some(Message::Response(_, reply)))
                        if matches!(
                            (self.readable, stage, reply),
                            (
                                true,
                                TransferStage::Data,
                                Reply::Chunk(_) | Reply::Closed | Reply::Error(_),
                            ) | (_, _, Reply::Ack | Reply::Error(_))
                        ) => {}
                    _ => return false,
                }
            } else if let Some(message) = messages.next()
                && (stage != TransferStage::Closed
                    || self.closed.is_some()
                    || !matches!(
                        message,
                        Message::Response(_, Reply::Closed | Reply::Error(_))
                    ))
            {
                return false;
            }
            if messages.next().is_some() {
                return false;
            }
        }
        true
    }
}

pub(in crate::builtin) fn transferable(vm: &Vm, source: ObjectId) -> Result<bool, VmError> {
    Ok(match vm.heap.get(source)? {
        Object::ReadableStream(s) => s.reader.is_none(),
        Object::WritableStream(s) => s.writer.is_none(),
        Object::TransformStream(s) => {
            matches!(vm.heap.get(s.readable)?, Object::ReadableStream(r) if r.reader.is_none())
                && matches!(vm.heap.get(s.writable)?, Object::WritableStream(w) if w.writer.is_none())
        }
        _ => false,
    })
}

fn object(result: Invocation) -> Result<ObjectId, VmError> {
    let Invocation::Value(value) = result else {
        return Err(invalid());
    };
    let ValueRef::Object(id) = value.view() else {
        return Err(invalid());
    };
    Ok(id)
}

fn invalid() -> VmError {
    VmError::InvalidState("invalid stream transfer state".into())
}

impl Vm {
    pub(in crate::builtin) fn allocate_transfer_proxy(
        &mut self,
        source: ObjectId,
    ) -> Result<ObjectId, VmError> {
        let start = self.heap.alloc_promise();
        self.settle(start, Settlement::Fulfilled(Value::UNDEFINED))?;
        Ok(match self.heap.get(source)? {
            Object::ReadableStream(_) => {
                self.create_readable_internal(Value::UNDEFINED, 0.0, None, false, start)?
            }
            Object::WritableStream(_) => {
                self.create_writable_internal(WritableSink::default(), 1.0, start)?
            }
            Object::TransformStream(_) => {
                let r = self.create_readable_internal(Value::UNDEFINED, 0.0, None, false, start)?;
                let w = self.create_writable_internal(WritableSink::default(), 1.0, start)?;
                self.create_transfer_transform(r, w)?
            }
            _ => return Err(invalid()),
        })
    }

    pub(in crate::builtin) fn commit_stream_transfer(
        &mut self,
        roots: &ActiveRoots<'_>,
        source: ObjectId,
        target: ObjectId,
    ) -> Result<(), VmError> {
        match self.heap.get(source)? {
            Object::TransformStream(s) => {
                let (readable, writable) = (s.readable, s.writable);
                let proxy = self.transform(target)?;
                let (r, w) = (proxy.readable, proxy.writable);
                self.connect_transfer(roots, readable, r, true)?;
                self.connect_transfer(roots, writable, w, false)
            }
            Object::ReadableStream(_) => self.connect_transfer(roots, source, target, true),
            Object::WritableStream(_) => self.connect_transfer(roots, source, target, false),
            _ => Err(invalid()),
        }
    }

    fn connect_transfer(
        &mut self,
        roots: &ActiveRoots<'_>,
        original: ObjectId,
        proxy: ObjectId,
        readable: bool,
    ) -> Result<(), VmError> {
        let (lock, closed) = if readable {
            let lock = object(self.acquire_readable_reader(
                roots,
                original,
                self.builtins.get("readableStreamDefaultReader.prototype"),
            )?)?;
            self.readable_reader_mut(lock)?.internal = true;
            self.readable_mut(original)?.disturbed = true;
            (lock, self.readable_reader(lock)?.closed)
        } else {
            let lock = object(self.acquire_writable_writer(
                roots,
                original,
                self.builtins.get("writableStreamDefaultWriter.prototype"),
            )?)?;
            self.mark_promise_handled(self.writable_writer(lock)?.ready)?;
            (lock, self.writable_writer(lock)?.closed)
        };
        let id = self.heap.alloc(Object::StreamTransfer(TransferRecord {
            original,
            lock,
            proxy,
            readable,
            closed: Some(closed),
            data: None,
            finish: None,
            ended: false,
            queue: VecDeque::new(),
        }));
        if readable {
            self.readable_mut(proxy)?.source = Value::object(id);
        } else {
            self.writable_mut(proxy)?.sink.receiver = Value::object(id);
        }
        self.observe_stream_job(closed, StreamJob::Transfer(id, TransferStage::Closed))
    }

    fn transfer_record(&self, id: ObjectId) -> Result<&TransferRecord, VmError> {
        match self.heap.get(id)? {
            Object::StreamTransfer(r) => Ok(r),
            _ => Err(invalid()),
        }
    }

    fn transfer_record_mut(&mut self, id: ObjectId) -> Result<&mut TransferRecord, VmError> {
        match self.heap.get_mut(id)? {
            Object::StreamTransfer(r) => Ok(r),
            _ => Err(invalid()),
        }
    }

    fn post_transfer(&mut self, id: ObjectId, message: Message) -> Result<(), VmError> {
        let n = self.next_stream_message;
        self.next_stream_message = n.checked_add(1).ok_or_else(invalid)?;
        self.transfer_record_mut(id)?.queue.push_back((n, message));
        self.ordinary_tasks
            .push_back(OrdinaryTask::StreamMessage(id, n));
        Ok(())
    }

    fn transfer_request(
        &mut self,
        id: ObjectId,
        stage: TransferStage,
        cmd: Command,
    ) -> Result<ObjectId, VmError> {
        if self.transfer_record(id)?.flight(stage).is_some() {
            return Err(invalid());
        }
        let result = self.heap.alloc_promise();
        self.heap.promise_mut(result)?.stream_dependency = Some(id);
        *self.transfer_record_mut(id)?.slot(stage) = Some(Flight {
            result,
            phase: Phase::Sending,
        });
        self.post_transfer(id, Message::Request(stage, cmd))?;
        Ok(result)
    }

    pub(super) fn transfer_pull(&mut self, id: ObjectId) -> Result<ObjectId, VmError> {
        self.transfer_request(id, TransferStage::Data, Command::Pull)
    }

    pub(super) fn transfer_send(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        flight: WritableFlight,
        value: Option<&Value>,
    ) -> Result<ObjectId, VmError> {
        let roots = &roots.with(&id);
        let (stage, cmd) = match (flight, value) {
            (WritableFlight::Close, None) => (TransferStage::Finish, Command::Close),
            (WritableFlight::Write, Some(value)) => (
                TransferStage::Data,
                match clone_message(self, host, roots, value)? {
                    Ok(chunk) => Command::Write(chunk),
                    Err(error) => Command::Invalid(error),
                },
            ),
            (WritableFlight::Abort(_), Some(reason)) => (
                TransferStage::Finish,
                Command::Stop(self.transfer_reason(host, roots, reason)?),
            ),
            _ => return Err(invalid()),
        };
        self.transfer_request(id, stage, cmd)
    }

    pub(super) fn transfer_cancel(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        reason: &Value,
    ) -> Result<ObjectId, VmError> {
        let reason = self.transfer_reason(host, &roots.with(&id), reason)?;
        self.transfer_request(id, TransferStage::Finish, Command::Stop(reason))
    }

    fn transfer_reason(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        reason: &Value,
    ) -> Result<Value, VmError> {
        Ok(match clone_message(self, host, roots, reason)? {
            Ok(value) | Err(value) => value,
        })
    }

    pub(crate) fn deliver_stream_message(
        &mut self,
        host: &mut dyn Host,
        id: ObjectId,
        sequence: u64,
    ) -> Result<(), VmError> {
        let Some((n, message)) = self.transfer_record_mut(id)?.queue.pop_front() else {
            return Err(invalid());
        };
        if n != sequence {
            return Err(invalid());
        }
        let roots = &ActiveRoots::EMPTY.with(&id);
        let record = self.transfer_record(id)?;
        let (original, lock, proxy, readable, ended) = (
            record.original,
            record.lock,
            record.proxy,
            record.readable,
            record.ended,
        );
        match message {
            Message::Request(stage, cmd) => {
                let wait = object(match cmd {
                    Command::Pull => self.read_readable(host, roots, lock)?,
                    Command::Write(chunk) => {
                        self.writable_write(host, &roots.with(&chunk), lock, &chunk)?
                    }
                    Command::Close => self.writable_close(host, roots, original)?,
                    Command::Stop(reason) if readable => {
                        self.cancel_readable(host, &roots.with(&reason), original, &reason)?
                    }
                    Command::Stop(reason) => {
                        self.writable_abort(host, &roots.with(&reason), original, &reason)?
                    }
                    Command::Invalid(error) => {
                        let reason = self.transfer_reason(host, &roots.with(&error), &error)?;
                        let wait = object(self.writable_abort(
                            host,
                            &roots.with(&reason),
                            original,
                            &reason,
                        )?)?;
                        self.mark_promise_handled(wait)?;
                        return self.transfer_reply(id, stage, Reply::Error(error));
                    }
                })?;
                self.transfer_record_mut(id)?
                    .slot(stage)
                    .as_mut()
                    .ok_or_else(invalid)?
                    .phase = Phase::Waiting(wait);
                self.observe_stream_job(wait, StreamJob::Transfer(id, stage))
            }
            Message::Response(stage, reply) => {
                let flight = if stage == TransferStage::Closed {
                    None
                } else {
                    self.transfer_record_mut(id)?.slot(stage).take()
                };
                let finishing = self.transfer_record(id)?.finish.is_some();
                let abort_pending = !readable && self.writable(proxy)?.abort.is_some();
                let terminal = stage == TransferStage::Finish
                    || (matches!(&reply, Reply::Closed | Reply::Error(_))
                        && !finishing
                        && !abort_pending);
                let reason = match &reply {
                    Reply::Error(value) | Reply::Chunk(value) => Some(value),
                    _ => None,
                };
                let live = (flight.map(|f| f.result), reason);
                let roots = &roots.with(&live);
                if terminal {
                    self.end_transfer(id)?;
                }
                if !ended && !finishing {
                    match &reply {
                        Reply::Chunk(chunk)
                            if readable
                                && matches!(
                                    self.readable(proxy)?.state,
                                    ReadableState::Readable
                                ) =>
                        {
                            self.enqueue_readable(host, roots, proxy, chunk)?;
                        }
                        Reply::Closed if readable => {
                            self.readable_mut(proxy)?.source = Value::UNDEFINED;
                            self.request_readable_close(host, roots, proxy)?;
                        }
                        Reply::Closed => {
                            let error =
                                self.type_error(roots, "Transferred writable stream is closed")?;
                            self.writable_error(host, roots, proxy, &error)?;
                        }
                        Reply::Error(error) if readable && stage != TransferStage::Finish => {
                            self.error_readable(proxy, error)?
                        }
                        Reply::Error(error) if stage == TransferStage::Closed => {
                            self.writable_error(host, roots, proxy, error)?
                        }
                        _ => {}
                    }
                }
                if let Some(flight) = flight {
                    self.settle(
                        flight.result,
                        match reply {
                            Reply::Error(error) => Settlement::Rejected(error),
                            _ => Settlement::Fulfilled(Value::UNDEFINED),
                        },
                    )?;
                }
                Ok(())
            }
        }
    }

    fn end_transfer(&mut self, id: ObjectId) -> Result<(), VmError> {
        let r = self.transfer_record_mut(id)?;
        r.ended = true;
        if let Some(wait) = r.closed.take() {
            let job = ReactionJob::Streams(StreamJob::Transfer(id, TransferStage::Closed));
            self.heap.promise_mut(wait)?.reactions.retain(|r| *r != job);
        }
        Ok(())
    }

    fn transfer_reply(
        &mut self,
        id: ObjectId,
        stage: TransferStage,
        reply: Reply,
    ) -> Result<(), VmError> {
        if stage != TransferStage::Closed {
            self.transfer_record_mut(id)?
                .slot(stage)
                .as_mut()
                .ok_or_else(invalid)?
                .phase = Phase::Returning;
        }
        self.post_transfer(id, Message::Response(stage, reply))
    }

    pub(super) fn resume_transfer(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        stage: TransferStage,
        settlement: &Settlement,
    ) -> Result<(), VmError> {
        let r = self.transfer_record(id)?;
        let (original, readable, data, finishing) =
            (r.original, r.readable, r.data, r.finish.is_some());
        if stage == TransferStage::Closed {
            self.transfer_record_mut(id)?.closed = None;
            if finishing || (readable && data.is_some_and(|f| matches!(f.phase, Phase::Waiting(_))))
            {
                return Ok(());
            }
        }
        let reply = match settlement {
            Settlement::Rejected(error) => Reply::Error(self.transfer_reason(host, roots, error)?),
            Settlement::Fulfilled(_) if stage == TransferStage::Closed => Reply::Closed,
            Settlement::Fulfilled(result) if readable && stage == TransferStage::Data => {
                let ValueRef::Object(result) = result.view() else {
                    return Err(invalid());
                };
                let get = |name| match self
                    .heap
                    .get(result)?
                    .own_property(PropertyKeyRef::String(name))
                    .map(|p| p.into_owned())
                {
                    Some(Property::Data(v)) => Ok(v),
                    _ => Err(invalid()),
                };
                if get("done")?.truthy() {
                    Reply::Closed
                } else if finishing {
                    Reply::Ack
                } else {
                    match clone_message(self, host, roots, &get("value")?)? {
                        Ok(chunk) => Reply::Chunk(chunk),
                        Err(error) => {
                            let roots = &roots.with(&error);
                            let reason = self.transfer_reason(host, roots, &error)?;
                            let wait = object(self.cancel_readable(
                                host,
                                &roots.with(&reason),
                                original,
                                &reason,
                            )?)?;
                            self.mark_promise_handled(wait)?;
                            Reply::Error(error)
                        }
                    }
                }
            }
            Settlement::Fulfilled(_) => Reply::Ack,
        };
        let after_chunk = matches!(&reply, Reply::Chunk(_))
            && self.transfer_record(id)?.closed.is_none()
            && !self
                .transfer_record(id)?
                .queue
                .iter()
                .any(|(_, m)| matches!(m, Message::Response(TransferStage::Closed, _)));
        self.transfer_reply(id, stage, reply)?;
        if after_chunk {
            let terminal = match self.readable(original)?.state.clone() {
                ReadableState::Closed => Reply::Closed,
                ReadableState::Errored(reason) => {
                    Reply::Error(self.transfer_reason(host, roots, &reason)?)
                }
                ReadableState::Readable => return Ok(()),
            };
            self.transfer_reply(id, TransferStage::Closed, terminal)?;
        }
        Ok(())
    }
}

pub(super) fn validate_proxy(heap: &Heap, value: &Value, id: ObjectId, readable: bool) -> bool {
    match value.view() {
        ValueRef::Object(link) => match heap.get(link) {
            Ok(Object::StreamTransfer(record)) => record.owns_proxy(id, readable),
            _ => true,
        },
        _ => true,
    }
}

fn matches_tasks(record: &TransferRecord, queued: impl Iterator<Item = u64>) -> bool {
    record.queue.iter().map(|(n, _)| *n).eq(queued)
}

pub(crate) fn validate_tasks(
    heap: &Heap,
    tasks: &[OrdinaryTask],
    next: u64,
) -> Result<(), VmError> {
    let invalid = || VmError::InvalidSnapshot("inconsistent stream message FIFO".into());
    let mut queued = BTreeMap::<ObjectId, Vec<u64>>::new();
    let mut previous = None;
    for task in tasks {
        if let OrdinaryTask::StreamMessage(id, seq) = *task {
            if seq >= next || previous.is_some_and(|old| old >= seq) {
                return Err(invalid());
            }
            previous = Some(seq);
            queued.entry(id).or_default().push(seq);
        }
    }
    let mut originals = BTreeSet::new();
    let mut proxies = BTreeSet::new();
    for (id, object) in heap.iter() {
        if let Object::StreamTransfer(record) = object
            && (!originals.insert(record.original)
                || !proxies.insert(record.proxy)
                || !matches_tasks(record, queued.remove(&id).into_iter().flatten()))
        {
            return Err(invalid());
        }
    }
    if !queued.is_empty() {
        return Err(invalid());
    }
    Ok(())
}
