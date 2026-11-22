//! AST lowering. The only pass that walks function bodies.
//!
//! A [`Lowerer`] composes [`Env`], [`Emitter`], and [`Control`] and produces a
//! [`LoweredFunction`]. Nested functions are a worklist of Oxc nodes plus the
//! scope chain captured at the definition, not a recursive borrow of the lowerer.

mod class;
mod control;
mod expr;
mod pattern;
mod stmt;

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use oxc::ast::ast::*;
use oxc::ast_visit::{Visit, walk};
use oxc::span::{GetSpan, Span};
use oxc::syntax::operator::UnaryOperator;
use oxc::syntax::scope::ScopeFlags;
use yield_vm::BindingId;
use yield_vm::bytecode::{
    BinaryOp, Build, Constant, Instr, ModuleExport, ModuleTarget, SourceModule,
};

use crate::allocate;
use crate::bindings;
use crate::cfg::FunctionCfg;
use crate::emit::{Emitter, Reg};
use crate::env::{BindingKind, CapturedEnv, Env};
use crate::error::{CompileError, unsupported};
use crate::frontend::link::{DEFAULT_SLOT, Graph, Target};
use crate::ir::{GeneratorStartSite, IterationEnvGroup, LoweredFunction};
use crate::names::Tables;
use crate::promote;
use crate::session::Session;
use control::Control;

pub(crate) fn lower_graph<'ast>(
    graph: &'ast Graph<'ast>,
    options: &crate::CompileOptions,
) -> Result<Build, CompileError> {
    let mut session = Session::new();
    // Reserve graph-wide binding IDs before lowering the first declaration.
    let mut reservations = Vec::with_capacity(graph.modules.len());
    for module in &graph.modules {
        let mut ids = BTreeMap::new();
        for name in &module.locals {
            let next = session.binding_ids();
            let id = *next;
            *next = next
                .checked_add(1)
                .ok_or_else(|| CompileError::internal("too many bindings").in_module(&module.id))?;
            ids.insert(name.as_str(), id);
        }
        reservations.push(ids);
    }
    let target = |target: &Target| match target {
        Target::Binding { module, name } => ModuleTarget::Binding {
            module: *module,
            binding: reservations[*module as usize][name.as_str()],
        },
        Target::Namespace { module } => ModuleTarget::Namespace { module: *module },
        Target::Host(import) => ModuleTarget::Host(*import),
    };
    let mut keep = vec![BTreeSet::new(); graph.modules.len()];
    for module in &graph.modules {
        for export in module.exports.values() {
            if let ModuleTarget::Binding { module, binding } = target(export) {
                keep[module as usize].insert(binding);
            }
        }
    }
    let preload_names = graph
        .preload
        .map(|index| &graph.modules[index as usize].exports);
    let mut getters = BTreeMap::new();
    let mut modules = Vec::with_capacity(graph.modules.len());
    for (index, module) in graph.modules.iter().enumerate() {
        let index = index as u32;
        let source = module.program.source_text;
        let result = (|| {
            let scope = ModuleScope {
                index,
                source,
                entry: index == graph.entry,
                slots: &reservations[index as usize],
                module_bindings: reservations[index as usize].values().copied().collect(),
                imports: module
                    .imports
                    .iter()
                    .map(|(name, link)| (name.clone(), target(link)))
                    .collect(),
                preload: if module.in_preload {
                    None
                } else {
                    preload_names
                },
            };
            let declaration = session.alloc_func_id(source, Span::default())?;
            let (evaluation, name) = if index == graph.entry {
                (0, "\0script".to_owned())
            } else {
                let id = session.alloc_func_id(source, Span::default())?;
                (
                    id,
                    if Some(index) == graph.preload {
                        "\0preload".to_owned()
                    } else {
                        format!("\0evaluate@{index}")
                    },
                )
            };
            lower_module(
                &mut session,
                &scope,
                &module.program.body,
                declaration,
                evaluation,
                name,
                &keep[index as usize],
            )?;
            let mut exports = BTreeMap::new();
            for (name, link) in &module.exports {
                let target = target(link);
                let getter = match getters.entry(target) {
                    std::collections::btree_map::Entry::Occupied(entry) => *entry.get(),
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        *entry.insert(session.getter(target)?)
                    }
                };
                exports.insert(name.to_owned(), ModuleExport { target, getter });
            }
            Ok(SourceModule {
                id: module.id.clone(),
                declaration,
                evaluation,
                dependencies: module.deps.clone(),
                exports,
            })
        })();
        modules.push(result.map_err(|err: CompileError| err.in_module(&module.id))?);
    }
    session.finish(modules, graph.entry, graph.preload, options)
}

struct ModuleScope<'src> {
    index: u32,
    source: &'src str,
    entry: bool,
    slots: &'src BTreeMap<&'src str, BindingId>,
    module_bindings: BTreeSet<BindingId>,
    imports: BTreeMap<String, ModuleTarget>,
    preload: Option<&'src BTreeMap<String, Target>>,
}

#[derive(Default)]
struct TypeofCandidateScan {
    found: bool,
}

impl<'ast> Visit<'ast> for TypeofCandidateScan {
    fn visit_unary_expression(&mut self, expression: &UnaryExpression<'ast>) {
        if expression.operator == UnaryOperator::Typeof {
            let mut operand = &expression.argument;
            while let Expression::ParenthesizedExpression(paren) = operand {
                operand = &paren.expression;
            }
            if matches!(operand, Expression::Identifier(_)) {
                self.found = true;
            }
        }
        if !self.found {
            walk::walk_unary_expression(self, expression);
        }
    }
}

enum LoweringMode<'a> {
    Probe(&'a mut BTreeSet<String>),
    Emit(&'a BTreeSet<String>),
}

impl LoweringMode<'_> {
    fn reborrow(&mut self) -> LoweringMode<'_> {
        match self {
            Self::Probe(names) => LoweringMode::Probe(names),
            Self::Emit(names) => LoweringMode::Emit(names),
        }
    }
}

fn lower_module<'ast>(
    session: &mut Session,
    scope: &ModuleScope<'_>,
    statements: &'ast [Statement<'ast>],
    declaration: u32,
    evaluation: u32,
    name: String,
    public: &BTreeSet<BindingId>,
) -> Result<(), CompileError> {
    let mut scan = TypeofCandidateScan::default();
    for statement in statements {
        scan.visit_statement(statement);
        if scan.found {
            break;
        }
    }
    let mut missing = BTreeSet::new();
    if scan.found {
        // Reuse the declaration/evaluation roots and immutable module links,
        // but never commit probe constants, nested IDs, or lowered functions.
        lower_module_functions(
            &mut session.fork_for_probe(),
            scope,
            statements,
            declaration,
            evaluation,
            name.clone(),
            LoweringMode::Probe(&mut missing),
        )?;
    }
    let lowered = lower_module_functions(
        session,
        scope,
        statements,
        declaration,
        evaluation,
        name,
        LoweringMode::Emit(&missing),
    )?;
    let mut facts = bindings::classify(&lowered)?;
    facts.shared.extend(public);
    let mut lowered = lowered
        .into_iter()
        .map(FunctionCfg::from_lowered)
        .collect::<Result<Vec<_>, _>>()?;
    for function in &mut lowered {
        promote::promote(scope.source, function, &facts)?;
    }
    let depths = function_depths(&lowered, &[declaration, evaluation]);
    let loop_free: BTreeSet<u32> = lowered
        .iter()
        .filter(|function| !has_iteration_environment(function))
        .map(|function| function.attrs.func_id)
        .collect();
    for mut function in lowered {
        direct_bindings(
            &mut function,
            scope.index,
            &scope.module_bindings,
            &facts,
            &depths,
            &loop_free,
        );
        crate::propagate::optimize(&mut function, session);
        function.simplify_jumps();
        function.validate()?;
        let allocated = allocate::allocate(scope.source, function)?;
        session.push_function(allocated);
    }
    Ok(())
}

