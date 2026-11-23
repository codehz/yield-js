//! The strategy size functions are shared per realm, not per instance.

use super::{dictionary_member, install};
use crate::VmError;
use crate::builtin::event::new_target_prototype;
use crate::builtin::globals::Owner;
use crate::builtin::roots::{RootKind, RootSpec};
use crate::builtin::{FUNCTION_PARENT, OBJECT_PARENT};
use crate::gc::ActiveRoots;
use crate::heap::{Attributes, Object, PlainObject};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::Invocation;
use crate::value::{PropertyKeyRef, Value, ValueRef};
use crate::vm::Vm;

macro_rules! strategy {
    ($owner:ident, $root:literal, $name:literal, $byte_length:literal, $length:literal, $size:expr) => {
        pub(in crate::builtin) const $owner: Owner = {
            const PROTOTYPE: &str = concat!($root, ".prototype");
            const SHARED_SIZE: &str = concat!($root, ".size");
            const CONSTRUCTOR: IntrinsicKey = IntrinsicKey::new($name, Slot::Global, "");
            const SIZE: IntrinsicKey = IntrinsicKey::new($name, Slot::Prototype, "size");
            const MEMBERS: &[IntrinsicDef] = &[
                IntrinsicDef::constructor_with_target(
                    CONSTRUCTOR,
                    1,
                    |vm, _, roots, _, _| {
                        vm.throw_type_error(roots, concat!($name, " requires new"))
                    },
                    |vm, host, roots, args, target| {
                        construct(vm, host, roots, args, target, PROTOTYPE, $byte_length)
                    },
                ),
                IntrinsicDef::call(
                    IntrinsicKey::new($name, Slot::PrototypeGetter, "highWaterMark"),
                    0,
                    |vm, _, roots, _, receiver| get(vm, roots, receiver, $byte_length, None),
                ),
                IntrinsicDef::call(
                    IntrinsicKey::new($name, Slot::PrototypeGetter, "size"),
                    0,
                    |vm, _, roots, _, receiver| {
                        get(vm, roots, receiver, $byte_length, Some(SHARED_SIZE))
                    },
                ),
            ];
            Owner::new(
                $root,
                &[
                    RootSpec::new($root, RootKind::Native(CONSTRUCTOR), FUNCTION_PARENT)
                        .fixed(PROTOTYPE)
                        .backlink(Some(Attributes::METHOD)),
                    RootSpec::new(PROTOTYPE, RootKind::Plain, OBJECT_PARENT),
                    RootSpec::new(SHARED_SIZE, RootKind::Native(SIZE), FUNCTION_PARENT),
                ],
                &[MEMBERS, &[IntrinsicDef::call(SIZE, $length, $size)]],
                Some(|vm, _| install(vm, $name, $root, PROTOTYPE, MEMBERS)),
            )
        };
    };
}

strategy!(
    COUNT_OWNER,
    "countQueuingStrategy",
    "CountQueuingStrategy",
    false,
    0,
    |_, _, _, _, _| Ok(Invocation::Value(Value::number(1.0)))
);
strategy!(
    BYTE_LENGTH_OWNER,
    "byteLengthQueuingStrategy",
    "ByteLengthQueuingStrategy",
    true,
    1,
    |vm, host, roots, args, _| vm.read_property(
        host,
        roots,
        arg(&args),
        PropertyKeyRef::String("byteLength")
    )
);

fn arg(args: &[Value]) -> &Value {
    static UNDEFINED: Value = Value::UNDEFINED;
    args.first().unwrap_or(&UNDEFINED)
}

fn construct(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    target: &Value,
    fallback: &'static str,
    byte_length: bool,
) -> Result<Invocation, VmError> {
    let roots = roots.with(&args);
    let roots = roots.with(target);
    let value = match dictionary_member(vm, host, &roots, arg(&args), "highWaterMark")? {
        Invocation::Value(value) if matches!(value.view(), ValueRef::Undefined) => {
            return vm.throw_type_error(&roots, "highWaterMark is required");
        }
        Invocation::Value(value) => value,
        thrown => return Ok(thrown),
    };
    let mark = match vm.coerce_number(host, &roots.with(&value), &value)? {
        Ok(value) => value,
        Err(error) => return Ok(Invocation::Thrown(error)),
    };
    let prototype = match new_target_prototype(vm, host, &roots, target, fallback)? {
        Ok(prototype) => prototype,
        Err(error) => return Ok(Invocation::Thrown(error)),
    };
    Ok(Invocation::Value(Value::object(vm.heap.alloc(
        Object::QueuingStrategy {
            object: PlainObject::new(Some(prototype)),
            high_water_mark: Value::number(mark),
            byte_length,
        },
    ))))
}

fn get(
    vm: &mut Vm,
    roots: &ActiveRoots<'_>,
    receiver: &Value,
    expected: bool,
    size: Option<&'static str>,
) -> Result<Invocation, VmError> {
    if let ValueRef::Object(id) = receiver.view()
        && let Object::QueuingStrategy {
            high_water_mark,
            byte_length,
            ..
        } = vm.heap.get(id)?
        && *byte_length == expected
    {
        return Ok(Invocation::Value(match size {
            Some(root) => Value::object(vm.builtins.get(root)),
            None => high_water_mark.clone(),
        }));
    }
    vm.throw_type_error(roots, "Incompatible queuing strategy receiver")
}
