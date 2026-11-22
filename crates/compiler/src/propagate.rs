//! Forward must-facts over logical-register basic blocks.
//!
//! A missing block input means unreachable; a missing register fact means
//! unknown. Incoming facts intersect, including on back edges. A conditional
//! can initially select one edge and later open the other as facts are lost.
//! Rewrite only after that fixed point, using the original instructions for
//! transfer so changes cannot affect the proof for subsequent instructions.

use std::collections::{BTreeMap, BTreeSet};

use yield_vm::Value;
use yield_vm::bytecode::{Constant, Instr};

use crate::cfg::{BlockId, Condition, ENTRY, Edge, FunctionCfg, Terminator};
use crate::emit::Reg;
use crate::instr::{map_slots, may_throw, uses_defs};
use crate::ir::logical;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Literal {
    Undefined,
    Pool(u32),
}

impl Literal {
    fn same(self, other: Self, pool: &[Constant]) -> bool {
        match (self, other) {
            (Self::Pool(a), Self::Pool(b)) if a != b => {
                match (&pool[a as usize], &pool[b as usize]) {
                    // Neither NaN comparison nor numeric equality is suitable
                    // here: +0 and -0 must stay distinct at a join.
                    (Constant::Number(a), Constant::Number(b)) => a.to_bits() == b.to_bits(),
                    (a, b) => a == b,
                }
            }
            _ => self == other,
        }
    }

    fn truthy(self, pool: &[Constant]) -> bool {
        let Self::Pool(index) = self else {
            return false;
        };
        match &pool[index as usize] {
            Constant::Null => false,
            Constant::Bool(value) => *value,
            Constant::Number(value) => *value != 0.0 && !value.is_nan(),
            Constant::String(value) => !value.is_empty(),
            Constant::BigInt(value) => Value::bigint(value.clone()).truthy(),
        }
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

    fn transfer(&mut self, instruction: &Instr, defs: &BTreeSet<Reg>) {
        if matches!(instruction, Instr::Move { dst, src } if dst == src) {
            return;
        }
        let literal = match *instruction {
            Instr::Const { dst, constant } => Some((dst.index, Literal::Pool(constant))),
            Instr::Undefined { dst } => Some((dst.index, Literal::Undefined)),
            Instr::Move { dst, src } => self.literals.get(&src.index).map(|&v| (dst.index, v)),
            _ => None,
        };
        for &dst in defs {
            self.kill(dst);
        }
        if let Some((dst, value)) = literal {
            self.literals.insert(dst, value);
        }
        if let Instr::Move { dst, src } = *instruction {
            // Track the direct equality, not its current canonical source.
            // Losing unrelated facts at a join cannot then change this fact.
            self.copies.insert(dst.index, src.index);
        }
    }

    fn folded_branch(&self, end: &Terminator, pool: &[Constant]) -> Option<Terminator> {
        let Terminator::Branch {
            test: Condition::Truthy { cond, when },
            taken,
            next,
        } = *end
        else {
            return None;
        };
        let value = self.literals.get(&cond.index)?;
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
    ) -> Option<Instr> {
        if let Instr::Move { dst, src } = *instruction {
            match self.literals.get(&src.index) {
                Some(Literal::Pool(constant)) => {
                    return Some(Instr::Const {
                        dst,
                        constant: *constant,
                    });
                }
                Some(Literal::Undefined) => return Some(Instr::Undefined { dst }),
                None => {}
            }
        }
        let forward = |reg| self.forward(reg, |reg| defs.contains(&reg));
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

pub(crate) fn optimize(function: &mut FunctionCfg, pool: &[Constant]) {
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
                facts.transfer(&stmt.instr, &uses_defs(&stmt.instr).1);
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
            let rewritten = facts.rewrite(&stmt.instr, &uses, &defs);
            facts.transfer(&stmt.instr, &defs);
            if let Some(rewritten) = rewritten {
                stmt.instr = rewritten;
            }
        }
        if let Some(end) = facts.folded_branch(&block.end, pool) {
            block.end = end;
        } else {
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
            block.statements.pop();
            block.end = end;
        }
    }
}
