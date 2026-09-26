use crate::lexer::{ByteRange, TokenType};
use crate::syntax::{self, FrontendError, SyntaxKind, SyntaxNode, SyntaxTree};
use std::collections::HashSet;
use unicode_width::UnicodeWidthStr;

use super::{FormatError, FormatOptions};

pub(super) fn format_source(source: &str, options: FormatOptions) -> Result<String, FormatError> {
    if options.indent_width == 0 || options.line_width == 0 {
        return Err(FormatError::input(
            "indent width and line width must be positive",
        ));
    }

    let parsed = syntax::parse_source(source);
    if parsed.has_errors() {
        let errors = parsed
            .errors
            .iter()
            .map(|error| display_frontend_error(error, source))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(FormatError::parse(errors));
    }
    let formatted = Printer::new(&parsed.tree, options).format();
    let reparsed = syntax::parse_source(&formatted);
    if reparsed.has_errors() {
        let errors = reparsed
            .errors
            .iter()
            .map(|error| display_frontend_error(error, &formatted))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(FormatError::parse(format!(
            "formatter produced invalid syntax:\n{errors}"
        )));
    }
    if significant_token_texts(&parsed.tree) != significant_token_texts(&reparsed.tree) {
        return Err(FormatError::parse(
            "formatter changed token spelling or order; refusing to return output",
        ));
    }
    Ok(formatted)
}

fn significant_token_texts(tree: &SyntaxTree) -> Vec<&str> {
    tree.tokens()
        .iter()
        .filter(|token| {
            !matches!(
                token.kind(),
                TokenType::Eof | TokenType::Whitespace | TokenType::NewLine
            )
        })
        .map(|token| token.text(tree.source()))
        .collect()
}

fn display_frontend_error(error: &FrontendError, source: &str) -> String {
    error.display_with_source(source)
}

struct Printer<'a> {
    tree: &'a SyntaxTree,
    options: FormatOptions,
    output: String,
    indent: usize,
    at_line_start: bool,
    pending_newlines: usize,
    previous: Option<usize>,
    saw_significant: bool,
    wrapped_commas: HashSet<usize>,
}

impl<'a> Printer<'a> {
    fn new(tree: &'a SyntaxTree, options: FormatOptions) -> Self {
        Self {
            tree,
            options,
            output: String::new(),
            indent: 0,
            at_line_start: true,
            pending_newlines: 0,
            previous: None,
            saw_significant: false,
            wrapped_commas: find_wrapped_commas(tree, options.indent_width, options.line_width),
        }
    }

    fn format(mut self) -> String {
        for (index, token) in self.tree.tokens().iter().enumerate() {
            match token.kind() {
                TokenType::Eof => break,
                TokenType::Whitespace => {}
                TokenType::NewLine => {
                    self.pending_newlines = self.pending_newlines.saturating_add(1).min(2);
                }
                kind => self.write_token(index, kind),
            }
        }
        if self.saw_significant && !self.output.ends_with('\n') {
            self.output.push('\n');
        }
        self.output
    }

    fn write_token(&mut self, index: usize, kind: &TokenType) {
        let range = self.tree.tokens()[index].range();
        let text = self.tree.token_text(index).unwrap_or("");
        let block_open =
            matches!(kind, TokenType::OpenBrace) && is_open_block(self.tree.root(), range.start);
        let block_close =
            matches!(kind, TokenType::CloseBrace) && is_close_block(self.tree.root(), range.start);

        if let Some(previous_index) = self.previous {
            let previous_kind = self.tree.tokens()[previous_index].kind();
            let previous_range = self.tree.tokens()[previous_index].range();
            // Suppress a source newline only while both adjacent tokens are
            // inside the same continuation region. A break after the closing
            // delimiter ends the expression and must remain significant.
            let continuation = !has_block_context(self.tree, range.start)
                && !has_block_context(self.tree, previous_range.start)
                && is_continuation_context(self.tree, range.start)
                && is_continuation_context(self.tree, previous_range.start);
            let mut requested_newlines = self.pending_newlines;
            if (matches!(previous_kind, TokenType::OpenBrace)
                && is_open_block(self.tree.root(), previous_range.start))
                || block_close
            {
                requested_newlines = requested_newlines.max(1);
            }
            if continuation && !block_close {
                requested_newlines = 0;
            }
            if requested_newlines > 0 {
                let count = if continuation {
                    1
                } else {
                    requested_newlines.min(2)
                };
                self.newlines(count);
            } else if self.needs_space(
                previous_index,
                index,
                previous_kind,
                kind,
                previous_range,
                range,
            ) {
                self.space();
            }
        } else if self.pending_newlines > 0 {
            // Leading blank lines are discarded for a stable file start.
            self.pending_newlines = 0;
        }
        self.pending_newlines = 0;

        if block_close {
            self.indent = self.indent.saturating_sub(1);
        }
        self.write_indent();
        self.output.push_str(text);
        self.at_line_start = text.ends_with('\n');
        self.saw_significant = true;

        if block_open {
            self.indent = self.indent.saturating_add(1);
            self.newlines(1);
        } else if matches!(kind, TokenType::LineComment(_)) {
            self.newlines_preserving_token_text(1);
        } else if self.wrapped_commas.contains(&index) {
            self.newlines(1);
        }
        self.previous = Some(index);
    }

