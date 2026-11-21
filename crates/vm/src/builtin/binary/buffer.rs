//! ArrayBuffer: the only owner of a byte block.
//!
//! A buffer is resizable when `max_byte_length` is `Some`, and detached when
//! `bytes` is `None`. Detaching keeps the resizable flag, so `resizable` and
//! `maxByteLength` stay meaningful after a transfer.

use super::{ARRAY_BUFFER, ARRAY_BUFFER_PROTOTYPE, Completion, arg, getter, is_view, method};
use crate::builtin::define_builtin_data;
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{Attributes, Object, TO_STRING_TAG_SYMBOL};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::Invocation;
use crate::value::{ObjectId, PropertyKeyRef, Value, ValueRef};
use crate::vm::Vm;

const SPECIES: IntrinsicKey = IntrinsicKey::new("ArrayBuffer", Slot::StaticSymbolGetter, "species");
const IS_VIEW: IntrinsicKey = IntrinsicKey::new("ArrayBuffer", Slot::Static, "isView");
const BYTE_LENGTH: IntrinsicKey =
    IntrinsicKey::new("ArrayBuffer", Slot::PrototypeGetter, "byteLength");
const MAX_LENGTH: IntrinsicKey =
    IntrinsicKey::new("ArrayBuffer", Slot::PrototypeGetter, "maxByteLength");
const RESIZABLE: IntrinsicKey =
    IntrinsicKey::new("ArrayBuffer", Slot::PrototypeGetter, "resizable");
const DETACHED: IntrinsicKey = IntrinsicKey::new("ArrayBuffer", Slot::PrototypeGetter, "detached");
const SLICE: IntrinsicKey = IntrinsicKey::new("ArrayBuffer", Slot::Prototype, "slice");
const RESIZE: IntrinsicKey = IntrinsicKey::new("ArrayBuffer", Slot::Prototype, "resize");
const TRANSFER: IntrinsicKey = IntrinsicKey::new("ArrayBuffer", Slot::Prototype, "transfer");
const TRANSFER_FIXED: IntrinsicKey =
    IntrinsicKey::new("ArrayBuffer", Slot::Prototype, "transferToFixedLength");

pub(super) const INTRINSICS: &[IntrinsicDef] = &[
    IntrinsicDef::constructor_with_target(
        ARRAY_BUFFER,
        1,
        |vm, _, _, _, _| vm.throw_type_error("Constructor ArrayBuffer requires new"),
        construct,
    ),
    IntrinsicDef::call(SPECIES, 0, |_, _, _, _, receiver| {
        Ok(Invocation::Value(receiver.clone()))
    }),
    IntrinsicDef::call(IS_VIEW, 1, |vm, _, _, args, _| {
        Ok(Invocation::Value(Value::boolean(is_view(
            &vm.heap,
            arg(&args, 0),
        ))))
    }),
    IntrinsicDef::call(BYTE_LENGTH, 0, |vm, _, _, _, receiver| {
        property(vm, receiver, |buffer| {
            Value::number(buffer.byte_length().unwrap_or(0) as f64)
        })
    }),
    IntrinsicDef::call(MAX_LENGTH, 0, |vm, _, _, _, receiver| {
        property(vm, receiver, |buffer| {
            let length = match (buffer.byte_length(), buffer.max_byte_length) {
                (None, _) | (Some(_), None) => buffer.byte_length().unwrap_or(0),
                (Some(_), Some(max)) => max,
            };
            Value::number(length as f64)
        })
    }),
    IntrinsicDef::call(RESIZABLE, 0, |vm, _, _, _, receiver| {
        property(vm, receiver, |buffer| Value::boolean(buffer.is_resizable()))
    }),
    IntrinsicDef::call(DETACHED, 0, |vm, _, _, _, receiver| {
        property(vm, receiver, |buffer| Value::boolean(buffer.is_detached()))
    }),
    IntrinsicDef::call(SLICE, 2, |vm, host, roots, args, receiver| {
        vm.array_buffer_slice(host, roots, receiver, &args)
    }),
    IntrinsicDef::call(RESIZE, 1, |vm, host, roots, args, receiver| {
        vm.array_buffer_resize(host, roots, receiver, &args)
    }),
    IntrinsicDef::call(TRANSFER, 0, |vm, host, roots, args, receiver| {
        vm.array_buffer_transfer(host, roots, receiver, &args, true)
    }),
    IntrinsicDef::call(TRANSFER_FIXED, 0, |vm, host, roots, args, receiver| {
        vm.array_buffer_transfer(host, roots, receiver, &args, false)
    }),
];

