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

use crate::analyze::{self, Edge, Suspensions};
use crate::emit::Reg;
use crate::error::CompileError;
use crate::instr::{map_slots, may_throw, uses_defs};
use crate::ir::{
    LoweredFunction, compact_dead_instructions, generator_stubs, logical, resume_stubs,
};

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
        if analyze::is_suspend(instruction)
            || matches!(instruction, Instr::Move { dst, src } if dst == src)
        {
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

    fn branch_target(&self, pc: usize, instruction: &Instr, pool: &[Constant]) -> Option<usize> {
        let Instr::BranchFalse { cond, target } = *instruction else {
            return None;
        };
        let value = self.literals.get(&cond.index)?;
        Some(if value.truthy(pool) {
            pc + 1
        } else {
            target as usize
        })
    }

    fn rewrite(
        &self,
        pc: usize,
        instruction: &Instr,
        uses: &BTreeSet<Reg>,
        defs: &BTreeSet<Reg>,
        pool: &[Constant],
    ) -> Option<Instr> {
        if let Some(target) = self.branch_target(pc, instruction, pool) {
            return Some(Instr::Jump {
                target: target as u32,
            });
        }
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
        let forward = |reg| {
            if defs.contains(&reg) {
                return reg;
            }
            let mut reg = reg;
            while let Some(&src) = self.copies.get(&reg) {
                if defs.contains(&src) {
                    break;
                }
                reg = src;
            }
            reg
        };
        uses.iter()
            .any(|&reg| forward(reg) != reg)
            .then(|| map_slots(instruction, &|slot| logical(forward(slot.index))))
    }
}

struct Block {
    start: usize,
    end: usize,
    edges: Vec<Edge>,
    input: Option<Facts>,
}

