//! Binary data: ArrayBuffer, DataView, and the twelve TypedArray constructors.
//!
//! The buffer object owns the bytes and every view names it, so aliasing is the
//! object graph the snapshot already records. Element encoding is shared with
//! DataView through [`element`]; index semantics live in the heap, because an
//! element read needs the viewed buffer.
//!
//! Every user-visible conversion is a completion: a getter, host call, or
//! `valueOf` can resize, transfer, or collect the buffer, so no borrow of the
//! bytes survives one.

pub(crate) mod element;

mod buffer;
mod codec;
pub(crate) mod text;
mod typed;
mod view;

use crate::builtin::globals::Owner;
use crate::builtin::{
    FUNCTION_PARENT, OBJECT_PARENT, RootKind, RootSpec, define_builtin_data, install_getter,
    link_constructor,
};
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{ArrayBufferObject, Attributes, ElementKind, Object, PlainObject, ViewRange};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::Invocation;
use crate::value::{JsBigInt, ObjectId, PropertyKey, PropertyKeyRef, Value, ValueRef};
use crate::vm::Vm;

/// A script throw stays a value until it leaves the VM.
pub(crate) type Completion<T> = Result<Result<T, Value>, VmError>;

/// `2^53 - 1`, the largest byte length ToIndex returns.
pub(crate) const MAX_BYTE_LENGTH: usize = (1 << 53) - 1;

pub(crate) const ARRAY_BUFFER_ROOT: &str = "arrayBuffer";
pub(crate) const ARRAY_BUFFER_PROTOTYPE: &str = "arrayBuffer.prototype";
pub(crate) const DATA_VIEW_ROOT: &str = "dataView";
pub(crate) const DATA_VIEW_PROTOTYPE: &str = "dataView.prototype";
pub(crate) const TYPED_ARRAY_ROOT: &str = "typedArray";
pub(crate) const TYPED_ARRAY_PROTOTYPE: &str = "typedArray.prototype";

pub(crate) const ARRAY_BUFFER: IntrinsicKey = IntrinsicKey::new("ArrayBuffer", Slot::Global, "");
pub(crate) const DATA_VIEW: IntrinsicKey = IntrinsicKey::new("DataView", Slot::Global, "");
pub(crate) const TYPED_ARRAY: IntrinsicKey = IntrinsicKey::new("TypedArray", Slot::Global, "");

/// One concrete TypedArray family. The shared constructor and prototype are
/// roots of their own; a family only adds its name, width, and instance type.
pub(crate) struct Family {
    pub(crate) kind: ElementKind,
    pub(crate) root: &'static str,
    pub(crate) prototype: &'static str,
}

macro_rules! families {
    ($($function:ident: $kind:ident => $root:literal, $prototype:literal),+ $(,)?) => {
        pub(crate) const FAMILIES: &[Family] = &[
            $(Family { kind: ElementKind::$kind, root: $root, prototype: $prototype }),+
        ];

        pub(crate) const BINARY_ROOTS: &[RootSpec] = &[
            RootSpec::new(ARRAY_BUFFER_ROOT, RootKind::Native(ARRAY_BUFFER), FUNCTION_PARENT)
                .fixed(ARRAY_BUFFER_PROTOTYPE)
                .backlink(Some(Attributes::METHOD)),
            RootSpec::new(ARRAY_BUFFER_PROTOTYPE, RootKind::Plain, OBJECT_PARENT),
            RootSpec::new(DATA_VIEW_ROOT, RootKind::Native(DATA_VIEW), FUNCTION_PARENT)
                .fixed(DATA_VIEW_PROTOTYPE)
                .backlink(Some(Attributes::METHOD)),
            RootSpec::new(DATA_VIEW_PROTOTYPE, RootKind::Plain, OBJECT_PARENT),
            RootSpec::new(TYPED_ARRAY_ROOT, RootKind::Native(TYPED_ARRAY), FUNCTION_PARENT)
                .fixed(TYPED_ARRAY_PROTOTYPE)
                .backlink(Some(Attributes::METHOD)),
            RootSpec::new(TYPED_ARRAY_PROTOTYPE, RootKind::Plain, OBJECT_PARENT),
            $(
                RootSpec::new(
                    $root,
                    RootKind::Native(IntrinsicKey::new(ElementKind::$kind.name(), Slot::Global, "")),
                    Some(TYPED_ARRAY_ROOT),
                )
                .fixed($prototype)
                .backlink(Some(Attributes::METHOD)),
                RootSpec::new($prototype, RootKind::Plain, Some(TYPED_ARRAY_PROTOTYPE)),
            )+
        ];

        const CONSTRUCTORS: &[IntrinsicDef] = &[
            $(
                IntrinsicDef::constructor_with_target(
                    IntrinsicKey::new(ElementKind::$kind.name(), Slot::Global, ""),
                    3,
                    typed::call_requires_new,
                    family_constructors::$function,
                ),
            )+
        ];
    };
}

