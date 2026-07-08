//! Styling for diagnostic output, built on anstyle.
//!
//! Styles always render ANSI escape codes. The emitter writes through
//! anstream, which strips the codes when stderr is not a terminal or when
//! NO_COLOR is set, so no manual TTY detection is needed here.

use anstyle::{AnsiColor, Style};

const ERROR: Style = AnsiColor::Red.on_default().bold();
const WARNING: Style = AnsiColor::Yellow.on_default().bold();
const NOTE: Style = AnsiColor::Cyan.on_default().bold();
const HELP: Style = AnsiColor::Cyan.on_default().bold();
const BOLD: Style = Style::new().bold();
const LOCATION: Style = AnsiColor::Cyan.on_default();
const PRIMARY_LABEL: Style = AnsiColor::Red.on_default();
const SECONDARY_LABEL: Style = AnsiColor::Blue.on_default();
const LINE_NUMBER: Style = AnsiColor::Blue.on_default().bold();

/// Configuration for color output.
///
/// Color stripping is handled by anstream at write time, so `Auto` is the
/// only mode: emit codes and let the stream decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorConfig {
    Auto,
}

/// Renders text wrapped in ANSI style codes for diagnostic output.
pub struct Styles;

impl Styles {
    pub fn new(_config: ColorConfig) -> Self {
        Self
    }

    fn styled(style: Style, text: &str) -> String {
        format!("{style}{text}{style:#}")
    }

    pub fn error(&self, text: &str) -> String {
        Self::styled(ERROR, text)
    }

    pub fn warning(&self, text: &str) -> String {
        Self::styled(WARNING, text)
    }

    pub fn note(&self, text: &str) -> String {
        Self::styled(NOTE, text)
    }

    pub fn help(&self, text: &str) -> String {
        Self::styled(HELP, text)
    }

    pub fn bold(&self, text: &str) -> String {
        Self::styled(BOLD, text)
    }

    pub fn location(&self, text: &str) -> String {
        Self::styled(LOCATION, text)
    }

    pub fn primary_label(&self, text: &str) -> String {
        Self::styled(PRIMARY_LABEL, text)
    }

    pub fn secondary_label(&self, text: &str) -> String {
        Self::styled(SECONDARY_LABEL, text)
    }

    pub fn line_number(&self, text: &str) -> String {
        Self::styled(LINE_NUMBER, text)
    }
}

impl Default for Styles {
    fn default() -> Self {
        Self::new(ColorConfig::Auto)
    }
}
