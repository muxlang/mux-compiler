//! Diagnostic emitter for formatted error output.

use super::{
    ColorConfig, Diagnostic, DiagnosticCode, Files, LabelStyle, Level, MAX_DIAGNOSTICS, Styles,
};
use crate::lexer::Span;
use anstream::eprintln;
use std::cmp::{max, min};
use unicode_width::UnicodeWidthChar;

struct SourceLine<'a> {
    content: &'a str,
    start_byte: usize,
    end_byte: usize,
}

fn source_lines(source: &str) -> Vec<SourceLine<'_>> {
    let mut lines = Vec::new();
    let mut start_byte = 0;
    for (newline, _) in source.match_indices('\n') {
        let mut end_byte = newline;
        if end_byte > start_byte && source.as_bytes()[end_byte - 1] == b'\r' {
            end_byte -= 1;
        }
        lines.push(SourceLine {
            content: &source[start_byte..end_byte],
            start_byte,
            end_byte,
        });
        start_byte = newline + 1;
    }
    lines.push(SourceLine {
        content: &source[start_byte..],
        start_byte,
        end_byte: source.len(),
    });
    lines
}

fn display_width(text: &str) -> usize {
    text.chars()
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(1))
        .sum()
}

/// Keep noisy recovery output useful without hiding the fact that more
/// diagnostics existed.
/// Trait for emitting diagnostics to output.
pub trait DiagnosticEmitter {
    fn emit(&self, diagnostic: &Diagnostic, files: &Files);
    fn emit_batch(&self, diagnostics: &[Diagnostic], files: &Files);
}

/// Standard diagnostic emitter with Rust-style formatting.
pub struct StandardEmitter {
    pub styles: Styles,
}

impl StandardEmitter {
    #[must_use]
    pub fn new(config: ColorConfig) -> Self {
        Self {
            styles: Styles::new(config),
        }
    }

    /// Get the line number width for proper alignment.
    fn line_number_width(max_line: usize) -> usize {
        max_line.to_string().len().max(2)
    }

    /// Render a single line of source with line number.
    fn render_source_line(&self, line_number: usize, line_content: &str, width: usize) -> String {
        let line_num_str = self.styles.line_number(&format!("{line_number:width$}"));
        let line_content = line_content.trim_end();
        if line_content.is_empty() {
            format!("{line_num_str} |")
        } else {
            format!("{line_num_str} | {line_content}")
        }
    }

    /// Render the gutter (line number column) without source.
    fn render_gutter(width: usize) -> String {
        format!("{:width$} |", "", width = width)
    }

    /// Render underline/caret indicators for a label.
    fn render_label_underline(
        &self,
        span: &Span,
        line_number: usize,
        line: &SourceLine<'_>,
        style: LabelStyle,
        width: usize,
    ) -> String {
        let gutter = Self::render_gutter(width);

        let (start_col, end_col) = if let Some(range) = span.byte_range {
            let start_byte = range.start.max(line.start_byte).min(line.end_byte);
            let end_byte = range.end.max(line.start_byte).min(line.end_byte);
            let start = start_byte.saturating_sub(line.start_byte);
            let end = end_byte.saturating_sub(line.start_byte);
            (
                display_width(&line.content[..start]),
                display_width(&line.content[..end]),
            )
        } else {
            let start = if span.row_start == line_number {
                span.col_start.saturating_sub(1)
            } else {
                0
            };
            let end = if let Some(end_line) = span.row_end {
                if end_line == line_number {
                    span.col_end.unwrap_or(span.col_start).saturating_sub(1)
                } else {
                    display_width(line.content)
                }
            } else {
                start
            };
            (start, end)
        };

        let underline_len = (end_col.saturating_sub(start_col)).max(1);
        let indicator = "^".repeat(underline_len);
        let colored_indicator = match style {
            LabelStyle::Primary => self.styles.primary_label(&indicator),
            LabelStyle::Secondary => self.styles.secondary_label(&indicator),
        };

        format!("{} {}{}", gutter, " ".repeat(start_col), colored_indicator)
    }

