//! Statement lowering. Control-flow constructs record jumps on [`Control`];
//! they do not patch slots or allocate bindings themselves.

use oxc::ast::ast::*;
use oxc::span::GetSpan;
use yield_vm::bytecode::Instr;

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
            Statement::SwitchStatement(statement) => Err(unsupported(
                self.source_text(),
                statement.span,
                "switch statement",
            )),
            Statement::TryStatement(statement) => self.compile_try(statement),
            Statement::ForInStatement(statement) => {
                Err(unsupported(self.source_text(), statement.span, "for-in"))
            }
            Statement::ForOfStatement(statement) => {
                Err(unsupported(self.source_text(), statement.span, "for-of"))
            }
            Statement::LabeledStatement(statement) => Err(unsupported(
                self.source_text(),
                statement.span,
                "labelled statement",
            )),
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

    fn compile_break(&mut self, statement: &BreakStatement<'_>) -> Result<(), CompileError> {
        if statement.label.is_some() {
            return Err(unsupported(
                self.source_text(),
                statement.span,
                "labelled break",
            ));
        }
        let Some(depth) = self.control.loop_finalizer_depth() else {
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
        self.control.push_break(jump);
        Ok(())
    }

    fn compile_continue(&mut self, statement: &ContinueStatement<'_>) -> Result<(), CompileError> {
        if statement.label.is_some() {
            return Err(unsupported(
                self.source_text(),
                statement.span,
                "labelled continue",
            ));
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
