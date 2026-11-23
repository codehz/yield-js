//! Writable stream slots and sink algorithms. Queue entries keep their size
//! until the corresponding sink write settles.

use super::readable::{
    arg, callback_member, converted, illegal_constructor, interface, is_undefined, member,
};
use super::{StreamJob, install};
use crate::VmError;
use crate::builtin::event::new_target_prototype;
use crate::builtin::globals::Owner;
use crate::builtin::roots::{RootKind, RootSpec};
use crate::builtin::{FUNCTION_PARENT, OBJECT_PARENT};
use crate::gc::ActiveRoots;
use crate::heap::{
    Attributes, Object, PlainObject, PromiseState, WritableAbort, WritableFlight, WritableRequest,
    WritableSink, WritableState, WritableStreamObject, WritableWriterObject,
};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::{Arguments, Invocation, Settlement};
use crate::value::{ObjectId, Value, ValueRef};
use crate::vm::Vm;

const STREAM_PROTO: &str = "writableStream.prototype";
const WRITER_PROTO: &str = "writableStreamDefaultWriter.prototype";
const CONTROLLER_PROTO: &str = "writableStreamDefaultController.prototype";

interface!(WRITABLE_OWNER, "writableStream", "WritableStream", STREAM_PROTO,
    KEY => IntrinsicDef::constructor_with_target(KEY, 0, illegal_constructor, construct),
    PrototypeGetter "locked" 0 => locked,
    Prototype "abort" 0 => stream_abort,
    Prototype "close" 0 => stream_close,
    Prototype "getWriter" 0 => get_writer,
);
interface!(WRITER_OWNER, "writableStreamDefaultWriter", "WritableStreamDefaultWriter", WRITER_PROTO,
    KEY => IntrinsicDef::constructor_with_target(KEY, 1, illegal_constructor, construct_writer),
    PrototypeGetter "closed" 0 => closed,
    PrototypeGetter "desiredSize" 0 => desired_size,
    PrototypeGetter "ready" 0 => ready,
    Prototype "abort" 0 => writer_abort,
    Prototype "close" 0 => writer_close,
    Prototype "releaseLock" 0 => release_lock,
    Prototype "write" 0 => write,
);
interface!(WRITABLE_CONTROLLER_OWNER, "writableStreamDefaultController", "WritableStreamDefaultController", CONTROLLER_PROTO,
    KEY => IntrinsicDef::call(KEY, 0, illegal_constructor),
    PrototypeGetter "signal" 0 => signal,
    Prototype "error" 0 => error,
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
    let sink = arg(&args, 0);
    if !matches!(sink.view(), ValueRef::Undefined | ValueRef::Object(_)) {
        return vm.throw_type_error(roots, "Underlying sink must be an object");
    }
    let abort = converted!(callback_member(vm, host, roots, sink, "abort"));
    let roots = &roots.with(&abort);
    let close = converted!(callback_member(vm, host, roots, sink, "close"));
    let roots = &roots.with(&close);
    let start = converted!(callback_member(vm, host, roots, sink, "start"));
    let roots = &roots.with(&start);
    let kind = converted!(member(vm, host, roots, sink, "type"));
    let has_type = !is_undefined(&kind);
    let write = converted!(callback_member(vm, host, roots, sink, "write"));
    let roots = &roots.with(&write);
    let strategy = arg(&args, 1);
    let mark = converted!(member(vm, host, roots, strategy, "highWaterMark"));
    let mark = if is_undefined(&mark) {
        1.0
    } else {
        converted!(vm.coerce_number(host, &roots.with(&mark), &mark))
    };
    let size = converted!(callback_member(vm, host, roots, strategy, "size"));
    let roots = &roots.with(&size);
    let prototype = converted!(new_target_prototype(vm, host, roots, target, STREAM_PROTO));
    if has_type {
        return vm.throw_range_error(roots, "Unsupported writable stream type");
    }
    if mark.is_nan() || mark < 0.0 {
        return vm.throw_range_error(roots, "Invalid writable stream highWaterMark");
    }
    vm.create_writable(
        host,
        roots,
        prototype,
        WritableSink {
            receiver: sink.clone(),
            write: write.clone(),
            close: close.clone(),
            abort: abort.clone(),
            size: size.clone(),
        },
        start.clone(),
        mark,
    )
}

