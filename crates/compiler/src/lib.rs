//! Strict-mode JavaScript subset compiler for the resumable VM.
//!
//! Oxc parses the source, which is lowered to logical-register code and then
//! decoded into a control-flow IR. Optimizations operate on blocks; only final
//! layout assigns bytecode PCs and slot allocation produces the VM function.
//! Source and graph transforms edit the workspace-pinned Oxc AST; the VM and
//! bytecode contracts are independent of Oxc.
//!
//! ```text
//! source roots and resolver
//!   → frontend   parse and link reachable modules
//!   → lower      module declaration and evaluation phases, then nested functions
//!   → bindings   owner table; cross-function reads and writes stay shared
//!   → cfg        block IR with explicit resumption and exception edges
//!   → promote    unshared bindings become logical registers; non-escaping
//!                loop bindings leave the per-iteration environment
//!   → propagate  cross-block literal and copy facts, branch folding, unreachable code
//!   → analyze    CFG live-in and dead pure writes, including resume and handler edges
//!   → allocate   final bytecode layout, then persistent and transient frame slots
//!   → Build
//! ```

mod allocate;
mod analyze;
mod bindings;
mod cfg;
mod emit;
mod env;
mod error;
mod frontend;
mod instr;
mod ir;
mod lower;
mod names;
mod promote;
mod propagate;
mod resolver;
mod session;
mod source_lines;
mod transform;

use std::collections::{BTreeMap, BTreeSet};

use oxc::allocator::Allocator;
use oxc::span::Span;
use yield_vm::bytecode::Build;

pub use error::CompileError;
pub use frontend::link::{Dependency, DependencyKind, ModuleGraph};
pub use resolver::{MemoryResolver, ModuleResolver, NodeResolver};
pub use transform::{
    BuildTransform, GraphTransform, ParsedModule, SourceTransform, TransformError,
};

/// A stable, nonempty canonical source module identity.
pub type ModuleId = String;

/// Source roots, resolver and transforms for one build.
pub struct Project<'a> {
    pub entry: ModuleId,
    pub preload: Option<ModuleId>,
    resolver: Box<dyn ModuleResolver + 'a>,
    sources: Vec<Box<dyn SourceTransform + 'a>>,
    graphs: Vec<Box<dyn GraphTransform + 'a>>,
    builds: Vec<Box<dyn BuildTransform + 'a>>,
}

impl<'a> Project<'a> {
    pub fn new(entry: impl Into<ModuleId>, resolver: impl ModuleResolver + 'a) -> Self {
        Self {
            entry: entry.into(),
            preload: None,
            resolver: Box::new(resolver),
            sources: Vec::new(),
            graphs: Vec::new(),
            builds: Vec::new(),
        }
    }

    /// Uses the rooted filesystem and installed Node packages as source modules.
    pub fn from_directory(
        root: &std::path::Path,
        entry: impl Into<ModuleId>,
    ) -> Result<Self, String> {
        Ok(Self::new(entry, NodeResolver::new(root)?))
    }

    pub fn with_preload(mut self, id: impl Into<ModuleId>) -> Self {
        self.preload = Some(id.into());
        self
    }

    pub fn with_source_transform(mut self, transform: impl SourceTransform + 'a) -> Self {
        self.sources.push(Box::new(transform));
        self
    }

    pub fn with_graph_transform(mut self, transform: impl GraphTransform + 'a) -> Self {
        self.graphs.push(Box::new(transform));
        self
    }

    pub fn with_build_transform(mut self, transform: impl BuildTransform + 'a) -> Self {
        self.builds.push(Box::new(transform));
        self
    }
}

#[derive(Debug, Clone)]
pub struct Compiler {
    /// Static host module specifiers and available exports. The build retains
    /// this manifest; the VM resolves it before executing the script.
    pub imports: BTreeMap<String, BTreeSet<String>>,
    /// Emit source sites for synchronous Error stacks (enabled by default).
    pub stack_traces: bool,
    /// Canonical source module IDs whose frames should never be shown.
    pub framework_modules: BTreeSet<String>,
    /// Automatic JSX runtime package (defaults to `react`).
    /// A `.jsx` module using JSX imports the helpers it needs from this package's `/jsx-runtime`.
    pub jsx_runtime: String,
}

impl Default for Compiler {
    fn default() -> Self {
        Self {
            imports: BTreeMap::new(),
            stack_traces: true,
            framework_modules: BTreeSet::new(),
            jsx_runtime: "react".into(),
        }
    }
}

impl Compiler {
    /// Reads the host import manifest from a declaration file.
    ///
    /// Only direct value declarations inside string-named module blocks count;
    /// the parser rejects export lists, aliases and re-exports it cannot read.
    pub fn from_dts(source: &str) -> Result<Self, CompileError> {
        let imports = yield_host_dts::parse(source).map_err(|error| {
            CompileError::new(source, Span::new(error.start, error.end), error.message)
        })?;
        Ok(Self {
            imports,
            ..Self::default()
        })
    }

    /// Runs source transforms, graph transforms, lowering and build transforms.
    pub fn compile(&self, mut project: Project<'_>) -> Result<Build, CompileError> {
        let allocator = Allocator::default();
        let resolver = project.resolver.as_ref();
        let mut graph = frontend::link::link(
            &allocator,
            &project.entry,
            project.preload.as_deref(),
            resolver,
            self,
            &mut project.sources,
        )?;
        for (index, transform) in project.graphs.iter_mut().enumerate() {
            let stage = format!("graph transform {}", index + 1);
            transform.transform(&mut graph).map_err(|error| {
                let module = error.module.as_ref().and_then(|id| graph.module(id));
                error.diagnostic(&stage, module)
            })?;
            graph.relink(resolver, self).map_err(|error| {
                if error.stage().is_some() {
                    error
                } else {
                    error.in_stage(&stage)
                }
            })?;
        }
        let mut build = lower::lower_graph(&graph, self)?;
        for (index, transform) in project.builds.iter_mut().enumerate() {
            let stage = format!("build transform {}", index + 1);
            transform
                .transform(&mut build)
                .map_err(|error| error.diagnostic(&stage, None))?;
            build.finalize();
            build
                .validate()
                .map_err(|error| CompileError::internal(error.to_string()).in_stage(&stage))?;
        }
        Ok(build)
    }
}

fn root_error(request: &str, reason: String) -> CompileError {
    CompileError::internal(format!("source root {request:?}: {reason}")).in_module(request)
}

fn resolved_id(
    resolver: &dyn ModuleResolver,
    referrer: Option<&ModuleId>,
    specifier: &str,
) -> Result<ModuleId, String> {
    let id = resolver.resolve(referrer, specifier)?;
    if id.is_empty() {
        return Err("resolver returned an empty module ID".into());
    }
    Ok(id)
}
