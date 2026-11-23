//! Full heap edge walk used by restore_validated.

use std::collections::{BTreeMap, BTreeSet};

use crate::builtin::is_callable_object;
use crate::bytecode::Build;
use crate::error::VmError;
use crate::heap::{AbortStep, Heap, Object, OwnProperty, OwnedEdge, PrivateMemberKind, Property};
use crate::heap::{CollectionIteration, CollectionKind, EventPayload, push_value};
use crate::value::{ObjectId, PropertyKeyRef, Value, ValueRef};

use crate::snapshot::record::valid_string_position;

fn check_abort_state(
    heap: &Heap,
    id: ObjectId,
    object: &Object,
    controller_signals: &mut BTreeSet<ObjectId>,
) -> Result<Option<u64>, VmError> {
    let Object::AbortSignal { signal, target, .. } = object else {
        if let Object::AbortController { signal, .. } = object
            && (!matches!(heap.get(*signal), Ok(Object::AbortSignal { signal, .. }) if !signal.dependent)
                || !controller_signals.insert(*signal))
        {
            return Err(VmError::InvalidSnapshot(format!(
                "abort controller {id} has invalid signal {signal}"
            )));
        }
        return Ok(None);
    };

    let invalid = |detail| VmError::InvalidSnapshot(format!("abort signal {id} has {detail}"));
    if matches!(
        signal.reason.as_ref().map(Value::view),
        Some(ValueRef::Undefined)
    ) || (signal.reason.is_some() && !signal.steps.is_empty())
    {
        return Err(invalid("invalid aborted state"));
    }
    if (!signal.dependent && !signal.sources.is_empty())
        || (signal.dependent && !signal.dependents.is_empty())
    {
        return Err(invalid("invalid dependency direction"));
    }
    let mut seen = BTreeSet::new();
    for &source in &signal.sources {
        if !seen.insert(source)
            || !matches!(heap.get(source), Ok(Object::AbortSignal { signal: source, .. })
                if !source.dependent
                    && source.dependents.contains(&id)
                    && (source.reason.is_none() || signal.reason.is_some()))
        {
            return Err(invalid("invalid source link"));
        }
    }
    seen.clear();
    for &dependent in &signal.dependents {
        if !seen.insert(dependent)
            || !matches!(heap.get(dependent), Ok(Object::AbortSignal { signal, .. })
                if signal.dependent && signal.sources.contains(&id))
        {
            return Err(invalid("invalid dependent link"));
        }
    }
    let mut listener_steps = BTreeSet::new();
    for step in &signal.steps {
        if let AbortStep::Listener {
            target,
            id: listener,
        } = *step
            && (!listener_steps.insert((target, listener))
                || !heap
                    .get(target)
                    .ok()
                    .and_then(Object::event_target_state)
                    .is_some_and(|target| {
                        target
                            .listeners
                            .iter()
                            .any(|entry| entry.id == listener && entry.signal == Some(id))
                    }))
        {
            return Err(invalid("invalid listener step"));
        }
    }
    if let Some(listener) = signal.onabort
        && !target.listeners.iter().any(|entry| {
            entry.id == listener
                && entry.ty == "abort"
                && !entry.capture
                && !entry.passive
                && !entry.once
                && entry.signal.is_none()
                && heap.get(entry.callback).is_ok_and(is_callable_object)
        })
    {
        return Err(invalid("invalid onabort listener"));
    }
    Ok(signal.onabort)
}

