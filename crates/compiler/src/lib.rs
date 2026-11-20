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
mod session;

use std::collections::{BTreeMap, BTreeSet};

use oxc::allocator::Allocator;
use yield_vm::bytecode::Build;

pub use error::CompileError;

#[derive(Debug, Clone, Default)]
pub struct CompileOptions {
    /// Static host module specifiers and available exports. The build retains
    /// this manifest; the VM resolves it before executing the script.
    pub imports: BTreeMap<String, BTreeSet<String>>,
}

/// Compiles a strict-mode JavaScript program into a versioned bytecode build.
/// Parsing uses ES module grammar so strict syntax is rejected up front.
/// Evaluation is a synthetic script (function id 0). Direct function
/// declarations become entries in that script's environment.
pub fn compile(source: &str, options: &CompileOptions) -> Result<Build, CompileError> {
    let allocator = Allocator::default();
    let program = frontend::parse(&allocator, source)?;
    let mut session = session::Session::new(source, options);
    lower::lower_program(&mut session, &program)?;
    session.finish()
}
