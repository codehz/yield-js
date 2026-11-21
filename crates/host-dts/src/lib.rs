//! Host declaration-file inventory.
//!
//! A host ships one `.d.ts` describing the modules it publishes. This crate
//! reads the declaration syntax alone: no type checker, no module graph, no
//! reachable-name filtering. The result maps each declared host module to the
//! value names that module must implement at runtime.
//!
//! Only direct declarations are understood. Value export lists, local aliases,
//! re-exports, `export *`, `export =`, namespace re-exports and non-literal
//! module declarations are rejected with a located diagnostic instead of being
//! skipped. A module whose body is exclusively type declarations is retained
//! with an empty export set, so the compiler keeps the module in its manifest.
//!
//! Every message is UTF-8 byte spans into the input; the caller owns line and
//! column computation.

use std::collections::{BTreeMap, BTreeSet};

use oxc::allocator::Allocator;
use oxc::ast::ast::{
    Declaration, ExportDefaultDeclarationKind, ExportNamedDeclaration, Statement,
    TSNamespaceDeclaration, TSNamespaceDeclarationBody,
};
use oxc::diagnostics::Diagnostic;
use oxc::parser::Parser;
use oxc::span::{GetSpan, SourceType, Span};

/// A declaration-file failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub message: String,
    /// UTF-8 byte offset of the first byte of the offending syntax.
    pub start: u32,
    /// UTF-8 byte offset one past the last byte of the offending syntax.
    pub end: u32,
}

impl Error {
    fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            start: span.start,
            end: span.end,
        }
    }

    fn unsupported(span: Span, feature: &str) -> Self {
        Self::new(span, format!("unsupported syntax: {feature}"))
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} (byte {})", self.message, self.start)
    }
}

impl std::error::Error for Error {}

/// Reads the declared host modules and their required value exports.
pub fn parse(source: &str) -> Result<BTreeMap<String, BTreeSet<String>>, Error> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::d_ts()).parse();
    if let Some(diagnostic) = parsed.diagnostics.first() {
        let span = diagnostic_span(diagnostic);
        return Err(Error::new(span, diagnostic.to_string()));
    }
    let mut modules: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for statement in &parsed.program.body {
        match statement {
            Statement::TSExternalModuleDeclaration(declaration) => {
                let Some(name) = declaration.id.value.as_str() else {
                    return Err(Error::unsupported(
                        declaration.id.span,
                        "module name with a lone surrogate",
                    ));
                };
                let mut exports = modules.remove(name).unwrap_or_default();
                if let Some(body) = &declaration.body {
                    collect_module_body(&body.body, &mut exports)?;
                }
                modules.insert(name.to_owned(), exports);
            }
            Statement::TSInterfaceDeclaration(_) => {}
            Statement::TSTypeAliasDeclaration(_) => {}
            Statement::TSImportEqualsDeclaration(declaration) => {
                return Err(Error::unsupported(
                    declaration.span,
                    "import-equals declaration outside a host module",
                ));
            }
            Statement::ImportDeclaration(declaration) => {
                return Err(Error::unsupported(
                    declaration.span,
                    "import declaration in a host declaration file",
                ));
            }
            Statement::TSGlobalDeclaration(declaration) => {
                return Err(Error::unsupported(
                    declaration.span,
                    "global augmentation outside a host module",
                ));
            }
            Statement::TSNamespaceDeclaration(declaration) => {
                return Err(Error::unsupported(
                    declaration.span,
                    if declaration.id.name.is_empty() {
                        "namespace declaration outside a host module"
                    } else {
                        "non-literal module declaration; host modules require a string name"
                    },
                ));
            }
            Statement::TSExportAssignment(declaration) => {
                return Err(Error::unsupported(declaration.span, "`export =`"));
            }
            Statement::TSNamespaceExportDeclaration(declaration) => {
                return Err(Error::unsupported(declaration.span, "`export as namespace`"));
            }
            Statement::ExportAllDeclaration(declaration) => {
                return Err(Error::unsupported(declaration.span, "`export *`"));
            }
            Statement::ExportFromDeclaration(declaration) => {
                return Err(Error::unsupported(declaration.span, "re-export"));
            }
            Statement::ExportNamedDeclaration(declaration) => {
                reject_value_export_list(declaration)?;
            }
            Statement::ExportDefaultDeclaration(declaration) => {
                return Err(Error::unsupported(
                    declaration.span,
                    "default export outside a host module",
                ));
            }
            Statement::ExportDeclaration(declaration) => {
                return Err(Error::unsupported(
                    declaration.span,
                    "export outside a host module",
                ));
            }
            other => {
                return Err(Error::unsupported(
                    other.span(),
                    "top-level value declaration outside a host module",
                ));
            }
        }
    }
    Ok(modules)
}

