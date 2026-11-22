//! Built-in and native binding validation, plus date and binary shape checks.

use std::collections::BTreeSet;

use crate::binding::Bindings;
use crate::builtin::date_calendar::TIME_CLIP_BOUND;
use crate::error::VmError;
use crate::heap::{Heap, NativeObject, Object, WeakKey};
use crate::value::{ObjectId, Value, ValueRef};

pub(super) fn valid_date_time(object: &Object) -> bool {
    !matches!(object, Object::Date { time: Some(time), .. }
        if !(-TIME_CLIP_BOUND..=TIME_CLIP_BOUND).contains(time))
}

/// A buffer's length never exceeds its maximum; a view keeps whole elements
/// and a representable range. A buffer that is currently detached still bounds
/// its views by the maximum it was created with, so a detached view record is
/// not rejected for looking out of range. The reason is a located diagnostic.
pub(super) fn binary_object_error(heap: &Heap, id: ObjectId) -> Option<String> {
    let capacity = |buffer: ObjectId| -> Result<usize, String> {
        let buffer = heap
            .buffer(buffer)
            .ok()
            .flatten()
            .ok_or_else(|| format!("binary object {id} references an invalid buffer"))?;
        Ok(match (buffer.max_byte_length, buffer.byte_length()) {
            (Some(max), _) => max,
            (None, Some(length)) => length,
            // A detached fixed buffer no longer records its original length.
            (None, None) => usize::MAX,
        })
    };
    let view_range =
        |buffer: ObjectId, offset: usize, length: Option<usize>| -> Result<(), String> {
            match length {
                Some(length) => {
                    let capacity = capacity(buffer)?;
                    if offset.checked_add(length).is_none_or(|end| end > capacity) {
                        return Err(format!("binary object {id} exceeds its buffer"));
                    }
                }
                None => {
                    let buffer = heap.buffer(buffer).ok().flatten().ok_or_else(|| {
                        format!("binary object {id} references an invalid buffer")
                    })?;
                    if !buffer.is_resizable() {
                        return Err(format!(
                            "binary object {id} tracks the length of a fixed-length buffer"
                        ));
                    }
                }
            }
            Ok(())
        };
    match heap.get(id) {
        Err(_) => Some(format!("binary object {id} is missing")),
        Ok(Object::ArrayBuffer(buffer)) => buffer
            .byte_length()
            .zip(buffer.max_byte_length)
            .filter(|(length, max)| length > max)
            .map(|_| format!("ArrayBuffer {id} exceeds its maximum byte length")),
        Ok(Object::TypedArray(view)) => {
            let width = view.kind.bytes();
            if view.byte_offset % width != 0 {
                Some(format!("TypedArray {id} has an unaligned byte offset"))
            } else if view.byte_length.is_some_and(|length| length % width != 0) {
                Some(format!("TypedArray {id} has a partial element length"))
            } else {
                view_range(view.buffer, view.byte_offset, view.byte_length).err()
            }
        }
        Ok(Object::DataView(view)) => {
            view_range(view.buffer, view.byte_offset, view.byte_length).err()
        }
        Ok(_) => None,
    }
}

pub(super) fn validate_weak_key(
    heap: &Heap,
    owner: &str,
    id: ObjectId,
    key: WeakKey,
) -> Result<(), VmError> {
    let valid = match key {
        WeakKey::Object(target) => !matches!(
            heap.get(target),
            Err(_)
                | Ok(Object::Environment(_)
                    | Object::ForInIterator { .. }
                    | Object::DisposeState(_))
        ),
        WeakKey::Symbol(target) => heap.weak_key(&Value::symbol(target)).is_some(),
    };
    if !valid {
        return Err(VmError::InvalidSnapshot(format!(
            "{owner} {id} has an invalid key {key}"
        )));
    }
    Ok(())
}

pub(super) fn valid_native_target(
    bindings: &Bindings,
    target: crate::binding::NativeTarget,
) -> bool {
    match target {
        crate::binding::NativeTarget::Export(id) => matches!(
            bindings.export_decl(id),
            Some(crate::binding::ExportDecl::Function(_))
        ),
        crate::binding::NativeTarget::Method(id) => bindings.method(id).is_some(),
        crate::binding::NativeTarget::Accessor {
            class,
            accessor,
            set,
        } => bindings
            .accessor(class as usize, accessor as usize)
            .is_some_and(|declaration| {
                if set {
                    declaration.set.is_some()
                } else {
                    declaration.get.is_some()
                }
            }),
    }
}

pub(super) fn validate_native_targets(bindings: &Bindings, heap: &Heap) -> Result<(), VmError> {
    let mut host_identities = BTreeSet::new();
    let mut intrinsic_identities = BTreeSet::new();
    for id in heap.ids() {
        let unique = match heap.get(id)? {
            Object::Native(NativeObject::Host {
                target,
                constructable,
                ..
            }) => {
                if !valid_native_target(bindings, *target) {
                    return Err(VmError::InvalidSnapshot(format!(
                        "native object {id} references an undeclared binding"
                    )));
                }
                host_identities.insert((*target, *constructable))
            }
            Object::Native(NativeObject::Intrinsic { intrinsic, .. }) => {
                // IntrinsicId deserialization has already linked a declared key.
                intrinsic_identities.insert(*intrinsic)
            }
            _ => continue,
        };
        if !unique {
            return Err(VmError::InvalidSnapshot(format!(
                "duplicate native object identity at {id}"
            )));
        }
    }
    Ok(())
}

pub(super) fn validate_native_import_objects(
    heap: &Heap,
    imports: &[crate::vm::ImportModule],
) -> Result<(), VmError> {
    let mut identities = BTreeSet::new();
    for import in imports {
        for value in &import.values {
            if let ValueRef::Object(id) = value.view()
                && let Object::Native(NativeObject::Host {
                    target,
                    constructable,
                    ..
                }) = heap.get(id)?
                && !identities.insert((*target, *constructable))
            {
                return Err(VmError::InvalidSnapshot(format!(
                    "duplicate native import identity at {id}"
                )));
            }
        }
    }
    Ok(())
}
