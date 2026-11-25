//! Reachable source discovery and static linking, independent of evaluation.

mod resolve;

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Deref;

use oxc::allocator::Allocator;
use oxc::ast::ast::*;
use oxc::span::{GetSpan, Span};
use yield_vm::bytecode::ImportDecl;

use crate::error::{CompileError, unsupported};
use crate::{Compiler, ModuleId, ModuleResolver, ParsedModule, ResolvedModule, SourceTransform};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Binding { module: u32, name: String },
    Namespace { module: u32 },
    Host(ImportDecl),
}

#[derive(Clone, Copy, Debug)]
enum Edge {
    Source(u32),
    Host(u32),
}

struct Request {
    edge: Edge,
    name: Option<String>,
    span: Span,
}

enum Export {
    Local(String),
    Request(Request),
}

#[derive(Default)]
struct Record {
    explicit: BTreeMap<String, (Export, Span)>,
    stars: Vec<Edge>,
    locals: BTreeSet<String>,
    imports: BTreeMap<String, Request>,
}

pub(crate) const DEFAULT_SLOT: &str = "\0default";

/// The kind of a module request, before import/export binding resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DependencyKind {
    Static,
    /// A literal `import()` expression whose source target is linked eagerly.
    Dynamic,
    Host,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependency {
    pub specifier: String,
    pub kind: DependencyKind,
    /// Canonical source ID, or host manifest name for a host edge.
    pub target: ModuleId,
}

pub(crate) struct Module<'a> {
    parsed: ParsedModule<'a>,
    original_source: &'a str,
    /// Framework mark the resolver reported for this module's definition.
    pub(crate) is_framework: bool,
    pub(crate) dependencies: Vec<Dependency>,
    pub(crate) deps: Vec<u32>,
    pub(crate) in_preload: bool,
    pub(crate) locals: BTreeSet<String>,
    pub(crate) imports: BTreeMap<String, Target>,
    pub(crate) exports: BTreeMap<String, Target>,
}

impl<'a> Deref for Module<'a> {
    type Target = ParsedModule<'a>;

    fn deref(&self) -> &Self::Target {
        &self.parsed
    }
}

/// A reachable source graph. Only parsed ASTs are mutable; dependency and
/// linker tables are derived again after each graph transform. New source
/// requests must resolve to existing graph modules. Only static requests can
/// resolve through the host import manifest.
pub struct ModuleGraph<'a> {
    pub(crate) modules: Vec<Module<'a>>,
    pub(crate) entry: u32,
    pub(crate) preload: Option<u32>,
}

impl<'a> ModuleGraph<'a> {
    pub fn entry(&self) -> &ModuleId {
        self.modules[self.entry as usize].id()
    }

    pub fn preload(&self) -> Option<&ModuleId> {
        self.preload.map(|id| self.modules[id as usize].id())
    }

