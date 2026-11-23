//! Web IDL installation and argument conversion shared by the stream interfaces.

mod strategy;

pub(super) use strategy::{BYTE_LENGTH_OWNER, COUNT_OWNER};

use crate::VmError;
use crate::builtin::{define_builtin_data, link_constructor};
use crate::gc::ActiveRoots;
use crate::heap::{Attributes, TO_STRING_TAG_SYMBOL};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, Slot};
use crate::runtime::Invocation;
use crate::value::{PropertyKey, PropertyKeyRef, Value};
use crate::vm::Vm;

fn install(
    vm: &mut Vm,
    name: &str,
    root: &'static str,
    prototype: &'static str,
    members: &[IntrinsicDef],
) {
    let constructor = vm.builtins.get(root);
    let prototype = vm.builtins.get(prototype);
    link_constructor(vm, constructor, prototype);
    for def in members {
        let key = def.key;
        match key.slot {
            Slot::Static | Slot::Prototype => {
                let function = vm.alloc_intrinsic(key);
                let target = if key.slot == Slot::Static {
                    constructor
                } else {
                    prototype
                };
                define_builtin_data(
                    vm,
                    target,
                    key.member,
                    Value::object(function),
                    Attributes::ORDINARY,
                );
            }
            Slot::PrototypeGetter => {
                let getter = vm.alloc_intrinsic(key);
                super::event::install_accessor(vm, prototype, key.member, getter, None, true)
                    .expect("stream interface attribute");
            }
            _ => {}
        }
    }
    define_builtin_data(
        vm,
        prototype,
        PropertyKey::Symbol(TO_STRING_TAG_SYMBOL),
        Value::string(name.into()),
        Attributes::FUNCTION_METADATA,
    );
}

fn dictionary_member(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    dictionary: &Value,
    member: &str,
) -> Result<Invocation, VmError> {
    use crate::value::ValueRef;
    match dictionary.view() {
        ValueRef::Null | ValueRef::Undefined => Ok(Invocation::Value(Value::UNDEFINED)),
        ValueRef::Object(_) => {
            vm.read_property(host, roots, dictionary, PropertyKeyRef::String(member))
        }
        _ => vm.throw_type_error(roots, "Options must be an object"),
    }
}
