//! Strict-mode JavaScript subset compiler for the resumable VM.
//!
//! Oxc parses the source. This crate then lowers the supported subset to its
//! own IR — logical-register bytecode plus await and exception side tables —
//! analyzes liveness, and allocates frame slots. The VM bytecode contract is
//! produced only by that last pass. Oxc types do not escape this crate except
//! as the byte [`oxc::span::Span`] on [`CompileError`].
//!
//! ```text
//! source roots and resolver
//!   → frontend   parse and link reachable modules
//!   → lower      module declaration and evaluation phases, then nested functions
//!   → bindings   owner table; cross-function reads and writes stay shared
//!   → promote    unshared bindings become logical registers
//!   → analyze    CFG live-in, including resume and handler edges
//!   → allocate   persistent slots for cross-await values, transient otherwise
//!   → Build
//! ```

mod allocate;
mod analyze;
mod bindings;
mod emit;
mod env;
mod error;
mod frontend;
mod instr;
mod ir;
mod lower;
mod names;
mod promote;
mod resolver;
mod session;

use std::collections::{BTreeMap, BTreeSet};

use oxc::allocator::Allocator;
use yield_vm::bytecode::Build;

pub use error::CompileError;
pub use resolver::{FileResolver, MemoryResolver, ModuleResolver};

#[derive(Debug, Clone, Default)]
pub struct CompileOptions {
    /// Static host module specifiers and available exports. The build retains
    /// this manifest; the VM resolves it before executing the script.
    pub imports: BTreeMap<String, BTreeSet<String>>,
}

impl CompileOptions {
    /// Builds the host import manifest from binding declarations.
    pub fn from_bindings(bindings: &yield_vm::Bindings) -> Self {
        Self {
            imports: bindings
                .module_names()
                .map(|module| {
                    (
                        module.to_owned(),
                        bindings.export_names(module).map(str::to_owned).collect(),
                    )
                })
                .collect(),
        }
    }
}

/// Compiles explicit source roots with a synchronous source resolver.
pub fn compile(
    entry: &str,
    preload: Option<&str>,
    resolver: &dyn ModuleResolver,
    options: &CompileOptions,
) -> Result<Build, CompileError> {
    let allocator = Allocator::default();
    let graph = frontend::link::link(&allocator, entry, preload, resolver, options)?;
    lower::lower_graph(&graph, options)
}

fn root_error(request: &str, reason: String) -> CompileError {
    CompileError::internal(format!("source root {request:?}: {reason}")).in_module(request)
}

fn resolved_id(
    resolver: &dyn ModuleResolver,
    referrer: Option<&str>,
    specifier: &str,
) -> Result<String, String> {
    let id = resolver.resolve(referrer, specifier)?;
    if id.is_empty() {
        return Err("resolver returned an empty module ID".into());
    }
    Ok(id)
}
