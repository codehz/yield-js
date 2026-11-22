//! Restoration of heap objects: private fields, generators, and templates.

use std::collections::{BTreeMap, BTreeSet};

use crate::bytecode::Build;
use crate::error::VmError;
use crate::heap::GeneratorStatus;
use crate::heap::{
    AsyncCompletion, Attributes, EnvRole, Heap, IntegrityLevel, Object, OwnProperty,
    PrivateElement, PrivateMemberKind, PromiseState, Property,
};
use crate::value::{ObjectId, PropertyKeyRef, Value, ValueRef};

use crate::snapshot::record::*;

pub(super) fn load_private_elements(
    heap: &mut Heap,
    records: &[PrivateRecord],
) -> Result<(), VmError> {
    for record in records {
        let invalid = || {
            VmError::InvalidSnapshot(format!(
                "invalid private brand {} index {} on receiver {}",
                record.brand, record.index, record.receiver
            ))
        };
        if matches!(
            heap.get(record.receiver),
            Err(_)
                | Ok(Object::Environment(_)
                    | Object::ForInIterator { .. }
                    | Object::DisposeState(_))
        ) {
            return Err(invalid());
        }
        let class = heap
            .function(record.brand)
            .and_then(|f| f.class.as_ref())
            .ok_or_else(invalid)?;
        let member = class
            .source
            .private
            .get(&record.index)
            .filter(|_| record.index < class.source.private_count)
            .ok_or_else(invalid)?;
        if (member.is_static && record.brand != record.receiver)
            || matches!(member.kind, PrivateMemberKind::Field)
                != matches!(record.element, PrivateElement::Field(_))
        {
            return Err(invalid());
        }
        if let PrivateElement::Field(value) = &record.element {
            require_value_objects(heap, std::iter::once(value))?;
            if matches!(value.view(), ValueRef::Object(id) if matches!(heap.get(id), Ok(Object::Environment(_) | Object::ForInIterator { .. } | Object::DisposeState(_))))
            {
                return Err(invalid());
            }
        }
        if heap
            .private
            .entry(record.receiver)
            .or_default()
            .insert((record.brand, record.index), record.element.clone())
            .is_some()
        {
            return Err(invalid());
        }
    }
    Ok(())
}

/// A restored generator must name a real generator function, a resume point its
/// saved value count matches, and a frame state that could exist at a snapshot
/// boundary. Executing generators have a native frame and cannot be restored.
pub(super) fn load_generators(
    build: &Build,
    heap: &Heap,
    records: BTreeMap<ObjectId, crate::heap::GeneratorObject>,
) -> Result<BTreeMap<ObjectId, crate::heap::GeneratorObject>, VmError> {
    for (id, generator) in &records {
        let invalid = |detail: &str| VmError::InvalidSnapshot(format!("generator {id} {detail}"));
        if !matches!(heap.get(*id), Ok(Object::Plain(_))) {
            return Err(invalid("is not an ordinary object"));
        }
        let function = build
            .function(generator.func_id)
            .filter(|function| function.is_generator)
            .ok_or_else(|| invalid("references a non-generator function"))?;
        let async_generator = function.is_async;
        match generator.status {
            GeneratorStatus::Executing => {
                return Err(invalid("was captured while executing"));
            }
            GeneratorStatus::Completed => {
                if generator.env.is_some() || !generator.values.is_empty() {
                    return Err(invalid("is completed but keeps a frame"));
                }
                if !generator.requests.is_empty() {
                    return Err(invalid("is completed with unsettled requests"));
                }
            }
            GeneratorStatus::Awaiting => {
                if !async_generator {
                    return Err(invalid("awaits inside a synchronous generator"));
                }
                if generator.env.is_some() || !generator.values.is_empty() {
                    return Err(invalid("is awaiting but keeps a frame"));
                }
                if generator.requests.is_empty() {
                    return Err(invalid("is awaiting without a request"));
                }
            }
            GeneratorStatus::Closing => {
                if !async_generator {
                    return Err(invalid("closes inside a synchronous generator"));
                }
                if generator.env.is_some() || !generator.values.is_empty() {
                    return Err(invalid("is closing but keeps a frame"));
                }
                match generator.requests.front() {
                    Some(head) if head.completion == AsyncCompletion::Return => {}
                    _ => return Err(invalid("is closing without a return request")),
                }
            }
            GeneratorStatus::Start => {
                if !generator.requests.is_empty() {
                    return Err(invalid("is unstarted with unsettled requests"));
                }
                let start = function
                    .generator_start
                    .as_ref()
                    .ok_or_else(|| invalid("has no start entry"))?;
                if start.saved_slots.len() != generator.values.len() {
                    return Err(invalid("has the wrong number of start values"));
                }
            }
            GeneratorStatus::Suspended(index) => {
                if !generator.requests.is_empty() {
                    return Err(invalid("is suspended with unsettled requests"));
                }
                let entry = function
                    .generator_resumes
                    .get(index as usize)
                    .ok_or_else(|| invalid("has an invalid resume index"))?;
                if entry.saved_slots.len() != generator.values.len() {
                    return Err(invalid("has the wrong number of saved values"));
                }
            }
        }
        if !matches!(
            generator.status,
            GeneratorStatus::Completed | GeneratorStatus::Awaiting | GeneratorStatus::Closing
        ) && generator.env.is_some() != function.needs_environment()
        {
            return Err(invalid("has the wrong environment presence"));
        }
        if let Some(env) = generator.env {
            require_environment(heap, *id, env, EnvRole::Closure)
                .map_err(|_| invalid("references an invalid environment"))?;
        }
        require_value_objects(heap, generator.values.iter())?;
        let mut promises = BTreeSet::new();
        for request in &generator.requests {
            if !matches!(
                heap.get(request.promise),
                Ok(Object::Promise(promise)) if matches!(promise.state, PromiseState::Pending)
            ) {
                return Err(invalid("has a request that is not a pending Promise"));
            }
            if !promises.insert(request.promise) {
                return Err(invalid("has two requests for one Promise"));
            }
            if let ValueRef::Object(target) = request.argument.view()
                && !heap.contains(target)
            {
                return Err(invalid("has a request argument that is not an object"));
            }
        }
    }
    Ok(records)
}

