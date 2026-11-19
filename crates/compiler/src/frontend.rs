//! Oxc parse.
//!
//! The source is parsed as an ES module so the parser applies strict-mode
//! grammar. Module declarations are still rejected by lowering. Only this
//! module and [`crate::lower`] see the Oxc AST.

use oxc::allocator::Allocator;
use oxc::ast::ast::Program;
use oxc::diagnostics::Diagnostic;
use oxc::parser::Parser;
use oxc::span::{SourceType, Span};

use crate::error::CompileError;

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
    Ok(parser_return.program)
}
