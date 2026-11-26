//! Restoration of host classes, host instances, module states, and environments.

use std::collections::BTreeMap;

use crate::binding::Bindings;
use crate::error::{ErrorClass, VmError};
use crate::heap::{EnvRole, Heap, NativeObject, Object};
use crate::runtime::{HostInstance, HostSlot, ModuleState};
use crate::value::{ObjectId, Value, ValueRef};

use crate::snapshot::record::*;

pub(super) fn load_host_classes(
    bindings: &Bindings,
    heap: &Heap,
    records: &BTreeMap<String, ObjectId>,
) -> Result<Vec<crate::heap::HostClassRecord>, VmError> {
    if records.len() != bindings.classes_len() {
        return Err(VmError::InvalidSnapshot(format!(
            "class prototype count mismatch: snapshot {}, bindings {}",
            records.len(),
            bindings.classes_len()
        )));
    }
    let mut classes = Vec::with_capacity(bindings.classes_len());
    for (class, tag) in bindings.classes() {
        let id = *records
            .get(tag)
            .ok_or_else(|| VmError::InvalidSnapshot(format!("snapshot is missing class {tag}")))?;
        match heap.get(id) {
            Ok(Object::Plain(_)) => {}
            Ok(_) => {
                return Err(VmError::InvalidSnapshot(format!(
                    "class {tag} prototype {id:?} is not a plain object"
                )));
            }
            Err(_) => {
                return Err(VmError::InvalidSnapshot(format!(
                    "class {tag} prototype {id:?} does not exist"
                )));
            }
        }
        classes.push(crate::heap::HostClassRecord {
            prototype: id,
            source: class,
        });
    }
    Ok(classes)
}

/// Re-links the declared error subclass objects. Records must cover every
/// declaration exactly, in declaration order, and each constructor must be
/// the export's native target.
pub(super) fn load_error_exports(
    bindings: &Bindings,
    heap: &Heap,
    records: &[ErrorExportRecord],
) -> Result<Vec<(ErrorClass, crate::vm::ErrorExport)>, VmError> {
    let declarations: Vec<_> = bindings.error_constructor_decls().collect();
    if records.len() != declarations.len() {
        return Err(VmError::InvalidSnapshot(format!(
            "error export count mismatch: snapshot {}, bindings {}",
            records.len(),
            declarations.len()
        )));
    }
    let mut exports = Vec::with_capacity(declarations.len());
    for (record, (id, class, name)) in records.iter().zip(&declarations) {
        let ErrorClass::Host {
            module: class_module,
            name: class_name,
        } = *class
        else {
            return Err(VmError::InvalidState(
                "builtin error class stored as an export".to_owned(),
            ));
        };
        if record.module != class_module || record.export != *name || record.export != class_name {
            return Err(VmError::InvalidSnapshot(format!(
                "error export {}:{} does not match the declaration {}:{}",
                record.module, record.export, class_module, class_name
            )));
        }
        match heap.get(record.constructor) {
            Ok(Object::Native(NativeObject::Host {
                target,
                constructable,
                ..
            })) if *target == crate::binding::NativeTarget::Export(*id) && *constructable => {}
            Ok(_) => {
                return Err(VmError::InvalidSnapshot(format!(
                    "error export {}.{} constructor {id:?} is not the declared constructor",
                    record.module, record.export
                )));
            }
            Err(_) => {
                return Err(VmError::InvalidSnapshot(format!(
                    "error export {}.{} constructor {id:?} does not exist",
                    record.module, record.export
                )));
            }
        }
        match heap.get(record.prototype) {
            Ok(Object::Plain(_)) => {}
            Ok(_) => {
                return Err(VmError::InvalidSnapshot(format!(
                    "error export {}.{} prototype {id:?} is not a plain object",
                    record.module, record.export
                )));
            }
            Err(_) => {
                return Err(VmError::InvalidSnapshot(format!(
                    "error export {}.{} prototype {id:?} does not exist",
                    record.module, record.export
                )));
            }
        }
        exports.push((
            *class,
            crate::vm::ErrorExport {
                constructor: record.constructor,
                prototype: record.prototype,
            },
        ));
    }
    Ok(exports)
}