    fn needs_space(
        &self,
        previous_index: usize,
        current_index: usize,
        previous: &TokenType,
        current: &TokenType,
        previous_range: ByteRange,
        current_range: ByteRange,
    ) -> bool {
        use TokenType as T;

        if self.at_line_start {
            return false;
        }
        if matches!(
            current,
            T::CloseParen | T::CloseBracket | T::Dot | T::Comma | T::Colon
        ) {
            return false;
        }
        if matches!(current, T::Incr | T::Decr) {
            return false;
        }
        if matches!(previous, T::OpenParen | T::OpenBracket | T::Dot) {
            return false;
        }
        if matches!(previous, T::Comma | T::Colon) {
            return true;
        }
        if matches!(previous, T::CloseParen) && matches!(current, T::Returns | T::Where) {
            return true;
        }
        if matches!(current, T::OpenBrace) {
            return true;
        }
        if matches!(previous, T::OpenBrace) {
            return true;
        }
        if matches!(current, T::CloseBrace) || matches!(previous, T::CloseBrace) {
            return true;
        }
        if matches!(current, T::LineComment(_) | T::MultilineComment(_)) {
            return !matches!(previous, T::NewLine);
        }
        if is_operator(previous) || is_operator(current) {
            let generic = self.in_kind(previous_range.start, SyntaxKind::TypeArguments)
                || self.in_kind(current_range.start, SyntaxKind::TypeArguments);
            let reference_type = matches!(previous, T::Ref) || matches!(current, T::Ref);
            if generic && (matches!(previous, T::Lt | T::Gt) || matches!(current, T::Lt | T::Gt)) {
                return false;
            }
            if reference_type && self.in_kind(current_range.start, SyntaxKind::Type) {
                return false;
            }
            if self.is_prefix_at(current_index) {
                return is_word(previous)
                    || is_operator(previous)
                    || matches!(previous, T::CloseParen | T::CloseBracket);
            }
            if self.is_prefix_at(previous_index) {
                return false;
            }
            return true;
        }
        if matches!(current, T::OpenParen | T::OpenBracket) {
            return false;
        }
        is_word(previous) && is_word(current)
    }

    fn in_kind(&self, offset: usize, kind: SyntaxKind) -> bool {
        has_kind_at(self.tree.root(), offset, kind)
    }

    fn is_prefix_at(&self, index: usize) -> bool {
        let Some(token) = self.tree.tokens().get(index) else {
            return false;
        };
        if !is_prefix_operator(token.kind()) {
            return false;
        }
        let previous = self.tree.tokens()[..index]
            .iter()
            .rev()
            .find(|token| {
                !matches!(
                    token.kind(),
                    TokenType::Whitespace
                        | TokenType::NewLine
                        | TokenType::Eof
                        | TokenType::LineComment(_)
                        | TokenType::MultilineComment(_)
                )
            })
            .map(|token| token.kind());
        match previous {
            None => true,
            Some(previous) => {
                matches!(
                    previous,
                    TokenType::OpenParen
                        | TokenType::OpenBracket
                        | TokenType::OpenBrace
                        | TokenType::Comma
                        | TokenType::Colon
                        | TokenType::Return
                ) || is_operator(previous)
            }
        }
    }

    fn space(&mut self) {
        if !self.at_line_start && !self.output.chars().last().is_some_and(char::is_whitespace) {
            self.output.push(' ');
        }
    }

