//! Linear logical-register input from the syntax lowerer.
//!
//! Each [`Slot`] is logical: `kind` is meaningless and `index` is a [`Reg`].
//! Binding classification reads this input before CFG construction consumes it.

use std::collections::BTreeMap;

use oxc::span::Span;
use yield_vm::BindingId;
use yield_vm::bytecode::{ExceptionHandler, Instr, Slot, SlotKind};

use crate::emit::Reg;

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

/// Function attributes independent of code layout.
#[derive(Debug)]
pub(crate) struct FunctionAttrs {
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
    pub(crate) span: Span,
}

/// Linear code and side tables emitted by syntax lowering.
#[derive(Debug)]
pub(crate) struct LoweredFunction {
    pub(crate) attrs: FunctionAttrs,
    pub(crate) code: Vec<Instr>,
    pub(crate) origins: Option<Vec<Option<u32>>>,
    pub(crate) awaits: Vec<AwaitSite>,
    /// Where parameter initialization hands over to the generator body.
    pub(crate) generator_start: Option<GeneratorStartSite>,
    pub(crate) yields: Vec<YieldSite>,
    pub(crate) handlers: Vec<ExceptionHandler>,
    pub(crate) iteration_envs: Vec<IterationEnvGroup>,
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

pub(crate) fn logical(reg: Reg) -> Slot {
    Slot {
        kind: SlotKind::Transient,
        index: reg,
    }
}
