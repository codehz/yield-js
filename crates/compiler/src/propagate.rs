//! Forward must-facts over logical-register basic blocks.
//!
//! A missing block input means unreachable; a missing register fact means
//! unknown. Incoming facts intersect, including on back edges. A conditional
//! can initially select one edge and later open the other as facts are lost.
//! Rewrite only after that fixed point, using the original instructions for
//! transfer so changes cannot affect the proof for subsequent instructions.

use std::collections::{BTreeMap, BTreeSet};

use yield_vm::Value;
use yield_vm::bytecode::{BinaryOp, Constant, Instr, Slot, UnaryOp};

use crate::cfg::{BlockId, Condition, ENTRY, Edge, FunctionCfg, Terminator};
use crate::emit::Reg;
use crate::instr::{map_slots, may_throw, uses_defs};
use crate::ir::logical;
use crate::session::Session;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Literal {
    Undefined,
    Pool(u32),
    Bool(bool),
    // Derived facts do not enter the pool until the fixed point is known.
    Number(u64),
}

impl Literal {
    fn from_pool(index: u32, pool: &[Constant]) -> Self {
        match pool[index as usize] {
            Constant::Bool(value) => Self::Bool(value),
            Constant::Number(value) => Self::Number(value.to_bits()),
            _ => Self::Pool(index),
        }
    }

    fn number(value: f64) -> Self {
        // NaN payloads are not observable; keep emitted constants deterministic.
        let value = if value.is_nan() { f64::NAN } else { value };
        Self::Number(value.to_bits())
    }

    fn pool_index(self, session: &mut Session) -> Option<u32> {
        Some(match self {
            Self::Undefined => return None,
            Self::Pool(index) => index,
            Self::Bool(value) => session.intern(Constant::Bool(value)),
            Self::Number(bits) => session.intern(Constant::Number(f64::from_bits(bits))),
        })
    }

    fn same(self, other: Self, pool: &[Constant]) -> bool {
        match (self, other) {
            (Self::Pool(a), Self::Pool(b)) => a == b || pool[a as usize] == pool[b as usize],
            _ => self == other,
        }
    }

    fn truthy(self, pool: &[Constant]) -> bool {
        match self {
            Self::Undefined => false,
            Self::Bool(value) => value,
            Self::Number(bits) => {
                let value = f64::from_bits(bits);
                value != 0.0 && !value.is_nan()
            }
            Self::Pool(index) => match &pool[index as usize] {
                Constant::Null => false,
                Constant::String(value) => !value.is_empty(),
                Constant::BigInt(value) => Value::bigint(value.clone()).truthy(),
                _ => unreachable!("numeric and boolean facts are inline"),
            },
        }
    }

    fn binary(self, op: BinaryOp, rhs: Self, pool: &[Constant]) -> Option<Self> {
        if matches!(op, BinaryOp::StrictEq | BinaryOp::StrictNe) {
            let equal = match (self, rhs) {
                (Self::Number(a), Self::Number(b)) => f64::from_bits(a) == f64::from_bits(b),
                _ => self.same(rhs, pool),
            };
            return Some(Self::Bool(equal == (op == BinaryOp::StrictEq)));
        }
        let (Self::Number(lhs), Self::Number(rhs)) = (self, rhs) else {
            return None;
        };
        let (lhs, rhs) = (f64::from_bits(lhs), f64::from_bits(rhs));
        Some(match op {
            BinaryOp::Add => Self::number(lhs + rhs),
            BinaryOp::Sub => Self::number(lhs - rhs),
            BinaryOp::Mul => Self::number(lhs * rhs),
            BinaryOp::Div => Self::number(lhs / rhs),
            BinaryOp::Rem => Self::number(lhs % rhs),
            BinaryOp::Lt => Self::Bool(lhs < rhs),
            BinaryOp::Le => Self::Bool(lhs <= rhs),
            BinaryOp::Gt => Self::Bool(lhs > rhs),
            BinaryOp::Ge => Self::Bool(lhs >= rhs),
            BinaryOp::Eq => Self::Bool(lhs == rhs),
            BinaryOp::Ne => Self::Bool(lhs != rhs),
            _ => return None,
        })
    }

    fn unary(self, op: UnaryOp, pool: &[Constant]) -> Option<Self> {
        if op == UnaryOp::Not {
            return Some(Self::Bool(!self.truthy(pool)));
        }
        let Self::Number(bits) = self else {
            return None;
        };
        let number = f64::from_bits(bits);
        Some(match op {
            UnaryOp::Neg => Self::number(-number),
            UnaryOp::Pos | UnaryOp::ToNumeric => self,
            UnaryOp::Increment => Self::number(number + 1.0),
            UnaryOp::Decrement => Self::number(number - 1.0),
            _ => return None,
        })
    }
}

#[derive(Clone, Default)]
struct Facts {
    literals: BTreeMap<Reg, Literal>,
    copies: BTreeMap<Reg, Reg>,
}