    pub fn modules(&self) -> impl ExactSizeIterator<Item = &ParsedModule<'a>> {
        self.modules.iter().map(|module| &module.parsed)
    }

    pub fn modules_mut(&mut self) -> impl ExactSizeIterator<Item = &mut ParsedModule<'a>> {
        self.modules.iter_mut().map(|module| &mut module.parsed)
    }

    pub fn module(&self, id: &ModuleId) -> Option<&ParsedModule<'a>> {
        self.modules().find(|module| module.id() == id)
    }

    pub fn module_mut(&mut self, id: &ModuleId) -> Option<&mut ParsedModule<'a>> {
        self.modules_mut().find(|module| module.id() == id)
    }

    pub fn dependencies(&self, id: &ModuleId) -> Option<&[Dependency]> {
        self.modules
            .iter()
            .find(|module| module.id() == id)
            .map(|module| module.dependencies.as_slice())
    }

    pub(crate) fn relink(
        &mut self,
        resolver: &dyn ModuleResolver,
        options: &Compiler,
    ) -> Result<(), CompileError> {
        let hosts = hosts(options)?;
        let ids: BTreeMap<_, _> = self
            .modules
            .iter()
            .enumerate()
            .map(|(index, module)| (module.id.clone(), index as u32))
            .collect();
        let mut records = Vec::with_capacity(self.modules.len());
        // Every edge re-resolution below refreshes the mark of the module it targets,
        // so a graph transform cannot leave a stale framework classification behind.
        let mut framework: Vec<bool> = self
            .modules
            .iter()
            .map(|module| module.is_framework)
            .collect();
        for module in &mut self.modules {
            super::validate_syntax(&module.program).map_err(|err| err.in_module(&module.id))?;
            super::jsx::lower(&mut module.parsed, &options.jsx_runtime)?;
            let mut record = Record::default();
            let mut deps = Vec::new();
            let mut dependencies = Vec::new();
            let mut seen = BTreeSet::new();
            let source = module.source();
            for stmt in &module.program.body {
                let edge = if let Some((specifier, span)) = module_request(module, stmt)? {
                    let (edge, kind, target) = if let Some(&host) = hosts.get(specifier) {
                        (Edge::Host(host), DependencyKind::Host, specifier.to_owned())
                    } else {
                        let locate =
                            |reason| dependency_error(source, span, &module.id, specifier, reason);
                        let ResolvedModule {
                            id: target,
                            is_framework,
                        } = crate::resolved_id(resolver, Some(&module.id), specifier)
                            .map_err(locate)?;
                        let index = *ids.get(&target).ok_or_else(|| {
                            locate(format!("target {target:?} is not in the graph"))
                        })?;
                        framework[index as usize] = is_framework;
                        if seen.insert(index) {
                            deps.push(index);
                        }
                        (Edge::Source(index), DependencyKind::Static, target)
                    };
                    dependencies.push(Dependency {
                        specifier: specifier.to_owned(),
                        kind,
                        target,
                    });
                    Some(edge)
                } else {
                    None
                };
                extract(&mut record, stmt, edge, source)
                    .map_err(|err| err.in_module(&module.id))?;
            }
            let dynamic = super::dynamic_imports(&module.program).map_err(|(span, feature)| {
                unsupported(source, span, feature).in_module(&module.id)
            })?;
            for (specifier, span) in dynamic {
                let locate = |reason| dependency_error(source, span, &module.id, specifier, reason);
                let ResolvedModule {
                    id: target_id,
                    is_framework,
                } = crate::resolved_id(resolver, Some(&module.id), specifier).map_err(locate)?;
                let target = *ids
                    .get(&target_id)
                    .ok_or_else(|| locate(format!("target {target_id:?} is not in the graph")))?;
                framework[target as usize] = is_framework;
                if seen.insert(target) {
                    deps.push(target);
                }
                dependencies.push(Dependency {
                    specifier: specifier.to_owned(),
                    kind: DependencyKind::Dynamic,
                    target: target_id,
                });
            }
            record.locals = crate::lower::module_locals(source, &module.program.body)
                .map_err(|err| err.in_module(&module.id))?;
            records.push(record);
            module.deps = deps;
            module.dependencies = dependencies;
            module.in_preload = false;
        }
        for (module, is_framework) in self.modules.iter_mut().zip(framework) {
            module.is_framework = is_framework;
        }
        let mut work = self.preload.into_iter().collect::<Vec<_>>();
        while let Some(index) = work.pop() {
            let module = &mut self.modules[index as usize];
            if !std::mem::replace(&mut module.in_preload, true) {
                work.extend_from_slice(&module.deps);
            }
        }
        self.resolve_bindings(records, options)?;
        self.prune_unreachable();
        Ok(())
    }

    fn prune_unreachable(&mut self) {
        let mut reachable = vec![false; self.modules.len()];
        let mut work = self
            .preload
            .into_iter()
            .chain([self.entry])
            .collect::<Vec<_>>();
        while let Some(index) = work.pop() {
            if !std::mem::replace(&mut reachable[index as usize], true) {
                work.extend_from_slice(&self.modules[index as usize].deps);
            }
        }
        let mut remap = Vec::with_capacity(reachable.len());
        let mut next = 0;
        self.modules.retain(|_| {
            let keep = reachable[remap.len()];
            remap.push(keep.then_some(next));
            next += u32::from(keep);
            keep
        });
        let remap = |index: u32| remap[index as usize].expect("target of reachable module");
        self.entry = remap(self.entry);
        self.preload = self.preload.map(remap);
        for module in &mut self.modules {
            for dep in &mut module.deps {
                *dep = remap(*dep);
            }
            for target in module
                .imports
                .values_mut()
                .chain(module.exports.values_mut())
            {
                match target {
                    Target::Binding { module, .. } | Target::Namespace { module } => {
                        *module = remap(*module)
                    }
                    Target::Host(_) => {}
                }
            }
        }
    }

    fn resolve_bindings(
        &mut self,
        records: Vec<Record>,
        options: &Compiler,
    ) -> Result<(), CompileError> {
        let resolved = resolve::resolve(&self.modules, &records, options)?;
        for ((module, names), record) in self.modules.iter_mut().zip(resolved).zip(records) {
            module.imports = names.imports;
            module.exports = names.exports;
            module.locals = record.locals;
        }
        Ok(())
    }
}

