//! TypedArray: one constructor per element type over a shared exotic object.
//!
//! Indexed access, own keys, and property definition all live in the heap so a
//! single definition of the integer-indexed exotic methods covers every path.
//! Methods here follow the ES2026 algorithms step by step: the initial length is
//! snapshotted, and each later element access rechecks the view because a
//! callback or conversion may have shrunk or detached the buffer.

use std::cmp::Ordering;

use super::{
    Completion, FAMILIES, TYPED_ARRAY, TYPED_ARRAY_PROTOTYPE, arg, element, getter, method,
};
use crate::builtin::define_builtin_data;
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{
    ArrayIterationKind, Attributes, ITERATOR_SYMBOL, Object, TO_STRING_TAG_SYMBOL,
    TypedArrayObject, ViewRange,
};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::ops;
use crate::runtime::{Arguments, Invocation};
use crate::value::{ObjectId, PropertyKeyRef, Value, ValueRef};
use crate::vm::Vm;

use super::ElementKind;

const FROM: IntrinsicKey = IntrinsicKey::new("TypedArray", Slot::Static, "from");
const OF: IntrinsicKey = IntrinsicKey::new("TypedArray", Slot::Static, "of");
const SPECIES: IntrinsicKey = IntrinsicKey::new("TypedArray", Slot::StaticSymbolGetter, "species");
const BUFFER: IntrinsicKey = IntrinsicKey::new("TypedArray", Slot::PrototypeGetter, "buffer");
const BYTE_LENGTH: IntrinsicKey =
    IntrinsicKey::new("TypedArray", Slot::PrototypeGetter, "byteLength");
const BYTE_OFFSET: IntrinsicKey =
    IntrinsicKey::new("TypedArray", Slot::PrototypeGetter, "byteOffset");
const LENGTH: IntrinsicKey = IntrinsicKey::new("TypedArray", Slot::PrototypeGetter, "length");
const TO_STRING_TAG: IntrinsicKey =
    IntrinsicKey::new("TypedArray", Slot::PrototypeSymbolGetter, "toStringTag");
const VALUES: IntrinsicKey = IntrinsicKey::new("TypedArray", Slot::Prototype, "values");
const ENTRIES: IntrinsicKey = IntrinsicKey::new("TypedArray", Slot::Prototype, "entries");
const KEYS: IntrinsicKey = IntrinsicKey::new("TypedArray", Slot::Prototype, "keys");

macro_rules! members {
    ($($name:literal : $length:literal),+ $(,)?) => {
        pub(super) const INTRINSICS: &[IntrinsicDef] = &[
            IntrinsicDef::constructor(
                TYPED_ARRAY,
                0,
                |vm, _, roots, _, _| vm.throw_type_error(roots, "%TypedArray% is not directly callable"),
                |vm, _, roots, _, _| {
                    vm.throw_type_error(roots, "%TypedArray% is not directly constructable")
                },
            ),
            IntrinsicDef::call(FROM, 1, |vm, host, roots, args, receiver| {
                vm.typed_from(host, roots, receiver, &args)
            }),
            IntrinsicDef::call(OF, 0, |vm, host, roots, args, receiver| {
                vm.typed_of(host, roots, receiver, args)
            }),
            IntrinsicDef::call(SPECIES, 0, |_, _, _, _, receiver| {
                Ok(Invocation::Value(receiver.clone()))
            }),
            IntrinsicDef::call(BUFFER, 0, |vm, _, roots, _, receiver| {
                vm.typed_record(roots, receiver, |view| Value::object(view.buffer))
            }),
            IntrinsicDef::call(BYTE_LENGTH, 0, |vm, _, roots, _, receiver| {
                vm.typed_byte_length(roots, receiver)
            }),
            IntrinsicDef::call(BYTE_OFFSET, 0, |vm, _, roots, _, receiver| {
                vm.typed_byte_offset(roots, receiver)
            }),
            IntrinsicDef::call(LENGTH, 0, |vm, _, roots, _, receiver| vm.typed_length(roots, receiver)),
            IntrinsicDef::call(TO_STRING_TAG, 0, |vm, _, _, _, receiver| {
                Ok(Invocation::Value(match receiver.view() {
                    ValueRef::Object(id) => match vm.heap.typed_array(id)? {
                        Some(view) => Value::string(view.kind.name().into()),
                        None => Value::UNDEFINED,
                    },
                    _ => Value::UNDEFINED,
                }))
            }),
            IntrinsicDef::call(VALUES, 0, |vm, _, roots, _, receiver| {
                vm.typed_iterator(roots, receiver, ArrayIterationKind::Values)
            }),
            IntrinsicDef::call(ENTRIES, 0, |vm, _, roots, _, receiver| {
                vm.typed_iterator(roots, receiver, ArrayIterationKind::Entries)
            }),
            IntrinsicDef::call(KEYS, 0, |vm, _, roots, _, receiver| {
                vm.typed_iterator(roots, receiver, ArrayIterationKind::Keys)
            }),
            $(
                IntrinsicDef::call(
                    IntrinsicKey::new("TypedArray", Slot::Prototype, $name),
                    $length,
                    |vm, host, roots, args, receiver| {
                        vm.typed_method(host, roots, $name, receiver, &args)
                    },
                ),
            )+
            IntrinsicDef::call(
                IntrinsicKey::new("Uint8Array", Slot::Static, "fromBase64"),
                1,
                |vm, host, roots, args, _| vm.uint8_from_base64(host, roots, &args),
            ),
            IntrinsicDef::call(
                IntrinsicKey::new("Uint8Array", Slot::Static, "fromHex"),
                1,
                |vm, host, roots, args, _| vm.uint8_from_hex(host, roots, &args),
            ),
            IntrinsicDef::call(
                IntrinsicKey::new("Uint8Array", Slot::Prototype, "setFromBase64"),
                1,
                |vm, host, roots, args, receiver| {
                    vm.uint8_set_from_base64(host, roots, receiver, &args)
                },
            ),
            IntrinsicDef::call(
                IntrinsicKey::new("Uint8Array", Slot::Prototype, "setFromHex"),
                1,
                |vm, _, roots, args, receiver| vm.uint8_set_from_hex(roots, receiver, &args),
            ),
            IntrinsicDef::call(
                IntrinsicKey::new("Uint8Array", Slot::Prototype, "toBase64"),
                0,
                |vm, host, roots, args, receiver| vm.uint8_to_base64(host, roots, receiver, &args),
            ),
            IntrinsicDef::call(
                IntrinsicKey::new("Uint8Array", Slot::Prototype, "toHex"),
                0,
                |vm, _, roots, _, receiver| vm.uint8_to_hex(roots, receiver),
            ),
        ];

        /// Shared members live on `%TypedArray%.prototype` only; concrete
        /// prototypes inherit them.
        fn install_prototype(vm: &mut Vm) {
            let prototype = vm.builtins.get(TYPED_ARRAY_PROTOTYPE);
            for key in [BUFFER, BYTE_LENGTH, BYTE_OFFSET, LENGTH] {
                getter(vm, prototype, key.member, key);
            }
            getter(
                vm,
                prototype,
                crate::value::PropertyKey::Symbol(TO_STRING_TAG_SYMBOL),
                TO_STRING_TAG,
            );
            for key in [VALUES, ENTRIES, KEYS] {
                method(vm, prototype, key.member, key);
            }
            // @@iterator is the very same function object as `values`; a second
            // intrinsic with the same identity would not survive snapshotting.
            let values = vm
                .heap
                .get(prototype)
                .expect("%TypedArray%.prototype")
                .own_property(PropertyKeyRef::String("values"))
                .and_then(|property| property.data_value().cloned())
                .expect("TypedArray values method");
            define_builtin_data(
                vm,
                prototype,
                crate::value::PropertyKey::Symbol(ITERATOR_SYMBOL),
                values,
                Attributes::METHOD,
            );
            $(
                let key = IntrinsicKey::new("TypedArray", Slot::Prototype, $name);
                method(vm, prototype, key.member, key);
            )+
        }
    };
}

members! {
    "at": 1,
    "copyWithin": 2,
    "every": 1,
    "fill": 1,
    "filter": 1,
    "find": 1,
    "findIndex": 1,
    "findLast": 1,
    "findLastIndex": 1,
    "forEach": 1,
    "includes": 1,
    "indexOf": 1,
    "join": 1,
    "lastIndexOf": 1,
    "map": 1,
    "reduce": 1,
    "reduceRight": 1,
    "reverse": 0,
    "set": 1,
    "slice": 2,
    "some": 1,
    "sort": 1,
    "subarray": 2,
    "toLocaleString": 0,
    "toReversed": 0,
    "toSorted": 1,
    "with": 2,
}

