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

/// One normal control edge. `written` is the register the suspension entry
/// injects before the target runs, so the incoming value is not live there.
struct Edge {
    pc: usize,
    written: Option<Reg>,
}

pub(crate) fn live_in(
    code: &[Instr],
    suspensions: Suspensions<'_>,
    handlers: &[ExceptionHandler],
) -> Result<Vec<BTreeSet<Reg>>, CompileError> {
    let mut live_in = vec![BTreeSet::new(); code.len()];
    loop {
        let mut changed = false;
        for pc in (0..code.len()).rev() {
            let (uses, defs) = instr::uses_defs(&code[pc]);
            let mut next = BTreeSet::new();
            for edge in edges(code, suspensions, pc)? {
                let mut incoming = live_in[edge.pc].clone();
                for definition in &defs {
                    incoming.remove(definition);
                }
                if let Some(written) = edge.written {
                    incoming.remove(&written);
                }
                next.extend(incoming);
            }
            if may_throw(&code[pc]) {
                for handler in handlers.iter().filter(|handler| {
                    handler.start_pc as usize <= pc && pc < handler.end_pc as usize
                }) {
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
            return Ok(live_in);
        }
    }
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

fn edges(
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
        Instr::BranchFalse { target, .. } => vec![
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
