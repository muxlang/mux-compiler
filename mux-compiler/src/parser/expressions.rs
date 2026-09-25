use super::*;
use crate::syntax::SyntaxData;

impl<'a> Parser<'a> {
    pub fn parse_expression(&mut self) -> ParserResult<ExpressionNode> {
        let start = self.current;
        let result = self.parse_precedence(Precedence::Assignment);
        if result.is_ok() {
            self.record_syntax_node(SyntaxKind::Expression, start, self.current);
        } else if self.current > start {
            self.record_syntax_node(SyntaxKind::Error, start, self.current);
        }
        result
    }

    pub(super) fn parse_precedence(
        &mut self,
        min_precedence: Precedence,
    ) -> ParserResult<ExpressionNode> {
        let expression_start = self.current;
        let mut value = self.parse_unary()?;

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

            let next_precedence = if op_token.is_right_associative() {
                op_precedence
            } else {
                op_precedence.next_higher()
            };

            let right = self.parse_precedence(next_precedence)?;

            let left_span = *value.span();
            let right_span = *right.span();
            if let (Some(operator), Some(left), Some(right)) = (
                operator_span.byte_range,
                left_span.byte_range,
                right_span.byte_range,
            ) {
                self.record_typed_syntax_node(
                    SyntaxKind::BinaryExpression,
                    expression_start,
                    self.current,
                    SyntaxData::Binary {
                        operator,
                        left,
                        right,
                    },
                );
            }
            let combined_span = left_span.combine(&right_span);
            if self.mode == ParserMode::SyntaxOnly
                && !self.contains_postfix_update(combined_span.byte_range)
            {
                // Binary structure is already represented by SyntaxData. Keep
                // the left expression only as a span carrier for parser callers;
                // syntax lowering reconstructs the binary tree from its ranges.
                value.span = combined_span;
            } else {
                let new_value = ExpressionNode {
                    kind: ExpressionKind::Binary {
                        left: Box::new(value),
                        op: op_token,
                        op_span: operator_span,
                        right: Box::new(right),
                    },
                    span: combined_span,
                };
                value = new_value;
            }
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

    fn contains_postfix_update(&self, range: Option<crate::lexer::ByteRange>) -> bool {
        let Some(range) = range else {
            return false;
        };
        self.syntax_events.iter().any(|event| {
            event.range.start >= range.start
                && event.range.end <= range.end
                && matches!(
                    event.data.as_ref(),
                    Some(SyntaxData::Unary { postfix: true, .. })
                )
        })
    }

    pub(super) fn parse_unary(&mut self) -> ParserResult<ExpressionNode> {
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
            let expr = self.parse_precedence(Precedence::Unary)?;
            let expr_span = *expr.span();
            if let (Some(operator), Some(operand)) =
                (op_token.span.byte_range, expr_span.byte_range)
            {
                self.record_typed_syntax_node(
                    SyntaxKind::UnaryExpression,
                    start,
                    self.current,
                    SyntaxData::Unary {
                        operator,
                        operand,
                        postfix: false,
                    },
                );
            }
            let span = op_token.span.combine(&expr_span);
            let op = UnaryOp::parse(&op_token)?;
            let expression = if self.mode == ParserMode::SyntaxOnly
                && !self.contains_postfix_update(span.byte_range)
            {
                // The unary syntax event already records the operator and
                // operand ranges. Keep the operand only as a span carrier;
                // syntax lowering reconstructs the unary AST from the event.
                let mut expression = expr;
                expression.span = span;
                expression
            } else {
                ExpressionNode {
                    kind: ExpressionKind::Unary {
                        op,
                        op_span: op_token.span,
                        expr: Box::new(expr),
                        postfix: false,
                    },
                    span,
                }
            };
            if expression.span.byte_range.is_none() {
                self.record_syntax_node(SyntaxKind::UnaryExpression, start, self.current);
            }
            Ok(expression)
        } else {
            let expr = self.parse_primary()?;
            self.parse_postfix_operators(expr)
        }
    }

    pub(super) fn parse_collection_literal(
        &mut self,
        start_span: Span,
    ) -> ParserResult<ExpressionNode> {
        let start = self.token_index_for_span(start_span);
        let result = self.parse_collection_literal_inner(start_span);
        if let Ok(expression) = &result {
            let kind = match &expression.kind {
                ExpressionKind::MapLiteral { .. } => SyntaxKind::MapLiteral,
                ExpressionKind::SetLiteral(_) => SyntaxKind::SetLiteral,
                _ => SyntaxKind::Delimited,
            };
            let end =
                self.matching_delimiter_end(start, TokenType::OpenBrace, TokenType::CloseBrace);
            self.record_syntax_node(kind, start, end);
        }
        result
    }

    fn parse_collection_literal_inner(&mut self, start_span: Span) -> ParserResult<ExpressionNode> {
        self.skip_newlines();

        if self.check(TokenType::CloseBrace) {
            return self.parse_empty_set(start_span);
        }
        // No key expression can begin with `:`, so no lookahead past it is needed.
        if self.check(TokenType::Colon) {
            return self.parse_empty_map(start_span);
        }

        let first_expr = self.parse_expression()?;
        if self.matches(&[TokenType::Colon]) {
            self.parse_map_literal(start_span, first_expr)
        } else {
            self.parse_set_literal(start_span, first_expr)
        }
    }

