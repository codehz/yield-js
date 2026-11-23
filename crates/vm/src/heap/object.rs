use std::collections::BTreeSet;

use super::store::lookup_tables;
use super::types::{
    AbortStep, ArrayBufferObject, BoundObject, BoxedPrimitive, DataViewObject, DisposableResource,
    EnvironmentObject, EventTargetState, FunctionObject, IntegrityLevel, NativeObject, Object,
    OwnProperty, PromiseCallback, PromiseObject, PromiseState, Property, PropertyTables,
    PropertyTablesMut, Resolution, TextDecoderObject, TypedArrayObject, array_index,
    collect_own_keys,
};
use crate::ops::canonical_index_key;
use crate::value::{
    ContinuationId, JsString, ObjectId, PropertyKey, PropertyKeyRef, SymbolId, Value, ValueRef,
};
use serde::{Deserialize, Serialize};

/// Why an object id must be an environment. The role selects the restore error.
#[derive(Clone, Copy)]
pub(crate) enum EnvRole {
    Closure,
    Parent,
}

/// Owned edge so a walker can drop the source object and then query the table.
#[derive(Clone, Copy)]
pub(crate) enum OwnedEdge {
    Object(ObjectId),
    Accessor(ObjectId),
    Prototype(ObjectId),
    Environment { id: ObjectId, role: EnvRole },
    Continuation(ContinuationId),
    Symbol(SymbolId),
}

pub(crate) fn string_own_property(
    text: &JsString,
    key: PropertyKeyRef<'_>,
) -> Option<OwnProperty<'static>> {
    if crate::perf::ENABLED {
        crate::perf::record(|counters| counters.property_lookups += 1);
    }
    lookup_string_property(text, key)
}

fn lookup_string_property(
    text: &JsString,
    key: PropertyKeyRef<'_>,
) -> Option<OwnProperty<'static>> {
    if key.is_str("length") {
        return Some(OwnProperty::StringLength(text.unit_len()));
    }
    text.unit_at(canonical_index_key(key)?)
        .map(OwnProperty::StringIndex)
}

/// One waiter on a promise. `Await` is the original continuation reaction.
/// The other variants are script handlers and adoption.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReactionJob {
    Await(ContinuationId),
    /// `then` / `catch`. A missing handler passes the settlement through.
    Script {
        resolve: Value,
        reject: Value,
        on_fulfilled: Option<Value>,
        on_rejected: Option<Value>,
    },
    /// Copy the source settlement onto `result`; `result.adopting` points back here.
    Adopt {
        result: ObjectId,
    },
    /// An async generator already finished its body and is assimilating the
    /// queue head's `return` argument. The request settles with `done: true`
    /// once the awaited value is known.
    GeneratorClose {
        generator: ObjectId,
        request: ObjectId,
    },
    DisposeNext {
        state: ObjectId,
        result: ObjectId,
    },
    ArrayFromAsync(crate::builtin::array::ArrayFromAsyncJob),
    Streams(crate::builtin::streams::StreamJob),
}

