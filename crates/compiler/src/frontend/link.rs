//! Reachable source discovery and static linking, independent of evaluation.

mod resolve;

use std::collections::{BTreeMap, BTreeSet};

use oxc::allocator::Allocator;
use oxc::ast::ast::*;
use oxc::span::{GetSpan, Span};
use yield_vm::bytecode::ImportDecl;

use crate::error::{CompileError, unsupported};
use crate::{CompileOptions, ModuleResolver};

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

pub(crate) struct Module<'a> {
    pub(crate) id: String,
    pub(crate) program: Program<'a>,
    pub(crate) deps: Vec<u32>,
    pub(crate) in_preload: bool,
    pub(crate) locals: BTreeSet<String>,
    pub(crate) imports: BTreeMap<String, Target>,
    pub(crate) exports: BTreeMap<String, Target>,
}

pub(crate) struct Graph<'a> {
    pub(crate) modules: Vec<Module<'a>>,
    pub(crate) entry: u32,
    pub(crate) preload: Option<u32>,
}

pub(crate) fn link<'a>(
    allocator: &'a Allocator,
    entry: &str,
    preload: Option<&str>,
    resolver: &'a dyn ModuleResolver,
    options: &CompileOptions,
) -> Result<Graph<'a>, CompileError> {
    let hosts: BTreeMap<_, _> = options
        .imports
        .keys()
        .map(String::as_str)
        .enumerate()
        .map(|(index, key)| u32::try_from(index).map(|index| (key, index)))
        .collect::<Result<_, _>>()
        .map_err(|_| CompileError::internal("too many host modules"))?;
    let mut graph = Graph {
        modules: Vec::new(),
        entry: 0,
        preload: None,
    };
    let mut ids = BTreeMap::new();
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
        let id = crate::resolved_id(resolver, None, root).map_err(root_error)?;
        let (index, fresh) = insert(
            &mut graph, &mut ids, allocator, resolver, id, in_preload, root_error,
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
                work.pop();
                continue;
            };
            *cursor += 1;
            let mut fresh = None;
            let edge = if let Some(request) = request(stmt).map_err(|(span, feature)| {
                unsupported(source, span, feature).in_module(&current.id)
            })? {
                let referrer = current.id.clone();
                let specifier = request.value.as_str().ok_or_else(|| {
                    unsupported(source, request.span, "non-string module specifier")
                        .in_module(&referrer)
                })?;
                if let Some(&host) = hosts.get(specifier) {
                    Some(Edge::Host(host))
                } else {
                    let span = request.span;
                    let location = |reason| {
                        CompileError::new(
                            source,
                            span,
                            format!("source dependency {specifier:?} from {referrer:?}: {reason}"),
                        )
                        .in_module(&referrer)
                    };
                    let id = crate::resolved_id(resolver, Some(&referrer), specifier)
                        .map_err(location)?;
                    let (target, is_new) = insert(
                        &mut graph, &mut ids, allocator, resolver, id, in_preload, location,
                    )?;
                    if seen.insert(target) {
                        graph.modules[i].deps.push(target);
                    }
                    if is_new {
                        fresh = Some(target);
                    }
                    Some(Edge::Source(target))
                }
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
    let resolved = resolve::resolve(&graph.modules, &records, options)?;
    for (module, names) in graph.modules.iter_mut().zip(resolved) {
        module.imports = names.imports;
        module.exports = names.exports;
    }
    for (module, record) in graph.modules.iter_mut().zip(records) {
        module.locals = record.locals;
    }
    Ok(graph)
}

fn insert<'a>(
    graph: &mut Graph<'a>,
    ids: &mut BTreeMap<String, u32>,
    allocator: &'a Allocator,
    resolver: &'a dyn ModuleResolver,
    id: String,
    in_preload: bool,
    locate: impl Fn(String) -> CompileError,
) -> Result<(u32, bool), CompileError> {
    if let Some(&index) = ids.get(&id) {
        return Ok((index, false));
    }
    let index =
        u32::try_from(graph.modules.len()).map_err(|_| locate("too many source modules".into()))?;
    let source = resolver.load(&id).map_err(locate)?;
    let source = match source {
        std::borrow::Cow::Borrowed(text) => text,
        std::borrow::Cow::Owned(text) => allocator.alloc_str(&text),
    };
    let program = super::parse(allocator, source).map_err(|err| err.in_module(&id))?;
    ids.insert(id.clone(), index);
    graph.modules.push(Module {
        id,
        program,
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
