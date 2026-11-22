#[yield_binding_macros::builtin_global(name = "Promise", root = "promise", kind = "constructor")]
pub(crate) mod members {
    use crate::builtin::globals::Owner;
    use crate::builtin::roots::{RootKind, RootSpec};
    use crate::builtin::{FUNCTION_PARENT, OBJECT_PARENT, define_builtin_data};
    use crate::heap::{Attributes, SPECIES_SYMBOL};
    use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
    use crate::runtime::Invocation;
    use crate::value::Value;
    use crate::vm::Vm;

    const CONSTRUCTOR: IntrinsicKey = IntrinsicKey::new(NAME, Slot::Global, "");
    pub(crate) const THEN: IntrinsicKey = IntrinsicKey::new(NAME, Slot::Prototype, "then");
    const ROOTS: &[RootSpec] = &[
        RootSpec::new(ROOT, RootKind::Native(CONSTRUCTOR), FUNCTION_PARENT)
            .fixed("promise.prototype"),
        RootSpec::new("promise.prototype", RootKind::Plain, OBJECT_PARENT),
    ];
    const GLOBAL: &[IntrinsicDef] = &[IntrinsicDef::constructor(
        CONSTRUCTOR,
        1,
        |vm, _, roots, _, _| vm.throw_type_error(roots, "Promise constructor requires new"),
        |vm, host, roots, args, prototype| vm.construct_promise(host, roots, args, prototype),
    )];
    pub(crate) const OWNER: Owner = Owner::new(ROOT, ROOTS, &[GLOBAL, INTRINSICS], Some(install));

    fn install(vm: &mut Vm, _: &Value) {
        let constructor = vm.builtins.get(ROOT);
        let prototype = vm.builtins.get("promise.prototype");
        install_members(vm, constructor, prototype);
        define_builtin_data(
            vm,
            prototype,
            "constructor",
            Value::object(constructor),
            Attributes::METHOD,
        );
        define_builtin_data(
            vm,
            constructor,
            "prototype",
            Value::object(prototype),
            Attributes::FIXED,
        );
    }

    static_calls! {
        get symbol(SPECIES_SYMBOL) species: 0 => |_, _, _, _, receiver| {
            Ok(Invocation::Value(receiver.clone()))
        };
        resolve: 1 | reject: 1 | all: 1 | allSettled: 1 | race: 1 | any: 1 | withResolvers: 0 | try: 1 as key => |vm, host, roots, args, receiver| {
            vm.call_promise_static(host, roots, receiver, key, args)
        };
    }

    prototype_calls! {
        then: 2 | catch: 1 | finally: 1 as key => |vm, host, roots, args, receiver| {
            vm.call_promise_method(host, roots, receiver, key, args)
        };
    }
}
