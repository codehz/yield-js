//! Shared iterator prototypes, helpers, and disposal methods.

use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{
    ASYNC_DISPOSE_SYMBOL, Attributes, DISPOSE_SYMBOL, DescriptorUpdate, ITERATOR_SYMBOL, Object,
    PlainObject, ReactionJob, TO_STRING_TAG_SYMBOL,
};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::resolve::AwaitPrepared;
use crate::runtime::{Arguments, Invocation, Settlement};
use crate::value::{PropertyKey, PropertyKeyRef, Value, ValueRef};
use crate::vm::Vm;

use super::{RootKind, RootSpec, define_builtin_data, globals::Owner};

pub(crate) mod helpers;
use helpers::{Method, Protocol};

const ROOT: &str = "iterator";
pub(crate) const SYNC_ROOT: &str = "iterator.prototype";
pub(crate) const ASYNC_ROOT: &str = "asyncIterator.prototype";
const HELPER_ROOT: &str = "iteratorHelper.prototype";
const WRAPPER_ROOT: &str = "wrapForValidIterator.prototype";
const UNWRAP_ROOT: &str = "asyncIterator.dispose.unwrap";
pub(crate) const ROOTS: &[RootSpec] = &[
    RootSpec::new(SYNC_ROOT, RootKind::Plain, Some("object.prototype")),
    RootSpec::new(
        ROOT,
        RootKind::Native(CONSTRUCTOR),
        Some("function.prototype"),
    )
    .fixed(SYNC_ROOT),
    RootSpec::new(WRAPPER_ROOT, RootKind::Plain, Some(SYNC_ROOT)),
    RootSpec::new(HELPER_ROOT, RootKind::Plain, Some(SYNC_ROOT)),
    RootSpec::new(ASYNC_ROOT, RootKind::Plain, Some("object.prototype")),
    RootSpec::new(
        UNWRAP_ROOT,
        RootKind::Native(UNWRAP),
        Some("function.prototype"),
    ),
];

pub(crate) const OWNER: Owner = Owner::new(ROOT, &[], &[], None);

const CONSTRUCTOR: IntrinsicKey = IntrinsicKey::new("Iterator", Slot::Global, "");
const FROM: IntrinsicKey = IntrinsicKey::new("Iterator", Slot::Static, "from");
const CONCAT: IntrinsicKey = IntrinsicKey::new("Iterator", Slot::Static, "concat");
const WRAPPER_NEXT: IntrinsicKey =
    IntrinsicKey::new("WrapForValidIterator", Slot::Prototype, "next");
const WRAPPER_RETURN: IntrinsicKey =
    IntrinsicKey::new("WrapForValidIterator", Slot::Prototype, "return");
const CONSTRUCTOR_GET: IntrinsicKey =
    IntrinsicKey::new("Iterator", Slot::PrototypeGetter, "constructor");
const CONSTRUCTOR_SET: IntrinsicKey =
    IntrinsicKey::new("Iterator", Slot::PrototypeSetter, "constructor");
const DISPOSE: IntrinsicKey = IntrinsicKey::new("Iterator", Slot::PrototypeSymbol, "dispose");
const ASYNC_DISPOSE: IntrinsicKey =
    IntrinsicKey::new("AsyncIterator", Slot::PrototypeSymbol, "asyncDispose");
const UNWRAP: IntrinsicKey = IntrinsicKey::new("AsyncIterator", Slot::Static, "");

const HELPER_NEXT: IntrinsicKey = IntrinsicKey::new("IteratorHelper", Slot::Prototype, "next");
const HELPER_RETURN: IntrinsicKey = IntrinsicKey::new("IteratorHelper", Slot::Prototype, "return");
const TAG_GET: IntrinsicKey =
    IntrinsicKey::new("Iterator", Slot::PrototypeSymbolGetter, "toStringTag");
const TAG_SET: IntrinsicKey =
    IntrinsicKey::new("Iterator", Slot::PrototypeSymbolSetter, "toStringTag");

