//! Statement lowering. Control-flow constructs record jumps on [`Control`];
//! they do not patch slots or allocate bindings themselves.

use oxc::ast::ast::*;
use oxc::span::GetSpan;
use yield_vm::bytecode::{BinaryOp, Instr};

use super::{FunctionSource, Lowerer};
use crate::error::{CompileError, unsupported};

impl<'ast, 'borrow, 'src> Lowerer<'ast, 'borrow, 'src> {
    pub(super) fn compile_statement<'node>(
        &mut self,
        statement: &'node Statement<'node>,
    ) -> Result<(), CompileError>
    where
        'node: 'ast,
    {
        match statement {
            Statement::BlockStatement(block) => self.compile_block(block),
            Statement::EmptyStatement(_) => Ok(()),
            Statement::ExpressionStatement(statement) => {
                self.compile_expression(&statement.expression)?;
                Ok(())
            }
            Statement::VariableDeclaration(declaration) => {
                self.compile_variable_declaration(declaration)
            }
            Statement::FunctionDeclaration(function) => {
                if self.hoisted.contains(&function.span.start) {
                    return Ok(());
                }
                let name = function
                    .id
                    .as_ref()
                    .ok_or_else(|| {
                        unsupported(self.source_text(), function.span, "anonymous declaration")
                    })?
                    .name
                    .as_str()
                    .to_owned();
                let (binding, _) =
                    self.declare(&name, VariableDeclarationKind::Let, function.span)?;
                let func_id = self.enqueue_function(
                    FunctionSource::Function(function),
                    &name,
                    function.span,
                )?;
                self.init_function_binding(binding, func_id)
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
            Statement::ForInStatement(statement) => {
                Err(unsupported(self.source_text(), statement.span, "for-in"))
            }
            Statement::ForOfStatement(statement) => {
                Err(unsupported(self.source_text(), statement.span, "for-of"))
            }
            Statement::LabeledStatement(statement) => self.compile_labeled(statement),
            Statement::ClassDeclaration(statement) => Err(unsupported(
                self.source_text(),
                statement.span,
                "class declaration",
            )),
            other => Err(unsupported(
                self.source_text(),
                other.span(),
                "top-level/module statement",
            )),
        }
    }

    /// Label chain lowering. Only a chain ending in `while`, `do…while`, or a
    /// plain `for` binds its names to the loop (for `continue`); any other
    /// terminal statement makes the names plain `break` targets. Each name in
    /// the chain must be unique among currently active labels. The terminal
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
                        | Statement::ForStatement(_) => Some(self.control.loops_len()),
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
            if self.control.finalizer_depth() > depth {
                return Err(unsupported(
                    self.source_text(),
                    statement.span,
                    "break across finally",
                ));
            }
            let jump = self.jump();
            self.control.push_label_break(index, jump);
            return Ok(());
        }
        let Some((target, depth)) = self.control.innermost_break() else {
            return Err(CompileError::new(
                self.source_text(),
                statement.span,
                "break outside loop",
            ));
        };
        if self.control.finalizer_depth() > depth {
            return Err(unsupported(
                self.source_text(),
                statement.span,
                "break across finally",
            ));
        }
        let jump = self.jump();
        match target {
            super::control::BreakTarget::Loop => self.control.push_break(jump),
            super::control::BreakTarget::Switch => self.control.push_switch_break(jump),
        }
        Ok(())
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
            let depth = self.control.label_finalizer_depth(index);
            if self.control.finalizer_depth() > depth {
                return Err(unsupported(
                    self.source_text(),
                    statement.span,
                    "continue across finally",
                ));
            }
            if let Some(target) = self.control.loop_continue_target(loop_index) {
                self.jump_to(target);
            } else {
                let jump = self.jump();
                self.control.push_continue_to(loop_index, jump);
            }
            return Ok(());
        }
        let Some((continue_target, loop_depth)) = self.control.innermost_continue() else {
            return Err(CompileError::new(
                self.source_text(),
                statement.span,
                "continue outside loop",
            ));
        };
        if self.control.finalizer_depth() > loop_depth {
            return Err(unsupported(
                self.source_text(),
                statement.span,
                "continue across finally",
            ));
        }
        if let Some(target) = continue_target {
            self.jump_to(target);
        } else {
            let jump = self.jump();
            self.control.push_continue(jump);
        }
        Ok(())
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
