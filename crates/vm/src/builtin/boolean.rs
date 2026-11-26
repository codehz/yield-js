#[yield_binding_macros::builtin_global(name = "Boolean", root = "boolean", kind = "constructor")]
pub(crate) mod members {
    pub(crate) struct BuiltinTable<P>(std::marker::PhantomData<P>);

    use crate::builtin::globals::Owner;
    use crate::builtin::roots::{RootKind, RootSpec};
    use crate::builtin::{FUNCTION_PARENT, OBJECT_PARENT, link_constructor};
    use crate::error::VmError;
    use crate::gc::ActiveRoots;
    use crate::heap::{BoxedPrimitive, Object};
    use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
    use crate::runtime::Invocation;
    use crate::value::{Value, ValueRef};
    use crate::vm::VmCore;

    const KEY: IntrinsicKey = IntrinsicKey::new(NAME, Slot::Global, "");
    const PROTOTYPE: &str = "boolean.prototype";
    impl<P: crate::host::PlatformServices> BuiltinTable<P> {
        pub(crate) const OWNER: Owner<P> = Owner::new(
            ROOT,
            &[
                RootSpec::new(ROOT, RootKind::Native(KEY), FUNCTION_PARENT).fixed(PROTOTYPE),
                RootSpec::new(PROTOTYPE, RootKind::Plain, OBJECT_PARENT),
            ],
            &[
                &[IntrinsicDef::constructor(
                    KEY,
                    1,
                    |_, _, _, args, _| {
                        Ok(Invocation::Value(Value::boolean(
                            args.first().is_some_and(Value::truthy),
                        )))
                    },
                    |vm, _, roots, args, prototype| {
                        let result = vm.construct_object(&[Value::boolean(
                            args.first().is_some_and(Value::truthy),
                        )])?;
                        vm.with_result_prototype(roots, result, prototype)
                    },
                )],
                Globals::<P>::INTRINSICS,
            ],
            Some(install),
        );
    }

    fn install(vm: &mut VmCore, _: &Value) {
        let constructor = vm.builtins.get(ROOT);
        let prototype = vm.builtins.get(PROTOTYPE);
        link_constructor(vm, constructor, prototype);
        install_members(vm, constructor, prototype);
    }

    fn boolean_value(
        vm: &mut VmCore,
        roots: &ActiveRoots<'_>,
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
                _ => return vm.throw_type_error(roots, "incompatible Boolean receiver"),
            },
            _ => return vm.throw_type_error(roots, "incompatible Boolean receiver"),
        };
        Ok(Invocation::Value(if stringify {
            Value::string(value.to_string().into())
        } else {
            Value::boolean(value)
        }))
    }

    prototype_calls! {
        valueOf: 0 => |vm, _, roots, _, receiver| boolean_value(vm, roots, receiver, false);
        toString: 0 => |vm, _, roots, _, receiver| boolean_value(vm, roots, receiver, true);
    }
}
