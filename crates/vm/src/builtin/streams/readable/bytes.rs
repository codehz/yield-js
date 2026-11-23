use super::*;
use crate::builtin::binary::{Completion, DATA_VIEW_PROTOTYPE, FAMILIES};
use crate::heap::{
    ByteStreamRange, DataViewObject, ElementKind, PullInto, PullIntoReader, ReadableByteState,
    TypedArrayObject, ViewRange, view_range,
};

pub(super) const BYTE_CONTROLLER_PROTO: &str = "readableByteStreamController.prototype";
pub(super) const BYOB_READER_PROTO: &str = "readableStreamBYOBReader.prototype";
const BYOB_REQUEST_PROTO: &str = "readableStreamBYOBRequest.prototype";
const UINT8: Option<ElementKind> = Some(ElementKind::Uint8);

interface!(BYTE_CONTROLLER_OWNER, "readableByteStreamController", "ReadableByteStreamController", BYTE_CONTROLLER_PROTO,
    KEY => IntrinsicDef::call(KEY, 0, illegal_constructor),
    PrototypeGetter "byobRequest" 0 => byob_request,
    PrototypeGetter "desiredSize" 0 => byte_desired_size,
    Prototype "close" 0 => byte_close,
    Prototype "enqueue" 1 => byte_enqueue,
    Prototype "error" 1 => byte_error,
);
interface!(BYOB_READER_OWNER, "readableStreamBYOBReader", "ReadableStreamBYOBReader", BYOB_READER_PROTO,
    KEY => IntrinsicDef::constructor_with_target(KEY, 1, illegal_constructor, construct_byob_reader),
    PrototypeGetter "closed" 0 => byob_closed,
    Prototype "cancel" 0 => byob_cancel,
    Prototype "read" 1 => byob_read,
    Prototype "releaseLock" 0 => byob_release,
);
interface!(BYOB_REQUEST_OWNER, "readableStreamBYOBRequest", "ReadableStreamBYOBRequest", BYOB_REQUEST_PROTO,
    KEY => IntrinsicDef::call(KEY, 0, illegal_constructor),
    PrototypeGetter "view" 0 => request_view,
    Prototype "respond" 1 => respond,
    Prototype "respondWithNewView" 1 => respond_with_new_view,
);

pub(super) fn enforce_unsigned(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    value: &Value,
) -> Completion<usize> {
    let number = match vm.coerce_number(host, &roots.with(value), value)? {
        Ok(number) => number.trunc(),
        Err(error) => return Ok(Err(error)),
    };
    // [EnforceRange] unsigned long long: 2^64 is not representable, and the
    // largest double below it is the largest representable u64 value.
    if number.is_nan() || !(0.0..18_446_744_073_709_551_616.0).contains(&number) {
        return Ok(Err(
            vm.type_error(roots, "Unsigned stream argument is out of range")?
        ));
    }
    Ok(Ok(number as usize))
}

fn construct_byob_reader(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    target: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = stream_id(vm, arg(&args, 0))? else {
        return vm.throw_type_error(roots, "Expected a ReadableStream");
    };
    let live = (stream, target);
    let roots = &roots.with(&live);
    let proto = converted!(new_target_prototype(
        vm,
        host,
        roots,
        target,
        BYOB_READER_PROTO
    ));
    vm.acquire_reader(roots, stream, proto, true)
}

fn byob_closed(
    vm: &mut Vm,
    _: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    let Some(reader) = reader_of_kind(vm, receiver, true)? else {
        return vm.throw_type_error(roots, "Expected a ReadableStreamBYOBReader");
    };
    Ok(promise_or_object(vm.readable_reader(reader)?.closed))
}

fn byob_cancel(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    let Some(reader) = reader_of_kind(vm, receiver, true)? else {
        return vm.throw_type_error(roots, "Expected a ReadableStreamBYOBReader");
    };
    let Some(stream) = vm.readable_reader(reader)?.stream else {
        return vm.reject_readable_type(roots, "Reader has no stream");
    };
    vm.cancel_readable(host, roots, stream, arg(&args, 0))
}

fn byob_release(
    vm: &mut Vm,
    _: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    let Some(reader) = reader_of_kind(vm, receiver, true)? else {
        return vm.throw_type_error(roots, "Expected a ReadableStreamBYOBReader");
    };
    vm.release_readable_reader(roots, reader)?;
    Ok(Invocation::Value(Value::UNDEFINED))
}

