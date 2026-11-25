//! Restricted CommonJS interop for the Web production resolver.

use std::cell::Cell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write;

use oxc::allocator::Allocator;
use oxc::ast::ast::*;
use oxc::ast_visit::{Visit, walk};
use oxc::diagnostics::Diagnostic;
use oxc::parser::Parser;
use oxc::span::{GetSpan, SourceType, Span};
use oxc::syntax::operator::{BinaryOperator, UnaryOperator};
use oxc::syntax::scope::{ScopeFlags, ScopeId};

use crate::error::{CompileError, unsupported};

use super::js_string;

pub(super) struct Factory {
    pub(super) requires: Vec<(String, Span)>,
    pub(super) names: BTreeSet<String>,
    pub(super) forwards: BTreeSet<usize>,
    body: String,
    prefix: String,
}

impl Factory {
    pub(super) fn render(&self, targets: &[String]) -> String {
        assert_eq!(self.requires.len(), targets.len());
        let mut output = String::new();
        let p = &self.prefix;
        for (index, target) in targets.iter().enumerate() {
            writeln!(
                output,
                "import {{ init as {p}require{index} }} from {};",
                js_string(target)
            )
            .unwrap();
        }
        writeln!(
            output,
            "var {p}cache;\nexport {{ {p}init as init }};\nfunction {p}init() {{
  if ({p}cache) return {p}cache.exports;
  const {p}module = {{ exports: {{}} }};
  {p}cache = {p}module;
  try {{
    (function (exports, module) {{"
        )
        .unwrap();
        output.push_str(&self.body);
        writeln!(
            output,
            "\n    }}).call({p}module.exports, {p}module.exports, {p}module);
    return {p}module.exports;
  }} catch ({p}error) {{
    {p}cache = void 0;
    throw {p}error;
  }}
}}"
        )
        .unwrap();
        output
    }
}

pub(super) fn analyze(id: &str, source: &str) -> Result<Factory, CompileError> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::cjs()).parse();
    if let Some(diagnostic) = parsed.diagnostics.first() {
        let span = diagnostic
            .labels()
            .first()
            .map(|label| Span::new(label.offset(), label.offset() + label.len()))
            .unwrap_or_default();
        return Err(CompileError::new(source, span, diagnostic.to_string()).in_module(id));
    }
    let program = &parsed.program;
    let mut bindings = Bindings::default();
    bindings.visit_program(program);
    let mut prefix = String::from("$cjs$");
    while bindings
        .identifiers
        .iter()
        .any(|name| name.starts_with(&prefix))
    {
        prefix.push('$');
    }
    let mut scan = Scan {
        bindings: &bindings.scopes,
        scopes: Vec::new(),
        prefix: &prefix,
        requires: Vec::new(),
        names: BTreeSet::new(),
        forwards: BTreeSet::new(),
        edits: Vec::new(),
        error: None,
    };
    if let Some(hashbang) = &program.hashbang {
        scan.replace(hashbang.span, "");
    }
    scan.visit_program(program);
    if let Some((span, feature)) = scan.error {
        return Err(unsupported(source, span, feature).in_module(id));
    }
    scan.edits.sort_unstable_by_key(|(span, _)| span.start);
    let mut body = String::with_capacity(source.len());
    let mut end = 0;
    for (span, replacement) in scan.edits {
        body.push_str(&source[end..span.start as usize]);
        body.push_str(&replacement);
        end = span.end as usize;
    }
    body.push_str(&source[end..]);
    Ok(Factory {
        requires: scan.requires,
        names: scan.names,
        forwards: scan.forwards,
        body,
        prefix,
    })
}

// Keep lexical scope discovery independent of reachability: a reference can be
// shadowed by a hoisted declaration even when its initializer never executes.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Scope {
    Ast(*const Cell<Option<ScopeId>>),
    FunctionBody(Span),
    ClassExpression(Span),
}