mod family_constructors {
    use super::ElementKind;
    use crate::error::VmError;
    use crate::gc::ActiveRoots;
    use crate::host::Host;
    use crate::runtime::Invocation;
    use crate::value::Value;
    use crate::vm::Vm;

    macro_rules! constructors {
        ($($function:ident => $kind:ident),+ $(,)?) => {
            $(
                pub(super) fn $function(
                    vm: &mut Vm,
                    host: &mut dyn Host,
                    roots: &ActiveRoots<'_>,
                    args: Vec<Value>,
                    new_target: &Value,
                ) -> Result<Invocation, VmError> {
                    super::typed::construct(vm, host, roots, args, new_target, ElementKind::$kind)
                }
            )+
        };
    }

    constructors!(
        int8 => Int8,
        uint8 => Uint8,
        uint8_clamped => Uint8Clamped,
        int16 => Int16,
        uint16 => Uint16,
        int32 => Int32,
        uint32 => Uint32,
        float16 => Float16,
        float32 => Float32,
        float64 => Float64,
        big_int64 => BigInt64,
        big_uint64 => BigUint64,
    );
}

families! {
    int8: Int8 => "int8Array", "int8Array.prototype",
    uint8: Uint8 => "uint8Array", "uint8Array.prototype",
    uint8_clamped: Uint8Clamped => "uint8ClampedArray", "uint8ClampedArray.prototype",
    int16: Int16 => "int16Array", "int16Array.prototype",
    uint16: Uint16 => "uint16Array", "uint16Array.prototype",
    int32: Int32 => "int32Array", "int32Array.prototype",
    uint32: Uint32 => "uint32Array", "uint32Array.prototype",
    float16: Float16 => "float16Array", "float16Array.prototype",
    float32: Float32 => "float32Array", "float32Array.prototype",
    float64: Float64 => "float64Array", "float64Array.prototype",
    big_int64: BigInt64 => "bigInt64Array", "bigInt64Array.prototype",
    big_uint64: BigUint64 => "bigUint64Array", "bigUint64Array.prototype",
}

const GROUPS: &[&[IntrinsicDef]] = &[
    buffer::INTRINSICS,
    view::INTRINSICS,
    typed::INTRINSICS,
    CONSTRUCTORS,
];

pub(crate) const OWNER: Owner = Owner::new(ARRAY_BUFFER_ROOT, BINARY_ROOTS, GROUPS, Some(install));

/// Binary roots are allocated as a block, so the first binary global installs
/// all of them: two shared prototypes plus one constructor per element type.
fn install(vm: &mut Vm, _: &Value) {
    for (root, prototype) in [
        (ARRAY_BUFFER_ROOT, ARRAY_BUFFER_PROTOTYPE),
        (DATA_VIEW_ROOT, DATA_VIEW_PROTOTYPE),
        (TYPED_ARRAY_ROOT, TYPED_ARRAY_PROTOTYPE),
    ] {
        link_constructor(vm, vm.builtins.get(root), vm.builtins.get(prototype));
    }
    buffer::install(vm);
    view::install(vm);
    for family in FAMILIES {
        link_constructor(
            vm,
            vm.builtins.get(family.root),
            vm.builtins.get(family.prototype),
        );
        // A concrete constructor inherits statics from %TypedArray%, and its
        // prototype inherits every shared method from %TypedArray%.prototype.
        let constructor = vm.builtins.get(family.root);
        let prototype = vm.builtins.get(family.prototype);
        let shared = vm.builtins.get(TYPED_ARRAY_ROOT);
        let shared_prototype = vm.builtins.get(TYPED_ARRAY_PROTOTYPE);
        assert!(
            vm.heap
                .set_prototype(constructor, Some(shared))
                .expect("typed array constructor prototype"),
            "typed array constructor shares %TypedArray%"
        );
        assert!(
            vm.heap
                .set_prototype(prototype, Some(shared_prototype))
                .expect("typed array instance prototype"),
            "typed array prototype inherits %TypedArray%.prototype"
        );
        for target in [constructor, prototype] {
            define_builtin_data(
                vm,
                target,
                "BYTES_PER_ELEMENT",
                Value::number(family.kind.bytes() as f64),
                Attributes::FIXED,
            );
        }
    }
    typed::install(vm);
}

