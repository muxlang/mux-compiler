use super::expressions::ParsedExpression;
use super::types::TypeFact;
use super::*;
use crate::ast::SpanExt;

/// Parsed source range for a block.
pub(super) struct BlockStatementFact {
    pub(super) range: ByteRange,
}

/// Parsed source facts for a `where` clause.
pub(super) struct WhereClauseFact {
    pub(super) span: Span,
}

#[derive(Clone, Copy)]
pub(super) enum ParsedPatternKind {
    Literal,
    Identifier,
    Wildcard,
    EnumVariant,
    List { elements: usize, has_rest: bool },
}

impl<'a> Parser<'a> {
    pub(super) fn skip_newlines_before(&mut self, token_type: TokenType) -> bool {
        let mut i = 0;
        while self
            .peek_ahead(i)
            .is_some_and(|t| t.token_type == TokenType::NewLine)
        {
            i += 1;
        }
        if self
            .peek_ahead(i)
            .is_some_and(|t| t.token_type == token_type)
        {
            self.skip_newlines();
            true
        } else {
            false
        }
    }

    /// Parse an optional `where { ... }` constraint block. Predicates are
    /// boolean expressions separated like set-literal elements: commas, with
    /// newlines freely allowed around predicates and separators, and a
    /// trailing comma tolerated. The clause may start on a following line
    /// (`func f(a)\n    where { ... }`); if no `where` follows, any newlines
    /// looked past are left unconsumed.
    pub(super) fn parse_where_clause_fact(&mut self) -> ParserResult<Option<WhereClauseFact>> {
        if !self.skip_newlines_before(TokenType::Where) {
            return Ok(None);
        }
        let start_span = self.peek().span;
        let start = self.current;
        self.consume_token(TokenType::Where, "Expected 'where' keyword")?;
        self.skip_newlines();
        self.consume_token(TokenType::OpenBrace, "Expected '{' after 'where'")?;
        let mut predicate_ranges = Vec::new();
        loop {
            self.skip_newlines();
            if self.check(TokenType::CloseBrace) {
                break;
            }
            let predicate = self.parse_expression_parsed()?;
            predicate_ranges.push(
                predicate
                    .span
                    .byte_range
                    .expect("where predicate has source range"),
            );
            self.skip_newlines();
            if !self.matches(&[TokenType::Comma]) {
                break;
            }
        }
        self.skip_newlines();
        let end_span = self.consume_token(
            TokenType::CloseBrace,
            "Expected '}' after where predicates (predicates are comma-separated)",
        )?;
        let span = start_span.combine(&end_span);
        if predicate_ranges.is_empty() {
            return Err(ParserError::with_help(
                DiagnosticCode::ParseExpectedToken,
                "Empty 'where' block",
                span,
                "A where block must contain at least one boolean predicate. Example: where { value > 0 }",
            ));
        }
        self.record_typed_syntax_node(
            SyntaxKind::WhereClause,
            start,
            self.current,
            AstLoweringData::WhereClause {
                predicates: predicate_ranges,
            },
        );
        Ok(Some(WhereClauseFact { span }))
    }

    pub(super) fn parse_required_return_type(&mut self) -> ParserResult<TypeFact> {
        if self.matches(&[TokenType::Returns]) {
            self.parse_type_fact()
        } else {
            Err(ParserError::with_help(
                DiagnosticCode::ParseExpectedToken,
                format!(
                    "Expected 'returns' before return type, found {}",
                    Self::describe_token(&self.peek().token_type)
                ),
                self.peek().span,
                "All functions must declare a return type. Use 'returns void' for functions that return nothing. Example: func foo() returns int { ... }",
            ))
        }
    }

