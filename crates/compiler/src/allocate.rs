//! Frame-slot allocation.
//!
//! A logical register live at any await resume entry becomes a fixed
//! persistent slot. Every other register is transient and is dropped on
//! suspend. Persistent slots are not reused across different live values.

use std::collections::{BTreeMap, BTreeSet};
use yield_vm::bytecode::{
    ExceptionHandler, Function as BytecodeFunction, ParamTarget, ResumeEntry, Slot, SlotKind,
};

use crate::analyze;
use crate::emit::Reg;
use crate::error::CompileError;
use crate::instr::map_slots;
use crate::ir::LoweredFunction;

pub(crate) fn allocate(
    source: &str,
    lowered: LoweredFunction,
) -> Result<BytecodeFunction, CompileError> {
    let mut resumes = Vec::with_capacity(lowered.awaits.len());
    for await_site in &lowered.awaits {
        let result = Slot {
            kind: SlotKind::Transient,
            index: await_site.result,
        };
        resumes.push(ResumeEntry {
            fulfill_pc: await_site.fulfill_pc as u32,
            fulfill_dst: result,
            reject_pc: await_site.reject_pc as u32,
            reject_dst: result,
            saved_slots: Vec::new(),
        });
    }

    let live_in = analyze::live_in(&lowered.code, &resumes, &lowered.handlers)?;
    let mut saved_sets = Vec::with_capacity(resumes.len());
    for resume in &resumes {
        saved_sets.push(saved_set(source, &lowered, &live_in, resume)?);
    }
    let mut saved_regs = BTreeSet::new();
    for saved in &saved_sets {
        saved_regs.extend(saved);
    }

    let layout = FrameLayout::new(lowered.reg_count, &saved_regs);
    let code = lowered
        .code
        .iter()
        .map(|instruction| map_slots(instruction, &|slot| layout.remap(slot)))
        .collect::<Vec<_>>();
    let mut remapped_resumes = resumes;
    for (resume, saved) in remapped_resumes.iter_mut().zip(&saved_sets) {
        resume.saved_slots = saved
            .iter()
            .map(|reg| layout.persistent[reg])
            .collect::<Vec<_>>();
        resume.saved_slots.sort_unstable();
        resume.fulfill_dst = layout.remap(resume.fulfill_dst);
        resume.reject_dst = layout.remap(resume.reject_dst);
    }
    let handlers = lowered
        .handlers
        .iter()
        .map(|handler| ExceptionHandler {
            start_pc: handler.start_pc,
            end_pc: handler.end_pc,
            target_pc: handler.target_pc,
            dst: layout.remap(handler.dst),
        })
        .collect::<Vec<_>>();

    let param_target = |binding| match lowered.promoted.get(&binding) {
        Some(reg) => ParamTarget::Slot(layout.remap(Slot {
            kind: SlotKind::Transient,
            index: *reg,
        })),
        None => ParamTarget::Binding(binding),
    };
    let params = lowered
        .param_bindings
        .iter()
        .copied()
        .map(param_target)
        .collect();
    let rest_param = lowered.rest_binding.map(param_target);
    let this_param = lowered.this_binding.map(param_target);

    Ok(BytecodeFunction {
        func_id: lowered.func_id,
        name: lowered.name,
        is_async: lowered.is_async,
        is_arrow: lowered.is_arrow,
        is_method: lowered.is_method,
        length: lowered.length,
        params,
        rest_param,
        this_param,
        new_target_param: lowered.new_target_binding.map(param_target),
        arguments_param: lowered.arguments_binding.map(param_target),
        transient_slots: layout.transient_count,
        persistent_slots: layout.persistent_count,
        code,
        resumes: remapped_resumes,
        handlers,
    })
}

/// Fixed persistent slots for cross-await values; the rest are transient.
struct FrameLayout {
    persistent: BTreeMap<Reg, u16>,
    transient: BTreeMap<Reg, u16>,
    persistent_count: u16,
    transient_count: u16,
}

impl FrameLayout {
    fn new(reg_count: u16, saved: &BTreeSet<Reg>) -> Self {
        let persistent = saved
            .iter()
            .copied()
            .enumerate()
            .map(|(index, reg)| (reg, index as u16))
            .collect::<BTreeMap<_, _>>();
        let transient = (0..reg_count)
            .filter(|reg| !persistent.contains_key(reg))
            .enumerate()
            .map(|(index, reg)| (reg, index as u16))
            .collect::<BTreeMap<_, _>>();
        Self {
            persistent_count: persistent.len() as u16,
            transient_count: transient.len() as u16,
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

fn saved_set(
    source: &str,
    lowered: &LoweredFunction,
    live_in: &[BTreeSet<Reg>],
    resume: &ResumeEntry,
) -> Result<BTreeSet<Reg>, CompileError> {
    let mut saved = live_in[resume.fulfill_pc as usize].clone();
    saved.remove(&resume.fulfill_dst.index);
    let mut reject = live_in[resume.reject_pc as usize].clone();
    reject.remove(&resume.reject_dst.index);
    saved.extend(reject);
    if saved.iter().any(|reg| *reg >= lowered.reg_count) {
        return Err(CompileError::new(
            source,
            lowered.span,
            "internal liveness error: unknown register",
        ));
    }
    Ok(saved)
}
