use super::{FUNCTION_PARENT, OBJECT_PARENT, define_builtin_data, link_constructor};
use crate::builtin::globals::Owner;
use crate::builtin::roots::{RootKind, RootSpec};
use crate::heap::{
    Attributes, HAS_INSTANCE_SYMBOL, ITERATOR_SYMBOL, MATCH_ALL_SYMBOL, MATCH_SYMBOL,
    REPLACE_SYMBOL, SEARCH_SYMBOL, SPECIES_SYMBOL, SPLIT_SYMBOL, TO_PRIMITIVE_SYMBOL,
};
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::value::Value;
use crate::vm::Vm;

pub(super) const CONSTRUCTOR: IntrinsicKey = IntrinsicKey::new(members::NAME, Slot::Global, "");
const PROTOTYPE: &str = "symbol.prototype";

#[yield_binding_macros::builtin_global(name = "Symbol", root = "symbol", kind = "constructor")]
mod members {
    use crate::vm::Vm;

    prototype_calls! {
        valueOf: 0 | toString: 0 as name => |vm, _, _, _, receiver| vm.symbol_method(receiver, name);
    }

    static_calls! {
        r#for: 1 => |vm, host, roots, args, receiver| vm.symbol_static(receiver, |vm, args| vm.symbol_for(host, roots, args), args);
        keyFor: 1 => |vm, _, _, args, receiver| vm.symbol_static(receiver, Vm::symbol_key_for, args);
    }

    prototype_calls! {
        get description: 0 => |vm, _, _, _, receiver| vm.symbol_method(receiver, "get description");
    }
}

pub(crate) const OWNER: Owner = Owner::new(
    members::ROOT,
    &[
        RootSpec::new(
            members::ROOT,
            RootKind::Native(CONSTRUCTOR),
            FUNCTION_PARENT,
        )
        .fixed(PROTOTYPE),
        RootSpec::new(PROTOTYPE, RootKind::Plain, OBJECT_PARENT),
    ],
    &[
        &[IntrinsicDef::call(
            CONSTRUCTOR,
            0,
            |vm, host, roots, args, _| vm.symbol_constructor(host, roots, args),
        )],
        members::INTRINSICS,
    ],
    Some(install),
);

fn install(vm: &mut Vm, _: &Value) {
    let constructor = vm.builtins.get(members::ROOT);
    let prototype = vm.builtins.get(PROTOTYPE);
    link_constructor(vm, constructor, prototype);
    members::install_members(vm, constructor, prototype);
    for (name, id) in [
        ("iterator", ITERATOR_SYMBOL),
        ("toPrimitive", TO_PRIMITIVE_SYMBOL),
        ("species", SPECIES_SYMBOL),
        ("match", MATCH_SYMBOL),
        ("matchAll", MATCH_ALL_SYMBOL),
        ("replace", REPLACE_SYMBOL),
        ("search", SEARCH_SYMBOL),
        ("split", SPLIT_SYMBOL),
        ("hasInstance", HAS_INSTANCE_SYMBOL),
    ] {
        define_builtin_data(vm, constructor, name, Value::symbol(id), Attributes::FIXED);
    }
}
