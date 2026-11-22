//! DataView: unaligned, explicitly endian element access.
//!
//! A DataView over a resizable buffer tracks the buffer's length (`None`), so a
//! resize changes `byteLength` and can push a fixed view out of bounds, which
//! makes every element access a TypeError until the buffer grows back.

use super::{Completion, DATA_VIEW, DATA_VIEW_PROTOTYPE, arg, element, getter, method};
use crate::builtin::define_builtin_data;
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{
    Attributes, DataViewObject, ElementKind, Object, TO_STRING_TAG_SYMBOL, ViewRange,
};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::Invocation;
use crate::value::{ObjectId, Value, ValueRef};
use crate::vm::Vm;

const BUFFER: IntrinsicKey = IntrinsicKey::new("DataView", Slot::PrototypeGetter, "buffer");
const BYTE_LENGTH: IntrinsicKey =
    IntrinsicKey::new("DataView", Slot::PrototypeGetter, "byteLength");
const BYTE_OFFSET: IntrinsicKey =
    IntrinsicKey::new("DataView", Slot::PrototypeGetter, "byteOffset");

macro_rules! data_view_members {
    ($($get:literal => $set:literal : $kind:ident),+ $(,)?) => {
        pub(super) const INTRINSICS: &[IntrinsicDef] = &[
            IntrinsicDef::constructor_with_target(
                DATA_VIEW,
                1,
                |vm, _, roots, _, _| vm.throw_type_error(roots, "Constructor DataView requires new"),
                construct,
            ),
            IntrinsicDef::call(BUFFER, 0, |vm, _, roots, _, receiver| {
                brand(vm, roots, receiver, |view| Value::object(view.buffer))
            }),
            IntrinsicDef::call(BYTE_LENGTH, 0, |vm, _, roots, _, receiver| {
                vm.data_view_length(roots, receiver)
            }),
            IntrinsicDef::call(BYTE_OFFSET, 0, |vm, _, roots, _, receiver| {
                vm.data_view_offset(roots, receiver)
            }),
            $(
                IntrinsicDef::call(
                    IntrinsicKey::new("DataView", Slot::Prototype, $get),
                    1,
                    |vm, host, roots, args, receiver| {
                        vm.data_view_get(host, roots, receiver, &args, ElementKind::$kind)
                    },
                ),
                IntrinsicDef::call(
                    IntrinsicKey::new("DataView", Slot::Prototype, $set),
                    2,
                    |vm, host, roots, args, receiver| {
                        vm.data_view_set(host, roots, receiver, &args, ElementKind::$kind)
                    },
                ),
            )+
        ];

        /// Property installation repeats the same member list.
        fn install_elements(vm: &mut Vm) {
            let prototype = vm.builtins.get(DATA_VIEW_PROTOTYPE);
            $(
                let get = IntrinsicKey::new("DataView", Slot::Prototype, $get);
                method(vm, prototype, get.member, get);
                let set = IntrinsicKey::new("DataView", Slot::Prototype, $set);
                method(vm, prototype, set.member, set);
            )+
        }
    };
}

data_view_members! {
    "getInt8" => "setInt8": Int8,
    "getUint8" => "setUint8": Uint8,
    "getInt16" => "setInt16": Int16,
    "getUint16" => "setUint16": Uint16,
    "getInt32" => "setInt32": Int32,
    "getUint32" => "setUint32": Uint32,
    "getFloat16" => "setFloat16": Float16,
    "getFloat32" => "setFloat32": Float32,
    "getFloat64" => "setFloat64": Float64,
    "getBigInt64" => "setBigInt64": BigInt64,
    "getBigUint64" => "setBigUint64": BigUint64,
}

pub(super) fn install(vm: &mut Vm) {
    let prototype = vm.builtins.get(DATA_VIEW_PROTOTYPE);
    for key in [BUFFER, BYTE_LENGTH, BYTE_OFFSET] {
        getter(vm, prototype, key.member, key);
    }
    install_elements(vm);
    define_builtin_data(
        vm,
        prototype,
        crate::value::PropertyKey::Symbol(TO_STRING_TAG_SYMBOL),
        Value::string("DataView".into()),
        Attributes::FUNCTION_METADATA,
    );
}

/// A getter body: brand check, then read the view's own record.
fn brand(
    vm: &mut Vm,
    roots: &ActiveRoots<'_>,
    receiver: &Value,
    read: impl Fn(&DataViewObject) -> Value,
) -> Result<Invocation, VmError> {
    let id = match vm.require_data_view(roots, receiver)? {
        Ok(id) => id,
        Err(error) => return Ok(Invocation::Thrown(error)),
    };
    let view = vm.heap.data_view(id)?.expect("validated DataView receiver");
    Ok(Invocation::Value(read(view)))
}

