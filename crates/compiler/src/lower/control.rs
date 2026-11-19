//! Exception and abrupt-completion lowering.
//!
//! `finally` does not use the VM's native exception stack. A pending
//! `return` or `throw` is an explicit completion (kind + payload) routed to
//! the finalizer, then dispatched after the finalizer body. `await` records
//! both resume entries before slot allocation fills `saved_slots`.

use oxc::ast::ast::*;
use yield_vm::bytecode::{ExceptionHandler, Instr};

use super::Lowerer;
use crate::emit::Reg;
use crate::error::{CompileError, unsupported};
use crate::ir::AwaitSite;

#[derive(Clone, Copy)]
pub(super) enum Completion {
    Normal,
    Return,
    Throw,
}

impl Completion {
    fn value(self) -> f64 {
        match self {
            Self::Normal => 0.0,
            Self::Return => 1.0,
            Self::Throw => 2.0,
        }
    }
}

pub(super) struct Loop {
    pub(super) break_jumps: Vec<usize>,
    pub(super) continue_jumps: Vec<usize>,
    pub(super) continue_target: Option<usize>,
    pub(super) finalizer_depth: usize,
}

struct Finalizer {
    kind: Reg,
    payload: Reg,
    entry: Option<usize>,
    entry_jumps: Vec<usize>,
}

pub(super) struct Control {
    loops: Vec<Loop>,
    finalizers: Vec<Finalizer>,
    handlers: Vec<ExceptionHandler>,
    awaits: Vec<AwaitSite>,
}

impl Control {
    pub(super) fn new() -> Self {
        Self {
            loops: Vec::new(),
            finalizers: Vec::new(),
            handlers: Vec::new(),
            awaits: Vec::new(),
        }
    }

    pub(super) fn finalizers_finished(&self) -> bool {
        self.finalizers.is_empty()
    }

    pub(super) fn finalizer_depth(&self) -> usize {
        self.finalizers.len()
    }

    pub(super) fn loop_finalizer_depth(&self) -> Option<usize> {
        self.loops
            .last()
            .map(|loop_context| loop_context.finalizer_depth)
    }

    pub(super) fn innermost_continue(&self) -> Option<(Option<usize>, usize)> {
        self.loops
            .last()
            .map(|loop_context| (loop_context.continue_target, loop_context.finalizer_depth))
    }

    pub(super) fn push_loop(&mut self, continue_target: Option<usize>) {
        self.loops.push(Loop {
            break_jumps: Vec::new(),
            continue_jumps: Vec::new(),
            continue_target,
            finalizer_depth: self.finalizers.len(),
        });
    }

    pub(super) fn pop_loop(&mut self) -> Loop {
        self.loops.pop().expect("loop stack")
    }

    pub(super) fn push_break(&mut self, jump: usize) {
        self.loops
            .last_mut()
            .expect("loop stack checked above")
            .break_jumps
            .push(jump);
    }

    pub(super) fn push_continue(&mut self, jump: usize) {
        self.loops
            .last_mut()
            .expect("loop stack checked above")
            .continue_jumps
            .push(jump);
    }

    pub(super) fn take_awaits(&mut self) -> Vec<AwaitSite> {
        std::mem::take(&mut self.awaits)
    }

    pub(super) fn take_handlers(&mut self) -> Vec<ExceptionHandler> {
        std::mem::take(&mut self.handlers)
    }

    fn push_handler(&mut self, handler: ExceptionHandler) {
        self.handlers.push(handler);
    }
}

impl<'ast, 'borrow, 'src> Lowerer<'ast, 'borrow, 'src> {
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

    pub(super) fn compile_try(
        &mut self,
        statement: &'ast TryStatement<'ast>,
    ) -> Result<(), CompileError> {
        let has_finalizer = statement.finalizer.is_some();
        let exception = self.alloc_reg()?;
        if has_finalizer {
            let kind = self.alloc_reg()?;
            let payload = self.alloc_reg()?;
            self.control.finalizers.push(Finalizer {
                kind,
                payload,
                entry: None,
                entry_jumps: Vec::new(),
            });
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
            let catch_stub = self.pc();
            self.env.enter_block();
            if let Some(parameter) = &handler.param {
                let BindingPattern::BindingIdentifier(identifier) = &parameter.pattern else {
                    return Err(unsupported(
                        self.source_text(),
                        parameter.span,
                        "catch parameter destructuring",
                    ));
                };
                let (binding, _) = self.declare(
                    identifier.name.as_str(),
                    VariableDeclarationKind::Let,
                    parameter.span,
                )?;
                self.emit(Instr::DeclareBinding { binding });
                self.emit(Instr::SetBinding {
                    binding,
                    src: self.slot(exception),
                });
            }
            let catch_start = self.pc();
            self.predeclare_scope(&handler.body.body)?;
            for statement in &handler.body.body {
                self.compile_statement(statement)?;
            }
            if has_finalizer {
                self.route_completion(Completion::Normal, None)?;
            } else {
                end_jumps.push(self.jump());
            }
            let catch_region_end = self.pc();
            self.env.exit_block();
            catch_body = Some((catch_stub, catch_start, catch_region_end));
        }

        if has_finalizer {
            // Stop routing returns from the finalizer to itself.
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

            if let Some((catch_stub, catch_start, catch_region_end)) = catch_body {
                self.control.push_handler(ExceptionHandler {
                    start_pc: catch_start as u32,
                    end_pc: catch_region_end as u32,
                    target_pc: throw_stub as u32,
                    dst: self.slot(exception),
                });
                self.control.push_handler(ExceptionHandler {
                    start_pc: try_start as u32,
                    end_pc: try_end as u32,
                    target_pc: catch_stub as u32,
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
            if let Some((catch_stub, _, _)) = catch_body {
                self.control.push_handler(ExceptionHandler {
                    start_pc: try_start as u32,
                    end_pc: try_end as u32,
                    target_pc: catch_stub as u32,
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
        let throw_pc = self.pc();
        self.patch_jump(not_return, throw_pc)?;
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
