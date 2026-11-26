//! ReadableStream's iterable source and asynchronous iterator bindings.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use super::readable::{READER_PROTO, arg, stream_id};
use super::{StreamJob, dictionary_member};
use crate::VmError;
use crate::builtin::define_builtin_data;
use crate::gc::ActiveRoots;
use crate::heap::{
    ASYNC_ITERATOR_SYMBOL, Attributes, Heap, ITERATOR_SYMBOL, Object, OwnedEdge, PlainObject,
    PromiseState, TO_STRING_TAG_SYMBOL, push_value,
};
use crate::host::{Host, PlatformServices};
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::{Arguments, Invocation, Settlement};
use crate::value::{ObjectId, PropertyKey, PropertyKeyRef, Value, ValueRef};
use crate::vm::VmCore;

const PROTO: &str = "ReadableStreamAsyncIterator.prototype";
const NEXT: IntrinsicKey =
    IntrinsicKey::new("ReadableStreamAsyncIterator", Slot::Prototype, "next");
const RETURN: IntrinsicKey =
    IntrinsicKey::new("ReadableStreamAsyncIterator", Slot::Prototype, "return");
const ITERATE: IntrinsicKey = IntrinsicKey::new(
    "ReadableStreamAsyncIterator",
    Slot::PrototypeSymbol,
    "asyncIterator",
);
impl<P: crate::host::PlatformServices> BuiltinTable<P> {
    pub(super) const FROM: IntrinsicDef<P> = IntrinsicDef::call(
        IntrinsicKey::new("ReadableStream", Slot::Static, "from"),
        1,
        from,
    );
}
impl<P: crate::host::PlatformServices> BuiltinTable<P> {
    pub(super) const VALUES: IntrinsicDef<P> = IntrinsicDef::call(
        IntrinsicKey::new("ReadableStream", Slot::Prototype, "values"),
        0,
        values,
    );
}

impl<P: crate::host::PlatformServices> BuiltinTable<P> {
    pub(crate) const INTRINSICS: &[IntrinsicDef<P>] = &[
        Self::FROM,
        Self::VALUES,
        IntrinsicDef::call(NEXT, 0, |vm, host, roots, _, receiver| {
            vm.queue_iterator_request(host, roots, receiver, None)
        }),
        IntrinsicDef::call(RETURN, 1, |vm, host, roots, args, receiver| {
            vm.queue_iterator_request(host, roots, receiver, Some(arg(&args, 0).clone()))
        }),
        IntrinsicDef::call(ITERATE, 0, |_, _, _, _, receiver| {
            Ok(Invocation::Value(receiver.clone()))
        }),
    ];
}

