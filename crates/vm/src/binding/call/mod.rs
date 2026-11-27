//! Runtime call context for typed host bindings.
//!
//! [`JsValue`] handles are valid only for the VM generation and call that
//! produced them. Slot writes are the only way to retain a value across calls.

mod alloc;
mod cx;
mod guest;
mod inspect;
mod instance;
mod module;
mod operation;
mod slots;
mod value;

pub use cx::CallCx;
pub use inspect::{Inspect, InspectKey, InspectKind, InspectValue};
pub use instance::{Instance, This};
pub use module::{ClassCx, ModuleCx, ModuleDispatch, with_class, with_module};
pub use value::JsValue;
pub(crate) use value::{HandleRoots, Origin};

use crate::heap::Attributes;

/// Attributes for prototype methods: writable and configurable, not enumerable.
pub(crate) const PROTOTYPE_METHOD_ATTRIBUTES: Attributes = Attributes::METHOD;
