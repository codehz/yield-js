//! Statement lowering. Control-flow constructs record jumps on [`Control`];
//! they do not patch slots or allocate bindings themselves.

use oxc::ast::ast::*;
use oxc::span::GetSpan;
use yield_vm::BindingId;
use yield_vm::bytecode::{BinaryOp, Instr};

use super::control::{CloseMode, JumpTarget};
use super::{FunctionSource, Lowerer, for_each_binding_name, local_declaration};
use crate::emit::Reg;
use crate::error::{CompileError, unsupported};
use crate::ir::IterationEnvGroup;

impl<'ast, 'borrow, 'src> Lowerer<'ast, 'borrow, 'src> {
    pub(super) fn compile_statement<'node>(
        &mut self,
        statement: &'node Statement<'node>,
    ) -> Result<(), CompileError>
    where
        'node: 'ast,
    {
        if let Some(declaration) = local_declaration(statement) {
            return self.compile_declaration(declaration);
        }
        match statement {
            Statement::BlockStatement(block) => self.compile_block(block),
            Statement::EmptyStatement(_) => Ok(()),
            Statement::ExpressionStatement(statement) => {
                self.compile_expression(&statement.expression)?;
                Ok(())
            }
            Statement::ReturnStatement(statement) => {
                let value = match &statement.argument {
                    Some(expression) => self.compile_expression(expression)?,
                    None => self.emit_undefined()?,
                };
                self.emit_return(value)
            }
            Statement::ThrowStatement(statement) => {
                let value = self.compile_expression(&statement.argument)?;
                self.emit(Instr::Throw {
                    src: self.slot(value),
                });
                Ok(())
            }
            Statement::IfStatement(statement) => {
                if let Some(test) = self.guard_truth(&statement.test) {
                    return if test {
                        self.compile_statement(&statement.consequent)
                    } else if let Some(alternate) = &statement.alternate {
                        self.compile_statement(alternate)
                    } else {
                        Ok(())
                    };
                }
                let branch = self.compile_condition(&statement.test)?;
                self.compile_statement(&statement.consequent)?;
                let end = self.jump();
                let alternate = self.pc();
                self.patch_jump(branch, alternate)?;
                if let Some(alternate_statement) = &statement.alternate {
                    self.compile_statement(alternate_statement)?;
                }
                let end_target = self.pc();
                self.patch_jump(end, end_target)?;
                Ok(())
            }
            Statement::WhileStatement(statement) => {
                let start = self.emit.loop_header();
                let exit_branch = self.compile_condition(&statement.test)?;
                self.control.push_loop(Some(start));
                self.compile_statement(&statement.body)?;
                let loop_context = self.control.pop_loop();
                self.jump_to(start);
                let end = self.pc();
                self.patch_jump(exit_branch, end)?;
                self.patch_loop(loop_context, end)
            }
            Statement::DoWhileStatement(statement) => {
                let start = self.emit.loop_header();
                self.control.push_loop(None);
                self.compile_statement(&statement.body)?;
                let test_pc = self.pc();
                let mut loop_context = self.control.pop_loop();
                for jump in loop_context.continue_jumps.drain(..) {
                    self.patch_jump(jump, test_pc)?;
                }
                self.compile_condition(&statement.test)?;
                let back = self.pc();
                self.jump_to(start);
                let end = self.pc();
                let exit_branch = back - 1;
                self.patch_jump(exit_branch, end)?;
                loop_context.continue_target = Some(test_pc);
                self.patch_loop(loop_context, end)
            }
            Statement::ForStatement(statement) => {
                self.env.enter_block();
                if let Some(ForStatementInit::VariableDeclaration(declaration)) = &statement.init {
                    self.predeclare_variable_list(declaration)?;
                }
                if let Some(init) = &statement.init {
                    match init {
                        ForStatementInit::VariableDeclaration(declaration) => {
                            self.compile_variable_declaration(declaration)?
                        }
                        other => {
                            if let Some(expression) = other.as_expression() {
                                self.compile_expression(expression)?;
                            } else {
                                return Err(unsupported(
                                    self.source_text(),
                                    other.span(),
                                    "for-loop initializer",
                                ));
                            }
                        }
                    }
                }
                let bindings = self.for_init_bindings(&statement.init)?;
                let saved = self.save_iteration_env(&bindings)?;
                if let Some(saved) = saved {
                    self.enter_iteration_env(saved);
                }
                let test_pc = self.emit.loop_header();
                let exit_branch = if let Some(test) = &statement.test {
                    Some(self.compile_condition(test)?)
                } else {
                    None
                };
                self.control.push_loop(None);
                self.compile_statement(&statement.body)?;
                let update_pc = self.pc();
                let mut loop_context = self.control.pop_loop();
                for jump in loop_context.continue_jumps.drain(..) {
                    self.patch_jump(jump, update_pc)?;
                }
                // The fresh iteration environment is created before the
                // increment, so closures created in the body keep the value
                // the body observed while the update mutates the next copy.
                if let Some(saved) = saved {
                    self.enter_iteration_env(saved);
                }
                if let Some(update) = &statement.update {
                    self.compile_expression(update)?;
                }
                self.jump_to(test_pc);
                let end = self.pc();
                if let Some(exit_branch) = exit_branch {
                    self.patch_jump(exit_branch, end)?;
                }
                loop_context.continue_target = Some(update_pc);
                self.patch_loop(loop_context, end)?;
                if let Some(saved) = saved {
                    self.leave_iteration_env(saved);
                }
                self.env.exit_block();
                Ok(())
            }
            Statement::BreakStatement(statement) => self.compile_break(statement),
            Statement::ContinueStatement(statement) => self.compile_continue(statement),
            Statement::DebuggerStatement(statement) => Err(unsupported(
                self.source_text(),
                statement.span,
                "debugger statement",
            )),
            Statement::WithStatement(statement) => Err(unsupported(
                self.source_text(),
                statement.span,
                "with statement",
            )),
            Statement::SwitchStatement(statement) => self.compile_switch(statement),
            Statement::TryStatement(statement) => self.compile_try(statement),
            Statement::ForInStatement(statement) => self.compile_for_in(statement),
            Statement::ForOfStatement(statement) => self.compile_for_of(statement),
            Statement::LabeledStatement(statement) => self.compile_labeled(statement),
            Statement::ExportNamedDeclaration(_) => Ok(()),
            Statement::ExportDefaultDeclaration(export) => self.compile_export_default(export),
            Statement::ExportFromDeclaration(_)
            | Statement::ExportAllDeclaration(_)
            | Statement::ImportDeclaration(_) => Ok(()),
            other => Err(unsupported(
                self.source_text(),
                other.span(),
                "top-level/module statement",
            )),
        }
    }

    fn compile_declaration(
        &mut self,
        declaration: &'ast Declaration<'ast>,
    ) -> Result<(), CompileError> {
        match declaration {
            Declaration::VariableDeclaration(declaration) => {
                self.compile_variable_declaration(declaration)
            }
            Declaration::FunctionDeclaration(function) => {
                if self.hoisted.contains(&function.span.start) {
                    return Ok(());
                }
                let name = super::function_name(self.source_text(), function)?;
                let (binding, _) =
                    self.declare(&name, VariableDeclarationKind::Let, function.span)?;
                let func_id = self.enqueue_function(
                    FunctionSource::Function(function),
                    &name,
                    function.span,
                )?;
                self.init_function_binding(binding, func_id)
            }
            Declaration::ClassDeclaration(class) => self.compile_class_declaration(class),
            other => Err(unsupported(
                self.source_text(),
                other.span(),
                "top-level/module statement",
            )),
        }
    }

    /// `export default <expression>` and anonymous declaration forms. The
    /// public slot was registered during export collection; this evaluates the
    /// value once at the original statement position.
    fn compile_export_default(
        &mut self,
        export: &'ast ExportDefaultDeclaration<'ast>,
    ) -> Result<(), CompileError> {
        match &export.declaration {
            ExportDefaultDeclarationKind::FunctionDeclaration(_) => {
                // A named default function was hoisted and initialized at
                // module entry; an anonymous one was initialized during export
                // collection. Neither runs here.
                return Ok(());
            }
            ExportDefaultDeclarationKind::ClassDeclaration(class) if class.id.is_some() => {
                // The public name aliases the local class binding, so
                // initializing that binding at the statement publishes the
                // default as well.
                self.compile_class_declaration(class)?;
                return Ok(());
            }
            ExportDefaultDeclarationKind::TSInterfaceDeclaration(_) => {
                return Err(unsupported(
                    self.source_text(),
                    export.span,
                    "TypeScript interface",
                ));
            }
            _ => {}
        };
        let Some(binding) = self.default_binding else {
            return Err(CompileError::internal(
                "internal compiler error: default export slot was not registered",
            ));
        };
        let value = match &export.declaration {
            ExportDefaultDeclarationKind::ClassDeclaration(class) => {
                // A named class initializes its local binding here, exactly as
                // an ordinary class declaration would; the public slot then
                // mirrors that value. An anonymous class is compiled here as
                // an expression named "default".
                match &class.id {
                    Some(identifier) => {
                        self.compile_class_declaration(class)?;
                        let name = identifier.name.as_str();
                        let local = self.lookup(name).ok_or_else(|| {
                            CompileError::internal(format!(
                                "internal compiler error: default class {name:?} was not declared"
                            ))
                        })?;
                        let value = self.alloc_reg()?;
                        self.emit_binding_read(value, local);
                        value
                    }
                    None => self.compile_class(class, "default", None)?,
                }
            }
            _ => {
                let expression = export
                    .declaration
                    .as_expression()
                    .expect("expression form checked at export collection");
                self.compile_named_expression(expression, "default")?
            }
        };
        self.emit(Instr::SetBinding {
            binding,
            src: self.slot(value),
        });
        Ok(())
    }

    fn compile_for_in(
        &mut self,
        statement: &'ast ForInStatement<'ast>,
    ) -> Result<(), CompileError> {
        self.env.enter_block();
        let bindings = self.declare_iteration_bindings(&statement.left, "for-in")?;
        let source = self.compile_expression(&statement.right)?;

        let iterator = self.alloc_reg()?;
        self.emit(Instr::GetForInIterator {
            dst: self.slot(iterator),
            source: self.slot(source),
        });
        let saved = self.save_iteration_env(&bindings)?;
        let step_pc = self.pc();
        if let Some(saved) = saved {
            self.enter_iteration_env(saved);
        }
        let done = self.alloc_reg()?;
        let value = self.alloc_reg()?;
        self.emit(Instr::ForInNext {
            done: self.slot(done),
            value: self.slot(value),
            iterator: self.slot(iterator),
        });
        let has_value = self.branch_false(done);
        let exhausted = self.jump();
        self.patch_jump(has_value, self.pc())?;

        // Enumeration has no user-visible iterator to close on abrupt exit.
        self.control.push_loop(Some(step_pc));
        self.assign_iteration_value(&statement.left, value)?;
        self.compile_statement(&statement.body)?;
        let loop_context = self.control.pop_loop();
        self.jump_to(step_pc);
        let end = self.pc();
        self.patch_jump(exhausted, end)?;
        self.patch_loop(loop_context, end)?;
        if let Some(saved) = saved {
            self.leave_iteration_env(saved);
        }
        self.env.exit_block();
        Ok(())
    }

    fn compile_for_of(
        &mut self,
        statement: &'ast ForOfStatement<'ast>,
    ) -> Result<(), CompileError> {
        if statement.r#await {
            return self.compile_for_await_of(statement);
        }
        self.env.enter_block();
        let bindings = self.declare_iteration_bindings(&statement.left, "for-of")?;

        let iterable = self.compile_expression(&statement.right)?;
        let iterator = self.alloc_reg()?;
        let next = self.alloc_reg()?;
        self.emit(Instr::GetIterator {
            iterator: self.slot(iterator),
            next: self.slot(next),
            iterable: self.slot(iterable),
        });
        let saved = self.save_iteration_env(&bindings)?;
        let step_pc = self.pc();
        if let Some(saved) = saved {
            self.enter_iteration_env(saved);
        }
        let done = self.alloc_reg()?;
        let value = self.alloc_reg()?;
        self.emit(Instr::IteratorNext {
            done: self.slot(done),
            value: self.slot(value),
            iterator: self.slot(iterator),
            next: self.slot(next),
        });
        let has_value = self.branch_false(done);
        let exhausted = self.jump();
        self.patch_jump(has_value, self.pc())?;
        let loop_context =
            self.compile_iterator_body(iterator, step_pc, CloseMode::Sync, |this| {
                this.assign_iteration_value(&statement.left, value)?;
                this.compile_statement(&statement.body)
            })?;

        let end = self.pc();
        self.patch_jump(exhausted, end)?;
        self.patch_loop(loop_context, end)?;
        if let Some(saved) = saved {
            self.leave_iteration_env(saved);
        }
        self.env.exit_block();
        Ok(())
    }

    /// `for await…of`: the asynchronous iterator protocol, expanded inline.
    ///
    /// The head awaits every native step and falls back to `Symbol.iterator`
    /// for a missing `[Symbol.asyncIterator]`, awaiting only each iteration's
    /// `value` there. The cached `next`, the iterator, and the fallback flag are
    /// ordinary live slots across suspension, so restore keeps them.
    fn compile_for_await_of(
        &mut self,
        statement: &'ast ForOfStatement<'ast>,
    ) -> Result<(), CompileError> {
        if self.is_script {
            return Err(unsupported(
                self.source_text(),
                statement.span,
                "top-level await",
            ));
        }
        if !self.is_async {
            return Err(unsupported(
                self.source_text(),
                statement.span,
                "for-await-of outside async function",
            ));
        }
        self.env.enter_block();
        let bindings = self.declare_iteration_bindings(&statement.left, "for-await-of")?;

        let iterable = self.compile_expression(&statement.right)?;
        let (iterator, next, sync) = self.emit_async_iterator_setup(iterable)?;

        let saved = self.save_iteration_env(&bindings)?;
        let step_pc = self.pc();
        if let Some(saved) = saved {
            self.enter_iteration_env(saved);
        }
        let result = self.alloc_reg()?;
        let async_step = self.branch_false(sync);
        self.emit(Instr::IteratorCall {
            dst: self.slot(result),
            iterator: self.slot(iterator),
            method: self.slot(next),
            argument: None,
        });
        let joined = self.jump();
        self.patch_jump(async_step, self.pc())?;
        self.emit_async_method_call(result, next, None, iterator)?;
        self.patch_jump(joined, self.pc())?;

        let done = self.alloc_reg()?;
        self.emit_member_read(done, result, "done")?;
        let not_done = self.branch_false(done);
        let exhausted = self.jump();
        self.patch_jump(not_done, self.pc())?;
        let value = self.alloc_reg()?;
        self.emit_member_read(value, result, "value")?;
        let native = self.branch_false(sync);
        let awaited_value = self.emit_await(value)?;
        self.emit(Instr::Move {
            dst: self.slot(value),
            src: self.slot(awaited_value),
        });
        self.patch_jump(native, self.pc())?;

        let loop_context =
            self.compile_iterator_body(iterator, step_pc, CloseMode::Async, |this| {
                this.assign_iteration_value(&statement.left, value)?;
                this.compile_statement(&statement.body)
            })?;

        let end = self.pc();
        self.patch_jump(exhausted, end)?;
        self.patch_loop(loop_context, end)?;
        if let Some(saved) = saved {
            self.leave_iteration_env(saved);
        }
        self.env.exit_block();
        Ok(())
    }

    fn declare_iteration_bindings(
        &mut self,
        target: &'ast ForStatementLeft<'ast>,
        loop_kind: &str,
    ) -> Result<Vec<BindingId>, CompileError> {
        let ForStatementLeft::VariableDeclaration(declaration) = target else {
            return Ok(Vec::new());
        };
        let [declarator] = declaration.declarations.as_slice() else {
            return Err(unsupported(
                self.source_text(),
                declaration.span,
                &format!("{loop_kind} declaration"),
            ));
        };
        if let Some(initializer) = &declarator.init {
            return Err(unsupported(
                self.source_text(),
                initializer.span(),
                &format!("{loop_kind} initializer"),
            ));
        }
        self.predeclare_variable_list(declaration)?;
        // `var` exists even when the iteration yields nothing.
        if declaration.kind == VariableDeclarationKind::Var {
            for_each_binding_name(&declarator.id, &mut |identifier| {
                let (binding, _) =
                    self.declare(identifier.name.as_str(), declaration.kind, identifier.span)?;
                if !self.is_script {
                    self.emit(Instr::DeclareBinding { binding });
                }
                Ok::<(), CompileError>(())
            })?;
            return Ok(Vec::new());
        }
        let mut bindings = Vec::new();
        for_each_binding_name(&declarator.id, &mut |identifier| {
            if let Some(binding) = self.env.current_binding(identifier.name.as_str()) {
                bindings.push(binding);
            }
            Ok::<(), CompileError>(())
        })?;
        Ok(bindings)
    }

    /// Save the outer environment once before a per-iteration loop. `None`
    /// when the loop needs no fresh binding environment.
    fn save_iteration_env(
        &mut self,
        bindings: &[BindingId],
    ) -> Result<Option<usize>, CompileError> {
        if bindings.is_empty() {
            return Ok(None);
        }
        let saved_env = self.alloc_reg()?;
        let save_pc = self.pc();
        self.emit(Instr::SaveEnv {
            dst: self.slot(saved_env),
        });
        let group = self.iteration_envs.len();
        self.iteration_envs.push(IterationEnvGroup {
            bindings: bindings.to_vec(),
            saved_env,
            save_pc,
            enter_pcs: Vec::new(),
            restore_pc: None,
        });
        Ok(Some(group))
    }

    fn enter_iteration_env(&mut self, group: usize) {
        let saved_env = self.iteration_envs[group].saved_env;
        let bindings = self.iteration_envs[group].bindings.clone();
        let enter_pc = self.pc();
        self.emit(Instr::NewIterationEnv {
            parent: self.slot(saved_env),
            bindings,
        });
        self.iteration_envs[group].enter_pcs.push(enter_pc);
    }

    fn leave_iteration_env(&mut self, group: usize) {
        let saved_env = self.iteration_envs[group].saved_env;
        let restore_pc = self.pc();
        self.emit(Instr::RestoreEnv {
            src: self.slot(saved_env),
        });
        self.iteration_envs[group].restore_pc = Some(restore_pc);
    }

    /// Loop bindings declared by a C-style `for` initializer. `var` stays
    /// function-scoped and is shared by every iteration, so it is excluded.
    fn for_init_bindings(
        &mut self,
        init: &Option<ForStatementInit<'ast>>,
    ) -> Result<Vec<BindingId>, CompileError> {
        let Some(ForStatementInit::VariableDeclaration(declaration)) = init else {
            return Ok(Vec::new());
        };
        if declaration.kind == VariableDeclarationKind::Var {
            return Ok(Vec::new());
        }
        let mut bindings = Vec::new();
        for declarator in &declaration.declarations {
            for_each_binding_name(&declarator.id, &mut |identifier| {
                if let Some(binding) = self.env.current_binding(identifier.name.as_str()) {
                    bindings.push(binding);
                }
                Ok::<(), CompileError>(())
            })?;
        }
        Ok(bindings)
    }

    fn assign_iteration_value(
        &mut self,
        target: &'ast ForStatementLeft<'ast>,
        value: Reg,
    ) -> Result<(), CompileError> {
        if let ForStatementLeft::VariableDeclaration(declaration) = target {
            let declarator = &declaration.declarations[0];
            self.compile_binding_pattern(&declarator.id, value)
        } else {
            self.compile_assignment_pattern(target.to_assignment_target(), value)
        }
    }

    /// Label chain lowering. A chain ending in a loop binds its names to the
    /// loop (for `continue`); any other terminal makes them plain `break` targets.
    /// Active label names must be unique. The terminal
    /// statement is compiled once; after it, every break jump recorded on the
    /// chain's frames targets the instruction after it, which is exactly the
    /// loop exit for loop labels.
    fn compile_labeled<'node>(
        &mut self,
        statement: &'node LabeledStatement<'node>,
    ) -> Result<(), CompileError>
    where
        'node: 'ast,
    {
        let mut chain = Vec::new();
        let mut current = statement;
        loop {
            chain.push(current);
            match &current.body {
                Statement::LabeledStatement(inner) => current = inner,
                other => {
                    let loop_index = match other {
                        Statement::WhileStatement(_)
                        | Statement::DoWhileStatement(_)
                        | Statement::ForStatement(_)
                        | Statement::ForInStatement(_)
                        | Statement::ForOfStatement(_) => Some(self.control.loops_len()),
                        _ => None,
                    };
                    let base = self.control.labels_len();
                    for labelled in chain {
                        let name = labelled.label.name.as_str();
                        if self.control.find_label(name).is_some() {
                            return Err(CompileError::new(
                                self.source_text(),
                                labelled.label.span(),
                                "duplicate label",
                            ));
                        }
                        self.control.push_label(name, loop_index);
                    }
                    match self.compile_statement(&current.body) {
                        Ok(()) => {
                            let end = self.pc();
                            for frame in self.control.pop_labels_to(base) {
                                for jump in frame.break_jumps {
                                    self.patch_jump(jump, end)?;
                                }
                            }
                            return Ok(());
                        }
                        Err(error) => {
                            self.control.pop_labels_to(base);
                            return Err(error);
                        }
                    }
                }
            }
        }
    }

    fn compile_break(&mut self, statement: &BreakStatement<'_>) -> Result<(), CompileError> {
        if let Some(label) = &statement.label {
            let Some(index) = self.control.find_label(label.name.as_str()) else {
                return Err(CompileError::new(
                    self.source_text(),
                    label.span(),
                    "unknown label",
                ));
            };
            let depth = self.control.label_finalizer_depth(index);
            return self.emit_control_jump(JumpTarget::LabelBreak(index), depth);
        }
        let Some((target, depth)) = self.control.innermost_break() else {
            return Err(CompileError::new(
                self.source_text(),
                statement.span,
                "break outside loop",
            ));
        };
        self.emit_control_jump(target, depth)
    }

    /// Non-labelled `switch` lowering: strict-equality test chain in source
    /// order, then the bodies, then patching. Fall-through is sequential
    /// emission; all cases share one block scope.
    fn compile_switch(
        &mut self,
        statement: &'ast SwitchStatement<'ast>,
    ) -> Result<(), CompileError> {
        let disc = self.compile_expression(&statement.discriminant)?;
        self.env.enter_block();
        for case in &statement.cases {
            self.predeclare_scope(&case.consequent)?;
        }
        self.control.push_switch();

        // Test chain: (branch_to_next_test, jump_to_body, case index).
        let mut tests: Vec<(usize, usize, usize)> = Vec::new();
        let mut default_index: Option<usize> = None;
        for (index, case) in statement.cases.iter().enumerate() {
            let Some(test) = &case.test else {
                if default_index.is_some() {
                    return Err(unsupported(
                        self.source_text(),
                        case.span,
                        "duplicate default clause",
                    ));
                }
                default_index = Some(index);
                continue;
            };
            let test_reg = self.compile_expression(test)?;
            let branch = self.branch_compare_false(disc, BinaryOp::StrictEq, test_reg);
            let entry = self.jump();
            tests.push((branch, entry, index));
        }
        let default_jump = self.jump();

        // Bodies in source order. A case with statements records the PC of
        // its first instruction as its body entry.
        let mut body_pcs: Vec<Option<usize>> = vec![None; statement.cases.len()];
        for (index, case) in statement.cases.iter().enumerate() {
            if !case.consequent.is_empty() {
                body_pcs[index] = Some(self.pc());
            }
            for inner in &case.consequent {
                self.compile_statement(inner)?;
            }
        }
        let end = self.pc();

        // An empty consequent falls through: its body entry is the next case
        // with statements, or `end`.
        let mut next_pc = end;
        for slot in body_pcs.iter_mut().rev() {
            if let Some(existing) = *slot {
                next_pc = existing;
            } else {
                *slot = Some(next_pc);
            }
        }

        let body_pc = |index: usize| body_pcs[index].expect("filled above");
        // A false branch falls to the next test's first instruction, which is
        // the instruction right after this segment's entry jump. For the last
        // test that is exactly `default_jump`.
        for (_, entry, _) in &tests {
            self.patch_jump(*entry - 1, *entry + 1)?;
        }
        for (_, entry, index) in tests {
            self.patch_jump(entry, body_pc(index))?;
        }
        match default_index {
            Some(index) => self.patch_jump(default_jump, body_pc(index))?,
            None => self.patch_jump(default_jump, end)?,
        }
        let switch_context = self.control.pop_switch();
        for jump in switch_context.break_jumps {
            self.patch_jump(jump, end)?;
        }
        self.env.exit_block();
        Ok(())
    }

    fn compile_continue(&mut self, statement: &ContinueStatement<'_>) -> Result<(), CompileError> {
        if let Some(label) = &statement.label {
            let Some(index) = self.control.find_label(label.name.as_str()) else {
                return Err(CompileError::new(
                    self.source_text(),
                    label.span(),
                    "unknown label",
                ));
            };
            let Some(loop_index) = self.control.label_loop_index(index) else {
                return Err(CompileError::new(
                    self.source_text(),
                    statement.span,
                    "continue label is not a loop",
                ));
            };
            let depth = self.control.loop_continue_depth(loop_index);
            return self.emit_control_jump(JumpTarget::LoopContinue(loop_index), depth);
        }
        let Some((target, depth)) = self.control.innermost_continue() else {
            return Err(CompileError::new(
                self.source_text(),
                statement.span,
                "continue outside loop",
            ));
        };
        self.emit_control_jump(target, depth)
    }

    fn patch_loop(
        &mut self,
        loop_context: super::control::Loop,
        end: usize,
    ) -> Result<(), CompileError> {
        for jump in loop_context.break_jumps {
            self.patch_jump(jump, end)?;
        }
        Ok(())
    }
}
