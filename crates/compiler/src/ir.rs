//! Compiler IR after syntax lowering and before frame-slot allocation.
//!
//! Instructions still use the VM [`Instr`] enum, but every [`Slot`] is a
//! logical register: `kind` is meaningless and `index` is a [`Reg`]. Oxc is
//! gone. Promotion, optimization, liveness, and allocation operate on this IR.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use oxc::span::Span;
use yield_vm::BindingId;
use yield_vm::bytecode::{ExceptionHandler, GeneratorEntry, Instr, ResumeEntry, Slot, SlotKind};

use crate::emit::Reg;
use crate::error::CompileError;

/// A value location assigned during lowering, before persistent/transient split.
pub(crate) type LogicalReg = Reg;
#[derive(Debug, Clone, Copy)]
pub(crate) struct AwaitSite {
    /// Destination of a fulfilled settlement; also the fall-through register.
    pub(crate) result: LogicalReg,
    /// Destination of a rejection. Usually the same register, but a generator
    /// return entry routes the reason into its existing throw stub instead.
    pub(crate) reject: LogicalReg,
    pub(crate) fulfill_pc: usize,
    pub(crate) reject_pc: usize,
}

/// One lowered per-iteration environment group, before escape analysis decides
/// which bindings still need fresh copies.
#[derive(Debug)]
pub(crate) struct IterationEnvGroup {
    /// Loop-head bindings copied by `NewIterationEnv`.
    pub(crate) bindings: Vec<BindingId>,
    /// Register holding the outer environment saved by `SaveEnv`.
    pub(crate) saved_env: LogicalReg,
    /// PC of the `SaveEnv`.
    pub(crate) save_pc: usize,
    /// PCs of every `NewIterationEnv` that installs a fresh environment.
    /// A C-style `for` has one before the test and one before `update`.
    pub(crate) enter_pcs: Vec<usize>,
    /// PC of the `RestoreEnv` after the loop.
    pub(crate) restore_pc: Option<usize>,
}

/// One function lowered to logical-register bytecode.
///
/// `resumes` are not filled in yet: each [`AwaitSite`] becomes a
/// [`yield_vm::bytecode::ResumeEntry`] once liveness decides `saved_slots`.
#[derive(Debug)]
pub(crate) struct LoweredFunction {
    pub(crate) func_id: u32,
    pub(crate) name: String,
    pub(crate) initial_name: String,
    pub(crate) is_async: bool,
    pub(crate) is_arrow: bool,
    pub(crate) is_method: bool,
    pub(crate) is_generator: bool,
    pub(crate) length: u16,
    pub(crate) param_bindings: Vec<BindingId>,
    pub(crate) rest_binding: Option<BindingId>,
    /// Implicit receiver. Not a source parameter and not a JavaScript argument.
    pub(crate) this_binding: Option<BindingId>,
    pub(crate) home_binding: Option<BindingId>,
    pub(crate) new_target_binding: Option<BindingId>,
    pub(crate) arguments_binding: Option<BindingId>,
    /// Non-shared bindings assigned a fresh logical register, in `BindingId` order.
    pub(crate) promoted: BTreeMap<BindingId, LogicalReg>,
    pub(crate) reg_count: u16,
    pub(crate) code: Vec<Instr>,
    pub(crate) awaits: Vec<AwaitSite>,
    /// Where parameter initialization hands over to the generator body.
    pub(crate) generator_start: Option<GeneratorStartSite>,
    pub(crate) yields: Vec<YieldSite>,
    pub(crate) handlers: Vec<ExceptionHandler>,
    pub(crate) iteration_envs: Vec<IterationEnvGroup>,
    pub(crate) span: Span,
}

/// One lowered `yield`, before liveness decides `saved_slots`.
///
/// Each of the three externally injected completions has its own landing site.
/// The return site reads its payload with `LoadGeneratorReturn` instead of a
/// table destination, so no slot exists for it here.
#[derive(Debug, Clone, Copy)]
pub(crate) struct YieldSite {
    /// Register written before entering `next_pc`.
    pub(crate) next: LogicalReg,
    /// Register written before entering `throw_pc`.
    pub(crate) thrown: LogicalReg,
    pub(crate) next_pc: usize,
    pub(crate) throw_pc: usize,
    pub(crate) return_pc: usize,
}

/// Where a generator body starts in logical-register code.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GeneratorStartSite {
    pub(crate) body_pc: usize,
}

