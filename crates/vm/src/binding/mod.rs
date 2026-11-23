//! Static host bindings: modules, classes, values, and recoverable operations.
//!
//! Declarations are immutable and can be shared between VMs. A VM owns every
//! prototype, instance, module state, and operation record derived from them.
//! Host code never receives a heap id it can use outside the current call: a
//! [`JsValue`] carries the calling VM generation and call id, and slot writes
//! are the only way to retain a value across calls.

mod call;
mod codec;
mod schema;

pub use call::*;
pub use codec::{Decode, Encode};
pub use schema::*;
