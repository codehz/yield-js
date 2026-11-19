//! Move bindings that are not shared across functions into logical registers.
//!
//! Replacements are one instruction for one instruction, so jump, resume, and
//! handler PCs stay put. Expression temps are left alone; a later store cannot
//! overwrite an operand that a compound expression still needs.

use std::collections::{BTreeMap, VecDeque};

use yield_vm::BindingId;
use yield_vm::bytecode::{ExceptionHandler, Instr, ResumeEntry, Slot, SlotKind};

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
    let mut accepted: Vec<(BindingId, BTreeMap<usize, bool>)> = Vec::new();
    for (binding, owner) in &facts.owners {
        if *owner != function.func_id || facts.shared.contains(binding) {
            continue;
        }
        if let Some(inits) = promotion_inits(function, &resumes, *binding)? {
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
    Ok(())
}

/// `true` means the declaration already has a value and must self-move.
fn promotion_inits(
    function: &LoweredFunction,
    resumes: &[ResumeEntry],
    binding: BindingId,
) -> Result<Option<BTreeMap<usize, bool>>, CompileError> {
    let is_param = function.param_bindings.contains(&binding)
        || function.rest_binding == Some(binding)
        || function.this_binding == Some(binding);
    let incoming = declared_in(function, resumes, binding, is_param)?;
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
    resumes: &[ResumeEntry],
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
        for successor in analyze::normal_successors(&function.code, resumes, pc)? {
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
                reject_dst: result,
                saved_slots: Vec::new(),
            }
        })
        .collect()
}
