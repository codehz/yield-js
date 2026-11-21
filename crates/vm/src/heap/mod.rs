//! Object table. Strong edges are listed by [`Object::write_edges`] plus
//! combine-state edges appended by [`Heap::write_edges`]. Snapshot capture and
//! restore handle weak collection entries separately from strong edges.
//!
//! Script property tables store data or accessor properties. Only a missing
//! key walks the prototype chain; environments and native handles are opaque.

mod object;
mod store;
mod types;

pub use object::ReactionJob;
pub(crate) use object::{EnvRole, OwnedEdge, push_value, string_own_property};
pub(crate) use store::{Heap, object_id};
pub use types::{
    ArrayObject, BoundObject, BoxedPrimitive, CollectionIteration, CollectionKind, CombineKind,
    CombineState, EnvironmentObject, FunctionObject, NativeObject, Object, PlainObject,
    PromiseCallback, PromiseObject, PromiseState, Property, PropertyTable, Resolution, WeakKey,
};
pub(crate) use types::{
    Attributes, ClassElement, DescriptorUpdate, HAS_INSTANCE_DESCRIPTION, HAS_INSTANCE_SYMBOL,
    HostClassRecord, ITERATOR_DESCRIPTION, ITERATOR_SYMBOL, IntegrityLevel, MATCH_ALL_DESCRIPTION,
    MATCH_ALL_SYMBOL, MATCH_DESCRIPTION, MATCH_SYMBOL, OwnProperty, PrivateElement, PrivateMember,
    PrivateMemberKind, REPLACE_DESCRIPTION, REPLACE_SYMBOL, SEARCH_DESCRIPTION, SEARCH_SYMBOL,
    SPECIES_DESCRIPTION, SPECIES_SYMBOL, SPLIT_DESCRIPTION, SPLIT_SYMBOL, ScriptClass,
    ScriptClassSource, TO_PRIMITIVE_DESCRIPTION, TO_PRIMITIVE_SYMBOL,
};
