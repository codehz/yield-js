//! Forward copies within a straight-line region of logical-register code.
//! A fact holds only until a write to either register or a control-flow entry.

use std::collections::BTreeMap;

use yield_vm::bytecode::Instr;

use crate::emit::Reg;
use crate::instr::{map_slots, uses_defs};
use crate::ir::{LoweredFunction, logical};

pub(crate) fn propagate_copies(function: &mut LoweredFunction) {
    let mut entries = vec![false; function.code.len()];
    for instruction in &function.code {
        if let Instr::Jump { target } | Instr::BranchFalse { target, .. } = *instruction {
            entries[target as usize] = true;
        }
    }
    for handler in &function.handlers {
        entries[handler.target_pc as usize] = true;
    }
    for site in &function.awaits {
        entries[site.fulfill_pc] = true;
        entries[site.reject_pc] = true;
    }
    for site in &function.yields {
        entries[site.next_pc] = true;
        entries[site.throw_pc] = true;
        entries[site.return_pc] = true;
    }
    if let Some(start) = function.generator_start {
        entries[start.body_pc] = true;
    }

    let mut copies = BTreeMap::<Reg, (Reg, usize)>::new();
    // A copy records the source's last write, so one source write invalidates
    // every dependent copy without scanning the map.
    let mut written_at = vec![0; usize::from(function.reg_count)];
    for (pc, (entry, instruction)) in entries.into_iter().zip(&mut function.code).enumerate() {
        if entry {
            copies.clear();
        }
        if matches!(instruction, Instr::Move { dst, src } if dst == src) {
            continue;
        }
        if !copies.is_empty() {
            let (uses, defs) = uses_defs(instruction);
            // map_slots visits both reads and writes. Leave read/write operands
            // alone rather than inadvertently changing their destination.
            let forward = |reg| {
                let &(src, version) = copies.get(&reg)?;
                (written_at[usize::from(src)] == version
                    && !defs.contains(&reg)
                    && !defs.contains(&src))
                .then_some(src)
            };
            if uses.iter().any(|&reg| forward(reg).is_some()) {
                *instruction = map_slots(instruction, &|slot| {
                    forward(slot.index).map_or(slot, logical)
                });
            }
            for dst in defs {
                copies.remove(&dst);
                written_at[usize::from(dst)] = pc + 1;
            }
        }
        match *instruction {
            Instr::Move { dst, src } if dst != src => {
                copies.insert(dst.index, (src.index, written_at[usize::from(src.index)]));
            }
            Instr::Jump { .. }
            | Instr::BranchFalse { .. }
            | Instr::Await { .. }
            | Instr::Yield { .. }
            | Instr::GeneratorStart
            | Instr::Return { .. }
            | Instr::Throw { .. }
            | Instr::ThrowAssignmentToConst
            | Instr::ThrowDeleteSuper => copies.clear(),
            _ => {}
        }
    }
}