fn construct(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    new_target: &Value,
) -> Result<Invocation, VmError> {
    let roots = &roots.with(&args);
    let buffer =
        match vm.require_array_buffer(roots, arg(&args, 0), "DataView requires an ArrayBuffer")? {
            Ok(id) => id,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
    let roots = &roots.with(&buffer);
    let offset = match vm.coerce_index(host, roots, arg(&args, 1))? {
        Ok(offset) => offset,
        Err(error) => return Ok(Invocation::Thrown(error)),
    };
    let Some(record) = vm.heap.buffer(buffer)? else {
        unreachable!("buffer was validated")
    };
    let Some(buffer_length) = record.byte_length() else {
        return vm.throw_type_error(roots, "cannot view a detached ArrayBuffer");
    };
    let resizable = record.is_resizable();
    if offset > buffer_length {
        return vm.throw_range_error(roots, "DataView byteOffset exceeds the buffer length");
    }
    let length = match arg(&args, 2).view() {
        // Only a resizable buffer lets a view track the buffer's length.
        ValueRef::Undefined if resizable => None,
        ValueRef::Undefined => Some(buffer_length - offset),
        _ => {
            let length = match vm.coerce_index(host, roots, arg(&args, 2))? {
                Ok(length) => length,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            if offset
                .checked_add(length)
                .is_none_or(|end| end > buffer_length)
            {
                return vm
                    .throw_range_error(roots, "DataView byteLength exceeds the buffer length");
            }
            Some(length)
        }
    };
    let prototype = match vm.prototype_from(host, roots, new_target, DATA_VIEW_PROTOTYPE)? {
        Ok(id) => id,
        Err(error) => return Ok(Invocation::Thrown(error)),
    };
    let view = vm.heap.alloc(Object::DataView(DataViewObject {
        object: crate::heap::PlainObject::new(Some(prototype)),
        buffer,
        byte_offset: offset,
        byte_length: length,
    }));
    Ok(Invocation::Value(Value::object(view)))
}

impl Vm {
    /// `get DataView.prototype.byteLength`: out of bounds is a TypeError.
    fn data_view_length(
        &mut self,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let id = match self.require_data_view(roots, receiver)? {
            Ok(id) => id,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        match self.data_view_size(roots, id)? {
            Ok(size) => Ok(Invocation::Value(Value::number(size as f64))),
            Err(error) => Ok(Invocation::Thrown(error)),
        }
    }

    /// `get DataView.prototype.byteOffset`: out of bounds is a TypeError too.
    fn data_view_offset(
        &mut self,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let id = match self.require_data_view(roots, receiver)? {
            Ok(id) => id,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        if let Err(error) = self.data_view_size(roots, id)? {
            return Ok(Invocation::Thrown(error));
        }
        let view = self
            .heap
            .data_view(id)?
            .expect("validated DataView receiver");
        Ok(Invocation::Value(Value::number(view.byte_offset as f64)))
    }

    /// IsViewOutOfBounds plus GetViewByteLength: the current extent, or a
    /// TypeError when the view no longer fits its buffer.
    pub(crate) fn data_view_size(
        &mut self,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
    ) -> Completion<usize> {
        let Some(view) = self.heap.data_view(id)? else {
            return Ok(Err(self.type_error(
                roots,
                "DataView method called on an incompatible receiver",
            )?));
        };
        match self.heap.data_view_range(view)? {
            ViewRange::Detached => Ok(Err(
                self.type_error(roots, "DataView is backed by a detached ArrayBuffer")?
            )),
            ViewRange::OutOfBounds => Ok(Err(
                self.type_error(roots, "DataView is out of bounds of its ArrayBuffer")?
            )),
            ViewRange::Bytes { len, .. } => Ok(Ok(len)),
        }
    }

    fn data_view_get(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        args: &[Value],
        kind: ElementKind,
    ) -> Result<Invocation, VmError> {
        let id = match self.require_data_view(roots, receiver)? {
            Ok(id) => id,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let view = (id, args);
        let roots = &roots.with(&view);
        let index = match self.coerce_index(host, roots, arg(args, 0))? {
            Ok(index) => index,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let little = arg(args, 1).truthy();
        let size = match self.data_view_size(roots, id)? {
            Ok(size) => size,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        if index.checked_add(kind.bytes()).is_none_or(|end| end > size) {
            return self.throw_range_error(roots, "DataView offset is outside the view");
        }
        let Some(view) = self.heap.data_view(id)? else {
            unreachable!("receiver was validated")
        };
        let start = view.byte_offset + index;
        let Some(bytes) = self.heap.view_bytes(view.buffer)? else {
            unreachable!("an in-bounds view has bytes")
        };
        let bits = element::load(&bytes[start..], kind, little);
        Ok(Invocation::Value(element::decode(kind, bits)))
    }

    fn data_view_set(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        args: &[Value],
        kind: ElementKind,
    ) -> Result<Invocation, VmError> {
        let id = match self.require_data_view(roots, receiver)? {
            Ok(id) => id,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let view = (id, args);
        let roots = &roots.with(&view);
        let index = match self.coerce_index(host, roots, arg(args, 0))? {
            Ok(index) => index,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let bits = match self.element_bits(host, roots, kind, arg(args, 1))? {
            Ok(bits) => bits,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let little = arg(args, 2).truthy();
        let size = match self.data_view_size(roots, id)? {
            Ok(size) => size,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        if index.checked_add(kind.bytes()).is_none_or(|end| end > size) {
            return self.throw_range_error(roots, "DataView offset is outside the view");
        }
        let Some(view) = self.heap.data_view(id)? else {
            unreachable!("receiver was validated")
        };
        let (buffer, start) = (view.buffer, view.byte_offset + index);
        let Some(Object::ArrayBuffer(target)) = Some(self.heap.get_mut(buffer)?) else {
            unreachable!("a view names an ArrayBuffer")
        };
        let Some(bytes) = target.bytes.as_mut() else {
            unreachable!("an in-bounds view has bytes")
        };
        element::store(&mut bytes[start..], kind, little, bits);
        Ok(Invocation::Value(Value::UNDEFINED))
    }
}
