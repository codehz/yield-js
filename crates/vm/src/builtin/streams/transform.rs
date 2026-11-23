//! Transform streams compose native readable and writable algorithms. All waits
//! are heap promises; reactions resume by transform identity and stage.

use super::StreamJob;
use super::install;
use super::readable::{
    arg, callback_member, can_close_or_enqueue, converted, illegal_constructor, interface,
    is_undefined, member,
};
use crate::VmError;
use crate::builtin::event::new_target_prototype;
use crate::builtin::globals::Owner;
use crate::builtin::is_callable_object;
use crate::builtin::roots::{RootKind, RootSpec};
use crate::builtin::{FUNCTION_PARENT, OBJECT_PARENT};
use crate::gc::ActiveRoots;
use crate::heap::{
    Attributes, Heap, Object, PlainObject, PromiseState, ReactionJob, ReadableState,
    TransformAlgorithms, TransformStage, TransformStreamObject, WritableFlight, WritableSink,
    WritableState,
};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::{Arguments, Invocation, Settlement};
use crate::value::{ObjectId, Value, ValueRef};
use crate::vm::Vm;

const STREAM_PROTO: &str = "transformStream.prototype";
const CONTROLLER_PROTO: &str = "transformStreamDefaultController.prototype";

interface!(TRANSFORM_OWNER, "transformStream", "TransformStream", STREAM_PROTO,
    KEY => IntrinsicDef::constructor_with_target(KEY, 0, illegal_constructor, construct),
    PrototypeGetter "readable" 0 => readable,
    PrototypeGetter "writable" 0 => writable,
);
interface!(TRANSFORM_CONTROLLER_OWNER, "transformStreamDefaultController", "TransformStreamDefaultController", CONTROLLER_PROTO,
    KEY => IntrinsicDef::call(KEY, 0, illegal_constructor),
    PrototypeGetter "desiredSize" 0 => desired_size,
    Prototype "enqueue" 0 => enqueue,
    Prototype "error" 0 => error,
    Prototype "terminate" 0 => terminate,
);

fn construct(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    target: &Value,
) -> Result<Invocation, VmError> {
    let roots = &roots.with(&args);
    let roots = &roots.with(target);
    let receiver = arg(&args, 0);
    if !matches!(receiver.view(), ValueRef::Undefined | ValueRef::Object(_)) {
        return vm.throw_type_error(roots, "Transformer must be an object");
    }
    // The strategies are Web IDL arguments; the transformer dictionary is
    // converted in the constructor steps, preserving the original receiver.
    let ws = arg(&args, 1);
    let wm = converted!(strategy_mark(vm, host, roots, ws, 1.0));
    let wsize = converted!(callback_member(vm, host, roots, ws, "size"));
    let roots = &roots.with(&wsize);
    let rs = arg(&args, 2);
    let rm = converted!(strategy_mark(vm, host, roots, rs, 0.0));
    let rsize = converted!(callback_member(vm, host, roots, rs, "size"));
    let roots = &roots.with(&rsize);
    let cancel = converted!(callback_member(vm, host, roots, receiver, "cancel"));
    let roots = &roots.with(&cancel);
    let flush = converted!(callback_member(vm, host, roots, receiver, "flush"));
    let roots = &roots.with(&flush);
    let rt = !is_undefined(&converted!(member(
        vm,
        host,
        roots,
        receiver,
        "readableType"
    )));
    let start = converted!(callback_member(vm, host, roots, receiver, "start"));
    let roots = &roots.with(&start);
    let transform = converted!(callback_member(vm, host, roots, receiver, "transform"));
    let roots = &roots.with(&transform);
    let wt = !is_undefined(&converted!(member(
        vm,
        host,
        roots,
        receiver,
        "writableType"
    )));
    let prototype = converted!(new_target_prototype(vm, host, roots, target, STREAM_PROTO));
    if rt || wt {
        return vm.throw_range_error(roots, "Unsupported transform stream type");
    }
    if rm.is_nan() || rm < 0.0 || wm.is_nan() || wm < 0.0 {
        return vm.throw_range_error(roots, "Invalid transform stream highWaterMark");
    }
    let start_promise = vm.heap.alloc_promise();
    let stream = vm.create_transform_internal(
        prototype,
        wm,
        wsize.clone(),
        rm,
        rsize.clone(),
        start_promise,
        TransformAlgorithms {
            receiver: receiver.clone(),
            cancel: cancel.clone(),
            flush: flush.clone(),
            transform: transform.clone(),
        },
    )?;
    let roots = &roots.with(&stream);
    let result = match &start {
        Some(start) => vm.invoke(
            host,
            roots,
            start,
            Arguments::Values(&[Value::object(vm.transform(stream)?.controller)]),
            receiver,
        )?,
        None => Invocation::Value(Value::UNDEFINED),
    };
    match result {
        Invocation::Thrown(error) => Ok(Invocation::Thrown(error)),
        Invocation::Value(value) => {
            vm.resolve_promise(host, &roots.with(&value), start_promise, value.clone())?;
            Ok(Invocation::Value(Value::object(stream)))
        }
    }
}