#[derive(Default)]
struct Bindings<'a> {
    scopes: HashMap<Scope, HashSet<&'a str>>,
    stack: Vec<(Scope, bool)>,
    identifiers: HashSet<&'a str>,
    var_bindings: HashSet<Span>,
}

impl<'a> Bindings<'a> {
    fn bind(&mut self, scope: Scope, ident: &BindingIdentifier<'a>) {
        self.scopes
            .entry(scope)
            .or_default()
            .insert(ident.name.as_str());
        self.identifiers.insert(ident.name.as_str());
    }
}

impl<'a> Visit<'a> for Bindings<'a> {
    fn enter_scope(&mut self, flags: ScopeFlags, id: &Cell<Option<ScopeId>>) {
        self.stack.push((Scope::Ast(id), flags.is_var()));
    }

    fn leave_scope(&mut self) {
        self.stack.pop();
    }

    fn visit_binding_identifier(&mut self, ident: &BindingIdentifier<'a>) {
        if !self.var_bindings.contains(&ident.span) {
            self.bind(self.stack.last().unwrap().0, ident);
        }
    }

    fn visit_identifier_reference(&mut self, ident: &IdentifierReference<'a>) {
        self.identifiers.insert(ident.name.as_str());
    }

    fn visit_variable_declaration(&mut self, declaration: &VariableDeclaration<'a>) {
        if declaration.kind.is_var() {
            let scope = self.stack.iter().rev().find(|(_, var)| *var).unwrap().0;
            for decl in &declaration.declarations {
                for ident in decl.id.get_binding_identifiers() {
                    self.var_bindings.insert(ident.span);
                    self.bind(scope, ident);
                }
            }
        }
        walk::walk_variable_declaration(self, declaration);
    }

    fn visit_function(&mut self, function: &Function<'a>, flags: ScopeFlags) {
        if function.is_declaration()
            && let Some(ident) = &function.id
        {
            self.bind(self.stack.last().unwrap().0, ident);
        }
        walk::walk_function(self, function, flags);
    }

    fn visit_function_body(&mut self, body: &FunctionBody<'a>) {
        self.stack.push((Scope::FunctionBody(body.span), true));
        walk::walk_function_body(self, body);
        self.stack.pop();
    }

    fn visit_class(&mut self, class: &Class<'a>) {
        if !class.is_declaration() {
            self.stack.push((Scope::ClassExpression(class.span), false));
        }
        walk::walk_class(self, class);
        if !class.is_declaration() {
            self.stack.pop();
        }
    }
}

struct Scan<'a, 'b> {
    bindings: &'b HashMap<Scope, HashSet<&'a str>>,
    scopes: Vec<Scope>,
    prefix: &'b str,
    requires: Vec<(String, Span)>,
    names: BTreeSet<String>,
    forwards: BTreeSet<usize>,
    edits: Vec<(Span, String)>,
    error: Option<(Span, &'static str)>,
}

fn member<'a, 'b>(expr: &'b Expression<'a>) -> Option<&'b MemberExpression<'a>> {
    expr.without_parentheses().as_member_expression()
}

fn property<'a>(member: &MemberExpression<'a>) -> Option<&'a str> {
    member.static_property_name().and_then(|name| name.as_str())
}