pub(in crate::builtin::streams) fn byob_read(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    let Some(reader) = reader_of_kind(vm, receiver, true)? else {
        return vm.reject_readable_type(roots, "Expected a ReadableStreamBYOBReader");
    };
    let live = (reader, &args);
    let roots = &roots.with(&live);
    let view = arg(&args, 0);
    if !crate::builtin::binary::is_view(&vm.heap, view) {
        return vm.reject_readable_type(roots, "Expected an ArrayBufferView");
    }
    let reject = |vm: &mut Vm, error| {
        Ok(promise_or_object(
            vm.readable_settled_promise(Settlement::Rejected(error))?,
        ))
    };
    let min = match member(vm, host, roots, arg(&args, 1), "min")? {
        Ok(min) => min,
        Err(error) => return reject(vm, error),
    };
    let min = if is_undefined(&min) {
        1
    } else {
        match enforce_unsigned(vm, host, roots, &min)? {
            Ok(min) => min,
            Err(error) => return reject(vm, error),
        }
    };
    let Some((range, kind, buffer_length)) = vm.byte_view_range(view)? else {
        return vm.reject_readable_type(roots, "Invalid BYOB view");
    };
    if range.length == 0 || min == 0 {
        return vm.reject_readable_type(roots, "BYOB view length and minimum must be positive");
    }
    let element_size = kind.map_or(1, ElementKind::bytes);
    if min > range.length / element_size {
        let error = vm.make_range_error(roots, "BYOB minimum exceeds view length")?;
        return reject(vm, error);
    }
    let Some(stream) = vm.readable_reader(reader)?.stream else {
        return vm.reject_readable_type(roots, "Reader has no stream");
    };
    vm.readable_mut(stream)?.disturbed = true;
    if let ReadableState::Errored(error) = &vm.readable(stream)?.state {
        return reject(vm, error.clone());
    }
    let buffer = match vm.transfer_stream_buffer(roots, range.buffer)? {
        Ok(buffer) => buffer,
        Err(error) => return reject(vm, error),
    };
    let range = ByteStreamRange { buffer, ..range };
    let request = vm.heap.alloc_promise();
    vm.heap.promise_mut(request)?.stream_dependency = Some(reader);
    let live = (stream, request);
    let roots = &roots.with(&live);
    if matches!(vm.readable(stream)?.state, ReadableState::Closed)
        && vm.byte(stream)?.pending.is_empty()
    {
        let chunk = vm.make_byte_view(ByteStreamRange { length: 0, ..range }, kind);
        vm.finish_read(host, roots, request, chunk, true)?;
    } else {
        let had_pending = !vm.byte(stream)?.pending.is_empty();
        vm.byte_mut(stream)?.pending.push_back(PullInto {
            range,
            buffer_length,
            bytes_filled: 0,
            minimum_fill: min * element_size,
            element_size,
            view_kind: kind,
            reader_type: PullIntoReader::Byob,
            request: Some(request),
        });
        vm.add_byte_read_request(reader, request)?;
        if !had_pending {
            let ready = vm.fill_byte_reads(roots, stream)?;
            if vm.readable(stream)?.close_requested
                && !vm.byte(stream)?.pending.is_empty()
                && vm.byte(stream)?.queue.is_empty()
            {
                // A close requested with queued data cannot satisfy this new minimum.
                let error =
                    vm.type_error(roots, "Byte stream closed before reaching BYOB minimum")?;
                vm.error_readable(stream, &error)?;
            } else {
                vm.deliver_byte_reads(host, roots, stream, ready, true)?;
            }
        }
    }
    Ok(promise_or_object(request))
}

fn byte_desired_size(
    vm: &mut Vm,
    _: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = controller_of_kind(vm, receiver, true)? else {
        return vm.throw_type_error(roots, "Expected a ReadableByteStreamController");
    };
    let record = vm.readable(stream)?;
    Ok(Invocation::Value(match record.state {
        ReadableState::Readable => Value::number(record.high_water_mark - record.total_size),
        ReadableState::Closed => Value::number(0.0),
        ReadableState::Errored(_) => Value::NULL,
    }))
}