pub(super) fn install(vm: &mut Vm) {
    let constructor = vm.builtins.get(super::TYPED_ARRAY_ROOT);
    let prototype = vm.builtins.get(TYPED_ARRAY_PROTOTYPE);
    method(vm, constructor, FROM.member, FROM);
    method(vm, constructor, OF.member, OF);
    method(vm, constructor, SPECIES.member, SPECIES);
    install_prototype(vm);
    // Shared `toString` is literally `%Array.prototype.toString%`.
    let to_string = vm
        .heap
        .get(vm.builtins.get("array.prototype"))
        .expect("Array prototype")
        .own_property(PropertyKeyRef::String("toString"))
        .and_then(|property| property.data_value().cloned());
    if let Some(to_string) = to_string {
        define_builtin_data(vm, prototype, "toString", to_string, Attributes::METHOD);
    }
    let uint8 = vm.builtins.get("uint8Array");
    let uint8_prototype = vm.builtins.get("uint8Array.prototype");
    // Codec members are Uint8Array-only and installed on that family alone.
    for name in ["fromBase64", "fromHex"] {
        let key = IntrinsicKey::new("Uint8Array", Slot::Static, name);
        method(vm, uint8, name, key);
    }
    for name in ["setFromBase64", "setFromHex", "toBase64", "toHex"] {
        let key = IntrinsicKey::new("Uint8Array", Slot::Prototype, name);
        method(vm, uint8_prototype, name, key);
    }
}

/// `%TypedArray%.prototype` is not callable as a constructor.
pub(super) fn call_requires_new(
    vm: &mut Vm,
    _: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    _: &Value,
) -> Result<Invocation, VmError> {
    vm.throw_type_error(roots, "Constructor TypedArray requires new")
}

/// `%TypedArray%.from`'s optional mapper and its receiver, already validated.
struct MapFn<'a> {
    enabled: bool,
    function: &'a Value,
    this_arg: &'a Value,
}

/// The 23.2.5.1 `TypedArray (...args)` dispatch for one element kind.
pub(super) fn construct(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    new_target: &Value,
    kind: ElementKind,
) -> Result<Invocation, VmError> {
    let roots = &roots.with(&args);
    if args.is_empty() {
        return invocation(vm.allocate_typed(host, roots, kind, new_target, 0, 0));
    }
    let first = arg(&args, 0).clone();
    if !matches!(first.view(), ValueRef::Object(_)) {
        let length = match vm.coerce_index(host, roots, &first)? {
            Ok(length) => length,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        return invocation(vm.allocate_typed(host, roots, kind, new_target, length, 0));
    }
    let object = match vm.allocate_typed(host, roots, kind, new_target, 0, 0)? {
        Ok(object) => object,
        Err(error) => return Ok(Invocation::Thrown(error)),
    };
    let roots = &roots.with(&object);
    let result = if vm.heap.typed_array(object_id_of(&first)?)?.is_some() {
        vm.init_from_typed_array(host, roots, object, &first, kind)
    } else if vm.heap.buffer(object_id_of(&first)?)?.is_some() {
        vm.init_from_array_buffer(host, roots, object, &first, &args)
    } else {
        vm.init_from_elements(host, roots, object, &first, kind)
    };
    match result? {
        Ok(()) => Ok(Invocation::Value(Value::object(object))),
        Err(error) => Ok(Invocation::Thrown(error)),
    }
}

fn object_id_of(value: &Value) -> Result<ObjectId, VmError> {
    match value.view() {
        ValueRef::Object(id) => Ok(id),
        _ => Err(VmError::InvalidState(
            "typed array source is not an object".to_owned(),
        )),
    }
}

fn invocation(result: Completion<ObjectId>) -> Result<Invocation, VmError> {
    Ok(match result? {
        Ok(id) => Invocation::Value(Value::object(id)),
        Err(error) => Invocation::Thrown(error),
    })
}

fn family_root(kind: ElementKind) -> &'static str {
    FAMILIES
        .iter()
        .find(|family| family.kind == kind)
        .expect("every element kind has a family")
        .root
}

fn family_prototype(kind: ElementKind) -> &'static str {
    FAMILIES
        .iter()
        .find(|family| family.kind == kind)
        .expect("every element kind has a family")
        .prototype
}

impl Vm {
    /// AllocateTypedArray plus AllocateTypedArrayBuffer in one step: the object
    /// and its buffer are always created together here, so no view ever names a
    /// buffer that does not exist.
    fn allocate_typed(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        kind: ElementKind,
        new_target: &Value,
        length: usize,
        byte_offset: usize,
    ) -> Completion<ObjectId> {
        let prototype =
            match self.prototype_from(host, roots, new_target, family_prototype(kind))? {
                Ok(prototype) => prototype,
                Err(error) => return Ok(Err(error)),
            };
        let Some(byte_length) = length.checked_mul(kind.bytes()) else {
            return Ok(Err(
                self.make_range_error(roots, "TypedArray byte length is too large")?
            ));
        };
        let buffer = match self.allocate_array_buffer(roots, byte_length, None)? {
            Ok(buffer) => buffer,
            Err(error) => return Ok(Err(error)),
        };
        Ok(Ok(self.heap.alloc(Object::TypedArray(TypedArrayObject {
            object: crate::heap::PlainObject::new(Some(prototype)),
            buffer,
            byte_offset,
            byte_length: Some(byte_length),
            kind,
        }))))
    }

    /// Attach a freshly allocated buffer of `length` elements to an existing
    /// object, which is how every non-buffer constructor overload finishes.
    fn attach_buffer(
        &mut self,
        roots: &ActiveRoots<'_>,
        object: ObjectId,
        kind: ElementKind,
        length: usize,
    ) -> Completion<()> {
        let Some(byte_length) = length.checked_mul(kind.bytes()) else {
            return Ok(Err(
                self.make_range_error(roots, "TypedArray byte length is too large")?
            ));
        };
        let buffer = match self.allocate_array_buffer(roots, byte_length, None)? {
            Ok(buffer) => buffer,
            Err(error) => return Ok(Err(error)),
        };
        let Some(Object::TypedArray(view)) = Some(self.heap.get_mut(object)?) else {
            return Err(VmError::InvalidState(
                "typed array attachment requires a TypedArray".to_owned(),
            ));
        };
        view.buffer = buffer;
        view.byte_offset = 0;
        view.byte_length = Some(byte_length);
        Ok(Ok(()))
    }

    /// InitializeTypedArrayFromTypedArray: always a fresh buffer, so the copy is
    /// never observable through the source.
    fn init_from_typed_array(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        object: ObjectId,
        source: &Value,
        kind: ElementKind,
    ) -> Completion<()> {
        let ValueRef::Object(source_id) = source.view() else {
            unreachable!("caller checked the source is an object")
        };
        let Some(source_view) = self.heap.typed_array(source_id)? else {
            unreachable!("caller checked the source is a TypedArray")
        };
        let source_kind = source_view.kind;
        if source_kind.is_bigint() != kind.is_bigint() {
            return Ok(Err(self.type_error(
                roots,
                "cannot initialize a BigInt TypedArray from a Number TypedArray",
            )?));
        }
        let source_length = match self.validate_typed_bounds(roots, source_id)? {
            Ok(length) => length,
            Err(error) => return Ok(Err(error)),
        };
        let Some(byte_length) = source_length.checked_mul(kind.bytes()) else {
            return Ok(Err(
                self.make_range_error(roots, "TypedArray byte length is too large")?
            ));
        };
        if byte_length > super::MAX_BYTE_LENGTH {
            return Ok(Err(
                self.make_range_error(roots, "TypedArray byte length is too large")?
            ));
        }
        let buffer = match self.allocate_array_buffer(roots, byte_length, None)? {
            Ok(buffer) => buffer,
            Err(error) => return Ok(Err(error)),
        };
        if source_kind == kind {
            let bytes = self.typed_bytes(source_id, 0, source_length)?;
            self.write_buffer(buffer, 0, &bytes)?;
        } else {
            for index in 0..source_length as u64 {
                let value = self.element_value(source_id, index)?;
                let bits = match self.element_bits(host, roots, kind, &value)? {
                    Ok(bits) => bits,
                    Err(error) => return Ok(Err(error)),
                };
                self.write_buffer_element(buffer, index, kind, bits)?;
            }
        }
        let Some(Object::TypedArray(view)) = Some(self.heap.get_mut(object)?) else {
            unreachable!("caller passed a TypedArray")
        };
        view.buffer = buffer;
        view.byte_offset = 0;
        view.byte_length = Some(byte_length);
        Ok(Ok(()))
    }

