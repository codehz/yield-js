//! Readable streams. Durable callback waits are heap edges and stream
//! reactions; synchronous guest calls never hold heap borrows.

use std::collections::VecDeque;

use super::{StreamJob, dictionary_member, install};
use crate::VmError;
use crate::builtin::event::new_target_prototype;
use crate::builtin::globals::Owner;
use crate::builtin::roots::{RootKind, RootSpec};
use crate::builtin::{FUNCTION_PARENT, OBJECT_PARENT};
use crate::gc::ActiveRoots;
use crate::heap::{
    Attributes, Object, PlainObject, ReadableReaderObject, ReadableState, ReadableStreamObject,
};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::{Arguments, Invocation, Settlement};
use crate::value::{ObjectId, Value, ValueRef};
use crate::vm::VmCore;

const STREAM_PROTO: &str = "readableStream.prototype";
pub(super) const READER_PROTO: &str = "readableStreamDefaultReader.prototype";
const CONTROLLER_PROTO: &str = "readableStreamDefaultController.prototype";

macro_rules! interface {
    ($owner:ident, $root:literal, $name:literal, $proto:ident, $key:ident => $ctor:expr,
        $($slot:ident $member:literal $len:literal => $call:expr),* $(,)?) => {
        pub(in crate::builtin) const $owner: Owner = {
            const $key: IntrinsicKey = IntrinsicKey::new($name, Slot::Global, "");
            const MEMBERS: &[IntrinsicDef] = &[
                $ctor,
                $(IntrinsicDef::call(IntrinsicKey::new($name, Slot::$slot, $member), $len, $call),)*
            ];
            Owner::new(
                $root,
                &[
                    RootSpec::new($root, RootKind::Native($key), FUNCTION_PARENT)
                        .fixed($proto)
                        .backlink(Some(crate::heap::Attributes::METHOD)),
                    RootSpec::new($proto, RootKind::Plain, OBJECT_PARENT),
                ],
                &[MEMBERS],
                Some(|vm, _| install(vm, $name, $root, $proto, MEMBERS)),
            )
        };
    };
}
pub(super) use interface;

pub(in crate::builtin) const STREAM_OWNER: Owner = {
    const KEY: IntrinsicKey = IntrinsicKey::new("ReadableStream", Slot::Global, "");
    const MEMBERS: &[IntrinsicDef] = &[
        IntrinsicDef::constructor_with_target(KEY, 0, illegal_constructor, construct_stream),
        IntrinsicDef::call(
            IntrinsicKey::new("ReadableStream", Slot::PrototypeGetter, "locked"),
            0,
            locked,
        ),
        IntrinsicDef::call(
            IntrinsicKey::new("ReadableStream", Slot::Prototype, "cancel"),
            0,
            stream_cancel,
        ),
        IntrinsicDef::call(
            IntrinsicKey::new("ReadableStream", Slot::Prototype, "getReader"),
            0,
            get_reader,
        ),
        IntrinsicDef::call(
            IntrinsicKey::new("ReadableStream", Slot::Prototype, "pipeTo"),
            1,
            super::pipe::pipe_to,
        ),
        IntrinsicDef::call(
            IntrinsicKey::new("ReadableStream", Slot::Prototype, "pipeThrough"),
            1,
            super::pipe::pipe_through,
        ),
        IntrinsicDef::call(
            IntrinsicKey::new("ReadableStream", Slot::Prototype, "tee"),
            0,
            super::tee::tee,
        ),
    ];
    Owner::new(
        "readableStream",
        &[
            RootSpec::new("readableStream", RootKind::Native(KEY), FUNCTION_PARENT)
                .fixed(STREAM_PROTO)
                .backlink(Some(Attributes::METHOD)),
            RootSpec::new(STREAM_PROTO, RootKind::Plain, OBJECT_PARENT),
            RootSpec::new(
                "ReadableStreamAsyncIterator.prototype",
                RootKind::Plain,
                Some(crate::builtin::iterator::ASYNC_ROOT),
            ),
        ],
        &[MEMBERS, super::iteration::INTRINSICS],
        Some(|vm, _| {
            install(
                vm,
                "ReadableStream",
                "readableStream",
                STREAM_PROTO,
                MEMBERS,
            );
            super::iteration::install(vm);
        }),
    )
};
interface!(READER_OWNER, "readableStreamDefaultReader", "ReadableStreamDefaultReader", READER_PROTO,
    KEY => IntrinsicDef::constructor_with_target(KEY, 1, illegal_constructor, construct_reader),
    PrototypeGetter "closed" 0 => reader_closed,
    Prototype "cancel" 0 => reader_cancel,
    Prototype "read" 0 => reader_read,
    Prototype "releaseLock" 0 => release_lock,
);
interface!(CONTROLLER_OWNER, "readableStreamDefaultController", "ReadableStreamDefaultController", CONTROLLER_PROTO,
    KEY => IntrinsicDef::call(KEY, 0, illegal_constructor),
    PrototypeGetter "desiredSize" 0 => desired_size,
    Prototype "close" 0 => controller_close,
    Prototype "enqueue" 0 => controller_enqueue,
    Prototype "error" 0 => controller_error,
);

