//! //! Jumps and conditional branches. They only read slots and write the PC; they never allocate.

use crate::bytecode::{Constant, Instr};
use crate::error::VmError;
use crate::frame::Frame;
use crate::gc::ActiveRoots;
use crate::host::{Host, PlatformServices};
use crate::vm::VmCore;

use super::Step;

impl VmCore {
    pub(super) fn step_branch<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        frame: &mut Frame,
        instr: &Instr,
        constants: &[Constant],
    ) -> Result<Step, VmError> {
        // Reentrant calls borrow the initialized slots; destination writes
        // happen after the last use of this view in each instruction.
        let frame_roots = &roots.with(&*frame);
        match *instr {
            Instr::Jump { target } => {
                frame.pc = target as usize;
                Ok(Step::Next)
            }
            Instr::BranchFalse { cond, target } => {
                if !frame.read(cond)?.truthy() {
                    frame.pc = target as usize;
                }
                Ok(Step::Next)
            }
            Instr::BranchTrue { cond, target } => {
                if frame.read(cond)?.truthy() {
                    frame.pc = target as usize;
                }
                Ok(Step::Next)
            }
            Instr::BranchCompareFalse {
                lhs,
                op,
                rhs,
                target,
            } => {
                let lhs = frame.read(lhs)?;
                let rhs = frame.read(rhs)?;
                match self.branch_compare(host, frame_roots, op, lhs, rhs)? {
                    Ok(true) => {}
                    Ok(false) => frame.pc = target as usize,
                    Err(error) => return Ok(Step::Throw(error)),
                }
                Ok(Step::Next)
            }
            Instr::BranchCompareConstFalse {
                lhs,
                op,
                constant,
                target,
            } => {
                let lhs = frame.read(lhs)?;
                let rhs = Self::constant(constants, constant)?;
                match self.branch_compare(host, frame_roots, op, lhs, rhs)? {
                    Ok(true) => {}
                    Ok(false) => frame.pc = target as usize,
                    Err(error) => return Ok(Step::Throw(error)),
                }
                Ok(Step::Next)
            }
            Instr::BranchCompareTrue {
                lhs,
                op,
                rhs,
                target,
            } => {
                let lhs = frame.read(lhs)?;
                let rhs = frame.read(rhs)?;
                match self.branch_compare(host, frame_roots, op, lhs, rhs)? {
                    Ok(true) => frame.pc = target as usize,
                    Ok(false) => {}
                    Err(error) => return Ok(Step::Throw(error)),
                }
                Ok(Step::Next)
            }
            Instr::BranchCompareConstTrue {
                lhs,
                op,
                constant,
                target,
            } => {
                let lhs = frame.read(lhs)?;
                let rhs = Self::constant(constants, constant)?;
                match self.branch_compare(host, frame_roots, op, lhs, rhs)? {
                    Ok(true) => frame.pc = target as usize,
                    Ok(false) => {}
                    Err(error) => return Ok(Step::Throw(error)),
                }
                Ok(Step::Next)
            }
            Instr::BranchBinary {
                op,
                lhs,
                rhs,
                target,
            } => {
                let lhs = frame.read(lhs)?;
                let rhs = frame.read(rhs)?;
                if Self::is_comparison(op) {
                    match self.branch_compare(host, frame_roots, op, lhs, rhs)? {
                        Ok(true) => {}
                        Ok(false) => frame.pc = target as usize,
                        Err(error) => return Ok(Step::Throw(error)),
                    }
                    return Ok(Step::Next);
                }
                match self.binary_value(host, frame_roots, op, lhs, rhs)? {
                    Ok(value) => {
                        if !value.truthy() {
                            frame.pc = target as usize;
                        }
                        Ok(Step::Next)
                    }
                    Err(error) => Ok(Step::Throw(error)),
                }
            }
            Instr::BranchBinaryConst {
                op,
                lhs,
                constant,
                target,
            } => {
                let lhs = frame.read(lhs)?;
                let rhs = Self::constant(constants, constant)?;
                if Self::is_comparison(op) {
                    match self.branch_compare(host, frame_roots, op, lhs, rhs)? {
                        Ok(true) => {}
                        Ok(false) => frame.pc = target as usize,
                        Err(error) => return Ok(Step::Throw(error)),
                    }
                    return Ok(Step::Next);
                }
                match self.binary_value(host, frame_roots, op, lhs, rhs)? {
                    Ok(value) => {
                        if !value.truthy() {
                            frame.pc = target as usize;
                        }
                        Ok(Step::Next)
                    }
                    Err(error) => Ok(Step::Throw(error)),
                }
            }
            Instr::BranchUnary { op, src, target } => {
                let value = frame.read(src)?;
                match self.unary_value(host, frame_roots, op, value)? {
                    Ok(value) => {
                        if !value.truthy() {
                            frame.pc = target as usize;
                        }
                        Ok(Step::Next)
                    }
                    Err(error) => Ok(Step::Throw(error)),
                }
            }
            _ => unreachable!("step dispatch routes only these opcodes here"),
        }
    }
}