fn hosts(options: &Compiler) -> Result<BTreeMap<&str, u32>, CompileError> {
    options
        .imports
        .keys()
        .map(String::as_str)
        .enumerate()
        .map(|(index, key)| u32::try_from(index).map(|index| (key, index)))
        .collect::<Result<_, _>>()
        .map_err(|_| CompileError::internal("too many host modules"))
}

fn module_request<'s>(
    module: &Module<'_>,
    stmt: &'s Statement<'_>,
) -> Result<Option<(&'s str, Span)>, CompileError> {
    let source = module.source();
    let Some(request) = request(stmt)
        .map_err(|(span, feature)| unsupported(source, span, feature).in_module(&module.id))?
    else {
        return Ok(None);
    };
    let specifier = request.value.as_str().ok_or_else(|| {
        unsupported(source, request.span, "non-string module specifier").in_module(&module.id)
    })?;
    Ok(Some((specifier, request.span)))
}

fn dependency_error(
    source: &str,
    span: Span,
    referrer: &str,
    specifier: &str,
    reason: String,
) -> CompileError {
    let error = CompileError::new(
        source,
        span,
        format!("source dependency {specifier:?} from {referrer:?}: {reason}"),
    )
    .in_module(referrer);
    if span == super::jsx::IMPORT_SPAN {
        error.in_stage(super::jsx::STAGE)
    } else {
        error
    }
}

pub(crate) fn link<'a>(
    allocator: &'a Allocator,
    entry: &str,
    preload: Option<&str>,
    resolver: &dyn ModuleResolver,
    options: &Compiler,
    transforms: &mut [Box<dyn SourceTransform + '_>],
) -> Result<ModuleGraph<'a>, CompileError> {
    let hosts = hosts(options)?;
    let mut graph = ModuleGraph {
        modules: Vec::new(),
        entry: 0,
        preload: None,
    };
    let mut ids = BTreeMap::new();
    let mut discovery = Discovery {
        allocator,
        resolver,
        transforms,
        jsx_runtime: &options.jsx_runtime,
    };
    let mut records = Vec::new();
    for (root, in_preload) in preload
        .into_iter()
        .map(|name| (name, true))
        .chain([(entry, false)])
    {
        let root_error = |reason| crate::root_error(root, reason);
        if root.is_empty() {
            return Err(root_error("empty source root".into()));
        }
        let ResolvedModule { id, is_framework } =
            crate::resolved_id(resolver, None, root).map_err(root_error)?;
        let (index, fresh) = insert(
            &mut graph,
            &mut ids,
            &mut discovery,
            id,
            is_framework,
            in_preload,
            root_error,
        )?;
        if in_preload {
            graph.preload = Some(index);
        } else {
            graph.entry = index;
        }
        if !fresh {
            continue;
        }
        records.push(Record::default());
        let mut work = vec![(index, 0, BTreeSet::new())];
        while let Some((module, cursor, seen)) = work.last_mut() {
            let i = *module as usize;
            let current = &graph.modules[i];
            let source = current.program.source_text;
            let Some(stmt) = current.program.body.get(*cursor) else {
                records[i].locals = crate::lower::module_locals(source, &current.program.body)
                    .map_err(|error| error.in_module(&current.id))?;
                let dynamic =
                    super::dynamic_imports(&current.program).map_err(|(span, feature)| {
                        unsupported(source, span, feature).in_module(&current.id)
                    })?;
                let referrer = current.id.clone();
                let mut next = Vec::new();
                for (specifier, span) in dynamic {
                    let locate =
                        |reason| dependency_error(source, span, &referrer, specifier, reason);
                    let ResolvedModule { id, is_framework } =
                        crate::resolved_id(resolver, Some(&referrer), specifier).map_err(locate)?;
                    let (target, fresh) = insert(
                        &mut graph,
                        &mut ids,
                        &mut discovery,
                        id,
                        is_framework,
                        in_preload,
                        locate,
                    )?;
                    if seen.insert(target) {
                        graph.modules[i].deps.push(target);
                    }
                    let target_id = graph.modules[target as usize].id.clone();
                    graph.modules[i].dependencies.push(Dependency {
                        specifier: specifier.to_owned(),
                        kind: DependencyKind::Dynamic,
                        target: target_id,
                    });
                    if fresh {
                        records.push(Record::default());
                        next.push((target, 0, BTreeSet::new()));
                    }
                }
                work.pop();
                work.extend(next.into_iter().rev());
                continue;
            };
            *cursor += 1;
            let mut fresh = None;
            let edge = if let Some((specifier, span)) = module_request(current, stmt)? {
                let specifier = specifier.to_owned();
                let (edge, kind, target) = if let Some(&host) = hosts.get(specifier.as_str()) {
                    (Edge::Host(host), DependencyKind::Host, specifier.clone())
                } else {
                    let referrer = current.id.clone();
                    let location =
                        |reason| dependency_error(source, span, &referrer, &specifier, reason);
                    let ResolvedModule { id, is_framework } =
                        crate::resolved_id(resolver, Some(&referrer), &specifier)
                            .map_err(location)?;
                    let (target, is_new) = insert(
                        &mut graph,
                        &mut ids,
                        &mut discovery,
                        id,
                        is_framework,
                        in_preload,
                        location,
                    )?;
                    if seen.insert(target) {
                        graph.modules[i].deps.push(target);
                    }
                    if is_new {
                        fresh = Some(target);
                    }
                    (
                        Edge::Source(target),
                        DependencyKind::Static,
                        graph.modules[target as usize].id.clone(),
                    )
                };
                graph.modules[i].dependencies.push(Dependency {
                    specifier,
                    kind,
                    target,
                });
                Some(edge)
            } else {
                None
            };
            let current = &graph.modules[i];
            extract(
                &mut records[i],
                &current.program.body[*cursor - 1],
                edge,
                source,
            )
            .map_err(|err| err.in_module(&current.id))?;
            if let Some(next) = fresh {
                records.push(Record::default());
                work.push((next, 0, BTreeSet::new()));
            }
        }
    }
    graph.resolve_bindings(records, options)?;
    Ok(graph)
}

