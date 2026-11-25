use std::collections::{BTreeMap, BTreeSet, VecDeque};

use oxc::span::Span;
use yield_vm::bytecode::ImportDecl;

use super::{Edge, Export, Module, Record, Request, Target};
use crate::{CompileError, Compiler};

pub(super) struct Resolved {
    pub(super) imports: BTreeMap<String, Target>,
    pub(super) exports: BTreeMap<String, Target>,
}

type Key<'a> = (u32, &'a str);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Terminal<'a> {
    Binding(Key<'a>),
    Namespace(u32),
    Host(ImportDecl),
}

impl Terminal<'_> {
    fn target(self) -> Target {
        match self {
            Self::Binding((module, name)) => Target::Binding {
                module,
                name: name.to_owned(),
            },
            Self::Namespace(module) => Target::Namespace { module },
            Self::Host(decl) => Target::Host(decl),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome<'a> {
    Unique(Terminal<'a>),
    Ambiguous,
}

struct Worklist<'a> {
    values: Vec<BTreeMap<&'a str, Outcome<'a>>>,
    pending: VecDeque<Key<'a>>,
}

impl<'a> Worklist<'a> {
    fn merge(&mut self, (module, name): Key<'a>, value: Outcome<'a>) {
        use std::collections::btree_map::Entry;

        match self.values[module as usize].entry(name) {
            Entry::Vacant(entry) => {
                entry.insert(value);
            }
            Entry::Occupied(mut entry) => {
                if *entry.get() == value || *entry.get() == Outcome::Ambiguous {
                    return;
                }
                entry.insert(Outcome::Ambiguous);
            }
        }
        self.pending.push_back((module, name));
    }
}

struct Host<'a> {
    name: &'a str,
    exports: Vec<&'a str>,
}

fn host_target<'a>(module: u32, export: Option<u32>) -> Terminal<'a> {
    Terminal::Host(ImportDecl { module, export })
}

fn explicit_request<'a>(record: &'a Record, export: &'a Export) -> Option<&'a Request> {
    match export {
        Export::Local(name) => record.imports.get(name),
        Export::Request(request) => Some(request),
    }
}

fn source_key(request: &Request) -> Option<Key<'_>> {
    match (request.edge, request.name.as_deref()) {
        (Edge::Source(module), Some(name)) => Some((module, name)),
        _ => None,
    }
}

fn request_outcome<'a>(
    edge: Edge,
    name: Option<&str>,
    hosts: &[Host<'_>],
    values: &[BTreeMap<&'a str, Outcome<'a>>],
) -> Option<Outcome<'a>> {
    match (edge, name) {
        (Edge::Source(module), Some(name)) => values[module as usize].get(name).copied(),
        (Edge::Source(module), None) => Some(Outcome::Unique(Terminal::Namespace(module))),
        (Edge::Host(module), name) => {
            let export = match name {
                Some(name) => {
                    Some(hosts[module as usize].exports.binary_search(&name).ok()? as u32)
                }
                None => None,
            };
            Some(Outcome::Unique(host_target(module, export)))
        }
    }
}