    /// InitializeTypedArrayFromArrayBuffer.
    fn init_from_array_buffer(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        object: ObjectId,
        buffer_value: &Value,
        args: &[Value],
    ) -> Completion<()> {
        let ValueRef::Object(buffer) = buffer_value.view() else {
            unreachable!("caller checked the source is an ArrayBuffer")
        };
        let Some(record) = self.heap.buffer(buffer)? else {
            unreachable!("caller checked the source is an ArrayBuffer")
        };
        let kind = match self.heap.typed_array(object)? {
            Some(view) => view.kind,
            None => {
                return Err(VmError::InvalidState(
                    "typed array initialization requires a TypedArray".to_owned(),
                ));
            }
        };
        let element_size = kind.bytes();
        let resizable = record.is_resizable();
        let offset = match self.coerce_index(host, roots, arg(args, 1))? {
            Ok(offset) => offset,
            Err(error) => return Ok(Err(error)),
        };
        if offset % element_size != 0 {
            return Ok(Err(self.make_range_error(
                roots,
                "TypedArray byte offset must be element aligned",
            )?));
        }
        let length_arg = arg(args, 2);
        let length = if matches!(length_arg.view(), ValueRef::Undefined) {
            None
        } else {
            match self.coerce_index(host, roots, length_arg)? {
                Ok(length) => Some(length),
                Err(error) => return Ok(Err(error)),
            }
        };
        let Some(record) = self.heap.buffer(buffer)? else {
            unreachable!("buffer was validated")
        };
        let Some(buffer_length) = record.byte_length() else {
            return Ok(Err(self.type_error(
                roots,
                "cannot create a TypedArray over a detached ArrayBuffer",
            )?));
        };
        let (byte_length, view_length) = match length {
            None if !resizable => {
                if buffer_length % element_size != 0 {
                    return Ok(Err(self.make_range_error(
                        roots,
                        "ArrayBuffer byte length is not a multiple of the element size",
                    )?));
                }
                let Some(new_byte_length) = buffer_length.checked_sub(offset) else {
                    return Ok(Err(self.make_range_error(
                        roots,
                        "TypedArray byte offset exceeds the buffer",
                    )?));
                };
                (Some(new_byte_length), Some(new_byte_length / element_size))
            }
            None => {
                if offset > buffer_length {
                    return Ok(Err(self.make_range_error(
                        roots,
                        "TypedArray byte offset exceeds the buffer",
                    )?));
                }
                (None, None)
            }
            Some(length) => {
                let Some(new_byte_length) = length.checked_mul(element_size) else {
                    return Ok(Err(
                        self.make_range_error(roots, "TypedArray byte length is too large")?
                    ));
                };
                if offset
                    .checked_add(new_byte_length)
                    .is_none_or(|end| end > buffer_length)
                {
                    return Ok(Err(
                        self.make_range_error(roots, "TypedArray exceeds the ArrayBuffer")?
                    ));
                }
                (Some(new_byte_length), Some(length))
            }
        };
        let Some(Object::TypedArray(view)) = Some(self.heap.get_mut(object)?) else {
            unreachable!("caller passed a TypedArray")
        };
        view.buffer = buffer;
        view.byte_offset = offset;
        view.byte_length = byte_length;
        let _ = view_length;
        Ok(Ok(()))
    }

    /// InitializeTypedArrayFromElements: iterable first, then array-like.
    fn init_from_elements(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        object: ObjectId,
        source: &Value,
        kind: ElementKind,
    ) -> Completion<()> {
        let view = (object, source);
        let roots = &roots.with(&view);
        let method = match self.read_property(
            host,
            roots,
            source,
            PropertyKeyRef::Symbol(ITERATOR_SYMBOL),
        )? {
            Invocation::Value(value) => value,
            Invocation::Thrown(error) => return Ok(Err(error)),
        };
        if !matches!(method.view(), ValueRef::Undefined | ValueRef::Null) {
            let list = match self.iterator_to_list(host, roots, source, &method)? {
                Ok(list) => list,
                Err(error) => return Ok(Err(error)),
            };
            let roots = &roots.with(&list);
            match self.attach_buffer(roots, object, kind, list.len())? {
                Ok(()) => {}
                Err(error) => return Ok(Err(error)),
            }
            for (index, value) in list.iter().enumerate() {
                let roots = &roots.with(value);
                match self.typed_set_element(host, roots, object, index as u64, value)? {
                    Ok(()) => {}
                    Err(error) => return Ok(Err(error)),
                }
            }
            return Ok(Ok(()));
        }
        let source_id = object_id_of(source)?;
        let length = match self.length_of_array_like(host, roots, source_id)? {
            Ok(length) => length,
            Err(error) => return Ok(Err(error)),
        };
        if length > super::MAX_BYTE_LENGTH as u64 {
            return Ok(Err(
                self.make_range_error(roots, "TypedArray length is too large")?
            ));
        }
        match self.attach_buffer(roots, object, kind, length as usize)? {
            Ok(()) => {}
            Err(error) => return Ok(Err(error)),
        }
        for index in 0..length {
            let key = index.to_string();
            let value =
                match self.read_property(host, roots, source, PropertyKeyRef::String(&key))? {
                    Invocation::Value(value) => value,
                    Invocation::Thrown(error) => return Ok(Err(error)),
                };
            let roots = &roots.with(&value);
            match self.typed_set_element(host, roots, object, index, &value)? {
                Ok(()) => {}
                Err(error) => return Ok(Err(error)),
            }
        }
        Ok(Ok(()))
    }

    /// IteratorToList over an explicit `@@iterator` method.
    pub(crate) fn iterator_to_list(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        iterable: &Value,
        method: &Value,
    ) -> Completion<Vec<Value>> {
        let roots = &roots.with(&iterable);
        let (iterator, next) = match self.get_iterator_from_method(host, roots, iterable, method)? {
            Ok(iterator) => iterator,
            Err(error) => return Ok(Err(error)),
        };
        let view = (&iterator, &next);
        let roots = &roots.with(&view);
        let mut list = Vec::new();
        loop {
            let roots = &roots.with(&list);
            match self.iterator_step(host, roots, &iterator, &next)? {
                Ok(Some(value)) => list.push(value),
                Ok(None) => return Ok(Ok(list)),
                Err(error) => {
                    // The consumer is not the one aborting, so no close.
                    return Ok(Err(error));
                }
            }
        }
    }

    /// Copy `count` elements of a validated view as raw little-endian bytes.
    fn typed_bytes(&self, id: ObjectId, start: u64, count: usize) -> Result<Vec<u8>, VmError> {
        let Some(view) = self.heap.typed_array(id)? else {
            return Ok(Vec::new());
        };
        let width = view.kind.bytes();
        let ViewRange::Bytes { start: base, len } = self.heap.typed_range(view)? else {
            return Ok(Vec::new());
        };
        let Some(offset) = usize::try_from(start)
            .ok()
            .and_then(|start| start.checked_mul(width))
        else {
            return Ok(Vec::new());
        };
        let Some(length) = count.checked_mul(width) else {
            return Ok(Vec::new());
        };
        if offset + length > len {
            return Ok(Vec::new());
        }
        let Some(bytes) = self.heap.view_bytes(view.buffer)? else {
            return Ok(Vec::new());
        };
        Ok(bytes[base + offset..base + offset + length].to_vec())
    }

    /// Raw byte write into a buffer, used by clone and copy paths.
    pub(in crate::builtin) fn write_buffer(
        &mut self,
        buffer: ObjectId,
        at: usize,
        bytes: &[u8],
    ) -> Result<(), VmError> {
        let Some(Object::ArrayBuffer(target)) = Some(self.heap.get_mut(buffer)?) else {
            return Err(VmError::InvalidState(
                "byte write requires an ArrayBuffer".to_owned(),
            ));
        };
        let Some(block) = target.bytes.as_mut() else {
            return Err(VmError::InvalidState(
                "byte write requires an attached ArrayBuffer".to_owned(),
            ));
        };
        if at + bytes.len() > block.len() {
            return Err(VmError::InvalidState(
                "byte write exceeds the ArrayBuffer".to_owned(),
            ));
        }
        block[at..at + bytes.len()].copy_from_slice(bytes);
        Ok(())
    }

    fn write_buffer_element(
        &mut self,
        buffer: ObjectId,
        index: u64,
        kind: ElementKind,
        bits: u64,
    ) -> Result<(), VmError> {
        let width = kind.bytes();
        let Some(offset) = usize::try_from(index)
            .ok()
            .and_then(|index| index.checked_mul(width))
        else {
            return Ok(());
        };
        let Some(Object::ArrayBuffer(target)) = Some(self.heap.get_mut(buffer)?) else {
            return Ok(());
        };
        let Some(block) = target.bytes.as_mut() else {
            return Ok(());
        };
        if offset + width > block.len() {
            return Ok(());
        }
        element::store(&mut block[offset..], kind, true, bits);
        Ok(())
    }

    /// TypedArrayGetElement: an invalid index reads as `undefined`.
    pub(crate) fn element_value(&self, id: ObjectId, index: u64) -> Result<Value, VmError> {
        Ok(self
            .heap
            .typed_element(id, index)?
            .unwrap_or(Value::UNDEFINED))
    }

    /// IsValidIntegerIndex for one element of a view.
    pub(crate) fn element_valid(&self, id: ObjectId, index: u64) -> Result<bool, VmError> {
        Ok(self.heap.typed_element(id, index)?.is_some())
    }

    /// TypedArrayCreateSameType: always the intrinsic constructor of the kind.
    fn same_type_create(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        length: usize,
    ) -> Completion<ObjectId> {
        let Some(kind) = self.heap.typed_array(id)?.map(|view| view.kind) else {
            return Err(VmError::InvalidState(
                "same-type creation requires a TypedArray".to_owned(),
            ));
        };
        let constructor = Value::object(self.builtins.get(family_root(kind)));
        let roots = &roots.with(&constructor);
        let args = vec![Value::number(length as f64)];
        self.typed_create_from_constructor(host, roots, &constructor, args, Some(kind))
    }

