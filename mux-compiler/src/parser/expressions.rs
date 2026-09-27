use super::*;
use crate::ast::{Precedence, SpanExt};
use crate::syntax::AstLoweringData;

/// Expression facts retained while parsing for syntax events and disambiguation.
pub(super) struct ParsedExpression {
    pub(super) span: Span,
    pub(super) event_start: usize,
    generic_target: Option<GenericTarget>,
}

/// Token range and the case bit needed to disambiguate a generic call.
struct GenericTarget {
    starts_uppercase: bool,
}

enum PrimaryToken {
    OpenParen,
    Int,
    Bool,
    None,
    Char,
    Str,
    Bytes,
    Float,
    OpenBracket,
    OpenBrace,
    Func,
    If,
    Match,
    Id,
    Other,
}

impl ParsedExpression {}

impl<'a> Parser<'a> {
    pub(super) fn parse_expression_parsed(&mut self) -> ParserResult<ParsedExpression> {
        let start = self.current;
        let event_start = self.syntax_events.len();
        let result = self.parse_precedence_parsed(Precedence::Assignment);
        if result.is_ok() {
            self.record_syntax_node(SyntaxKind::Expression, start, self.current);
        } else if self.current > start {
            self.record_syntax_node(SyntaxKind::Error, start, self.current);
        }
        result.map(|mut expression| {
            expression.event_start = event_start;
            expression
        })
    }

    fn parse_precedence_parsed(
        &mut self,
        min_precedence: Precedence,
    ) -> ParserResult<ParsedExpression> {
        let expression_start = self.current;
        let mut value = self.parse_unary_parsed()?;

        // important, do not consume the operator until after checking precedence.
        // otherwise we may consume a lower-precedence operator in a recursive call and lose it.
        while let Some(op_token) = self.peek_operator() {
            let op_precedence = self.get_operator_precedence(&op_token)?;
            if op_precedence < min_precedence {
                break;
            }

            // now it is safe to consume the operator
            let operator_span = self.peek().span;
            let _ = self.consume_operator();

            let next_precedence = if matches!(
                op_token,
                TokenType::Eq
                    | TokenType::PlusEq
                    | TokenType::MinusEq
                    | TokenType::StarEq
                    | TokenType::SlashEq
                    | TokenType::PercentEq
                    | TokenType::StarStar
            ) {
                op_precedence
            } else {
                op_precedence.next_higher()
            };

            let right = self.parse_precedence_parsed(next_precedence)?;

            let left_span = value.span;
            let right_span = right.span;
            if let (Some(operator), Some(left), Some(right)) = (
                operator_span.byte_range,
                left_span.byte_range,
                right_span.byte_range,
            ) {
                self.record_typed_syntax_node(
                    SyntaxKind::BinaryExpression,
                    expression_start,
                    self.current,
                    AstLoweringData::Binary {
                        operator,
                        left,
                        right,
                    },
                );
            }
            let combined_span = left_span.combine(&right_span);
            value = ParsedExpression {
                span: combined_span,
                event_start: 0,
                generic_target: None,
            };
            if value.span.byte_range.is_none() {
                self.record_syntax_node(
                    SyntaxKind::BinaryExpression,
                    expression_start,
                    self.current,
                );
            }
        }

        Ok(value)
    }

    pub(super) fn first_postfix_update_in(
        &self,
        range: Option<crate::lexer::ByteRange>,
        event_start: usize,
    ) -> Option<crate::lexer::ByteRange> {
        let range = range?;
        let events = &self.syntax_events[event_start..];
        let lambda_bodies = events
            .iter()
            .filter_map(|event| {
                let inside_expression =
                    event.range.start >= range.start && event.range.end <= range.end;
                match event.data.as_ref() {
                    Some(AstLoweringData::Lambda { body, .. }) if inside_expression => Some(*body),
                    _ => None,
                }
            })
            .collect::<Vec<_>>();
        events
            .iter()
            .filter(|event| {
                event.range.start >= range.start
                    && event.range.end <= range.end
                    && matches!(
                        event.data.as_ref(),
                        Some(AstLoweringData::Unary { postfix: true, .. })
                    )
                    && !events.iter().any(|statement| {
                        matches!(
                            statement.data.as_ref(),
                            Some(AstLoweringData::ExpressionStatement { expression, .. })
                                if *expression == event.range
                        )
                    })
                    && !lambda_bodies
                        .iter()
                        .any(|body| event.range.start >= body.start && event.range.end <= body.end)
            })
            .map(|event| event.range)
            .min_by_key(|update| (update.start, update.end))
    }

    pub(super) fn span_for_byte_range(&self, range: crate::lexer::ByteRange) -> Span {
        let start = self.tokens.iter().find_map(|token| {
            token
                .span
                .byte_range
                .is_some_and(|token_range| token_range.start == range.start)
                .then_some(token.span)
        });
        let end = self.tokens.iter().find_map(|token| {
            token
                .span
                .byte_range
                .is_some_and(|token_range| token_range.end == range.end)
                .then_some(token.span)
        });
        match (start, end) {
            (Some(start), Some(end)) => start.combine(&end),
            (Some(_), None) | (None, Some(_)) => Span::new(range.start, range.end),
            (None, None) => Span::new(range.start, range.end),
        }
    }

