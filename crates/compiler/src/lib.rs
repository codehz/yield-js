//! Strict-mode JavaScript subset compiler for the resumable VM PoC.
//!
//! The compiler consumes Oxc's AST and emits the VM-owned bytecode contract.
//! Oxc types do not escape this crate.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use oxc::allocator::Allocator;
use oxc::ast::ast::*;
use oxc::diagnostics::Diagnostic;
use oxc::parser::Parser;
use oxc::span::{GetSpan, SourceType, Span};
use oxc::syntax::operator::{AssignmentOperator, BinaryOperator, LogicalOperator, UnaryOperator};
use yield_vm::bytecode::{
    BinaryOp, Build, Constant, Function as BytecodeFunction, ImportDecl, Instr, ResumeEntry, Slot,
    SlotKind, UnaryOp,
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
    let program = parser_return.program;

    let mut functions = Vec::new();
    let mut function_names = BTreeMap::new();
    let mut declarations = Vec::new();
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
                if function_names.contains_key(&name) {
                    return Err(CompileError::new(
                        source,
                        function.span,
                        format!("duplicate function declaration {name:?}"),
                    ));
                }
                let func_id = declarations.len() as u32;
                function_names.insert(name, func_id);
                declarations.push(function);
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

    let mut constants = Vec::new();
    for (func_id, function) in declarations.iter().enumerate() {
        let function = compile_function(
            source,
            function,
            func_id as u32,
            &mut constants,
            &import_names,
            &function_names,
        )?;
        functions.push(function);
    }

    let mut build = Build::new(constants, imports, functions);
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

struct AwaitInfo {
    pc: usize,
    result: Reg,
}

struct LoopContext {
    break_jumps: Vec<usize>,
    continue_jumps: Vec<usize>,
    continue_target: Option<usize>,
}

struct FunctionBuilder<'a> {
    source: &'a str,
    import_names: &'a BTreeMap<String, u32>,
    function_names: &'a BTreeMap<String, u32>,
    constants: &'a mut Vec<Constant>,
    func_id: u32,
    name: String,
    is_async: bool,
    in_async: bool,
    next_reg: Reg,
    param_slots: Vec<Slot>,
    function_scope: BTreeMap<String, Reg>,
    scopes: Vec<BTreeMap<String, Reg>>,
    code: Vec<Instr>,
    awaits: Vec<AwaitInfo>,
    loops: Vec<LoopContext>,
}

impl<'a> FunctionBuilder<'a> {
    fn new(
        source: &'a str,
        import_names: &'a BTreeMap<String, u32>,
        function_names: &'a BTreeMap<String, u32>,
        constants: &'a mut Vec<Constant>,
        func_id: u32,
        name: String,
        is_async: bool,
    ) -> Self {
        Self {
            source,
            import_names,
            function_names,
            constants,
            func_id,
            name,
            is_async,
            in_async: is_async,
            next_reg: 0,
            param_slots: Vec::new(),
            function_scope: BTreeMap::new(),
            scopes: vec![BTreeMap::new()],
            code: Vec::new(),
            awaits: Vec::new(),
            loops: Vec::new(),
        }
    }