fn object(id: ObjectId) -> Invocation {
    Invocation::Value(Value::object(id))
}

fn stream_id(vm: &Vm, value: &Value) -> Result<Option<ObjectId>, VmError> {
    let ValueRef::Object(id) = value.view() else {
        return Ok(None);
    };
    Ok(matches!(vm.heap.get(id)?, Object::WritableStream(_)).then_some(id))
}

fn writer_id(vm: &Vm, value: &Value) -> Result<Option<ObjectId>, VmError> {
    let ValueRef::Object(id) = value.view() else {
        return Ok(None);
    };
    Ok(matches!(vm.heap.get(id)?, Object::WritableWriter(_)).then_some(id))
}

fn controller_stream(vm: &Vm, value: &Value) -> Result<Option<ObjectId>, VmError> {
    let ValueRef::Object(id) = value.view() else {
        return Ok(None);
    };
    Ok(match vm.heap.get(id)? {
        Object::WritableController { stream, .. } => Some(*stream),
        _ => None,
    })
}

fn locked(
    vm: &mut Vm,
    _: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    this: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = stream_id(vm, this)? else {
        return vm.throw_type_error(roots, "Expected a WritableStream");
    };
    Ok(Invocation::Value(Value::boolean(
        vm.writable(stream)?.writer.is_some(),
    )))
}

fn get_writer(
    vm: &mut Vm,
    _: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    this: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = stream_id(vm, this)? else {
        return vm.throw_type_error(roots, "Expected a WritableStream");
    };
    vm.acquire_writable_writer(roots, stream, vm.builtins.get(WRITER_PROTO))
}

fn construct_writer(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    target: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = stream_id(vm, arg(&args, 0))? else {
        return vm.throw_type_error(roots, "Expected a WritableStream");
    };
    let live = (stream, target);
    let roots = &roots.with(&live);
    let prototype = converted!(new_target_prototype(vm, host, roots, target, WRITER_PROTO));
    vm.acquire_writable_writer(roots, stream, prototype)
}

fn stream_abort(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    this: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = stream_id(vm, this)? else {
        return vm.reject_writable_type(roots, "Expected a WritableStream");
    };
    if vm.writable(stream)?.writer.is_some() {
        return vm.reject_writable_type(roots, "WritableStream is locked");
    }
    vm.writable_abort(host, roots, stream, arg(&args, 0))
}

fn stream_close(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    this: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = stream_id(vm, this)? else {
        return vm.reject_writable_type(roots, "Expected a WritableStream");
    };
    if vm.writable(stream)?.writer.is_some() {
        return vm.reject_writable_type(roots, "WritableStream is locked");
    }
    vm.writable_close(host, roots, stream)
}

fn writer_abort(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    this: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = writer_id(vm, this)?
        .map(|id| vm.writable_writer(id))
        .transpose()?
        .and_then(|w| w.stream)
    else {
        return vm.reject_writable_type(roots, "Expected a writer with a stream");
    };
    vm.writable_abort(host, roots, stream, arg(&args, 0))
}

fn writer_close(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    this: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = writer_id(vm, this)?
        .map(|id| vm.writable_writer(id))
        .transpose()?
        .and_then(|w| w.stream)
    else {
        return vm.reject_writable_type(roots, "Expected a writer with a stream");
    };
    vm.writable_close(host, roots, stream)
}

fn write(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    this: &Value,
) -> Result<Invocation, VmError> {
    let Some(writer) = writer_id(vm, this)? else {
        return vm.reject_writable_type(roots, "Expected a WritableStreamDefaultWriter");
    };
    vm.writable_write(host, roots, writer, arg(&args, 0))
}

fn release_lock(
    vm: &mut Vm,
    _: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    this: &Value,
) -> Result<Invocation, VmError> {
    let Some(writer) = writer_id(vm, this)? else {
        return vm.throw_type_error(roots, "Expected a WritableStreamDefaultWriter");
    };
    vm.release_writable_writer(roots, writer)?;
    Ok(Invocation::Value(Value::UNDEFINED))
}