pub(super) fn illegal_constructor(
    vm: &mut VmCore,
    _: &Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    _: &Value,
) -> Result<Invocation, VmError> {
    vm.throw_type_error(roots, "Illegal stream constructor call")
}

pub(super) fn arg(args: &[Value], index: usize) -> &Value {
    static UNDEFINED: Value = Value::UNDEFINED;
    args.get(index).unwrap_or(&UNDEFINED)
}

pub(super) fn is_undefined(value: &Value) -> bool {
    matches!(value.view(), ValueRef::Undefined)
}

// Unwrap an IDL conversion, propagating a script exception rather than a VM error.
macro_rules! converted {
    ($expression:expr) => {
        match $expression? {
            Ok(value) => value,
            Err(error) => return Ok(Invocation::Thrown(error)),
        }
    };
}
pub(super) use converted;

pub(super) mod bytes;
pub(in crate::builtin) use bytes::{BYOB_READER_OWNER, BYOB_REQUEST_OWNER, BYTE_CONTROLLER_OWNER};
use bytes::{BYOB_READER_PROTO, BYTE_CONTROLLER_PROTO, enforce_unsigned};

pub(super) fn member(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    input: &Value,
    name: &str,
) -> Result<Result<Value, Value>, VmError> {
    Ok(match dictionary_member(vm, host, roots, input, name)? {
        Invocation::Value(value) => Ok(value),
        Invocation::Thrown(error) => Err(error),
    })
}

pub(super) fn callback_member(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    input: &Value,
    name: &str,
) -> Result<Result<Option<Value>, Value>, VmError> {
    let value = match member(vm, host, roots, input, name)? {
        Ok(value) => value,
        Err(error) => return Ok(Err(error)),
    };
    if is_undefined(&value) {
        Ok(Ok(None))
    } else if vm.is_callable(&value) {
        Ok(Ok(Some(value)))
    } else {
        Ok(Err(
            vm.type_error(roots, "Stream callback must be callable")?
        ))
    }
}

fn construct_stream(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    target: &Value,
) -> Result<Invocation, VmError> {
    let roots = &roots.with(&args);
    let roots = &roots.with(target);
    let source = arg(&args, 0);
    if !matches!(
        source.view(),
        ValueRef::Null | ValueRef::Undefined | ValueRef::Object(_)
    ) {
        return vm.throw_type_error(roots, "Underlying source must be an object");
    }

    // The underlying source dictionary converts first in IDL member order,
    // then the queuing strategy dictionary.
    let strategy = arg(&args, 1);
    let start = converted!(callback_member(vm, host, roots, source, "start"));
    let roots = &roots.with(&start);
    let pull = converted!(callback_member(vm, host, roots, source, "pull"));
    let roots = &roots.with(&pull);
    let cancel = converted!(callback_member(vm, host, roots, source, "cancel"));
    let roots = &roots.with(&cancel);
    let kind = converted!(member(vm, host, roots, source, "type"));
    let byte = !is_undefined(&kind);
    if byte {
        let kind = converted!(vm.coerce_string(host, &roots.with(&kind), &kind));
        if kind != "bytes" {
            return vm.throw_type_error(roots, "Invalid readable stream type");
        }
    }
    let allocation = converted!(member(vm, host, roots, source, "autoAllocateChunkSize"));
    let allocation = if is_undefined(&allocation) {
        None
    } else {
        Some(converted!(enforce_unsigned(vm, host, roots, &allocation)))
    };
    let mark = converted!(member(vm, host, roots, strategy, "highWaterMark"));
    let mark = if is_undefined(&mark) {
        None
    } else {
        Some(converted!(vm.coerce_number(
            host,
            &roots.with(&mark),
            &mark
        )))
    };
    let size = converted!(callback_member(vm, host, roots, strategy, "size"));
    let roots = &roots.with(&size);
    let prototype = converted!(new_target_prototype(vm, host, roots, target, STREAM_PROTO));
    let roots = &roots.with(&prototype);

    if byte && size.is_some() {
        return vm.throw_range_error(roots, "A byte stream cannot use a size algorithm");
    }
    let high_water_mark = mark.unwrap_or(if byte { 0.0 } else { 1.0 });
    if high_water_mark.is_nan() || high_water_mark < 0.0 {
        return vm.throw_range_error(roots, "Invalid readable stream highWaterMark");
    }
    if byte && allocation == Some(0) {
        return vm.throw_type_error(roots, "autoAllocateChunkSize must be positive");
    }
    let stream = vm.allocate_readable(
        prototype,
        source.clone(),
        high_water_mark,
        size.clone(),
        byte,
        allocation,
    )?;
    let controller = vm.readable(stream)?.controller;
    let record = vm.readable_mut(stream)?;
    record.pull = pull.clone();
    record.cancel = cancel.clone();
    let roots = &roots.with(&stream);
    let start_result = match &start {
        Some(start) => match vm.invoke(
            host,
            roots,
            start,
            Arguments::Values(&[Value::object(controller)]),
            source,
        )? {
            Invocation::Value(value) => value,
            thrown => return Ok(thrown),
        },
        None => Value::UNDEFINED,
    };
    let wait = vm.stream_callback_promise(host, roots, start_result)?;
    vm.readable_mut(stream)?.start_wait = Some(wait);
    vm.observe_stream_job(wait, StreamJob::ReadableStart(stream))?;
    Ok(promise_or_object(stream))
}