    /// Parse a statement for syntax consumers without assembling its root
    pub(super) fn statement(&mut self) -> ParserResult<()> {
        let checkpoint = self.checkpoint();
        let result = self.statement_inner();
        let kind = if self
            .tokens
            .get(checkpoint.current)
            .is_some_and(|token| token.token_type == TokenType::OpenBrace)
        {
            SyntaxKind::Block
        } else {
            SyntaxKind::Statement
        };
        if result.is_ok() {
            self.record_syntax_node(kind, checkpoint.current, self.current);
        } else if self.current > checkpoint.current {
            self.record_syntax_node(SyntaxKind::Error, checkpoint.current, self.current);
        }
        result
    }

    fn statement_inner(&mut self) -> ParserResult<()> {
        if self.matches(&[TokenType::If]) {
            self.if_statement().map(drop)
        } else if self.matches(&[TokenType::While]) {
            self.while_statement().map(drop)
        } else if self.matches(&[TokenType::For]) {
            self.for_statement().map(drop)
        } else if self.matches(&[TokenType::Match]) {
            self.match_statement().map(drop)
        } else if self.matches(&[TokenType::Return]) {
            self.return_statement()
        } else if self.matches(&[TokenType::Break]) {
            self.break_statement()
        } else if self.matches(&[TokenType::Continue]) {
            self.continue_statement()
        } else if self.matches(&[TokenType::Func]) {
            self.function_declaration_fact(false)
        } else if self.check(TokenType::OpenBrace) {
            self.block().map(drop)
        } else if self.looks_like_typed_decl() {
            self.typed_declaration_fact().map(drop)
        } else {
            self.parse_expression_statement()
        }
    }

    pub(super) fn looks_like_typed_decl(&self) -> bool {
        let n = self.tokens.len();
        let i = self.current;

        if let Some(j) = self.skip_type(i)
            && j < n
            && let TokenType::Id(_) = self.tokens[j].token_type
            && j + 1 < n
        {
            return match &self.tokens[j + 1].token_type {
                TokenType::Eq | TokenType::NewLine | TokenType::CloseBrace | TokenType::Eof => true,
                // `Type name` with no initializer (#393). A type followed by a
                // name followed by the end of the statement is unambiguously a
                // declaration - Mux has no expression form where two
                // identifiers sit side by side.
                _ => false,
            };
        }
        false
    }

    pub(super) fn skip_type(&self, mut i: usize) -> Option<usize> {
        let n = self.tokens.len();
        if i >= n {
            return None;
        }
        match self.tokens[i].token_type {
            TokenType::Ref => {
                i += 1;
                self.skip_type(i)
            }
            TokenType::OpenParen => self.skip_function_type(i, n),
            TokenType::Id(_) => self.skip_identifier_type(i, n),
            _ => None,
        }
    }

    pub(super) fn skip_function_type(&self, mut i: usize, n: usize) -> Option<usize> {
        i += 1;
        let mut depth = 1usize;
        while i < n && depth > 0 {
            match self.tokens[i].token_type {
                TokenType::OpenParen => depth += 1,
                TokenType::CloseParen => depth -= 1,
                _ => {}
            }
            i += 1;
        }
        if depth != 0 {
            return None;
        }
        if i >= n || self.tokens[i].token_type != TokenType::Returns {
            return None;
        }
        i += 1;
        self.skip_type(i)
    }

    pub(super) fn skip_identifier_type(&self, mut i: usize, n: usize) -> Option<usize> {
        i += 1;
        if i < n && self.tokens[i].token_type == TokenType::Lt {
            i += 1;
            let mut depth = 1usize;
            while i < n && depth > 0 {
                match self.tokens[i].token_type {
                    TokenType::Lt => depth += 1,
                    TokenType::Gt => depth -= 1,
                    _ => {}
                }
                i += 1;
            }
            if depth != 0 {
                return None;
            }
        }
        Some(i)
    }

