//! VM builtins: promises, microtasks, and object model methods.
//!
//! Script-visible `Error` and `TypeError` are objects, including VM-generated
//! TypeErrors. The `AggregateError` rejection from `Promise.any` is a string.
//!
//! `queueMicrotask` enqueues a VM microtask. A throw from its callback has no
//! result promise and is delivered through `Host::report_error`. Unhandled
//! promise rejections are not tracked.

use crate::JsString;
use crate::builtin::string::utf16_len;
use crate::error::VmError;
use crate::heap::BoxedPrimitive;
use crate::heap::{
    Attributes, BoundObject, CombineKind, CombineState, DescriptorUpdate, ITERATOR_SYMBOL,
    IntegrityLevel, MATCH_ALL_SYMBOL, MATCH_SYMBOL, Object, PlainObject, PromiseCallback,
    PromiseState, Property, REPLACE_SYMBOL, ReactionJob, Resolution, SEARCH_SYMBOL, SPECIES_SYMBOL,
    SPLIT_SYMBOL, TO_PRIMITIVE_SYMBOL,
};
use crate::host::Host;
use crate::intrinsic::{CallableTarget, IntrinsicDef, IntrinsicKey, Slot};
use crate::ops;
use crate::runtime::{Invocation, Microtask, Settlement};
use crate::value::{ObjectId, PropertyKey, PropertyKeyRef, Value};
use crate::vm::Vm;

use globals::Owner;

const OBJECT_PARENT: Option<&str> = Some("object.prototype");
const FUNCTION_PARENT: Option<&str> = Some("function.prototype");

fn define_builtin_data(
    vm: &mut Vm,
    target: ObjectId,
    key: impl Into<PropertyKey>,
    value: Value,
    attrs: Attributes,
) {
    vm.heap
        .define_data(target, key, value, attrs)
        .expect("builtin data property");
}

fn link_constructor(vm: &mut Vm, constructor: ObjectId, prototype: ObjectId) {
    define_builtin_data(
        vm,
        constructor,
        "prototype",
        Value::Object(prototype),
        Attributes::FIXED,
    );
    define_builtin_data(
        vm,
        prototype,
        "constructor",
        Value::Object(constructor),
        Attributes::METHOD,
    );
}

fn install_getter(vm: &mut Vm, target: ObjectId, key: PropertyKey, intrinsic: IntrinsicKey) {
    let getter = vm.alloc_intrinsic(intrinsic);
    assert!(
        vm.heap
            .apply_descriptor(
                target,
                key,
                DescriptorUpdate {
                    get: Some(Some(getter)),
                    configurable: Some(true),
                    ..DescriptorUpdate::default()
                }
            )
            .expect("builtin getter")
    );
}

pub(super) mod queue_family {
    use super::*;
    const NAME: &str = "queueMicrotask";
    const KEY: IntrinsicKey = IntrinsicKey::new(NAME, Slot::Global, "");
    pub(crate) const OWNER: Owner = Owner::new(
        NAME,
        &[RootSpec::new(NAME, RootKind::Native(KEY), FUNCTION_PARENT)],
        &[&[IntrinsicDef::call(KEY, |vm, _, args, _| {
            vm.queue_microtask(args)
        })]],
        None,
    );
}