struct Discovery<'a, 'ctx, 'transform> {
    allocator: &'a Allocator,
    resolver: &'ctx dyn ModuleResolver,
    transforms: &'ctx mut [Box<dyn SourceTransform + 'transform>],
    jsx_runtime: &'ctx str,
}

fn insert<'a>(
    graph: &mut ModuleGraph<'a>,
    ids: &mut BTreeMap<ModuleId, u32>,
    discovery: &mut Discovery<'a, '_, '_>,
    id: ModuleId,
    is_framework: bool,
    in_preload: bool,
    locate: impl Fn(String) -> CompileError,
) -> Result<(u32, bool), CompileError> {
    let Discovery {
        allocator,
        resolver,
        transforms,
        jsx_runtime,
    } = discovery;
    let allocator = *allocator;
    let loaded = resolver.load_for_compile(&id, &locate)?;
    if let Some(&index) = ids.get(&id) {
        if graph.modules[index as usize].original_source != loaded.as_ref() {
            return Err(locate(format!(
                "conflicting source bodies for module ID {id:?}"
            )));
        }
        return Ok((index, false));
    }
    let index =
        u32::try_from(graph.modules.len()).map_err(|_| locate("too many source modules".into()))?;
    let source = allocator.alloc_str(loaded.as_ref());
    let program = super::parse(allocator, source, &id).map_err(|err| err.in_module(&id))?;
    let mut parsed = ParsedModule {
        id: id.clone(),
        allocator,
        program,
    };
    for (index, transform) in transforms.iter_mut().enumerate() {
        let stage = format!("source transform {}", index + 1);
        transform
            .transform(&mut parsed)
            .map_err(|error| error.diagnostic(&stage, Some(&parsed)))?;
        super::validate_syntax(&parsed.program)
            .map_err(|error| error.in_module(&id).in_stage(&stage))?;
    }
    super::jsx::lower(&mut parsed, jsx_runtime)?;
    ids.insert(id.clone(), index);
    graph.modules.push(Module {
        parsed,
        original_source: source,
        is_framework,
        dependencies: Vec::new(),
        deps: Vec::new(),
        in_preload,
        locals: BTreeSet::new(),
        imports: BTreeMap::new(),
        exports: BTreeMap::new(),
    });
    Ok((index, true))
}