pub(super) fn install(vm: &mut Vm) {
    let constructor = vm.builtins.get(super::ARRAY_BUFFER_ROOT);
    let prototype = vm.builtins.get(ARRAY_BUFFER_PROTOTYPE);
    method(vm, constructor, SPECIES.member, SPECIES);
    method(vm, constructor, IS_VIEW.member, IS_VIEW);
    for key in [BYTE_LENGTH, MAX_LENGTH, RESIZABLE, DETACHED] {
        getter(vm, prototype, key.member, key);
    }
    for key in [SLICE, RESIZE, TRANSFER, TRANSFER_FIXED] {
        method(vm, prototype, key.member, key);
    }
    define_builtin_data(
        vm,
        prototype,
        crate::value::PropertyKey::Symbol(TO_STRING_TAG_SYMBOL),
        Value::string("ArrayBuffer".into()),
        Attributes::FUNCTION_METADATA,
    );
}

/// A getter body: branding first, then read the buffer's current state.
fn property(
    vm: &mut Vm,
    receiver: &Value,
    read: impl Fn(&crate::heap::ArrayBufferObject) -> Value,
) -> Result<Invocation, VmError> {
    let id =
        match vm.require_array_buffer(receiver, "ArrayBuffer method requires an ArrayBuffer")? {
            Ok(id) => id,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
    let buffer = vm.heap.buffer(id)?.expect("validated ArrayBuffer receiver");
    Ok(Invocation::Value(read(buffer)))
}

fn construct(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    new_target: &Value,
) -> Result<Invocation, VmError> {
    let roots = &roots.with(&args);
    let byte_length = match vm.coerce_index(host, roots, arg(&args, 0))? {
        Ok(length) => length,
        Err(error) => return Ok(Invocation::Thrown(error)),
    };
    let max = match max_byte_length_option(vm, host, roots, arg(&args, 1))? {
        Ok(max) => max,
        Err(error) => return Ok(Invocation::Thrown(error)),
    };
    let prototype = match vm.prototype_from(host, roots, new_target, ARRAY_BUFFER_PROTOTYPE)? {
        Ok(id) => id,
        Err(error) => return Ok(Invocation::Thrown(error)),
    };
    if max.is_some_and(|max| byte_length > max) {
        return vm.throw_range_error("ArrayBuffer byte length exceeds maxByteLength");
    }
    let roots = &roots.with(&prototype);
    match vm.allocate_array_buffer_with(byte_length, max, ARRAY_BUFFER_PROTOTYPE)? {
        Ok(id) => {
            // AllocateArrayBuffer honours newTarget's prototype, so restore the
            // one read before the allocation.
            let _ = roots;
            vm.heap.set_prototype(id, Some(prototype))?;
            Ok(Invocation::Value(Value::object(id)))
        }
        Err(error) => Ok(Invocation::Thrown(error)),
    }
}

/// GetArrayBufferMaxByteLengthOption: a non-object options value is ignored.
pub(super) fn max_byte_length_option(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    options: &Value,
) -> Completion<Option<usize>> {
    if !matches!(options.view(), ValueRef::Object(_)) {
        return Ok(Ok(None));
    }
    let roots = &roots.with(options);
    let value = match vm.read_property(
        host,
        roots,
        options,
        PropertyKeyRef::String("maxByteLength"),
    )? {
        Invocation::Value(value) => value,
        Invocation::Thrown(error) => return Ok(Err(error)),
    };
    if matches!(value.view(), ValueRef::Undefined) {
        return Ok(Ok(None));
    }
    let roots = &roots.with(&value);
    Ok(vm.coerce_index(host, roots, &value)?.map(Some))
}

impl Vm {
    fn array_buffer_slice(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let id = match self
            .require_array_buffer(receiver, "ArrayBuffer method requires an ArrayBuffer")?
        {
            Ok(id) => id,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let view = (id, args);
        let roots = &roots.with(&view);
        if self
            .heap
            .buffer(id)?
            .is_some_and(|buffer| buffer.is_detached())
        {
            return self.throw_type_error("cannot slice a detached ArrayBuffer");
        }
        let length = self
            .heap
            .buffer(id)?
            .and_then(|buffer| buffer.byte_length())
            .unwrap_or(0);
        let first = match self.clamped_index(host, roots, arg(args, 0), length)? {
            Ok(index) => index,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let final_index = if matches!(arg(args, 1).view(), ValueRef::Undefined) {
            length
        } else {
            match self.clamped_index(host, roots, arg(args, 1), length)? {
                Ok(index) => index,
                Err(error) => return Ok(Invocation::Thrown(error)),
            }
        };
        let new_length = final_index.saturating_sub(first);
        let constructor = match self.species_constructor(
            host,
            roots,
            &Value::object(id),
            super::ARRAY_BUFFER_ROOT,
        )? {
            Ok(constructor) => constructor,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let roots = &roots.with(&constructor);
        let created = match self.construct_with(
            host,
            roots,
            &constructor,
            vec![Value::number(new_length as f64)],
            &constructor,
        )? {
            Invocation::Value(value) => value,
            thrown @ Invocation::Thrown(_) => return Ok(thrown),
        };
        let ValueRef::Object(new) = created.view() else {
            return self.throw_type_error("ArrayBuffer slice species did not return an object");
        };
        let Some(target) = self.heap.buffer(new)? else {
            return self
                .throw_type_error("ArrayBuffer slice species did not return an ArrayBuffer");
        };
        if target.is_detached() {
            return self.throw_type_error("ArrayBuffer slice species returned a detached buffer");
        }
        if new == id {
            return self.throw_type_error("ArrayBuffer slice species returned the source buffer");
        }
        if target.byte_length().unwrap_or(0) < new_length {
            return self.throw_type_error("ArrayBuffer slice species returned a short buffer");
        }
        // Re-read the source: a species constructor may have detached it.
        let Some(source) = self.heap.buffer(id)? else {
            unreachable!("receiver was validated")
        };
        let Some(source_bytes) = source.bytes.as_deref() else {
            return self.throw_type_error("cannot slice a detached ArrayBuffer");
        };
        let count = new_length.min(length - first);
        let tail = source_bytes[first..first + count].to_vec();
        let Some(Object::ArrayBuffer(target)) = Some(self.heap.get_mut(new)?) else {
            unreachable!("target was validated")
        };
        let target_bytes = target.bytes.as_mut().expect("target is attached");
        target_bytes[..count].copy_from_slice(&tail);
        Ok(Invocation::Value(created))
    }

    fn array_buffer_resize(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let id = match self
            .require_array_buffer(receiver, "ArrayBuffer method requires an ArrayBuffer")?
        {
            Ok(id) => id,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let view = (id, args);
        let roots = &roots.with(&view);
        let Some(max) = self
            .heap
            .buffer(id)?
            .and_then(|buffer| buffer.max_byte_length)
        else {
            return self.throw_type_error("cannot resize a fixed-length ArrayBuffer");
        };
        let new_length = match self.coerce_index(host, roots, arg(args, 0))? {
            Ok(length) => length,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        if self
            .heap
            .buffer(id)?
            .is_some_and(|buffer| buffer.is_detached())
        {
            return self.throw_type_error("cannot resize a detached ArrayBuffer");
        }
        if new_length > max {
            return self.throw_range_error("ArrayBuffer resize exceeds maxByteLength");
        }
        match self.resize_block(id, new_length)? {
            Ok(()) => Ok(Invocation::Value(Value::UNDEFINED)),
            Err(error) => Ok(Invocation::Thrown(error)),
        }
    }

    /// Replace the byte block, zero-filling a grown tail.
    pub(crate) fn resize_block(&mut self, id: ObjectId, new_length: usize) -> Completion<()> {
        let mut block: Vec<u8> = Vec::new();
        if block.try_reserve_exact(new_length).is_err() {
            return Ok(Err(self.make_range_error("ArrayBuffer resize failed")?));
        }
        let Some(Object::ArrayBuffer(buffer)) = Some(self.heap.get_mut(id)?) else {
            unreachable!("caller validated an ArrayBuffer")
        };
        let bytes = buffer.bytes.as_mut().expect("caller checked attached");
        let keep = bytes.len().min(new_length);
        block.extend_from_slice(&bytes[..keep]);
        block.resize(new_length, 0);
        *bytes = block;
        Ok(Ok(()))
    }

    fn array_buffer_transfer(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        args: &[Value],
        preserve_resizability: bool,
    ) -> Result<Invocation, VmError> {
        let id = match self
            .require_array_buffer(receiver, "ArrayBuffer method requires an ArrayBuffer")?
        {
            Ok(id) => id,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let view = (id, args);
        let roots = &roots.with(&view);
        let new_length = if matches!(arg(args, 0).view(), ValueRef::Undefined) {
            self.heap
                .buffer(id)?
                .and_then(|buffer| buffer.byte_length())
        } else {
            match self.coerce_index(host, roots, arg(args, 0))? {
                Ok(length) => Some(length),
                Err(error) => return Ok(Invocation::Thrown(error)),
            }
        };
        let Some(new_length) = new_length else {
            return self.throw_type_error("cannot transfer a detached ArrayBuffer");
        };
        let Some(buffer) = self.heap.buffer(id)? else {
            unreachable!("receiver was validated")
        };
        let max = if preserve_resizability {
            buffer.max_byte_length
        } else {
            None
        };
        if buffer.is_detached() {
            return self.throw_type_error("cannot transfer a detached ArrayBuffer");
        }
        let bytes = buffer.bytes.clone().expect("attached buffer has bytes");
        if max.is_some_and(|max| new_length > max) {
            return self.throw_range_error("ArrayBuffer transfer exceeds maxByteLength");
        }
        let result = match self.allocate_array_buffer(new_length, max)? {
            Ok(id) => id,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let copied = new_length.min(bytes.len());
        let Some(Object::ArrayBuffer(target)) = Some(self.heap.get_mut(result)?) else {
            unreachable!("freshly allocated ArrayBuffer")
        };
        target.bytes.as_mut().expect("new buffer is attached")[..copied]
            .copy_from_slice(&bytes[..copied]);
        // Detach only after the copy has succeeded.
        let Some(Object::ArrayBuffer(source)) = Some(self.heap.get_mut(id)?) else {
            unreachable!("receiver was validated")
        };
        source.bytes = None;
        Ok(Invocation::Value(Value::object(result)))
    }

    /// SpeciesConstructor: the `constructor` property, then `@@species`.
    pub(crate) fn species_constructor(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        value: &Value,
        default: &'static str,
    ) -> Completion<Value> {
        let roots = &roots.with(value);
        let fallback = Value::object(self.builtins.get(default));
        let mut constructor =
            match self.read_property(host, roots, value, PropertyKeyRef::String("constructor"))? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
        if matches!(constructor.view(), ValueRef::Undefined) {
            return Ok(Ok(fallback));
        }
        if !matches!(constructor.view(), ValueRef::Object(_)) {
            return Ok(Err(
                self.type_error("constructor property must be an object")?
            ));
        }
        let roots = &roots.with(&constructor);
        constructor = match self.read_property(
            host,
            roots,
            &constructor,
            PropertyKeyRef::Symbol(crate::heap::SPECIES_SYMBOL),
        )? {
            Invocation::Value(value) => value,
            Invocation::Thrown(error) => return Ok(Err(error)),
        };
        if matches!(constructor.view(), ValueRef::Null | ValueRef::Undefined) {
            return Ok(Ok(fallback));
        }
        if !self.is_constructor(&constructor)? {
            return Ok(Err(self.type_error("species value is not a constructor")?));
        }
        Ok(Ok(constructor))
    }
}