fn merge(
    blocks: &mut [Block],
    pending: &mut BTreeSet<usize>,
    target: usize,
    facts: &Facts,
    written: Option<Reg>,
    pool: &[Constant],
) {
    let changed = match &mut blocks[target].input {
        Some(input) => input.meet(facts, written, pool),
        input @ None => {
            let mut first = facts.clone();
            if let Some(reg) = written {
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

pub(crate) fn optimize(
    function: &mut LoweredFunction,
    pool: &[Constant],
) -> Result<(), CompileError> {
    let code = &function.code;
    let awaits = resume_stubs(function);
    let generators = generator_stubs(function);
    let suspensions = Suspensions {
        awaits: &awaits,
        generators: &generators,
    };
    let mut starts = BTreeSet::from([0, code.len()]);
    if let Some(start) = function.generator_start {
        starts.insert(start.body_pc - 1);
    }
    for (pc, instruction) in code.iter().enumerate() {
        if analyze::ends_block(instruction) {
            starts.insert(pc + 1);
            starts.extend(
                analyze::edges(code, suspensions, pc)?
                    .into_iter()
                    .map(|edge| edge.pc),
            );
        }
    }
    starts.extend(
        function
            .handlers
            .iter()
            .map(|handler| handler.target_pc as usize),
    );
    let mut blocks = Vec::with_capacity(starts.len() - 1);
    let mut block_at = vec![0; code.len()];
    let mut starts = starts.into_iter();
    let mut start = starts.next().unwrap();
    for end in starts {
        block_at[start..end].fill(blocks.len());
        blocks.push(Block {
            start,
            end,
            edges: analyze::edges(code, suspensions, end - 1)?,
            input: None,
        });
        start = end;
    }

    let empty = Facts::default();
    let mut pending = BTreeSet::new();
    merge(&mut blocks, &mut pending, 0, &empty, None, pool);
    loop {
        while let Some(block) = pending.pop_first() {
            let mut facts = blocks[block].input.as_ref().unwrap().clone();
            let (start, end) = (blocks[block].start, blocks[block].end);
            for (offset, instruction) in code[start..end].iter().enumerate() {
                let pc = start + offset;
                if may_throw(instruction) {
                    // The VM has not committed the destination write on this
                    // edge. Kill only the exception's injected destination.
                    for handler in analyze::covering_handlers(&function.handlers, pc) {
                        merge(
                            &mut blocks,
                            &mut pending,
                            block_at[handler.target_pc as usize],
                            &facts,
                            Some(handler.dst.index),
                            pool,
                        );
                    }
                }
                facts.transfer(instruction, &uses_defs(instruction).1);
            }
            let branch = facts.branch_target(end - 1, &code[end - 1], pool);
            for i in 0..blocks[block].edges.len() {
                let Edge { pc, written } = blocks[block].edges[i];
                if branch.is_none_or(|target| target == pc) {
                    merge(
                        &mut blocks,
                        &mut pending,
                        block_at[pc],
                        &facts,
                        written,
                        pool,
                    );
                }
            }
        }
        // The verifier requires a generator boundary even if parameter
        // initialization always throws. Retain its structurally required body
        // graph, without inheriting any facts from the real entry path.
        if let Some(start) = function.generator_start {
            let boundary = block_at[start.body_pc - 1];
            if blocks[boundary].input.is_none() {
                merge(&mut blocks, &mut pending, boundary, &empty, None, pool);
                continue;
            }
        }
        break;
    }

    let mut dead = BTreeSet::new();
    for Block {
        start, end, input, ..
    } in blocks
    {
        if let Some(mut facts) = input {
            for pc in start..end {
                let instruction = &mut function.code[pc];
                let (uses, defs) = uses_defs(instruction);
                let rewritten = facts.rewrite(pc, instruction, &uses, &defs, pool);
                facts.transfer(instruction, &defs);
                if let Some(rewritten) = rewritten {
                    if matches!(instruction, Instr::BranchFalse { .. })
                        && matches!(rewritten, Instr::Jump { target } if target as usize == pc + 1)
                    {
                        dead.insert(pc);
                    }
                    *instruction = rewritten;
                }
            }
        } else {
            dead.extend(start..end);
        }
    }
    fuse_branch_tests(function, &mut dead);
    function.handlers.retain(|handler| {
        (handler.start_pc as usize..handler.end_pc as usize)
            .any(|pc| !dead.contains(&pc) && may_throw(&function.code[pc]))
    });
    if !dead.is_empty() {
        preserve_yield_stub_predecessors(function, &mut dead);
        compact_dead_instructions(function, &mut dead)?;
    }
    Ok(())
}

/// Fuses `Binary`/`BinaryConst`/`Unary` followed by `BranchFalse` when the
/// result register has exactly one use. The branch instruction consumes the
/// operands and jumps directly, so no result slot is written. A branch that is
/// itself a control-flow target cannot be removed: some paths would skip the
/// computation that must precede the new branch.
fn fuse_branch_tests(function: &mut LoweredFunction, dead: &mut BTreeSet<usize>) {
    let mut targets = BTreeSet::new();
    for instruction in &function.code {
        let target = match instruction {
            Instr::Jump { target }
            | Instr::BranchFalse { target, .. }
            | Instr::BranchTrue { target, .. }
            | Instr::BranchCompareFalse { target, .. }
            | Instr::BranchCompareConstFalse { target, .. }
            | Instr::BranchCompareTrue { target, .. }
            | Instr::BranchCompareConstTrue { target, .. }
            | Instr::BranchBinary { target, .. }
            | Instr::BranchBinaryConst { target, .. }
            | Instr::BranchUnary { target, .. } => Some(*target as usize),
            _ => None,
        };
        if let Some(target) = target {
            targets.insert(target);
        }
    }
    targets.extend(
        function
            .handlers
            .iter()
            .map(|handler| handler.target_pc as usize),
    );
    for site in &function.awaits {
        targets.extend([site.fulfill_pc, site.reject_pc]);
    }
    for site in &function.yields {
        targets.extend([site.next_pc, site.throw_pc, site.return_pc]);
    }
    if let Some(start) = function.generator_start {
        targets.insert(start.body_pc);
    }

    let mut uses: BTreeMap<Reg, usize> = BTreeMap::new();
    for (pc, instruction) in function.code.iter().enumerate() {
        if dead.contains(&pc) {
            continue;
        }
        for reg in uses_defs(instruction).0 {
            *uses.entry(reg).or_default() += 1;
        }
    }

    let mut pc = 0;
    while pc + 1 < function.code.len() {
        if dead.contains(&pc) || dead.contains(&(pc + 1)) || targets.contains(&(pc + 1)) {
            pc += 1;
            continue;
        }
        let Instr::BranchFalse { cond, target } = function.code[pc + 1] else {
            pc += 1;
            continue;
        };
        let replacement = match function.code[pc] {
            Instr::Binary { dst, op, lhs, rhs }
                if dst == cond && uses.get(&dst.index) == Some(&1) =>
            {
                Instr::BranchBinary {
                    op,
                    lhs,
                    rhs,
                    target,
                }
            }
            Instr::BinaryConst {
                dst,
                op,
                lhs,
                constant,
            } if dst == cond && uses.get(&dst.index) == Some(&1) => Instr::BranchBinaryConst {
                op,
                lhs,
                constant,
                target,
            },
            Instr::Unary { dst, op, src } if dst == cond && uses.get(&dst.index) == Some(&1) => {
                Instr::BranchUnary { op, src, target }
            }
            _ => {
                pc += 1;
                continue;
            }
        };
        function.code[pc] = replacement;
        dead.insert(pc + 1);
        pc += 2;
    }
}

/// The verifier checks even lexical predecessors that are unreachable. If
/// deleting a separator exposes a fall-through opcode (the VM's verifier even
/// counts Yield as one), retain a detached jump in the removed gap.
fn preserve_yield_stub_predecessors(function: &mut LoweredFunction, dead: &mut BTreeSet<usize>) {
    let mut stubs = BTreeSet::new();
    for (pc, instruction) in function.code.iter().enumerate() {
        if !dead.contains(&pc)
            && let Instr::Yield { generator, .. } = instruction
        {
            let site = function.yields[*generator as usize];
            stubs.extend([site.throw_pc, site.return_pc]);
        }
    }
    for pc in stubs {
        if pc == 0 || !dead.contains(&(pc - 1)) {
            continue;
        }
        if let Some(previous) = (0..pc).rev().find(|previous| !dead.contains(previous))
            && !matches!(
                function.code[previous],
                Instr::Jump { .. }
                    | Instr::Return { .. }
                    | Instr::Throw { .. }
                    | Instr::GeneratorStart
            )
        {
            dead.remove(&(pc - 1));
            function.code[pc - 1] = Instr::Jump {
                target: (pc - 1) as u32,
            };
        }
    }
}
