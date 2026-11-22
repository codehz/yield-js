//! Lay out surviving blocks in input order, then encode concrete targets.

use std::collections::BTreeSet;

use yield_vm::bytecode::{
    ExceptionHandler, GeneratorEntry, GeneratorStart, Instr, ResumeEntry, StackSite,
};

use super::{BlockId, Condition, FunctionCfg, Terminator, invalid};
use crate::analyze::Liveness;
use crate::emit::Reg;
use crate::error::CompileError;
use crate::instr::may_throw;
use crate::ir::{FunctionAttrs, logical};
use crate::source_lines::SourceLines;

/// Concrete layout, still using logical registers in code and table entries.
/// The recovery tables' `saved_slots` carry logical registers until allocation.
pub(crate) struct LaidOutFunction {
    pub(crate) attrs: FunctionAttrs,
    pub(crate) code: Vec<Instr>,
    pub(crate) bounds: RegisterBounds,
    pub(crate) resumes: Vec<ResumeEntry>,
    pub(crate) yields: Vec<GeneratorEntry>,
    pub(crate) generator_start: Option<GeneratorStart>,
    pub(crate) handlers: Vec<ExceptionHandler>,
}

#[derive(Default)]
pub(crate) struct RegisterBounds(pub(crate) Vec<Option<(usize, usize)>>);

impl RegisterBounds {
    pub(crate) fn touch(&mut self, reg: Reg, pc: usize) {
        let bound = &mut self.0[usize::from(reg)];
        let (first, last) = bound.unwrap_or((pc, pc));
        *bound = Some((first.min(pc), last.max(pc)));
    }
}

struct Position {
    start: u32,
    separator: bool,
    encode_end: bool,
}

fn index(value: usize) -> Result<u32, CompileError> {
    u32::try_from(value).map_err(|_| invalid("output index out of range"))
}

