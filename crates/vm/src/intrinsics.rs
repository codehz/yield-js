//! Type markers for VM-owned constructors. They identify intrinsic roots,
//! independently of mutable JavaScript global bindings.

use crate::binding::{ClassParent, ParentRef};
use crate::builtin::{dom_exception, error_family};
use crate::host::Platform;
use crate::intrinsic::ClassDef;

mod sealed {
    pub(crate) trait Installed {
        const CLASS: &'static super::ClassDef;
    }
    pub(crate) trait MessageFirst {}
}

pub(crate) use sealed::Installed;

/// An intrinsic class installed by the VM.
#[allow(private_bounds)]
pub trait IntrinsicClass: ClassParent + sealed::Installed {}

/// An intrinsic Error class whose first argument is an optional message.
#[allow(private_bounds)]
pub trait MessageErrorClass: IntrinsicClass + sealed::MessageFirst {}

macro_rules! classes {
    ($($marker:ident => $family:ident::$owner:ident),+ $(,)?) => {$ (
        #[doc = concat!("Marker for the intrinsic `", stringify!($marker), "` constructor.")]
        pub enum $marker {}

        impl sealed::Installed for $marker {
            const CLASS: &'static ClassDef = match $family::BuiltinTable::<Platform>::$owner.class {
                Some(class) => class,
                None => panic!("missing intrinsic class descriptor"),
            };
        }

        impl ClassParent for $marker {
            const PARENT_REF: ParentRef = ParentRef::Intrinsic(<Self as sealed::Installed>::CLASS.root);
        }

        impl IntrinsicClass for $marker {}
    )+};
}

classes! {
    Error => error_family::ERROR,
    TypeError => error_family::TYPE_ERROR,
    RangeError => error_family::RANGE_ERROR,
    ReferenceError => error_family::REFERENCE_ERROR,
    SyntaxError => error_family::SYNTAX_ERROR,
    EvalError => error_family::EVAL_ERROR,
    URIError => error_family::URI_ERROR,
    AggregateError => error_family::AGGREGATE_ERROR,
    SuppressedError => error_family::SUPPRESSED_ERROR,
    DOMException => dom_exception::OWNER,
}

macro_rules! message_errors {
    ($($marker:ident),+ $(,)?) => {$ (
        impl sealed::MessageFirst for $marker {}
        impl MessageErrorClass for $marker {}
    )+};
}

message_errors!(
    Error,
    TypeError,
    RangeError,
    ReferenceError,
    SyntaxError,
    EvalError,
    URIError
);