pub(super) fn resolve(
    modules: &[Module<'_>],
    records: &[Record],
    options: &Compiler,
) -> Result<Vec<Resolved>, CompileError> {
    let hosts: Vec<_> = options
        .imports
        .iter()
        .map(|(name, exports)| {
            if u32::try_from(exports.len()).is_err() {
                return Err(CompileError::internal("too many host module exports"));
            }
            Ok(Host {
                name,
                exports: exports.iter().map(String::as_str).collect(),
            })
        })
        .collect::<Result<_, _>>()?;
    let mut work = Worklist {
        values: vec![BTreeMap::new(); records.len()],
        pending: VecDeque::new(),
    };
    let mut star_users = vec![Vec::new(); records.len()];
    let mut alias_users = vec![BTreeMap::<&str, Vec<Key<'_>>>::new(); records.len()];

    for (index, record) in records.iter().enumerate() {
        let module = index as u32;
        for (name, (export, _)) in &record.explicit {
            let key = (module, name.as_str());
            if let Some(request) = explicit_request(record, export) {
                if let Some((source, name)) = source_key(request) {
                    alias_users[source as usize]
                        .entry(name)
                        .or_default()
                        .push(key);
                } else if let Some(outcome) =
                    request_outcome(request.edge, request.name.as_deref(), &hosts, &work.values)
                {
                    work.merge(key, outcome);
                }
            } else if let Export::Local(local) = export
                && record.locals.contains(local)
            {
                work.merge(
                    key,
                    Outcome::Unique(Terminal::Binding((module, local.as_str()))),
                );
            }
        }
        for edge in &record.stars {
            match *edge {
                Edge::Source(source) => star_users[source as usize].push(module),
                Edge::Host(host) => {
                    for (slot, &name) in hosts[host as usize].exports.iter().enumerate() {
                        if name != "default" && !record.explicit.contains_key(name) {
                            work.merge(
                                (module, name),
                                Outcome::Unique(host_target(host, Some(slot as u32))),
                            );
                        }
                    }
                }
            }
        }
    }

    // Propagate terminal identities backwards to a fixed point. A back edge
    // contributes no targets on its own; a cycle with a reachable terminal
    // still receives that terminal. Each key advances at most twice: absent
    // to unique, then to ambiguous (or straight from absent to ambiguous).
    // This also discovers star names without scanning unrelated modules.
    while let Some((module, name)) = work.pending.pop_front() {
        let outcome = work.values[module as usize][name];
        if let Some(users) = alias_users[module as usize].get(name) {
            for &user in users {
                work.merge(user, outcome);
            }
        }
        if name != "default" {
            for &user in &star_users[module as usize] {
                if !records[user as usize].explicit.contains_key(name) {
                    work.merge((user, name), outcome);
                }
            }
        }
    }

    let mut resolved = Vec::with_capacity(records.len());
    for (index, (module, record)) in modules.iter().zip(records).enumerate() {
        for (name, (export, span)) in &record.explicit {
            if let Some(request) = explicit_request(record, export) {
                require_request(module, request, &hosts, modules, records, &work.values)?;
            } else if !work.values[index].contains_key(name.as_str()) {
                let Export::Local(local) = export else {
                    unreachable!();
                };
                return Err(error(
                    module,
                    *span,
                    format!("unknown export binding {local:?}"),
                ));
            }
        }
        let imports = record
            .imports
            .iter()
            .map(|(local, request)| {
                require_request(module, request, &hosts, modules, records, &work.values)
                    .map(|target| (local.clone(), target))
            })
            .collect::<Result<_, _>>()?;
        let exports = work.values[index]
            .iter()
            .filter_map(|(&name, outcome)| match *outcome {
                Outcome::Unique(terminal) => Some((name.to_owned(), terminal.target())),
                Outcome::Ambiguous => None,
            })
            .collect();
        resolved.push(Resolved { imports, exports });
    }
    Ok(resolved)
}

fn require_request<'a>(
    module: &Module<'_>,
    request: &Request,
    hosts: &[Host<'_>],
    modules: &[Module<'_>],
    records: &[Record],
    values: &[BTreeMap<&'a str, Outcome<'a>>],
) -> Result<Target, CompileError> {
    let outcome = request_outcome(request.edge, request.name.as_deref(), hosts, values);
    if let Some(Outcome::Unique(terminal)) = outcome {
        return Ok(terminal.target());
    }
    let name = request
        .name
        .as_deref()
        .expect("namespace resolves directly");
    let message = match request.edge {
        Edge::Host(host) => format!(
            "host module {:?} has no export {name:?}",
            hosts[host as usize].name
        ),
        Edge::Source(source) => {
            let id = &modules[source as usize].id;
            if outcome == Some(Outcome::Ambiguous) {
                format!("module {id:?} has ambiguous export {name:?}")
            } else if explicit_cycle(records, source, name) {
                format!(
                    "circular export alias: module {id:?} has no export {name:?} with a binding"
                )
            } else {
                format!("module {id:?} has no export {name:?}")
            }
        }
    };
    Err(error(module, request.span, message))
}

fn explicit_cycle(records: &[Record], module: u32, name: &str) -> bool {
    let mut key = (module, name.to_owned());
    let mut seen = BTreeSet::new();
    loop {
        if !seen.insert(key.clone()) {
            return true;
        }
        let record = &records[key.0 as usize];
        let Some(next) = record
            .explicit
            .get(key.1.as_str())
            .and_then(|(export, _)| explicit_request(record, export))
            .and_then(source_key)
        else {
            return false;
        };
        key = (next.0, next.1.to_owned());
    }
}

fn error(module: &Module<'_>, span: Span, message: String) -> CompileError {
    CompileError::new(module.program.source_text, span, message).in_module(&module.id)
}
