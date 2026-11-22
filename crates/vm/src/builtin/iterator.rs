//! Shared disposal methods for synchronous and asynchronous iterators.

use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{ASYNC_DISPOSE_SYMBOL, Attributes, DISPOSE_SYMBOL, ReactionJob};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::resolve::AwaitPrepared;
use crate::runtime::{Arguments, Invocation, Settlement};
use crate::value::{PropertyKey, PropertyKeyRef, Value, ValueRef};
use crate::vm::Vm;

use super::{RootKind, RootSpec, define_builtin_data};

pub(crate) const SYNC_ROOT: &str = "iterator.prototype";
pub(crate) const ASYNC_ROOT: &str = "asyncIterator.prototype";
const UNWRAP_ROOT: &str = "asyncIterator.dispose.unwrap";
pub(crate) const ROOTS: &[RootSpec] = &[
    RootSpec::new(SYNC_ROOT, RootKind::Plain, Some("object.prototype")),
    RootSpec::new(ASYNC_ROOT, RootKind::Plain, Some("object.prototype")),
    RootSpec::new(
        UNWRAP_ROOT,
        RootKind::Native(UNWRAP),
        Some("function.prototype"),
    ),
];

const DISPOSE: IntrinsicKey = IntrinsicKey::new("Iterator", Slot::PrototypeSymbol, "dispose");
const ASYNC_DISPOSE: IntrinsicKey =
    IntrinsicKey::new("AsyncIterator", Slot::PrototypeSymbol, "asyncDispose");
const UNWRAP: IntrinsicKey = IntrinsicKey::new("AsyncIterator", Slot::Static, "");

pub(crate) const INTRINSICS: &[IntrinsicDef] = &[
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
];

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
            _ => vm.throw_type_error("Iterator return is not callable"),
        },
        thrown => Ok(thrown),
    }
}

pub(crate) fn install(vm: &mut Vm) {
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
