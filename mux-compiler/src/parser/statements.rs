use super::expressions::ParsedExpression;
use super::types::TypeFact;
use super::*;

/// Parsed facts for leaf statements. The syntax event is authoritative; this
/// temporary value carries expressions needed by the compatibility parser.
pub(super) struct LeafStatement {
    range: ByteRange,
    span: Span,
    kind: LeafStatementKind,
}

enum LeafStatementKind {
    Expression(ExpressionNode),
    Return(Option<ExpressionNode>),
    Break,
    Continue,
}

/// Parsed facts for a block. The syntax event records its structural range;
/// child statements remain available here for compatibility parser callers.
pub(super) struct BlockStatementFact {
    pub(super) range: ByteRange,
    pub(super) span: Span,
    pub(super) statements: Vec<StatementNode>,
}

/// Parsed facts for a `where` clause. Predicate expressions are retained for
/// the compatibility parser; syntax consumers use the recorded ranges.
pub(super) struct WhereClauseFact {
    pub(super) range: ByteRange,
    pub(super) span: Span,
    predicates: Vec<ExpressionNode>,
}

impl WhereClauseFact {
    pub(super) fn into_compatibility(self) -> WhereClause {
        let Self {
            range,
            span,
            predicates,
        } = self;
        debug_assert!(span.byte_range.is_some_and(|span_range| {
            range.start <= span_range.start && span_range.end <= range.end
        }));
        WhereClause { predicates, span }
    }
}

impl BlockStatementFact {
    pub(super) fn into_compatibility_ast(self) -> AstNode {
        let Self {
            range,
            span,
            statements,
        } = self;
        debug_assert!(span.byte_range.is_some_and(|span_range| {
            range.start <= span_range.start && span_range.end <= range.end
        }));
        AstNode::Statement(StatementNode {
            kind: StatementKind::Block(statements),
            span,
        })
    }
}

/// Parsed facts for a `while` statement. Syntax data is authoritative; the
/// remaining parser values are retained for the compatibility AST adapter.
struct WhileStatementFact {
    range: ByteRange,
    span: Span,
    condition: Option<ExpressionNode>,
    body: Vec<StatementNode>,
}

impl WhileStatementFact {
    fn into_compatibility_ast(self) -> AstNode {
        let Self {
            range,
            span,
            condition,
            body,
        } = self;
        debug_assert!(span.byte_range.is_some_and(|span_range| {
            range.start <= span_range.start && span_range.end <= range.end
        }));
        AstNode::Statement(StatementNode {
            kind: StatementKind::While {
                cond: condition.expect("compatibility while condition"),
                body,
            },
            span,
        })
    }
}

/// Parsed facts for a `for` statement, materialized into the legacy AST only
/// at the statement dispatch boundary.
struct ForStatementFact {
    range: ByteRange,
    span: Span,
    variable: String,
    variable_type: TypeFact,
    iterator: Option<ExpressionNode>,
    body: Vec<StatementNode>,
}

impl ForStatementFact {
    fn into_compatibility_ast(self) -> AstNode {
        let Self {
            range,
            span,
            variable,
            variable_type,
            iterator,
            body,
        } = self;
        debug_assert!(span.byte_range.is_some_and(|span_range| {
            range.start <= span_range.start && span_range.end <= range.end
        }));
        AstNode::Statement(StatementNode {
            kind: StatementKind::For {
                var: variable,
                var_type: variable_type.into_compat_type_node(),
                iter: iterator.expect("compatibility for iterator"),
                body,
            },
            span,
        })
    }
}

/// Parsed facts for a `match` statement, kept separate from the compatibility
/// AST assembled at statement dispatch.
struct MatchStatementFact {
    range: ByteRange,
    span: Span,
    expression: Option<ExpressionNode>,
    arms: Vec<MatchArm>,
}

struct ParsedPatternFact {
    node: Option<PatternNode>,
    kind: ParsedPatternKind,
}

#[derive(Clone, Copy)]
enum ParsedPatternKind {
    Literal,
    Identifier,
    Wildcard,
    EnumVariant,
    List { elements: usize, has_rest: bool },
}

impl MatchStatementFact {
    fn into_compatibility_ast(self) -> AstNode {
        let Self {
            range,
            span,
            expression,
            arms,
        } = self;
        debug_assert!(span.byte_range.is_some_and(|span_range| {
            range.start <= span_range.start && span_range.end <= range.end
        }));
        AstNode::Statement(StatementNode {
            kind: StatementKind::Match {
                expr: expression.expect("compatibility match expression"),
                arms,
            },
            span,
        })
    }
}

/// Syntax facts for an `if` statement. Child expressions and blocks remain
/// parser values until the compatibility adapter assembles the AST node.
struct IfStatementFact {
    range: ByteRange,
    span: Span,
    condition: Option<ExpressionNode>,
    then_block: Vec<StatementNode>,
    else_block: Option<Vec<StatementNode>>,
}

