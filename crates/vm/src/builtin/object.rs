#[yield_binding_macros::builtin_global(name = "Object", root = "object", kind = "constructor")]
pub(crate) mod members {
    use crate::builtin::globals::Owner;
    use crate::builtin::roots::{RootKind, RootSpec};
    use crate::builtin::{FUNCTION_PARENT, link_constructor};
    use crate::heap::IntegrityLevel;
    use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
    use crate::ops;
    use crate::runtime::Invocation;
    use crate::value::{Value, ValueRef};
    use crate::vm::Vm;

    const CONSTRUCTOR: IntrinsicKey = IntrinsicKey::new(NAME, Slot::Global, "");
    pub(crate) const ROOTS: &[RootSpec] = &[
        RootSpec::new("object.prototype", RootKind::Plain, None).fixed_parent(),
        RootSpec::new(ROOT, RootKind::Native(CONSTRUCTOR), FUNCTION_PARENT)
            .fixed("object.prototype"),
    ];
    const GLOBAL: &[IntrinsicDef] = &[IntrinsicDef::constructor(
        CONSTRUCTOR,
        1,
        |vm, _, _, args, _| vm.construct_object(&args),
        |vm, _, _, args, prototype| match prototype {
            Some(prototype) => Ok(Invocation::Value(Value::object(
                vm.heap.alloc_plain_with(Some(prototype)),
            ))),
            None => vm.construct_object(&args),
        },
    )];

    pub(crate) const OWNER: Owner = Owner::new(ROOT, ROOTS, &[GLOBAL, INTRINSICS], Some(install));

    fn install(vm: &mut Vm, _: &Value) {
        let constructor = vm.builtins.get(ROOT);
        let prototype = vm.builtins.get("object.prototype");
        link_constructor(vm, constructor, prototype);
        install_members(vm, constructor, prototype);
    }

    static_calls! {
        assign: 2 => |vm, host, roots, args, _| vm.object_assign(host, roots, &args);
        is: 2 => |_, _, _, args, _| Ok(Invocation::Value(Value::boolean(ops::same_value(
            args.first().unwrap_or(&Value::UNDEFINED), args.get(1).unwrap_or(&Value::UNDEFINED),
        ))));
        keys: 1 => |vm, host, roots, args, _| vm.object_keys(host, roots, &args);
        values: 1 => |vm, host, roots, args, _| vm.object_values_or_entries(host, roots, &args, false);
        entries: 1 => |vm, host, roots, args, _| vm.object_values_or_entries(host, roots, &args, true);
        fromEntries: 1 => |vm, host, roots, args, _| vm.object_from_entries(host, roots, &args);
        create: 2 => |vm, host, roots, args, _| vm.object_create(host, roots, &args);
        getPrototypeOf: 1 => |vm, _, _, args, _| vm.get_prototype_of(&args, false);
        setPrototypeOf: 2 => |vm, _, _, args, _| vm.set_prototype_of(&args, false);
        getOwnPropertyDescriptor: 2 => |vm, host, roots, args, _| vm.get_own_property_descriptor(host, roots, &args);
        getOwnPropertyDescriptors: 1 => |vm, host, roots, args, _| vm.get_own_property_descriptors(host, roots, &args);
        defineProperty: 3 => |vm, host, roots, args, _| vm.define_property_builtin(host, roots, &args, false);
        defineProperties: 2 => |vm, host, roots, args, _| {
            let Some(ValueRef::Object(target)) = args.first().map(Value::view) else {
                return vm.throw_type_error("Object.defineProperties requires an object target");
            };
            vm.define_properties(host, roots, target, args.get(1))
        };
        preventExtensions: 1 => |vm, _, _, args, _| vm.prevent_extensions_builtin_impl(&args, false);
        isExtensible: 1 => |vm, _, _, args, _| vm.is_extensible_builtin_impl(&args, false);
        seal: 1 => |vm, host, roots, args, _| vm.set_integrity(host, roots, &args, IntegrityLevel::Sealed);
        isSealed: 1 => |vm, host, roots, args, _| vm.test_integrity(host, roots, &args, IntegrityLevel::Sealed);
        freeze: 1 => |vm, host, roots, args, _| vm.set_integrity(host, roots, &args, IntegrityLevel::Frozen);
        isFrozen: 1 => |vm, host, roots, args, _| vm.test_integrity(host, roots, &args, IntegrityLevel::Frozen);
    }