fn closed(
    vm: &mut Vm,
    _: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    this: &Value,
) -> Result<Invocation, VmError> {
    let Some(writer) = writer_id(vm, this)? else {
        return vm.throw_type_error(roots, "Expected a WritableStreamDefaultWriter");
    };
    Ok(object(vm.writable_writer(writer)?.closed))
}

fn ready(
    vm: &mut Vm,
    _: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    this: &Value,
) -> Result<Invocation, VmError> {
    let Some(writer) = writer_id(vm, this)? else {
        return vm.throw_type_error(roots, "Expected a WritableStreamDefaultWriter");
    };
    Ok(object(vm.writable_writer(writer)?.ready))
}

fn desired_size(
    vm: &mut Vm,
    _: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    this: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = writer_id(vm, this)?
        .map(|id| vm.writable_writer(id))
        .transpose()?
        .and_then(|w| w.stream)
    else {
        return vm.throw_type_error(roots, "Expected a writer with a stream");
    };
    let s = vm.writable(stream)?;
    Ok(Invocation::Value(match s.state {
        WritableState::Writable => Value::number(s.high_water_mark - s.total_size),
        WritableState::Closed => Value::number(0.0),
        WritableState::Erroring(_) | WritableState::Errored(_) => Value::NULL,
    }))
}

fn signal(
    vm: &mut Vm,
    _: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    this: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = controller_stream(vm, this)? else {
        return vm.throw_type_error(roots, "Expected a WritableStreamDefaultController");
    };
    Ok(object(vm.writable(stream)?.signal))
}

fn error(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    this: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = controller_stream(vm, this)? else {
        return vm.throw_type_error(roots, "Expected a WritableStreamDefaultController");
    };
    vm.writable_error(host, roots, stream, arg(&args, 0))?;
    Ok(Invocation::Value(Value::UNDEFINED))
}

impl Vm {
    pub(crate) fn writable(&self, id: ObjectId) -> Result<&WritableStreamObject, VmError> {
        match self.heap.get(id)? {
            Object::WritableStream(s) => Ok(s),
            _ => Err(VmError::InvalidState("expected writable stream".into())),
        }
    }

    fn writable_mut(&mut self, id: ObjectId) -> Result<&mut WritableStreamObject, VmError> {
        match self.heap.get_mut(id)? {
            Object::WritableStream(s) => Ok(s),
            _ => Err(VmError::InvalidState("expected writable stream".into())),
        }
    }

    pub(crate) fn writable_writer(&self, id: ObjectId) -> Result<&WritableWriterObject, VmError> {
        match self.heap.get(id)? {
            Object::WritableWriter(w) => Ok(w),
            _ => Err(VmError::InvalidState("expected writable writer".into())),
        }
    }

    fn writable_writer_mut(&mut self, id: ObjectId) -> Result<&mut WritableWriterObject, VmError> {
        match self.heap.get_mut(id)? {
            Object::WritableWriter(w) => Ok(w),
            _ => Err(VmError::InvalidState("expected writable writer".into())),
        }
    }

    pub(crate) fn create_writable(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        prototype: ObjectId,
        sink: WritableSink,
        start: Option<Value>,
        high_water_mark: f64,
    ) -> Result<Invocation, VmError> {
        let controller_proto = self.builtins.get(CONTROLLER_PROTO);
        let controller = self.heap.alloc_plain_with(Some(controller_proto));
        let signal = self.alloc_abort_signal(Default::default());
        let stream = self
            .heap
            .alloc(Object::WritableStream(WritableStreamObject {
                object: PlainObject::new(Some(prototype)),
                controller,
                signal,
                writer: None,
                state: WritableState::Writable,
                sink,
                queue: Default::default(),
                total_size: 0.0,
                high_water_mark,
                backpressure: high_water_mark <= 0.0,
                close: None,
                abort: None,
                started: false,
                start_wait: None,
                flight: None,
                flight_wait: None,
            }));
        *self.heap.get_mut(controller)? = Object::WritableController {
            object: PlainObject::new(Some(controller_proto)),
            stream,
        };
        let roots = &roots.with(&stream);
        let result = if let Some(start) = start {
            let receiver = self.writable(stream)?.sink.receiver.clone();
            match self.invoke(
                host,
                roots,
                &start,
                Arguments::Values(&[Value::object(controller)]),
                &receiver,
            )? {
                Invocation::Value(v) => v,
                thrown => return Ok(thrown),
            }
        } else {
            Value::UNDEFINED
        };
        let wait = self.stream_callback_promise(host, roots, result)?;
        self.writable_mut(stream)?.start_wait = Some(wait);
        self.observe_stream_job(wait, StreamJob::WritableStart(stream))?;
        Ok(object(stream))
    }