/// Template objects are permanent roots: the snapshot must name exactly one
/// frozen cooked array per build call site, in order, with a FIXED `raw`
/// property that points at the matching frozen raw array.
pub(super) fn load_templates(
    build: &Build,
    heap: &Heap,
    ids: Vec<ObjectId>,
) -> Result<Vec<ObjectId>, VmError> {
    if ids.len() != build.templates.len() {
        return Err(VmError::InvalidSnapshot(format!(
            "template count mismatch: snapshot {}, build {}",
            ids.len(),
            build.templates.len()
        )));
    }
    let mut seen = BTreeSet::new();
    for (index, (&id, site)) in ids.iter().zip(&build.templates).enumerate() {
        let invalid = |detail: &str| {
            VmError::InvalidSnapshot(format!("template {index} object {id}: {detail}"))
        };
        if !seen.insert(id) {
            return Err(VmError::InvalidSnapshot(format!(
                "duplicate template object {id}"
            )));
        }
        let Ok(object) = heap.get(id) else {
            return Err(invalid("missing object"));
        };
        let Object::Array(array) = object else {
            return Err(invalid("not an array"));
        };
        if !object.test_integrity_level(IntegrityLevel::Frozen) {
            return Err(invalid("not frozen"));
        }
        if array.length != site.cooked.len() {
            return Err(invalid("cooked array length mismatch"));
        }
        for (position, expected) in site.cooked.iter().enumerate() {
            let Some(OwnProperty::Stored(Property::Data(value), attributes)) =
                array.own_index(position)
            else {
                return Err(invalid("cooked element is not a data property"));
            };
            let valid = attributes.enumerable
                && !attributes.writable
                && !attributes.configurable
                && matches!(
                    (expected, value.view()),
                    (None, ValueRef::Undefined) | (Some(_), ValueRef::String(_))
                );
            let matched = match (expected, value.view()) {
                (None, ValueRef::Undefined) => true,
                (Some(expected), ValueRef::String(actual)) => actual == expected,
                _ => false,
            };
            if !valid || !matched {
                return Err(invalid("cooked element value mismatch"));
            }
        }
        let Some(raw_property) = object.own_property(PropertyKeyRef::String("raw")) else {
            return Err(invalid("missing raw property"));
        };
        if raw_property.attributes() != Attributes::FIXED {
            return Err(invalid("raw property is not fixed"));
        }
        let Some(ValueRef::Object(raw_id)) = raw_property.data_value().map(Value::view) else {
            return Err(invalid("raw property is not an array"));
        };
        let Ok(raw_object) = heap.get(raw_id) else {
            return Err(invalid("raw array is missing"));
        };
        let Object::Array(raw_array) = raw_object else {
            return Err(invalid("raw property is not an array"));
        };
        if !raw_object.test_integrity_level(IntegrityLevel::Frozen) {
            return Err(invalid("raw array is not frozen"));
        }
        if raw_array.length != site.raw.len() {
            return Err(invalid("raw array length mismatch"));
        }
        for (position, expected) in site.raw.iter().enumerate() {
            let Some(OwnProperty::Stored(Property::Data(value), attributes)) =
                raw_array.own_index(position)
            else {
                return Err(invalid("raw element is not a data property"));
            };
            let valid = attributes.enumerable
                && !attributes.writable
                && !attributes.configurable
                && matches!(value.view(), ValueRef::String(_));
            let matched = matches!(value.view(), ValueRef::String(actual) if actual == expected);
            if !valid || !matched {
                return Err(invalid("raw element value mismatch"));
            }
        }
    }
    Ok(ids)
}

use super::load_modules::{require_environment, require_value_objects};
