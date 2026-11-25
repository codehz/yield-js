//! Native pipe ownership and promise-driven pumping. Only the first shutdown
//! trigger chooses an action; later reactions can drain writes, not replace it.

use std::collections::VecDeque;

use super::readable::arg;
use super::{StreamJob, dictionary_member};
use crate::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{
    Heap, Object, OwnedEdge, PromiseState, Property, ReactionJob, ReadableState, WritableState,
    push_value,
};
use crate::host::Host;
use crate::runtime::{Invocation, Settlement};
use crate::value::{ObjectId, PropertyKeyRef, Value, ValueRef};
use crate::vm::VmCore;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PipeStage {
    Ready,
    Read,
    Write(ObjectId),
    SourceClosed,
    DestinationClosed,
    Shutdown,
    Action(ObjectId),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Options {
    prevent_abort: bool,
    prevent_cancel: bool,
    prevent_close: bool,
    signal: Option<ObjectId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum Stop {
    SourceError(Value),
    DestinationError(Value),
    SourceClosed,
    Signal(Value),
}

impl Stop {
    fn reason(&self) -> Option<&Value> {
        match self {
            Self::SourceError(reason) | Self::DestinationError(reason) | Self::Signal(reason) => {
                Some(reason)
            }
            Self::SourceClosed => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Shutdown {
    stop: Stop,
    drain: bool,
    drain_read: bool,
    queued: bool,
    actions: Vec<ObjectId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ActivePipe {
    source: ObjectId,
    destination: ObjectId,
    reader: ObjectId,
    writer: ObjectId,
    result: ObjectId,
    options: Options,
    source_closed: Option<ObjectId>,
    destination_closed: Option<ObjectId>,
    ready: Option<ObjectId>,
    read: Option<ObjectId>,
    writes: VecDeque<ObjectId>,
    // A synchronous read/write can run user code before returning its promise.
    dispatching: bool,
    shutdown: Option<Shutdown>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PipeRecord {
    active: Option<ActivePipe>,
}

impl ActivePipe {
    fn observations(&self) -> impl Iterator<Item = (ObjectId, PipeStage)> + '_ {
        [
            (self.source_closed, PipeStage::SourceClosed),
            (self.destination_closed, PipeStage::DestinationClosed),
            (self.ready, PipeStage::Ready),
            (self.read, PipeStage::Read),
        ]
        .into_iter()
        .filter_map(|(id, stage)| Some((id?, stage)))
        .chain(self.writes.iter().map(|&id| (id, PipeStage::Write(id))))
        .chain(self.shutdown.iter().flat_map(|shutdown| {
            shutdown
                .actions
                .iter()
                .map(|&id| (id, PipeStage::Action(id)))
        }))
    }
}

impl PipeRecord {
    pub(crate) fn write_edges(&self, edges: &mut Vec<OwnedEdge>) {
        let Some(p) = &self.active else { return };
        edges.extend(
            [p.source, p.destination, p.reader, p.writer, p.result]
                .into_iter()
                .map(OwnedEdge::Object),
        );
        edges.extend(p.options.signal.map(OwnedEdge::Object));
        edges.extend(p.observations().map(|(id, _)| OwnedEdge::Object(id)));
        if let Some(reason) = p.shutdown.as_ref().and_then(|s| s.stop.reason()) {
            push_value(edges, reason);
        }
    }

    pub(crate) fn accepts_job(&self, stage: PipeStage) -> bool {
        self.active.as_ref().is_some_and(|p| {
            p.observations().any(|(_, candidate)| candidate == stage)
                || (stage == PipeStage::Shutdown && p.shutdown.as_ref().is_some_and(|s| s.queued))
        })
    }

    pub(crate) fn validate_job(&self, promise: ObjectId, stage: PipeStage) -> bool {
        self.active
            .as_ref()
            .is_some_and(|p| p.observations().any(|entry| entry == (promise, stage)))
    }

    pub(crate) fn validate(&self, heap: &Heap, target: ObjectId) -> bool {
        let Some(p) = &self.active else { return true };
        let (
            Ok(Object::ReadableStream(r)),
            Ok(Object::WritableStream(w)),
            Ok(Object::ReadableReader(reader)),
            Ok(Object::WritableWriter(writer)),
        ) = (
            heap.get(p.source),
            heap.get(p.destination),
            heap.get(p.reader),
            heap.get(p.writer),
        )
        else {
            return false;
        };
        let mut write_requests = w.queue.iter();
        if p.dispatching
            || p.ready.is_some() && p.read.is_some()
            || !r.disturbed
            || !reader.internal
            || reader.byob
            || r.reader != Some(p.reader)
            || reader.stream != Some(p.source)
            || w.writer != Some(p.writer)
            || writer.stream != Some(p.destination)
            || p.source_closed.is_some_and(|id| id != reader.closed)
            || p.destination_closed.is_some_and(|id| id != writer.closed)
            || p.ready.is_some_and(|id| id != writer.ready)
            || p.read.is_some_and(|id| !reader.requests.contains(&id))
            || !p
                .writes
                .iter()
                .all(|&id| write_requests.any(|request| request.promise == id))
            || !matches!(heap.promise(p.result), Ok(result) if matches!(result.state, PromiseState::Pending) && result.stream_dependency == Some(target))
        {
            return false;
        }
        if let Some(signal) = p.options.signal {
            let Ok(Object::AbortSignal { signal, .. }) = heap.get(signal) else {
                return false;
            };
            let registered = signal.pipe_steps.iter().filter(|&&id| id == target).count();
            if registered != signal.reason.is_none() as usize {
                return false;
            }
        }
        if let Some(s) = &p.shutdown
            && (s.queued
                || s.drain_read
                || s.actions.len() > 2
                || (s.actions.is_empty() && (!s.drain || p.writes.is_empty())))
        {
            return false;
        }
        let mut seen = std::collections::HashSet::new();
        p.observations().all(|(id, stage)| {
            seen.insert(id) && matches!(heap.promise(id), Ok(promise)
                if matches!(promise.state, PromiseState::Pending)
                    && promise.reactions.iter().filter(|reaction| **reaction == ReactionJob::Streams(StreamJob::Pipe { target, stage })).count() == 1)
        })
    }
}

pub(super) fn pipe_to(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    match pipe_call(vm, host, roots, &args, receiver, false)? {
        Invocation::Thrown(reason) => {
            let promise = vm.heap.alloc_promise();
            vm.settle(promise, Settlement::Rejected(reason))?;
            Ok(Invocation::Value(Value::object(promise)))
        }
        result => Ok(result),
    }
}

pub(super) fn pipe_through(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    pipe_call(vm, host, roots, &args, receiver, true)
}

fn pipe_call(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    args: &[Value],
    receiver: &Value,
    through: bool,
) -> Result<Invocation, VmError> {
    let live = (args, receiver);
    let roots = &roots.with(&live);
    let ValueRef::Object(source) = receiver.view() else {
        return vm.throw_type_error(roots, "Expected a ReadableStream");
    };
    if !matches!(vm.heap.get(source)?, Object::ReadableStream(_)) {
        return vm.throw_type_error(roots, "Expected a ReadableStream");
    }
    macro_rules! dictionary {
        ($roots:expr, $value:expr, $name:literal) => {
            match dictionary_member(vm, host, $roots, $value, $name)? {
                Invocation::Value(value) => value,
                thrown => return Ok(thrown),
            }
        };
    }
    let pair = arg(args, 0);
    let output = if through {
        let readable = dictionary!(roots, pair, "readable");
        match readable.view() {
            ValueRef::Object(id) if matches!(vm.heap.get(id)?, Object::ReadableStream(_)) => {
                Some(id)
            }
            _ => return vm.throw_type_error(roots, "Expected a readable stream in the pair"),
        }
    } else {
        None
    };
    let roots = &roots.with(&output);
    let destination = if through {
        dictionary!(roots, pair, "writable")
    } else {
        pair.clone()
    };
    let ValueRef::Object(destination) = destination.view() else {
        return vm.throw_type_error(roots, "Expected a WritableStream");
    };
    if !matches!(vm.heap.get(destination)?, Object::WritableStream(_)) {
        return vm.throw_type_error(roots, "Expected a WritableStream");
    }
    let roots = &roots.with(&destination);
    let options = arg(args, 1);
    let prevent_abort = dictionary!(roots, options, "preventAbort").truthy();
    let prevent_cancel = dictionary!(roots, options, "preventCancel").truthy();
    let prevent_close = dictionary!(roots, options, "preventClose").truthy();
    let signal = match dictionary!(roots, options, "signal").view() {
        ValueRef::Undefined => None,
        ValueRef::Object(id) if vm.abort_signal_state(id)?.is_some() => Some(id),
        _ => return vm.throw_type_error(roots, "Expected an AbortSignal"),
    };
    let roots = &roots.with(&signal);
    if vm.readable(source)?.reader.is_some() || vm.writable(destination)?.writer.is_some() {
        return vm.throw_type_error(roots, "Cannot pipe a locked stream");
    }
    let result = vm.start_stream_pipe(
        host,
        roots,
        source,
        destination,
        Options {
            prevent_abort,
            prevent_cancel,
            prevent_close,
            signal,
        },
        through,
    )?;
    Ok(Invocation::Value(Value::object(output.unwrap_or(result))))
}

fn pipe_object(result: Invocation) -> Result<ObjectId, VmError> {
    if let Invocation::Value(value) = result
        && let ValueRef::Object(id) = value.view()
    {
        return Ok(id);
    }
    Err(VmError::InvalidState(
        "Expected stream operation object".into(),
    ))
}

impl VmCore {
    fn pipe_record_mut(&mut self, target: ObjectId) -> Result<&mut PipeRecord, VmError> {
        match self.heap.get_mut(target)? {
            Object::StreamPipe(record) => Ok(record),
            _ => Err(VmError::InvalidState("Expected stream pipe record".into())),
        }
    }

    fn active_pipe(&self, target: ObjectId) -> Result<Option<&ActivePipe>, VmError> {
        match self.heap.get(target)? {
            Object::StreamPipe(record) => Ok(record.active.as_ref()),
            _ => Err(VmError::InvalidState("Expected stream pipe record".into())),
        }
    }

    fn start_stream_pipe(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        source: ObjectId,
        destination: ObjectId,
        options: Options,
        handled: bool,
    ) -> Result<ObjectId, VmError> {
        let reader = pipe_object(self.acquire_readable_reader(
            roots,
            source,
            self.builtins.get("readableStreamDefaultReader.prototype"),
        )?)?;
        self.readable_reader_mut(reader)?.internal = true;
        let writer = pipe_object(self.acquire_writable_writer(
            roots,
            destination,
            self.builtins.get("writableStreamDefaultWriter.prototype"),
        )?)?;
        self.readable_mut(source)?.disturbed = true;
        let result = self.heap.alloc_promise();
        if handled {
            self.mark_promise_handled(result)?;
        }
        let source_closed = self.readable_reader(reader)?.closed;
        let destination_closed = self.writable_writer(writer)?.closed;
        let signal = options.signal;
        let target = self.heap.alloc(Object::StreamPipe(PipeRecord {
            active: Some(ActivePipe {
                source,
                destination,
                reader,
                writer,
                result,
                options,
                source_closed: Some(source_closed),
                destination_closed: Some(destination_closed),
                ready: None,
                read: None,
                writes: VecDeque::new(),
                dispatching: false,
                shutdown: None,
            }),
        }));
        self.heap.promise_mut(result)?.stream_dependency = Some(target);
        let roots = &roots.with(&target);
        if let Some(signal) = signal {
            let state = self
                .abort_signal_state_mut(signal)?
                .expect("pipe signal brand checked");
            if let Some(reason) = state.reason.clone() {
                let p = self.pipe_record_mut(target)?.active.as_mut().unwrap();
                p.source_closed = None;
                p.destination_closed = None;
                self.abort_stream_pipe(host, roots, target, &reason)?;
                return Ok(result);
            }
            state.pipe_steps.push(target);
        }
        self.observe_pipe(source_closed, target, PipeStage::SourceClosed)?;
        self.observe_pipe(destination_closed, target, PipeStage::DestinationClosed)?;
        self.pump_pipe(host, roots, target)?;
        Ok(result)
    }

    fn observe_pipe(
        &mut self,
        promise: ObjectId,
        target: ObjectId,
        stage: PipeStage,
    ) -> Result<(), VmError> {
        self.observe_stream_job(promise, StreamJob::Pipe { target, stage })
    }

    // A close discovered while processing a read may be the close caused by
    // consuming the final queued chunk. Do not discard that chunk.
    fn poll_pipe_stop(
        &mut self,
        roots: &ActiveRoots<'_>,
        source: ObjectId,
        destination: ObjectId,
        allow_source_close: bool,
    ) -> Result<Option<Stop>, VmError> {
        if let ReadableState::Errored(reason) = &self.readable(source)?.state {
            return Ok(Some(Stop::SourceError(reason.clone())));
        }
        let dest = self.writable(destination)?;
        if let WritableState::Errored(reason) = &dest.state {
            return Ok(Some(Stop::DestinationError(reason.clone())));
        }
        if allow_source_close && matches!(self.readable(source)?.state, ReadableState::Closed) {
            return Ok(Some(Stop::SourceClosed));
        }
        if dest.close.is_some() || matches!(dest.state, WritableState::Closed) {
            return Ok(Some(Stop::DestinationError(
                self.type_error(roots, "Pipe destination is closing or closed")?,
            )));
        }
        Ok(None)
    }

    fn pump_pipe(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
    ) -> Result<(), VmError> {
        let Some(p) = self.active_pipe(target)? else {
            return Ok(());
        };
        if p.shutdown.is_some() || p.dispatching || p.read.is_some() || p.ready.is_some() {
            return Ok(());
        }
        let (source, destination, writer) = (p.source, p.destination, p.writer);
        if let Some(stop) = self.poll_pipe_stop(roots, source, destination, true)? {
            return self.stop_pipe(host, roots, target, stop);
        }
        if !matches!(self.writable(destination)?.state, WritableState::Writable) {
            return Ok(()); // Erroring: the writer's closed promise reports the terminal reason.
        }
        let ready = self.writable_writer(writer)?.ready;
        self.pipe_record_mut(target)?.active.as_mut().unwrap().ready = Some(ready);
        self.observe_pipe(ready, target, PipeStage::Ready)
    }

    pub(crate) fn abort_stream_pipe(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
        reason: &Value,
    ) -> Result<(), VmError> {
        self.stop_pipe(
            host,
            &roots.with(&target),
            target,
            Stop::Signal(reason.clone()),
        )
    }

    fn stop_pipe(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
        stop: Stop,
    ) -> Result<(), VmError> {
        let Some(p) = self.active_pipe(target)? else {
            return Ok(());
        };
        if p.shutdown.is_some() {
            return Ok(());
        }
        let dest = self.writable(p.destination)?;
        let drain = matches!(dest.state, WritableState::Writable) && dest.close.is_none();
        // Pending reads must not hold shutdown open: a source might never
        // produce a chunk. Only a chunk already delivered is eligible to drain.
        let drain_read = drain && self.pipe_read_delivered(p.read);
        self.pipe_record_mut(target)?
            .active
            .as_mut()
            .unwrap()
            .shutdown = Some(Shutdown {
            stop,
            drain,
            drain_read,
            queued: false,
            actions: Vec::new(),
        });
        self.resume_pipe_shutdown(host, roots, target)
    }

    fn pipe_read_delivered(&self, read: Option<ObjectId>) -> bool {
        read.is_some_and(|id| {
            matches!(self.heap.promise(id), Ok(promise) if matches!(promise.state, PromiseState::Fulfilled(_)))
        })
    }

    fn resume_pipe_shutdown(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
    ) -> Result<(), VmError> {
        let Some(p) = self.active_pipe(target)? else {
            return Ok(());
        };
        let Some(s) = &p.shutdown else { return Ok(()) };
        if p.dispatching || s.queued || !s.actions.is_empty() {
            return Ok(());
        }
        if s.drain {
            if !p.writes.is_empty() || s.drain_read {
                return Ok(());
            }
            self.pipe_record_mut(target)?
                .active
                .as_mut()
                .unwrap()
                .shutdown
                .as_mut()
                .unwrap()
                .queued = true;
            self.enqueue_job(
                ReactionJob::Streams(StreamJob::Pipe {
                    target,
                    stage: PipeStage::Shutdown,
                }),
                Settlement::Fulfilled(Value::UNDEFINED),
            )
        } else {
            self.perform_pipe_actions(host, roots, target)
        }
    }

    fn perform_pipe_actions(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
    ) -> Result<(), VmError> {
        let p = self.active_pipe(target)?.unwrap();
        let (source, destination) = (p.source, p.destination);
        let options = p.options.clone();
        let stop = p.shutdown.as_ref().unwrap().stop.clone();
        let undefined = Value::UNDEFINED;
        let reason = stop.reason().unwrap_or(&undefined);
        let roots = &roots.with(reason);
        let abort = !options.prevent_abort
            && match &stop {
                Stop::SourceError(_) => true,
                Stop::Signal(_) => {
                    matches!(self.writable(destination)?.state, WritableState::Writable)
                }
                _ => false,
            };
        if abort {
            let action = pipe_object(self.writable_abort(host, roots, destination, reason)?)?;
            self.add_pipe_action(target, action)?;
        }
        let cancel = !options.prevent_cancel
            && match &stop {
                Stop::DestinationError(_) => true,
                Stop::Signal(_) => matches!(self.readable(source)?.state, ReadableState::Readable),
                _ => false,
            };
        if cancel {
            let action = pipe_object(self.cancel_readable(host, roots, source, reason)?)?;
            self.add_pipe_action(target, action)?;
        }
        if matches!(stop, Stop::SourceClosed) && !options.prevent_close {
            let dest = self.writable(destination)?;
            if dest.close.is_none() {
                match &dest.state {
                    WritableState::Closed => (),
                    WritableState::Errored(reason) => {
                        return self.finish_pipe(roots, target, Some(reason.clone()));
                    }
                    _ => {
                        let action = pipe_object(self.writable_close(host, roots, destination)?)?;
                        self.add_pipe_action(target, action)?;
                    }
                }
            }
        }
        if self
            .active_pipe(target)?
            .unwrap()
            .shutdown
            .as_ref()
            .unwrap()
            .actions
            .is_empty()
        {
            return self.finish_pipe(roots, target, stop.reason().cloned());
        }
        Ok(())
    }

    fn add_pipe_action(&mut self, target: ObjectId, promise: ObjectId) -> Result<(), VmError> {
        self.pipe_record_mut(target)?
            .active
            .as_mut()
            .unwrap()
            .shutdown
            .as_mut()
            .unwrap()
            .actions
            .push(promise);
        self.observe_pipe(promise, target, PipeStage::Action(promise))
    }

    fn finish_pipe(
        &mut self,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
        error: Option<Value>,
    ) -> Result<(), VmError> {
        let p = self.pipe_record_mut(target)?.active.take().unwrap();
        let live = (p.reader, p.writer, p.result, &error);
        let roots = &roots.with(&live);
        for (id, stage) in p.observations() {
            self.heap.promise_mut(id)?.reactions.retain(|reaction| {
                !matches!(reaction, ReactionJob::Streams(StreamJob::Pipe { target: owner, stage: step }) if *owner == target && *step == stage)
            });
        }
        if let Some(signal) = p.options.signal {
            self.abort_signal_state_mut(signal)?
                .expect("pipe signal")
                .pipe_steps
                .retain(|&id| id != target);
        }
        self.release_writable_writer(roots, p.writer)?;
        self.release_readable_reader(roots, p.reader)?;
        self.heap.promise_mut(p.result)?.stream_dependency = None;
        self.settle(
            p.result,
            match error {
                Some(error) => Settlement::Rejected(error),
                None => Settlement::Fulfilled(Value::UNDEFINED),
            },
        )
    }

    pub(crate) fn run_pipe_job(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
        stage: PipeStage,
        settlement: &Settlement,
    ) -> Result<(), VmError> {
        let roots = &roots.with(&target);
        let Some(p) = self.active_pipe(target)? else {
            return Ok(());
        };
        let (source, destination, reader, writer) = (p.source, p.destination, p.reader, p.writer);
        if !matches!(self.heap.get(target)?, Object::StreamPipe(record) if record.accepts_job(stage))
        {
            return Ok(());
        }
        let p = self.pipe_record_mut(target)?.active.as_mut().unwrap();
        match stage {
            PipeStage::SourceClosed => {
                p.source_closed = None;
                if p.shutdown.is_some() {
                    return Ok(());
                }
                let stop = match settlement {
                    Settlement::Fulfilled(_) => Stop::SourceClosed,
                    Settlement::Rejected(reason) => Stop::SourceError(reason.clone()),
                };
                self.stop_pipe(host, roots, target, stop)?;
            }
            PipeStage::DestinationClosed => {
                p.destination_closed = None;
                if p.shutdown.is_none() {
                    let reason = match settlement {
                        Settlement::Fulfilled(_) => {
                            self.type_error(roots, "Pipe destination closed")?
                        }
                        Settlement::Rejected(reason) => reason.clone(),
                    };
                    self.stop_pipe(host, roots, target, Stop::DestinationError(reason))?;
                }
            }
            PipeStage::Ready => {
                p.ready = None;
                if p.shutdown.is_some() {
                    return Ok(());
                }
                if let Some(stop) = self.poll_pipe_stop(roots, source, destination, true)? {
                    return self.stop_pipe(host, roots, target, stop);
                }
                if !matches!(settlement, Settlement::Fulfilled(_))
                    || !matches!(self.writable(destination)?.state, WritableState::Writable)
                {
                    return Ok(());
                }
                self.pipe_record_mut(target)?
                    .active
                    .as_mut()
                    .unwrap()
                    .dispatching = true;
                let read = pipe_object(self.read_readable(host, roots, reader)?)?;
                let delivered = self.pipe_read_delivered(Some(read));
                let p = self.pipe_record_mut(target)?.active.as_mut().unwrap();
                p.dispatching = false;
                p.read = Some(read);
                if let Some(shutdown) = &mut p.shutdown {
                    shutdown.drain_read = shutdown.drain && delivered;
                }
                self.observe_pipe(read, target, PipeStage::Read)?;
                self.resume_pipe_shutdown(host, roots, target)?;
            }
            PipeStage::Read => {
                p.read = None;
                let shutting_down = p.shutdown.is_some();
                let drain = p
                    .shutdown
                    .as_mut()
                    .is_some_and(|s| std::mem::take(&mut s.drain_read));
                if !shutting_down
                    && let Some(stop) = self.poll_pipe_stop(roots, source, destination, false)?
                {
                    return self.stop_pipe(host, roots, target, stop);
                }
                if !shutting_down || drain {
                    match settlement {
                        Settlement::Rejected(error) if !shutting_down => {
                            return self.stop_pipe(
                                host,
                                roots,
                                target,
                                Stop::SourceError(error.clone()),
                            );
                        }
                        Settlement::Fulfilled(result) => {
                            let id = pipe_object(Invocation::Value(result.clone()))?;
                            let value = |name| match self
                                .heap
                                .get(id)?
                                .own_property(PropertyKeyRef::String(name))
                                .map(|prop| prop.into_owned())
                            {
                                Some(Property::Data(value)) => Ok(value),
                                _ => Err(VmError::InvalidState(
                                    "Invalid native pipe read result".into(),
                                )),
                            };
                            if value("done")?.truthy() {
                                if !shutting_down {
                                    return self.stop_pipe(host, roots, target, Stop::SourceClosed);
                                }
                            } else if matches!(
                                self.writable(destination)?.state,
                                WritableState::Writable
                            ) {
                                let chunk = value("value")?;
                                self.pipe_record_mut(target)?
                                    .active
                                    .as_mut()
                                    .unwrap()
                                    .dispatching = true;
                                let write = pipe_object(self.writable_write(
                                    host,
                                    &roots.with(&chunk),
                                    writer,
                                    &chunk,
                                )?)?;
                                let p = self.pipe_record_mut(target)?.active.as_mut().unwrap();
                                p.dispatching = false;
                                p.writes.push_back(write);
                                self.observe_pipe(write, target, PipeStage::Write(write))?;
                            }
                        }
                        _ => (),
                    }
                }
                self.resume_pipe_shutdown(host, roots, target)?;
                self.pump_pipe(host, roots, target)?;
            }
            PipeStage::Write(id) => {
                if p.writes.front() == Some(&id) {
                    p.writes.pop_front();
                } else {
                    p.writes.retain(|&promise| promise != id);
                }
                self.resume_pipe_shutdown(host, roots, target)?;
                self.pump_pipe(host, roots, target)?;
            }
            PipeStage::Shutdown => {
                p.shutdown.as_mut().unwrap().queued = false;
                self.perform_pipe_actions(host, roots, target)?;
            }
            PipeStage::Action(id) => {
                let shutdown = p.shutdown.as_mut().unwrap();
                shutdown.actions.retain(|&promise| promise != id);
                if let Settlement::Rejected(error) = settlement {
                    self.finish_pipe(roots, target, Some(error.clone()))?;
                } else if shutdown.actions.is_empty() {
                    let error = shutdown.stop.reason().cloned();
                    self.finish_pipe(roots, target, error)?;
                }
            }
        }
        Ok(())
    }
}