    /// TypedArraySpeciesCreate: the exemplar's species, then a brand and
    /// content-type check on whatever it built.
    fn species_create(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        args: Vec<Value>,
    ) -> Completion<ObjectId> {
        let Some(kind) = self.heap.typed_array(id)?.map(|view| view.kind) else {
            return Err(VmError::InvalidState(
                "species creation requires a TypedArray".to_owned(),
            ));
        };
        let exemplar = Value::object(id);
        let constructor =
            match self.species_constructor(host, roots, &exemplar, family_root(kind))? {
                Ok(constructor) => constructor,
                Err(error) => return Ok(Err(error)),
            };
        let roots = &roots.with(&constructor);
        self.typed_create_from_constructor(host, roots, &constructor, args, Some(kind))
    }

    /// TypedArrayCreateFromConstructor: the result must be a TypedArray of the
    /// right content type and at least the requested length.
    fn typed_create_from_constructor(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        constructor: &Value,
        args: Vec<Value>,
        expected: Option<ElementKind>,
    ) -> Completion<ObjectId> {
        let requested = match args.as_slice() {
            [Value { .. }] => match args[0].view() {
                ValueRef::Number(length) => Some(length),
                _ => None,
            },
            _ => None,
        };
        let rooted = args.clone();
        let roots = &roots.with(&rooted);
        let created = match self.construct_with(
            host,
            roots,
            constructor,
            Arguments::Values(&args),
            constructor,
        )? {
            Invocation::Value(value) => value,
            thrown @ Invocation::Thrown(_) => {
                return Ok(Err(match thrown {
                    Invocation::Thrown(error) => error,
                    Invocation::Value(_) => unreachable!(),
                }));
            }
        };
        let ValueRef::Object(id) = created.view() else {
            return Ok(Err(self.type_error(
                roots,
                "TypedArray constructor did not return an object",
            )?));
        };
        if self.heap.typed_array(id)?.is_none() {
            return Ok(Err(self.type_error(
                roots,
                "TypedArray constructor did not return a TypedArray",
            )?));
        }
        if let Some(requested) = requested {
            if self.typed_out_of_bounds(id)? {
                return Ok(Err(self.type_error(
                    roots,
                    "TypedArray constructor returned an out-of-bounds view",
                )?));
            }
            let length = match self.validate_typed_bounds(roots, id)? {
                Ok(length) => length,
                Err(error) => return Ok(Err(error)),
            };
            if length < requested as usize {
                return Ok(Err(self.type_error(
                    roots,
                    "TypedArray constructor returned a shorter view",
                )?));
            }
        }
        if let Some(expected) = expected {
            let kind = self.heap.typed_array(id)?.map(|view| view.kind);
            if kind.map(ElementKind::is_bigint) != Some(expected.is_bigint()) {
                return Ok(Err(self.type_error(
                    roots,
                    "TypedArray species has an incompatible content type",
                )?));
            }
        }
        Ok(Ok(id))
    }