/// Project a field of the ordinary-object record an object kind stores.
///
/// `$storage` binds the record (`PlainObject` for the plain-carrying kinds), so
/// `$projection` always reads the same field names. `None` means the kind has
/// no ordinary-object storage at all. This is the single list of kinds that
/// answers "where does this object keep its prototype, extensibility, and
/// property tables"; adding a variant without deciding this fails to compile.
#[rustfmt::skip]
macro_rules! map_object_storage {
    ($receiver:expr, |$storage:ident| $projection:expr) => {
        match $receiver {
            Object::Plain($storage)
            | Object::Boxed { object: $storage, .. }
            | Object::RegExp { object: $storage, .. }
            | Object::Proxy { object: $storage, .. }
            | Object::ProxyRevoker { object: $storage, .. }
            | Object::Map { object: $storage, .. }
            | Object::Date { object: $storage, .. }
            | Object::DomException { object: $storage, .. }
            | Object::Event { object: $storage, .. }
            | Object::EventTarget { object: $storage, .. }
            | Object::Performance { object: $storage, .. }
            | Object::AbortController { object: $storage, .. }
            | Object::AbortSignal { object: $storage, .. }
            | Object::ArrayBuffer(ArrayBufferObject { object: $storage, .. })
            | Object::TypedArray(TypedArrayObject { object: $storage, .. })
            | Object::DataView(DataViewObject { object: $storage, .. })
            | Object::TextEncoder { object: $storage }
            | Object::TextDecoder(TextDecoderObject { object: $storage, .. })
            | Object::QueuingStrategy { object: $storage, .. }
            | Object::ReadableStream(super::types::ReadableStreamObject { object: $storage, .. })
            | Object::ReadableReader(super::types::ReadableReaderObject { object: $storage, .. })
            | Object::WritableStream(super::types::WritableStreamObject { object: $storage, .. })
            | Object::WritableWriter(super::types::WritableWriterObject { object: $storage, .. })
            | Object::WritableController { object: $storage, .. }
            | Object::TransformStream(super::types::TransformStreamObject { object: $storage, .. })
            | Object::TransformController { object: $storage, .. }
            | Object::StreamIterator(crate::builtin::streams::iteration::StreamIterator { object: $storage, .. })
            | Object::ReadableController { object: $storage, .. }
            | Object::ReadableByobRequest { object: $storage, .. }
            | Object::Url { object: $storage, .. }
            | Object::UrlSearchParams { object: $storage, .. }
            | Object::UrlSearchParamsIterator { object: $storage, .. }
            | Object::UrlPattern { object: $storage, .. }
            | Object::Set { object: $storage, .. }
            | Object::WeakMap { object: $storage, .. }
            | Object::WeakSet { object: $storage, .. }
            | Object::CollectionIterator { object: $storage, .. }
            | Object::ArrayIterator { object: $storage, .. }
            | Object::IteratorHelper { object: $storage, .. }
            | Object::IteratorWrapper { object: $storage, .. }
            | Object::StringIterator { object: $storage, .. }
            | Object::RegExpStringIterator { object: $storage, .. }
            | Object::Bound(BoundObject { object: $storage, .. })
            | Object::PromiseCallback { object: $storage, .. }
            | Object::Resolution(Resolution { object: $storage, .. })
            | Object::DisposableStack { object: $storage, .. }
            | Object::Native(
                NativeObject::Intrinsic { object: $storage, .. }
                | NativeObject::Host { object: $storage, .. },
            ) => Some($projection),
            Object::Array($storage) => Some($projection),
            Object::Function($storage) => Some($projection),
            Object::Promise($storage) => Some($projection),
            Object::Environment(_) | Object::ForInIterator { .. } | Object::DisposeState(_)
            | Object::StreamPipe(_) | Object::Tee(_) | Object::StreamFromIterator(_) => None,
        }
    };
}

impl Object {
    pub(crate) fn event_target_state(&self) -> Option<&EventTargetState> {
        match self {
            Self::EventTarget { target, .. }
            | Self::AbortSignal { target, .. }
            | Self::Performance { target, .. } => Some(target),
            _ => None,
        }
    }

    pub(crate) fn prune_weak_abort_edges(&mut self, live: &BTreeSet<ObjectId>) {
        if let Self::AbortSignal { signal, .. } = self {
            signal.sources.retain(|id| live.contains(id));
            signal.dependents.retain(|id| live.contains(id));
            signal.pipe_steps.retain(|id| live.contains(id));
            signal.steps.retain(|step| match step {
                AbortStep::Listener { target, .. } => live.contains(target),
                AbortStep::Operation(_) => true,
            });
        }
    }

    pub(crate) fn has_error_brand(&self) -> bool {
        matches!(self, Self::DomException { .. })
            || matches!(self, Self::Plain(object) if object.is_error)
    }

    pub fn as_promise(&self) -> Option<&PromiseObject> {
        match self {
            Self::Promise(promise) => Some(promise),
            _ => None,
        }
    }

    pub fn as_promise_mut(&mut self) -> Option<&mut PromiseObject> {
        match self {
            Self::Promise(promise) => Some(promise),
            _ => None,
        }
    }

    pub fn as_function(&self) -> Option<&FunctionObject> {
        match self {
            Self::Function(function) => Some(function),
            _ => None,
        }
    }

