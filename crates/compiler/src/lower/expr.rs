//! Expression lowering. Names go through [`crate::names`]; await goes through
//! [`Control`](super::control::Control)'s resume table.

use oxc::ast::ast::*;
use oxc::span::GetSpan;
use oxc::syntax::operator::{
    AssignmentOperator, BinaryOperator, LogicalOperator, UnaryOperator, UpdateOperator,
};
use yield_vm::bytecode::{
    AccessorPrefix, BinaryOp, CallArgs, Constant, Instr, PropertyDefinition, UnaryOp,
};
use yield_vm::{BindingId, JsString};

use super::{FunctionSource, HOME_BINDING, Lowerer, NEW_TARGET_BINDING, THIS_BINDING};
use crate::emit::Reg;
use crate::error::{CompileError, unsupported};
use crate::names::{Callee, ReadOnlyName, ValueName, builtin_index};

#[derive(Clone, Copy)]
pub(super) enum PropertyRef {
    Member {
        object: Reg,
        key: Reg,
    },
    Super {
        base: Reg,
        key: Reg,
        receiver: Reg,
    },
    Private {
        object: Reg,
        brand: Reg,
        lexical: BindingId,
        index: u32,
    },
}

impl PropertyRef {
    fn receiver(self) -> Reg {
        match self {
            Self::Member { object, .. } | Self::Private { object, .. } => object,
            Self::Super { receiver, .. } => receiver,
        }
    }
}

enum GuardValue {
    Bool(bool),
    String(JsString),
}

impl GuardValue {
    fn truthy(&self) -> bool {
        match self {
            Self::Bool(value) => *value,
            Self::String(value) => !value.is_empty(),
        }
    }

    fn takes_rhs(&self, op: LogicalOperator) -> bool {
        match op {
            LogicalOperator::And => self.truthy(),
            LogicalOperator::Or => !self.truthy(),
            LogicalOperator::Coalesce => false,
        }
    }
}

impl<'ast, 'borrow, 'src> Lowerer<'ast, 'borrow, 'src> {
    pub(super) fn compile_named_expression(
        &mut self,
        expression: &'ast Expression<'ast>,
        name: &str,
    ) -> Result<Reg, CompileError> {
        self.compile_named_expression_with_key(expression, name, None)
    }

    pub(super) fn compile_named_expression_with_key(
        &mut self,
        mut expression: &'ast Expression<'ast>,
        name: &str,
        key: Option<Reg>,
    ) -> Result<Reg, CompileError> {
        while let Expression::ParenthesizedExpression(paren) = expression {
            expression = &paren.expression;
        }
        let function = match expression {
            Expression::ClassExpression(class) if class.id.is_none() => {
                return self.compile_class(class, name, key);
            }
            Expression::FunctionExpression(function) if function.id.is_none() => {
                let id = self.enqueue_named_function(
                    FunctionSource::Function(function),
                    "function",
                    name.to_owned(),
                    function.span,
                )?;
                self.emit_function_value(id)?
            }
            Expression::ArrowFunctionExpression(arrow) => {
                let id = self.enqueue_named_function(
                    FunctionSource::Arrow(arrow),
                    "arrow",
                    name.to_owned(),
                    arrow.span,
                )?;
                self.emit_function_value(id)?
            }
            _ => return self.compile_expression(expression),
        };
        if let Some(key) = key {
            self.emit_function_name(function, key, PropertyDefinition::Data);
        }
        Ok(function)
    }

    pub(super) fn emit_function_name(&mut self, function: Reg, key: Reg, kind: PropertyDefinition) {
        self.emit(Instr::SetFunctionName {
            function: self.slot(function),
            key: self.slot(key),
            prefix: match kind {
                PropertyDefinition::Data => None,
                PropertyDefinition::Get => Some(AccessorPrefix::Get),
                PropertyDefinition::Set => Some(AccessorPrefix::Set),
            },
        });
    }

    pub(super) fn guard_truth(&self, expression: &Expression<'_>) -> Option<bool> {
        Some(self.typeof_guard(expression)?.truthy())
    }

    fn typeof_guard(&self, expression: &Expression<'_>) -> Option<GuardValue> {
        if self.is_probe() {
            return None;
        }
        let (value, used_probe) = self.eval_guard(expression)?;
        used_probe.then_some(value)
    }

