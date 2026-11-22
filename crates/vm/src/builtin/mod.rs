//! VM builtins: promises, microtasks, and object model methods.
//!
//! Script-visible `Error` and `TypeError` are objects, including VM-generated
//! TypeErrors. `Promise.any` rejects with an `AggregateError` object.
//!
//! `queueMicrotask` enqueues a VM microtask. A throw from its callback has no
//! result promise and is delivered through `Host::report_error`. Promise
//! rejection notifications are delivered separately at the microtask checkpoint.

use crate::heap::{Attributes, DescriptorUpdate};
use crate::intrinsic::IntrinsicKey;
use crate::value::{ObjectId, PropertyKey, Value};
use crate::vm::Vm;

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
        Value::object(prototype),
        Attributes::FIXED,
    );
    define_builtin_data(
        vm,
        prototype,
        "constructor",
        Value::object(constructor),
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

pub(crate) const ROOT_STRING_PROTOTYPE: &str = "string.prototype";
pub(crate) const ROOT_STRING_ITERATOR: &str = "string.iterator";
pub(crate) const ROOT_STRING_ITERATOR_PROTOTYPE: &str = "stringIterator.prototype";
pub(crate) const ROOT_RANGEERROR_PROTOTYPE: &str = "rangeerror.prototype";
pub(crate) const ROOT_NUMBER_PROTOTYPE: &str = "number.prototype";
pub(crate) const ROOT_PROXY: &str = "proxy";
pub(crate) const ROOT_OBJECT_PROTOTYPE: &str = "object.prototype";
pub(crate) const ROOT_FUNCTION_PROTOTYPE: &str = "function.prototype";

pub(crate) mod array;
pub(crate) mod async_generator;
mod bigint;
pub(crate) mod binary;
mod boolean;
pub(crate) mod call;
pub(crate) mod collections;
mod date;
#[path = "date/calendar.rs"]
pub(crate) mod date_calendar;
mod disposable_stack;
mod disposal;
pub(crate) mod dom_exception;
pub(super) mod error_family;
pub(crate) mod generator;
mod global_functions;
pub(crate) mod globals;
pub(crate) mod iterator;
pub(crate) mod json;
pub(crate) mod math;
pub(crate) mod number;
mod object;
mod promise;
pub(crate) mod proxy;
pub(super) mod queue_family;
mod reflect;
pub(crate) mod regexp_runtime;
pub(crate) mod roots;
pub(crate) mod string;
pub(super) mod symbol_family;

pub(crate) use call::is_callable_object;
pub use globals::BUILTINS;
pub(crate) use roots::{BuiltinRoots, RootKind, RootSpec};
