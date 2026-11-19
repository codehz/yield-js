//! Oxc parse and the top-level subset.
//!
//! The source is parsed as an ES module only to force strict mode. Module
//! syntax is rejected. Only this module and [`crate::lower`] see the Oxc AST.

use std::collections::BTreeMap;

use oxc::allocator::Allocator;
use oxc::ast::ast::{Function, Program, Statement};
use oxc::diagnostics::Diagnostic;
use oxc::parser::Parser;
use oxc::span::{GetSpan, SourceType, Span};

use crate::error::{CompileError, unsupported};

pub(crate) struct TopLevel<'ast> {
    pub(crate) names: BTreeMap<String, u32>,
    pub(crate) functions: Vec<(String, &'ast Function<'ast>)>,
}

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

pub(crate) fn collect_top_level<'ast>(
    source: &str,
    program: &'ast Program<'ast>,
) -> Result<TopLevel<'ast>, CompileError> {
    let mut names = BTreeMap::new();
    let mut functions = Vec::new();
    for statement in &program.body {
        match statement {
            Statement::FunctionDeclaration(function) => {
                let name = function
                    .id
                    .as_ref()
                    .ok_or_else(|| unsupported(source, function.span, "anonymous declaration"))?
                    .name
                    .as_str()
                    .to_owned();
                if names.contains_key(&name) {
                    return Err(CompileError::new(
                        source,
                        function.span,
                        format!("duplicate function declaration {name:?}"),
                    ));
                }
                let func_id = functions.len() as u32;
                names.insert(name.clone(), func_id);
                functions.push((name, function.as_ref()));
            }
            Statement::EmptyStatement(_) => {}
            other => {
                return Err(CompileError::new(
                    source,
                    other.span(),
                    "top-level statements other than function declarations are not supported",
                ));
            }
        }
    }
    if functions.is_empty() {
        return Err(CompileError::new(
            source,
            Span::default(),
            "the PoC requires at least one top-level function declaration",
        ));
    }
    Ok(TopLevel { names, functions })
}