impl<'a> Scan<'a, '_> {
    fn reject(&mut self, span: Span, feature: &'static str) {
        self.error.get_or_insert((span, feature));
    }

    fn replace(&mut self, span: Span, replacement: impl Into<String>) {
        self.edits.push((span, replacement.into()));
    }

    fn is_free(&self, name: &str) -> bool {
        !self.scopes.iter().any(|scope| {
            self.bindings
                .get(scope)
                .is_some_and(|names| names.contains(name))
        })
    }

    fn free(&self, expr: &Expression<'a>, name: &str) -> bool {
        matches!(expr.without_parentheses(), Expression::Identifier(ident) if ident.name == name)
            && self.is_free(name)
    }

    fn module_exports(&self, member: &MemberExpression<'a>) -> bool {
        !member.optional()
            && property(member) == Some("exports")
            && self.free(member.object(), "module")
    }

    fn export_object(&self, expr: &Expression<'a>) -> bool {
        self.free(expr, "exports") || member(expr).is_some_and(|m| self.module_exports(m))
    }

    fn node_env(&self, member: &MemberExpression<'a>) -> bool {
        !member.optional()
            && property(member) == Some("NODE_ENV")
            && member_env(member.object()).is_some_and(|env| self.free(env.object(), "process"))
    }

    fn string_value(&self, expr: &Expression<'a>) -> Option<(&'a str, bool)> {
        match expr.without_parentheses() {
            Expression::StringLiteral(lit) => Some((lit.value.as_str()?, false)),
            _ if member(expr).is_some_and(|m| self.node_env(m)) => Some(("production", true)),
            _ => None,
        }
    }

    fn comparison(&self, binary: &BinaryExpression<'a>) -> Option<bool> {
        let (left, left_env) = self.string_value(&binary.left)?;
        let (right, right_env) = self.string_value(&binary.right)?;
        if !(left_env || right_env) {
            return None;
        }
        match binary.operator {
            BinaryOperator::Equality | BinaryOperator::StrictEquality => Some(left == right),
            BinaryOperator::Inequality | BinaryOperator::StrictInequality => Some(left != right),
            _ => None,
        }
    }

    fn truth(&self, test: &Expression<'a>) -> Option<bool> {
        match test.without_parentheses() {
            Expression::BooleanLiteral(lit) => Some(lit.value),
            Expression::BinaryExpression(binary) => self.comparison(binary),
            Expression::UnaryExpression(unary) if unary.operator == UnaryOperator::LogicalNot => {
                self.truth(&unary.argument).map(|value| !value)
            }
            _ => self
                .string_value(test)
                .map(|(string, _)| !string.is_empty()),
        }
    }

    fn boolean(&mut self, span: Span, value: bool) {
        self.replace(span, if value { " true " } else { " false " });
    }

    fn dead(&mut self, statement: &Statement<'a>) {
        let mut hoisted = Hoisted::default();
        hoisted.visit_statement(statement);
        let replacement = if hoisted.names.is_empty() {
            "{}".to_owned()
        } else {
            format!(
                "{{var {};}}",
                hoisted.names.into_iter().collect::<Vec<_>>().join(",")
            )
        };
        self.replace(statement.span(), replacement);
    }

    fn require(&mut self, call: &CallExpression<'a>) -> Option<usize> {
        let [Argument::StringLiteral(literal)] = call.arguments.as_slice() else {
            self.reject(
                call.span,
                "CommonJS require must have one string literal argument",
            );
            return None;
        };
        if call.optional {
            self.reject(call.span, "optional CommonJS require");
            return None;
        }
        let Some(specifier) = literal.value.as_str() else {
            self.reject(literal.span, "invalid CommonJS require specifier");
            return None;
        };
        let index = self.requires.len();
        self.requires.push((specifier.to_owned(), call.span));
        // An added leading parenthesis could attach this call to a preceding
        // expression statement which relied on automatic semicolon insertion.
        self.replace(call.span, format!(" {}require{index}()", self.prefix));
        Some(index)
    }

    fn export_replacement(&mut self, value: &Expression<'a>) {
        match value.without_parentheses() {
            Expression::CallExpression(call) if self.free(&call.callee, "require") => {
                if let Some(index) = self.require(call) {
                    self.forwards.insert(index);
                }
            }
            Expression::ObjectExpression(object) => {
                for property in &object.properties {
                    let ObjectPropertyKind::ObjectProperty(property) = property else {
                        self.reject(property.span(), "indeterminate CommonJS export shape");
                        continue;
                    };
                    let name = property.key.static_name();
                    let Some(name) = name.as_ref().and_then(|name| name.as_str()) else {
                        self.reject(property.span, "indeterminate CommonJS export name");
                        continue;
                    };
                    if name == "__proto__" {
                        self.reject(property.span, "CommonJS exports prototype change");
                    }
                    self.names.insert(name.to_owned());
                }
                walk::walk_object_expression(self, object);
            }
            Expression::ConditionalExpression(conditional) => {
                if let Some(value) = self.truth(&conditional.test) {
                    self.boolean(conditional.test.span(), value);
                    let (live, dead) = if value {
                        (&conditional.consequent, &conditional.alternate)
                    } else {
                        (&conditional.alternate, &conditional.consequent)
                    };
                    self.replace(dead.span(), "(void 0)");
                    self.export_replacement(live);
                } else {
                    self.visit_expression(&conditional.test);
                    self.export_replacement(&conditional.consequent);
                    self.export_replacement(&conditional.alternate);
                }
            }
            _ => self.reject(value.span(), "indeterminate CommonJS export shape"),
        }
    }

    fn visit_export_member(&mut self, member: &MemberExpression<'a>, write: bool) -> bool {
        if !self.export_object(member.object()) {
            return false;
        }
        match property(member) {
            Some("__proto__") => self.reject(member.span(), "CommonJS exports prototype access"),
            Some(name) if write => {
                self.names.insert(name.to_owned());
            }
            Some(_) => {}
            None => self.reject(member.span(), "indeterminate CommonJS export name"),
        }
        true
    }
}

