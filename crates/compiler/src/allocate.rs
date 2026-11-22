//! Frame-slot allocation.
//!
//! A logical register live at any suspension entry becomes a fixed persistent
//! slot. Other registers use transient slots, reused across disjoint live
//! intervals and dropped on suspend. Persistent slots are never reused.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use yield_vm::bytecode::{Function as BytecodeFunction, ParamTarget, Slot, SlotKind};

use crate::analyze;
use crate::cfg::{FunctionCfg, LaidOutFunction};
use crate::emit::Reg;
use crate::error::CompileError;
use crate::instr::map_slots;
use crate::ir::FunctionAttrs;

pub(crate) fn allocate(
    source: &str,
    mut function: FunctionCfg,
) -> Result<BytecodeFunction, CompileError> {
    let live = analyze::eliminate_dead_writes(&mut function);
    let mut lowered = function.into_lowered(&live)?;
    let mut saved_regs = BTreeSet::new();
    for slots in lowered
        .resumes
        .iter()
        .map(|entry| &entry.saved_slots)
        .chain(lowered.yields.iter().map(|entry| &entry.saved_slots))
        .chain(
            lowered
                .generator_start
                .iter()
                .map(|entry| &entry.saved_slots),
        )
    {
        saved_regs.extend(slots.iter().copied());
    }
    check_saved(source, &lowered.attrs, &saved_regs)?;

    let layout = FrameLayout::new(&mut lowered, &saved_regs);
    for instruction in &mut lowered.code {
        *instruction = map_slots(instruction, &|slot| layout.remap(slot));
    }
    for resume in &mut lowered.resumes {
        remap_saved(&layout, &mut resume.saved_slots);
        resume.fulfill_dst = layout.remap(resume.fulfill_dst);
        resume.reject_dst = layout.remap(resume.reject_dst);
    }
    for entry in &mut lowered.yields {
        remap_saved(&layout, &mut entry.saved_slots);
        entry.next_dst = layout.remap(entry.next_dst);
        entry.throw_dst = layout.remap(entry.throw_dst);
    }
    if let Some(start) = &mut lowered.generator_start {
        remap_saved(&layout, &mut start.saved_slots);
    }
    for handler in &mut lowered.handlers {
        handler.dst = layout.remap(handler.dst);
    }

    let attrs = lowered.attrs;
    let param_target = |binding| match attrs.promoted.get(&binding) {
        Some(reg) => ParamTarget::Slot(layout.remap(Slot {
            kind: SlotKind::Transient,
            index: *reg,
        })),
        None => ParamTarget::Binding(binding),
    };
    let params = attrs
        .param_bindings
        .iter()
        .copied()
        .map(param_target)
        .collect();
    let rest_param = attrs.rest_binding.map(param_target);
    let this_param = attrs.this_binding.map(param_target);
    Ok(BytecodeFunction {
        func_id: attrs.func_id,
        name: attrs.name,
        initial_name: attrs.initial_name,
        is_async: attrs.is_async,
        is_arrow: attrs.is_arrow,
        is_method: attrs.is_method,
        is_generator: attrs.is_generator,
        length: attrs.length,
        params,
        rest_param,
        this_param,
        home_param: attrs.home_binding.map(param_target),
        new_target_param: attrs.new_target_binding.map(param_target),
        arguments_param: attrs.arguments_binding.map(param_target),
        transient_slots: layout.transient_count,
        persistent_slots: layout.persistent_count,
        code: lowered.code,
        resumes: lowered.resumes,
        generator_start: lowered.generator_start,
        generator_resumes: lowered.yields,
        handlers: lowered.handlers,
    })
}

/// Fixed persistent slots and reusable transient slots.
struct FrameLayout {
    persistent: BTreeMap<Reg, u16>,
    transient: BTreeMap<Reg, u16>,
    persistent_count: u16,
    transient_count: u16,
}

