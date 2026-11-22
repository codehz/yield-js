//! CFG liveness over lowered bytecode.
//!
//! Successors include the fall-through edge, explicit jumps, both await resume
//! entries, all three generator resume entries, and every covering exception
//! edge of a throwing instruction. A suspension edge writes the value it was
//! resumed with, so that destination is not live on the edge even though the
//! suspension instruction itself declares no definition. A normal edge kills
//! the instruction's definitions before joining; an exception edge kills only
//! the handler destination. Uses are joined last, so a self-move is read before
//! it is written. The saved set at an await or yield is the union of each
//! entry's live-in after subtracting that entry's own injected result register;
//! subtracting after the union would drop a value another entry still needs.
//!
//! Only plain slot copies and constant loads may be deleted. Their operands
//! become live only if the written value is needed, so dead copy chains vanish
//! in the same fixed point used for allocation.
use std::collections::BTreeSet;

use yield_vm::bytecode::{ExceptionHandler, GeneratorEntry, Instr, ResumeEntry};

use crate::emit::Reg;
use crate::error::CompileError;
use crate::instr::{self, may_throw};

/// Every suspension table of one function, so edges stay independent of shape.
#[derive(Clone, Copy)]
pub(crate) struct Suspensions<'a> {
    pub(crate) awaits: &'a [ResumeEntry],
    pub(crate) generators: &'a [GeneratorEntry],
}

/// A normal or resumption edge and its optional injected register.
pub(crate) struct Edge {
    pub(crate) pc: usize,
    pub(crate) written: Option<Reg>,
}

pub(crate) struct Liveness {
    pub(crate) live_in: Vec<BTreeSet<Reg>>,
    pub(crate) dead_writes: BTreeSet<usize>,
}

pub(crate) fn live_in_and_dead_writes(
    code: &[Instr],
    suspensions: Suspensions<'_>,
    handlers: &[ExceptionHandler],
) -> Result<Liveness, CompileError> {
    let mut live_in = vec![BTreeSet::new(); code.len()];
    loop {
        let mut changed = false;
        for pc in (0..code.len()).rev() {
            if removable_write(&code[pc], live_in.get(pc + 1)) {
                let next = &live_in[pc + 1];
                if *next != live_in[pc] {
                    live_in[pc] = next.clone();
                    changed = true;
                }
                continue;
            }
            let (uses, defs) = instr::uses_defs(&code[pc]);
            let mut next = BTreeSet::new();
            for edge in edges(code, suspensions, pc)? {
                let mut incoming = live_in[edge.pc].clone();
                if !is_suspend(&code[pc]) {
                    for definition in &defs {
                        incoming.remove(definition);
                    }
                }
                if let Some(written) = edge.written {
                    incoming.remove(&written);
                }
                next.extend(incoming);
            }
            if may_throw(&code[pc]) {
                for handler in covering_handlers(handlers, pc) {
                    let mut target = live_in[handler.target_pc as usize].clone();
                    target.remove(&handler.dst.index);
                    next.extend(target);
                }
            }
            next.extend(uses);
            if next != live_in[pc] {
                live_in[pc] = next;
                changed = true;
            }
        }
        if !changed {
            let dead_writes = code
                .iter()
                .enumerate()
                .filter_map(|(pc, instruction)| {
                    removable_write(instruction, live_in.get(pc + 1)).then_some(pc)
                })
                .collect();
            return Ok(Liveness {
                live_in,
                dead_writes,
            });
        }
    }
}

/// The allowlist is stronger than `!may_throw`: none of these instructions
/// can run script, mutate the heap, or change control flow. Compiler-emitted
/// moves read initialized registers; constants refer to the build's pool.
fn removable_write(instruction: &Instr, after: Option<&BTreeSet<Reg>>) -> bool {
    let Some(after) = after else {
        return false;
    };
    match instruction {
        Instr::Move { dst, src } => dst == src || !after.contains(&dst.index),
        Instr::Const { dst, .. } | Instr::Undefined { dst } => !after.contains(&dst.index),
        _ => false,
    }
}

pub(crate) fn covering_handlers(
    handlers: &[ExceptionHandler],
    pc: usize,
) -> impl Iterator<Item = &ExceptionHandler> {
    handlers
        .iter()
        .filter(move |handler| handler.start_pc as usize <= pc && pc < handler.end_pc as usize)
}

pub(crate) fn is_suspend(instruction: &Instr) -> bool {
    matches!(instruction, Instr::Await { .. } | Instr::Yield { .. })
}

pub(crate) fn ends_block(instruction: &Instr) -> bool {
    is_suspend(instruction)
        || matches!(
            instruction,
            Instr::Jump { .. }
                | Instr::BranchFalse { .. }
                | Instr::BranchCompareFalse { .. }
                | Instr::BranchCompareConstFalse { .. }
                | Instr::GeneratorStart
                | Instr::Return { .. }
                | Instr::Throw { .. }
                | Instr::ThrowAssignmentToConst
                | Instr::ThrowDeleteSuper
        )
}

/// Successors without the per-edge destinations, for passes that only need
/// reachability.
pub(crate) fn normal_successors(
    code: &[Instr],
    suspensions: Suspensions<'_>,
    pc: usize,
) -> Result<Vec<usize>, CompileError> {
    Ok(edges(code, suspensions, pc)?
        .into_iter()
        .map(|edge| edge.pc)
        .collect())
}

pub(crate) fn edges(
    code: &[Instr],
    suspensions: Suspensions<'_>,
    pc: usize,
) -> Result<Vec<Edge>, CompileError> {
    let result = match &code[pc] {
        Instr::Await { resume, .. } => {
            let resume = suspensions.awaits.get(*resume as usize).ok_or_else(|| {
                CompileError::internal("internal liveness error: missing resume entry")
            })?;
            vec![
                Edge {
                    pc: resume.fulfill_pc as usize,
                    written: Some(resume.fulfill_dst.index),
                },
                Edge {
                    pc: resume.reject_pc as usize,
                    written: Some(resume.reject_dst.index),
                },
            ]
        }
        Instr::Yield { generator, .. } => {
            let entry = suspensions
                .generators
                .get(*generator as usize)
                .ok_or_else(|| {
                    CompileError::internal("internal liveness error: missing generator entry")
                })?;
            vec![
                Edge {
                    pc: entry.next_pc as usize,
                    written: Some(entry.next_dst.index),
                },
                Edge {
                    pc: entry.throw_pc as usize,
                    written: Some(entry.throw_dst.index),
                },
                Edge {
                    pc: entry.return_pc as usize,
                    written: None,
                },
            ]
        }
        Instr::Jump { target } => vec![Edge {
            pc: *target as usize,
            written: None,
        }],
        Instr::BranchFalse { target, .. }
        | Instr::BranchCompareFalse { target, .. }
        | Instr::BranchCompareConstFalse { target, .. } => vec![
            Edge {
                pc: *target as usize,
                written: None,
            },
            Edge {
                pc: pc + 1,
                written: None,
            },
        ],
        Instr::Return { .. }
        | Instr::Throw { .. }
        | Instr::ThrowAssignmentToConst
        | Instr::ThrowDeleteSuper => Vec::new(),
        // Physically adjacent, but only the generator dispatcher can enter it.
        Instr::GeneratorStart => vec![Edge {
            pc: pc + 1,
            written: None,
        }],
        _ => vec![Edge {
            pc: pc + 1,
            written: None,
        }],
    };
    for edge in &result {
        if edge.pc >= code.len() {
            return Err(CompileError::internal(
                "internal liveness error: successor out of range",
            ));
        }
    }
    Ok(result)
}
