use super::{FUNCTION_PARENT, OBJECT_PARENT, define_builtin_data, link_constructor};
use crate::builtin::globals::Owner;
use crate::builtin::roots::{RootKind, RootSpec};
use crate::heap::Attributes;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::value::{JsString, Value};

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
                    RootSpec::new(PROTOTYPE, RootKind::Plain, $parent).with_fixed_parent(!$base),
                ],
                &[&[IntrinsicDef::constructor(
                    KEY,
                    |vm, host, roots, args, _| {
                        vm.construct_error(host, roots, $name, PROTOTYPE, args)
                    },
                    |vm, host, roots, args, prototype| {
                        let result = vm.construct_error(host, roots, $name, PROTOTYPE, args)?;
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
                        Value::string($name.into()),
                        Attributes::METHOD,
                    );
                    if $base {
                        define_builtin_data(
                            vm,
                            prototype,
                            "message",
                            Value::string(JsString::default()),
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
