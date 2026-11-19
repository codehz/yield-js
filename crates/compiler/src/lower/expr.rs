//! Expression lowering. Names go through [`crate::names`]; await goes through
//! [`Control`](super::control::Control)'s resume table.

use oxc::ast::ast::*;
use oxc::span::GetSpan;
use oxc::syntax::operator::{
    AssignmentOperator, BinaryOperator, LogicalOperator, UnaryOperator, UpdateOperator,
};
use yield_vm::BindingId;
use yield_vm::bytecode::{BinaryOp, Constant, Instr, PropertyDefinition, Slot, UnaryOp};

use super::{FunctionSource, Lowerer, THIS_BINDING};
use crate::emit::Reg;
use crate::error::{CompileError, unsupported};
use crate::names::{Callee, ValueName};

impl<'ast, 'borrow, 'src> Lowerer<'ast, 'borrow, 'src> {
    pub(super) fn compile_expression<'node>(
        &mut self,
        expression: &'node Expression<'node>,
    ) -> Result<Reg, CompileError>
    where
        'node: 'ast,
    {
        match expression {
            Expression::BooleanLiteral(literal) => {
                self.emit_constant(Constant::Bool(literal.value))
            }
            Expression::NullLiteral(_) => self.emit_constant(Constant::Null),
            Expression::NumericLiteral(literal) => {
                if !literal.value.is_finite() {
                    return Err(unsupported(
                        self.source_text(),
                        literal.span,
                        "non-finite numeric literal",
                    ));
                }
                self.emit_constant(Constant::Number(literal.value))
            }
            Expression::StringLiteral(literal) => {
                let value = literal.value.as_str().ok_or_else(|| {
                    unsupported(
                        self.source_text(),
                        literal.span,
                        "string with a lone surrogate",
                    )
                })?;
                self.emit_constant(Constant::String(value.to_owned()))
            }
            Expression::Identifier(identifier) => self.compile_identifier(identifier),
            Expression::ParenthesizedExpression(expression) => {
                self.compile_expression(&expression.expression)
            }
            Expression::SequenceExpression(expression) => {
                let mut last = None;
                for expression in &expression.expressions {
                    last = Some(self.compile_expression(expression)?);
                }
                last.ok_or_else(|| {
                    unsupported(
                        self.source_text(),
                        expression.span,
                        "empty sequence expression",
                    )
                })
            }
            Expression::BinaryExpression(expression) => self.compile_binary(expression),
            Expression::LogicalExpression(expression) => self.compile_logical(expression),
            Expression::ConditionalExpression(expression) => {
                let result = self.alloc_reg()?;
                let test = self.compile_expression(&expression.test)?;
                let alternate_branch = self.branch_false(test);
                let consequent = self.compile_expression(&expression.consequent)?;
                self.emit(Instr::Move {
                    dst: self.slot(result),
                    src: self.slot(consequent),
                });
                let end = self.jump();
                let alternate = self.pc();
                self.patch_jump(alternate_branch, alternate)?;
                let alternate_value = self.compile_expression(&expression.alternate)?;
                self.emit(Instr::Move {
                    dst: self.slot(result),
                    src: self.slot(alternate_value),
                });
                let end_target = self.pc();
                self.patch_jump(end, end_target)?;
                Ok(result)
            }
            Expression::UnaryExpression(expression) => self.compile_unary(expression),
            Expression::AwaitExpression(expression) => {
                if !self.is_async {
                    return Err(unsupported(
                        self.source_text(),
                        expression.span,
                        "await outside async function",
                    ));
                }
                let input = self.compile_expression(&expression.argument)?;
                self.emit_await(input)
            }
            Expression::CallExpression(expression) => self.compile_call(expression),
            Expression::AssignmentExpression(expression) => self.compile_assignment(expression),
            Expression::UpdateExpression(expression) => self.compile_update(expression),
            Expression::ObjectExpression(expression) => self.compile_object(expression),
            Expression::ArrayExpression(expression) => self.compile_array(expression),
            Expression::StaticMemberExpression(member) => self.compile_static_member_value(member),
            Expression::ComputedMemberExpression(member) => {
                let (object, key) = self.compile_computed_member(member)?;
                let dst = self.alloc_reg()?;
                self.emit(Instr::GetMember {
                    dst: self.slot(dst),
                    object: self.slot(object),
                    key: self.slot(key),
                });
                Ok(dst)
            }
            Expression::NewExpression(expression) => self.compile_new(expression),
            Expression::ThisExpression(expression) => self.compile_this(expression.span),
            Expression::FunctionExpression(function) => {
                let hint = function
                    .id
                    .as_ref()
                    .map(|id| id.name.as_str())
                    .unwrap_or("function");
                let func_id =
                    self.enqueue_function(FunctionSource::Function(function), hint, function.span)?;
                self.emit_function_value(func_id)
            }
            Expression::ArrowFunctionExpression(arrow) => {
                let func_id =
                    self.enqueue_function(FunctionSource::Arrow(arrow), "arrow", arrow.span)?;
                self.emit_function_value(func_id)
            }
            Expression::TemplateLiteral(template) => self.compile_template_literal(template),
            other => Err(unsupported(
                self.source_text(),
                other.span(),
                "expression form outside the PoC subset",
            )),
        }
    }

    /// Lower a non-tagged template literal by interleaving fixed text with
    /// interpolated expressions in source order, accumulating through
    /// `BinaryOp::Add` so each expression is evaluated immediately before the
    /// next piece of text is appended.
    fn compile_template_literal(
        &mut self,
        template: &'ast TemplateLiteral<'ast>,
    ) -> Result<Reg, CompileError> {
        // n expressions means n + 1 text chunks; the first chunk seeds the
        // accumulator, even when its text is empty.
        let first = template.quasis.first().ok_or_else(|| {
            unsupported(
                self.source_text(),
                template.span,
                "template literal without text chunks",
            )
        })?;
        let first_text = self.template_element_text(first)?;
        let mut acc = self.emit_constant(Constant::String(first_text.to_owned()))?;
        for (index, expression) in template.expressions.iter().enumerate() {
            let value = self.compile_expression(expression)?;
            acc = self.emit_string_add(acc, value)?;
            // Text after expression i is quasi i + 1; skip appending it when
            // empty so the accumulator stays a plain value.
            let tail = &template.quasis[index + 1];
            let tail_text = self.template_element_text(tail)?;
            if !tail_text.is_empty() {
                let text = self.emit_constant(Constant::String(tail_text.to_owned()))?;
                acc = self.emit_string_add(acc, text)?;
            }
        }
        Ok(acc)
    }

    /// `cooked` is the only accepted text source: no slicing the raw source,
    /// no re-decoding of escapes.
    fn template_element_text(
        &self,
        element: &'ast TemplateElement<'ast>,
    ) -> Result<&'ast str, CompileError> {
        element
            .value
            .cooked
            .as_ref()
            .and_then(|cooked| cooked.as_str())
            .ok_or_else(|| unsupported(self.source_text(), element.span, "invalid template escape"))
    }

    fn emit_string_add(&mut self, lhs: Reg, rhs: Reg) -> Result<Reg, CompileError> {
        let dst = self.alloc_reg()?;
        self.emit(Instr::Binary {
            dst: self.slot(dst),
            op: BinaryOp::Add,
            lhs: self.slot(lhs),
            rhs: self.slot(rhs),
        });
        Ok(dst)
    }

    fn compile_identifier(
        &mut self,
        identifier: &IdentifierReference<'_>,
    ) -> Result<Reg, CompileError> {
        let name = identifier.name.as_str();
        if let Some(binding) = self.lookup(name) {
            self.check_tdz(binding, name, identifier.span)?;
        }
        match self.session.names().value(&self.env, name) {
            ValueName::Binding(binding) => {
                let dst = self.alloc_reg()?;
                self.emit(Instr::GetBinding {
                    dst: self.slot(dst),
                    binding,
                });
                Ok(dst)
            }
            ValueName::Undefined => self.emit_undefined(),
            ValueName::Builtin(index) => {
                let dst = self.alloc_reg()?;
                self.emit(Instr::GetBuiltin {
                    dst: self.slot(dst),
                    builtin: index as u32,
                });
                Ok(dst)
            }
            ValueName::TopFunction(function) => {
                let dst = self.alloc_reg()?;
                self.emit(Instr::GetTopFunction {
                    dst: self.slot(dst),
                    function,
                });
                Ok(dst)
            }
            ValueName::NotFirstClass => Err(CompileError::new(
                self.source_text(),
                identifier.span,
                format!("import value {name:?} is not first-class in this PoC"),
            )),
            ValueName::Unknown => Err(CompileError::new(
                self.source_text(),
                identifier.span,
                format!("unknown binding {name:?}"),
            )),
        }
    }

    fn compile_binary(
        &mut self,
        expression: &'ast BinaryExpression<'ast>,
    ) -> Result<Reg, CompileError> {
        let operator = match expression.operator {
            BinaryOperator::Addition => BinaryOp::Add,
            BinaryOperator::Subtraction => BinaryOp::Sub,
            BinaryOperator::Multiplication => BinaryOp::Mul,
            BinaryOperator::Division => BinaryOp::Div,
            BinaryOperator::Remainder => BinaryOp::Rem,
            BinaryOperator::Exponential => BinaryOp::Exp,
            BinaryOperator::LessThan => BinaryOp::Lt,
            BinaryOperator::LessEqualThan => BinaryOp::Le,
            BinaryOperator::GreaterThan => BinaryOp::Gt,
            BinaryOperator::GreaterEqualThan => BinaryOp::Ge,
            BinaryOperator::Equality => BinaryOp::Eq,
            BinaryOperator::Inequality => BinaryOp::Ne,
            BinaryOperator::StrictEquality => BinaryOp::StrictEq,
            BinaryOperator::StrictInequality => BinaryOp::StrictNe,
            BinaryOperator::BitwiseAnd => BinaryOp::BitAnd,
            BinaryOperator::BitwiseOR => BinaryOp::BitOr,
            BinaryOperator::BitwiseXOR => BinaryOp::BitXor,
            BinaryOperator::ShiftLeft => BinaryOp::Shl,
            BinaryOperator::ShiftRight => BinaryOp::Shr,
            BinaryOperator::ShiftRightZeroFill => BinaryOp::UShr,
            BinaryOperator::In | BinaryOperator::Instanceof => {
                return Err(unsupported(
                    self.source_text(),
                    expression.span,
                    if matches!(expression.operator, BinaryOperator::In) {
                        "in operator"
                    } else {
                        "instanceof operator"
                    },
                ));
            }
        };
        let lhs = self.compile_expression(&expression.left)?;
        let rhs = self.compile_expression(&expression.right)?;
        let dst = self.alloc_reg()?;
        self.emit(Instr::Binary {
            dst: self.slot(dst),
            op: operator,
            lhs: self.slot(lhs),
            rhs: self.slot(rhs),
        });
        Ok(dst)
    }

    fn compile_logical(
        &mut self,
        expression: &'ast LogicalExpression<'ast>,
    ) -> Result<Reg, CompileError> {
        let result = self.alloc_reg()?;
        let left = self.compile_expression(&expression.left)?;
        self.emit(Instr::Move {
            dst: self.slot(result),
            src: self.slot(left),
        });
        let end = self.begin_logical_rhs(expression.operator, left)?;
        let right = self.compile_expression(&expression.right)?;
        self.emit(Instr::Move {
            dst: self.slot(result),
            src: self.slot(right),
        });
        self.patch_jump(end, self.pc())?;
        Ok(result)
    }

    /// Emits the short-circuit test. The returned jump lands after the RHS;
    /// the instruction stream is already at the RHS entry.
    fn begin_logical_rhs(&mut self, op: LogicalOperator, left: Reg) -> Result<usize, CompileError> {
        match op {
            LogicalOperator::And => Ok(self.branch_false(left)),
            LogicalOperator::Or => {
                let right_branch = self.branch_false(left);
                let end = self.jump();
                self.patch_jump(right_branch, self.pc())?;
                Ok(end)
            }
            LogicalOperator::Coalesce => {
                let null_value = self.emit_constant(Constant::Null)?;
                let is_null = self.alloc_reg()?;
                self.emit(Instr::Binary {
                    dst: self.slot(is_null),
                    op: BinaryOp::StrictEq,
                    lhs: self.slot(left),
                    rhs: self.slot(null_value),
                });
                let check_undefined = self.branch_false(is_null);
                let right_jump = self.jump();
                self.patch_jump(check_undefined, self.pc())?;
                let undefined_reg = self.alloc_reg()?;
                self.emit(Instr::Undefined {
                    dst: self.slot(undefined_reg),
                });
                let is_undefined = self.alloc_reg()?;
                self.emit(Instr::Binary {
                    dst: self.slot(is_undefined),
                    op: BinaryOp::StrictEq,
                    lhs: self.slot(left),
                    rhs: self.slot(undefined_reg),
                });
                let end = self.branch_false(is_undefined);
                self.patch_jump(right_jump, self.pc())?;
                Ok(end)
            }
        }
    }

    fn compile_unary(
        &mut self,
        expression: &'ast UnaryExpression<'ast>,
    ) -> Result<Reg, CompileError> {
        if matches!(expression.operator, UnaryOperator::Delete) {
            return Err(unsupported(self.source_text(), expression.span, "delete"));
        }
        if matches!(expression.operator, UnaryOperator::Typeof)
            && let Expression::Identifier(identifier) = &expression.argument
        {
            let name = identifier.name.as_str();
            if let Some(binding) = self.lookup(name) {
                self.check_tdz(binding, name, identifier.span)?;
            }
            if let Some(folded) = self.session.names().typeof_fold(&self.env, name) {
                return self.emit_constant(Constant::String(folded.to_owned()));
            }
        }
        let src = self.compile_expression(&expression.argument)?;
        let op = match expression.operator {
            UnaryOperator::UnaryNegation => UnaryOp::Neg,
            UnaryOperator::UnaryPlus => UnaryOp::Pos,
            UnaryOperator::LogicalNot => UnaryOp::Not,
            UnaryOperator::BitwiseNot => UnaryOp::BitNot,
            UnaryOperator::Typeof => UnaryOp::TypeOf,
            UnaryOperator::Void => UnaryOp::Void,
            UnaryOperator::Delete => unreachable!(),
        };
        let dst = self.alloc_reg()?;
        self.emit(Instr::Unary {
            dst: self.slot(dst),
            op,
            src: self.slot(src),
        });
        Ok(dst)
    }

    fn compile_call(
        &mut self,
        expression: &'ast CallExpression<'ast>,
    ) -> Result<Reg, CompileError> {
        if expression.optional {
            return Err(unsupported(
                self.source_text(),
                expression.span,
                "optional call",
            ));
        }

        let mut callee = &expression.callee;
        while let Expression::ParenthesizedExpression(paren) = callee {
            callee = &paren.expression;
        }

        if let Expression::Identifier(callee) = callee {
            let name = callee.name.as_str();
            match self.session.names().callee(&self.env, name) {
                Callee::Binding(binding) => {
                    self.check_tdz(binding, name, callee.span)?;
                    let callee = self.alloc_reg()?;
                    self.emit(Instr::GetBinding {
                        dst: self.slot(callee),
                        binding,
                    });
                    let args = self.compile_arguments(expression)?;
                    let dst = self.alloc_reg()?;
                    self.emit(Instr::CallValue {
                        dst: self.slot(dst),
                        callee: self.slot(callee),
                        args,
                        receiver: None,
                    });
                    return Ok(dst);
                }
                Callee::Import(import) => {
                    let args = self.compile_arguments(expression)?;
                    let dst = self.alloc_reg()?;
                    self.emit(Instr::CallImport {
                        dst: self.slot(dst),
                        import,
                        args,
                    });
                    return Ok(dst);
                }
                Callee::Function(function) => {
                    let args = self.compile_arguments(expression)?;
                    let dst = self.alloc_reg()?;
                    self.emit(Instr::CallFunction {
                        dst: self.slot(dst),
                        function,
                        args,
                    });
                    return Ok(dst);
                }
                Callee::Builtin(index) => {
                    let callee = self.alloc_reg()?;
                    self.emit(Instr::GetBuiltin {
                        dst: self.slot(callee),
                        builtin: index as u32,
                    });
                    let args = self.compile_argument_slice(&expression.arguments)?;
                    let dst = self.alloc_reg()?;
                    self.emit(Instr::CallValue {
                        dst: self.slot(dst),
                        callee: self.slot(callee),
                        args,
                        receiver: None,
                    });
                    return Ok(dst);
                }
                Callee::Unknown => {
                    return Err(CompileError::new(
                        self.source_text(),
                        callee.span,
                        format!("unknown callee {name:?}"),
                    ));
                }
            }
        }
        if let Some(member) = callee.as_member_expression() {
            let (object, key) = self.compile_member_parts(member)?;
            let callee = self.alloc_reg()?;
            self.emit(Instr::GetMember {
                dst: self.slot(callee),
                object: self.slot(object),
                key: self.slot(key),
            });
            let args = self.compile_arguments(expression)?;
            let dst = self.alloc_reg()?;
            self.emit(Instr::CallValue {
                dst: self.slot(dst),
                callee: self.slot(callee),
                args,
                receiver: Some(self.slot(object)),
            });
            return Ok(dst);
        }

        let callee = self.compile_expression(callee)?;
        let args = self.compile_arguments(expression)?;
        let dst = self.alloc_reg()?;
        self.emit(Instr::CallValue {
            dst: self.slot(dst),
            callee: self.slot(callee),
            args,
            receiver: None,
        });
        Ok(dst)
    }

    fn compile_argument_slice(
        &mut self,
        arguments: &'ast [Argument<'ast>],
    ) -> Result<Vec<Slot>, CompileError> {
        let mut args = Vec::with_capacity(arguments.len());
        for argument in arguments {
            let Some(expression) = argument.as_expression() else {
                return Err(unsupported(
                    self.source_text(),
                    argument.span(),
                    "spread argument",
                ));
            };
            let reg = self.compile_expression(expression)?;
            args.push(self.slot(reg));
        }
        Ok(args)
    }

    fn compile_arguments(
        &mut self,
        expression: &'ast CallExpression<'ast>,
    ) -> Result<Vec<Slot>, CompileError> {
        self.compile_argument_slice(&expression.arguments)
    }

    fn compile_new(&mut self, expression: &'ast NewExpression<'ast>) -> Result<Reg, CompileError> {
        let callee = self.compile_expression(&expression.callee)?;
        let args = self.compile_argument_slice(&expression.arguments)?;
        let dst = self.alloc_reg()?;
        self.emit(Instr::Construct {
            dst: self.slot(dst),
            callee: self.slot(callee),
            args,
        });
        Ok(dst)
    }

    fn compile_this(&mut self, span: oxc::span::Span) -> Result<Reg, CompileError> {
        let Some(binding) = self.lookup(THIS_BINDING) else {
            return Err(CompileError::new(
                self.source_text(),
                span,
                "this is not available",
            ));
        };
        let dst = self.alloc_reg()?;
        self.emit(Instr::GetBinding {
            dst: self.slot(dst),
            binding,
        });
        Ok(dst)
    }

    fn compile_static_member_value(
        &mut self,
        member: &'ast StaticMemberExpression<'ast>,
    ) -> Result<Reg, CompileError> {
        if member.optional {
            return Err(unsupported(
                self.source_text(),
                member.span,
                "optional member access",
            ));
        }
        let (object, key) = self.compile_static_member(member)?;
        let dst = self.alloc_reg()?;
        self.emit(Instr::GetMember {
            dst: self.slot(dst),
            object: self.slot(object),
            key: self.slot(key),
        });
        Ok(dst)
    }

    fn compile_assignment(
        &mut self,
        expression: &'ast AssignmentExpression<'ast>,
    ) -> Result<Reg, CompileError> {
        if let AssignmentTarget::AssignmentTargetIdentifier(identifier) = &expression.left {
            let name = identifier.name.as_str();
            let binding = self.lookup(name).ok_or_else(|| {
                CompileError::new(
                    self.source_text(),
                    identifier.span,
                    format!("assignment to unknown binding {name:?}"),
                )
            })?;
            self.check_tdz(binding, name, identifier.span)?;
            if let Some(op) = logical_assignment_operator(expression.operator) {
                let result = self.alloc_reg()?;
                self.emit(Instr::GetBinding {
                    dst: self.slot(result),
                    binding,
                });
                let end = self.begin_logical_rhs(op, result)?;
                let right = self.compile_expression(&expression.right)?;
                self.emit(Instr::SetBinding {
                    binding,
                    src: self.slot(right),
                });
                self.emit(Instr::Move {
                    dst: self.slot(result),
                    src: self.slot(right),
                });
                self.patch_jump(end, self.pc())?;
                return Ok(result);
            }
            let value = if matches!(expression.operator, AssignmentOperator::Assign) {
                self.compile_expression(&expression.right)?
            } else {
                let old = self.alloc_reg()?;
                self.emit(Instr::GetBinding {
                    dst: self.slot(old),
                    binding,
                });
                let right = self.compile_expression(&expression.right)?;
                let op = binary_assignment_operator(expression.operator);
                let value = self.alloc_reg()?;
                self.emit(Instr::Binary {
                    dst: self.slot(value),
                    op,
                    lhs: self.slot(old),
                    rhs: self.slot(right),
                });
                value
            };
            self.emit(Instr::SetBinding {
                binding,
                src: self.slot(value),
            });
            let result = self.alloc_reg()?;
            self.emit(Instr::Move {
                dst: self.slot(result),
                src: self.slot(value),
            });
            return Ok(result);
        }

        if let Some(member) = expression.left.as_member_expression() {
            let (object, key) = self.compile_member_parts(member)?;
            if let Some(op) = logical_assignment_operator(expression.operator) {
                let result = self.alloc_reg()?;
                self.emit(Instr::GetMember {
                    dst: self.slot(result),
                    object: self.slot(object),
                    key: self.slot(key),
                });
                let end = self.begin_logical_rhs(op, result)?;
                let right = self.compile_expression(&expression.right)?;
                self.emit(Instr::SetMember {
                    object: self.slot(object),
                    key: self.slot(key),
                    src: self.slot(right),
                });
                self.emit(Instr::Move {
                    dst: self.slot(result),
                    src: self.slot(right),
                });
                self.patch_jump(end, self.pc())?;
                return Ok(result);
            }
            let value = if matches!(expression.operator, AssignmentOperator::Assign) {
                self.compile_expression(&expression.right)?
            } else {
                let old = self.alloc_reg()?;
                self.emit(Instr::GetMember {
                    dst: self.slot(old),
                    object: self.slot(object),
                    key: self.slot(key),
                });
                let right = self.compile_expression(&expression.right)?;
                let op = binary_assignment_operator(expression.operator);
                let value = self.alloc_reg()?;
                self.emit(Instr::Binary {
                    dst: self.slot(value),
                    op,
                    lhs: self.slot(old),
                    rhs: self.slot(right),
                });
                value
            };
            self.emit(Instr::SetMember {
                object: self.slot(object),
                key: self.slot(key),
                src: self.slot(value),
            });
            let result = self.alloc_reg()?;
            self.emit(Instr::Move {
                dst: self.slot(result),
                src: self.slot(value),
            });
            return Ok(result);
        }

        Err(unsupported(
            self.source_text(),
            expression.span,
            "assignment to non-identifier/non-member target",
        ))
    }

    fn compile_update(
        &mut self,
        expression: &'ast UpdateExpression<'ast>,
    ) -> Result<Reg, CompileError> {
        enum Target {
            Binding(BindingId),
            Member(Reg, Reg),
        }

        let target = if let SimpleAssignmentTarget::AssignmentTargetIdentifier(identifier) =
            &expression.argument
        {
            let name = identifier.name.as_str();
            let binding = self.lookup(name).ok_or_else(|| {
                CompileError::new(
                    self.source_text(),
                    identifier.span,
                    format!("assignment to unknown binding {name:?}"),
                )
            })?;
            self.check_tdz(binding, name, identifier.span)?;
            Target::Binding(binding)
        } else if let Some(member) = expression.argument.as_member_expression() {
            let (object, key) = self.compile_member_parts(member)?;
            Target::Member(object, key)
        } else {
            return Err(unsupported(
                self.source_text(),
                expression.argument.span(),
                "update target outside the PoC subset",
            ));
        };

        let old = self.alloc_reg()?;
        match &target {
            Target::Binding(binding) => self.emit(Instr::GetBinding {
                dst: self.slot(old),
                binding: *binding,
            }),
            Target::Member(object, key) => self.emit(Instr::GetMember {
                dst: self.slot(old),
                object: self.slot(*object),
                key: self.slot(*key),
            }),
        };
        let numeric = self.alloc_reg()?;
        self.emit(Instr::Unary {
            dst: self.slot(numeric),
            op: UnaryOp::Pos,
            src: self.slot(old),
        });
        let one = self.emit_constant(Constant::Number(1.0))?;
        let op = match expression.operator {
            UpdateOperator::Increment => BinaryOp::Add,
            UpdateOperator::Decrement => BinaryOp::Sub,
        };
        let updated = self.alloc_reg()?;
        self.emit(Instr::Binary {
            dst: self.slot(updated),
            op,
            lhs: self.slot(numeric),
            rhs: self.slot(one),
        });
        match &target {
            Target::Binding(binding) => self.emit(Instr::SetBinding {
                binding: *binding,
                src: self.slot(updated),
            }),
            Target::Member(object, key) => self.emit(Instr::SetMember {
                object: self.slot(*object),
                key: self.slot(*key),
                src: self.slot(updated),
            }),
        };
        Ok(if expression.prefix { updated } else { numeric })
    }

    fn compile_member_parts(
        &mut self,
        member: &'ast MemberExpression<'ast>,
    ) -> Result<(Reg, Reg), CompileError> {
        match member {
            MemberExpression::StaticMemberExpression(member) => self.compile_static_member(member),
            MemberExpression::ComputedMemberExpression(member) => {
                self.compile_computed_member(member)
            }
            MemberExpression::PrivateFieldExpression(member) => Err(unsupported(
                self.source_text(),
                member.span,
                "private field access",
            )),
        }
    }

    fn compile_static_member(
        &mut self,
        member: &'ast StaticMemberExpression<'ast>,
    ) -> Result<(Reg, Reg), CompileError> {
        if member.optional {
            return Err(unsupported(
                self.source_text(),
                member.span,
                "optional member access",
            ));
        }
        let object = self.compile_expression(&member.object)?;
        let key = self.emit_constant(Constant::String(member.property.name.as_str().to_owned()))?;
        Ok((object, key))
    }

    fn compile_computed_member(
        &mut self,
        member: &'ast ComputedMemberExpression<'ast>,
    ) -> Result<(Reg, Reg), CompileError> {
        if member.optional {
            return Err(unsupported(
                self.source_text(),
                member.span,
                "optional member access",
            ));
        }
        let object = self.compile_expression(&member.object)?;
        let key = self.compile_expression(&member.expression)?;
        Ok((object, key))
    }

    fn compile_object(
        &mut self,
        object: &'ast ObjectExpression<'ast>,
    ) -> Result<Reg, CompileError> {
        let dst = self.alloc_reg()?;
        self.emit(Instr::NewObject {
            dst: self.slot(dst),
        });
        for property in &object.properties {
            let ObjectPropertyKind::ObjectProperty(property) = property else {
                return Err(unsupported(
                    self.source_text(),
                    property.span(),
                    "object spread",
                ));
            };
            let key = if property.computed {
                let key = property.key.as_expression().ok_or_else(|| {
                    unsupported(
                        self.source_text(),
                        property.key.span(),
                        "unsupported computed property key",
                    )
                })?;
                self.compile_expression(key)?
            } else if let Some(name) = property.key.static_name() {
                let name = name.into_cow_str().ok_or_else(|| {
                    unsupported(
                        self.source_text(),
                        property.key.span(),
                        "property key with a lone surrogate",
                    )
                })?;
                self.emit_constant(Constant::String(name.into_owned()))?
            } else {
                return Err(unsupported(
                    self.source_text(),
                    property.key.span(),
                    "unsupported property key",
                ));
            };
            let kind = match property.kind {
                PropertyKind::Init => PropertyDefinition::Data,
                PropertyKind::Get => PropertyDefinition::Get,
                PropertyKind::Set => PropertyDefinition::Set,
            };
            let value = if property.method || property.kind != PropertyKind::Init {
                let Expression::FunctionExpression(function) = &property.value else {
                    return Err(unsupported(
                        self.source_text(),
                        property.span,
                        "object member without a function",
                    ));
                };
                let (hint, arity) = match kind {
                    PropertyDefinition::Data => ("method", None),
                    PropertyDefinition::Get => ("getter", Some(0)),
                    PropertyDefinition::Set => ("setter", Some(1)),
                };
                if let Some(arity) = arity
                    && (function.params.items.len() != arity || function.params.rest.is_some())
                {
                    return Err(unsupported(
                        self.source_text(),
                        function.params.span,
                        &format!("{hint} requires {arity} simple parameters"),
                    ));
                }
                let id =
                    self.enqueue_function(FunctionSource::Method(function), hint, function.span)?;
                self.emit_function_value(id)?
            } else {
                self.compile_expression(&property.value)?
            };
            self.emit(Instr::DefineProperty {
                object: self.slot(dst),
                key: self.slot(key),
                src: self.slot(value),
                kind,
            });
        }
        Ok(dst)
    }

    fn compile_array(&mut self, array: &'ast ArrayExpression<'ast>) -> Result<Reg, CompileError> {
        let mut elements = Vec::with_capacity(array.elements.len());
        for element in &array.elements {
            match element {
                ArrayExpressionElement::Elision(_) => {
                    let value = self.emit_undefined()?;
                    elements.push(self.slot(value));
                }
                ArrayExpressionElement::SpreadElement(_) => {
                    return Err(unsupported(
                        self.source_text(),
                        element.span(),
                        "array spread",
                    ));
                }
                other => {
                    let expression = other.as_expression().ok_or_else(|| {
                        unsupported(
                            self.source_text(),
                            other.span(),
                            "array element outside the PoC subset",
                        )
                    })?;
                    let value = self.compile_expression(expression)?;
                    elements.push(self.slot(value));
                }
            }
        }
        let dst = self.alloc_reg()?;
        self.emit(Instr::NewArray {
            dst: self.slot(dst),
            elements,
        });
        Ok(dst)
    }
}