fn diagnostic_span(diagnostic: &impl Diagnostic) -> Span {
    diagnostic
        .labels()
        .first()
        .map(|label| Span::new(label.offset(), label.offset() + label.len()))
        .unwrap_or_default()
}

fn reject_value_export_list(declaration: &ExportNamedDeclaration<'_>) -> Result<(), Error> {
    if declaration.export_kind.is_type() {
        return Ok(());
    }
    if let Some(specifier) = declaration
        .specifiers
        .iter()
        .find(|specifier| !specifier.export_kind.is_type())
    {
        let span = Span::new(specifier.span.start, specifier.span.end);
        return Err(Error::unsupported(span, "value export list"));
    }
    Ok(())
}

fn collect_module_body(
    statements: &[Statement<'_>],
    exports: &mut BTreeSet<String>,
) -> Result<(), Error> {
    for statement in statements {
        match statement {
            Statement::FunctionDeclaration(declaration) => {
                if let Some(id) = &declaration.id {
                    exports.insert(id.name.to_string());
                }
            }
            Statement::ClassDeclaration(declaration) => {
                if let Some(id) = &declaration.id {
                    exports.insert(id.name.to_string());
                }
            }
            Statement::VariableDeclaration(declaration) => {
                for declarator in &declaration.declarations {
                    let Some(id) = declarator.id.get_binding_identifier() else {
                        return Err(Error::unsupported(
                            declarator.id.span(),
                            "binding pattern in a host declaration",
                        ));
                    };
                    exports.insert(id.name.to_string());
                }
            }
            Statement::TSEnumDeclaration(declaration) => {
                if !declaration.r#const {
                    exports.insert(declaration.id.name.to_string());
                }
            }
            Statement::TSNamespaceDeclaration(declaration) => {
                if namespace_has_value(declaration) {
                    exports.insert(declaration.id.name.to_string());
                }
            }
            Statement::TSInterfaceDeclaration(_) | Statement::TSTypeAliasDeclaration(_) => {}
            Statement::ExportDeclaration(declaration) => {
                collect_export(&declaration.declaration, exports)?;
            }
            Statement::ExportDefaultDeclaration(declaration) => match &declaration.declaration {
                ExportDefaultDeclarationKind::FunctionDeclaration(_)
                | ExportDefaultDeclarationKind::ClassDeclaration(_) => {
                    exports.insert("default".to_owned());
                }
                ExportDefaultDeclarationKind::TSInterfaceDeclaration(_) => {}
                _ => {
                    return Err(Error::unsupported(
                        declaration.span,
                        "default export of a value expression",
                    ));
                }
            },
            Statement::ExportNamedDeclaration(declaration) => {
                reject_value_export_list(declaration)?;
            }
            Statement::TSExportAssignment(declaration) => {
                return Err(Error::unsupported(declaration.span, "`export =`"));
            }
            Statement::ExportFromDeclaration(declaration) => {
                return Err(Error::unsupported(declaration.span, "re-export"));
            }
            Statement::ExportAllDeclaration(declaration) => {
                return Err(Error::unsupported(declaration.span, "`export *`"));
            }
            Statement::TSNamespaceExportDeclaration(declaration) => {
                return Err(Error::unsupported(declaration.span, "`export as namespace`"));
            }
            other => {
                return Err(Error::unsupported(
                    other.span(),
                    "statement inside a host module",
                ));
            }
        }
    }
    Ok(())
}