    fn if_statement(&mut self) -> ParserResult<()> {
        let start = self.current.saturating_sub(1);
        let start_span = self.tokens[start].span;
        let condition = self.parse_expression_parsed()?;
        let condition_range = condition
            .span
            .byte_range
            .expect("if condition has source range");
        self.check_no_postfix_increment_decrement_parsed(&condition)?;
        self.skip_newlines();

        let then_event_start = self.syntax_events.len();
        let then_block = self.block()?;
        let then_range = then_block.range;
        let then_ast_range =
            self.last_ast_statement_range_since(then_event_start, Some(then_range));

        self.skip_newlines();
        let else_event_start = self.syntax_events.len();
        let else_range = self.parse_else_branch()?;
        let start_range = start_span.byte_range.expect("if statement start range");
        let ast_end = if let Some(else_range) = else_range {
            self.last_ast_statement_range_since(else_event_start, Some(else_range))
                .or(Some(else_range))
                .map_or(start_range.end, |range| range.end)
        } else {
            then_ast_range.map_or(start_range.end, |range| range.end)
        };
        self.record_typed_syntax_node(
            SyntaxKind::IfStatement,
            start,
            self.current,
            AstLoweringData::IfStatement {
                condition: condition_range,
                then_block: then_range,
                else_branch: else_range,
                ast_span: ByteRange::new(start_range.start, ast_end),
            },
        );
        Ok(())
    }

    fn parse_else_branch(&mut self) -> ParserResult<Option<ByteRange>> {
        if !self.matches(&[TokenType::Else]) {
            return Ok(None);
        }
        self.skip_newlines();
        if self.matches(&[TokenType::If]) {
            let nested_start = self.syntax_events.len();
            self.if_statement()?;
            return Ok(self.last_statement_range_since(nested_start));
        }
        if !self.check(TokenType::OpenBrace) {
            return Err(ParserError::new(
                DiagnosticCode::ParseExpectedToken,
                "Expected '{' after else",
                self.peek().span,
            ));
        }
        Ok(Some(self.block()?.range))
    }

    fn while_statement(&mut self) -> ParserResult<()> {
        let start = self.current.saturating_sub(1);
        let start_span = self.tokens[start].span;
        let condition = self.parse_expression_parsed()?;
        let condition_range = condition
            .span
            .byte_range
            .expect("while condition has source range");
        self.check_no_postfix_increment_decrement_parsed(&condition)?;
        self.skip_newlines();
        self.loop_depth += 1;
        let body_event_start = self.syntax_events.len();
        let body_result = self.block();
        self.loop_depth -= 1;
        let body_range = body_result?.range;
        let ast_body_range =
            self.last_ast_statement_range_since(body_event_start, Some(body_range));
        let start_range = start_span.byte_range.expect("while statement start range");
        self.record_typed_syntax_node(
            SyntaxKind::WhileStatement,
            start,
            self.current,
            AstLoweringData::WhileStatement {
                condition: condition_range,
                body: body_range,
                ast_span: ByteRange::new(
                    start_range.start,
                    ast_body_range.map_or(start_range.end, |range| range.end),
                ),
            },
        );
        Ok(())
    }

    fn for_statement(&mut self) -> ParserResult<()> {
        let start = self.current.saturating_sub(1);
        let start_span = self.tokens[start].span;
        let var_type = self.parse_type_fact()?;
        self.consume_identifier_fact("Expected variable name")?;
        let variable_range = self
            .previous()
            .span
            .byte_range
            .expect("for variable name has source range");
        let variable_type_range = var_type
            .source_range()
            .expect("for variable type has source range");
        self.consume_token(TokenType::In, "Expected 'in' after variable")?;
        let iter = self.parse_expression_parsed()?;
        let iterator_range = iter.span.byte_range.expect("for iterator has source range");
        self.check_no_postfix_increment_decrement_parsed(&iter)?;
        self.skip_newlines();
        self.loop_depth += 1;
        let body_is_block = self.check(TokenType::OpenBrace);
        let body_event_start = self.syntax_events.len();
        let body_result = if body_is_block {
            self.block().map(|_| ())
        } else {
            self.statement()
        };
        self.loop_depth -= 1;
        body_result?;
        let body_range = self
            .last_statement_range_since(body_event_start)
            .expect("parsed for body has a syntax range");
        let start_range = start_span.byte_range.expect("for statement start range");
        let ast_end = self
            .last_ast_statement_range_since(body_event_start, Some(body_range))
            .map_or(start_range.end, |range| range.end);
        self.record_typed_syntax_node(
            SyntaxKind::ForStatement,
            start,
            self.current,
            AstLoweringData::ForStatement {
                variable: variable_range,
                variable_type: variable_type_range,
                iterator: iterator_range,
                body: body_range,
                body_is_block,
                ast_span: ByteRange::new(start_range.start, ast_end),
            },
        );
        Ok(())
    }

