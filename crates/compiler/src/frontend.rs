//! Strict ES module parsing and reachable source linking.

pub(crate) mod link;

use oxc::allocator::Allocator;
use oxc::ast::ast::*;
use oxc::ast_visit::{Visit, walk};
use oxc::diagnostics::Diagnostic;
use oxc::parser::Parser;
use oxc::span::{SourceType, Span};
use oxc::syntax::scope::ScopeFlags;

use crate::error::{CompileError, unsupported};

pub(crate) fn parse<'a>(
    allocator: &'a Allocator,
    source: &'a str,
) -> Result<Program<'a>, CompileError> {
    let parser_return = Parser::new(allocator, source, SourceType::mjs()).parse();
    if let Some(diagnostic) = parser_return.diagnostics.first() {
        let span = diagnostic
            .labels()
            .first()
            .map(|label| Span::new(label.offset(), label.offset() + label.len()))
            .unwrap_or_default();
        return Err(CompileError::new(source, span, diagnostic.to_string()));
    }
    let mut validator = ResourceSyntax {
        source,
        async_context: false,
        error: None,
    };
    validator.visit_program(&parser_return.program);
    if let Some(error) = validator.error {
        return Err(error);
    }
    Ok(parser_return.program)
}

// Run after Oxc's diagnostics but before any lowering can skip dead code.
struct ResourceSyntax<'src> {
    source: &'src str,
    async_context: bool,
    error: Option<CompileError>,
}

impl ResourceSyntax<'_> {
    fn reject(&mut self, span: Span, feature: &str) {
        if self.error.is_none() {
            self.error = Some(unsupported(self.source, span, feature));
        }
    }

    fn check(&mut self, declaration: &VariableDeclaration<'_>, needs_init: bool) {
        if !declaration.kind.is_using() {
            return;
        }
        if declaration.kind.is_await_using() && !self.async_context {
            self.reject(declaration.span, "await using outside an async function");
        }
        for declarator in &declaration.declarations {
            if !declarator.id.is_binding_identifier() {
                self.reject(declarator.span, "resource declaration binding pattern");
            }
            if needs_init && declarator.init.is_none() {
                self.reject(
                    declarator.span,
                    "resource declaration without an initializer",
                );
            }
        }
    }
}

impl<'ast> Visit<'ast> for ResourceSyntax<'_> {
    fn visit_variable_declaration(&mut self, declaration: &VariableDeclaration<'ast>) {
        self.check(declaration, true);
        walk::walk_variable_declaration(self, declaration);
    }

    fn visit_for_in_statement(&mut self, statement: &ForInStatement<'ast>) {
        if let ForStatementLeft::VariableDeclaration(declaration) = &statement.left
            && declaration.kind.is_using()
        {
            self.reject(declaration.span, "resource declaration in a for-in head");
        }
        walk::walk_for_in_statement(self, statement);
    }

    fn visit_for_statement_left(&mut self, left: &ForStatementLeft<'ast>) {
        if let ForStatementLeft::VariableDeclaration(declaration) = left {
            // An iterator head receives its initializer from the iteration.
            self.check(declaration, false);
            walk::walk_variable_declaration(self, declaration);
        } else {
            walk::walk_for_statement_left(self, left);
        }
    }

    fn visit_function(&mut self, function: &Function<'ast>, flags: ScopeFlags) {
        let outer = std::mem::replace(&mut self.async_context, function.r#async);
        walk::walk_function(self, function, flags);
        self.async_context = outer;
    }

    fn visit_arrow_function_expression(&mut self, arrow: &ArrowFunctionExpression<'ast>) {
        let outer = std::mem::replace(&mut self.async_context, arrow.r#async);
        walk::walk_arrow_function_expression(self, arrow);
        self.async_context = outer;
    }

    fn visit_static_block(&mut self, block: &StaticBlock<'ast>) {
        let outer = std::mem::replace(&mut self.async_context, false);
        walk::walk_static_block(self, block);
        self.async_context = outer;
    }
}