    fn newlines(&mut self, count: usize) {
        self.trim_trailing_spaces();
        self.append_newlines(count);
    }

    fn newlines_preserving_token_text(&mut self, count: usize) {
        self.append_newlines(count);
    }

    fn append_newlines(&mut self, count: usize) {
        let current = self
            .output
            .chars()
            .rev()
            .take_while(|ch| *ch == '\n')
            .count();
        for _ in current..count.clamp(1, 2) {
            self.output.push('\n');
        }
        self.at_line_start = true;
    }

    fn write_indent(&mut self) {
        if self.at_line_start {
            for _ in 0..self.indent.saturating_mul(self.options.indent_width) {
                self.output.push(' ');
            }
            self.at_line_start = false;
        }
    }

    fn trim_trailing_spaces(&mut self) {
        while self.output.ends_with(' ') || self.output.ends_with('\t') {
            self.output.pop();
        }
    }
}

fn has_kind_at(node: &SyntaxNode, offset: usize, kind: SyntaxKind) -> bool {
    node.range().start <= offset
        && offset < node.range().end
        && (node.kind() == kind
            || node.children().iter().any(|child| match child {
                syntax::SyntaxElement::Node(child) => has_kind_at(child, offset, kind),
                syntax::SyntaxElement::Token(_) => false,
            }))
}

fn is_open_block(root: &SyntaxNode, offset: usize) -> bool {
    has_any_block_context(root, offset)
}

fn is_close_block(root: &SyntaxNode, offset: usize) -> bool {
    is_open_block(root, offset)
}

fn is_continuation_context(tree: &SyntaxTree, offset: usize) -> bool {
    [
        SyntaxKind::CallArguments,
        SyntaxKind::ParameterList,
        SyntaxKind::ParenthesizedExpression,
        SyntaxKind::TupleExpression,
        SyntaxKind::ListLiteral,
        SyntaxKind::IndexExpression,
    ]
    .into_iter()
    .any(|kind| has_kind_at(tree.root(), offset, kind))
}

fn has_block_context(tree: &SyntaxTree, offset: usize) -> bool {
    has_any_block_context(tree.root(), offset)
}

fn has_any_block_context(node: &SyntaxNode, offset: usize) -> bool {
    [
        SyntaxKind::Block,
        SyntaxKind::ClassBody,
        SyntaxKind::InterfaceBody,
        SyntaxKind::EnumBody,
        SyntaxKind::MatchArms,
    ]
    .into_iter()
    .any(|kind| has_kind_at(node, offset, kind))
}

fn find_wrapped_commas(
    tree: &SyntaxTree,
    indent_width: usize,
    line_width: usize,
) -> HashSet<usize> {
    let eligible = [
        SyntaxKind::CallArguments,
        SyntaxKind::ParameterList,
        SyntaxKind::ListLiteral,
        SyntaxKind::TupleExpression,
    ];
    let mut ranges = Vec::new();
    collect_nodes(tree.root(), &mut ranges, &eligible);
    let mut commas = HashSet::new();
    for (kind, range) in ranges {
        let Some(flat) = tree.source().get(range.start..range.end) else {
            continue;
        };
        if canonical_width_estimate(tree, range, flat, indent_width) <= line_width {
            continue;
        }
        for (index, token) in tree.tokens().iter().enumerate() {
            let token_range = token.range();
            if token_range.start < range.start || token_range.end > range.end {
                continue;
            }
            if matches!(token.kind(), TokenType::Comma)
                && innermost_eligible_range(tree.root(), token_range.start, &eligible)
                    == Some((kind, range))
            {
                commas.insert(index);
            }
        }
    }
    commas
}

fn canonical_width_estimate(
    tree: &SyntaxTree,
    range: ByteRange,
    _text: &str,
    indent_width: usize,
) -> usize {
    let line_start = tree.source()[..range.start]
        .rfind('\n')
        .map_or(0, |newline| newline + 1);
    let prefix_width = token_width(tree, ByteRange::new(line_start, range.start));
    let node_width = token_width(tree, range);
    let separator_spaces = tree
        .tokens()
        .iter()
        .filter(|token| {
            let token_range = token.range();
            token_range.start >= range.start && token_range.end <= range.end
        })
        .map(|token| match token.kind() {
            TokenType::Comma => 1,
            kind if is_operator(kind) => 2,
            _ => 0,
        })
        .sum::<usize>();
    let indent = block_depth_at(tree.root(), range.start).saturating_mul(indent_width);
    prefix_width + node_width + separator_spaces + indent
}