pub(in crate::builtin::streams) fn byte_close(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = controller_of_kind(vm, receiver, true)? else {
        return vm.throw_type_error(roots, "Expected a ReadableByteStreamController");
    };
    if !can_close_or_enqueue(vm.readable(stream)?) {
        return vm.throw_type_error(roots, "Byte stream cannot be closed");
    }
    if !vm.byte(stream)?.queue.is_empty() {
        vm.readable_mut(stream)?.close_requested = true;
    } else if let Err(error) = vm.close_byte_stream(host, roots, stream)? {
        return Ok(Invocation::Thrown(error));
    }
    Ok(Invocation::Value(Value::UNDEFINED))
}

fn byte_error(
    vm: &mut Vm,
    _: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = controller_of_kind(vm, receiver, true)? else {
        return vm.throw_type_error(roots, "Expected a ReadableByteStreamController");
    };
    vm.error_readable(stream, arg(&args, 0))?;
    Ok(Invocation::Value(Value::UNDEFINED))
}

pub(in crate::builtin::streams) fn byte_enqueue(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = controller_of_kind(vm, receiver, true)? else {
        return vm.throw_type_error(roots, "Expected a ReadableByteStreamController");
    };
    let Some((range, _, _)) = vm.byte_view_range(arg(&args, 0))? else {
        return vm.throw_type_error(roots, "Expected an attached, in-bounds ArrayBufferView");
    };
    if range.length == 0 || !can_close_or_enqueue(vm.readable(stream)?) {
        return vm.throw_type_error(roots, "Byte stream cannot accept this chunk");
    }
    let live = (stream, &args);
    let roots = &roots.with(&live);
    let buffer = converted!(vm.transfer_stream_buffer(roots, range.buffer));
    let roots = &roots.with(&buffer);
    if let Some(head) = vm.byte(stream)?.pending.front().copied() {
        if vm
            .heap
            .buffer(head.range.buffer)?
            .and_then(|b| b.byte_length())
            .is_none()
        {
            return vm.throw_type_error(roots, "Pending BYOB buffer is detached");
        }
        vm.invalidate_byob_request(stream)?;
        let replacement = converted!(vm.transfer_stream_buffer(roots, head.range.buffer));
        vm.byte_mut(stream)?.pending[0].range.buffer = replacement;
        converted!(vm.queue_released_head(roots, stream));
    }
    vm.push_byte_range(stream, ByteStreamRange { buffer, ..range })?;
    let ready = vm.fill_byte_reads(roots, stream)?;
    vm.deliver_byte_reads(host, roots, stream, ready, false)?;
    Ok(Invocation::Value(Value::UNDEFINED))
}

fn byob_request(
    vm: &mut Vm,
    _: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = controller_of_kind(vm, receiver, true)? else {
        return vm.throw_type_error(roots, "Expected a ReadableByteStreamController");
    };
    let byte = vm.byte(stream)?;
    if let Some(request) = byte.byob_request {
        return Ok(promise_or_object(request));
    }
    let Some(head) = byte.pending.front().copied() else {
        return Ok(Invocation::Value(Value::NULL));
    };
    let controller = vm.readable(stream)?.controller;
    let range = ByteStreamRange {
        offset: head.range.offset + head.bytes_filled,
        length: head.range.length - head.bytes_filled,
        ..head.range
    };
    let ValueRef::Object(view) = vm.make_byte_view(range, UINT8).view() else {
        unreachable!()
    };
    let request = vm.heap.alloc(Object::ReadableByobRequest {
        object: PlainObject::new(Some(vm.builtins.get(BYOB_REQUEST_PROTO))),
        controller: Some(controller),
        view: Some(view),
    });
    vm.byte_mut(stream)?.byob_request = Some(request);
    Ok(promise_or_object(request))
}

fn request_view(
    vm: &mut Vm,
    _: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    if let ValueRef::Object(id) = receiver.view()
        && let Object::ReadableByobRequest { view, .. } = vm.heap.get(id)?
    {
        return Ok(Invocation::Value(view.map_or(Value::NULL, Value::object)));
    }
    vm.throw_type_error(roots, "Expected a ReadableStreamBYOBRequest")
}

fn request_stream(vm: &Vm, receiver: &Value) -> Result<Option<ObjectId>, VmError> {
    let ValueRef::Object(id) = receiver.view() else {
        return Ok(None);
    };
    match vm.heap.get(id)? {
        Object::ReadableByobRequest {
            controller: Some(controller),
            ..
        } => controller_of_kind(vm, &Value::object(*controller), true),
        _ => Ok(None),
    }
}