    /// Emit a single diagnostic with source context.
    fn emit_single(&self, diagnostic: &Diagnostic, source: &str, file_path: &str) {
        self.emit_header(diagnostic);

        if diagnostic.labels.is_empty() {
            eprintln!();
            return;
        }

        let lines = source_lines(source);
        let (min_line, max_line) = Self::label_line_range(diagnostic, &lines);
        let width = Self::line_number_width(max_line);

        self.emit_file_location(diagnostic, file_path, &lines, min_line, width);
        for line_num in min_line..=max_line {
            self.emit_source_context_line(diagnostic, &lines, line_num, width);
        }

        self.emit_help(diagnostic, width);

        eprintln!();
    }

    fn emit_header(&self, diagnostic: &Diagnostic) {
        let level_str = match diagnostic.level {
            Level::Error => self.styles.error("error"),
            Level::Warning => self.styles.warning("warning"),
        };

        eprintln!(
            "{}[{}]: {}",
            level_str,
            diagnostic.code,
            self.styles.bold(&diagnostic.message)
        );
    }

    fn label_line_range(diagnostic: &Diagnostic, lines: &[SourceLine<'_>]) -> (usize, usize) {
        let mut min_line = usize::MAX;
        let mut max_line = 0;

        for label in &diagnostic.labels {
            let (start, end) = Self::span_line_range(&label.span, lines);
            min_line = min(min_line, start);
            max_line = max(max_line, end);
        }

        (min_line, max_line)
    }

    fn line_index_at(lines: &[SourceLine<'_>], byte: usize) -> usize {
        lines
            .partition_point(|line| line.start_byte <= byte)
            .saturating_sub(1)
            .min(lines.len().saturating_sub(1))
    }

    fn span_line_range(span: &Span, lines: &[SourceLine<'_>]) -> (usize, usize) {
        let Some(range) = span.byte_range else {
            return (span.row_start, span.row_end.unwrap_or(span.row_start));
        };
        let start = Self::line_index_at(lines, range.start);
        let end_byte = if range.end > range.start {
            range.end - 1
        } else {
            range.end
        };
        let end = Self::line_index_at(lines, end_byte);
        (start + 1, end + 1)
    }

    fn span_start_column(span: &Span, lines: &[SourceLine<'_>]) -> usize {
        let Some(range) = span.byte_range else {
            return span.col_start;
        };
        let line = &lines[Self::line_index_at(lines, range.start)];
        let byte = range
            .start
            .saturating_sub(line.start_byte)
            .min(line.content.len());
        display_width(&line.content[..byte]) + 1
    }

    fn emit_file_location(
        &self,
        diagnostic: &Diagnostic,
        file_path: &str,
        lines: &[SourceLine<'_>],
        min_line: usize,
        width: usize,
    ) {
        let column = diagnostic
            .labels
            .first()
            .map_or(1, |label| Self::span_start_column(&label.span, lines));
        let location = format!("--> {}:{}:{}", file_path, min_line, column);
        eprintln!("{}", self.styles.location(&location));
        eprintln!("{}", Self::render_gutter(width));
    }

    fn label_covers_line(line_num: usize, span: &Span, lines: &[SourceLine<'_>]) -> bool {
        let (start, end) = Self::span_line_range(span, lines);
        (start..=end).contains(&line_num)
    }

    fn emit_label_message(&self, label: &super::Label, width: usize) {
        if let Some(ref msg) = label.message {
            let colored_msg = match label.style {
                LabelStyle::Primary => self.styles.primary_label(msg),
                LabelStyle::Secondary => self.styles.secondary_label(msg),
            };
            eprintln!("{} {}", Self::render_gutter(width), colored_msg);
        }
    }

    fn emit_source_context_line(
        &self,
        diagnostic: &Diagnostic,
        lines: &[SourceLine<'_>],
        line_num: usize,
        width: usize,
    ) {
        let line_idx = line_num.saturating_sub(1);
        if line_idx >= lines.len() {
            return;
        }

        let line = &lines[line_idx];
        let line_content = line.content;
        eprintln!("{}", self.render_source_line(line_num, line_content, width));

        for label in &diagnostic.labels {
            if !Self::label_covers_line(line_num, &label.span, lines) {
                continue;
            }

            eprintln!(
                "{}",
                self.render_label_underline(&label.span, line_num, line, label.style, width)
            );
            self.emit_label_message(label, width);
        }
    }

    fn emit_help(&self, diagnostic: &Diagnostic, width: usize) {
        if let Some(ref help) = diagnostic.help {
            eprintln!("{}", Self::render_gutter(width));
            eprintln!(
                "{} {} {}",
                self.styles.line_number("="),
                self.styles.help("help:"),
                help
            );
        }
    }

    fn emit_invalid_provenance(&self, reason: &str) {
        let diagnostic = Diagnostic::new(DiagnosticCode::InternalCompiler).with_message(format!(
            "internal compiler error while rendering a diagnostic: {reason}"
        ));
        self.emit_header(&diagnostic);
        eprintln!(
            "{} {}",
            self.styles.line_number("="),
            self.styles
                .help("help: please report this compiler error to the Mux maintainers")
        );
        eprintln!();
    }

    fn ordered_diagnostics<'a>(
        diagnostics: &'a [Diagnostic],
        files: &Files,
    ) -> Vec<&'a Diagnostic> {
        let mut ordered: Vec<&Diagnostic> = diagnostics.iter().collect();
        ordered.sort_by_key(|diagnostic| super::sort_key(diagnostic, files));
        ordered
    }
}

impl Default for StandardEmitter {
    fn default() -> Self {
        Self::new(ColorConfig::Auto)
    }
}

impl DiagnosticEmitter for StandardEmitter {
    fn emit(&self, diagnostic: &Diagnostic, files: &Files) {
        let Some(file_id) = diagnostic.file_id else {
            self.emit_invalid_provenance("diagnostic has no source file");
            return;
        };
        let Some(file_info) = files.get(file_id) else {
            self.emit_invalid_provenance("diagnostic refers to an unknown source file");
            return;
        };
        let file_path = file_info.path.to_string_lossy();
        let source = &file_info.source;

        self.emit_single(diagnostic, source, &file_path);
    }

    fn emit_batch(&self, diagnostics: &[Diagnostic], files: &Files) {
        let ordered = Self::ordered_diagnostics(diagnostics, files);

        let error_count = ordered.iter().filter(|d| d.level == Level::Error).count();
        let warning_count = ordered.iter().filter(|d| d.level == Level::Warning).count();

        if error_count > 0 {
            eprintln!(
                "{}: {} error{} found\n",
                self.styles.error("error"),
                error_count,
                if error_count == 1 { "" } else { "s" }
            );
        }
        if warning_count > 0 {
            eprintln!(
                "{}: {} warning{}\n",
                self.styles.warning("warning"),
                warning_count,
                if warning_count == 1 { "" } else { "s" }
            );
        }

        for diagnostic in ordered.iter().take(MAX_DIAGNOSTICS) {
            let Some(file_id) = diagnostic.file_id else {
                self.emit_invalid_provenance("diagnostic has no source file");
                continue;
            };
            let Some(file_info) = files.get(file_id) else {
                self.emit_invalid_provenance("diagnostic refers to an unknown source file");
                continue;
            };
            let file_path = file_info.path.to_string_lossy();
            self.emit_single(diagnostic, &file_info.source, &file_path);
        }

        if ordered.len() > MAX_DIAGNOSTICS {
            eprintln!(
                "{}: output truncated; {} additional diagnostics omitted (maximum is {})",
                self.styles.warning("warning"),
                ordered.len() - MAX_DIAGNOSTICS,
                MAX_DIAGNOSTICS
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DiagnosticEmitter, MAX_DIAGNOSTICS, StandardEmitter};
    use crate::diagnostic::{Diagnostic, DiagnosticCode, Files, Label};
    use crate::lexer::Span;

    fn diagnostic(
        code: DiagnosticCode,
        file_id: crate::diagnostic::FileId,
        row: usize,
    ) -> Diagnostic {
        Diagnostic::new(code)
            .with_label(Label::primary(Span::new(row, 1), ""))
            .with_file_id(file_id)
    }

    #[test]
    fn batch_order_is_stable_across_input_order() {
        let mut files = Files::new();
        let b = files.add("b.mux", "x\n".to_owned());
        let a = files.add("a.mux", "x\n".to_owned());
        let diagnostics = vec![
            diagnostic(DiagnosticCode::ImportFailure, b, 1),
            diagnostic(DiagnosticCode::UnusedBinding, a, 1),
            diagnostic(DiagnosticCode::UndefinedName, a, 1),
        ];

        let ordered = StandardEmitter::ordered_diagnostics(&diagnostics, &files);
        let codes: Vec<_> = ordered.iter().map(|diagnostic| diagnostic.code).collect();
        assert_eq!(
            codes,
            vec![
                DiagnosticCode::UndefinedName,
                DiagnosticCode::UnusedBinding,
                DiagnosticCode::ImportFailure,
            ]
        );
    }

    #[test]
    fn batch_limit_is_explicit_and_bounded() {
        assert_eq!(MAX_DIAGNOSTICS, 100);
    }

    #[test]
    fn invalid_provenance_emits_a_controlled_internal_error() {
        let emitter = StandardEmitter::new(super::ColorConfig::Auto);
        let files = Files::new();
        let diagnostic = Diagnostic::new(DiagnosticCode::UndefinedName);

        assert!(std::panic::catch_unwind(|| emitter.emit(&diagnostic, &files)).is_ok());
    }

    #[test]
    fn source_line_fixture_preserves_numbering_and_trimmed_content() {
        let emitter = StandardEmitter::new(super::ColorConfig::Auto);

        assert_eq!(
            emitter.render_source_line(3, "  let answer = 42;  ", 2),
            "\u{1b}[1m\u{1b}[38;2;96;165;250m 3\u{1b}[0m |   let answer = 42;"
        );
        assert_eq!(
            emitter.render_source_line(4, "   ", 2),
            "\u{1b}[1m\u{1b}[38;2;96;165;250m 4\u{1b}[0m |"
        );
    }

    #[test]
    fn byte_range_underlines_use_display_width_not_utf8_length() {
        let emitter = StandardEmitter::new(super::ColorConfig::Auto);
        let text = "界x\nrest";
        let lines = super::source_lines(text);
        let span = Span {
            row_start: 1,
            row_end: Some(2),
            col_start: 1,
            col_end: Some(5),
            byte_range: Some(crate::lexer::ByteRange::new(0, text.len())),
        };

        let rendered = emitter.render_label_underline(
            &span,
            1,
            &lines[0],
            crate::diagnostic::LabelStyle::Primary,
            2,
        );

        assert_eq!(rendered.matches('^').count(), 3);
    }

    #[test]
    fn byte_ranges_determine_diagnostic_lines_and_columns() {
        let text = "a\n界x\nlast";
        let lines = super::source_lines(text);
        let span = Span {
            row_start: 99,
            row_end: Some(100),
            col_start: 88,
            col_end: Some(89),
            byte_range: Some(crate::lexer::ByteRange::new(5, 6)),
        };

        assert_eq!(StandardEmitter::span_line_range(&span, &lines), (2, 2));
        assert_eq!(StandardEmitter::span_start_column(&span, &lines), 3);
        assert!(StandardEmitter::label_covers_line(2, &span, &lines));
        assert!(!StandardEmitter::label_covers_line(99, &span, &lines));
    }
}
