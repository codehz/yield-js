//! Which bindings must stay in a heap environment.
//!
//! Ownership comes only from parameters and `DeclareBinding`. A read or write
//! in a different function keeps that id shared, including a capture nested
//! several functions away and a capture that is only written. A closure
//! instruction does not by itself pin every binding in the scope.

use std::collections::{BTreeMap, BTreeSet};

use yield_vm::BindingId;
use yield_vm::bytecode::Instr;

use crate::error::CompileError;
use crate::ir::LoweredFunction;

pub(crate) struct BindingFacts {
    pub(crate) owners: BTreeMap<BindingId, u32>,
    pub(crate) shared: BTreeSet<BindingId>,
}

pub(crate) fn classify(functions: &[LoweredFunction]) -> Result<BindingFacts, CompileError> {
    let mut owners = BTreeMap::new();
    for function in functions {
        for binding in function.param_bindings.iter().chain(&function.rest_binding) {
            record_owner(&mut owners, *binding, function.func_id)?;
        }
        if let Some(binding) = function.this_binding {
            record_owner(&mut owners, binding, function.func_id)?;
        }
        for instruction in &function.code {
            if let Instr::DeclareBinding { binding } = instruction {
                record_owner(&mut owners, *binding, function.func_id)?;
            }
        }
    }

    let mut shared = BTreeSet::new();
    for function in functions {
        for instruction in &function.code {
            let binding = match instruction {
                Instr::GetBinding { binding, .. } | Instr::SetBinding { binding, .. } => *binding,
                _ => continue,
            };
            let Some(owner) = owners.get(&binding).copied() else {
                return Err(CompileError::internal(format!(
                    "internal compiler error: binding {binding} has no owner"
                )));
            };
            if owner != function.func_id {
                shared.insert(binding);
            }
        }
    }

    Ok(BindingFacts { owners, shared })
}

fn record_owner(
    owners: &mut BTreeMap<BindingId, u32>,
    binding: BindingId,
    func_id: u32,
) -> Result<(), CompileError> {
    match owners.entry(binding) {
        std::collections::btree_map::Entry::Vacant(slot) => {
            slot.insert(func_id);
            Ok(())
        }
        std::collections::btree_map::Entry::Occupied(slot) => {
            if *slot.get() == func_id {
                Ok(())
            } else {
                Err(CompileError::internal(format!(
                    "internal compiler error: binding {binding} has conflicting owners {} and {func_id}",
                    slot.get()
                )))
            }
        }
    }
}
