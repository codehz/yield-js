//! Object table. Strong edges are listed by [`Object::write_edges`] plus
//! combine-state edges appended by [`Heap::write_edges`]. Snapshot capture and
//! restore handle weak collection entries separately from strong edges.
//!
//! Script property tables store data or accessor properties. Only a missing
//! key walks the prototype chain; environments and native handles are opaque.

mod binary;
mod object;
mod store;
mod types;

pub(crate) use binary::{OwnPropertyLookup, TypedIndex};
pub use object::ReactionJob;
pub(crate) use object::{EnvRole, OwnedEdge, push_value, string_own_property};
pub(crate) use store::{Heap, object_id};
pub(crate) use types::ViewRange;
pub(crate) use types::view_range;
pub(crate) use types::{
    ASYNC_DISPOSE_DESCRIPTION, ASYNC_DISPOSE_SYMBOL, ASYNC_ITERATOR_DESCRIPTION,
    ASYNC_ITERATOR_SYMBOL, Attributes, ClassElement, DISPOSE_DESCRIPTION, DISPOSE_SYMBOL,
    DescriptorUpdate, HAS_INSTANCE_DESCRIPTION, HAS_INSTANCE_SYMBOL, HostClassRecord,
    IS_CONCAT_SPREADABLE_DESCRIPTION, IS_CONCAT_SPREADABLE_SYMBOL, ITERATOR_DESCRIPTION,
    ITERATOR_SYMBOL, IntegrityLevel, MATCH_ALL_DESCRIPTION, MATCH_ALL_SYMBOL, MATCH_DESCRIPTION,
    MATCH_SYMBOL, OwnProperty, PrivateElement, PrivateMember, PrivateMemberKind,
    REPLACE_DESCRIPTION, REPLACE_SYMBOL, SEARCH_DESCRIPTION, SEARCH_SYMBOL, SPECIES_DESCRIPTION,
    SPECIES_SYMBOL, SPLIT_DESCRIPTION, SPLIT_SYMBOL, ScriptClass, ScriptClassSource,
    TO_PRIMITIVE_DESCRIPTION, TO_PRIMITIVE_SYMBOL, TO_STRING_TAG_DESCRIPTION, TO_STRING_TAG_SYMBOL,
};
pub use types::{
    ArrayBufferObject, ArrayIterationKind, ArrayObject, AsyncCompletion, AsyncRequest, BoundObject,
    BoxedPrimitive, CollectionIteration, CollectionKind, CombineKind, CombineState, DataViewObject,
    ElementKind, EnvironmentObject, FunctionObject, GeneratorObject, GeneratorStatus, NativeObject,
    Object, PlainObject, PromiseCallback, PromiseObject, PromiseState, Property, PropertyTable,
    RejectionStatus, Resolution, TextDecoderObject, TypedArrayObject, WeakKey,
};