    /// A shared getter body: brand check, then read the view's record.
    fn typed_record(
        &mut self,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        read: impl Fn(&TypedArrayObject) -> Value,
    ) -> Result<Invocation, VmError> {
        let (id, _) = match self.require_typed(roots, receiver)? {
            Ok(id) => id,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let view = self.heap.typed_array(id)?.expect("validated TypedArray");
        Ok(Invocation::Value(read(view)))
    }

    fn typed_length(
        &mut self,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let (id, _) = match self.require_typed(roots, receiver)? {
            Ok(id) => id,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        // An out-of-bounds or detached view reports zero, not an error.
        match self.validate_typed_bounds(roots, id)? {
            Ok(length) => Ok(Invocation::Value(Value::number(length as f64))),
            Err(_) => Ok(Invocation::Value(Value::number(0.0))),
        }
    }

    fn typed_byte_length(
        &mut self,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let (id, kind) = match self.require_typed(roots, receiver)? {
            Ok(id) => id,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let length = match self.validate_typed_bounds(roots, id)? {
            Ok(length) => length,
            Err(_) => return Ok(Invocation::Value(Value::number(0.0))),
        };
        Ok(Invocation::Value(Value::number(
            (length * kind.bytes()) as f64,
        )))
    }

    fn typed_byte_offset(
        &mut self,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let (id, _) = match self.require_typed(roots, receiver)? {
            Ok(id) => id,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let Some(view) = self.heap.typed_array(id)? else {
            unreachable!("receiver was validated")
        };
        let offset = view.byte_offset;
        if self.typed_out_of_bounds(id)? {
            return Ok(Invocation::Value(Value::number(0.0)));
        }
        Ok(Invocation::Value(Value::number(offset as f64)))
    }

    /// Shared iterator creation; the array iterator re-reads the target each step.
    fn typed_iterator(
        &mut self,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        kind: ArrayIterationKind,
    ) -> Result<Invocation, VmError> {
        let (id, _) = match self.require_typed(roots, receiver)? {
            Ok(id) => id,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        if self.typed_out_of_bounds(id)? {
            return self.throw_type_error(roots, "TypedArray is detached or out of bounds");
        }
        self.array_iterator(roots, &Value::object(id), kind)
    }
}

/// One entry point for the shared prototype methods.
impl Vm {
    #[allow(
        clippy::too_many_lines,
        reason = "One dispatch over the shared prototype member table"
    )]
    pub(super) fn typed_method(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        name: &'static str,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let (id, kind) = match self.require_typed(roots, receiver)? {
            Ok(value) => value,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let view = (id, args);
        let roots = &roots.with(&view);
        let length = match self.validate_typed_bounds(roots, id)? {
            Ok(length) => length,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let result = match name {
            "at" => self.typed_at(host, roots, id, length, args),
            "copyWithin" => self.typed_copy_within(host, roots, id, kind, length, args),
            "every" => self.typed_predicate(host, roots, id, length, args, Predicate::Every),
            "fill" => self.typed_fill(host, roots, id, kind, length, args),
            "filter" => self.typed_filter(host, roots, id, length, args),
            "find" => self.typed_predicate(host, roots, id, length, args, Predicate::Find),
            "findIndex" => {
                self.typed_predicate(host, roots, id, length, args, Predicate::FindIndex)
            }
            "findLast" => self.typed_predicate(host, roots, id, length, args, Predicate::FindLast),
            "findLastIndex" => {
                self.typed_predicate(host, roots, id, length, args, Predicate::FindLastIndex)
            }
            "forEach" => self.typed_predicate(host, roots, id, length, args, Predicate::ForEach),
            "includes" => self.typed_search(host, roots, id, length, args, Search::Includes),
            "indexOf" => self.typed_search(host, roots, id, length, args, Search::IndexOf),
            "lastIndexOf" => self.typed_search(host, roots, id, length, args, Search::LastIndexOf),
            "join" => self.typed_join(host, roots, id, length, args),
            "map" => self.typed_map(host, roots, id, length, args),
            "reduce" => self.typed_reduce(host, roots, id, length, args, false),
            "reduceRight" => self.typed_reduce(host, roots, id, length, args, true),
            "reverse" => self.typed_reverse(id, length),
            "set" => self.typed_set(host, roots, id, kind, args),
            "slice" => self.typed_slice(host, roots, id, kind, length, args),
            "some" => self.typed_predicate(host, roots, id, length, args, Predicate::Some),
            "sort" => self.typed_sort(host, roots, id, length, args, false),
            "subarray" => self.typed_subarray(host, roots, id, kind, length, args),
            "toLocaleString" => self.typed_locale_string(host, roots, id, length),
            "toReversed" => self.typed_to_reversed(host, roots, id, length),
            "toSorted" => self.typed_sort(host, roots, id, length, args, true),
            "with" => self.typed_with(host, roots, id, length, args),
            _ => unreachable!("unhandled TypedArray member {name}"),
        };
        Ok(match result? {
            Ok(value) => Invocation::Value(value),
            Err(error) => Invocation::Thrown(error),
        })
    }
}

#[derive(Clone, Copy)]
enum Predicate {
    Every,
    Some,
    ForEach,
    Find,
    FindIndex,
    FindLast,
    FindLastIndex,
}

#[derive(Clone, Copy)]
enum Search {
    Includes,
    IndexOf,
    LastIndexOf,
}

impl Vm {
    fn typed_at(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        length: usize,
        args: &[Value],
    ) -> Completion<Value> {
        let index = match self.absolute_index(host, roots, arg(args, 0), length)? {
            Ok(index) => index,
            Err(error) => return Ok(Err(error)),
        };
        if index < 0.0 || index >= length as f64 {
            return Ok(Ok(Value::UNDEFINED));
        }
        Ok(Ok(self.element_value(id, index as u64)?))
    }

    fn typed_copy_within(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        kind: ElementKind,
        length: usize,
        args: &[Value],
    ) -> Completion<Value> {
        let target = match self.clamped_index(host, roots, arg(args, 0), length)? {
            Ok(index) => index,
            Err(error) => return Ok(Err(error)),
        };
        let start = match self.clamped_index(host, roots, arg(args, 1), length)? {
            Ok(index) => index,
            Err(error) => return Ok(Err(error)),
        };
        let end = if matches!(arg(args, 2).view(), ValueRef::Undefined) {
            length
        } else {
            match self.clamped_index(host, roots, arg(args, 2), length)? {
                Ok(index) => index,
                Err(error) => return Ok(Err(error)),
            }
        };
        let count = end.saturating_sub(start).min(length.saturating_sub(target));
        if count > 0 {
            // Side effects may have shrunk the view; keep the longest
            // still-applicable prefix.
            let length = match self.validate_typed_bounds(roots, id)? {
                Ok(length) => length,
                Err(error) => return Ok(Err(error)),
            };
            let count = count
                .min(length.saturating_sub(start))
                .min(length.saturating_sub(target));
            if count > 0 {
                let width = kind.bytes();
                let bytes = self.typed_bytes(id, start as u64, count)?;
                let buffer = self.buffer_of(id)?;
                let base = self.byte_offset_of(id)? + target * width;
                let Some(Object::ArrayBuffer(target_buffer)) = Some(self.heap.get_mut(buffer)?)
                else {
                    unreachable!("a view names an ArrayBuffer")
                };
                if let Some(block) = target_buffer.bytes.as_mut()
                    && base + bytes.len() <= block.len()
                {
                    block[base..base + bytes.len()].copy_from_slice(&bytes);
                }
            }
        }
        Ok(Ok(Value::object(id)))
    }

    fn typed_fill(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        kind: ElementKind,
        length: usize,
        args: &[Value],
    ) -> Completion<Value> {
        let value = match self.element_bits(host, roots, kind, arg(args, 0))? {
            Ok(bits) => bits,
            Err(error) => return Ok(Err(error)),
        };
        let start = match self.clamped_index(host, roots, arg(args, 1), length)? {
            Ok(index) => index,
            Err(error) => return Ok(Err(error)),
        };
        let end = if matches!(arg(args, 2).view(), ValueRef::Undefined) {
            length
        } else {
            match self.clamped_index(host, roots, arg(args, 2), length)? {
                Ok(index) => index,
                Err(error) => return Ok(Err(error)),
            }
        };
        let length = match self.validate_typed_bounds(roots, id)? {
            Ok(length) => length,
            Err(error) => return Ok(Err(error)),
        };
        let end = end.min(length);
        if start < end {
            // One pre-converted bit pattern fills the whole range.
            let buffer = self.buffer_of(id)?;
            let base = self.byte_offset_of(id)? + start * kind.bytes();
            let count = (end - start) * kind.bytes();
            let width = kind.bytes();
            let Some(Object::ArrayBuffer(target)) = Some(self.heap.get_mut(buffer)?) else {
                unreachable!("a view names an ArrayBuffer")
            };
            if let Some(block) = target.bytes.as_mut()
                && base + count <= block.len()
            {
                let mut pattern = vec![0u8; width];
                element::store(&mut pattern, kind, true, value);
                for chunk in block[base..base + count].chunks_mut(width) {
                    chunk.copy_from_slice(&pattern);
                }
            }
        }
        Ok(Ok(Value::object(id)))
    }

    fn typed_predicate(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        length: usize,
        args: &[Value],
        kind: Predicate,
    ) -> Completion<Value> {
        let callback = arg(args, 0).clone();
        if !self.is_callable(&callback) {
            return Ok(Err(self.type_error(
                roots,
                "TypedArray predicate requires a callable callback",
            )?));
        }
        let this_arg = arg(args, 1).clone();
        let descending = matches!(kind, Predicate::FindLast | Predicate::FindLastIndex);
        let view = (&callback, &this_arg);
        let roots = &roots.with(&view);
        for step in 0..length {
            let index = if descending { length - step - 1 } else { step };
            let value = self.element_value(id, index as u64)?;
            let call_roots = &roots.with(&value);
            let result = match self.invoke(
                host,
                call_roots,
                &callback,
                Arguments::Values(&[
                    value.clone(),
                    Value::number(index as f64),
                    Value::object(id),
                ]),
                &this_arg,
            )? {
                Invocation::Value(result) => result,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
            match kind {
                Predicate::Every => {
                    if !result.truthy() {
                        return Ok(Ok(Value::boolean(false)));
                    }
                }
                Predicate::Some => {
                    if result.truthy() {
                        return Ok(Ok(Value::boolean(true)));
                    }
                }
                Predicate::ForEach => {}
                Predicate::Find | Predicate::FindLast => {
                    if result.truthy() {
                        return Ok(Ok(value));
                    }
                }
                Predicate::FindIndex | Predicate::FindLastIndex => {
                    if result.truthy() {
                        return Ok(Ok(Value::number(index as f64)));
                    }
                }
            }
        }
        Ok(Ok(match kind {
            Predicate::Every => Value::boolean(true),
            Predicate::Some => Value::boolean(false),
            Predicate::ForEach => Value::UNDEFINED,
            Predicate::Find | Predicate::FindLast => Value::UNDEFINED,
            Predicate::FindIndex | Predicate::FindLastIndex => Value::number(-1.0),
        }))
    }

    fn typed_search(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        length: usize,
        args: &[Value],
        kind: Search,
    ) -> Completion<Value> {
        let needle = arg(args, 0).clone();
        if length == 0 {
            return Ok(Ok(match kind {
                Search::Includes => Value::boolean(false),
                Search::IndexOf | Search::LastIndexOf => Value::number(-1.0),
            }));
        }
        let roots = &roots.with(&needle);
        let found = match kind {
            Search::LastIndexOf => {
                let mut index = match args.get(1).map(Value::view) {
                    None | Some(ValueRef::Undefined) => length.saturating_sub(1),
                    _ => {
                        let absolute =
                            match self.absolute_index(host, roots, arg(args, 1), length)? {
                                Ok(index) => index,
                                Err(error) => return Ok(Err(error)),
                            };
                        if absolute < 0.0 {
                            return Ok(Ok(Value::number(-1.0)));
                        }
                        (absolute as usize).min(length - 1)
                    }
                };
                loop {
                    if self.element_valid(id, index as u64)? {
                        let value = self.element_value(id, index as u64)?;
                        if self.matches_search(&needle, &value, kind)? {
                            return Ok(Ok(Value::number(index as f64)));
                        }
                    }
                    if index == 0 {
                        break;
                    }
                    index -= 1;
                }
                None
            }
            _ => {
                let start = match self.clamped_index(host, roots, arg(args, 1), length)? {
                    Ok(index) => index,
                    Err(error) => return Ok(Err(error)),
                };
                let mut found = None;
                for index in start..length {
                    if self.element_valid(id, index as u64)? {
                        let value = self.element_value(id, index as u64)?;
                        if self.matches_search(&needle, &value, kind)? {
                            found = Some(index);
                            break;
                        }
                    }
                }
                found
            }
        };
        Ok(Ok(match (kind, found) {
            (Search::Includes, _) => Value::boolean(found.is_some()),
            (Search::IndexOf, Some(index)) | (Search::LastIndexOf, Some(index)) => {
                Value::number(index as f64)
            }
            (Search::IndexOf | Search::LastIndexOf, None) => Value::number(-1.0),
        }))
    }

    fn matches_search(
        &mut self,
        needle: &Value,
        value: &Value,
        kind: Search,
    ) -> Result<bool, VmError> {
        Ok(match kind {
            Search::Includes => same_value_zero(needle, value),
            _ => ops::strict_equal(needle, value),
        })
    }

    fn typed_join(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        length: usize,
        args: &[Value],
    ) -> Completion<Value> {
        let separator = if matches!(arg(args, 0).view(), ValueRef::Undefined) {
            ",".to_owned()
        } else {
            match self.coerce_string(host, roots, arg(args, 0))? {
                Ok(text) => text.to_utf8().unwrap_or_default(),
                Err(error) => return Ok(Err(error)),
            }
        };
        let mut out = String::new();
        for index in 0..length {
            if index > 0 {
                out.push_str(&separator);
            }
            let value = self.element_value(id, index as u64)?;
            if !matches!(value.view(), ValueRef::Undefined | ValueRef::Null) {
                let text = ops::to_js_string(&value);
                out.push_str(&text.to_utf8().unwrap_or_default());
            }
        }
        Ok(Ok(Value::string(out.into())))
    }

    fn typed_locale_string(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        length: usize,
    ) -> Completion<Value> {
        let mut out = String::new();
        for index in 0..length {
            if index > 0 {
                out.push(',');
            }
            let value = self.element_value(id, index as u64)?;
            if matches!(value.view(), ValueRef::Undefined | ValueRef::Null) {
                continue;
            }
            let roots = &roots.with(&value);
            let method = match self.read_property(
                host,
                roots,
                &value,
                PropertyKeyRef::String("toLocaleString"),
            )? {
                Invocation::Value(method) => method,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
            if !self.is_callable(&method) {
                return Ok(Err(
                    self.type_error(roots, "element toLocaleString is not callable")?
                ));
            }
            let roots = &roots.with(&method);
            let text = match self.invoke(host, roots, &method, Arguments::EMPTY, &value)? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
            let roots = &roots.with(&text);
            match self.coerce_string(host, roots, &text)? {
                Ok(text) => out.push_str(&text.to_utf8().unwrap_or_default()),
                Err(error) => return Ok(Err(error)),
            }
        }
        Ok(Ok(Value::string(out.into())))
    }

    fn typed_map(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        length: usize,
        args: &[Value],
    ) -> Completion<Value> {
        let callback = arg(args, 0).clone();
        if !self.is_callable(&callback) {
            return Ok(Err(self.type_error(
                roots,
                "TypedArray map requires a callable callback",
            )?));
        }
        let this_arg = arg(args, 1).clone();
        let view = (&callback, &this_arg);
        let roots = &roots.with(&view);
        let result =
            match self.species_create(host, roots, id, vec![Value::number(length as f64)])? {
                Ok(result) => result,
                Err(error) => return Ok(Err(error)),
            };
        let roots = &roots.with(&result);
        for index in 0..length {
            let value = self.element_value(id, index as u64)?;
            let call_roots = &roots.with(&value);
            let mapped = match self.invoke(
                host,
                call_roots,
                &callback,
                Arguments::Values(&[
                    value.clone(),
                    Value::number(index as f64),
                    Value::object(id),
                ]),
                &this_arg,
            )? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
            let call_roots = &roots.with(&mapped);
            match self.typed_set_element(host, call_roots, result, index as u64, &mapped)? {
                Ok(()) => {}
                Err(error) => return Ok(Err(error)),
            }
        }
        Ok(Ok(Value::object(result)))
    }

    fn typed_filter(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        length: usize,
        args: &[Value],
    ) -> Completion<Value> {
        let callback = arg(args, 0).clone();
        if !self.is_callable(&callback) {
            return Ok(Err(self.type_error(
                roots,
                "TypedArray filter requires a callable callback",
            )?));
        }
        let this_arg = arg(args, 1).clone();
        let view = (&callback, &this_arg);
        let roots = &roots.with(&view);
        let mut kept = Vec::new();
        for index in 0..length {
            let value = self.element_value(id, index as u64)?;
            let call_roots = &roots.with(&value);
            let selected = match self.invoke(
                host,
                call_roots,
                &callback,
                Arguments::Values(&[
                    value.clone(),
                    Value::number(index as f64),
                    Value::object(id),
                ]),
                &this_arg,
            )? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
            if selected.truthy() {
                kept.push(value);
            }
        }
        let roots = &roots.with(&kept);
        let result =
            match self.species_create(host, roots, id, vec![Value::number(kept.len() as f64)])? {
                Ok(result) => result,
                Err(error) => return Ok(Err(error)),
            };
        let roots = &roots.with(&result);
        for (index, value) in kept.iter().enumerate() {
            match self.typed_set_element(host, roots, result, index as u64, value)? {
                Ok(()) => {}
                Err(error) => return Ok(Err(error)),
            }
        }
        Ok(Ok(Value::object(result)))
    }

    fn typed_reduce(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        length: usize,
        args: &[Value],
        right: bool,
    ) -> Completion<Value> {
        let callback = arg(args, 0).clone();
        if !self.is_callable(&callback) {
            return Ok(Err(self.type_error(
                roots,
                "TypedArray reduce requires a callable callback",
            )?));
        }
        let present = args.len() > 1;
        if length == 0 && !present {
            return Ok(Err(self.type_error(
                roots,
                "reduce of an empty TypedArray with no initial value",
            )?));
        }
        let mut accumulator = if present {
            arg(args, 1).clone()
        } else {
            let index = if right { length - 1 } else { 0 };
            self.element_value(id, index as u64)?
        };
        let mut step = usize::from(!present);
        let roots = &roots.with(&callback);
        while step < length {
            let index = if right { length - step - 1 } else { step };
            let value = self.element_value(id, index as u64)?;
            let view = (&accumulator, &value);
            let call_roots = &roots.with(&view);
            accumulator = match self.invoke(
                host,
                call_roots,
                &callback,
                Arguments::Values(&[
                    accumulator.clone(),
                    value.clone(),
                    Value::number(index as f64),
                    Value::object(id),
                ]),
                &Value::UNDEFINED,
            )? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
            step += 1;
        }
        Ok(Ok(accumulator))
    }

    fn typed_reverse(&mut self, id: ObjectId, length: usize) -> Completion<Value> {
        for lower in 0..length / 2 {
            let upper = length - lower - 1;
            let lower_value = self.element_value(id, lower as u64)?;
            let upper_value = self.element_value(id, upper as u64)?;
            if let (ValueRef::Number(_), ValueRef::Number(_))
            | (ValueRef::BigInt(_), ValueRef::BigInt(_)) =
                (lower_value.view(), upper_value.view())
            {
                let kind = self
                    .heap
                    .typed_array(id)?
                    .map(|view| view.kind)
                    .expect("receiver was validated");
                let lower_bits = self.bits_of(kind, &lower_value);
                let upper_bits = self.bits_of(kind, &upper_value);
                self.heap
                    .store_typed_element(id, lower as u64, upper_bits)?;
                self.heap
                    .store_typed_element(id, upper as u64, lower_bits)?;
            }
        }
        Ok(Ok(Value::object(id)))
    }

    /// Bits of an element that is already in the element's numeric domain.
    fn bits_of(&self, kind: ElementKind, value: &Value) -> u64 {
        match value.view() {
            ValueRef::Number(number) => element::number_bits(kind, number),
            ValueRef::BigInt(big) => element::bigint_bits(big),
            _ => 0,
        }
    }

    fn typed_set(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        kind: ElementKind,
        args: &[Value],
    ) -> Completion<Value> {
        let offset = match self.integer_or_infinity(host, roots, arg(args, 1))? {
            Ok(offset) => offset,
            Err(error) => return Ok(Err(error)),
        };
        if offset < 0.0 {
            return Ok(Err(
                self.make_range_error(roots, "TypedArray set offset is negative")?
            ));
        }
        let source = arg(args, 0).clone();
        let roots = &roots.with(&source);
        let source_view = match source.view() {
            ValueRef::Object(source_id) => self.heap.typed_array(source_id)?.is_some(),
            _ => false,
        };
        let result = if source_view {
            self.set_from_typed(host, roots, id, kind, offset, &source)
        } else {
            self.set_from_array_like(host, roots, id, offset, &source)
        };
        match result? {
            Ok(()) => Ok(Ok(Value::UNDEFINED)),
            Err(error) => Ok(Err(error)),
        }
    }

    fn set_from_typed(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
        kind: ElementKind,
        offset: f64,
        source: &Value,
    ) -> Completion<()> {
        let ValueRef::Object(source_id) = source.view() else {
            unreachable!("caller checked the source is an object")
        };
        let target_length = match self.validate_typed_bounds(roots, target)? {
            Ok(length) => length,
            Err(error) => return Ok(Err(error)),
        };
        let source_length = match self.validate_typed_bounds(roots, source_id)? {
            Ok(length) => length,
            Err(error) => return Ok(Err(error)),
        };
        let source_kind = match self.heap.typed_array(source_id)? {
            Some(view) => view.kind,
            None => unreachable!("caller checked the source is a TypedArray"),
        };
        if kind.is_bigint() != source_kind.is_bigint() {
            return Ok(Err(self.type_error(
                roots,
                "cannot set BigInt elements from a Number TypedArray",
            )?));
        }
        if offset.is_infinite() || offset + source_length as f64 > target_length as f64 {
            return Ok(Err(self.make_range_error(
                roots,
                "TypedArray set exceeds the target length",
            )?));
        }
        let offset = offset as usize;
        if source_length == 0 {
            return Ok(Ok(()));
        }
        // A source over the target's own buffer must be snapshotted first: the
        // copy would otherwise read bytes it already overwrote.
        let shared = self.buffer_of(target)? == self.buffer_of(source_id)?;
        if shared {
            let snapshot = self.typed_bytes(source_id, 0, source_length)?;
            let buffer = self.buffer_of(target)?;
            let target_base = self.byte_offset_of(target)? + offset * kind.bytes();
            if source_kind == kind {
                if let Some(Object::ArrayBuffer(record)) = Some(self.heap.get_mut(buffer)?)
                    && let Some(block) = record.bytes.as_mut()
                    && target_base + snapshot.len() <= block.len()
                {
                    block[target_base..target_base + snapshot.len()].copy_from_slice(&snapshot);
                }
                return Ok(Ok(()));
            }
            let mut out = Vec::with_capacity(source_length * kind.bytes());
            for index in 0..source_length {
                let start = index * source_kind.bytes();
                let bits = element::load(&snapshot[start..], source_kind, true);
                let value = element::decode(source_kind, bits);
                let bits = match self.element_bits(host, roots, kind, &value)? {
                    Ok(bits) => bits,
                    Err(error) => return Ok(Err(error)),
                };
                let mut chunk = vec![0u8; kind.bytes()];
                element::store(&mut chunk, kind, true, bits);
                out.extend_from_slice(&chunk);
            }
            if let Some(Object::ArrayBuffer(record)) = Some(self.heap.get_mut(buffer)?)
                && let Some(block) = record.bytes.as_mut()
                && target_base + out.len() <= block.len()
            {
                block[target_base..target_base + out.len()].copy_from_slice(&out);
            }
            return Ok(Ok(()));
        }
        if source_kind == kind {
            let bytes = self.typed_bytes(source_id, 0, source_length)?;
            let buffer = self.buffer_of(target)?;
            let base = self.byte_offset_of(target)? + offset * kind.bytes();
            if let Some(Object::ArrayBuffer(record)) = Some(self.heap.get_mut(buffer)?)
                && let Some(block) = record.bytes.as_mut()
                && base + bytes.len() <= block.len()
            {
                block[base..base + bytes.len()].copy_from_slice(&bytes);
            }
            return Ok(Ok(()));
        }
        for index in 0..source_length {
            let value = self.element_value(source_id, index as u64)?;
            let roots = &roots.with(&value);
            match self.typed_set_element(host, roots, target, (offset + index) as u64, &value)? {
                Ok(()) => {}
                Err(error) => return Ok(Err(error)),
            }
        }
        Ok(Ok(()))
    }

    fn set_from_array_like(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
        offset: f64,
        source: &Value,
    ) -> Completion<()> {
        let target_length = match self.validate_typed_bounds(roots, target)? {
            Ok(length) => length,
            Err(error) => return Ok(Err(error)),
        };
        if matches!(source.view(), ValueRef::Null | ValueRef::Undefined) {
            return Ok(Err(self.type_error(
                roots,
                "TypedArray set source cannot be null or undefined",
            )?));
        }
        let source_id = self.coercible_object(source);
        let roots = &roots.with(&source_id);
        let source_length = match self.length_of_array_like(host, roots, source_id)? {
            Ok(length) => length,
            Err(error) => return Ok(Err(error)),
        };
        if offset.is_infinite() || offset + source_length as f64 > target_length as f64 {
            return Ok(Err(self.make_range_error(
                roots,
                "TypedArray set exceeds the target length",
            )?));
        }
        let offset = offset as usize;
        for index in 0..source_length {
            let key = index.to_string();
            let value = match self.read_property(
                host,
                roots,
                &Value::object(source_id),
                PropertyKeyRef::String(&key),
            )? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
            let call_roots = &roots.with(&value);
            match self.typed_set_element(
                host,
                call_roots,
                target,
                (offset + index as usize) as u64,
                &value,
            )? {
                Ok(()) => {}
                Err(error) => return Ok(Err(error)),
            }
        }
        Ok(Ok(()))
    }

    fn typed_slice(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        kind: ElementKind,
        length: usize,
        args: &[Value],
    ) -> Completion<Value> {
        let start = match self.clamped_index(host, roots, arg(args, 0), length)? {
            Ok(index) => index,
            Err(error) => return Ok(Err(error)),
        };
        let end = if matches!(arg(args, 1).view(), ValueRef::Undefined) {
            length
        } else {
            match self.clamped_index(host, roots, arg(args, 1), length)? {
                Ok(index) => index,
                Err(error) => return Ok(Err(error)),
            }
        };
        let count = end.saturating_sub(start);
        let result =
            match self.species_create(host, roots, id, vec![Value::number(count as f64)])? {
                Ok(result) => result,
                Err(error) => return Ok(Err(error)),
            };
        if count > 0 {
            let roots = &roots.with(&result);
            let length = match self.validate_typed_bounds(roots, id)? {
                Ok(length) => length,
                Err(error) => return Ok(Err(error)),
            };
            let end = end.min(length);
            let count = end.saturating_sub(start);
            let target_kind = self
                .heap
                .typed_array(result)?
                .map(|view| view.kind)
                .expect("species create returned a TypedArray");
            if target_kind == kind {
                let bytes = self.typed_bytes(id, start as u64, count)?;
                let buffer = self.buffer_of(result)?;
                let base = self.byte_offset_of(result)?;
                if let Some(Object::ArrayBuffer(record)) = Some(self.heap.get_mut(buffer)?)
                    && let Some(block) = record.bytes.as_mut()
                    && base + bytes.len() <= block.len()
                {
                    block[base..base + bytes.len()].copy_from_slice(&bytes);
                }
            } else {
                for index in 0..count {
                    let value = self.element_value(id, (start + index) as u64)?;
                    let call_roots = &roots.with(&value);
                    match self.typed_set_element(host, call_roots, result, index as u64, &value)? {
                        Ok(()) => {}
                        Err(error) => return Ok(Err(error)),
                    }
                }
            }
        }
        Ok(Ok(Value::object(result)))
    }

    fn typed_subarray(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        kind: ElementKind,
        length: usize,
        args: &[Value],
    ) -> Completion<Value> {
        let Some(view) = self.heap.typed_array(id)? else {
            unreachable!("receiver was validated")
        };
        let (buffer, byte_offset, tracking) =
            (view.buffer, view.byte_offset, view.byte_length.is_none());
        let source_length = if self.typed_out_of_bounds(id)? {
            0
        } else {
            length
        };
        let start = match self.clamped_index(host, roots, arg(args, 0), source_length)? {
            Ok(index) => index,
            Err(error) => return Ok(Err(error)),
        };
        let begin = byte_offset + start * kind.bytes();
        if tracking && matches!(arg(args, 1).view(), ValueRef::Undefined) {
            return match self.species_create(
                host,
                roots,
                id,
                vec![Value::object(buffer), Value::number(begin as f64)],
            )? {
                Ok(result) => Ok(Ok(Value::object(result))),
                Err(error) => Ok(Err(error)),
            };
        }
        let end = if matches!(arg(args, 1).view(), ValueRef::Undefined) {
            source_length
        } else {
            match self.clamped_index(host, roots, arg(args, 1), source_length)? {
                Ok(index) => index,
                Err(error) => return Ok(Err(error)),
            }
        };
        let count = end.saturating_sub(start);
        match self.species_create(
            host,
            roots,
            id,
            vec![
                Value::object(buffer),
                Value::number(begin as f64),
                Value::number(count as f64),
            ],
        )? {
            Ok(result) => Ok(Ok(Value::object(result))),
            Err(error) => Ok(Err(error)),
        }
    }

    fn typed_to_reversed(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        length: usize,
    ) -> Completion<Value> {
        let result = match self.same_type_create(host, roots, id, length)? {
            Ok(result) => result,
            Err(error) => return Ok(Err(error)),
        };
        let roots = &roots.with(&result);
        let kind = self
            .heap
            .typed_array(id)?
            .map(|view| view.kind)
            .expect("receiver was validated");
        for index in 0..length {
            let from = length - index - 1;
            let value = self.element_value(id, from as u64)?;
            let call_roots = &roots.with(&value);
            match self.typed_set_element(host, call_roots, result, index as u64, &value)? {
                Ok(()) => {}
                Err(error) => return Ok(Err(error)),
            }
            let _ = kind;
        }
        Ok(Ok(Value::object(result)))
    }

    fn typed_with(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        length: usize,
        args: &[Value],
    ) -> Completion<Value> {
        let index = match self.absolute_index(host, roots, arg(args, 0), length)? {
            Ok(index) => index,
            Err(error) => return Ok(Err(error)),
        };
        let kind = self
            .heap
            .typed_array(id)?
            .map(|view| view.kind)
            .expect("receiver was validated");
        // The value converts before the index is validated.
        let bits = match self.element_bits(host, roots, kind, arg(args, 1))? {
            Ok(bits) => bits,
            Err(error) => return Ok(Err(error)),
        };
        if index < 0.0 || index >= length as f64 {
            return Ok(Err(self.make_range_error(
                roots,
                "TypedArray with index is out of range",
            )?));
        }
        let index = index as usize;
        let result = match self.same_type_create(host, roots, id, length)? {
            Ok(result) => result,
            Err(error) => return Ok(Err(error)),
        };
        let roots = &roots.with(&result);
        for position in 0..length {
            if position == index {
                let buffer = self.buffer_of(result)?;
                let at = self.byte_offset_of(result)? + position * kind.bytes();
                if let Some(Object::ArrayBuffer(record)) = Some(self.heap.get_mut(buffer)?)
                    && let Some(block) = record.bytes.as_mut()
                    && at + kind.bytes() <= block.len()
                {
                    element::store(&mut block[at..], kind, true, bits);
                }
            } else {
                let value = self.element_value(id, position as u64)?;
                let call_roots = &roots.with(&value);
                match self.typed_set_element(host, call_roots, result, position as u64, &value)? {
                    Ok(()) => {}
                    Err(error) => return Ok(Err(error)),
                }
            }
        }
        Ok(Ok(Value::object(result)))
    }

    fn typed_sort(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        length: usize,
        args: &[Value],
        copying: bool,
    ) -> Completion<Value> {
        let comparator = arg(args, 0).clone();
        if !matches!(comparator.view(), ValueRef::Undefined) && !self.is_callable(&comparator) {
            return Ok(Err(self.type_error(
                roots,
                "TypedArray sort comparator is not callable",
            )?));
        }
        let roots = &roots.with(&comparator);
        let result = if copying {
            match self.same_type_create(host, roots, id, length)? {
                Ok(result) => result,
                Err(error) => return Ok(Err(error)),
            }
        } else {
            id
        };
        let roots = &roots.with(&result);
        let mut items = Vec::with_capacity(length);
        for index in 0..length {
            items.push(self.element_value(id, index as u64)?);
        }
        let kind = self
            .heap
            .typed_array(id)?
            .map(|view| view.kind)
            .expect("receiver was validated");
        match self.sort_items(host, roots, &mut items, &comparator, kind)? {
            Ok(()) => {}
            Err(error) => return Ok(Err(error)),
        }
        for (index, value) in items.iter().enumerate() {
            let call_roots = &roots.with(value);
            match self.typed_set_element(host, call_roots, result, index as u64, value)? {
                Ok(()) => {}
                Err(error) => return Ok(Err(error)),
            }
        }
        Ok(Ok(Value::object(result)))
    }

    /// SortIndexedProperties with `CompareTypedArrayElements`: a stable sort
    /// whose comparator may run script code.
    fn sort_items(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        items: &mut [Value],
        comparator: &Value,
        kind: ElementKind,
    ) -> Completion<()> {
        let roots = &roots.with(&comparator);
        let len = items.len();
        if len < 2 {
            return Ok(Ok(()));
        }
        // Bottom-up merge sort: stable, and every comparison can throw.
        let mut width = 1;
        let mut buffer: Vec<Value> = items.to_vec();
        while width < len {
            let mut left = 0;
            while left < len {
                let middle = (left + width).min(len);
                let right = (left + 2 * width).min(len);
                let mut i = left;
                let mut j = middle;
                let mut out = left;
                while i < middle && j < right {
                    let (a, b) = (items[i].clone(), items[j].clone());
                    let order =
                        match self.compare_elements(host, roots, &a, &b, comparator, kind)? {
                            Ok(order) => order,
                            Err(error) => return Ok(Err(error)),
                        };
                    if order != Ordering::Greater {
                        buffer[out] = items[i].clone();
                        i += 1;
                    } else {
                        buffer[out] = items[j].clone();
                        j += 1;
                    }
                    out += 1;
                }
                while i < middle {
                    buffer[out] = items[i].clone();
                    i += 1;
                    out += 1;
                }
                while j < right {
                    buffer[out] = items[j].clone();
                    j += 1;
                    out += 1;
                }
                left = right;
            }
            items.clone_from_slice(&buffer);
            width *= 2;
        }
        Ok(Ok(()))
    }

    /// CompareTypedArrayElements: numeric order with NaN last and -0 before +0.
    fn compare_elements(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        x: &Value,
        y: &Value,
        comparator: &Value,
        kind: ElementKind,
    ) -> Completion<Ordering> {
        if !matches!(comparator.view(), ValueRef::Undefined) {
            let view = (x, y);
            let roots = &roots.with(&view);
            let result = match self.invoke(
                host,
                roots,
                comparator,
                Arguments::Values(&[x.clone(), y.clone()]),
                &Value::UNDEFINED,
            )? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
            let number = match self.coerce_number(host, roots, &result)? {
                Ok(number) => number,
                Err(error) => return Ok(Err(error)),
            };
            return Ok(Ok(if number.is_nan() || number == 0.0 {
                Ordering::Equal
            } else if number < 0.0 {
                Ordering::Less
            } else {
                Ordering::Greater
            }));
        }
        let _ = kind;
        let (a, b) = (numeric(x), numeric(y));
        Ok(Ok(match (a, b) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(a), Some(b)) => {
                if a < b {
                    Ordering::Less
                } else if a > b {
                    Ordering::Greater
                } else if a == 0.0 && b == 0.0 {
                    match (a.is_sign_negative(), b.is_sign_negative()) {
                        (true, false) => Ordering::Less,
                        (false, true) => Ordering::Greater,
                        _ => Ordering::Equal,
                    }
                } else {
                    Ordering::Equal
                }
            }
        }))
    }

    fn buffer_of(&self, id: ObjectId) -> Result<ObjectId, VmError> {
        Ok(self
            .heap
            .typed_array(id)?
            .map(|view| view.buffer)
            .unwrap_or(id))
    }

    fn byte_offset_of(&self, id: ObjectId) -> Result<usize, VmError> {
        Ok(self
            .heap
            .typed_array(id)?
            .map_or(0, |view| view.byte_offset))
    }

    fn typed_from(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        if !self.is_constructor(receiver)? {
            return self.throw_type_error(roots, "%TypedArray%.from requires a constructor");
        }
        let mapper = arg(args, 1).clone();
        let mapping = !matches!(mapper.view(), ValueRef::Undefined);
        if mapping && !self.is_callable(&mapper) {
            return self.throw_type_error(roots, "%TypedArray%.from mapper is not callable");
        }
        let this_arg = arg(args, 2).clone();
        let source = arg(args, 0).clone();
        let view = (receiver, &mapper, &this_arg, &source);
        let roots = &roots.with(&view);
        let map = MapFn {
            enabled: mapping,
            function: &mapper,
            this_arg: &this_arg,
        };
        let result = self.typed_from_impl(host, roots, receiver, &source, map);
        Ok(match result? {
            Ok(value) => Invocation::Value(value),
            Err(error) => Invocation::Thrown(error),
        })
    }

    fn typed_from_impl(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        constructor: &Value,
        source: &Value,
        map: MapFn<'_>,
    ) -> Completion<Value> {
        let method = if matches!(source.view(), ValueRef::Object(_) | ValueRef::String(_)) {
            match self.read_property(
                host,
                roots,
                source,
                PropertyKeyRef::Symbol(ITERATOR_SYMBOL),
            )? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            }
        } else {
            Value::UNDEFINED
        };
        if !matches!(method.view(), ValueRef::Undefined | ValueRef::Null) {
            let list = match self.iterator_to_list(host, roots, source, &method)? {
                Ok(list) => list,
                Err(error) => return Ok(Err(error)),
            };
            let roots = &roots.with(&list);
            let target = match self.typed_create_from_constructor(
                host,
                roots,
                constructor,
                vec![Value::number(list.len() as f64)],
                None,
            )? {
                Ok(target) => target,
                Err(error) => return Ok(Err(error)),
            };
            let roots = &roots.with(&target);
            for (index, value) in list.iter().enumerate() {
                let mapped = if map.enabled {
                    let call_roots = &roots.with(value);
                    match self.invoke(
                        host,
                        call_roots,
                        map.function,
                        Arguments::Values(&[value.clone(), Value::number(index as f64)]),
                        map.this_arg,
                    )? {
                        Invocation::Value(value) => value,
                        Invocation::Thrown(error) => return Ok(Err(error)),
                    }
                } else {
                    value.clone()
                };
                let call_roots = &roots.with(&mapped);
                match self.typed_set_element(host, call_roots, target, index as u64, &mapped)? {
                    Ok(()) => {}
                    Err(error) => return Ok(Err(error)),
                }
            }
            return Ok(Ok(Value::object(target)));
        }
        let source_id = self.coercible_object(source);
        let roots = &roots.with(&source_id);
        let length = match self.length_of_array_like(host, roots, source_id)? {
            Ok(length) => length,
            Err(error) => return Ok(Err(error)),
        };
        let target = match self.typed_create_from_constructor(
            host,
            roots,
            constructor,
            vec![Value::number(length as f64)],
            None,
        )? {
            Ok(target) => target,
            Err(error) => return Ok(Err(error)),
        };
        let roots = &roots.with(&target);
        for index in 0..length {
            let key = index.to_string();
            let value = match self.read_property(
                host,
                roots,
                &Value::object(source_id),
                PropertyKeyRef::String(&key),
            )? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
            let mapped = if map.enabled {
                let call_roots = &roots.with(&value);
                match self.invoke(
                    host,
                    call_roots,
                    map.function,
                    Arguments::Values(&[value.clone(), Value::number(index as f64)]),
                    map.this_arg,
                )? {
                    Invocation::Value(value) => value,
                    Invocation::Thrown(error) => return Ok(Err(error)),
                }
            } else {
                value
            };
            let call_roots = &roots.with(&mapped);
            match self.typed_set_element(host, call_roots, target, index, &mapped)? {
                Ok(()) => {}
                Err(error) => return Ok(Err(error)),
            }
        }
        Ok(Ok(Value::object(target)))
    }

    fn typed_of(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        if !self.is_constructor(receiver)? {
            return self.throw_type_error(roots, "%TypedArray%.of requires a constructor");
        }
        let roots = &roots.with(&args);
        let result = self.typed_of_impl(host, roots, receiver, &args);
        Ok(match result? {
            Ok(value) => Invocation::Value(value),
            Err(error) => Invocation::Thrown(error),
        })
    }

    fn typed_of_impl(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        constructor: &Value,
        items: &[Value],
    ) -> Completion<Value> {
        let target = match self.typed_create_from_constructor(
            host,
            roots,
            constructor,
            vec![Value::number(items.len() as f64)],
            None,
        )? {
            Ok(target) => target,
            Err(error) => return Ok(Err(error)),
        };
        let roots = &roots.with(&target);
        for (index, value) in items.iter().enumerate() {
            match self.typed_set_element(host, roots, target, index as u64, value)? {
                Ok(()) => {}
                Err(error) => return Ok(Err(error)),
            }
        }
        Ok(Ok(Value::object(target)))
    }
}

/// Numeric view of a sorted element: `None` is NaN, which sorts last.
fn numeric(value: &Value) -> Option<f64> {
    match value.view() {
        ValueRef::Number(number) if number.is_nan() => None,
        ValueRef::Number(number) => Some(number),
        ValueRef::BigInt(big) => Some(big.to_f64()),
        _ => None,
    }
}

/// SameValueZero, which treats every NaN as equal to every other NaN.
fn same_value_zero(lhs: &Value, rhs: &Value) -> bool {
    match (lhs.view(), rhs.view()) {
        (ValueRef::Number(lhs), ValueRef::Number(rhs)) => {
            lhs == rhs || lhs.is_nan() && rhs.is_nan()
        }
        _ => ops::strict_equal(lhs, rhs),
    }
}
