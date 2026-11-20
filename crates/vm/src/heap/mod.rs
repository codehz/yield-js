//! Object table. Strong edges are listed by [`Object::write_edges`] plus
//! combine-state edges appended by [`Heap::write_edges`]. Snapshot capture and
//! restore handle WeakMap entries separately as ephemerons.
//!
//! Script property tables store data or accessor properties. Only a missing
//! key walks the prototype chain; environments and native handles are opaque.

include!("types.rs");
include!("object.rs");
include!("store.rs");
