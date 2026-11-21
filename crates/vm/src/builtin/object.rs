#[yield_binding_macros::builtin_global(name = "Object", root = "object", kind = "constructor")]
pub(crate) mod members {
    use crate::builtin::roots::{RootKind, RootSpec};
    use crate::builtin::{FUNCTION_PARENT, link_constructor};
    use crate::heap::IntegrityLevel;
    use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
    use crate::ops;
    use crate::runtime::Invocation;
    use crate::value::Value;
    use crate::vm::Vm;

    const CONSTRUCTOR: IntrinsicKey = IntrinsicKey::new(NAME, Slot::Global, "");
    pub(crate) const ROOTS: &[RootSpec] = &[
        RootSpec::new("object.prototype", RootKind::Plain, None).fixed_parent(),
        RootSpec::new(ROOT, RootKind::Native(CONSTRUCTOR), FUNCTION_PARENT)
            .fixed("object.prototype"),
    ];
    pub(crate) const GLOBAL: &[IntrinsicDef] = &[IntrinsicDef::constructor(
        CONSTRUCTOR,
        |vm, _, _, args, _| vm.construct_object(&args),
        |vm, _, _, args, prototype| match prototype {
            Some(prototype) => Ok(Invocation::Value(Value::Object(
                vm.heap.alloc_plain_with(Some(prototype)),
            ))),
            None => vm.construct_object(&args),
        },
    )];

    pub(crate) fn install(vm: &mut Vm, _: &Value) {
        let constructor = vm.builtins.get(ROOT);
        let prototype = vm.builtins.get("object.prototype");
        link_constructor(vm, constructor, prototype);
        install_members(vm, constructor, prototype);
    }

    static_calls! {
        assign => |vm, host, roots, args, _| vm.object_assign(host, roots, &args);
        is => |_, _, _, args, _| Ok(Invocation::Value(Value::Bool(ops::same_value(
            args.first().unwrap_or(&Value::Undefined), args.get(1).unwrap_or(&Value::Undefined),
        ))));
        keys => |vm, host, roots, args, _| vm.object_keys(host, roots, &args);
        create => |vm, host, roots, args, _| vm.object_create(host, roots, &args);
        getPrototypeOf => |vm, _, _, args, _| vm.get_prototype_of(&args, false);
        setPrototypeOf => |vm, _, _, args, _| vm.set_prototype_of(&args, false);
        getOwnPropertyDescriptor => |vm, host, roots, args, _| vm.get_own_property_descriptor(host, roots, &args);
        getOwnPropertyDescriptors => |vm, host, roots, args, _| vm.get_own_property_descriptors(host, roots, &args);
        defineProperty => |vm, host, roots, args, _| vm.define_property_builtin(host, roots, &args, false);
        defineProperties => |vm, host, roots, args, _| {
            let Some(Value::Object(target)) = args.first() else {
                return vm.throw_type_error("Object.defineProperties requires an object target");
            };
            vm.define_properties(host, roots, *target, args.get(1))
        };
        preventExtensions => |vm, _, _, args, _| vm.prevent_extensions_builtin_impl(&args, false);
        isExtensible => |vm, _, _, args, _| vm.is_extensible_builtin_impl(&args, false);
        seal => |vm, host, roots, args, _| vm.set_integrity(host, roots, &args, IntegrityLevel::Sealed);
        isSealed => |vm, host, roots, args, _| vm.test_integrity(host, roots, &args, IntegrityLevel::Sealed);
        freeze => |vm, host, roots, args, _| vm.set_integrity(host, roots, &args, IntegrityLevel::Frozen);
        isFrozen => |vm, host, roots, args, _| vm.test_integrity(host, roots, &args, IntegrityLevel::Frozen);
    }

    prototype_calls! {
        hasOwnProperty => |vm, host, roots, args, receiver| {
            let id = match *receiver {
                Value::Object(id) if vm.heap.is_environment(id) == Some(false) => id,
                _ => return vm.throw_type_error("incompatible Object.prototype receiver"),
            };
            let key = match vm.coerce_property_key(host, roots, args.first().unwrap_or(&Value::Undefined))? {
                Ok(key) => key,
                Err(thrown) => return Ok(Invocation::Thrown(thrown)),
            };
            match vm.own_descriptor(host, roots, id, key.borrowed())? {
                Ok(descriptor) => Ok(Invocation::Value(Value::Bool(descriptor.is_some()))),
                Err(error) => Ok(Invocation::Thrown(error)),
            }
        };
        valueOf => |vm, _, _, _, receiver| match *receiver {
            Value::Object(id) if vm.heap.is_environment(id) == Some(false) => {
                Ok(Invocation::Value(Value::Object(id)))
            }
            _ => vm.throw_type_error("incompatible Object.prototype receiver"),
        };
        toString => |vm, _, _, _, receiver| match receiver {
            Value::Object(id) if vm.heap.is_environment(*id) == Some(true) => {
                vm.throw_type_error("incompatible Object.prototype receiver")
            }
            Value::Object(id) => Ok(Invocation::Value(Value::String(
                format!("[object {}]", vm.object_tag(*id)?).into(),
            ))),
            _ => Ok(Invocation::Value(Value::String("[object Undefined]".into()))),
        };
    }
}
