//! Move bindings that are not shared across functions into logical registers.
//!
//! Ordinary replacements are one instruction for one instruction, so jump,
//! resume, and handler PCs stay put. Expression temps are left alone; a later
//! store cannot overwrite an operand that a compound expression still needs.
//!
//! After rewriting, loop-head bindings that no nested function reads or writes
//! are removed from their per-iteration environment. A group that keeps no
//! binding and has no captured body declaration is deleted by a compacting
//! pass that also remaps every PC-bearing table.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use yield_vm::BindingId;
use yield_vm::bytecode::{ExceptionHandler, GeneratorEntry, Instr, ResumeEntry, Slot, SlotKind};

use crate::analyze;
use crate::bindings::BindingFacts;
use crate::emit::Reg;
use crate::error::CompileError;
use crate::instr::may_throw;
use crate::ir::LoweredFunction;

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
    function: &mut LoweredFunction,
    facts: &BindingFacts,
) -> Result<(), CompileError> {
    let resumes = resume_stubs(function);
    let generators = generator_stubs(function);
    let suspensions = analyze::Suspensions {
        awaits: &resumes,
        generators: &generators,
    };
    let mut accepted: Vec<(BindingId, BTreeMap<usize, bool>)> = Vec::new();
    for (binding, owner) in &facts.owners {
        if *owner != function.func_id || facts.shared.contains(binding) {
            continue;
        }
        if let Some(inits) = promotion_inits(function, suspensions, *binding)? {
            accepted.push((*binding, inits));
        }
    }

    let mut locals = BTreeMap::new();
    let mut next = function.reg_count;
    for (binding, _) in &accepted {
        let reg = next;
        next = next
            .checked_add(1)
            .ok_or_else(|| CompileError::new(source, function.span, "too many slots"))?;
        locals.insert(*binding, reg);
    }
    function.reg_count = next;
    function.promoted = locals;

    let mut code = Vec::with_capacity(function.code.len());
    for (pc, instruction) in function.code.iter().enumerate() {
        code.push(rewrite(instruction, pc, &function.promoted, &accepted));
    }
    function.code = code;
    elide_iteration_environments(function, facts)?;
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
fn elide_iteration_environments(
    function: &mut LoweredFunction,
    facts: &BindingFacts,
) -> Result<(), CompileError> {
    if function.iteration_envs.is_empty() {
        return Ok(());
    }
    for instruction in &mut function.code {
        if let Instr::NewIterationEnv { bindings, .. } = instruction {
            bindings.retain(|binding| !function.promoted.contains_key(binding));
        }
    }

    let groups = std::mem::take(&mut function.iteration_envs);
    let mut dead = BTreeSet::new();
    for group in &groups {
        let escaping_head = group
            .bindings
            .iter()
            .any(|binding| facts.shared.contains(binding));
        let body_declares_environment_binding = match group.restore_pc {
            Some(restore_pc) => {
                function.code[group.save_pc..restore_pc]
                    .iter()
                    .any(|instruction| match instruction {
                        Instr::DeclareBinding { binding } => {
                            !function.promoted.contains_key(binding)
                                && facts.shared.contains(binding)
                        }
                        _ => false,
                    })
            }
            // A missing restore is malformed lowering; keep the group rather
            // than deleting state that is still referenced.
            None => true,
        };
        if escaping_head || body_declares_environment_binding {
            continue;
        }
        dead.insert(group.save_pc);
        dead.extend(group.enter_pcs.iter().copied());
        let restore_pc = group.restore_pc.ok_or_else(|| {
            CompileError::internal("internal compiler error: iteration environment has no restore")
        })?;
        dead.insert(restore_pc);
    }
    if dead.is_empty() {
        return Ok(());
    }
    compact_dead_instructions(function, &dead)
}

/// Delete instructions and remap every PC-bearing field. Jump targets that
/// pointed at a deleted instruction land on the next surviving instruction,
/// which is the loop header or exit the deleted environment instruction stood
/// in front of.
fn compact_dead_instructions(
    function: &mut LoweredFunction,
    dead: &BTreeSet<usize>,
) -> Result<(), CompileError> {
    let old_len = function.code.len();
    if dead.iter().any(|&pc| pc >= old_len) {
        return Err(CompileError::internal(
            "internal compiler error: deleted instruction is out of range",
        ));
    }
    let mut mapping = Vec::with_capacity(old_len + 1);
    let mut next = 0u32;
    for pc in 0..old_len {
        mapping.push(next);
        if !dead.contains(&pc) {
            next = next.checked_add(1).ok_or_else(|| {
                CompileError::internal("internal compiler error: too many instructions")
            })?;
        }
    }
    mapping.push(next);
    let remap = |pc: u32| -> Result<u32, CompileError> {
        mapping
            .get(pc as usize)
            .copied()
            .ok_or_else(|| CompileError::internal("internal compiler error: PC is out of range"))
    };

    let mut code = Vec::with_capacity(next as usize);
    for (pc, instruction) in std::mem::take(&mut function.code).into_iter().enumerate() {
        if dead.contains(&pc) {
            continue;
        }
        code.push(match instruction {
            Instr::Jump { target } => Instr::Jump {
                target: remap(target)?,
            },
            Instr::BranchFalse { cond, target } => Instr::BranchFalse {
                cond,
                target: remap(target)?,
            },
            other => other,
        });
    }
    function.code = code;

    for site in &mut function.awaits {
        site.fulfill_pc = remap(site.fulfill_pc as u32)? as usize;
        site.reject_pc = remap(site.reject_pc as u32)? as usize;
    }
    for site in &mut function.yields {
        site.next_pc = remap(site.next_pc as u32)? as usize;
        site.throw_pc = remap(site.throw_pc as u32)? as usize;
        site.return_pc = remap(site.return_pc as u32)? as usize;
    }
    if let Some(start) = &mut function.generator_start {
        start.body_pc = remap(start.body_pc as u32)? as usize;
    }
    for handler in &mut function.handlers {
        handler.start_pc = remap(handler.start_pc)?;
        handler.end_pc = remap(handler.end_pc)?;
        handler.target_pc = remap(handler.target_pc)?;
    }
    Ok(())
}

/// `true` means the declaration already has a value and must self-move.
fn promotion_inits(
    function: &LoweredFunction,
    suspensions: analyze::Suspensions<'_>,
    binding: BindingId,
) -> Result<Option<BTreeMap<usize, bool>>, CompileError> {
    let is_param = function.param_bindings.contains(&binding)
        || function.rest_binding == Some(binding)
        || function.this_binding == Some(binding)
        || function.home_binding == Some(binding)
        || function.new_target_binding == Some(binding)
        || function.arguments_binding == Some(binding);
    let incoming = declared_in(function, suspensions, binding, is_param)?;
    let mut inits = BTreeMap::new();
    for (pc, instruction) in function.code.iter().enumerate() {
        let Some(state) = incoming[pc] else {
            if let Instr::DeclareBinding { binding: id } = instruction
                && *id == binding
            {
                inits.insert(pc, is_param);
            }
            continue;
        };
        match instruction {
            Instr::GetBinding { binding: id, .. } | Instr::SetBinding { binding: id, .. }
                if *id == binding && state != Declared::Yes =>
            {
                return Ok(None);
            }
            Instr::DeclareBinding { binding: id } if *id == binding => {
                if is_param {
                    inits.insert(pc, true);
                } else if state == Declared::Maybe {
                    return Ok(None);
                } else {
                    inits.insert(pc, state == Declared::Yes);
                }
            }
            _ => {}
        }
    }
    Ok(Some(inits))
}

fn declared_in(
    function: &LoweredFunction,
    suspensions: analyze::Suspensions<'_>,
    binding: BindingId,
    entry_declared: bool,
) -> Result<Vec<Option<Declared>>, CompileError> {
    let mut incoming = vec![None; function.code.len()];
    if function.code.is_empty() {
        return Ok(incoming);
    }
    incoming[0] = Some(if entry_declared {
        Declared::Yes
    } else {
        Declared::No
    });
    let mut work = VecDeque::from([0]);
    while let Some(pc) = work.pop_front() {
        let before = incoming[pc].expect("queued instruction is reachable");
        let after = if matches!(function.code[pc], Instr::DeclareBinding { binding: id } if id == binding)
        {
            Declared::Yes
        } else {
            before
        };
        for successor in analyze::normal_successors(&function.code, suspensions, pc)? {
            if note(&mut incoming, successor, after) {
                work.push_back(successor);
            }
        }
        if may_throw(&function.code[pc]) {
            for handler in covering(&function.handlers, pc) {
                let target = handler.target_pc as usize;
                if target >= function.code.len() {
                    return Err(CompileError::internal(
                        "internal compiler error: exception edge is out of range",
                    ));
                }
                if note(&mut incoming, target, before) {
                    work.push_back(target);
                }
            }
        }
    }
    Ok(incoming)
}

fn note(incoming: &mut [Option<Declared>], pc: usize, state: Declared) -> bool {
    match incoming[pc] {
        None => {
            incoming[pc] = Some(state);
            true
        }
        Some(existing) => {
            let joined = existing.join(state);
            if joined == existing {
                false
            } else {
                incoming[pc] = Some(joined);
                true
            }
        }
    }
}

fn covering(handlers: &[ExceptionHandler], pc: usize) -> impl Iterator<Item = &ExceptionHandler> {
    handlers
        .iter()
        .filter(move |handler| handler.start_pc as usize <= pc && pc < handler.end_pc as usize)
}

fn rewrite(
    instruction: &Instr,
    pc: usize,
    locals: &BTreeMap<BindingId, Reg>,
    accepted: &[(BindingId, BTreeMap<usize, bool>)],
) -> Instr {
    match instruction {
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
            let keep = accepted
                .iter()
                .find(|(id, _)| id == binding)
                .and_then(|(_, inits)| inits.get(&pc).copied())
                .unwrap_or(false);
            if keep {
                Instr::Move {
                    dst: local,
                    src: local,
                }
            } else {
                Instr::Undefined { dst: local }
            }
        }
        other => other.clone(),
    }
}

fn logical(reg: Reg) -> Slot {
    Slot {
        kind: SlotKind::Transient,
        index: reg,
    }
}

fn generator_stubs(function: &LoweredFunction) -> Vec<GeneratorEntry> {
    function
        .yields
        .iter()
        .map(|site| GeneratorEntry {
            next_pc: site.next_pc as u32,
            next_dst: logical(site.next),
            throw_pc: site.throw_pc as u32,
            throw_dst: logical(site.thrown),
            return_pc: site.return_pc as u32,
            saved_slots: Vec::new(),
        })
        .collect()
}

fn resume_stubs(function: &LoweredFunction) -> Vec<ResumeEntry> {
    function
        .awaits
        .iter()
        .map(|site| {
            let result = Slot {
                kind: SlotKind::Transient,
                index: site.result,
            };
            ResumeEntry {
                fulfill_pc: site.fulfill_pc as u32,
                fulfill_dst: result,
                reject_pc: site.reject_pc as u32,
                reject_dst: logical(site.reject),
                saved_slots: Vec::new(),
            }
        })
        .collect()
}