    pub(super) fn parse_empty_set(&mut self, start_span: Span) -> ParserResult<ExpressionNode> {
        let start = self.token_index_for_span(start_span);
        let end_span =
            self.consume_token(TokenType::CloseBrace, "Expected '}' after collection")?;
        self.record_typed_syntax_node(
            SyntaxKind::SetLiteral,
            start,
            self.current,
            SyntaxData::Set {
                elements: Vec::new(),
            },
        );
        Ok(ExpressionNode {
            kind: ExpressionKind::SetLiteral(vec![]),
            span: start_span.combine(&end_span),
        })
    }

    pub(super) fn parse_empty_map(&mut self, start_span: Span) -> ParserResult<ExpressionNode> {
        let start = self.token_index_for_span(start_span);
        self.consume_token(TokenType::Colon, "Expected ':' in empty map literal")?;
        self.skip_newlines();
        let end_span = self.consume_token(TokenType::CloseBrace, "Expected '}' after '{:'")?;
        self.record_typed_syntax_node(
            SyntaxKind::MapLiteral,
            start,
            self.current,
            SyntaxData::Map {
                entries: Vec::new(),
            },
        );
        Ok(ExpressionNode {
            kind: ExpressionKind::MapLiteral {
                key_type: Box::new(TypeNode {
                    kind: TypeKind::Auto,
                    span: start_span,
                }),
                value_type: Box::new(TypeNode {
                    kind: TypeKind::Auto,
                    span: start_span,
                }),
                entries: vec![],
            },
            span: start_span.combine(&end_span),
        })
    }

    pub(super) fn parse_map_literal(
        &mut self,
        start_span: Span,
        first_key: ExpressionNode,
    ) -> ParserResult<ExpressionNode> {
        self.parse_map_literal_inner(start_span, first_key)
    }

    fn parse_map_literal_inner(
        &mut self,
        start_span: Span,
        first_key: ExpressionNode,
    ) -> ParserResult<ExpressionNode> {
        let first_value = self.parse_expression()?;
        let mut entries = vec![(first_key, first_value)];
        self.parse_collection_entries(&mut entries, true)?;
        let end_span =
            self.consume_token(TokenType::CloseBrace, "Expected '}' after collection")?;
        let syntax_entries = entries
            .iter()
            .map(|(key, value)| {
                (
                    key.span.byte_range.expect("map key has source range"),
                    value.span.byte_range.expect("map value has source range"),
                )
            })
            .collect();
        self.record_typed_syntax_node(
            SyntaxKind::MapLiteral,
            self.token_index_for_span(start_span),
            self.current,
            SyntaxData::Map {
                entries: syntax_entries,
            },
        );
        Ok(ExpressionNode {
            kind: ExpressionKind::MapLiteral {
                key_type: Box::new(TypeNode {
                    kind: TypeKind::Auto,
                    span: start_span,
                }),
                value_type: Box::new(TypeNode {
                    kind: TypeKind::Auto,
                    span: start_span,
                }),
                entries,
            },
            span: start_span.combine(&end_span),
        })
    }

    pub(super) fn parse_set_literal(
        &mut self,
        start_span: Span,
        first_elem: ExpressionNode,
    ) -> ParserResult<ExpressionNode> {
        self.parse_set_literal_inner(start_span, first_elem)
    }

    fn parse_set_literal_inner(
        &mut self,
        start_span: Span,
        first_elem: ExpressionNode,
    ) -> ParserResult<ExpressionNode> {
        let mut elements = vec![first_elem];
        self.parse_set_entries(&mut elements)?;
        let end_span =
            self.consume_token(TokenType::CloseBrace, "Expected '}' after collection")?;
        let syntax_elements = elements
            .iter()
            .map(|element| {
                element
                    .span
                    .byte_range
                    .expect("set element has source range")
            })
            .collect();
        self.record_typed_syntax_node(
            SyntaxKind::SetLiteral,
            self.token_index_for_span(start_span),
            self.current,
            SyntaxData::Set {
                elements: syntax_elements,
            },
        );
        Ok(ExpressionNode {
            kind: ExpressionKind::SetLiteral(elements),
            span: start_span.combine(&end_span),
        })
    }

    pub(super) fn parse_set_entries(
        &mut self,
        elements: &mut Vec<ExpressionNode>,
    ) -> ParserResult<()> {
        loop {
            self.skip_newlines();
            if !self.has_comma_or_entry()? {
                break;
            }
            let elem = self.parse_expression()?;
            elements.push(elem);
        }
        Ok(())
    }