// `iteration_envs` is consumed by promotion's elision pass, so inspect the
// surviving block instructions instead of relying on the group metadata.
fn has_iteration_environment(function: &FunctionCfg) -> bool {
    function.instructions().any(|instruction| {
        matches!(
            instruction,
            Instr::SaveEnv { .. } | Instr::NewIterationEnv { .. } | Instr::RestoreEnv { .. }
        )
    })
}

/// Compiler-known environment depth for a function's own invocation
/// environment, relative to its module environment. Only functions with no
/// per-iteration environments and a consistent closure-creation depth qualify.
fn function_depths(functions: &[FunctionCfg], initializers: &[u32]) -> BTreeMap<u32, u16> {
    let initializers: BTreeSet<u32> = initializers.iter().copied().collect();
    let by_id: BTreeMap<u32, &FunctionCfg> = functions
        .iter()
        .map(|function| (function.attrs.func_id, function))
        .collect();
    let mut sites: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    let mut scoped = BTreeSet::new();
    for function in functions {
        let creator = function.attrs.func_id;
        for instruction in function.instructions() {
            match instruction {
                Instr::Closure { function, .. } => {
                    sites.entry(*function).or_default().push(creator);
                }
                Instr::ScopedClosure { function, .. } => {
                    scoped.insert(*function);
                }
                _ => {}
            }
        }
    }

    fn depth(
        id: u32,
        initializers: &BTreeSet<u32>,
        by_id: &BTreeMap<u32, &FunctionCfg>,
        sites: &BTreeMap<u32, Vec<u32>>,
        scoped: &BTreeSet<u32>,
        cache: &mut BTreeMap<u32, Option<u16>>,
        visiting: &mut BTreeSet<u32>,
    ) -> Option<u16> {
        if initializers.contains(&id) {
            return Some(0);
        }
        if let Some(cached) = cache.get(&id) {
            return *cached;
        }
        if scoped.contains(&id) || !visiting.insert(id) {
            cache.insert(id, None);
            return None;
        }
        let mut result = None;
        if let Some(creators) = sites.get(&id) {
            for creator in creators {
                let Some(function) = by_id.get(creator) else {
                    result = None;
                    break;
                };
                if has_iteration_environment(function) {
                    result = None;
                    break;
                }
                let Some(base) = depth(
                    function.attrs.func_id,
                    initializers,
                    by_id,
                    sites,
                    scoped,
                    cache,
                    visiting,
                ) else {
                    result = None;
                    break;
                };
                let Some(next) = base.checked_add(1) else {
                    result = None;
                    break;
                };
                match result {
                    None => result = Some(next),
                    Some(existing) if existing == next => {}
                    Some(_) => {
                        result = None;
                        break;
                    }
                }
            }
        }
        visiting.remove(&id);
        cache.insert(id, result);
        result
    }

    let mut cache = BTreeMap::new();
    let mut visiting = BTreeSet::new();
    for &id in by_id.keys() {
        depth(
            id,
            &initializers,
            &by_id,
            &sites,
            &scoped,
            &mut cache,
            &mut visiting,
        );
    }
    cache
        .into_iter()
        .filter_map(|(function, depth)| depth.map(|depth| (function, depth)))
        .collect()
}

/// Replace environment-chain lookups with direct accesses. Module bindings
/// address the module environment; other bindings use a compile-time depth
/// when the function and owner are loop-free and the closure-creation chain is
/// fixed. The direct forms deliberately skip the linked-read TDZ check so a
/// nested closure still observes `undefined` for an uninitialized module slot.
fn direct_bindings(
    function: &mut FunctionCfg,
    module: u32,
    module_bindings: &BTreeSet<BindingId>,
    facts: &bindings::BindingFacts,
    depths: &BTreeMap<u32, u16>,
    loop_free: &BTreeSet<u32>,
) {
    let current = function.attrs.func_id;
    let loop_free_here = loop_free.contains(&current);
    let base = depths.get(&current).copied();
    for instruction in function.instructions_mut() {
        *instruction = match instruction {
            Instr::GetBinding { dst, binding } => {
                if module_bindings.contains(binding) {
                    Instr::GetModuleBindingDirect {
                        dst: *dst,
                        module,
                        binding: *binding,
                    }
                } else if loop_free_here
                    && let Some(depth) =
                        binding_depth(*binding, current, base, facts, depths, loop_free)
                {
                    Instr::GetBindingDirect {
                        dst: *dst,
                        depth,
                        binding: *binding,
                    }
                } else {
                    continue;
                }
            }
            Instr::SetBinding { binding, src } => {
                if module_bindings.contains(binding) {
                    Instr::SetModuleBindingDirect {
                        module,
                        binding: *binding,
                        src: *src,
                    }
                } else if loop_free_here
                    && let Some(depth) =
                        binding_depth(*binding, current, base, facts, depths, loop_free)
                {
                    Instr::SetBindingDirect {
                        depth,
                        binding: *binding,
                        src: *src,
                    }
                } else {
                    continue;
                }
            }
            _ => continue,
        };
    }
}

fn binding_depth(
    binding: BindingId,
    current: u32,
    base: Option<u16>,
    facts: &bindings::BindingFacts,
    depths: &BTreeMap<u32, u16>,
    loop_free: &BTreeSet<u32>,
) -> Option<u16> {
    let owner = *facts.owners.get(&binding)?;
    if !loop_free.contains(&owner) {
        return None;
    }
    if owner == current {
        return Some(0);
    }
    let depth = base?.checked_sub(*depths.get(&owner)?)?;
    (depth != 0).then_some(depth)
}

fn lower_module_functions<'ast>(
    session: &mut Session,
    scope: &ModuleScope<'_>,
    statements: &'ast [Statement<'ast>],
    declaration: u32,
    evaluation: u32,
    name: String,
    mut mode: LoweringMode<'_>,
) -> Result<Vec<LoweredFunction>, CompileError> {
    let probe = matches!(&mode, LoweringMode::Probe(_));
    let mut tasks = VecDeque::new();
    let script = |func_id, name| Task {
        source: FunctionSource::Script(statements),
        func_id,
        name,
        initial_name: String::new(),
        outer: CapturedEnv::default(),
        class_bindings: BTreeSet::new(),
        private_scopes: Vec::new(),
        derived_this: false,
    };
    let mut declare = Lowerer::new(
        session,
        &mut tasks,
        scope,
        script(declaration, format!("\0declare@{declaration}")),
        mode.reborrow(),
    );
    declare.module_declaration = true;
    let declared = declare.compile()?;
    let (env, hoisted, default_binding) = (declare.env, declare.hoisted, declare.default_binding);
    let mut evaluate = Lowerer::new(
        session,
        &mut tasks,
        scope,
        script(evaluation, name),
        mode.reborrow(),
    );
    evaluate.env = env;
    evaluate.hoisted = hoisted;
    evaluate.default_binding = default_binding;
    let evaluated = evaluate.compile()?;
    let mut lowered = if probe {
        Vec::new()
    } else {
        vec![declared, evaluated]
    };
    while let Some(task) = tasks.pop_front() {
        let function = Lowerer::new(session, &mut tasks, scope, task, mode.reborrow()).compile()?;
        if !probe {
            lowered.push(function);
        }
    }
    Ok(lowered)
}

#[derive(Clone, Copy)]
enum FunctionSource<'ast> {
    Script(&'ast [Statement<'ast>]),
    Function(&'ast Function<'ast>),
    Method(&'ast Function<'ast>),
    Constructor(&'ast Function<'ast>),
    DerivedConstructor(&'ast Function<'ast>),
    Arrow(&'ast ArrowFunctionExpression<'ast>),
    DefaultConstructor(Span),
    ClassField(&'ast Expression<'ast>, &'ast PropertyKey<'ast>, bool),
    ClassBlock(&'ast StaticBlock<'ast>),
}