macro_rules! iterator_methods {
    ($($name:literal: $method:ident / $length:literal),* $(,)?; $($other:expr),* $(,)?) => {
        pub(crate) const INTRINSICS: &[IntrinsicDef] = &[
            $(IntrinsicDef::call(IntrinsicKey::new("Iterator", Slot::Prototype, $name), $length,
                |vm, host, roots, args, receiver| {
                    helpers::invoke(Method::$method, vm, host, roots, Arguments::Values(&args), receiver)
                }),)*
            $($other,)*
        ];
        const METHODS: &[(&str, IntrinsicKey)] = &[
            $(($name, IntrinsicKey::new("Iterator", Slot::Prototype, $name)),)*
        ];
    };
}

iterator_methods!(
    "map": Map / 1,
    "filter": Filter / 1,
    "take": Take / 1,
    "drop": Drop / 1,
    "flatMap": FlatMap / 1,
    "reduce": Reduce / 1,
    "toArray": ToArray / 0,
    "forEach": ForEach / 1,
    "some": Some / 1,
    "every": Every / 1,
    "find": Find / 1;
    IntrinsicDef::constructor_with_target(CONSTRUCTOR, 0, |vm, _, roots, _, _| {
        vm.throw_type_error(roots, "Iterator is an abstract constructor")
    }, construct),
    IntrinsicDef::call(FROM, 1, |vm, host, roots, args, _| {
        from(vm, host, roots, args.first().unwrap_or(&Value::UNDEFINED))
    }),
    IntrinsicDef::call(CONCAT, 0, |vm, host, roots, args, _| {
        helpers::concat(vm, host, roots, args)
    }),
    IntrinsicDef::call(CONSTRUCTOR_GET, 0, |vm, _, _, _, _| {
        Ok(Invocation::Value(Value::object(vm.builtins.get(ROOT))))
    }),
    IntrinsicDef::call(CONSTRUCTOR_SET, 1, |vm, host, roots, args, receiver| {
        set_own(vm, host, roots, args.into_iter().next().unwrap_or(Value::UNDEFINED), receiver,
            "constructor".into())
    }),
    IntrinsicDef::call(WRAPPER_NEXT, 0, |vm, host, roots, _, receiver| {
        forward(vm, host, roots, receiver, false)
    }),
    IntrinsicDef::call(WRAPPER_RETURN, 0, |vm, host, roots, _, receiver| {
        forward(vm, host, roots, receiver, true)
    }),
    IntrinsicDef::call(TAG_GET, 0, |_, _, _, _, _| {
        Ok(Invocation::Value(Value::string("Iterator".into())))
    }),
    IntrinsicDef::call(TAG_SET, 1, |vm, host, roots, args, receiver| {
        set_own(vm, host, roots, args.into_iter().next().unwrap_or(Value::UNDEFINED), receiver,
            PropertyKey::Symbol(TO_STRING_TAG_SYMBOL))
    }),
    IntrinsicDef::call(HELPER_NEXT, 0, |vm, host, roots, _, receiver| {
        helpers::resume(vm, host, roots, receiver, false)
    }),
    IntrinsicDef::call(HELPER_RETURN, 0, |vm, host, roots, _, receiver| {
        helpers::resume(vm, host, roots, receiver, true)
    }),
    IntrinsicDef::call(DISPOSE, 0, |vm, host, roots, _, receiver| {
        match return_method(vm, host, roots, receiver)? {
            Invocation::Value(method) if method == Value::UNDEFINED => {}
            Invocation::Value(method) => {
                if let thrown @ Invocation::Thrown(_) =
                    vm.invoke(host, roots, &method, Arguments::Values(&[]), receiver)?
                {
                    return Ok(thrown);
                }
            }
            thrown => return Ok(thrown),
        }
        Ok(Invocation::Value(Value::UNDEFINED))
    }),
    IntrinsicDef::call(ASYNC_DISPOSE, 0, |vm, host, roots, _, receiver| {
        let result = vm.heap.alloc_promise();
        let roots = &roots.with(&result);
        let settlement = match return_method(vm, host, roots, receiver)? {
            Invocation::Thrown(error) => Settlement::Rejected(error),
            Invocation::Value(method) if method == Value::UNDEFINED => {
                Settlement::Fulfilled(Value::UNDEFINED)
            }
            Invocation::Value(method) => match vm.invoke(
                host,
                roots,
                &method,
                Arguments::Values(&[Value::UNDEFINED]),
                receiver,
            )? {
                Invocation::Thrown(error) => Settlement::Rejected(error),
                Invocation::Value(value) => match vm.prepare_await(host, roots, value)? {
                    Err(error) => Settlement::Rejected(error),
                    Ok(prepared) => {
                        let (resolve, reject) = vm.alloc_resolution_pair(result, false)?;
                        let job = ReactionJob::Script {
                            resolve: Value::object(resolve),
                            reject: Value::object(reject),
                            on_fulfilled: Some(Value::object(vm.builtins.get(UNWRAP_ROOT))),
                            on_rejected: None,
                        };
                        match prepared {
                            AwaitPrepared::Promise(source) => vm.react(source, job)?,
                            AwaitPrepared::Plain(_) => {
                                vm.enqueue_job(job, Settlement::Fulfilled(Value::UNDEFINED))?
                            }
                        }
                        return Ok(Invocation::Value(Value::object(result)));
                    }
                },
            },
        };
        vm.settle(result, settlement)?;
        Ok(Invocation::Value(Value::object(result)))
    }),
    IntrinsicDef::call(UNWRAP, 1, |_, _, _, _, _| {
        Ok(Invocation::Value(Value::UNDEFINED))
    }),
);

