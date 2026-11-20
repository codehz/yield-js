use super::{
    FUNCTION_PARENT, OBJECT_PARENT, define_builtin_data, install_getter, link_constructor,
};
use crate::builtin::globals::Owner;
use crate::builtin::roots::{RootKind, RootSpec};
use crate::heap::{
    Attributes, ITERATOR_SYMBOL, MATCH_ALL_SYMBOL, MATCH_SYMBOL, REPLACE_SYMBOL, SEARCH_SYMBOL,
    SPECIES_SYMBOL, SPLIT_SYMBOL, TO_PRIMITIVE_SYMBOL,
};
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::value::{PropertyKey, Value};
use crate::vm::Vm;

pub(super) const CONSTRUCTOR: IntrinsicKey = IntrinsicKey::new("Symbol", Slot::Global, "");
const DESCRIPTION: IntrinsicKey = IntrinsicKey::new("Symbol", Slot::PrototypeGetter, "description");
const ROOT: &str = "symbol";
const PROTOTYPE: &str = "symbol.prototype";

macro_rules! symbol_methods {
    ($($slot:ident $name:literal => $call:expr),* $(,)?) => {
        const METHODS: &[IntrinsicDef] = &[$(IntrinsicDef::call(IntrinsicKey::new("Symbol", Slot::$slot, $name), $call)),*];
    };
}

symbol_methods! {
    Prototype "valueOf" => |vm, _, _, receiver| vm.symbol_method(receiver, "valueOf"),
    Prototype "toString" => |vm, _, _, receiver| vm.symbol_method(receiver, "toString"),
    Static "for" => |vm, host, args, receiver| vm.symbol_static(receiver, |vm, args| vm.symbol_for(host, args), args),
    Static "keyFor" => |vm, _, args, receiver| vm.symbol_static(receiver, Vm::symbol_key_for, args),
}

pub(crate) const OWNER: Owner = Owner::new(
    ROOT,
    &[
        RootSpec::new(ROOT, RootKind::Native(CONSTRUCTOR), FUNCTION_PARENT).fixed(PROTOTYPE),
        RootSpec::new(PROTOTYPE, RootKind::Plain, OBJECT_PARENT),
    ],
    &[
        &[
            IntrinsicDef::call(CONSTRUCTOR, |vm, host, args, _| {
                vm.symbol_constructor(host, args)
            }),
            IntrinsicDef::call(DESCRIPTION, |vm, _, _, receiver| {
                vm.symbol_method(receiver, "get description")
            }),
        ],
        METHODS,
    ],
    Some(install),
);

fn install(vm: &mut Vm, _: &Value) {
    let constructor = vm.builtins.get(ROOT);
    let prototype = vm.builtins.get(PROTOTYPE);
    link_constructor(vm, constructor, prototype);
    for def in METHODS {
        let target = if def.key.slot == Slot::Static {
            constructor
        } else {
            prototype
        };
        let method = vm.alloc_intrinsic(def.key);
        define_builtin_data(
            vm,
            target,
            def.key.member,
            Value::Object(method),
            Attributes::METHOD,
        );
    }
    install_getter(
        vm,
        prototype,
        PropertyKey::String("description".into()),
        DESCRIPTION,
    );
    for (name, id) in [
        ("iterator", ITERATOR_SYMBOL),
        ("toPrimitive", TO_PRIMITIVE_SYMBOL),
        ("species", SPECIES_SYMBOL),
        ("match", MATCH_SYMBOL),
        ("matchAll", MATCH_ALL_SYMBOL),
        ("replace", REPLACE_SYMBOL),
        ("search", SEARCH_SYMBOL),
        ("split", SPLIT_SYMBOL),
    ] {
        define_builtin_data(vm, constructor, name, Value::Symbol(id), Attributes::FIXED);
    }
}
