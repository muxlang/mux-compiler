use crate::lexer::TokenType;
use crate::syntax::{self, FrontendError, SyntaxKind, SyntaxNode, SyntaxTree};
use std::collections::{HashMap, HashSet};
use unicode_width::UnicodeWidthStr;

use super::{BraceStyle, FormatError, FormatOptions, IndentType, TrailingComma, WherePosition};

pub(super) fn format_source(source: &str, options: FormatOptions) -> Result<String, FormatError> {
    if options.indent_count == 0 || options.line_width == 0 {
        return Err(FormatError::input(
            "indent count and line width must be positive",
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
    let original_tokens = canonical_token_texts(&parsed.tree);
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
        if original_tokens != canonical_token_texts(&reparsed.tree) {
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

fn canonical_token_texts(tree: &SyntaxTree) -> Vec<String> {
    let ignored_commas = trailing_comma_indices(tree);
    tree.tokens()
        .iter()
        .enumerate()
        .filter(|(index, token)| {
            !ignored_commas.contains(index)
                && !matches!(
                    token.kind(),
                    TokenType::Eof | TokenType::Whitespace | TokenType::NewLine
                )
        })
        .map(|(_, token)| token.text(tree.source()).to_owned())
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
    wrapped_operators: HashSet<usize>,
    forced_newlines_before: HashMap<usize, usize>,
    insert_commas_after: HashSet<usize>,
    remove_commas: HashSet<usize>,
    contexts: Vec<TokenContext>,
}

impl<'a> Printer<'a> {
    fn new(tree: &'a SyntaxTree, options: FormatOptions) -> Self {
        let contexts = TokenContexts::from_tree(tree);
        let indent_width = width_indent(options);
        let wrapped_commas = find_wrapped_commas(tree, indent_width, options.line_width, &contexts);
        let wrapped_operators =
            find_wrapped_operators(tree, indent_width, options.line_width, &contexts);
        let forced_newlines_before = layout_newlines(tree, options);
        let (insert_commas_after, remove_commas) = trailing_comma_edits(
            tree,
            options.trailing_comma,
            &wrapped_commas,
            &wrapped_operators,
        );
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
            wrapped_operators,
            forced_newlines_before,
            insert_commas_after,
            remove_commas,
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
                TokenType::Comma if self.remove_commas.contains(&index) => {}
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
            if let Some(count) = self.forced_newlines_before.get(&index) {
                requested_newlines = *count;
            }
            if block_open {
                requested_newlines = match self.options.brace_style {
                    BraceStyle::SameLine => 0,
                    BraceStyle::NextLine => 1,
                };
            }
            if matches!(kind, TokenType::Where) {
                requested_newlines = match self.options.where_position {
                    WherePosition::OwnLine => 1,
                    WherePosition::SameLine => 0,
                };
            }
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
                let count = if continuation { 1 } else { requested_newlines };
                self.newlines(count);
            } else if !(self.insert_commas_after.contains(&previous_index)
                && matches!(kind, TokenType::CloseBrace))
                && self.needs_space(previous_index, index, previous_kind, kind)
            {
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
        if self.insert_commas_after.contains(&index) {
            self.output.push(',');
        }
        self.at_line_start = text.ends_with('\n');
        self.saw_significant = true;

        if block_open {
            self.indent = self.indent.saturating_add(1);
            self.newlines(1);
        } else if matches!(kind, TokenType::LineComment(_)) {
            self.newlines_preserving_token_text(1);
        } else if self.wrapped_commas.contains(&index) || self.wrapped_operators.contains(&index) {
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
        if matches!(previous, T::Comma) && matches!(current, T::CloseBrace) {
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
        for _ in current..count.max(1) {
            self.output.push('\n');
        }
        self.at_line_start = true;
    }

    fn write_indent(&mut self) {
        if self.at_line_start {
            for _ in 0..self.indent.saturating_mul(self.options.indent_count) {
                self.output.push(match self.options.indent_type {
                    IndentType::Space => ' ',
                    IndentType::Tab => '\t',
                });
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

fn width_indent(options: FormatOptions) -> usize {
    match options.indent_type {
        IndentType::Space => options.indent_count,
        // Width estimation uses the common four-column display width for tabs.
        IndentType::Tab => options.indent_count.saturating_mul(4),
    }
}

fn layout_newlines(tree: &SyntaxTree, options: FormatOptions) -> HashMap<usize, usize> {
    let mut result = HashMap::new();
    let root_children = direct_nodes(tree.root());
    add_spacing_between_nodes(
        tree,
        &root_children,
        options.blank_lines_between_declarations,
        None,
        &mut result,
    );

    fn visit_bodies(
        tree: &SyntaxTree,
        node: &SyntaxNode,
        options: FormatOptions,
        result: &mut HashMap<usize, usize>,
    ) {
        if matches!(
            node.kind(),
            SyntaxKind::ClassBody | SyntaxKind::InterfaceBody | SyntaxKind::EnumBody
        ) {
            let members = direct_nodes(node);
            add_spacing_between_nodes(
                tree,
                &members,
                options.blank_lines_between_members,
                Some(options.blank_lines_before_functions),
                result,
            );
        }
        if node.kind() == SyntaxKind::MatchExpression {
            let arms = direct_nodes(node)
                .into_iter()
                .filter(|arm| arm.kind() == SyntaxKind::MatchArm)
                .collect::<Vec<_>>();
            add_spacing_between_nodes(tree, &arms, 0, None, result);
        }
        for child in node.children() {
            if let syntax::SyntaxElement::Node(child) = child {
                visit_bodies(tree, child, options, result);
            }
        }
    }
    visit_bodies(tree, tree.root(), options, &mut result);
    result
}

fn direct_nodes(node: &SyntaxNode) -> Vec<&SyntaxNode> {
    node.children()
        .iter()
        .filter_map(|child| match child {
            syntax::SyntaxElement::Node(child) => Some(child.as_ref()),
            syntax::SyntaxElement::Token(_) => None,
        })
        .collect()
}

fn add_spacing_between_nodes(
    tree: &SyntaxTree,
    nodes: &[&SyntaxNode],
    blank_lines: usize,
    function_blank_lines: Option<usize>,
    result: &mut HashMap<usize, usize>,
) {
    for pair in nodes.windows(2) {
        let previous_end = pair[0].range().end;
        let next_start = pair[1].range().start;
        let first_index = tree
            .tokens()
            .partition_point(|token| token.range().start < previous_end);
        let next_index = tree
            .tokens()
            .partition_point(|token| token.range().start < next_start);
        let marker = (first_index..next_index)
            .find(|index| {
                matches!(
                    tree.tokens()[*index].kind(),
                    TokenType::LineComment(_) | TokenType::MultilineComment(_)
                )
            })
            .or_else(|| {
                (next_index..tree.tokens().len()).find(|index| {
                    !matches!(
                        tree.tokens()[*index].kind(),
                        TokenType::Whitespace | TokenType::NewLine | TokenType::Eof
                    )
                })
            });
        if let Some(index) = marker {
            let starts_function = function_blank_lines.is_some_and(|_| {
                matches!(
                    tree.tokens()[index].kind(),
                    TokenType::Func | TokenType::Common
                ) || node_starts_with_function(tree, pair[1])
            });
            let selected = if starts_function {
                function_blank_lines.unwrap_or(blank_lines)
            } else {
                blank_lines
            };
            result.insert(index, selected.saturating_add(1));
        }
    }
}

fn node_starts_with_function(tree: &SyntaxTree, node: &SyntaxNode) -> bool {
    let range = node.range();
    let start = tree
        .tokens()
        .partition_point(|token| token.range().start < range.start);
    tree.tokens()
        .get(start..)
        .and_then(|tokens| {
            tokens.iter().find(|token| {
                !matches!(
                    token.kind(),
                    TokenType::Whitespace | TokenType::NewLine | TokenType::Eof
                )
            })
        })
        .is_some_and(|token| matches!(token.kind(), TokenType::Func | TokenType::Common))
}

fn trailing_comma_indices(tree: &SyntaxTree) -> HashSet<usize> {
    let mut ranges = Vec::new();
    collect_eligible_ranges(
        tree.root(),
        0,
        &[
            SyntaxKind::ListLiteral,
            SyntaxKind::MapLiteral,
            SyntaxKind::MatchExpression,
        ],
        tree.tokens(),
        &mut ranges,
    );
    ranges
        .into_iter()
        .filter_map(|range| trailing_comma_before_close(tree, range.start_token, range.end_token))
        .collect()
}

fn trailing_comma_before_close(tree: &SyntaxTree, start: usize, end: usize) -> Option<usize> {
    if start >= end {
        return None;
    }
    let close = (start..end).rev().find(|index| {
        matches!(
            tree.tokens()[*index].kind(),
            TokenType::CloseBracket | TokenType::CloseBrace
        )
    })?;
    (start..close)
        .rev()
        .find(|index| {
            !matches!(
                tree.tokens()[*index].kind(),
                TokenType::Whitespace
                    | TokenType::NewLine
                    | TokenType::LineComment(_)
                    | TokenType::MultilineComment(_)
                    | TokenType::Eof
            )
        })
        .filter(|index| matches!(tree.tokens()[*index].kind(), TokenType::Comma))
}

fn trailing_comma_edits(
    tree: &SyntaxTree,
    policy: TrailingComma,
    wrapped_commas: &HashSet<usize>,
    wrapped_operators: &HashSet<usize>,
) -> (HashSet<usize>, HashSet<usize>) {
    let mut ranges = Vec::new();
    collect_eligible_ranges(
        tree.root(),
        0,
        &[
            SyntaxKind::ListLiteral,
            SyntaxKind::MapLiteral,
            SyntaxKind::MatchExpression,
        ],
        tree.tokens(),
        &mut ranges,
    );
    let existing = trailing_comma_indices(tree);
    let mut insert_after = HashSet::new();
    let mut remove = HashSet::new();
    for range in ranges {
        let close = (range.start_token..range.end_token).rev().find(|index| {
            matches!(
                tree.tokens()[*index].kind(),
                TokenType::CloseBracket | TokenType::CloseBrace
            )
        });
        let Some(close) = close else { continue };
        let current = (range.start_token..close).rev().find(|index| {
            !matches!(
                tree.tokens()[*index].kind(),
                TokenType::Whitespace
                    | TokenType::NewLine
                    | TokenType::LineComment(_)
                    | TokenType::MultilineComment(_)
                    | TokenType::Eof
            )
        });
        let Some(last) = current else { continue };
        if matches!(
            tree.tokens()[last].kind(),
            TokenType::OpenBracket | TokenType::OpenBrace
        ) {
            continue;
        }
        let has_trailing = existing.contains(&last);
        let multiline = range.start_token..range.end_token;
        let is_multiline =
            tree.tokens()[multiline.clone()]
                .iter()
                .enumerate()
                .any(|(offset, token)| {
                    let index = multiline.start + offset;
                    wrapped_commas.contains(&index)
                        || wrapped_operators.contains(&index)
                        || matches!(
                            token.kind(),
                            TokenType::LineComment(_) | TokenType::MultilineComment(_)
                        )
                });
        let wants_trailing = match policy {
            TrailingComma::Never => false,
            TrailingComma::Always => true,
            TrailingComma::Multiline => is_multiline || range.kind == SyntaxKind::MatchExpression,
        };
        if has_trailing && !wants_trailing {
            remove.insert(last);
        } else if wants_trailing && !has_trailing {
            insert_after.insert(last);
        }
    }
    (insert_after, remove)
}

fn find_wrapped_operators(
    tree: &SyntaxTree,
    indent_width: usize,
    line_width: usize,
    contexts: &[TokenContext],
) -> HashSet<usize> {
    let width_prefixes = TokenWidthPrefixes::new(tree);
    let mut ranges = Vec::new();
    collect_eligible_ranges(
        tree.root(),
        0,
        &[SyntaxKind::BinaryExpression],
        tree.tokens(),
        &mut ranges,
    );
    let mut wrapped = HashSet::new();
    for range in ranges {
        if width_prefixes.estimate(
            tree,
            range.start_token,
            range.end_token,
            indent_width,
            contexts[range.start_token].block_depth,
        ) <= line_width
        {
            continue;
        }
        for (offset, context) in contexts
            .iter()
            .enumerate()
            .take(range.end_token)
            .skip(range.start_token)
        {
            let index = offset;
            let kind = tree.tokens()[index].kind();
            if is_binary_break_operator(kind)
                && !context.type_context
                && !context.type_arguments
                && !is_prefix_operator_at(tree, index)
            {
                wrapped.insert(index);
            }
        }
    }
    wrapped
}

fn is_binary_break_operator(kind: &TokenType) -> bool {
    matches!(
        kind,
        TokenType::Plus
            | TokenType::Minus
            | TokenType::Star
            | TokenType::StarStar
            | TokenType::Slash
            | TokenType::Percent
            | TokenType::EqEq
            | TokenType::NotEq
            | TokenType::Lt
            | TokenType::Gt
            | TokenType::Le
            | TokenType::Ge
            | TokenType::And
            | TokenType::Or
            | TokenType::In
    )
}

fn is_prefix_operator_at(tree: &SyntaxTree, index: usize) -> bool {
    let previous = tree.tokens()[..index]
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
    );
    let continuation = matches!(
        kind,
        SyntaxKind::CallArguments
            | SyntaxKind::ParameterList
            | SyntaxKind::ParenthesizedExpression
            | SyntaxKind::TupleExpression
            | SyntaxKind::ListLiteral
            | SyntaxKind::MapLiteral
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
    if kind == SyntaxKind::MatchExpression
        && let Some((open, close)) = match_expression_braces(node, tokens)
    {
        deltas[open].block += 1;
        deltas[close + 1].block -= 1;
    }
    for child in node.children() {
        if let syntax::SyntaxElement::Node(child) = child {
            collect_context_deltas(child, tokens, deltas);
        }
    }
}

fn match_expression_braces(
    node: &SyntaxNode,
    tokens: &[syntax::SyntaxToken],
) -> Option<(usize, usize)> {
    let range = node.range();
    let start = tokens.partition_point(|token| token.range().start < range.start);
    let end = tokens.partition_point(|token| token.range().start < range.end);
    let close = (start..end)
        .rev()
        .find(|index| matches!(tokens[*index].kind(), TokenType::CloseBrace))?;
    let mut depth = 0_usize;
    for index in (start..=close).rev() {
        match tokens[index].kind() {
            TokenType::CloseBrace => depth += 1,
            TokenType::OpenBrace => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some((index, close));
                }
            }
            _ => {}
        }
    }
    None
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
        SyntaxKind::MapLiteral,
        SyntaxKind::MatchExpression,
    ];
    let width_prefixes = TokenWidthPrefixes::new(tree);
    let trailing_commas = trailing_comma_indices(tree);
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
            && !trailing_commas.contains(&index)
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
    kind: SyntaxKind,
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
                kind: node.kind(),
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
        let options = FormatOptions {
            line_width: 10,
            ..FormatOptions::default()
        };
        let formatted = format_source(source, options).unwrap();

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
            line_width: 27,
            ..FormatOptions::default()
        };

        let formatted = format_source(source, options).unwrap();

        assert!(formatted.contains("func f(int first,\n"));
        assert!(formatted.contains("int first,\n"));
    }

    #[test]
    fn custom_indentation_and_width_are_idempotent() {
        let source = "if true { auto value=1 }\n";
        let options = FormatOptions {
            indent_count: 2,
            line_width: 20,
            ..FormatOptions::default()
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
    fn long_binary_expressions_wrap_after_operators_and_stay_idempotent() {
        let source = "auto total = first_operand + second_operand + third_operand\n";
        let options = FormatOptions {
            line_width: 35,
            ..FormatOptions::default()
        };
        let formatted = format_source(source, options).unwrap();

        assert!(formatted.contains("+\n"), "{formatted}");
        assert!(!formatted.contains("\n    +"), "{formatted}");
        assert_eq!(format_source(&formatted, options).unwrap(), formatted);
    }

    #[test]
    fn configured_braces_where_clauses_and_blank_lines_are_applied() {
        let source = concat!(
            "func first() returns void { return }\n",
            "func second() returns void { return }\n",
            "func constrained<T>(T value) where { true } returns void { return }\n"
        );
        let options = FormatOptions {
            brace_style: super::super::BraceStyle::NextLine,
            where_position: super::super::WherePosition::SameLine,
            blank_lines_between_declarations: 0,
            ..FormatOptions::default()
        };
        let formatted = format_source(source, options).unwrap();

        assert!(formatted.contains("returns void\n{"), "{formatted}");
        assert!(formatted.contains(") where {"), "{formatted}");
        assert!(formatted.contains("}\nfunc second"), "{formatted}");
        assert_eq!(format_source(&formatted, options).unwrap(), formatted);
    }

    #[test]
    fn default_spacing_separates_top_level_declarations_by_one_blank_line() {
        let source =
            "func first() returns void { return }\nfunc second() returns void { return }\n";
        let formatted = format_source(source, FormatOptions::default()).unwrap();

        assert!(formatted.contains("}\n\nfunc second"), "{formatted}");
    }

    #[test]
    fn tab_indentation_uses_one_tab_per_level_by_default() {
        let options = FormatOptions {
            indent_type: IndentType::Tab,
            indent_count: 1,
            ..FormatOptions::default()
        };
        let formatted = format_source("if true { auto value=1 }", options).unwrap();

        assert!(formatted.contains("\n\tauto value = 1\n"), "{formatted:?}");
    }

    #[test]
    fn trailing_comma_policy_adds_and_removes_only_collection_trailing_commas() {
        let source = "auto values = [1, 2,]\n";
        let never = FormatOptions {
            trailing_comma: TrailingComma::Never,
            ..FormatOptions::default()
        };
        let formatted_never = format_source(source, never).unwrap();
        assert!(formatted_never.contains("[1, 2]"), "{formatted_never}");

        let always = FormatOptions {
            trailing_comma: TrailingComma::Always,
            ..FormatOptions::default()
        };
        let formatted_always = format_source("auto values = [1, 2]\n", always).unwrap();
        assert!(formatted_always.contains("[1, 2,]"), "{formatted_always}");
        assert_eq!(
            format_source(&formatted_always, always).unwrap(),
            formatted_always
        );
    }

    #[test]
    fn default_multiline_trailing_comma_and_member_spacing_are_canonical() {
        let source = concat!(
            "class Sample {\n",
            "int first\n",
            "int second\n",
            "func one() returns void { return }\n",
            "func two() returns void { return }\n",
            "}\n",
            "auto values=[first_name, second_name]\n"
        );
        let options = FormatOptions {
            line_width: 24,
            ..FormatOptions::default()
        };
        let formatted = format_source(source, options).unwrap();

        assert!(
            formatted.contains("int first\n    int second\n\n    func one"),
            "{formatted}"
        );
        assert!(
            formatted.contains("func one() returns void {\n        return\n    }\n\n    func two"),
            "{formatted}"
        );
        assert!(formatted.contains("second_name,]"), "{formatted}");
        assert_eq!(format_source(&formatted, options).unwrap(), formatted);
    }

    #[test]
    fn trailing_commas_apply_to_maps_and_match_arms() {
        let source = concat!(
            "auto values = {1: 2, 3: 4}\n",
            "auto result = match true { true { 1 } false { 2 } }\n"
        );
        let options = FormatOptions {
            line_width: 10,
            ..FormatOptions::default()
        };
        let formatted = format_source(source, options).unwrap();

        assert!(formatted.contains("3: 4,}"), "{formatted}");
        assert!(
            formatted.contains("false {\n        2\n    },"),
            "{formatted}"
        );
        assert_eq!(format_source(&formatted, options).unwrap(), formatted);
    }

    #[test]
    fn test_annotation_comment_remains_before_its_test() {
        let source = "// mux:test timeout=10\ntest \"short\" { print(\"short\") }\n";
        let formatted = format_source(source, FormatOptions::default()).unwrap();
        assert!(formatted.contains("// mux:test timeout=10\ntest \"short\""));
    }
}
