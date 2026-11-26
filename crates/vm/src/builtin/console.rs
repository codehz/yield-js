//! The console namespace has its own ordinary prototype and detached-callable methods.

use super::globals::Owner;
use super::{OBJECT_PARENT, RootKind, RootSpec, define_builtin_data};
use crate::heap::{Attributes, TO_STRING_TAG_SYMBOL};
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::vm::VmCore;
use crate::{Value, value::PropertyKey};

mod logging;
mod state;
mod table;

const ROOTS: &[RootSpec] = &[
    RootSpec::new("console.prototype", RootKind::Plain, OBJECT_PARENT),
    RootSpec::new("console", RootKind::Plain, Some("console.prototype")),
];

macro_rules! console_methods {
    ($($variant:ident => $name:literal),+ $(,)?) => {
        #[derive(Clone, Copy)]
        enum Method { $($variant),+ }

        impl<P: crate::host::PlatformServices> BuiltinTable<P> {
            const METHODS: &[IntrinsicDef<P>] = &[$(IntrinsicDef::call(
                IntrinsicKey::new("console", Slot::Static, $name),
                0,
                |vm, host, roots, args, _| state::call(vm, host, roots, &args, Method::$variant),
            )),+];
        }

        impl<P: crate::host::PlatformServices> BuiltinTable<P> {
            pub(super) const OWNER: Owner<P> = Owner::new("console", ROOTS, &[Self::METHODS], Some(install));
        }
    };
}

console_methods! {
    Assert => "assert", Clear => "clear", Count => "count", CountReset => "countReset",
    Debug => "debug", Dir => "dir", Dirxml => "dirxml", Error => "error", Group => "group",
    GroupCollapsed => "groupCollapsed", GroupEnd => "groupEnd", Info => "info", Log => "log",
    Table => "table", Time => "time", TimeLog => "timeLog", TimeEnd => "timeEnd", Trace => "trace",
    Warn => "warn",
}

fn install(vm: &mut VmCore, _: &Value) {
    let object = vm.builtins.get("console");
    for method in BuiltinTable::<crate::host::Platform>::METHODS {
        let key = method.key;
        let function = vm.alloc_intrinsic(key);
        define_builtin_data(
            vm,
            object,
            key.member,
            Value::object(function),
            Attributes::ORDINARY,
        );
    }
    define_builtin_data(
        vm,
        object,
        PropertyKey::Symbol(TO_STRING_TAG_SYMBOL),
        Value::string("console".into()),
        Attributes::FUNCTION_METADATA,
    );
}

pub(crate) struct BuiltinTable<P>(std::marker::PhantomData<P>);