    fn writable_request(&mut self, owner: ObjectId) -> Result<ObjectId, VmError> {
        let p = self.heap.alloc_promise();
        self.heap.promise_mut(p)?.stream_dependency = Some(owner);
        Ok(p)
    }

    fn writable_fulfilled(&mut self) -> Result<ObjectId, VmError> {
        let p = self.heap.alloc_promise();
        self.settle(p, Settlement::Fulfilled(Value::UNDEFINED))?;
        Ok(p)
    }

    fn reject_writable_type(
        &mut self,
        roots: &ActiveRoots<'_>,
        message: &str,
    ) -> Result<Invocation, VmError> {
        let error = self.type_error(roots, message)?;
        self.reject_writable(error)
    }

    fn reject_writable(&mut self, error: Value) -> Result<Invocation, VmError> {
        let p = self.heap.alloc_promise();
        self.settle(p, Settlement::Rejected(error))?;
        Ok(object(p))
    }

    fn ensure_writable_rejected(
        &mut self,
        promise: ObjectId,
        error: &Value,
    ) -> Result<ObjectId, VmError> {
        let p = if matches!(self.heap.promise(promise)?.state, PromiseState::Pending) {
            promise
        } else {
            self.heap.alloc_promise()
        };
        self.settle(p, Settlement::Rejected(error.clone()))?;
        self.mark_promise_handled(p)?;
        Ok(p)
    }

    pub(crate) fn acquire_writable_writer(
        &mut self,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
        prototype: ObjectId,
    ) -> Result<Invocation, VmError> {
        let s = self.writable(stream)?;
        if s.writer.is_some() {
            return self.throw_type_error(&roots.with(&stream), "WritableStream is locked");
        }
        let pending_ready =
            matches!(s.state, WritableState::Writable) && s.close.is_none() && s.backpressure;
        let state = s.state.clone();
        let ready = self.heap.alloc_promise();
        let closed = self.heap.alloc_promise();
        let writer = self
            .heap
            .alloc(Object::WritableWriter(WritableWriterObject {
                object: PlainObject::new(Some(prototype)),
                stream: Some(stream),
                ready,
                closed,
            }));
        self.writable_mut(stream)?.writer = Some(writer);
        for p in [ready, closed] {
            self.heap.promise_mut(p)?.stream_dependency = Some(writer);
        }
        match state {
            WritableState::Writable => {
                if !pending_ready {
                    self.settle(ready, Settlement::Fulfilled(Value::UNDEFINED))?;
                }
            }
            WritableState::Closed => {
                self.settle(ready, Settlement::Fulfilled(Value::UNDEFINED))?;
                self.settle(closed, Settlement::Fulfilled(Value::UNDEFINED))?;
            }
            WritableState::Erroring(error) => {
                self.ensure_writable_rejected(ready, &error)?;
            }
            WritableState::Errored(error) => {
                self.ensure_writable_rejected(ready, &error)?;
                self.ensure_writable_rejected(closed, &error)?;
            }
        }
        Ok(object(writer))
    }