    fn parse_unary_parsed(&mut self) -> ParserResult<ParsedExpression> {
        let start = self.current;
        if let Some(op_token) = self.consume_if_unary_operator() {
            // Reject prefix ++ and --
            if matches!(op_token.token_type, TokenType::Incr | TokenType::Decr) {
                return Err(ParserError::with_help(
                    DiagnosticCode::ParseExpectedToken,
                    "Increment/Decrement operator can only be used in the postfix position",
                    op_token.span,
                    "Place the operator after the variable: 'x++' or 'x--' instead of '++x' or '--x'",
                ));
            }
            let expr = self.parse_precedence_parsed(Precedence::Unary)?;
            let expr_span = expr.span;
            if let (Some(operator), Some(operand)) =
                (op_token.span.byte_range, expr_span.byte_range)
            {
                self.record_typed_syntax_node(
                    SyntaxKind::UnaryExpression,
                    start,
                    self.current,
                    AstLoweringData::Unary {
                        operator,
                        operand,
                        postfix: false,
                    },
                );
            }
            let span = op_token.span.combine(&expr_span);
            if span.byte_range.is_none() {
                self.record_syntax_node(SyntaxKind::UnaryExpression, start, self.current);
            }
            Ok(ParsedExpression {
                span,
                event_start: 0,
                generic_target: None,
            })
        } else {
            let expr = self.parse_primary_parsed()?;
            self.parse_postfix_operators_parsed(expr)
        }
    }

    fn parse_collection_literal_parsed(
        &mut self,
        start_span: Span,
    ) -> ParserResult<ParsedExpression> {
        let start = self.token_index_for_span(start_span);
        let result = self.parse_collection_literal_inner_parsed(start_span);
        if let Ok(expression) = &result {
            let data = expression.span.byte_range.and_then(|range| {
                self.syntax_events.iter().rev().find_map(|event| {
                    (event.range == range)
                        .then_some(event.data.as_ref())
                        .flatten()
                })
            });
            let kind = match data {
                Some(AstLoweringData::Map { .. }) => SyntaxKind::MapLiteral,
                Some(AstLoweringData::Set { .. }) => SyntaxKind::SetLiteral,
                _ => SyntaxKind::Delimited,
            };
            let end =
                self.matching_delimiter_end(start, TokenType::OpenBrace, TokenType::CloseBrace);
            self.record_syntax_node(kind, start, end);
        }
        result
    }

    fn parse_collection_literal_inner_parsed(
        &mut self,
        start_span: Span,
    ) -> ParserResult<ParsedExpression> {
        self.skip_newlines();

        if self.check(TokenType::CloseBrace) {
            return self.parse_empty_set_parsed(start_span);
        }
        // No key expression can begin with `:`, so no lookahead past it is needed.
        if self.check(TokenType::Colon) {
            return self.parse_empty_map_parsed(start_span);
        }

        let first_expr = self.parse_expression_parsed()?;
        if self.matches(&[TokenType::Colon]) {
            self.parse_map_literal_parsed(start_span, first_expr)
        } else {
            self.parse_set_literal_parsed(start_span, first_expr)
        }
    }

    fn parse_empty_set_parsed(&mut self, start_span: Span) -> ParserResult<ParsedExpression> {
        let start = self.token_index_for_span(start_span);
        let end_span =
            self.consume_token(TokenType::CloseBrace, "Expected '}' after collection")?;
        self.record_typed_syntax_node(
            SyntaxKind::SetLiteral,
            start,
            self.current,
            AstLoweringData::Set {
                elements: Vec::new(),
            },
        );
        let span = start_span.combine(&end_span);
        Ok(ParsedExpression {
            span,
            event_start: 0,
            generic_target: None,
        })
    }

    fn parse_empty_map_parsed(&mut self, start_span: Span) -> ParserResult<ParsedExpression> {
        let start = self.token_index_for_span(start_span);
        self.consume_token(TokenType::Colon, "Expected ':' in empty map literal")?;
        self.skip_newlines();
        let end_span = self.consume_token(TokenType::CloseBrace, "Expected '}' after '{:'")?;
        self.record_typed_syntax_node(
            SyntaxKind::MapLiteral,
            start,
            self.current,
            AstLoweringData::Map {
                entries: Vec::new(),
                inferred_type_span: start_span.byte_range.expect("empty map opening range"),
            },
        );
        let span = start_span.combine(&end_span);
        Ok(ParsedExpression {
            span,
            event_start: 0,
            generic_target: None,
        })
    }

    fn parse_map_literal_parsed(
        &mut self,
        start_span: Span,
        first_key: ParsedExpression,
    ) -> ParserResult<ParsedExpression> {
        let first_value = self.parse_expression_parsed()?;
        let mut syntax_entries = vec![(
            first_key.span.byte_range.expect("map key has source range"),
            first_value
                .span
                .byte_range
                .expect("map value has source range"),
        )];
        self.parse_collection_entries_parsed(&mut syntax_entries)?;
        let end_span =
            self.consume_token(TokenType::CloseBrace, "Expected '}' after collection")?;
        self.record_typed_syntax_node(
            SyntaxKind::MapLiteral,
            self.token_index_for_span(start_span),
            self.current,
            AstLoweringData::Map {
                entries: syntax_entries,
                inferred_type_span: start_span.byte_range.expect("map opening range"),
            },
        );
        let span = start_span.combine(&end_span);
        Ok(ParsedExpression {
            span,
            event_start: 0,
            generic_target: None,
        })
    }

    fn parse_set_literal_parsed(
        &mut self,
        start_span: Span,
        first_elem: ParsedExpression,
    ) -> ParserResult<ParsedExpression> {
        let mut syntax_elements = vec![
            first_elem
                .span
                .byte_range
                .expect("set element has source range"),
        ];
        self.parse_set_entries_parsed(&mut syntax_elements)?;
        let end_span =
            self.consume_token(TokenType::CloseBrace, "Expected '}' after collection")?;
        self.record_typed_syntax_node(
            SyntaxKind::SetLiteral,
            self.token_index_for_span(start_span),
            self.current,
            AstLoweringData::Set {
                elements: syntax_elements,
            },
        );
        let span = start_span.combine(&end_span);
        Ok(ParsedExpression {
            span,
            event_start: 0,
            generic_target: None,
        })
    }

