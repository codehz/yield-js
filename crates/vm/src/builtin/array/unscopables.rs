//! Install the Array prototype's ordinary, null-prototype unscopables list.

use crate::builtin::define_builtin_data;
use crate::heap::{Attributes, UNSCOPABLES_SYMBOL};
use crate::value::{ObjectId, PropertyKey, Value};
use crate::vm::VmCore;

pub(super) fn install(vm: &mut VmCore, prototype: ObjectId) {
    let list = vm.heap.alloc_plain_with(None);
    for name in [
        "at",
        "copyWithin",
        "entries",
        "fill",
        "find",
        "findIndex",
        "findLast",
        "findLastIndex",
        "flat",
        "flatMap",
        "includes",
        "keys",
        "toReversed",
        "toSorted",
        "toSpliced",
        "values",
    ] {
        define_builtin_data(vm, list, name, Value::boolean(true), Attributes::ORDINARY);
    }
    define_builtin_data(
        vm,
        prototype,
        PropertyKey::Symbol(UNSCOPABLES_SYMBOL),
        Value::object(list),
        Attributes::FUNCTION_METADATA,
    );
}
