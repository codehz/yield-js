//! Move bindings that are not shared across functions into logical registers.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::bindings::BindingFacts;
use crate::cfg::{BlockId, ENTRY, FunctionCfg, InstrId};
use crate::emit::Reg;
use crate::error::CompileError;
use crate::instr::may_throw;
use crate::ir::logical;
use yield_vm::BindingId;
use yield_vm::bytecode::Instr;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Declared {
    No,
    Yes,
    Maybe,
}

impl Declared {
    fn join(self, other: Self) -> Self {
        match (self, other) {
            (Self::No, Self::No) => Self::No,
            (Self::Yes, Self::Yes) => Self::Yes,
            _ => Self::Maybe,
        }
    }
}

pub(crate) fn promote(
    source: &str,
    function: &mut FunctionCfg,
    facts: &BindingFacts,
) -> Result<(), CompileError> {
    let mut accepted = BTreeMap::new();
    for (binding, owner) in &facts.owners {
        if *owner != function.attrs.func_id || facts.shared.contains(binding) {
            continue;
        }
        if let Some(inits) = promotion_inits(function, *binding) {
            accepted.insert(*binding, inits);
        }
    }

    let mut locals = BTreeMap::new();
    let mut next = function.attrs.reg_count;
    for &binding in accepted.keys() {
        let reg = next;
        next = next
            .checked_add(1)
            .ok_or_else(|| CompileError::new(source, function.attrs.span, "too many slots"))?;
        locals.insert(binding, reg);
    }
    function.attrs.reg_count = next;
    function.attrs.promoted = locals;

    for block in function.blocks.iter_mut().flatten() {
        for stmt in &mut block.statements {
            if let Some(replacement) =
                rewrite(&stmt.instr, stmt.id, &function.attrs.promoted, &accepted)
            {
                stmt.instr = replacement;
            }
        }
    }
    elide_iteration_environments(function, facts);
    Ok(())
}

/// Remove promoted bindings from every per-iteration environment and delete
/// groups that no longer carry any binding.
///
/// A group must stay even when every loop-head binding is promoted: a captured
/// binding declared inside the loop body still lands in the current
/// environment, and a closure created there observes that per-iteration cell.
/// Keeping the group with an empty binding list allocates the fresh
/// environment without copying the promoted loop-head bindings. A body binding
/// that merely failed promotion but is not captured does not need a fresh cell;
/// the function environment is enough because no closure can observe it.
fn elide_iteration_environments(function: &mut FunctionCfg, facts: &BindingFacts) {
    if function.iteration_envs.is_empty() {
        return;
    }
    let captured_decls: BTreeSet<_> = function
        .blocks
        .iter()
        .flatten()
        .flat_map(|block| &block.statements)
        .filter_map(|stmt| match stmt.instr {
            Instr::DeclareBinding { binding } if facts.shared.contains(&binding) => Some(stmt.id),
            _ => None,
        })
        .collect();
    let mut dead = BTreeSet::new();
    for group in std::mem::take(&mut function.iteration_envs) {
        // Keep groups lacking a restore: that state may still be referenced.
        let Some(restore) = group.restore else {
            continue;
        };
        if group
            .bindings
            .iter()
            .any(|binding| facts.shared.contains(binding))
            || captured_decls.range(group.save..restore).next().is_some()
        {
            continue;
        }
        dead.extend([group.save, restore]);
        dead.extend(group.enters);
    }
    for block in function.blocks.iter_mut().flatten() {
        block.statements.retain_mut(|stmt| {
            if dead.contains(&stmt.id) {
                return false;
            }
            if let Instr::NewIterationEnv { bindings, .. } = &mut stmt.instr {
                bindings.retain(|binding| !function.attrs.promoted.contains_key(binding));
            }
            true
        });
    }
}

/// `true` means the declaration already has a value and must self-move.
fn promotion_inits(function: &FunctionCfg, binding: BindingId) -> Option<BTreeMap<InstrId, bool>> {
    let attrs = &function.attrs;
    let is_param = attrs.param_bindings.contains(&binding)
        || attrs.rest_binding == Some(binding)
        || attrs.this_binding == Some(binding)
        || attrs.home_binding == Some(binding)
        || attrs.new_target_binding == Some(binding)
        || attrs.arguments_binding == Some(binding);
    let incoming = declared_in(function, binding, is_param);
    let mut inits = BTreeMap::new();
    for (block, mut state) in function.blocks.iter().zip(incoming) {
        for stmt in &block.as_ref().unwrap().statements {
            match stmt.instr {
                Instr::GetBinding { binding: id, .. } | Instr::SetBinding { binding: id, .. }
                    if id == binding && state.is_some_and(|state| state != Declared::Yes) =>
                {
                    return None;
                }
                Instr::DeclareBinding { binding: id } if id == binding => {
                    if !is_param && state == Some(Declared::Maybe) {
                        return None;
                    }
                    inits.insert(stmt.id, is_param || state == Some(Declared::Yes));
                    state = state.map(|_| Declared::Yes);
                }
                _ => {}
            }
        }
    }
    Some(inits)
}

fn declared_in(
    function: &FunctionCfg,
    binding: BindingId,
    entry_declared: bool,
) -> Vec<Option<Declared>> {
    let mut incoming = vec![None; function.blocks.len()];
    incoming[ENTRY.0] = Some(if entry_declared {
        Declared::Yes
    } else {
        Declared::No
    });
    let mut work = VecDeque::from([ENTRY]);
    while let Some(id) = work.pop_front() {
        let block = function.blocks[id.0].as_ref().unwrap();
        let mut state = incoming[id.0].unwrap();
        let mut propagate = |target, state| {
            if note(&mut incoming, target, state) {
                work.push_back(target);
            }
        };
        for stmt in &block.statements {
            for edge in block.exception_edges(&function.handlers, may_throw(&stmt.instr)) {
                propagate(edge.target, state);
            }
            if matches!(stmt.instr, Instr::DeclareBinding { binding: id } if id == binding) {
                state = Declared::Yes;
            }
        }
        for edge in block
            .exception_edges(&function.handlers, block.end.may_throw())
            .chain(block.end.successors())
        {
            propagate(edge.target, state);
        }
    }
    incoming
}

fn note(incoming: &mut [Option<Declared>], block: BlockId, state: Declared) -> bool {
    match incoming[block.0] {
        None => {
            incoming[block.0] = Some(state);
            true
        }
        Some(existing) => {
            let joined = existing.join(state);
            if joined == existing {
                false
            } else {
                incoming[block.0] = Some(joined);
                true
            }
        }
    }
}

fn rewrite(
    instruction: &Instr,
    id: InstrId,
    locals: &BTreeMap<BindingId, Reg>,
    accepted: &BTreeMap<BindingId, BTreeMap<InstrId, bool>>,
) -> Option<Instr> {
    Some(match instruction {
        Instr::GetBinding { dst, binding } if locals.contains_key(binding) => Instr::Move {
            dst: *dst,
            src: logical(locals[binding]),
        },
        Instr::SetBinding { binding, src } if locals.contains_key(binding) => Instr::Move {
            dst: logical(locals[binding]),
            src: *src,
        },
        Instr::DeclareBinding { binding } if locals.contains_key(binding) => {
            let local = logical(locals[binding]);
            let keep = accepted[binding][&id];
            if keep {
                Instr::Move {
                    dst: local,
                    src: local,
                }
            } else {
                Instr::Undefined { dst: local }
            }
        }
        _ => return None,
    })
}
