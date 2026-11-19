//! Exception and abrupt-completion lowering.
//!
//! `finally` does not use the VM's native exception stack. A pending
//! `return`, `throw`, `break`, or `continue` is an explicit completion
//! (kind + payload) routed to the finalizer and dispatched after its body.
//! `await` records both resume entries before slot allocation fills `saved_slots`.

use oxc::ast::ast::*;
use yield_vm::bytecode::{ExceptionHandler, Instr};

use super::{Lowerer, for_each_binding_name};
use crate::emit::Reg;
use crate::error::CompileError;
use crate::ir::AwaitSite;

#[derive(Clone, Copy)]
pub(super) enum Completion {
    Normal,
    Return,
    Throw,
    Jump(usize),
}

impl Completion {
    fn value(self) -> f64 {
        match self {
            Self::Normal => 0.0,
            Self::Return => 1.0,
            Self::Throw => 2.0,
            Self::Jump(index) => 3.0 + index as f64,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum JumpTarget {
    LoopBreak(usize),
    SwitchBreak(usize),
    LabelBreak(usize),
    LoopContinue(usize),
}

pub(super) struct Loop {
    pub(super) break_jumps: Vec<usize>,
    pub(super) continue_jumps: Vec<usize>,
    pub(super) continue_target: Option<usize>,
    pub(super) finalizer_depth: usize,
    pub(super) seq: usize,
    break_depth: usize,
}

pub(super) struct Switch {
    pub(super) break_jumps: Vec<usize>,
    pub(super) finalizer_depth: usize,
    pub(super) seq: usize,
}

struct Finalizer {
    kind: Reg,
    payload: Reg,
    entry: Option<usize>,
    entry_jumps: Vec<usize>,
    jump_routes: Vec<(JumpTarget, usize)>,
}

/// One active statement label. `loop_index` is the position in `loops` the
/// labelled loop occupies (or will occupy) while this frame is active;
/// `None` for labels on plain blocks, whose only use is `break`.
pub(super) struct LabelFrame<'ast> {
    name: &'ast str,
    finalizer_depth: usize,
    loop_index: Option<usize>,
    pub(super) break_jumps: Vec<usize>,
}

pub(super) struct Control<'ast> {
    loops: Vec<Loop>,
    switches: Vec<Switch>,
    labels: Vec<LabelFrame<'ast>>,
    finalizers: Vec<Finalizer>,
    handlers: Vec<ExceptionHandler>,
    awaits: Vec<AwaitSite>,
    seq: usize,
}

impl<'ast> Control<'ast> {
    pub(super) fn new() -> Self {
        Self {
            loops: Vec::new(),
            switches: Vec::new(),
            labels: Vec::new(),
            finalizers: Vec::new(),
            handlers: Vec::new(),
            awaits: Vec::new(),
            seq: 0,
        }
    }

    pub(super) fn finalizers_finished(&self) -> bool {
        self.finalizers.is_empty()
    }

    fn finalizer_depth(&self) -> usize {
        self.finalizers.len()
    }

    pub(super) fn innermost_continue(&self) -> Option<(JumpTarget, usize)> {
        let index = self.loops.len().checked_sub(1)?;
        Some((
            JumpTarget::LoopContinue(index),
            self.loops[index].finalizer_depth,
        ))
    }

    pub(super) fn push_loop(&mut self, continue_target: Option<usize>) {
        let seq = self.seq;
        self.seq += 1;
        self.loops.push(Loop {
            break_jumps: Vec::new(),
            continue_jumps: Vec::new(),
            continue_target,
            finalizer_depth: self.finalizers.len(),
            break_depth: self.finalizers.len(),
            seq,
        });
    }

    fn push_iterator_loop(&mut self, continue_target: usize) {
        self.push_loop(Some(continue_target));
        // Continuing this loop keeps its iterator open; breaking exits it.
        self.loops.last_mut().expect("loop stack").break_depth -= 1;
    }

    pub(super) fn pop_loop(&mut self) -> Loop {
        self.loops.pop().expect("loop stack")
    }

    pub(super) fn push_switch(&mut self) {
        let seq = self.seq;
        self.seq += 1;
        self.switches.push(Switch {
            break_jumps: Vec::new(),
            finalizer_depth: self.finalizers.len(),
            seq,
        });
    }

    pub(super) fn pop_switch(&mut self) -> Switch {
        self.switches.pop().expect("switch stack")
    }

    /// Innermost breakable: the top of whichever stack has the larger
    /// sequence number. Returns its indexed target and finalizer depth.
    pub(super) fn innermost_break(&self) -> Option<(JumpTarget, usize)> {
        match (self.loops.last(), self.switches.last()) {
            (l, Some(s)) if l.is_none_or(|l| l.seq < s.seq) => Some((
                JumpTarget::SwitchBreak(self.switches.len() - 1),
                s.finalizer_depth,
            )),
            (Some(l), _) => Some((JumpTarget::LoopBreak(self.loops.len() - 1), l.break_depth)),
            (None, _) => None,
        }
    }