    fn match_statement(&mut self) -> ParserResult<()> {
        let statement_start = self.current.saturating_sub(1);
        let start_span = self.tokens[self.current].span;
        let expr = self.parse_expression_parsed()?;
        let expression_range = expr.span.byte_range.expect("match expression source range");
        self.check_no_postfix_increment_decrement_parsed(&expr)?;
        self.consume_token(TokenType::OpenBrace, "Expected '{' after match expression")?;
        self.skip_newlines();
        let mut arm_ranges = Vec::new();
        while !self.check(TokenType::CloseBrace) && !self.is_at_end() {
            let arm_events_start = self.syntax_events.len();
            self.parse_match_arm(start_span)?;
            arm_ranges.push(
                self.last_syntax_range_since(arm_events_start, |data| {
                    matches!(data, AstLoweringData::MatchArm { .. })
                })
                .expect("match arm syntax range"),
            );
            self.skip_newlines();
            if self.matches(&[TokenType::Comma]) {
                self.skip_newlines();
                if self.check(TokenType::CloseBrace) {
                    break;
                }
            }
        }
        let end_span =
            self.consume_token(TokenType::CloseBrace, "Expected '}' after match arms")?;
        self.record_typed_syntax_node(
            SyntaxKind::MatchStatement,
            statement_start,
            self.current,
            AstLoweringData::MatchStatement {
                expression: expression_range,
                arms: arm_ranges,
                ast_span: start_span
                    .combine(&end_span)
                    .byte_range
                    .expect("match statement span range"),
            },
        );
        Ok(())
    }

    pub(super) fn parse_match_arm(&mut self, start_span: Span) -> ParserResult<()> {
        let arm_start = self.current;
        let pattern_start = self.syntax_events.len();
        self.parse_pattern_fact()?;
        let pattern_range = self
            .last_syntax_range_since(pattern_start, |data| {
                matches!(data, AstLoweringData::Pattern(_))
            })
            .expect("parsed match pattern has a syntax range");
        let guard_range = if self.matches(&[TokenType::If]) {
            self.parse_expression_parsed()?.span.byte_range
        } else {
            None
        };
        self.skip_newlines();
        let body_start = self.syntax_events.len();
        self.parse_match_arm_body(start_span)?;
        let body_range = self
            .last_statement_range_since(body_start)
            .expect("parsed match arm body has a syntax range");
        self.record_typed_syntax_node(
            SyntaxKind::MatchArm,
            arm_start,
            self.current,
            AstLoweringData::MatchArm {
                pattern: pattern_range,
                guard: guard_range,
                body: body_range,
                body_is_expression: false,
            },
        );
        Ok(())
    }

    pub(super) fn parse_match_arm_body(&mut self, _start_span: Span) -> ParserResult<()> {
        if self.check(TokenType::OpenBrace) {
            self.block()?;
        } else if self.matches(&[TokenType::Colon]) {
            self.skip_newlines();
            if self.check(TokenType::OpenBrace) {
                self.block()?;
            } else {
                self.statement()?;
            }
        } else {
            self.statement()?;
        }
        Ok(())
    }

