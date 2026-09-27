//! Source formatter and file discovery for Mux programs.

mod files;
mod print;

pub use files::{FormatOutcome, format_paths, format_paths_with_options};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndentType {
    /// Use spaces for each indentation level.
    Space,
    /// Use tab characters for each indentation level.
    Tab,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BraceStyle {
    /// Put a block's opening brace on the declaration or control line.
    SameLine,
    /// Put a block's opening brace on the next line.
    NextLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WherePosition {
    /// Put a `where` clause on its own line.
    OwnLine,
    /// Keep a `where` clause on the declaration line.
    SameLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrailingComma {
    /// Include a trailing comma when the collection or match arms wrap.
    Multiline,
    /// Omit a trailing comma.
    Never,
    /// Include a trailing comma whenever the syntax permits one.
    Always,
}

/// Formatting policy. The width controls optional wrapping; it does not force
/// otherwise valid source to wrap when doing so would make layout ambiguous.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatOptions {
    /// Use spaces or tabs for indentation.
    pub indent_type: IndentType,
    /// Number of indentation units per nesting level.
    pub indent_count: usize,
    /// Preferred maximum line width for wrapping.
    pub line_width: usize,
    /// Placement of block opening braces.
    pub brace_style: BraceStyle,
    /// Placement of `where` clauses.
    pub where_position: WherePosition,
    /// Empty lines between top-level declarations.
    pub blank_lines_between_declarations: usize,
    /// Empty lines between ordinary members in class and type bodies.
    pub blank_lines_between_members: usize,
    /// Empty lines before function members.
    pub blank_lines_before_functions: usize,
    /// Policy for trailing commas where the grammar supports them.
    pub trailing_comma: TrailingComma,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            indent_type: IndentType::Space,
            indent_count: 4,
            line_width: 80,
            brace_style: BraceStyle::SameLine,
            where_position: WherePosition::OwnLine,
            blank_lines_between_declarations: 1,
            blank_lines_between_members: 0,
            blank_lines_before_functions: 1,
            trailing_comma: TrailingComma::Multiline,
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

/// Format source using the default Mux formatter settings.
pub fn format_source(source: &str) -> Result<String, FormatError> {
    format_source_with_options(source, FormatOptions::default())
}

/// Format source using caller-supplied formatting options.
pub fn format_source_with_options(
    source: &str,
    options: FormatOptions,
) -> Result<String, FormatError> {
    print::format_source(source, options)
}
