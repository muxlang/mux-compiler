//! Parser error types.

use crate::ast::ParseError;
use crate::diagnostic::{self, Diagnostic, DiagnosticCode, FileId, ToDiagnostic};
use crate::lexer::{Span, Token};

/// The result type for parser operations.
pub type ParserResult<T> = Result<T, ParserError>;

/// An error that occurred during parsing.
#[derive(Debug, Clone, PartialEq)]
pub struct ParserError {
    pub code: DiagnosticCode,
    pub message: String,
    pub help: Option<String>,
    pub span: Span,
}

impl ParserError {
    pub fn new(code: DiagnosticCode, message: impl Into<String>, span: Span) -> Self {
        Self {
            code,
            message: message.into(),
            help: None,
            span,
        }
    }

    pub fn from_token(code: DiagnosticCode, message: impl Into<String>, token: &Token) -> Self {
        Self {
            code,
            message: message.into(),
            help: None,
            span: token.span,
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
            message: message.into(),
            help: Some(help.into()),
            span,
        }
    }
}

impl ToDiagnostic for ParserError {
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

impl std::fmt::Display for ParserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Parser error at {}:{} - {}",
            self.span.row_start, self.span.col_start, self.message
        )
    }
}

impl std::error::Error for ParserError {}

impl From<ParseError> for ParserError {
    fn from(err: ParseError) -> Self {
        Self {
            code: DiagnosticCode::ParseExpectedToken,
            message: err.message,
            help: None,
            span: err.span,
        }
    }
}