impl FunctionSource<'_> {
    fn span(self) -> Span {
        match self {
            Self::Function(function)
            | Self::Method(function)
            | Self::Constructor(function)
            | Self::DerivedConstructor(function) => function.span,
            Self::Arrow(arrow) => arrow.span,
            Self::DefaultConstructor(span) => span,
            Self::ClassField(expression, ..) => expression.span(),
            Self::ClassBlock(block) => block.span,
            Self::Script(statements) => statements
                .first()
                .map(|statement| statement.span())
                .unwrap_or_default(),
        }
    }

    fn is_async(self) -> bool {
        match self {
            Self::Function(function)
            | Self::Method(function)
            | Self::Constructor(function)
            | Self::DerivedConstructor(function) => function.r#async,
            Self::Arrow(arrow) => arrow.r#async,
            Self::Script(_)
            | Self::DefaultConstructor(_)
            | Self::ClassField(..)
            | Self::ClassBlock(_) => false,
        }
    }

    fn is_arrow(self) -> bool {
        matches!(self, Self::Arrow(_))
    }

    fn is_generator(self) -> bool {
        match self {
            Self::Function(function)
            | Self::Method(function)
            | Self::Constructor(function)
            | Self::DerivedConstructor(function) => function.generator,
            Self::Arrow(_)
            | Self::Script(_)
            | Self::DefaultConstructor(_)
            | Self::ClassField(..)
            | Self::ClassBlock(_) => false,
        }
    }
}

struct Task<'ast> {
    source: FunctionSource<'ast>,
    func_id: u32,
    name: String,
    initial_name: String,
    outer: CapturedEnv,
    class_bindings: BTreeSet<BindingId>,
    private_scopes: Vec<std::rc::Rc<class::PrivateScope<'ast>>>,
    derived_this: bool,
}

struct Lowerer<'ast, 'borrow, 'src> {
    session: &'borrow mut Session,
    tasks: &'borrow mut VecDeque<Task<'ast>>,
    module: &'borrow ModuleScope<'src>,
    mode: LoweringMode<'borrow>,
    func_id: u32,
    name: String,
    initial_name: String,
    source: FunctionSource<'ast>,
    span: Span,
    is_async: bool,
    is_arrow: bool,
    is_method: bool,
    is_generator: bool,
    is_script: bool,
    module_declaration: bool,
    // In an explicit derived constructor and its arrows, the this binding
    // holds the opaque construction context rather than the receiver.
    derived_this: bool,
    env: Env,
    // Inner names live in the class-scope chain, never in ordinary binding cells.
    class_bindings: BTreeSet<BindingId>,
    private_scopes: Vec<std::rc::Rc<class::PrivateScope<'ast>>>,
    // None uses the ambient scope inherited by the current closure.
    class_scope: Option<Reg>,
    emit: Emitter<'src>,
    control: Control<'ast>,
    dispose_scope: Option<Reg>,
    param_bindings: Vec<BindingId>,
    rest_binding: Option<BindingId>,
    this_binding: Option<BindingId>,
    home_binding: Option<BindingId>,
    new_target_binding: Option<BindingId>,
    arguments_binding: Option<BindingId>,
    hoisted: BTreeSet<u32>,
    // The synthetic slot for a default export without a local name.
    default_binding: Option<BindingId>,
    iteration_envs: Vec<IterationEnvGroup>,
}

impl<'ast, 'borrow, 'src> Lowerer<'ast, 'borrow, 'src> {
    fn new(
        session: &'borrow mut Session,
        tasks: &'borrow mut VecDeque<Task<'ast>>,
        module: &'borrow ModuleScope<'src>,
        task: Task<'ast>,
        mode: LoweringMode<'borrow>,
    ) -> Self {
        let span = task.source.span();
        let is_async = task.source.is_async();
        Self {
            session,
            tasks,
            module,
            mode,
            func_id: task.func_id,
            name: task.name,
            initial_name: task.initial_name,
            source: task.source,
            span,
            is_async,
            is_arrow: task.source.is_arrow(),
            is_method: matches!(
                task.source,
                FunctionSource::Method(_)
                    | FunctionSource::ClassField(..)
                    | FunctionSource::ClassBlock(_)
            ),
            is_generator: task.source.is_generator(),
            is_script: matches!(task.source, FunctionSource::Script(_)),
            module_declaration: false,
            derived_this: task.derived_this,
            env: Env::new(task.outer),
            class_bindings: task.class_bindings,
            private_scopes: task.private_scopes,
            class_scope: None,
            emit: Emitter::new(module.source, span),
            control: Control::new(),
            dispose_scope: None,
            param_bindings: Vec::new(),
            rest_binding: None,
            this_binding: None,
            home_binding: None,
            new_target_binding: None,
            arguments_binding: None,
            hoisted: BTreeSet::new(),
            default_binding: None,
            iteration_envs: Vec::new(),
        }
    }

    fn is_probe(&self) -> bool {
        matches!(&self.mode, LoweringMode::Probe(_))
    }

    fn record_typeof_fold(&mut self, name: &str) {
        if let LoweringMode::Probe(names) = &mut self.mode {
            names.insert(name.to_owned());
        }
    }

    fn probed_missing(&self, name: &str) -> bool {
        match &self.mode {
            LoweringMode::Probe(_) => false,
            LoweringMode::Emit(names) => names.contains(name),
        }
    }

