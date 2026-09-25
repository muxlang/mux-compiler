//! Source formatter and file discovery for Mux programs.

mod files;
mod print;

pub use files::{FormatOutcome, format_paths};

/// Formatting policy. The width controls optional wrapping; it does not force
/// otherwise valid source to wrap when doing so would make layout ambiguous.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatOptions {
    pub indent_width: usize,
    pub line_width: usize,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            indent_width: 4,
            line_width: 80,
        }
    }
}

/// An input, parsing, or file operation error from formatting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatError {
    message: String,
}

impl FormatError {
    pub(super) fn input(message: impl Into<String>) -> Self {
        Self {
            message: format!("mux format: {}", message.into()),
        }
    }

    pub(super) fn parse(message: impl Into<String>) -> Self {
        Self {
            message: format!("mux format: {}", message.into()),
        }
    }

    pub(super) fn io(message: impl Into<String>) -> Self {
        Self {
            message: format!("mux format: {}", message.into()),
        }
    }

    fn with_path(mut self, path: &std::path::Path) -> Self {
        self.message = format!("{}: {}", path.display(), self.message);
        self
    }
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for FormatError {}

/// Format source using the default four-space indentation and 80-column width.
pub fn format_source(source: &str) -> Result<String, FormatError> {
    format_source_with_options(source, FormatOptions::default())
}

/// Format source using caller-supplied indentation and line width.
pub fn format_source_with_options(
    source: &str,
    options: FormatOptions,
) -> Result<String, FormatError> {
    print::format_source(source, options)
}