    fn parse_set_entries_parsed(
        &mut self,
        syntax_elements: &mut Vec<crate::lexer::ByteRange>,
    ) -> ParserResult<()> {
        loop {
            self.skip_newlines();
            if !self.has_comma_or_entry()? {
                break;
            }
            let elem = self.parse_expression_parsed()?;
            syntax_elements.push(elem.span.byte_range.expect("set element has source range"));
        }
        Ok(())
    }

    fn parse_collection_entries_parsed(
        &mut self,
        syntax_entries: &mut Vec<(crate::lexer::ByteRange, crate::lexer::ByteRange)>,
    ) -> ParserResult<()> {
        loop {
            self.skip_newlines();
            if !self.has_comma_or_entry()? {
                break;
            }
            let key = self.parse_expression_parsed()?;
            self.consume_token(TokenType::Colon, "Expected ':' after map key")?;
            let value = self.parse_expression_parsed()?;
            syntax_entries.push((
                key.span.byte_range.expect("map key has source range"),
                value.span.byte_range.expect("map value has source range"),
            ));
        }
        Ok(())
    }

    pub(super) fn has_comma_or_entry(&mut self) -> ParserResult<bool> {
        if self.matches(&[TokenType::Comma]) {
            self.skip_newlines();
            if self.check(TokenType::CloseBrace) {
                return Ok(false);
            }
            return Ok(true);
        }
        self.skip_newlines_after_comma()
    }