    fn compile(&mut self) -> Result<LoweredFunction, CompileError> {
        let source = self.source;
        self.install_home_binding(source);
        self.install_this_binding(source);
        self.install_new_target_binding(source);
        let length = self.compile_parameters(source)?;
        // Parameter initialization runs in the call; the body runs per resume.
        let generator_start = if self.is_generator {
            self.emit(Instr::GeneratorStart);
            Some(GeneratorStartSite { body_pc: self.pc() })
        } else {
            None
        };
        match source {
            FunctionSource::DefaultConstructor(_) => self.emit_implicit_return()?,
            FunctionSource::ClassField(..) | FunctionSource::ClassBlock(_)
                if function_uses_arguments(source) =>
            {
                return Err(unsupported(
                    self.source_text(),
                    self.span,
                    "arguments in a class initializer",
                ));
            }
            FunctionSource::ClassField(expression, key, name_from_key) => {
                let (name, key) = if name_from_key {
                    // The initializer receives the already-converted field key.
                    let binding = self.alloc_binding();
                    self.param_bindings.push(binding);
                    let key = self.alloc_reg()?;
                    self.emit_binding_read(key, binding);
                    (String::new(), Some(key))
                } else {
                    (self.static_key_name(key)?.expect("static field name"), None)
                };
                let value = self.compile_named_expression_with_key(expression, &name, key)?;
                self.emit(Instr::Return {
                    src: self.slot(value),
                });
            }
            FunctionSource::ClassBlock(block) => {
                // A separate function environment keeps even deeply nested var
                // declarations local to this static block.
                self.predeclare_function_body(&block.body)?;
                self.compile_disposable_body(&block.body)?;
                self.emit_implicit_return()?;
            }
            FunctionSource::Script(statements) => {
                if self.module_declaration {
                    self.hoist_script(statements)?;
                    self.hoist_anonymous_default(statements)?;
                } else {
                    self.compile_disposable_body(statements)?;
                }
                self.emit_implicit_return()?;
            }
            FunctionSource::Function(function)
            | FunctionSource::Method(function)
            | FunctionSource::Constructor(function)
            | FunctionSource::DerivedConstructor(function) => {
                let body = function.body.as_ref().ok_or_else(|| {
                    unsupported(
                        self.source_text(),
                        function.span,
                        "declaration without body",
                    )
                })?;
                self.predeclare_function_body(&body.statements)?;
                self.compile_disposable_body(&body.statements)?;
                self.emit_implicit_return()?;
            }
            FunctionSource::Arrow(arrow) => match &arrow.body {
                ArrowFunctionBody::FunctionBody(body) => {
                    self.predeclare_function_body(&body.statements)?;
                    self.compile_disposable_body(&body.statements)?;
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
            attrs: crate::ir::FunctionAttrs {
                func_id: self.func_id,
                name: self.name.clone(),
                initial_name: std::mem::take(&mut self.initial_name),
                is_async: self.is_async,
                is_arrow: self.is_arrow,
                is_method: self.is_method,
                is_generator: self.is_generator,
                length,
                param_bindings: self.param_bindings.clone(),
                rest_binding: self.rest_binding,
                this_binding: self.this_binding,
                home_binding: self.home_binding,
                new_target_binding: self.new_target_binding,
                arguments_binding: self.arguments_binding,
                promoted: BTreeMap::new(),
                reg_count: self.emit.reg_count(),
                span: self.span,
            },
            code: self.emit.take_code(),
            awaits: self.control.take_awaits(),
            generator_start,
            yields: self.control.take_yields(),
            handlers: self.control.take_handlers(),
            iteration_envs: std::mem::take(&mut self.iteration_envs),
        })
    }
    fn install_this_binding(&mut self, source: FunctionSource<'ast>) {
        if self.is_arrow
            || !(self.derived_this || self.home_binding.is_some() || function_uses_this(source))
        {
            return;
        }
        let binding = self.alloc_binding();
        self.env.insert_function(THIS_BINDING.to_owned(), binding);
        self.this_binding = Some(binding);
    }

    fn install_home_binding(&mut self, source: FunctionSource<'ast>) {
        let mut scan = SuperScan { found: false };
        match source {
            FunctionSource::Method(function)
            | FunctionSource::Constructor(function)
            | FunctionSource::DerivedConstructor(function) => {
                scan.visit_formal_parameters(&function.params);
                if let Some(body) = &function.body {
                    scan.visit_function_body(body);
                }
            }
            FunctionSource::ClassField(expression, ..) => scan.visit_expression(expression),
            FunctionSource::ClassBlock(block) => scan.visit_statements(&block.body),
            _ => return,
        }
        if scan.found {
            let binding = self.alloc_binding();
            self.env.insert_function(HOME_BINDING.to_owned(), binding);
            self.home_binding = Some(binding);
        }
    }

    fn install_new_target_binding(&mut self, source: FunctionSource<'ast>) {
        let mut scan = NewTargetScan { found: false };
        match source {
            FunctionSource::Function(function)
            | FunctionSource::Method(function)
            | FunctionSource::Constructor(function)
            | FunctionSource::DerivedConstructor(function) => {
                scan.visit_formal_parameters(&function.params);
                if let Some(body) = &function.body {
                    scan.visit_function_body(body);
                }
            }
            FunctionSource::ClassField(expression, ..) => scan.visit_expression(expression),
            FunctionSource::ClassBlock(block) => scan.visit_statements(&block.body),
            _ => return,
        }
        if scan.found {
            let binding = self.alloc_binding();
            self.env
                .insert_function(NEW_TARGET_BINDING.to_owned(), binding);
            self.new_target_binding = Some(binding);
        }
    }

    fn compile_parameters(&mut self, source: FunctionSource<'ast>) -> Result<u16, CompileError> {
        match source {
            FunctionSource::Function(function)
            | FunctionSource::Method(function)
            | FunctionSource::Constructor(function)
            | FunctionSource::DerivedConstructor(function) => {
                if function.generator
                    && matches!(
                        source,
                        FunctionSource::Constructor(_) | FunctionSource::DerivedConstructor(_)
                    )
                {
                    return Err(unsupported(
                        self.source_text(),
                        function.span,
                        "generator constructor",
                    ));
                }
                self.compile_parameter_list(&function.params)
            }
            FunctionSource::Arrow(arrow) => self.compile_parameter_list(&arrow.params),
            FunctionSource::Script(_)
            | FunctionSource::DefaultConstructor(_)
            | FunctionSource::ClassField(..)
            | FunctionSource::ClassBlock(_) => Ok(0),
        }
    }

    fn compile_parameter_list(
        &mut self,
        params: &'ast FormalParameters<'ast>,
    ) -> Result<u16, CompileError> {
        let length = params
            .items
            .iter()
            .take_while(|parameter| parameter.initializer.is_none())
            .count();
        let length = u16::try_from(length).map_err(|_| {
            CompileError::new(self.source_text(), params.span, "too many parameters")
        })?;
        let needs_prologue = params.rest.is_some()
            || params.items.iter().any(|parameter| {
                parameter.initializer.is_some()
                    || !matches!(&parameter.pattern, BindingPattern::BindingIdentifier(_))
            });
        let mut pending = Vec::new();
        let patterns = params
            .items
            .iter()
            .map(|parameter| &parameter.pattern)
            .chain(params.rest.iter().map(|rest| &rest.rest.argument));
        for pattern in patterns {
            for_each_binding_name(pattern, &mut |identifier| {
                let name = identifier.name.as_str();
                if self.env.has_function_binding(name) {
                    return Err(CompileError::new(
                        self.source_text(),
                        identifier.span,
                        format!("duplicate parameter {name:?}"),
                    ));
                }
                let binding = self.alloc_binding();
                self.env.insert_function(name.to_owned(), binding);
                if needs_prologue {
                    pending.push(binding);
                } else {
                    self.param_bindings.push(binding);
                }
                Ok::<(), CompileError>(())
            })?;
        }
        if !self.env.has_function_binding("arguments") && function_uses_arguments(self.source) {
            let binding = self.alloc_binding();
            self.env.insert_function("arguments".to_owned(), binding);
            self.arguments_binding = Some(binding);
        }
        if needs_prologue {
            // Keep incoming arguments separate from the names initialized by
            // patterns, so a default can see earlier but not later bindings.
            self.open_pending(pending)?;
            for parameter in &params.items {
                let binding = self.alloc_binding();
                self.param_bindings.push(binding);
                let value = self.alloc_reg()?;
                self.emit(Instr::GetBinding {
                    dst: self.slot(value),
                    binding,
                });
                let value = if let Some(initializer) = &parameter.initializer {
                    let name = match &parameter.pattern {
                        BindingPattern::BindingIdentifier(id) => id.name.as_str(),
                        _ => "",
                    };
                    self.compile_pattern_default(value, initializer, name)?
                } else {
                    value
                };
                self.compile_binding_pattern(&parameter.pattern, value)?;
            }
            if let Some(rest) = &params.rest {
                let binding = self.alloc_binding();
                self.rest_binding = Some(binding);
                let value = self.alloc_reg()?;
                self.emit(Instr::GetBinding {
                    dst: self.slot(value),
                    binding,
                });
                self.compile_binding_pattern(&rest.rest.argument, value)?;
            }
        }
        Ok(length)
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
        self.module.source
    }

    fn names(&self) -> Tables<'_> {
        Tables {
            imports: &self.module.imports,
            preload: self.module.preload,
        }
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

    pub(super) fn branch_true(&mut self, cond: Reg) -> usize {
        self.emit.branch_true(cond)
    }

    pub(super) fn branch_compare_false(&mut self, lhs: Reg, op: BinaryOp, rhs: Reg) -> usize {
        self.emit.branch_compare_false(lhs, op, rhs)
    }

    pub(super) fn branch_compare_const_false(
        &mut self,
        lhs: Reg,
        op: BinaryOp,
        constant: u32,
    ) -> usize {
        self.emit.branch_compare_const_false(lhs, op, constant)
    }

    pub(super) fn branch_compare_true(&mut self, lhs: Reg, op: BinaryOp, rhs: Reg) -> usize {
        self.emit.branch_compare_true(lhs, op, rhs)
    }

    pub(super) fn branch_compare_const_true(
        &mut self,
        lhs: Reg,
        op: BinaryOp,
        constant: u32,
    ) -> usize {
        self.emit.branch_compare_const_true(lhs, op, constant)
    }

    pub(super) fn patch_jump(&mut self, index: usize, target: usize) -> Result<(), CompileError> {
        self.emit.patch(index, target)
    }

    fn patch_condition(&mut self, branches: Vec<usize>, target: usize) -> Result<(), CompileError> {
        for branch in branches {
            self.patch_jump(branch, target)?;
        }
        Ok(())
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
        let binding_kind = as_binding_kind(kind);
        let source = self.module.source;
        self.env
            .declare(self.session.binding_ids(), source, name, binding_kind, span)
    }

    pub(super) fn lookup(&self, name: &str) -> Option<BindingId> {
        self.env.lookup(name)
    }

    fn emit_binding_read(&mut self, dst: Reg, binding: BindingId) {
        let dst = self.slot(dst);
        self.emit(if self.class_bindings.contains(&binding) {
            Instr::LoadClassBinding {
                dst,
                scope: self.class_scope.map(|scope| self.slot(scope)),
                binding,
            }
        } else {
            Instr::GetBinding { dst, binding }
        });
    }

    /// Emit the PutValue step after evaluating an assignment's inputs.
    pub(super) fn put_binding(&mut self, binding: BindingId, value: Reg) -> usize {
        if self.env.is_const(binding) {
            self.emit(Instr::ThrowAssignmentToConst)
        } else {
            self.emit(Instr::SetBinding {
                binding,
                src: self.slot(value),
            })
        }
    }

    pub(super) fn check_tdz(
        &self,
        binding: BindingId,
        name: &str,
        span: Span,
    ) -> Result<(), CompileError> {
        self.env.check_tdz(binding, name, self.module.source, span)
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
        hint: impl std::fmt::Display,
        span: Span,
    ) -> Result<u32, CompileError> {
        let initial_name = match source {
            FunctionSource::Function(function) => function
                .id
                .as_ref()
                .map_or_else(String::new, |id| id.name.as_str().to_owned()),
            _ => String::new(),
        };
        self.enqueue_named_function(source, hint, initial_name, span)
    }

    pub(super) fn enqueue_named_function(
        &mut self,
        source: FunctionSource<'ast>,
        hint: impl std::fmt::Display,
        initial_name: String,
        span: Span,
    ) -> Result<u32, CompileError> {
        let func_id = self.session.alloc_func_id(self.module.source, span)?;
        self.tasks.push_back(Task {
            source,
            func_id,
            name: format!("{hint}@{func_id}"),
            initial_name,
            outer: self.capture_for(source),
            class_bindings: self.class_bindings.clone(),
            private_scopes: self.private_scopes.clone(),
            derived_this: matches!(source, FunctionSource::DerivedConstructor(_))
                || (source.is_arrow() && self.derived_this),
        });
        Ok(func_id)
    }

    fn capture_for(&self, source: FunctionSource<'ast>) -> CapturedEnv {
        let mut outer = self.env.capture();
        if !source.is_arrow() {
            outer.remove(HOME_BINDING);
        }
        outer
    }

    pub(super) fn emit_function_value(&mut self, func_id: u32) -> Result<Reg, CompileError> {
        let dst = self.alloc_reg()?;
        let dst_slot = self.slot(dst);
        self.emit(if let Some(scope) = self.class_scope {
            Instr::ScopedClosure {
                dst: dst_slot,
                scope: self.slot(scope),
                function: func_id,
            }
        } else {
            Instr::Closure {
                dst: dst_slot,
                function: func_id,
            }
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
    fn enqueue_direct(
        &mut self,
        function: &'ast Function<'ast>,
        name: &str,
        span: Span,
    ) -> Result<u32, CompileError> {
        let func_id = self.session.alloc_func_id(self.module.source, span)?;
        self.tasks.push_back(Task {
            source: FunctionSource::Function(function),
            func_id,
            name: if self.module.entry {
                name.to_owned()
            } else {
                format!("{name}@{func_id}")
            },
            initial_name: name.to_owned(),
            outer: self.capture_for(FunctionSource::Function(function)),
            class_bindings: self.class_bindings.clone(),
            private_scopes: self.private_scopes.clone(),
            derived_this: false,
        });
        Ok(func_id)
    }

    /// Script-level `var` and direct function declarations are created before
    /// the body, including `var` nested in blocks, loops, and `try`. Nested
    /// functions are not scanned. A direct function shares its binding with
    /// `var` of the same name: the hoisted closure wins over an uninitialized
    /// `var`, and an initializer later updates the binding.
    fn hoist_script(&mut self, statements: &'ast [Statement<'ast>]) -> Result<(), CompileError> {
        let scan = script_scan(self.source_text(), statements)?;
        for (_, function) in &scan.functions {
            self.hoisted.insert(function.span.start);
        }
        for name in scan.vars.keys() {
            self.predeclare_name(name, VariableDeclarationKind::Var);
        }
        self.predeclare_scope(statements)?;
        let function_names = scan
            .functions
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<BTreeSet<_>>();
        let mut plain = Vec::new();
        for name in scan.vars.keys() {
            if function_names.contains(name) {
                continue;
            }
            let Some(binding) = self.lookup(name) else {
                return Err(CompileError::internal(format!(
                    "internal compiler error: script var {name:?} was not declared"
                )));
            };
            plain.push(binding);
        }
        if !plain.is_empty() {
            let undefined = self.emit_undefined()?;
            let src = self.slot(undefined);
            for binding in plain {
                self.emit(Instr::DeclareBinding { binding });
                self.emit(Instr::SetBinding { binding, src });
            }
        }
        for (name, function) in scan.functions {
            let Some(binding) = self.lookup(&name) else {
                return Err(CompileError::internal(format!(
                    "internal compiler error: script function {name:?} was not declared"
                )));
            };
            let func_id = self.enqueue_direct(function, &name, function.span)?;
            self.init_function_binding(binding, func_id)?;
        }
        Ok(())
    }

    fn hoist_anonymous_default(
        &mut self,
        statements: &'ast [Statement<'ast>],
    ) -> Result<(), CompileError> {
        let Some(&binding) = self.module.slots.get(DEFAULT_SLOT) else {
            return Ok(());
        };
        self.default_binding = Some(binding);
        for stmt in statements {
            if let Statement::ExportDefaultDeclaration(export) = stmt {
                if let ExportDefaultDeclarationKind::FunctionDeclaration(function) =
                    &export.declaration
                {
                    let id = self.enqueue_named_function(
                        FunctionSource::Function(function),
                        "default",
                        "default".to_owned(),
                        function.span,
                    )?;
                    self.init_function_binding(binding, id)?;
                } else {
                    self.emit(Instr::DeclareBinding { binding });
                }
                break;
            }
        }
        Ok(())
    }

    fn predeclare_function_body(
        &mut self,
        statements: &'ast [Statement<'ast>],
    ) -> Result<(), CompileError> {
        let vars = collect_list(self.source_text(), statements, false, &mut Vec::new())?;
        for name in vars.into_keys() {
            if let Some(binding) = self.predeclare_name(&name, VariableDeclarationKind::Var) {
                self.emit(Instr::DeclareBinding { binding });
            }
        }
        self.predeclare_scope(statements)
    }

    pub(super) fn predeclare_scope(
        &mut self,
        statements: &'ast [Statement<'ast>],
    ) -> Result<(), CompileError> {
        let mut function_names = BTreeSet::new();
        let mut lexical = Vec::new();
        for statement in statements {
            match local_declaration(statement) {
                Some(Declaration::VariableDeclaration(declaration)) => {
                    lexical.extend(self.register_declarators(declaration));
                }
                Some(Declaration::FunctionDeclaration(function)) => {
                    if let Some(identifier) = &function.id {
                        let name = identifier.name.as_str();
                        function_names.insert(name.to_owned());
                        self.predeclare_name(name, VariableDeclarationKind::Let);
                    }
                }
                Some(Declaration::ClassDeclaration(class)) => {
                    if let Some(id) = &class.id {
                        let name = id.name.as_str();
                        if let Some(binding) =
                            self.predeclare_name(name, VariableDeclarationKind::Let)
                        {
                            lexical.push((binding, name.to_owned()));
                        }
                    }
                }
                _ => {}
            }
            if let Statement::ExportDefaultDeclaration(export) = statement {
                match &export.declaration {
                    ExportDefaultDeclarationKind::FunctionDeclaration(function) => {
                        if let Some(identifier) = &function.id {
                            let name = identifier.name.as_str();
                            function_names.insert(name.to_owned());
                            self.predeclare_name(name, VariableDeclarationKind::Let);
                        }
                    }
                    ExportDefaultDeclarationKind::ClassDeclaration(class) => {
                        if let Some(id) = &class.id {
                            let name = id.name.as_str();
                            if let Some(binding) =
                                self.predeclare_name(name, VariableDeclarationKind::Let)
                            {
                                lexical.push((binding, name.to_owned()));
                            }
                        }
                    }
                    _ => {}
                }
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
            if let Statement::ExportDefaultDeclaration(export) = statement {
                let ExportDefaultDeclarationKind::FunctionDeclaration(function) =
                    &export.declaration
                else {
                    continue;
                };
                let Some(identifier) = &function.id else {
                    continue;
                };
                if !self.hoisted.insert(function.span.start) {
                    continue;
                }
                let name = identifier.name.as_str().to_owned();
                let (binding, _) =
                    self.declare(&name, VariableDeclarationKind::Let, function.span)?;
                let func_id = self.enqueue_function(
                    FunctionSource::Function(function),
                    &name,
                    function.span,
                )?;
                self.init_function_binding(binding, func_id)?;
                continue;
            }
            let Some(Declaration::FunctionDeclaration(function)) = local_declaration(statement)
            else {
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
        let lexical = self.register_declarators(declaration);
        self.open_pending(lexical.into_iter().map(|(binding, _)| binding))
    }

    fn register_declarators(
        &mut self,
        declaration: &'ast VariableDeclaration<'ast>,
    ) -> Vec<(BindingId, String)> {
        let mut lexical = Vec::new();
        let is_lexical = declaration.kind.is_lexical();
        for declarator in &declaration.declarations {
            for_each_binding_name(&declarator.id, &mut |identifier| {
                let name = identifier.name.as_str();
                if let Some(binding) = self.predeclare_name(name, declaration.kind)
                    && is_lexical
                {
                    lexical.push((binding, name.to_owned()));
                }
                Ok::<(), std::convert::Infallible>(())
            })
            .expect("infallible binding walk");
        }
        lexical
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
        if self.module_declaration {
            for binding in unique {
                self.emit(Instr::DeclareBinding { binding });
            }
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

    fn predeclare_name(&mut self, name: &str, kind: VariableDeclarationKind) -> Option<BindingId> {
        let binding_kind = as_binding_kind(kind);
        if self.module_declaration
            && let Some(&id) = self.module.slots.get(name)
        {
            return self.env.predeclare(&mut { id }, name, binding_kind);
        }
        self.env
            .predeclare(self.session.binding_ids(), name, binding_kind)
    }

    pub(super) fn compile_block(
        &mut self,
        block: &'ast BlockStatement<'ast>,
    ) -> Result<(), CompileError> {
        self.env.enter_block();
        self.predeclare_scope(&block.body)?;
        self.compile_disposable_body(&block.body)?;
        self.env.exit_block();
        Ok(())
    }

    fn compile_disposable_body(
        &mut self,
        statements: &'ast [Statement<'ast>],
    ) -> Result<(), CompileError> {
        self.compile_disposal_scope(disposal_hint(statements.iter()), |this| {
            for statement in statements {
                this.compile_statement(statement)?;
            }
            Ok(())
        })
    }

    pub(super) fn compile_variable_declaration(
        &mut self,
        declaration: &'ast VariableDeclaration<'ast>,
    ) -> Result<(), CompileError> {
        let lexical = declaration.kind.is_lexical();
        for declarator in &declaration.declarations {
            let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
                for_each_binding_name(&declarator.id, &mut |identifier| {
                    let (binding, _) =
                        self.declare(identifier.name.as_str(), declaration.kind, identifier.span)?;
                    if !lexical && !self.is_script {
                        self.emit(Instr::DeclareBinding { binding });
                    }
                    Ok::<(), CompileError>(())
                })?;
                let expression = declarator.init.as_ref().ok_or_else(|| {
                    CompileError::new(
                        self.source_text(),
                        declarator.span,
                        "destructuring declaration requires an initializer",
                    )
                })?;
                let value = self.compile_expression(expression)?;
                self.compile_binding_pattern(&declarator.id, value)?;
                continue;
            };
            let name = identifier.name.as_str();
            let (binding, _) = self.declare(name, declaration.kind, declarator.span)?;
            if lexical {
                let value = if let Some(expression) = &declarator.init {
                    self.compile_named_expression(expression, name)?
                } else {
                    // A closure may have written this binding before the
                    // declaration. The declaration overwrites that write.
                    self.emit_undefined()?
                };
                if declaration.kind.is_using() {
                    self.emit_add_resource(value, declaration.kind.is_await_using())?;
                }
                self.emit(Instr::SetBinding {
                    binding,
                    src: self.slot(value),
                });
                self.env.clear_pending(binding);
            } else {
                if !self.is_script {
                    self.emit(Instr::DeclareBinding { binding });
                }
                if let Some(expression) = &declarator.init {
                    let value = self.compile_named_expression(expression, name)?;
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

fn as_binding_kind(kind: VariableDeclarationKind) -> BindingKind {
    match kind {
        VariableDeclarationKind::Var => BindingKind::Var,
        VariableDeclarationKind::Let => BindingKind::Lexical,
        VariableDeclarationKind::Const
        | VariableDeclarationKind::Using
        | VariableDeclarationKind::AwaitUsing => BindingKind::Const,
    }
}

/// Inspect only declarations that belong to this statement list's scope.
fn disposal_hint<'ast>(statements: impl Iterator<Item = &'ast Statement<'ast>>) -> Option<bool> {
    let mut hint = None;
    for statement in statements {
        if let Some(Declaration::VariableDeclaration(declaration)) = local_declaration(statement) {
            if declaration.kind.is_await_using() {
                return Some(true);
            }
            if declaration.kind.is_using() {
                hint = Some(false);
            }
        }
    }
    hint
}

/// Not a JavaScript identifier, so script cannot name or shadow this binding.
pub(super) const THIS_BINDING: &str = "\0this";
const HOME_BINDING: &str = "\0home";
const NEW_TARGET_BINDING: &str = "\0new.target";

// Class element bodies have their own execution context. Heritage and
// computed keys, including those of fields, still execute in the enclosing one.
macro_rules! scan_class_keys {
    () => {
        fn visit_property_definition(&mut self, field: &PropertyDefinition<'a>) {
            if field.computed {
                self.visit_property_key(&field.key);
            }
        }

        fn visit_static_block(&mut self, _: &StaticBlock<'a>) {}
    };
}

struct SuperScan {
    found: bool,
}

impl<'a> Visit<'a> for SuperScan {
    fn visit_static_member_expression(&mut self, member: &StaticMemberExpression<'a>) {
        self.found |= matches!(member.object, Expression::Super(_));
        walk::walk_static_member_expression(self, member);
    }

    fn visit_computed_member_expression(&mut self, member: &ComputedMemberExpression<'a>) {
        self.found |= matches!(member.object, Expression::Super(_));
        walk::walk_computed_member_expression(self, member);
    }

    fn visit_function(&mut self, _: &Function<'a>, _: ScopeFlags) {}
    scan_class_keys!();
}

struct NewTargetScan {
    found: bool,
}

impl<'a> Visit<'a> for NewTargetScan {
    fn visit_new_target(&mut self, _: &NewTarget) {
        self.found = true;
    }

    // Ordinary nested functions provide their own target; arrows inherit ours.
    fn visit_function(&mut self, _: &Function<'a>, _: ScopeFlags) {}
    scan_class_keys!();
}

fn function_uses_this(source: FunctionSource<'_>) -> bool {
    let mut scan = ThisScan { found: false };
    match source {
        FunctionSource::Function(function)
        | FunctionSource::Method(function)
        | FunctionSource::Constructor(function) => {
            scan.visit_formal_parameters(&function.params);
            if let Some(body) = &function.body {
                scan.visit_function_body(body);
            }
        }
        FunctionSource::ClassField(expression, ..) => scan.visit_expression(expression),
        FunctionSource::ClassBlock(block) => scan.visit_statements(&block.body),
        _ => {}
    }
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
    scan_class_keys!();
}

fn function_uses_arguments(source: FunctionSource<'_>) -> bool {
    let mut scan = ArgumentsScan { found: false };
    match source {
        FunctionSource::Function(function)
        | FunctionSource::Method(function)
        | FunctionSource::Constructor(function)
        | FunctionSource::DerivedConstructor(function) => {
            scan.visit_formal_parameters(&function.params);
            if let Some(body) = &function.body {
                scan.visit_function_body(body);
            }
        }
        FunctionSource::ClassField(expression, ..) => scan.visit_expression(expression),
        FunctionSource::ClassBlock(block) => scan.visit_statements(&block.body),
        _ => {}
    }
    scan.found
}

struct ArgumentsScan {
    found: bool,
}

impl<'a> Visit<'a> for ArgumentsScan {
    fn visit_identifier_reference(&mut self, identifier: &IdentifierReference<'a>) {
        self.found |= identifier.name.as_str() == "arguments";
    }

    fn visit_function(&mut self, _: &Function<'a>, _: ScopeFlags) {}
    scan_class_keys!();
}

struct ScriptScan<'ast> {
    vars: BTreeMap<String, Span>,
    functions: Vec<(String, &'ast Function<'ast>)>,
}

/// Oxc represents declarations with `export` separately from export lists.
fn local_declaration<'ast>(statement: &'ast Statement<'ast>) -> Option<&'ast Declaration<'ast>> {
    match statement {
        Statement::ExportDeclaration(export) => Some(&export.declaration),
        _ => statement.as_declaration(),
    }
}

fn script_scan<'ast>(
    source: &str,
    statements: &'ast [Statement<'ast>],
) -> Result<ScriptScan<'ast>, CompileError> {
    let mut functions = Vec::new();
    let vars = collect_list(source, statements, true, &mut functions)?;
    Ok(ScriptScan { vars, functions })
}

pub(crate) fn module_locals<'ast>(
    source: &str,
    statements: &'ast [Statement<'ast>],
) -> Result<BTreeSet<String>, CompileError> {
    let mut names: BTreeSet<_> = script_scan(source, statements)?.vars.into_keys().collect();
    for stmt in statements {
        let id = match local_declaration(stmt) {
            Some(Declaration::VariableDeclaration(decl)) => {
                for item in &decl.declarations {
                    for_each_binding_name(&item.id, &mut |id| {
                        names.insert(id.name.as_str().to_owned());
                        Ok::<_, CompileError>(())
                    })?;
                }
                continue;
            }
            Some(Declaration::ClassDeclaration(class)) => class.id.as_ref(),
            _ => {
                if let Statement::ExportDefaultDeclaration(export) = stmt {
                    let id = match &export.declaration {
                        ExportDefaultDeclarationKind::ClassDeclaration(class) => class.id.as_ref(),
                        ExportDefaultDeclarationKind::FunctionDeclaration(function) => {
                            function.id.as_ref()
                        }
                        _ => None,
                    };
                    if id.is_none() {
                        names.insert(crate::frontend::link::DEFAULT_SLOT.into());
                    }
                    id
                } else {
                    None
                }
            }
        };
        if let Some(id) = id {
            names.insert(id.name.as_str().to_owned());
        }
    }
    Ok(names)
}

fn collect_list<'ast>(
    source: &str,
    statements: &'ast [Statement<'ast>],
    func_as_var: bool,
    functions: &mut Vec<(String, &'ast Function<'ast>)>,
) -> Result<BTreeMap<String, Span>, CompileError> {
    let mut lexical = BTreeMap::new();
    let mut vars = BTreeMap::new();
    for statement in statements {
        collect_stmt(
            source,
            statement,
            func_as_var,
            &mut lexical,
            &mut vars,
            functions,
        )?;
    }
    reject_var_lexical(source, &lexical, &vars)?;
    Ok(vars)
}

fn collect_stmt<'ast>(
    source: &str,
    statement: &'ast Statement<'ast>,
    func_as_var: bool,
    lexical: &mut BTreeMap<String, Span>,
    vars: &mut BTreeMap<String, Span>,
    functions: &mut Vec<(String, &'ast Function<'ast>)>,
) -> Result<(), CompileError> {
    match local_declaration(statement) {
        Some(Declaration::VariableDeclaration(declaration)) => {
            bind_declaration(source, declaration, lexical, vars)?;
        }
        Some(Declaration::FunctionDeclaration(function)) => {
            let name = function_name(source, function)?;
            if func_as_var {
                if functions.iter().any(|(existing, _)| existing == &name) {
                    return Err(CompileError::new(
                        source,
                        function.span,
                        format!("duplicate function declaration {name:?}"),
                    ));
                }
                if lexical.contains_key(&name) {
                    return Err(CompileError::new(
                        source,
                        function.span,
                        format!("duplicate lexical binding {name:?}"),
                    ));
                }
                functions.push((name.clone(), function));
                vars.entry(name).or_insert(function.span);
            } else {
                add_lexical(source, lexical, &name, function.span)?;
            }
        }
        Some(Declaration::ClassDeclaration(class)) => {
            if let Some(id) = &class.id {
                add_lexical(source, lexical, id.name.as_str(), class.span)?;
            }
        }
        _ => {}
    }
    if let Statement::ExportDefaultDeclaration(export) = statement {
        match &export.declaration {
            ExportDefaultDeclarationKind::FunctionDeclaration(function) => {
                let Some(identifier) = &function.id else {
                    // The anonymous default function is enqueued and
                    // initialized during export collection; it binds no
                    // script-local name.
                    return Ok(());
                };
                let name = identifier.name.as_str();
                if func_as_var {
                    if functions.iter().any(|(existing, _)| existing == name) {
                        return Err(CompileError::new(
                            source,
                            function.span,
                            format!("duplicate function declaration {name:?}"),
                        ));
                    }
                    if lexical.contains_key(name) {
                        return Err(CompileError::new(
                            source,
                            function.span,
                            format!("duplicate lexical binding {name:?}"),
                        ));
                    }
                    functions.push((name.to_owned(), function));
                    vars.entry(name.to_owned()).or_insert(function.span);
                } else {
                    add_lexical(source, lexical, name, function.span)?;
                }
            }
            ExportDefaultDeclarationKind::ClassDeclaration(class) => {
                if let Some(id) = &class.id {
                    add_lexical(source, lexical, id.name.as_str(), class.span)?;
                }
            }
            _ => {}
        }
    }
    match statement {
        Statement::ImportDeclaration(import) => {
            for specifier in import.specifiers.iter().flatten() {
                let local = specifier.local();
                add_lexical(source, lexical, local.name.as_str(), local.span)?;
            }
        }
        Statement::BlockStatement(block) => {
            merge_vars(vars, collect_list(source, &block.body, false, functions)?);
        }
        Statement::IfStatement(statement) => {
            merge_vars(
                vars,
                collect_nested(source, &statement.consequent, functions)?,
            );
            if let Some(alternate) = &statement.alternate {
                merge_vars(vars, collect_nested(source, alternate, functions)?);
            }
        }
        Statement::WhileStatement(statement) => {
            merge_vars(vars, collect_nested(source, &statement.body, functions)?);
        }
        Statement::DoWhileStatement(statement) => {
            merge_vars(vars, collect_nested(source, &statement.body, functions)?);
        }
        Statement::ForStatement(statement) => {
            let body_vars = collect_nested(source, &statement.body, functions)?;
            if let Some(ForStatementInit::VariableDeclaration(declaration)) = &statement.init {
                bind_for_header(source, declaration, &body_vars, vars)?;
            }
            merge_vars(vars, body_vars);
        }
        Statement::ForInStatement(statement) => {
            let body_vars = collect_nested(source, &statement.body, functions)?;
            if let ForStatementLeft::VariableDeclaration(declaration) = &statement.left {
                bind_for_header(source, declaration, &body_vars, vars)?;
            }
            merge_vars(vars, body_vars);
        }
        Statement::ForOfStatement(statement) => {
            let body_vars = collect_nested(source, &statement.body, functions)?;
            if let ForStatementLeft::VariableDeclaration(declaration) = &statement.left {
                bind_for_header(source, declaration, &body_vars, vars)?;
            }
            merge_vars(vars, body_vars);
        }
        Statement::SwitchStatement(statement) => {
            let mut switch_lexical = BTreeMap::new();
            let mut switch_vars = BTreeMap::new();
            for case in &statement.cases {
                for statement in &case.consequent {
                    collect_stmt(
                        source,
                        statement,
                        false,
                        &mut switch_lexical,
                        &mut switch_vars,
                        functions,
                    )?;
                }
            }
            reject_var_lexical(source, &switch_lexical, &switch_vars)?;
            merge_vars(vars, switch_vars);
        }
        Statement::TryStatement(statement) => {
            merge_vars(
                vars,
                collect_list(source, &statement.block.body, false, functions)?,
            );
            if let Some(handler) = &statement.handler {
                merge_vars(
                    vars,
                    collect_list(source, &handler.body.body, false, functions)?,
                );
            }
            if let Some(finalizer) = &statement.finalizer {
                merge_vars(
                    vars,
                    collect_list(source, &finalizer.body, false, functions)?,
                );
            }
        }
        Statement::LabeledStatement(statement) => {
            collect_stmt(
                source,
                &statement.body,
                func_as_var,
                lexical,
                vars,
                functions,
            )?;
        }
        Statement::WithStatement(statement) => {
            merge_vars(vars, collect_nested(source, &statement.body, functions)?);
        }
        _ => {}
    }
    Ok(())
}

fn collect_nested<'ast>(
    source: &str,
    statement: &'ast Statement<'ast>,
    functions: &mut Vec<(String, &'ast Function<'ast>)>,
) -> Result<BTreeMap<String, Span>, CompileError> {
    let mut lexical = BTreeMap::new();
    let mut vars = BTreeMap::new();
    collect_stmt(source, statement, false, &mut lexical, &mut vars, functions)?;
    reject_var_lexical(source, &lexical, &vars)?;
    Ok(vars)
}

