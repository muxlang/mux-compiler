//! Styling for diagnostic output, built on anstyle.
//!
//! Styles always render ANSI escape codes. The emitter writes through
//! anstream, which strips the codes when stderr is not a terminal or when
//! NO_COLOR is set, so no manual TTY detection is needed here.

use anstyle::{AnsiColor, Color, RgbColor, Style};

const ERROR: Style = AnsiColor::Red.on_default().bold();
const WARNING: Style = AnsiColor::Yellow.on_default().bold();
const NOTE: Style = AnsiColor::Cyan.on_default().bold();
const HELP: Style = AnsiColor::Cyan.on_default().bold();
const BOLD: Style = Style::new().bold();
const LOCATION: Style = AnsiColor::Cyan.on_default();
const PRIMARY_LABEL: Style = AnsiColor::Red.on_default();
const SECONDARY_LABEL: Style = Style::new().fg_color(Some(Color::Rgb(RgbColor(0x60, 0xa5, 0xfa))));
const LINE_NUMBER: Style = Style::new()
    .fg_color(Some(Color::Rgb(RgbColor(0x60, 0xa5, 0xfa))))
    .bold();

/// Configuration for color output.
///
/// Color stripping is handled by anstream at write time, so `Auto` is the
/// only mode: emit codes and let the stream decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorConfig {
    Auto,
}

/// Renders text wrapped in ANSI style codes for diagnostic output.
#[derive(Default)]
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

#[cfg(test)]
mod tests {
    use super::*;

    // The rendered escape codes are asserted literally: they are part of the
    // CLI's visual contract (anstream strips them for non-TTY consumers).
    #[test]
    fn severity_styles_render_bold_colored_text() {
        let styles = Styles;
        assert_eq!(styles.error("e"), "\u{1b}[1m\u{1b}[31me\u{1b}[0m");
        assert_eq!(styles.warning("w"), "\u{1b}[1m\u{1b}[33mw\u{1b}[0m");
        assert_eq!(styles.note("n"), "\u{1b}[1m\u{1b}[36mn\u{1b}[0m");
        assert_eq!(styles.help("h"), "\u{1b}[1m\u{1b}[36mh\u{1b}[0m");
    }

    #[test]
    fn structural_styles_render_expected_codes() {
        let styles = Styles::new(ColorConfig::Auto);
        assert_eq!(styles.bold("b"), "\u{1b}[1mb\u{1b}[0m");
        assert_eq!(styles.location("--> f:1:2"), "\u{1b}[36m--> f:1:2\u{1b}[0m");
        assert_eq!(styles.primary_label("^^^"), "\u{1b}[31m^^^\u{1b}[0m");
        assert_eq!(
            styles.secondary_label("---"),
            "\u{1b}[38;2;96;165;250m---\u{1b}[0m"
        );
        assert_eq!(
            styles.line_number("12"),
            "\u{1b}[1m\u{1b}[38;2;96;165;250m12\u{1b}[0m"
        );
    }
}
