//! Strict-mode JavaScript subset compiler for the resumable VM.
//!
//! The compiler consumes Oxc's AST and emits the VM-owned bytecode contract.
//! Oxc types do not escape this crate. Lexical bindings live in heap
//! environments so closures and `await` share the same mutable state.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use oxc::allocator::Allocator;
use oxc::ast::ast::*;
use oxc::diagnostics::Diagnostic;
use oxc::parser::Parser;
use oxc::span::{GetSpan, SourceType, Span};
use oxc::syntax::operator::{AssignmentOperator, BinaryOperator, LogicalOperator, UnaryOperator};
use yield_vm::BindingId;
use yield_vm::bytecode::{
    BinaryOp, Build, Constant, ExceptionHandler, Function as BytecodeFunction, ImportDecl, Instr,
    ResumeEntry, Slot, SlotKind, UnaryOp,
};

#[derive(Debug, Clone, Default)]
pub struct CompileOptions {
    /// Host bindings declared by the build. Missing bindings are rejected by
    /// the VM before any script code runs.
    pub imports: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CompileError {
    message: String,
    span: Span,
    line: usize,
    column: usize,
}

impl CompileError {
    fn new(source: &str, span: Span, message: impl Into<String>) -> Self {
        let start = (span.start as usize).min(source.len());
        let prefix = &source[..start];
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
        let column = prefix[line_start..].chars().count() + 1;
        Self {
            message: message.into(),
            span,
            line,
            column,
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn span(&self) -> Span {
        self.span
    }

    pub fn line(&self) -> usize {
        self.line
    }

    pub fn column(&self) -> usize {
        self.column
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} at {}:{} (byte {})",
            self.message, self.line, self.column, self.span.start
        )
    }
}

impl std::error::Error for CompileError {}

/// Compiles a strict-mode JavaScript source bundle into a versioned bytecode
/// build. The source is parsed as an ES module only to guarantee strict-mode
/// parsing; module declarations are rejected by the subset compiler.
pub fn compile(source: &str, options: &CompileOptions) -> Result<Build, CompileError> {
    let allocator = Allocator::default();
    let parser_return = Parser::new(&allocator, source, SourceType::mjs()).parse();
    if let Some(diagnostic) = parser_return.diagnostics.first() {
        let span = diagnostic
            .labels()
            .first()
            .map(|label| Span::new(label.offset(), label.offset() + label.len()))
            .unwrap_or_default();
        return Err(CompileError::new(source, span, diagnostic.to_string()));
    }
    let parsed = parser_return.program;

    let mut function_names = BTreeMap::new();
    let mut declarations = Vec::new();
    for statement in &parsed.body {
        match statement {
            Statement::FunctionDeclaration(function) => {
                let name = function
                    .id
                    .as_ref()
                    .ok_or_else(|| unsupported(source, function.span, "anonymous declaration"))?
                    .name
                    .as_str()
                    .to_owned();
                if function_names.contains_key(&name) {
                    return Err(CompileError::new(
                        source,
                        function.span,
                        format!("duplicate function declaration {name:?}"),
                    ));
                }
                let func_id = declarations.len() as u32;
                function_names.insert(name.clone(), func_id);
                declarations.push((name, function));
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
    if declarations.is_empty() {
        return Err(CompileError::new(
            source,
            Span::default(),
            "the PoC requires at least one top-level function declaration",
        ));
    }

    let mut imports = Vec::with_capacity(options.imports.len());
    let mut import_names = BTreeMap::new();
    for name in &options.imports {
        if import_names.contains_key(name) {
            return Err(CompileError::new(
                source,
                Span::default(),
                format!("duplicate host import {name:?}"),
            ));
        }
        import_names.insert(name.clone(), imports.len() as u32);
        imports.push(ImportDecl { name: name.clone() });
    }

    let mut state = ProgramState {
        source,
        import_names,
        top_level_names: function_names,
        constants: Vec::new(),
        functions: Vec::new(),
        tasks: VecDeque::new(),
        next_func_id: declarations.len() as u32,
        next_binding_id: 0,
    };
    for (func_id, (name, function)) in declarations.into_iter().enumerate() {
        state.tasks.push_back(FunctionTask {
            source: FunctionSource::Function(function),
            func_id: func_id as u32,
            name,
            outer_scopes: Vec::new(),
        });
    }

    while let Some(task) = state.tasks.pop_front() {
        let mut builder = FunctionBuilder::new(&mut state, task);
        let function = builder.compile()?;
        state.functions.push(function);
    }

    let mut build = Build::new(state.constants, imports, state.functions);
    build.finalize();
    build.validate().map_err(|error| {
        CompileError::new(
            source,
            Span::default(),
            format!("internal bytecode validation failed: {error}"),
        )
    })?;
    Ok(build)
}

fn unsupported(source: &str, span: Span, feature: &str) -> CompileError {
    CompileError::new(source, span, format!("unsupported syntax: {feature}"))
}

type Reg = u16;

struct ProgramState<'ast> {
    source: &'ast str,
    import_names: BTreeMap<String, u32>,
    top_level_names: BTreeMap<String, u32>,
    constants: Vec<Constant>,
    functions: Vec<BytecodeFunction>,
    tasks: VecDeque<FunctionTask<'ast>>,
    next_func_id: u32,
    next_binding_id: BindingId,
}

#[derive(Clone, Copy)]
enum FunctionSource<'ast> {
    Function(&'ast Function<'ast>),
    Arrow(&'ast ArrowFunctionExpression<'ast>),
}

impl FunctionSource<'_> {
    fn span(self) -> Span {
        match self {
            Self::Function(function) => function.span,
            Self::Arrow(arrow) => arrow.span,
        }
    }

    fn is_async(self) -> bool {
        match self {
            Self::Function(function) => function.r#async,
            Self::Arrow(arrow) => arrow.r#async,
        }
    }
}

struct FunctionTask<'ast> {
    source: FunctionSource<'ast>,
    func_id: u32,
    name: String,
    outer_scopes: Vec<BTreeMap<String, BindingId>>,
}

struct AwaitInfo {
    result: Reg,
    fulfill_pc: usize,
    reject_pc: usize,
}

struct LoopContext {
    break_jumps: Vec<usize>,
    continue_jumps: Vec<usize>,
    continue_target: Option<usize>,
    finalizer_depth: usize,
}

struct FinalizerContext {
    kind: Reg,
    payload: Reg,
    entry: Option<usize>,
    entry_jumps: Vec<usize>,
}

const COMPLETION_NORMAL: f64 = 0.0;
const COMPLETION_RETURN: f64 = 1.0;
const COMPLETION_THROW: f64 = 2.0;

struct FunctionBuilder<'ast, 'state> {
    program: &'state mut ProgramState<'ast>,
    func_id: u32,
    name: String,
    source: FunctionSource<'ast>,
    span: Span,
    is_async: bool,
    outer_scopes: Vec<BTreeMap<String, BindingId>>,
    next_reg: Reg,
    param_bindings: Vec<BindingId>,
    function_scope: BTreeMap<String, BindingId>,
    scopes: Vec<BTreeMap<String, BindingId>>,
    code: Vec<Instr>,
    awaits: Vec<AwaitInfo>,
    loops: Vec<LoopContext>,
    finalizers: Vec<FinalizerContext>,
    handlers: Vec<ExceptionHandler>,
    hoisted_functions: BTreeSet<u32>,
}

impl<'ast, 'state> FunctionBuilder<'ast, 'state> {
    fn new(program: &'state mut ProgramState<'ast>, task: FunctionTask<'ast>) -> Self {
        Self {
            program,
            func_id: task.func_id,
            name: task.name,
            source: task.source,
            span: task.source.span(),
            is_async: task.source.is_async(),
            outer_scopes: task.outer_scopes,
            next_reg: 0,
            param_bindings: Vec::new(),
            function_scope: BTreeMap::new(),
            scopes: vec![BTreeMap::new()],
            code: Vec::new(),
            awaits: Vec::new(),
            loops: Vec::new(),
            finalizers: Vec::new(),
            handlers: Vec::new(),
            hoisted_functions: BTreeSet::new(),
        }
    }