fn bind_declaration(
    source: &str,
    declaration: &VariableDeclaration<'_>,
    lexical: &mut BTreeMap<String, Span>,
    vars: &mut BTreeMap<String, Span>,
) -> Result<(), CompileError> {
    for declarator in &declaration.declarations {
        for_each_binding_name(&declarator.id, &mut |identifier| {
            let name = identifier.name.as_str();
            match declaration.kind {
                VariableDeclarationKind::Var => {
                    vars.entry(name.to_owned()).or_insert(identifier.span);
                }
                VariableDeclarationKind::Let
                | VariableDeclarationKind::Const
                | VariableDeclarationKind::Using
                | VariableDeclarationKind::AwaitUsing => {
                    add_lexical(source, lexical, name, identifier.span)?;
                }
            }
            Ok::<(), CompileError>(())
        })?;
    }
    Ok(())
}

fn bind_for_header(
    source: &str,
    declaration: &VariableDeclaration<'_>,
    body_vars: &BTreeMap<String, Span>,
    vars: &mut BTreeMap<String, Span>,
) -> Result<(), CompileError> {
    for declarator in &declaration.declarations {
        for_each_binding_name(&declarator.id, &mut |identifier| {
            let name = identifier.name.as_str();
            if declaration.kind == VariableDeclarationKind::Var {
                vars.entry(name.to_owned()).or_insert(identifier.span);
            } else if body_vars.contains_key(name) {
                return Err(CompileError::new(
                    source,
                    identifier.span,
                    format!("duplicate lexical binding {name:?}"),
                ));
            }
            Ok(())
        })?;
    }
    Ok(())
}

