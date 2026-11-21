//! Oxc parse.
//!
//! The source is parsed as a strict ES module. Lowering supports local named
//! exports without a module loader. Only this module and [`crate::lower`] see
//! the Oxc AST.

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

pub(crate) fn requests<'a, 'b>(
    program: &'b Program<'a>,
) -> impl Iterator<Item = &'b oxc::ast::ast::StringLiteral<'a>> {
    use oxc::ast::ast::Statement;
    program.body.iter().filter_map(|stmt| match stmt {
        Statement::ImportDeclaration(import) => Some(&import.source),
        Statement::ExportFromDeclaration(export) => Some(&export.source),
        Statement::ExportAllDeclaration(export) => Some(&export.source),
        _ => None,
    })
}

pub(crate) fn check_dependencies(
    program: &Program<'_>,
    id: &str,
    source: &str,
    resolver: &dyn crate::ModuleResolver,
    options: &crate::CompileOptions,
    loaded_roots: &[&str],
) -> Result<(), CompileError> {
    use oxc::ast::ast::Statement;
    for statement in &program.body {
        let span = match statement {
            Statement::ExportFromDeclaration(export) => export.span,
            Statement::ExportAllDeclaration(export) => export.span,
            Statement::ImportDeclaration(import)
                if import.phase.is_some() || import.with_clause.is_some() =>
            {
                return Err(crate::error::unsupported(
                    source,
                    import.span,
                    "import phases or attributes",
                ));
            }
            _ => continue,
        };
        return Err(crate::error::unsupported(source, span, "re-export"));
    }
    for request in requests(program) {
        let specifier = request.value.as_str().ok_or_else(|| {
            CompileError::new(
                source,
                request.span,
                "unsupported syntax: unpaired surrogate in module specifier",
            )
        })?;
        if options.imports.contains_key(specifier) {
            continue;
        }
        let result = crate::resolved_id(resolver, Some(id), specifier).and_then(|target| {
            if loaded_roots.contains(&target.as_str()) {
                Ok(())
            } else {
                resolver.load(&target).map(|_| ())
            }
        });
        let reason = result
            .err()
            .unwrap_or_else(|| "source dependency linking is not supported".into());
        return Err(CompileError::new(
            source,
            request.span,
            format!("source dependency {specifier:?} from {id:?}: {reason}"),
        ));
    }
    Ok(())
}
