//! AST lowering. The only pass that walks function bodies.
//!
//! A [`Lowerer`] composes [`Env`], [`Emitter`], and [`Control`] and produces a
//! [`LoweredFunction`]. Nested functions are a worklist of Oxc nodes plus the
//! scope chain captured at the definition, not a recursive borrow of the lowerer.

mod control;
mod expr;
mod stmt;

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use oxc::ast::ast::*;
use oxc::ast_visit::Visit;
use oxc::span::{GetSpan, Span};
use oxc::syntax::scope::ScopeFlags;
use yield_vm::BindingId;
use yield_vm::bytecode::{Constant, Instr};

use crate::allocate;
use crate::bindings;
use crate::emit::{Emitter, Reg};
use crate::env::{BindingKind, Env};
use crate::error::{CompileError, unsupported};
use crate::ir::LoweredFunction;
use crate::promote;
use crate::session::Session;
use control::Control;

pub(crate) fn lower_all<'ast>(
    session: &mut Session<'_>,
    functions: Vec<(String, &'ast Function<'ast>)>,
) -> Result<(), CompileError> {
    let mut tasks = VecDeque::new();
    for (func_id, (name, function)) in functions.into_iter().enumerate() {
        tasks.push_back(Task {
            source: FunctionSource::Function(function),
            func_id: func_id as u32,
            name,
            outer_scopes: Vec::new(),
        });
    }
    let mut lowered = Vec::new();
    while let Some(task) = tasks.pop_front() {
        lowered.push(Lowerer::compile_task(session, &mut tasks, task)?);
    }
    let facts = bindings::classify(&lowered)?;
    for function in &mut lowered {
        promote::promote(session.source, function, &facts)?;
    }
    for function in lowered {
        let function = allocate::allocate(session.source, function)?;
        session.push_function(function);
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum FunctionSource<'ast> {
    Function(&'ast Function<'ast>),
    Method(&'ast Function<'ast>),
    Arrow(&'ast ArrowFunctionExpression<'ast>),
}

impl FunctionSource<'_> {
    fn span(self) -> Span {
        match self {
            Self::Function(function) | Self::Method(function) => function.span,
            Self::Arrow(arrow) => arrow.span,
        }
    }

    fn is_async(self) -> bool {
        match self {
            Self::Function(function) | Self::Method(function) => function.r#async,
            Self::Arrow(arrow) => arrow.r#async,
        }
    }

    fn is_arrow(self) -> bool {
        matches!(self, Self::Arrow(_))
    }
}

struct Task<'ast> {
    source: FunctionSource<'ast>,
    func_id: u32,
    name: String,
    outer_scopes: Vec<BTreeMap<String, BindingId>>,
}

struct Lowerer<'ast, 'borrow, 'src> {
    session: &'borrow mut Session<'src>,
    tasks: &'borrow mut VecDeque<Task<'ast>>,
    func_id: u32,
    name: String,
    source: FunctionSource<'ast>,
    span: Span,
    is_async: bool,
    is_arrow: bool,
    is_method: bool,
    env: Env,
    emit: Emitter<'src>,
    control: Control,
    param_bindings: Vec<BindingId>,
    this_binding: Option<BindingId>,
    hoisted: BTreeSet<u32>,
}

impl<'ast, 'borrow, 'src> Lowerer<'ast, 'borrow, 'src> {
    fn compile_task(
        session: &'borrow mut Session<'src>,
        tasks: &'borrow mut VecDeque<Task<'ast>>,
        task: Task<'ast>,
    ) -> Result<LoweredFunction, CompileError> {
        let mut lowerer = Self::new(session, tasks, task);
        lowerer.compile()
    }