fn promise_or_object(id: ObjectId) -> Invocation {
    Invocation::Value(Value::object(id))
}

pub(super) fn stream_id(vm: &VmCore, value: &Value) -> Result<Option<ObjectId>, VmError> {
    let ValueRef::Object(id) = value.view() else {
        return Ok(None);
    };
    Ok(matches!(vm.heap.get(id)?, Object::ReadableStream(_)).then_some(id))
}

fn reader_id(vm: &VmCore, value: &Value) -> Result<Option<ObjectId>, VmError> {
    reader_of_kind(vm, value, false)
}

fn reader_of_kind(vm: &VmCore, value: &Value, byob: bool) -> Result<Option<ObjectId>, VmError> {
    let ValueRef::Object(id) = value.view() else {
        return Ok(None);
    };
    Ok(matches!(vm.heap.get(id)?, Object::ReadableReader(r) if r.byob == byob).then_some(id))
}

fn controller_stream(vm: &VmCore, value: &Value) -> Result<Option<ObjectId>, VmError> {
    controller_of_kind(vm, value, false)
}

fn controller_of_kind(vm: &VmCore, value: &Value, byte: bool) -> Result<Option<ObjectId>, VmError> {
    let ValueRef::Object(id) = value.view() else {
        return Ok(None);
    };
    let Object::ReadableController { stream, .. } = vm.heap.get(id)? else {
        return Ok(None);
    };
    let stream = *stream;
    Ok((vm.readable(stream)?.byte.is_some() == byte).then_some(stream))
}

fn locked(
    vm: &mut VmCore,
    _: &Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = stream_id(vm, receiver)? else {
        return vm.throw_type_error(roots, "Expected a ReadableStream");
    };
    Ok(Invocation::Value(Value::boolean(
        vm.readable(stream)?.reader.is_some(),
    )))
}

fn get_reader(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = stream_id(vm, receiver)? else {
        return vm.throw_type_error(roots, "Expected a ReadableStream");
    };
    let live = (stream, &args);
    let roots = &roots.with(&live);
    let mode = converted!(member(vm, host, roots, arg(&args, 0), "mode"));
    if !is_undefined(&mode) {
        let mode = converted!(vm.coerce_string(host, &roots.with(&mode), &mode));
        if mode != "byob" {
            return vm.throw_type_error(roots, "Invalid readable stream reader mode");
        }
        return vm.acquire_reader(roots, stream, vm.builtins.get(BYOB_READER_PROTO), true);
    }
    vm.acquire_readable_reader(roots, stream, vm.builtins.get(READER_PROTO))
}

fn construct_reader(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    target: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = stream_id(vm, arg(&args, 0))? else {
        return vm.throw_type_error(roots, "Expected a ReadableStream");
    };
    let live = (stream, target);
    let roots = &roots.with(&live);
    let prototype = converted!(new_target_prototype(vm, host, roots, target, READER_PROTO));
    vm.acquire_readable_reader(roots, stream, prototype)
}

fn reader_closed(
    vm: &mut VmCore,
    _: &Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    let Some(reader) = reader_id(vm, receiver)? else {
        return vm.throw_type_error(roots, "Expected a ReadableStreamDefaultReader");
    };
    Ok(promise_or_object(vm.readable_reader(reader)?.closed))
}

fn stream_cancel(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = stream_id(vm, receiver)? else {
        return vm.reject_readable_type(roots, "Expected a ReadableStream");
    };
    if vm.readable(stream)?.reader.is_some() {
        return vm.reject_readable_type(roots, "Cannot cancel a locked ReadableStream");
    }
    vm.cancel_readable(host, roots, stream, arg(&args, 0))
}

fn reader_cancel(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    let Some(reader) = reader_id(vm, receiver)? else {
        return vm.reject_readable_type(roots, "Expected a ReadableStreamDefaultReader");
    };
    let Some(stream) = vm.readable_reader(reader)?.stream else {
        return vm.reject_readable_type(roots, "Reader has no stream");
    };
    vm.cancel_readable(host, roots, stream, arg(&args, 0))
}

