//! Exception and abrupt-completion lowering.
//!
//! `finally` does not use the VM's native exception stack. A pending
//! `return`, `throw`, `break`, or `continue` is an explicit completion
//! (kind + payload) routed to the finalizer and dispatched after its body.
//! `await` records both resume entries before slot allocation fills `saved_slots`.

use oxc::ast::ast::*;
use yield_vm::bytecode::{BinaryOp, CallArgs, Constant, ExceptionHandler, Instr};

use super::{Lowerer, for_each_binding_name};
use crate::emit::Reg;
use crate::error::CompileError;
use crate::ir::{AwaitSite, YieldSite};
use crate::names::builtin_index;

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

/// Which iterator protocol a loop body closes with on abrupt exit.
#[derive(Clone, Copy)]
pub(super) enum CloseMode {
    Sync,
    Async,
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
    yields: Vec<YieldSite>,
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
            yields: Vec::new(),
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

    pub(super) fn take_yields(&mut self) -> Vec<YieldSite> {
        std::mem::take(&mut self.yields)
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
            reject: result,
            fulfill_pc,
            reject_pc,
        });
        Ok(result)
    }

    /// Emit an `await` whose rejection lands on an existing block.
    ///
    /// A generator `return` entry reuses the yield's throw stub, so a rejected
    /// argument is thrown where the yield's own `throw` completion is thrown.
    fn emit_await_routing(
        &mut self,
        input: Reg,
        reject: Reg,
        reject_pc: usize,
    ) -> Result<Reg, CompileError> {
        let result = self.alloc_reg()?;
        let resume = self.control.awaits.len() as u32;
        self.emit(Instr::Await {
            dst: self.slot(result),
            input: self.slot(input),
            resume,
        });
        let fulfill_pc = self.pc();
        self.control.awaits.push(AwaitSite {
            result,
            reject,
            fulfill_pc,
            reject_pc,
        });
        Ok(result)
    }

    fn is_async_generator(&self) -> bool {
        self.is_async && self.is_generator
    }

    pub(super) fn emit_return(&mut self, value: Reg) -> Result<(), CompileError> {
        // An async generator's return completion is assimilated before the
        // finally clauses run, so a rejected value can reach a lexical catch.
        let value = if self.is_async_generator() {
            self.emit_await(value)?
        } else {
            value
        };
        if self.control.finalizers.is_empty() {
            self.emit(Instr::Return {
                src: self.slot(value),
            });
            return Ok(());
        }
        self.route_completion(Completion::Return, Some(value))?;
        Ok(())
    }

    /// Emit one generator suspension and return the register its `next` value
    /// lands in.
    ///
    /// The throw and return entries form a stub block before the operand, jumped
    /// over on the normal path. That keeps `next_pc` the fall-through of `Yield`
    /// while giving the throw completion a `Throw` the current handler can
    /// catch, and the return completion an `emit_pending_return` that crosses
    /// the current finalizer without entering a catch clause.
    pub(super) fn emit_yield(&mut self, input: Reg) -> Result<Reg, CompileError> {
        // A consumer of an async generator only ever observes the assimilated
        // value, and a rejection must reach the lexical handlers around `yield`.
        let input = if self.is_async_generator() {
            self.emit_await(input)?
        } else {
            input
        };
        let next = self.alloc_reg()?;
        let thrown = self.alloc_reg()?;
        let skip = self.jump();
        let throw_pc = self.pc();
        self.emit(Instr::Throw {
            src: self.slot(thrown),
        });
        let return_pc = self.pc();
        let payload = self.alloc_reg()?;
        self.emit(Instr::LoadGeneratorReturn {
            dst: self.slot(payload),
        });
        if self.is_async_generator() {
            // An injected `return` value is assimilated before the finally
            // clauses run; a rejection enters the same throw entry.
            let awaited = self.emit_await_routing(payload, thrown, throw_pc)?;
            self.emit_pending_return(awaited)?;
        } else {
            self.emit_pending_return(payload)?;
        }
        let yield_pc = self.pc();
        self.patch_jump(skip, yield_pc)?;
        let generator = self.control.yields.len() as u32;
        self.emit(Instr::Yield {
            input: self.slot(input),
            generator,
            delegate: false,
        });
        self.control.yields.push(YieldSite {
            next,
            thrown,
            next_pc: yield_pc + 1,
            throw_pc,
            return_pc,
        });
        Ok(next)
    }

    /// Resolve the iterator of an asynchronous iteration.
    ///
    /// Returns `(iterator, next, sync)`. `sync` is `false` on the native path
    /// and `true` on the `Symbol.iterator` fallback, and it stays an ordinary
    /// live slot for the whole loop. The async method result is assimilated and
    /// revalidated before `next` is read.
    pub(super) fn emit_async_iterator_setup(
        &mut self,
        iterable: Reg,
    ) -> Result<(Reg, Reg, Reg), CompileError> {
        let iterator = self.alloc_reg()?;
        let next = self.alloc_reg()?;
        let sync = self.alloc_reg()?;
        let symbol = self.alloc_reg()?;
        let builtin = builtin_index("Symbol").ok_or_else(|| {
            CompileError::new(
                self.source_text(),
                self.span,
                "internal compiler error: missing Symbol builtin",
            )
        })?;
        self.emit(Instr::GetBuiltin {
            dst: self.slot(symbol),
            builtin: builtin as u32,
        });
        let key = self.emit_constant(Constant::String("asyncIterator".into()))?;
        let well_known = self.alloc_reg()?;
        self.emit(Instr::GetMember {
            dst: self.slot(well_known),
            object: self.slot(symbol),
            key: self.slot(key),
        });
        let method = self.alloc_reg()?;
        self.emit(Instr::GetMember {
            dst: self.slot(method),
            object: self.slot(iterable),
            key: self.slot(well_known),
        });
        let absent = self.emit_absent_branch(method)?;
        let raw = self.alloc_reg()?;
        self.emit(Instr::CallValue {
            dst: self.slot(raw),
            callee: self.slot(method),
            args: CallArgs::Slots(Vec::new().into()),
            receiver: Some(self.slot(iterable)),
        });
        let awaited = self.emit_await(raw)?;
        self.emit(Instr::RequireObject {
            src: self.slot(awaited),
        });
        self.emit(Instr::Move {
            dst: self.slot(iterator),
            src: self.slot(awaited),
        });
        self.emit_member_read(next, iterator, "next")?;
        let native = self.emit_constant(Constant::Bool(false))?;
        self.emit(Instr::Move {
            dst: self.slot(sync),
            src: self.slot(native),
        });
        let present = self.jump();
        let fallback = self.pc();
        for jump in absent {
            self.patch_jump(jump, fallback)?;
        }
        self.emit(Instr::GetIterator {
            iterator: self.slot(iterator),
            next: self.slot(next),
            iterable: self.slot(iterable),
        });
        let fallback_flag = self.emit_constant(Constant::Bool(true))?;
        self.emit(Instr::Move {
            dst: self.slot(sync),
            src: self.slot(fallback_flag),
        });
        let join = self.pc();
        self.patch_jump(present, join)?;
        Ok((iterator, next, sync))
    }

    /// Close an asynchronous iterator, discarding every close failure.
    ///
    /// A throwing `return` getter, a rejected `await`, and a non-object awaited
    /// result all land on the instruction after the close, so the original
    /// completion proceeds unchanged.
    pub(super) fn emit_async_iterator_close(&mut self, iterator: Reg) -> Result<(), CompileError> {
        let start = self.pc();
        let method = self.alloc_reg()?;
        self.emit_member_read(method, iterator, "return")?;
        let absent = self.emit_absent_branch(method)?;
        let closed = self.alloc_reg()?;
        self.emit_async_method_call(closed, method, None, iterator)?;
        let after = self.pc();
        for jump in absent {
            self.patch_jump(jump, after)?;
        }
        let ignored = self.alloc_reg()?;
        self.control.push_handler(ExceptionHandler {
            start_pc: start as u32,
            end_pc: after as u32,
            target_pc: after as u32,
            dst: self.slot(ignored),
        });
        Ok(())
    }

    /// Emit `yield*` as an explicit delegation loop and return the
    /// expression's value register.
    ///
    /// A synchronous generator hands the delegate's result objects back
    /// unwrapped and reads `done` / `value` only for its own completion. An
    /// async generator resolves `[Symbol.asyncIterator]` first, assimilates
    /// every method result before reading the iteration record, and yields the
    /// value itself so the request machinery wraps it. A delegate without
    /// `[Symbol.asyncIterator]` falls back to `Symbol.iterator`; the flag
    /// recorded once stays a live slot across every suspension.
    ///
    /// All three completions of the single suspension dispatch to the
    /// delegate's own `throw` / `return` methods first, and only fall back to
    /// the outer throw or return when the delegate cannot handle them.
    pub(super) fn emit_yield_delegate(&mut self, iterable: Reg) -> Result<Reg, CompileError> {
        let is_async = self.is_async_generator();
        let (iterator, next, sync_flag) = if is_async {
            let (iterator, next, sync) = self.emit_async_iterator_setup(iterable)?;
            (iterator, next, Some(sync))
        } else {
            let iterator = self.alloc_reg()?;
            let next = self.alloc_reg()?;
            self.emit(Instr::GetIterator {
                iterator: self.slot(iterator),
                next: self.slot(next),
                iterable: self.slot(iterable),
            });
            (iterator, next, None)
        };
        // Also the suspension's next destination, so a consumer's next value
        // lands exactly where the delegate step reads it. Every path into the
        // loop, including the native async prologue, initializes it first.
        let sent = self.alloc_reg()?;
        self.emit(Instr::Undefined {
            dst: self.slot(sent),
        });
        let head = self.pc();
        let result = self.alloc_reg()?;
        let mut to_process = Vec::new();
        let mut step_async = None;
        if let Some(flag) = sync_flag {
            step_async = Some(self.branch_false(flag));
        }
        self.emit(Instr::IteratorCall {
            dst: self.slot(result),
            iterator: self.slot(iterator),
            method: self.slot(next),
            argument: Some(self.slot(sent)),
        });
        to_process.push(self.jump());
        if let Some(jump) = step_async {
            let step = self.pc();
            self.patch_jump(jump, step)?;
            self.emit_async_method_call(result, next, Some(sent), iterator)?;
            to_process.push(self.jump());
        }

        // throw entry: delegate to the iterator's own throw method.
        let throw_pc = self.pc();
        let thrown = self.alloc_reg()?;
        let throw_method = self.alloc_reg()?;
        self.emit_member_read(throw_method, iterator, "throw")?;
        let absent_throw = self.emit_absent_branch(throw_method)?;
        let mut throw_async = None;
        if let Some(flag) = sync_flag {
            throw_async = Some(self.branch_false(flag));
        }
        self.emit(Instr::IteratorCall {
            dst: self.slot(result),
            iterator: self.slot(iterator),
            method: self.slot(throw_method),
            argument: Some(self.slot(thrown)),
        });
        to_process.push(self.jump());
        if let Some(jump) = throw_async {
            let step = self.pc();
            self.patch_jump(jump, step)?;
            self.emit_async_method_call(result, throw_method, Some(thrown), iterator)?;
            to_process.push(self.jump());
        }
        let absent_throw_pc = self.pc();
        for jump in absent_throw {
            self.patch_jump(jump, absent_throw_pc)?;
        }
        if is_async {
            // Close the delegate with no argument first; a failing close wins
            // over the TypeError that reports the missing method.
            let close_method = self.alloc_reg()?;
            self.emit_member_read(close_method, iterator, "return")?;
            let absent_close = self.emit_absent_branch(close_method)?;
            let closed = self.alloc_reg()?;
            self.emit_async_method_call(closed, close_method, None, iterator)?;
            let absent_close_pc = self.pc();
            for jump in absent_close {
                self.patch_jump(jump, absent_close_pc)?;
            }
        } else {
            self.emit(Instr::IteratorClose {
                iterator: self.slot(iterator),
                preserve_throw: false,
            });
        }
        let error = self.emit_type_error("iterator does not have a throw method")?;
        self.emit(Instr::Throw {
            src: self.slot(error),
        });

        // return entry: delegate to the iterator's own return method.
        let return_pc = self.pc();
        let returned = self.alloc_reg()?;
        self.emit(Instr::LoadGeneratorReturn {
            dst: self.slot(returned),
        });
        let return_method = self.alloc_reg()?;
        self.emit_member_read(return_method, iterator, "return")?;
        let absent_return = self.emit_absent_branch(return_method)?;
        let mut return_async = None;
        if let Some(flag) = sync_flag {
            return_async = Some(self.branch_false(flag));
        }
        self.emit(Instr::IteratorCall {
            dst: self.slot(result),
            iterator: self.slot(iterator),
            method: self.slot(return_method),
            argument: Some(self.slot(returned)),
        });
        let mut to_check_return = vec![self.jump()];
        if let Some(jump) = return_async {
            let step = self.pc();
            self.patch_jump(jump, step)?;
            self.emit_async_method_call(result, return_method, Some(returned), iterator)?;
            to_check_return.push(self.jump());
        }
        let absent_return_pc = self.pc();
        for jump in absent_return {
            self.patch_jump(jump, absent_return_pc)?;
        }
        // No return method: the incoming return completion propagates outward.
        self.emit_pending_return(returned)?;

        // next and throw completions share the done/value reads.
        let process = self.pc();
        for jump in to_process {
            self.patch_jump(jump, process)?;
        }
        let done = self.alloc_reg()?;
        self.emit_member_read(done, result, "done")?;
        let value = self.alloc_reg()?;
        // An async generator yields the value itself, so the not-done path
        // reads it too. A synchronous one hands the result object back and only
        // needs the completion value.
        let not_done = if is_async {
            self.emit_member_read(value, result, "value")?;
            self.branch_false(done)
        } else {
            let branch = self.branch_false(done);
            self.emit_member_read(value, result, "value")?;
            branch
        };
        let finish = self.jump();

        let yield_pc = self.pc();
        self.patch_jump(not_done, yield_pc)?;
        let generator = self.control.yields.len() as u32;
        if let Some(flag) = sync_flag {
            // A synchronous delegate's value is assimilated before it is
            // yielded; a native async iterator already produced the value.
            let native = self.branch_false(flag);
            let awaited = self.emit_await(value)?;
            self.emit(Instr::Move {
                dst: self.slot(value),
                src: self.slot(awaited),
            });
            let present = self.pc();
            self.patch_jump(native, present)?;
        }
        let yield_instruction = self.pc();
        if is_async {
            self.emit(Instr::Yield {
                input: self.slot(value),
                generator,
                delegate: false,
            });
        } else {
            self.emit(Instr::Yield {
                input: self.slot(result),
                generator,
                delegate: true,
            });
        }
        self.jump_to(head);
        self.control.yields.push(YieldSite {
            next: sent,
            thrown,
            next_pc: yield_instruction + 1,
            throw_pc,
            return_pc,
        });

        // A delegated return that is not done keeps the loop running.
        let check_return = self.pc();
        for jump in to_check_return {
            self.patch_jump(jump, check_return)?;
        }
        let return_done = self.alloc_reg()?;
        self.emit_member_read(return_done, result, "done")?;
        let return_not_done = self.branch_false(return_done);
        let return_value = self.alloc_reg()?;
        self.emit_member_read(return_value, result, "value")?;
        self.emit_pending_return(return_value)?;
        let resume_loop = self.pc();
        self.patch_jump(return_not_done, resume_loop)?;
        self.emit(Instr::Undefined {
            dst: self.slot(sent),
        });
        self.jump_to(head);

        let after = self.pc();
        self.patch_jump(finish, after)?;
        Ok(value)
    }

    /// Call one iterator method, assimilate its result, and revalidate the
    /// object the asynchronous iterator protocol requires.
    pub(super) fn emit_async_method_call(
        &mut self,
        dst: Reg,
        method: Reg,
        argument: Option<Reg>,
        iterator: Reg,
    ) -> Result<(), CompileError> {
        let raw = self.alloc_reg()?;
        self.emit(Instr::CallValue {
            dst: self.slot(raw),
            callee: self.slot(method),
            args: match argument {
                Some(argument) => CallArgs::Slots(vec![self.slot(argument)].into()),
                None => CallArgs::Slots(Vec::new().into()),
            },
            receiver: Some(self.slot(iterator)),
        });
        let awaited = self.emit_await(raw)?;
        self.emit(Instr::RequireObject {
            src: self.slot(awaited),
        });
        self.emit(Instr::Move {
            dst: self.slot(dst),
            src: self.slot(awaited),
        });
        Ok(())
    }

    /// Read a fixed own-or-inherited property from `object`.
    pub(super) fn emit_member_read(
        &mut self,
        dst: Reg,
        object: Reg,
        name: &str,
    ) -> Result<(), CompileError> {
        let key = self.emit_constant(Constant::String(name.into()))?;
        self.emit(Instr::GetMember {
            dst: self.slot(dst),
            object: self.slot(object),
            key: self.slot(key),
        });
        Ok(())
    }

    /// Branch on a missing method: `value` is `undefined` or `null`.
    ///
    /// Returns the jumps taken when the method is absent; the fall-through is
    /// the present path, which must validate callability.
    fn emit_absent_branch(&mut self, value: Reg) -> Result<Vec<usize>, CompileError> {
        let mut absent = Vec::new();
        let undefined = self.emit_undefined()?;
        let is_undefined = self.alloc_reg()?;
        self.emit(Instr::Binary {
            dst: self.slot(is_undefined),
            op: BinaryOp::StrictEq,
            lhs: self.slot(value),
            rhs: self.slot(undefined),
        });
        let not_undefined = self.branch_false(is_undefined);
        absent.push(self.jump());
        let present = self.pc();
        self.patch_jump(not_undefined, present)?;
        let null = self.emit_constant(Constant::Null)?;
        let is_null = self.alloc_reg()?;
        self.emit(Instr::Binary {
            dst: self.slot(is_null),
            op: BinaryOp::StrictEq,
            lhs: self.slot(value),
            rhs: self.slot(null),
        });
        let not_null = self.branch_false(is_null);
        absent.push(self.jump());
        let present = self.pc();
        self.patch_jump(not_null, present)?;
        Ok(absent)
    }

    /// Build the internal `TypeError` a delegation fallback throws.
    fn emit_type_error(&mut self, message: &str) -> Result<Reg, CompileError> {
        let builtin = builtin_index("TypeError").ok_or_else(|| {
            CompileError::internal("internal compiler error: no TypeError builtin")
        })?;
        let message = self.emit_constant(Constant::String(message.into()))?;
        let callee = self.alloc_reg()?;
        self.emit(Instr::GetBuiltin {
            dst: self.slot(callee),
            builtin: builtin as u32,
        });
        let error = self.alloc_reg()?;
        self.emit(Instr::CallValue {
            dst: self.slot(error),
            callee: self.slot(callee),
            args: CallArgs::Slots(vec![self.slot(message)].into()),
            receiver: None,
        });
        Ok(error)
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
        close: CloseMode,
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
        self.emit_iterator_close(iterator, close, true)?;
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
        self.emit_iterator_close(iterator, close, false)?;
        self.dispatch_finally(&context)?;
        Ok(self.control.pop_loop())
    }

    /// Close `iterator` for the loop's protocol, preserving a pending throw
    /// when it is the original abrupt completion.
    fn emit_iterator_close(
        &mut self,
        iterator: Reg,
        close: CloseMode,
        preserve_throw: bool,
    ) -> Result<(), CompileError> {
        match close {
            CloseMode::Sync => {
                self.emit(Instr::IteratorClose {
                    iterator: self.slot(iterator),
                    preserve_throw,
                });
            }
            CloseMode::Async => self.emit_async_iterator_close(iterator)?,
        }
        Ok(())
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

    pub(super) fn emit_pending_return(&mut self, payload: Reg) -> Result<(), CompileError> {
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
