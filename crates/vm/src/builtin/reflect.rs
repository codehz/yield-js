#[yield_binding_macros::builtin_global(name = "Reflect", root = "reflect")]
pub(crate) mod namespace {
    use crate::error::VmError;
    use crate::gc::ActiveRoots;
    use crate::host::Host;
    use crate::runtime::Invocation;
    use crate::value::{PropertyKey, Value, ValueRef};
    use crate::vm::Vm;

    #[function(name = "ownKeys", length = 1)]
    fn own_keys(
        vm: &mut Vm,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let Some(ValueRef::Object(id)) = args.first().map(Value::view) else {
            return vm.throw_type_error(roots, "Reflect.ownKeys requires an object");
        };
        let keys = match vm.own_keys(host, roots, id, false)? {
            Ok(keys) => keys,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let keys = keys
            .into_iter()
            .map(|key| match key {
                PropertyKey::String(key) => Value::string(key),
                PropertyKey::Symbol(id) => Value::symbol(id),
            })
            .collect();
        Ok(Invocation::Value(Value::object(vm.heap.alloc_array(keys))))
    }

    #[function(name = "deleteProperty", length = 2)]
    fn delete_property(
        vm: &mut Vm,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let Some(target) = args
            .first()
            .filter(|value| matches!(value.view(), ValueRef::Object(_)))
        else {
            return vm.throw_type_error(roots, "Reflect.deleteProperty requires an object");
        };
        vm.delete_property(
            host,
            roots,
            target,
            args.get(1).unwrap_or(&Value::UNDEFINED),
        )
    }

    #[function(name = "defineProperty", length = 3)]
    fn define_property(
        vm: &mut Vm,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        vm.define_property_builtin(host, roots, args, true)
    }

    #[function(name = "preventExtensions", length = 1)]
    fn prevent_extensions(
        vm: &mut Vm,
        _host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        vm.prevent_extensions_builtin_impl(roots, args, true)
    }

    #[function(name = "isExtensible", length = 1)]
    fn is_extensible(
        vm: &mut Vm,
        _host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        vm.is_extensible_builtin_impl(roots, args, true)
    }

    #[function(name = "getPrototypeOf", length = 1)]
    fn get_prototype_of(
        vm: &mut Vm,
        _host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        vm.get_prototype_of(roots, args, true)
    }

    #[function(name = "setPrototypeOf", length = 2)]
    fn set_prototype_of(
        vm: &mut Vm,
        _host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        vm.set_prototype_of(roots, args, true)
    }

    #[function(name = "get", length = 2)]
    fn get(
        vm: &mut Vm,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let Some(ValueRef::Object(target)) = args.first().map(Value::view) else {
            return vm.throw_type_error(roots, "Reflect.get requires an object");
        };
        let key =
            match vm.coerce_property_key(host, roots, args.get(1).unwrap_or(&Value::UNDEFINED))? {
                Ok(key) => key,
                Err(thrown) => return Ok(Invocation::Thrown(thrown)),
            };
        vm.read_property_with_receiver(
            host,
            roots,
            target,
            args.get(2).unwrap_or(&args[0]),
            key.borrowed(),
        )
    }

    #[function(name = "set", length = 3)]
    fn set(
        vm: &mut Vm,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let Some(ValueRef::Object(target)) = args.first().map(Value::view) else {
            return vm.throw_type_error(roots, "Reflect.set requires an object");
        };
        let key =
            match vm.coerce_property_key(host, roots, args.get(1).unwrap_or(&Value::UNDEFINED))? {
                Ok(key) => key,
                Err(thrown) => return Ok(Invocation::Thrown(thrown)),
            };
        let value = args.get(2).cloned().unwrap_or(Value::UNDEFINED);
        let receiver = args.get(3).unwrap_or(&args[0]);
        vm.set_property(host, roots, target, receiver, key, value, false)
    }

    #[function(name = "has", length = 2)]
    fn has(
        vm: &mut Vm,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let Some(ValueRef::Object(target)) = args.first().map(Value::view) else {
            return vm.throw_type_error(roots, "Reflect.has requires an object");
        };
        let key =
            match vm.coerce_property_key(host, roots, args.get(1).unwrap_or(&Value::UNDEFINED))? {
                Ok(key) => key,
                Err(thrown) => return Ok(Invocation::Thrown(thrown)),
            };
        vm.has_property_key(host, roots, target, key.borrowed())
    }

    #[function(name = "getOwnPropertyDescriptor", length = 2)]
    fn get_own_property_descriptor(
        vm: &mut Vm,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        vm.get_own_property_descriptor(host, roots, args, true)
    }

    #[function(name = "apply", length = 3)]
    fn apply(
        vm: &mut Vm,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        vm.reflect_call(host, roots, args, false)
    }

    #[function(name = "construct", length = 2)]
    fn construct(
        vm: &mut Vm,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        vm.reflect_call(host, roots, args, true)
    }
}
