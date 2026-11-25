//! Strict ES module parsing and reachable source linking.

pub(crate) mod jsx;
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
    module_id: &str,
) -> Result<Program<'a>, CompileError> {
    let source_type = SourceType::mjs().with_jsx(module_id.ends_with(".jsx"));
    let parser_return = Parser::new(allocator, source, source_type).parse();
    if let Some(diagnostic) = parser_return.diagnostics.first() {
        let span = diagnostic
            .labels()
            .first()
            .map(|label| Span::new(label.offset(), label.offset() + label.len()))
            .unwrap_or_default();
        return Err(CompileError::new(source, span, diagnostic.to_string()));
    }
    validate_syntax(&parser_return.program)?;
    Ok(parser_return.program)
}

pub(crate) fn validate_syntax(program: &Program<'_>) -> Result<(), CompileError> {
    let mut validator = ResourceSyntax {
        source: program.source_text,
        async_context: false,
        error: None,
    };
    validator.visit_program(program);
    validator.error.map_or(Ok(()), Err)
}

pub(crate) fn dynamic_imports<'a>(
    program: &Program<'a>,
) -> Result<Vec<(&'a str, Span)>, (Span, &'static str)> {
    struct Scan<'a> {
        imports: Vec<(&'a str, Span)>,
        error: Option<(Span, &'static str)>,
    }
    impl<'a> Visit<'a> for Scan<'a> {
        fn visit_import_expression(&mut self, import: &ImportExpression<'a>) {
            if self.error.is_none() {
                if import.options.is_some() || import.phase.is_some() {
                    self.error = Some((import.span, "dynamic import phases or options"));
                } else if let Expression::StringLiteral(literal) = &import.source {
                    if let Some(specifier) = literal.value.as_str() {
                        self.imports.push((specifier, import.span));
                    } else {
                        self.error = Some((import.span, "non-string module specifier"));
                    }
                } else {
                    self.error = Some((import.span, "dynamic import with non-literal specifier"));
                }
            }
            walk::walk_import_expression(self, import);
        }
    }
    let mut scan = Scan {
        imports: Vec::new(),
        error: None,
    };
    scan.visit_program(program);
    scan.error.map_or(Ok(scan.imports), Err)
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