fn strategy_mark(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    input: &Value,
    default: f64,
) -> Result<Result<f64, Value>, VmError> {
    match member(vm, host, roots, input, "highWaterMark")? {
        Ok(mark) if is_undefined(&mark) => Ok(Ok(default)),
        Ok(mark) => vm.coerce_number(host, &roots.with(&mark), &mark),
        Err(error) => Ok(Err(error)),
    }
}

fn readable(
    vm: &mut Vm,
    _: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    this: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = stream_id(vm, this)? else {
        return vm.throw_type_error(roots, "Expected a TransformStream");
    };
    Ok(Invocation::Value(Value::object(
        vm.transform(stream)?.readable,
    )))
}

fn writable(
    vm: &mut Vm,
    _: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    this: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = stream_id(vm, this)? else {
        return vm.throw_type_error(roots, "Expected a TransformStream");
    };
    Ok(Invocation::Value(Value::object(
        vm.transform(stream)?.writable,
    )))
}

fn desired_size(
    vm: &mut Vm,
    _: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    this: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = controller_stream(vm, this)? else {
        return vm.throw_type_error(roots, "Expected a TransformStreamDefaultController");
    };
    let r = vm.readable(vm.transform(stream)?.readable)?;
    Ok(Invocation::Value(match r.state {
        ReadableState::Closed => Value::number(0.0),
        ReadableState::Errored(_) => Value::NULL,
        ReadableState::Readable => Value::number(r.high_water_mark - r.total_size),
    }))
}

fn enqueue(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    this: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = controller_stream(vm, this)? else {
        return vm.throw_type_error(roots, "Expected a TransformStreamDefaultController");
    };
    vm.transform_enqueue(host, roots, stream, arg(&args, 0))
}

fn error(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    this: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = controller_stream(vm, this)? else {
        return vm.throw_type_error(roots, "Expected a TransformStreamDefaultController");
    };
    vm.transform_error(host, roots, stream, arg(&args, 0))?;
    Ok(Invocation::Value(Value::UNDEFINED))
}

fn terminate(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    this: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = controller_stream(vm, this)? else {
        return vm.throw_type_error(roots, "Expected a TransformStreamDefaultController");
    };
    let roots = &roots.with(&stream);
    vm.request_readable_close(host, roots, vm.transform(stream)?.readable)?;
    let error = vm.type_error(roots, "TransformStream was terminated")?;
    vm.transform_error_writable(host, roots, stream, &error)?;
    Ok(Invocation::Value(Value::UNDEFINED))
}

pub(super) fn stream_id(vm: &Vm, value: &Value) -> Result<Option<ObjectId>, VmError> {
    let ValueRef::Object(id) = value.view() else {
        return Ok(None);
    };
    Ok(matches!(vm.heap.get(id)?, Object::TransformStream(_)).then_some(id))
}