/// Compact code and all PC-bearing fields. A deleted target lands on the next
/// surviving instruction, except at exclusive entries (which keep a bridge).
/// `dead` is updated to contain exactly the PCs that were removed.
pub(crate) fn compact_dead_instructions(
    function: &mut LoweredFunction,
    dead: &mut BTreeSet<usize>,
) -> Result<(), CompileError> {
    let old_len = function.code.len();
    if dead.iter().any(|&pc| pc >= old_len) {
        return Err(CompileError::internal(
            "internal compiler error: deleted instruction is out of range",
        ));
    }
    let mut exclusive = BTreeSet::new();
    for (pc, instruction) in function.code.iter().enumerate() {
        if !dead.contains(&pc)
            && let Instr::Yield { generator, .. } = instruction
        {
            let site = &function.yields[*generator as usize];
            exclusive.extend([site.throw_pc, site.return_pc]);
        }
    }
    if let Some(start) = function.generator_start
        && dead.remove(&start.body_pc)
    {
        function.code[start.body_pc] = Instr::Jump {
            target: start.body_pc as u32 + 1,
        };
    }
    // A pure write at an exclusive entry may be dead, but remapping that
    // entry onto the next instruction could make it an ordinary jump target.
    for pc in exclusive {
        if dead.remove(&pc) {
            function.code[pc] = Instr::Jump {
                target: pc as u32 + 1,
            };
        }
    }

    let mut mapping = Vec::with_capacity(old_len + 1);
    let mut next = 0u32;
    for pc in 0..old_len {
        mapping.push(next);
        if !dead.contains(&pc) {
            next = next.checked_add(1).ok_or_else(|| {
                CompileError::internal("internal compiler error: too many instructions")
            })?;
        }
    }
    mapping.push(next);
    let remap = |pc: u32| -> Result<u32, CompileError> {
        mapping
            .get(pc as usize)
            .copied()
            .ok_or_else(|| CompileError::internal("internal compiler error: PC is out of range"))
    };

    let mut code = Vec::with_capacity(next as usize);
    let old_awaits = std::mem::take(&mut function.awaits);
    let old_yields = std::mem::take(&mut function.yields);
    let mut await_ids = vec![None; old_awaits.len()];
    let mut yield_ids = vec![None; old_yields.len()];
    for (pc, instruction) in std::mem::take(&mut function.code).into_iter().enumerate() {
        if dead.contains(&pc) {
            continue;
        }
        code.push(match instruction {
            Instr::Jump { target } => Instr::Jump {
                target: remap(target)?,
            },
            Instr::BranchFalse { cond, target } => Instr::BranchFalse {
                cond,
                target: remap(target)?,
            },
            Instr::Await { dst, input, resume } => {
                let id = await_ids[resume as usize].get_or_insert_with(|| {
                    let id = function.awaits.len() as u32;
                    function.awaits.push(old_awaits[resume as usize]);
                    id
                });
                Instr::Await {
                    dst,
                    input,
                    resume: *id,
                }
            }
            Instr::Yield {
                input,
                generator,
                delegate,
            } => {
                let id = yield_ids[generator as usize].get_or_insert_with(|| {
                    let id = function.yields.len() as u32;
                    function.yields.push(old_yields[generator as usize]);
                    id
                });
                Instr::Yield {
                    input,
                    generator: *id,
                    delegate,
                }
            }
            other => other,
        });
    }
    function.code = code;

    for site in &mut function.awaits {
        site.fulfill_pc = remap(site.fulfill_pc as u32)? as usize;
        site.reject_pc = remap(site.reject_pc as u32)? as usize;
    }
    for site in &mut function.yields {
        site.next_pc = remap(site.next_pc as u32)? as usize;
        site.throw_pc = remap(site.throw_pc as u32)? as usize;
        site.return_pc = remap(site.return_pc as u32)? as usize;
    }
    if let Some(start) = &mut function.generator_start {
        start.body_pc = remap(start.body_pc as u32)? as usize;
    }
    // Compaction can make distinct ranges equal. The VM prefers the latest
    // start, then earliest end, then first table entry. Keep that tie order.
    function
        .handlers
        .sort_by_key(|handler| (Reverse(handler.start_pc), handler.end_pc));
    for handler in &mut function.handlers {
        handler.start_pc = remap(handler.start_pc)?;
        handler.end_pc = remap(handler.end_pc)?;
        handler.target_pc = remap(handler.target_pc)?;
    }
    // An empty protected range has no throwing instructions left.
    function
        .handlers
        .retain(|handler| handler.start_pc != handler.end_pc);
    Ok(())
}

pub(crate) fn logical(reg: Reg) -> Slot {
    Slot {
        kind: SlotKind::Transient,
        index: reg,
    }
}

pub(crate) fn generator_stubs(function: &LoweredFunction) -> Vec<GeneratorEntry> {
    function
        .yields
        .iter()
        .map(|site| GeneratorEntry {
            next_pc: site.next_pc as u32,
            next_dst: logical(site.next),
            throw_pc: site.throw_pc as u32,
            throw_dst: logical(site.thrown),
            return_pc: site.return_pc as u32,
            saved_slots: Vec::new(),
        })
        .collect()
}

pub(crate) fn resume_stubs(function: &LoweredFunction) -> Vec<ResumeEntry> {
    function
        .awaits
        .iter()
        .map(|site| ResumeEntry {
            fulfill_pc: site.fulfill_pc as u32,
            fulfill_dst: logical(site.result),
            reject_pc: site.reject_pc as u32,
            reject_dst: logical(site.reject),
            saved_slots: Vec::new(),
        })
        .collect()
}
