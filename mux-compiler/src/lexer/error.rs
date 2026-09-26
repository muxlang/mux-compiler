//! Error types for the lexer.

use super::span::Span;
use crate::diagnostic::{self, Diagnostic, DiagnosticCode, FileId, ToDiagnostic};

/// An error that occurred during lexical analysis.
#[derive(Debug, Clone, PartialEq)]
pub struct LexerError {
    pub code: DiagnosticCode,
    pub message: Box<str>,
    pub help: Option<Box<str>>,
    pub span: Span,
}

impl LexerError {
    pub fn new(code: DiagnosticCode, message: impl Into<String>, span: Span) -> Self {
        Self {
            code,
            message: message.into().into_boxed_str(),
            help: None,
            span,
        }
    }

    pub fn with_help(
        code: DiagnosticCode,
        message: impl Into<String>,
        span: Span,
        help: impl Into<String>,
    ) -> Self {
        Self {
            code,
            message: message.into().into_boxed_str(),
            help: Some(help.into().into_boxed_str()),
            span,
        }
    }
}

impl ToDiagnostic for LexerError {
    fn to_diagnostic(&self, file_id: FileId) -> Diagnostic {
        diagnostic::diagnostic_from_parts_with_help(
            self.code,
            &self.message,
            self.help.as_deref(),
            self.span,
            file_id,
        )
    }
}

impl std::fmt::Display for LexerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Lexer error at {}:{} - {}",
            self.span.row_start, self.span.col_start, self.message
        )
    }
}

impl std::error::Error for LexerError {}