fn token_width(tree: &SyntaxTree, range: ByteRange) -> usize {
    tree.tokens()
        .iter()
        .filter(|token| {
            let token_range = token.range();
            token_range.start >= range.start && token_range.end <= range.end
        })
        .filter(|token| {
            !matches!(
                token.kind(),
                TokenType::Whitespace | TokenType::NewLine | TokenType::Eof
            )
        })
        .map(|token| UnicodeWidthStr::width(token.text(tree.source())))
        .sum()
}

fn block_depth_at(node: &SyntaxNode, offset: usize) -> usize {
    if !(node.range().start <= offset && offset < node.range().end) {
        return 0;
    }
    let own = if matches!(
        node.kind(),
        SyntaxKind::Block
            | SyntaxKind::ClassBody
            | SyntaxKind::InterfaceBody
            | SyntaxKind::EnumBody
            | SyntaxKind::MatchArms
    ) {
        1
    } else {
        0
    };
    node.children()
        .iter()
        .filter_map(|child| match child {
            syntax::SyntaxElement::Node(child)
                if child.range().start <= offset && offset < child.range().end =>
            {
                Some(block_depth_at(child, offset))
            }
            _ => None,
        })
        .max()
        .unwrap_or(0)
        + own
}

fn collect_nodes(
    node: &SyntaxNode,
    collected: &mut Vec<(SyntaxKind, ByteRange)>,
    eligible: &[SyntaxKind],
) {
    if eligible.contains(&node.kind()) {
        collected.push((node.kind(), node.range()));
    }
    for child in node.children() {
        if let syntax::SyntaxElement::Node(child) = child {
            collect_nodes(child, collected, eligible);
        }
    }
}

fn innermost_eligible_range(
    node: &SyntaxNode,
    offset: usize,
    eligible: &[SyntaxKind],
) -> Option<(SyntaxKind, ByteRange)> {
    if !(node.range().start <= offset && offset < node.range().end) {
        return None;
    }
    let mut current = eligible
        .contains(&node.kind())
        .then_some((node.kind(), node.range()));
    for child in node.children() {
        if let syntax::SyntaxElement::Node(child) = child
            && child.range().start <= offset
            && offset < child.range().end
        {
            current = innermost_eligible_range(child, offset, eligible).or(current);
            break;
        }
    }
    current
}

fn is_word(token: &TokenType) -> bool {
    use TokenType as T;
    matches!(
        token,
        T::Auto
            | T::Func
            | T::Returns
            | T::Return
            | T::Use
            | T::If
            | T::Else
            | T::For
            | T::While
            | T::Match
            | T::Const
            | T::Class
            | T::Interface
            | T::Enum
            | T::Import
            | T::Is
            | T::As
            | T::In
            | T::Break
            | T::Continue
            | T::None
            | T::Common
            | T::Where
            | T::Test
            | T::Int(_)
            | T::Float(_)
            | T::Bool(_)
            | T::Char(_)
            | T::Str(_)
            | T::Bytes(_)
            | T::Underscore
            | T::Id(_)
            | T::LineComment(_)
            | T::MultilineComment(_)
    )
}

fn is_operator(token: &TokenType) -> bool {
    use TokenType as T;
    matches!(
        token,
        T::Eq
            | T::Plus
            | T::Minus
            | T::Star
            | T::StarStar
            | T::Slash
            | T::Percent
            | T::Lt
            | T::Gt
            | T::Le
            | T::Ge
            | T::EqEq
            | T::NotEq
            | T::Bang
            | T::Incr
            | T::Decr
            | T::PlusEq
            | T::MinusEq
            | T::StarEq
            | T::SlashEq
            | T::PercentEq
            | T::And
            | T::Or
            | T::Ref
    )
}

