//! The VM-relative clock and its branded EventTarget singleton.

use super::event::{TARGET_PROTOTYPE, TARGET_ROOT, install_accessor};
use super::{define_builtin_data, link_constructor};
use crate::VmError;
use crate::builtin::globals::Owner;
use crate::builtin::roots::{RootKind, RootSpec};
use crate::gc::ActiveRoots;
use crate::heap::{Attributes, DescriptorUpdate, Object, TO_STRING_TAG_SYMBOL};
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::Invocation;
use crate::value::{PropertyKey, Value, ValueRef};
use crate::vm::VmCore;

const ROOT: &str = "Performance";
const PROTOTYPE: &str = "performance.prototype";
pub(super) const INSTANCE: &str = "performance";
const CONSTRUCTOR: IntrinsicKey = IntrinsicKey::new(ROOT, Slot::Global, "");
const NOW: IntrinsicKey = IntrinsicKey::new(ROOT, Slot::Prototype, "now");
const ORIGIN: IntrinsicKey = IntrinsicKey::new(ROOT, Slot::PrototypeGetter, "timeOrigin");
const JSON: IntrinsicKey = IntrinsicKey::new(ROOT, Slot::Prototype, "toJSON");
const GET_GLOBAL: IntrinsicKey = IntrinsicKey::new("globalThis", Slot::StaticGetter, INSTANCE);
const SET_GLOBAL: IntrinsicKey = IntrinsicKey::new("globalThis", Slot::StaticSetter, INSTANCE);

const MEMBERS: &[IntrinsicDef] = &[
    IntrinsicDef::call(GET_GLOBAL, 0, |vm, _, _, _, _| {
        Ok(Invocation::Value(Value::object(vm.builtins.get(INSTANCE))))
    }),
    IntrinsicDef::call(SET_GLOBAL, 1, |vm, _, roots, args, receiver| {
        let ValueRef::Object(receiver) = receiver.view() else {
            return vm.throw_type_error(roots, "Incompatible performance receiver");
        };
        let value = args.into_iter().next().unwrap_or(Value::UNDEFINED);
        if !vm.heap.apply_descriptor(
            receiver,
            INSTANCE.into(),
            DescriptorUpdate {
                value: Some(value),
                writable: Some(true),
                enumerable: Some(true),
                configurable: Some(true),
                ..Default::default()
            },
        )? {
            return vm.throw_type_error(roots, "Cannot replace performance");
        }
        Ok(Invocation::Value(Value::UNDEFINED))
    }),
    IntrinsicDef::constructor_with_target(
        CONSTRUCTOR,
        0,
        |vm, _, roots, _, _| vm.throw_type_error(roots, "Illegal Performance constructor"),
        |vm, _, roots, _, _| vm.throw_type_error(roots, "Illegal Performance constructor"),
    ),
    IntrinsicDef::call(NOW, 0, |vm, _, roots, _, receiver| {
        read(vm, roots, receiver, |vm| Value::number(vm.clock.now()))
    }),
    IntrinsicDef::call(ORIGIN, 0, |vm, _, roots, _, receiver| {
        read(vm, roots, receiver, |_| Value::number(0.0))
    }),
    IntrinsicDef::call(JSON, 0, |vm, _, roots, _, receiver| {
        read(vm, roots, receiver, |vm| {
            let result = vm.heap.alloc_plain();
            define_builtin_data(
                vm,
                result,
                "timeOrigin",
                Value::number(0.0),
                Attributes::ORDINARY,
            );
            Value::object(result)
        })
    }),
];

pub(super) const OWNER: Owner = Owner::new(
    ROOT,
    &[
        RootSpec::new(ROOT, RootKind::Native(CONSTRUCTOR), Some(TARGET_ROOT))
            .fixed(PROTOTYPE)
            .backlink(Some(Attributes::METHOD)),
        RootSpec::new(PROTOTYPE, RootKind::Plain, Some(TARGET_PROTOTYPE)),
        RootSpec::new(INSTANCE, RootKind::Performance, Some(PROTOTYPE)),
    ],
    &[MEMBERS],
    Some(install),
);

fn install(vm: &mut VmCore, _: &Value) {
    let global = vm.builtins.get("globalThis");
    let get = vm.alloc_intrinsic(GET_GLOBAL);
    let set = vm.alloc_intrinsic(SET_GLOBAL);
    install_accessor(vm, global, INSTANCE, get, Some(set), true)
        .expect("performance global accessor");
    let prototype = vm.builtins.get(PROTOTYPE);
    for key in [NOW, JSON] {
        let method = vm.alloc_intrinsic(key);
        define_builtin_data(
            vm,
            prototype,
            key.member,
            Value::object(method),
            Attributes::ORDINARY,
        );
    }
    let getter = vm.alloc_intrinsic(ORIGIN);
    install_accessor(vm, prototype, ORIGIN.member, getter, None, true)
        .expect("Performance prototype accessor");
    define_builtin_data(
        vm,
        prototype,
        PropertyKey::Symbol(TO_STRING_TAG_SYMBOL),
        Value::string(ROOT.into()),
        Attributes::FUNCTION_METADATA,
    );
    link_constructor(vm, vm.builtins.get(ROOT), prototype);
}

fn read(
    vm: &mut VmCore,
    roots: &ActiveRoots<'_>,
    receiver: &Value,
    get: impl FnOnce(&mut VmCore) -> Value,
) -> Result<Invocation, VmError> {
    if !matches!(receiver.view(), crate::value::ValueRef::Object(id) if matches!(vm.heap.get(id), Ok(Object::Performance { .. })))
    {
        return vm.throw_type_error(roots, "Incompatible Performance receiver");
    }
    Ok(Invocation::Value(get(vm)))
}
