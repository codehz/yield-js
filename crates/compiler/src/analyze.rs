//! Backward liveness on logical-register blocks, before bytecode layout.
//!
//! Definitions commit only on the normal path. An exceptional or resumption
//! edge instead kills just its own injected slot, before joining the other
//! edges. Each instruction's operands are read before any writes commit.

use std::collections::BTreeSet;

use yield_vm::bytecode::Instr;

use crate::cfg::{Block, Edge, FunctionCfg, Handler};
use crate::emit::Reg;
use crate::instr::{may_throw, uses_defs};

pub(crate) type LiveSet = BTreeSet<Reg>;

pub(crate) struct Liveness {
    live_in: Vec<LiveSet>,
}

pub(crate) fn eliminate_dead_writes(function: &mut FunctionCfg) -> Liveness {
    let mut live = Liveness::solve(function, true);
    let mut changed = false;
    for block in function.blocks.iter_mut().flatten() {
        let mut after = live.before_end(block, &function.handlers);
        let mut dead = Vec::new();
        for (index, stmt) in block.statements.iter().enumerate().rev() {
            if removable_write(&stmt.instr, &after) {
                dead.push(index);
            } else {
                live.before_statement(block, &function.handlers, &stmt.instr, &mut after);
            }
        }
        if !dead.is_empty() {
            changed = true;
            let mut index = 0;
            block.statements.retain(|_| {
                let remove = dead.last() == Some(&index);
                index += 1;
                if remove {
                    dead.pop();
                }
                !remove
            });
        }
    }
    if changed {
        live = Liveness::solve(function, false);
    }
    live
}

impl Liveness {
    fn solve(function: &FunctionCfg, skip_dead: bool) -> Self {
        let mut live = Self {
            live_in: vec![LiveSet::new(); function.blocks.len()],
        };
        loop {
            let mut changed = false;
            for (id, block) in function.blocks.iter().enumerate().rev() {
                let Some(block) = block else { continue };
                let mut before = live.before_end(block, &function.handlers);
                for stmt in block.statements.iter().rev() {
                    if !skip_dead || !removable_write(&stmt.instr, &before) {
                        live.before_statement(block, &function.handlers, &stmt.instr, &mut before);
                    }
                }
                if before != live.live_in[id] {
                    live.live_in[id] = before;
                    changed = true;
                }
            }
            if !changed {
                return live;
            }
        }
    }

    fn join_edge(&self, set: &mut LiveSet, edge: Edge) {
        set.extend(
            self.live_in[edge.target.0]
                .iter()
                .copied()
                .filter(|&reg| Some(reg) != edge.injected),
        );
    }

    fn before_end(&self, block: &Block, handlers: &[Handler]) -> LiveSet {
        let mut before = block.end.uses();
        for edge in block
            .end
            .successors()
            .chain(block.exception_edges(handlers, block.end.may_throw()))
        {
            self.join_edge(&mut before, edge);
        }
        before
    }

    fn before_statement(
        &self,
        block: &Block,
        handlers: &[Handler],
        instr: &Instr,
        set: &mut LiveSet,
    ) {
        let (uses, defs) = uses_defs(instr);
        for def in defs {
            set.remove(&def);
        }
        for edge in block.exception_edges(handlers, may_throw(instr)) {
            self.join_edge(set, edge);
        }
        set.extend(uses);
    }

    /// Append the live-in of each emitted instruction, in layout order. A
    /// synthetic continuation jump uses its target live-in; `Next` has no
    /// operands or writes, so its transfer already gives exactly that set.
    pub(crate) fn append_points(
        &self,
        block: &Block,
        handlers: &[Handler],
        encode_end: bool,
        out: &mut Vec<LiveSet>,
    ) {
        let start = out.len();
        let mut before = self.before_end(block, handlers);
        if encode_end {
            out.push(before.clone());
        }
        for stmt in block.statements.iter().rev() {
            self.before_statement(block, handlers, &stmt.instr, &mut before);
            out.push(before.clone());
        }
        out[start..].reverse();
    }
}

/// No script, heap mutation or control flow. Moves read initialized slots.
fn removable_write(instruction: &Instr, after: &LiveSet) -> bool {
    match instruction {
        Instr::Move { dst, src } => dst == src || !after.contains(&dst.index),
        Instr::Const { dst, .. } | Instr::Undefined { dst } => !after.contains(&dst.index),
        _ => false,
    }
}