    fn compile<'ast>(
        &mut self,
        function: &'ast Function<'ast>,
    ) -> Result<BytecodeFunction, CompileError> {
        if function.generator {
            return Err(unsupported(self.source, function.span, "generator"));
        }
        if function.params.rest.is_some() {
            return Err(unsupported(self.source, function.span, "rest parameter"));
        }
        for parameter in &function.params.items {
            if parameter.initializer.is_some() {
                return Err(unsupported(
                    self.source,
                    parameter.span,
                    "default parameter",
                ));
            }
            let BindingPattern::BindingIdentifier(identifier) = &parameter.pattern else {
                return Err(unsupported(
                    self.source,
                    parameter.span,
                    "parameter destructuring",
                ));
            };
            let name = identifier.name.as_str().to_owned();
            if self.function_scope.contains_key(&name) {
                return Err(CompileError::new(
                    self.source,
                    parameter.span,
                    format!("duplicate parameter {name:?}"),
                ));
            }
            let reg = self.alloc_reg()?;
            self.function_scope.insert(name, reg);
            self.param_slots.push(self.slot(reg));
        }

        let body = function
            .body
            .as_ref()
            .ok_or_else(|| unsupported(self.source, function.span, "declaration without body"))?;
        for statement in &body.statements {
            self.compile_statement(statement)?;
        }

        // Implicit `return undefined`.
        let implicit = self.alloc_reg()?;
        self.emit(Instr::Undefined {
            dst: self.slot(implicit),
        });
        self.emit(Instr::Return {
            src: self.slot(implicit),
        });

        // A reject landing point exists per `AWAIT`. The rejection value is
        // written into the same result slot by the VM before entering it.
        let reject_base = self.code.len();
        let reject_slots = self
            .awaits
            .iter()
            .map(|await_info| self.slot(await_info.result))
            .collect::<Vec<_>>();
        for src in reject_slots {
            self.emit(Instr::Throw { src });
        }

        let mut resumes = Vec::with_capacity(self.awaits.len());
        for (resume_id, await_info) in self.awaits.iter().enumerate() {
            resumes.push(ResumeEntry {
                fulfill_pc: (await_info.pc + 1) as u32,
                fulfill_dst: self.slot(await_info.result),
                reject_pc: (reject_base + resume_id) as u32,
                reject_dst: self.slot(await_info.result),
                saved_slots: Vec::new(),
            });
        }

        let live_in = compute_live_in(&self.code, &resumes)?;
        let mut saved_regs = BTreeSet::new();
        for (resume_id, resume) in resumes.iter().enumerate() {
            let mut saved = live_in[resume.fulfill_pc as usize].clone();
            saved.extend(live_in[resume.reject_pc as usize].iter().copied());
            saved.remove(&resume.fulfill_dst.index);
            saved.remove(&resume.reject_dst.index);
            if saved.iter().any(|reg| *reg >= self.next_reg) {
                return Err(CompileError::new(
                    self.source,
                    function.span,
                    "internal liveness error: unknown register",
                ));
            }
            saved_regs.extend(saved.iter().copied());
            let _ = resume_id;
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
        for (resume_id, resume) in resumes.iter_mut().enumerate() {
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
            let _ = resume_id;
        }

        Ok(BytecodeFunction {
            func_id: self.func_id,
            name: self.name.clone(),
            is_async: self.is_async,
            param_slots: self.param_slots.iter().copied().map(remap).collect(),
            transient_slots: transient_map.len() as u16,
            persistent_slots: persistent_map.len() as u16,
            code,
            resumes,
        })
    }

    fn alloc_reg(&mut self) -> Result<Reg, CompileError> {
        let reg = self.next_reg;
        self.next_reg = self
            .next_reg
            .checked_add(1)
            .ok_or_else(|| CompileError::new(self.source, Span::default(), "too many slots"))?;
        Ok(reg)
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
                self.source,
                Span::default(),
                "internal compiler error: patch target is not a jump",
            )),
        }
    }

    fn declare(
        &mut self,
        name: &str,
        kind: VariableDeclarationKind,
        span: Span,
    ) -> Result<Reg, CompileError> {
        match kind {
            VariableDeclarationKind::Var => {
                if let Some(reg) = self.function_scope.get(name) {
                    return Ok(*reg);
                }
                let reg = self.alloc_reg()?;
                self.function_scope.insert(name.to_owned(), reg);
                Ok(reg)
            }
            VariableDeclarationKind::Let | VariableDeclarationKind::Const => {
                let conflicts_with_function_scope =
                    self.scopes.len() == 1 && self.function_scope.contains_key(name);
                let current = self.scopes.last().expect("scope stack is never empty");
                if current.contains_key(name) || conflicts_with_function_scope {
                    return Err(CompileError::new(
                        self.source,
                        span,
                        format!("duplicate lexical binding {name:?}"),
                    ));
                }
                let reg = self.alloc_reg()?;
                self.scopes
                    .last_mut()
                    .expect("scope stack is never empty")
                    .insert(name.to_owned(), reg);
                Ok(reg)
            }
            other => Err(unsupported(
                self.source,
                span,
                match other {
                    VariableDeclarationKind::Using => "using declaration",
                    VariableDeclarationKind::AwaitUsing => "await using declaration",
                    _ => "variable declaration kind",
                },
            )),
        }
    }

    fn lookup(&self, name: &str) -> Option<Reg> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
            .or_else(|| self.function_scope.get(name).copied())
    }

    fn is_host_or_function(&self, name: &str) -> bool {
        self.import_names.contains_key(name) || self.function_names.contains_key(name)
    }

    fn is_known_name(&self, name: &str) -> bool {
        name == "undefined" || self.lookup(name).is_some() || self.is_host_or_function(name)
    }

    fn compile_statement<'ast>(
        &mut self,
        statement: &'ast Statement<'ast>,
    ) -> Result<(), CompileError> {
        match statement {
            Statement::BlockStatement(block) => {
                self.scopes.push(BTreeMap::new());
                for statement in &block.body {
                    self.compile_statement(statement)?;
                }
                self.scopes.pop();
                Ok(())
            }
            Statement::EmptyStatement(_) => Ok(()),
            Statement::ExpressionStatement(statement) => {
                self.compile_expression(&statement.expression)?;
                Ok(())
            }
            Statement::VariableDeclaration(declaration) => {
                self.compile_variable_declaration(declaration)
            }
            Statement::FunctionDeclaration(function) => Err(unsupported(
                self.source,
                function.span,
                "nested function declaration",
            )),
            Statement::ReturnStatement(statement) => {
                let value = match &statement.argument {
                    Some(expression) => self.compile_expression(expression)?,
                    None => {
                        let reg = self.alloc_reg()?;
                        self.emit(Instr::Undefined {
                            dst: self.slot(reg),
                        });
                        reg
                    }
                };
                self.emit(Instr::Return {
                    src: self.slot(value),
                });
                Ok(())
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
                // The branch is patched below; true falls through to the back edge.
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
                                    self.source,
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
                    return Err(unsupported(self.source, statement.span, "labelled break"));
                }
                if self.loops.is_empty() {
                    return Err(CompileError::new(
                        self.source,
                        statement.span,
                        "break outside loop",
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
                        self.source,
                        statement.span,
                        "labelled continue",
                    ));
                }
                if self.loops.is_empty() {
                    return Err(CompileError::new(
                        self.source,
                        statement.span,
                        "continue outside loop",
                    ));
                }
                if let Some(target) = self
                    .loops
                    .last()
                    .expect("loop stack checked above")
                    .continue_target
                {
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
                self.source,
                statement.span,
                "debugger statement",
            )),
            Statement::WithStatement(statement) => {
                Err(unsupported(self.source, statement.span, "with statement"))
            }
            Statement::SwitchStatement(statement) => {
                Err(unsupported(self.source, statement.span, "switch statement"))
            }
            Statement::TryStatement(statement) => Err(unsupported(
                self.source,
                statement.span,
                "try/catch/finally",
            )),
            Statement::ForInStatement(statement) => {
                Err(unsupported(self.source, statement.span, "for-in"))
            }
            Statement::ForOfStatement(statement) => {
                Err(unsupported(self.source, statement.span, "for-of"))
            }
            Statement::LabeledStatement(statement) => Err(unsupported(
                self.source,
                statement.span,
                "labelled statement",
            )),
            Statement::ClassDeclaration(statement) => Err(unsupported(
                self.source,
                statement.span,
                "class declaration",
            )),
            other => Err(unsupported(
                self.source,
                other.span(),
                "top-level/module statement",
            )),
        }
    }

    fn patch_loop(&mut self, loop_context: LoopContext, end: usize) -> Result<(), CompileError> {
        for jump in loop_context.break_jumps {
            self.patch_jump(jump, end)?;
        }
        Ok(())
    }

    fn compile_variable_declaration(
        &mut self,
        declaration: &VariableDeclaration<'_>,
    ) -> Result<(), CompileError> {
        for declarator in &declaration.declarations {
            let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
                return Err(unsupported(
                    self.source,
                    declarator.span,
                    "destructuring declaration",
                ));
            };
            let name = identifier.name.as_str();
            let reg = self.declare(name, declaration.kind, declarator.span)?;
            let value = match &declarator.init {
                Some(expression) => self.compile_expression(expression)?,
                None => {
                    let reg = self.alloc_reg()?;
                    self.emit(Instr::Undefined {
                        dst: self.slot(reg),
                    });
                    reg
                }
            };
            self.emit(Instr::Move {
                dst: self.slot(reg),
                src: self.slot(value),
            });
        }
        Ok(())
    }

    fn compile_expression<'ast>(
        &mut self,
        expression: &'ast Expression<'ast>,
    ) -> Result<Reg, CompileError> {
        match expression {
            Expression::BooleanLiteral(literal) => {
                self.emit_constant(Constant::Bool(literal.value))
            }
            Expression::NullLiteral(_) => self.emit_constant(Constant::Null),
            Expression::NumericLiteral(literal) => {
                if !literal.value.is_finite() {
                    return Err(unsupported(
                        self.source,
                        literal.span,
                        "non-finite numeric literal",
                    ));
                }
                self.emit_constant(Constant::Number(literal.value))
            }
            Expression::StringLiteral(literal) => {
                let value = literal.value.as_str().ok_or_else(|| {
                    unsupported(self.source, literal.span, "string with a lone surrogate")
                })?;
                self.emit_constant(Constant::String(value.to_owned()))
            }
            Expression::Identifier(identifier) => {
                let name = identifier.name.as_str();
                if let Some(reg) = self.lookup(name) {
                    let dst = self.alloc_reg()?;
                    self.emit(Instr::Move {
                        dst: self.slot(dst),
                        src: self.slot(reg),
                    });
                    Ok(dst)
                } else if name == "undefined" {
                    let dst = self.alloc_reg()?;
                    self.emit(Instr::Undefined {
                        dst: self.slot(dst),
                    });
                    Ok(dst)
                } else if self.is_host_or_function(name) {
                    Err(CompileError::new(
                        self.source,
                        identifier.span,
                        format!("function/import value {name:?} is not first-class in this PoC"),
                    ))
                } else {
                    Err(CompileError::new(
                        self.source,
                        identifier.span,
                        format!("unknown binding {name:?}"),
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
                    unsupported(self.source, expression.span, "empty sequence expression")
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
                if !self.in_async {
                    return Err(unsupported(
                        self.source,
                        expression.span,
                        "await outside async function",
                    ));
                }
                let input = self.compile_expression(&expression.argument)?;
                let result = self.alloc_reg()?;
                let resume = self.awaits.len() as u32;
                let pc = self.emit(Instr::Await {
                    dst: self.slot(result),
                    input: self.slot(input),
                    resume,
                });
                self.awaits.push(AwaitInfo { pc, result });
                Ok(result)
            }
            Expression::CallExpression(expression) => self.compile_call(expression),
            Expression::AssignmentExpression(expression) => self.compile_assignment(expression),
            other => Err(unsupported(
                self.source,
                other.span(),
                "expression form outside the PoC subset",
            )),
        }
    }

    fn emit_constant(&mut self, constant: Constant) -> Result<Reg, CompileError> {
        let index = self.constants.len() as u32;
        self.constants.push(constant);
        let dst = self.alloc_reg()?;
        self.emit(Instr::Const {
            dst: self.slot(dst),
            constant: index,
        });
        Ok(dst)
    }

    fn compile_binary(&mut self, expression: &BinaryExpression<'_>) -> Result<Reg, CompileError> {
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
                    self.source,
                    expression.span,
                    if matches!(expression.operator, BinaryOperator::In) {
                        "in operator"
                    } else {
                        "instanceof operator"
                    },
                ));
            }
        };
        // Preserve left-to-right evaluation order.
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

    fn compile_logical(&mut self, expression: &LogicalExpression<'_>) -> Result<Reg, CompileError> {
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

    fn compile_unary(&mut self, expression: &UnaryExpression<'_>) -> Result<Reg, CompileError> {
        if matches!(expression.operator, UnaryOperator::Delete) {
            return Err(unsupported(self.source, expression.span, "delete"));
        }
        if matches!(expression.operator, UnaryOperator::Typeof)
            && let Expression::Identifier(identifier) = &expression.argument
        {
            let name = identifier.name.as_str();
            if !self.is_known_name(name) {
                return self.emit_constant(Constant::String("undefined".to_owned()));
            }
            if self.is_host_or_function(name) {
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

    fn compile_call(&mut self, expression: &CallExpression<'_>) -> Result<Reg, CompileError> {
        if expression.optional {
            return Err(unsupported(self.source, expression.span, "optional call"));
        }
        let Expression::Identifier(callee) = &expression.callee else {
            return Err(unsupported(
                self.source,
                expression.callee.span(),
                "non-identifier callee",
            ));
        };
        let name = callee.name.as_str();
        if self.lookup(name).is_some() {
            return Err(unsupported(
                self.source,
                expression.span,
                "calling local function values",
            ));
        }
        let mut args = Vec::with_capacity(expression.arguments.len());
        for argument in &expression.arguments {
            let Some(expression) = argument.as_expression() else {
                return Err(unsupported(self.source, argument.span(), "spread argument"));
            };
            let reg = self.compile_expression(expression)?;
            args.push(self.slot(reg));
        }
        let dst = self.alloc_reg()?;
        if let Some(import) = self.import_names.get(name).copied() {
            self.emit(Instr::CallImport {
                dst: self.slot(dst),
                import,
                args,
            });
        } else if let Some(function) = self.function_names.get(name).copied() {
            self.emit(Instr::CallFunction {
                dst: self.slot(dst),
                function,
                args,
            });
        } else {
            return Err(CompileError::new(
                self.source,
                callee.span,
                format!("unknown callee {name:?}"),
            ));
        }
        Ok(dst)
    }

    fn compile_assignment(
        &mut self,
        expression: &AssignmentExpression<'_>,
    ) -> Result<Reg, CompileError> {
        let AssignmentTarget::AssignmentTargetIdentifier(identifier) = &expression.left else {
            return Err(unsupported(
                self.source,
                expression.span,
                "assignment to non-identifier target",
            ));
        };
        let name = identifier.name.as_str();
        let target = self.lookup(name).ok_or_else(|| {
            CompileError::new(
                self.source,
                identifier.span,
                format!("assignment to unknown binding {name:?}"),
            )
        })?;
        let value = if matches!(expression.operator, AssignmentOperator::Assign) {
            self.compile_expression(&expression.right)?
        } else {
            let old = self.alloc_reg()?;
            self.emit(Instr::Move {
                dst: self.slot(old),
                src: self.slot(target),
            });
            let right = self.compile_expression(&expression.right)?;
            let op = match expression.operator {
                AssignmentOperator::Addition => BinaryOp::Add,
                AssignmentOperator::Subtraction => BinaryOp::Sub,
                AssignmentOperator::Multiplication => BinaryOp::Mul,
                AssignmentOperator::Division => BinaryOp::Div,
                AssignmentOperator::Remainder => BinaryOp::Rem,
                AssignmentOperator::Exponential => BinaryOp::Exp,
                AssignmentOperator::ShiftLeft => BinaryOp::Shl,
                AssignmentOperator::ShiftRight => BinaryOp::Shr,
                AssignmentOperator::ShiftRightZeroFill => BinaryOp::UShr,
                AssignmentOperator::BitwiseOR => BinaryOp::BitOr,
                AssignmentOperator::BitwiseXOR => BinaryOp::BitXor,
                AssignmentOperator::BitwiseAnd => BinaryOp::BitAnd,
                AssignmentOperator::Assign => unreachable!(),
                AssignmentOperator::LogicalOr
                | AssignmentOperator::LogicalAnd
                | AssignmentOperator::LogicalNullish => {
                    return Err(unsupported(
                        self.source,
                        expression.span,
                        "logical assignment",
                    ));
                }
            };
            let value = self.alloc_reg()?;
            self.emit(Instr::Binary {
                dst: self.slot(value),
                op,
                lhs: self.slot(old),
                rhs: self.slot(right),
            });
            value
        };
        self.emit(Instr::Move {
            dst: self.slot(target),
            src: self.slot(value),
        });
        let result = self.alloc_reg()?;
        self.emit(Instr::Move {
            dst: self.slot(result),
            src: self.slot(value),
        });
        Ok(result)
    }
}

fn compile_function(
    source: &str,
    function: &Function<'_>,
    func_id: u32,
    constants: &mut Vec<Constant>,
    import_names: &BTreeMap<String, u32>,
    function_names: &BTreeMap<String, u32>,
) -> Result<BytecodeFunction, CompileError> {
    let name = function
        .id
        .as_ref()
        .expect("top-level declarations always have names")
        .name
        .as_str()
        .to_owned();
    let mut builder = FunctionBuilder::new(
        source,
        import_names,
        function_names,
        constants,
        func_id,
        name,
        function.r#async,
    );
    builder.compile(function)
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
) -> Result<Vec<BTreeSet<Reg>>, CompileError> {
    let mut live_in = vec![BTreeSet::new(); code.len()];
    loop {
        let mut changed = false;
        for pc in (0..code.len()).rev() {
            let (uses, defs) = instruction_uses_defs(&code[pc]);
            let mut next = uses;
            for successor in successors(code, resumes, pc)? {
                next.extend(live_in[successor].iter().copied());
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
        Instr::Await { dst, input, .. } => {
            uses.insert(input.index);
            // The VM writes the result on resume. It is not an input to the
            // suspension itself.
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

fn successors(
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
