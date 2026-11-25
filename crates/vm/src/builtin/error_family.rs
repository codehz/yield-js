use super::{FUNCTION_PARENT, OBJECT_PARENT, define_builtin_data, link_constructor};
use crate::VmError;
use crate::builtin::globals::Owner;
use crate::builtin::roots::{RootKind, RootSpec};
use crate::gc::ActiveRoots;
use crate::heap::Attributes;
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::Invocation;
use crate::value::{JsString, PropertyKeyRef, Value, ValueRef};
use crate::vm::VmCore;

/// `Error.prototype.toString` is shared by every error class through the
/// prototype chain, so it is registered under the `error.prototype` owner.
const PROTOTYPE_TO_STRING: IntrinsicKey =
    IntrinsicKey::new("error.prototype", Slot::Prototype, "toString");
const IS_ERROR: IntrinsicKey = IntrinsicKey::new("Error", Slot::Static, "isError");
const ERROR_STATIC_INTRINSICS: &[IntrinsicDef] =
    &[IntrinsicDef::call(IS_ERROR, 1, |vm, _, _, args, _| {
        let branded = match args.first().map(Value::view) {
            Some(ValueRef::Object(id)) => vm.heap.get(id)?.has_error_brand(),
            _ => false,
        };
        Ok(Invocation::Value(Value::boolean(branded)))
    })];
const ERROR_PROTOTYPE_INTRINSICS: &[IntrinsicDef] = &[IntrinsicDef::call(
    PROTOTYPE_TO_STRING,
    0,
    |vm, host, roots, _, receiver| vm.error_to_string(host, roots, receiver),
)];
const NO_PROTOTYPE_INTRINSICS: &[IntrinsicDef] = &[];

impl VmCore {
    fn error_to_string(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        if matches!(receiver.view(), ValueRef::Null | ValueRef::Undefined) {
            return self.throw_type_error(
                roots,
                "Error.prototype.toString called on null or undefined",
            );
        }
        let roots = roots.with(receiver);
        let mut boxed = None;
        let name = match self.read_to_object_property(
            host,
            &roots,
            receiver,
            &mut boxed,
            PropertyKeyRef::String("name"),
        )? {
            Invocation::Value(value) => value,
            thrown => return Ok(thrown),
        };
        // Spec order: convert `name` before reading or converting `message`.
        let name = if matches!(name.view(), ValueRef::Undefined) {
            JsString::from("Error")
        } else {
            match self.coerce_string(host, &roots, &name)? {
                Ok(text) => text,
                Err(error) => return Ok(Invocation::Thrown(error)),
            }
        };
        let message = match self.read_to_object_property(
            host,
            &roots,
            receiver,
            &mut boxed,
            PropertyKeyRef::String("message"),
        )? {
            Invocation::Value(value) => value,
            thrown => return Ok(thrown),
        };
        let message = match message.view() {
            ValueRef::Undefined => JsString::default(),
            _ => match self.coerce_string(host, &roots, &message)? {
                Ok(text) => text,
                Err(error) => return Ok(Invocation::Thrown(error)),
            },
        };
        let mut text = name;
        if !message.is_empty() {
            text.push_str(&JsString::from(": "));
            text.push_str(&message);
        }
        Ok(Invocation::Value(Value::string(text)))
    }
}

macro_rules! error_class {
    ($owner:ident, $name:literal, $root:literal, $parent:expr, $base:literal, $length:literal) => {
        error_class!(
            $owner,
            $name,
            $root,
            $parent,
            $base,
            $length,
            Some("error"),
            true,
            NO_PROTOTYPE_INTRINSICS,
            &[]
        );
    };
    ($owner:ident, $name:literal, $root:literal, $parent:expr, $base:literal, $length:literal, $constructor_parent:expr, $own_message:tt, $prototype_intrinsics:expr, $static_intrinsics:expr) => {
        pub(crate) const $owner: Owner = {
            const PROTOTYPE: &str = concat!($root, ".prototype");
            const KEY: IntrinsicKey = IntrinsicKey::new($name, Slot::Global, "");
            Owner::new(
                $root,
                &[
                    RootSpec::new($root, RootKind::Native(KEY), $constructor_parent)
                        .fixed(PROTOTYPE)
                        .backlink(Some(Attributes::METHOD)),
                    RootSpec::new(PROTOTYPE, RootKind::Plain, $parent).with_fixed_parent(!$base),
                ],
                &[
                    &[IntrinsicDef::constructor(
                        KEY,
                        $length,
                        |vm, host, roots, args, _| {
                            vm.construct_error(host, roots, $name, PROTOTYPE, args)
                        },
                        |vm, host, roots, args, prototype| {
                            let result = vm.construct_error(host, roots, $name, PROTOTYPE, args)?;
                            vm.with_result_prototype(roots, result, prototype)
                        },
                    )],
                    $prototype_intrinsics,
                    $static_intrinsics,
                ],
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
                    if $own_message {
                        define_builtin_data(
                            vm,
                            prototype,
                            "message",
                            Value::string(JsString::default()),
                            Attributes::METHOD,
                        );
                    }
                    if $root == "error" {
                        let method = vm.alloc_intrinsic(IS_ERROR);
                        define_builtin_data(
                            vm,
                            constructor,
                            "isError",
                            Value::object(method),
                            Attributes::METHOD,
                        );
                        let method = vm.alloc_intrinsic(PROTOTYPE_TO_STRING);
                        define_builtin_data(
                            vm,
                            prototype,
                            "toString",
                            Value::object(method),
                            Attributes::METHOD,
                        );
                    }
                    link_constructor(vm, constructor, prototype);
                }),
            )
        };
    };
}

error_class!(
    ERROR,
    "Error",
    "error",
    OBJECT_PARENT,
    true,
    1,
    FUNCTION_PARENT,
    true,
    ERROR_PROTOTYPE_INTRINSICS,
    ERROR_STATIC_INTRINSICS
);
error_class!(
    TYPE_ERROR,
    "TypeError",
    "typeerror",
    Some("error.prototype"),
    false,
    1
);
error_class!(
    RANGE_ERROR,
    "RangeError",
    "rangeerror",
    Some("error.prototype"),
    false,
    1
);
error_class!(
    REFERENCE_ERROR,
    "ReferenceError",
    "referenceerror",
    Some("error.prototype"),
    false,
    1
);
error_class!(
    SYNTAX_ERROR,
    "SyntaxError",
    "syntaxerror",
    Some("error.prototype"),
    false,
    1
);
error_class!(
    EVAL_ERROR,
    "EvalError",
    "evalerror",
    Some("error.prototype"),
    false,
    1
);
error_class!(
    URI_ERROR,
    "URIError",
    "urierror",
    Some("error.prototype"),
    false,
    1
);
error_class!(
    AGGREGATE_ERROR,
    "AggregateError",
    "aggregateerror",
    Some("error.prototype"),
    false,
    2
);
error_class!(
    SUPPRESSED_ERROR,
    "SuppressedError",
    "suppressederror",
    Some("error.prototype"),
    false,
    3,
    Some("error"),
    true,
    NO_PROTOTYPE_INTRINSICS,
    &[]
);
