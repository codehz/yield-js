use crate::builtin::array;
use crate::builtin::boolean::members as boolean_family;
use crate::builtin::collections;
use crate::builtin::json::namespace as json;
use crate::builtin::math::namespace as math;
use crate::builtin::number;
use crate::builtin::object::members as object_members;
use crate::builtin::promise::members as promise_members;
use crate::builtin::proxy;
use crate::builtin::reflect::namespace as reflect_namespace;
use crate::builtin::regexp_runtime as regexp;
use crate::builtin::roots::{RootKind, RootSpec};
use crate::builtin::string;
use crate::builtin::{error_family, queue_family, symbol_family};
use crate::error::VmError;
use crate::exec;
use crate::heap::{Attributes, HAS_INSTANCE_SYMBOL};
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::value::{PropertyKey, Value};
use crate::vm::Vm;

pub(crate) struct Owner {
    root: &'static str,
    roots: &'static [RootSpec],
    groups: &'static [&'static [IntrinsicDef]],
    install: Option<fn(&mut Vm, &Value)>,
}

impl Owner {
    pub(crate) const fn new(
        root: &'static str,
        roots: &'static [RootSpec],
        groups: &'static [&'static [IntrinsicDef]],
        install: Option<fn(&mut Vm, &Value)>,
    ) -> Self {
        Self {
            root,
            roots,
            groups,
            install,
        }
    }
}

enum Global {
    Owner(Owner),
    Number(f64),
}

macro_rules! builtin_globals {
    ($($name:expr => $global:expr),+ $(,)?) => {
        const GLOBALS: &[Global] = &[$($global),+];

        /// VM-provided globals in bytecode operand order. Append; never reorder.
        pub const BUILTINS: [&str; GLOBALS.len()] = [$($name),+];
    };
}

// Foundational roots and non-global functions are not bytecode globals.
const ITERATOR_IDENTITY: IntrinsicKey =
    IntrinsicKey::new("Iterator", Slot::PrototypeSymbol, "identity");
const ITERATOR_INTRINSICS: &[IntrinsicDef] = &[IntrinsicDef::call(
    ITERATOR_IDENTITY,
    |_, _, _, _, receiver| Ok(crate::runtime::Invocation::Value(receiver.clone())),
)];
const GLOBAL_THIS: &str = "globalThis";
const GLOBAL_THIS_ROOTS: &[RootSpec] = &[RootSpec::new(
    GLOBAL_THIS,
    RootKind::Plain,
    Some("object.prototype"),
)];

builtin_globals! {
    promise_members::NAME => Global::Owner(promise_members::OWNER),
    "queueMicrotask" => Global::Owner(queue_family::OWNER),
    "Error" => Global::Owner(error_family::ERROR),
    "TypeError" => Global::Owner(error_family::TYPE_ERROR),
    "Symbol" => Global::Owner(symbol_family::OWNER),
    reflect_namespace::NAME => Global::Owner(reflect_namespace::OWNER),
    object_members::NAME => Global::Owner(object_members::OWNER),
    "NaN" => Global::Number(f64::NAN),
    "Infinity" => Global::Number(f64::INFINITY),
    "Array" => Global::Owner(array::OWNER),
    string::members::NAME => Global::Owner(string::OWNER),
    "RangeError" => Global::Owner(error_family::RANGE_ERROR),
    "ReferenceError" => Global::Owner(error_family::REFERENCE_ERROR),
    "SyntaxError" => Global::Owner(error_family::SYNTAX_ERROR),
    "EvalError" => Global::Owner(error_family::EVAL_ERROR),
    "URIError" => Global::Owner(error_family::URI_ERROR),
    "AggregateError" => Global::Owner(error_family::AGGREGATE_ERROR),
    number::members::NAME => Global::Owner(number::OWNER),
    math::NAME => Global::Owner(math::OWNER),
    json::NAME => Global::Owner(json::OWNER),
    "Map" => Global::Owner(collections::MAP_OWNER),
    "Set" => Global::Owner(collections::SET_OWNER),
    "WeakMap" => Global::Owner(collections::WEAK_MAP_OWNER),
    "Proxy" => Global::Owner(proxy::OWNER),
    "Boolean" => Global::Owner(boolean_family::OWNER),
    GLOBAL_THIS => Global::Owner(Owner::new(GLOBAL_THIS, GLOBAL_THIS_ROOTS, &[], None)),
    "RegExp" => Global::Owner(regexp::OWNER),
    "WeakSet" => Global::Owner(collections::WEAK_SET_OWNER),
}

fn owners() -> impl Iterator<Item = &'static Owner> {
    GLOBALS.iter().filter_map(|global| match global {
        Global::Owner(owner) => Some(owner),
        Global::Number(_) => None,
    })
}

pub(crate) fn intrinsic_groups() -> impl Iterator<Item = &'static [IntrinsicDef]> {
    owners()
        .flat_map(|owner| owner.groups.iter().copied())
        .chain([exec::FUNCTION_INTRINSICS, ITERATOR_INTRINSICS])
}

pub(crate) fn root_groups() -> impl Iterator<Item = &'static [RootSpec]> {
    let (object, object_rest) = object_members::ROOTS.split_at(1);
    let (function, function_rest) = exec::FUNCTION_ROOTS.split_at(1);
    [object, function, function_rest]
        .into_iter()
        .chain(owners().map(move |owner| {
            if owner.root == object_members::ROOT {
                object_rest
            } else {
                owner.roots
            }
        }))
}

impl Vm {
    pub(crate) fn builtin_value(&self, index: u32) -> Result<Value, VmError> {
        match GLOBALS.get(index as usize) {
            Some(Global::Owner(owner)) => Ok(Value::Object(self.builtins.get(owner.root))),
            Some(Global::Number(number)) => Ok(Value::Number(*number)),
            None => Err(VmError::InvalidState(format!(
                "builtin {index} does not exist"
            ))),
        }
    }

    pub(crate) fn init_builtins(&mut self) {
        self.allocate_builtin_roots();
        let function_prototype = self.builtins.get("function.prototype");
        let to_string =
            self.alloc_intrinsic(IntrinsicKey::new("Function", Slot::Prototype, "toString"));
        self.heap
            .define_data(
                function_prototype,
                "toString",
                Value::Object(to_string),
                Attributes::METHOD,
            )
            .expect("Function.prototype stores toString");
        let has_instance = self.alloc_intrinsic(exec::FUNCTION_HAS_INSTANCE);
        super::define_builtin_data(
            self,
            function_prototype,
            PropertyKey::Symbol(HAS_INSTANCE_SYMBOL),
            Value::Object(has_instance),
            Attributes::FIXED,
        );
        let identity = Value::Object(self.alloc_intrinsic(ITERATOR_IDENTITY));
        for owner in owners() {
            if let Some(install) = owner.install {
                install(self, &identity);
            }
        }
        let global_this = self.builtins.get(GLOBAL_THIS);
        for (index, name) in BUILTINS.into_iter().enumerate() {
            let value = self.builtin_value(index as u32).expect("global builtin");
            self.heap
                .define_data(global_this, name, value, Attributes::ORDINARY)
                .expect("global builtin property");
        }
        self.heap
            .define_data(
                global_this,
                "undefined",
                Value::Undefined,
                Attributes::ORDINARY,
            )
            .expect("global undefined property");
    }
}
