//! Source-located compile failures.
//!
//! The only Oxc type on the public error surface is [`Span`], a byte range.
//! Parser diagnostics are mapped here so the rest of the compiler does not
//! depend on Oxc's diagnostic types.

use std::fmt;

use oxc::span::Span;

#[derive(Debug, Clone)]
pub struct CompileError {
    message: String,
    span: Span,
    line: usize,
    column: usize,
}

impl CompileError {
    pub(crate) fn new(source: &str, span: Span, message: impl Into<String>) -> Self {
        let start = (span.start as usize).min(source.len());
        let prefix = &source[..start];
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
        let column = prefix[line_start..].chars().count() + 1;
        Self {
            message: message.into(),
            span,
            line,
            column,
        }
    }

    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self::new("", Span::default(), message)
    }

    pub(crate) fn in_preload(mut self) -> Self {
        self.message.insert_str(0, "preload: ");
        self
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
        write!(
            formatter,
            "{} at {}:{} (byte {})",
            self.message, self.line, self.column, self.span.start
        )
    }
}

impl std::error::Error for CompileError {}

pub(crate) fn unsupported(source: &str, span: Span, feature: &str) -> CompileError {
    CompileError::new(source, span, format!("unsupported syntax: {feature}"))
}