/// Visit only bound identifiers, in the order in which the pattern binds them.
pub(crate) fn for_each_binding_name<E>(
    pattern: &BindingPattern<'_>,
    visit: &mut impl FnMut(&BindingIdentifier<'_>) -> Result<(), E>,
) -> Result<(), E> {
    match pattern {
        BindingPattern::BindingIdentifier(identifier) => visit(identifier)?,
        BindingPattern::AssignmentPattern(pattern) => for_each_binding_name(&pattern.left, visit)?,
        BindingPattern::ObjectPattern(pattern) => {
            for property in &pattern.properties {
                for_each_binding_name(&property.value, visit)?;
            }
            if let Some(rest) = &pattern.rest {
                for_each_binding_name(&rest.argument, visit)?;
            }
        }
        BindingPattern::ArrayPattern(pattern) => {
            for element in pattern.elements.iter().flatten() {
                for_each_binding_name(element, visit)?;
            }
            if let Some(rest) = &pattern.rest {
                for_each_binding_name(&rest.argument, visit)?;
            }
        }
    }
    Ok(())
}

fn function_name<'ast>(
    source: &str,
    function: &'ast Function<'ast>,
) -> Result<String, CompileError> {
    function
        .id
        .as_ref()
        .map(|identifier| identifier.name.as_str().to_owned())
        .ok_or_else(|| unsupported(source, function.span, "anonymous declaration"))
}

fn add_lexical(
    source: &str,
    lexical: &mut BTreeMap<String, Span>,
    name: &str,
    span: Span,
) -> Result<(), CompileError> {
    if lexical.contains_key(name) {
        return Err(CompileError::new(
            source,
            span,
            format!("duplicate lexical binding {name:?}"),
        ));
    }
    lexical.insert(name.to_owned(), span);
    Ok(())
}

fn reject_var_lexical(
    source: &str,
    lexical: &BTreeMap<String, Span>,
    vars: &BTreeMap<String, Span>,
) -> Result<(), CompileError> {
    for (name, var_span) in vars {
        if let Some(lex_span) = lexical.get(name) {
            let span = if var_span.start >= lex_span.start {
                *var_span
            } else {
                *lex_span
            };
            return Err(CompileError::new(
                source,
                span,
                format!("duplicate lexical binding {name:?}"),
            ));
        }
    }
    Ok(())
}

fn merge_vars(into: &mut BTreeMap<String, Span>, child: BTreeMap<String, Span>) {
    for (name, span) in child {
        into.entry(name).or_insert(span);
    }
}