impl FunctionCfg {
    pub(crate) fn into_lowered(
        self,
        live: &Liveness,
        mut lines: Option<&mut SourceLines>,
    ) -> Result<(LaidOutFunction, Vec<StackSite>), CompileError> {
        let Self {
            attrs,
            blocks,
            handlers,
            ..
        } = self;
        let mut distinct = BTreeSet::new();
        let mut stubs = BTreeSet::new();
        for block in blocks.iter().flatten() {
            match block.end {
                Terminator::GeneratorStart { body } => {
                    distinct.insert(body);
                }
                Terminator::Yield {
                    thrown, returned, ..
                } => {
                    stubs.extend([thrown.target, returned]);
                }
                _ => {}
            }
        }
        distinct.extend(stubs.iter().copied());
        // A handler entry cannot compact into its own protected interval.
        distinct.extend(handlers.iter().map(|h| h.entry.target));

        let mut positions: Vec<Option<Position>> = (0..blocks.len()).map(|_| None).collect();
        let mut order = blocks
            .iter()
            .enumerate()
            .filter_map(|(id, block)| block.as_ref().map(|block| (BlockId(id), block)))
            .peekable();
        let mut len = 0u32;
        let mut falls_through = false;
        while let Some((id, block)) = order.next() {
            let adjacent = order.peek().map(|(id, _)| *id);
            // The verifier checks lexical predecessors, even when the preceding
            // opcode (e.g. Yield) cannot actually fall through to this stub.
            let separator = falls_through && stubs.contains(&id);
            let encode_end = !matches!(block.end, Terminator::Next(target) | Terminator::Jump(target)
                if Some(target) == adjacent && !(block.statements.is_empty() && distinct.contains(&id)));
            let start = len
                .checked_add(u32::from(separator))
                .ok_or_else(|| invalid("too many instructions"))?;
            let width = block
                .statements
                .len()
                .checked_add(usize::from(encode_end))
                .ok_or_else(|| invalid("too many instructions"))?;
            len = start
                .checked_add(index(width)?)
                .ok_or_else(|| invalid("too many instructions"))?;
            if width != 0 {
                falls_through = match &block.end {
                    Terminator::Next(_) | Terminator::Jump(_) => !encode_end,
                    Terminator::GeneratorStart { .. } => false,
                    Terminator::Finish(instr) => {
                        !matches!(instr, Instr::Return { .. } | Instr::Throw { .. })
                    }
                    _ => true,
                };
            }
            positions[id.0] = Some(Position {
                start,
                separator,
                encode_end,
            });
        }
        let pc = |id: BlockId| {
            positions
                .get(id.0)
                .and_then(Option::as_ref)
                .map(|pos| pos.start)
                .filter(|&pc| pc < len)
                .ok_or_else(|| invalid("layout target missing"))
        };
        let bounds = RegisterBounds(vec![None; usize::from(attrs.reg_count)]);
        let mut lowered = LaidOutFunction {
            attrs,
            code: Vec::with_capacity(len as usize),
            bounds,
            resumes: Vec::new(),
            yields: Vec::new(),
            generator_start: None,
            handlers: Vec::new(),
        };
        let mut stack_sites = Vec::new();
        let mut previous = None;
        let mut record = |pc: usize, origin: Option<u32>| {
            if previous == origin {
                return;
            }
            previous = origin;
            if let Some(lines) = lines.as_deref_mut() {
                let (line, column) = origin.map_or((0, 0), |offset| lines.position(offset));
                stack_sites.push(StackSite {
                    pc: pc as u32,
                    line,
                    column,
                });
            }
        };
        let mut ranges = vec![None::<(u32, u32, bool)>; handlers.len()];
        for (id, block) in blocks.into_iter().enumerate() {
            let Some(block) = block else { continue };
            let pos = positions[id].as_ref().unwrap();
            let code = &mut lowered.code;
            if pos.separator {
                record(code.len(), None);
                code.push(Instr::Jump {
                    target: pos.start - 1,
                });
            }
            live.visit_points(
                &block,
                &handlers,
                pos.start as usize,
                pos.encode_end,
                |reg, pc| lowered.bounds.touch(reg, pc),
            );
            let mut can_throw = block.end.may_throw();
            for statement in block.statements {
                can_throw |= may_throw(&statement.instr);
                record(code.len(), statement.origin);
                code.push(statement.instr);
            }
            if pos.encode_end {
                let saved = match block.end {
                    Terminator::Await { .. }
                    | Terminator::Yield { .. }
                    | Terminator::GeneratorStart { .. } => live.saved_on_entry(&block.end),
                    _ => Vec::new(),
                };
                let next_pc = index(code.len() + 1)?;
                let adjacent = |target| {
                    let target = pc(target)?;
                    if target != next_pc {
                        return Err(invalid("required fallthrough is not adjacent"));
                    }
                    Ok(target)
                };
                let instr = match block.end {
                    Terminator::Next(target) | Terminator::Jump(target) => Instr::Jump {
                        target: pc(target)?,
                    },
                    Terminator::Branch { test, taken, next } => {
                        adjacent(next)?;
                        test.encode(pc(taken)?)
                    }
                    Terminator::Finish(instr) => instr,
                    Terminator::Await {
                        input,
                        fulfill,
                        reject,
                    } => {
                        let resume = index(lowered.resumes.len())?;
                        lowered.resumes.push(ResumeEntry {
                            fulfill_dst: logical(fulfill.dst),
                            reject_dst: logical(reject.dst),
                            fulfill_pc: pc(fulfill.target)?,
                            reject_pc: pc(reject.target)?,
                            saved_slots: saved,
                        });
                        Instr::Await {
                            input,
                            dst: logical(fulfill.dst),
                            resume,
                        }
                    }
                    Terminator::Yield {
                        input,
                        delegate,
                        next,
                        thrown,
                        returned,
                    } => {
                        let generator = index(lowered.yields.len())?;
                        lowered.yields.push(GeneratorEntry {
                            next_dst: logical(next.dst),
                            throw_dst: logical(thrown.dst),
                            next_pc: adjacent(next.target)?,
                            throw_pc: pc(thrown.target)?,
                            return_pc: pc(returned)?,
                            saved_slots: saved,
                        });
                        Instr::Yield {
                            input,
                            generator,
                            delegate,
                        }
                    }
                    Terminator::GeneratorStart { body } => {
                        let start = GeneratorStart {
                            body_pc: adjacent(body)?,
                            saved_slots: saved,
                        };
                        if lowered.generator_start.replace(start).is_some() {
                            return Err(invalid("multiple generator starts"));
                        }
                        Instr::GeneratorStart
                    }
                };
                record(code.len(), block.end_origin);
                code.push(instr);
            }
            let end = index(code.len())?;
            for handler in block.protected_by {
                let range = ranges[handler].get_or_insert((pos.start, end, false));
                range.1 = end;
                range.2 |= can_throw;
            }
        }
        for (handler, range) in handlers.into_iter().zip(ranges) {
            if let Some((start_pc, end_pc, true)) = range {
                lowered.handlers.push(ExceptionHandler {
                    start_pc,
                    end_pc,
                    target_pc: pc(handler.entry.target)?,
                    dst: logical(handler.entry.dst),
                });
            }
        }
        Ok((lowered, stack_sites))
    }
}

impl Condition {
    fn encode(self, target: u32) -> Instr {
        match self {
            Self::Truthy { cond, when: false } => Instr::BranchFalse { cond, target },
            Self::Truthy { cond, when: true } => Instr::BranchTrue { cond, target },
            Self::Compare {
                lhs,
                op,
                rhs,
                when: false,
            } => Instr::BranchCompareFalse {
                lhs,
                op,
                rhs,
                target,
            },
            Self::Compare {
                lhs,
                op,
                rhs,
                when: true,
            } => Instr::BranchCompareTrue {
                lhs,
                op,
                rhs,
                target,
            },
            Self::CompareConst {
                lhs,
                op,
                constant,
                when: false,
            } => Instr::BranchCompareConstFalse {
                lhs,
                op,
                constant,
                target,
            },
            Self::CompareConst {
                lhs,
                op,
                constant,
                when: true,
            } => Instr::BranchCompareConstTrue {
                lhs,
                op,
                constant,
                target,
            },
            Self::Binary { lhs, op, rhs } => Instr::BranchBinary {
                lhs,
                op,
                rhs,
                target,
            },
            Self::BinaryConst { lhs, op, constant } => Instr::BranchBinaryConst {
                lhs,
                op,
                constant,
                target,
            },
            Self::Unary { op, src } => Instr::BranchUnary { op, src, target },
        }
    }
}