fn reader_read(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    let Some(reader) = reader_id(vm, receiver)? else {
        return vm.reject_readable_type(roots, "Expected a ReadableStreamDefaultReader");
    };
    vm.read_readable(host, roots, reader)
}

fn release_lock(
    vm: &mut VmCore,
    _: &Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    let Some(reader) = reader_id(vm, receiver)? else {
        return vm.throw_type_error(roots, "Expected a ReadableStreamDefaultReader");
    };
    vm.release_readable_reader(roots, reader)?;
    Ok(Invocation::Value(Value::UNDEFINED))
}

fn desired_size(
    vm: &mut VmCore,
    _: &Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = controller_stream(vm, receiver)? else {
        return vm.throw_type_error(roots, "Expected a ReadableStreamDefaultController");
    };
    let stream = vm.readable(stream)?;
    Ok(Invocation::Value(match stream.state {
        ReadableState::Errored(_) => Value::NULL,
        ReadableState::Closed => Value::number(0.0),
        ReadableState::Readable => Value::number(stream.high_water_mark - stream.total_size),
    }))
}

fn controller_close(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = controller_stream(vm, receiver)? else {
        return vm.throw_type_error(roots, "Expected a ReadableStreamDefaultController");
    };
    if !can_close_or_enqueue(vm.readable(stream)?) {
        return vm.throw_type_error(roots, "ReadableStream cannot be closed");
    }
    vm.request_readable_close(host, roots, stream)?;
    Ok(Invocation::Value(Value::UNDEFINED))
}

fn controller_enqueue(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = controller_stream(vm, receiver)? else {
        return vm.throw_type_error(roots, "Expected a ReadableStreamDefaultController");
    };
    if !can_close_or_enqueue(vm.readable(stream)?) {
        return vm.throw_type_error(roots, "ReadableStream cannot accept chunks");
    }
    vm.enqueue_readable(host, roots, stream, arg(&args, 0))
}

fn controller_error(
    vm: &mut VmCore,
    _: &Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = controller_stream(vm, receiver)? else {
        return vm.throw_type_error(roots, "Expected a ReadableStreamDefaultController");
    };
    vm.error_readable(stream, arg(&args, 0))?;
    Ok(Invocation::Value(Value::UNDEFINED))
}

pub(super) fn can_close_or_enqueue(stream: &ReadableStreamObject) -> bool {
    !stream.close_requested && matches!(stream.state, ReadableState::Readable)
}

fn clear_algorithms(stream: &mut ReadableStreamObject) {
    stream.source = Value::UNDEFINED;
    stream.pull = None;
    stream.cancel = None;
    stream.size = None;
}

fn reset_queue(stream: &mut ReadableStreamObject) {
    stream.queue.clear();
    stream.total_size = 0.0;
}

impl VmCore {
    fn allocate_readable(
        &mut self,
        prototype: ObjectId,
        source: Value,
        high_water_mark: f64,
        size: Option<Value>,
        byte: bool,
        allocation: Option<usize>,
    ) -> Result<ObjectId, VmError> {
        let controller_proto = self.builtins.get(if byte {
            BYTE_CONTROLLER_PROTO
        } else {
            CONTROLLER_PROTO
        });
        let controller = self.heap.alloc_plain_with(Some(controller_proto));
        let stream = self
            .heap
            .alloc(Object::ReadableStream(ReadableStreamObject {
                object: PlainObject::new(Some(prototype)),
                controller,
                reader: None,
                state: ReadableState::Readable,
                disturbed: false,
                queue: VecDeque::new(),
                byte: byte.then(|| crate::heap::ReadableByteState {
                    queue: VecDeque::new(),
                    pending: VecDeque::new(),
                    byob_request: None,
                    auto_allocate: allocation,
                }),
                total_size: 0.0,
                high_water_mark,
                size,
                source,
                pull: None,
                cancel: None,
                started: false,
                pulling: false,
                pull_again: false,
                close_requested: false,
                start_wait: None,
                pull_wait: None,
            }));
        *self.heap.get_mut(controller)? = Object::ReadableController {
            object: PlainObject::new(Some(controller_proto)),
            stream,
        };
        Ok(stream)
    }