fn controller_stream(vm: &Vm, value: &Value) -> Result<Option<ObjectId>, VmError> {
    let ValueRef::Object(id) = value.view() else {
        return Ok(None);
    };
    Ok(match vm.heap.get(id)? {
        Object::TransformController { stream, .. } => Some(*stream),
        _ => None,
    })
}

impl Vm {
    pub(in crate::builtin) fn transform(
        &self,
        stream: ObjectId,
    ) -> Result<&TransformStreamObject, VmError> {
        match self.heap.get(stream)? {
            Object::TransformStream(s) => Ok(s),
            _ => Err(VmError::InvalidState("expected transform stream".into())),
        }
    }

    fn transform_mut(&mut self, stream: ObjectId) -> Result<&mut TransformStreamObject, VmError> {
        match self.heap.get_mut(stream)? {
            Object::TransformStream(s) => Ok(s),
            _ => Err(VmError::InvalidState("expected transform stream".into())),
        }
    }

    /// Wrap transfer proxies without supplying a local transformer.
    pub(super) fn create_transfer_transform(
        &mut self,
        readable: ObjectId,
        writable: ObjectId,
    ) -> Result<ObjectId, VmError> {
        let prototype = self.builtins.get(STREAM_PROTO);
        let stream = self.heap.alloc_plain_with(Some(prototype));
        let controller = self.heap.alloc(Object::TransformController {
            object: PlainObject::new(Some(self.builtins.get(CONTROLLER_PROTO))),
            stream,
        });
        let backpressure_change = self.heap.alloc_promise();
        *self.heap.get_mut(stream)? = Object::TransformStream(TransformStreamObject {
            transfer_proxy: true,
            object: PlainObject::new(Some(prototype)),
            readable,
            writable,
            controller,
            algorithms: None,
            backpressure: true,
            backpressure_change,
            write: None,
            finish: None,
            write_wait: None,
            finish_wait: None,
            finish_reason: Value::UNDEFINED,
        });
        Ok(stream)
    }

