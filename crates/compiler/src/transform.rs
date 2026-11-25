//! Public source, graph and build transform contracts.

use std::ops::Range;

use oxc::{allocator::Allocator, ast::ast::Program, span::Span};
use yield_vm::bytecode::Build;

use crate::{CompileError, ModuleGraph, ModuleId};

/// A canonical source module with a compiler-owned parsed program.
pub struct ParsedModule<'a> {
    pub(crate) id: ModuleId,
    pub(crate) allocator: &'a Allocator,
    pub(crate) program: Program<'a>,
}

impl<'a> ParsedModule<'a> {
    pub fn id(&self) -> &ModuleId {
        &self.id
    }

    pub fn source(&self) -> &'a str {
        self.program.source_text
    }

    /// Replaces the module's source and reparses it before the next stage.
    pub fn replace_source(&mut self, source: &str) -> Result<(), CompileError> {
        let program = crate::frontend::parse(self.allocator, self.allocator.alloc_str(source))
            .map_err(|error| error.in_module(&self.id))?;
        self.program = program;
        Ok(())
    }
}

pub trait SourceTransform {
    fn transform(&mut self, module: &mut ParsedModule<'_>) -> Result<(), TransformError>;
}

pub trait GraphTransform {
    fn transform(&mut self, graph: &mut ModuleGraph<'_>) -> Result<(), TransformError>;
}

pub trait BuildTransform {
    fn transform(&mut self, build: &mut Build) -> Result<(), TransformError>;
}

/// A transform failure. Graph transforms can identify the module owning a span.
#[derive(Debug, Clone)]
pub struct TransformError {
    pub message: String,
    pub span: Option<Range<u32>>,
    pub module: Option<ModuleId>,
    parse_error: Option<Box<CompileError>>,
}

impl TransformError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            span: None,
            module: None,
            parse_error: None,
        }
    }

    pub fn at(mut self, span: Range<u32>) -> Self {
        self.span = Some(span);
        self
    }

    pub fn in_module(mut self, module: impl Into<ModuleId>) -> Self {
        self.module = Some(module.into());
        self
    }

    pub(crate) fn diagnostic(self, stage: &str, module: Option<&ParsedModule<'_>>) -> CompileError {
        if let Some(error) = self.parse_error {
            return error.in_stage(stage);
        }
        let error = match (module, self.span) {
            (Some(module), Some(span)) => CompileError::new(
                module.source(),
                Span::new(span.start, span.end),
                self.message,
            ),
            _ => CompileError::internal(self.message),
        };
        let error = error.in_stage(stage);
        match module
            .map(|module| module.id().as_str())
            .or(self.module.as_deref())
        {
            Some(id) => error.in_module(id),
            None => error,
        }
    }
}

impl From<CompileError> for TransformError {
    fn from(error: CompileError) -> Self {
        Self {
            message: error.message().to_owned(),
            span: None,
            module: None,
            parse_error: Some(Box::new(error)),
        }
    }
}