    pub(super) fn return_statement(&mut self) -> ParserResult<()> {
        let start = self.current.saturating_sub(1);
        let start_span = self.tokens[start].span;
        let (value, end_span) = if self.is_at_end()
            || self.check(TokenType::NewLine)
            || self.check(TokenType::CloseBrace)
        {
            (None, start_span)
        } else {
            let expression = self.parse_expression_parsed()?;
            self.check_no_postfix_increment_decrement_parsed(&expression)?;
            (
                Some(
                    expression
                        .span
                        .byte_range
                        .expect("return value has source range"),
                ),
                expression.span,
            )
        };
        self.record_typed_syntax_node(
            SyntaxKind::Statement,
            start,
            self.current,
            AstLoweringData::ReturnStatement {
                value,
                ast_span: start_span
                    .combine(&end_span)
                    .byte_range
                    .expect("return statement range"),
            },
        );
        Ok(())
    }

    fn break_statement(&mut self) -> ParserResult<()> {
        let start = self.current.saturating_sub(1);
        let span = self.tokens[start].span;
        if self.loop_depth == 0 {
            return Err(ParserError::with_help(
                DiagnosticCode::ParseControlFlowOutsideLoop,
                "Cannot use 'break' outside of a loop",
                span,
                "'break' can only be used inside a 'for' or 'while' loop",
            ));
        }
        self.record_typed_syntax_node(
            SyntaxKind::Statement,
            start,
            self.current,
            AstLoweringData::BreakStatement {
                ast_span: span.byte_range.expect("break statement range"),
            },
        );
        Ok(())
    }

    fn continue_statement(&mut self) -> ParserResult<()> {
        let start = self.current.saturating_sub(1);
        let span = self.tokens[start].span;
        if self.loop_depth == 0 {
            return Err(ParserError::with_help(
                DiagnosticCode::ParseControlFlowOutsideLoop,
                "Cannot use 'continue' outside of a loop",
                span,
                "'continue' can only be used inside a 'for' or 'while' loop",
            ));
        }
        self.record_typed_syntax_node(
            SyntaxKind::Statement,
            start,
            self.current,
            AstLoweringData::ContinueStatement {
                ast_span: span.byte_range.expect("continue statement range"),
            },
        );
        Ok(())
    }

    pub(super) fn parse_pattern_fact(&mut self) -> ParserResult<ParsedPatternKind> {
        let pattern_start = self.current;
        let events_start = self.syntax_events.len();
        let kind = self.parse_pattern_inner_fact()?;
        let mut candidates = self.syntax_ranges_since(events_start, |data| {
            matches!(data, AstLoweringData::Pattern(_))
        });
        candidates.sort_by_key(|range| (range.start, std::cmp::Reverse(range.end)));
        let mut child_ranges = Vec::new();
        let mut enclosing = Vec::new();
        for candidate in candidates {
            while enclosing
                .last()
                .is_some_and(|range: &ByteRange| range.end <= candidate.start)
            {
                enclosing.pop();
            }
            if enclosing
                .last()
                .is_some_and(|range| candidate.end <= range.end)
            {
                continue;
            }
            child_ranges.push(candidate);
            enclosing.push(candidate);
        }
        let start_token_range = self.tokens[pattern_start]
            .span
            .byte_range
            .expect("pattern token source range");
        let syntax_pattern = match kind {
            ParsedPatternKind::Literal => SyntaxPattern::Literal {
                token: start_token_range,
            },
            ParsedPatternKind::Identifier => SyntaxPattern::Identifier {
                token: start_token_range,
            },
            ParsedPatternKind::Wildcard => SyntaxPattern::Wildcard,
            ParsedPatternKind::EnumVariant => SyntaxPattern::EnumVariant {
                name: start_token_range,
                args: child_ranges,
            },
            ParsedPatternKind::List { elements, has_rest } => SyntaxPattern::List {
                elements: child_ranges.iter().take(elements).copied().collect(),
                rest: has_rest
                    .then(|| child_ranges.get(elements).copied())
                    .flatten(),
            },
        };
        self.record_typed_syntax_node(
            SyntaxKind::Pattern,
            pattern_start,
            self.current,
            AstLoweringData::Pattern(syntax_pattern),
        );
        Ok(kind)
    }

