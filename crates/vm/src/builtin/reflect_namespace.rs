#[yield_binding_macros::builtin_global(name = "Reflect", root = "reflect")]
pub(crate) mod reflect_namespace {
    use super::{Host, Invocation, PropertyKey, Value, Vm, VmError};

    #[function(name = "ownKeys")]
    fn own_keys(vm: &mut Vm, host: &mut dyn Host, args: &[Value]) -> Result<Invocation, VmError> {
        let Some(Value::Object(id)) = args.first() else {
            return vm.throw_type_error("Reflect.ownKeys requires an object");
        };
        let keys = match vm.own_keys(host, *id, false)? {
            Ok(keys) => keys,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let keys = keys
            .into_iter()
            .map(|key| match key {
                PropertyKey::String(key) => Value::String(key),
                PropertyKey::Symbol(id) => Value::Symbol(id),
            })
            .collect();
        Ok(Invocation::Value(Value::Object(vm.heap.alloc_array(keys))))
    }

    #[function(name = "deleteProperty")]
    fn delete_property(
        vm: &mut Vm,
        host: &mut dyn Host,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let Some(target @ Value::Object(_)) = args.first() else {
            return vm.throw_type_error("Reflect.deleteProperty requires an object");
        };
        vm.delete_property(host, target, args.get(1).unwrap_or(&Value::Undefined))
    }

    #[function(name = "defineProperty")]
    fn define_property(
        vm: &mut Vm,
        host: &mut dyn Host,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        vm.define_property_builtin(host, args, true)
    }

    #[function(name = "preventExtensions")]
    fn prevent_extensions(
        vm: &mut Vm,
        _host: &mut dyn Host,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        vm.prevent_extensions_builtin_impl(args, true)
    }

    #[function(name = "isExtensible")]
    fn is_extensible(
        vm: &mut Vm,
        _host: &mut dyn Host,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        vm.is_extensible_builtin_impl(args, true)
    }

    #[function(name = "getPrototypeOf")]
    fn get_prototype_of(
        vm: &mut Vm,
        _host: &mut dyn Host,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        vm.get_prototype_of(args, true)
    }

    #[function(name = "setPrototypeOf")]
    fn set_prototype_of(
        vm: &mut Vm,
        _host: &mut dyn Host,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        vm.set_prototype_of(args, true)
    }

    #[function(name = "get")]
    fn get(vm: &mut Vm, host: &mut dyn Host, args: &[Value]) -> Result<Invocation, VmError> {
        let Some(Value::Object(target)) = args.first() else {
            return vm.throw_type_error("Reflect.get requires an object");
        };
        let key = match vm.coerce_property_key(host, args.get(1).unwrap_or(&Value::Undefined))? {
            Ok(key) => key,
            Err(thrown) => return Ok(Invocation::Thrown(thrown)),
        };
        vm.read_property_with_receiver(
            host,
            *target,
            args.get(2).unwrap_or(&args[0]),
            key.borrowed(),
        )
    }

    #[function(name = "set")]
    fn set(vm: &mut Vm, host: &mut dyn Host, args: &[Value]) -> Result<Invocation, VmError> {
        let Some(Value::Object(target)) = args.first() else {
            return vm.throw_type_error("Reflect.set requires an object");
        };
        let key = match vm.coerce_property_key(host, args.get(1).unwrap_or(&Value::Undefined))? {
            Ok(key) => key,
            Err(thrown) => return Ok(Invocation::Thrown(thrown)),
        };
        let value = args.get(2).cloned().unwrap_or(Value::Undefined);
        let receiver = args.get(3).unwrap_or(&args[0]);
        vm.set_property(host, *target, receiver, key, value, false)
    }

    #[function(name = "has")]
    fn has(vm: &mut Vm, host: &mut dyn Host, args: &[Value]) -> Result<Invocation, VmError> {
        let Some(Value::Object(target)) = args.first() else {
            return vm.throw_type_error("Reflect.has requires an object");
        };
        let key = match vm.coerce_property_key(host, args.get(1).unwrap_or(&Value::Undefined))? {
            Ok(key) => key,
            Err(thrown) => return Ok(Invocation::Thrown(thrown)),
        };
        vm.has_property_key(host, *target, key.borrowed())
    }

    #[function(name = "getOwnPropertyDescriptor")]
    fn get_own_property_descriptor(
        vm: &mut Vm,
        host: &mut dyn Host,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        vm.get_own_property_descriptor(host, args)
    }

    #[function(name = "apply")]
    fn apply(vm: &mut Vm, host: &mut dyn Host, args: &[Value]) -> Result<Invocation, VmError> {
        vm.reflect_call(host, args, false)
    }

    #[function(name = "construct")]
    fn construct(vm: &mut Vm, host: &mut dyn Host, args: &[Value]) -> Result<Invocation, VmError> {
        vm.reflect_call(host, args, true)
    }
}
