#[yield_binding_macros::builtin_global(name = "Boolean", root = "boolean", kind = "constructor")]
pub(crate) mod members {
    use crate::builtin::globals::Owner;
    use crate::builtin::roots::{RootKind, RootSpec};
    use crate::builtin::{FUNCTION_PARENT, OBJECT_PARENT, link_constructor};
    use crate::error::VmError;
    use crate::heap::{BoxedPrimitive, Object};
    use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
    use crate::runtime::Invocation;
    use crate::value::{Value, ValueRef};
    use crate::vm::Vm;

    const KEY: IntrinsicKey = IntrinsicKey::new(NAME, Slot::Global, "");
    const PROTOTYPE: &str = "boolean.prototype";
    pub(crate) const OWNER: Owner = Owner::new(
        ROOT,
        &[
            RootSpec::new(ROOT, RootKind::Native(KEY), FUNCTION_PARENT).fixed(PROTOTYPE),
            RootSpec::new(PROTOTYPE, RootKind::Plain, OBJECT_PARENT),
        ],
        &[
            &[IntrinsicDef::constructor(
                KEY,
                |_, _, _, args, _| {
                    Ok(Invocation::Value(Value::boolean(
                        args.first().is_some_and(Value::truthy),
                    )))
                },
                |vm, _, _, args, prototype| {
                    let result = vm.construct_object(&[Value::boolean(
                        args.first().is_some_and(Value::truthy),
                    )])?;
                    vm.with_result_prototype(result, prototype)
                },
            )],
            INTRINSICS,
        ],
        Some(install),
    );

    fn install(vm: &mut Vm, _: &Value) {
        let constructor = vm.builtins.get(ROOT);
        let prototype = vm.builtins.get(PROTOTYPE);
        link_constructor(vm, constructor, prototype);
        install_members(vm, constructor, prototype);
    }

    fn boolean_value(
        vm: &mut Vm,
        receiver: &Value,
        stringify: bool,
    ) -> Result<Invocation, VmError> {
        let value = match receiver.view() {
            ValueRef::Bool(value) => value,
            ValueRef::Object(id) => match vm.heap.get(id)? {
                Object::Boxed {
                    primitive: BoxedPrimitive::Bool(value),
                    ..
                } => *value,
                _ => return vm.throw_type_error("incompatible Boolean receiver"),
            },
            _ => return vm.throw_type_error("incompatible Boolean receiver"),
        };
        Ok(Invocation::Value(if stringify {
            Value::string(value.to_string().into())
        } else {
            Value::boolean(value)
        }))
    }

    prototype_calls! {
        valueOf => |vm, _, _, _, receiver| boolean_value(vm, receiver, false);
        toString => |vm, _, _, _, receiver| boolean_value(vm, receiver, true);
    }
}