    pub(super) fn loops_len(&self) -> usize {
        self.loops.len()
    }

    pub(super) fn labels_len(&self) -> usize {
        self.labels.len()
    }

    /// Innermost active frame with this name.
    pub(super) fn find_label(&self, name: &str) -> Option<usize> {
        self.labels.iter().rposition(|frame| frame.name == name)
    }

    /// Pushes one label frame. Duplicate detection is the caller's job.
    pub(super) fn push_label(&mut self, name: &'ast str, loop_index: Option<usize>) {
        self.labels.push(LabelFrame {
            name,
            finalizer_depth: self.finalizers.len(),
            loop_index,
            break_jumps: Vec::new(),
        });
    }

    /// Pops label frames down to `len`, returning the removed frames so the
    /// caller can patch their break jumps.
    pub(super) fn pop_labels_to(&mut self, len: usize) -> Vec<LabelFrame<'ast>> {
        self.labels.split_off(len)
    }

    pub(super) fn label_finalizer_depth(&self, index: usize) -> usize {
        self.labels[index].finalizer_depth
    }

    pub(super) fn label_loop_index(&self, index: usize) -> Option<usize> {
        self.labels[index].loop_index
    }

    pub(super) fn loop_continue_depth(&self, index: usize) -> usize {
        self.loops[index].finalizer_depth
    }

    fn push_continue_to(&mut self, index: usize, jump: usize) {
        self.loops[index].continue_jumps.push(jump);
    }

    pub(super) fn take_awaits(&mut self) -> Vec<AwaitSite> {
        std::mem::take(&mut self.awaits)
    }

    pub(super) fn take_handlers(&mut self) -> Vec<ExceptionHandler> {
        std::mem::take(&mut self.handlers)
    }

    pub(super) fn push_handler(&mut self, handler: ExceptionHandler) {
        self.handlers.push(handler);
    }
}

impl<'ast, 'borrow, 'src> Lowerer<'ast, 'borrow, 'src> {
    pub(super) fn emit_control_jump(
        &mut self,
        target: JumpTarget,
        depth: usize,
    ) -> Result<(), CompileError> {
        let current_depth = self.control.finalizer_depth();
        debug_assert!(depth <= current_depth);
        if depth < current_depth {
            // The target encloses this finalizer and stays on its stack
            // until the last finalizer it crosses has dispatched the jump.
            let routes = &mut self
                .control
                .finalizers
                .last_mut()
                .expect("finalizer")
                .jump_routes;
            let route = (target, depth);
            let index = routes
                .iter()
                .position(|entry| *entry == route)
                .unwrap_or_else(|| {
                    let index = routes.len();
                    routes.push(route);
                    index
                });
            self.route_completion(Completion::Jump(index), None)?;
            return Ok(());
        }
        match target {
            JumpTarget::LoopBreak(index) => {
                let jump = self.jump();
                self.control.loops[index].break_jumps.push(jump);
            }
            JumpTarget::SwitchBreak(index) => {
                let jump = self.jump();
                self.control.switches[index].break_jumps.push(jump);
            }
            JumpTarget::LabelBreak(index) => {
                let jump = self.jump();
                self.control.labels[index].break_jumps.push(jump);
            }
            JumpTarget::LoopContinue(index) => {
                if let Some(pc) = self.control.loops[index].continue_target {
                    self.jump_to(pc);
                } else {
                    let jump = self.jump();
                    self.control.push_continue_to(index, jump);
                }
            }
        }
        Ok(())
    }

    pub(super) fn emit_await(&mut self, input: Reg) -> Result<Reg, CompileError> {
        let result = self.alloc_reg()?;
        let resume = self.control.awaits.len() as u32;
        self.emit(Instr::Await {
            dst: self.slot(result),
            input: self.slot(input),
            resume,
        });
        let skip_reject = self.jump();
        let reject_pc = self.pc();
        self.emit(Instr::Throw {
            src: self.slot(result),
        });
        let fulfill_pc = self.pc();
        self.patch_jump(skip_reject, fulfill_pc)?;
        self.control.awaits.push(AwaitSite {
            result,
            fulfill_pc,
            reject_pc,
        });
        Ok(result)
    }

    pub(super) fn emit_return(&mut self, value: Reg) -> Result<(), CompileError> {
        if self.control.finalizers.is_empty() {
            self.emit(Instr::Return {
                src: self.slot(value),
            });
            return Ok(());
        }
        self.route_completion(Completion::Return, Some(value))?;
        Ok(())
    }