fn request<'a, 'b>(
    stmt: &'b Statement<'a>,
) -> Result<Option<&'b StringLiteral<'a>>, (Span, &'static str)> {
    match stmt {
        Statement::ImportDeclaration(import)
            if import.phase.is_some() || import.with_clause.is_some() =>
        {
            Err((import.span, "import phases or attributes"))
        }
        Statement::ExportFromDeclaration(export) if export.with_clause.is_some() => {
            Err((export.span, "export attributes"))
        }
        Statement::ExportAllDeclaration(export) if export.with_clause.is_some() => {
            Err((export.span, "export attributes"))
        }
        Statement::ImportDeclaration(import) => Ok(Some(&import.source)),
        Statement::ExportFromDeclaration(export) => Ok(Some(&export.source)),
        Statement::ExportAllDeclaration(export) => Ok(Some(&export.source)),
        _ => Ok(None),
    }
}

fn extract(
    record: &mut Record,
    stmt: &Statement<'_>,
    edge: Option<Edge>,
    source: &str,
) -> Result<(), CompileError> {
    let edge = || edge.expect("source request extracted");
    let insert = |record: &mut Record, name: &str, target, span| {
        if record
            .explicit
            .insert(name.to_owned(), (target, span))
            .is_some()
        {
            return Err(CompileError::new(
                source,
                span,
                format!("duplicate export {name:?}"),
            ));
        }
        Ok(())
    };
    let local = |record: &mut Record, id: &BindingIdentifier<'_>| {
        let name = id.name.as_str();
        insert(record, name, Export::Local(name.to_owned()), id.span)
    };
    match stmt {
        Statement::ImportDeclaration(import) => {
            use ImportDeclarationSpecifier::*;
            for spec in import.specifiers.iter().flatten() {
                let name = spec.local().name.as_str();
                let (requested, span) = match spec {
                    ImportSpecifier(spec) => (
                        Some(spec.imported.name().as_str().to_owned()),
                        spec.imported.span(),
                    ),
                    ImportDefaultSpecifier(spec) => (Some("default".into()), spec.span),
                    ImportNamespaceSpecifier(spec) => (None, spec.span),
                };
                record.imports.insert(
                    name.to_owned(),
                    Request {
                        edge: edge(),
                        name: requested,
                        span,
                    },
                );
            }
        }
        Statement::ExportNamedDeclaration(export) => {
            for spec in &export.specifiers {
                insert(
                    record,
                    spec.exported.name().as_str(),
                    Export::Local(spec.local.name().as_str().to_owned()),
                    spec.exported.span(),
                )?;
            }
        }
        Statement::ExportFromDeclaration(export) => {
            for spec in &export.specifiers {
                insert(
                    record,
                    spec.exported.name().as_str(),
                    Export::Request(Request {
                        edge: edge(),
                        name: Some(spec.local.name().as_str().to_owned()),
                        span: spec.local.span(),
                    }),
                    spec.local.span(),
                )?;
            }
        }
        Statement::ExportAllDeclaration(export) => {
            if let Some(name) = &export.exported {
                insert(
                    record,
                    name.name().as_str(),
                    Export::Request(Request {
                        edge: edge(),
                        name: None,
                        span: name.span(),
                    }),
                    name.span(),
                )?;
            } else {
                record.stars.push(edge());
            }
        }
        Statement::ExportDeclaration(export) => match &export.declaration {
            Declaration::ClassDeclaration(class) => {
                if let Some(id) = &class.id {
                    local(record, id)?;
                }
            }
            Declaration::FunctionDeclaration(function) => {
                if let Some(id) = &function.id {
                    local(record, id)?;
                }
            }
            Declaration::VariableDeclaration(decl) => {
                for item in &decl.declarations {
                    crate::lower::for_each_binding_name(&item.id, &mut |id| local(record, id))?;
                }
            }
            _ => {}
        },
        Statement::ExportDefaultDeclaration(export) => {
            let id = match &export.declaration {
                ExportDefaultDeclarationKind::FunctionDeclaration(function) => function.id.as_ref(),
                ExportDefaultDeclarationKind::ClassDeclaration(class) => class.id.as_ref(),
                _ => None,
            };
            insert(
                record,
                "default",
                Export::Local(id.map_or(DEFAULT_SLOT, |id| id.name.as_str()).to_owned()),
                export.span,
            )?;
        }
        _ => {}
    }
    Ok(())
}
