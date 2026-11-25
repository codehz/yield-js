//! Source-located compile failures.
//!
//! Parser diagnostics are mapped here without exposing Oxc diagnostic types.

use std::fmt;

use oxc::span::Span;

#[derive(Debug, Clone)]
pub struct CompileError {
    module: String,
    message: String,
    stage: Option<String>,
    span: Span,
    line: usize,
    column: usize,
}

impl CompileError {
    pub(crate) fn new(source: &str, span: Span, message: impl Into<String>) -> Self {
        let start = span.start as usize;
        if span == Span::default()
            || span.end < span.start
            || span.end as usize > source.len()
            || !source.is_char_boundary(start)
            || !source.is_char_boundary(span.end as usize)
        {
            return Self::internal(message);
        }
        let prefix = &source[..start];
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
        let column = prefix[line_start..].chars().count() + 1;
        Self {
            module: String::new(),
            message: message.into(),
            stage: None,
            span,
            line,
            column,
        }
    }

    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self {
            module: String::new(),
            message: message.into(),
            stage: None,
            span: Span::default(),
            line: 0,
            column: 0,
        }
    }

    pub(crate) fn in_module(mut self, module: &str) -> Self {
        self.module = module.to_owned();
        self
    }

    pub(crate) fn in_stage(mut self, stage: &str) -> Self {
        self.stage = Some(stage.to_owned());
        self
    }

    pub(crate) fn at_generated_position(mut self, original: String) -> Self {
        self.module = original;
        self.message.insert_str(0, "post-conversion position: ");
        self
    }

    pub fn stage(&self) -> Option<&str> {
        self.stage.as_deref()
    }

    pub fn module(&self) -> &str {
        &self.module
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn span(&self) -> Span {
        self.span
    }

    pub fn line(&self) -> usize {
        self.line
    }

    pub fn column(&self) -> usize {
        self.column
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(stage) = &self.stage {
            write!(formatter, "{stage}: ")?;
        }
        if self.line != 0 {
            write!(
                formatter,
                "{} at {}:{}:{} (byte {})",
                self.message, self.module, self.line, self.column, self.span.start
            )
        } else if !self.module.is_empty() {
            write!(formatter, "{} in {}", self.message, self.module)
        } else {
            formatter.write_str(&self.message)
        }
    }
}

impl std::error::Error for CompileError {}

pub(crate) fn unsupported(source: &str, span: Span, feature: &str) -> CompileError {
    CompileError::new(source, span, format!("unsupported syntax: {feature}"))
}