    pub(crate) fn release_writable_writer(
        &mut self,
        roots: &ActiveRoots<'_>,
        writer: ObjectId,
    ) -> Result<(), VmError> {
        let w = self.writable_writer(writer)?;
        let Some(stream) = w.stream else {
            return Ok(());
        };
        let (ready, closed) = (w.ready, w.closed);
        let error = self.type_error(&roots.with(&writer), "Writer lock released")?;
        let ready = self.ensure_writable_rejected(ready, &error)?;
        let closed = self.ensure_writable_rejected(closed, &error)?;
        let w = self.writable_writer_mut(writer)?;
        w.ready = ready;
        w.closed = closed;
        w.stream = None;
        self.writable_mut(stream)?.writer = None;
        Ok(())
    }

    pub(crate) fn writable_write(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        writer: ObjectId,
        chunk: &Value,
    ) -> Result<Invocation, VmError> {
        let Some(stream) = self.writable_writer(writer)?.stream else {
            return self.reject_writable_type(roots, "Writer lock released");
        };
        let live = (writer, stream, chunk);
        let roots = &roots.with(&live);
        let size = if let Some(callback) = self.writable(stream)?.sink.size.clone() {
            let result = match self.invoke(
                host,
                roots,
                &callback,
                Arguments::Values(std::slice::from_ref(chunk)),
                &Value::UNDEFINED,
            )? {
                Invocation::Value(v) => self.coerce_number(host, &roots.with(&v), &v)?,
                Invocation::Thrown(e) => Err(e),
            };
            match result {
                Ok(n) => n,
                Err(e) => {
                    self.writable_error(host, roots, stream, &e)?;
                    1.0
                }
            }
        } else {
            1.0
        };
        if self.writable_writer(writer)?.stream != Some(stream) {
            return self.reject_writable_type(roots, "Writer lock released during size conversion");
        }
        let s = self.writable(stream)?;
        if let WritableState::Errored(error) = &s.state {
            return self.reject_writable(error.clone());
        }
        if s.close.is_some() || matches!(s.state, WritableState::Closed) {
            return self.reject_writable_type(roots, "WritableStream is closing or closed");
        }
        if let WritableState::Erroring(error) = &s.state {
            return self.reject_writable(error.clone());
        }
        let promise = self.writable_request(stream)?;
        let roots = &roots.with(&promise);
        let valid = size.is_finite() && size >= 0.0;
        let s = self.writable_mut(stream)?;
        // An invalid size still has a write request to be rejected on finishing erroring.
        let size = if valid { size } else { 0.0 };
        s.queue.push_back(WritableRequest {
            promise,
            chunk: chunk.clone(),
            size,
        });
        s.total_size += size;
        if valid {
            self.update_writable_backpressure(stream)?;
            self.advance_writable(host, roots, stream)?;
        } else {
            let error = self.make_range_error(roots, "Invalid writable chunk size")?;
            self.writable_error(host, roots, stream, &error)?;
        }
        Ok(object(promise))
    }

