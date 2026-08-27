//! Structured source edits attached to diagnostics.
//!
//! Textual help is intentionally not represented by this type. An edit can be
//! consumed by `mux fix` only when the producer has explicitly marked it
//! machine-applicable and the compiler can validate its source range.

use super::{DiagnosticCode, FileId};
use crate::lexer::Span;

/// A byte range in one source file. The range is half-open: `[start, end)`.
///
/// Spans remain row/column based for diagnostics. Byte ranges are calculated
/// only by the fix engine, against the exact source it is about to edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceRange {
    pub start_byte: usize,
    pub end_byte: usize,
}

impl SourceRange {
    pub const fn new(start_byte: usize, end_byte: usize) -> Self {
        Self {
            start_byte,
            end_byte,
        }
    }

    pub const fn is_empty(self) -> bool {
        self.start_byte == self.end_byte
    }
}

/// How safe it is to apply an edit automatically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Applicability {
    /// The compiler proved that applying this edit preserves program meaning.
    MachineApplicable,
    /// The edit is plausible but needs human review.
    MaybeIncorrect,
    /// The edit contains placeholders for the user to fill in.
    HasPlaceholders,
    /// The producer did not provide a safety classification.
    Unspecified,
}

impl Applicability {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MachineApplicable => "machine-applicable",
            Self::MaybeIncorrect => "maybe-incorrect",
            Self::HasPlaceholders => "has-placeholders",
            Self::Unspecified => "unspecified",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditReplacement {
    Text(String),
    Source(Span),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanEdit {
    pub target: Span,
    pub replacement: EditReplacement,
    pub applicability: Applicability,
    pub diagnostic_code: DiagnosticCode,
}

impl SpanEdit {
    pub fn machine_applicable_text(
        target: Span,
        replacement: impl Into<String>,
        diagnostic_code: DiagnosticCode,
    ) -> Self {
        Self {
            target,
            replacement: EditReplacement::Text(replacement.into()),
            applicability: Applicability::MachineApplicable,
            diagnostic_code,
        }
    }

    pub fn machine_applicable_source(
        target: Span,
        source: Span,
        diagnostic_code: DiagnosticCode,
    ) -> Self {
        Self {
            target,
            replacement: EditReplacement::Source(source),
            applicability: Applicability::MachineApplicable,
            diagnostic_code,
        }
    }
}

/// One replacement in a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub file_id: FileId,
    pub range: SourceRange,
    pub replacement: String,
    pub applicability: Applicability,
    pub diagnostic_code: DiagnosticCode,
}

impl TextEdit {
    pub fn machine_applicable(
        file_id: FileId,
        range: SourceRange,
        replacement: impl Into<String>,
        diagnostic_code: DiagnosticCode,
    ) -> Self {
        Self {
            file_id,
            range,
            replacement: replacement.into(),
            applicability: Applicability::MachineApplicable,
            diagnostic_code,
        }
    }

    pub const fn is_machine_applicable(&self) -> bool {
        matches!(self.applicability, Applicability::MachineApplicable)
    }
}