    pub fn as_environment(&self) -> Option<&EnvironmentObject> {
        match self {
            Self::Environment(environment) => Some(environment),
            _ => None,
        }
    }

    pub fn as_environment_mut(&mut self) -> Option<&mut EnvironmentObject> {
        match self {
            Self::Environment(environment) => Some(environment),
            _ => None,
        }
    }

    fn extensible_mut(&mut self) -> Option<&mut bool> {
        map_object_storage!(self, |o| &mut o.extensible)
    }

    pub(crate) fn is_extensible(&self) -> bool {
        map_object_storage!(self, |o| o.extensible).unwrap_or(false)
    }

    pub(crate) fn prevent_extensions(&mut self) {
        if let Some(extensible) = self.extensible_mut() {
            *extensible = false;
        }
    }

    pub(crate) fn set_integrity_level(&mut self, level: IntegrityLevel) {
        self.prevent_extensions();
        if let Some((props, symbols)) = self.tables_mut() {
            props.restrict(level);
            symbols.restrict(level);
        }
        if let Self::Array(array) = self {
            for (property, attributes) in array.elements.values_mut() {
                level.restrict(property, attributes);
            }
            if matches!(level, IntegrityLevel::Frozen) {
                array.length_writable = false;
            }
        }
    }

    pub(crate) fn test_integrity_level(&self, level: IntegrityLevel) -> bool {
        if self.is_extensible() {
            return false;
        }
        if let Self::Array(array) = self
            && (matches!(level, IntegrityLevel::Frozen) && array.length_writable
                || !array.elements.values().all(|(p, a)| level.allows(p, *a)))
        {
            return false;
        }
        self.tables()
            .is_none_or(|(props, symbols)| props.satisfies(level) && symbols.satisfies(level))
    }