pub(super) fn check_heap_edges(
    build: &Build,
    heap: &Heap,
    global: ObjectId,
) -> Result<(), VmError> {
    let mut edges = Vec::new();
    let mut prototypes = BTreeMap::new();
    let mut revocable = BTreeSet::new();
    let mut controller_signals = BTreeSet::new();
    use crate::bytecode::Instr;
    let definitions = build
        .functions
        .iter()
        .flat_map(|f| &f.code)
        .filter_map(|instr| {
            if let Instr::CreateClass {
                constructor,
                heritage,
                default_constructor,
                private_count,
                private_binding,
                ..
            } = instr
            {
                Some((
                    *constructor,
                    (
                        heritage.is_some(),
                        *default_constructor,
                        *private_count,
                        *private_binding,
                    ),
                ))
            } else {
                None
            }
        })
        .collect::<BTreeMap<_, _>>();
    for id in heap.ids() {
        use crate::builtin::call::{DERIVED_CLASS, DERIVED_TARGET, DERIVED_THIS};
        if let Some(error) = binary_object_error(heap, id) {
            return Err(VmError::InvalidSnapshot(error));
        }
        if let Object::Environment(env) = heap.get(id)?
            && env.bindings.range(DERIVED_THIS..).next().is_some()
        {
            let valid_class = matches!(env.bindings.get(&DERIVED_CLASS).map(Value::view), Some(ValueRef::Object(class))
                if heap.function(class).and_then(|f| f.class.as_ref()).is_some_and(|c| c.source.derived && !c.source.default_constructor));
            let valid_target = matches!(env.bindings.get(&DERIVED_TARGET).map(Value::view), Some(ValueRef::Object(target))
                if heap.get(target).is_ok_and(crate::builtin::is_callable_object));
            let valid_this = match env.bindings.get(&DERIVED_THIS).map(Value::view) {
                None => true,
                Some(ValueRef::Object(receiver)) => !matches!(
                    heap.get(receiver),
                    Err(_)
                        | Ok(Object::Environment(_)
                            | Object::ForInIterator { .. }
                            | Object::DisposeState(_))
                ),
                _ => false,
            };
            if env.parent.is_some()
                || env.bindings.keys().any(|key| *key < DERIVED_THIS)
                || !valid_class
                || !valid_target
                || !valid_this
            {
                return Err(VmError::InvalidSnapshot(format!(
                    "invalid derived constructor context {id}"
                )));
            }
        }
        if let Object::Function(function) = heap.get(id)? {
            let code = build
                .function(function.func_id)
                .filter(|_| !build.is_initializer(function.func_id));
            let Some(code) = code else {
                return Err(VmError::InvalidSnapshot(format!(
                    "function object {id} references an invalid function"
                )));
            };
            if (code.home_param.is_some() && function.home.is_none())
                || function.home.is_some_and(|home| {
                    !matches!(heap.get(home), Ok(Object::Function(_) | Object::Plain(_)))
                })
            {
                return Err(VmError::InvalidSnapshot(format!(
                    "function object {id} has an invalid home object"
                )));
            }
            if let Some(class) = &function.class
                && (!code.is_constructable()
                    || definitions.get(&function.func_id) != Some(&(class.source.derived, class.source.default_constructor, class.source.private_count, class.source.private_binding))
                    || function.home != Some(class.prototype)
                    || class.source.private_binding.is_some_and(|binding| !matches!(heap.environment(function.env), Ok(env) if env.bindings.get(&binding) == Some(&Value::object(id))))
                    || !matches!(heap.get(class.prototype), Ok(Object::Plain(_)))
                    || !matches!(heap.get(id)?.own_property(PropertyKeyRef::String("prototype")),
                        Some(OwnProperty::Stored(Property::Data(prototype), attributes))
                            if matches!(prototype.view(), ValueRef::Object(id) if id == class.prototype)
                                && attributes == crate::heap::Attributes::FIXED))
            {
                return Err(VmError::InvalidSnapshot(format!(
                    "function object {id} has an invalid class record"
                )));
            }
            if let Some(class) = &function.class {
                let mut fields = BTreeSet::new();
                for (&index, member) in &class.source.private {
                    let home = Some(if member.is_static {
                        id
                    } else {
                        class.prototype
                    });
                    let valid_closure = |target, accessor: bool| {
                        heap.function(target).is_some_and(|f| {
                            f.home == home
                                && build.function(f.func_id).is_some_and(|code| {
                                    !build.is_initializer(code.func_id)
                                        && code.is_method
                                        && (!accessor || code.is_accessor_method())
                                })
                        })
                    };
                    let valid = match member.kind {
                        PrivateMemberKind::Field => {
                            fields.insert(index);
                            true
                        }
                        PrivateMemberKind::Method(target) => valid_closure(target, false),
                        PrivateMemberKind::Accessor { get, set } => {
                            (get.is_some() || set.is_some())
                                && get
                                    .into_iter()
                                    .chain(set)
                                    .all(|target| valid_closure(target, true))
                        }
                    };
                    if index >= class.source.private_count || !valid {
                        return Err(VmError::InvalidSnapshot(format!(
                            "class {id} has an invalid private declaration {index}"
                        )));
                    }
                }
                for element in &class.source.elements {
                    if let crate::heap::ClassElement::PrivateField { index, .. } = element
                        && !fields.remove(index)
                    {
                        return Err(VmError::InvalidSnapshot(format!(
                            "class {id} has an invalid private field {index}"
                        )));
                    }
                    if let Some(closure) = element.closure()
                        && !heap
                            .function(closure)
                            .and_then(|f| build.function(f.func_id))
                            .is_some_and(|f| {
                                !build.is_initializer(f.func_id) && f.is_accessor_method()
                            })
                    {
                        return Err(VmError::InvalidSnapshot(format!(
                            "class {id} has an invalid initializer closure"
                        )));
                    }
                }
                if !fields.is_empty() {
                    return Err(VmError::InvalidSnapshot(format!(
                        "class {id} has missing private field definitions"
                    )));
                }
            }
        }
        // The shared edge walk below checks the bound receiver and arguments.
        if let Object::Bound(bound) = heap.get(id)? {
            let callable = heap
                .get(bound.target)
                .is_ok_and(crate::builtin::is_callable_object);
            let valid = callable
                && match heap.function(bound.target) {
                    Some(function) => build.function(function.func_id).is_some(),
                    None => true,
                };
            if !valid {
                return Err(VmError::InvalidSnapshot(format!(
                    "bound target {} is missing or not a function",
                    bound.target
                )));
            }
        }
        if let Object::CollectionIterator {
            kind,
            iteration,
            target,
            next_index,
            ..
        } = heap.get(id)?
        {
            let length = match (kind, target) {
                (_, None) => Some(0),
                (CollectionKind::Map, Some(target)) => match heap.get(*target) {
                    Ok(Object::Map { entries, .. }) => Some(entries.len()),
                    _ => None,
                },
                (CollectionKind::Set, Some(target)) => match heap.get(*target) {
                    Ok(Object::Set { elements, .. }) => Some(elements.len()),
                    _ => None,
                },
            };
            if length.is_none_or(|length| *next_index > length)
                || matches!(
                    (kind, iteration),
                    (CollectionKind::Set, CollectionIteration::Keys)
                )
            {
                return Err(VmError::InvalidSnapshot(format!(
                    "collection iterator {id} has an invalid target, kind, or position"
                )));
            }
        }
        if let Object::ArrayIterator {
            array: Some(array), ..
        } = heap.get(id)?
            && heap.get(*array).is_err()
        {
            return Err(VmError::InvalidSnapshot(format!(
                "array iterator {id} references missing source {array}"
            )));
        }
        if let Object::IteratorHelper { state, .. } = heap.get(id)?
            && !state.valid(heap)
        {
            return Err(VmError::InvalidSnapshot(format!(
                "iterator helper {id} has invalid suspended state"
            )));
        }
        if let Object::IteratorWrapper { record, .. } = heap.get(id)?
            && !record.valid(heap)
        {
            return Err(VmError::InvalidSnapshot(format!(
                "wrapped iterator {id} has an invalid iterator record"
            )));
        }
        if let Object::StringIterator {
            string, position, ..
        } = heap.get(id)?
            && !valid_string_position(string, *position)
        {
            return Err(VmError::InvalidSnapshot(format!(
                "string iterator {id} has an invalid UTF-16 position"
            )));
        }
        if let Object::TextDecoder(decoder) = heap.get(id)? {
            let prefix_len = usize::from(decoder.prefix_len);
            // A saved prefix must be the beginning of one scalar, not a
            // complete or plainly invalid sequence.
            let prefix_valid = prefix_len <= decoder.prefix.len()
                && (prefix_len == 0
                    || std::str::from_utf8(&decoder.prefix[..prefix_len]).is_err_and(|error| {
                        error.valid_up_to() == 0 && error.error_len().is_none()
                    }));
            // Only an open stream can leave a prefix, and only a fatal error
            // in an open stream retains unprocessed bytes.
            let stream_valid = decoder.streaming || prefix_len == 0 && decoder.pending.is_empty();
            let pending_valid = decoder.pending.is_empty() || decoder.fatal && decoder.streaming;
            if !prefix_valid || !stream_valid || !pending_valid {
                return Err(VmError::InvalidSnapshot(format!(
                    "TextDecoder {id} has invalid stream state"
                )));
            }
        }
        if let Object::RegExpStringIterator { regexp, .. } = heap.get(id)?
            && heap.get(*regexp).is_err()
        {
            return Err(VmError::InvalidSnapshot(format!(
                "RegExp string iterator {id} references missing regexp {regexp}"
            )));
        }
        if let regexp @ Object::RegExp { .. } = heap.get(id)?
            && !regexp
                .own_property(PropertyKeyRef::String("lastIndex"))
                .is_some_and(|property| {
                    matches!(property, OwnProperty::Stored(Property::Data(_), attributes)
                        if !attributes.enumerable && !attributes.configurable)
                })
        {
            return Err(VmError::InvalidSnapshot(format!(
                "RegExp {id} has an invalid lastIndex property"
            )));
        }
        let object = heap.get(id)?;
        if let Object::ForInIterator {
            current,
            pending,
            visited,
            string_indices,
        } = object
            && (current.is_some_and(|source| {
                matches!(
                    heap.get(source),
                    Err(_)
                        | Ok(Object::Environment(_)
                            | Object::ForInIterator { .. }
                            | Object::DisposeState(_))
                )
            }) || (current.is_none() && pending.is_some())
                || string_indices.start > string_indices.end
                || (!string_indices.is_empty()
                    && (pending.is_some()
                        || visited.len() != 1
                        || !visited.first().is_some_and(|key| key == "length"))))
        {
            return Err(VmError::InvalidSnapshot(format!(
                "for-in iterator {id} has invalid cursor state"
            )));
        }
        if let Object::Event { event, .. } = object {
            if event.phase != 0
                || event.dispatching
                || event.in_passive
                || event.current_target.is_some()
            {
                return Err(VmError::InvalidSnapshot(format!(
                    "event {id} has active dispatch state"
                )));
            }
            if let Some(target) = event.target
                && !heap
                    .get(target)
                    .is_ok_and(|object| object.event_target_state().is_some())
            {
                return Err(VmError::InvalidSnapshot(format!(
                    "event {id} has invalid target {target}"
                )));
            }
            let guest_object = |object| {
                heap.get(object).is_ok_and(|object| {
                    !matches!(
                        object,
                        Object::Environment(_)
                            | Object::ForInIterator { .. }
                            | Object::DisposeState(_)
                    )
                })
            };
            let guest_value = |value: &Value| match value.view() {
                ValueRef::Object(object) => guest_object(object),
                _ => true,
            };
            let valid_payload = match &event.payload {
                EventPayload::Event | EventPayload::CustomEvent { .. } => true,
                EventPayload::ErrorEvent {
                    filename, error, ..
                } => {
                    event.initialized
                        && guest_value(error)
                        && (filename.is_latin1()
                            || char::decode_utf16(filename.units()).all(|unit| unit.is_ok()))
                }
                EventPayload::PromiseRejectionEvent { promise, reason } => {
                    event.initialized && guest_object(*promise) && guest_value(reason)
                }
            };
            if !valid_payload {
                return Err(VmError::InvalidSnapshot(format!(
                    "event {id} has an invalid diagnostic payload"
                )));
            }
        }
        let onabort = check_abort_state(heap, id, object, &mut controller_signals)?;
        if let Some(target) = object.event_target_state() {
            let mut handlers = BTreeSet::new();
            for (ty, id_slot) in crate::builtin::global_handlers::TYPES
                .into_iter()
                .zip(target.global_handlers)
            {
                if let Some(handler) = id_slot
                    && (id != global
                        || !handlers.insert(handler)
                        || !target.listeners.iter().any(|entry| {
                            entry.id == handler
                                && entry.global_handler
                                && entry.ty == ty
                                && !entry.capture
                                && !entry.passive
                                && !entry.once
                                && entry.signal.is_none()
                        }))
                {
                    return Err(VmError::InvalidSnapshot(format!(
                        "event target {id} has an invalid {ty} handler"
                    )));
                }
            }
            let mut previous_id = None;
            let mut registrations = BTreeSet::new();
            for listener in &target.listeners {
                if listener.id >= target.next_id
                    || previous_id.is_some_and(|previous| previous >= listener.id)
                {
                    return Err(VmError::InvalidSnapshot(format!(
                        "event target {id} has invalid listener id {}",
                        listener.id
                    )));
                }
                previous_id = Some(listener.id);
                if matches!(
                    heap.get(listener.callback),
                    Err(_)
                        | Ok(Object::Environment(_)
                            | Object::ForInIterator { .. }
                            | Object::DisposeState(_))
                ) {
                    return Err(VmError::InvalidSnapshot(format!(
                        "event target {id} has invalid listener callback {}",
                        listener.callback
                    )));
                }
                if let Some(signal) = listener.signal
                    && !matches!(heap.get(signal), Ok(Object::AbortSignal { signal, .. })
                        if signal.reason.is_none()
                            && signal.steps.contains(&AbortStep::Listener { target: id, id: listener.id }))
                {
                    return Err(VmError::InvalidSnapshot(format!(
                        "event target {id} has invalid listener signal for {}",
                        listener.id
                    )));
                }
                if listener.global_handler != handlers.contains(&listener.id) {
                    return Err(VmError::InvalidSnapshot(format!(
                        "event target {id} has an unmatched global handler registration"
                    )));
                }
                if Some(listener.id) != onabort
                    && !listener.global_handler
                    && !registrations.insert((&listener.ty, listener.callback, listener.capture))
                {
                    return Err(VmError::InvalidSnapshot(format!(
                        "event target {id} has duplicate listener registrations"
                    )));
                }
            }
        }
        heap.write_edges(id, object, &mut edges);
        match object {
            Object::WeakMap { entries, .. } => {
                for (&key, value) in entries {
                    validate_weak_key(heap, "WeakMap", id, key)?;
                    push_value(&mut edges, value);
                }
            }
            Object::WeakSet { entries, .. } => {
                for &key in entries {
                    validate_weak_key(heap, "WeakSet", id, key)?;
                }
            }
            _ => {}
        }
        for edge in edges.drain(..) {
            match edge {
                OwnedEdge::Object(target) => {
                    if !heap.contains(target) {
                        return Err(VmError::InvalidSnapshot(format!(
                            "value references missing object {target}"
                        )));
                    }
                }
                OwnedEdge::Accessor(target) => match heap.get(target) {
                    Ok(object) if is_callable_object(object) => {}
                    Ok(_) => {
                        return Err(VmError::InvalidSnapshot(format!(
                            "object {id} accessor has invalid target {target}"
                        )));
                    }
                    Err(_) => {
                        return Err(VmError::InvalidSnapshot(format!(
                            "object {id} accessor references missing object {target}"
                        )));
                    }
                },
                OwnedEdge::Prototype(target) => match heap.is_environment(target) {
                    None => {
                        return Err(VmError::InvalidSnapshot(format!(
                            "object {id} prototype {target} does not exist"
                        )));
                    }
                    Some(true) => {
                        return Err(VmError::InvalidSnapshot(format!(
                            "object {id} prototype {target} is an environment"
                        )));
                    }
                    Some(false) if matches!(heap.get(target), Ok(Object::DisposeState(_))) => {
                        return Err(VmError::InvalidSnapshot(format!(
                            "object {id} prototype {target} is an internal disposal state"
                        )));
                    }
                    Some(false) => {
                        prototypes.insert(id, target);
                    }
                },
                OwnedEdge::Environment { id: env, role } => {
                    require_environment(heap, id, env, role)?;
                }
                OwnedEdge::Continuation(_) => {}
                OwnedEdge::Symbol(symbol) => {
                    if !heap.symbols.contains_key(&symbol) {
                        return Err(VmError::InvalidSnapshot(format!(
                            "object {id} references missing symbol {symbol}"
                        )));
                    }
                }
            }
        }
        if let Object::Proxy {
            slots: Some((target, handler)),
            ..
        } = heap.get(id)?
        {
            if !matches!(
                heap.get(*target),
                Ok(Object::Plain(_)
                    | Object::Array(_)
                    | Object::Boxed { .. }
                    | Object::Date { .. }
                    | Object::DomException { .. }
                    | Object::Event { .. }
                    | Object::EventTarget { .. }
                    | Object::Performance { .. }
                    | Object::AbortController { .. }
                    | Object::AbortSignal { .. }
                    | Object::ArrayBuffer(_)
                    | Object::TypedArray(_)
                    | Object::DataView(_)
                    | Object::Proxy { .. })
            ) {
                return Err(VmError::InvalidSnapshot(format!(
                    "proxy {id} has invalid target {target}"
                )));
            }
            if matches!(
                heap.get(*handler),
                Err(_)
                    | Ok(Object::Environment(_)
                        | Object::ForInIterator { .. }
                        | Object::DisposeState(_))
            ) {
                return Err(VmError::InvalidSnapshot(format!(
                    "proxy {id} has invalid handler {handler}"
                )));
            }
            // Follow the target before reading its prototype. This checks both
            // target-only cycles and cycles involving ordinary prototype links.
            prototypes.insert(id, *target);
        } else if let Object::ProxyRevoker {
            proxy: Some(proxy), ..
        } = heap.get(id)?
            && (!matches!(heap.get(*proxy), Ok(Object::Proxy { slots: Some(_), .. }))
                || !revocable.insert(*proxy))
        {
            return Err(VmError::InvalidSnapshot(format!(
                "proxy revoker {id} has invalid or duplicate authority over proxy {proxy}"
            )));
        }
    }

    let mut completed = BTreeSet::new();
    let mut path = BTreeSet::new();
    for &id in prototypes.keys() {
        let mut current = id;
        while !completed.contains(&current) {
            if !path.insert(current) {
                return Err(VmError::InvalidSnapshot(format!(
                    "prototype chain of object {id} is cyclic"
                )));
            }
            let Some(&next) = prototypes.get(&current) else {
                break;
            };
            current = next;
        }
        completed.extend(std::mem::take(&mut path));
    }
    Ok(())
}

use super::{
    load_modules::require_environment,
    validate_builtins::{binary_object_error, validate_weak_key},
};
