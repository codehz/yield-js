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
//!   → frontend   parse, top-level function declarations
//!   → lower      bindings, exceptions, await  →  LoweredFunction
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
mod session;

use oxc::allocator::Allocator;
use yield_vm::bytecode::Build;

pub use error::CompileError;

#[derive(Debug, Clone, Default)]
pub struct CompileOptions {
    /// Host bindings declared by the build. Missing bindings are rejected by
    /// the VM before any script code runs.
    pub imports: Vec<String>,
}

/// Compiles a strict-mode JavaScript source bundle into a versioned bytecode
/// build. The source is parsed as an ES module only to guarantee strict-mode
/// parsing; module declarations are rejected by the subset compiler.
pub fn compile(source: &str, options: &CompileOptions) -> Result<Build, CompileError> {
    let allocator = Allocator::default();
    let program = frontend::parse(&allocator, source)?;
    let top_level = frontend::collect_top_level(source, &program)?;
    let mut session = session::Session::new(
        source,
        options,
        top_level.names,
        top_level.functions.len() as u32,
    )?;
    lower::lower_all(&mut session, top_level.functions)?;
    session.finish()
}