    fn parse_pattern_inner_fact(&mut self) -> ParserResult<ParsedPatternKind> {
        match &self.peek().token_type {
            TokenType::None => {
                self.advance();
                Ok(ParsedPatternKind::EnumVariant)
            }
            TokenType::Id(_) => {
                self.advance();
                if self.matches(&[TokenType::OpenParen]) {
                    if !self.check(TokenType::CloseParen) {
                        loop {
                            self.parse_pattern_fact()?;
                            if !self.matches(&[TokenType::Comma]) {
                                break;
                            }
                            self.skip_newlines();
                        }
                    }
                    self.consume_token(
                        TokenType::CloseParen,
                        "Expected ')' after enum variant arguments",
                    )?;
                    Ok(ParsedPatternKind::EnumVariant)
                } else {
                    Ok(ParsedPatternKind::Identifier)
                }
            }
            TokenType::Underscore => {
                self.advance();
                Ok(ParsedPatternKind::Wildcard)
            }
            TokenType::OpenBracket => {
                self.advance();
                self.skip_newlines();
                let mut element_count = 0;
                let mut has_rest = false;
                if !self.check(TokenType::CloseBracket) {
                    loop {
                        self.skip_newlines();
                        if self.check(TokenType::DotDot) {
                            self.advance();
                            has_rest = true;
                            self.parse_pattern_fact()?;
                            self.skip_newlines();
                            break;
                        }
                        self.parse_pattern_fact()?;
                        element_count += 1;
                        self.skip_newlines();
                        if !self.matches(&[TokenType::Comma]) {
                            break;
                        }
                        self.skip_newlines();
                    }
                }
                self.consume_token(TokenType::CloseBracket, "Expected ']' after list pattern")?;
                Ok(ParsedPatternKind::List {
                    elements: element_count,
                    has_rest,
                })
            }
            _ => {
                let token = self.consume();
                if !matches!(
                    &token.token_type,
                    TokenType::Int(_)
                        | TokenType::Float(_)
                        | TokenType::Bool(_)
                        | TokenType::Char(_)
                        | TokenType::Str(_)
                ) {
                    return Err(ParserError::from_token(
                        DiagnosticCode::InvalidPattern,
                        "Expected pattern",
                        token,
                    ));
                }
                Ok(ParsedPatternKind::Literal)
            }
        }
    }

    pub(super) fn skip_newlines(&mut self) -> usize {
        let mut count = 0;
        while self.matches(&[TokenType::NewLine]) {
            count += 1;
        }
        count
    }

    pub(super) fn block(&mut self) -> ParserResult<BlockStatementFact> {
        let start = self.current;
        let result = self.block_inner();
        if result.is_ok() {
            self.record_typed_syntax_node(
                SyntaxKind::Block,
                start,
                self.current,
                AstLoweringData::Block,
            );
        } else if self.current > start {
            self.record_syntax_node(SyntaxKind::Error, start, self.current);
        }
        result
    }

    fn block_inner(&mut self) -> ParserResult<BlockStatementFact> {
        let block_start = self.current;
        self.consume_token(TokenType::OpenBrace, "Expected '{' before block")?;
        self.skip_newlines();

        if self.matches(&[TokenType::CloseBrace]) {
            return Ok(BlockStatementFact {
                range: self
                    .source_range_for_tokens(block_start, self.current)
                    .expect("empty block has source range"),
            });
        }

        self.parse_syntax_block_statements_loop()?;
        if self.check(TokenType::CloseBrace) {
            self.consume_token(TokenType::CloseBrace, "Expected '}' after block")?;
        } else {
            return Err(ParserError::new(
                DiagnosticCode::ParseExpectedToken,
                "Expected '}' after block".to_string(),
                self.peek().span,
            ));
        }
        Ok(BlockStatementFact {
            range: self
                .source_range_for_tokens(block_start, self.current)
                .expect("block has source range"),
        })
    }