impl Facts {
    fn kill(&mut self, reg: Reg) {
        self.literals.remove(&reg);
        self.copies.retain(|&dst, src| dst != reg && *src != reg);
    }

    fn meet(&mut self, from: &Self, written: Option<Reg>, pool: &[Constant]) -> bool {
        let before = self.literals.len() + self.copies.len();
        self.literals.retain(|reg, literal| {
            written != Some(*reg)
                && from
                    .literals
                    .get(reg)
                    .is_some_and(|b| literal.same(*b, pool))
        });
        self.copies.retain(|reg, src| {
            written != Some(*reg) && written != Some(*src) && from.copies.get(reg) == Some(&*src)
        });
        before != self.literals.len() + self.copies.len()
    }

    fn literal(&self, slot: Slot) -> Option<Literal> {
        self.literals.get(&slot.index).copied()
    }

    fn result(&self, instruction: &Instr, pool: &[Constant]) -> Option<(Slot, Literal)> {
        Some(match *instruction {
            Instr::Const { dst, constant } => (dst, Literal::from_pool(constant, pool)),
            Instr::Undefined { dst } => (dst, Literal::Undefined),
            Instr::Move { dst, src } => (dst, self.literal(src)?),
            Instr::Binary { dst, lhs, op, rhs } => (
                dst,
                self.literal(lhs)?.binary(op, self.literal(rhs)?, pool)?,
            ),
            Instr::BinaryConst {
                dst,
                lhs,
                op,
                constant,
            } => (
                dst,
                self.literal(lhs)?
                    .binary(op, Literal::from_pool(constant, pool), pool)?,
            ),
            Instr::Unary { dst, op, src } => (dst, self.literal(src)?.unary(op, pool)?),
            _ => return None,
        })
    }

    fn transfer(
        &mut self,
        instruction: &Instr,
        defs: &BTreeSet<Reg>,
        result: Option<(Slot, Literal)>,
    ) {
        if matches!(instruction, Instr::Move { dst, src } if dst == src) {
            return;
        }
        for &dst in defs {
            self.kill(dst);
        }
        if let Some((dst, value)) = result {
            self.literals.insert(dst.index, value);
        }
        if let Instr::Move { dst, src } = *instruction {
            // Track the direct equality, not its current canonical source.
            // Losing unrelated facts at a join cannot then change this fact.
            self.copies.insert(dst.index, src.index);
        }
    }

    fn folded_branch(&self, end: &Terminator, pool: &[Constant]) -> Option<Terminator> {
        let Terminator::Branch { test, taken, next } = *end else {
            return None;
        };
        let (value, when) = match test {
            Condition::Truthy { cond, when } => (self.literal(cond)?, when),
            Condition::Compare { lhs, op, rhs, when } => (
                self.literal(lhs)?.binary(op, self.literal(rhs)?, pool)?,
                when,
            ),
            Condition::CompareConst {
                lhs,
                op,
                constant,
                when,
            } => (
                self.literal(lhs)?
                    .binary(op, Literal::from_pool(constant, pool), pool)?,
                when,
            ),
            _ => return None,
        };
        Some(if value.truthy(pool) == when {
            Terminator::Jump(taken)
        } else {
            Terminator::Next(next)
        })
    }

    fn forward(&self, mut reg: Reg, blocked: impl Fn(Reg) -> bool) -> Reg {
        if !blocked(reg) {
            while let Some(&src) = self.copies.get(&reg) {
                if blocked(src) {
                    break;
                }
                reg = src;
            }
        }
        reg
    }

    fn rewrite(
        &self,
        instruction: &Instr,
        uses: &BTreeSet<Reg>,
        defs: &BTreeSet<Reg>,
        result: Option<(Slot, Literal)>,
        session: &mut Session,
    ) -> Option<Instr> {
        if !matches!(instruction, Instr::Const { .. } | Instr::Undefined { .. })
            && let Some((dst, literal)) = result
        {
            return Some(match literal.pool_index(session) {
                Some(constant) => Instr::Const { dst, constant },
                None => Instr::Undefined { dst },
            });
        }
        let forward = |reg| self.forward(reg, |reg| defs.contains(&reg));
        if let Instr::Binary { dst, lhs, op, rhs } = *instruction
            && let Some(constant) = self.literal(rhs).and_then(|v| v.pool_index(session))
        {
            // Replace only the RHS read, not the operation or its conversions.
            return Some(Instr::BinaryConst {
                dst,
                lhs: logical(forward(lhs.index)),
                op,
                constant,
            });
        }
        uses.iter()
            .any(|&reg| forward(reg) != reg)
            .then(|| map_slots(instruction, &|slot| logical(forward(slot.index))))
    }
}

