//! Compiler IR after syntax lowering and before frame-slot allocation.
//!
//! Instructions still use the VM [`Instr`] enum, but every [`Slot`] is a
//! logical register: `kind` is meaningless and `index` is a [`Reg`]. Oxc is
//! gone. Liveness and slot allocation are the only consumers.

use std::collections::BTreeMap;

use oxc::span::Span;
use yield_vm::BindingId;
use yield_vm::bytecode::{ExceptionHandler, Instr};

use crate::emit::Reg;

/// A value location assigned during lowering, before persistent/transient split.
pub(crate) type LogicalReg = Reg;
#[derive(Debug)]
pub(crate) struct AwaitSite {
    pub(crate) result: LogicalReg,
    pub(crate) fulfill_pc: usize,
    pub(crate) reject_pc: usize,
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
    pub(crate) handlers: Vec<ExceptionHandler>,
    pub(crate) span: Span,
}
