//! Snapshot header, symbol table, and source module validation.

use std::collections::{BTreeMap, BTreeSet};

use crate::builtin::roots::BuiltinRoots;
use crate::bytecode::{BYTECODE_VERSION, Build};
use crate::error::VmError;
use crate::heap::{
    ASYNC_DISPOSE_DESCRIPTION, ASYNC_DISPOSE_SYMBOL, ASYNC_ITERATOR_DESCRIPTION,
    ASYNC_ITERATOR_SYMBOL, DISPOSE_DESCRIPTION, DISPOSE_SYMBOL, HAS_INSTANCE_DESCRIPTION,
    HAS_INSTANCE_SYMBOL, Heap, IS_CONCAT_SPREADABLE_DESCRIPTION, IS_CONCAT_SPREADABLE_SYMBOL,
    ITERATOR_DESCRIPTION, ITERATOR_SYMBOL, MATCH_ALL_DESCRIPTION, MATCH_ALL_SYMBOL,
    MATCH_DESCRIPTION, MATCH_SYMBOL, Object, OwnProperty, Property, REPLACE_DESCRIPTION,
    REPLACE_SYMBOL, SEARCH_DESCRIPTION, SEARCH_SYMBOL, SPECIES_DESCRIPTION, SPECIES_SYMBOL,
    SPLIT_DESCRIPTION, SPLIT_SYMBOL, TO_PRIMITIVE_DESCRIPTION, TO_PRIMITIVE_SYMBOL,
    TO_STRING_TAG_DESCRIPTION, TO_STRING_TAG_SYMBOL, UNSCOPABLES_DESCRIPTION, UNSCOPABLES_SYMBOL,
};
use crate::value::{ObjectId, PropertyKeyRef};

use crate::snapshot::record::*;