    pub(crate) fn readable_from_bytes(&mut self, bytes: Vec<u8>) -> Result<ObjectId, VmError> {
        use crate::builtin::binary::ARRAY_BUFFER_PROTOTYPE;
        use crate::heap::{ArrayBufferObject, ByteStreamRange};

        let stream = self.allocate_readable(
            self.builtins.get(STREAM_PROTO),
            Value::UNDEFINED,
            0.0,
            None,
            true,
            None,
        )?;
        let length = bytes.len();
        let buffer = (length != 0).then(|| {
            self.heap.alloc(Object::ArrayBuffer(ArrayBufferObject {
                object: PlainObject::new(Some(self.builtins.get(ARRAY_BUFFER_PROTOTYPE))),
                bytes: Some(bytes),
                max_byte_length: None,
            }))
        });
        let record = self.readable_mut(stream)?;
        record.started = true;
        if let Some(buffer) = buffer {
            record.close_requested = true;
            record.total_size = length as f64;
            record
                .byte
                .as_mut()
                .expect("byte stream")
                .queue
                .push_back(ByteStreamRange {
                    buffer,
                    offset: 0,
                    length,
                });
        } else {
            record.state = ReadableState::Closed;
        }
        Ok(stream)
    }

    pub(super) fn create_readable_internal(
        &mut self,
        source: Value,
        hwm: f64,
        size: Option<Value>,
        byte: bool,
        start: ObjectId,
    ) -> Result<ObjectId, VmError> {
        let stream = self.allocate_readable(
            self.builtins.get(STREAM_PROTO),
            source,
            hwm,
            size,
            byte,
            None,
        )?;
        self.readable_mut(stream)?.start_wait = Some(start);
        self.observe_stream_job(start, StreamJob::ReadableStart(stream))?;
        Ok(stream)
    }

    pub(super) fn readable(&self, id: ObjectId) -> Result<&ReadableStreamObject, VmError> {
        match self.heap.get(id)? {
            Object::ReadableStream(stream) => Ok(stream),
            _ => Err(VmError::InvalidState(
                "Expected readable stream record".into(),
            )),
        }
    }

    pub(super) fn readable_mut(
        &mut self,
        id: ObjectId,
    ) -> Result<&mut ReadableStreamObject, VmError> {
        match self.heap.get_mut(id)? {
            Object::ReadableStream(stream) => Ok(stream),
            _ => Err(VmError::InvalidState(
                "Expected readable stream record".into(),
            )),
        }
    }

    pub(super) fn readable_reader(&self, id: ObjectId) -> Result<&ReadableReaderObject, VmError> {
        match self.heap.get(id)? {
            Object::ReadableReader(reader) => Ok(reader),
            _ => Err(VmError::InvalidState(
                "Expected readable reader record".into(),
            )),
        }
    }

    pub(super) fn readable_reader_mut(
        &mut self,
        id: ObjectId,
    ) -> Result<&mut ReadableReaderObject, VmError> {
        match self.heap.get_mut(id)? {
            Object::ReadableReader(reader) => Ok(reader),
            _ => Err(VmError::InvalidState(
                "Expected readable reader record".into(),
            )),
        }
    }

    pub(super) fn readable_settled_promise(
        &mut self,
        settlement: Settlement,
    ) -> Result<ObjectId, VmError> {
        let promise = self.heap.alloc_promise();
        self.settle(promise, settlement)?;
        Ok(promise)
    }

    fn reject_readable_type(
        &mut self,
        roots: &ActiveRoots<'_>,
        message: &str,
    ) -> Result<Invocation, VmError> {
        let error = self.type_error(roots, message)?;
        Ok(promise_or_object(
            self.readable_settled_promise(Settlement::Rejected(error))?,
        ))
    }

    pub(super) fn settle_readable(
        &mut self,
        promise: ObjectId,
        settlement: Settlement,
    ) -> Result<(), VmError> {
        self.heap.promise_mut(promise)?.stream_dependency = None;
        self.settle(promise, settlement)
    }

    pub(crate) fn acquire_readable_reader(
        &mut self,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
        prototype: ObjectId,
    ) -> Result<Invocation, VmError> {
        self.acquire_reader(roots, stream, prototype, false)
    }

    pub(super) fn acquire_reader(
        &mut self,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
        prototype: ObjectId,
        byob: bool,
    ) -> Result<Invocation, VmError> {
        let record = self.readable(stream)?;
        if record.reader.is_some() {
            return self.throw_type_error(roots, "ReadableStream is already locked");
        }
        if byob && record.byte.is_none() {
            return self.throw_type_error(roots, "A BYOB reader requires a readable byte stream");
        }
        let state = record.state.clone();
        let closed = self.heap.alloc_promise();
        let reader = self
            .heap
            .alloc(Object::ReadableReader(ReadableReaderObject {
                object: PlainObject::new(Some(prototype)),
                stream: Some(stream),
                closed,
                byob,
                internal: false,
                iterator: None,
                requests: VecDeque::new(),
            }));
        self.readable_mut(stream)?.reader = Some(reader);
        match state {
            ReadableState::Readable => {
                self.heap.promise_mut(closed)?.stream_dependency = Some(reader)
            }
            ReadableState::Closed => {
                self.settle(closed, Settlement::Fulfilled(Value::UNDEFINED))?
            }
            ReadableState::Errored(error) => {
                self.mark_promise_handled(closed)?;
                self.settle(closed, Settlement::Rejected(error))?;
            }
        }
        Ok(promise_or_object(reader))
    }

