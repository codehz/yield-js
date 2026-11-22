//! Backward liveness on logical-register blocks, before bytecode layout.
//!
//! Definitions commit only on the normal path. An exceptional or resumption
//! edge instead kills just its own injected slot, before joining the other
//! edges. Each instruction's operands are read before any writes commit.

use std::collections::BTreeSet;

use yield_vm::bytecode::Instr;

use crate::cfg::{Block, Edge, FunctionCfg, Handler, Terminator};
use crate::emit::Reg;
use crate::instr::{may_throw, uses_defs};

pub(crate) type LiveSet = BTreeSet<Reg>;

pub(crate) struct Liveness {
    live_in: Vec<LiveSet>,
}

pub(crate) fn eliminate_dead_writes(function: &mut FunctionCfg) -> Liveness {
    let predecessors = predecessors(function);
    let mut live = Liveness::solve(function, &predecessors, true);
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
        live = Liveness::solve(function, &predecessors, false);
    }
    live
}

fn predecessors(function: &FunctionCfg) -> Vec<Vec<usize>> {
    let mut predecessors = vec![Vec::new(); function.blocks.len()];
    for (id, block) in function.blocks.iter().enumerate() {
        let Some(block) = block else { continue };
        let can_throw = block.end.may_throw()
            || (!block.protected_by.is_empty()
                && block.statements.iter().any(|stmt| may_throw(&stmt.instr)));
        for edge in block
            .end
            .successors()
            .chain(block.exception_edges(&function.handlers, can_throw))
        {
            let incoming = &mut predecessors[edge.target.0];
            if incoming.last() != Some(&id) {
                incoming.push(id);
            }
        }
    }
    predecessors
}

impl Liveness {
    fn solve(function: &FunctionCfg, predecessors: &[Vec<usize>], skip_dead: bool) -> Self {
        let mut live = Self {
            live_in: vec![LiveSet::new(); function.blocks.len()],
        };
        // Prefer reverse layout order, but only revisit dependent blocks.
        let mut pending: BTreeSet<_> = function
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(id, block)| block.as_ref().map(|_| id))
            .collect();
        while let Some(id) = pending.pop_last() {
            let block = function.blocks[id].as_ref().unwrap();
            let mut before = live.before_end(block, &function.handlers);
            for stmt in block.statements.iter().rev() {
                if !skip_dead || !removable_write(&stmt.instr, &before) {
                    live.before_statement(block, &function.handlers, &stmt.instr, &mut before);
                }
            }
            if before != live.live_in[id] {
                live.live_in[id] = before;
                pending.extend(predecessors[id].iter().copied());
            }
        }
        live
    }

    fn join_edge(&self, set: &mut LiveSet, edge: Edge) {
        set.extend(
            self.live_in[edge.target.0]
                .iter()
                .copied()
                .filter(|&reg| Some(reg) != edge.injected),
        );
    }

    /// Save the union after subtracting each edge's own injection.
    pub(crate) fn saved_on_entry(&self, end: &Terminator) -> Vec<Reg> {
        let mut saved = LiveSet::new();
        for edge in end.successors() {
            self.join_edge(&mut saved, edge);
        }
        saved.into_iter().collect()
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
    ) -> LiveSet {
        let (uses, defs) = uses_defs(instr);
        for def in &defs {
            set.remove(def);
        }
        for edge in block.exception_edges(handlers, may_throw(instr)) {
            self.join_edge(set, edge);
        }
        set.extend(uses);
        defs
    }

    /// Visit live-ins and writes in an already positioned block, without
    /// retaining a copy of each live set.
    pub(crate) fn visit_points(
        &self,
        block: &Block,
        handlers: &[Handler],
        start: usize,
        encode_end: bool,
        mut touch: impl FnMut(Reg, usize),
    ) {
        let mut before = self.before_end(block, handlers);
        if encode_end {
            let pc = start + block.statements.len();
            for &reg in &before {
                touch(reg, pc);
            }
            // Await encodes a destination here; the other terminator writes
            // occur only at their respective entry PCs.
            if let Terminator::Await { fulfill, .. } = block.end {
                touch(fulfill.dst, pc);
            }
        }
        for (index, stmt) in block.statements.iter().enumerate().rev() {
            let defs = self.before_statement(block, handlers, &stmt.instr, &mut before);
            for reg in before.iter().copied().chain(defs) {
                touch(reg, start + index);
            }
        }
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