    /// Internal initialization shared by guest transformers and native producers.
    /// The caller owns the shared start capability; neither endpoint runs script here.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::builtin) fn create_transform_internal(
        &mut self,
        prototype: ObjectId,
        writable_hwm: f64,
        writable_size: Option<Value>,
        readable_hwm: f64,
        readable_size: Option<Value>,
        start: ObjectId,
        algorithms: TransformAlgorithms,
    ) -> Result<ObjectId, VmError> {
        let stream = self.heap.alloc_plain_with(Some(prototype));
        let controller = self.heap.alloc(Object::TransformController {
            object: PlainObject::new(Some(self.builtins.get(CONTROLLER_PROTO))),
            stream,
        });
        let source = Value::object(stream);
        let writable = self.create_writable_internal(
            WritableSink {
                receiver: source.clone(),
                size: writable_size,
                ..WritableSink::default()
            },
            writable_hwm,
            start,
        )?;
        let readable =
            self.create_readable_internal(source, readable_hwm, readable_size, false, start)?;
        let backpressure_change = self.heap.alloc_promise();
        *self.heap.get_mut(stream)? = Object::TransformStream(TransformStreamObject {
            object: PlainObject::new(Some(prototype)),
            readable,
            writable,
            controller,
            algorithms: Some(algorithms),
            transfer_proxy: false,
            backpressure: true,
            backpressure_change,
            write: None,
            write_wait: None,
            finish: None,
            finish_wait: None,
            finish_reason: Value::UNDEFINED,
        });
        Ok(stream)
    }

    pub(in crate::builtin) fn transform_enqueue(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
        chunk: &Value,
    ) -> Result<Invocation, VmError> {
        let live = (stream, chunk);
        let roots = &roots.with(&live);
        let readable = self.transform(stream)?.readable;
        if !can_close_or_enqueue(self.readable(readable)?) {
            return self.throw_type_error(roots, "TransformStream cannot accept chunks");
        }
        if let Invocation::Thrown(error) = self.enqueue_readable(host, roots, readable, chunk)? {
            self.transform_error_writable(host, roots, stream, &error)?;
            let stored = match &self.readable(readable)?.state {
                ReadableState::Errored(stored) => stored.clone(),
                _ => error,
            };
            return Ok(Invocation::Thrown(stored));
        }
        let r = self.readable(readable)?;
        let pending = match r.reader {
            Some(reader) => !self.readable_reader(reader)?.requests.is_empty(),
            None => false,
        };
        let should_pull = can_close_or_enqueue(r)
            && r.started
            && (pending || r.high_water_mark - r.total_size > 0.0);
        if !should_pull && !self.transform(stream)?.backpressure {
            self.set_transform_backpressure(stream, true)?;
        }
        Ok(Invocation::Value(Value::UNDEFINED))
    }

    fn set_transform_backpressure(
        &mut self,
        stream: ObjectId,
        pressure: bool,
    ) -> Result<ObjectId, VmError> {
        let old = self.transform(stream)?.backpressure_change;
        let next = self.heap.alloc_promise();
        let s = self.transform_mut(stream)?;
        s.backpressure = pressure;
        s.backpressure_change = next;
        self.settle(old, Settlement::Fulfilled(Value::UNDEFINED))?;
        Ok(next)
    }

    pub(super) fn invoke_transform_pull(&mut self, stream: ObjectId) -> Result<ObjectId, VmError> {
        if self.transform(stream)?.backpressure {
            self.set_transform_backpressure(stream, false)
        } else {
            Ok(self.transform(stream)?.backpressure_change)
        }
    }

    fn unblock_transform_write(&mut self, stream: ObjectId) -> Result<(), VmError> {
        if self.transform(stream)?.backpressure {
            self.set_transform_backpressure(stream, false)?;
        }
        Ok(())
    }

    fn transform_error_writable(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
        error: &Value,
    ) -> Result<(), VmError> {
        let live = (stream, error);
        let roots = &roots.with(&live);
        let s = self.transform_mut(stream)?;
        s.algorithms = None;
        let writable = s.writable;
        self.writable_error(host, roots, writable, error)?;
        self.unblock_transform_write(stream)
    }

    fn transform_error(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
        error: &Value,
    ) -> Result<(), VmError> {
        self.error_readable(self.transform(stream)?.readable, error)?;
        self.transform_error_writable(host, roots, stream, error)
    }

    fn transform_request(&mut self, stream: ObjectId) -> Result<ObjectId, VmError> {
        let p = self.heap.alloc_promise();
        self.heap.promise_mut(p)?.stream_dependency = Some(stream);
        Ok(p)
    }

    pub(super) fn invoke_transform_write(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
        chunk: &Value,
    ) -> Result<ObjectId, VmError> {
        let live = (stream, chunk);
        let roots = &roots.with(&live);
        let result = self.transform_request(stream)?;
        self.transform_mut(stream)?.write = Some(result);
        let s = self.transform(stream)?;
        if s.backpressure {
            self.wait_transform(stream, s.backpressure_change, TransformStage::Backpressure)?;
        } else {
            self.perform_transform(host, roots, stream, chunk)?;
        }
        Ok(result)
    }

    /// Dispatch boundary for native producers: extend the durable algorithm
    /// record and dispatch here for transform, flush, and cancellation. The
    /// callback outcome is normalized and observed by the caller.
    fn invoke_transform_algorithm(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
        stage: TransformStage,
        value: &Value,
    ) -> Result<Invocation, VmError> {
        let s = self.transform(stream)?;
        let Some(algorithms) = &s.algorithms else {
            return Ok(Invocation::Value(Value::UNDEFINED));
        };
        let callback = match stage {
            TransformStage::Transform => &algorithms.transform,
            TransformStage::Flush => &algorithms.flush,
            TransformStage::SinkAbort | TransformStage::SourceCancel => &algorithms.cancel,
            TransformStage::Backpressure => unreachable!("backpressure is not a callback"),
        };
        let Some(callback) = callback.clone() else {
            return if stage == TransformStage::Transform {
                self.transform_enqueue(host, roots, stream, value)
            } else {
                Ok(Invocation::Value(Value::UNDEFINED))
            };
        };
        let receiver = algorithms.receiver.clone();
        let controller = Value::object(s.controller);
        let pair;
        let args = match stage {
            TransformStage::Transform => {
                pair = [value.clone(), controller];
                &pair[..]
            }
            TransformStage::Flush => std::slice::from_ref(&controller),
            _ => std::slice::from_ref(value),
        };
        self.invoke(host, roots, &callback, Arguments::Values(args), &receiver)
    }

    fn perform_transform(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
        chunk: &Value,
    ) -> Result<(), VmError> {
        let result =
            self.invoke_transform_algorithm(host, roots, stream, TransformStage::Transform, chunk)?;
        let wait = self.transform_callback_promise(host, roots, result)?;
        self.wait_transform(stream, wait, TransformStage::Transform)
    }

    pub(super) fn invoke_transform_flush(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
    ) -> Result<ObjectId, VmError> {
        self.finish_transform(
            host,
            roots,
            stream,
            TransformStage::Flush,
            &Value::UNDEFINED,
        )
    }

    pub(super) fn invoke_transform_cancel(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
        reason: &Value,
        sink: bool,
    ) -> Result<ObjectId, VmError> {
        let stage = if sink {
            TransformStage::SinkAbort
        } else {
            TransformStage::SourceCancel
        };
        self.finish_transform(host, roots, stream, stage, reason)
    }

    fn finish_transform(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
        stage: TransformStage,
        reason: &Value,
    ) -> Result<ObjectId, VmError> {
        if let Some(result) = self.transform(stream)?.finish {
            return Ok(result);
        }
        let live = (stream, reason);
        let roots = &roots.with(&live);
        let result = self.transform_request(stream)?;
        let s = self.transform_mut(stream)?;
        s.finish = Some(result);
        s.finish_reason = reason.clone();
        let invocation = self.invoke_transform_algorithm(host, roots, stream, stage, reason)?;
        let wait = self.transform_callback_promise(host, roots, invocation)?;
        self.transform_mut(stream)?.algorithms = None;
        self.wait_transform(stream, wait, stage)?;
        Ok(result)
    }

    fn transform_callback_promise(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        invocation: Invocation,
    ) -> Result<ObjectId, VmError> {
        match invocation {
            Invocation::Value(value) => self.stream_callback_promise(host, roots, value),
            Invocation::Thrown(error) => {
                let promise = self.heap.alloc_promise();
                self.settle(promise, Settlement::Rejected(error))?;
                Ok(promise)
            }
        }
    }

    fn wait_transform(
        &mut self,
        stream: ObjectId,
        wait: ObjectId,
        stage: TransformStage,
    ) -> Result<(), VmError> {
        let s = self.transform_mut(stream)?;
        if is_write_stage(stage) {
            s.write_wait = Some((wait, stage));
        } else {
            s.finish_wait = Some((wait, stage));
        }
        self.observe_stream_job(wait, StreamJob::Transform(stream, stage))
    }

    pub(super) fn resume_transform_job(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
        stage: TransformStage,
        settlement: &Settlement,
    ) -> Result<(), VmError> {
        let roots = &roots.with(&stream);
        let roots = &roots.with(settlement);
        let s = self.transform_mut(stream)?;
        if is_write_stage(stage) {
            s.write_wait = None;
            if stage == TransformStage::Backpressure {
                let writable = s.writable;
                let writable = self.writable(writable)?;
                match &writable.state {
                    WritableState::Erroring(error) | WritableState::Errored(error) => {
                        let error = error.clone();
                        return self.settle_transform_write(stream, Settlement::Rejected(error));
                    }
                    _ => {}
                }
                let chunk = writable
                    .queue
                    .front()
                    .ok_or_else(|| VmError::InvalidState("missing transform write chunk".into()))?
                    .chunk
                    .clone();
                return self.perform_transform(host, &roots.with(&chunk), stream, &chunk);
            }
            if let Settlement::Rejected(error) = settlement {
                self.transform_error(host, roots, stream, error)?;
            }
            return self.settle_transform_write(stream, settlement.clone());
        }
        s.finish_wait = None;
        let readable = s.readable;
        let writable = s.writable;
        let reason = std::mem::replace(&mut s.finish_reason, Value::UNDEFINED);
        let roots = &roots.with(&reason);
        let finish = s
            .finish
            .ok_or_else(|| VmError::InvalidState("missing transform finish promise".into()))?;
        let roots = &roots.with(&finish);
        let result = match stage {
            TransformStage::SourceCancel => match settlement {
                Settlement::Fulfilled(_) => {
                    if let WritableState::Errored(error) = &self.writable(writable)?.state {
                        Settlement::Rejected(error.clone())
                    } else {
                        self.writable_error(host, roots, writable, &reason)?;
                        self.unblock_transform_write(stream)?;
                        Settlement::Fulfilled(Value::UNDEFINED)
                    }
                }
                Settlement::Rejected(error) => {
                    self.writable_error(host, roots, writable, error)?;
                    self.unblock_transform_write(stream)?;
                    Settlement::Rejected(error.clone())
                }
            },
            TransformStage::Flush | TransformStage::SinkAbort => match settlement {
                Settlement::Fulfilled(_) => {
                    if let ReadableState::Errored(error) = &self.readable(readable)?.state {
                        Settlement::Rejected(error.clone())
                    } else {
                        if stage == TransformStage::Flush {
                            self.request_readable_close(host, roots, readable)?;
                        } else {
                            self.error_readable(readable, &reason)?;
                        }
                        Settlement::Fulfilled(Value::UNDEFINED)
                    }
                }
                Settlement::Rejected(error) => {
                    self.error_readable(readable, error)?;
                    Settlement::Rejected(error.clone())
                }
            },
            TransformStage::Backpressure | TransformStage::Transform => unreachable!(),
        };
        self.settle(finish, result)
    }

    fn settle_transform_write(
        &mut self,
        stream: ObjectId,
        settlement: Settlement,
    ) -> Result<(), VmError> {
        let result = self
            .transform_mut(stream)?
            .write
            .take()
            .ok_or_else(|| VmError::InvalidState("missing transform write promise".into()))?;
        self.settle(result, settlement)
    }
}

