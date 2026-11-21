//! Frame-slot allocation.
//!
//! A logical register live at any suspension entry becomes a fixed persistent
//! slot. Every other register is transient and is dropped on suspend.
//! Persistent slots are not reused across different live values.

use std::collections::{BTreeMap, BTreeSet};
use yield_vm::bytecode::{
    ExceptionHandler, Function as BytecodeFunction, GeneratorEntry, GeneratorStart, ParamTarget,
    ResumeEntry, Slot, SlotKind,
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
        let result = logical(await_site.result);
        resumes.push(ResumeEntry {
            fulfill_pc: await_site.fulfill_pc as u32,
            fulfill_dst: result,
            reject_pc: await_site.reject_pc as u32,
            reject_dst: result,
            saved_slots: Vec::new(),
        });
    }
    let mut generators = Vec::with_capacity(lowered.yields.len());
    for site in &lowered.yields {
        generators.push(GeneratorEntry {
            next_pc: site.next_pc as u32,
            next_dst: logical(site.next),
            throw_pc: site.throw_pc as u32,
            throw_dst: logical(site.thrown),
            return_pc: site.return_pc as u32,
            saved_slots: Vec::new(),
        });
    }

    let suspensions = analyze::Suspensions {
        awaits: &resumes,
        generators: &generators,
    };
    let live_in = analyze::live_in(&lowered.code, suspensions, &lowered.handlers)?;
    let mut saved_sets = Vec::with_capacity(resumes.len());
    for resume in &resumes {
        saved_sets.push(saved_set(source, &lowered, &live_in, resume)?);
    }
    let mut generator_saved = Vec::with_capacity(generators.len());
    for entry in &generators {
        generator_saved.push(generator_saved_set(source, &lowered, &live_in, entry)?);
    }
    let mut saved_regs = BTreeSet::new();
    for saved in saved_sets.iter().chain(&generator_saved) {
        saved_regs.extend(saved);
    }
    // A generator body starts with every parameter-initialization value that is
    // still live, so the whole live-in set must survive the call.
    if let Some(start) = lowered.generator_start {
        saved_regs.extend(&live_in[start.body_pc]);
    }

    let layout = FrameLayout::new(lowered.reg_count, &saved_regs);
    let code = lowered
        .code
        .iter()
        .map(|instruction| map_slots(instruction, &|slot| layout.remap(slot)))
        .collect::<Vec<_>>();
    let mut remapped_resumes = resumes;
    for (resume, saved) in remapped_resumes.iter_mut().zip(&saved_sets) {
        resume.saved_slots = remapped_saved(&layout, saved);
        resume.fulfill_dst = layout.remap(resume.fulfill_dst);
        resume.reject_dst = layout.remap(resume.reject_dst);
    }
    let mut remapped_generators = generators;
    for (entry, saved) in remapped_generators.iter_mut().zip(&generator_saved) {
        entry.saved_slots = remapped_saved(&layout, saved);
        entry.next_dst = layout.remap(entry.next_dst);
        entry.throw_dst = layout.remap(entry.throw_dst);
    }
    let generator_start = lowered.generator_start.map(|start| GeneratorStart {
        body_pc: start.body_pc as u32,
        saved_slots: remapped_saved(&layout, &live_in[start.body_pc]),
    });
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
        initial_name: lowered.initial_name,
        is_async: lowered.is_async,
        is_arrow: lowered.is_arrow,
        is_method: lowered.is_method,
        is_generator: lowered.is_generator,
        length: lowered.length,
        params,
        rest_param,
        this_param,
        home_param: lowered.home_binding.map(param_target),
        new_target_param: lowered.new_target_binding.map(param_target),
        arguments_param: lowered.arguments_binding.map(param_target),
        transient_slots: layout.transient_count,
        persistent_slots: layout.persistent_count,
        code,
        resumes: remapped_resumes,
        generator_start,
        generator_resumes: remapped_generators,
        handlers,
    })
}

fn logical(reg: Reg) -> Slot {
    Slot {
        kind: SlotKind::Transient,
        index: reg,
    }
}

/// Fixed persistent slots for cross-suspension values; the rest are transient.
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

fn remapped_saved(layout: &FrameLayout, saved: &BTreeSet<Reg>) -> Vec<u16> {
    let mut slots = saved
        .iter()
        .map(|reg| layout.persistent[reg])
        .collect::<Vec<_>>();
    slots.sort_unstable();
    slots
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
    check_saved(source, lowered, &saved)?;
    Ok(saved)
}

/// Each entry subtracts only its own injected result before the three sets are
/// joined. Subtracting after the union would drop a value live on another entry.
fn generator_saved_set(
    source: &str,
    lowered: &LoweredFunction,
    live_in: &[BTreeSet<Reg>],
    entry: &GeneratorEntry,
) -> Result<BTreeSet<Reg>, CompileError> {
    let mut saved = live_in[entry.next_pc as usize].clone();
    saved.remove(&entry.next_dst.index);
    let mut thrown = live_in[entry.throw_pc as usize].clone();
    thrown.remove(&entry.throw_dst.index);
    saved.extend(thrown);
    saved.extend(&live_in[entry.return_pc as usize]);
    check_saved(source, lowered, &saved)?;
    Ok(saved)
}

fn check_saved(
    source: &str,
    lowered: &LoweredFunction,
    saved: &BTreeSet<Reg>,
) -> Result<(), CompileError> {
    if saved.iter().any(|reg| *reg >= lowered.reg_count) {
        return Err(CompileError::new(
            source,
            lowered.span,
            "internal liveness error: unknown register",
        ));
    }
    Ok(())
}