fn respond(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    let ValueRef::Object(id) = receiver.view() else {
        return vm.throw_type_error(roots, "Expected a ReadableStreamBYOBRequest");
    };
    if !matches!(vm.heap.get(id)?, Object::ReadableByobRequest { .. }) {
        return vm.throw_type_error(roots, "Expected a ReadableStreamBYOBRequest");
    }
    let roots = &roots.with(&id);
    let written = converted!(enforce_unsigned(vm, host, roots, arg(&args, 0)));
    let Some(stream) = request_stream(vm, receiver)? else {
        return vm.throw_type_error(roots, "BYOB request is invalidated");
    };
    let buffer = vm.byte(stream)?.pending[0].range.buffer;
    if vm
        .heap
        .buffer(buffer)?
        .and_then(|b| b.byte_length())
        .is_none()
    {
        return vm.throw_type_error(roots, "BYOB request buffer is detached");
    }
    vm.respond_byte_request(host, roots, stream, written, None)
}

fn respond_with_new_view(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    let Some(stream) = request_stream(vm, receiver)? else {
        return vm.throw_type_error(roots, "Expected an active ReadableStreamBYOBRequest");
    };
    let live = (stream, &args);
    let roots = &roots.with(&live);
    let Some((range, _, buffer_length)) = vm.byte_view_range(arg(&args, 0))? else {
        return vm.throw_type_error(roots, "Expected an attached, in-bounds ArrayBufferView");
    };
    vm.respond_byte_request(
        host,
        roots,
        stream,
        range.length,
        Some((range, buffer_length)),
    )
}

impl Vm {
    pub(in crate::builtin::streams) fn byte(
        &self,
        stream: ObjectId,
    ) -> Result<&ReadableByteState, VmError> {
        self.readable(stream)?
            .byte
            .as_ref()
            .ok_or_else(|| VmError::InvalidState("Expected byte stream".into()))
    }

    pub(in crate::builtin::streams) fn byte_mut(
        &mut self,
        stream: ObjectId,
    ) -> Result<&mut ReadableByteState, VmError> {
        self.readable_mut(stream)?
            .byte
            .as_mut()
            .ok_or_else(|| VmError::InvalidState("Expected byte stream".into()))
    }

    pub(in crate::builtin::streams) fn byte_view_range(
        &self,
        value: &Value,
    ) -> Result<Option<(ByteStreamRange, Option<ElementKind>, usize)>, VmError> {
        let ValueRef::Object(id) = value.view() else {
            return Ok(None);
        };
        let (buffer, offset, length, kind) = match self.heap.get(id)? {
            Object::TypedArray(v) => (v.buffer, v.byte_offset, v.byte_length, Some(v.kind)),
            Object::DataView(v) => (v.buffer, v.byte_offset, v.byte_length, None),
            _ => return Ok(None),
        };
        let bytes = self.heap.buffer(buffer)?.and_then(|b| b.bytes.as_deref());
        let ViewRange::Bytes { start, len } =
            view_range(bytes, offset, length, kind.map_or(1, ElementKind::bytes))
        else {
            return Ok(None);
        };
        Ok(Some((
            ByteStreamRange {
                buffer,
                offset: start,
                length: len,
            },
            kind,
            bytes.expect("attached view").len(),
        )))
    }

    pub(in crate::builtin::streams) fn make_byte_view(
        &mut self,
        range: ByteStreamRange,
        kind: Option<ElementKind>,
    ) -> Value {
        let ByteStreamRange {
            buffer,
            offset: byte_offset,
            length,
        } = range;
        let byte_length = Some(length);
        let object = match kind {
            Some(kind) => {
                let family = FAMILIES
                    .iter()
                    .find(|family| family.kind == kind)
                    .expect("intrinsic view family");
                Object::TypedArray(TypedArrayObject {
                    object: PlainObject::new(Some(self.builtins.get(family.prototype))),
                    buffer,
                    byte_offset,
                    byte_length,
                    kind,
                })
            }
            None => Object::DataView(DataViewObject {
                object: PlainObject::new(Some(self.builtins.get(DATA_VIEW_PROTOTYPE))),
                buffer,
                byte_offset,
                byte_length,
            }),
        };
        Value::object(self.heap.alloc(object))
    }

