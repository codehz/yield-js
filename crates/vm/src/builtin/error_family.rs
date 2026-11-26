use super::{FUNCTION_PARENT, OBJECT_PARENT, define_builtin_data, link_constructor};
use crate::builtin::globals::Owner;
use crate::builtin::roots::{RootKind, RootSpec};
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::Attributes;
use crate::host::{Host, PlatformServices};
use crate::intrinsic::{ClassArgs, ClassDef, ClassPrototype, IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::Invocation;
use crate::value::{JsString, PropertyKeyRef, Value, ValueRef};
use crate::vm::VmCore;

/// `Error.prototype.toString` is shared by every error class through the
/// prototype chain, so it is registered under the `error.prototype` owner.
const PROTOTYPE_TO_STRING: IntrinsicKey =
    IntrinsicKey::new("error.prototype", Slot::Prototype, "toString");
const IS_ERROR: IntrinsicKey = IntrinsicKey::new("Error", Slot::Static, "isError");
impl<P: crate::host::PlatformServices> BuiltinTable<P> {
    const ERROR_STATIC_INTRINSICS: &[IntrinsicDef<P>] =
        &[IntrinsicDef::call(IS_ERROR, 1, |vm, _, _, args, _| {
            let branded = match args.first().map(Value::view) {
                Some(ValueRef::Object(id)) => vm.heap.get(id)?.has_error_brand(),
                _ => false,
            };
            Ok(Invocation::Value(Value::boolean(branded)))
        })];
}
impl<P: crate::host::PlatformServices> BuiltinTable<P> {
    const ERROR_PROTOTYPE_INTRINSICS: &[IntrinsicDef<P>] = &[IntrinsicDef::call(
        PROTOTYPE_TO_STRING,
        0,
        |vm, host, roots, _, receiver| vm.error_to_string(host, roots, receiver),
    )];
}
impl<P: crate::host::PlatformServices> BuiltinTable<P> {
    const NO_PROTOTYPE_INTRINSICS: &[IntrinsicDef<P>] = &[];
}

impl VmCore {
    fn error_to_string<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
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
    ($owner:ident, $name:literal, $root:literal, $length:literal, $initialize:ident) => {
        error_class!(
            $owner,
            $name,
            $root,
            Some("error.prototype"),
            $length,
            $initialize,
            Some("error"),
            NO_PROTOTYPE_INTRINSICS,
            NO_PROTOTYPE_INTRINSICS
        );
    };
    ($owner:ident, $name:literal, $root:literal, $parent:expr, $length:literal, $initialize:ident, $constructor_parent:expr, $prototype_intrinsics:ident, $static_intrinsics:ident) => {
        impl<P: crate::host::PlatformServices> BuiltinTable<P> {
            pub(crate) const $owner: Owner<P> = {
                const PROTOTYPE: &str = concat!($root, ".prototype");
                const KEY: IntrinsicKey = IntrinsicKey::new($name, Slot::Global, "");
                const CLASS: &ClassDef = &ClassDef {
                    root: $root,
                    prototype: PROTOTYPE,
                    key: KEY,
                    valid_instance: |object| matches!(object, crate::heap::Object::Plain(plain) if plain.is_error),
                    instance_description: "a branded Error",
                };
                Owner::new(
                    $root,
                    &[
                        RootSpec::new($root, RootKind::Native(KEY), $constructor_parent)
                            .fixed(PROTOTYPE)
                            .backlink(Some(Attributes::METHOD)),
                        RootSpec::new(PROTOTYPE, RootKind::Plain, $parent),
                    ],
                    &[
                        &[IntrinsicDef::class(
                            CLASS,
                            $length,
                            |vm, host, roots, args, _| {
                                let prototype = vm.builtins.get(PROTOTYPE);
                                vm.$initialize(host, &roots.with(&args), ClassPrototype::Resolved(prototype), $name, ClassArgs::Values(&args))
                            },
                            VmCore::$initialize,
                        )],
                        Self::$prototype_intrinsics,
                        Self::$static_intrinsics,
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
                        define_builtin_data(
                            vm,
                            prototype,
                            "message",
                            Value::string(JsString::default()),
                            Attributes::METHOD,
                        );
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
                .with_class(CLASS)
            };
        }
    };
}

error_class!(
    ERROR,
    "Error",
    "error",
    OBJECT_PARENT,
    1,
    initialize_error,
    FUNCTION_PARENT,
    ERROR_PROTOTYPE_INTRINSICS,
    ERROR_STATIC_INTRINSICS
);
error_class!(TYPE_ERROR, "TypeError", "typeerror", 1, initialize_error);
error_class!(RANGE_ERROR, "RangeError", "rangeerror", 1, initialize_error);
error_class!(
    REFERENCE_ERROR,
    "ReferenceError",
    "referenceerror",
    1,
    initialize_error
);
error_class!(
    SYNTAX_ERROR,
    "SyntaxError",
    "syntaxerror",
    1,
    initialize_error
);
error_class!(EVAL_ERROR, "EvalError", "evalerror", 1, initialize_error);
error_class!(URI_ERROR, "URIError", "urierror", 1, initialize_error);
error_class!(
    AGGREGATE_ERROR,
    "AggregateError",
    "aggregateerror",
    2,
    initialize_aggregate_error
);
error_class!(
    SUPPRESSED_ERROR,
    "SuppressedError",
    "suppressederror",
    3,
    initialize_suppressed_error
);

pub(crate) struct BuiltinTable<P>(std::marker::PhantomData<P>);