fn logical_assignment_operator(op: AssignmentOperator) -> Option<LogicalOperator> {
    match op {
        AssignmentOperator::LogicalAnd => Some(LogicalOperator::And),
        AssignmentOperator::LogicalOr => Some(LogicalOperator::Or),
        AssignmentOperator::LogicalNullish => Some(LogicalOperator::Coalesce),
        _ => None,
    }
}

fn binary_assignment_operator(op: AssignmentOperator) -> BinaryOp {
    match op {
        AssignmentOperator::Assign
        | AssignmentOperator::LogicalOr
        | AssignmentOperator::LogicalAnd
        | AssignmentOperator::LogicalNullish => unreachable!(),
        AssignmentOperator::Addition => BinaryOp::Add,
        AssignmentOperator::Subtraction => BinaryOp::Sub,
        AssignmentOperator::Multiplication => BinaryOp::Mul,
        AssignmentOperator::Division => BinaryOp::Div,
        AssignmentOperator::Remainder => BinaryOp::Rem,
        AssignmentOperator::Exponential => BinaryOp::Exp,
        AssignmentOperator::ShiftLeft => BinaryOp::Shl,
        AssignmentOperator::ShiftRight => BinaryOp::Shr,
        AssignmentOperator::ShiftRightZeroFill => BinaryOp::UShr,
        AssignmentOperator::BitwiseOR => BinaryOp::BitOr,
        AssignmentOperator::BitwiseXOR => BinaryOp::BitXor,
        AssignmentOperator::BitwiseAnd => BinaryOp::BitAnd,
    }
}