pub(crate) fn install(vm: &mut VmCore) {
    let stream = vm.builtins.get("readableStream");
    let prototype = vm.builtins.get("readableStream.prototype");
    let from = vm.alloc_intrinsic(BuiltinTable::<crate::host::Platform>::FROM.key);
    define_builtin_data(
        vm,
        stream,
        BuiltinTable::<crate::host::Platform>::FROM.key.member,
        Value::object(from),
        Attributes::ORDINARY,
    );
    // Web IDL requires `values` and `[Symbol.asyncIterator]` to be one function.
    let values = vm.alloc_intrinsic(BuiltinTable::<crate::host::Platform>::VALUES.key);
    define_builtin_data(
        vm,
        prototype,
        BuiltinTable::<crate::host::Platform>::VALUES.key.member,
        Value::object(values),
        Attributes::ORDINARY,
    );
    define_builtin_data(
        vm,
        prototype,
        PropertyKey::Symbol(ASYNC_ITERATOR_SYMBOL),
        Value::object(values),
        Attributes::METHOD,
    );
    let iterator = vm.builtins.get(PROTO);
    for key in [NEXT, RETURN] {
        let method = vm.alloc_intrinsic(key);
        define_builtin_data(
            vm,
            iterator,
            key.member,
            Value::object(method),
            Attributes::ORDINARY,
        );
    }
    let iterate = vm.alloc_intrinsic(ITERATE);
    define_builtin_data(
        vm,
        iterator,
        PropertyKey::Symbol(ASYNC_ITERATOR_SYMBOL),
        Value::object(iterate),
        Attributes::METHOD,
    );
    define_builtin_data(
        vm,
        iterator,
        PropertyKey::Symbol(TO_STRING_TAG_SYMBOL),
        Value::string("ReadableStream AsyncIterator".into()),
        Attributes::FUNCTION_METADATA,
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IterationStage {
    FromNext,
    FromNextValue,
    FromReturn,
    Read,
    Cancel,
    Advance,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct FromFlight {
    stage: IterationStage,
    output: ObjectId,
    wait: ObjectId,
    stream: Option<ObjectId>,
    done: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FromIterator {
    pub(crate) object: PlainObject,
    iterator: Value,
    next: Value,
    sync: bool,
    pull: Option<FromFlight>,
    close: Option<FromFlight>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IteratorRequest {
    result: ObjectId,
    // None is next(); Some(undefined) is return(undefined).
    reason: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamIterator {
    pub(crate) object: PlainObject,
    reader: Option<ObjectId>,
    prevent_cancel: bool,
    requests: VecDeque<IteratorRequest>,
    wait: Option<(IterationStage, ObjectId)>,
}

impl StreamIterator {
    pub(super) fn owns_result(&self, id: ObjectId) -> bool {
        self.requests.iter().any(|request| request.result == id)
    }
}

impl FromIterator {
    pub(super) fn owns_output(&self, id: ObjectId) -> bool {
        self.pull
            .iter()
            .chain(self.close.iter())
            .any(|flight| flight.output == id)
    }

    pub(crate) fn write_edges(&self, out: &mut Vec<OwnedEdge>) {
        push_value(out, &self.iterator);
        push_value(out, &self.next);
        for flight in self.pull.iter().chain(self.close.iter()) {
            out.push(OwnedEdge::Object(flight.output));
            out.push(OwnedEdge::Object(flight.wait));
            if let Some(stream) = flight.stream {
                out.push(OwnedEdge::Object(stream));
            }
        }
    }
}

impl StreamIterator {
    pub(crate) fn write_edges(&self, out: &mut Vec<OwnedEdge>) {
        if let Some(reader) = self.reader {
            out.push(OwnedEdge::Object(reader));
        }
        if let Some((_, wait)) = self.wait {
            out.push(OwnedEdge::Object(wait));
        }
        for request in &self.requests {
            out.push(OwnedEdge::Object(request.result));
            if let Some(reason) = &request.reason {
                push_value(out, reason);
            }
        }
    }
}

macro_rules! script {
    ($operation:expr) => {
        match $operation? {
            Invocation::Value(value) => value,
            thrown => return Ok(thrown),
        }
    };
}

fn from<P: PlatformServices>(
    vm: &mut VmCore,
    host: &Host<P>,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    _: &Value,
) -> Result<Invocation, VmError> {
    let iterable = arg(&args, 0);
    if !matches!(iterable.view(), ValueRef::Object(_)) {
        return vm.throw_type_error(roots, "ReadableStream.from requires an iterable object");
    }
    let method = script!(vm.read_property(
        host,
        roots,
        iterable,
        PropertyKeyRef::Symbol(ASYNC_ITERATOR_SYMBOL)
    ));
    let sync = nullish(&method);
    let method = if sync {
        script!(vm.read_property(
            host,
            roots,
            iterable,
            PropertyKeyRef::Symbol(ITERATOR_SYMBOL)
        ))
    } else {
        method
    };
    let roots = &roots.with(&method);
    if !vm.is_callable(&method) {
        return vm.throw_type_error(roots, "Iterator method must be callable");
    }
    let (iterator, next) = match vm.get_iterator_from_method(host, roots, iterable, &method)? {
        Ok(record) => record,
        Err(error) => return Ok(Invocation::Thrown(error)),
    };
    let source = vm.heap.alloc(Object::StreamFromIterator(FromIterator {
        object: PlainObject::new(None),
        iterator,
        next,
        sync,
        pull: None,
        close: None,
    }));
    let source = Value::object(source);
    let _roots = &roots.with(&source);
    let start = vm.readable_settled_promise(Settlement::Fulfilled(Value::UNDEFINED))?;
    let stream = vm.create_readable_internal(source, 0.0, None, false, start)?;
    Ok(Invocation::Value(Value::object(stream)))
}

fn values<P: PlatformServices>(
    vm: &mut VmCore,
    host: &Host<P>,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = stream_id(vm, receiver)? else {
        return vm.throw_type_error(
            roots,
            "ReadableStream.values called on incompatible receiver",
        );
    };
    let roots = &roots.with(&stream);
    let prevent_cancel = script!(dictionary_member(
        vm,
        host,
        roots,
        arg(&args, 0),
        "preventCancel"
    ))
    .truthy();
    let reader = script!(vm.acquire_readable_reader(roots, stream, vm.builtins.get(READER_PROTO)));
    let ValueRef::Object(reader) = reader.view() else {
        unreachable!("reader allocation")
    };
    let object = PlainObject::new(Some(vm.builtins.get(PROTO)));
    let iterator = vm.heap.alloc(Object::StreamIterator(StreamIterator {
        object,
        reader: Some(reader),
        prevent_cancel,
        requests: VecDeque::new(),
        wait: None,
    }));
    vm.readable_reader_mut(reader)?.internal = true;
    vm.readable_reader_mut(reader)?.iterator = Some(iterator);
    Ok(Invocation::Value(Value::object(iterator)))
}

fn nullish(value: &Value) -> bool {
    matches!(value.view(), ValueRef::Null | ValueRef::Undefined)
}

impl VmCore {
    fn iterator_record(&self, source: ObjectId) -> Result<&FromIterator, VmError> {
        match self.heap.get(source)? {
            Object::StreamFromIterator(record) => Ok(record),
            _ => Err(VmError::InvalidState(
                "Invalid ReadableStream iterable source".into(),
            )),
        }
    }

    fn flight_mut(
        &mut self,
        source: ObjectId,
        closing: bool,
    ) -> Result<&mut Option<FromFlight>, VmError> {
        match self.heap.get_mut(source)? {
            Object::StreamFromIterator(record) => Ok(if closing {
                &mut record.close
            } else {
                &mut record.pull
            }),
            _ => Err(VmError::InvalidState(
                "Invalid ReadableStream iterable source".into(),
            )),
        }
    }

    pub(super) fn pull_from_iterator<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        source: ObjectId,
        stream: ObjectId,
    ) -> Result<ObjectId, VmError> {
        let record = self.iterator_record(source)?;
        let (iterator, next, sync) = (record.iterator.clone(), record.next.clone(), record.sync);
        let live = (source, stream, (&iterator, &next));
        let roots = &roots.with(&live);
        let result = self.invoke(host, roots, &next, Arguments::EMPTY, &iterator)?;
        self.begin_from_flight(host, roots, source, Some(stream), sync, result)
    }

    pub(super) fn cancel_from_iterator<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        source: ObjectId,
        reason: &Value,
    ) -> Result<ObjectId, VmError> {
        let record = self.iterator_record(source)?;
        let (iterator, sync) = (record.iterator.clone(), record.sync);
        let live = (source, &iterator, reason);
        let roots = &roots.with(&live);
        let result =
            match self.read_property(host, roots, &iterator, PropertyKeyRef::String("return"))? {
                Invocation::Value(method) if nullish(&method) => {
                    return self.readable_settled_promise(Settlement::Fulfilled(Value::UNDEFINED));
                }
                Invocation::Value(method) => self.invoke(
                    host,
                    roots,
                    &method,
                    Arguments::Values(std::slice::from_ref(reason)),
                    &iterator,
                )?,
                thrown => thrown,
            };
        self.begin_from_flight(host, roots, source, None, sync, result)
    }

    fn begin_from_flight<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        source: ObjectId,
        stream: Option<ObjectId>,
        sync: bool,
        result: Invocation,
    ) -> Result<ObjectId, VmError> {
        let result = match result {
            Invocation::Value(result) => result,
            Invocation::Thrown(error) => {
                return self.readable_settled_promise(Settlement::Rejected(error));
            }
        };
        let roots = &roots.with(&result);
        let closing = stream.is_none();
        if !closing && !matches!(result.view(), ValueRef::Object(_)) {
            let error = self.type_error(roots, "Iterator next must return an object")?;
            return self.readable_settled_promise(Settlement::Rejected(error));
        }
        let (stage, value, done) = if closing {
            (IterationStage::FromReturn, result.clone(), false)
        } else if sync {
            match self.result_parts(host, roots, &result)? {
                Ok((done, value)) => (IterationStage::FromNextValue, value, done),
                Err(error) => return self.readable_settled_promise(Settlement::Rejected(error)),
            }
        } else {
            (IterationStage::FromNext, result.clone(), false)
        };
        let wait = self.stream_callback_promise(host, roots, value)?;
        let output = self.heap.alloc_promise();
        self.heap.promise_mut(output)?.stream_dependency = Some(source);
        *self.flight_mut(source, closing)? = Some(FromFlight {
            stage,
            output,
            wait,
            stream,
            done,
        });
        self.observe_stream_job(
            wait,
            StreamJob::Iteration {
                target: source,
                stage,
            },
        )?;
        Ok(output)
    }

    fn result_parts<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        result: &Value,
    ) -> Result<Result<(bool, Value), Value>, VmError> {
        if !matches!(result.view(), ValueRef::Object(_)) {
            return Ok(Err(
                self.type_error(roots, "Iterator method must return an object")?
            ));
        }
        let roots = &roots.with(result);
        let done = match self.read_property(host, roots, result, PropertyKeyRef::String("done"))? {
            Invocation::Value(value) => value.truthy(),
            Invocation::Thrown(error) => return Ok(Err(error)),
        };
        if done {
            return Ok(Ok((true, Value::UNDEFINED)));
        }
        let value =
            match self.read_property(host, roots, result, PropertyKeyRef::String("value"))? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
        Ok(Ok((false, value)))
    }

    fn resume_source_iteration<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        source: ObjectId,
        stage: IterationStage,
        settlement: &Settlement,
    ) -> Result<(), VmError> {
        let closing = matches!(stage, IterationStage::FromReturn);
        let flight = self
            .flight_mut(source, closing)?
            .take()
            .filter(|flight| flight.stage == stage)
            .ok_or_else(|| {
                VmError::InvalidState("Unexpected iterable source continuation".into())
            })?;
        let live = (source, flight.output, flight.stream);
        let roots = &roots.with(&live);
        let outcome = match settlement {
            Settlement::Rejected(error) => {
                // The async-from-sync next wrapper closes a still-open sync iterator
                // when assimilation of its yielded value rejects, preserving the error.
                if stage == IterationStage::FromNextValue && !flight.done {
                    let iterator = self.iterator_record(source)?.iterator.clone();
                    self.close_iterator(host, roots, &iterator)?;
                }
                Err(error.clone())
            }
            Settlement::Fulfilled(result) => match stage {
                IterationStage::FromNext => self.result_parts(host, roots, result)?,
                IterationStage::FromNextValue => Ok((flight.done, result.clone())),
                IterationStage::FromReturn if !matches!(result.view(), ValueRef::Object(_)) => {
                    Err(self.type_error(roots, "Iterator return must return an object")?)
                }
                _ => Ok((true, Value::UNDEFINED)),
            },
        };
        let settlement = match outcome {
            Ok((done, value)) => {
                if let Some(stream) = flight.stream {
                    if done {
                        self.request_readable_close(host, roots, stream)?;
                    } else if let Invocation::Thrown(error) =
                        self.enqueue_readable(host, roots, stream, &value)?
                    {
                        return self
                            .finish_iteration_promise(flight.output, Settlement::Rejected(error));
                    }
                }
                Settlement::Fulfilled(Value::UNDEFINED)
            }
            Err(error) => Settlement::Rejected(error),
        };
        self.finish_iteration_promise(flight.output, settlement)
    }

    fn stream_iterator_mut(&mut self, target: ObjectId) -> Result<&mut StreamIterator, VmError> {
        match self.heap.get_mut(target)? {
            Object::StreamIterator(iterator) => Ok(iterator),
            _ => Err(VmError::InvalidState(
                "Invalid stream iterator continuation".into(),
            )),
        }
    }

    fn queue_iterator_request<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        reason: Option<Value>,
    ) -> Result<Invocation, VmError> {
        let target = match receiver.view() {
            ValueRef::Object(id) if matches!(self.heap.get(id)?, Object::StreamIterator(_)) => id,
            _ => {
                let error =
                    self.type_error(roots, "Incompatible ReadableStream async iterator receiver")?;
                let result = self.readable_settled_promise(Settlement::Rejected(error))?;
                return Ok(Invocation::Value(Value::object(result)));
            }
        };
        let queued_reason = reason.clone();
        let queued = (target, queued_reason.as_ref());
        let roots = &roots.with(&queued);
        let result = self.heap.alloc_promise();
        self.heap.promise_mut(result)?.stream_dependency = Some(target);
        let iterator = self.stream_iterator_mut(target)?;
        let idle = iterator.requests.is_empty();
        iterator
            .requests
            .push_back(IteratorRequest { result, reason });
        if idle {
            self.run_iterator_request(host, roots, target)?;
        }
        Ok(Invocation::Value(Value::object(result)))
    }

    fn run_iterator_request<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
    ) -> Result<(), VmError> {
        let roots = &roots.with(&target);
        let iterator = self.stream_iterator_mut(target)?;
        let request = iterator.requests.front().expect("queued iterator request");
        let reason = request.reason.clone();
        let prevent_cancel = iterator.prevent_cancel;
        let reader = iterator.reader;
        let Some(reader) = reader else {
            return self.finish_iterator_result(target, reason.unwrap_or(Value::UNDEFINED), true);
        };
        let live = (reader, &reason);
        let roots = &roots.with(&live);
        let (stage, wait) = if let Some(reason) = reason.as_ref() {
            self.stream_iterator_mut(target)?.reader = None;
            if prevent_cancel {
                self.release_readable_reader(roots, reader)?;
                return self.finish_iterator_result(target, reason.clone(), true);
            }
            let stream = self.readable_reader(reader)?.stream.ok_or_else(|| {
                VmError::InvalidState("Stream iterator reader lost its stream".into())
            })?;
            let result = self.cancel_readable(host, roots, stream, reason)?;
            self.release_readable_reader(roots, reader)?;
            (IterationStage::Cancel, result)
        } else {
            (
                IterationStage::Read,
                self.read_readable(host, roots, reader)?,
            )
        };
        match wait {
            Invocation::Value(value) => {
                let ValueRef::Object(wait) = value.view() else {
                    unreachable!("stream operation promise")
                };
                self.wait_iteration(target, stage, wait)
            }
            Invocation::Thrown(error) => {
                self.finish_iterator_request(target, Settlement::Rejected(error))
            }
        }
    }

    fn wait_iteration(
        &mut self,
        target: ObjectId,
        stage: IterationStage,
        wait: ObjectId,
    ) -> Result<(), VmError> {
        self.stream_iterator_mut(target)?.wait = Some((stage, wait));
        self.observe_stream_job(wait, StreamJob::Iteration { target, stage })
    }

    fn finish_iterator_result(
        &mut self,
        target: ObjectId,
        value: Value,
        done: bool,
    ) -> Result<(), VmError> {
        let result = self.iterator_result_value(value, done)?;
        self.finish_iterator_request(target, Settlement::Fulfilled(result))
    }

    fn finish_iterator_request(
        &mut self,
        target: ObjectId,
        settlement: Settlement,
    ) -> Result<(), VmError> {
        let iterator = self.stream_iterator_mut(target)?;
        let request = iterator
            .requests
            .pop_front()
            .expect("active iterator request");
        let advance = !iterator.requests.is_empty();
        self.finish_iteration_promise(request.result, settlement)?;
        if advance {
            // Start the next operation on a separate microtask, on rejection as well.
            let gate = self.readable_settled_promise(Settlement::Fulfilled(Value::UNDEFINED))?;
            self.wait_iteration(target, IterationStage::Advance, gate)?;
        }
        Ok(())
    }

    /// Synchronous close/error steps release an internal async iterator reader
    /// before its pending read result reaches the iterator machinery.
    pub(super) fn release_finished_stream_iterator(
        &mut self,
        roots: &ActiveRoots<'_>,
        reader: ObjectId,
    ) -> Result<(), VmError> {
        let Some(target) = self.readable_reader_mut(reader)?.iterator.take() else {
            return Ok(());
        };
        if let Ok(iterator) = self.stream_iterator_mut(target)
            && iterator.reader == Some(reader)
        {
            iterator.reader = None;
        }
        self.release_readable_reader(roots, reader)
    }

    fn finish_iterator_reader(
        &mut self,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
    ) -> Result<(), VmError> {
        if let Some(reader) = self.stream_iterator_mut(target)?.reader.take() {
            self.release_readable_reader(roots, reader)?;
        }
        Ok(())
    }

    pub(super) fn resume_iteration_job<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
        stage: IterationStage,
        settlement: &Settlement,
    ) -> Result<(), VmError> {
        match stage {
            IterationStage::FromNext
            | IterationStage::FromNextValue
            | IterationStage::FromReturn => {
                return self.resume_source_iteration(host, roots, target, stage, settlement);
            }
            _ => {}
        }
        let roots = &roots.with(&target);
        let operation = self.stream_iterator_mut(target)?.wait.take();
        if !operation.is_some_and(|(expected, _)| expected == stage) {
            return Err(VmError::InvalidState(
                "Unexpected stream iterator continuation".into(),
            ));
        }
        match (stage, settlement) {
            (IterationStage::Advance, _) => self.run_iterator_request(host, roots, target),
            (IterationStage::Cancel, Settlement::Fulfilled(_)) => {
                let value = self
                    .stream_iterator_mut(target)?
                    .requests
                    .front()
                    .and_then(|request| request.reason.clone())
                    .expect("return reason");
                self.finish_iterator_result(target, value, true)
            }
            (IterationStage::Read, Settlement::Fulfilled(result)) => {
                // The native default reader yields an ordinary iterator result with
                // own data properties. No user-overridable reader method is involved.
                match self.result_parts(host, roots, result)? {
                    Ok((false, _)) => self.finish_iterator_request(target, settlement.clone()),
                    Ok((true, _)) => {
                        self.finish_iterator_reader(roots, target)?;
                        self.finish_iterator_request(target, settlement.clone())
                    }
                    Err(error) => {
                        self.finish_iterator_reader(roots, target)?;
                        self.finish_iterator_request(target, Settlement::Rejected(error))
                    }
                }
            }
            (IterationStage::Read | IterationStage::Cancel, Settlement::Rejected(error)) => {
                self.finish_iterator_reader(roots, target)?;
                self.finish_iterator_request(target, Settlement::Rejected(error.clone()))
            }
            _ => unreachable!("iterable source continuation already dispatched"),
        }
    }

    fn finish_iteration_promise(
        &mut self,
        promise: ObjectId,
        settlement: Settlement,
    ) -> Result<(), VmError> {
        self.heap.promise_mut(promise)?.stream_dependency = None;
        self.settle(promise, settlement)
    }
}

