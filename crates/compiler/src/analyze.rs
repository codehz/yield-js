//! CFG liveness over lowered bytecode.
//!
//! Successors include the fall-through edge, explicit jumps, both await resume
//! entries, and every covering exception edge of a throwing instruction. A
//! normal edge kills the instruction's definitions before joining; an exception
//! edge kills only the handler destination. Uses are joined last, so a
//! self-move is read before it is written. The saved set at an await is the
//! union of each resume entry's live-in after subtracting that entry's result
//! register.
use std::collections::BTreeSet;

use yield_vm::bytecode::{ExceptionHandler, Instr, ResumeEntry};

use crate::emit::Reg;
use crate::error::CompileError;
use crate::instr::{self, may_throw};

pub(crate) fn live_in(
    code: &[Instr],
    resumes: &[ResumeEntry],
    handlers: &[ExceptionHandler],
) -> Result<Vec<BTreeSet<Reg>>, CompileError> {
    let mut live_in = vec![BTreeSet::new(); code.len()];
    loop {
        let mut changed = false;
        for pc in (0..code.len()).rev() {
            let (uses, defs) = instr::uses_defs(&code[pc]);
            let mut next = BTreeSet::new();
            for successor in normal_successors(code, resumes, pc)? {
                let mut incoming = live_in[successor].clone();
                for definition in &defs {
                    incoming.remove(definition);
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

pub(crate) fn normal_successors(
    code: &[Instr],
    resumes: &[ResumeEntry],
    pc: usize,
) -> Result<Vec<usize>, CompileError> {
    let result = match &code[pc] {
        Instr::Await { resume, .. } => {
            let resume = resumes.get(*resume as usize).ok_or_else(|| {
                CompileError::internal("internal liveness error: missing resume entry")
            })?;
            vec![resume.fulfill_pc as usize, resume.reject_pc as usize]
        }
        Instr::Jump { target } => vec![*target as usize],
        Instr::BranchFalse { target, .. } => vec![*target as usize, pc + 1],
        Instr::Return { .. } | Instr::Throw { .. } | Instr::ThrowAssignmentToConst => Vec::new(),
        _ => vec![pc + 1],
    };
    for successor in &result {
        if *successor >= code.len() {
            return Err(CompileError::internal(
                "internal liveness error: successor out of range",
            ));
        }
    }
    Ok(result)
}