    fn transfer_stream_buffer(
        &mut self,
        roots: &ActiveRoots<'_>,
        buffer: ObjectId,
    ) -> Completion<ObjectId> {
        let Some(length) = self.heap.buffer(buffer)?.and_then(|b| b.byte_length()) else {
            return Ok(Err(self.type_error(
                roots,
                "Cannot transfer a detached byte stream buffer",
            )?));
        };
        // TransferArrayBuffer produces a fixed-length buffer, including for a RAB input.
        self.transfer_array_buffer_block(roots, buffer, length, None)
    }

    fn invalidate_byob_request(&mut self, stream: ObjectId) -> Result<(), VmError> {
        if let Some(request) = self.byte_mut(stream)?.byob_request.take() {
            let Object::ReadableByobRequest {
                controller, view, ..
            } = self.heap.get_mut(request)?
            else {
                return Err(VmError::InvalidState("Invalid BYOB request".into()));
            };
            *controller = None;
            *view = None;
        }
        Ok(())
    }

    pub(super) fn clear_byte_pending(&mut self, stream: ObjectId) -> Result<(), VmError> {
        if self.readable(stream)?.byte.is_some() {
            self.invalidate_byob_request(stream)?;
            self.byte_mut(stream)?.pending.clear();
        }
        Ok(())
    }

    pub(super) fn release_byte_reader(&mut self, stream: ObjectId) -> Result<(), VmError> {
        if let Some(byte) = &mut self.readable_mut(stream)?.byte
            && let Some(head) = byte.pending.front_mut()
        {
            head.reader_type = PullIntoReader::None;
            head.request = None;
            byte.pending.truncate(1);
        }
        Ok(())
    }

    fn add_byte_read_request(
        &mut self,
        reader: ObjectId,
        request: ObjectId,
    ) -> Result<(), VmError> {
        self.heap.promise_mut(request)?.stream_dependency = Some(reader);
        self.readable_reader_mut(reader)?
            .requests
            .push_back(request);
        Ok(())
    }

    pub(super) fn read_byte_default(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
        reader: ObjectId,
    ) -> Result<Invocation, VmError> {
        let request = self.heap.alloc_promise();
        self.heap.promise_mut(request)?.stream_dependency = Some(reader);
        let live = (stream, reader, request);
        let roots = &roots.with(&live);
        let state = {
            let record = self.readable_mut(stream)?;
            record.disturbed = true;
            record.state.clone()
        };
        match state {
            ReadableState::Closed => {
                self.finish_read(host, roots, request, Value::UNDEFINED, true)?
            }
            ReadableState::Errored(error) => {
                self.settle(request, Settlement::Rejected(error))?;
            }
            ReadableState::Readable => {
                if self.byte(stream)?.queue.is_empty()
                    && let Some(length) = self.byte(stream)?.auto_allocate
                {
                    let buffer = match self.allocate_array_buffer(roots, length, None)? {
                        Ok(buffer) => buffer,
                        Err(error) => {
                            self.settle(request, Settlement::Rejected(error))?;
                            return Ok(promise_or_object(request));
                        }
                    };
                    self.byte_mut(stream)?.pending.push_back(PullInto {
                        range: ByteStreamRange {
                            buffer,
                            offset: 0,
                            length,
                        },
                        buffer_length: length,
                        bytes_filled: 0,
                        minimum_fill: 1,
                        element_size: 1,
                        view_kind: UINT8,
                        reader_type: PullIntoReader::Default,
                        request: Some(request),
                    });
                }
                self.add_byte_read_request(reader, request)?;
                let ready = self.fill_byte_reads(roots, stream)?;
                self.deliver_byte_reads(host, roots, stream, ready, true)?;
            }
        }
        Ok(promise_or_object(request))
    }

    pub(crate) fn create_byte_stream(&mut self) -> Result<ObjectId, VmError> {
        let stream = self.allocate_readable(
            self.builtins.get(STREAM_PROTO),
            Value::UNDEFINED,
            0.0,
            None,
            true,
            None,
        )?;
        self.readable_mut(stream)?.started = true;
        Ok(stream)
    }