pub(super) fn validate_job(
    heap: &Heap,
    source: ObjectId,
    target: ObjectId,
    stage: IterationStage,
) -> bool {
    match heap.get(target) {
        Ok(Object::StreamFromIterator(record)) => record
            .pull
            .iter()
            .chain(record.close.iter())
            .any(|flight| flight.stage == stage && flight.wait == source),
        Ok(Object::StreamIterator(record)) => record.wait == Some((stage, source)),
        _ => false,
    }
}

pub(super) fn validate_record(heap: &Heap, id: ObjectId, object: &Object) -> bool {
    use crate::heap::ReactionJob;
    let pending_output = |output| {
        heap.promise(output).is_ok_and(|promise| {
            matches!(promise.state, PromiseState::Pending)
                && !promise.locked
                && promise.adopting.is_none()
                && promise.stream_dependency == Some(id)
        })
    };
    let wait_for = |stage, wait| {
        heap.promise(wait).is_ok_and(|promise| {
            matches!(promise.state, PromiseState::Pending)
                && promise
                    .reactions
                    .iter()
                    .filter(|job| {
                        **job == ReactionJob::Streams(StreamJob::Iteration { target: id, stage })
                    })
                    .count()
                    == 1
        })
    };
    match object {
        Object::StreamFromIterator(record) => {
            matches!(record.iterator.view(), ValueRef::Object(_))
                && record.pull.is_none_or(|flight| {
                    flight.stage
                        == if record.sync {
                            IterationStage::FromNextValue
                        } else {
                            IterationStage::FromNext
                        }
                        && flight.stream.is_some_and(|stream| {
                            matches!(heap.get(stream),
                            Ok(Object::ReadableStream(s)) if s.pull_wait == Some(flight.output))
                        })
                        && pending_output(flight.output)
                        && wait_for(flight.stage, flight.wait)
                })
                && record.close.is_none_or(|flight| {
                    flight.stage == IterationStage::FromReturn
                        && flight.stream.is_none()
                        && pending_output(flight.output)
                        && wait_for(flight.stage, flight.wait)
                })
        }
        Object::StreamIterator(record) => {
            if !record.reader.is_none_or(|reader_id| {
                matches!(heap.get(reader_id),
                Ok(Object::ReadableReader(reader)) if !reader.byob && reader.internal
                    && reader.iterator == Some(id) && reader.stream.is_some())
            }) || record
                .requests
                .iter()
                .any(|request| !pending_output(request.result))
            {
                return false;
            }
            let mut results = std::collections::BTreeSet::new();
            if !record
                .requests
                .iter()
                .all(|request| results.insert(request.result))
            {
                return false;
            }
            match (record.requests.front(), record.wait) {
                (None, None) => true,
                (Some(request), Some((IterationStage::Read, wait))) => {
                    request.reason.is_none() && wait_for(IterationStage::Read, wait)
                        && record.reader.is_some_and(|id| matches!(heap.get(id),
                            Ok(Object::ReadableReader(reader)) if reader.requests.front() == Some(&wait)))
                }
                (Some(request), Some((IterationStage::Cancel, wait))) => {
                    request.reason.is_some() && record.reader.is_none() && wait_for(IterationStage::Cancel, wait)
                }
                (Some(_), Some((IterationStage::Advance, wait))) => {
                    wait_for(IterationStage::Advance, wait)
                }
                _ => false,
            }
        }
        _ => true,
    }
}

pub(crate) struct BuiltinTable<P>(std::marker::PhantomData<P>);
