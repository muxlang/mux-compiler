use crate::lexer::TokenType;
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
    let original_tokens: Vec<String> = significant_token_texts(&parsed.tree)
        .into_iter()
        .map(str::to_owned)
        .collect();
    let mut current = source.to_owned();
    let mut tree = parsed.tree;
    let mut seen = HashSet::new();
    seen.insert(current.clone());
    loop {
        let formatted = Printer::new(&tree, options).format();
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
        if original_tokens
            != significant_token_texts(&reparsed.tree)
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        {
            return Err(FormatError::parse(
                "formatter changed token spelling or order; refusing to return output",
            ));
        }
        if formatted == current {
            return Ok(formatted);
        }
        if !seen.insert(formatted.clone()) {
            return Err(FormatError::parse(
                "formatter layout did not converge; refusing to return unstable output",
            ));
        }
        current = formatted;
        tree = reparsed.tree;
    }
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
    contexts: Vec<TokenContext>,
}

impl<'a> Printer<'a> {
    fn new(tree: &'a SyntaxTree, options: FormatOptions) -> Self {
        let contexts = TokenContexts::from_tree(tree);
        let wrapped_commas =
            find_wrapped_commas(tree, options.indent_width, options.line_width, &contexts);
        Self {
            tree,
            options,
            output: String::new(),
            indent: 0,
            at_line_start: true,
            pending_newlines: 0,
            previous: None,
            saw_significant: false,
            wrapped_commas,
            contexts,
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
        let text = self.tree.token_text(index).unwrap_or("");
        let block_open = matches!(kind, TokenType::OpenBrace) && self.contexts[index].block;
        let block_close = matches!(kind, TokenType::CloseBrace) && self.contexts[index].block;

        if let Some(previous_index) = self.previous {
            let previous_kind = self.tree.tokens()[previous_index].kind();
            // Suppress a source newline only while both adjacent tokens are
            // inside the same continuation region. A break after the closing
            // delimiter ends the expression and must remain significant.
            let continuation = !self.contexts[index].block
                && !self.contexts[previous_index].block
                && self.contexts[index].continuation
                && self.contexts[previous_index].continuation;
            let mut requested_newlines = self.pending_newlines;
            if (matches!(previous_kind, TokenType::OpenBrace)
                && self.contexts[previous_index].block)
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
            } else if self.needs_space(previous_index, index, previous_kind, kind) {
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
            let generic = self.contexts[previous_index].type_arguments
                || self.contexts[current_index].type_arguments;
            let reference_type = matches!(previous, T::Ref) || matches!(current, T::Ref);
            if generic && (matches!(previous, T::Lt | T::Gt) || matches!(current, T::Lt | T::Gt)) {
                return false;
            }
            if reference_type && self.contexts[current_index].type_context {
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

#[derive(Debug, Default, Clone, Copy)]
struct ContextDelta {
    block: isize,
    continuation: isize,
    type_arguments: isize,
    type_context: isize,
}

#[derive(Debug, Default, Clone, Copy)]
struct TokenContext {
    block: bool,
    block_depth: usize,
    continuation: bool,
    type_arguments: bool,
    type_context: bool,
}

struct TokenContexts;

impl TokenContexts {
    fn from_tree(tree: &SyntaxTree) -> Vec<TokenContext> {
        let tokens = tree.tokens();
        let mut deltas = vec![ContextDelta::default(); tokens.len() + 1];
        collect_context_deltas(tree.root(), tokens, &mut deltas);

        let mut active = ContextDelta::default();
        deltas
            .into_iter()
            .take(tokens.len())
            .map(|delta| {
                active.block += delta.block;
                active.continuation += delta.continuation;
                active.type_arguments += delta.type_arguments;
                active.type_context += delta.type_context;
                TokenContext {
                    block: active.block > 0,
                    block_depth: active.block.max(0) as usize,
                    continuation: active.continuation > 0,
                    type_arguments: active.type_arguments > 0,
                    type_context: active.type_context > 0,
                }
            })
            .collect()
    }
}

fn collect_context_deltas(
    node: &SyntaxNode,
    tokens: &[syntax::SyntaxToken],
    deltas: &mut [ContextDelta],
) {
    let kind = node.kind();
    let block = matches!(
        kind,
        SyntaxKind::Block
            | SyntaxKind::ClassBody
            | SyntaxKind::InterfaceBody
            | SyntaxKind::EnumBody
            | SyntaxKind::MatchArms
    );
    let continuation = matches!(
        kind,
        SyntaxKind::CallArguments
            | SyntaxKind::ParameterList
            | SyntaxKind::ParenthesizedExpression
            | SyntaxKind::TupleExpression
            | SyntaxKind::ListLiteral
            | SyntaxKind::IndexExpression
    );
    let type_arguments = kind == SyntaxKind::TypeArguments;
    let type_context = kind == SyntaxKind::Type;
    if block || continuation || type_arguments || type_context {
        let range = node.range();
        let start = tokens.partition_point(|token| token.range().start < range.start);
        let end = tokens.partition_point(|token| token.range().start < range.end);
        if start < end {
            for (index, amount) in [(start, 1), (end, -1)] {
                if block {
                    deltas[index].block += amount;
                }
                if continuation {
                    deltas[index].continuation += amount;
                }
                if type_arguments {
                    deltas[index].type_arguments += amount;
                }
                if type_context {
                    deltas[index].type_context += amount;
                }
            }
        }
    }
    for child in node.children() {
        if let syntax::SyntaxElement::Node(child) = child {
            collect_context_deltas(child, tokens, deltas);
        }
    }
}

fn find_wrapped_commas(
    tree: &SyntaxTree,
    indent_width: usize,
    line_width: usize,
    contexts: &[TokenContext],
) -> HashSet<usize> {
    let eligible = [
        SyntaxKind::CallArguments,
        SyntaxKind::ParameterList,
        SyntaxKind::ListLiteral,
        SyntaxKind::TupleExpression,
    ];
    let width_prefixes = TokenWidthPrefixes::new(tree);
    let mut ranges = Vec::new();
    collect_eligible_ranges(tree.root(), 0, &eligible, tree.tokens(), &mut ranges);
    for range in &mut ranges {
        range.wraps = width_prefixes.estimate(
            tree,
            range.start_token,
            range.end_token,
            indent_width,
            contexts[range.start_token].block_depth,
        ) > line_width;
    }
    ranges.sort_by_key(|range: &EligibleRange| {
        (
            range.start_token,
            std::cmp::Reverse(range.end_token),
            range.depth,
        )
    });

    let mut commas = HashSet::new();
    let mut active = Vec::new();
    let mut next_range = 0;
    for (index, token) in tree.tokens().iter().enumerate() {
        while active
            .last()
            .is_some_and(|active_index: &usize| ranges[*active_index].end_token <= index)
        {
            active.pop();
        }
        while ranges
            .get(next_range)
            .is_some_and(|range| range.start_token == index)
        {
            active.push(next_range);
            next_range += 1;
        }
        if matches!(token.kind(), TokenType::Comma)
            && active
                .last()
                .is_some_and(|active_index| ranges[*active_index].wraps)
        {
            commas.insert(index);
        }
    }
    commas
}

struct EligibleRange {
    start_token: usize,
    end_token: usize,
    depth: usize,
    wraps: bool,
}

struct TokenWidthPrefixes {
    width: Vec<usize>,
    separators: Vec<usize>,
    word_separators: Vec<usize>,
}

impl TokenWidthPrefixes {
    fn new(tree: &SyntaxTree) -> Self {
        let tokens = tree.tokens();
        let mut width = Vec::with_capacity(tokens.len() + 1);
        let mut separators = Vec::with_capacity(tokens.len() + 1);
        let mut word_separators = Vec::with_capacity(tokens.len() + 1);
        width.push(0);
        separators.push(0);
        word_separators.push(0);
        let mut previous_word = false;

        for token in tokens {
            let significant = !matches!(
                token.kind(),
                TokenType::Whitespace | TokenType::NewLine | TokenType::Eof
            );
            let token_width = if significant {
                UnicodeWidthStr::width(token.text(tree.source()))
            } else {
                0
            };
            let separator = match token.kind() {
                TokenType::Comma => 1,
                kind if is_operator(kind) => 2,
                _ => 0,
            };
            let is_current_word = significant && is_word(token.kind());

            width.push(width.last().copied().unwrap_or_default() + token_width);
            separators.push(separators.last().copied().unwrap_or_default() + separator);
            word_separators.push(
                word_separators.last().copied().unwrap_or_default()
                    + usize::from(previous_word && is_current_word),
            );
            if significant {
                previous_word = is_current_word;
            }
        }
        Self {
            width,
            separators,
            word_separators,
        }
    }

    fn estimate(
        &self,
        tree: &SyntaxTree,
        start: usize,
        end: usize,
        indent_width: usize,
        block_depth: usize,
    ) -> usize {
        let range = tree.tokens()[start].range();
        let line_start = tree.source()[..range.start]
            .rfind('\n')
            .map_or(0, |newline| newline + 1);
        let prefix_start = tree
            .tokens()
            .partition_point(|token| token.range().start < line_start);
        let prefix_width = self.width[start] - self.width[prefix_start];
        let node_width = self.width[end] - self.width[start];
        let separator_spaces = self.separators[end] - self.separators[start];
        let word_separators = if start < end {
            self.word_separators[end] - self.word_separators[start + 1]
        } else {
            0
        };
        let indent = block_depth.saturating_mul(indent_width);
        prefix_width + node_width + separator_spaces + word_separators + indent
    }
}

fn collect_eligible_ranges(
    node: &SyntaxNode,
    depth: usize,
    eligible: &[SyntaxKind],
    tokens: &[syntax::SyntaxToken],
    collected: &mut Vec<EligibleRange>,
) {
    if eligible.contains(&node.kind()) {
        let range = node.range();
        let start_token = tokens.partition_point(|token| token.range().start < range.start);
        let end_token = tokens.partition_point(|token| token.range().start < range.end);
        if start_token < end_token {
            collected.push(EligibleRange {
                start_token,
                end_token,
                depth,
                wraps: false,
            });
        }
    }
    for child in node.children() {
        if let syntax::SyntaxElement::Node(child) = child {
            collect_eligible_ranges(child, depth + 1, eligible, tokens, collected);
        }
    }
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
    fn nested_call_wrapping_is_idempotent_after_outer_call_wraps() {
        let identifier = "a".repeat(57);
        let source = format!("auto v = outer({identifier}, inner(x, y))\n");

        let formatted = format_source(&source, FormatOptions::default()).unwrap();

        assert_eq!(
            format_source(&formatted, FormatOptions::default()).unwrap(),
            formatted
        );
        assert!(formatted.contains("inner(x, y)"));
    }

    #[test]
    fn parameter_width_estimate_includes_spaces_between_type_and_name() {
        let source = "func f(int first, int second) returns void {\nreturn\n}\n";
        let options = FormatOptions {
            indent_width: 4,
            line_width: 27,
        };

        let formatted = format_source(source, options).unwrap();

        assert!(formatted.contains("func f(int first,\n"));
        assert!(formatted.contains("int first,\n"));
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