    pub(crate) fn release_readable_reader(
        &mut self,
        roots: &ActiveRoots<'_>,
        reader: ObjectId,
    ) -> Result<(), VmError> {
        let record = self.readable_reader(reader)?;
        let Some(stream) = record.stream else {
            return Ok(());
        };
        let closed = record.closed;
        let error = self.type_error(roots, "Reader was released")?;
        if matches!(self.readable(stream)?.state, ReadableState::Readable) {
            self.mark_promise_handled(closed)?;
            self.settle_readable(closed, Settlement::Rejected(error))?;
        } else {
            let closed = self.heap.alloc_promise();
            self.mark_promise_handled(closed)?;
            self.settle(closed, Settlement::Rejected(error))?;
            self.readable_reader_mut(reader)?.closed = closed;
        }
        self.release_byte_reader(stream)?;
        self.readable_mut(stream)?.reader = None;
        let record = self.readable_reader_mut(reader)?;
        record.stream = None;
        let requests = std::mem::take(&mut record.requests);
        if !requests.is_empty() {
            let error = self.type_error(roots, "Pending read lost its reader lock")?;
            for request in requests {
                self.settle_readable(request, Settlement::Rejected(error.clone()))?;
            }
        }
        Ok(())
    }

    pub(crate) fn read_readable(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        reader: ObjectId,
    ) -> Result<Invocation, VmError> {
        let Some(stream) = self.readable_reader(reader)?.stream else {
            return self.reject_readable_type(roots, "Reader has no stream");
        };
        if self.readable(stream)?.byte.is_some() {
            return self.read_byte_default(host, roots, stream, reader);
        }
        let request = self.heap.alloc_promise();
        let live = (reader, stream, request);
        self.heap.promise_mut(request)?.stream_dependency = Some(reader);
        let roots = &roots.with(&live);
        let record = self.readable_mut(stream)?;
        record.disturbed = true;
        match &record.state {
            ReadableState::Closed => {
                self.finish_read(host, roots, request, Value::UNDEFINED, true)?
            }
            ReadableState::Errored(error) => {
                let error = error.clone();
                self.settle(request, Settlement::Rejected(error))?;
            }
            ReadableState::Readable => {
                if let Some((chunk, size)) = record.queue.pop_front() {
                    record.total_size = if record.queue.is_empty() {
                        0.0
                    } else {
                        (record.total_size - size).max(0.0)
                    };
                    let roots = &roots.with(&chunk);
                    if record.close_requested && record.queue.is_empty() {
                        clear_algorithms(record);
                        self.close_readable(host, roots, stream)?;
                    } else {
                        self.pull_readable_if_needed(host, roots, stream)?;
                    }
                    self.finish_read(host, roots, request, chunk.clone(), false)?;
                } else {
                    self.heap.promise_mut(request)?.stream_dependency = Some(reader);
                    self.readable_reader_mut(reader)?
                        .requests
                        .push_back(request);
                    self.pull_readable_if_needed(host, roots, stream)?;
                }
            }
        }
        Ok(promise_or_object(request))
    }

    pub(super) fn request_readable_close(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
    ) -> Result<(), VmError> {
        let record = self.readable_mut(stream)?;
        if can_close_or_enqueue(record) {
            record.close_requested = true;
            if record.queue.is_empty() {
                clear_algorithms(record);
                self.close_readable(host, roots, stream)?;
            }
        }
        Ok(())
    }

    fn close_readable(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
    ) -> Result<(), VmError> {
        let record = self.readable_mut(stream)?;
        record.state = ReadableState::Closed;
        if let Some(reader) = record.reader {
            let record = self.readable_reader_mut(reader)?;
            let closed = record.closed;
            let requests = if record.byob {
                VecDeque::new()
            } else {
                std::mem::take(&mut record.requests)
            };
            let drained = !requests.is_empty();
            self.settle_readable(closed, Settlement::Fulfilled(Value::UNDEFINED))?;
            let views = requests.as_slices();
            let live = (stream, reader, views);
            let roots = &roots.with(&live);
            for &request in &requests {
                self.finish_read(host, roots, request, Value::UNDEFINED, true)?;
            }
            if drained && self.readable_reader(reader)?.iterator.is_some() {
                self.release_finished_stream_iterator(roots, reader)?;
            }
        }
        Ok(())
    }

    fn finish_read(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        request: ObjectId,
        chunk: Value,
        done: bool,
    ) -> Result<(), VmError> {
        let internal = self.heap.promise(request)?.stream_dependency.is_some_and(
            |reader| matches!(self.heap.get(reader), Ok(Object::ReadableReader(r)) if r.internal),
        );
        let result = self.iterator_result_value(chunk, done)?;
        if internal {
            self.settle(request, Settlement::Fulfilled(result))
        } else {
            self.heap.promise_mut(request)?.stream_dependency = None;
            self.resolve_promise(host, &roots.with(&request), request, result)
        }
    }