fn construct(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    _args: Vec<Value>,
    new_target: &Value,
) -> Result<Invocation, VmError> {
    if *new_target == Value::object(vm.builtins.get(ROOT)) {
        return vm.throw_type_error(roots, "Iterator is an abstract constructor");
    }
    let prototype =
        match vm.read_property(host, roots, new_target, PropertyKeyRef::String("prototype"))? {
            Invocation::Value(value) => match value.view() {
                ValueRef::Object(id) => id,
                _ => vm.builtins.get(SYNC_ROOT),
            },
            thrown => return Ok(thrown),
        };
    Ok(Invocation::Value(Value::object(
        vm.heap.alloc_plain_with(Some(prototype)),
    )))
}

fn from(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    item: &Value,
) -> Result<Invocation, VmError> {
    let record = match (Protocol { vm, host, roots }).flatten(item, true)? {
        Ok(record) => record,
        Err(error) => return Ok(Invocation::Thrown(error)),
    };
    let edges = record.values();
    let constructor = Value::object(vm.builtins.get(ROOT));
    match vm.ordinary_has_instance(host, &roots.with(&edges), &record.iterator, &constructor)? {
        Invocation::Value(result) if result.truthy() => Ok(Invocation::Value(record.iterator)),
        Invocation::Value(_) => {
            let wrapper = vm.heap.alloc(Object::IteratorWrapper {
                object: PlainObject::new(Some(vm.builtins.get(WRAPPER_ROOT))),
                record,
            });
            Ok(Invocation::Value(Value::object(wrapper)))
        }
        thrown => Ok(thrown),
    }
}

fn forward(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    receiver: &Value,
    returning: bool,
) -> Result<Invocation, VmError> {
    let invalid = "wrapped iterator method requires a wrapped iterator";
    let ValueRef::Object(id) = receiver.view() else {
        return vm.throw_type_error(roots, invalid);
    };
    let Object::IteratorWrapper { record, .. } = vm.heap.get(id)? else {
        return vm.throw_type_error(roots, invalid);
    };
    let iterator = record.iterator.clone();
    let method = if returning {
        match return_method(vm, host, &roots.with(&iterator), &iterator)? {
            Invocation::Value(method) if method == Value::UNDEFINED => {
                return vm.iterator_result(Value::UNDEFINED, true);
            }
            Invocation::Value(method) => method,
            thrown => return Ok(thrown),
        }
    } else {
        record.next.clone()
    };
    vm.invoke(host, roots, &method, Arguments::EMPTY, &iterator)
}