pub(super) fn check_snapshot_header(snapshot: &Snapshot) -> Result<(), VmError> {
    if snapshot.format_version != SNAPSHOT_FORMAT_VERSION {
        return Err(VmError::InvalidSnapshot(format!(
            "unsupported snapshot format {}, expected {}",
            snapshot.format_version, SNAPSHOT_FORMAT_VERSION
        )));
    }
    if snapshot.bytecode_version != BYTECODE_VERSION {
        return Err(VmError::InvalidSnapshot(format!(
            "unsupported bytecode version {}, expected {}",
            snapshot.bytecode_version, BYTECODE_VERSION
        )));
    }
    if snapshot.random_state == 0 {
        return Err(VmError::InvalidSnapshot(
            "invalid Math.random state".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn authorize_fetch_environment(
    recorded: &crate::fetch::FetchEnvironment,
    provided: Option<&crate::fetch::FetchEnvironment>,
) -> Result<(), VmError> {
    let authorized = match provided {
        Some(provided) => {
            provided.origin == recorded.origin
                && provided.partition == recorded.partition
                && provided.authorization == recorded.authorization
        }
        None => *recorded == crate::fetch::FetchEnvironment::default(),
    };
    if authorized {
        Ok(())
    } else {
        Err(VmError::InvalidSnapshot(
            "fetch environment is not authorized for restore".to_owned(),
        ))
    }
}

/// Symbol tables validate exact reserved well-known records and registry integrity
/// before any heap edge is checked; all ids stay below the high-water mark.
pub(super) fn validate_symbol_tables(snapshot: &Snapshot) -> Result<(), VmError> {
    if snapshot.next_symbol_id == 0 {
        return Err(VmError::InvalidSnapshot(
            "next symbol id must be positive".to_owned(),
        ));
    }
    for (id, description) in &snapshot.symbols {
        if *id >= snapshot.next_symbol_id {
            return Err(VmError::InvalidSnapshot(format!(
                "symbol {id} is not below the next symbol id {}",
                snapshot.next_symbol_id
            )));
        }
        let reserved = match *id {
            ITERATOR_SYMBOL => Some(ITERATOR_DESCRIPTION),
            TO_PRIMITIVE_SYMBOL => Some(TO_PRIMITIVE_DESCRIPTION),
            SPECIES_SYMBOL => Some(SPECIES_DESCRIPTION),
            MATCH_SYMBOL => Some(MATCH_DESCRIPTION),
            MATCH_ALL_SYMBOL => Some(MATCH_ALL_DESCRIPTION),
            REPLACE_SYMBOL => Some(REPLACE_DESCRIPTION),
            SEARCH_SYMBOL => Some(SEARCH_DESCRIPTION),
            SPLIT_SYMBOL => Some(SPLIT_DESCRIPTION),
            HAS_INSTANCE_SYMBOL => Some(HAS_INSTANCE_DESCRIPTION),
            IS_CONCAT_SPREADABLE_SYMBOL => Some(IS_CONCAT_SPREADABLE_DESCRIPTION),
            ASYNC_ITERATOR_SYMBOL => Some(ASYNC_ITERATOR_DESCRIPTION),
            TO_STRING_TAG_SYMBOL => Some(TO_STRING_TAG_DESCRIPTION),
            DISPOSE_SYMBOL => Some(DISPOSE_DESCRIPTION),
            ASYNC_DISPOSE_SYMBOL => Some(ASYNC_DISPOSE_DESCRIPTION),
            UNSCOPABLES_SYMBOL => Some(UNSCOPABLES_DESCRIPTION),
            _ => None,
        };
        if let Some(expected) = reserved
            && !description
                .as_ref()
                .is_some_and(|description| description.units().eq(expected.encode_utf16()))
        {
            return Err(VmError::InvalidSnapshot(format!(
                "reserved symbol {id} has an invalid description"
            )));
        }
    }
    for (id, description) in [
        (ITERATOR_SYMBOL, ITERATOR_DESCRIPTION),
        (TO_PRIMITIVE_SYMBOL, TO_PRIMITIVE_DESCRIPTION),
        (SPECIES_SYMBOL, SPECIES_DESCRIPTION),
        (MATCH_SYMBOL, MATCH_DESCRIPTION),
        (MATCH_ALL_SYMBOL, MATCH_ALL_DESCRIPTION),
        (REPLACE_SYMBOL, REPLACE_DESCRIPTION),
        (SEARCH_SYMBOL, SEARCH_DESCRIPTION),
        (SPLIT_SYMBOL, SPLIT_DESCRIPTION),
        (HAS_INSTANCE_SYMBOL, HAS_INSTANCE_DESCRIPTION),
        (
            IS_CONCAT_SPREADABLE_SYMBOL,
            IS_CONCAT_SPREADABLE_DESCRIPTION,
        ),
        (ASYNC_ITERATOR_SYMBOL, ASYNC_ITERATOR_DESCRIPTION),
        (TO_STRING_TAG_SYMBOL, TO_STRING_TAG_DESCRIPTION),
        (DISPOSE_SYMBOL, DISPOSE_DESCRIPTION),
        (ASYNC_DISPOSE_SYMBOL, ASYNC_DISPOSE_DESCRIPTION),
        (UNSCOPABLES_SYMBOL, UNSCOPABLES_DESCRIPTION),
    ] {
        if !snapshot
            .symbols
            .get(&id)
            .and_then(Option::as_ref)
            .is_some_and(|actual| actual.units().eq(description.encode_utf16()))
        {
            return Err(VmError::InvalidSnapshot(format!(
                "snapshot is missing reserved symbol {description}"
            )));
        }
    }
    let mut registered = BTreeSet::new();
    for (key, id) in &snapshot.symbol_registry {
        if let Some(description) = match *id {
            ITERATOR_SYMBOL => Some(ITERATOR_DESCRIPTION),
            TO_PRIMITIVE_SYMBOL => Some(TO_PRIMITIVE_DESCRIPTION),
            SPECIES_SYMBOL => Some(SPECIES_DESCRIPTION),
            MATCH_SYMBOL => Some(MATCH_DESCRIPTION),
            MATCH_ALL_SYMBOL => Some(MATCH_ALL_DESCRIPTION),
            REPLACE_SYMBOL => Some(REPLACE_DESCRIPTION),
            SEARCH_SYMBOL => Some(SEARCH_DESCRIPTION),
            SPLIT_SYMBOL => Some(SPLIT_DESCRIPTION),
            HAS_INSTANCE_SYMBOL => Some(HAS_INSTANCE_DESCRIPTION),
            IS_CONCAT_SPREADABLE_SYMBOL => Some(IS_CONCAT_SPREADABLE_DESCRIPTION),
            ASYNC_ITERATOR_SYMBOL => Some(ASYNC_ITERATOR_DESCRIPTION),
            TO_STRING_TAG_SYMBOL => Some(TO_STRING_TAG_DESCRIPTION),
            DISPOSE_SYMBOL => Some(DISPOSE_DESCRIPTION),
            ASYNC_DISPOSE_SYMBOL => Some(ASYNC_DISPOSE_DESCRIPTION),
            UNSCOPABLES_SYMBOL => Some(UNSCOPABLES_DESCRIPTION),
            _ => None,
        } {
            return Err(VmError::InvalidSnapshot(format!(
                "registry key {key:?} points at reserved symbol {description}"
            )));
        }
        if !registered.insert(*id) {
            return Err(VmError::InvalidSnapshot(format!(
                "registry key {key:?} duplicates symbol {id}"
            )));
        }
        match snapshot.symbols.get(id) {
            Some(Some(description)) if description == key => {}
            _ => {
                return Err(VmError::InvalidSnapshot(format!(
                    "registry key {key:?} does not describe symbol {id}"
                )));
            }
        }
    }
    Ok(())
}

pub(super) fn validate_source_modules(
    build: &Build,
    heap: &Heap,
    builtins: &BuiltinRoots,
    envs: &[Option<ObjectId>],
    namespaces: &[ObjectId],
    host_namespaces: &[ObjectId],
) -> Result<(), VmError> {
    if envs.len() != build.modules.len() || namespaces.len() != build.modules.len() {
        return Err(VmError::InvalidSnapshot(
            "source module root count mismatch".to_owned(),
        ));
    }
    if host_namespaces.iter().copied().chain(builtins.ids()).any(
        |id| matches!(heap.get(id), Ok(Object::Plain(object)) if object.module_namespace.is_some()),
    ) {
        return Err(VmError::InvalidSnapshot(
            "a host or builtin root aliases a source module namespace".to_owned(),
        ));
    }

    // Identity tags also distinguish modules with identical (or empty) shape.
    let mut sentinel = None;
    for (id, object) in heap.iter() {
        if !matches!(object, Object::Plain(_))
            && object.has_error_brand() != matches!(object, Object::DomException { .. })
        {
            return Err(VmError::InvalidSnapshot(format!(
                "invalid ErrorData slot on object {id}"
            )));
        }
        match object {
            Object::Environment(env) => {
                if !env.uninitialized.is_empty() {
                    return Err(VmError::InvalidSnapshot(format!(
                        "environment {id} has uninitialized module bindings"
                    )));
                }
                if let Some(module) = env.module
                    && envs.get(module as usize) != Some(&Some(id))
                {
                    return Err(VmError::InvalidSnapshot(format!(
                        "environment {id} is not the root for source module {module}"
                    )));
                }
                if env.module_getter_sentinel
                    && (env.module.is_some()
                        || env.parent.is_some()
                        || !env.bindings.is_empty()
                        || sentinel.replace(id).is_some())
                {
                    return Err(VmError::InvalidSnapshot(format!(
                        "invalid module getter sentinel environment {id}"
                    )));
                }
            }
            Object::Plain(object) => {
                if object.is_raw_json {
                    let invalid =
                        || VmError::InvalidSnapshot(format!("invalid raw JSON object {id}"));
                    let source = heap
                        .raw_json_text(id)
                        .map_err(|_| invalid())?
                        .ok_or_else(invalid)?;
                    let stored = heap.get(id)?;
                    let Some(OwnProperty::Stored(_, attributes)) =
                        stored.own_property(PropertyKeyRef::String("rawJSON"))
                    else {
                        return Err(invalid());
                    };
                    if object.is_error
                        || object.module_namespace.is_some()
                        || object.prototype.is_some()
                        || stored.is_extensible()
                        || attributes != crate::vm::MODULE_EXPORT_ATTRIBUTES
                        || object.props.iter().count() != 1
                        || object.symbol_props.iter().next().is_some()
                        || !crate::builtin::json::is_raw_json_source(source)
                    {
                        return Err(invalid());
                    }
                }
                if let Some(module) = object.module_namespace
                    && namespaces.get(module as usize) != Some(&id)
                {
                    return Err(VmError::InvalidSnapshot(format!(
                        "namespace {id} is not the root for source module {module}"
                    )));
                }
            }
            _ => {}
        }
    }

    let mut getters = BTreeMap::new();
    let mut uses_sentinel = false;
    let mut validate_getter = |object: &Object,
                               name: &str,
                               export: &crate::bytecode::ModuleExport,
                               attributes: crate::heap::Attributes,
                               owner: &str| {
        let invalid = |detail| VmError::InvalidSnapshot(format!("{owner} {name:?}: {detail}"));
        let Some(OwnProperty::Stored(
            Property::Accessor {
                get: Some(id),
                set: None,
            },
            actual_attributes,
        )) = object.own_property(PropertyKeyRef::String(name))
        else {
            return Err(invalid("missing getter-only own property"));
        };
        if actual_attributes != attributes {
            return Err(invalid("invalid attributes"));
        }
        let getter = heap
            .function(*id)
            .filter(|getter| {
                getter.func_id == export.getter && getter.home.is_none() && getter.class.is_none()
            })
            .ok_or_else(|| invalid("wrong getter function"))?;
        let capture = match crate::vm::module_getter_capture(export.target) {
            Some(module) => envs[module as usize],
            None => {
                uses_sentinel = true;
                sentinel
            }
        };
        if capture != Some(getter.env) {
            return Err(invalid("wrong getter environment"));
        }
        if *getters.entry(export.target).or_insert(*id) != *id {
            return Err(invalid("aliases do not share a getter"));
        }
        Ok(())
    };

    for (index, module) in build.modules.iter().enumerate() {
        let module_index = index as u32;
        let env = envs[index];
        if env.is_some() != build.module_needs_environment(module_index) {
            return Err(VmError::InvalidSnapshot(format!(
                "wrong environment presence for source module {:?}",
                module.id
            )));
        }
        if let Some(id) = env {
            let invalid = || {
                VmError::InvalidSnapshot(format!(
                    "invalid environment {id} for source module {:?}",
                    module.id
                ))
            };
            let Ok(Object::Environment(env)) = heap.get(id) else {
                return Err(invalid());
            };
            if env.module != Some(module_index) || env.parent.is_some() {
                return Err(invalid());
            }
            let declared: BTreeSet<_> = build.module_bindings(module_index).collect();
            if declared
                .iter()
                .any(|binding| !env.bindings.contains_key(binding))
            {
                return Err(invalid());
            }
            // Evaluation can add captured block bindings to the same environment.
            // Conditional blocks need not have run, unlike module declarations.
            let evaluation = build
                .function(module.evaluation)
                .expect("validated evaluation");
            let block_bindings: BTreeSet<_> = evaluation
                .code
                .iter()
                .filter_map(|instr| match instr {
                    crate::bytecode::Instr::DeclareBinding { binding } => Some(*binding),
                    _ => None,
                })
                .collect();
            if env
                .bindings
                .keys()
                .any(|binding| !declared.contains(binding) && !block_bindings.contains(binding))
            {
                return Err(invalid());
            }
        }

        let id = namespaces[index];
        let owner = format!("namespace for source module {:?}", module.id);
        let invalid = || VmError::InvalidSnapshot(format!("invalid {owner}: object {id}"));
        let Ok(object @ Object::Plain(plain)) = heap.get(id) else {
            return Err(invalid());
        };
        if plain.module_namespace != Some(module_index)
            || plain.is_error
            || object.prototype().is_some()
            || object.is_extensible()
            || plain.props.iter().count() != module.exports.len()
            || plain.symbol_props.iter().next().is_some()
        {
            return Err(invalid());
        }
        for (name, export) in &module.exports {
            validate_getter(
                object,
                name,
                export,
                crate::vm::MODULE_EXPORT_ATTRIBUTES,
                &owner,
            )?;
        }
    }

    if let Some(preload) = build.preload {
        let global = heap.get(builtins.get("globalThis"))?;
        for (name, export) in &build.modules[preload as usize].exports {
            validate_getter(
                global,
                name,
                export,
                crate::vm::PRELOAD_EXPORT_ATTRIBUTES,
                "preload global",
            )?;
        }
    }
    if sentinel.is_some() != uses_sentinel {
        return Err(VmError::InvalidSnapshot(
            "unexpected module getter sentinel environment".to_owned(),
        ));
    }
    Ok(())
}