pub(crate) fn method(
    vm: &mut Vm,
    target: ObjectId,
    key: impl Into<PropertyKey>,
    intrinsic: IntrinsicKey,
) {
    let function = vm.alloc_intrinsic(intrinsic);
    define_builtin_data(vm, target, key, Value::object(function), Attributes::METHOD);
}

pub(crate) fn getter(
    vm: &mut Vm,
    target: ObjectId,
    key: impl Into<PropertyKey>,
    intrinsic: IntrinsicKey,
) {
    install_getter(vm, target, key.into(), intrinsic);
}

impl Vm {
    /// CreateByteDataBlock: zero-filled, with allocation failure as RangeError.
    fn allocate_block(&mut self, roots: &ActiveRoots<'_>, length: usize) -> Completion<Vec<u8>> {
        if length > MAX_BYTE_LENGTH {
            return Ok(Err(
                self.make_range_error(roots, "Invalid array buffer length")?
            ));
        }
        let mut bytes = Vec::new();
        if bytes.try_reserve_exact(length).is_err() {
            return Ok(Err(
                self.make_range_error(roots, "Array buffer allocation failed")?
            ));
        }
        bytes.extend(std::iter::repeat_n(0u8, length));
        Ok(Ok(bytes))
    }

    /// AllocateArrayBuffer for a buffer created by the engine itself: typed
    /// arrays and clones always use `%ArrayBuffer.prototype%`.
    pub(crate) fn allocate_array_buffer(
        &mut self,
        roots: &ActiveRoots<'_>,
        byte_length: usize,
        max_byte_length: Option<usize>,
    ) -> Completion<ObjectId> {
        self.allocate_array_buffer_with(roots, byte_length, max_byte_length, ARRAY_BUFFER_PROTOTYPE)
    }

    pub(crate) fn allocate_array_buffer_with(
        &mut self,
        roots: &ActiveRoots<'_>,
        byte_length: usize,
        max_byte_length: Option<usize>,
        prototype: &'static str,
    ) -> Completion<ObjectId> {
        if max_byte_length.is_some_and(|max| byte_length > max) {
            return Ok(Err(self.make_range_error(
                roots,
                "ArrayBuffer byte length exceeds maxByteLength",
            )?));
        }
        let bytes = match self.allocate_block(roots, byte_length)? {
            Ok(bytes) => bytes,
            Err(error) => return Ok(Err(error)),
        };
        let prototype = self.builtins.get(prototype);
        Ok(Ok(self.heap.alloc(Object::ArrayBuffer(
            ArrayBufferObject {
                object: PlainObject::new(Some(prototype)),
                bytes: Some(bytes),
                max_byte_length,
            },
        ))))
    }

    /// ToIntegerOrInfinity: truncate towards zero, with NaN read as zero.
    pub(crate) fn integer_or_infinity(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        value: &Value,
    ) -> Completion<f64> {
        let number = match self.coerce_number(host, roots, value)? {
            Ok(number) => number,
            Err(error) => return Ok(Err(error)),
        };
        Ok(Ok(if number.is_nan() {
            0.0
        } else if number.is_infinite() {
            number
        } else {
            number.trunc()
        }))
    }