fn collect_export(declaration: &Declaration<'_>, exports: &mut BTreeSet<String>) -> Result<(), Error> {
    match declaration {
        Declaration::FunctionDeclaration(declaration) => {
            if let Some(id) = &declaration.id {
                exports.insert(id.name.to_string());
            }
        }
        Declaration::ClassDeclaration(declaration) => {
            if let Some(id) = &declaration.id {
                exports.insert(id.name.to_string());
            }
        }
        Declaration::VariableDeclaration(declaration) => {
            for declarator in &declaration.declarations {
                let Some(id) = declarator.id.get_binding_identifier() else {
                    return Err(Error::unsupported(
                        declarator.id.span(),
                        "binding pattern in a host declaration",
                    ));
                };
                exports.insert(id.name.to_string());
            }
        }
        Declaration::TSEnumDeclaration(declaration) => {
            if !declaration.r#const {
                exports.insert(declaration.id.name.to_string());
            }
        }
        Declaration::TSNamespaceDeclaration(declaration) => {
            if namespace_has_value(declaration) {
                exports.insert(declaration.id.name.to_string());
            }
        }
        Declaration::TSInterfaceDeclaration(_) | Declaration::TSTypeAliasDeclaration(_) => {}
        other => {
            return Err(Error::unsupported(
                other.span(),
                "exported declaration inside a host module",
            ));
        }
    }
    Ok(())
}

fn namespace_has_value(declaration: &TSNamespaceDeclaration<'_>) -> bool {
    match &declaration.body {
        TSNamespaceDeclarationBody::TSNamespaceDeclaration(inner) => namespace_has_value(inner),
        TSNamespaceDeclarationBody::TSModuleBlock(block) => {
            block.body.iter().any(statement_has_value)
        }
    }
}

fn statement_has_value(statement: &Statement<'_>) -> bool {
    match statement {
        Statement::FunctionDeclaration(declaration) => declaration.id.is_some(),
        Statement::ClassDeclaration(declaration) => declaration.id.is_some(),
        Statement::VariableDeclaration(declaration) => !declaration.declarations.is_empty(),
        Statement::TSEnumDeclaration(declaration) => !declaration.r#const,
        Statement::TSNamespaceDeclaration(declaration) => namespace_has_value(declaration),
        Statement::ExportDeclaration(declaration) => declaration_has_value(&declaration.declaration),
        Statement::ExportDefaultDeclaration(declaration) => matches!(
            &declaration.declaration,
            ExportDefaultDeclarationKind::FunctionDeclaration(_)
                | ExportDefaultDeclarationKind::ClassDeclaration(_)
        ),
        _ => false,
    }
}

