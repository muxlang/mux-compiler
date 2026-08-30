use crate::diagnostic::{self, Diagnostic, DiagnosticCode, FileId, SpanEdit, ToDiagnostic};
use crate::lexer::Span;
use crate::semantics::format::format_span_location;

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticError {
    pub code: DiagnosticCode,
    pub message: Box<str>,
    pub help: Option<Box<str>>,
    pub span: Span,
    pub file_id: Option<FileId>,
    pub span_edits: Option<Box<[SpanEdit]>>,
}

impl SemanticError {
    pub fn new(code: DiagnosticCode, message: impl Into<String>, span: Span) -> Self {
        Self {
            code,
            message: message.into().into_boxed_str(),
            help: None,
            span,
            file_id: None,
            span_edits: None,
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
            file_id: None,
            span_edits: None,
        }
    }

    #[must_use]
    pub fn with_span_edit(mut self, edit: SpanEdit) -> Self {
        let mut edits = self
            .span_edits
            .take()
            .map_or_else(Vec::new, <[SpanEdit]>::into_vec);
        edits.push(edit);
        self.span_edits = Some(edits.into_boxed_slice());
        self
    }
}

impl ToDiagnostic for SemanticError {
    fn to_diagnostic(&self, file_id: FileId) -> Diagnostic {
        let diagnostic = diagnostic::diagnostic_from_parts_with_help(
            self.code,
            &self.message,
            self.help.as_deref(),
            self.span,
            self.file_id.unwrap_or(file_id),
        );
        match &self.span_edits {
            Some(edits) => diagnostic.with_span_edits(edits.iter().cloned()),
            None => diagnostic,
        }
    }
}

impl std::fmt::Display for SemanticError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Semantic error at {}: {}",
            format_span_location(&self.span),
            self.message
        )
    }
}

impl std::error::Error for SemanticError {}