    pub(super) fn skip_newlines_after_comma(&mut self) -> ParserResult<bool> {
        let mut i = 0;
        let mut found_newlines = false;
        while let Some(t) = self.peek_ahead(i) {
            if t.token_type == TokenType::NewLine {
                found_newlines = true;
                i += 1;
            } else {
                break;
            }
        }
        if found_newlines
            && self
                .peek_ahead(i)
                .is_some_and(|t| t.token_type == TokenType::Comma)
            && self.peek_ahead(i + 1).is_some_and(|t| {
                !matches!(
                    t.token_type,
                    TokenType::CloseBrace | TokenType::NewLine | TokenType::Comma
                )
            })
        {
            for _ in 0..=i {
                self.advance();
            }
            self.skip_newlines();
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn parse_list_literal_expression_parsed(
        &mut self,
        start_span: Span,
    ) -> ParserResult<ParsedExpression> {
        let start = self.current.saturating_sub(1);
        let result = self.parse_list_literal_expression_inner_parsed(start_span);
        if result.is_ok() {
            let end =
                self.matching_delimiter_end(start, TokenType::OpenBracket, TokenType::CloseBracket);
            self.record_syntax_node(SyntaxKind::ListLiteral, start, end);
        }
        result
    }

    fn parse_list_literal_expression_inner_parsed(
        &mut self,
        start_span: Span,
    ) -> ParserResult<ParsedExpression> {
        let mut element_ranges = Vec::new();

        self.skip_newlines();

        if !self.check(TokenType::CloseBracket) {
            loop {
                let element = self.parse_expression_parsed()?;
                element_ranges.push(
                    element
                        .span
                        .byte_range
                        .expect("list element has source range"),
                );
                self.skip_newlines();

                if self.matches(&[TokenType::Comma]) {
                    self.skip_newlines();
                    if self.check(TokenType::CloseBracket) {
                        break;
                    }
                } else if !self.consume_newline_delimited_comma() {
                    break;
                }
            }
        }

        self.skip_newlines();

        let end_span = self.consume_list_literal_close_span()?;
        self.record_typed_syntax_node(
            SyntaxKind::ListLiteral,
            self.token_index_for_span(start_span),
            self.current,
            AstLoweringData::List {
                elements: element_ranges,
            },
        );
        let span = start_span.combine(&end_span);
        Ok(ParsedExpression {
            span,
            event_start: 0,
            generic_target: None,
        })
    }

    pub(super) fn consume_list_literal_close_span(&mut self) -> ParserResult<Span> {
        self.consume_token(TokenType::CloseBracket, "Expected ']' after list elements")
    }

    pub(super) fn consume_newline_delimited_comma(&mut self) -> bool {
        let mut i = 0;
        let mut found_newlines = false;
        while let Some(t) = self.peek_ahead(i) {
            if t.token_type == TokenType::NewLine {
                found_newlines = true;
                i += 1;
            } else {
                break;
            }
        }

        if !found_newlines {
            return false;
        }

        let comma_after_newlines = self
            .peek_ahead(i)
            .is_some_and(|t| t.token_type == TokenType::Comma);
        let has_expression_after_comma = self.peek_ahead(i + 1).is_some_and(|t| {
            !matches!(
                t.token_type,
                TokenType::CloseBracket | TokenType::NewLine | TokenType::Comma
            )
        });

        if !(comma_after_newlines && has_expression_after_comma) {
            return false;
        }

        for _ in 0..=i {
            self.advance();
        }
        self.skip_newlines();
        true
    }

    fn parse_lambda_expression_parsed(
        &mut self,
        start_span: Span,
    ) -> ParserResult<ParsedExpression> {
        let lambda_start = self.token_index_for_span(start_span);
        self.consume_token(TokenType::OpenParen, "Expected '(' after 'func' in lambda")?;
        let parameters_start = self.syntax_events.len();

        if !self.check(TokenType::CloseParen) {
            loop {
                let parameter_start = self.current;
                let param_type = self.parse_type_fact()?;
                let type_range = param_type
                    .source_range()
                    .expect("lambda parameter type source range");
                self.consume_identifier_fact("Expected parameter name")?;
                let name_range = self
                    .previous()
                    .span
                    .byte_range
                    .expect("lambda parameter name source range");
                if self.matches(&[TokenType::Eq]) {
                    return Err(ParserError::with_help(
                        DiagnosticCode::ParseExpectedToken,
                        "Default arguments are not supported in lambda expressions",
                        self.previous().span,
                        "Lambda parameters cannot have default values. Define a named function instead if you need default parameters.",
                    ));
                }
                self.record_typed_syntax_node(
                    SyntaxKind::Parameter,
                    parameter_start,
                    self.current,
                    AstLoweringData::Parameter {
                        name: name_range,
                        type_range,
                        default_value: None,
                    },
                );
                if !self.matches(&[TokenType::Comma]) {
                    break;
                }
                self.skip_newlines();
            }
        }
        let parameter_ranges = self.syntax_ranges_since(parameters_start, |data| {
            matches!(data, AstLoweringData::Parameter { .. })
        });

        self.consume_token(TokenType::CloseParen, "Expected ')' after parameters")?;

        let where_start = self.syntax_events.len();
        let where_clause_fact = self.parse_where_clause_fact()?;
        let where_range = self.last_syntax_range_since(where_start, |data| {
            matches!(data, AstLoweringData::WhereClause { .. })
        });
        if where_clause_fact.is_some() {
            self.skip_newlines();
        }

        let return_type_fact = if self.matches(&[TokenType::Returns]) {
            self.parse_type_fact()?
        } else {
            return Err(ParserError::with_help(
                DiagnosticCode::ParseExpectedToken,
                format!(
                    "Expected 'returns' after lambda parameters, found {}",
                    Self::describe_token(&self.peek().token_type)
                ),
                self.peek().span,
                "Lambda expressions require an explicit return type. Example: func(int x) returns int { return x + 1 }",
            ));
        };
        let return_type_range = return_type_fact
            .source_range()
            .expect("lambda return type source range");

        let body = self.block()?;
        let body_range = body.range;
        let start_range = start_span.byte_range.expect("lambda start source range");
        let ast_span = ByteRange::new(start_range.start, body_range.end);
        let span = Span::new(ast_span.start, ast_span.end);
        self.record_typed_syntax_node(
            SyntaxKind::LambdaExpression,
            lambda_start,
            self.current,
            AstLoweringData::Lambda {
                parameters: parameter_ranges,
                return_type: return_type_range,
                where_clause: where_range,
                body: body_range,
                ast_span,
            },
        );
        Ok(ParsedExpression {
            span,
            event_start: 0,
            generic_target: None,
        })
    }

    fn parse_if_expression_parsed(&mut self, token_span: Span) -> ParserResult<ParsedExpression> {
        let expression_start = self.token_index_for_span(token_span);
        let cond = self.parse_expression_parsed()?;
        let condition_range = cond
            .span
            .byte_range
            .expect("if expression condition source range");
        let then_expr = self.parse_if_branch_expression_parsed("then expression")?;
        let then_range = then_expr
            .span
            .byte_range
            .expect("if expression then branch source range");
        // Allow newlines around `else` (`}\nelse` and `else\nif`), matching the
        // statement-form if. An if-expression always requires an else, so skipping
        // to it is unambiguous.
        self.skip_newlines();
        self.consume_token(TokenType::Else, "Expected 'else' after then branch")?;
        self.skip_newlines();
        // `else if ...` chains into a nested if-expression; a bare `else` takes a
        // block. The nested If becomes this expression's else branch, so no new AST
        // shape is needed.
        let else_expr = if self.check(TokenType::If) {
            let if_span = self.advance().span;
            self.parse_if_expression_parsed(if_span)?
        } else {
            self.parse_if_branch_expression_parsed("else expression")?
        };
        let else_range = else_expr
            .span
            .byte_range
            .expect("if expression else branch source range");
        let span = token_span.combine(&self.previous().span);
        self.record_typed_syntax_node(
            SyntaxKind::IfExpression,
            expression_start,
            self.current,
            AstLoweringData::IfExpression {
                condition: condition_range,
                then_expression: then_range,
                else_expression: else_range,
                ast_span: span.byte_range.expect("if expression AST span range"),
            },
        );
        Ok(ParsedExpression {
            span,
            event_start: 0,
            generic_target: None,
        })
    }

    fn parse_match_expression_parsed(
        &mut self,
        token_span: Span,
    ) -> ParserResult<ParsedExpression> {
        let match_start = self.token_index_for_span(token_span);
        let expr = self.parse_expression_parsed()?;
        let expression_range = expr
            .span
            .byte_range
            .expect("match expression value source range");
        if let Some(range) = self.first_postfix_update_in(expr.span.byte_range, expr.event_start) {
            return Err(ParserError::with_help(
                DiagnosticCode::ParseExpectedToken,
                "Increment/Decrement operator can only be used as a standalone statement",
                self.span_for_byte_range(range),
                "Expressions like 'x + y++' are not supported. Use 'y++' as a separate statement before the expression.",
            ));
        }
        self.consume_token(TokenType::OpenBrace, "Expected '{' after match expression")?;
        self.skip_newlines();
        let mut arm_ranges = Vec::new();
        while !self.check(TokenType::CloseBrace) && !self.is_at_end() {
            let arm_start = self.current;
            let pattern_events_start = self.syntax_events.len();
            self.parse_pattern_fact()?;
            let pattern_range = self
                .last_syntax_range_since(pattern_events_start, |data| {
                    matches!(data, AstLoweringData::Pattern(_))
                })
                .expect("match expression pattern range");
            let guard = if self.matches(&[TokenType::If]) {
                Some(self.parse_expression_parsed()?)
            } else {
                None
            };
            let guard_range = guard.as_ref().and_then(|guard| guard.span.byte_range);
            self.skip_newlines();
            self.consume_token(TokenType::OpenBrace, "Expected '{' before match arm value")?;
            self.skip_newlines();
            let (body_range, body_is_expression) = if self.matches(&[TokenType::Return]) {
                let body_events_start = self.syntax_events.len();
                self.return_statement()?;
                let body_range = self
                    .last_statement_range_since(body_events_start)
                    .expect("match expression return body range");
                (body_range, false)
            } else {
                let value = self.parse_expression_parsed()?;
                let body_range = value
                    .span
                    .byte_range
                    .expect("match expression arm value range");
                (body_range, true)
            };
            self.skip_newlines();
            self.consume_token(TokenType::CloseBrace, "Expected '}' after match arm value")?;
            let arm_range = self
                .source_range_for_tokens(arm_start, self.current)
                .expect("match expression arm source range");
            self.record_typed_syntax_node(
                SyntaxKind::MatchArm,
                arm_start,
                self.current,
                AstLoweringData::MatchArm {
                    pattern: pattern_range,
                    guard: guard_range,
                    body: body_range,
                    body_is_expression,
                },
            );
            arm_ranges.push(arm_range);
            self.skip_newlines();
            if self.matches(&[TokenType::Comma]) {
                self.skip_newlines();
            }
        }
        let end_span =
            self.consume_token(TokenType::CloseBrace, "Expected '}' after match arms")?;
        let span = token_span.combine(&end_span);
        self.record_typed_syntax_node(
            SyntaxKind::MatchExpression,
            match_start,
            self.current,
            AstLoweringData::MatchExpression {
                expression: expression_range,
                arms: arm_ranges,
                ast_span: span.byte_range.expect("match expression AST span range"),
            },
        );
        Ok(ParsedExpression {
            span,
            event_start: 0,
            generic_target: None,
        })
    }

    /// Parse one branch of an if-expression: `{ <expr> }`. Newlines are allowed
    /// around the value expression so a branch can span multiple lines; the branch
    /// is still a single value, not a statement block.
    fn parse_if_branch_expression_parsed(&mut self, what: &str) -> ParserResult<ParsedExpression> {
        self.consume_token(
            TokenType::OpenBrace,
            &format!("Expected '{{' before {what}"),
        )?;
        self.skip_newlines();
        let expr = self.parse_expression_parsed()?;
        self.skip_newlines();
        self.consume_token(
            TokenType::CloseBrace,
            &format!("Expected '}}' after {what}"),
        )?;
        Ok(expr)
    }

    pub(super) fn unexpected_primary_error(
        &self,
        token_type: TokenType,
        token_span: Span,
    ) -> ParserError {
        let token_desc = Self::describe_token(&token_type);

        if matches!(token_type, TokenType::Return) {
            return ParserError::with_help(
                DiagnosticCode::ParseExpectedToken,
                "Unexpected 'return' statement",
                token_span,
                "'return' can only be used inside a function body",
            );
        }

        ParserError::from_token(
            DiagnosticCode::ParseExpectedExpression,
            format!("Expected expression, found {token_desc}"),
            &Token {
                token_type,
                span: token_span,
            },
        )
    }

    fn parse_primary_parsed(&mut self) -> ParserResult<ParsedExpression> {
        if self.is_at_end() {
            // Use the last token's span to show where we expected an expression
            let error_span = self
                .tokens
                .last()
                .map_or_else(|| Span::empty(0), |t| t.span);
            return Err(ParserError::new(
                DiagnosticCode::ParseExpectedExpression,
                "Expected expression, found end of input",
                error_span,
            ));
        }

        let token_index = self.current;
        let token = self.consume();
        let token_span = token.span;
        let token_type = match &token.token_type {
            TokenType::OpenParen => PrimaryToken::OpenParen,
            TokenType::Int(_) => PrimaryToken::Int,
            TokenType::Bool(_) => PrimaryToken::Bool,
            TokenType::None => PrimaryToken::None,
            TokenType::Char(_) => PrimaryToken::Char,
            TokenType::Str(_) => PrimaryToken::Str,
            TokenType::Bytes(_) => PrimaryToken::Bytes,
            TokenType::Float(_) => PrimaryToken::Float,
            TokenType::OpenBracket => PrimaryToken::OpenBracket,
            TokenType::OpenBrace => PrimaryToken::OpenBrace,
            TokenType::Func => PrimaryToken::Func,
            TokenType::If => PrimaryToken::If,
            TokenType::Match => PrimaryToken::Match,
            TokenType::Id(_) => PrimaryToken::Id,
            _ => PrimaryToken::Other,
        };

        let result = match token_type {
            PrimaryToken::OpenParen => {
                self.parse_parenthesized_or_tuple_expression_parsed(token_span)
            }
            PrimaryToken::Int
            | PrimaryToken::Bool
            | PrimaryToken::None
            | PrimaryToken::Char
            | PrimaryToken::Str
            | PrimaryToken::Bytes
            | PrimaryToken::Float => self.parse_scalar_primary(token_span, None),
            PrimaryToken::OpenBracket => self.parse_list_literal_expression_parsed(token_span),
            PrimaryToken::OpenBrace => self.parse_collection_literal_parsed(token_span),
            PrimaryToken::Func => self.parse_lambda_expression_parsed(token_span),
            PrimaryToken::If => self.parse_if_expression_parsed(token_span),
            PrimaryToken::Match => self.parse_match_expression_parsed(token_span),
            PrimaryToken::Id => {
                let TokenType::Id(name) = &self.tokens[token_index].token_type else {
                    unreachable!("primary identifier token changed during parsing")
                };
                let generic_target = Some(GenericTarget {
                    starts_uppercase: name.chars().next().is_some_and(|c| c.is_ascii_uppercase()),
                });
                self.parse_scalar_primary(token_span, generic_target)
            }
            PrimaryToken::Other => Err(self
                .unexpected_primary_error(self.tokens[token_index].token_type.clone(), token_span)),
        };
        if result.is_ok()
            && let Some(range) = token_span.byte_range
        {
            let data = match &self.tokens[token_index].token_type {
                TokenType::Id(_) => Some(AstLoweringData::Name { token: range }),
                TokenType::Int(_)
                | TokenType::Float(_)
                | TokenType::Bool(_)
                | TokenType::Char(_)
                | TokenType::Str(_)
                | TokenType::Bytes(_)
                | TokenType::None => Some(AstLoweringData::Literal { token: range }),
                _ => None,
            };
            if let Some(data) = data {
                self.record_typed_syntax_node(
                    SyntaxKind::Expression,
                    token_index,
                    token_index + 1,
                    data,
                );
            }
        }
        result
    }

    fn parse_scalar_primary(
        &mut self,
        span: Span,
        generic_target: Option<GenericTarget>,
    ) -> ParserResult<ParsedExpression> {
        Ok(ParsedExpression {
            span,
            event_start: 0,
            generic_target,
        })
    }

    fn parse_parenthesized_or_tuple_expression_parsed(
        &mut self,
        start_span: Span,
    ) -> ParserResult<ParsedExpression> {
        let syntax_start = self.token_index_for_span(start_span);
        self.skip_newlines();

        if self.check(TokenType::CloseParen) {
            self.consume_token(TokenType::CloseParen, "Expected ')' after expression")?;
            return Err(ParserError::with_help(
                DiagnosticCode::ParseExpectedToken,
                "Tuple must have exactly 2 elements, found empty parentheses",
                start_span.combine(&self.previous().span),
                "Tuples are created with two elements: (value1, value2). Example: auto pair = (1, \"hello\")",
            ));
        }

        let first = self.parse_expression_parsed()?;
        self.skip_newlines();

        if self.matches(&[TokenType::Comma]) {
            self.skip_newlines();
            if self.check(TokenType::CloseParen) {
                self.consume_token(TokenType::CloseParen, "Expected ')' after tuple elements")?;
                return Err(ParserError::with_help(
                    DiagnosticCode::ParseExpectedToken,
                    "Tuple must have exactly 2 elements, found only 1",
                    start_span.combine(&self.previous().span),
                    "Tuples require exactly two elements: (value1, value2). A trailing comma after a single value is not allowed.",
                ));
            }

            let second = self.parse_expression_parsed()?;
            let first_range = first
                .span
                .byte_range
                .expect("parsed expression has source range");
            let second_range = second
                .span
                .byte_range
                .expect("parsed expression has source range");
            self.skip_newlines();
            let close_span =
                self.consume_token(TokenType::CloseParen, "Expected ')' after tuple elements")?;
            let span = start_span.combine(&close_span);
            self.record_typed_syntax_node(
                SyntaxKind::TupleExpression,
                syntax_start,
                self.current,
                AstLoweringData::Tuple {
                    first: first_range,
                    second: second_range,
                },
            );
            return Ok(ParsedExpression {
                span,
                event_start: 0,
                generic_target: None,
            });
        }

        let expression_range = first
            .span
            .byte_range
            .expect("parsed expression has source range");
        self.consume_token(TokenType::CloseParen, "Expected ')' after expression")?;
        self.record_typed_syntax_node(
            SyntaxKind::ParenthesizedExpression,
            syntax_start,
            self.current,
            AstLoweringData::Parenthesized {
                expression: expression_range,
            },
        );
        Ok(first)
    }

    pub(super) fn find_matching_generic_gt_index(&self) -> Option<usize> {
        let mut i = self.current + 1;
        let mut depth = 1usize;
        while i < self.tokens.len() {
            match self.tokens[i].token_type {
                TokenType::Lt => depth += 1,
                TokenType::Gt => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                TokenType::Eof
                | TokenType::NewLine
                | TokenType::OpenBrace
                | TokenType::CloseBrace => {
                    return None;
                }
                _ => {}
            }
            i += 1;
        }
        None
    }

    pub(super) fn should_consume_generics_for_target(
        &self,
        target_starts_uppercase: bool,
        gt_idx: usize,
    ) -> bool {
        if let Some(next) = self.tokens.get(gt_idx + 1) {
            matches!(
                next.token_type,
                TokenType::Dot
                    | TokenType::OpenParen
                    | TokenType::CloseParen
                    | TokenType::Eq
                    | TokenType::NewLine
                    | TokenType::Eof
            ) || target_starts_uppercase
        } else {
            true
        }
    }

    fn parse_generic_postfix_parsed(
        &mut self,
        expr: &ParsedExpression,
    ) -> ParserResult<Option<ParsedExpression>> {
        let Some(target_fact) = expr.generic_target.as_ref() else {
            return Ok(None);
        };
        let Some(gt_idx) = self.find_matching_generic_gt_index() else {
            return Ok(None);
        };
        if !self.should_consume_generics_for_target(target_fact.starts_uppercase, gt_idx) {
            return Ok(None);
        }

        let arguments_start = self.current;
        let _ = self.matches(&[TokenType::Lt]);
        let type_arg_facts = self.parse_type_argument_facts()?;
        let end_span = self.consume_token(TokenType::Gt, "Expected '>' after type arguments")?;
        let target = expr
            .span
            .byte_range
            .expect("generic target has source range");
        let arguments = type_arg_facts
            .iter()
            .map(|argument| {
                argument
                    .source_range()
                    .expect("generic type argument has source range")
            })
            .collect();
        let syntax_start = self.token_index_for_span(expr.span);
        self.record_typed_syntax_node(
            SyntaxKind::Expression,
            syntax_start,
            self.current,
            AstLoweringData::Generic { target, arguments },
        );
        self.record_syntax_node(SyntaxKind::TypeArguments, arguments_start, self.current);
        let span = expr.span.combine(&end_span);
        Ok(Some(ParsedExpression {
            span,
            event_start: 0,
            generic_target: None,
        }))
    }

    fn parse_postfix_operators_parsed(
        &mut self,
        mut expr: ParsedExpression,
    ) -> ParserResult<ParsedExpression> {
        loop {
            if self.matches(&[TokenType::OpenParen]) {
                let start = self.current.saturating_sub(1);
                let callee_range = expr.span.byte_range.expect("call target has source range");
                let callee_start = self.token_index_for_span(expr.span);
                let mut argument_ranges = Vec::new();
                if !self.check(TokenType::CloseParen) {
                    loop {
                        self.skip_newlines();
                        let argument = self.parse_expression_parsed()?;
                        argument_ranges.push(
                            argument
                                .span
                                .byte_range
                                .expect("call argument has source range"),
                        );
                        self.skip_newlines();
                        if !self.matches(&[TokenType::Comma]) {
                            break;
                        }
                        self.skip_newlines();
                        if self.check(TokenType::CloseParen) {
                            break;
                        }
                    }
                }
                let end_span =
                    self.consume_token(TokenType::CloseParen, "Expected ')' after arguments")?;
                self.record_typed_syntax_node(
                    SyntaxKind::Expression,
                    callee_start,
                    self.current,
                    AstLoweringData::Call {
                        callee: callee_range,
                        arguments: argument_ranges,
                    },
                );
                self.record_syntax_node(SyntaxKind::CallArguments, start, self.current);
                let span = expr.span.combine(&end_span);
                expr = ParsedExpression {
                    span,
                    event_start: 0,
                    generic_target: None,
                };
                continue;
            }

            if self.matches(&[TokenType::Dot]) {
                let base_range = expr.span.byte_range.expect("field base has source range");
                let start = self.token_index_for_span(expr.span);
                self.consume_identifier_fact("Expected field name after '.'")?;
                let field_span = self.tokens[self.current - 1].span;
                if let Some(field_range) = field_span.byte_range {
                    self.record_typed_syntax_node(
                        SyntaxKind::Expression,
                        start,
                        self.current,
                        AstLoweringData::FieldAccess {
                            base: base_range,
                            field: field_range,
                        },
                    );
                }
                let span = expr.span.combine(&field_span);
                let generic_target = expr.generic_target.take();
                expr = ParsedExpression {
                    span,
                    event_start: 0,
                    generic_target,
                };
                continue;
            }

            if self.matches(&[TokenType::OpenBracket]) {
                let start = self.current.saturating_sub(1);
                let base_range = expr.span.byte_range.expect("index base has source range");
                let base_span = expr.span;
                let syntax_start = self.token_index_for_span(base_span);
                if self.matches(&[TokenType::Colon]) {
                    expr = self.finish_slice_parsed(base_span, None)?;
                } else {
                    let index = self.parse_expression_parsed()?;
                    if self.matches(&[TokenType::Colon]) {
                        expr = self.finish_slice_parsed(base_span, Some(index))?;
                    } else {
                        let end_span = self
                            .consume_token(TokenType::CloseBracket, "Expected ']' after index")?;
                        let index_range = index.span.byte_range.expect("index has source range");
                        self.record_typed_syntax_node(
                            SyntaxKind::Expression,
                            syntax_start,
                            self.current,
                            AstLoweringData::Index {
                                base: base_range,
                                index: index_range,
                            },
                        );
                        expr = ParsedExpression {
                            span: base_span.combine(&end_span),
                            event_start: 0,
                            generic_target: None,
                        };
                    }
                }
                let end = self.matching_delimiter_end(
                    start,
                    TokenType::OpenBracket,
                    TokenType::CloseBracket,
                );
                self.record_syntax_node(SyntaxKind::IndexExpression, start, end);
                continue;
            }

            if self.check(TokenType::Lt) {
                if let Some(next) = self.parse_generic_postfix_parsed(&expr)? {
                    expr = next;
                    continue;
                }
                break;
            }

            if self.matches(&[TokenType::Incr]) || self.matches(&[TokenType::Decr]) {
                let op_span = self.previous().span;
                let operand = expr.span;
                if let (Some(operand), Some(operator)) = (operand.byte_range, op_span.byte_range) {
                    self.record_typed_syntax_node(
                        SyntaxKind::UnaryExpression,
                        self.token_index_for_span(expr.span),
                        self.current,
                        AstLoweringData::Unary {
                            operator,
                            operand,
                            postfix: true,
                        },
                    );
                }
                let span = expr.span.combine(&op_span);
                expr = ParsedExpression {
                    span,
                    event_start: 0,
                    generic_target: None,
                };
                continue;
            }

            if self.skip_newline_gap_for_postfix() {
                continue;
            }
            break;
        }
        Ok(expr)
    }

    fn finish_slice_parsed(
        &mut self,
        base_span: Span,
        start: Option<ParsedExpression>,
    ) -> ParserResult<ParsedExpression> {
        let base_range = base_span.byte_range.expect("slice base has source range");
        let slice_start = self.token_index_for_span(base_span);
        let start_range = start
            .as_ref()
            .and_then(|expression| expression.span.byte_range);
        let end = if self.check(TokenType::CloseBracket) {
            None
        } else {
            Some(self.parse_expression_parsed()?)
        };
        let end_range = end
            .as_ref()
            .and_then(|expression| expression.span.byte_range);
        let end_span = self.consume_token(TokenType::CloseBracket, "Expected ']' after slice")?;
        let span = base_span.combine(&end_span);
        self.record_typed_syntax_node(
            SyntaxKind::SliceExpression,
            slice_start,
            self.current,
            AstLoweringData::Slice {
                base: base_range,
                start: start_range,
                end: end_range,
            },
        );
        Ok(ParsedExpression {
            span,
            event_start: 0,
            generic_target: None,
        })
    }

    pub(super) fn skip_newline_gap_for_postfix(&mut self) -> bool {
        let mut lookahead = self.current;
        let mut found_newlines = false;
        while let Some(token) = self.tokens.get(lookahead) {
            if token.token_type == TokenType::NewLine {
                found_newlines = true;
                lookahead += 1;
            } else {
                break;
            }
        }

        if found_newlines && let Some(next) = self.tokens.get(lookahead) {
            match next.token_type {
                TokenType::Dot | TokenType::OpenParen | TokenType::OpenBracket => {
                    self.set_position(lookahead);
                    return true;
                }
                _ => {}
            }
        }

        false
    }

    pub(super) fn consume_operator(&mut self) -> Option<TokenType> {
        let operator = self.peek_operator()?;
        self.advance();
        Some(operator)
    }

    pub(super) fn peek_operator(&self) -> Option<TokenType> {
        if self.is_at_end() {
            return None;
        }

        match self.peek().token_type {
            TokenType::Plus
            | TokenType::Minus
            | TokenType::Star
            | TokenType::Slash
            | TokenType::Percent
            | TokenType::StarStar
            | TokenType::Eq
            | TokenType::EqEq
            | TokenType::NotEq
            | TokenType::Lt
            | TokenType::Gt
            | TokenType::Le
            | TokenType::Ge
            | TokenType::And
            | TokenType::Or
            | TokenType::In
            | TokenType::PlusEq
            | TokenType::MinusEq
            | TokenType::StarEq
            | TokenType::SlashEq
            | TokenType::PercentEq => Some(self.peek().token_type.clone()),
            _ => None,
        }
    }

    pub(super) fn consume_if_unary_operator(&mut self) -> Option<Token> {
        if self.is_at_end() {
            return None;
        }

        let token = self.peek();
        match &token.token_type {
            TokenType::Minus
            | TokenType::Use
            | TokenType::Bang
            | TokenType::Ref
            | TokenType::Incr
            | TokenType::Decr
            | TokenType::Star => {
                let token_clone = token.clone();
                self.advance();
                Some(token_clone)
            }
            _ => None,
        }
    }

    pub(super) fn get_operator_precedence(&self, op: &TokenType) -> ParserResult<Precedence> {
        let precedence = match op {
            TokenType::Eq
            | TokenType::PlusEq
            | TokenType::MinusEq
            | TokenType::StarEq
            | TokenType::SlashEq
            | TokenType::PercentEq => Precedence::Assignment,

            TokenType::Or => Precedence::Or,

            TokenType::And => Precedence::And,

            TokenType::In => Precedence::Comparison,

            TokenType::EqEq | TokenType::NotEq => Precedence::Equality,

            TokenType::Lt | TokenType::Le | TokenType::Gt | TokenType::Ge => Precedence::Comparison,

            TokenType::Plus | TokenType::Minus => Precedence::Term,

            TokenType::Star | TokenType::Slash | TokenType::Percent => Precedence::Factor,

            TokenType::StarStar => Precedence::Exponent,
            _ => {
                return Err(ParserError::new(
                    DiagnosticCode::ParseExpectedToken,
                    "Expected binary operator",
                    self.peek().span,
                ));
            }
        };
        Ok(precedence)
    }
}

#[cfg(test)]
mod tests {
    use crate::ast::{AstNode, ExpressionKind, StatementKind};
    use crate::syntax::parse_source;

    fn parse_test_expression(expression: &str) -> (crate::ast::ExpressionNode, String) {
        let source = format!("test \"parser expression\" {{\n{expression}\n}}");
        let parsed = parse_source(&source);
        let nodes = parsed
            .lower()
            .unwrap_or_else(|error| panic!("expression fixture should parse: {error:?}"));
        let [AstNode::Test { body, .. }] = nodes.as_slice() else {
            panic!("expected one lowered test block");
        };
        let Some(StatementKind::Expression(expression)) =
            body.first().map(|statement| &statement.kind)
        else {
            panic!("expected one expression statement");
        };
        (expression.clone(), source)
    }

    #[test]
    fn binary_operator_span_points_to_operator_token() {
        let (expression, source) = parse_test_expression("1 + 2");

        let ExpressionKind::Binary { op_span, .. } = expression.kind else {
            panic!("expected binary expression");
        };
        let operator_range = op_span
            .byte_range
            .expect("lowered operator has a byte range");
        assert_eq!(&source[operator_range.start..operator_range.end], "+");
    }

    #[test]
    fn generic_target_keeps_all_qualified_field_segments() {
        let (expression, _) = parse_test_expression("module.factory.Builder<int>(item)");

        assert!(matches!(
            &expression.kind,
            ExpressionKind::Call { func, .. }
                if matches!(&func.kind, ExpressionKind::GenericType(name, _)
                    if name == "module.factory.Builder")
        ));
    }
}