    // Track whether an actually evaluated operand used a missing-name fact.
    fn eval_guard(&self, expression: &Expression<'_>) -> Option<(GuardValue, bool)> {
        use GuardValue::{Bool, String};
        match expression.without_parentheses() {
            Expression::BooleanLiteral(literal) => Some((Bool(literal.value), false)),
            Expression::StringLiteral(literal) => {
                Some((String(self.string_literal_text(literal).ok()?), false))
            }
            Expression::UnaryExpression(unary) => match unary.operator {
                UnaryOperator::Typeof => {
                    let Expression::Identifier(id) = unary.argument.without_parentheses() else {
                        return None;
                    };
                    let name = id.name.as_str();
                    if !self.probed_missing(name) {
                        return None;
                    }
                    let folded = self.names().typeof_fold(&self.env, name)?;
                    Some((String(folded.into()), true))
                }
                UnaryOperator::LogicalNot => {
                    let (value, used_probe) = self.eval_guard(&unary.argument)?;
                    Some((Bool(!value.truthy()), used_probe))
                }
                _ => None,
            },
            Expression::BinaryExpression(binary)
                if matches!(
                    binary.left.without_parentheses(),
                    Expression::StringLiteral(_)
                ) || matches!(
                    binary.right.without_parentheses(),
                    Expression::StringLiteral(_)
                ) =>
            {
                let (String(left), left_probe) = self.eval_guard(&binary.left)? else {
                    return None;
                };
                let (String(right), right_probe) = self.eval_guard(&binary.right)? else {
                    return None;
                };
                // JsString orders by UTF-16 code units, not Unicode scalar values.
                let order = left.cmp(&right);
                let result = match binary.operator {
                    BinaryOperator::Equality | BinaryOperator::StrictEquality => order.is_eq(),
                    BinaryOperator::Inequality | BinaryOperator::StrictInequality => !order.is_eq(),
                    BinaryOperator::LessThan => order.is_lt(),
                    BinaryOperator::LessEqualThan => !order.is_gt(),
                    BinaryOperator::GreaterThan => order.is_gt(),
                    BinaryOperator::GreaterEqualThan => !order.is_lt(),
                    _ => return None,
                };
                Some((Bool(result), left_probe || right_probe))
            }
            Expression::LogicalExpression(logical) => {
                let (left, left_probe) = self.eval_guard(&logical.left)?;
                if left.takes_rhs(logical.operator) {
                    let (right, right_probe) = self.eval_guard(&logical.right)?;
                    Some((right, left_probe || right_probe))
                } else {
                    Some((left, left_probe))
                }
            }
            _ => None,
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
            Expression::BigIntLiteral(literal) => {
                let value =
                    yield_vm::JsBigInt::from_decimal(literal.value.as_str()).ok_or_else(|| {
                        unsupported(self.source_text(), literal.span, "BigInt literal")
                    })?;
                self.emit_constant(Constant::BigInt(value))
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
            Expression::PrivateInExpression(expression) => {
                let object = self.compile_expression(&expression.right)?;
                let (brand, lexical, index) = self.load_private_brand(&expression.left)?;
                let dst = self.alloc_reg()?;
                self.emit(Instr::HasPrivate {
                    dst: self.slot(dst),
                    object: self.slot(object),
                    brand: self.slot(brand),
                    index,
                    lexical,
                });
                Ok(dst)
            }
            Expression::LogicalExpression(expression) => self.compile_logical(expression),
            Expression::ConditionalExpression(expression) => {
                if let Some(test) = self.guard_truth(&expression.test) {
                    return self.compile_expression(if test {
                        &expression.consequent
                    } else {
                        &expression.alternate
                    });
                }
                let result = self.alloc_reg()?;
                let alternate_branch = self.compile_condition(&expression.test)?;
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
            Expression::YieldExpression(expression) => self.compile_yield(expression),
            Expression::CallExpression(expression) => self.compile_call(expression),
            Expression::ChainExpression(expression) => Ok(self.compile_chain(expression, false)?.0),
            Expression::AssignmentExpression(expression) => self.compile_assignment(expression),
            Expression::UpdateExpression(expression) => self.compile_update(expression),
            Expression::ClassExpression(class) => self.compile_class(class, "", None),
            Expression::ObjectExpression(expression) => self.compile_object(expression),
            Expression::ArrayExpression(expression) => self.compile_array(expression),
            expression if let Some(member) = expression.as_member_expression() => {
                Ok(self.compile_member_read(member, None)?.0)
            }
            Expression::NewExpression(expression) => self.compile_new(expression),
            Expression::ThisExpression(_) => self.compile_this(),
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
            Expression::TaggedTemplateExpression(tagged) => self.compile_tagged_template(tagged),
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

    /// `yield` and `yield*`. The operand is evaluated exactly once, before the
    /// suspension, so a resume never re-runs it.
    fn compile_yield(
        &mut self,
        expression: &'ast YieldExpression<'ast>,
    ) -> Result<Reg, CompileError> {
        if !self.is_generator {
            return Err(unsupported(
                self.source_text(),
                expression.span,
                "yield outside a generator function",
            ));
        }
        let argument = match &expression.argument {
            Some(argument) => self.compile_expression(argument)?,
            None => self.emit_undefined()?,
        };
        if expression.delegate {
            return self.emit_yield_delegate(argument);
        }
        self.emit_yield(argument)
    }

    /// Lower ``tag`text${expr}` ``: evaluate the tag reference, load this call
    /// site's cached frozen template object, then evaluate substitutions in
    /// source order and call with the template object as the first argument.
    fn compile_tagged_template(
        &mut self,
        expression: &'ast TaggedTemplateExpression<'ast>,
    ) -> Result<Reg, CompileError> {
        let mut tag = &expression.tag;
        while let Expression::ParenthesizedExpression(paren) = tag {
            tag = &paren.expression;
        }
        let (callee, receiver) = match tag {
            Expression::Identifier(identifier) => {
                (self.compile_named_callee(identifier, false)?, None)
            }
            tag => self.compile_callee_value(tag)?,
        };
        let template = self.compile_template_site(&expression.quasi)?;
        let mut slots = Vec::with_capacity(expression.quasi.expressions.len() + 1);
        slots.push(self.slot(template));
        for value in &expression.quasi.expressions {
            let value = self.compile_expression(value)?;
            slots.push(self.slot(value));
        }
        let dst = self.alloc_reg()?;
        self.emit(Instr::CallValue {
            dst: self.slot(dst),
            callee: self.slot(callee),
            args: CallArgs::Slots(slots.into()),
            receiver: receiver.map(|receiver| self.slot(receiver)),
        });
        Ok(dst)
    }

    fn compile_template_site(
        &mut self,
        template: &'ast TemplateLiteral<'ast>,
    ) -> Result<Reg, CompileError> {
        if template.quasis.len() != template.expressions.len() + 1 {
            return Err(unsupported(
                self.source_text(),
                template.span,
                "malformed template literal",
            ));
        }
        let mut cooked = Vec::with_capacity(template.quasis.len());
        let mut raw = Vec::with_capacity(template.quasis.len());
        for element in &template.quasis {
            raw.push(element.value.raw.as_str().into());
            cooked.push(
                element
                    .value
                    .cooked
                    .map(|text| JsString::from_units(text.encode_utf16().collect())),
            );
        }
        let template = self.session.add_template(cooked, raw)?;
        let dst = self.alloc_reg()?;
        self.emit(Instr::GetTemplate {
            dst: self.slot(dst),
            template,
        });
        Ok(dst)
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
    pub(super) fn emit_missing_global(&mut self, name: &str) -> Result<Reg, CompileError> {
        let constructor = self.alloc_reg()?;
        self.emit(Instr::GetBuiltin {
            dst: self.slot(constructor),
            builtin: builtin_index("ReferenceError").expect("ReferenceError builtin") as u32,
        });
        let message =
            self.emit_constant(Constant::String(format!("{name} is not defined").into()))?;
        let error = self.alloc_reg()?;
        self.emit(Instr::Construct {
            dst: self.slot(error),
            callee: self.slot(constructor),
            args: CallArgs::Slots(vec![self.slot(message)].into()),
        });
        self.emit(Instr::Throw {
            src: self.slot(error),
        });
        Ok(error)
    }

    pub(super) fn static_key_name(
        &self,
        key: &PropertyKey<'_>,
    ) -> Result<Option<String>, CompileError> {
        Ok(match key {
            PropertyKey::PrivateIdentifier(id) => Some(format!("#{}", id.name)),
            PropertyKey::StringLiteral(literal) => {
                self.string_literal_text(literal)?.to_utf8().ok()
            }
            PropertyKey::NumericLiteral(literal) => {
                Some(ryu_js::Buffer::new().format(literal.value).to_owned())
            }
            key => key
                .static_name()
                .and_then(|name| name.into_cow_str())
                .map(|name| name.into_owned()),
        })
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
        } else if let PropertyKey::BigIntLiteral(literal) = key {
            self.emit_constant(Constant::String(literal.value.as_str().into()))
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
        match self.names().value(&self.env, name) {
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
            ValueName::Unknown if self.probed_missing(name) => self.emit_missing_global(name),
            ValueName::Unknown if self.is_probe() => self.emit_undefined(),
            ValueName::Unknown => Err(CompileError::new(
                self.source_text(),
                identifier.span,
                format!("unknown binding {name:?}"),
            )),
        }
    }

    fn emit_read_only(&mut self, name: ReadOnlyName<'_>) -> Result<Reg, CompileError> {
        match name {
            ReadOnlyName::Import(target) => {
                let dst = self.alloc_reg()?;
                self.emit(crate::session::read_target(self.slot(dst), target));
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
        if let Some(constant) = self.binary_constant(&expression.right)? {
            let constant = self.session.intern(constant);
            let dst = self.alloc_reg()?;
            self.emit(Instr::BinaryConst {
                dst: self.slot(dst),
                op: operator,
                lhs: self.slot(lhs),
                constant,
            });
            return Ok(dst);
        }
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

    /// Emits a conditional branch, fusing a direct comparison with its
    /// branch. The comparison never writes a result slot, so conditions such
    /// as `i < n` need neither a `Const` nor a boolean destination.
    pub(super) fn compile_condition(
        &mut self,
        expression: &'ast Expression<'ast>,
    ) -> Result<usize, CompileError> {
        let expression = expression.without_parentheses();
        if let Expression::BinaryExpression(binary) = expression
            && let Some(op) = comparison_operator(binary.operator)
        {
            let lhs = self.compile_expression(&binary.left)?;
            if let Some(constant) = self.binary_constant(&binary.right)? {
                let constant = self.session.intern(constant);
                return Ok(self.branch_compare_const_false(lhs, op, constant));
            }
            let rhs = self.compile_expression(&binary.right)?;
            return Ok(self.branch_compare_false(lhs, op, rhs));
        }
        let value = self.compile_expression(expression)?;
        Ok(self.branch_false(value))
    }

    /// A literal right operand has no evaluation side effects and can be
    /// carried by the instruction instead of a separate `Const`.
    fn binary_constant(
        &self,
        expression: &Expression<'ast>,
    ) -> Result<Option<Constant>, CompileError> {
        Ok(Some(match expression.without_parentheses() {
            Expression::BooleanLiteral(literal) => Constant::Bool(literal.value),
            Expression::NullLiteral(_) => Constant::Null,
            Expression::NumericLiteral(literal) => Constant::Number(literal.value),
            Expression::BigIntLiteral(literal) => {
                let value =
                    yield_vm::JsBigInt::from_decimal(literal.value.as_str()).ok_or_else(|| {
                        unsupported(self.source_text(), literal.span, "BigInt literal")
                    })?;
                Constant::BigInt(value)
            }
            Expression::StringLiteral(literal) => {
                Constant::String(self.string_literal_text(literal)?)
            }
            _ => return Ok(None),
        }))
    }

    fn compile_logical(
        &mut self,
        expression: &'ast LogicalExpression<'ast>,
    ) -> Result<Reg, CompileError> {
        if let Some(left) = self.typeof_guard(&expression.left) {
            if left.takes_rhs(expression.operator) {
                return self.compile_expression(&expression.right);
            }
            return self.emit_constant(match left {
                GuardValue::Bool(value) => Constant::Bool(value),
                GuardValue::String(value) => Constant::String(value),
            });
        }
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
        if matches!(expression.operator, UnaryOperator::Typeof) {
            let mut argument = &expression.argument;
            while let Expression::ParenthesizedExpression(paren) = argument {
                argument = &paren.expression;
            }
            if let Expression::Identifier(identifier) = argument {
                let name = identifier.name.as_str();
                if let Some(binding) = self.lookup(name) {
                    self.check_tdz(binding, name, identifier.span)?;
                }
                if let Some(folded) = self.names().typeof_fold(&self.env, name) {
                    self.record_typeof_fold(name);
                    return self.emit_constant(Constant::String(folded.into()));
                }
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
            self.check_delete_member(member)?;
            let reference = self.compile_member_ref(member, None)?;
            return self.emit_delete_property(reference);
        }
        if let Expression::ChainExpression(chain) = argument
            && let Some(member) = chain.expression.as_member_expression()
        {
            self.check_delete_member(member)?;
            let mut exits = Vec::new();
            let reference = self.compile_member_ref(member, Some(&mut exits))?;
            let result = self.emit_delete_property(reference)?;
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

    fn check_delete_member(&self, member: &MemberExpression<'_>) -> Result<(), CompileError> {
        if let MemberExpression::PrivateFieldExpression(member) = member {
            return Err(CompileError::new(
                self.source_text(),
                member.field.span,
                "cannot delete a private element",
            ));
        }
        Ok(())
    }

    fn emit_delete_property(&mut self, reference: PropertyRef) -> Result<Reg, CompileError> {
        let PropertyRef::Member { object, key } = reference else {
            self.emit(Instr::ThrowDeleteSuper);
            return self.emit_undefined();
        };
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
            let (value, object) = self.compile_member_read(member, Some(&mut exits))?;
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
            let (value, receiver) = self.compile_member_read(member, Some(exits))?;
            Ok((value, Some(receiver)))
        } else {
            Ok((self.compile_expression(expression)?, None))
        }
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

    fn emit_get_property(&mut self, reference: PropertyRef) -> Result<Reg, CompileError> {
        let (base, key, receiver) = match reference {
            PropertyRef::Member { object, key } => return self.emit_get_member(object, key),
            PropertyRef::Private {
                object,
                brand,
                index,
                lexical,
            } => {
                let dst = self.alloc_reg()?;
                self.emit(Instr::GetPrivate {
                    dst: self.slot(dst),
                    object: self.slot(object),
                    brand: self.slot(brand),
                    index,
                    lexical,
                });
                return Ok(dst);
            }
            PropertyRef::Super {
                base,
                key,
                receiver,
            } => (base, key, receiver),
        };
        let dst = self.alloc_reg()?;
        self.emit(Instr::GetSuper {
            dst: self.slot(dst),
            base: self.slot(base),
            key: self.slot(key),
            receiver: self.slot(receiver),
        });
        Ok(dst)
    }

    pub(super) fn emit_set_property(&mut self, reference: PropertyRef, value: Reg) -> usize {
        let src = self.slot(value);
        self.emit(match reference {
            PropertyRef::Member { object, key } => Instr::SetMember {
                object: self.slot(object),
                key: self.slot(key),
                src,
            },
            PropertyRef::Private {
                object,
                brand,
                index,
                lexical,
            } => Instr::SetPrivate {
                object: self.slot(object),
                brand: self.slot(brand),
                src,
                index,
                lexical,
            },
            PropertyRef::Super {
                base,
                key,
                receiver,
            } => Instr::SetSuper {
                base: self.slot(base),
                key: self.slot(key),
                receiver: self.slot(receiver),
                src,
            },
        })
    }

    // Property instructions convert their keys. Save the converted key
    // explicitly when the same reference serves both a read and a write.
    fn prepare_read_write_ref(
        &mut self,
        reference: PropertyRef,
        computed: bool,
    ) -> Result<PropertyRef, CompileError> {
        match reference {
            PropertyRef::Member { object, key } if computed => Ok(PropertyRef::Member {
                object,
                key: self.emit_to_property_key(key, Some(object))?,
            }),
            PropertyRef::Super {
                base,
                key,
                receiver,
            } if computed => Ok(PropertyRef::Super {
                base,
                key: self.emit_to_property_key(key, Some(base))?,
                receiver,
            }),
            _ => Ok(reference),
        }
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
        if let Expression::Super(super_expr) = callee {
            if expression.optional {
                return Err(CompileError::new(
                    self.source_text(),
                    super_expr.span,
                    "optional super() call",
                ));
            }
            if !self.derived_this {
                return Err(CompileError::new(
                    self.source_text(),
                    super_expr.span,
                    "super() is only allowed in a derived constructor or its lexical arrows",
                ));
            }
            let context = self.load_derived_context()?;
            let callee = self.alloc_reg()?;
            self.emit(Instr::GetSuperConstructor {
                dst: self.slot(callee),
                context: self.slot(context),
            });
            let args = self.compile_arguments(&expression.arguments)?;
            let dst = self.alloc_reg()?;
            self.emit(Instr::SuperConstruct {
                dst: self.slot(dst),
                context: self.slot(context),
                callee: self.slot(callee),
                args,
            });
            return Ok(dst);
        }

        let (callee, receiver) = if let Expression::Identifier(identifier) = callee {
            (
                self.compile_named_callee(identifier, expression.optional)?,
                None,
            )
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

    fn compile_named_callee(
        &mut self,
        identifier: &'ast IdentifierReference<'ast>,
        optional: bool,
    ) -> Result<Reg, CompileError> {
        let name = identifier.name.as_str();
        match self.names().callee(&self.env, name) {
            Callee::Binding(binding) => {
                self.check_tdz(binding, name, identifier.span)?;
                let value = self.alloc_reg()?;
                self.emit_binding_read(value, binding);
                Ok(value)
            }
            Callee::ReadOnly(read_only) => self.emit_read_only(read_only),
            Callee::Builtin(index) => {
                let value = self.alloc_reg()?;
                self.emit(Instr::GetBuiltin {
                    dst: self.slot(value),
                    builtin: index as u32,
                });
                Ok(value)
            }
            Callee::Unknown if optional && name == "undefined" => {
                self.compile_identifier(identifier)
            }
            Callee::Unknown if self.probed_missing(name) => self.emit_missing_global(name),
            Callee::Unknown if self.is_probe() => self.emit_undefined(),
            Callee::Unknown => Err(CompileError::new(
                self.source_text(),
                identifier.span,
                format!("unknown callee {name:?}"),
            )),
        }
    }

    fn compile_callee_value(
        &mut self,
        callee: &'ast Expression<'ast>,
    ) -> Result<(Reg, Option<Reg>), CompileError> {
        if let Expression::ChainExpression(chain) = callee {
            self.compile_chain(chain, true)
        } else if let Some(member) = callee.as_member_expression() {
            let (value, receiver) = self.compile_member_read(member, None)?;
            Ok((value, Some(receiver)))
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
            None => CallArgs::Slots(slots.into()),
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

    fn load_derived_context(&mut self) -> Result<Reg, CompileError> {
        let binding = self
            .lookup(THIS_BINDING)
            .expect("derived this binding installed");
        let context = self.alloc_reg()?;
        self.emit_binding_read(context, binding);
        Ok(context)
    }

    fn compile_this(&mut self) -> Result<Reg, CompileError> {
        if self.derived_this {
            let context = self.load_derived_context()?;
            let dst = self.alloc_reg()?;
            self.emit(Instr::GetDerivedThis {
                dst: self.slot(dst),
                context: self.slot(context),
            });
            return Ok(dst);
        }
        let Some(binding) = self.lookup(THIS_BINDING) else {
            return self.emit_undefined();
        };
        let dst = self.alloc_reg()?;
        self.emit_binding_read(dst, binding);
        Ok(dst)
    }

    fn compile_assignment(
        &mut self,
        expression: &'ast AssignmentExpression<'ast>,
    ) -> Result<Reg, CompileError> {
        if let AssignmentTarget::AssignmentTargetIdentifier(identifier) = &expression.left {
            let name = identifier.name.as_str();
            let Some(binding) = self.lookup(name) else {
                if let Some(read_only) = self.names().read_only(name) {
                    return self.compile_read_only_assignment(expression, read_only);
                }
                if self.probed_missing(name) {
                    if matches!(expression.operator, AssignmentOperator::Assign) {
                        self.compile_named_expression(&expression.right, name)?;
                    }
                    return self.emit_missing_global(name);
                }
                if self.is_probe() {
                    return self.compile_named_expression(&expression.right, name);
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
            let mut reference = self.compile_member_ref(member, None)?;
            if expression.operator != AssignmentOperator::Assign {
                reference = self.prepare_read_write_ref(reference, member.is_computed())?;
            }
            if let Some(op) = logical_assignment_operator(expression.operator) {
                let result = self.emit_get_property(reference)?;
                let end = self.begin_logical_rhs(op, result)?;
                let right = self.compile_expression(&expression.right)?;
                self.emit_set_property(reference, right);
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
                let old = self.emit_get_property(reference)?;
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
            self.emit_set_property(reference, value);
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
            Member(PropertyRef),
            ProbeMissing,
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
            } else if let Some(read_only) = self.names().read_only(name) {
                Target::ReadOnly(read_only)
            } else if self.probed_missing(name) {
                return self.emit_missing_global(name);
            } else if self.is_probe() {
                Target::ProbeMissing
            } else {
                return Err(CompileError::new(
                    self.source_text(),
                    identifier.span,
                    format!("assignment to unknown binding {name:?}"),
                ));
            }
        } else if let Some(member) = expression.argument.as_member_expression() {
            let reference = self.compile_member_ref(member, None)?;
            Target::Member(self.prepare_read_write_ref(reference, member.is_computed())?)
        } else {
            return Err(unsupported(
                self.source_text(),
                expression.argument.span(),
                "update target outside the PoC subset",
            ));
        };

        let old = match &target {
            Target::ReadOnly(name) => self.emit_read_only(*name)?,
            Target::Member(reference) => self.emit_get_property(*reference)?,
            Target::Binding(binding) => {
                let old = self.alloc_reg()?;
                self.emit_binding_read(old, *binding);
                old
            }
            Target::ProbeMissing => self.emit_undefined()?,
        };
        let numeric = self.alloc_reg()?;
        self.emit(Instr::Unary {
            dst: self.slot(numeric),
            op: UnaryOp::ToNumeric,
            src: self.slot(old),
        });
        let op = match expression.operator {
            UpdateOperator::Increment => UnaryOp::Increment,
            UpdateOperator::Decrement => UnaryOp::Decrement,
        };
        let updated = self.alloc_reg()?;
        self.emit(Instr::Unary {
            dst: self.slot(updated),
            op,
            src: self.slot(numeric),
        });
        match &target {
            Target::Binding(binding) => self.put_binding(*binding, updated),
            Target::ReadOnly(_) => self.emit(Instr::ThrowAssignmentToConst),
            Target::Member(reference) => self.emit_set_property(*reference, updated),
            Target::ProbeMissing => 0,
        };
        Ok(if expression.prefix { updated } else { numeric })
    }

    fn compile_member_read(
        &mut self,
        member: &'ast MemberExpression<'ast>,
        exits: Option<&mut Vec<usize>>,
    ) -> Result<(Reg, Reg), CompileError> {
        let key = match member {
            MemberExpression::StaticMemberExpression(member)
                if !matches!(member.object, Expression::Super(_)) =>
            {
                Some(member.property.name.as_str().into())
            }
            MemberExpression::ComputedMemberExpression(member)
                if !matches!(member.object, Expression::Super(_)) =>
            {
                match member.expression.without_parentheses() {
                    Expression::StringLiteral(literal) => Some(self.string_literal_text(literal)?),
                    Expression::TemplateLiteral(template) if template.expressions.is_empty() => {
                        template
                            .quasis
                            .first()
                            .map(|element| self.template_element_text(element))
                            .transpose()?
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(key) = key {
            let object = self.compile_member_object(member.object(), member.optional(), exits)?;
            let key = self.session.intern(Constant::String(key));
            let dst = self.alloc_reg()?;
            self.emit(Instr::GetMemberConstKey {
                dst: self.slot(dst),
                object: self.slot(object),
                key,
            });
            return Ok((dst, object));
        }

        let reference = self.compile_member_ref(member, exits)?;
        Ok((self.emit_get_property(reference)?, reference.receiver()))
    }

    pub(super) fn compile_member_ref(
        &mut self,
        member: &'ast MemberExpression<'ast>,
        exits: Option<&mut Vec<usize>>,
    ) -> Result<PropertyRef, CompileError> {
        match member {
            MemberExpression::StaticMemberExpression(member) => {
                self.compile_static_member(member, exits)
            }
            MemberExpression::ComputedMemberExpression(member) => {
                self.compile_computed_member(member, exits)
            }
            MemberExpression::PrivateFieldExpression(member) => {
                if matches!(member.object, Expression::Super(_)) {
                    return Err(CompileError::new(
                        self.source_text(),
                        member.span,
                        "cannot access a private element on super",
                    ));
                }
                let object = self.compile_member_object(&member.object, member.optional, exits)?;
                let (brand, lexical, index) = self.load_private_brand(&member.field)?;
                Ok(PropertyRef::Private {
                    object,
                    brand,
                    index,
                    lexical,
                })
            }
        }
    }

    fn compile_static_member(
        &mut self,
        member: &'ast StaticMemberExpression<'ast>,
        exits: Option<&mut Vec<usize>>,
    ) -> Result<PropertyRef, CompileError> {
        self.compile_property_ref(&member.object, member.optional, exits, |this| {
            this.emit_constant(Constant::String(member.property.name.as_str().into()))
        })
    }

    fn compile_computed_member(
        &mut self,
        member: &'ast ComputedMemberExpression<'ast>,
        exits: Option<&mut Vec<usize>>,
    ) -> Result<PropertyRef, CompileError> {
        self.compile_property_ref(&member.object, member.optional, exits, |this| {
            this.compile_expression(&member.expression)
        })
    }

    fn compile_property_ref(
        &mut self,
        object: &'ast Expression<'ast>,
        optional: bool,
        exits: Option<&mut Vec<usize>>,
        compile_key: impl FnOnce(&mut Self) -> Result<Reg, CompileError>,
    ) -> Result<PropertyRef, CompileError> {
        if matches!(object, Expression::Super(_)) {
            if optional {
                return Err(CompileError::new(
                    self.source_text(),
                    object.span(),
                    "optional access on super",
                ));
            }
            let Some(binding) = self.lookup(HOME_BINDING) else {
                return Err(CompileError::new(
                    self.source_text(),
                    object.span(),
                    "super property access outside a method or class initializer",
                ));
            };
            let receiver = self.compile_this()?;
            let key = compile_key(self)?;
            // Keep the raw key: a plain assignment converts it after the RHS,
            // and delete super fails without converting it at all.
            let home = self.alloc_reg()?;
            self.emit_binding_read(home, binding);
            let base = self.alloc_reg()?;
            self.emit(Instr::GetSuperBase {
                dst: self.slot(base),
                home: self.slot(home),
            });
            return Ok(PropertyRef::Super {
                base,
                key,
                receiver,
            });
        }

        let object = self.compile_member_object(object, optional, exits)?;
        let key = compile_key(self)?;
        Ok(PropertyRef::Member { object, key })
    }

    fn compile_member_object(
        &mut self,
        object: &'ast Expression<'ast>,
        optional: bool,
        exits: Option<&mut Vec<usize>>,
    ) -> Result<Reg, CompileError> {
        if let Some(exits) = exits {
            let (object, _) = self.compile_chain_link(object, exits)?;
            if optional {
                self.guard_chain_link(object, exits)?;
            }
            Ok(object)
        } else {
            if optional {
                return Err(unsupported(
                    self.source_text(),
                    object.span(),
                    "optional member access",
                ));
            }
            self.compile_expression(object)
        }
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
            let name = self.static_key_name(&property.key)?;
            let name_from_key = property.computed || name.is_none();
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
                let initial_name = if name_from_key {
                    String::new()
                } else {
                    format!("{prefix}{name}")
                };
                let id = self.enqueue_named_function(
                    FunctionSource::Method(function),
                    format_args!("{prefix}{name}"),
                    initial_name,
                    function.span,
                )?;
                let function = self.emit_function_value(id)?;
                if name_from_key {
                    self.emit_function_name(function, key, kind);
                }
                self.emit(Instr::SetHome {
                    function: self.slot(function),
                    home: self.slot(dst),
                });
                function
            } else {
                let name = if name_from_key { None } else { name.as_deref() };
                self.compile_named_expression_with_key(
                    &property.value,
                    name.unwrap_or(""),
                    name_from_key.then_some(key),
                )?
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

fn comparison_operator(operator: BinaryOperator) -> Option<BinaryOp> {
    Some(match operator {
        BinaryOperator::LessThan => BinaryOp::Lt,
        BinaryOperator::LessEqualThan => BinaryOp::Le,
        BinaryOperator::GreaterThan => BinaryOp::Gt,
        BinaryOperator::GreaterEqualThan => BinaryOp::Ge,
        BinaryOperator::Equality => BinaryOp::Eq,
        BinaryOperator::Inequality => BinaryOp::Ne,
        BinaryOperator::StrictEquality => BinaryOp::StrictEq,
        BinaryOperator::StrictInequality => BinaryOp::StrictNe,
        _ => return None,
    })
}