    fn new(
        session: &'borrow mut Session<'src>,
        tasks: &'borrow mut VecDeque<Task<'ast>>,
        task: Task<'ast>,
    ) -> Self {
        let span = task.source.span();
        let is_async = task.source.is_async();
        let source_text = session.source;
        Self {
            session,
            tasks,
            func_id: task.func_id,
            name: task.name,
            source: task.source,
            span,
            is_async,
            is_arrow: task.source.is_arrow(),
            is_method: matches!(task.source, FunctionSource::Method(_)),
            env: Env::new(task.outer_scopes),
            emit: Emitter::new(source_text, span),
            control: Control::new(),
            param_bindings: Vec::new(),
            this_binding: None,
            hoisted: BTreeSet::new(),
        }
    }

    fn compile(&mut self) -> Result<LoweredFunction, CompileError> {
        let source = self.source;
        self.compile_parameters(source)?;
        self.install_this_binding(source);
        match source {
            FunctionSource::Function(function) | FunctionSource::Method(function) => {
                let body = function.body.as_ref().ok_or_else(|| {
                    unsupported(
                        self.source_text(),
                        function.span,
                        "declaration without body",
                    )
                })?;
                self.predeclare_scope(&body.statements)?;
                for statement in &body.statements {
                    self.compile_statement(statement)?;
                }
                self.emit_implicit_return()?;
            }
            FunctionSource::Arrow(arrow) => match &arrow.body {
                ArrowFunctionBody::FunctionBody(body) => {
                    self.predeclare_scope(&body.statements)?;
                    for statement in &body.statements {
                        self.compile_statement(statement)?;
                    }
                    self.emit_implicit_return()?;
                }
                other => {
                    let expression = other.as_expression().ok_or_else(|| {
                        unsupported(
                            self.source_text(),
                            other.span(),
                            "arrow body outside the PoC subset",
                        )
                    })?;
                    let value = self.compile_expression(expression)?;
                    self.emit(Instr::Return {
                        src: self.slot(value),
                    });
                }
            },
        }

        if !self.control.finalizers_finished() {
            return Err(CompileError::new(
                self.source_text(),
                self.span,
                "internal compiler error: unterminated finally context",
            ));
        }

        Ok(LoweredFunction {
            func_id: self.func_id,
            name: self.name.clone(),
            is_async: self.is_async,
            is_arrow: self.is_arrow,
            is_method: self.is_method,
            param_bindings: self.param_bindings.clone(),
            this_binding: self.this_binding,
            promoted: BTreeMap::new(),
            reg_count: self.emit.reg_count(),
            code: self.emit.take_code(),
            awaits: self.control.take_awaits(),
            handlers: self.control.take_handlers(),
            span: self.span,
        })
    }
    fn install_this_binding(&mut self, source: FunctionSource<'ast>) {
        if self.is_arrow || !function_uses_this(source) {
            return;
        }
        let binding = self.alloc_binding();
        self.env.insert_function(THIS_BINDING.to_owned(), binding);
        self.this_binding = Some(binding);
    }

    fn compile_parameters(&mut self, source: FunctionSource<'ast>) -> Result<(), CompileError> {
        match source {
            FunctionSource::Function(function) | FunctionSource::Method(function) => {
                if function.generator {
                    return Err(unsupported(self.source_text(), function.span, "generator"));
                }
                self.compile_parameter_list(&function.params)
            }
            FunctionSource::Arrow(arrow) => self.compile_parameter_list(&arrow.params),
        }
    }

    fn compile_parameter_list(
        &mut self,
        params: &FormalParameters<'_>,
    ) -> Result<(), CompileError> {
        if params.rest.is_some() {
            return Err(unsupported(
                self.source_text(),
                params.span,
                "rest parameter",
            ));
        }
        for parameter in &params.items {
            if parameter.initializer.is_some() {
                return Err(unsupported(
                    self.source_text(),
                    parameter.span,
                    "default parameter",
                ));
            }
            let BindingPattern::BindingIdentifier(identifier) = &parameter.pattern else {
                return Err(unsupported(
                    self.source_text(),
                    parameter.span,
                    "parameter destructuring",
                ));
            };
            let name = identifier.name.as_str().to_owned();
            if self.env.has_function_binding(&name) {
                return Err(CompileError::new(
                    self.source_text(),
                    parameter.span,
                    format!("duplicate parameter {name:?}"),
                ));
            }
            let binding = self.alloc_binding();
            self.env.insert_function(name, binding);
            self.param_bindings.push(binding);
        }
        Ok(())
    }

