#[yield_binding_macros::builtin_global(name = "Promise", root = "promise", kind = "constructor")]
pub(crate) mod promise_members {
    use super::*;

    const CONSTRUCTOR: IntrinsicKey = IntrinsicKey::new(NAME, Slot::Global, "");
    const SPECIES: IntrinsicKey = IntrinsicKey::new(NAME, Slot::StaticSymbolGetter, "species");
    pub(crate) const THEN: IntrinsicKey = IntrinsicKey::new(NAME, Slot::Prototype, "then");
    pub(crate) const ROOTS: &[RootSpec] = &[
        RootSpec::new(ROOT, RootKind::Native(CONSTRUCTOR), FUNCTION_PARENT)
            .fixed("promise.prototype"),
        RootSpec::new("promise.prototype", RootKind::Plain, OBJECT_PARENT),
    ];
    pub(crate) const GLOBAL: &[IntrinsicDef] = &[
        IntrinsicDef::constructor(
            CONSTRUCTOR,
            |vm, _, _, _| vm.throw_type_error("Promise constructor requires new"),
            |vm, host, args, prototype| vm.construct_promise(host, args, prototype),
        ),
        IntrinsicDef::call(SPECIES, |_, _, _, receiver| {
            Ok(Invocation::Value(receiver.clone()))
        }),
    ];

    pub(crate) fn install(vm: &mut Vm, _: &Value) {
        let constructor = vm.builtins.get(ROOT);
        let prototype = vm.builtins.get("promise.prototype");
        install_getter(
            vm,
            constructor,
            PropertyKey::Symbol(SPECIES_SYMBOL),
            SPECIES,
        );
        install_members(vm, constructor, prototype);
        define_builtin_data(
            vm,
            prototype,
            "constructor",
            Value::Object(constructor),
            Attributes::METHOD,
        );
        define_builtin_data(
            vm,
            constructor,
            "prototype",
            Value::Object(prototype),
            Attributes::FIXED,
        );
    }

    static_calls! {
        resolve | reject | all | allSettled | race | any as key => |vm, host, args, receiver| {
            if !matches!(receiver, Value::Object(id) if *id == vm.builtins.get(ROOT)) {
                return vm.throw_type_error("incompatible Promise receiver");
            }
            vm.call_promise_static(host, receiver, key, args)
        };
    }

    prototype_calls! {
        then | catch | finally as key => |vm, host, args, receiver| {
            let Value::Object(id) = receiver else {
                return vm.throw_type_error("incompatible Promise receiver");
            };
            if vm.heap.get(*id)?.as_promise().is_none() {
                return vm.throw_type_error("incompatible Promise receiver");
            }
            vm.call_promise_method(host, *id, key, args)
        };
    }
}