    fn push_finalizer(&mut self) -> Result<(), CompileError> {
        let kind = self.alloc_reg()?;
        let payload = self.alloc_reg()?;
        self.control.finalizers.push(Finalizer {
            kind,
            payload,
            entry: None,
            entry_jumps: Vec::new(),
            jump_routes: Vec::new(),
        });
        Ok(())
    }

    /// Protect assignment and the body, not `next`, `done` or `value` reads.
    pub(super) fn compile_iterator_body(
        &mut self,
        iterator: Reg,
        step_pc: usize,
        body: impl FnOnce(&mut Self) -> Result<(), CompileError>,
    ) -> Result<Loop, CompileError> {
        let exception = self.alloc_reg()?;
        self.push_finalizer()?;
        self.control.push_iterator_loop(step_pc);
        let start = self.pc();
        body(self)?;
        let end = self.pc();
        self.jump_to(step_pc);

        let throw_pc = self.pc();
        self.emit(Instr::IteratorClose {
            iterator: self.slot(iterator),
            preserve_throw: true,
        });
        self.emit(Instr::Throw {
            src: self.slot(exception),
        });
        self.control.push_handler(ExceptionHandler {
            start_pc: start as u32,
            end_pc: end as u32,
            target_pc: throw_pc as u32,
            dst: self.slot(exception),
        });

        // The loop must stay on the control stack until pending breaks have
        // been dispatched. The close itself must not re-enter this finalizer.
        let context = self.control.finalizers.pop().expect("iterator finalizer");
        let close_pc = self.pc();
        for &jump in &context.entry_jumps {
            self.patch_jump(jump, close_pc)?;
        }
        self.emit(Instr::IteratorClose {
            iterator: self.slot(iterator),
            preserve_throw: false,
        });
        self.dispatch_finally(&context)?;
        Ok(self.control.pop_loop())
    }

    pub(super) fn compile_try(
        &mut self,
        statement: &'ast TryStatement<'ast>,
    ) -> Result<(), CompileError> {
        let has_finalizer = statement.finalizer.is_some();
        let exception = self.alloc_reg()?;
        if has_finalizer {
            self.push_finalizer()?;
        }

        let try_start = self.pc();
        self.compile_block(&statement.block)?;
        let try_end = self.pc();
        let mut end_jumps = Vec::new();
        if has_finalizer {
            self.route_completion(Completion::Normal, None)?;
        } else {
            end_jumps.push(self.jump());
        }

        let mut catch_body = None;
        if let Some(handler) = &statement.handler {
            // Parameter initialization can throw too: the finalizer's handler
            // must cover the entire catch clause, not just its body.
            let catch_start = self.pc();
            self.env.enter_block();
            if let Some(parameter) = &handler.param {
                let mut bindings = Vec::new();
                for_each_binding_name(&parameter.pattern, &mut |identifier| {
                    if let Some(binding) = self.predeclare_name(
                        identifier.name.as_str(),
                        VariableDeclarationKind::Let,
                        identifier.span,
                    )? {
                        bindings.push(binding);
                    }
                    Ok::<(), CompileError>(())
                })?;
                self.open_pending(bindings)?;
                self.compile_binding_pattern(&parameter.pattern, exception)?;
            }
            // Body declarations are not in scope in parameter initializers.
            self.compile_block(&handler.body)?;
            if has_finalizer {
                self.route_completion(Completion::Normal, None)?;
            } else {
                end_jumps.push(self.jump());
            }
            let catch_region_end = self.pc();
            self.env.exit_block();
            catch_body = Some((catch_start, catch_region_end));
        }

        if has_finalizer {
            // Abrupt completions in the finalizer must not re-enter it.
            let mut context = self.control.finalizers.pop().expect("finalizer stack");
            let throw_stub = self.pc();
            self.emit(Instr::Move {
                dst: self.slot(context.payload),
                src: self.slot(exception),
            });
            let throw_kind = self.emit_number(Completion::Throw.value())?;
            self.emit(Instr::Move {
                dst: self.slot(context.kind),
                src: self.slot(throw_kind),
            });
            let jump = self.jump();
            context.entry_jumps.push(jump);

            if let Some((catch_start, catch_region_end)) = catch_body {
                self.control.push_handler(ExceptionHandler {
                    start_pc: catch_start as u32,
                    end_pc: catch_region_end as u32,
                    target_pc: throw_stub as u32,
                    dst: self.slot(exception),
                });
                self.control.push_handler(ExceptionHandler {
                    start_pc: try_start as u32,
                    end_pc: try_end as u32,
                    target_pc: catch_start as u32,
                    dst: self.slot(exception),
                });
            } else {
                self.control.push_handler(ExceptionHandler {
                    start_pc: try_start as u32,
                    end_pc: try_end as u32,
                    target_pc: throw_stub as u32,
                    dst: self.slot(exception),
                });
            }

            let entry = self.pc();
            for jump in context.entry_jumps.drain(..) {
                self.patch_jump(jump, entry)?;
            }
            self.compile_block(statement.finalizer.as_ref().expect("finalizer"))?;
            self.dispatch_finally(&context)?;
        } else {
            if let Some((catch_start, _)) = catch_body {
                self.control.push_handler(ExceptionHandler {
                    start_pc: try_start as u32,
                    end_pc: try_end as u32,
                    target_pc: catch_start as u32,
                    dst: self.slot(exception),
                });
            }
            let end = self.pc();
            for jump in end_jumps {
                self.patch_jump(jump, end)?;
            }
        }

        Ok(())
    }