fn is_write_stage(stage: TransformStage) -> bool {
    matches!(
        stage,
        TransformStage::Backpressure | TransformStage::Transform
    )
}

pub(super) fn validate_transform_job(
    heap: &Heap,
    source: ObjectId,
    id: ObjectId,
    stage: TransformStage,
) -> bool {
    let Ok(Object::TransformStream(s)) = heap.get(id) else {
        return false;
    };
    let wait = if is_write_stage(stage) {
        s.write_wait
    } else {
        s.finish_wait
    };
    wait == Some((source, stage))
}

pub(super) fn validate_transform_dependency(heap: &Heap, promise: ObjectId, id: ObjectId) -> bool {
    matches!(heap.get(id), Ok(Object::TransformStream(s)) if s.write == Some(promise)
        || (s.finish == Some(promise) && s.finish_wait.is_some()))
}

pub(super) fn validate_transform_record(heap: &Heap, id: ObjectId, object: &Object) -> bool {
    let s = match object {
        Object::TransformStream(s) => s,
        Object::TransformController { stream, .. } => {
            return matches!(heap.get(*stream), Ok(Object::TransformStream(s)) if s.controller == id);
        }
        _ => return true,
    };
    let Ok(Object::ReadableStream(readable)) = heap.get(s.readable) else {
        return false;
    };
    let Ok(Object::WritableStream(writable)) = heap.get(s.writable) else {
        return false;
    };
    if s.transfer_proxy {
        let proxy_source = |value: &Value, endpoint, readable| {
            matches!(value.view(), ValueRef::Object(link)
                if matches!(heap.get(link), Ok(Object::StreamTransfer(r)) if r.owns_proxy(endpoint, readable)))
        };
        return matches!(heap.get(s.controller), Ok(Object::TransformController { stream, .. }) if *stream == id)
            && readable.byte.is_none()
            && readable.high_water_mark == 0.0
            && writable.high_water_mark == 1.0
            && readable.pull.is_none()
            && readable.cancel.is_none()
            && readable.size.is_none()
            && (proxy_source(&readable.source, s.readable, true)
                || ((!matches!(readable.state, ReadableState::Readable)
                    || readable.close_requested)
                    && is_undefined(&readable.source)))
            && (proxy_source(&writable.sink.receiver, s.writable, false)
                || (writable.sink == WritableSink::default()
                    && (!matches!(writable.state, crate::heap::WritableState::Writable)
                        || writable.flight == Some(WritableFlight::Close))))
            && s.algorithms.is_none()
            && s.backpressure
            && s.write.is_none()
            && s.finish.is_none()
            && s.write_wait.is_none()
            && s.finish_wait.is_none()
            && is_undefined(&s.finish_reason)
            && heap
                .promise(s.backpressure_change)
                .is_ok_and(|p| matches!(p.state, PromiseState::Pending));
    }
    let owner = Value::object(id);
    let pending = |p| {
        heap.promise(p)
            .is_ok_and(|p| matches!(p.state, PromiseState::Pending))
    };
    let request = |promise| {
        heap.promise(promise).is_ok_and(|p| {
            matches!(p.state, PromiseState::Pending)
                && !p.locked
                && p.adopting.is_none()
                && p.stream_dependency == Some(id)
        })
    };
    let wait = |source, stage| {
        heap.promise(source).is_ok_and(|p| {
            matches!(p.state, PromiseState::Pending)
                && p.reactions
                    .iter()
                    .filter(|r| **r == ReactionJob::Streams(StreamJob::Transform(id, stage)))
                    .count()
                    == 1
        })
    };
    matches!(heap.get(s.controller), Ok(Object::TransformController { stream, .. }) if *stream == id)
        && readable.byte.is_none()
        && readable.pull.is_none()
        && readable.cancel.is_none()
        && writable.sink.write.is_none()
        && writable.sink.close.is_none()
        && writable.sink.abort.is_none()
        && (readable.source == owner
            || (!matches!(readable.state, ReadableState::Readable)
                && is_undefined(&readable.source)))
        && (writable.sink.receiver == owner || is_undefined(&writable.sink.receiver))
        && pending(s.backpressure_change) && s.algorithms.as_ref().is_none_or(|a| {
        s.finish.is_none() && [&a.transform, &a.flush, &a.cancel].into_iter().flatten().all(|v| {
                matches!(v.view(), ValueRef::Object(f) if heap.get(f).is_ok_and(is_callable_object))
            })
    }) && match (s.write, s.write_wait) {
        (None, None) => true,
        (Some(p), Some((source, stage))) => {
            request(p)
                && is_write_stage(stage)
                && wait(source, stage)
                && writable.flight == Some(WritableFlight::Write)
                && writable.flight_wait == Some(p)
                && (stage != TransformStage::Backpressure
                    || (s.backpressure && source == s.backpressure_change))
        }
        _ => false,
    } && match (s.finish, s.finish_wait) {
        (None, None) => is_undefined(&s.finish_reason),
        (Some(p), None) => {
            heap.promise(p)
                .is_ok_and(|p| !matches!(p.state, PromiseState::Pending))
                && is_undefined(&s.finish_reason)
        }
        (Some(p), Some((source, stage))) => {
            request(p)
                && !is_write_stage(stage)
                && wait(source, stage)
                && (stage != TransformStage::Flush
                    || (is_undefined(&s.finish_reason)
                        && writable.flight == Some(WritableFlight::Close)
                        && writable.flight_wait == Some(p)))
                && (stage != TransformStage::SourceCancel
                    || matches!(readable.state, ReadableState::Closed))
                && (stage != TransformStage::SinkAbort
                    || (matches!(writable.flight, Some(WritableFlight::Abort(_)))
                        && writable.flight_wait == Some(p)))
        }
        _ => false,
    }
}