    fn emit_implicit_return(&mut self) -> Result<(), CompileError> {
        let implicit = self.alloc_reg()?;
        self.emit(Instr::Undefined {
            dst: self.slot(implicit),
        });
        self.emit(Instr::Return {
            src: self.slot(implicit),
        });
        Ok(())
    }

    pub(super) fn source_text(&self) -> &'src str {
        self.session.source
    }

    pub(super) fn alloc_reg(&mut self) -> Result<Reg, CompileError> {
        self.emit.alloc()
    }

    pub(super) fn slot(&self, reg: Reg) -> yield_vm::bytecode::Slot {
        self.emit.slot(reg)
    }

    pub(super) fn emit(&mut self, instruction: Instr) -> usize {
        self.emit.emit(instruction)
    }

    pub(super) fn pc(&self) -> usize {
        self.emit.pc()
    }

    pub(super) fn jump(&mut self) -> usize {
        self.emit.jump()
    }

    pub(super) fn jump_to(&mut self, target: usize) -> usize {
        self.emit.jump_to(target)
    }

    pub(super) fn branch_false(&mut self, cond: Reg) -> usize {
        self.emit.branch_false(cond)
    }

    pub(super) fn patch_jump(&mut self, index: usize, target: usize) -> Result<(), CompileError> {
        self.emit.patch(index, target)
    }

    fn alloc_binding(&mut self) -> BindingId {
        let next = self.session.binding_ids();
        let binding = *next;
        *next = next.checked_add(1).expect("binding id space exhausted");
        binding
    }

    pub(super) fn declare(
        &mut self,
        name: &str,
        kind: VariableDeclarationKind,
        span: Span,
    ) -> Result<(BindingId, bool), CompileError> {
        let binding_kind = as_binding_kind(self.source_text(), kind, span)?;
        let source = self.session.source;
        self.env
            .declare(self.session.binding_ids(), source, name, binding_kind, span)
    }

    pub(super) fn lookup(&self, name: &str) -> Option<BindingId> {
        self.env.lookup(name)
    }

    pub(super) fn check_tdz(
        &self,
        binding: BindingId,
        name: &str,
        span: Span,
    ) -> Result<(), CompileError> {
        self.env.check_tdz(binding, name, self.session.source, span)
    }
    pub(super) fn emit_constant(&mut self, constant: Constant) -> Result<Reg, CompileError> {
        let index = self.session.intern(constant);
        let dst = self.alloc_reg()?;
        self.emit(Instr::Const {
            dst: self.slot(dst),
            constant: index,
        });
        Ok(dst)
    }

    pub(super) fn emit_undefined(&mut self) -> Result<Reg, CompileError> {
        let dst = self.alloc_reg()?;
        self.emit(Instr::Undefined {
            dst: self.slot(dst),
        });
        Ok(dst)
    }

    pub(super) fn emit_number(&mut self, value: f64) -> Result<Reg, CompileError> {
        self.emit_constant(Constant::Number(value))
    }

    pub(super) fn enqueue_function(
        &mut self,
        source: FunctionSource<'ast>,
        hint: &str,
        span: Span,
    ) -> Result<u32, CompileError> {
        let func_id = self.session.alloc_func_id(span)?;
        let outer_scopes = self.env.capture();
        self.tasks.push_back(Task {
            source,
            func_id,
            name: format!("{hint}@{func_id}"),
            outer_scopes,
        });
        Ok(func_id)
    }

    pub(super) fn emit_function_value(&mut self, func_id: u32) -> Result<Reg, CompileError> {
        let dst = self.alloc_reg()?;
        self.emit(Instr::Closure {
            dst: self.slot(dst),
            function: func_id,
        });
        Ok(dst)
    }

    pub(super) fn init_function_binding(
        &mut self,
        binding: BindingId,
        func_id: u32,
    ) -> Result<(), CompileError> {
        let dst = self.emit_function_value(func_id)?;
        self.emit(Instr::DeclareBinding { binding });
        self.emit(Instr::SetBinding {
            binding,
            src: self.slot(dst),
        });
        Ok(())
    }

    pub(super) fn predeclare_scope(
        &mut self,
        statements: &'ast [Statement<'ast>],
    ) -> Result<(), CompileError> {
        let mut function_names = BTreeSet::new();
        let mut lexical = Vec::new();
        for statement in statements {
            match statement {
                Statement::VariableDeclaration(declaration) => {
                    lexical.extend(self.register_declarators(declaration)?);
                }
                Statement::FunctionDeclaration(function) => {
                    if let Some(identifier) = &function.id {
                        let name = identifier.name.as_str();
                        function_names.insert(name.to_owned());
                        self.predeclare_name(name, VariableDeclarationKind::Let, function.span)?;
                    }
                }
                _ => {}
            }
        }
        // Function declarations are initialized at block entry. They use a
        // lexical kind internally but are not pending.
        lexical.retain(|(_, name)| !function_names.contains(name));
        self.open_pending(lexical.iter().map(|(binding, _)| *binding))?;

        // Function declarations are initialized at block entry, before any
        // other statement runs. Their captured scope already includes every
        // lexical binding predeclared above.
        for statement in statements {
            let Statement::FunctionDeclaration(function) = statement else {
                continue;
            };
            if !self.hoisted.insert(function.span.start) {
                continue;
            }
            let name = function
                .id
                .as_ref()
                .ok_or_else(|| {
                    unsupported(self.source_text(), function.span, "anonymous declaration")
                })?
                .name
                .as_str()
                .to_owned();
            let (binding, _) = self.declare(&name, VariableDeclarationKind::Let, function.span)?;
            let func_id =
                self.enqueue_function(FunctionSource::Function(function), &name, function.span)?;
            self.init_function_binding(binding, func_id)?;
        }
        Ok(())
    }

    /// Register a `for` initializer list before its first initializer runs, so
    /// a later binding in the list shadows an outer name of the same spelling.
    pub(super) fn predeclare_variable_list(
        &mut self,
        declaration: &'ast VariableDeclaration<'ast>,
    ) -> Result<(), CompileError> {
        let lexical = self.register_declarators(declaration)?;
        self.open_pending(lexical.into_iter().map(|(binding, _)| binding))
    }

    fn register_declarators(
        &mut self,
        declaration: &'ast VariableDeclaration<'ast>,
    ) -> Result<Vec<(BindingId, String)>, CompileError> {
        let mut lexical = Vec::new();
        let is_lexical = matches!(
            declaration.kind,
            VariableDeclarationKind::Let | VariableDeclarationKind::Const
        );
        for declarator in &declaration.declarations {
            let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
                continue;
            };
            let name = identifier.name.as_str();
            let Some(binding) = self.predeclare_name(name, declaration.kind, declarator.span)?
            else {
                continue;
            };
            if is_lexical {
                lexical.push((binding, name.to_owned()));
            }
        }
        Ok(lexical)
    }

    /// Declare and store `undefined` for lexical bindings at scope entry.
    ///
    /// Heap `DeclareBinding` only inserts a missing binding, so a loop that
    /// re-enters this block must also store `undefined`. One undefined
    /// register is shared by every binding opened here.
    fn open_pending(
        &mut self,
        bindings: impl IntoIterator<Item = BindingId>,
    ) -> Result<(), CompileError> {
        let mut unique = Vec::new();
        let mut seen = BTreeSet::new();
        for binding in bindings {
            if seen.insert(binding) {
                unique.push(binding);
            }
        }
        for binding in &unique {
            self.env.mark_pending(*binding);
        }
        if unique.is_empty() {
            return Ok(());
        }
        let undefined = self.emit_undefined()?;
        let src = self.slot(undefined);
        for binding in unique {
            self.emit(Instr::DeclareBinding { binding });
            self.emit(Instr::SetBinding { binding, src });
        }
        Ok(())
    }

    fn predeclare_name(
        &mut self,
        name: &str,
        kind: VariableDeclarationKind,
        span: Span,
    ) -> Result<Option<BindingId>, CompileError> {
        let binding_kind = as_binding_kind(self.source_text(), kind, span)?;
        Ok(self
            .env
            .predeclare(self.session.binding_ids(), name, binding_kind))
    }

    pub(super) fn compile_block(
        &mut self,
        block: &'ast BlockStatement<'ast>,
    ) -> Result<(), CompileError> {
        self.env.enter_block();
        self.predeclare_scope(&block.body)?;
        for statement in &block.body {
            self.compile_statement(statement)?;
        }
        self.env.exit_block();
        Ok(())
    }

    pub(super) fn compile_variable_declaration(
        &mut self,
        declaration: &'ast VariableDeclaration<'ast>,
    ) -> Result<(), CompileError> {
        let lexical = matches!(
            declaration.kind,
            VariableDeclarationKind::Let | VariableDeclarationKind::Const
        );
        for declarator in &declaration.declarations {
            let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
                return Err(unsupported(
                    self.source_text(),
                    declarator.span,
                    "destructuring declaration",
                ));
            };
            let name = identifier.name.as_str();
            let (binding, _) = self.declare(name, declaration.kind, declarator.span)?;
            if lexical {
                let value = if let Some(expression) = &declarator.init {
                    self.compile_expression(expression)?
                } else {
                    // A closure may have written this binding before the
                    // declaration. The declaration overwrites that write.
                    self.emit_undefined()?
                };
                self.emit(Instr::SetBinding {
                    binding,
                    src: self.slot(value),
                });
                self.env.clear_pending(binding);
            } else {
                self.emit(Instr::DeclareBinding { binding });
                if let Some(expression) = &declarator.init {
                    let value = self.compile_expression(expression)?;
                    self.emit(Instr::SetBinding {
                        binding,
                        src: self.slot(value),
                    });
                }
            }
        }
        Ok(())
    }
}

