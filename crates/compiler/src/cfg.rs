//! Logical-register basic blocks. Only the lowering and layout boundaries
//! translate control flow between block identities and bytecode PCs.

use std::collections::BTreeSet;

use yield_vm::BindingId;
use yield_vm::bytecode::{BinaryOp, Instr, Slot, UnaryOp};

use crate::emit::Reg;
use crate::error::CompileError;
use crate::instr;
use crate::ir::{FunctionAttrs, LoweredFunction};

mod cleanup;
mod layout;
mod verify;
pub(crate) use layout::LaidOutFunction;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct BlockId(pub(crate) usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct InstrId(usize);

pub(crate) struct Statement {
    pub(crate) id: InstrId,
    pub(crate) instr: Instr,
}

pub(crate) struct Block {
    pub(crate) statements: Vec<Statement>,
    pub(crate) end: Terminator,
    /// Indices into `FunctionCfg::handlers`, in VM priority order.
    pub(crate) protected_by: Vec<usize>,
}

pub(crate) struct FunctionCfg {
    pub(crate) attrs: FunctionAttrs,
    /// Holes are removed blocks; surviving IDs and layout order never change.
    pub(crate) blocks: Vec<Option<Block>>,
    pub(crate) handlers: Vec<Handler>,
    pub(crate) generator_boundary: Option<BlockId>,
    pub(crate) iteration_envs: Vec<IterationEnv>,
}

pub(crate) struct Handler {
    pub(crate) entry: Injection,
}

pub(crate) struct IterationEnv {
    pub(crate) bindings: Vec<BindingId>,
    pub(crate) save: InstrId,
    pub(crate) enters: Vec<InstrId>,
    pub(crate) restore: Option<InstrId>,
}

#[derive(Clone, Copy)]
pub(crate) struct Injection {
    pub(crate) target: BlockId,
    pub(crate) dst: Reg,
}

#[derive(Clone, Copy)]
pub(crate) enum Condition {
    Truthy {
        cond: Slot,
        when: bool,
    },
    Compare {
        lhs: Slot,
        op: BinaryOp,
        rhs: Slot,
        when: bool,
    },
    CompareConst {
        lhs: Slot,
        op: BinaryOp,
        constant: u32,
        when: bool,
    },
    Binary {
        lhs: Slot,
        op: BinaryOp,
        rhs: Slot,
    },
    BinaryConst {
        lhs: Slot,
        op: BinaryOp,
        constant: u32,
    },
    Unary {
        op: UnaryOp,
        src: Slot,
    },
}

impl Condition {
    fn map_slots(&mut self, map: &mut impl FnMut(Slot) -> Slot) {
        match self {
            Self::Truthy { cond, .. } => *cond = map(*cond),
            Self::Compare { lhs, rhs, .. } | Self::Binary { lhs, rhs, .. } => {
                *lhs = map(*lhs);
                *rhs = map(*rhs);
            }
            Self::CompareConst { lhs, .. } | Self::BinaryConst { lhs, .. } => *lhs = map(*lhs),
            Self::Unary { src, .. } => *src = map(*src),
        }
    }
}

pub(crate) enum Terminator {
    Next(BlockId),
    Jump(BlockId),
    Branch {
        test: Condition,
        taken: BlockId,
        next: BlockId,
    },
    /// A return or a throw: no normal successor and no PC operands.
    Finish(Instr),
    Await {
        input: Slot,
        fulfill: Injection,
        reject: Injection,
    },
    Yield {
        input: Slot,
        delegate: bool,
        next: Injection,
        thrown: Injection,
        returned: BlockId,
    },
    GeneratorStart {
        body: BlockId,
    },
}

pub(crate) const ENTRY: BlockId = BlockId(0);

#[derive(Clone, Copy)]
pub(crate) struct Edge {
    pub(crate) target: BlockId,
    pub(crate) injected: Option<Reg>,
}

impl From<BlockId> for Edge {
    fn from(target: BlockId) -> Self {
        Self {
            target,
            injected: None,
        }
    }
}

impl From<Injection> for Edge {
    fn from(entry: Injection) -> Self {
        Self {
            target: entry.target,
            injected: Some(entry.dst),
        }
    }
}

impl Terminator {
    /// Includes externally dispatched entries and their individual writes.
    pub(crate) fn successors(&self) -> impl Iterator<Item = Edge> {
        let edges = match *self {
            Self::Next(target) | Self::Jump(target) | Self::GeneratorStart { body: target } => {
                [Some(target.into()), None, None]
            }
            Self::Branch { taken, next, .. } => [Some(taken.into()), Some(next.into()), None],
            Self::Await {
                fulfill, reject, ..
            } => [Some(fulfill.into()), Some(reject.into()), None],
            Self::Yield {
                next,
                thrown,
                returned,
                ..
            } => [
                Some(next.into()),
                Some(thrown.into()),
                Some(returned.into()),
            ],
            Self::Finish(_) => [None; 3],
        };
        edges.into_iter().flatten()
    }

    pub(crate) fn may_throw(&self) -> bool {
        match self {
            Self::Finish(instr) => instr::may_throw(instr),
            Self::Await { .. } => true, // Promise constructor lookup can throw before suspension.
            Self::Branch { test, .. } => match *test {
                Condition::Compare { op, .. }
                | Condition::CompareConst { op, .. }
                | Condition::Binary { op, .. }
                | Condition::BinaryConst { op, .. } => instr::binary_may_throw(op),
                Condition::Unary { op, .. } => instr::branch_unary_may_throw(op),
                Condition::Truthy { .. } => false,
            },
            _ => false,
        }
    }

    pub(crate) fn uses(&self) -> BTreeSet<Reg> {
        let mut uses = BTreeSet::new();
        match self {
            Self::Branch { test, .. } => {
                let mut test = *test;
                test.map_slots(&mut |slot| {
                    uses.insert(slot.index);
                    slot
                });
            }
            Self::Await { input, .. }
            | Self::Yield { input, .. }
            | Self::Finish(Instr::Return { src: input } | Instr::Throw { src: input }) => {
                uses.insert(input.index);
            }
            _ => {}
        }
        uses
    }

    pub(crate) fn map_uses(&mut self, mut map: impl FnMut(Slot) -> Slot) {
        match self {
            Self::Branch { test, .. } => test.map_slots(&mut map),
            Self::Await { input, .. }
            | Self::Yield { input, .. }
            | Self::Finish(Instr::Return { src: input } | Instr::Throw { src: input }) => {
                *input = map(*input)
            }
            _ => {}
        }
    }
}

impl Block {
    pub(crate) fn exception_edges<'a>(
        &'a self,
        handlers: &'a [Handler],
        may_throw: bool,
    ) -> impl Iterator<Item = Edge> + 'a {
        self.protected_by
            .iter()
            .filter(move |_| may_throw)
            .map(|&id| handlers[id].entry.into())
    }
}