fn is_prefix_operator(token: &TokenType) -> bool {
    matches!(
        token,
        TokenType::Plus | TokenType::Minus | TokenType::Bang | TokenType::Ref
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting_is_idempotent_and_preserves_literal_and_comment_text() {
        let source = "auto label=\"escaped\\tvalue\" // keep  trailing  \n";
        let formatted = format_source(source, FormatOptions::default()).unwrap();

        assert!(formatted.contains("\"escaped\\tvalue\""));
        assert!(formatted.contains("// keep  trailing  "));
        assert_eq!(
            format_source(&formatted, FormatOptions::default()).unwrap(),
            formatted
        );
    }

    #[test]
    fn unicode_text_and_crlf_inside_multiline_literals_are_preserved() {
        let source = "auto greeting=\"café 🐈\"\r\nauto poem=\"first\r\nsecond\"\r\n";
        let formatted = format_source(source, FormatOptions::default()).unwrap();

        assert!(formatted.contains("\"café 🐈\""));
        assert!(formatted.contains("\"first\r\nsecond\""));
        // Source line endings are normalized, while the literal's token text stays exact.
        assert_eq!(formatted.matches('\r').count(), 1);
        assert_eq!(
            format_source(&formatted, FormatOptions::default()).unwrap(),
            formatted
        );
    }

    #[test]
    fn multiline_string_literal_spelling_is_preserved_and_idempotent() {
        let source = "auto message=\"first\n  second\nthird\"\n";
        let formatted = format_source(source, FormatOptions::default()).unwrap();

        assert!(formatted.contains("\"first\n  second\nthird\""));
        assert_eq!(
            format_source(&formatted, FormatOptions::default()).unwrap(),
            formatted
        );
    }

    #[test]
    fn invalid_syntax_is_rejected() {
        assert!(format_source("auto =\n", FormatOptions::default()).is_err());
    }

    #[test]
    fn long_calls_and_parameter_lists_wrap_at_the_configured_width() {
        let source = concat!(
            "func build(int first_parameter_name, int second_parameter_name, int third_parameter_name) returns void {\n",
            "auto result = combine(first_argument_name, second_argument_name, third_argument_name)\n",
            "}\n"
        );
        let formatted = format_source(source, FormatOptions::default()).unwrap();

        assert!(formatted.contains("combine("));
        assert!(formatted.contains("func build("));
        assert!(formatted.contains("first_argument_name,\n"));
        assert!(formatted.contains("first_parameter_name,\n"));
        assert_eq!(
            format_source(&formatted, FormatOptions::default()).unwrap(),
            formatted
        );
    }

    #[test]
    fn custom_indentation_and_width_are_idempotent() {
        let source = "if true { auto value=1 }\n";
        let options = FormatOptions {
            indent_width: 2,
            line_width: 20,
        };
        let formatted = format_source(source, options).unwrap();
        assert!(formatted.contains("\n  auto value = 1\n"));
        assert_eq!(format_source(&formatted, options).unwrap(), formatted);
    }

    #[test]
    fn comments_inside_calls_stay_on_their_own_line_and_keep_their_text() {
        let source = "auto result = combine(first, // keep  these spaces  \nsecond)\n";
        let formatted = format_source(source, FormatOptions::default()).unwrap();

        assert!(formatted.contains("// keep  these spaces  \n"));
        assert!(formatted.contains("second)"));
        assert_eq!(
            format_source(&formatted, FormatOptions::default()).unwrap(),
            formatted
        );
    }

    #[test]
    fn lambda_blocks_reset_call_continuation_and_collection_braces_stay_inline() {
        let source = concat!(
            "auto task = spawn(func() returns void {\n",
            "auto inner=1\n",
            "})\n",
            "auto values={1,2}\n"
        );
        let formatted = format_source(source, FormatOptions::default()).unwrap();

        assert!(formatted.contains("func() returns void {\n    auto inner = 1\n"));
        assert!(formatted.contains("auto values = { 1, 2 }"));
        assert_eq!(
            format_source(&formatted, FormatOptions::default()).unwrap(),
            formatted
        );
    }

    #[test]
    fn generic_arguments_comparisons_unary_and_binary_operators_keep_distinctions() {
        let source = concat!(
            "auto value = build<Widget>(item)\n",
            "auto below = count<limit\n",
            "auto result = -first + second\n"
        );
        let formatted = format_source(source, FormatOptions::default()).unwrap();

        assert!(formatted.contains("build<Widget>(item)"));
        assert!(formatted.contains("count < limit"));
        assert!(formatted.contains("= -first + second"));
    }

    #[test]
    fn test_annotation_comment_remains_before_its_test() {
        let source = "// mux:test timeout=10\ntest \"short\" { print(\"short\") }\n";
        let formatted = format_source(source, FormatOptions::default()).unwrap();
        assert!(formatted.contains("// mux:test timeout=10\ntest \"short\""));
    }
}