fn as_binding_kind(
    source: &str,
    kind: VariableDeclarationKind,
    span: Span,
) -> Result<BindingKind, CompileError> {
    match kind {
        VariableDeclarationKind::Var => Ok(BindingKind::Var),
        VariableDeclarationKind::Let | VariableDeclarationKind::Const => Ok(BindingKind::Lexical),
        other => Err(unsupported(
            source,
            span,
            match other {
                VariableDeclarationKind::Using => "using declaration",
                VariableDeclarationKind::AwaitUsing => "await using declaration",
                _ => "variable declaration kind",
            },
        )),
    }
}

/// Not a JavaScript identifier, so script cannot name or shadow this binding.
pub(super) const THIS_BINDING: &str = "\0this";

fn function_uses_this(source: FunctionSource<'_>) -> bool {
    let (FunctionSource::Function(function) | FunctionSource::Method(function)) = source else {
        return false;
    };
    let Some(body) = &function.body else {
        return false;
    };
    let mut scan = ThisScan { found: false };
    scan.visit_function_body(body);
    scan.found
}

struct ThisScan {
    found: bool,
}

impl<'a> Visit<'a> for ThisScan {
    fn visit_this_expression(&mut self, _: &ThisExpression) {
        self.found = true;
    }

    fn visit_function(&mut self, _: &Function<'a>, _: ScopeFlags) {}
}