    fn parse_syntax_block_statements_loop(&mut self) -> ParserResult<()> {
        while !self.check(TokenType::CloseBrace) && !self.is_at_end() {
            self.skip_newlines();
            if self.check(TokenType::CloseBrace) {
                break;
            }
            self.parse_block_statement()?;
        }
        Ok(())
    }

    fn parse_block_statement(&mut self) -> ParserResult<()> {
        let start_position = self.current;
        match self.declaration() {
            Ok(()) => {
                self.skip_newlines();
                if self.current == start_position
                    && !self.check(TokenType::CloseBrace)
                    && !self.is_at_end()
                {
                    self.advance();
                }
                Ok(())
            }
            Err(e) => {
                self.record_error(e);
                self.synchronize();
                if self.current == start_position && !self.is_at_end() {
                    self.advance();
                }
                Ok(())
            }
        }
    }

    pub(super) fn is_in_block(&self) -> bool {
        // look backwards for an opening brace that doesn't have a matching closing brace.
        let mut brace_count: usize = 0;
        for i in (0..self.current).rev() {
            match self.tokens[i].token_type {
                TokenType::CloseBrace => brace_count += 1,
                TokenType::OpenBrace => {
                    if brace_count == 0 {
                        return true;
                    }
                    brace_count = brace_count.saturating_sub(1);
                }
                _ => {}
            }
        }
        false
    }

    fn parse_expression_statement(&mut self) -> ParserResult<()> {
        let start = self.current;
        let expr = self.parse_expression_parsed()?;
        self.record_typed_syntax_node(
            SyntaxKind::Statement,
            start,
            self.current,
            AstLoweringData::ExpressionStatement {
                expression: expr
                    .span
                    .byte_range
                    .expect("parsed expression has source range"),
                ast_span: expr
                    .span
                    .byte_range
                    .expect("expression statement AST range"),
            },
        );
        let has_newline = self.check(TokenType::NewLine);
        if !self.is_in_block() && !has_newline && self.current < self.tokens.len() {
            let next_token = &self.tokens[self.current];
            if self.is_statement_starter(&next_token.token_type) {
                return Err(ParserError::new(
                    DiagnosticCode::ParseExpectedToken,
                    "Expected newline before statement".to_string(),
                    next_token.span,
                ));
            }
        }
        let _ = self.skip_newlines();

        // Validate that postfix ++ and -- only appear at statement level
        self.validate_postfix_in_statement(&expr)?;

        Ok(())
    }

    pub(super) fn validate_postfix_in_statement(
        &self,
        expr: &ParsedExpression,
    ) -> ParserResult<()> {
        let range = expr.span.byte_range;
        let has_top_level_update = self.syntax_events.iter().any(|event| {
            Some(event.range) == range
                && matches!(
                    event.data.as_ref(),
                    Some(AstLoweringData::Unary { postfix: true, .. })
                )
        });
        if has_top_level_update {
            return Ok(());
        }
        self.check_no_postfix_increment_decrement_range(range)
    }

    pub(super) fn check_no_postfix_increment_decrement_parsed(
        &self,
        expr: &ParsedExpression,
    ) -> ParserResult<()> {
        self.check_no_postfix_increment_decrement_range(expr.span.byte_range)
    }

    #[allow(clippy::only_used_in_recursion)]
    pub(super) fn check_no_postfix_increment_decrement_range(
        &self,
        expression_range: Option<crate::lexer::ByteRange>,
    ) -> ParserResult<()> {
        if let Some(range) = self.first_postfix_update_in(expression_range) {
            return Err(ParserError::with_help(
                DiagnosticCode::ParseExpectedToken,
                "Increment/Decrement operator can only be used as a standalone statement",
                self.span_for_byte_range(range),
                "Expressions like 'x + y++' are not supported. Use 'y++' as a separate statement before the expression.",
            ));
        }
        Ok(())
    }
}