impl IfStatementFact {
    fn into_compatibility_ast(self) -> AstNode {
        let Self {
            range,
            span,
            condition,
            then_block,
            else_block,
        } = self;
        debug_assert!(span.byte_range.is_some_and(|span_range| {
            range.start <= span_range.start && span_range.end <= range.end
        }));
        AstNode::Statement(StatementNode {
            kind: StatementKind::If {
                cond: condition.expect("compatibility if condition"),
                then_block,
                else_block,
            },
            span,
        })
    }
}

impl LeafStatement {
    pub(super) fn into_compatibility_ast(self) -> AstNode {
        let LeafStatement { range, span, kind } = self;
        debug_assert!(span.byte_range.is_some_and(|span_range| {
            range.start <= span_range.start && span_range.end <= range.end
        }));
        AstNode::Statement(StatementNode {
            kind: match kind {
                LeafStatementKind::Expression(expression) => StatementKind::Expression(expression),
                LeafStatementKind::Return(value) => StatementKind::Return(value),
                LeafStatementKind::Break => StatementKind::Break,
                LeafStatementKind::Continue => StatementKind::Continue,
            },
            span,
        })
    }
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
        let mut predicates = Vec::new();
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
            if self.mode == ParserMode::Compatibility {
                predicates.push(predicate.node.expect("compatibility where predicate"));
            }
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
        let range = self
            .source_range_for_tokens(start, self.current)
            .expect("where clause has source range");
        self.record_typed_syntax_node(
            SyntaxKind::WhereClause,
            start,
            self.current,
            SyntaxData::WhereClause {
                predicates: predicate_ranges,
            },
        );
        Ok(Some(WhereClauseFact {
            range,
            span,
            predicates,
        }))
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

    pub(super) fn parse_function_body(
        &mut self,
        _start_span: Span,
    ) -> ParserResult<Vec<StatementNode>> {
        Ok(self.block()?.statements)
    }