impl FunctionCfg {
    pub(crate) fn instructions(&self) -> impl Iterator<Item = &Instr> {
        self.blocks
            .iter()
            .flatten()
            .flat_map(|block| block.statements.iter().map(|stmt| &stmt.instr))
    }

    pub(crate) fn instructions_mut(&mut self) -> impl Iterator<Item = &mut Instr> {
        self.blocks
            .iter_mut()
            .flatten()
            .flat_map(|block| block.statements.iter_mut().map(|stmt| &mut stmt.instr))
    }
}

fn invalid(message: &str) -> CompileError {
    CompileError::internal(format!("internal CFG error: {message}"))
}

impl FunctionCfg {
    pub(crate) fn from_lowered(function: LoweredFunction) -> Result<Self, CompileError> {
        let LoweredFunction {
            attrs,
            code,
            awaits,
            yields,
            handlers,
            generator_start,
            iteration_envs,
        } = function;
        let len = code.len();
        if len == 0 {
            return Err(invalid("empty function"));
        }
        if attrs.is_generator != generator_start.is_some() {
            return Err(invalid(
                "generator start metadata does not match function kind",
            ));
        }
        let mut starts = BTreeSet::from([0, len]);
        let mut add_target = |pc| {
            if pc >= len {
                return Err(invalid("target out of range"));
            }
            starts.insert(pc);
            Ok(())
        };
        for site in &awaits {
            add_target(site.fulfill_pc)?;
            add_target(site.reject_pc)?;
        }
        for site in &yields {
            add_target(site.next_pc)?;
            add_target(site.throw_pc)?;
            add_target(site.return_pc)?;
            if !matches!(code[site.return_pc], Instr::LoadGeneratorReturn { .. }) {
                return Err(invalid("yield return entry does not load its payload"));
            }
        }
        if let Some(start) = generator_start {
            add_target(start.body_pc)?;
            let pc = start
                .body_pc
                .checked_sub(1)
                .ok_or_else(|| invalid("missing generator boundary"))?;
            if !matches!(code[pc], Instr::GeneratorStart) {
                return Err(invalid("generator start does not precede its body"));
            }
            add_target(pc)?;
        }
        for handler in &handlers {
            add_target(handler.target_pc as usize)?;
            if handler.start_pc >= handler.end_pc || handler.end_pc as usize > len {
                return Err(invalid("invalid protected interval"));
            }
            if (handler.start_pc..handler.end_pc).contains(&handler.target_pc) {
                return Err(invalid("handler target is inside its protected interval"));
            }
        }
        for handler in &handlers {
            starts.extend([handler.start_pc as usize, handler.end_pc as usize]);
        }
        for (pc, instr) in code.iter().enumerate() {
            if let Some((_, target)) = decode_condition(instr) {
                if target as usize >= len || pc + 1 == len {
                    return Err(invalid("invalid conditional successor"));
                }
                starts.insert(target as usize);
            } else if let Instr::Jump { target } = instr {
                if *target as usize >= len {
                    return Err(invalid("jump target out of range"));
                }
                starts.insert(*target as usize);
            } else if !ends_block(instr) {
                continue;
            }
            starts.insert(pc + 1);
        }
        let iteration_envs = iteration_envs
            .into_iter()
            .map(|g| {
                if !matches!(code.get(g.save_pc), Some(Instr::SaveEnv { .. }))
                    || g.enter_pcs
                        .iter()
                        .any(|&pc| !matches!(code.get(pc), Some(Instr::NewIterationEnv { .. })))
                    || g.restore_pc.is_some_and(|pc| {
                        pc < g.save_pc || !matches!(code.get(pc), Some(Instr::RestoreEnv { .. }))
                    })
                {
                    return Err(invalid("invalid iteration environment instruction"));
                }
                Ok(IterationEnv {
                    bindings: g.bindings,
                    save: InstrId(g.save_pc),
                    enters: g.enter_pcs.into_iter().map(InstrId).collect(),
                    restore: g.restore_pc.map(InstrId),
                })
            })
            .collect::<Result<_, CompileError>>()?;
        let starts: Vec<_> = starts.into_iter().collect();
        let block_at = |pc| {
            starts
                .binary_search(&pc)
                .ok()
                .filter(|_| pc < len)
                .map(BlockId)
                .ok_or_else(|| invalid("target is not an executable block"))
        };
        // Sort by original boundaries before any block is deleted or aliased.
        let mut handlers = handlers;
        handlers.sort_by_key(|h| (std::cmp::Reverse(h.start_pc), h.end_pc));
        let entries = handlers
            .iter()
            .map(|h| {
                Ok(Handler {
                    entry: Injection {
                        target: block_at(h.target_pc as usize)?,
                        dst: h.dst.index,
                    },
                })
            })
            .collect::<Result<_, CompileError>>()?;
        let mut blocks = Vec::with_capacity(starts.len() - 1);
        let mut code = code.into_iter();
        for pair in starts.windows(2) {
            let [start, end] = [pair[0], pair[1]];
            let pc = end - 1;
            let statements = (start..pc).map(|pc| Statement {
                id: InstrId(pc),
                instr: code.next().unwrap(),
            });
            let mut statements: Vec<_> = statements.collect();
            let last = code.next().unwrap();
            let end = if let Some((test, target)) = decode_condition(&last) {
                Terminator::Branch {
                    test,
                    taken: block_at(target as usize)?,
                    next: block_at(end)?,
                }
            } else {
                match last {
                    Instr::Jump { target } => Terminator::Jump(block_at(target as usize)?),
                    Instr::Return { .. }
                    | Instr::Throw { .. }
                    | Instr::ThrowAssignmentToConst
                    | Instr::ThrowDeleteSuper => Terminator::Finish(last),
                    Instr::Await { input, dst, resume } => {
                        let site = awaits
                            .get(resume as usize)
                            .ok_or_else(|| invalid("missing await entry"))?;
                        if dst.index != site.result {
                            return Err(invalid(
                                "await destination does not match fulfillment entry",
                            ));
                        }
                        Terminator::Await {
                            input,
                            fulfill: Injection {
                                target: block_at(site.fulfill_pc)?,
                                dst: site.result,
                            },
                            reject: Injection {
                                target: block_at(site.reject_pc)?,
                                dst: site.reject,
                            },
                        }
                    }
                    Instr::Yield {
                        input,
                        generator,
                        delegate,
                    } => {
                        let site = yields
                            .get(generator as usize)
                            .ok_or_else(|| invalid("missing yield entry"))?;
                        if site.next_pc != end {
                            return Err(invalid("yield next entry does not follow its suspension"));
                        }
                        Terminator::Yield {
                            input,
                            delegate,
                            next: Injection {
                                target: block_at(site.next_pc)?,
                                dst: site.next,
                            },
                            thrown: Injection {
                                target: block_at(site.throw_pc)?,
                                dst: site.thrown,
                            },
                            returned: block_at(site.return_pc)?,
                        }
                    }
                    Instr::GeneratorStart => {
                        if generator_start.is_none_or(|s| s.body_pc != end) {
                            return Err(invalid("missing or misplaced generator start entry"));
                        }
                        Terminator::GeneratorStart {
                            body: block_at(end)?,
                        }
                    }
                    instr => {
                        statements.push(Statement {
                            id: InstrId(pc),
                            instr,
                        });
                        Terminator::Next(block_at(end)?)
                    }
                }
            };
            let protected_by = handlers
                .iter()
                .enumerate()
                .filter_map(|(i, h)| {
                    (h.start_pc as usize <= start && start < h.end_pc as usize).then_some(i)
                })
                .collect();
            blocks.push(Some(Block {
                statements,
                end,
                protected_by,
            }));
        }
        let cfg = Self {
            attrs,
            blocks,
            handlers: entries,
            iteration_envs,
            generator_boundary: generator_start
                .map(|s| block_at(s.body_pc - 1))
                .transpose()?,
        };
        cfg.validate()?;
        Ok(cfg)
    }
}