impl FrameLayout {
    fn new(lowered: &mut LaidOutFunction, saved: &BTreeSet<Reg>) -> Self {
        let persistent = saved
            .iter()
            .copied()
            .enumerate()
            .map(|(index, reg)| (reg, index as u16))
            .collect::<BTreeMap<_, _>>();
        let (transient, transient_count) = allocate_transients(lowered, saved);
        Self {
            persistent_count: persistent.len() as u16,
            transient_count,
            persistent,
            transient,
        }
    }

    fn remap(&self, slot: Slot) -> Slot {
        if self.persistent.contains_key(&slot.index) {
            Slot {
                kind: SlotKind::Persistent,
                index: self.persistent[&slot.index],
            }
        } else {
            Slot {
                kind: SlotKind::Transient,
                index: self.transient[&slot.index],
            }
        }
    }
}

/// Take a conservative linear interval over CFG live-ins and all writes.
/// Keeping whole instruction PCs inclusive avoids aliasing operands or outputs
/// of a single instruction, including definitions whose results are dead.
fn allocate_transients(
    lowered: &mut LaidOutFunction,
    saved: &BTreeSet<Reg>,
) -> (BTreeMap<Reg, u16>, u16) {
    let attrs = &lowered.attrs;
    let mut bounds = std::mem::take(&mut lowered.bounds);
    let mut touch = |reg: Reg, pc: usize| bounds.touch(reg, pc);

    // The VM writes parameters before the first instruction, and injects
    // exceptions and resume results before executing their target PCs.
    for binding in attrs
        .param_bindings
        .iter()
        .chain(&attrs.rest_binding)
        .chain(&attrs.this_binding)
        .chain(&attrs.home_binding)
        .chain(&attrs.new_target_binding)
        .chain(&attrs.arguments_binding)
    {
        if let Some(&reg) = attrs.promoted.get(binding) {
            touch(reg, 0);
        }
    }
    for handler in &lowered.handlers {
        touch(handler.dst.index, handler.target_pc as usize);
    }
    for entry in &lowered.resumes {
        touch(entry.fulfill_dst.index, entry.fulfill_pc as usize);
        touch(entry.reject_dst.index, entry.reject_pc as usize);
    }
    for entry in &lowered.yields {
        touch(entry.next_dst.index, entry.next_pc as usize);
        touch(entry.throw_dst.index, entry.throw_pc as usize);
    }

    let mut intervals = bounds
        .0
        .into_iter()
        .enumerate()
        .filter_map(|(reg, bound)| {
            let reg = reg as Reg;
            bound
                .filter(|_| !saved.contains(&reg))
                .map(|(first, last)| (first, last, reg))
        })
        .collect::<Vec<_>>();
    intervals.sort_unstable();
    let mut active = BinaryHeap::new();
    let mut free = Vec::new();
    let mut slots = BTreeMap::new();
    let mut count = 0;
    for (first, last, reg) in intervals {
        while let Some(&Reverse((end, slot))) = active.peek() {
            if end >= first {
                break;
            }
            active.pop();
            free.push(slot);
        }
        let slot = free.pop().unwrap_or_else(|| {
            let slot = count;
            count += 1;
            slot
        });
        slots.insert(reg, slot);
        active.push(Reverse((last, slot)));
    }
    (slots, count)
}

fn remap_saved(layout: &FrameLayout, saved: &mut [Reg]) {
    // Persistent slots follow register order, so this stays sorted.
    for reg in saved {
        *reg = layout.persistent[reg];
    }
}

fn check_saved(
    source: &str,
    attrs: &FunctionAttrs,
    saved: &BTreeSet<Reg>,
) -> Result<(), CompileError> {
    if saved.iter().any(|reg| *reg >= attrs.reg_count) {
        return Err(CompileError::new(
            source,
            attrs.span,
            "internal liveness error: unknown register",
        ));
    }
    Ok(())
}
