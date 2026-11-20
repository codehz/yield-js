//! Expression lowering. Names go through [`crate::names`]; await goes through
//! [`Control`](super::control::Control)'s resume table.

use oxc::ast::ast::*;
use oxc::span::GetSpan;
use oxc::syntax::operator::{
    AssignmentOperator, BinaryOperator, LogicalOperator, UnaryOperator, UpdateOperator,
};
use yield_vm::bytecode::{BinaryOp, CallArgs, Constant, Instr, PropertyDefinition, UnaryOp};
use yield_vm::{BindingId, JsString};

use super::{FunctionSource, Lowerer, NEW_TARGET_BINDING, THIS_BINDING};
use crate::emit::Reg;
use crate::error::{CompileError, unsupported};
use crate::names::{Callee, ReadOnlyName, ValueName, builtin_index};

impl<'ast, 'borrow, 'src> Lowerer<'ast, 'borrow, 'src> {
    pub(super) fn compile_named_expression(
        &mut self,
        mut expression: &'ast Expression<'ast>,
        name: &str,
    ) -> Result<Reg, CompileError> {
        while let Expression::ParenthesizedExpression(paren) = expression {
            expression = &paren.expression;
        }
        if let Expression::ClassExpression(class) = expression {
            self.compile_class(class, name)
        } else {
            self.compile_expression(expression)
        }
    }

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
                self.emit_constant(Constant::Number(literal.value))
            }
            Expression::StringLiteral(literal) => {
                let text = self.string_literal_text(literal)?;
                self.emit_constant(Constant::String(text))
            }
            Expression::RegExpLiteral(literal) => {
                let source = literal.regex.pattern.text.as_str();
                let flags = literal.regex.flags.to_string();
                yield_vm::validate_regexp(source, &flags).map_err(|message| {
                    unsupported(
                        self.source_text(),
                        literal.span,
                        &format!("RegExp literal: {message}"),
                    )
                })?;
                let dst = self.alloc_reg()?;
                self.emit(Instr::NewRegExp {
                    dst: self.slot(dst),
                    source: source.into(),
                    flags,
                });
                Ok(dst)
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
                    let feature = if self.is_script {
                        "top-level await"
                    } else {
                        "await outside async function"
                    };
                    return Err(unsupported(self.source_text(), expression.span, feature));
                }
                let input = self.compile_expression(&expression.argument)?;
                self.emit_await(input)
            }
            Expression::CallExpression(expression) => self.compile_call(expression),
            Expression::ChainExpression(expression) => Ok(self.compile_chain(expression, false)?.0),
            Expression::AssignmentExpression(expression) => self.compile_assignment(expression),
            Expression::UpdateExpression(expression) => self.compile_update(expression),
            Expression::ClassExpression(class) => self.compile_class(class, ""),
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
            Expression::NewTarget(property) => {
                let binding = self.lookup(NEW_TARGET_BINDING).ok_or_else(|| {
                    unsupported(
                        self.source_text(),
                        property.span,
                        "new.target outside a function",
                    )
                })?;
                let dst = self.alloc_reg()?;
                self.emit(Instr::GetBinding {
                    dst: self.slot(dst),
                    binding,
                });
                Ok(dst)
            }
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
            Expression::ImportExpression(import) => Err(unsupported(
                self.source_text(),
                import.span,
                "dynamic import",
            )),
            Expression::ImportMeta(property) => Err(unsupported(
                self.source_text(),
                property.span,
                "import.meta",
            )),
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
        let mut acc = self.emit_constant(Constant::String(first_text))?;
        for (index, expression) in template.expressions.iter().enumerate() {
            let value = self.compile_expression(expression)?;
            let value = self.emit_to_string(value)?;
            acc = self.emit_string_add(acc, value)?;
            // Skip empty text chunks; the accumulator is already a string.
            let tail = &template.quasis[index + 1];
            let tail_text = self.template_element_text(tail)?;
            if !tail_text.is_empty() {
                let text = self.emit_constant(Constant::String(tail_text))?;
                acc = self.emit_string_add(acc, text)?;
            }
        }
        Ok(acc)
    }

    fn template_element_text(
        &self,
        element: &'ast TemplateElement<'ast>,
    ) -> Result<JsString, CompileError> {
        let span = element.span;
        // Oxc's quasi span excludes the backticks and interpolation delimiters.
        if element.value.cooked.is_some()
            && let Some(text) = decode_string_body(span.source_text(self.source_text()), true)
        {
            return Ok(text);
        }
        Err(unsupported(
            self.source_text(),
            span,
            "invalid template escape",
        ))
    }

    fn string_literal_text(&self, literal: &StringLiteral<'_>) -> Result<JsString, CompileError> {
        let span = literal.span;
        let quoted = span.source_text(self.source_text());
        decode_string_body(&quoted[1..quoted.len() - 1], false)
            .ok_or_else(|| unsupported(self.source_text(), span, "invalid string escape"))
    }

    fn emit_to_string(&mut self, src: Reg) -> Result<Reg, CompileError> {
        let dst = self.alloc_reg()?;
        self.emit(Instr::ToString {
            dst: self.slot(dst),
            src: self.slot(src),
        });
        Ok(dst)
    }

    fn emit_to_property_key(&mut self, src: Reg, object: Option<Reg>) -> Result<Reg, CompileError> {
        let dst = self.alloc_reg()?;
        self.emit(Instr::ToPropertyKey {
            dst: self.slot(dst),
            src: self.slot(src),
            object: object.map(|reg| self.slot(reg)),
        });
        Ok(dst)
    }

    pub(super) fn compile_property_key(
        &mut self,
        key: &'ast PropertyKey<'ast>,
        computed: bool,
    ) -> Result<Reg, CompileError> {
        if computed {
            let expression = key.as_expression().ok_or_else(|| {
                unsupported(
                    self.source_text(),
                    key.span(),
                    "unsupported computed property key",
                )
            })?;
            let value = self.compile_expression(expression)?;
            self.emit_to_property_key(value, None)
        } else if let PropertyKey::StringLiteral(literal) = key {
            let text = self.string_literal_text(literal)?;
            self.emit_constant(Constant::String(text))
        } else if let PropertyKey::NumericLiteral(literal) = key {
            let mut buffer = ryu_js::Buffer::new();
            self.emit_constant(Constant::String(buffer.format(literal.value).into()))
        } else if let Some(name) = key.static_name().and_then(|name| name.into_cow_str()) {
            self.emit_constant(Constant::String(name.as_ref().into()))
        } else {
            Err(unsupported(
                self.source_text(),
                key.span(),
                "unsupported property key",
            ))
        }
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
                self.emit_binding_read(dst, binding);
                Ok(dst)
            }
            ValueName::ReadOnly(read_only) => self.emit_read_only(read_only),
            ValueName::Undefined => self.emit_undefined(),
            ValueName::Builtin(index) => {
                let dst = self.alloc_reg()?;
                self.emit(Instr::GetBuiltin {
                    dst: self.slot(dst),
                    builtin: index as u32,
                });
                Ok(dst)
            }
            ValueName::Unknown => Err(CompileError::new(
                self.source_text(),
                identifier.span,
                format!("unknown binding {name:?}"),
            )),
        }
    }

    fn emit_read_only(&mut self, name: ReadOnlyName<'_>) -> Result<Reg, CompileError> {
        match name {
            ReadOnlyName::Import(import) => {
                let dst = self.alloc_reg()?;
                self.emit(Instr::GetImport {
                    dst: self.slot(dst),
                    import,
                });
                Ok(dst)
            }
            ReadOnlyName::Preload(name) => {
                let global = self.alloc_reg()?;
                self.emit(Instr::GetBuiltin {
                    dst: self.slot(global),
                    builtin: builtin_index("globalThis").expect("globalThis builtin") as u32,
                });
                let key = self.emit_constant(Constant::String(name.into()))?;
                self.emit_get_member(global, key)
            }
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
            BinaryOperator::In => {
                let key = self.compile_expression(&expression.left)?;
                let object = self.compile_expression(&expression.right)?;
                let dst = self.alloc_reg()?;
                self.emit(Instr::HasProperty {
                    dst: self.slot(dst),
                    object: self.slot(object),
                    key: self.slot(key),
                });
                return Ok(dst);
            }
            BinaryOperator::Instanceof => {
                let value = self.compile_expression(&expression.left)?;
                let constructor = self.compile_expression(&expression.right)?;
                let dst = self.alloc_reg()?;
                self.emit(Instr::InstanceOf {
                    dst: self.slot(dst),
                    value: self.slot(value),
                    constructor: self.slot(constructor),
                });
                return Ok(dst);
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
            return self.compile_delete(&expression.argument);
        }
        if matches!(expression.operator, UnaryOperator::Typeof)
            && let Expression::Identifier(identifier) = &expression.argument
        {
            let name = identifier.name.as_str();
            if let Some(binding) = self.lookup(name) {
                self.check_tdz(binding, name, identifier.span)?;
            }
            if let Some(folded) = self.session.names().typeof_fold(&self.env, name) {
                return self.emit_constant(Constant::String(folded.into()));
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

    fn compile_delete(
        &mut self,
        mut argument: &'ast Expression<'ast>,
    ) -> Result<Reg, CompileError> {
        while let Expression::ParenthesizedExpression(paren) = argument {
            argument = &paren.expression;
        }
        if let Expression::Identifier(identifier) = argument {
            return Err(CompileError::new(
                self.source_text(),
                identifier.span,
                "cannot delete an unqualified identifier in strict mode",
            ));
        }
        if let Some(member) = argument.as_member_expression() {
            let (object, key) = self.compile_member_parts(member)?;
            return self.emit_delete_member(object, key);
        }
        if let Expression::ChainExpression(chain) = argument
            && let Some(member) = chain.expression.as_member_expression()
        {
            let mut exits = Vec::new();
            let (object, key) = self.compile_chain_member_parts(member, &mut exits)?;
            let result = self.emit_delete_member(object, key)?;
            if !exits.is_empty() {
                let end = self.jump();
                let short_circuit = self.pc();
                for exit in exits {
                    self.patch_jump(exit, short_circuit)?;
                }
                let success = self.emit_constant(Constant::Bool(true))?;
                self.emit(Instr::Move {
                    dst: self.slot(result),
                    src: self.slot(success),
                });
                self.patch_jump(end, self.pc())?;
            }
            return Ok(result);
        }
        self.compile_expression(argument)?;
        self.emit_constant(Constant::Bool(true))
    }

    fn emit_delete_member(&mut self, object: Reg, key: Reg) -> Result<Reg, CompileError> {
        let dst = self.alloc_reg()?;
        self.emit(Instr::DeleteMember {
            dst: self.slot(dst),
            object: self.slot(object),
            key: self.slot(key),
        });
        Ok(dst)
    }

    fn compile_chain(
        &mut self,
        chain: &'ast ChainExpression<'ast>,
        preserve_receiver: bool,
    ) -> Result<(Reg, Option<Reg>), CompileError> {
        let mut exits = Vec::new();
        let (value, receiver) = if let ChainElement::CallExpression(call) = &chain.expression {
            (self.compile_call_inner(call, Some(&mut exits))?, None)
        } else if let Some(member) = chain.expression.as_member_expression() {
            let (value, object) = self.compile_chain_member(member, &mut exits)?;
            (value, preserve_receiver.then_some(object))
        } else {
            return Err(unsupported(
                self.source_text(),
                chain.expression.span(),
                "expression form outside the PoC subset",
            ));
        };
        let result = self.alloc_reg()?;
        self.emit(Instr::Move {
            dst: self.slot(result),
            src: self.slot(value),
        });
        let result_receiver = if let Some(receiver) = receiver {
            let dst = self.alloc_reg()?;
            self.emit(Instr::Move {
                dst: self.slot(dst),
                src: self.slot(receiver),
            });
            Some(dst)
        } else {
            None
        };
        if !exits.is_empty() {
            let end = self.jump();
            let short_circuit = self.pc();
            for exit in exits {
                self.patch_jump(exit, short_circuit)?;
            }
            self.emit(Instr::Undefined {
                dst: self.slot(result),
            });
            if let Some(receiver) = result_receiver {
                self.emit(Instr::Undefined {
                    dst: self.slot(receiver),
                });
            }
            self.patch_jump(end, self.pc())?;
        }
        Ok((result, result_receiver))
    }

    fn guard_chain_link(&mut self, value: Reg, exits: &mut Vec<usize>) -> Result<(), CompileError> {
        let non_nullish = self.begin_logical_rhs(LogicalOperator::Coalesce, value)?;
        exits.push(self.jump());
        self.patch_jump(non_nullish, self.pc())
    }

    // Only unwrapped members and calls continue this chain. In particular,
    // parentheses and nested chain wrappers go through ordinary lowering.
    fn compile_chain_link(
        &mut self,
        expression: &'ast Expression<'ast>,
        exits: &mut Vec<usize>,
    ) -> Result<(Reg, Option<Reg>), CompileError> {
        if let Expression::CallExpression(call) = expression {
            Ok((self.compile_call_inner(call, Some(exits))?, None))
        } else if let Some(member) = expression.as_member_expression() {
            let (value, receiver) = self.compile_chain_member(member, exits)?;
            Ok((value, Some(receiver)))
        } else {
            Ok((self.compile_expression(expression)?, None))
        }
    }

    fn compile_chain_member(
        &mut self,
        member: &'ast MemberExpression<'ast>,
        exits: &mut Vec<usize>,
    ) -> Result<(Reg, Reg), CompileError> {
        let (object, key) = self.compile_chain_member_parts(member, exits)?;
        Ok((self.emit_get_member(object, key)?, object))
    }

    fn compile_chain_member_parts(
        &mut self,
        member: &'ast MemberExpression<'ast>,
        exits: &mut Vec<usize>,
    ) -> Result<(Reg, Reg), CompileError> {
        let (base, optional) = match member {
            MemberExpression::StaticMemberExpression(member) => (&member.object, member.optional),
            MemberExpression::ComputedMemberExpression(member) => (&member.object, member.optional),
            MemberExpression::PrivateFieldExpression(member) => {
                return Err(unsupported(
                    self.source_text(),
                    member.span,
                    "private field access",
                ));
            }
        };
        let (object, _) = self.compile_chain_link(base, exits)?;
        if optional {
            self.guard_chain_link(object, exits)?;
        }
        let key = match member {
            MemberExpression::StaticMemberExpression(member) => {
                self.emit_constant(Constant::String(member.property.name.as_str().into()))?
            }
            MemberExpression::ComputedMemberExpression(member) => {
                self.compile_expression(&member.expression)?
            }
            MemberExpression::PrivateFieldExpression(_) => unreachable!(),
        };
        Ok((object, key))
    }

    fn emit_get_member(&mut self, object: Reg, key: Reg) -> Result<Reg, CompileError> {
        let dst = self.alloc_reg()?;
        self.emit(Instr::GetMember {
            dst: self.slot(dst),
            object: self.slot(object),
            key: self.slot(key),
        });
        Ok(dst)
    }

    fn compile_call(
        &mut self,
        expression: &'ast CallExpression<'ast>,
    ) -> Result<Reg, CompileError> {
        self.compile_call_inner(expression, None)
    }

    fn compile_call_inner(
        &mut self,
        expression: &'ast CallExpression<'ast>,
        mut exits: Option<&mut Vec<usize>>,
    ) -> Result<Reg, CompileError> {
        if expression.optional && exits.is_none() {
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

        let (callee, receiver) = if let Expression::Identifier(identifier) = callee {
            let name = identifier.name.as_str();
            let value = match self.session.names().callee(&self.env, name) {
                Callee::Binding(binding) => {
                    self.check_tdz(binding, name, identifier.span)?;
                    let value = self.alloc_reg()?;
                    self.emit_binding_read(value, binding);
                    value
                }
                Callee::ReadOnly(read_only) => self.emit_read_only(read_only)?,
                Callee::Builtin(index) => {
                    let value = self.alloc_reg()?;
                    self.emit(Instr::GetBuiltin {
                        dst: self.slot(value),
                        builtin: index as u32,
                    });
                    value
                }
                Callee::Unknown if expression.optional && name == "undefined" => {
                    self.compile_identifier(identifier)?
                }
                Callee::Unknown => {
                    return Err(CompileError::new(
                        self.source_text(),
                        identifier.span,
                        format!("unknown callee {name:?}"),
                    ));
                }
            };
            (value, None)
        } else {
            // Parentheses end short-circuit propagation, but not the member reference.
            let boundary = matches!(expression.callee, Expression::ParenthesizedExpression(_))
                || matches!(callee, Expression::ChainExpression(_));
            match exits.as_deref_mut() {
                Some(exits) if !boundary => self.compile_chain_link(callee, exits)?,
                _ => self.compile_callee_value(callee)?,
            }
        };
        if let Some(exits) = exits.filter(|_| expression.optional) {
            self.guard_chain_link(callee, exits)?;
        }
        let args = self.compile_arguments(&expression.arguments)?;
        let dst = self.alloc_reg()?;
        self.emit(Instr::CallValue {
            dst: self.slot(dst),
            callee: self.slot(callee),
            args,
            receiver: receiver.map(|receiver| self.slot(receiver)),
        });
        Ok(dst)
    }

    fn compile_callee_value(
        &mut self,
        callee: &'ast Expression<'ast>,
    ) -> Result<(Reg, Option<Reg>), CompileError> {
        if let Expression::ChainExpression(chain) = callee {
            self.compile_chain(chain, true)
        } else if let Some(member) = callee.as_member_expression() {
            let (object, key) = self.compile_member_parts(member)?;
            Ok((self.emit_get_member(object, key)?, Some(object)))
        } else {
            Ok((self.compile_expression(callee)?, None))
        }
    }

    fn compile_arguments(
        &mut self,
        arguments: &'ast [Argument<'ast>],
    ) -> Result<CallArgs, CompileError> {
        let mut slots = Vec::new();
        let mut spread_array = None;
        for argument in arguments {
            match argument {
                Argument::SpreadElement(spread) => {
                    let array = if let Some(array) = spread_array {
                        array
                    } else {
                        let array = self.alloc_reg()?;
                        self.emit(Instr::NewArray {
                            dst: self.slot(array),
                            elements: std::mem::take(&mut slots),
                        });
                        spread_array = Some(array);
                        array
                    };
                    self.emit_spread(array, &spread.argument)?;
                }
                other => {
                    let value = self.compile_expression(other.to_expression())?;
                    if let Some(array) = spread_array {
                        self.append_array_element(array, value);
                    } else {
                        slots.push(self.slot(value));
                    }
                }
            }
        }
        Ok(match spread_array {
            Some(array) => CallArgs::Spread(self.slot(array)),
            None => CallArgs::Slots(slots),
        })
    }

    fn compile_new(&mut self, expression: &'ast NewExpression<'ast>) -> Result<Reg, CompileError> {
        let callee = self.compile_expression(&expression.callee)?;
        let args = self.compile_arguments(&expression.arguments)?;
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
            return self.emit_undefined();
        };
        let _ = span;
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
            let Some(binding) = self.lookup(name) else {
                if let Some(read_only) = self.session.names().read_only(name) {
                    return self.compile_read_only_assignment(expression, read_only);
                }
                return Err(CompileError::new(
                    self.source_text(),
                    identifier.span,
                    format!("assignment to unknown binding {name:?}"),
                ));
            };
            self.check_tdz(binding, name, identifier.span)?;
            if let Some(op) = logical_assignment_operator(expression.operator) {
                let result = self.alloc_reg()?;
                self.emit_binding_read(result, binding);
                let end = self.begin_logical_rhs(op, result)?;
                let right = self.compile_named_expression(&expression.right, name)?;
                self.put_binding(binding, right);
                self.emit(Instr::Move {
                    dst: self.slot(result),
                    src: self.slot(right),
                });
                self.patch_jump(end, self.pc())?;
                return Ok(result);
            }
            let value = if matches!(expression.operator, AssignmentOperator::Assign) {
                self.compile_named_expression(&expression.right, name)?
            } else {
                let old = self.alloc_reg()?;
                self.emit_binding_read(old, binding);
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
            self.put_binding(binding, value);
            let result = self.alloc_reg()?;
            self.emit(Instr::Move {
                dst: self.slot(result),
                src: self.slot(value),
            });
            return Ok(result);
        }

        if let Some(member) = expression.left.as_member_expression() {
            let (object, key) = self.compile_member_parts(member)?;
            let key = if member.is_computed() && expression.operator != AssignmentOperator::Assign {
                self.emit_to_property_key(key, Some(object))?
            } else {
                key
            };
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

        if expression.operator == AssignmentOperator::Assign
            && matches!(
                expression.left,
                AssignmentTarget::ArrayAssignmentTarget(_)
                    | AssignmentTarget::ObjectAssignmentTarget(_)
            )
        {
            let value = self.compile_expression(&expression.right)?;
            self.compile_assignment_pattern(&expression.left, value)?;
            return Ok(value);
        }

        Err(unsupported(
            self.source_text(),
            expression.span,
            "assignment to non-identifier/non-member target",
        ))
    }

    fn compile_read_only_assignment(
        &mut self,
        expression: &'ast AssignmentExpression<'ast>,
        name: ReadOnlyName<'_>,
    ) -> Result<Reg, CompileError> {
        let old = if matches!(expression.operator, AssignmentOperator::Assign) {
            None
        } else {
            Some(self.emit_read_only(name)?)
        };

        if let Some(op) = logical_assignment_operator(expression.operator) {
            let result = old.expect("logical assignment reads the current value");
            let end = self.begin_logical_rhs(op, result)?;
            let _right = self.compile_expression(&expression.right)?;
            self.emit(Instr::ThrowAssignmentToConst);
            self.patch_jump(end, self.pc())?;
            return Ok(result);
        }

        let right = self.compile_expression(&expression.right)?;
        let result = if let Some(old) = old {
            let result = self.alloc_reg()?;
            self.emit(Instr::Binary {
                dst: self.slot(result),
                op: binary_assignment_operator(expression.operator),
                lhs: self.slot(old),
                rhs: self.slot(right),
            });
            result
        } else {
            right
        };
        self.emit(Instr::ThrowAssignmentToConst);
        Ok(result)
    }

    fn compile_update(
        &mut self,
        expression: &'ast UpdateExpression<'ast>,
    ) -> Result<Reg, CompileError> {
        enum Target<'name> {
            Binding(BindingId),
            ReadOnly(ReadOnlyName<'name>),
            Member(Reg, Reg),
        }

        // The parser normalizes parenthesized simple assignment targets, so
        // `(x)++`, `++(obj.x)` and `((obj.x))--` arrive here as plain
        // identifier/member targets; no paren unwrapping is needed.
        let target = if let SimpleAssignmentTarget::AssignmentTargetIdentifier(identifier) =
            &expression.argument
        {
            let name = identifier.name.as_str();
            if let Some(binding) = self.lookup(name) {
                self.check_tdz(binding, name, identifier.span)?;
                Target::Binding(binding)
            } else if let Some(read_only) = self.session.names().read_only(name) {
                Target::ReadOnly(read_only)
            } else {
                return Err(CompileError::new(
                    self.source_text(),
                    identifier.span,
                    format!("assignment to unknown binding {name:?}"),
                ));
            }
        } else if let Some(member) = expression.argument.as_member_expression() {
            let (object, key) = self.compile_member_parts(member)?;
            let key = if member.is_computed() {
                self.emit_to_property_key(key, Some(object))?
            } else {
                key
            };
            Target::Member(object, key)
        } else {
            return Err(unsupported(
                self.source_text(),
                expression.argument.span(),
                "update target outside the PoC subset",
            ));
        };

        let old = match &target {
            Target::ReadOnly(name) => self.emit_read_only(*name)?,
            Target::Member(object, key) => self.emit_get_member(*object, *key)?,
            Target::Binding(binding) => {
                let old = self.alloc_reg()?;
                self.emit_binding_read(old, *binding);
                old
            }
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
            Target::Binding(binding) => self.put_binding(*binding, updated),
            Target::ReadOnly(_) => self.emit(Instr::ThrowAssignmentToConst),
            Target::Member(object, key) => self.emit(Instr::SetMember {
                object: self.slot(*object),
                key: self.slot(*key),
                src: self.slot(updated),
            }),
        };
        Ok(if expression.prefix { updated } else { numeric })
    }

    pub(super) fn compile_member_parts(
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
        let key = self.emit_constant(Constant::String(member.property.name.as_str().into()))?;
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
                let ObjectPropertyKind::SpreadProperty(spread) = property else {
                    unreachable!()
                };
                let source = self.compile_expression(&spread.argument)?;
                self.emit(Instr::CopyDataProperties {
                    target: self.slot(dst),
                    source: self.slot(source),
                });
                continue;
            };
            let key = self.compile_property_key(&property.key, property.computed)?;
            let mut buffer = ryu_js::Buffer::new();
            let name = match &property.key {
                PropertyKey::NumericLiteral(literal) => Some(buffer.format(literal.value).into()),
                key => key.static_name().and_then(|name| name.into_cow_str()),
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
                let (prefix, name) = match (kind, name.as_deref()) {
                    (PropertyDefinition::Data, Some(name)) => ("", name),
                    (PropertyDefinition::Get, Some(name)) => ("get ", name),
                    (PropertyDefinition::Set, Some(name)) => ("set ", name),
                    (_, None) => ("", hint),
                };
                let id = self.enqueue_function(
                    FunctionSource::Method(function),
                    format_args!("{prefix}{name}"),
                    function.span,
                )?;
                self.emit_function_value(id)?
            } else {
                self.compile_named_expression(&property.value, name.as_deref().unwrap_or(""))?
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
        let dst = self.alloc_reg()?;
        self.emit(Instr::NewArray {
            dst: self.slot(dst),
            elements: Vec::new(),
        });
        for element in &array.elements {
            match element {
                ArrayExpressionElement::Elision(_) => {
                    self.emit(Instr::ArrayHole {
                        array: self.slot(dst),
                    });
                }
                ArrayExpressionElement::SpreadElement(spread) => {
                    self.emit_spread(dst, &spread.argument)?;
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
                    self.append_array_element(dst, value);
                }
            }
        }
        Ok(dst)
    }

    fn append_array_element(&mut self, array: Reg, value: Reg) {
        self.emit(Instr::ArrayAppend {
            array: self.slot(array),
            src: self.slot(value),
        });
    }

    fn emit_spread(
        &mut self,
        array: Reg,
        iterable: &'ast Expression<'ast>,
    ) -> Result<(), CompileError> {
        let iterable = self.compile_expression(iterable)?;
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
        self.append_array_element(array, value);
        self.jump_to(step_pc);
        let end = self.pc();
        self.patch_jump(exhausted, end)?;
        Ok(())
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

/// Decode the body of an Oxc-validated string or template token. Unicode
/// escape values go directly to UTF-16 so surrogate code units survive.
fn decode_string_body(body: &str, template: bool) -> Option<JsString> {
    if !body.contains('\\') && (!template || !body.contains('\r')) {
        return Some(body.into());
    }

    let mut units = Vec::new();
    let mut chars = body.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' if template => {
                let _ = chars.next_if_eq(&'\n');
                units.push(b'\n'.into());
            }
            '\\' => {
                let value = match chars.next()? {
                    '\r' => {
                        let _ = chars.next_if_eq(&'\n');
                        continue;
                    }
                    '\n' | '\u{2028}' | '\u{2029}' => continue,
                    'b' => 0x08,
                    'f' => 0x0c,
                    'n' => 0x0a,
                    'r' => 0x0d,
                    't' => 0x09,
                    'v' => 0x0b,
                    '0' if !chars.peek().is_some_and(char::is_ascii_digit) => 0,
                    '0'..='9' => return None, // Legacy escapes are invalid in modules.
                    'u' if chars.next_if_eq(&'{').is_some() => {
                        let mut value = chars.next()?.to_digit(16)?;
                        loop {
                            let digit = chars.next()?;
                            if digit == '}' {
                                break;
                            }
                            value = value * 16 + digit.to_digit(16)?;
                            if value > 0x10ffff {
                                return None;
                            }
                        }
                        value
                    }
                    escape @ ('x' | 'u') => {
                        let count = if escape == 'x' { 2 } else { 4 };
                        (0..count).try_fold(0, |value, _| {
                            Some(value * 16 + chars.next()?.to_digit(16)?)
                        })?
                    }
                    ch => ch as u32,
                };
                if value <= 0xffff {
                    units.push(value as u16);
                } else {
                    let pair = value - 0x10000;
                    units.push(0xd800 | (pair >> 10) as u16);
                    units.push(0xdc00 | (pair & 0x3ff) as u16);
                }
            }
            ch => units.extend_from_slice(ch.encode_utf16(&mut [0; 2])),
        }
    }
    Some(JsString::from_units(units))
}