fn member_env<'a, 'b>(expr: &'b Expression<'a>) -> Option<&'b MemberExpression<'a>> {
    member(expr).filter(|m| !m.optional() && property(m) == Some("env"))
}

impl<'a> Visit<'a> for Scan<'a, '_> {
    fn enter_scope(&mut self, _: ScopeFlags, id: &Cell<Option<ScopeId>>) {
        self.scopes.push(Scope::Ast(id));
    }

    fn leave_scope(&mut self) {
        self.scopes.pop();
    }

    fn visit_function_body(&mut self, body: &FunctionBody<'a>) {
        self.scopes.push(Scope::FunctionBody(body.span));
        walk::walk_function_body(self, body);
        self.scopes.pop();
    }

    fn visit_class(&mut self, class: &Class<'a>) {
        if !class.is_declaration() {
            self.scopes.push(Scope::ClassExpression(class.span));
        }
        walk::walk_class(self, class);
        if !class.is_declaration() {
            self.scopes.pop();
        }
    }

    fn visit_identifier_reference(&mut self, ident: &IdentifierReference<'a>) {
        let name = ident.name.as_str();
        if self.is_free(name) {
            match name {
                "require" => self.reject(ident.span, "CommonJS require escape"),
                "exports" | "module" => {
                    self.reject(ident.span, "CommonJS exports or module escape")
                }
                _ => {}
            }
        }
    }

    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        if self.free(&call.callee, "require") {
            self.require(call);
        } else if self.free(&call.callee, "eval") && !call.optional {
            self.reject(call.span, "direct eval in CommonJS");
        } else {
            walk::walk_call_expression(self, call);
        }
    }

    fn visit_import_expression(&mut self, import: &ImportExpression<'a>) {
        match import.source.without_parentheses() {
            Expression::StringLiteral(literal)
                if literal
                    .value
                    .as_str()
                    .is_some_and(|specifier| !specifier.starts_with("cjs-factory:")) =>
            {
                walk::walk_import_expression(self, import);
            }
            _ => self.reject(
                import.span,
                "dynamic import in CommonJS must use a public literal specifier",
            ),
        }
    }

    fn visit_if_statement(&mut self, statement: &IfStatement<'a>) {
        if let Some(value) = self.truth(&statement.test) {
            self.boolean(statement.test.span(), value);
            if value {
                self.visit_statement(&statement.consequent);
                if let Some(alternate) = &statement.alternate {
                    self.dead(alternate);
                }
            } else {
                self.dead(&statement.consequent);
                if let Some(alternate) = &statement.alternate {
                    self.visit_statement(alternate);
                }
            }
        } else {
            walk::walk_if_statement(self, statement);
        }
    }

    fn visit_conditional_expression(&mut self, conditional: &ConditionalExpression<'a>) {
        if let Some(value) = self.truth(&conditional.test) {
            self.boolean(conditional.test.span(), value);
            let (live, dead) = if value {
                (&conditional.consequent, &conditional.alternate)
            } else {
                (&conditional.alternate, &conditional.consequent)
            };
            self.visit_expression(live);
            self.replace(dead.span(), "(void 0)");
        } else {
            walk::walk_conditional_expression(self, conditional);
        }
    }

    fn visit_binary_expression(&mut self, binary: &BinaryExpression<'a>) {
        if let Some(value) = self.comparison(binary) {
            self.boolean(binary.span, value);
        } else {
            walk::walk_binary_expression(self, binary);
        }
    }

    fn visit_unary_expression(&mut self, unary: &UnaryExpression<'a>) {
        if unary.operator == UnaryOperator::Delete
            && member(&unary.argument).is_some_and(|m| self.node_env(m))
        {
            self.reject(unary.span, "deletion of process.env.NODE_ENV in CommonJS");
        } else {
            walk::walk_unary_expression(self, unary);
        }
    }

    fn visit_expression(&mut self, expr: &Expression<'a>) {
        if let Some(member) = expr.as_member_expression()
            && self.node_env(member)
        {
            self.replace(member.span(), " \"production\" ");
        } else {
            walk::walk_expression(self, expr);
        }
    }

    fn visit_member_expression(&mut self, member: &MemberExpression<'a>) {
        if self.module_exports(member) {
            self.reject(member.span(), "CommonJS module.exports escape");
        } else if !self.visit_export_member(member, false) {
            walk::walk_member_expression(self, member);
        }
    }

    fn visit_assignment_expression(&mut self, assignment: &AssignmentExpression<'a>) {
        if assignment.operator.is_assign()
            && assignment
                .left
                .as_member_expression()
                .is_some_and(|m| self.module_exports(m))
        {
            self.export_replacement(&assignment.right);
        } else {
            walk::walk_assignment_expression(self, assignment);
        }
    }

    fn visit_simple_assignment_target(&mut self, target: &SimpleAssignmentTarget<'a>) {
        if let Some(member) = target.as_member_expression()
            && self.node_env(member)
        {
            self.reject(
                member.span(),
                "assignment to process.env.NODE_ENV in CommonJS",
            );
        } else if !target
            .as_member_expression()
            .is_some_and(|m| self.visit_export_member(m, true))
        {
            walk::walk_simple_assignment_target(self, target);
        }
    }

    fn visit_module_declaration(&mut self, declaration: &ModuleDeclaration<'a>) {
        self.reject(declaration.span(), "ES module declaration in CommonJS");
    }

    fn visit_with_statement(&mut self, statement: &WithStatement<'a>) {
        self.reject(statement.span, "with in CommonJS");
    }
}

#[derive(Default)]
struct Hoisted<'a> {
    names: BTreeSet<&'a str>,
}

impl<'a> Visit<'a> for Hoisted<'a> {
    fn visit_variable_declaration(&mut self, declaration: &VariableDeclaration<'a>) {
        if declaration.kind.is_var() {
            for decl in &declaration.declarations {
                self.names.extend(
                    decl.id
                        .get_binding_identifiers()
                        .into_iter()
                        .map(|ident| ident.name.as_str()),
                );
            }
        }
    }

    fn visit_function(&mut self, _: &Function<'a>, _: ScopeFlags) {}
    fn visit_class(&mut self, _: &Class<'a>) {}
    fn visit_expression(&mut self, _: &Expression<'a>) {}
}
