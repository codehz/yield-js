//! Structural checks at the logical CFG boundaries, before PC layout.

use yield_vm::bytecode::Instr;

use super::{BlockId, ENTRY, FunctionCfg, Terminator, ends_block, invalid};
use crate::error::CompileError;
use crate::instr::may_throw;

impl FunctionCfg {
    pub(crate) fn validate(&self) -> Result<(), CompileError> {
        let live = |id: BlockId| self.blocks.get(id.0).is_some_and(Option::is_some);
        if !live(ENTRY) || self.attrs.is_generator != self.generator_boundary.is_some() {
            return Err(invalid(
                "missing function entry or inconsistent generator boundary",
            ));
        }
        if self.generator_boundary.is_some_and(|id| {
            !live(id)
                || !matches!(
                    self.blocks[id.0].as_ref().unwrap().end,
                    Terminator::GeneratorStart { .. }
                )
        }) {
            return Err(invalid("missing generator start terminator"));
        }

        let mut exclusive = vec![false; self.blocks.len()];
        let mut order = self
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(id, block)| block.as_ref().map(|block| (BlockId(id), block)))
            .peekable();
        while let Some((id, block)) = order.next() {
            for edge in block.end.successors() {
                if !live(edge.target)
                    || edge.injected.is_some_and(|reg| reg >= self.attrs.reg_count)
                {
                    return Err(invalid("invalid terminator edge"));
                }
            }
            let adjacent = order.peek().map(|(id, _)| *id);
            match block.end {
                Terminator::Branch { next, .. } if Some(next) != adjacent => {
                    return Err(invalid("branch fallthrough is not adjacent"));
                }
                Terminator::GeneratorStart { body } => {
                    if Some(id) != self.generator_boundary || Some(body) != adjacent {
                        return Err(invalid("invalid generator start edge"));
                    }
                    exclusive[body.0] = true;
                }
                Terminator::Yield {
                    next,
                    thrown,
                    returned,
                    ..
                } => {
                    let return_entry = self.blocks[returned.0].as_ref().unwrap();
                    if !self.attrs.is_generator
                        || Some(next.target) != adjacent
                        || !matches!(
                            return_entry.statements.first().map(|stmt| &stmt.instr),
                            Some(Instr::LoadGeneratorReturn { .. })
                        )
                    {
                        return Err(invalid("invalid yield entries"));
                    }
                    exclusive[thrown.target.0] = true;
                    exclusive[returned.0] = true;
                }
                _ => {}
            }
        }

        if exclusive[ENTRY.0] {
            return Err(invalid("function entry is an exclusive generator entry"));
        }
        for (id, block) in self.blocks.iter().enumerate() {
            let Some(block) = block else { continue };
            match block.end {
                Terminator::Next(_) | Terminator::Jump(_) | Terminator::Branch { .. }
                    if block.end.successors().any(|edge| exclusive[edge.target.0]) =>
                {
                    return Err(invalid(
                        "ordinary control flow enters an exclusive generator entry",
                    ));
                }
                _ => {}
            }
            if block.statements.iter().any(|stmt| ends_block(&stmt.instr)) {
                return Err(invalid("control-flow instruction in block body"));
            }
            let can_throw = !block.protected_by.is_empty()
                && (block.end.may_throw()
                    || block.statements.iter().any(|stmt| may_throw(&stmt.instr)));
            for &handler in &block.protected_by {
                let Some(handler) = self.handlers.get(handler) else {
                    return Err(invalid("unknown exception handler"));
                };
                if handler.entry.target == BlockId(id)
                    || handler.entry.dst >= self.attrs.reg_count
                    || (can_throw
                        && (!live(handler.entry.target) || exclusive[handler.entry.target.0]))
                {
                    return Err(invalid("invalid exception handler edge"));
                }
            }
        }
        Ok(())
    }
}