    pub(super) fn parse_collection_entries(
        &mut self,
        entries: &mut Vec<(ExpressionNode, ExpressionNode)>,
        is_map: bool,
    ) -> ParserResult<()> {
        loop {
            self.skip_newlines();
            if !self.has_comma_or_entry()? {
                break;
            }
            if is_map {
                let key = self.parse_expression()?;
                self.consume_token(TokenType::Colon, "Expected ':' after map key")?;
                let value = self.parse_expression()?;
                entries.push((key, value));
            }
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

    pub(super) fn parse_parenthesized_or_tuple_expression(
        &mut self,
        start_span: Span,
    ) -> ParserResult<ExpressionNode> {
        self.parse_parenthesized_or_tuple_expression_inner(start_span)
    }

    fn parse_parenthesized_or_tuple_expression_inner(
        &mut self,
        start_span: Span,
    ) -> ParserResult<ExpressionNode> {
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

        let first_expr = self.parse_expression()?;
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

            let second_expr = self.parse_expression()?;
            let first_range = first_expr
                .span
                .byte_range
                .expect("parsed expression has source range");
            let second_range = second_expr
                .span
                .byte_range
                .expect("parsed expression has source range");
            self.skip_newlines();
            self.consume_token(TokenType::CloseParen, "Expected ')' after tuple elements")?;
            self.record_typed_syntax_node(
                SyntaxKind::TupleExpression,
                syntax_start,
                self.current,
                SyntaxData::Tuple {
                    first: first_range,
                    second: second_range,
                },
            );

            let tuple_expr = ExpressionNode {
                kind: ExpressionKind::TupleLiteral(vec![first_expr, second_expr]),
                span: start_span.combine(&self.previous().span),
            };
            return self.parse_postfix_operators(tuple_expr);
        }

        let expression_range = first_expr
            .span
            .byte_range
            .expect("parsed expression has source range");
        self.consume_token(TokenType::CloseParen, "Expected ')' after expression")?;
        self.record_typed_syntax_node(
            SyntaxKind::ParenthesizedExpression,
            syntax_start,
            self.current,
            SyntaxData::Parenthesized {
                expression: expression_range,
            },
        );
        self.parse_postfix_operators(first_expr)
    }

    pub(super) fn parse_list_literal_expression(
        &mut self,
        start_span: Span,
    ) -> ParserResult<ExpressionNode> {
        let start = self.current.saturating_sub(1);
        let result = self.parse_list_literal_expression_inner(start_span);
        if result.is_ok() {
            let end =
                self.matching_delimiter_end(start, TokenType::OpenBracket, TokenType::CloseBracket);
            self.record_syntax_node(SyntaxKind::ListLiteral, start, end);
        }
        result
    }

    fn parse_list_literal_expression_inner(
        &mut self,
        start_span: Span,
    ) -> ParserResult<ExpressionNode> {
        let mut elements = Vec::new();

        self.skip_newlines();

        if !self.check(TokenType::CloseBracket) {
            loop {
                elements.push(self.parse_expression()?);

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
        let element_ranges = elements
            .iter()
            .map(|element| {
                element
                    .span
                    .byte_range
                    .expect("list element has source range")
            })
            .collect();
        self.record_typed_syntax_node(
            SyntaxKind::ListLiteral,
            self.token_index_for_span(start_span),
            self.current,
            SyntaxData::List {
                elements: element_ranges,
            },
        );
        let expr = ExpressionNode {
            kind: ExpressionKind::ListLiteral(elements),
            span: start_span.combine(&end_span),
        };
        self.parse_postfix_operators(expr)
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

    pub(super) fn parse_lambda_expression(
        &mut self,
        start_span: Span,
    ) -> ParserResult<ExpressionNode> {
        let lambda_start = self.token_index_for_span(start_span);
        self.consume_token(TokenType::OpenParen, "Expected '(' after 'func' in lambda")?;
        let mut params = Vec::new();
        let parameters_start = self.syntax_events.len();

        if !self.check(TokenType::CloseParen) {
            loop {
                let parameter_start = self.current;
                let param_type = self.parse_type()?;
                let type_range = param_type
                    .span
                    .byte_range
                    .expect("lambda parameter type source range");
                let param_name = self.consume_identifier("Expected parameter name")?;
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
                    SyntaxData::Parameter {
                        name: name_range,
                        type_range,
                        default_value: None,
                    },
                );
                params.push(Param {
                    name: param_name,
                    type_: param_type,
                    default_value: None,
                });

                if !self.matches(&[TokenType::Comma]) {
                    break;
                }
                self.skip_newlines();
            }
        }
        let parameter_ranges = self.syntax_ranges_since(parameters_start, |data| {
            matches!(data, SyntaxData::Parameter { .. })
        });

        self.consume_token(TokenType::CloseParen, "Expected ')' after parameters")?;

        let where_start = self.syntax_events.len();
        let where_clause = self.parse_where_clause()?;
        let where_range = self.last_syntax_range_since(where_start, |data| {
            matches!(data, SyntaxData::WhereClause { .. })
        });
        if where_clause.is_some() {
            self.skip_newlines();
        }

        let return_type = if self.matches(&[TokenType::Returns]) {
            self.parse_type()?
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
        let return_type_range = return_type
            .span
            .byte_range
            .expect("lambda return type source range");

        let body = self.block()?;
        let body_range = body.range;
        let body_statements = body.statements;

        let end_span = body_statements.last().map_or(start_span, |s| s.span);
        let expr = ExpressionNode {
            kind: ExpressionKind::Lambda {
                params,
                return_type,
                body: body_statements,
                where_clause,
            },
            span: start_span.combine(&end_span),
        };
        self.record_typed_syntax_node(
            SyntaxKind::LambdaExpression,
            lambda_start,
            self.current,
            SyntaxData::Lambda {
                parameters: parameter_ranges,
                return_type: return_type_range,
                where_clause: where_range,
                body: body_range,
                ast_span: expr.span.byte_range.expect("lambda AST span range"),
            },
        );

        self.parse_postfix_operators(expr)
    }

    pub(super) fn parse_if_expression(&mut self, token_span: Span) -> ParserResult<ExpressionNode> {
        let expression_start = self.token_index_for_span(token_span);
        let cond = self.parse_expression()?;
        let condition_range = cond
            .span
            .byte_range
            .expect("if expression condition source range");
        let then_expr = self.parse_if_branch_expression("then expression")?;
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
            self.parse_if_expression(if_span)?
        } else {
            self.parse_if_branch_expression("else expression")?
        };
        let else_range = else_expr
            .span
            .byte_range
            .expect("if expression else branch source range");
        let span = token_span.combine(&self.previous().span);
        let expression = ExpressionNode {
            kind: ExpressionKind::If {
                cond: Box::new(cond),
                then_expr: Box::new(then_expr),
                else_expr: Box::new(else_expr),
            },
            span,
        };
        self.record_typed_syntax_node(
            SyntaxKind::IfExpression,
            expression_start,
            self.current,
            SyntaxData::IfExpression {
                condition: condition_range,
                then_expression: then_range,
                else_expression: else_range,
                ast_span: expression
                    .span
                    .byte_range
                    .expect("if expression AST span range"),
            },
        );
        Ok(expression)
    }

    pub(super) fn parse_match_expression(
        &mut self,
        token_span: Span,
    ) -> ParserResult<ExpressionNode> {
        let match_start = self.token_index_for_span(token_span);
        let expr = self.parse_expression()?;
        let expression_range = expr
            .span
            .byte_range
            .expect("match expression value source range");
        self.check_no_postfix_increment_decrement(&expr)?;
        self.consume_token(TokenType::OpenBrace, "Expected '{' after match expression")?;
        self.skip_newlines();
        let mut arms = Vec::new();
        let mut arm_ranges = Vec::new();
        while !self.check(TokenType::CloseBrace) && !self.is_at_end() {
            let arm_start = self.current;
            let pattern_events_start = self.syntax_events.len();
            let pattern = self.parse_pattern()?;
            let pattern_range = self
                .last_syntax_range_since(pattern_events_start, |data| {
                    matches!(data, SyntaxData::Pattern(_))
                })
                .expect("match expression pattern range");
            let guard = if self.matches(&[TokenType::If]) {
                Some(self.parse_expression()?)
            } else {
                None
            };
            let guard_range = guard.as_ref().and_then(|guard| guard.span.byte_range);
            self.skip_newlines();
            self.consume_token(TokenType::OpenBrace, "Expected '{' before match arm value")?;
            self.skip_newlines();
            let (body, body_range, body_is_expression) = if self.matches(&[TokenType::Return]) {
                let body_events_start = self.syntax_events.len();
                let AstNode::Statement(statement) =
                    self.return_statement()?.into_compatibility_ast()
                else {
                    return Err(ParserError::new(
                        DiagnosticCode::ParseExpectedToken,
                        "Expected return statement in match arm",
                        token_span,
                    ));
                };
                let body_range = self
                    .last_statement_range_since(body_events_start)
                    .expect("match expression return body range");
                (vec![statement], body_range, false)
            } else {
                let value = self.parse_expression()?;
                let body_range = value
                    .span
                    .byte_range
                    .expect("match expression arm value range");
                let statement = StatementNode {
                    span: value.span,
                    kind: StatementKind::Expression(value),
                };
                (vec![statement], body_range, true)
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
                SyntaxData::MatchArm {
                    pattern: pattern_range,
                    guard: guard_range,
                    body: body_range,
                    body_is_expression,
                },
            );
            arm_ranges.push(arm_range);
            arms.push(MatchArm {
                pattern,
                guard,
                body,
            });
            self.skip_newlines();
            if self.matches(&[TokenType::Comma]) {
                self.skip_newlines();
            }
        }
        let end_span =
            self.consume_token(TokenType::CloseBrace, "Expected '}' after match arms")?;
        let expression = ExpressionNode {
            kind: ExpressionKind::Match {
                expr: Box::new(expr),
                arms,
            },
            span: token_span.combine(&end_span),
        };
        self.record_typed_syntax_node(
            SyntaxKind::MatchExpression,
            match_start,
            self.current,
            SyntaxData::MatchExpression {
                expression: expression_range,
                arms: arm_ranges,
                ast_span: expression
                    .span
                    .byte_range
                    .expect("match expression AST span range"),
            },
        );
        Ok(expression)
    }

    /// Parse one branch of an if-expression: `{ <expr> }`. Newlines are allowed
    /// around the value expression so a branch can span multiple lines; the branch
    /// is still a single value, not a statement block.
    pub(super) fn parse_if_branch_expression(
        &mut self,
        what: &str,
    ) -> ParserResult<ExpressionNode> {
        self.consume_token(
            TokenType::OpenBrace,
            &format!("Expected '{{' before {what}"),
        )?;
        self.skip_newlines();
        let expr = self.parse_expression()?;
        self.skip_newlines();
        self.consume_token(
            TokenType::CloseBrace,
            &format!("Expected '}}' after {what}"),
        )?;
        Ok(expr)
    }

    pub(super) fn parse_postfixed_primary(
        &mut self,
        kind: ExpressionKind,
        token_span: Span,
    ) -> ParserResult<ExpressionNode> {
        let expr = ExpressionNode {
            kind,
            span: token_span,
        };
        self.parse_postfix_operators(expr)
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

    pub(super) fn parse_primary(&mut self) -> ParserResult<ExpressionNode> {
        if self.is_at_end() {
            // Use the last token's span to show where we expected an expression
            let error_span = self.tokens.last().map_or_else(
                || Span {
                    row_start: 1,
                    row_end: None,
                    col_start: 1,
                    col_end: None,
                    byte_range: Some(crate::lexer::ByteRange::empty(0)),
                },
                |t| t.span,
            );
            return Err(ParserError::new(
                DiagnosticCode::ParseExpectedExpression,
                "Expected expression, found end of input",
                error_span,
            ));
        }

        let token_index = self.current;
        let token = self.consume();
        let token_type = token.token_type.clone();
        let syntax_token_type = token_type.clone();
        let token_span = token.span;

        let result = match token_type {
            TokenType::OpenParen => self.parse_parenthesized_or_tuple_expression(token_span),

            TokenType::Int(n) => self.parse_postfixed_primary(
                ExpressionKind::Literal(LiteralNode::Integer(n)),
                token_span,
            ),

            TokenType::Bool(b) => self.parse_postfixed_primary(
                ExpressionKind::Literal(LiteralNode::Boolean(b)),
                token_span,
            ),

            TokenType::None => self.parse_postfixed_primary(ExpressionKind::None, token_span),

            TokenType::Char(c) => self
                .parse_postfixed_primary(ExpressionKind::Literal(LiteralNode::Char(c)), token_span),

            TokenType::Str(s) => self.parse_postfixed_primary(
                ExpressionKind::Literal(LiteralNode::String(s)),
                token_span,
            ),

            TokenType::Bytes(bytes) => self.parse_postfixed_primary(
                ExpressionKind::Literal(LiteralNode::Bytes(bytes)),
                token_span,
            ),

            TokenType::Float(f) => self.parse_postfixed_primary(
                ExpressionKind::Literal(LiteralNode::Float(f)),
                token_span,
            ),

            TokenType::OpenBracket => self.parse_list_literal_expression(token_span),

            TokenType::OpenBrace => self.parse_collection_literal(token_span),

            TokenType::Func => self.parse_lambda_expression(token_span),

            TokenType::If => self.parse_if_expression(token_span),

            TokenType::Match => self.parse_match_expression(token_span),

            TokenType::Id(id) => {
                // defer handling of '<' to binary operator parsing or parse_postfix_operators, which can disambiguate generics more safely.
                self.parse_postfixed_primary(ExpressionKind::Identifier(id.clone()), token_span)
            }

            _ => Err(self.unexpected_primary_error(token_type, token_span)),
        };
        if result.is_ok()
            && let Some(range) = token_span.byte_range
        {
            let data = match syntax_token_type {
                TokenType::Id(_) => Some(SyntaxData::Name { token: range }),
                TokenType::Int(_)
                | TokenType::Float(_)
                | TokenType::Bool(_)
                | TokenType::Char(_)
                | TokenType::Str(_)
                | TokenType::Bytes(_)
                | TokenType::None => Some(SyntaxData::Literal { token: range }),
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

    pub(super) fn parse_call_postfix(
        &mut self,
        expr: ExpressionNode,
    ) -> ParserResult<ExpressionNode> {
        let start = self.current.saturating_sub(1);
        let result = self.parse_call_postfix_inner(expr);
        if result.is_ok() {
            self.record_syntax_node(SyntaxKind::CallArguments, start, self.current);
        }
        result
    }

    fn parse_call_postfix_inner(&mut self, expr: ExpressionNode) -> ParserResult<ExpressionNode> {
        let callee_range = expr.span.byte_range.expect("call target has source range");
        let callee_start = self.token_index_for_span(expr.span);
        let mut args = Vec::new();
        if !self.check(TokenType::CloseParen) {
            loop {
                self.skip_newlines();
                args.push(self.parse_expression()?);
                self.skip_newlines();

                if !self.matches(&[TokenType::Comma]) {
                    break;
                }
                // A trailing comma ends the list. Collection literals already
                // allow one, and an argument list spanning several lines is
                // exactly where people write it - so rejecting it here was an
                // inconsistency between two forms that look the same.
                self.skip_newlines();
                if self.check(TokenType::CloseParen) {
                    break;
                }
            }
        }
        let end_span = self.consume_token(TokenType::CloseParen, "Expected ')' after arguments")?;
        let expr_span = *expr.span();
        let argument_ranges = args
            .iter()
            .map(|argument| {
                argument
                    .span
                    .byte_range
                    .expect("call argument has source range")
            })
            .collect();
        self.record_typed_syntax_node(
            SyntaxKind::Expression,
            callee_start,
            self.current,
            SyntaxData::Call {
                callee: callee_range,
                arguments: argument_ranges,
            },
        );
        Ok(ExpressionNode {
            kind: ExpressionKind::Call {
                func: Box::new(expr),
                args,
            },
            span: expr_span.combine(&end_span),
        })
    }

    pub(super) fn parse_field_access_postfix(
        &mut self,
        expr: ExpressionNode,
    ) -> ParserResult<ExpressionNode> {
        let base_range = expr.span.byte_range.expect("field base has source range");
        let start = self.token_index_for_span(expr.span);
        let field = self.consume_identifier("Expected field name after '.'")?;
        let expr_span = *expr.span();
        let field_span = self.tokens[self.current - 1].span;
        if let Some(field_range) = field_span.byte_range {
            self.record_typed_syntax_node(
                SyntaxKind::Expression,
                start,
                self.current,
                SyntaxData::FieldAccess {
                    base: base_range,
                    field: field_range,
                },
            );
        }
        Ok(ExpressionNode {
            kind: ExpressionKind::FieldAccess {
                expr: Box::new(expr),
                field,
            },
            span: expr_span.combine(&field_span),
        })
    }

    pub(super) fn parse_index_postfix(
        &mut self,
        expr: ExpressionNode,
    ) -> ParserResult<ExpressionNode> {
        let start = self.current.saturating_sub(1);
        let result = self.parse_index_postfix_inner(expr);
        if result.is_ok() {
            let end =
                self.matching_delimiter_end(start, TokenType::OpenBracket, TokenType::CloseBracket);
            self.record_syntax_node(SyntaxKind::IndexExpression, start, end);
        }
        result
    }

    fn parse_index_postfix_inner(&mut self, expr: ExpressionNode) -> ParserResult<ExpressionNode> {
        let base_range = expr.span.byte_range.expect("index base has source range");
        let start = self.token_index_for_span(expr.span);
        // `xs[:b]` - an omitted start, so the colon comes first and there is no
        // index expression to parse.
        if self.matches(&[TokenType::Colon]) {
            return self.finish_slice(expr, None);
        }

        let index = self.parse_expression()?;

        // `xs[a:b]` or `xs[a:]`. Deciding here, after the first expression,
        // is what lets an index and a slice share one entry point.
        if self.matches(&[TokenType::Colon]) {
            return self.finish_slice(expr, Some(index));
        }

        let end_span = self.consume_token(TokenType::CloseBracket, "Expected ']' after index")?;
        let expr_span = *expr.span();
        if let Some(index_range) = index.span.byte_range {
            self.record_typed_syntax_node(
                SyntaxKind::Expression,
                start,
                self.current,
                SyntaxData::Index {
                    base: base_range,
                    index: index_range,
                },
            );
        }
        Ok(ExpressionNode {
            kind: ExpressionKind::ListAccess {
                expr: Box::new(expr),
                index: Box::new(index),
            },
            span: expr_span.combine(&end_span),
        })
    }

    /// Finish `xs[start:` - the colon is consumed, the end bound is optional.
    pub(super) fn finish_slice(
        &mut self,
        expr: ExpressionNode,
        start: Option<ExpressionNode>,
    ) -> ParserResult<ExpressionNode> {
        let base_range = expr.span.byte_range.expect("slice base has source range");
        let slice_start = self.token_index_for_span(expr.span);
        let start_range = start
            .as_ref()
            .and_then(|expression| expression.span.byte_range);
        let end = if self.check(TokenType::CloseBracket) {
            None
        } else {
            Some(Box::new(self.parse_expression()?))
        };
        let end_span = self.consume_token(TokenType::CloseBracket, "Expected ']' after slice")?;
        let end_range = end
            .as_ref()
            .and_then(|expression| expression.span.byte_range);
        let expr_span = *expr.span();
        let expression = ExpressionNode {
            kind: ExpressionKind::Slice {
                expr: Box::new(expr),
                start: start.map(Box::new),
                end,
            },
            span: expr_span.combine(&end_span),
        };
        self.record_typed_syntax_node(
            SyntaxKind::SliceExpression,
            slice_start,
            self.current,
            SyntaxData::Slice {
                base: base_range,
                start: start_range,
                end: end_range,
            },
        );
        Ok(expression)
    }

    pub(super) fn should_consume_generic_type_args(
        &self,
        expr: &ExpressionNode,
    ) -> Option<(String, bool)> {
        if self.mode == ParserMode::SyntaxOnly
            && expr.span.byte_range.is_some_and(|range| {
                self.syntax_events.iter().any(|event| {
                    event.range == range
                        && matches!(event.data.as_ref(), Some(SyntaxData::Binary { .. }))
                })
            })
        {
            return None;
        }
        let generic_target_name = self.generic_target_name(expr)?;

        let gt_idx = self.find_matching_generic_gt_index()?;
        let should_consume = self.should_consume_generics_for_target(&generic_target_name, gt_idx);

        Some((generic_target_name, should_consume))
    }

    pub(super) fn generic_target_name(&self, expr: &ExpressionNode) -> Option<String> {
        match &expr.kind {
            ExpressionKind::Identifier(id) => Some(id.clone()),
            ExpressionKind::FieldAccess { expr, field } => {
                Some(format!("{}.{field}", self.generic_target_name(expr)?))
            }
            _ => None,
        }
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
        generic_target_name: &str,
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
            ) || generic_target_name
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_uppercase())
        } else {
            true
        }
    }

    pub(super) fn parse_generic_postfix(
        &mut self,
        expr: ExpressionNode,
    ) -> ParserResult<Option<ExpressionNode>> {
        let start = self.current;
        let result = self.parse_generic_postfix_inner(expr);
        if matches!(result, Ok(Some(_))) {
            self.record_syntax_node(SyntaxKind::TypeArguments, start, self.current);
        }
        result
    }

    fn parse_generic_postfix_inner(
        &mut self,
        expr: ExpressionNode,
    ) -> ParserResult<Option<ExpressionNode>> {
        let Some((name, should_consume_generics)) = self.should_consume_generic_type_args(&expr)
        else {
            return Ok(None);
        };

        if !should_consume_generics {
            return Ok(None);
        }

        let _ = self.matches(&[TokenType::Lt]);
        let type_args = self.parse_type_arguments()?;
        let end_span = self.consume_token(TokenType::Gt, "Expected '>' after type arguments")?;
        let target = expr
            .span
            .byte_range
            .expect("generic target has source range");
        let arguments = type_args
            .iter()
            .map(|argument| {
                argument
                    .span
                    .byte_range
                    .expect("generic type argument has source range")
            })
            .collect();
        let syntax_start = self.token_index_for_span(expr.span);
        self.record_typed_syntax_node(
            SyntaxKind::Expression,
            syntax_start,
            self.current,
            SyntaxData::Generic { target, arguments },
        );
        Ok(Some(ExpressionNode {
            kind: ExpressionKind::GenericType(name, type_args),
            span: expr.span.combine(&end_span),
        }))
    }

    pub(super) fn parse_postfix_update(
        &mut self,
        expr: ExpressionNode,
        op: UnaryOp,
    ) -> ExpressionNode {
        let expr_span = *expr.span();
        let op_span = self.previous().span;
        if let (Some(operand), Some(operator)) = (expr_span.byte_range, op_span.byte_range) {
            let start = self.token_index_for_span(expr_span);
            self.record_typed_syntax_node(
                SyntaxKind::UnaryExpression,
                start,
                self.current,
                SyntaxData::Unary {
                    operator,
                    operand,
                    postfix: true,
                },
            );
        }
        ExpressionNode {
            kind: ExpressionKind::Unary {
                op,
                op_span,
                expr: Box::new(expr),
                postfix: true,
            },
            span: expr_span.combine(&op_span),
        }
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

    pub(super) fn try_parse_postfix_operator(
        &mut self,
        expr: ExpressionNode,
    ) -> ParserResult<Option<ExpressionNode>> {
        if self.matches(&[TokenType::OpenParen]) {
            return self.parse_call_postfix(expr).map(Some);
        }

        if self.matches(&[TokenType::Dot]) {
            return self.parse_field_access_postfix(expr).map(Some);
        }

        if self.matches(&[TokenType::OpenBracket]) {
            return self.parse_index_postfix(expr).map(Some);
        }

        if self.check(TokenType::Lt) {
            return self
                .parse_generic_postfix(expr)
                .map(|maybe_expr| maybe_expr.or(None));
        }

        if self.matches(&[TokenType::Incr]) {
            return Ok(Some(self.parse_postfix_update(expr, UnaryOp::Incr)));
        }

        if self.matches(&[TokenType::Decr]) {
            return Ok(Some(self.parse_postfix_update(expr, UnaryOp::Decr)));
        }

        Ok(None)
    }

    pub(super) fn parse_postfix_operators(
        &mut self,
        mut expr: ExpressionNode,
    ) -> ParserResult<ExpressionNode> {
        loop {
            if let Some(next_expr) = self.try_parse_postfix_operator(expr.clone())? {
                expr = next_expr;
                continue;
            }

            if self.check(TokenType::Lt) {
                break;
            }

            if self.skip_newline_gap_for_postfix() {
                continue;
            }

            break;
        }

        Ok(expr)
    }

    pub(super) fn consume_operator(&mut self) -> Option<BinaryOp> {
        if self.is_at_end() {
            return None;
        }

        let token = self.peek();
        let result = match token.token_type {
            TokenType::Plus => Some(BinaryOp::Add),
            TokenType::Minus => Some(BinaryOp::Subtract),
            TokenType::Star => Some(BinaryOp::Multiply),
            TokenType::Slash => Some(BinaryOp::Divide),
            TokenType::Percent => Some(BinaryOp::Modulo),
            TokenType::StarStar => Some(BinaryOp::Exponent),
            TokenType::Eq => Some(BinaryOp::Assign),
            TokenType::EqEq => Some(BinaryOp::Equal),
            TokenType::NotEq => Some(BinaryOp::NotEqual),
            TokenType::Lt => Some(BinaryOp::Less),
            TokenType::Gt => Some(BinaryOp::Greater),
            TokenType::Le => Some(BinaryOp::LessEqual),
            TokenType::Ge => Some(BinaryOp::GreaterEqual),
            TokenType::And => Some(BinaryOp::LogicalAnd),
            TokenType::Or => Some(BinaryOp::LogicalOr),
            TokenType::In => Some(BinaryOp::In),
            TokenType::PlusEq => Some(BinaryOp::AddAssign),
            TokenType::MinusEq => Some(BinaryOp::SubtractAssign),
            TokenType::StarEq => Some(BinaryOp::MultiplyAssign),
            TokenType::SlashEq => Some(BinaryOp::DivideAssign),
            TokenType::PercentEq => Some(BinaryOp::ModuloAssign),
            _ => None,
        };

        if result.is_some() {
            self.advance();
        }
        result
    }

    pub(super) fn peek_operator(&self) -> Option<BinaryOp> {
        if self.is_at_end() {
            return None;
        }

        match self.peek().token_type {
            TokenType::Plus => Some(BinaryOp::Add),
            TokenType::Minus => Some(BinaryOp::Subtract),
            TokenType::Star => Some(BinaryOp::Multiply),
            TokenType::Slash => Some(BinaryOp::Divide),
            TokenType::Percent => Some(BinaryOp::Modulo),
            TokenType::StarStar => Some(BinaryOp::Exponent),
            TokenType::Eq => Some(BinaryOp::Assign),
            TokenType::EqEq => Some(BinaryOp::Equal),
            TokenType::NotEq => Some(BinaryOp::NotEqual),
            TokenType::Lt => Some(BinaryOp::Less),
            TokenType::Gt => Some(BinaryOp::Greater),
            TokenType::Le => Some(BinaryOp::LessEqual),
            TokenType::Ge => Some(BinaryOp::GreaterEqual),
            TokenType::And => Some(BinaryOp::LogicalAnd),
            TokenType::Or => Some(BinaryOp::LogicalOr),
            TokenType::In => Some(BinaryOp::In),
            TokenType::PlusEq => Some(BinaryOp::AddAssign),
            TokenType::MinusEq => Some(BinaryOp::SubtractAssign),
            TokenType::StarEq => Some(BinaryOp::MultiplyAssign),
            TokenType::SlashEq => Some(BinaryOp::DivideAssign),
            TokenType::PercentEq => Some(BinaryOp::ModuloAssign),
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

    pub(super) fn get_operator_precedence(&self, op: &BinaryOp) -> ParserResult<Precedence> {
        let precedence = match op {
            BinaryOp::Assign
            | BinaryOp::AddAssign
            | BinaryOp::SubtractAssign
            | BinaryOp::MultiplyAssign
            | BinaryOp::DivideAssign
            | BinaryOp::ModuloAssign => Precedence::Assignment,

            BinaryOp::LogicalOr => Precedence::Or,

            BinaryOp::LogicalAnd => Precedence::And,

            BinaryOp::In => Precedence::Comparison,

            BinaryOp::Equal | BinaryOp::NotEqual => Precedence::Equality,

            BinaryOp::Less | BinaryOp::LessEqual | BinaryOp::Greater | BinaryOp::GreaterEqual => {
                Precedence::Comparison
            }

            BinaryOp::Add | BinaryOp::Subtract => Precedence::Term,

            BinaryOp::Multiply | BinaryOp::Divide | BinaryOp::Modulo => Precedence::Factor,

            BinaryOp::Exponent => Precedence::Exponent,
        };
        Ok(precedence)
    }
}

#[cfg(test)]
mod tests {
    use super::Parser;
    use crate::ast::ExpressionKind;
    use crate::lexer::{ByteRange, Lexer};
    use crate::source::Source;

    #[test]
    fn binary_operator_span_points_to_operator_token() {
        let mut source = Source::from_string("1 + 2".to_owned());
        let tokens = Lexer::new(&mut source).lex_all().expect("valid expression");
        let mut parser = Parser::new(&tokens);
        let expression = parser.parse_expression().expect("expression parses");

        assert!(matches!(
            expression.kind,
            ExpressionKind::Binary { op_span, .. }
                if op_span.byte_range == Some(ByteRange::new(2, 3))
        ));
    }

    #[test]
    fn generic_target_keeps_all_qualified_field_segments() {
        let mut source = Source::from_string("module.factory.Builder<int>(item)".to_owned());
        let tokens = Lexer::new(&mut source)
            .lex_all()
            .expect("valid generic expression");
        let mut parser = Parser::new(&tokens);
        let expression = parser
            .parse_expression()
            .expect("qualified generic expression parses");

        assert!(matches!(
            &expression.kind,
            ExpressionKind::Call { func, .. }
                if matches!(&func.kind, ExpressionKind::GenericType(name, _)
                    if name == "module.factory.Builder")
        ));
    }
}