    pub(super) fn tables(&self) -> Option<PropertyTables<'_>> {
        map_object_storage!(self, |o| (&o.props, &o.symbol_props))
    }

    pub(super) fn tables_mut(&mut self) -> Option<PropertyTablesMut<'_>> {
        map_object_storage!(self, |o| (&mut o.props, &mut o.symbol_props))
    }

    fn boxed_string(&self) -> Option<&JsString> {
        match self {
            Self::Boxed {
                primitive: BoxedPrimitive::String(text),
                ..
            } => Some(text),
            _ => None,
        }
    }

    pub(super) fn string_own_property(
        &self,
        key: PropertyKeyRef<'_>,
    ) -> Option<OwnProperty<'static>> {
        lookup_string_property(self.boxed_string()?, key)
    }

    pub(crate) fn own_property(&self, key: PropertyKeyRef<'_>) -> Option<OwnProperty<'_>> {
        if let Self::Array(array) = self {
            if key.is_str("length") {
                if crate::perf::ENABLED {
                    crate::perf::record(|counters| counters.property_lookups += 1);
                }
                return Some(OwnProperty::ArrayLength(
                    array.length,
                    array.length_writable,
                ));
            }
            if let Some(index) = array_index(key) {
                return array.own_index(index);
            }
        }
        if crate::perf::ENABLED {
            crate::perf::record(|counters| counters.property_lookups += 1);
        }
        if let Some(property) = self.string_own_property(key) {
            return Some(property);
        }
        let (props, symbols) = self.tables()?;
        lookup_tables(props, symbols, key).map(|p| OwnProperty::Stored(&p.value, p.attributes))
    }

    pub(crate) fn own_keys(
        &self,
        enumerable_strings_only: bool,
    ) -> impl Iterator<Item = PropertyKey> + '_ {
        let mut keys = Vec::new();
        if let Some((props, symbols)) = self.tables() {
            collect_own_keys(&mut keys, props, symbols, enumerable_strings_only);
        }
        let string_length = self.boxed_string().map(JsString::unit_len);
        if string_length.is_some() && !enumerable_strings_only {
            keys.push(((1, 0), PropertyKeyRef::String("length")));
        }
        if matches!(self, Self::Plain(object) if object.module_namespace.is_some()) {
            // Module namespace export names are ordered by UTF-16 code units,
            // including names that ordinary objects would treat as array indices.
            keys.sort_unstable_by_key(|&(_, key)| key);
        } else {
            keys.sort_unstable_by_key(|&(order, _)| order);
        }
        let array = match self {
            Self::Array(array) => Some(array),
            _ => None,
        };
        array
            .into_iter()
            .flat_map(move |array| {
                array
                    .elements
                    .iter()
                    .filter(move |(_, (_, attributes))| {
                        !enumerable_strings_only || attributes.enumerable
                    })
                    .map(|(index, _)| PropertyKey::String(index.to_string().into()))
            })
            .chain(
                array
                    .filter(|_| !enumerable_strings_only)
                    .map(|_| "length".into()),
            )
            .chain(
                (0..string_length.unwrap_or(0))
                    .map(|index| PropertyKey::String(index.to_string().into())),
            )
            .chain(keys.into_iter().map(|(_, key)| key.to_owned()))
    }

    pub(super) fn valid_property_tables(&self) -> bool {
        if let Some((props, symbols)) = self.tables()
            && (!props.valid_order() || !symbols.valid_order())
        {
            return false;
        }
        match self {
            Self::Array(array) => {
                array.length <= u32::MAX as usize
                    && array
                        .elements
                        .last_key_value()
                        .is_none_or(|(&index, _)| index < array.length)
                    && array.props.iter().all(|(key, _)| {
                        let key = PropertyKeyRef::JsString(key);
                        !key.is_str("length") && array_index(key).is_none()
                    })
            }
            Self::TypedArray(view) => {
                view.object.props.iter().all(|(key, _)| {
                    !super::binary::is_canonical_index(PropertyKeyRef::JsString(key))
                })
            }
            Self::Boxed {
                primitive: BoxedPrimitive::String(text),
                object,
            } => {
                let length = text.unit_len();
                object.props.iter().all(|(key, _)| {
                    let key = PropertyKeyRef::JsString(key);
                    !key.is_str("length")
                        && canonical_index_key(key).is_none_or(|index| index >= length)
                })
            }
            _ => true,
        }
    }

    pub(crate) fn prototype(&self) -> Option<ObjectId> {
        map_object_storage!(self, |o| o.prototype).flatten()
    }

    pub(super) fn prototype_mut(&mut self) -> Option<&mut Option<ObjectId>> {
        map_object_storage!(self, |o| &mut o.prototype)
    }

    /// Replace `out` with this object's outgoing strong references.
    pub(crate) fn write_edges(&self, out: &mut Vec<OwnedEdge>) {
        out.clear();
        if let Some(id) = self.prototype() {
            out.push(OwnedEdge::Prototype(id));
        }
        if let Some((props, symbols)) = self.tables() {
            for (_, property) in props.iter() {
                push_property(out, property);
            }
            for (key, property) in symbols.iter() {
                out.push(OwnedEdge::Symbol(*key));
                push_property(out, property);
            }
        }
        match self {
            Self::Proxy {
                slots: Some((target, handler)),
                ..
            } => {
                out.push(OwnedEdge::Object(*target));
                out.push(OwnedEdge::Object(*handler));
            }
            Self::ProxyRevoker {
                proxy: Some(proxy), ..
            } => out.push(OwnedEdge::Object(*proxy)),
            Self::Event { event, .. } => {
                match &event.payload {
                    super::EventPayload::Event => {}
                    super::EventPayload::CustomEvent { detail } => push_value(out, detail),
                    super::EventPayload::ErrorEvent { error, .. } => push_value(out, error),
                    super::EventPayload::PromiseRejectionEvent { promise, reason } => {
                        out.push(OwnedEdge::Object(*promise));
                        push_value(out, reason);
                    }
                }
                if let Some(target) = event.target {
                    out.push(OwnedEdge::Object(target));
                }
            }
            Self::Promise(promise) => {
                if let PromiseState::Fulfilled(value) | PromiseState::Rejected(value) =
                    &promise.state
                {
                    push_value(out, value);
                }
                if let Some(id) = promise.adopting {
                    out.push(OwnedEdge::Object(id));
                }
                if let Some(id) = promise.disposal {
                    out.push(OwnedEdge::Object(id));
                }
                if let Some(id) = promise.stream_dependency {
                    out.push(OwnedEdge::Object(id));
                }
                for job in &promise.reactions {
                    job.write_edges(out);
                }
            }
            Self::DisposableStack { records, .. } => {
                for record in records {
                    record.write_edges(out);
                }
            }
            Self::DisposeState(state) => {
                for record in &state.records {
                    record.write_edges(out);
                }
                if let Some(error) = &state.error {
                    push_value(out, error);
                }
                for id in state.result.into_iter().chain(state.waiting) {
                    out.push(OwnedEdge::Object(id));
                }
            }
            Self::PromiseCallback { callback, .. } => match callback {
                PromiseCallback::Executor(Some((resolve, reject))) => {
                    push_value(out, resolve);
                    push_value(out, reject);
                }
                PromiseCallback::Finally {
                    handler, species, ..
                } => {
                    push_value(out, handler);
                    push_value(out, species);
                }
                PromiseCallback::Restore { value, .. } => push_value(out, value),
                PromiseCallback::CombineElement { state, .. } => {
                    out.push(OwnedEdge::Object(*state))
                }
                PromiseCallback::Executor(None) => {}
            },
            Self::Resolution(resolution) => {
                out.push(OwnedEdge::Object(resolution.promise));
                if resolution.pair != 0 {
                    out.push(OwnedEdge::Object(resolution.pair));
                }
            }
            Self::Boxed { primitive, .. } => {
                if let BoxedPrimitive::Symbol(symbol) = primitive {
                    out.push(OwnedEdge::Symbol(*symbol));
                }
            }
            Self::EventTarget { target, .. }
            | Self::AbortSignal { target, .. }
            | Self::Performance { target, .. } => {
                for listener in &target.listeners {
                    out.push(OwnedEdge::Object(listener.callback));
                    if let Some(signal) = listener.signal {
                        out.push(OwnedEdge::Object(signal));
                    }
                }
                if let Self::AbortSignal { signal, .. } = self
                    && let Some(reason) = &signal.reason
                {
                    push_value(out, reason);
                }
            }
            Self::AbortController { signal, .. } => out.push(OwnedEdge::Object(*signal)),
            Self::Url { params, .. } | Self::UrlSearchParams { owner: params, .. } => {
                if let Some(id) = params {
                    out.push(OwnedEdge::Object(*id));
                }
            }
            Self::UrlSearchParamsIterator { params, .. } => out.push(OwnedEdge::Object(*params)),
            Self::ReadableController { stream, .. } => out.push(OwnedEdge::Object(*stream)),
            Self::WritableController { stream, .. } => out.push(OwnedEdge::Object(*stream)),
            Self::TransformController { stream, .. } => out.push(OwnedEdge::Object(*stream)),
            Self::TransformStream(s) => {
                out.extend(
                    [s.readable, s.writable, s.controller, s.backpressure_change]
                        .into_iter()
                        .chain(s.write)
                        .chain(s.finish)
                        .chain(s.write_wait.map(|w| w.0))
                        .chain(s.finish_wait.map(|w| w.0))
                        .map(OwnedEdge::Object),
                );
                push_value(out, &s.finish_reason);
                if let Some(a) = &s.algorithms {
                    for value in [&a.transform, &a.flush, &a.cancel]
                        .into_iter()
                        .flatten()
                        .chain([&a.receiver])
                    {
                        push_value(out, value);
                    }
                }
            }
            Self::StreamPipe(p) => p.write_edges(out),
            Self::Tee(t) => t.write_edges(out),
            Self::StreamIterator(i) => i.write_edges(out),
            Self::StreamFromIterator(i) => i.write_edges(out),
            Self::WritableWriter(writer) => {
                out.extend(
                    [Some(writer.ready), Some(writer.closed), writer.stream]
                        .into_iter()
                        .flatten()
                        .map(OwnedEdge::Object),
                );
            }
            Self::WritableStream(s) => {
                use super::types::{WritableFlight, WritableState};
                out.extend(
                    [
                        Some(s.controller),
                        Some(s.signal),
                        s.writer,
                        s.close,
                        s.abort.as_ref().map(|a| a.promise),
                        s.start_wait,
                        s.flight_wait,
                        match s.flight {
                            Some(WritableFlight::Abort(p)) => Some(p),
                            _ => None,
                        },
                    ]
                    .into_iter()
                    .flatten()
                    .map(OwnedEdge::Object),
                );
                if let WritableState::Erroring(reason) | WritableState::Errored(reason) = &s.state {
                    push_value(out, reason);
                }
                for request in &s.queue {
                    out.push(OwnedEdge::Object(request.promise));
                    push_value(out, &request.chunk);
                }
                push_value(out, &s.sink.receiver);
                for value in [&s.sink.write, &s.sink.close, &s.sink.abort, &s.sink.size]
                    .into_iter()
                    .flatten()
                {
                    push_value(out, value);
                }
            }
            Self::ReadableByobRequest {
                controller, view, ..
            } => {
                out.extend(
                    [*controller, *view]
                        .into_iter()
                        .flatten()
                        .map(OwnedEdge::Object),
                );
            }
            Self::ReadableReader(reader) => {
                out.extend(
                    reader
                        .stream
                        .into_iter()
                        .chain([reader.closed])
                        .chain(reader.iterator)
                        .chain(reader.requests.iter().copied())
                        .map(OwnedEdge::Object),
                );
            }
            Self::ReadableStream(stream) => {
                out.extend(
                    [
                        Some(stream.controller),
                        stream.reader,
                        stream.start_wait,
                        stream.pull_wait,
                    ]
                    .into_iter()
                    .flatten()
                    .map(OwnedEdge::Object),
                );
                if let super::types::ReadableState::Errored(reason) = &stream.state {
                    push_value(out, reason);
                }
                for (chunk, _) in &stream.queue {
                    push_value(out, chunk);
                }
                if let Some(byte) = &stream.byte {
                    out.extend(byte.byob_request.into_iter().map(OwnedEdge::Object));
                    out.extend(
                        byte.queue
                            .iter()
                            .map(|range| OwnedEdge::Object(range.buffer)),
                    );
                    for descriptor in &byte.pending {
                        out.push(OwnedEdge::Object(descriptor.range.buffer));
                        out.extend(descriptor.request.into_iter().map(OwnedEdge::Object));
                    }
                }
                for value in [&stream.size, &stream.pull, &stream.cancel]
                    .into_iter()
                    .flatten()
                    .chain([&stream.source])
                {
                    push_value(out, value);
                }
            }
            Self::Native(_)
            | Self::Plain(_)
            | Self::Proxy { slots: None, .. }
            | Self::ProxyRevoker { proxy: None, .. }
            | Self::RegExp { .. }
            | Self::Date { .. }
            | Self::DomException { .. }
            | Self::TextEncoder { .. }
            | Self::TextDecoder(_)
            | Self::QueuingStrategy { .. }
            | Self::UrlPattern { .. }
            | Self::WeakMap { .. }
            | Self::WeakSet { .. }
            | Self::StringIterator { .. } => {}
            // Byte content is not a heap edge; only the buffer object is.
            Self::ArrayBuffer(_) => {}
            Self::RegExpStringIterator { regexp, .. } => out.push(OwnedEdge::Object(*regexp)),
            // Views name their buffer; the bytes themselves have no GC edges.
            Self::TypedArray(TypedArrayObject { buffer, .. })
            | Self::DataView(DataViewObject { buffer, .. }) => {
                out.push(OwnedEdge::Object(*buffer));
            }
            Self::Map { entries, .. } => {
                for (key, value) in entries.iter().flatten() {
                    push_value(out, key);
                    push_value(out, value);
                }
            }
            Self::Set { elements, .. } => {
                for value in elements.iter().flatten() {
                    push_value(out, value);
                }
            }
            Self::CollectionIterator { target, .. } => {
                if let Some(id) = target {
                    out.push(OwnedEdge::Object(*id));
                }
            }
            Self::ArrayIterator { array, .. } => {
                if let Some(id) = array {
                    out.push(OwnedEdge::Object(*id));
                }
            }
            Self::IteratorHelper { state, .. } => {
                for value in state.values() {
                    push_value(out, value);
                }
            }
            Self::IteratorWrapper { record, .. } => {
                for value in record.values() {
                    push_value(out, value);
                }
            }
            Self::ForInIterator { current, .. } => {
                if let Some(id) = current {
                    out.push(OwnedEdge::Object(*id));
                }
            }
            Self::Array(array) => {
                for (property, _) in array.elements.values() {
                    push_property(out, property);
                }
            }
            Self::Function(function) => {
                out.push(OwnedEdge::Environment {
                    id: function.env,
                    role: EnvRole::Closure,
                });
                if let Some(home) = function.home {
                    out.push(OwnedEdge::Object(home));
                }
                if let Some(class) = &function.class {
                    out.push(OwnedEdge::Object(class.prototype));
                    for element in &class.source.elements {
                        if let Some(closure) = element.closure() {
                            out.push(OwnedEdge::Object(closure));
                        }
                        if let super::types::ClassElement::Field {
                            key: PropertyKey::Symbol(symbol),
                            ..
                        } = element
                        {
                            out.push(OwnedEdge::Symbol(*symbol));
                        }
                    }
                    for member in class.source.private.values() {
                        use super::types::PrivateMemberKind;
                        match member.kind {
                            PrivateMemberKind::Field => {}
                            PrivateMemberKind::Method(id) => out.push(OwnedEdge::Object(id)),
                            PrivateMemberKind::Accessor { get, set } => {
                                out.extend(get.into_iter().chain(set).map(OwnedEdge::Accessor));
                            }
                        }
                    }
                }
            }
            Self::Bound(bound) => {
                out.push(OwnedEdge::Object(bound.target));
                push_value(out, &bound.bound_this);
                for arg in &bound.args {
                    push_value(out, arg);
                }
            }
            Self::Environment(environment) => {
                if let Some(id) = environment.parent {
                    out.push(OwnedEdge::Environment {
                        id,
                        role: EnvRole::Parent,
                    });
                }
                for value in environment.bindings.values() {
                    push_value(out, value);
                }
            }
        }
    }
}

