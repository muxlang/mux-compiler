//! Diagnostic system for error reporting and formatting.
//!
//! Provides centralized diagnostics inspired by Rust's error formatting.
//! Supports color-coded output, multi-line span highlighting, and grouped error reporting.

pub mod catalog;
mod edit;
mod emitter;
mod files;
pub mod fix;
mod styles;

pub use catalog::DiagnosticCode;
pub use edit::{Applicability, EditReplacement, SourceRange, SpanEdit, TextEdit};
pub use emitter::{DiagnosticEmitter, StandardEmitter};
pub use files::{FileId, Files};
pub use styles::{ColorConfig, Styles};

use crate::lexer::Span;

/// Maximum number of diagnostics presented for one analysis.
pub const MAX_DIAGNOSTICS: usize = 100;

/// The severity level of a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error,
    Warning,
}

/// The style of a label (primary or secondary).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum LabelStyle {
    Primary,
    Secondary,
}

/// A label that points to a specific span in the source code.
#[derive(Debug, Clone)]
pub struct Label {
    pub span: Span,
    pub message: Option<String>,
    pub style: LabelStyle,
}

impl Label {
    pub fn primary(span: Span, message: impl Into<String>) -> Self {
        let msg = message.into();
        Self {
            span,
            message: if msg.is_empty() { None } else { Some(msg) },
            style: LabelStyle::Primary,
        }
    }
}

/// A diagnostic message with associated labels and help text.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub level: Level,
    pub message: String,
    pub labels: Vec<Label>,
    pub help: Option<String>,
    pub file_id: Option<FileId>,
    pub edits: Vec<TextEdit>,
    pub span_edits: Vec<SpanEdit>,
}

impl Diagnostic {
    pub fn new(code: DiagnosticCode) -> Self {
        Self {
            code,
            level: code.level(),
            message: String::new(),
            labels: Vec::new(),
            help: None,
            file_id: None,
            edits: Vec::new(),
            span_edits: Vec::new(),
        }
    }

    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = message.into();
        self
    }

    pub fn with_label(mut self, label: Label) -> Self {
        self.labels.push(label);
        self
    }

    pub fn with_help(mut self, help: Option<impl Into<String>>) -> Self {
        self.help = help.map(|h| h.into());
        self
    }

    pub fn with_file_id(mut self, file_id: FileId) -> Self {
        self.file_id = Some(file_id);
        self
    }

    pub fn with_span_edit(mut self, edit: SpanEdit) -> Self {
        self.span_edits.push(edit);
        self
    }

    pub fn with_span_edits(mut self, edits: impl IntoIterator<Item = SpanEdit>) -> Self {
        self.span_edits.extend(edits);
        self
    }
}

/// Trait for types that can be converted to a diagnostic.
pub trait ToDiagnostic {
    fn to_diagnostic(&self, file_id: FileId) -> Diagnostic;
}

pub fn diagnostic_from_parts_with_help(
    code: DiagnosticCode,
    message: &str,
    help: Option<&str>,
    span: Span,
    file_id: FileId,
) -> Diagnostic {
    Diagnostic::new(code)
        .with_message(message)
        .with_label(Label::primary(span, ""))
        .with_help(help.map(str::to_owned))
        .with_file_id(file_id)
}
