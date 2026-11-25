//! Lower JSX directly to the JavaScript AST used by the linker and lowerer.

use std::collections::HashSet;

use oxc::allocator::{Allocator, ArenaVec, TakeIn};
use oxc::ast::{ast::*, builder::AstBuilder};
use oxc::ast_visit::{Visit, VisitMut, walk_mut};
use oxc::span::{SPAN, Span};
use oxc::str::{Ident, JSStr, JSStrBuilder};
use oxc::syntax::identifier::is_white_space_single_line;
use oxc::syntax::xml_entities::decode_entities;

use crate::{CompileError, ParsedModule};

pub(crate) const STAGE: &str = "JSX transform";
// Keep generated JSX imports distinguishable from other unlocated imports.
pub(crate) const IMPORT_SPAN: Span = Span::new(u32::MAX, u32::MAX);

pub(crate) fn lower(module: &mut ParsedModule<'_>, runtime: &str) -> Result<(), CompileError> {
    if !module.program.source_type.is_jsx() {
        return Ok(());
    }
    let mut names = Names::default();
    names.visit_program(&module.program);
    let mut pass = Lower {
        ast: AstBuilder::new(module.allocator),
        allocator: module.allocator,
        names,
        next: 0,
        imports: [None; 3],
        source: module.program.source_text,
        error: None,
    };
    pass.visit_program(&mut module.program);
    let fail = |error: CompileError| error.in_module(&module.id).in_stage(STAGE);
    if let Some(error) = pass.error {
        return Err(fail(error));
    }
    if pass.imports.iter().all(Option::is_none) {
        return Ok(());
    }
    if runtime.is_empty() {
        return Err(fail(CompileError::internal("empty JSX runtime package")));
    }
    let specifiers = ["jsx", "jsxs", "Fragment"]
        .into_iter()
        .zip(pass.imports)
        .filter_map(|(export, local)| {
            Some(ImportDeclarationSpecifier::new_import_specifier(
                SPAN,
                ModuleExportName::new_identifier_name(IMPORT_SPAN, export, &pass.ast),
                BindingIdentifier::new(SPAN, local?, &pass.ast),
                ImportOrExportKind::Value,
                &pass.ast,
            ))
        });
    let specifiers = ArenaVec::from_iter_in(specifiers, &pass.ast);
    let source = StringLiteral::new(
        IMPORT_SPAN,
        JSStr::from_str_in(&format!("{runtime}/jsx-runtime"), &pass.ast),
        None,
        &pass.ast,
    );
    module.program.body.push(Statement::new_import_declaration(
        SPAN,
        Some(specifiers),
        source,
        None,
        None,
        ImportOrExportKind::Value,
        &pass.ast,
    ));
    Ok(())
}

#[derive(Default)]
struct Names<'a>(HashSet<&'a str>);

impl<'a> Visit<'a> for Names<'a> {
    fn visit_identifier_reference(&mut self, node: &IdentifierReference<'a>) {
        self.0.insert(node.name.as_str());
    }

    fn visit_binding_identifier(&mut self, node: &BindingIdentifier<'a>) {
        self.0.insert(node.name.as_str());
    }
}

struct Lower<'a> {
    ast: AstBuilder<'a>,
    allocator: &'a Allocator,
    names: Names<'a>,
    next: usize,
    imports: [Option<Ident<'a>>; 3],
    source: &'a str,
    error: Option<CompileError>,
}

impl<'a> VisitMut<'a> for Lower<'a> {
    fn visit_expression(&mut self, node: &mut Expression<'a>) {
        // Walk normal JS expressions inside the JSX first. Direct JSX children
        // and JSX-valued attributes are converted when consuming their parent.
        walk_mut::walk_expression(self, node);
        if self.error.is_some() {
            return;
        }
        if matches!(node, Expression::JSXElement(_) | Expression::JSXFragment(_)) {
            let result = match node.take_in(&self.ast) {
                Expression::JSXElement(element) => self.element(element.unbox()),
                Expression::JSXFragment(fragment) => self.fragment(fragment.unbox()),
                _ => unreachable!(),
            };
            match result {
                Ok(expression) => *node = expression,
                Err(error) => self.error = Some(error),
            }
        }
    }
}