    /// ToIndex: an integer index in `0 ..= 2^53 - 1`.
    pub(crate) fn coerce_index(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        value: &Value,
    ) -> Completion<usize> {
        let number = match self.integer_or_infinity(host, roots, value)? {
            Ok(number) => number,
            Err(error) => return Ok(Err(error)),
        };
        if number < 0.0 || number > MAX_BYTE_LENGTH as f64 {
            return Ok(Err(self.make_range_error(roots, "Invalid index")?));
        }
        Ok(Ok(number as usize))
    }

    /// ToAbsoluteIndex: a negative index counts from the end.
    pub(crate) fn absolute_index(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        value: &Value,
        length: usize,
    ) -> Completion<f64> {
        let index = match self.integer_or_infinity(host, roots, value)? {
            Ok(index) => index,
            Err(error) => return Ok(Err(error)),
        };
        Ok(Ok(if index.is_finite() && index < 0.0 {
            length as f64 + index
        } else {
            index
        }))
    }

    /// ToClampedIndex: clamp a relative index into `0 ..= length`.
    pub(crate) fn clamped_index(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        value: &Value,
        length: usize,
    ) -> Completion<usize> {
        let index = match self.absolute_index(host, roots, value, length)? {
            Ok(index) => index,
            Err(error) => return Ok(Err(error)),
        };
        Ok(Ok(index.clamp(0.0, length as f64) as usize))
    }

    /// ToBigInt: unlike the `BigInt` constructor, a Number is a TypeError.
    pub(crate) fn coerce_to_bigint(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        value: &Value,
    ) -> Completion<JsBigInt> {
        let primitive =
            match self.coerce_primitive(host, roots, value, crate::exec::Hint::Number)? {
                Ok(primitive) => primitive,
                Err(error) => return Ok(Err(error)),
            };
        Ok(match primitive.view() {
            ValueRef::Undefined => {
                Err(self.type_error(roots, "Cannot convert undefined to a BigInt")?)
            }
            ValueRef::Null => Err(self.type_error(roots, "Cannot convert null to a BigInt")?),
            ValueRef::Bool(value) => Ok(JsBigInt::from_i64(i64::from(value))),
            ValueRef::BigInt(value) => Ok(value.clone()),
            ValueRef::Number(_) => {
                Err(self.type_error(roots, "Cannot convert a Number to a BigInt element")?)
            }
            ValueRef::String(text) => {
                match text
                    .to_utf8()
                    .ok()
                    .and_then(|text| JsBigInt::from_js_string(&text))
                {
                    Some(value) => Ok(value),
                    None => Err(self.alloc_error(
                        roots,
                        self.builtins.get("syntaxerror.prototype"),
                        "SyntaxError",
                        Some("Cannot convert string to a BigInt".into()),
                    )?),
                }
            }
            ValueRef::Symbol(_) => {
                Err(self.type_error(roots, "Cannot convert a Symbol value to a BigInt")?)
            }
            ValueRef::Object(_) => unreachable!("ToPrimitive returned an object"),
        })
    }

    /// Convert a value into the raw bits one element stores.
    pub(crate) fn element_bits(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        kind: ElementKind,
        value: &Value,
    ) -> Completion<u64> {
        if kind.is_bigint() {
            let big = match self.coerce_to_bigint(host, roots, value)? {
                Ok(big) => big,
                Err(error) => return Ok(Err(error)),
            };
            return Ok(Ok(element::bigint_bits(&big)));
        }
        let number = match self.coerce_number(host, roots, value)? {
            Ok(number) => number,
            Err(error) => return Ok(Err(error)),
        };
        Ok(Ok(element::number_bits(kind, number)))
    }

    /// TypedArraySetElement: convert first, then recheck, then store.
    pub(crate) fn typed_set_element(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        index: u64,
        value: &Value,
    ) -> Completion<()> {
        let Some(kind) = self.heap.typed_array(id)?.map(|view| view.kind) else {
            return Err(VmError::InvalidState(
                "typed element store requires a TypedArray".to_owned(),
            ));
        };
        let bits = match self.element_bits(host, roots, kind, value)? {
            Ok(bits) => bits,
            Err(error) => return Ok(Err(error)),
        };
        self.heap.store_typed_element(id, index, bits)?;
        Ok(Ok(()))
    }