    fn compile(&mut self) -> Result<BytecodeFunction, CompileError> {
        let source = self.task_source();
        self.compile_parameters(source)?;
        match source {
            FunctionSource::Function(function) => {
                let body = function.body.as_ref().ok_or_else(|| {
                    unsupported(
                        self.program.source,
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
                            self.program.source,
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

        if !self.finalizers.is_empty() {
            return Err(CompileError::new(
                self.program.source,
                self.span,
                "internal compiler error: unterminated finally context",
            ));
        }

        let mut resumes = Vec::with_capacity(self.awaits.len());
        for await_info in &self.awaits {
            resumes.push(ResumeEntry {
                fulfill_pc: await_info.fulfill_pc as u32,
                fulfill_dst: self.slot(await_info.result),
                reject_pc: await_info.reject_pc as u32,
                reject_dst: self.slot(await_info.result),
                saved_slots: Vec::new(),
            });
        }

        let live_in = compute_live_in(&self.code, &resumes, &self.handlers)?;
        let mut saved_regs = BTreeSet::new();
        for resume in &resumes {
            let mut saved = live_in[resume.fulfill_pc as usize].clone();
            saved.extend(live_in[resume.reject_pc as usize].iter().copied());
            saved.remove(&resume.fulfill_dst.index);
            saved.remove(&resume.reject_dst.index);
            if saved.iter().any(|reg| *reg >= self.next_reg) {
                return Err(CompileError::new(
                    self.program.source,
                    self.span,
                    "internal liveness error: unknown register",
                ));
            }
            saved_regs.extend(saved);
        }

        let persistent_map = saved_regs
            .iter()
            .copied()
            .enumerate()
            .map(|(index, reg)| (reg, index as u16))
            .collect::<BTreeMap<_, _>>();
        let transient_map = (0..self.next_reg)
            .filter(|reg| !persistent_map.contains_key(reg))
            .enumerate()
            .map(|(index, reg)| (reg, index as u16))
            .collect::<BTreeMap<_, _>>();

        let remap = |slot: Slot| -> Slot {
            if persistent_map.contains_key(&slot.index) {
                Slot {
                    kind: SlotKind::Persistent,
                    index: persistent_map[&slot.index],
                }
            } else {
                Slot {
                    kind: SlotKind::Transient,
                    index: transient_map[&slot.index],
                }
            }
        };

        let code = self
            .code
            .iter()
            .map(|instruction| remap_instruction(instruction, &remap))
            .collect::<Vec<_>>();
        let mut remapped_resumes = resumes;
        for resume in &mut remapped_resumes {
            let mut saved = live_in[resume.fulfill_pc as usize].clone();
            saved.extend(live_in[resume.reject_pc as usize].iter().copied());
            saved.remove(&resume.fulfill_dst.index);
            saved.remove(&resume.reject_dst.index);
            resume.saved_slots = saved
                .iter()
                .map(|reg| persistent_map[reg])
                .collect::<Vec<_>>();
            resume.saved_slots.sort_unstable();
            resume.fulfill_dst = remap(resume.fulfill_dst);
            resume.reject_dst = remap(resume.reject_dst);
        }
        let handlers = self
            .handlers
            .iter()
            .map(|handler| ExceptionHandler {
                start_pc: handler.start_pc,
                end_pc: handler.end_pc,
                target_pc: handler.target_pc,
                dst: remap(handler.dst),
            })
            .collect::<Vec<_>>();

        Ok(BytecodeFunction {
            func_id: self.func_id,
            name: self.name.clone(),
            is_async: self.is_async,
            param_bindings: self.param_bindings.clone(),
            transient_slots: transient_map.len() as u16,
            persistent_slots: persistent_map.len() as u16,
            code,
            resumes: remapped_resumes,
            handlers,
        })
    }

    fn task_source(&self) -> FunctionSource<'ast> {
        self.source
    }

    fn compile_parameters(&mut self, source: FunctionSource<'ast>) -> Result<(), CompileError> {
        match source {
            FunctionSource::Function(function) => {
                if function.generator {
                    return Err(unsupported(self.program.source, function.span, "generator"));
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
                self.program.source,
                params.span,
                "rest parameter",
            ));
        }
        for parameter in &params.items {
            if parameter.initializer.is_some() {
                return Err(unsupported(
                    self.program.source,
                    parameter.span,
                    "default parameter",
                ));
            }
            let BindingPattern::BindingIdentifier(identifier) = &parameter.pattern else {
                return Err(unsupported(
                    self.program.source,
                    parameter.span,
                    "parameter destructuring",
                ));
            };
            let name = identifier.name.as_str().to_owned();
            if self.function_scope.contains_key(&name) {
                return Err(CompileError::new(
                    self.program.source,
                    parameter.span,
                    format!("duplicate parameter {name:?}"),
                ));
            }
            let binding = self.alloc_binding();
            self.function_scope.insert(name, binding);
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

    fn alloc_reg(&mut self) -> Result<Reg, CompileError> {
        let reg = self.next_reg;
        self.next_reg = self.next_reg.checked_add(1).ok_or_else(|| {
            CompileError::new(self.program.source, Span::default(), "too many slots")
        })?;
        Ok(reg)
    }

    fn alloc_binding(&mut self) -> BindingId {
        let binding = self.program.next_binding_id;
        self.program.next_binding_id = self
            .program
            .next_binding_id
            .checked_add(1)
            .expect("binding id space exhausted");
        binding
    }

    fn slot(&self, reg: Reg) -> Slot {
        Slot {
            kind: SlotKind::Transient,
            index: reg,
        }
    }

    fn emit(&mut self, instruction: Instr) -> usize {
        let index = self.code.len();
        self.code.push(instruction);
        index
    }

    fn patch_jump(&mut self, index: usize, target: usize) -> Result<(), CompileError> {
        let target = target as u32;
        match self.code.get_mut(index) {
            Some(Instr::Jump { target: slot }) | Some(Instr::BranchFalse { target: slot, .. }) => {
                *slot = target;
                Ok(())
            }
            _ => Err(CompileError::new(
                self.program.source,
                self.span,
                "internal compiler error: patch target is not a jump",
            )),
        }
    }

    fn declare(
        &mut self,
        name: &str,
        kind: VariableDeclarationKind,
        span: Span,
    ) -> Result<(BindingId, bool), CompileError> {
        match kind {
            VariableDeclarationKind::Var => {
                if let Some(binding) = self.function_scope.get(name) {
                    return Ok((*binding, false));
                }
                let binding = self.alloc_binding();
                self.function_scope.insert(name.to_owned(), binding);
                Ok((binding, true))
            }
            VariableDeclarationKind::Let | VariableDeclarationKind::Const => {
                let conflicts_with_function_scope =
                    self.scopes.len() == 1 && self.function_scope.contains_key(name);
                let current = self.scopes.last().expect("scope stack is never empty");
                if let Some(binding) = current.get(name) {
                    return Ok((*binding, false));
                }
                if conflicts_with_function_scope {
                    return Err(CompileError::new(
                        self.program.source,
                        span,
                        format!("duplicate lexical binding {name:?}"),
                    ));
                }
                let binding = self.alloc_binding();
                self.scopes
                    .last_mut()
                    .expect("scope stack is never empty")
                    .insert(name.to_owned(), binding);
                Ok((binding, true))
            }
            other => Err(unsupported(
                self.program.source,
                span,
                match other {
                    VariableDeclarationKind::Using => "using declaration",
                    VariableDeclarationKind::AwaitUsing => "await using declaration",
                    _ => "variable declaration kind",
                },
            )),
        }
    }

    fn lookup(&self, name: &str) -> Option<BindingId> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
            .or_else(|| self.function_scope.get(name).copied())
            .or_else(|| {
                self.outer_scopes
                    .iter()
                    .rev()
                    .find_map(|scope| scope.get(name).copied())
            })
    }

    fn is_top_or_import(&self, name: &str) -> bool {
        self.program.import_names.contains_key(name)
            || self.program.top_level_names.contains_key(name)
    }

    fn is_known_name(&self, name: &str) -> bool {
        name == "undefined" || self.lookup(name).is_some() || self.is_top_or_import(name)
    }

    fn predeclare_scope(
        &mut self,
        statements: &'ast [Statement<'ast>],
    ) -> Result<(), CompileError> {
        for statement in statements {
            match statement {
                Statement::VariableDeclaration(declaration) => {
                    for declarator in &declaration.declarations {
                        if let BindingPattern::BindingIdentifier(identifier) = &declarator.id {
                            self.predeclare_name(
                                identifier.name.as_str(),
                                declaration.kind,
                                declarator.span,
                            )?;
                        }
                    }
                }
                Statement::FunctionDeclaration(function) => {
                    if let Some(identifier) = &function.id {
                        self.predeclare_name(
                            identifier.name.as_str(),
                            VariableDeclarationKind::Let,
                            function.span,
                        )?;
                    }
                }
                _ => {}
            }
        }

        // Function declarations are initialized at block entry, before any
        // other statement runs. Their captured scope already includes every
        // lexical binding predeclared above.
        for statement in statements {
            let Statement::FunctionDeclaration(function) = statement else {
                continue;
            };
            if !self.hoisted_functions.insert(function.span.start) {
                continue;
            }
            let name = function
                .id
                .as_ref()
                .ok_or_else(|| {
                    unsupported(self.program.source, function.span, "anonymous declaration")
                })?
                .name
                .as_str()
                .to_owned();
            let (binding, _) = self.declare(&name, VariableDeclarationKind::Let, function.span)?;
            let func_id =
                self.enqueue_function(FunctionSource::Function(function), &name, function.span)?;
            let dst = self.alloc_reg()?;
            self.emit(Instr::Closure {
                dst: self.slot(dst),
                function: func_id,
            });
            self.emit(Instr::DeclareBinding { binding });
            self.emit(Instr::SetBinding {
                binding,
                src: self.slot(dst),
            });
        }
        Ok(())
    }

    fn predeclare_name(
        &mut self,
        name: &str,
        kind: VariableDeclarationKind,
        span: Span,
    ) -> Result<(), CompileError> {
        match kind {
            VariableDeclarationKind::Var => {
                if !self.function_scope.contains_key(name) {
                    let binding = self.alloc_binding();
                    self.function_scope.insert(name.to_owned(), binding);
                }
            }
            VariableDeclarationKind::Let | VariableDeclarationKind::Const => {
                let conflicts_with_function_scope =
                    self.scopes.len() == 1 && self.function_scope.contains_key(name);
                if !conflicts_with_function_scope
                    && !self
                        .scopes
                        .last()
                        .expect("scope stack is never empty")
                        .contains_key(name)
                {
                    let binding = self.alloc_binding();
                    self.scopes
                        .last_mut()
                        .expect("scope stack is never empty")
                        .insert(name.to_owned(), binding);
                }
            }
            other => {
                return Err(unsupported(
                    self.program.source,
                    span,
                    match other {
                        VariableDeclarationKind::Using => "using declaration",
                        VariableDeclarationKind::AwaitUsing => "await using declaration",
                        _ => "variable declaration kind",
                    },
                ));
            }
        }
        Ok(())
    }

    fn compile_statement<'node>(
        &mut self,
        statement: &'node Statement<'node>,
    ) -> Result<(), CompileError>
    where
        'node: 'ast,
    {
        match statement {
            Statement::BlockStatement(block) => self.compile_block(block),
            Statement::EmptyStatement(_) => Ok(()),
            Statement::ExpressionStatement(statement) => {
                self.compile_expression(&statement.expression)?;
                Ok(())
            }
            Statement::VariableDeclaration(declaration) => {
                self.compile_variable_declaration(declaration)
            }
            Statement::FunctionDeclaration(function) => {
                if self.hoisted_functions.contains(&function.span.start) {
                    return Ok(());
                }
                let name = function
                    .id
                    .as_ref()
                    .ok_or_else(|| {
                        unsupported(self.program.source, function.span, "anonymous declaration")
                    })?
                    .name
                    .as_str()
                    .to_owned();
                let (binding, _) =
                    self.declare(&name, VariableDeclarationKind::Let, function.span)?;
                let func_id = self.enqueue_function(
                    FunctionSource::Function(function),
                    &name,
                    function.span,
                )?;
                let dst = self.alloc_reg()?;
                self.emit(Instr::Closure {
                    dst: self.slot(dst),
                    function: func_id,
                });
                self.emit(Instr::DeclareBinding { binding });
                self.emit(Instr::SetBinding {
                    binding,
                    src: self.slot(dst),
                });
                Ok(())
            }
            Statement::ReturnStatement(statement) => {
                let value = match &statement.argument {
                    Some(expression) => self.compile_expression(expression)?,
                    None => self.emit_undefined()?,
                };
                self.emit_return(value)
            }
            Statement::ThrowStatement(statement) => {
                let value = self.compile_expression(&statement.argument)?;
                self.emit(Instr::Throw {
                    src: self.slot(value),
                });
                Ok(())
            }
            Statement::IfStatement(statement) => {
                let test = self.compile_expression(&statement.test)?;
                let branch = self.emit(Instr::BranchFalse {
                    cond: self.slot(test),
                    target: u32::MAX,
                });
                self.compile_statement(&statement.consequent)?;
                let end = self.emit(Instr::Jump { target: u32::MAX });
                let alternate = self.code.len();
                self.patch_jump(branch, alternate)?;
                if let Some(alternate_statement) = &statement.alternate {
                    self.compile_statement(alternate_statement)?;
                }
                let end_target = self.code.len();
                self.patch_jump(end, end_target)?;
                Ok(())
            }
            Statement::WhileStatement(statement) => {
                let start = self.code.len();
                let test = self.compile_expression(&statement.test)?;
                let exit_branch = self.emit(Instr::BranchFalse {
                    cond: self.slot(test),
                    target: u32::MAX,
                });
                self.loops.push(LoopContext {
                    break_jumps: Vec::new(),
                    continue_jumps: Vec::new(),
                    continue_target: Some(start),
                    finalizer_depth: self.finalizers.len(),
                });
                self.compile_statement(&statement.body)?;
                let loop_context = self.loops.pop().expect("loop stack");
                self.emit(Instr::Jump {
                    target: start as u32,
                });
                let end = self.code.len();
                self.patch_jump(exit_branch, end)?;
                self.patch_loop(loop_context, end)
            }
            Statement::DoWhileStatement(statement) => {
                let start = self.code.len();
                self.loops.push(LoopContext {
                    break_jumps: Vec::new(),
                    continue_jumps: Vec::new(),
                    continue_target: None,
                    finalizer_depth: self.finalizers.len(),
                });
                self.compile_statement(&statement.body)?;
                let test_pc = self.code.len();
                let mut loop_context = self.loops.pop().expect("loop stack");
                for jump in loop_context.continue_jumps.drain(..) {
                    self.patch_jump(jump, test_pc)?;
                }
                let test = self.compile_expression(&statement.test)?;
                self.emit(Instr::BranchFalse {
                    cond: self.slot(test),
                    target: u32::MAX,
                });
                let back = self.code.len();
                self.emit(Instr::Jump {
                    target: start as u32,
                });
                let end = self.code.len();
                let exit_branch = back - 1;
                self.patch_jump(exit_branch, end)?;
                loop_context.continue_target = Some(test_pc);
                self.patch_loop(loop_context, end)
            }
            Statement::ForStatement(statement) => {
                self.scopes.push(BTreeMap::new());
                if let Some(init) = &statement.init {
                    match init {
                        ForStatementInit::VariableDeclaration(declaration) => {
                            self.compile_variable_declaration(declaration)?
                        }
                        other => {
                            if let Some(expression) = other.as_expression() {
                                self.compile_expression(expression)?;
                            } else {
                                return Err(unsupported(
                                    self.program.source,
                                    other.span(),
                                    "for-loop initializer",
                                ));
                            }
                        }
                    }
                }
                let test_pc = self.code.len();
                let exit_branch = if let Some(test) = &statement.test {
                    let test = self.compile_expression(test)?;
                    Some(self.emit(Instr::BranchFalse {
                        cond: self.slot(test),
                        target: u32::MAX,
                    }))
                } else {
                    None
                };
                self.loops.push(LoopContext {
                    break_jumps: Vec::new(),
                    continue_jumps: Vec::new(),
                    continue_target: None,
                    finalizer_depth: self.finalizers.len(),
                });
                self.compile_statement(&statement.body)?;
                let update_pc = self.code.len();
                let mut loop_context = self.loops.pop().expect("loop stack");
                for jump in loop_context.continue_jumps.drain(..) {
                    self.patch_jump(jump, update_pc)?;
                }
                if let Some(update) = &statement.update {
                    self.compile_expression(update)?;
                }
                self.emit(Instr::Jump {
                    target: test_pc as u32,
                });
                let end = self.code.len();
                if let Some(exit_branch) = exit_branch {
                    self.patch_jump(exit_branch, end)?;
                }
                loop_context.continue_target = Some(update_pc);
                self.patch_loop(loop_context, end)?;
                self.scopes.pop();
                Ok(())
            }
            Statement::BreakStatement(statement) => {
                if statement.label.is_some() {
                    return Err(unsupported(
                        self.program.source,
                        statement.span,
                        "labelled break",
                    ));
                }
                let Some(loop_context) = self.loops.last() else {
                    return Err(CompileError::new(
                        self.program.source,
                        statement.span,
                        "break outside loop",
                    ));
                };
                if self.finalizers.len() > loop_context.finalizer_depth {
                    return Err(unsupported(
                        self.program.source,
                        statement.span,
                        "break across finally",
                    ));
                }
                let jump = self.emit(Instr::Jump { target: u32::MAX });
                self.loops
                    .last_mut()
                    .expect("loop stack checked above")
                    .break_jumps
                    .push(jump);
                Ok(())
            }
            Statement::ContinueStatement(statement) => {
                if statement.label.is_some() {
                    return Err(unsupported(
                        self.program.source,
                        statement.span,
                        "labelled continue",
                    ));
                }
                let Some(loop_context) = self.loops.last() else {
                    return Err(CompileError::new(
                        self.program.source,
                        statement.span,
                        "continue outside loop",
                    ));
                };
                if self.finalizers.len() > loop_context.finalizer_depth {
                    return Err(unsupported(
                        self.program.source,
                        statement.span,
                        "continue across finally",
                    ));
                }
                if let Some(target) = loop_context.continue_target {
                    self.emit(Instr::Jump {
                        target: target as u32,
                    });
                } else {
                    let jump = self.emit(Instr::Jump { target: u32::MAX });
                    self.loops
                        .last_mut()
                        .expect("loop stack checked above")
                        .continue_jumps
                        .push(jump);
                }
                Ok(())
            }
            Statement::DebuggerStatement(statement) => Err(unsupported(
                self.program.source,
                statement.span,
                "debugger statement",
            )),
            Statement::WithStatement(statement) => Err(unsupported(
                self.program.source,
                statement.span,
                "with statement",
            )),
            Statement::SwitchStatement(statement) => Err(unsupported(
                self.program.source,
                statement.span,
                "switch statement",
            )),
            Statement::TryStatement(statement) => self.compile_try(statement),
            Statement::ForInStatement(statement) => {
                Err(unsupported(self.program.source, statement.span, "for-in"))
            }
            Statement::ForOfStatement(statement) => {
                Err(unsupported(self.program.source, statement.span, "for-of"))
            }
            Statement::LabeledStatement(statement) => Err(unsupported(
                self.program.source,
                statement.span,
                "labelled statement",
            )),
            Statement::ClassDeclaration(statement) => Err(unsupported(
                self.program.source,
                statement.span,
                "class declaration",
            )),
            other => Err(unsupported(
                self.program.source,
                other.span(),
                "top-level/module statement",
            )),
        }
    }

    fn compile_block(&mut self, block: &'ast BlockStatement<'ast>) -> Result<(), CompileError> {
        self.scopes.push(BTreeMap::new());
        self.predeclare_scope(&block.body)?;
        for statement in &block.body {
            self.compile_statement(statement)?;
        }
        self.scopes.pop();
        Ok(())
    }

    fn patch_loop(&mut self, loop_context: LoopContext, end: usize) -> Result<(), CompileError> {
        for jump in loop_context.break_jumps {
            self.patch_jump(jump, end)?;
        }
        Ok(())
    }

    fn compile_variable_declaration(
        &mut self,
        declaration: &'ast VariableDeclaration<'ast>,
    ) -> Result<(), CompileError> {
        for declarator in &declaration.declarations {
            let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
                return Err(unsupported(
                    self.program.source,
                    declarator.span,
                    "destructuring declaration",
                ));
            };
            let name = identifier.name.as_str();
            let (binding, _) = self.declare(name, declaration.kind, declarator.span)?;
            self.emit(Instr::DeclareBinding { binding });
            if let Some(expression) = &declarator.init {
                let value = self.compile_expression(expression)?;
                self.emit(Instr::SetBinding {
                    binding,
                    src: self.slot(value),
                });
            }
        }
        Ok(())
    }

    fn compile_expression<'node>(
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
                if !literal.value.is_finite() {
                    return Err(unsupported(
                        self.program.source,
                        literal.span,
                        "non-finite numeric literal",
                    ));
                }
                self.emit_constant(Constant::Number(literal.value))
            }
            Expression::StringLiteral(literal) => {
                let value = literal.value.as_str().ok_or_else(|| {
                    unsupported(
                        self.program.source,
                        literal.span,
                        "string with a lone surrogate",
                    )
                })?;
                self.emit_constant(Constant::String(value.to_owned()))
            }
            Expression::Identifier(identifier) => {
                if let Some(binding) = self.lookup(identifier.name.as_str()) {
                    let dst = self.alloc_reg()?;
                    self.emit(Instr::GetBinding {
                        dst: self.slot(dst),
                        binding,
                    });
                    Ok(dst)
                } else if identifier.name.as_str() == "undefined" {
                    self.emit_undefined()
                } else if self.is_top_or_import(identifier.name.as_str()) {
                    Err(CompileError::new(
                        self.program.source,
                        identifier.span,
                        format!(
                            "function/import value {:?} is not first-class in this PoC",
                            identifier.name.as_str()
                        ),
                    ))
                } else {
                    Err(CompileError::new(
                        self.program.source,
                        identifier.span,
                        format!("unknown binding {:?}", identifier.name.as_str()),
                    ))
                }
            }
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
                        self.program.source,
                        expression.span,
                        "empty sequence expression",
                    )
                })
            }
            Expression::BinaryExpression(expression) => self.compile_binary(expression),
            Expression::LogicalExpression(expression) => self.compile_logical(expression),
            Expression::ConditionalExpression(expression) => {
                let result = self.alloc_reg()?;
                let test = self.compile_expression(&expression.test)?;
                let alternate_branch = self.emit(Instr::BranchFalse {
                    cond: self.slot(test),
                    target: u32::MAX,
                });
                let consequent = self.compile_expression(&expression.consequent)?;
                self.emit(Instr::Move {
                    dst: self.slot(result),
                    src: self.slot(consequent),
                });
                let end = self.emit(Instr::Jump { target: u32::MAX });
                let alternate = self.code.len();
                self.patch_jump(alternate_branch, alternate)?;
                let alternate_value = self.compile_expression(&expression.alternate)?;
                self.emit(Instr::Move {
                    dst: self.slot(result),
                    src: self.slot(alternate_value),
                });
                let end_target = self.code.len();
                self.patch_jump(end, end_target)?;
                Ok(result)
            }
            Expression::UnaryExpression(expression) => self.compile_unary(expression),
            Expression::AwaitExpression(expression) => {
                if !self.is_async {
                    return Err(unsupported(
                        self.program.source,
                        expression.span,
                        "await outside async function",
                    ));
                }
                let input = self.compile_expression(&expression.argument)?;
                let result = self.alloc_reg()?;
                let resume = self.awaits.len() as u32;
                self.emit(Instr::Await {
                    dst: self.slot(result),
                    input: self.slot(input),
                    resume,
                });
                let skip_reject = self.emit(Instr::Jump { target: u32::MAX });
                let reject_pc = self.code.len();
                self.emit(Instr::Throw {
                    src: self.slot(result),
                });
                let fulfill_pc = self.code.len();
                self.patch_jump(skip_reject, fulfill_pc)?;
                self.awaits.push(AwaitInfo {
                    result,
                    fulfill_pc,
                    reject_pc,
                });
                Ok(result)
            }
            Expression::CallExpression(expression) => self.compile_call(expression),
            Expression::AssignmentExpression(expression) => self.compile_assignment(expression),
            Expression::ObjectExpression(expression) => self.compile_object(expression),
            Expression::ArrayExpression(expression) => self.compile_array(expression),
            Expression::StaticMemberExpression(member) => {
                if member.optional {
                    return Err(unsupported(
                        self.program.source,
                        member.span,
                        "optional member access",
                    ));
                }
                let object = self.compile_expression(&member.object)?;
                let key =
                    self.emit_constant(Constant::String(member.property.name.as_str().to_owned()))?;
                let dst = self.alloc_reg()?;
                self.emit(Instr::GetMember {
                    dst: self.slot(dst),
                    object: self.slot(object),
                    key: self.slot(key),
                });
                Ok(dst)
            }
            Expression::ComputedMemberExpression(member) => {
                if member.optional {
                    return Err(unsupported(
                        self.program.source,
                        member.span,
                        "optional member access",
                    ));
                }
                let object = self.compile_expression(&member.object)?;
                let key = self.compile_expression(&member.expression)?;
                let dst = self.alloc_reg()?;
                self.emit(Instr::GetMember {
                    dst: self.slot(dst),
                    object: self.slot(object),
                    key: self.slot(key),
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
                let dst = self.alloc_reg()?;
                self.emit(Instr::Closure {
                    dst: self.slot(dst),
                    function: func_id,
                });
                Ok(dst)
            }
            Expression::ArrowFunctionExpression(arrow) => {
                let func_id =
                    self.enqueue_function(FunctionSource::Arrow(arrow), "arrow", arrow.span)?;
                let dst = self.alloc_reg()?;
                self.emit(Instr::Closure {
                    dst: self.slot(dst),
                    function: func_id,
                });
                Ok(dst)
            }
            other => Err(unsupported(
                self.program.source,
                other.span(),
                "expression form outside the PoC subset",
            )),
        }
    }

    fn enqueue_function(
        &mut self,
        source: FunctionSource<'ast>,
        hint: &str,
        span: Span,
    ) -> Result<u32, CompileError> {
        let func_id = self.program.next_func_id;
        self.program.next_func_id =
            self.program.next_func_id.checked_add(1).ok_or_else(|| {
                CompileError::new(self.program.source, span, "too many functions")
            })?;
        let mut outer_scopes = self.outer_scopes.clone();
        outer_scopes.push(self.function_scope.clone());
        outer_scopes.extend(self.scopes.iter().cloned());
        self.program.tasks.push_back(FunctionTask {
            source,
            func_id,
            name: format!("{hint}@{func_id}"),
            outer_scopes,
        });
        Ok(func_id)
    }

    fn emit_constant(&mut self, constant: Constant) -> Result<Reg, CompileError> {
        let index = self.program.constants.len() as u32;
        self.program.constants.push(constant);
        let dst = self.alloc_reg()?;
        self.emit(Instr::Const {
            dst: self.slot(dst),
            constant: index,
        });
        Ok(dst)
    }

    fn emit_undefined(&mut self) -> Result<Reg, CompileError> {
        let dst = self.alloc_reg()?;
        self.emit(Instr::Undefined {
            dst: self.slot(dst),
        });
        Ok(dst)
    }

    fn emit_number(&mut self, value: f64) -> Result<Reg, CompileError> {
        self.emit_constant(Constant::Number(value))
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
            BinaryOperator::In | BinaryOperator::Instanceof => {
                return Err(unsupported(
                    self.program.source,
                    expression.span,
                    if matches!(expression.operator, BinaryOperator::In) {
                        "in operator"
                    } else {
                        "instanceof operator"
                    },
                ));
            }
        };
        let lhs = self.compile_expression(&expression.left)?;
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

    fn compile_logical(
        &mut self,
        expression: &'ast LogicalExpression<'ast>,
    ) -> Result<Reg, CompileError> {
        let result = self.alloc_reg()?;
        let left = self.compile_expression(&expression.left)?;
        self.emit(Instr::Move {
            dst: self.slot(result),
            src: self.slot(left),
        });
        match expression.operator {
            LogicalOperator::Or => {
                let right_branch = self.emit(Instr::BranchFalse {
                    cond: self.slot(left),
                    target: u32::MAX,
                });
                let end = self.emit(Instr::Jump { target: u32::MAX });
                let right_pc = self.code.len();
                self.patch_jump(right_branch, right_pc)?;
                let right = self.compile_expression(&expression.right)?;
                self.emit(Instr::Move {
                    dst: self.slot(result),
                    src: self.slot(right),
                });
                let end_pc = self.code.len();
                self.patch_jump(end, end_pc)?;
            }
            LogicalOperator::And => {
                let end = self.emit(Instr::BranchFalse {
                    cond: self.slot(left),
                    target: u32::MAX,
                });
                let right = self.compile_expression(&expression.right)?;
                self.emit(Instr::Move {
                    dst: self.slot(result),
                    src: self.slot(right),
                });
                let end_pc = self.code.len();
                self.patch_jump(end, end_pc)?;
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
                let check_undefined = self.emit(Instr::BranchFalse {
                    cond: self.slot(is_null),
                    target: u32::MAX,
                });
                let right_jump = self.emit(Instr::Jump { target: u32::MAX });
                let undefined_pc = self.code.len();
                self.patch_jump(check_undefined, undefined_pc)?;
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
                let end = self.emit(Instr::BranchFalse {
                    cond: self.slot(is_undefined),
                    target: u32::MAX,
                });
                let right_pc = self.code.len();
                self.patch_jump(right_jump, right_pc)?;
                let right = self.compile_expression(&expression.right)?;
                self.emit(Instr::Move {
                    dst: self.slot(result),
                    src: self.slot(right),
                });
                let end_pc = self.code.len();
                self.patch_jump(end, end_pc)?;
            }
        }
        Ok(result)
    }

    fn compile_unary(
        &mut self,
        expression: &'ast UnaryExpression<'ast>,
    ) -> Result<Reg, CompileError> {
        if matches!(expression.operator, UnaryOperator::Delete) {
            return Err(unsupported(self.program.source, expression.span, "delete"));
        }
        if matches!(expression.operator, UnaryOperator::Typeof)
            && let Expression::Identifier(identifier) = &expression.argument
        {
            let name = identifier.name.as_str();
            if !self.is_known_name(name) {
                return self.emit_constant(Constant::String("undefined".to_owned()));
            }
            if self.is_top_or_import(name) {
                return self.emit_constant(Constant::String("function".to_owned()));
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

    fn compile_call(
        &mut self,
        expression: &'ast CallExpression<'ast>,
    ) -> Result<Reg, CompileError> {
        if expression.optional {
            return Err(unsupported(
                self.program.source,
                expression.span,
                "optional call",
            ));
        }

        if let Expression::Identifier(callee) = &expression.callee {
            let name = callee.name.as_str();
            if let Some(binding) = self.lookup(name) {
                let callee = self.alloc_reg()?;
                self.emit(Instr::GetBinding {
                    dst: self.slot(callee),
                    binding,
                });
                let args = self.compile_arguments(expression)?;
                let dst = self.alloc_reg()?;
                self.emit(Instr::CallValue {
                    dst: self.slot(dst),
                    callee: self.slot(callee),
                    args,
                });
                return Ok(dst);
            }
            if let Some(import) = self.program.import_names.get(name).copied() {
                let args = self.compile_arguments(expression)?;
                let dst = self.alloc_reg()?;
                self.emit(Instr::CallImport {
                    dst: self.slot(dst),
                    import,
                    args,
                });
                return Ok(dst);
            }
            if let Some(function) = self.program.top_level_names.get(name).copied() {
                let args = self.compile_arguments(expression)?;
                let dst = self.alloc_reg()?;
                self.emit(Instr::CallFunction {
                    dst: self.slot(dst),
                    function,
                    args,
                });
                return Ok(dst);
            }
            return Err(CompileError::new(
                self.program.source,
                callee.span,
                format!("unknown callee {name:?}"),
            ));
        }

        // Member/value callees run before argument evaluation.
        let callee = self.compile_expression(&expression.callee)?;
        let args = self.compile_arguments(expression)?;
        let dst = self.alloc_reg()?;
        self.emit(Instr::CallValue {
            dst: self.slot(dst),
            callee: self.slot(callee),
            args,
        });
        Ok(dst)
    }

    fn compile_arguments(
        &mut self,
        expression: &'ast CallExpression<'ast>,
    ) -> Result<Vec<Slot>, CompileError> {
        let mut args = Vec::with_capacity(expression.arguments.len());
        for argument in &expression.arguments {
            let Some(expression) = argument.as_expression() else {
                return Err(unsupported(
                    self.program.source,
                    argument.span(),
                    "spread argument",
                ));
            };
            let reg = self.compile_expression(expression)?;
            args.push(self.slot(reg));
        }
        Ok(args)
    }

    fn compile_assignment(
        &mut self,
        expression: &'ast AssignmentExpression<'ast>,
    ) -> Result<Reg, CompileError> {
        if let AssignmentTarget::AssignmentTargetIdentifier(identifier) = &expression.left {
            let name = identifier.name.as_str();
            let binding = self.lookup(name).ok_or_else(|| {
                CompileError::new(
                    self.program.source,
                    identifier.span,
                    format!("assignment to unknown binding {name:?}"),
                )
            })?;
            let value = if matches!(expression.operator, AssignmentOperator::Assign) {
                self.compile_expression(&expression.right)?
            } else {
                let old = self.alloc_reg()?;
                self.emit(Instr::GetBinding {
                    dst: self.slot(old),
                    binding,
                });
                let right = self.compile_expression(&expression.right)?;
                let op = binary_assignment_operator(self.program.source, expression)?;
                let value = self.alloc_reg()?;
                self.emit(Instr::Binary {
                    dst: self.slot(value),
                    op,
                    lhs: self.slot(old),
                    rhs: self.slot(right),
                });
                value
            };
            self.emit(Instr::SetBinding {
                binding,
                src: self.slot(value),
            });
            let result = self.alloc_reg()?;
            self.emit(Instr::Move {
                dst: self.slot(result),
                src: self.slot(value),
            });
            return Ok(result);
        }

        if let Some(member) = expression.left.as_member_expression() {
            let (object, key) = self.compile_member_parts(member)?;
            let value = if matches!(expression.operator, AssignmentOperator::Assign) {
                self.compile_expression(&expression.right)?
            } else {
                let old = self.alloc_reg()?;
                self.emit(Instr::GetMember {
                    dst: self.slot(old),
                    object: self.slot(object),
                    key: self.slot(key),
                });
                let right = self.compile_expression(&expression.right)?;
                let op = binary_assignment_operator(self.program.source, expression)?;
                let value = self.alloc_reg()?;
                self.emit(Instr::Binary {
                    dst: self.slot(value),
                    op,
                    lhs: self.slot(old),
                    rhs: self.slot(right),
                });
                value
            };
            self.emit(Instr::SetMember {
                object: self.slot(object),
                key: self.slot(key),
                src: self.slot(value),
            });
            let result = self.alloc_reg()?;
            self.emit(Instr::Move {
                dst: self.slot(result),
                src: self.slot(value),
            });
            return Ok(result);
        }

        Err(unsupported(
            self.program.source,
            expression.span,
            "assignment to non-identifier/non-member target",
        ))
    }

    fn compile_member_parts(
        &mut self,
        member: &'ast MemberExpression<'ast>,
    ) -> Result<(Reg, Reg), CompileError> {
        match member {
            MemberExpression::StaticMemberExpression(member) => {
                if member.optional {
                    return Err(unsupported(
                        self.program.source,
                        member.span,
                        "optional member access",
                    ));
                }
                let object = self.compile_expression(&member.object)?;
                let key =
                    self.emit_constant(Constant::String(member.property.name.as_str().to_owned()))?;
                Ok((object, key))
            }
            MemberExpression::ComputedMemberExpression(member) => {
                if member.optional {
                    return Err(unsupported(
                        self.program.source,
                        member.span,
                        "optional member access",
                    ));
                }
                let object = self.compile_expression(&member.object)?;
                let key = self.compile_expression(&member.expression)?;
                Ok((object, key))
            }
            MemberExpression::PrivateFieldExpression(member) => Err(unsupported(
                self.program.source,
                member.span,
                "private field access",
            )),
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
                return Err(unsupported(
                    self.program.source,
                    property.span(),
                    "object spread",
                ));
            };
            if property.method || property.kind != PropertyKind::Init {
                return Err(unsupported(
                    self.program.source,
                    property.span,
                    "object method/getter/setter",
                ));
            }
            let key = if let Some(name) = property.key.static_name() {
                let name = name.into_cow_str().ok_or_else(|| {
                    unsupported(
                        self.program.source,
                        property.key.span(),
                        "property key with a lone surrogate",
                    )
                })?;
                self.emit_constant(Constant::String(name.into_owned()))?
            } else if property.computed {
                let key = property.key.as_expression().ok_or_else(|| {
                    unsupported(
                        self.program.source,
                        property.key.span(),
                        "unsupported computed property key",
                    )
                })?;
                self.compile_expression(key)?
            } else {
                return Err(unsupported(
                    self.program.source,
                    property.key.span(),
                    "unsupported property key",
                ));
            };
            let value = self.compile_expression(&property.value)?;
            self.emit(Instr::SetMember {
                object: self.slot(dst),
                key: self.slot(key),
                src: self.slot(value),
            });
        }
        Ok(dst)
    }

    fn compile_array(&mut self, array: &'ast ArrayExpression<'ast>) -> Result<Reg, CompileError> {
        let mut elements = Vec::with_capacity(array.elements.len());
        for element in &array.elements {
            match element {
                ArrayExpressionElement::Elision(_) => {
                    let value = self.emit_undefined()?;
                    elements.push(self.slot(value));
                }
                ArrayExpressionElement::SpreadElement(_) => {
                    return Err(unsupported(
                        self.program.source,
                        element.span(),
                        "array spread",
                    ));
                }
                other => {
                    let expression = other.as_expression().ok_or_else(|| {
                        unsupported(
                            self.program.source,
                            other.span(),
                            "array element outside the PoC subset",
                        )
                    })?;
                    let value = self.compile_expression(expression)?;
                    elements.push(self.slot(value));
                }
            }
        }
        let dst = self.alloc_reg()?;
        self.emit(Instr::NewArray {
            dst: self.slot(dst),
            elements,
        });
        Ok(dst)
    }

    fn compile_try(&mut self, statement: &'ast TryStatement<'ast>) -> Result<(), CompileError> {
        let has_finalizer = statement.finalizer.is_some();
        let exception = self.alloc_reg()?;
        if has_finalizer {
            let kind = self.alloc_reg()?;
            let payload = self.alloc_reg()?;
            self.finalizers.push(FinalizerContext {
                kind,
                payload,
                entry: None,
                entry_jumps: Vec::new(),
            });
        }

        let try_start = self.code.len();
        self.compile_block(&statement.block)?;
        let try_end = self.code.len();
        let mut end_jumps = Vec::new();
        if has_finalizer {
            self.emit_completion_to_finalizer(COMPLETION_NORMAL, None)?;
        } else {
            end_jumps.push(self.emit(Instr::Jump { target: u32::MAX }));
        }

        let mut catch_body = None;
        if let Some(handler) = &statement.handler {
            let catch_stub = self.code.len();
            self.scopes.push(BTreeMap::new());
            if let Some(parameter) = &handler.param {
                let BindingPattern::BindingIdentifier(identifier) = &parameter.pattern else {
                    return Err(unsupported(
                        self.program.source,
                        parameter.span,
                        "catch parameter destructuring",
                    ));
                };
                let (binding, _) = self.declare(
                    identifier.name.as_str(),
                    VariableDeclarationKind::Let,
                    parameter.span,
                )?;
                self.emit(Instr::DeclareBinding { binding });
                self.emit(Instr::SetBinding {
                    binding,
                    src: self.slot(exception),
                });
            }
            let catch_start = self.code.len();
            self.predeclare_scope(&handler.body.body)?;
            for statement in &handler.body.body {
                self.compile_statement(statement)?;
            }
            if has_finalizer {
                self.emit_completion_to_finalizer(COMPLETION_NORMAL, None)?;
            } else {
                end_jumps.push(self.emit(Instr::Jump { target: u32::MAX }));
            }
            let catch_region_end = self.code.len();
            self.scopes.pop();
            catch_body = Some((catch_stub, catch_start, catch_region_end));
        }

        if has_finalizer {
            // Stop routing returns from the finalizer to itself.
            let mut context = self.finalizers.pop().expect("finalizer stack");
            let throw_stub = self.code.len();
            self.emit(Instr::Move {
                dst: self.slot(context.payload),
                src: self.slot(exception),
            });
            let throw_kind = self.emit_number(COMPLETION_THROW)?;
            self.emit(Instr::Move {
                dst: self.slot(context.kind),
                src: self.slot(throw_kind),
            });
            let jump = self.emit(Instr::Jump { target: u32::MAX });
            context.entry_jumps.push(jump);

            if let Some((catch_stub, catch_start, catch_region_end)) = catch_body {
                self.handlers.push(ExceptionHandler {
                    start_pc: catch_start as u32,
                    end_pc: catch_region_end as u32,
                    target_pc: throw_stub as u32,
                    dst: self.slot(exception),
                });
                self.handlers.push(ExceptionHandler {
                    start_pc: try_start as u32,
                    end_pc: try_end as u32,
                    target_pc: catch_stub as u32,
                    dst: self.slot(exception),
                });
            } else {
                self.handlers.push(ExceptionHandler {
                    start_pc: try_start as u32,
                    end_pc: try_end as u32,
                    target_pc: throw_stub as u32,
                    dst: self.slot(exception),
                });
            }

            let entry = self.code.len();
            for jump in context.entry_jumps.drain(..) {
                self.patch_jump(jump, entry)?;
            }
            self.compile_block(statement.finalizer.as_ref().expect("finalizer"))?;
            self.emit_finally_dispatch(&context)?;
        } else {
            if let Some((catch_stub, _, _)) = catch_body {
                self.handlers.push(ExceptionHandler {
                    start_pc: try_start as u32,
                    end_pc: try_end as u32,
                    target_pc: catch_stub as u32,
                    dst: self.slot(exception),
                });
            }
            let end = self.code.len();
            for jump in end_jumps {
                self.patch_jump(jump, end)?;
            }
        }

        Ok(())
    }

    fn emit_completion_to_finalizer(
        &mut self,
        kind_value: f64,
        payload: Option<Reg>,
    ) -> Result<usize, CompileError> {
        let (kind, payload_slot) = {
            let context = self.finalizers.last().ok_or_else(|| {
                CompileError::new(
                    self.program.source,
                    self.span,
                    "internal compiler error: missing finalizer",
                )
            })?;
            (context.kind, context.payload)
        };
        let kind_reg = self.emit_number(kind_value)?;
        self.emit(Instr::Move {
            dst: self.slot(kind),
            src: self.slot(kind_reg),
        });
        match payload {
            Some(reg) => {
                self.emit(Instr::Move {
                    dst: self.slot(payload_slot),
                    src: self.slot(reg),
                });
            }
            None => {
                self.emit(Instr::Undefined {
                    dst: self.slot(payload_slot),
                });
            }
        }
        let jump = self.emit(Instr::Jump { target: u32::MAX });
        self.finalizers
            .last_mut()
            .expect("finalizer checked above")
            .entry_jumps
            .push(jump);
        Ok(jump)
    }

    fn emit_return(&mut self, value: Reg) -> Result<(), CompileError> {
        if self.finalizers.is_empty() {
            self.emit(Instr::Return {
                src: self.slot(value),
            });
            return Ok(());
        }
        self.emit_completion_to_finalizer(COMPLETION_RETURN, Some(value))?;
        Ok(())
    }

    fn emit_finally_dispatch(&mut self, context: &FinalizerContext) -> Result<usize, CompileError> {
        let normal = self.emit_number(COMPLETION_NORMAL)?;
        let is_normal = self.alloc_reg()?;
        self.emit(Instr::Binary {
            dst: self.slot(is_normal),
            op: BinaryOp::StrictEq,
            lhs: self.slot(context.kind),
            rhs: self.slot(normal),
        });
        let not_normal = self.emit(Instr::BranchFalse {
            cond: self.slot(is_normal),
            target: u32::MAX,
        });
        let normal_jump = self.emit(Instr::Jump { target: u32::MAX });
        let not_normal_pc = self.code.len();
        self.patch_jump(not_normal, not_normal_pc)?;

        let returning = self.emit_number(COMPLETION_RETURN)?;
        let is_return = self.alloc_reg()?;
        self.emit(Instr::Binary {
            dst: self.slot(is_return),
            op: BinaryOp::StrictEq,
            lhs: self.slot(context.kind),
            rhs: self.slot(returning),
        });
        let not_return = self.emit(Instr::BranchFalse {
            cond: self.slot(is_return),
            target: u32::MAX,
        });
        self.emit_pending_return(context.payload)?;
        let throw_pc = self.code.len();
        self.patch_jump(not_return, throw_pc)?;
        self.emit(Instr::Throw {
            src: self.slot(context.payload),
        });
        let end = self.code.len();
        self.patch_jump(normal_jump, end)?;
        Ok(end)
    }

    fn emit_pending_return(&mut self, payload: Reg) -> Result<(), CompileError> {
        if let Some(outer) = self.finalizers.last() {
            let (kind, outer_payload, entry) = (outer.kind, outer.payload, outer.entry);
            self.emit(Instr::Move {
                dst: self.slot(outer_payload),
                src: self.slot(payload),
            });
            let returning = self.emit_number(COMPLETION_RETURN)?;
            self.emit(Instr::Move {
                dst: self.slot(kind),
                src: self.slot(returning),
            });
            if let Some(entry) = entry {
                self.emit(Instr::Jump {
                    target: entry as u32,
                });
            } else {
                let jump = self.emit(Instr::Jump { target: u32::MAX });
                self.finalizers
                    .last_mut()
                    .expect("outer finalizer")
                    .entry_jumps
                    .push(jump);
            }
        } else {
            self.emit(Instr::Return {
                src: self.slot(payload),
            });
        }
        Ok(())
    }
}

fn binary_assignment_operator(
    source: &str,
    expression: &AssignmentExpression<'_>,
) -> Result<BinaryOp, CompileError> {
    match expression.operator {
        AssignmentOperator::Assign => unreachable!(),
        AssignmentOperator::Addition => Ok(BinaryOp::Add),
        AssignmentOperator::Subtraction => Ok(BinaryOp::Sub),
        AssignmentOperator::Multiplication => Ok(BinaryOp::Mul),
        AssignmentOperator::Division => Ok(BinaryOp::Div),
        AssignmentOperator::Remainder => Ok(BinaryOp::Rem),
        AssignmentOperator::Exponential => Ok(BinaryOp::Exp),
        AssignmentOperator::ShiftLeft => Ok(BinaryOp::Shl),
        AssignmentOperator::ShiftRight => Ok(BinaryOp::Shr),
        AssignmentOperator::ShiftRightZeroFill => Ok(BinaryOp::UShr),
        AssignmentOperator::BitwiseOR => Ok(BinaryOp::BitOr),
        AssignmentOperator::BitwiseXOR => Ok(BinaryOp::BitXor),
        AssignmentOperator::BitwiseAnd => Ok(BinaryOp::BitAnd),
        AssignmentOperator::LogicalOr
        | AssignmentOperator::LogicalAnd
        | AssignmentOperator::LogicalNullish => {
            Err(unsupported(source, expression.span, "logical assignment"))
        }
    }
}

fn remap_instruction(instruction: &Instr, remap: &impl Fn(Slot) -> Slot) -> Instr {
    match instruction.clone() {
        Instr::Const { dst, constant } => Instr::Const {
            dst: remap(dst),
            constant,
        },
        Instr::Undefined { dst } => Instr::Undefined { dst: remap(dst) },
        Instr::Move { dst, src } => Instr::Move {
            dst: remap(dst),
            src: remap(src),
        },
        Instr::Binary { dst, op, lhs, rhs } => Instr::Binary {
            dst: remap(dst),
            op,
            lhs: remap(lhs),
            rhs: remap(rhs),
        },
        Instr::Unary { dst, op, src } => Instr::Unary {
            dst: remap(dst),
            op,
            src: remap(src),
        },
        Instr::CallImport { dst, import, args } => Instr::CallImport {
            dst: remap(dst),
            import,
            args: args.into_iter().map(remap).collect(),
        },
        Instr::CallFunction {
            dst,
            function,
            args,
        } => Instr::CallFunction {
            dst: remap(dst),
            function,
            args: args.into_iter().map(remap).collect(),
        },
        Instr::CallValue { dst, callee, args } => Instr::CallValue {
            dst: remap(dst),
            callee: remap(callee),
            args: args.into_iter().map(remap).collect(),
        },
        Instr::Closure { dst, function } => Instr::Closure {
            dst: remap(dst),
            function,
        },
        Instr::NewObject { dst } => Instr::NewObject { dst: remap(dst) },
        Instr::NewArray { dst, elements } => Instr::NewArray {
            dst: remap(dst),
            elements: elements.into_iter().map(remap).collect(),
        },
        Instr::GetMember { dst, object, key } => Instr::GetMember {
            dst: remap(dst),
            object: remap(object),
            key: remap(key),
        },
        Instr::SetMember { object, key, src } => Instr::SetMember {
            object: remap(object),
            key: remap(key),
            src: remap(src),
        },
        Instr::GetBinding { dst, binding } => Instr::GetBinding {
            dst: remap(dst),
            binding,
        },
        Instr::SetBinding { binding, src } => Instr::SetBinding {
            binding,
            src: remap(src),
        },
        Instr::DeclareBinding { binding } => Instr::DeclareBinding { binding },
        Instr::Await { dst, input, resume } => Instr::Await {
            dst: remap(dst),
            input: remap(input),
            resume,
        },
        Instr::Jump { target } => Instr::Jump { target },
        Instr::BranchFalse { cond, target } => Instr::BranchFalse {
            cond: remap(cond),
            target,
        },
        Instr::Return { src } => Instr::Return { src: remap(src) },
        Instr::Throw { src } => Instr::Throw { src: remap(src) },
    }
}

fn compute_live_in(
    code: &[Instr],
    resumes: &[ResumeEntry],
    handlers: &[ExceptionHandler],
) -> Result<Vec<BTreeSet<Reg>>, CompileError> {
    let mut live_in = vec![BTreeSet::new(); code.len()];
    loop {
        let mut changed = false;
        for pc in (0..code.len()).rev() {
            let (uses, defs) = instruction_uses_defs(&code[pc]);
            let mut next = uses;
            for successor in normal_successors(code, resumes, pc)? {
                next.extend(live_in[successor].iter().copied());
            }
            if instruction_may_throw(&code[pc]) {
                for handler in handlers.iter().filter(|handler| {
                    handler.start_pc as usize <= pc && pc < handler.end_pc as usize
                }) {
                    let mut target = live_in[handler.target_pc as usize].clone();
                    target.remove(&handler.dst.index);
                    next.extend(target);
                }
            }
            for definition in defs {
                next.remove(&definition);
            }
            if next != live_in[pc] {
                live_in[pc] = next;
                changed = true;
            }
        }
        if !changed {
            return Ok(live_in);
        }
    }
}

fn instruction_may_throw(instruction: &Instr) -> bool {
    matches!(
        instruction,
        Instr::Throw { .. } | Instr::CallFunction { .. } | Instr::CallValue { .. }
    )
}

fn instruction_uses_defs(instruction: &Instr) -> (BTreeSet<Reg>, BTreeSet<Reg>) {
    let mut uses = BTreeSet::new();
    let mut defs = BTreeSet::new();
    match instruction {
        Instr::Const { dst, .. } | Instr::Undefined { dst } => {
            defs.insert(dst.index);
        }
        Instr::Move { dst, src } => {
            uses.insert(src.index);
            defs.insert(dst.index);
        }
        Instr::Binary { dst, lhs, rhs, .. } => {
            uses.insert(lhs.index);
            uses.insert(rhs.index);
            defs.insert(dst.index);
        }
        Instr::Unary { dst, src, .. } => {
            uses.insert(src.index);
            defs.insert(dst.index);
        }
        Instr::CallImport { dst, args, .. } | Instr::CallFunction { dst, args, .. } => {
            for arg in args {
                uses.insert(arg.index);
            }
            defs.insert(dst.index);
        }
        Instr::CallValue { dst, callee, args } => {
            uses.insert(callee.index);
            for arg in args {
                uses.insert(arg.index);
            }
            defs.insert(dst.index);
        }
        Instr::Closure { dst, .. } | Instr::NewObject { dst } | Instr::GetBinding { dst, .. } => {
            defs.insert(dst.index);
        }
        Instr::NewArray { dst, elements } => {
            for element in elements {
                uses.insert(element.index);
            }
            defs.insert(dst.index);
        }
        Instr::GetMember { dst, object, key } => {
            uses.insert(object.index);
            uses.insert(key.index);
            defs.insert(dst.index);
        }
        Instr::SetMember { object, key, src } => {
            uses.insert(object.index);
            uses.insert(key.index);
            uses.insert(src.index);
        }
        Instr::SetBinding { src, .. } => {
            uses.insert(src.index);
        }
        Instr::DeclareBinding { .. } => {}
        Instr::Await { dst, input, .. } => {
            uses.insert(input.index);
            defs.insert(dst.index);
        }
        Instr::Jump { .. } => {}
        Instr::BranchFalse { cond, .. } => {
            uses.insert(cond.index);
        }
        Instr::Return { src } | Instr::Throw { src } => {
            uses.insert(src.index);
        }
    }
    (uses, defs)
}

fn normal_successors(
    code: &[Instr],
    resumes: &[ResumeEntry],
    pc: usize,
) -> Result<Vec<usize>, CompileError> {
    let result = match &code[pc] {
        Instr::Await { resume, .. } => {
            let resume = resumes.get(*resume as usize).ok_or_else(|| {
                CompileError::new(
                    "",
                    Span::default(),
                    "internal liveness error: missing resume entry",
                )
            })?;
            vec![resume.fulfill_pc as usize, resume.reject_pc as usize]
        }
        Instr::Jump { target } => vec![*target as usize],
        Instr::BranchFalse { target, .. } => vec![*target as usize, pc + 1],
        Instr::Return { .. } | Instr::Throw { .. } => Vec::new(),
        _ => vec![pc + 1],
    };
    for successor in &result {
        if *successor >= code.len() {
            return Err(CompileError::new(
                "",
                Span::default(),
                "internal liveness error: successor out of range",
            ));
        }
    }
    Ok(result)
}