impl<'a> Lower<'a> {
    fn fresh(&mut self) -> Ident<'a> {
        loop {
            let name = format!("$jsx{}", self.next);
            self.next += 1;
            if !self.names.0.contains(name.as_str()) {
                return Ident::from_str_in(&name, &self.ast);
            }
        }
    }

    fn import(&mut self, index: usize) -> Expression<'a> {
        let name = match self.imports[index] {
            Some(name) => name,
            None => {
                let name = self.fresh();
                self.imports[index] = Some(name);
                name
            }
        };
        Expression::new_identifier(SPAN, name, &self.ast)
    }

    fn unsupported(&self, span: Span, feature: &str) -> CompileError {
        crate::error::unsupported(self.source, span, feature)
    }

    fn element(&mut self, element: JSXElement<'a>) -> Result<Expression<'a>, CompileError> {
        let opening = element.opening_element.unbox();
        let tag = match opening.name {
            JSXElementName::Identifier(name) => {
                Expression::new_string_literal(name.span, name.name, None, &self.ast)
            }
            JSXElementName::IdentifierReference(name) => Expression::Identifier(name),
            JSXElementName::ThisExpression(value) => Expression::ThisExpression(value),
            JSXElementName::MemberExpression(member) => self.member(member.unbox()),
            JSXElementName::NamespacedName(name) => {
                return Err(self.unsupported(name.span, "JSX namespace"));
            }
        };
        let mut props = ArenaVec::with_capacity_in(opening.attributes.len() + 1, &self.ast);
        let mut keyed = false;
        for attribute in opening.attributes {
            match attribute {
                JSXAttributeItem::SpreadAttribute(spread) => {
                    let spread = spread.unbox();
                    props.push(ObjectPropertyKind::new_spread_property(
                        spread.span,
                        spread.argument,
                        &self.ast,
                    ));
                }
                JSXAttributeItem::Attribute(attribute) => {
                    let attribute = attribute.unbox();
                    let name = match attribute.name {
                        JSXAttributeName::Identifier(name) => name,
                        JSXAttributeName::NamespacedName(name) => {
                            return Err(self.unsupported(name.span, "JSX namespace"));
                        }
                    };
                    keyed |= name.name.as_str() == "key";
                    let value = match attribute.value {
                        None => Expression::new_boolean_literal(attribute.span, true, &self.ast),
                        Some(JSXAttributeValue::StringLiteral(value)) => {
                            let text = value.value.as_str().expect("parsed JSX string is UTF-8");
                            Expression::new_string_literal(
                                value.span,
                                self.decode(text),
                                None,
                                &self.ast,
                            )
                        }
                        Some(JSXAttributeValue::ExpressionContainer(value)) => {
                            value.unbox().expression.into_expression()
                        }
                        Some(JSXAttributeValue::Element(value)) => self.element(value.unbox())?,
                        Some(JSXAttributeValue::Fragment(value)) => self.fragment(value.unbox())?,
                    };
                    props.push(self.prop(attribute.span, name.name.as_str(), value));
                }
            }
        }
        self.call(element.span, tag, props, element.children, keyed)
    }

    fn member(&self, member: JSXMemberExpression<'a>) -> Expression<'a> {
        let object = match member.object {
            JSXMemberExpressionObject::IdentifierReference(name) => Expression::Identifier(name),
            JSXMemberExpressionObject::ThisExpression(value) => Expression::ThisExpression(value),
            JSXMemberExpressionObject::MemberExpression(inner) => self.member(inner.unbox()),
        };
        Expression::new_static_member_expression(
            member.span,
            object,
            IdentifierName::new(
                member.property.span,
                member.property.name.as_str(),
                &self.ast,
            ),
            false,
            &self.ast,
        )
    }

    fn fragment(&mut self, fragment: JSXFragment<'a>) -> Result<Expression<'a>, CompileError> {
        let tag = self.import(2);
        let props = ArenaVec::new_in(&self.ast);
        self.call(fragment.span, tag, props, fragment.children, false)
    }

    fn prop(&self, span: Span, name: &'a str, value: Expression<'a>) -> ObjectPropertyKind<'a> {
        let key = PropertyKey::new_string_literal(span, name, None, &self.ast);
        // A JSX prop named __proto__ is data, not an object-literal prototype setter.
        ObjectPropertyKind::new_object_property(
            span,
            PropertyKind::Init,
            key,
            value,
            false,
            false,
            name == "__proto__",
            &self.ast,
        )
    }

    fn call(
        &mut self,
        span: Span,
        tag: Expression<'a>,
        mut props: ArenaVec<'a, ObjectPropertyKind<'a>>,
        children: ArenaVec<'a, JSXChild<'a>>,
        keyed: bool,
    ) -> Result<Expression<'a>, CompileError> {
        let mut values = ArenaVec::new_in(&self.ast);
        for child in children {
            let value = match child {
                JSXChild::Text(text) => {
                    let Some(value) = self.text(text.value.as_str()) else {
                        continue;
                    };
                    Expression::new_string_literal(text.span, value, None, &self.ast)
                }
                JSXChild::ExpressionContainer(value) => match value.unbox().expression {
                    JSXExpression::EmptyExpression(_) => continue,
                    value => value.into_expression(),
                },
                JSXChild::Element(value) => self.element(value.unbox())?,
                JSXChild::Fragment(value) => self.fragment(value.unbox())?,
                JSXChild::Spread(value) => {
                    return Err(self.unsupported(value.span, "JSX spread child"));
                }
            };
            values.push(ArrayExpressionElement::from(value));
        }
        let multiple = values.len() > 1;
        if multiple {
            props.push(self.prop(
                span,
                "children",
                Expression::new_array_expression(span, values, &self.ast),
            ));
        } else if let Some(value) = values.pop() {
            props.push(self.prop(span, "children", value.into_expression()));
        }
        let callee = self.import(usize::from(multiple));
        let object = Expression::new_object_expression(span, props, &self.ast);
        let args =
            ArenaVec::from_array_in([Argument::from(tag), Argument::from(object)], &self.ast);
        let callee = if keyed {
            self.key_adapter(callee)
        } else {
            callee
        };
        Ok(Expression::new_call_expression(
            span, callee, None, args, false, &self.ast,
        ))
    }

    // Evaluate the tag and the full object in the caller: spreads and explicit
    // keys interleave naturally, and await/yield/this remain in their original
    // context. Then split the final own key off the fresh props object. Without
    // an explicit key, spread-only keys pass straight through to the runtime.
    fn key_adapter(&mut self, callee: Expression<'a>) -> Expression<'a> {
        let tag = self.fresh();
        let key = self.fresh();
        let props = self.fresh();
        let ast = &self.ast;
        let binding = |name| BindingPattern::new_binding_identifier(SPAN, name, ast);
        let property = BindingProperty::new(
            SPAN,
            PropertyKey::new_static_identifier(SPAN, "key", ast),
            binding(key),
            false,
            false,
            ast,
        );
        let pattern = BindingPattern::new_object_pattern(
            SPAN,
            ArenaVec::from_array_in([property], ast),
            Some(BindingRestElement::boxed(SPAN, binding(props), ast)),
            ast,
        );
        let param = |pattern| {
            FormalParameter::new(
                SPAN,
                ArenaVec::new_in(ast),
                pattern,
                None,
                None,
                false,
                None,
                false,
                false,
                ast,
            )
        };
        let params = FormalParameters::boxed(
            SPAN,
            FormalParameterKind::ArrowFormalParameters,
            ArenaVec::from_array_in([param(binding(tag)), param(pattern)], ast),
            None,
            ast,
        );
        let args = [tag, props, key]
            .map(|name| Argument::from(Expression::new_identifier(SPAN, name, ast)));
        let call = Expression::new_call_expression(
            SPAN,
            callee,
            None,
            ArenaVec::from_array_in(args, ast),
            false,
            ast,
        );
        Expression::new_arrow_function_expression(SPAN, false, None, params, None, call.into(), ast)
    }

    fn decode(&self, text: &'a str) -> JSStr<'a> {
        let mut buffer = None;
        decode_entities(text, &mut buffer, text.len(), self.allocator);
        match buffer {
            Some(buffer) => buffer.into_js_str(),
            None => JSStr::from(text),
        }
    }

    fn text(&self, text: &'a str) -> Option<JSStr<'a>> {
        let mut lines = text.split(['\n', '\r', '\u{2028}', '\u{2029}']).peekable();
        let mut output: Option<JSStrBuilder<'a>> = None;
        let mut first = true;
        while let Some(line) = lines.next() {
            let last = lines.peek().is_none();
            if first && last {
                return (!line.is_empty()).then(|| self.decode(line));
            }
            let trimmed = match (first, last) {
                (true, false) => line.trim_end_matches(is_white_space_single_line),
                (false, true) => line.trim_start_matches(is_white_space_single_line),
                _ => line.trim_matches(is_white_space_single_line),
            };
            first = false;
            if trimmed.is_empty() {
                continue;
            }
            if let Some(buffer) = &mut output {
                buffer.push(' ');
            } else {
                output = Some(JSStrBuilder::with_capacity_in(text.len(), &self.ast));
            }
            decode_entities(trimmed, &mut output, text.len(), self.allocator);
        }
        output.map(JSStrBuilder::into_js_str)
    }
}