    fn route_completion(
        &mut self,
        kind: Completion,
        payload: Option<Reg>,
    ) -> Result<usize, CompileError> {
        let (kind_reg, payload_slot) = {
            let context = self.control.finalizers.last().ok_or_else(|| {
                CompileError::new(
                    self.source_text(),
                    self.span,
                    "internal compiler error: missing finalizer",
                )
            })?;
            (context.kind, context.payload)
        };
        let kind_value = self.emit_number(kind.value())?;
        self.emit(Instr::Move {
            dst: self.slot(kind_reg),
            src: self.slot(kind_value),
        });
        match payload {
            Some(reg) => {
                self.emit(Instr::Move {
                    dst: self.slot(payload_slot),
                    src: self.slot(reg),
                });
            }
            None => {
                self.emit(Instr::Undefined {
                    dst: self.slot(payload_slot),
                });
            }
        }
        let jump = self.jump();
        self.control
            .finalizers
            .last_mut()
            .expect("finalizer checked above")
            .entry_jumps
            .push(jump);
        Ok(jump)
    }

    fn dispatch_finally(&mut self, context: &Finalizer) -> Result<usize, CompileError> {
        let normal = self.emit_number(Completion::Normal.value())?;
        let is_normal = self.alloc_reg()?;
        self.emit(Instr::Binary {
            dst: self.slot(is_normal),
            op: yield_vm::bytecode::BinaryOp::StrictEq,
            lhs: self.slot(context.kind),
            rhs: self.slot(normal),
        });
        let not_normal = self.branch_false(is_normal);
        let normal_jump = self.jump();
        let not_normal_pc = self.pc();
        self.patch_jump(not_normal, not_normal_pc)?;

        let returning = self.emit_number(Completion::Return.value())?;
        let is_return = self.alloc_reg()?;
        self.emit(Instr::Binary {
            dst: self.slot(is_return),
            op: yield_vm::bytecode::BinaryOp::StrictEq,
            lhs: self.slot(context.kind),
            rhs: self.slot(returning),
        });
        let not_return = self.branch_false(is_return);
        self.emit_pending_return(context.payload)?;
        self.patch_jump(not_return, self.pc())?;

        for (index, &(target, depth)) in context.jump_routes.iter().enumerate() {
            let jumping = self.emit_number(Completion::Jump(index).value())?;
            let is_jump = self.alloc_reg()?;
            self.emit(Instr::Binary {
                dst: self.slot(is_jump),
                op: yield_vm::bytecode::BinaryOp::StrictEq,
                lhs: self.slot(context.kind),
                rhs: self.slot(jumping),
            });
            let not_jump = self.branch_false(is_jump);
            self.emit_control_jump(target, depth)?;
            self.patch_jump(not_jump, self.pc())?;
        }
        self.emit(Instr::Throw {
            src: self.slot(context.payload),
        });
        let end = self.pc();
        self.patch_jump(normal_jump, end)?;
        Ok(end)
    }

    fn emit_pending_return(&mut self, payload: Reg) -> Result<(), CompileError> {
        if self.control.finalizers.last().is_some() {
            let (kind, outer_payload, entry) = {
                let outer = self.control.finalizers.last().expect("outer finalizer");
                (outer.kind, outer.payload, outer.entry)
            };
            self.emit(Instr::Move {
                dst: self.slot(outer_payload),
                src: self.slot(payload),
            });
            let returning = self.emit_number(Completion::Return.value())?;
            self.emit(Instr::Move {
                dst: self.slot(kind),
                src: self.slot(returning),
            });
            if let Some(entry) = entry {
                self.jump_to(entry);
            } else {
                let jump = self.jump();
                self.control
                    .finalizers
                    .last_mut()
                    .expect("outer finalizer")
                    .entry_jumps
                    .push(jump);
            }
        } else {
            self.emit(Instr::Return {
                src: self.slot(payload),
            });
        }
        Ok(())
    }
}