    prototype_calls! {
        hasOwnProperty: 1 => |vm, host, roots, args, receiver| {
            let id = match receiver.view() {
                ValueRef::Object(id) if vm.heap.is_environment(id) == Some(false) => id,
                _ => return vm.throw_type_error("incompatible Object.prototype receiver"),
            };
            let key = match vm.coerce_property_key(host, roots, args.first().unwrap_or(&Value::UNDEFINED))? {
                Ok(key) => key,
                Err(thrown) => return Ok(Invocation::Thrown(thrown)),
            };
            match vm.own_descriptor(host, roots, id, key.borrowed())? {
                Ok(descriptor) => Ok(Invocation::Value(Value::boolean(descriptor.is_some()))),
                Err(error) => Ok(Invocation::Thrown(error)),
            }
        };
        isPrototypeOf: 1 => |vm, _, _, args, receiver| {
            let result = |value| Ok(Invocation::Value(Value::boolean(value)));
            let Some(ValueRef::Object(target)) = args.first().map(Value::view) else {
                return result(false);
            };
            let receiver = match receiver.view() {
                ValueRef::Object(id) => id,
                ValueRef::Null | ValueRef::Undefined => {
                    return vm.throw_type_error("incompatible Object.prototype.isPrototypeOf receiver");
                }
                _ => return result(false),
            };
            let mut prototype = vm.heap.prototype_of(target)?;
            while let Some(id) = prototype {
                if id == receiver {
                    return result(true);
                }
                prototype = vm.heap.prototype_of(id)?;
            }
            result(false)
        };
        propertyIsEnumerable: 1 => |vm, host, roots, args, receiver| {
            let roots = &roots.with(receiver);
            let key = match vm.coerce_property_key(host, roots, args.first().unwrap_or(&Value::UNDEFINED))? {
                Ok(key) => key,
                Err(thrown) => return Ok(Invocation::Thrown(thrown)),
            };
            let id = match receiver.view() {
                ValueRef::Object(id) => id,
                ValueRef::String(text) if !text.is_empty() => {
                    let Invocation::Value(value) = vm.construct_object(std::slice::from_ref(receiver))? else {
                        unreachable!("Object(string) returns a boxed string");
                    };
                    let ValueRef::Object(id) = value.view() else {
                        unreachable!("Object(string) returns a boxed string");
                    };
                    id
                }
                ValueRef::Null | ValueRef::Undefined => {
                    return vm.throw_type_error("incompatible Object.prototype.propertyIsEnumerable receiver");
                }
                _ => return Ok(Invocation::Value(Value::boolean(false))),
            };
            match vm.own_descriptor(host, roots, id, key.borrowed())? {
                Ok(descriptor) => Ok(Invocation::Value(Value::boolean(
                    descriptor.is_some_and(|(_, attributes)| attributes.enumerable),
                ))),
                Err(error) => Ok(Invocation::Thrown(error)),
            }
        };
        toLocaleString: 0 => |vm, host, roots, _, receiver| {
            let roots = &roots.with(receiver);
            let method = match vm.read_member(host, roots, receiver, &Value::string("toString".into()))? {
                Invocation::Value(method) => method,
                thrown @ Invocation::Thrown(_) => return Ok(thrown),
            };
            vm.invoke(host, roots, &method, Vec::new(), receiver)
        };
        valueOf: 0 => |vm, _, _, _, receiver| match receiver.view() {
            ValueRef::Object(id) if vm.heap.is_environment(id) == Some(false) => {
                Ok(Invocation::Value(Value::object(id)))
            }
            _ => vm.throw_type_error("incompatible Object.prototype receiver"),
        };
        toString: 0 => |vm, _, _, _, receiver| match receiver.view() {
            ValueRef::Object(id) if vm.heap.is_environment(id) == Some(true) => {
                vm.throw_type_error("incompatible Object.prototype receiver")
            }
            ValueRef::Object(id) => Ok(Invocation::Value(Value::string(
                format!("[object {}]", vm.object_tag(id)?).into(),
            ))),
            _ => Ok(Invocation::Value(Value::string("[object Undefined]".into()))),
        };
    }
}
