//! Statement lowering. Control-flow constructs record jumps on [`Control`];
//! they do not patch slots or allocate bindings themselves.

use oxc::ast::ast::*;
use oxc::span::GetSpan;
use yield_vm::BindingId;
use yield_vm::bytecode::{BinaryOp, Instr};

use super::control::JumpTarget;
use super::{FunctionSource, Lowerer, local_declaration};
use crate::emit::Reg;
use crate::error::{CompileError, unsupported};

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
                let test = self.compile_expression(&statement.test)?;
                let branch = self.branch_false(test);
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
                let start = self.pc();
                let test = self.compile_expression(&statement.test)?;
                let exit_branch = self.branch_false(test);
                self.control.push_loop(Some(start));
                self.compile_statement(&statement.body)?;
                let loop_context = self.control.pop_loop();
                self.jump_to(start);
                let end = self.pc();
                self.patch_jump(exit_branch, end)?;
                self.patch_loop(loop_context, end)
            }
            Statement::DoWhileStatement(statement) => {
                let start = self.pc();
                self.control.push_loop(None);
                self.compile_statement(&statement.body)?;
                let test_pc = self.pc();
                let mut loop_context = self.control.pop_loop();
                for jump in loop_context.continue_jumps.drain(..) {
                    self.patch_jump(jump, test_pc)?;
                }
                let test = self.compile_expression(&statement.test)?;
                self.branch_false(test);
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
                let test_pc = self.pc();
                let exit_branch = if let Some(test) = &statement.test {
                    let test = self.compile_expression(test)?;
                    Some(self.branch_false(test))
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
            Statement::ExportDefaultDeclaration(export) => Err(unsupported(
                self.source_text(),
                export.span,
                "default export",
            )),
            Statement::ExportFromDeclaration(export) => {
                Err(unsupported(self.source_text(), export.span, "re-export"))
            }
            Statement::ExportAllDeclaration(export) => {
                Err(unsupported(self.source_text(), export.span, "re-export"))
            }
            Statement::ImportDeclaration(import) => Err(unsupported(
                self.source_text(),
                import.span,
                "import declaration",
            )),
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
            Declaration::ClassDeclaration(class) => Err(unsupported(
                self.source_text(),
                class.span,
                "class declaration",
            )),
            other => Err(unsupported(
                self.source_text(),
                other.span(),
                "top-level/module statement",
            )),
        }
    }

    fn compile_for_in(
        &mut self,
        statement: &'ast ForInStatement<'ast>,
    ) -> Result<(), CompileError> {
        self.env.enter_block();
        let binding = self.declare_iteration_binding(&statement.left, "for-in")?;
        let source = self.compile_expression(&statement.right)?;
        if let Some(binding) = binding {
            self.env.clear_pending(binding);
        }

        let iterator = self.alloc_reg()?;
        self.emit(Instr::GetForInIterator {
            dst: self.slot(iterator),
            source: self.slot(source),
        });
        let step_pc = self.pc();
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
        self.assign_iteration_value(&statement.left, binding, value, "for-in")?;
        self.compile_statement(&statement.body)?;
        let loop_context = self.control.pop_loop();
        self.jump_to(step_pc);
        let end = self.pc();
        self.patch_jump(exhausted, end)?;
        self.patch_loop(loop_context, end)?;
        self.env.exit_block();
        Ok(())
    }

    fn compile_for_of(
        &mut self,
        statement: &'ast ForOfStatement<'ast>,
    ) -> Result<(), CompileError> {
        if statement.r#await {
            return Err(unsupported(
                self.source_text(),
                statement.span,
                if self.is_script {
                    "top-level await"
                } else {
                    "for-await-of"
                },
            ));
        }
        self.env.enter_block();
        let binding = self.declare_iteration_binding(&statement.left, "for-of")?;

        let iterable = self.compile_expression(&statement.right)?;
        if let Some(binding) = binding {
            self.env.clear_pending(binding);
        }
        let iterator = self.alloc_reg()?;
        let next = self.alloc_reg()?;
        self.emit(Instr::GetIterator {
            iterator: self.slot(iterator),
            next: self.slot(next),
            iterable: self.slot(iterable),
        });
        let step_pc = self.pc();
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
        let loop_context = self.compile_iterator_body(iterator, step_pc, |this| {
            this.assign_iteration_value(&statement.left, binding, value, "for-of")?;
            this.compile_statement(&statement.body)
        })?;

        let end = self.pc();
        self.patch_jump(exhausted, end)?;
        self.patch_loop(loop_context, end)?;
        self.env.exit_block();
        Ok(())
    }

    fn declare_iteration_binding(
        &mut self,
        target: &'ast ForStatementLeft<'ast>,
        loop_kind: &str,
    ) -> Result<Option<BindingId>, CompileError> {
        let ForStatementLeft::VariableDeclaration(declaration) = target else {
            return Ok(None);
        };
        let [declarator] = declaration.declarations.as_slice() else {
            return Err(unsupported(
                self.source_text(),
                declaration.span,
                &format!("{loop_kind} declaration"),
            ));
        };
        let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
            return Err(unsupported(
                self.source_text(),
                declarator.span,
                &format!("{loop_kind} destructuring"),
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
        let (binding, _) =
            self.declare(identifier.name.as_str(), declaration.kind, declarator.span)?;
        // `var` exists even when the iteration yields nothing.
        if declaration.kind == VariableDeclarationKind::Var {
            self.emit(Instr::DeclareBinding { binding });
        }
        Ok(Some(binding))
    }

    fn assign_iteration_value(
        &mut self,
        target: &'ast ForStatementLeft<'ast>,
        binding: Option<BindingId>,
        value: Reg,
        loop_kind: &str,
    ) -> Result<(), CompileError> {
        let src = self.slot(value);
        if let Some(binding) = binding {
            self.emit(Instr::SetBinding { binding, src });
        } else if let ForStatementLeft::AssignmentTargetIdentifier(identifier) = target {
            let name = identifier.name.as_str();
            let binding = self.lookup(name).ok_or_else(|| {
                CompileError::new(
                    self.source_text(),
                    identifier.span,
                    format!("assignment to unknown binding {name:?}"),
                )
            })?;
            self.check_tdz(binding, name, identifier.span)?;
            self.emit(Instr::SetBinding { binding, src });
        } else if let Some(member) = target.as_member_expression() {
            let (object, key) = self.compile_member_parts(member)?;
            self.emit(Instr::SetMember {
                object: self.slot(object),
                key: self.slot(key),
                src,
            });
        } else {
            return Err(unsupported(
                self.source_text(),
                target.span(),
                &format!("{loop_kind} destructuring"),
            ));
        }
        Ok(())
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
            let match_reg = self.alloc_reg()?;
            self.emit(Instr::Binary {
                dst: self.slot(match_reg),
                op: BinaryOp::StrictEq,
                lhs: self.slot(disc),
                rhs: self.slot(test_reg),
            });
            let branch = self.branch_false(match_reg);
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