    /// Enqueues an owned block without a script-visible controller.
    pub(crate) fn enqueue_byte_vec(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
        bytes: Vec<u8>,
    ) -> Result<(), VmError> {
        use crate::builtin::binary::ARRAY_BUFFER_PROTOTYPE;
        use crate::heap::ArrayBufferObject;

        if !matches!(self.readable(stream)?.state, ReadableState::Readable) {
            return Err(VmError::InvalidState(
                "cannot enqueue into a closed body stream".into(),
            ));
        }
        if bytes.is_empty() {
            return Ok(());
        }
        let length = bytes.len();
        let buffer = self.heap.alloc(Object::ArrayBuffer(ArrayBufferObject {
            object: PlainObject::new(Some(self.builtins.get(ARRAY_BUFFER_PROTOTYPE))),
            bytes: Some(bytes),
            max_byte_length: None,
        }));
        self.push_byte_range(
            stream,
            ByteStreamRange {
                buffer,
                offset: 0,
                length,
            },
        )?;
        let ready = self.fill_byte_reads(roots, stream)?;
        self.deliver_byte_reads(host, roots, stream, ready, false)
    }

    pub(crate) fn close_body_stream(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
    ) -> Result<(), VmError> {
        if !matches!(self.readable(stream)?.state, ReadableState::Readable) {
            return Ok(());
        }
        if !self.byte(stream)?.queue.is_empty() {
            self.readable_mut(stream)?.close_requested = true;
            return Ok(());
        }
        let _ = self.close_byte_stream(host, roots, stream)?;
        Ok(())
    }

    pub(crate) fn error_body_stream(
        &mut self,
        stream: ObjectId,
        error: &Value,
    ) -> Result<(), VmError> {
        self.error_readable(stream, error)
    }

    fn push_byte_range(&mut self, stream: ObjectId, range: ByteStreamRange) -> Result<(), VmError> {
        let record = self.readable_mut(stream)?;
        record.total_size += range.length as f64;
        self.byte_mut(stream)?.queue.push_back(range);
        Ok(())
    }

    fn consume_byte_range(&mut self, stream: ObjectId, length: usize) -> Result<(), VmError> {
        let byte = self.byte_mut(stream)?;
        let head = &mut byte.queue[0];
        if head.length == length {
            byte.queue.pop_front();
        } else {
            head.offset += length;
            head.length -= length;
        }
        let empty = byte.queue.is_empty();
        let record = self.readable_mut(stream)?;
        record.total_size = if empty {
            0.0
        } else {
            record.total_size - length as f64
        };
        self.fetch_bytes_consumed(stream, length as u64)?;
        Ok(())
    }

    /// Copy between two independently owned blocks without an intermediate allocation.
    fn copy_stream_bytes(
        &mut self,
        src: ByteStreamRange,
        dest: ObjectId,
        offset: usize,
    ) -> Result<(), VmError> {
        if src.buffer == dest {
            return Err(VmError::InvalidState("Aliased byte stream buffers".into()));
        }
        let Object::ArrayBuffer(buffer) = self.heap.get_mut(src.buffer)? else {
            return Err(VmError::InvalidState("Expected byte queue buffer".into()));
        };
        let bytes = buffer.bytes.take();
        let result = (|| {
            let Object::ArrayBuffer(buffer) = self.heap.get_mut(dest).ok()? else {
                return None;
            };
            let input = bytes
                .as_ref()?
                .get(src.offset..src.offset.checked_add(src.length)?)?;
            let output = buffer
                .bytes
                .as_mut()?
                .get_mut(offset..offset.checked_add(src.length)?)?;
            output.copy_from_slice(input);
            Some(())
        })();
        let Object::ArrayBuffer(buffer) = self.heap.get_mut(src.buffer)? else {
            unreachable!()
        };
        buffer.bytes = bytes;
        result.ok_or_else(|| VmError::InvalidState("Invalid byte stream copy range".into()))
    }

    pub(in crate::builtin::streams) fn clone_byte_range(
        &mut self,
        roots: &ActiveRoots<'_>,
        src: ByteStreamRange,
    ) -> Completion<ByteStreamRange> {
        let buffer = match self.allocate_array_buffer(&roots.with(&src.buffer), src.length, None)? {
            Ok(buffer) => buffer,
            Err(error) => return Ok(Err(error)),
        };
        self.copy_stream_bytes(src, buffer, 0)?;
        Ok(Ok(ByteStreamRange {
            buffer,
            offset: 0,
            length: src.length,
        }))
    }