    pub(super) fn error_readable(
        &mut self,
        stream: ObjectId,
        error: &Value,
    ) -> Result<(), VmError> {
        let record = self.readable_mut(stream)?;
        if !matches!(record.state, ReadableState::Readable) {
            return Ok(());
        }
        self.clear_byte_pending(stream)?;
        let record = self.readable_mut(stream)?;
        if let Some(byte) = &mut record.byte {
            byte.queue.clear();
        }
        reset_queue(record);
        clear_algorithms(record);
        record.state = ReadableState::Errored(error.clone());
        if let Some(reader) = record.reader {
            let record = self.readable_reader_mut(reader)?;
            let closed = record.closed;
            let requests = std::mem::take(&mut record.requests);
            let drained = !requests.is_empty();
            self.mark_promise_handled(closed)?;
            self.settle_readable(closed, Settlement::Rejected(error.clone()))?;
            for request in requests {
                self.settle_readable(request, Settlement::Rejected(error.clone()))?;
            }
            if drained && self.readable_reader(reader)?.iterator.is_some() {
                self.release_finished_stream_iterator(&ActiveRoots::EMPTY, reader)?;
            }
        }
        Ok(())
    }

    pub(super) fn enqueue_readable(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
        chunk: &Value,
    ) -> Result<Invocation, VmError> {
        let record = self.readable(stream)?;
        if !can_close_or_enqueue(record) {
            return Ok(Invocation::Value(Value::UNDEFINED));
        }
        let reader = record.reader;
        let live = (stream, chunk);
        let roots = &roots.with(&live);
        let request = match reader {
            Some(reader) => self.readable_reader_mut(reader)?.requests.pop_front(),
            None => None,
        };
        if let Some(request) = request {
            self.finish_read(host, roots, request, chunk.clone(), false)?;
        } else {
            let size = if let Some(size) = self.readable(stream)?.size.clone() {
                match self.invoke(
                    host,
                    roots,
                    &size,
                    Arguments::Values(std::slice::from_ref(chunk)),
                    &Value::UNDEFINED,
                )? {
                    Invocation::Value(value) => {
                        self.coerce_number(host, &roots.with(&value), &value)?
                    }
                    Invocation::Thrown(error) => Err(error),
                }
            } else {
                Ok(1.0)
            };
            let size = match size {
                Ok(size) if size.is_finite() && size >= 0.0 => size,
                invalid => {
                    let error = match invalid {
                        Err(error) => error,
                        Ok(_) => {
                            self.make_range_error(roots, "Invalid readable stream chunk size")?
                        }
                    };
                    self.error_readable(stream, &error)?;
                    return Ok(Invocation::Thrown(error));
                }
            };
            // The standard appends after size conversion, even if the size
            // callback reentered the controller or reader in the meantime.
            let record = self.readable_mut(stream)?;
            record.queue.push_back((chunk.clone(), size));
            record.total_size += size;
        }
        self.pull_readable_if_needed(host, roots, stream)?;
        Ok(Invocation::Value(Value::UNDEFINED))
    }

    pub(super) fn cancel_readable(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
        reason: &Value,
    ) -> Result<Invocation, VmError> {
        let record = self.readable_mut(stream)?;
        record.disturbed = true;
        let settlement = match &record.state {
            ReadableState::Closed => Some(Settlement::Fulfilled(Value::UNDEFINED)),
            ReadableState::Errored(error) => Some(Settlement::Rejected(error.clone())),
            ReadableState::Readable => None,
        };
        if let Some(settlement) = settlement {
            return Ok(promise_or_object(
                self.readable_settled_promise(settlement)?,
            ));
        }

        let live = (stream, reason);
        let roots = &roots.with(&live);
        self.close_readable(host, roots, stream)?;
        self.clear_byte_pending(stream)?;
        let requests = match self.readable(stream)?.reader {
            Some(reader) => std::mem::take(&mut self.readable_reader_mut(reader)?.requests),
            None => VecDeque::new(),
        };
        let slices = requests.as_slices();
        let roots = &roots.with(&slices);
        let record = self.readable_mut(stream)?;
        reset_queue(record);
        if let Some(byte) = &mut record.byte {
            byte.queue.clear();
        }
        let callback = record.cancel.take();
        let source = std::mem::replace(&mut record.source, Value::UNDEFINED);
        clear_algorithms(record);
        let callback_roots = (callback.as_ref(), &source);
        let roots = &roots.with(&callback_roots);
        for request in requests.iter().copied() {
            self.finish_read(host, roots, request, Value::UNDEFINED, true)?;
        }
        let native = match source.view() {
            ValueRef::Object(id) => match self.heap.get(id)? {
                Object::Tee(_) => Some(self.tee_cancel(host, roots, id, stream, reason)?),
                Object::StreamTransfer(_) => Some(self.transfer_cancel(host, roots, id, reason)?),
                Object::TransformStream(_) => {
                    Some(self.invoke_transform_cancel(host, roots, id, reason, false)?)
                }
                Object::StreamFromIterator(_) => {
                    Some(self.cancel_from_iterator(host, roots, id, reason)?)
                }
                _ => None,
            },
            _ => None,
        };
        if let Some(wait) = native {
            let result = self.heap.alloc_promise();
            self.heap.promise_mut(result)?.stream_dependency = Some(wait);
            self.observe_stream_job(wait, StreamJob::ReadableCancel(result))?;
            return Ok(promise_or_object(result));
        }
        let wait = self.invoke_readable_callback(host, roots, callback.clone(), &source, reason)?;
        let result = self.heap.alloc_promise();
        self.heap.promise_mut(result)?.stream_dependency = Some(wait);
        self.observe_stream_job(wait, StreamJob::ReadableCancel(result))?;
        Ok(promise_or_object(result))
    }

