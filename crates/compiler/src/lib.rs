//! Strict-mode JavaScript subset compiler for the resumable VM.
//!
//! Oxc parses the source. This crate then lowers the supported subset to its
//! own IR — logical-register bytecode plus await and exception side tables —
//! analyzes liveness, and allocates frame slots. The VM bytecode contract is
//! produced only by that last pass. Oxc types do not escape this crate except
//! as the byte [`oxc::span::Span`] on [`CompileError`].
//!
//! ```text
//! source
//!   → frontend   parse
//!   → lower      script function 0, then nested functions
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
    let root = |specifier: &str| {
        let result = if specifier.is_empty() {
            Err("empty source root".into())
        } else {
            resolved_id(resolver, None, specifier)
        };
        result.map_err(|reason| root_error(specifier, reason))
    };
    let preload_id = preload.map(root).transpose()?;
    let entry_id = root(entry)?;
    let preload_source = preload_id
        .as_deref()
        .zip(preload)
        .map(|(id, request)| {
            resolver
                .load(id)
                .map_err(|reason| root_error(request, reason))
        })
        .transpose()?;
    let entry_source = match (&preload_source, preload_id.as_deref()) {
        (Some(source), Some(id)) if id == entry_id => std::borrow::Cow::Borrowed(source.as_ref()),
        _ => resolver
            .load(&entry_id)
            .map_err(|reason| root_error(entry, reason))?,
    };
    let loaded_roots: Vec<_> = preload_id
        .as_deref()
        .into_iter()
        .chain([entry_id.as_str()])
        .collect();
    let allocator = Allocator::default();
    let mut session =
        session::Session::new(preload_source.as_deref().unwrap_or(&entry_source), options);
    if let Some((id, source)) = preload_id.as_deref().zip(preload_source.as_deref()) {
        (|| {
            let initializer = session.alloc_func_id(oxc::span::Span::default())?;
            let program = frontend::parse(&allocator, source)?;
            frontend::check_dependencies(&program, id, source, resolver, options, &loaded_roots)?;
            lower::lower_program(&mut session, &program, initializer, "\0preload")?;
            session.finish_preload(initializer, &entry_source)
        })()
        .map_err(|err: CompileError| err.in_module(id))?;
    }
    (|| {
        let program = frontend::parse(&allocator, &entry_source)?;
        frontend::check_dependencies(
            &program,
            &entry_id,
            &entry_source,
            resolver,
            options,
            &loaded_roots,
        )?;
        lower::lower_program(&mut session, &program, 0, "\0script")?;
        session.finish()
    })()
    .map_err(|err: CompileError| err.in_module(&entry_id))
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