    /// RequireInternalSlot(obj, [[TypedArrayName]]).
    pub(crate) fn require_typed(
        &mut self,
        roots: &ActiveRoots<'_>,
        value: &Value,
    ) -> Completion<(ObjectId, ElementKind)> {
        let ValueRef::Object(id) = value.view() else {
            return Ok(Err(
                self.type_error(roots, "TypedArray method called on a non-object")?
            ));
        };
        match self.heap.typed_array(id)? {
            Some(view) => Ok(Ok((id, view.kind))),
            None => Ok(Err(self.type_error(
                roots,
                "TypedArray method called on an incompatible receiver",
            )?)),
        }
    }

    /// ValidateTypedArrayBounds: a detached or out-of-range view throws, so
    /// every method that touches elements starts here.
    pub(crate) fn validate_typed_bounds(
        &mut self,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
    ) -> Completion<usize> {
        let Some(view) = self.heap.typed_array(id)? else {
            return Ok(Err(self.type_error(
                roots,
                "TypedArray method called on an incompatible receiver",
            )?));
        };
        let range = self.heap.typed_range(view)?;
        if matches!(range, ViewRange::Detached | ViewRange::OutOfBounds) {
            return Ok(Err(
                self.type_error(roots, "TypedArray is detached or out of bounds")?
            ));
        }
        Ok(Ok(range.len() / view.kind.bytes()))
    }

    /// IsTypedArrayOutOfBounds: detached counts as out of bounds.
    pub(crate) fn typed_out_of_bounds(&self, id: ObjectId) -> Result<bool, VmError> {
        let Some(view) = self.heap.typed_array(id)? else {
            return Ok(false);
        };
        Ok(matches!(
            self.heap.typed_range(view)?,
            ViewRange::Detached | ViewRange::OutOfBounds
        ))
    }

    pub(crate) fn require_array_buffer(
        &mut self,
        roots: &ActiveRoots<'_>,
        value: &Value,
        what: &str,
    ) -> Completion<ObjectId> {
        let ValueRef::Object(id) = value.view() else {
            return Ok(Err(self.type_error(roots, what)?));
        };
        if self.heap.buffer(id)?.is_none() {
            return Ok(Err(self.type_error(roots, what)?));
        }
        Ok(Ok(id))
    }

    pub(crate) fn require_data_view(
        &mut self,
        roots: &ActiveRoots<'_>,
        value: &Value,
    ) -> Completion<ObjectId> {
        let ValueRef::Object(id) = value.view() else {
            return Ok(Err(
                self.type_error(roots, "DataView method called on a non-object")?
            ));
        };
        if self.heap.data_view(id)?.is_none() {
            return Ok(Err(self.type_error(
                roots,
                "DataView method called on an incompatible receiver",
            )?));
        }
        Ok(Ok(id))
    }

    /// `GetPrototypeFromConstructor`: a non-object prototype falls back to the
    /// intrinsic, after any getter on `newTarget` has run.
    pub(crate) fn prototype_from(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        new_target: &Value,
        default: &'static str,
    ) -> Completion<ObjectId> {
        let fallback = self.builtins.get(default);
        match self.read_property(host, roots, new_target, PropertyKeyRef::String("prototype"))? {
            Invocation::Value(value) => Ok(Ok(match value.view() {
                ValueRef::Object(id) => id,
                _ => fallback,
            })),
            Invocation::Thrown(error) => Ok(Err(error)),
        }
    }
}

/// The argument at `index`, or `undefined`.
pub(crate) fn arg(args: &[Value], index: usize) -> &Value {
    static UNDEFINED: Value = Value::UNDEFINED;
    args.get(index).unwrap_or(&UNDEFINED)
}

/// `IsView` for `ArrayBuffer.isView`: a proxy over a view is not a view.
pub(crate) fn is_view(heap: &crate::heap::Heap, value: &Value) -> bool {
    let ValueRef::Object(id) = value.view() else {
        return false;
    };
    matches!(
        heap.get(id),
        Ok(Object::TypedArray(_) | Object::DataView(_))
    )
}