    fn invoke_readable_callback(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        callback: Option<Value>,
        source: &Value,
        argument: &Value,
    ) -> Result<ObjectId, VmError> {
        let Some(callback) = callback else {
            return self.readable_settled_promise(Settlement::Fulfilled(Value::UNDEFINED));
        };
        match self.invoke(
            host,
            roots,
            &callback,
            Arguments::Values(std::slice::from_ref(argument)),
            source,
        )? {
            Invocation::Value(value) => self.stream_callback_promise(host, roots, value),
            Invocation::Thrown(error) => self.readable_settled_promise(Settlement::Rejected(error)),
        }
    }

    pub(super) fn pull_readable_if_needed(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
    ) -> Result<(), VmError> {
        let record = self.readable(stream)?;
        if !can_close_or_enqueue(record) || !record.started {
            return Ok(());
        }
        let pending = match record.reader {
            Some(reader) => !self.readable_reader(reader)?.requests.is_empty(),
            None => false,
        };
        let has_capacity = record.high_water_mark - record.total_size > 0.0;
        if !pending && !has_capacity {
            return Ok(());
        }
        let record = self.readable_mut(stream)?;
        if record.pulling {
            record.pull_again = true;
            return Ok(());
        }
        record.pulling = true;
        let callback = record.pull.clone();
        let source = record.source.clone();
        let controller = Value::object(record.controller);
        let roots = &roots.with(&stream);
        let native = match source.view() {
            ValueRef::Object(id) => match self.heap.get(id)? {
                Object::Tee(_) => Some(self.tee_pull(host, roots, id, stream)?),
                Object::StreamTransfer(_) => Some(self.transfer_pull(id)?),
                Object::TransformStream(_) => Some(self.invoke_transform_pull(id)?),
                Object::StreamFromIterator(_) => {
                    Some(self.pull_from_iterator(host, roots, id, stream)?)
                }
                _ => None,
            },
            _ => None,
        };
        let wait = match native {
            Some(wait) => wait,
            None => self.invoke_readable_callback(host, roots, callback, &source, &controller)?,
        };
        self.readable_mut(stream)?.pull_wait = Some(wait);
        self.observe_stream_job(wait, StreamJob::ReadablePull(stream))
    }

    pub(crate) fn resume_readable_job(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        job: StreamJob,
        settlement: &Settlement,
    ) -> Result<(), VmError> {
        match job {
            StreamJob::ReadableStart(stream) => {
                let record = self.readable_mut(stream)?;
                record.start_wait = None;
                match settlement {
                    Settlement::Fulfilled(_) => {
                        record.started = true;
                        self.pull_readable_if_needed(host, roots, stream)
                    }
                    Settlement::Rejected(error) => self.error_readable(stream, error),
                }
            }
            StreamJob::ReadablePull(stream) => {
                let record = self.readable_mut(stream)?;
                record.pull_wait = None;
                record.pulling = false;
                let again = std::mem::take(&mut record.pull_again);
                match settlement {
                    Settlement::Fulfilled(_) if again => {
                        self.pull_readable_if_needed(host, roots, stream)
                    }
                    Settlement::Fulfilled(_) => Ok(()),
                    Settlement::Rejected(error) => self.error_readable(stream, error),
                }
            }
            StreamJob::ReadableCancel(promise) => self.settle_readable(
                promise,
                match settlement {
                    Settlement::Fulfilled(_) => Settlement::Fulfilled(Value::UNDEFINED),
                    Settlement::Rejected(error) => Settlement::Rejected(error.clone()),
                },
            ),
            _ => unreachable!("writable stream job dispatched to readable streams"),
        }
    }
}