fn declaration_has_value(declaration: &Declaration<'_>) -> bool {
    match declaration {
        Declaration::FunctionDeclaration(declaration) => declaration.id.is_some(),
        Declaration::ClassDeclaration(declaration) => declaration.id.is_some(),
        Declaration::VariableDeclaration(declaration) => !declaration.declarations.is_empty(),
        Declaration::TSEnumDeclaration(declaration) => !declaration.r#const,
        Declaration::TSNamespaceDeclaration(declaration) => namespace_has_value(declaration),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exports<'a>(modules: &'a BTreeMap<String, BTreeSet<String>>, module: &str) -> Vec<&'a str> {
        modules
            .get(module)
            .map(|names| names.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }

    #[test]
    fn collects_direct_value_exports_and_ignores_types() {
        let source = r#"
declare module "a" {
    interface Box<T> { value: T }
    type Alias<T> = Box<T> | null;
    export function id<T extends Box<string>>(x: T): T;
    export function id(x: number): number;
    export const shared: number, other: string;
    export class Store<T> { private item: T; get(): T; }
    export type Only = string;
    export interface Face { a: number }
    const enum Hidden { A }
    enum Visible { A }
    namespace Types { interface Nested { a: 1 } }
    namespace Values { function nested(): void; }
}
declare module "b" {
    export const only: boolean;
}
declare module "types" {
    export interface Empty {}
}
"#;
        let modules = parse(source).unwrap();
        assert_eq!(modules.len(), 3);
        assert_eq!(
            exports(&modules, "a"),
            ["Store", "Values", "Visible", "id", "other", "shared"]
        );
        assert_eq!(exports(&modules, "b"), ["only"]);
        assert!(exports(&modules, "types").is_empty());
    }

    #[test]
    fn merges_modules_and_ignores_extra_members_and_signatures() {
        let first = parse(
            r#"declare module "m" { export function f<T>(x: T): T; export interface I<T> { y: T } }"#,
        )
        .unwrap();
        let second = parse(
            r#"declare module "m" { export function f(): void; export function f(a: string): void; interface Blah {} }"#,
        )
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(exports(&first, "m"), ["f"]);
    }

    #[test]
    fn default_function_and_class_are_values() {
        let modules = parse(
            r#"declare module "d" { export default function start(): void; }
declare module "e" { export default class Stop {} }
declare module "f" { export default interface Shape { a: 1 } }"#,
        )
        .unwrap();
        assert_eq!(exports(&modules, "d"), ["default"]);
        assert_eq!(exports(&modules, "e"), ["default"]);
        assert!(exports(&modules, "f").is_empty());
    }

    #[test]
    fn empty_source_has_no_modules() {
        assert!(parse("").unwrap().is_empty());
        assert!(parse("export {};\n").unwrap().is_empty());
    }

    #[test]
    fn rejects_reexport_at_its_statement() {
        let source = "declare module \"a\" {\n    export * from \"./b\";\n}\n";
        let error = parse(source).unwrap_err();
        assert_eq!(error.message, "unsupported syntax: `export *`");
        assert_eq!(
            &source[error.start as usize..error.start as usize + 10],
            "export * f"
        );
    }

    #[test]
    fn rejects_value_export_lists_and_export_assignment() {
        let error = parse("declare module \"a\" {\n  export { x };\n}\n").unwrap_err();
        assert!(error.message.starts_with("unsupported syntax: value export list"));
        assert_eq!(error.start, 32);
        let error = parse("declare module \"a\" {}\nexport = 1;\n").unwrap_err();
        assert_eq!(error.message, "unsupported syntax: `export =`");
    }

    #[test]
    fn rejects_non_host_top_level_values_and_nonliteral_modules() {
        let error = parse("declare const helper: number;\n").unwrap_err();
        assert_eq!(
            error.message,
            "unsupported syntax: top-level value declaration outside a host module"
        );
        let error = parse("namespace Widget { function f(): void; }\n").unwrap_err();
        assert!(error.message.contains("non-literal module declaration"));
    }

    #[test]
    fn reports_first_parser_diagnostic_span() {
        let source = "declare module \"a\" { function ( }\n";
        let error = parse(source).unwrap_err();
        assert!(!error.message.starts_with("unsupported syntax:"), "{error}");
        assert!(error.start >= 24);
        assert!(error.end > error.start);
    }

    #[test]
    fn type_only_exports_do_not_require_values() {
        let modules = parse(
            r#"declare module "t" {
    export type A = string;
    export type { B };
    export interface C {}
    const enum D { X }
}"#,
        )
        .unwrap();
        assert!(exports(&modules, "t").is_empty());
    }
}