pub(super) fn load_host_instances(
    bindings: &Bindings,
    heap: &Heap,
    records: &[HostInstanceRecord],
) -> Result<BTreeMap<ObjectId, HostInstance>, VmError> {
    let mut instances = BTreeMap::new();
    for record in records {
        if instances.contains_key(&record.object) {
            return Err(VmError::InvalidSnapshot(format!(
                "duplicate host instance object {}",
                record.object
            )));
        }
        let class = bindings.class_by_tag(&record.class).ok_or_else(|| {
            VmError::InvalidSnapshot(format!("unknown host class {:?}", record.class))
        })?;
        let is_event_target = bindings.class_is_event_target(class);
        match heap.get(record.object) {
            Ok(Object::EventTarget { .. }) if is_event_target => {}
            Ok(Object::Plain(_)) if !is_event_target => {}
            Ok(_) => {
                let expected = if is_event_target {
                    "an EventTarget"
                } else {
                    "a plain object"
                };
                return Err(VmError::InvalidSnapshot(format!(
                    "host instance {} is not {expected}",
                    record.object
                )));
            }
            Err(_) => {
                return Err(VmError::InvalidSnapshot(format!(
                    "host instance {} does not exist",
                    record.object
                )));
            }
        }
        let expected_slots = bindings.class_slots(class);
        if record.slots.len() != expected_slots.len() {
            return Err(VmError::InvalidSnapshot(format!(
                "class {} slot count mismatch: snapshot {}, bindings {}",
                record.class,
                record.slots.len(),
                expected_slots.len()
            )));
        }
        let mut slots = Vec::with_capacity(record.slots.len());
        for (slot, declaration) in record.slots.iter().zip(expected_slots) {
            let slot = match (slot, declaration.kind) {
                (HostSlotRecord::Js(value), crate::binding::SlotKind::Js) => {
                    HostSlot::Js(value.clone())
                }
                (HostSlotRecord::JsList(values), crate::binding::SlotKind::JsList) => {
                    HostSlot::JsList(values.clone())
                }
                _ => {
                    return Err(VmError::InvalidSnapshot(format!(
                        "class {} slot {:?} kind mismatch",
                        record.class, declaration.name
                    )));
                }
            };
            for value in match &slot {
                HostSlot::Js(value) => std::slice::from_ref(value),
                HostSlot::JsList(values) => values.as_slice(),
            } {
                require_value_objects(heap, std::iter::once(value))?;
            }
            slots.push(slot);
        }
        let payload = if bindings.class_has_payload_codec(class) {
            let payload = record.payload.as_ref().ok_or_else(|| {
                VmError::InvalidSnapshot(format!("class {} is missing its payload", record.class))
            })?;
            Some(
                bindings
                    .class_decode(class, &payload.bytes)
                    .map_err(|error| {
                        VmError::InvalidSnapshot(format!("class {} payload: {error}", record.class))
                    })?,
            )
        } else {
            if record.payload.is_some() {
                return Err(VmError::InvalidSnapshot(format!(
                    "class {} has no payload codec",
                    record.class
                )));
            }
            None
        };
        instances.insert(
            record.object,
            HostInstance {
                class,
                slots,
                payload,
            },
        );
    }
    Ok(instances)
}

pub(super) fn load_module_states(
    bindings: &Bindings,
    heap: &Heap,
    records: &[ModuleStateRecord],
) -> Result<Vec<ModuleState>, VmError> {
    let count = bindings.module_count();
    let mut by_name: BTreeMap<&str, &ModuleStateRecord> = BTreeMap::new();
    for record in records {
        if by_name.insert(record.module.as_str(), record).is_some() {
            return Err(VmError::InvalidSnapshot(format!(
                "duplicate module state {:?}",
                record.module
            )));
        }
    }
    if by_name.len() != count {
        return Err(VmError::InvalidSnapshot(format!(
            "module state count mismatch: snapshot {}, bindings {}",
            by_name.len(),
            count
        )));
    }
    let mut states = Vec::with_capacity(count);
    for module in 0..count {
        let name = bindings.module_name(module).expect("linked module index");
        let record = by_name.get(name).ok_or_else(|| {
            VmError::InvalidSnapshot(format!("snapshot is missing module state {name:?}"))
        })?;
        let expected_slots = bindings.state_slots(module);
        if record.slots.len() != expected_slots.len() {
            return Err(VmError::InvalidSnapshot(format!(
                "module {name} state slot count mismatch"
            )));
        }
        let mut slots = Vec::with_capacity(record.slots.len());
        for (slot, declaration) in record.slots.iter().zip(expected_slots) {
            let slot = match (slot, declaration.kind) {
                (HostSlotRecord::Js(value), crate::binding::SlotKind::Js) => {
                    HostSlot::Js(value.clone())
                }
                (HostSlotRecord::JsList(values), crate::binding::SlotKind::JsList) => {
                    HostSlot::JsList(values.clone())
                }
                _ => {
                    return Err(VmError::InvalidSnapshot(format!(
                        "module {name} state slot {:?} kind mismatch",
                        declaration.name
                    )));
                }
            };
            for value in match &slot {
                HostSlot::Js(value) => std::slice::from_ref(value),
                HostSlot::JsList(values) => values.as_slice(),
            } {
                require_value_objects(heap, std::iter::once(value))?;
            }
            slots.push(slot);
        }
        let payload = bindings
            .state_decode(module, &record.bytes)
            .map_err(|error| {
                VmError::InvalidSnapshot(format!("module {name} state payload: {error}"))
            })?;
        states.push(ModuleState { slots, payload });
    }
    Ok(states)
}

pub(super) fn require_environment(
    heap: &Heap,
    owner: ObjectId,
    env: ObjectId,
    role: EnvRole,
) -> Result<(), VmError> {
    match heap.is_environment(env) {
        Some(true) => Ok(()),
        Some(false) => Err(VmError::InvalidSnapshot(match role {
            EnvRole::Closure => {
                format!("function object {owner} references non-environment {env}")
            }
            EnvRole::Parent => {
                format!("environment object {owner} references non-environment parent {env}")
            }
        })),
        None => Err(VmError::InvalidSnapshot(match role {
            EnvRole::Closure => {
                format!("function object {owner} references missing environment {env}")
            }
            EnvRole::Parent => {
                format!("environment object {owner} references missing parent {env}")
            }
        })),
    }
}

pub(super) fn require_value_objects<'a>(
    heap: &Heap,
    values: impl IntoIterator<Item = &'a Value>,
) -> Result<(), VmError> {
    for value in values {
        match value.view() {
            ValueRef::Object(id) if !heap.contains(id) => {
                return Err(VmError::InvalidSnapshot(format!(
                    "value references missing object {id}"
                )));
            }
            ValueRef::Symbol(id) if !heap.symbols.contains_key(&id) => {
                return Err(VmError::InvalidSnapshot(format!(
                    "value references missing symbol {id}"
                )));
            }
            _ => {}
        }
    }
    Ok(())
}