#[yield_binding_macros::builtin_global(name = "Boolean", root = "boolean", kind = "constructor")]
pub(super) mod boolean_family {
    use super::*;
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
                |_, _, args, _| {
                    Ok(Invocation::Value(Value::Bool(
                        args.first().is_some_and(Value::truthy),
                    )))
                },
                |vm, _, args, prototype| {
                    let result = vm.construct_object(&[Value::Bool(
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
        let value = match receiver {
            Value::Bool(value) => *value,
            Value::Object(id) => match vm.heap.get(*id)? {
                Object::Boxed {
                    primitive: BoxedPrimitive::Bool(value),
                    ..
                } => *value,
                _ => return vm.throw_type_error("incompatible Boolean receiver"),
            },
            _ => return vm.throw_type_error("incompatible Boolean receiver"),
        };
        Ok(Invocation::Value(if stringify {
            Value::String(value.to_string().into())
        } else {
            Value::Bool(value)
        }))
    }

    prototype_calls! {
        valueOf => |vm, _, _, receiver| boolean_value(vm, receiver, false);
        toString => |vm, _, _, receiver| boolean_value(vm, receiver, true);
    }
}

pub(super) mod symbol_family {
    use super::*;
    pub(super) const CONSTRUCTOR: IntrinsicKey = IntrinsicKey::new("Symbol", Slot::Global, "");
    const DESCRIPTION: IntrinsicKey =
        IntrinsicKey::new("Symbol", Slot::PrototypeGetter, "description");
    const ROOT: &str = "symbol";
    const PROTOTYPE: &str = "symbol.prototype";

    macro_rules! symbol_methods {
        ($($slot:ident $name:literal => $call:expr),* $(,)?) => {
            const METHODS: &[IntrinsicDef] = &[$(IntrinsicDef::call(IntrinsicKey::new("Symbol", Slot::$slot, $name), $call)),*];
        };
    }

    symbol_methods! {
        Prototype "valueOf" => |vm, _, _, receiver| vm.symbol_method(receiver, "valueOf"),
        Prototype "toString" => |vm, _, _, receiver| vm.symbol_method(receiver, "toString"),
        Static "for" => |vm, host, args, receiver| vm.symbol_static(receiver, |vm, args| vm.symbol_for(host, args), args),
        Static "keyFor" => |vm, _, args, receiver| vm.symbol_static(receiver, Vm::symbol_key_for, args),
    }

    pub(crate) const OWNER: Owner = Owner::new(
        ROOT,
        &[
            RootSpec::new(ROOT, RootKind::Native(CONSTRUCTOR), FUNCTION_PARENT).fixed(PROTOTYPE),
            RootSpec::new(PROTOTYPE, RootKind::Plain, OBJECT_PARENT),
        ],
        &[
            &[
                IntrinsicDef::call(CONSTRUCTOR, |vm, host, args, _| {
                    vm.symbol_constructor(host, args)
                }),
                IntrinsicDef::call(DESCRIPTION, |vm, _, _, receiver| {
                    vm.symbol_method(receiver, "get description")
                }),
            ],
            METHODS,
        ],
        Some(install),
    );

    fn install(vm: &mut Vm, _: &Value) {
        let constructor = vm.builtins.get(ROOT);
        let prototype = vm.builtins.get(PROTOTYPE);
        link_constructor(vm, constructor, prototype);
        for def in METHODS {
            let target = if def.key.slot == Slot::Static {
                constructor
            } else {
                prototype
            };
            let method = vm.alloc_intrinsic(def.key);
            define_builtin_data(
                vm,
                target,
                def.key.member,
                Value::Object(method),
                Attributes::METHOD,
            );
        }
        install_getter(
            vm,
            prototype,
            PropertyKey::String("description".into()),
            DESCRIPTION,
        );
        for (name, id) in [
            ("iterator", ITERATOR_SYMBOL),
            ("toPrimitive", TO_PRIMITIVE_SYMBOL),
            ("species", SPECIES_SYMBOL),
            ("match", MATCH_SYMBOL),
            ("matchAll", MATCH_ALL_SYMBOL),
            ("replace", REPLACE_SYMBOL),
            ("search", SEARCH_SYMBOL),
            ("split", SPLIT_SYMBOL),
        ] {
            define_builtin_data(vm, constructor, name, Value::Symbol(id), Attributes::FIXED);
        }
    }
}

pub(super) mod error_family {
    use super::*;

    macro_rules! error_class {
        ($owner:ident, $name:literal, $root:literal, $parent:expr, $base:literal) => {
            pub(crate) const $owner: Owner = {
                const PROTOTYPE: &str = concat!($root, ".prototype");
                const KEY: IntrinsicKey = IntrinsicKey::new($name, Slot::Global, "");
                Owner::new(
                    $root,
                    &[
                        RootSpec::new($root, RootKind::Native(KEY), FUNCTION_PARENT)
                            .fixed(PROTOTYPE)
                            .backlink(Some(Attributes::METHOD)),
                        RootSpec::new(PROTOTYPE, RootKind::Plain, $parent)
                            .with_fixed_parent(!$base),
                    ],
                    &[&[IntrinsicDef::constructor(
                        KEY,
                        |vm, host, args, _| vm.construct_error(host, $name, PROTOTYPE, args),
                        |vm, host, args, prototype| {
                            let result = vm.construct_error(host, $name, PROTOTYPE, args)?;
                            vm.with_result_prototype(result, prototype)
                        },
                    )]],
                    Some(|vm, _| {
                        let prototype = vm.builtins.get(PROTOTYPE);
                        let constructor = vm.builtins.get($root);
                        define_builtin_data(
                            vm,
                            prototype,
                            "name",
                            Value::String($name.into()),
                            Attributes::METHOD,
                        );
                        if $base {
                            define_builtin_data(
                                vm,
                                prototype,
                                "message",
                                Value::String(JsString::default()),
                                Attributes::METHOD,
                            );
                        }
                        link_constructor(vm, constructor, prototype);
                    }),
                )
            };
        };
    }

    error_class!(ERROR, "Error", "error", OBJECT_PARENT, true);
    error_class!(
        TYPE_ERROR,
        "TypeError",
        "typeerror",
        Some("error.prototype"),
        false
    );
    error_class!(
        RANGE_ERROR,
        "RangeError",
        "rangeerror",
        Some("error.prototype"),
        false
    );
    error_class!(
        REFERENCE_ERROR,
        "ReferenceError",
        "referenceerror",
        Some("error.prototype"),
        false
    );
    error_class!(
        SYNTAX_ERROR,
        "SyntaxError",
        "syntaxerror",
        Some("error.prototype"),
        false
    );
    error_class!(
        EVAL_ERROR,
        "EvalError",
        "evalerror",
        Some("error.prototype"),
        false
    );
    error_class!(
        URI_ERROR,
        "URIError",
        "urierror",
        Some("error.prototype"),
        false
    );
    error_class!(
        AGGREGATE_ERROR,
        "AggregateError",
        "aggregateerror",
        Some("error.prototype"),
        false
    );
}

pub(crate) const ROOT_STRING_PROTOTYPE: &str = "string.prototype";
pub(crate) const ROOT_STRING_ITERATOR: &str = "string.iterator";
pub(crate) const ROOT_STRING_ITERATOR_PROTOTYPE: &str = "stringIterator.prototype";
pub(crate) const ROOT_RANGEERROR_PROTOTYPE: &str = "rangeerror.prototype";
pub(crate) const ROOT_NUMBER_PROTOTYPE: &str = "number.prototype";
pub(crate) const ROOT_PROXY: &str = "proxy";
pub(crate) const ROOT_OBJECT_PROTOTYPE: &str = "object.prototype";
pub(crate) const ROOT_FUNCTION_PROTOTYPE: &str = "function.prototype";

pub(crate) mod array;
pub(crate) mod call;
pub(crate) mod collections;
pub(crate) mod globals;
pub(crate) mod json;
pub(crate) mod math;
pub(crate) mod number;
pub(crate) mod proxy;
pub(crate) mod regexp_runtime;
pub(crate) mod roots;
pub(crate) mod string;

include!("reflect_namespace.rs");
include!("object_members.rs");
include!("promise_members.rs");

pub(crate) use call::is_callable_object;
pub use globals::BUILTINS;
pub(crate) use roots::{BuiltinRoots, RootKind, RootSpec};