    pub(super) fn statement(&mut self) -> ParserResult<AstNode> {
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

    pub(super) fn statement_inner(&mut self) -> ParserResult<AstNode> {
        let result = if self.matches(&[TokenType::If]) {
            self.if_statement()
                .map(IfStatementFact::into_compatibility_ast)
        } else if self.matches(&[TokenType::While]) {
            self.while_statement()
                .map(WhileStatementFact::into_compatibility_ast)
        } else if self.matches(&[TokenType::For]) {
            self.for_statement()
                .map(ForStatementFact::into_compatibility_ast)
        } else if self.matches(&[TokenType::Match]) {
            self.match_statement()
                .map(MatchStatementFact::into_compatibility_ast)
        } else if self.matches(&[TokenType::Break]) {
            self.break_statement()
                .map(LeafStatement::into_compatibility_ast)
        } else if self.matches(&[TokenType::Continue]) {
            self.continue_statement()
                .map(LeafStatement::into_compatibility_ast)
        } else if self.matches(&[TokenType::Return]) {
            self.return_statement()
                .map(LeafStatement::into_compatibility_ast)
        } else if self.matches(&[TokenType::Func]) {
            self.function_declaration(false)
        } else if self.looks_like_typed_decl() {
            self.typed_declaration()
        } else if self.check(TokenType::OpenBrace) {
            self.block().map(BlockStatementFact::into_compatibility_ast)
        } else {
            self.expression_statement()
                .map(LeafStatement::into_compatibility_ast)
        }?;

        // only do newline-based termination at top level, skip for control-flow statements.
        if !self.is_in_block()
            && !matches!(
                &result,
                AstNode::Statement(stmt) if matches!(
                    stmt.kind,
                    StatementKind::If { .. } | StatementKind::While { .. } | StatementKind::For { .. } | StatementKind::Match { .. } | StatementKind::Function { .. }
                )
            )
            && let Err(e) = self.check_statement_termination()
        {
            self.record_error(e);
        }

        Ok(result)
    }

    /// Parse a statement for syntax consumers without assembling its root
    /// compatibility AST node.
    pub(super) fn syntax_only_statement(&mut self) -> ParserResult<()> {
        let checkpoint = self.checkpoint();
        let result = self.syntax_only_statement_inner();
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

    fn syntax_only_statement_inner(&mut self) -> ParserResult<()> {
        if self.matches(&[TokenType::If]) {
            self.if_statement().map(drop)
        } else if self.matches(&[TokenType::While]) {
            self.while_statement().map(drop)
        } else if self.matches(&[TokenType::For]) {
            self.for_statement().map(drop)
        } else if self.matches(&[TokenType::Match]) {
            self.match_statement().map(drop)
        } else if self.matches(&[TokenType::Return]) {
            self.syntax_only_return_statement()
        } else if self.matches(&[TokenType::Break]) {
            self.syntax_only_break_statement()
        } else if self.matches(&[TokenType::Continue]) {
            self.syntax_only_continue_statement()
        } else if self.matches(&[TokenType::Func]) {
            self.syntax_only_function_declaration(false)
        } else if self.check(TokenType::OpenBrace) {
            self.block().map(drop)
        } else if self.looks_like_typed_decl() {
            self.typed_declaration_fact().map(drop)
        } else {
            self.syntax_only_expression_statement()
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

    fn if_statement(&mut self) -> ParserResult<IfStatementFact> {
        let start = self.current.saturating_sub(1);
        let start_span = self.tokens[start].span;
        let condition = self.parse_expression_parsed()?;
        let condition_range = condition
            .span
            .byte_range
            .expect("if condition has source range");
        self.check_no_postfix_increment_decrement_parsed(&condition)?;
        self.skip_newlines();

        // Parse then block using the block() function directly
        let then_fact = self.block()?;
        let then_range = then_fact.range;
        let then_block = then_fact.statements;

        self.skip_newlines();
        let else_body_start = if self.check(TokenType::Else) {
            let mut index = self.current + 1;
            while self
                .tokens
                .get(index)
                .is_some_and(|token| token.token_type == TokenType::NewLine)
            {
                index += 1;
            }
            Some(index)
        } else {
            None
        };
        let else_event_start = self.syntax_events.len();
        let (else_block, end_span) = self.parse_else_branch(start_span, &then_block)?;
        let else_range = if else_block.is_some() {
            self.last_statement_range_since(else_event_start)
                .or_else(|| {
                    else_body_start.and_then(|body_start| {
                        self.source_range_for_tokens(body_start, self.current)
                    })
                })
        } else {
            None
        };
        let range = self
            .source_range_for_tokens(start, self.current)
            .expect("parsed if statement has a syntax range");
        self.record_typed_syntax_node(
            SyntaxKind::IfStatement,
            start,
            self.current,
            SyntaxData::IfStatement {
                condition: condition_range,
                then_block: then_range,
                else_branch: else_range,
            },
        );
        let span = start_span.combine(&end_span);
        let condition = condition.node;
        Ok(IfStatementFact {
            range,
            span,
            condition,
            then_block,
            else_block,
        })
    }

    pub(super) fn parse_else_branch(
        &mut self,
        start_span: Span,
        then_block: &[StatementNode],
    ) -> ParserResult<(Option<Vec<StatementNode>>, Span)> {
        if !self.matches(&[TokenType::Else]) {
            let end_span = then_block.last().map_or(start_span, |s| s.span);
            return Ok((None, end_span));
        }
        self.skip_newlines();
        if self.matches(&[TokenType::If]) {
            self.parse_else_if_branch()
        } else {
            self.parse_else_block()
        }
    }

    pub(super) fn parse_else_if_branch(
        &mut self,
    ) -> ParserResult<(Option<Vec<StatementNode>>, Span)> {
        if self.mode == ParserMode::SyntaxOnly {
            let nested = self.if_statement()?;
            return Ok((Some(Vec::new()), nested.span));
        }

        let nested = self.if_statement()?.into_compatibility_ast();
        match nested {
            AstNode::Statement(stmt) => {
                let end_span = stmt.span;
                Ok((Some(vec![stmt]), end_span))
            }
            _ => Err(ParserError::new(
                DiagnosticCode::ParseExpectedToken,
                "Expected statement after else if",
                self.previous().span,
            )),
        }
    }

    pub(super) fn parse_else_block(&mut self) -> ParserResult<(Option<Vec<StatementNode>>, Span)> {
        // Check for opening brace first with proper error message
        if !self.check(TokenType::OpenBrace) {
            return Err(ParserError::new(
                DiagnosticCode::ParseExpectedToken,
                "Expected '{' after else",
                self.peek().span,
            ));
        }
        // Parse the else block using block() which handles the opening brace
        let else_block = self.block()?.statements;
        let end_span = else_block
            .last()
            .map_or_else(|| self.tokens[self.current - 1].span, |s| s.span);
        Ok((Some(else_block), end_span))
    }

    fn while_statement(&mut self) -> ParserResult<WhileStatementFact> {
        let start = self.current.saturating_sub(1);
        let start_span = self.tokens[start].span;
        let condition = self.parse_expression_parsed()?;
        let condition_range = condition
            .span
            .byte_range
            .expect("while condition has source range");

        // Validate that postfix ++ and -- don't appear in condition
        self.check_no_postfix_increment_decrement_parsed(&condition)?;

        // allow newline(s) before body.
        self.skip_newlines();
        self.loop_depth += 1;
        let body_result = self.block();
        self.loop_depth -= 1;
        let body_fact = body_result?;
        let body_range = body_fact.range;
        let body_statements = body_fact.statements;

        let end_span = body_statements.last().map_or(start_span, |s| s.span);
        let span = start_span.combine(&end_span);

        let range = self
            .source_range_for_tokens(start, self.current)
            .expect("while statement has source range");
        self.record_typed_syntax_node(
            SyntaxKind::WhileStatement,
            start,
            self.current,
            SyntaxData::WhileStatement {
                condition: condition_range,
                body: body_range,
            },
        );

        let condition = condition.node;
        Ok(WhileStatementFact {
            range,
            span,
            condition,
            body: body_statements,
        })
    }

    fn for_statement(&mut self) -> ParserResult<ForStatementFact> {
        let start = self.current.saturating_sub(1);
        let start_span = self.tokens[start].span;

        // parse the variable type.
        let var_type = self.parse_type_fact()?;

        let var = self.consume_identifier("Expected variable name")?;
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

        // Validate that postfix ++ and -- don't appear in iterator expression
        self.check_no_postfix_increment_decrement_parsed(&iter)?;

        // allow newline(s) before body.
        self.skip_newlines();
        self.loop_depth += 1;
        let body_is_block = self.check(TokenType::OpenBrace);
        let body_event_start = self.syntax_events.len();
        let body_result: ParserResult<Option<AstNode>> = if self.mode == ParserMode::SyntaxOnly {
            if body_is_block {
                self.block().map(|_| None)
            } else {
                self.syntax_only_statement().map(|()| None)
            }
        } else if body_is_block {
            self.block()
                .map(BlockStatementFact::into_compatibility_ast)
                .map(Some)
        } else {
            self.statement().map(Some)
        };
        self.loop_depth -= 1;
        let body = body_result?;
        let body_range = self
            .last_statement_range_since(body_event_start)
            .expect("parsed for body has a syntax range");

        let body_statements = match (self.mode, body) {
            (ParserMode::SyntaxOnly, _) => Vec::new(),
            (ParserMode::Compatibility, Some(AstNode::Statement(stmt))) => match stmt.kind {
                StatementKind::Block(block) => block,
                _ => vec![stmt],
            },
            (ParserMode::Compatibility, _) => {
                return Err(ParserError::new(
                    DiagnosticCode::ParseExpectedToken,
                    "Expected statement after for loop",
                    start_span,
                ));
            }
        };

        let end_span = body_statements.last().map_or(start_span, |s| s.span);
        let span = start_span.combine(&end_span);
        let range = self
            .source_range_for_tokens(start, self.current)
            .expect("for statement has source range");
        self.record_typed_syntax_node(
            SyntaxKind::ForStatement,
            start,
            self.current,
            SyntaxData::ForStatement {
                variable: variable_range,
                variable_type: variable_type_range,
                iterator: iterator_range,
                body: body_range,
                body_is_block,
            },
        );
        let iterator = iter.node;
        Ok(ForStatementFact {
            range,
            span,
            variable: var,
            variable_type: var_type,
            iterator,
            body: body_statements,
        })
    }

    fn match_statement(&mut self) -> ParserResult<MatchStatementFact> {
        let statement_start = self.current.saturating_sub(1);
        let start_span = self.tokens[self.current].span;
        let expr = self.parse_expression_parsed()?;
        let expression_range = expr.span.byte_range.expect("match expression source range");

        // Validate that postfix ++ and -- don't appear in match expression
        self.check_no_postfix_increment_decrement_parsed(&expr)?;

        self.consume_token(TokenType::OpenBrace, "Expected '{' after match expression")?;
        self.skip_newlines();

        let mut arms = Vec::new();
        let mut arm_ranges = Vec::new();
        while !self.check(TokenType::CloseBrace) && !self.is_at_end() {
            let arm_events_start = self.syntax_events.len();
            if let Some(arm) = self.parse_match_arm(start_span)? {
                arms.push(arm);
            }
            arm_ranges.push(
                self.last_syntax_range_since(arm_events_start, |data| {
                    matches!(data, SyntaxData::MatchArm { .. })
                })
                .expect("match arm syntax range"),
            );
            self.skip_newlines();
            if self.matches(&[TokenType::Comma]) {
                self.skip_newlines();
                // if the next token is a closing brace, break to handle trailing comma
                if self.check(TokenType::CloseBrace) {
                    break;
                }
            }
        }

        let end_span =
            self.consume_token(TokenType::CloseBrace, "Expected '}' after match arms")?;
        let span = start_span.combine(&end_span);
        let range = self
            .source_range_for_tokens(statement_start, self.current)
            .expect("match statement has source range");
        self.record_typed_syntax_node(
            SyntaxKind::MatchStatement,
            statement_start,
            self.current,
            SyntaxData::MatchStatement {
                expression: expression_range,
                arms: arm_ranges,
                ast_span: span.byte_range.expect("match statement span range"),
            },
        );

        let expr = expr.node;
        Ok(MatchStatementFact {
            range,
            span,
            expression: expr,
            arms,
        })
    }

    /// Helper: Parses a single match arm, including pattern, optional guard, and arm body.
    pub(super) fn parse_match_arm(&mut self, start_span: Span) -> ParserResult<Option<MatchArm>> {
        let arm_start = self.current;
        let pattern_start = self.syntax_events.len();
        let pattern = self.parse_pattern_fact(self.mode == ParserMode::Compatibility)?;
        let pattern_range = self
            .last_syntax_range_since(pattern_start, |data| matches!(data, SyntaxData::Pattern(_)))
            .expect("parsed match pattern has a syntax range");
        let guard = if self.matches(&[TokenType::If]) {
            let expression = self.parse_expression_parsed()?;
            let guard_range = expression.span.byte_range;
            let compatibility_guard = expression.node;
            (compatibility_guard, guard_range)
        } else {
            (None, None)
        };
        let (guard, guard_range) = guard;
        self.skip_newlines();
        let body_start = self.syntax_events.len();
        let body = self.parse_match_arm_body(start_span)?;
        let body_range = self
            .last_statement_range_since(body_start)
            .expect("parsed match arm body has a syntax range");
        self.record_typed_syntax_node(
            SyntaxKind::MatchArm,
            arm_start,
            self.current,
            SyntaxData::MatchArm {
                pattern: pattern_range,
                guard: guard_range,
                body: body_range,
                body_is_expression: false,
            },
        );
        Ok(pattern.node.map(|pattern| MatchArm {
            pattern,
            guard,
            body: body.expect("compatibility match arm has an AST body"),
        }))
    }

    /// Helper: Parses the body of a match arm, returning a vector of statement nodes.
    pub(super) fn parse_match_arm_body(
        &mut self,
        start_span: Span,
    ) -> ParserResult<Option<Vec<StatementNode>>> {
        if self.mode == ParserMode::SyntaxOnly {
            if self.check(TokenType::OpenBrace) {
                self.block()?;
            } else if self.matches(&[TokenType::Colon]) {
                self.skip_newlines();
                if self.check(TokenType::OpenBrace) {
                    self.block()?;
                } else {
                    self.syntax_only_statement()?;
                }
            } else {
                self.syntax_only_statement()?;
            }
            return Ok(None);
        }

        let node = if self.check(TokenType::OpenBrace) {
            self.block()?.into_compatibility_ast()
        } else if self.matches(&[TokenType::Colon]) {
            self.skip_newlines();
            if self.check(TokenType::OpenBrace) {
                self.block()?.into_compatibility_ast()
            } else {
                self.statement()?
            }
        } else {
            self.statement()?
        };
        match node {
            AstNode::Statement(stmt) => match stmt.kind {
                StatementKind::Block(block) => Ok(Some(block)),
                _ => Ok(Some(vec![stmt])),
            },
            _ => Err(ParserError::new(
                DiagnosticCode::ParseExpectedToken,
                "Expected statement for match arm body",
                start_span,
            )),
        }
    }

    pub(super) fn parse_pattern_for_mode(
        &mut self,
        materialize: bool,
    ) -> ParserResult<Option<PatternNode>> {
        Ok(self.parse_pattern_fact(materialize)?.node)
    }

    fn parse_pattern_fact(&mut self, materialize: bool) -> ParserResult<ParsedPatternFact> {
        let pattern_start = self.current;
        let events_start = self.syntax_events.len();
        let ParsedPatternFact { node, kind } = self.parse_pattern_inner_fact(materialize)?;
        let mut candidates =
            self.syntax_ranges_since(events_start, |data| matches!(data, SyntaxData::Pattern(_)));
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
            SyntaxData::Pattern(syntax_pattern),
        );
        Ok(ParsedPatternFact { node, kind })
    }

    fn parse_pattern_inner_fact(&mut self, materialize: bool) -> ParserResult<ParsedPatternFact> {
        match &self.peek().token_type {
            TokenType::None => {
                self.advance(); // consume none
                Ok(ParsedPatternFact {
                    node: materialize.then(|| PatternNode::EnumVariant {
                        name: "none".to_string(),
                        args: vec![],
                    }),
                    kind: ParsedPatternKind::EnumVariant,
                })
            }
            TokenType::Id(name) => {
                let name_clone = materialize.then(|| name.clone());
                self.advance(); // consume the identifier
                if self.matches(&[TokenType::OpenParen]) {
                    let mut args = Vec::new();
                    if !self.check(TokenType::CloseParen) {
                        loop {
                            let arg = self.parse_pattern_fact(materialize)?;
                            if let Some(node) = arg.node {
                                args.push(node);
                            }
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
                    Ok(ParsedPatternFact {
                        node: materialize.then(|| PatternNode::EnumVariant {
                            name: name_clone.expect("compatibility pattern name"),
                            args,
                        }),
                        kind: ParsedPatternKind::EnumVariant,
                    })
                } else {
                    Ok(ParsedPatternFact {
                        node: name_clone.map(PatternNode::Identifier),
                        kind: ParsedPatternKind::Identifier,
                    })
                }
            }
            TokenType::Underscore => {
                self.advance(); // consume the underscore
                Ok(ParsedPatternFact {
                    node: if materialize {
                        Some(PatternNode::Wildcard)
                    } else {
                        None
                    },
                    kind: ParsedPatternKind::Wildcard,
                })
            }
            TokenType::OpenBracket => {
                self.advance();
                self.skip_newlines();
                let mut elements = Vec::new();
                let mut element_count = 0;
                let mut rest = None;
                let mut has_rest = false;
                if !self.check(TokenType::CloseBracket) {
                    loop {
                        self.skip_newlines();
                        if self.check(TokenType::DotDot) {
                            self.advance();
                            has_rest = true;
                            let parsed_rest = self.parse_pattern_fact(materialize)?;
                            rest = parsed_rest.node.map(Box::new);
                            self.skip_newlines();
                            break;
                        }
                        let element = self.parse_pattern_fact(materialize)?;
                        if let Some(node) = element.node {
                            elements.push(node);
                        }
                        element_count += 1;
                        self.skip_newlines();
                        if !self.matches(&[TokenType::Comma]) {
                            break;
                        }
                        self.skip_newlines();
                    }
                }
                self.consume_token(TokenType::CloseBracket, "Expected ']' after list pattern")?;
                Ok(ParsedPatternFact {
                    node: if materialize {
                        Some(PatternNode::List { elements, rest })
                    } else {
                        None
                    },
                    kind: ParsedPatternKind::List {
                        elements: element_count,
                        has_rest,
                    },
                })
            }
            _ => {
                let token = self.consume();
                let is_literal = matches!(
                    &token.token_type,
                    TokenType::Int(_)
                        | TokenType::Float(_)
                        | TokenType::Bool(_)
                        | TokenType::Char(_)
                        | TokenType::Str(_)
                );
                if !is_literal {
                    return Err(ParserError::from_token(
                        DiagnosticCode::InvalidPattern,
                        "Expected pattern",
                        token,
                    ));
                }
                Ok(ParsedPatternFact {
                    node: materialize.then(|| {
                        PatternNode::Literal(match &token.token_type {
                            TokenType::Int(n) => LiteralNode::Integer(*n),
                            TokenType::Float(f) => LiteralNode::Float(*f),
                            TokenType::Bool(b) => LiteralNode::Boolean(*b),
                            TokenType::Char(c) => LiteralNode::Char(*c),
                            TokenType::Str(s) => LiteralNode::String(s.clone()),
                            _ => unreachable!("literal pattern token was validated"),
                        })
                    }),
                    kind: ParsedPatternKind::Literal,
                })
            }
        }
    }

    pub(super) fn return_statement(&mut self) -> ParserResult<LeafStatement> {
        let (start, start_span, value, end_span) = self.parse_return_statement()?;
        Ok(LeafStatement {
            range: self
                .source_range_for_tokens(start, self.current)
                .expect("return statement has source range"),
            kind: LeafStatementKind::Return(value),
            span: start_span.combine(&end_span),
        })
    }

    fn syntax_only_return_statement(&mut self) -> ParserResult<()> {
        self.parse_return_statement().map(drop)
    }

    fn parse_return_statement(
        &mut self,
    ) -> ParserResult<(usize, Span, Option<ExpressionNode>, Span)> {
        let start = self.current.saturating_sub(1);
        let start_span = self.tokens[start].span;

        // Check if there's an expression after return
        let (value, value_range, end_span) = if self.is_at_end()
            || self.check(TokenType::NewLine)
            || self.check(TokenType::CloseBrace)
        {
            // return at end of input, or followed by newline/closing brace - void return
            (None, None, start_span)
        } else {
            let expr = self.parse_expression_parsed()?;

            // Validate that postfix ++ and -- don't appear in return value
            self.check_no_postfix_increment_decrement_parsed(&expr)?;

            let value_range = expr
                .span
                .byte_range
                .expect("parsed return expression has source range");
            let end_span = expr.span;
            (expr.node, Some(value_range), end_span)
        };

        self.record_typed_syntax_node(
            SyntaxKind::Statement,
            start,
            self.current,
            SyntaxData::ReturnStatement { value: value_range },
        );

        Ok((start, start_span, value, end_span))
    }

    pub(super) fn break_statement(&mut self) -> ParserResult<LeafStatement> {
        let (start, start_span) = self.parse_break_statement()?;
        Ok(LeafStatement {
            range: self
                .source_range_for_tokens(start, self.current)
                .expect("break statement has source range"),
            kind: LeafStatementKind::Break,
            span: start_span,
        })
    }

    fn syntax_only_break_statement(&mut self) -> ParserResult<()> {
        self.parse_break_statement().map(drop)
    }

    fn parse_break_statement(&mut self) -> ParserResult<(usize, Span)> {
        let start = self.current.saturating_sub(1);
        let start_span = self.tokens[start].span;

        if self.loop_depth == 0 {
            return Err(ParserError::with_help(
                DiagnosticCode::ParseControlFlowOutsideLoop,
                "Cannot use 'break' outside of a loop",
                start_span,
                "'break' can only be used inside a 'for' or 'while' loop",
            ));
        }

        self.record_typed_syntax_node(
            SyntaxKind::Statement,
            start,
            self.current,
            SyntaxData::BreakStatement,
        );

        Ok((start, start_span))
    }

    pub(super) fn continue_statement(&mut self) -> ParserResult<LeafStatement> {
        let (start, start_span) = self.parse_continue_statement()?;
        Ok(LeafStatement {
            range: self
                .source_range_for_tokens(start, self.current)
                .expect("continue statement has source range"),
            kind: LeafStatementKind::Continue,
            span: start_span,
        })
    }

    fn syntax_only_continue_statement(&mut self) -> ParserResult<()> {
        self.parse_continue_statement().map(drop)
    }

    fn parse_continue_statement(&mut self) -> ParserResult<(usize, Span)> {
        let start = self.current.saturating_sub(1);
        let start_span = self.tokens[start].span;

        if self.loop_depth == 0 {
            return Err(ParserError::with_help(
                DiagnosticCode::ParseControlFlowOutsideLoop,
                "Cannot use 'continue' outside of a loop",
                start_span,
                "'continue' can only be used inside a 'for' or 'while' loop",
            ));
        }

        self.record_typed_syntax_node(
            SyntaxKind::Statement,
            start,
            self.current,
            SyntaxData::ContinueStatement,
        );

        Ok((start, start_span))
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
                SyntaxData::Block,
            );
        } else if self.current > start {
            self.record_syntax_node(SyntaxKind::Error, start, self.current);
        }
        result
    }

    fn block_inner(&mut self) -> ParserResult<BlockStatementFact> {
        let block_start = self.current;
        let start_span = self.consume_token(TokenType::OpenBrace, "Expected '{' before block")?;
        self.skip_newlines();

        if self.matches(&[TokenType::CloseBrace]) {
            return Ok(BlockStatementFact {
                range: self
                    .source_range_for_tokens(block_start, self.current)
                    .expect("empty block has source range"),
                span: start_span.combine(&self.previous().span),
                statements: Vec::new(),
            });
        }

        let statements = self.parse_block_statements_loop()?;

        let end_span = if self.check(TokenType::CloseBrace) {
            self.consume_token(TokenType::CloseBrace, "Expected '}' after block")?
        } else {
            return Err(ParserError::new(
                DiagnosticCode::ParseExpectedToken,
                "Expected '}' after block".to_string(),
                self.peek().span,
            ));
        };
        let stmts: Vec<StatementNode> = statements
            .into_iter()
            .filter_map(AstNode::into_statement)
            .collect();

        Ok(BlockStatementFact {
            range: self
                .source_range_for_tokens(block_start, self.current)
                .expect("block has source range"),
            span: start_span.combine(&end_span),
            statements: stmts,
        })
    }

    pub(super) fn parse_block_statements_loop(&mut self) -> ParserResult<Vec<AstNode>> {
        let mut statements = Vec::new();
        while !self.check(TokenType::CloseBrace) && !self.is_at_end() {
            self.skip_newlines();
            if self.check(TokenType::CloseBrace) {
                break;
            }
            self.parse_block_statement(&mut statements)?;
        }
        Ok(statements)
    }

    pub(super) fn parse_block_statement(
        &mut self,
        statements: &mut Vec<AstNode>,
    ) -> ParserResult<()> {
        let start_position = self.current;
        match self.declaration() {
            Ok(Some(decl)) => {
                statements.push(decl);
                self.skip_newlines();
            }
            Ok(None) => {
                // Syntax-only declarations can consume a full construct without
                // producing a compatibility AST node. Advance only when parsing
                // made no progress, so recovered input cannot stall the block loop.
                if self.current == start_position
                    && !self.check(TokenType::CloseBrace)
                    && !self.is_at_end()
                {
                    self.advance();
                }
            }
            Err(e) => {
                self.record_error(e);
                self.synchronize();
                let current_pos = self.current;
                if self.current == current_pos && !self.is_at_end() {
                    self.advance();
                }
            }
        }
        Ok(())
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

    pub(super) fn expression_statement(&mut self) -> ParserResult<LeafStatement> {
        let (range, expr) = self.parse_expression_statement()?;
        let expr = expr.expect("compatibility expression statement value");
        let span = *expr.span();
        Ok(LeafStatement {
            range,
            kind: LeafStatementKind::Expression(expr),
            span,
        })
    }

    fn syntax_only_expression_statement(&mut self) -> ParserResult<()> {
        self.parse_expression_statement().map(drop)
    }

    fn parse_expression_statement(&mut self) -> ParserResult<(ByteRange, Option<ExpressionNode>)> {
        let start = self.current;
        let expr = self.parse_expression_parsed()?;
        self.record_typed_syntax_node(
            SyntaxKind::Statement,
            start,
            self.current,
            SyntaxData::ExpressionStatement {
                expression: expr
                    .span
                    .byte_range
                    .expect("parsed expression has source range"),
            },
        );
        let range = self
            .source_range_for_tokens(start, self.current)
            .expect("expression statement has source range");
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

        let expr = expr.node;
        Ok((range, expr))
    }

    pub(super) fn validate_postfix_in_statement(
        &self,
        expr: &ParsedExpression,
    ) -> ParserResult<()> {
        if self.mode == ParserMode::SyntaxOnly {
            let range = expr.span.byte_range;
            let has_top_level_update = self.syntax_events.iter().any(|event| {
                Some(event.range) == range
                    && matches!(
                        event.data.as_ref(),
                        Some(SyntaxData::Unary { postfix: true, .. })
                    )
            });
            if has_top_level_update {
                return Ok(());
            }
            return self.check_no_postfix_increment_decrement_range(range);
        }

        let expr = expr
            .node
            .as_ref()
            .expect("compatibility expression statement");
        // If this is a postfix ++ or -- at the top level, it's valid
        if let ExpressionKind::Unary { op, postfix, .. } = &expr.kind
            && *postfix
            && matches!(op, UnaryOp::Incr | UnaryOp::Decr)
        {
            return Ok(());
        }
        // Otherwise, check that no nested postfix ++ or -- exist
        self.check_no_postfix_increment_decrement(expr)
    }

    pub(super) fn check_no_postfix_increment_decrement_parsed(
        &self,
        expr: &ParsedExpression,
    ) -> ParserResult<()> {
        if self.mode == ParserMode::SyntaxOnly {
            self.check_no_postfix_increment_decrement_range(expr.span.byte_range)
        } else {
            self.check_no_postfix_increment_decrement(
                expr.node.as_ref().expect("compatibility parsed expression"),
            )
        }
    }

    #[allow(clippy::only_used_in_recursion)]
    pub(super) fn check_no_postfix_increment_decrement(
        &self,
        expr: &ExpressionNode,
    ) -> ParserResult<()> {
        if self.mode == ParserMode::SyntaxOnly {
            return self.check_no_postfix_increment_decrement_range(expr.span.byte_range);
        }
        self.check_no_postfix_increment_decrement_kind(&expr.kind, expr.span)
    }

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

    pub(super) fn check_no_postfix_increment_decrement_kind(
        &self,
        kind: &ExpressionKind,
        span: Span,
    ) -> ParserResult<()> {
        match kind {
            ExpressionKind::Unary {
                op,
                expr: inner,
                postfix,
                ..
            } => self.check_unary_postfix_nesting(op, *postfix, inner, span),
            ExpressionKind::Binary { left, right, .. } => {
                self.check_no_postfix_increment_decrement(left)?;
                self.check_no_postfix_increment_decrement(right)
            }
            ExpressionKind::Call { func, args } => {
                self.check_no_postfix_increment_decrement(func)?;
                self.check_no_postfix_increment_decrement_all(args)
            }
            ExpressionKind::FieldAccess { expr: inner, .. } => {
                self.check_no_postfix_increment_decrement(inner)
            }
            ExpressionKind::ListAccess { expr: inner, index } => {
                self.check_no_postfix_increment_decrement(inner)?;
                self.check_no_postfix_increment_decrement(index)
            }
            ExpressionKind::ListLiteral(elems) | ExpressionKind::SetLiteral(elems) => {
                self.check_no_postfix_increment_decrement_all(elems)
            }
            ExpressionKind::MapLiteral { entries, .. } => {
                self.check_no_postfix_increment_decrement_map_entries(entries)
            }
            ExpressionKind::If {
                cond,
                then_expr,
                else_expr,
            } => {
                self.check_no_postfix_increment_decrement(cond)?;
                self.check_no_postfix_increment_decrement(then_expr)?;
                self.check_no_postfix_increment_decrement(else_expr)
            }
            ExpressionKind::Lambda { body, .. } => {
                self.check_no_postfix_increment_decrement_lambda_body(body)
            }
            _ => Ok(()),
        }
    }

    pub(super) fn check_unary_postfix_nesting(
        &self,
        op: &UnaryOp,
        postfix: bool,
        inner: &ExpressionNode,
        span: Span,
    ) -> ParserResult<()> {
        if postfix && matches!(op, UnaryOp::Incr | UnaryOp::Decr) {
            return Err(ParserError::with_help(
                DiagnosticCode::ParseExpectedToken,
                "Increment/Decrement operator can only be used as a standalone statement",
                span,
                "Expressions like 'x + y++' are not supported. Use 'y++' as a separate statement before the expression.",
            ));
        }

        self.check_no_postfix_increment_decrement(inner)
    }

    pub(super) fn check_no_postfix_increment_decrement_all(
        &self,
        expressions: &[ExpressionNode],
    ) -> ParserResult<()> {
        for expression in expressions {
            self.check_no_postfix_increment_decrement(expression)?;
        }
        Ok(())
    }

    pub(super) fn check_no_postfix_increment_decrement_map_entries(
        &self,
        entries: &[(ExpressionNode, ExpressionNode)],
    ) -> ParserResult<()> {
        for (key, value) in entries {
            self.check_no_postfix_increment_decrement(key)?;
            self.check_no_postfix_increment_decrement(value)?;
        }
        Ok(())
    }

    pub(super) fn check_no_postfix_increment_decrement_lambda_body(
        &self,
        body: &[StatementNode],
    ) -> ParserResult<()> {
        for stmt in body {
            if let StatementKind::Expression(expr) = &stmt.kind {
                // A bare `x++` / `x--` is a valid standalone statement, in a lambda
                // body just as in any other function body. Only reject a postfix
                // `++`/`--` that is *nested* inside a larger expression, so check
                // the operand rather than the statement expression itself when the
                // statement is exactly a postfix increment/decrement.
                if let ExpressionKind::Unary {
                    op,
                    expr: inner,
                    postfix: true,
                    ..
                } = &expr.kind
                    && matches!(op, UnaryOp::Incr | UnaryOp::Decr)
                {
                    self.check_no_postfix_increment_decrement(inner)?;
                } else {
                    self.check_no_postfix_increment_decrement(expr)?;
                }
            }
        }
        Ok(())
    }
}
