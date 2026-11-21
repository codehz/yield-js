//! Logical-register instruction buffer.
//!
//! Lowering talks to this type instead of building VM slots. Every emitted
//! slot is a [`Reg`] disguised as `SlotKind::Transient`; [`crate::allocate`]
//! is what assigns real frame slots.

use oxc::span::Span;
use yield_vm::bytecode::{Instr, Slot, SlotKind};

use crate::error::CompileError;

/// Logical register. Stable for one function until slot allocation.
pub(crate) type Reg = u16;

pub(crate) struct Emitter<'src> {
    source: &'src str,
    span: Span,
    code: Vec<Instr>,
    next_reg: Reg,
}

impl<'src> Emitter<'src> {
    pub(crate) fn new(source: &'src str, span: Span) -> Self {
        Self {
            source,
            span,
            code: Vec::new(),
            next_reg: 0,
        }
    }

    pub(crate) fn pc(&self) -> usize {
        self.code.len()
    }

    pub(crate) fn loop_header(&mut self) -> usize {
        // Keep the first-resume entry exclusive to the generator dispatcher.
        // Only the initial resume takes this jump; back edges target the header.
        if matches!(self.code.last(), Some(Instr::GeneratorStart)) {
            self.jump_to(self.pc() + 1);
        }
        self.pc()
    }

    pub(crate) fn reg_count(&self) -> u16 {
        self.next_reg
    }

    pub(crate) fn take_code(&mut self) -> Vec<Instr> {
        std::mem::take(&mut self.code)
    }

    pub(crate) fn alloc(&mut self) -> Result<Reg, CompileError> {
        let reg = self.next_reg;
        self.next_reg = self
            .next_reg
            .checked_add(1)
            .ok_or_else(|| CompileError::new(self.source, Span::default(), "too many slots"))?;
        Ok(reg)
    }

    /// Encode a logical register as a slot. Kind is a placeholder.
    pub(crate) fn slot(&self, reg: Reg) -> Slot {
        Slot {
            kind: SlotKind::Transient,
            index: reg,
        }
    }

    pub(crate) fn emit(&mut self, instruction: Instr) -> usize {
        let index = self.code.len();
        self.code.push(instruction);
        index
    }

    pub(crate) fn jump(&mut self) -> usize {
        self.emit(Instr::Jump { target: u32::MAX })
    }

    pub(crate) fn jump_to(&mut self, target: usize) -> usize {
        self.emit(Instr::Jump {
            target: target as u32,
        })
    }

    pub(crate) fn branch_false(&mut self, cond: Reg) -> usize {
        self.emit(Instr::BranchFalse {
            cond: self.slot(cond),
            target: u32::MAX,
        })
    }

    pub(crate) fn patch(&mut self, index: usize, target: usize) -> Result<(), CompileError> {
        let target = target as u32;
        match self.code.get_mut(index) {
            Some(Instr::Jump { target: slot }) | Some(Instr::BranchFalse { target: slot, .. }) => {
                *slot = target;
                Ok(())
            }
            _ => Err(CompileError::new(
                self.source,
                self.span,
                "internal compiler error: patch target is not a jump",
            )),
        }
    }
}
