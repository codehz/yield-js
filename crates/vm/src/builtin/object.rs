#[yield_binding_macros::builtin_global(name = "Object", root = "object", kind = "constructor")]
pub(crate) mod members {
    use crate::builtin::globals::Owner;
    use crate::builtin::roots::{RootKind, RootSpec};
    use crate::builtin::{FUNCTION_PARENT, link_constructor};
    use crate::error::VmError;
    use crate::gc::ActiveRoots;
    use crate::heap::{IntegrityLevel, string_own_property};
    use crate::host::Host;
    use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
    use crate::ops;
    use crate::runtime::{Arguments, Invocation};
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
        getOwnPropertyNames: 1 => |vm, host, roots, args, _| vm.object_own_property_keys(host, roots, &args, false);
        getOwnPropertySymbols: 1 => |vm, host, roots, args, _| vm.object_own_property_keys(host, roots, &args, true);
        values: 1 => |vm, host, roots, args, _| vm.object_values_or_entries(host, roots, &args, false);
        entries: 1 => |vm, host, roots, args, _| vm.object_values_or_entries(host, roots, &args, true);
        fromEntries: 1 => |vm, host, roots, args, _| vm.object_from_entries(host, roots, &args);
        create: 2 => |vm, host, roots, args, _| vm.object_create(host, roots, &args);
        getPrototypeOf: 1 => |vm, _, roots, args, _| vm.get_prototype_of(roots, &args, false);
        setPrototypeOf: 2 => |vm, _, roots, args, _| vm.set_prototype_of(roots, &args, false);
        getOwnPropertyDescriptor: 2 => |vm, host, roots, args, _| vm.get_own_property_descriptor(host, roots, &args, false);
        getOwnPropertyDescriptors: 1 => |vm, host, roots, args, _| vm.get_own_property_descriptors(host, roots, &args);
        defineProperty: 3 => |vm, host, roots, args, _| vm.define_property_builtin(host, roots, &args, false);
        defineProperties: 2 => |vm, host, roots, args, _| {
            let Some(ValueRef::Object(target)) = args.first().map(Value::view) else {
                return vm.throw_type_error(roots, "Object.defineProperties requires an object target");
            };
            vm.define_properties(host, roots, target, args.get(1))
        };
        preventExtensions: 1 => |vm, _, roots, args, _| vm.prevent_extensions_builtin_impl(roots, &args, false);
        isExtensible: 1 => |vm, _, roots, args, _| vm.is_extensible_builtin_impl(roots, &args, false);
        seal: 1 => |vm, host, roots, args, _| vm.set_integrity(host, roots, &args, IntegrityLevel::Sealed);
        isSealed: 1 => |vm, host, roots, args, _| vm.test_integrity(host, roots, &args, IntegrityLevel::Sealed);
        freeze: 1 => |vm, host, roots, args, _| vm.set_integrity(host, roots, &args, IntegrityLevel::Frozen);
        isFrozen: 1 => |vm, host, roots, args, _| vm.test_integrity(host, roots, &args, IntegrityLevel::Frozen);
    }

    prototype_calls! {
        hasOwnProperty: 1 => |vm, host, roots, args, receiver| test_own_property(vm, host, roots, &args, receiver, false);
        isPrototypeOf: 1 => |vm, _, roots, args, receiver| {
            let result = |value| Ok(Invocation::Value(Value::boolean(value)));
            let Some(ValueRef::Object(target)) = args.first().map(Value::view) else {
                return result(false);
            };
            let receiver = match receiver.view() {
                ValueRef::Object(id) => id,
                ValueRef::Null | ValueRef::Undefined => {
                    return vm.throw_type_error(roots, "incompatible Object.prototype.isPrototypeOf receiver");
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
        propertyIsEnumerable: 1 => |vm, host, roots, args, receiver| test_own_property(vm, host, roots, &args, receiver, true);
        toLocaleString: 0 => |vm, host, roots, _, receiver| {
            let roots = &roots.with(receiver);
            let method = match vm.read_member(host, roots, receiver, &Value::string("toString".into()))? {
                Invocation::Value(method) => method,
                thrown @ Invocation::Thrown(_) => return Ok(thrown),
            };
            vm.invoke(host, roots, &method, Arguments::EMPTY, receiver)
        };
        valueOf: 0 => |vm, _, roots, _, receiver| match receiver.view() {
            ValueRef::Null | ValueRef::Undefined => vm.throw_type_error(roots, "incompatible Object.prototype.valueOf receiver"),
            ValueRef::Object(id) if vm.heap.is_environment(id) != Some(false) => vm.throw_type_error(roots, "incompatible Object.prototype.valueOf receiver"),
            _ => Ok(Invocation::Value(Value::object(vm.coercible_object(receiver)))),
        };
        toString: 0 => |vm, host, roots, _, receiver| vm.object_to_string(host, roots, receiver);
    }

    fn test_own_property(
        vm: &mut Vm,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        receiver: &Value,
        enumerable_only: bool,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(receiver);
        // Both methods convert the key before coercing the receiver.
        let key =
            match vm.coerce_property_key(host, roots, args.first().unwrap_or(&Value::UNDEFINED))? {
                Ok(key) => key,
                Err(thrown) => return Ok(Invocation::Thrown(thrown)),
            };
        let attributes = match receiver.view() {
            ValueRef::Object(id) if vm.heap.is_environment(id) == Some(false) => {
                match vm.own_descriptor(host, roots, id, key.borrowed())? {
                    Ok(descriptor) => descriptor.map(|(_, attributes)| attributes),
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
            }
            ValueRef::String(text) => {
                string_own_property(text, key.borrowed()).map(|property| property.attributes())
            }
            ValueRef::Null | ValueRef::Undefined | ValueRef::Object(_) => {
                return vm.throw_type_error(roots, "incompatible Object.prototype receiver");
            }
            _ => None,
        };
        Ok(Invocation::Value(Value::boolean(attributes.is_some_and(
            |attributes| !enumerable_only || attributes.enumerable,
        ))))
    }
}