fn merge(
    inputs: &mut [Option<Facts>],
    pending: &mut BTreeSet<BlockId>,
    Edge { target, injected }: Edge,
    facts: &Facts,
    pool: &[Constant],
) {
    let changed = match &mut inputs[target.0] {
        Some(input) => input.meet(facts, injected, pool),
        input @ None => {
            let mut first = facts.clone();
            if let Some(reg) = injected {
                first.kill(reg);
            }
            *input = Some(first);
            true
        }
    };
    if changed {
        pending.insert(target);
    }
}

pub(crate) fn optimize(function: &mut FunctionCfg, session: &mut Session) {
    let pool = session.constants();
    let mut inputs = vec![None; function.blocks.len()];
    let empty = Facts::default();
    let mut pending = BTreeSet::new();
    merge(&mut inputs, &mut pending, ENTRY.into(), &empty, pool);
    loop {
        while let Some(id) = pending.pop_first() {
            let block = function.blocks[id.0].as_ref().unwrap();
            let mut facts = inputs[id.0].as_ref().unwrap().clone();
            let mut send =
                |edge, facts: &Facts| merge(&mut inputs, &mut pending, edge, facts, pool);
            for stmt in &block.statements {
                // A throw has not committed the instruction's destination.
                for edge in block.exception_edges(&function.handlers, may_throw(&stmt.instr)) {
                    send(edge, &facts);
                }
                let result = facts.result(&stmt.instr, pool);
                facts.transfer(&stmt.instr, &uses_defs(&stmt.instr).1, result);
            }
            for edge in block.exception_edges(&function.handlers, block.end.may_throw()) {
                send(edge, &facts);
            }
            let folded = facts.folded_branch(&block.end, pool);
            for edge in folded.as_ref().unwrap_or(&block.end).successors() {
                send(edge, &facts);
            }
        }
        // Keep the required generator boundary and body even when parameter
        // initialization cannot reach them. No entry-path facts apply there.
        if let Some(boundary) = function.generator_boundary
            && inputs[boundary.0].is_none()
        {
            merge(&mut inputs, &mut pending, boundary.into(), &empty, pool);
            continue;
        }
        break;
    }

    for (block, input) in function.blocks.iter_mut().zip(inputs) {
        let Some(mut facts) = input else {
            *block = None;
            continue;
        };
        let block = block.as_mut().unwrap();
        for stmt in &mut block.statements {
            let (uses, defs) = uses_defs(&stmt.instr);
            let result = facts.result(&stmt.instr, session.constants());
            let rewritten = facts.rewrite(&stmt.instr, &uses, &defs, result, session);
            facts.transfer(&stmt.instr, &defs, result);
            if let Some(rewritten) = rewritten {
                stmt.instr = rewritten;
            }
        }
        if let Some(end) = facts.folded_branch(&block.end, session.constants()) {
            block.end = end;
        } else {
            if let Terminator::Branch { test, .. } = &mut block.end
                && let Condition::Compare { lhs, op, rhs, when } = *test
                && let Some(constant) = facts.literal(rhs).and_then(|v| v.pool_index(session))
            {
                *test = Condition::CompareConst {
                    lhs,
                    op,
                    constant,
                    when,
                };
            }
            let blocked = match block.end {
                Terminator::Await { fulfill, .. } => Some(fulfill.dst),
                _ => None,
            };
            block
                .end
                .map_uses(|slot| logical(facts.forward(slot.index, |reg| blocked == Some(reg))));
        }
    }
    fuse_branch_tests(function);
}

/// A block tail shares its protection region with its terminator, and nothing
/// can enter between the producer and the test.
fn fuse_branch_tests(function: &mut FunctionCfg) {
    let mut uses = BTreeMap::<Reg, usize>::new();
    for block in function.blocks.iter().flatten() {
        for reg in block
            .statements
            .iter()
            .flat_map(|stmt| uses_defs(&stmt.instr).0)
            .chain(block.end.uses())
        {
            *uses.entry(reg).or_default() += 1;
        }
    }

    for block in function.blocks.iter_mut().flatten() {
        let Terminator::Branch {
            test: Condition::Truthy { cond, when: false },
            taken,
            next,
        } = block.end
        else {
            continue;
        };
        if uses.get(&cond.index) != Some(&1) {
            continue;
        }
        let Some(last) = block.statements.last() else {
            continue;
        };
        let test = match last.instr {
            Instr::Binary { dst, lhs, op, rhs } if dst == cond => {
                Condition::Binary { lhs, op, rhs }
            }
            Instr::BinaryConst {
                dst,
                lhs,
                op,
                constant,
            } if dst == cond => Condition::BinaryConst { lhs, op, constant },
            Instr::Unary { dst, op, src } if dst == cond => Condition::Unary { op, src },
            _ => continue,
        };
        let end = Terminator::Branch { test, taken, next };
        if end.may_throw() == may_throw(&last.instr) {
            block.end_origin = last.origin;
            block.statements.pop();
            block.end = end;
        }
    }
}
