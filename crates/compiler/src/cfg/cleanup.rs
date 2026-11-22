//! Clean up ordinary jumps without changing the source order or entry roles.

use yield_vm::bytecode::BinaryOp;

use super::{BlockId, Condition, ENTRY, FunctionCfg, Terminator};
use crate::instr::may_throw;

#[derive(Clone, Copy)]
enum Destination {
    Unseen,
    Visiting,
    Resolved(Option<BlockId>),
}

impl FunctionCfg {
    pub(crate) fn simplify_jumps(&mut self) {
        let mut pinned = vec![false; self.blocks.len()];
        pinned[ENTRY.0] = true;
        if let Some(boundary) = self.generator_boundary {
            pinned[boundary.0] = true;
        }
        for handler in &self.handlers {
            pinned[handler.entry.target.0] = true;
        }
        for block in self.blocks.iter().flatten() {
            match block.end {
                Terminator::Await { .. }
                | Terminator::Yield { .. }
                | Terminator::GeneratorStart { .. } => {
                    for edge in block.end.successors() {
                        pinned[edge.target.0] = true;
                    }
                }
                _ => {}
            }
        }

        let mut destinations = vec![Destination::Unseen; self.blocks.len()];
        let mut path = Vec::new();
        for (index, block) in self.blocks.iter().enumerate() {
            if block.is_none() {
                continue;
            }
            let mut id = BlockId(index);
            // Resolve each trampoline once; a chain leading to a cycle stays as-is.
            let target = loop {
                match destinations[id.0] {
                    Destination::Resolved(target) => break target,
                    Destination::Visiting => break None,
                    Destination::Unseen => {}
                }
                let block = self.blocks[id.0].as_ref().unwrap();
                match block.end {
                    Terminator::Jump(next) | Terminator::Next(next)
                        if !pinned[id.0] && block.statements.is_empty() && !pinned[next.0] =>
                    {
                        destinations[id.0] = Destination::Visiting;
                        path.push(id);
                        id = next;
                    }
                    _ => {
                        destinations[id.0] = Destination::Resolved(Some(id));
                        break Some(id);
                    }
                }
            };
            for visited in path.drain(..) {
                destinations[visited.0] = Destination::Resolved(target);
            }
        }

        let mut redirected = false;
        for block in self.blocks.iter_mut().flatten() {
            match &mut block.end {
                // Keep the fallthrough at its source position. Only explicit jumps are threaded.
                Terminator::Jump(target) | Terminator::Branch { taken: target, .. } => {
                    if let Destination::Resolved(Some(resolved)) = destinations[target.0] {
                        redirected |= resolved != *target;
                        *target = resolved;
                    }
                }
                _ => {}
            }
            if let Terminator::Branch { test, taken, next } = block.end
                && (taken == next
                    || matches!(destinations[next.0], Destination::Resolved(Some(id)) if id == taken))
                && can_discard(test)
            {
                redirected |= taken != next;
                block.end = Terminator::Next(next);
            }
        }
        if redirected {
            self.prune_unreachable();
        }
    }

    fn prune_unreachable(&mut self) {
        let mut reached = vec![false; self.blocks.len()];
        let mut pending = Vec::new();
        let mut enqueue = |id: BlockId, pending: &mut Vec<BlockId>| {
            if !std::mem::replace(&mut reached[id.0], true) {
                pending.push(id);
            }
        };
        enqueue(ENTRY, &mut pending);
        if let Some(boundary) = self.generator_boundary {
            enqueue(boundary, &mut pending);
        }
        while let Some(id) = pending.pop() {
            let block = self.blocks[id.0].as_ref().unwrap();
            let throws = !block.protected_by.is_empty()
                && (block.end.may_throw()
                    || block.statements.iter().any(|stmt| may_throw(&stmt.instr)));
            for edge in block
                .end
                .successors()
                .chain(block.exception_edges(&self.handlers, throws))
            {
                enqueue(edge.target, &mut pending);
            }
        }
        for (block, reached) in self.blocks.iter_mut().zip(reached) {
            if !reached {
                *block = None;
            }
        }
    }
}

fn can_discard(test: Condition) -> bool {
    // Truthiness and strict equality do not coerce operands or invoke script.
    match test {
        Condition::Truthy { .. } => true,
        Condition::Compare { op, .. }
        | Condition::CompareConst { op, .. }
        | Condition::Binary { op, .. }
        | Condition::BinaryConst { op, .. } => {
            matches!(op, BinaryOp::StrictEq | BinaryOp::StrictNe)
        }
        Condition::Unary { .. } => false,
    }
}