    fn queue_released_head(&mut self, roots: &ActiveRoots<'_>, stream: ObjectId) -> Completion<()> {
        let head = self.byte(stream)?.pending[0];
        if head.reader_type != PullIntoReader::None {
            return Ok(Ok(()));
        }
        if head.bytes_filled != 0 {
            match self.clone_byte_range(
                roots,
                ByteStreamRange {
                    length: head.bytes_filled,
                    ..head.range
                },
            )? {
                Ok(range) => self.push_byte_range(stream, range)?,
                Err(error) => return Ok(Err(error)),
            }
        }
        self.byte_mut(stream)?.pending.pop_front();
        Ok(Ok(()))
    }

    /// Commit slots before resolving any promises: inherited `then` getters can reenter.
    fn take_byte_result(
        &mut self,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
    ) -> Result<(ObjectId, Value), VmError> {
        let head = self
            .byte_mut(stream)?
            .pending
            .pop_front()
            .expect("ready descriptor");
        let request = head.request.expect("descriptor has a reader");
        self.pop_byte_request(stream, request)?;
        let buffer = match self.transfer_stream_buffer(roots, head.range.buffer)? {
            Ok(buffer) => buffer,
            Err(_) => {
                return Err(VmError::InvalidState(
                    "Detached descriptor at commit".into(),
                ));
            }
        };
        let view = self.make_byte_view(
            ByteStreamRange {
                buffer,
                length: head.bytes_filled,
                ..head.range
            },
            head.view_kind,
        );
        Ok((request, view))
    }

    fn pop_byte_request(&mut self, stream: ObjectId, request: ObjectId) -> Result<(), VmError> {
        let reader = self
            .readable(stream)?
            .reader
            .expect("pending read has reader");
        if self.readable_reader_mut(reader)?.requests.pop_front() != Some(request) {
            return Err(VmError::InvalidState("Out-of-order byte read".into()));
        }
        self.heap.promise_mut(request)?.stream_dependency = None;
        Ok(())
    }

    fn fill_byte_reads(
        &mut self,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
    ) -> Result<Vec<(ObjectId, Value)>, VmError> {
        let mut ready = Vec::new();
        let Some(reader) = self.readable(stream)?.reader else {
            return Ok(ready);
        };
        if !self.readable_reader(reader)?.byob {
            while let (Some(&request), Some(&range)) = (
                self.readable_reader(reader)?.requests.front(),
                self.byte(stream)?.queue.front(),
            ) {
                let head = self.byte(stream)?.pending.front();
                if head.is_some_and(|d| d.request == Some(request)) {
                    self.invalidate_byob_request(stream)?;
                    self.byte_mut(stream)?.pending.pop_front();
                }
                self.consume_byte_range(stream, range.length)?;
                self.pop_byte_request(stream, request)?;
                ready.push((request, self.make_byte_view(range, UINT8)));
            }
            return Ok(ready);
        }
        while let Some(head) = self.byte(stream)?.pending.front().copied() {
            let queued = self.readable(stream)?.total_size as usize;
            if queued == 0 || head.reader_type == PullIntoReader::None {
                break;
            }
            let max_filled = head.bytes_filled + queued.min(head.range.length - head.bytes_filled);
            let aligned = max_filled - max_filled % head.element_size;
            let ready_now = aligned >= head.minimum_fill;
            let end = if ready_now { aligned } else { max_filled };
            let mut filled = head.bytes_filled;
            while filled < end {
                let mut range = self.byte(stream)?.queue[0];
                range.length = range.length.min(end - filled);
                self.copy_stream_bytes(range, head.range.buffer, head.range.offset + filled)?;
                self.consume_byte_range(stream, range.length)?;
                filled += range.length;
            }
            self.byte_mut(stream)?.pending[0].bytes_filled = filled;
            if ready_now {
                ready.push(self.take_byte_result(roots, stream)?);
            } else {
                break;
            }
        }
        Ok(ready)
    }