fn ends_block(instr: &Instr) -> bool {
    matches!(
        instr,
        Instr::Jump { .. }
            | Instr::Return { .. }
            | Instr::Throw { .. }
            | Instr::ThrowAssignmentToConst
            | Instr::ThrowDeleteSuper
            | Instr::Await { .. }
            | Instr::Yield { .. }
            | Instr::GeneratorStart
    ) || decode_condition(instr).is_some()
}

fn decode_condition(instr: &Instr) -> Option<(Condition, u32)> {
    Some(match *instr {
        Instr::BranchFalse { cond, target } => (Condition::Truthy { cond, when: false }, target),
        Instr::BranchTrue { cond, target } => (Condition::Truthy { cond, when: true }, target),
        Instr::BranchCompareFalse {
            lhs,
            op,
            rhs,
            target,
        } => (
            Condition::Compare {
                lhs,
                op,
                rhs,
                when: false,
            },
            target,
        ),
        Instr::BranchCompareTrue {
            lhs,
            op,
            rhs,
            target,
        } => (
            Condition::Compare {
                lhs,
                op,
                rhs,
                when: true,
            },
            target,
        ),
        Instr::BranchCompareConstFalse {
            lhs,
            op,
            constant,
            target,
        } => (
            Condition::CompareConst {
                lhs,
                op,
                constant,
                when: false,
            },
            target,
        ),
        Instr::BranchCompareConstTrue {
            lhs,
            op,
            constant,
            target,
        } => (
            Condition::CompareConst {
                lhs,
                op,
                constant,
                when: true,
            },
            target,
        ),
        Instr::BranchBinary {
            lhs,
            op,
            rhs,
            target,
        } => (Condition::Binary { lhs, op, rhs }, target),
        Instr::BranchBinaryConst {
            lhs,
            op,
            constant,
            target,
        } => (Condition::BinaryConst { lhs, op, constant }, target),
        Instr::BranchUnary { op, src, target } => (Condition::Unary { op, src }, target),
        _ => return None,
    })
}