    pub(crate) fn writable_close(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&stream);
        let s = self.writable(stream)?;
        if s.close.is_some() || matches!(s.state, WritableState::Closed | WritableState::Errored(_))
        {
            return self.reject_writable_type(roots, "WritableStream cannot be closed");
        }
        let writer = if matches!(s.state, WritableState::Writable) && s.backpressure {
            s.writer
        } else {
            None
        };
        let p = self.writable_request(stream)?;
        self.writable_mut(stream)?.close = Some(p);
        if let Some(writer) = writer {
            self.settle(
                self.writable_writer(writer)?.ready,
                Settlement::Fulfilled(Value::UNDEFINED),
            )?;
        }
        self.advance_writable(host, &roots.with(&p), stream)?;
        Ok(object(p))
    }

    pub(crate) fn writable_abort(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
        reason: &Value,
    ) -> Result<Invocation, VmError> {
        let live = (stream, reason);
        let roots = &roots.with(&live);
        let s = self.writable(stream)?;
        if matches!(s.state, WritableState::Closed | WritableState::Errored(_)) {
            return Ok(object(self.writable_fulfilled()?));
        }
        self.abort_signal(host, roots, s.signal, reason.clone())?;
        let s = self.writable(stream)?;
        if matches!(s.state, WritableState::Closed | WritableState::Errored(_)) {
            return Ok(object(self.writable_fulfilled()?));
        }
        if let Some(abort) = &s.abort {
            return Ok(object(abort.promise));
        }
        let already_erroring = matches!(s.state, WritableState::Erroring(_));
        let p = self.writable_request(stream)?;
        self.writable_mut(stream)?.abort = Some(WritableAbort {
            promise: p,
            already_erroring,
        });
        if !already_erroring {
            self.start_writable_erroring(host, &roots.with(&p), stream, reason)?;
        }
        Ok(object(p))
    }

    pub(crate) fn writable_error(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
        error: &Value,
    ) -> Result<(), VmError> {
        if matches!(self.writable(stream)?.state, WritableState::Writable) {
            self.writable_mut(stream)?.sink = WritableSink::default();
            self.start_writable_erroring(host, roots, stream, error)?;
        }
        Ok(())
    }

    fn start_writable_erroring(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
        error: &Value,
    ) -> Result<(), VmError> {
        let s = self.writable_mut(stream)?;
        s.state = WritableState::Erroring(error.clone());
        if let Some(writer) = s.writer {
            let ready =
                self.ensure_writable_rejected(self.writable_writer(writer)?.ready, error)?;
            self.writable_writer_mut(writer)?.ready = ready;
        }
        self.advance_writable(host, roots, stream)
    }

    fn update_writable_backpressure(&mut self, stream: ObjectId) -> Result<(), VmError> {
        let s = self.writable_mut(stream)?;
        let backpressure = s.high_water_mark - s.total_size <= 0.0;
        if s.backpressure == backpressure {
            return Ok(());
        }
        s.backpressure = backpressure;
        if let Some(writer) = s.writer {
            if backpressure {
                let ready = self.writable_request(writer)?;
                self.writable_writer_mut(writer)?.ready = ready;
            } else {
                self.settle(
                    self.writable_writer(writer)?.ready,
                    Settlement::Fulfilled(Value::UNDEFINED),
                )?;
            }
        }
        Ok(())
    }

    fn advance_writable(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
    ) -> Result<(), VmError> {
        let roots = &roots.with(&stream);
        let s = self.writable_mut(stream)?;
        if !s.started || s.flight.is_some() {
            return Ok(());
        }
        if matches!(s.state, WritableState::Erroring(_)) {
            return self.finish_writable_erroring(host, roots, stream);
        }
        if !matches!(s.state, WritableState::Writable) {
            return Ok(());
        }
        if let Some(request) = s.queue.front() {
            let args = [request.chunk.clone(), Value::object(s.controller)];
            let callback = s.sink.write.clone();
            let receiver = s.sink.receiver.clone();
            s.flight = Some(WritableFlight::Write);
            self.invoke_writable_flight(host, roots, stream, callback, receiver, &args)
        } else if s.close.is_some() {
            let callback = s.sink.close.clone();
            let receiver = s.sink.receiver.clone();
            s.flight = Some(WritableFlight::Close);
            self.invoke_writable_flight(host, roots, stream, callback, receiver, &[])?;
            self.writable_mut(stream)?.sink = WritableSink::default();
            Ok(())
        } else {
            Ok(())
        }
    }

    fn invoke_writable_flight(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
        callback: Option<Value>,
        receiver: Value,
        args: &[Value],
    ) -> Result<(), VmError> {
        let wait = if let Some(callback) = callback {
            match self.invoke(host, roots, &callback, Arguments::Values(args), &receiver)? {
                Invocation::Value(value) => self.stream_callback_promise(host, roots, value)?,
                Invocation::Thrown(error) => {
                    let p = self.heap.alloc_promise();
                    self.settle(p, Settlement::Rejected(error))?;
                    p
                }
            }
        } else {
            self.writable_fulfilled()?
        };
        self.writable_mut(stream)?.flight_wait = Some(wait);
        self.observe_stream_job(wait, StreamJob::WritableFlight(stream))
    }

    fn finish_writable_erroring(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
    ) -> Result<(), VmError> {
        let s = self.writable_mut(stream)?;
        let WritableState::Erroring(error) = &s.state else {
            unreachable!()
        };
        let error = error.clone();
        s.state = WritableState::Errored(error.clone());
        s.total_size = 0.0;
        let requests = std::mem::take(&mut s.queue);
        let abort = s.abort.take();
        for request in requests {
            self.settle(request.promise, Settlement::Rejected(error.clone()))?;
        }
        if let Some(abort) = abort {
            if !abort.already_erroring {
                let s = self.writable_mut(stream)?;
                let callback = s.sink.abort.clone();
                let receiver = s.sink.receiver.clone();
                s.flight = Some(WritableFlight::Abort(abort.promise));
                self.invoke_writable_flight(host, roots, stream, callback, receiver, &[error])?;
                self.writable_mut(stream)?.sink = WritableSink::default();
                return Ok(());
            }
            self.settle(abort.promise, Settlement::Rejected(error.clone()))?;
        }
        self.writable_mut(stream)?.sink = WritableSink::default();
        self.reject_writable_close(stream, &error)
    }

    fn reject_writable_close(&mut self, stream: ObjectId, error: &Value) -> Result<(), VmError> {
        if let Some(close) = self.writable_mut(stream)?.close.take() {
            self.settle(close, Settlement::Rejected(error.clone()))?;
        }
        if let Some(writer) = self.writable(stream)?.writer {
            let closed = self.writable_writer(writer)?.closed;
            if matches!(self.heap.promise(closed)?.state, PromiseState::Pending) {
                self.ensure_writable_rejected(closed, error)?;
            }
        }
        Ok(())
    }

    pub(super) fn resume_writable_job(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        job: StreamJob,
        settlement: &Settlement,
    ) -> Result<(), VmError> {
        let stream = job.target();
        let rejected = match settlement {
            Settlement::Rejected(e) => Some(e),
            _ => None,
        };
        let s = self.writable_mut(stream)?;
        if matches!(job, StreamJob::WritableStart(_)) {
            s.start_wait = None;
            s.started = true;
            if let Some(error) = rejected {
                if matches!(s.state, WritableState::Writable) {
                    return self.start_writable_erroring(host, roots, stream, error);
                }
            }
            return self.advance_writable(host, roots, stream);
        }
        s.flight_wait = None;
        let result = rejected.map_or(Settlement::Fulfilled(Value::UNDEFINED), |e| {
            Settlement::Rejected(e.clone())
        });
        match s.flight.take().expect("writable flight job") {
            WritableFlight::Write => {
                let request = s.queue.pop_front().expect("in-flight write request");
                let writable = matches!(s.state, WritableState::Writable);
                let update = rejected.is_none() && writable && s.close.is_none();
                // Infinity is sticky until ResetQueue, as in DequeueValue.
                s.total_size = (s.total_size - request.size).max(0.0);
                self.settle(request.promise, result)?;
                if let Some(error) = rejected {
                    if writable {
                        return self.writable_error(host, roots, stream, error);
                    }
                } else if update {
                    self.update_writable_backpressure(stream)?;
                }
                self.advance_writable(host, roots, stream)
            }
            WritableFlight::Close => {
                let close = s.close.take().expect("in-flight close request");
                let abort = s.abort.take();
                let writable = matches!(s.state, WritableState::Writable);
                self.settle(close, result.clone())?;
                if let Some(abort) = abort {
                    self.settle(abort.promise, result)?;
                }
                if let Some(error) = rejected {
                    if writable {
                        return self.start_writable_erroring(host, roots, stream, error);
                    }
                    self.advance_writable(host, roots, stream)
                } else {
                    let s = self.writable_mut(stream)?;
                    s.state = WritableState::Closed;
                    if let Some(writer) = s.writer {
                        self.settle(
                            self.writable_writer(writer)?.closed,
                            Settlement::Fulfilled(Value::UNDEFINED),
                        )?;
                    }
                    Ok(())
                }
            }
            WritableFlight::Abort(promise) => {
                let WritableState::Errored(error) = &s.state else {
                    unreachable!()
                };
                let error = error.clone();
                self.settle(promise, result)?;
                self.reject_writable_close(stream, &error)
            }
        }
    }
}