    /// Commit and close before resolving promises. Only a read path needs to
    /// call pull after delivery, because other callers pull once at the end.
    fn deliver_byte_reads(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
        ready: Vec<(ObjectId, Value)>,
        from_read: bool,
    ) -> Result<(), VmError> {
        let live = (stream, &ready);
        let roots = &roots.with(&live);
        let closing = self.readable(stream)?.close_requested && self.byte(stream)?.queue.is_empty();
        if closing {
            // Any insufficient new read is handled by byob_read before reaching here.
            let _ = self.close_byte_stream(host, roots, stream)?;
        } else if from_read {
            self.pull_readable_if_needed(host, roots, stream)?;
        }
        for (request, view) in &ready {
            self.finish_read(host, roots, *request, view.clone(), false)?;
        }
        if !from_read && !closing {
            self.pull_readable_if_needed(host, roots, stream)?;
        }
        Ok(())
    }

    fn close_byte_stream(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
    ) -> Completion<()> {
        let roots = &roots.with(&stream);
        let head = self.byte(stream)?.pending.front();
        if head.is_some_and(|d| d.bytes_filled % d.element_size != 0) {
            let error = self.type_error(roots, "Incomplete BYOB element at end of byte stream")?;
            self.error_readable(stream, &error)?;
            return Ok(Err(error));
        }
        if self
            .readable(stream)?
            .reader
            .is_some_and(|reader| self.readable_reader(reader).is_ok_and(|r| !r.byob))
        {
            for descriptor in &mut self.byte_mut(stream)?.pending {
                if descriptor.reader_type == PullIntoReader::Default {
                    descriptor.request = None;
                }
            }
        }
        clear_algorithms(self.readable_mut(stream)?);
        self.close_readable(host, roots, stream)?;
        Ok(Ok(()))
    }

    pub(in crate::builtin::streams) fn respond_byte_request(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        stream: ObjectId,
        written: usize,
        replacement: Option<(ByteStreamRange, usize)>,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&stream);
        let head = self.byte(stream)?.pending[0];
        let closed = matches!(self.readable(stream)?.state, ReadableState::Closed);
        if closed != (written == 0) {
            return self
                .throw_type_error(roots, "BYOB response length does not match stream state");
        }
        if written > head.range.length - head.bytes_filled {
            return self.throw_range_error(roots, "BYOB response exceeds the request range");
        }
        let source = if let Some((range, length)) = replacement {
            if range.offset != head.range.offset + head.bytes_filled || length != head.buffer_length
            {
                return self.throw_range_error(
                    roots,
                    "Replacement BYOB view has the wrong buffer length or offset",
                );
            }
            range.buffer
        } else {
            head.range.buffer
        };
        let buffer = converted!(self.transfer_stream_buffer(roots, source));
        self.invalidate_byob_request(stream)?;
        let descriptor = &mut self.byte_mut(stream)?.pending[0];
        descriptor.range.buffer = buffer;
        descriptor.bytes_filled += written;
        let filled = *descriptor;

        if closed {
            if filled.reader_type == PullIntoReader::None {
                self.byte_mut(stream)?.pending.pop_front();
            }
            let mut completed = Vec::new();
            while self
                .byte(stream)?
                .pending
                .front()
                .is_some_and(|d| d.reader_type == PullIntoReader::Byob)
            {
                completed.push(self.take_byte_result(roots, stream)?);
            }
            let roots = &roots.with(&completed);
            for (request, view) in &completed {
                self.finish_read(host, roots, *request, view.clone(), true)?;
            }
        } else {
            let mut ready = Vec::new();
            if filled.reader_type == PullIntoReader::None {
                converted!(self.queue_released_head(roots, stream));
            } else if filled.bytes_filled >= filled.minimum_fill {
                let remainder = filled.bytes_filled % filled.element_size;
                if remainder != 0 {
                    let range = ByteStreamRange {
                        buffer,
                        offset: filled.range.offset + filled.bytes_filled - remainder,
                        length: remainder,
                    };
                    let range = match self.clone_byte_range(roots, range)? {
                        Ok(range) => range,
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    };
                    self.push_byte_range(stream, range)?;
                    self.byte_mut(stream)?.pending[0].bytes_filled -= remainder;
                }
                ready.push(self.take_byte_result(roots, stream)?);
            }
            let ready_roots = &roots.with(&ready);
            let following = self.fill_byte_reads(ready_roots, stream)?;
            ready.extend(following);
            self.deliver_byte_reads(host, roots, stream, ready, false)?;
        }
        Ok(Invocation::Value(Value::UNDEFINED))
    }
}