// SetterThatIgnoresPrototypeProperties (ECMA-262 §7.3.37).
fn set_own(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    value: Value,
    receiver: &Value,
    key: PropertyKey,
) -> Result<Invocation, VmError> {
    let ValueRef::Object(id) = receiver.view() else {
        return vm.throw_type_error(roots, "iterator property receiver must be an object");
    };
    if id == vm.builtins.get(SYNC_ROOT) {
        return vm.throw_type_error(roots, "cannot assign the iterator prototype property");
    }
    let roots = &roots.with(&value);
    let result = match vm.own_descriptor(host, roots, id, key.borrowed())? {
        Err(error) => return Ok(Invocation::Thrown(error)),
        Ok(Some(_)) => vm.set_property(host, roots, id, receiver, key, value.clone(), true)?,
        Ok(None) => vm.apply_property_update(
            host,
            roots,
            id,
            key,
            DescriptorUpdate {
                value: Some(value.clone()),
                writable: Some(true),
                enumerable: Some(true),
                configurable: Some(true),
                ..DescriptorUpdate::default()
            },
        )?,
    };
    Ok(match result {
        Invocation::Value(_) => Invocation::Value(Value::UNDEFINED),
        thrown => thrown,
    })
}

fn return_method(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    match vm.read_property(host, roots, receiver, PropertyKeyRef::String("return"))? {
        Invocation::Value(value) => match value.view() {
            ValueRef::Undefined | ValueRef::Null => Ok(Invocation::Value(Value::UNDEFINED)),
            _ if vm.is_callable(&value) => Ok(Invocation::Value(value)),
            _ => vm.throw_type_error(roots, "Iterator return is not callable"),
        },
        thrown => Ok(thrown),
    }
}

pub(crate) fn install(vm: &mut Vm, identity: &Value) {
    let sync = vm.builtins.get(SYNC_ROOT);
    let helper = vm.builtins.get(HELPER_ROOT);
    let constructor = vm.builtins.get(ROOT);
    let wrapper = vm.builtins.get(WRAPPER_ROOT);
    define_builtin_data(
        vm,
        constructor,
        "prototype",
        Value::object(sync),
        Attributes::FIXED,
    );
    for (target, key) in [
        (constructor, FROM),
        (constructor, CONCAT),
        (wrapper, WRAPPER_NEXT),
        (wrapper, WRAPPER_RETURN),
    ] {
        let method = vm.alloc_intrinsic(key);
        define_builtin_data(
            vm,
            target,
            key.member,
            Value::object(method),
            Attributes::METHOD,
        );
    }
    for (key, get, set) in [
        ("constructor".into(), CONSTRUCTOR_GET, CONSTRUCTOR_SET),
        (PropertyKey::Symbol(TO_STRING_TAG_SYMBOL), TAG_GET, TAG_SET),
    ] {
        let get = vm.alloc_intrinsic(get);
        let set = vm.alloc_intrinsic(set);
        assert!(
            vm.heap
                .apply_descriptor(
                    sync,
                    key,
                    DescriptorUpdate {
                        get: Some(Some(get)),
                        set: Some(Some(set)),
                        configurable: Some(true),
                        ..DescriptorUpdate::default()
                    }
                )
                .expect("iterator prototype accessor")
        );
    }
    define_builtin_data(
        vm,
        sync,
        PropertyKey::Symbol(ITERATOR_SYMBOL),
        identity.clone(),
        Attributes::METHOD,
    );
    for &(name, key) in METHODS {
        let method = vm.alloc_intrinsic(key);
        define_builtin_data(vm, sync, name, Value::object(method), Attributes::METHOD);
    }
    for (name, key) in [("next", HELPER_NEXT), ("return", HELPER_RETURN)] {
        let method = vm.alloc_intrinsic(key);
        define_builtin_data(vm, helper, name, Value::object(method), Attributes::METHOD);
    }
    define_builtin_data(
        vm,
        helper,
        PropertyKey::Symbol(TO_STRING_TAG_SYMBOL),
        Value::string("Iterator Helper".into()),
        Attributes::FUNCTION_METADATA,
    );
    for (root, symbol, key) in [
        (SYNC_ROOT, DISPOSE_SYMBOL, DISPOSE),
        (ASYNC_ROOT, ASYNC_DISPOSE_SYMBOL, ASYNC_DISPOSE),
    ] {
        let method = vm.alloc_intrinsic(key);
        define_builtin_data(
            vm,
            vm.builtins.get(root),
            PropertyKey::Symbol(symbol),
            Value::object(method),
            Attributes::METHOD,
        );
    }
}