fn push_property(out: &mut Vec<OwnedEdge>, property: &Property) {
    match property {
        Property::Data(value) => push_value(out, value),
        Property::Accessor { get, set } => {
            out.extend([*get, *set].into_iter().flatten().map(OwnedEdge::Accessor));
        }
    }
}

pub(crate) fn push_value(out: &mut Vec<OwnedEdge>, value: &Value) {
    match value.view() {
        ValueRef::Object(id) => out.push(OwnedEdge::Object(id)),
        ValueRef::Symbol(id) => out.push(OwnedEdge::Symbol(id)),
        _ => {}
    }
}

impl DisposableResource {
    fn write_edges(&self, out: &mut Vec<OwnedEdge>) {
        match self {
            Self::Method { value, method, .. } | Self::Adopt { value, method, .. } => {
                push_value(out, value);
                push_value(out, method);
            }
            Self::Defer { method, .. } => push_value(out, method),
            Self::AsyncEmpty => {}
        }
    }
}

impl ReactionJob {
    pub(crate) fn write_edges(&self, out: &mut Vec<OwnedEdge>) {
        match self {
            Self::Await(id) => out.push(OwnedEdge::Continuation(*id)),
            Self::Script {
                resolve,
                reject,
                on_fulfilled,
                on_rejected,
            } => {
                push_value(out, resolve);
                push_value(out, reject);
                if let Some(value) = on_fulfilled {
                    push_value(out, value);
                }
                if let Some(value) = on_rejected {
                    push_value(out, value);
                }
            }
            Self::Adopt { result } => {
                out.push(OwnedEdge::Object(*result));
            }
            Self::GeneratorClose { generator, request } => {
                out.push(OwnedEdge::Object(*generator));
                out.push(OwnedEdge::Object(*request));
            }
            Self::DisposeNext { state, result } => {
                out.push(OwnedEdge::Object(*state));
                out.push(OwnedEdge::Object(*result));
            }
            Self::ArrayFromAsync(job) => job.write_edges(out),
            Self::Streams(job) => out.push(OwnedEdge::Object(job.target())),
        }
    }

    pub(crate) fn await_id(&self) -> Option<ContinuationId> {
        match self {
            Self::Await(id) => Some(*id),
            _ => None,
        }
    }
}
