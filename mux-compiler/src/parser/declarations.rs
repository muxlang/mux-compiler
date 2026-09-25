use super::*;

/// Source ranges recorded by the grammar for a variable declaration. The
/// expressions and type are retained only to materialize the legacy AST at
/// parser call sites; syntax consumers use the recorded ranges.
pub(super) struct VariableDeclarationFact {
    range: ByteRange,
    span: Span,
    kind: VariableDeclarationKind,
    name_range: ByteRange,
    name_span: Span,
    name: String,
    type_range: Option<ByteRange>,
    type_node: Option<TypeNode>,
    value: Option<ExpressionNode>,
}

impl VariableDeclarationFact {
    pub(super) fn into_compatibility_ast(self) -> AstNode {
        let VariableDeclarationFact {
            range,
            span,
            kind,
            name_range,
            name_span,
            name,
            type_range,
            type_node,
            value,
        } = self;
        debug_assert_eq!(name_span.byte_range, Some(name_range));
        debug_assert_eq!(
            type_range,
            type_node.as_ref().and_then(|ty| ty.span.byte_range)
        );
        debug_assert!(span.byte_range.is_some_and(|span_range| {
            range.start <= span_range.start && span_range.end <= range.end
        }));
        let statement = match kind {
            VariableDeclarationKind::Auto => StatementKind::AutoDecl(
                name,
                TypeNode {
                    kind: TypeKind::Auto,
                    span: name_span,
                },
                value.expect("auto declaration has initializer"),
            ),
            VariableDeclarationKind::Const => StatementKind::ConstDecl(
                name,
                type_node.expect("constant declaration has type"),
                value.expect("constant declaration has initializer"),
            ),
            VariableDeclarationKind::Typed => StatementKind::TypedDecl(
                name,
                type_node.expect("typed declaration has type"),
                value.expect("typed declaration has initializer"),
            ),
            VariableDeclarationKind::Uninitialized => StatementKind::UninitDecl(
                name,
                type_node.expect("uninitialized declaration has type"),
            ),
        };
        AstNode::Statement(StatementNode {
            kind: statement,
            span,
        })
    }
}

impl<'a> Parser<'a> {
    pub(super) fn declaration(&mut self) -> ParserResult<Option<AstNode>> {
        loop {
            let _ = self.skip_newlines();
            if self.is_at_end() {
                return Ok(None);
            }

            let start_position = self.current;
            let node_kind = self
                .tokens
                .get(start_position)
                .map(|token| self.classify_declaration(&token.token_type))
                .unwrap_or(SyntaxKind::Declaration);
            let result = self.parse_declaration_content();
            let end_position = self.current;

            while self.matches(&[TokenType::NewLine]) {}

            if let Err(ref e) = result {
                self.record_syntax_node(SyntaxKind::Error, start_position, end_position);
                self.record_error(e.clone());
                // Retry the next declaration to recover, but stop at a closing brace:
                // that terminates the enclosing block or class body, and retrying
                // there would parse '}' as a stray declaration and cascade a spurious
                // "Expected expression, found '}'" plus a lost terminator (issue #288,
                // in method bodies). Returning None lets the block/class loop see the
                // '}' and finish cleanly.
                if self.stopped || self.is_at_end() || self.check(TokenType::CloseBrace) {
                    return Ok(None);
                }

                // Recovery must make progress before trying another declaration. Most
                // parse errors consume input themselves; advance the offending token
                // here when one does not, preventing unbounded stack growth and retry
                // loops on tokens that cannot begin a declaration.
                if self.current == start_position {
                    self.advance();
                }
                continue;
            }

            self.record_syntax_node(node_kind, start_position, end_position);
            return result;
        }
    }

    pub(super) fn parse_declaration_content(&mut self) -> ParserResult<Option<AstNode>> {
        if self.check(TokenType::Auto) {
            self.auto_declaration().map(Some)
        } else if self.check(TokenType::Const) {
            self.const_declaration().map(Some)
        } else if self.check(TokenType::Common) {
            self.consume();
            self.function_declaration(true).map(Some)
        } else if self.check(TokenType::Func) {
            self.function_declaration(false).map(Some)
        } else if let TokenType::Id(_) = &self.peek().token_type {
            self.parse_id_start_declaration()
        } else if self.check(TokenType::Class) {
            self.class_declaration().map(Some)
        } else if self.check(TokenType::Interface) {
            self.interface_declaration().map(Some)
        } else if self.check(TokenType::Enum) {
            self.enum_declaration().map(Some)
        } else if self.check(TokenType::Test) {
            self.test_declaration().map(Some)
        } else if self.check(TokenType::Import) {
            self.import_declaration().map(Some)
        } else {
            self.statement().map(Some)
        }
    }

    pub(super) fn parse_id_start_declaration(&mut self) -> ParserResult<Option<AstNode>> {
        let checkpoint = self.checkpoint();
        if self.parse_type().is_ok() {
            self.parse_typed_or_statement(checkpoint)
        } else {
            self.rewind(checkpoint);
            self.statement().map(Some)
        }
    }

    pub(super) fn parse_typed_or_statement(
        &mut self,
        checkpoint: ParserCheckpoint,
    ) -> ParserResult<Option<AstNode>> {
        if let TokenType::Id(_) = &self.peek().token_type {
            let next = self.current + 1;
            if next < self.tokens.len() && self.tokens[next].token_type == TokenType::Eq {
                self.rewind(checkpoint);
                self.parse_typed_declaration_with_recovery()
            } else {
                self.rewind(checkpoint);
                self.statement().map(Some)
            }
        } else {
            self.rewind(checkpoint);
            self.statement().map(Some)
        }
    }

    pub(super) fn parse_typed_declaration_with_recovery(
        &mut self,
    ) -> ParserResult<Option<AstNode>> {
        match self.typed_declaration() {
            Ok(node) => Ok(Some(node)),
            Err(e)
                if matches!(
                    e.message.as_str(),
                    "must be terminated with a newline" | "expected newline after statement"
                ) =>
            {
                self.record_error(e);
                self.synchronize();
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    pub(super) fn auto_declaration(&mut self) -> ParserResult<AstNode> {
        self.auto_declaration_fact()
            .map(VariableDeclarationFact::into_compatibility_ast)
    }

    fn auto_declaration_fact(&mut self) -> ParserResult<VariableDeclarationFact> {
        let start = self.current;
        let start_span = self.peek().span;
        self.advance();

        let name = self.consume_identifier("Expected variable name after 'auto'")?;
        let name_span = self.tokens[self.current - 1].span;

        self.consume_token(TokenType::Eq, "Expected '=' after variable name")?;
        let value = self.parse_expression()?;

        // Validate that postfix ++ and -- don't appear in declarations
        self.check_no_postfix_increment_decrement(&value)?;

        self.record_typed_syntax_node(
            SyntaxKind::Statement,
            start,
            self.current,
            SyntaxData::VariableDeclaration {
                kind: VariableDeclarationKind::Auto,
                name: name_span
                    .byte_range
                    .expect("auto declaration name has source range"),
                type_range: None,
                value: Some(
                    value
                        .span
                        .byte_range
                        .expect("auto initializer has source range"),
                ),
            },
        );

        if !self.is_in_block() {
            if self.current < self.tokens.len() {
                let next_token = &self.tokens[self.current];
                if self.is_statement_starter(&next_token.token_type)
                    && !matches!(self.tokens[self.current - 1].token_type, TokenType::NewLine)
                {
                    return Err(ParserError::new(
                        DiagnosticCode::ParseExpectedToken,
                        "Expected newline after statement".to_string(),
                        next_token.span,
                    ));
                }
            }
        } else {
            let _ = self.skip_newlines();
        }

        let span = start_span.combine(&value.span);
        Ok(VariableDeclarationFact {
            range: self
                .source_range_for_tokens(start, self.current)
                .expect("auto declaration source range"),
            span,
            kind: VariableDeclarationKind::Auto,
            name_range: name_span
                .byte_range
                .expect("auto declaration name source range"),
            name_span,
            name,
            type_range: None,
            type_node: None,
            value: Some(value),
        })
    }

    pub(super) fn const_declaration(&mut self) -> ParserResult<AstNode> {
        self.const_declaration_fact()
            .map(VariableDeclarationFact::into_compatibility_ast)
    }

    fn const_declaration_fact(&mut self) -> ParserResult<VariableDeclarationFact> {
        let start = self.current;
        let start_span = self.peek().span;
        self.advance();

        let type_node = self.parse_type()?;
        let name = self.consume_identifier("Expected constant name after type")?;
        let name_span = self.previous().span;

        self.consume_token(TokenType::Eq, "Expected '=' after constant name")?;
        let value = self.parse_expression()?;

        // Validate that postfix ++ and -- don't appear in declarations
        self.check_no_postfix_increment_decrement(&value)?;

        self.record_typed_syntax_node(
            SyntaxKind::Statement,
            start,
            self.current,
            SyntaxData::VariableDeclaration {
                kind: VariableDeclarationKind::Const,
                name: name_span
                    .byte_range
                    .expect("constant name has source range"),
                type_range: Some(
                    type_node
                        .span
                        .byte_range
                        .expect("constant type has source range"),
                ),
                value: Some(
                    value
                        .span
                        .byte_range
                        .expect("constant initializer has source range"),
                ),
            },
        );

        let span = start_span.combine(&value.span);
        Ok(VariableDeclarationFact {
            range: self
                .source_range_for_tokens(start, self.current)
                .expect("constant declaration source range"),
            span,
            kind: VariableDeclarationKind::Const,
            name_range: name_span
                .byte_range
                .expect("constant name has source range"),
            name_span,
            name,
            type_range: Some(
                type_node
                    .span
                    .byte_range
                    .expect("constant type has source range"),
            ),
            type_node: Some(type_node),
            value: Some(value),
        })
    }

    pub(super) fn typed_declaration(&mut self) -> ParserResult<AstNode> {
        self.typed_declaration_fact()
            .map(VariableDeclarationFact::into_compatibility_ast)
    }

    fn typed_declaration_fact(&mut self) -> ParserResult<VariableDeclarationFact> {
        let start = self.current;
        let start_span = self.peek().span;
        let type_node = self.parse_type()?;
        let name = self.consume_identifier("Expected variable name after type")?;
        let name_span = self.previous().span;

        // `Type name` with no initializer. Deciding here, after the name, is
        // what lets an initialized and an uninitialized declaration share one
        // entry point (#393).
        if !self.check(TokenType::Eq) {
            self.record_typed_syntax_node(
                SyntaxKind::Statement,
                start,
                self.current,
                SyntaxData::VariableDeclaration {
                    kind: VariableDeclarationKind::Uninitialized,
                    name: name_span
                        .byte_range
                        .expect("uninitialized declaration name has source range"),
                    type_range: Some(
                        type_node
                            .span
                            .byte_range
                            .expect("uninitialized declaration type has source range"),
                    ),
                    value: None,
                },
            );
            let span = start_span.combine(&name_span);
            return Ok(VariableDeclarationFact {
                range: self
                    .source_range_for_tokens(start, self.current)
                    .expect("uninitialized declaration source range"),
                span,
                kind: VariableDeclarationKind::Uninitialized,
                name_range: name_span
                    .byte_range
                    .expect("uninitialized declaration name has source range"),
                name_span,
                name,
                type_range: Some(
                    type_node
                        .span
                        .byte_range
                        .expect("uninitialized declaration type has source range"),
                ),
                type_node: Some(type_node),
                value: None,
            });
        }

        self.consume_token(TokenType::Eq, "Expected '=' after variable name")?;
        let value = self.parse_expression()?;

        // Validate that postfix ++ and -- don't appear in declarations
        self.check_no_postfix_increment_decrement(&value)?;

        self.record_typed_syntax_node(
            SyntaxKind::Statement,
            start,
            self.current,
            SyntaxData::VariableDeclaration {
                kind: VariableDeclarationKind::Typed,
                name: name_span
                    .byte_range
                    .expect("typed declaration name has source range"),
                type_range: Some(
                    type_node
                        .span
                        .byte_range
                        .expect("typed declaration type has source range"),
                ),
                value: Some(
                    value
                        .span
                        .byte_range
                        .expect("typed initializer has source range"),
                ),
            },
        );

        let span = start_span.combine(&value.span);
        Ok(VariableDeclarationFact {
            range: self
                .source_range_for_tokens(start, self.current)
                .expect("typed declaration source range"),
            span,
            kind: VariableDeclarationKind::Typed,
            name_range: name_span
                .byte_range
                .expect("typed declaration name has source range"),
            name_span,
            name,
            type_range: Some(
                type_node
                    .span
                    .byte_range
                    .expect("typed declaration type has source range"),
            ),
            type_node: Some(type_node),
            value: Some(value),
        })
    }

    pub(super) fn class_declaration(&mut self) -> ParserResult<AstNode> {
        let class_start = self.current;
        let start_span = self.tokens[self.current].span;
        self.consume_token(TokenType::Class, "Expected 'class' keyword")?;
        let name = self.consume_identifier("Expected class name")?;
        let name_range = self
            .previous()
            .span
            .byte_range
            .expect("class name source range");
        let type_parameters_start = self.syntax_events.len();
        let type_params = self.parse_type_params_list()?;
        let type_parameters = self.syntax_ranges_since(type_parameters_start, |data| {
            matches!(data, SyntaxData::TypeParameter { .. })
        });
        let traits_start = self.syntax_events.len();
        let traits = self.parse_trait_list()?;
        let trait_ranges = self.syntax_ranges_since(traits_start, |data| {
            matches!(data, SyntaxData::TraitReference { .. })
        });
        let body_start = self.current;
        self.consume_token(TokenType::OpenBrace, "Expected '{' after class header")?;
        let members_start = self.syntax_events.len();
        let (fields, methods) = self.parse_class_body(&type_params, start_span)?;
        let field_ranges = self.syntax_ranges_since(members_start, |data| {
            matches!(data, SyntaxData::Field { .. })
        });
        let method_ranges = self.syntax_ranges_since(members_start, |data| {
            matches!(data, SyntaxData::ClassMethod { .. })
        });
        let end_span =
            self.consume_token(TokenType::CloseBrace, "Expected '}' after class body")?;
        self.record_syntax_node(SyntaxKind::ClassBody, body_start, self.current);
        let where_start = self.syntax_events.len();
        let where_clause = self.parse_where_clause()?;
        let where_range = self.last_syntax_range_since(where_start, |data| {
            matches!(data, SyntaxData::WhereClause { .. })
        });
        let full_span = match &where_clause {
            Some(clause) => start_span.combine(&clause.span),
            None => start_span.combine(&end_span),
        };
        self.record_typed_syntax_node(
            SyntaxKind::ClassDeclaration,
            class_start,
            self.current,
            SyntaxData::Class {
                name: name_range,
                type_parameters,
                traits: trait_ranges,
                fields: field_ranges,
                methods: method_ranges,
                where_clause: where_range,
                ast_span: full_span.byte_range.expect("class span source range"),
            },
        );
        Ok(AstNode::Class {
            name,
            type_params,
            traits,
            fields,
            methods,
            where_clause,
            span: full_span,
        })
    }

    /// Parse a named top-level test block:
    ///
    /// ```text
    /// test "addition" {
    ///     // ordinary Mux statements
    /// }
    /// ```
    ///
    /// The body deliberately reuses the ordinary block parser. This keeps test
    /// code subject to exactly the same syntax rules as application code while
    /// allowing semantic/code-generation passes to omit it from normal builds.
    pub(super) fn test_declaration(&mut self) -> ParserResult<AstNode> {
        let test_start = self.current;
        if self.is_in_block() {
            return Err(ParserError::with_help(
                DiagnosticCode::ParseExpectedToken,
                "Test blocks must be declared at the top level".to_string(),
                self.peek().span,
                "Move this test block outside of functions, classes, and other blocks.",
            ));
        }
        let start_span = self.consume_token(TokenType::Test, "Expected 'test' keyword")?;
        let (name, name_span, name_range) = {
            let name_token = self.consume();
            let name = match &name_token.token_type {
                TokenType::Str(name) => name.clone(),
                _ => {
                    return Err(ParserError::with_help(
                        DiagnosticCode::ParseExpectedToken,
                        "Expected a quoted test name after 'test'".to_string(),
                        name_token.span,
                        "Name tests with a string, for example: test \"adds numbers\" { ... }",
                    ));
                }
            };
            (
                name,
                name_token.span,
                name_token
                    .span
                    .byte_range
                    .expect("test name token source range"),
            )
        };
        if name.is_empty() {
            return Err(ParserError::new(
                DiagnosticCode::ParseExpectedToken,
                "Test name must not be empty".to_string(),
                name_span,
            ));
        }
        self.skip_newlines();
        let block = self.block()?;
        let AstNode::Statement(StatementNode {
            kind: StatementKind::Block(body),
            span: block_span,
        }) = block
        else {
            return Err(ParserError::new(
                DiagnosticCode::ParseExpectedToken,
                "Expected a block after test name".to_string(),
                name_span,
            ));
        };
        let span = start_span.combine(&block_span);
        self.record_typed_syntax_node(
            SyntaxKind::TestDeclaration,
            test_start,
            self.current,
            SyntaxData::Test {
                name: name_range,
                body: block_span.byte_range.expect("test block source range"),
                ast_span: span.byte_range.expect("test declaration source range"),
            },
        );
        Ok(AstNode::Test { name, body, span })
    }

    pub(super) fn parse_type_params_list(
        &mut self,
    ) -> ParserResult<Vec<(String, Vec<TraitBound>)>> {
        let start = self.current;
        if !self.matches(&[TokenType::Lt]) {
            return Ok(Vec::new());
        }
        let mut params = Vec::new();
        loop {
            let parameter_start = self.current;
            let param = self.consume_identifier("Expected type parameter name")?;
            let name = self
                .previous()
                .span
                .byte_range
                .expect("class type parameter name source range");
            let bounds_start = self.syntax_events.len();
            let bounds = self.parse_trait_bounds()?;
            let bound_ranges = self.syntax_ranges_since(bounds_start, |data| {
                matches!(data, SyntaxData::TraitBound { .. })
            });
            self.record_typed_syntax_node(
                SyntaxKind::TypeParameter,
                parameter_start,
                self.current,
                SyntaxData::TypeParameter {
                    name,
                    bounds: bound_ranges,
                },
            );
            params.push((param, bounds));
            if !self.matches(&[TokenType::Comma]) {
                break;
            }
        }
        self.consume_token(TokenType::Gt, "Expected '>' after type parameters")?;
        self.record_syntax_node(SyntaxKind::TypeArguments, start, self.current);
        Ok(params)
    }

    pub(super) fn parse_trait_bounds(&mut self) -> ParserResult<Vec<TraitBound>> {
        if !self.matches(&[TokenType::Is]) {
            return Ok(Vec::new());
        }
        let mut bounds = Vec::new();
        loop {
            let bound_start = self.current;
            let bound_name = self.consume_identifier("Expected trait name in bound")?;
            let bound_span = self.previous().span;
            let type_args = self.parse_optional_type_args()?;
            self.record_typed_syntax_node(
                SyntaxKind::TraitBound,
                bound_start,
                self.current,
                SyntaxData::TraitBound {
                    name: bound_span
                        .byte_range
                        .expect("trait bound name has source range"),
                    type_arguments: type_args
                        .iter()
                        .map(|argument| {
                            argument
                                .span
                                .byte_range
                                .expect("trait bound type argument has source range")
                        })
                        .collect(),
                },
            );
            bounds.push(TraitBound {
                name: bound_name,
                type_params: type_args,
                span: bound_span,
            });
            if !self.matches(&[TokenType::Ref]) {
                break;
            }
        }
        Ok(bounds)
    }

    pub(super) fn parse_trait_list(&mut self) -> ParserResult<Vec<TraitRef>> {
        if !self.matches(&[TokenType::Is]) {
            return Ok(Vec::new());
        }
        let mut traits_list = Vec::new();
        loop {
            let trait_start = self.current;
            let trait_name = self.consume_identifier("Expected trait name")?;
            let trait_span = self.previous().span;
            let name = trait_span
                .byte_range
                .expect("trait reference name source range");
            let type_arguments_start = self.syntax_events.len();
            let type_args = self.parse_optional_type_args()?;
            let type_arguments = self.syntax_ranges_since(type_arguments_start, |data| {
                matches!(
                    data,
                    SyntaxData::TypeName { .. }
                        | SyntaxData::TypeReference { .. }
                        | SyntaxData::TypeContainer { .. }
                        | SyntaxData::FunctionType { .. }
                )
            });
            self.record_typed_syntax_node(
                SyntaxKind::TraitReference,
                trait_start,
                self.current,
                SyntaxData::TraitReference {
                    name,
                    type_arguments,
                },
            );
            traits_list.push(TraitRef {
                name: trait_name,
                type_args,
                span: trait_span,
            });
            if !self.matches(&[TokenType::Comma]) {
                break;
            }
        }
        Ok(traits_list)
    }

    pub(super) fn parse_optional_type_args(&mut self) -> ParserResult<Vec<TypeNode>> {
        let start = self.current;
        if self.matches(&[TokenType::Lt]) {
            let args = self.parse_type_arguments()?;
            self.consume_token(TokenType::Gt, "Expected '>' after type arguments")?;
            self.record_syntax_node(SyntaxKind::TypeArguments, start, self.current);
            Ok(args)
        } else {
            Ok(Vec::new())
        }
    }

    pub(super) fn parse_class_body(
        &mut self,
        type_params: &[(String, Vec<TraitBound>)],
        start_span: Span,
    ) -> ParserResult<(Vec<Field>, Vec<FunctionNode>)> {
        let mut fields = Vec::new();
        let mut methods = Vec::new();
        while !self.check(TokenType::CloseBrace) && !self.is_at_end() {
            // Recover within the class body so a malformed member does not abort
            // the whole class and leave '}' to be misparsed as a stray top-level
            // token (issue #288). Skipping from the member start treats the member
            // as a balanced unit, so inner braces - a collection default or a
            // method body - are consumed with their matching open and never
            // mistaken for the class terminator.
            let checkpoint = self.checkpoint();
            if let Err(e) =
                self.parse_class_member(type_params, start_span, &mut fields, &mut methods)
            {
                self.rewind(checkpoint);
                self.record_error(e);
                self.recover_class_member();
                if self.current == checkpoint.current {
                    self.advance();
                }
            }
        }
        Ok((fields, methods))
    }

    /// Skip a malformed class member as a balanced unit, starting from the token
    /// the member began at. Matched brace pairs (a collection default, a method
    /// body) are consumed together; recovery ends at the member's terminating
    /// newline, or at a closing brace that balances no open seen here - the
    /// class's own terminator - which is left for the caller.
    pub(super) fn recover_class_member(&mut self) {
        let mut depth: i32 = 0;
        while !self.is_at_end() {
            match self.peek().token_type {
                TokenType::OpenBrace => {
                    depth += 1;
                    self.advance();
                }
                TokenType::CloseBrace => {
                    if depth == 0 {
                        return;
                    }
                    depth -= 1;
                    self.advance();
                }
                TokenType::NewLine if depth == 0 => {
                    self.advance();
                    return;
                }
                _ => {
                    self.advance();
                }
            }
        }
    }

    pub(super) fn parse_class_member(
        &mut self,
        type_params: &[(String, Vec<TraitBound>)],
        start_span: Span,
        fields: &mut Vec<Field>,
        methods: &mut Vec<FunctionNode>,
    ) -> ParserResult<()> {
        match self.peek().token_type {
            TokenType::Func => {
                let member_start = self.current;
                let function_event_start = self.syntax_events.len();
                let name_span = self.peek_ahead(1).map(|t| t.span);
                let func_node = self.function_declaration(false)?;
                if let AstNode::Function(func) = func_node {
                    if let Some(message) = reserved_class_method_error(&func.name) {
                        self.record_error(ParserError::new(
                            DiagnosticCode::ParseExpectedToken,
                            &message,
                            name_span.unwrap_or(func.span),
                        ));
                        return Ok(());
                    }
                    let function_range = self
                        .last_syntax_range_since(function_event_start, |data| {
                            matches!(data, SyntaxData::Function { .. })
                        })
                        .expect("parsed class method has a function context");
                    methods.push(func);
                    self.record_typed_syntax_node(
                        SyntaxKind::ClassMethod,
                        member_start,
                        self.current,
                        SyntaxData::ClassMethod {
                            function: function_range,
                        },
                    );
                } else {
                    return Err(ParserError::new(
                        DiagnosticCode::ParseExpectedToken,
                        "Expected function in class",
                        start_span,
                    ));
                }
            }
            TokenType::Common => {
                let member_start = self.current;
                self.consume();
                let function_event_start = self.syntax_events.len();
                let name_span = self.peek_ahead(1).map(|t| t.span);
                let func_node = self.function_declaration(true)?;
                if let AstNode::Function(func) = func_node {
                    if let Some(message) = reserved_class_method_error(&func.name) {
                        self.record_error(ParserError::new(
                            DiagnosticCode::ParseExpectedToken,
                            &message,
                            name_span.unwrap_or(func.span),
                        ));
                        return Ok(());
                    }
                    let function_range = self
                        .last_syntax_range_since(function_event_start, |data| {
                            matches!(data, SyntaxData::Function { .. })
                        })
                        .expect("parsed common class method has a function context");
                    methods.push(func);
                    self.record_typed_syntax_node(
                        SyntaxKind::ClassMethod,
                        member_start,
                        self.current,
                        SyntaxData::ClassMethod {
                            function: function_range,
                        },
                    );
                } else {
                    return Err(ParserError::new(
                        DiagnosticCode::ParseExpectedToken,
                        "Expected function in class",
                        start_span,
                    ));
                }
            }
            TokenType::Id(_) | TokenType::Const => {
                let field = self.parse_field_declaration(type_params)?;
                fields.push(field);
            }
            TokenType::NewLine => {
                self.consume_token(TokenType::NewLine, "Expected newline")?;
            }
            _ => {
                let token_desc = Self::describe_token(&self.peek().token_type);
                return Err(ParserError::with_help(
                    DiagnosticCode::ParseExpectedToken,
                    format!(
                        "Expected field or method declaration in class body, found {token_desc}"
                    ),
                    self.peek().span,
                    "Class bodies can only contain field declarations (e.g., 'int x = 0') and method declarations (e.g., 'func foo() returns void { ... }')",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn interface_declaration(&mut self) -> ParserResult<AstNode> {
        let interface_start = self.current;
        let start_span = self.tokens[self.current].span;
        self.consume_token(TokenType::Interface, "Expected 'interface' keyword")?;
        let name = self.consume_identifier("Expected interface name")?;
        let name_range = self
            .previous()
            .span
            .byte_range
            .expect("interface name source range");
        let type_parameters_start = self.syntax_events.len();
        let type_params = self.parse_interface_type_params()?;
        let type_parameters = self.syntax_ranges_since(type_parameters_start, |data| {
            matches!(data, SyntaxData::TypeParameter { .. })
        });
        let body_start = self.current;
        self.consume_token(TokenType::OpenBrace, "Expected '{' after interface header")?;
        let members_start = self.syntax_events.len();
        let (fields, methods) = self.parse_interface_body(&type_params, start_span)?;
        let field_ranges = self.syntax_ranges_since(members_start, |data| {
            matches!(data, SyntaxData::Field { .. })
        });
        let method_ranges = self.syntax_ranges_since(members_start, |data| {
            matches!(data, SyntaxData::Function { .. })
        });
        let end_span =
            self.consume_token(TokenType::CloseBrace, "Expected '}' after interface body")?;
        self.record_syntax_node(SyntaxKind::InterfaceBody, body_start, self.current);
        let full_span = start_span.combine(&end_span);
        self.record_typed_syntax_node(
            SyntaxKind::InterfaceDeclaration,
            interface_start,
            self.current,
            SyntaxData::Interface {
                name: name_range,
                type_parameters,
                fields: field_ranges,
                methods: method_ranges,
                ast_span: full_span.byte_range.expect("interface span source range"),
            },
        );
        Ok(AstNode::Interface {
            name,
            type_params,
            fields,
            methods,
            span: full_span,
        })
    }

    pub(super) fn parse_interface_type_params(
        &mut self,
    ) -> ParserResult<Vec<(String, Vec<TraitBound>)>> {
        let start = self.current;
        if !self.matches(&[TokenType::Lt]) {
            return Ok(Vec::new());
        }
        let mut params = Vec::new();
        if !self.check(TokenType::Gt) {
            loop {
                let parameter_start = self.current;
                let param = self.consume_identifier("Expected type parameter name")?;
                let name = self
                    .previous()
                    .span
                    .byte_range
                    .expect("interface type parameter name range");
                let bounds_start = self.syntax_events.len();
                let bounds = self.parse_colon_trait_bounds()?;
                let bound_ranges = self.syntax_ranges_since(bounds_start, |data| {
                    matches!(data, SyntaxData::TraitBound { .. })
                });
                self.record_typed_syntax_node(
                    SyntaxKind::TypeParameter,
                    parameter_start,
                    self.current,
                    SyntaxData::TypeParameter {
                        name,
                        bounds: bound_ranges,
                    },
                );
                params.push((param, bounds));
                if !self.matches(&[TokenType::Comma]) {
                    break;
                }
            }
        }
        self.consume_token(TokenType::Gt, "Expected '>' after type parameters")?;
        self.record_syntax_node(SyntaxKind::TypeArguments, start, self.current);
        Ok(params)
    }

    pub(super) fn parse_colon_trait_bounds(&mut self) -> ParserResult<Vec<TraitBound>> {
        if !self.matches(&[TokenType::Colon]) {
            return Ok(Vec::new());
        }
        let mut bounds = Vec::new();
        loop {
            let bound_start = self.current;
            let bound_name = self.consume_identifier("Expected trait name in bound")?;
            let bound_span = self.previous().span;
            let name = bound_span
                .byte_range
                .expect("trait bound name source range");
            let type_arguments_start = self.syntax_events.len();
            let type_args = self.parse_optional_type_args()?;
            let type_arguments = self.syntax_ranges_since(type_arguments_start, |data| {
                matches!(
                    data,
                    SyntaxData::TypeName { .. }
                        | SyntaxData::TypeReference { .. }
                        | SyntaxData::TypeContainer { .. }
                        | SyntaxData::FunctionType { .. }
                )
            });
            self.record_typed_syntax_node(
                SyntaxKind::TraitBound,
                bound_start,
                self.current,
                SyntaxData::TraitBound {
                    name,
                    type_arguments,
                },
            );
            bounds.push(TraitBound {
                name: bound_name,
                type_params: type_args,
                span: bound_span,
            });
            if !self.matches(&[TokenType::Plus]) {
                break;
            }
        }
        Ok(bounds)
    }

    pub(super) fn parse_interface_body(
        &mut self,
        type_params: &[(String, Vec<TraitBound>)],
        start_span: Span,
    ) -> ParserResult<(Vec<Field>, Vec<FunctionNode>)> {
        let mut fields = Vec::new();
        let mut methods = Vec::new();
        while !self.is_at_end() {
            self.skip_newlines();
            if self.check(TokenType::CloseBrace) {
                break;
            }
            self.parse_interface_member(type_params, start_span, &mut fields, &mut methods)?;
        }
        Ok((fields, methods))
    }

    pub(super) fn parse_interface_member(
        &mut self,
        type_params: &[(String, Vec<TraitBound>)],
        start_span: Span,
        fields: &mut Vec<Field>,
        methods: &mut Vec<FunctionNode>,
    ) -> ParserResult<()> {
        match self.peek().token_type {
            TokenType::Func => {
                let method = self.parse_interface_method(start_span)?;
                methods.push(method);
            }
            TokenType::Id(_) | TokenType::Const => {
                let field = self.parse_field_declaration(type_params)?;
                fields.push(field);
            }
            TokenType::NewLine => {
                self.consume_token(TokenType::NewLine, "Expected newline")?;
            }
            _ => {
                return Err(ParserError::new(
                    DiagnosticCode::ParseExpectedToken,
                    "Expected field or function declaration in interface",
                    self.peek().span,
                ));
            }
        }
        Ok(())
    }

    pub(super) fn parse_interface_method(
        &mut self,
        start_span: Span,
    ) -> ParserResult<FunctionNode> {
        let function_start = self.current;
        self.consume();
        let name = self.consume_identifier("Expected method name")?;
        let name_range = self.previous().span.byte_range.expect("method name range");
        let type_params_start = self.syntax_events.len();
        let type_params = self.parse_simple_type_params()?;
        let type_parameters = self.syntax_ranges_since(type_params_start, |data| {
            matches!(data, SyntaxData::TypeParameter { .. })
        });
        self.consume_token(TokenType::OpenParen, "Expected '(' after method name")?;
        let params_start = self.current;
        let params_event_start = self.syntax_events.len();
        let params = self.parse_param_list()?;
        let parameters = self.syntax_ranges_since(params_event_start, |data| {
            matches!(data, SyntaxData::Parameter { .. })
        });
        self.consume_token(TokenType::CloseParen, "Expected ')' after parameters")?;
        self.record_syntax_node(SyntaxKind::ParameterList, params_start, self.current);
        let where_event_start = self.syntax_events.len();
        let where_clause = self.parse_where_clause()?;
        let where_range = self.last_syntax_range_since(where_event_start, |data| {
            matches!(data, SyntaxData::WhereClause { .. })
        });
        if where_clause.is_some() {
            // The return type may continue on the next line after the block,
            // but do not eat the newline separating this member from the next.
            self.skip_newlines_before(TokenType::Returns);
        }
        let return_event_start = self.syntax_events.len();
        let return_type = self.parse_optional_return_type()?;
        let return_type_range = self.last_syntax_range_since(return_event_start, |data| {
            matches!(
                data,
                SyntaxData::TypeName { .. }
                    | SyntaxData::TypeReference { .. }
                    | SyntaxData::TypeContainer { .. }
                    | SyntaxData::FunctionType { .. }
            )
        });
        let return_type_span = return_type
            .span
            .byte_range
            .expect("method return type span range");
        let end = self.current;
        self.record_typed_syntax_node(
            SyntaxKind::FunctionDeclaration,
            function_start,
            end,
            SyntaxData::Function {
                name: name_range,
                ast_span: start_span.byte_range.expect("interface method span range"),
                type_parameters,
                parameters,
                return_type: return_type_range,
                return_type_span,
                where_clause: where_range,
                body: None,
                is_common: false,
            },
        );
        Ok(FunctionNode {
            name,
            type_params,
            params,
            return_type,
            body: vec![],
            span: start_span,
            is_common: false,
            where_clause,
        })
    }

    pub(super) fn parse_simple_type_params(
        &mut self,
    ) -> ParserResult<Vec<(String, Vec<TraitBound>)>> {
        let start = self.current;
        if !self.matches(&[TokenType::Lt]) {
            return Ok(Vec::new());
        }
        let mut params = Vec::new();
        if !self.check(TokenType::Gt) {
            loop {
                let parameter_start = self.current;
                let param = self.consume_identifier("Expected type parameter name")?;
                let name = self
                    .previous()
                    .span
                    .byte_range
                    .expect("type parameter name range");
                self.record_typed_syntax_node(
                    SyntaxKind::TypeParameter,
                    parameter_start,
                    self.current,
                    SyntaxData::TypeParameter {
                        name,
                        bounds: Vec::new(),
                    },
                );
                params.push((param, Vec::new()));
                if !self.matches(&[TokenType::Comma]) {
                    break;
                }
            }
        }
        self.consume_token(TokenType::Gt, "Expected '>' after type parameters")?;
        self.record_syntax_node(SyntaxKind::TypeArguments, start, self.current);
        Ok(params)
    }

    pub(super) fn parse_param_list(&mut self) -> ParserResult<Vec<Param>> {
        let mut params = Vec::new();
        if !self.check(TokenType::CloseParen) {
            loop {
                let parameter_start = self.current;
                let param_type = self.parse_type()?;
                let type_range = param_type.span.byte_range.expect("parameter type range");
                let param_name = self.consume_identifier("Expected parameter name")?;
                let name = self
                    .previous()
                    .span
                    .byte_range
                    .expect("parameter name range");
                self.record_typed_syntax_node(
                    SyntaxKind::Parameter,
                    parameter_start,
                    self.current,
                    SyntaxData::Parameter {
                        name,
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
            }
        }
        Ok(params)
    }

    pub(super) fn parse_optional_return_type(&mut self) -> ParserResult<TypeNode> {
        if self.matches(&[TokenType::Minus, TokenType::Gt]) || self.matches(&[TokenType::Returns]) {
            self.parse_type()
        } else {
            Ok(TypeNode {
                kind: TypeKind::Primitive(PrimitiveType::Void),
                span: self.peek().span,
            })
        }
    }

    pub(super) fn enum_declaration(&mut self) -> ParserResult<AstNode> {
        let enum_start = self.current;
        let start_span = self.tokens[self.current].span;
        self.consume_token(TokenType::Enum, "Expected 'enum' keyword")?;
        let name = self.consume_identifier("Expected enum name")?;
        let name_range = self
            .previous()
            .span
            .byte_range
            .expect("enum name source range");
        let type_parameters_start = self.syntax_events.len();
        let type_params = self.parse_colon_type_params()?;
        let type_parameters = self.syntax_ranges_since(type_parameters_start, |data| {
            matches!(data, SyntaxData::TypeParameter { .. })
        });
        let body_start = self.current;
        self.consume_token(TokenType::OpenBrace, "Expected '{' after enum header")?;
        let variants_start = self.syntax_events.len();
        let variants = self.parse_enum_variants()?;
        let variant_ranges = self.syntax_ranges_since(variants_start, |data| {
            matches!(data, SyntaxData::EnumVariant { .. })
        });
        let end_span =
            self.consume_token(TokenType::CloseBrace, "Expected '}' after enum variants")?;
        self.record_syntax_node(SyntaxKind::EnumBody, body_start, self.current);
        let full_span = start_span.combine(&end_span);
        self.record_typed_syntax_node(
            SyntaxKind::EnumDeclaration,
            enum_start,
            self.current,
            SyntaxData::Enum {
                name: name_range,
                type_parameters,
                variants: variant_ranges,
                ast_span: full_span.byte_range.expect("enum span source range"),
            },
        );
        Ok(AstNode::Enum {
            name,
            type_params,
            variants,
            span: full_span,
        })
    }

    pub(super) fn parse_colon_type_params(
        &mut self,
    ) -> ParserResult<Vec<(String, Vec<TraitBound>)>> {
        let start = self.current;
        if !self.matches(&[TokenType::Lt]) {
            return Ok(Vec::new());
        }
        let mut params = Vec::new();
        if !self.check(TokenType::Gt) {
            loop {
                let parameter_start = self.current;
                let param = self.consume_identifier("Expected type parameter name")?;
                let name = self
                    .previous()
                    .span
                    .byte_range
                    .expect("enum type parameter name source range");
                let bounds_start = self.syntax_events.len();
                let bounds = self.parse_colon_trait_bounds()?;
                let bound_ranges = self.syntax_ranges_since(bounds_start, |data| {
                    matches!(data, SyntaxData::TraitBound { .. })
                });
                self.record_typed_syntax_node(
                    SyntaxKind::TypeParameter,
                    parameter_start,
                    self.current,
                    SyntaxData::TypeParameter {
                        name,
                        bounds: bound_ranges,
                    },
                );
                params.push((param, bounds));
                if !self.matches(&[TokenType::Comma]) {
                    break;
                }
            }
        }
        self.consume_token(TokenType::Gt, "Expected '>' after type parameters")?;
        self.record_syntax_node(SyntaxKind::TypeArguments, start, self.current);
        Ok(params)
    }

    pub(super) fn parse_enum_variants(&mut self) -> ParserResult<Vec<EnumVariant>> {
        let mut variants = Vec::new();
        self.skip_newlines();
        while !self.check(TokenType::CloseBrace) && !self.is_at_end() {
            let variant = self.parse_single_enum_variant()?;
            variants.push(variant);
            if !self.matches(&[TokenType::Comma]) {
                self.skip_newlines();
                break;
            }
            self.skip_newlines();
            if self.check(TokenType::CloseBrace) {
                break;
            }
        }
        Ok(variants)
    }

    pub(super) fn parse_single_enum_variant(&mut self) -> ParserResult<EnumVariant> {
        let variant_start = self.current;
        let variant_name = self.consume_identifier("Expected variant name")?;
        let name = self
            .previous()
            .span
            .byte_range
            .expect("enum variant name source range");
        let fields_start = self.syntax_events.len();
        let data = self.parse_enum_variant_data()?;
        let fields = data.as_ref().map(|_| {
            self.syntax_ranges_since(fields_start, |data| {
                matches!(data, SyntaxData::EnumVariantField { .. })
            })
        });
        let where_start = self.syntax_events.len();
        let where_clause = if self.check(TokenType::Where) {
            self.parse_where_clause()?
        } else {
            // Variants are newline-separated, so only a same-line `where`
            // belongs to this variant.
            None
        };
        let where_range = self.last_syntax_range_since(where_start, |data| {
            matches!(data, SyntaxData::WhereClause { .. })
        });
        self.record_typed_syntax_node(
            SyntaxKind::EnumVariant,
            variant_start,
            self.current,
            SyntaxData::EnumVariant {
                name,
                fields,
                where_clause: where_range,
            },
        );
        Ok(EnumVariant {
            name: variant_name,
            data,
            where_clause,
        })
    }

    pub(super) fn parse_enum_variant_data(
        &mut self,
    ) -> ParserResult<Option<Vec<EnumVariantField>>> {
        if !self.matches(&[TokenType::OpenParen]) {
            return Ok(None);
        }
        let mut fields = Vec::new();
        if !self.check(TokenType::CloseParen) {
            loop {
                let field_start = self.current;
                let field_type = self.parse_type()?;
                let type_range = field_type
                    .span
                    .byte_range
                    .expect("enum payload field type range");
                let field_name = if let TokenType::Id(name) = self.peek().token_type.clone() {
                    self.advance();
                    Some(name)
                } else {
                    None
                };
                let field_name_range = self
                    .previous()
                    .span
                    .byte_range
                    .filter(|_| field_name.is_some());
                self.record_typed_syntax_node(
                    SyntaxKind::EnumVariantField,
                    field_start,
                    self.current,
                    SyntaxData::EnumVariantField {
                        field_name: field_name_range,
                        type_range,
                    },
                );
                fields.push((field_name, field_type));
                if !self.matches(&[TokenType::Comma]) {
                    break;
                }
            }
        }
        self.consume_token(TokenType::CloseParen, "Expected ')' after variant data")?;
        Ok(Some(fields))
    }

    pub(super) fn import_declaration(&mut self) -> ParserResult<AstNode> {
        let import_start = self.current;
        let start_span = self.consume_token(TokenType::Import, "Expected 'import' keyword")?;
        let path_start = self.current;
        let module_path = self.parse_module_path()?;
        let path_end = self.current;
        let spec_start = self.current;
        let spec = self.parse_import_spec(&module_path)?;
        let end_span = self.previous().span;
        let span = start_span.combine(&end_span);
        let module_path_range = self
            .source_range_for_tokens(path_start, path_end)
            .expect("import module path source range");
        let syntax_spec = self.import_syntax_spec(spec_start, self.current);
        self.record_typed_syntax_node(
            SyntaxKind::ImportDeclaration,
            import_start,
            self.current,
            SyntaxData::Import {
                module_path: module_path_range,
                spec: syntax_spec,
                ast_span: span.byte_range.expect("import span range"),
            },
        );
        Ok(AstNode::Statement(StatementNode {
            kind: StatementKind::Import { module_path, spec },
            span,
        }))
    }

    fn import_syntax_spec(&self, start: usize, end: usize) -> SyntaxImportSpec {
        let range = |index: usize| {
            self.tokens[index]
                .span
                .byte_range
                .expect("import token source range")
        };
        if self
            .tokens
            .get(start)
            .is_some_and(|token| token.token_type == TokenType::Dot)
        {
            match self.tokens.get(start + 1).map(|token| &token.token_type) {
                Some(TokenType::Star) => SyntaxImportSpec::Wildcard,
                Some(TokenType::OpenParen) => {
                    let mut items = Vec::new();
                    let mut index = start + 2;
                    while index < end && self.tokens[index].token_type != TokenType::CloseParen {
                        if matches!(&self.tokens[index].token_type, TokenType::Id(_)) {
                            let item = range(index);
                            index += 1;
                            let alias = if self
                                .tokens
                                .get(index)
                                .is_some_and(|token| token.token_type == TokenType::As)
                            {
                                index += 1;
                                let alias = range(index);
                                index += 1;
                                Some(alias)
                            } else {
                                None
                            };
                            items.push((item, alias));
                        } else {
                            index += 1;
                        }
                    }
                    SyntaxImportSpec::Items { items }
                }
                Some(TokenType::Id(_)) => {
                    let item = range(start + 1);
                    let alias = if self
                        .tokens
                        .get(start + 2)
                        .is_some_and(|token| token.token_type == TokenType::As)
                    {
                        Some(range(start + 3))
                    } else {
                        None
                    };
                    SyntaxImportSpec::Item { item, alias }
                }
                _ => SyntaxImportSpec::Wildcard,
            }
        } else if self
            .tokens
            .get(start)
            .is_some_and(|token| token.token_type == TokenType::As)
        {
            let alias = range(start + 1);
            if self.tokens[start + 1].token_type == TokenType::Underscore {
                SyntaxImportSpec::Module {
                    alias: SyntaxModuleAlias::Hidden,
                }
            } else {
                SyntaxImportSpec::Module {
                    alias: SyntaxModuleAlias::Explicit(alias),
                }
            }
        } else {
            SyntaxImportSpec::Module {
                alias: SyntaxModuleAlias::Default,
            }
        }
    }

    /// Extracted: Parses the import spec following an import path.
    pub(super) fn parse_import_spec(&mut self, module_path: &str) -> ParserResult<ImportSpec> {
        if self.matches(&[TokenType::Dot]) {
            self.parse_dot_import_spec()
        } else {
            self.parse_module_import_spec(module_path)
        }
    }

    /// Extracted: Handles dot import: .*, .(items), .item (with optional alias)
    pub(super) fn parse_dot_import_spec(&mut self) -> ParserResult<ImportSpec> {
        if self.matches(&[TokenType::Star]) {
            Ok(ImportSpec::Wildcard)
        } else if self.matches(&[TokenType::OpenParen]) {
            self.parse_import_items()
        } else {
            let item = self.consume_identifier("Expected item name after '.'")?;
            let alias = if self.matches(&[TokenType::As]) {
                Some(self.consume_identifier("Expected alias after 'as'")?)
            } else {
                None
            };
            Ok(ImportSpec::Item { item, alias })
        }
    }

    /// Extracted: Handles import module (as alias or as _)
    pub(super) fn parse_module_import_spec(
        &mut self,
        module_path: &str,
    ) -> ParserResult<ImportSpec> {
        let alias = if self.matches(&[TokenType::As]) {
            let alias_name = self.consume_identifier("Expected alias after 'as'")?;
            if alias_name == "_" {
                None
            } else {
                Some(alias_name)
            }
        } else {
            Some(
                module_path
                    .split('.')
                    .next_back()
                    .unwrap_or(module_path)
                    .to_string(),
            )
        };
        Ok(ImportSpec::Module { alias })
    }

    // Parse module path with support for dots, relative (./, ../), and absolute (/)
    pub(super) fn parse_module_path(&mut self) -> ParserResult<String> {
        let mut module_path = String::new();

        // Handle relative/absolute paths
        if self.matches(&[TokenType::Dot]) {
            self.consume_token(TokenType::Slash, "Expected '/' after '.'")?;
            module_path.push_str("./");
        } else if self.matches(&[TokenType::DotDot]) {
            self.consume_token(TokenType::Slash, "Expected '/' after '..'")?;
            module_path.push_str("../");
        } else if self.matches(&[TokenType::Slash]) {
            // Absolute path /module
            module_path.push('/');
        }

        // Parse first identifier
        module_path.push_str(&self.consume_identifier("Expected module path")?);

        // Parse remaining dotted parts (utils.logger.helpers)
        // Stop if we see:
        // - .* (wildcard import)
        // - .( (multiple items import)
        // - .identifier followed by end/as/newline (single item import)
        while self.check(TokenType::Dot) {
            let next = self.peek_ahead(1);

            // Stop if next token after dot is * or (
            if next.is_some_and(|t| matches!(t.token_type, TokenType::Star | TokenType::OpenParen))
            {
                break;
            }

            // Stop if next is identifier but there's no dot after it (single item import)
            if next.is_some_and(|t| matches!(t.token_type, TokenType::Id(_))) {
                let after_identifier = self.peek_ahead(2);
                if !after_identifier.is_some_and(|t| matches!(t.token_type, TokenType::Dot)) {
                    // This is .identifier at end - it's an item import, not part of module path
                    break;
                }
            }

            self.advance(); // consume dot
            module_path.push('.');
            module_path.push_str(&self.consume_identifier("Expected module name after '.'")?);
        }

        Ok(module_path)
    }

    // Parse (item1, item2 as alias, item3)
    pub(super) fn parse_import_items(&mut self) -> ParserResult<ImportSpec> {
        let mut items = Vec::new();

        if !self.check(TokenType::CloseParen) {
            loop {
                let item = self.consume_identifier("Expected item name")?;
                let alias = if self.matches(&[TokenType::As]) {
                    Some(self.consume_identifier("Expected alias after 'as'")?)
                } else {
                    None
                };
                items.push((item, alias));

                if !self.matches(&[TokenType::Comma]) {
                    break;
                }
                self.skip_newlines();
            }
        }

        self.consume_token(TokenType::CloseParen, "Expected ')' after import items")?;
        Ok(ImportSpec::Items { items })
    }

    pub(super) fn function_declaration(&mut self, is_common: bool) -> ParserResult<AstNode> {
        let function_start = self.current;
        let start_span = self.peek().span;
        self.consume_token(TokenType::Func, "Expected 'func' keyword")?;
        let name = self.consume_identifier("Expected function name")?;
        let name_range = self
            .previous()
            .span
            .byte_range
            .expect("function name range");
        let type_params_start = self.syntax_events.len();
        let type_params = self.parse_is_type_params()?;
        let type_parameters = self.syntax_ranges_since(type_params_start, |data| {
            matches!(data, SyntaxData::TypeParameter { .. })
        });
        let params_start = self.current;
        self.consume_token(TokenType::OpenParen, "Expected '(' after function name")?;
        let params_event_start = self.syntax_events.len();
        let params = self.parse_function_params()?;
        let parameters = self.syntax_ranges_since(params_event_start, |data| {
            matches!(data, SyntaxData::Parameter { .. })
        });
        self.consume_token(TokenType::CloseParen, "Expected ')' after parameters")?;
        self.record_syntax_node(SyntaxKind::ParameterList, params_start, self.current);
        let where_event_start = self.syntax_events.len();
        let where_clause = self.parse_where_clause()?;
        let where_range = self.last_syntax_range_since(where_event_start, |data| {
            matches!(data, SyntaxData::WhereClause { .. })
        });
        if where_clause.is_some() {
            self.skip_newlines();
        }
        let return_event_start = self.syntax_events.len();
        let return_type = self.parse_required_return_type()?;
        let return_type_range = self.last_syntax_range_since(return_event_start, |data| {
            matches!(
                data,
                SyntaxData::TypeName { .. }
                    | SyntaxData::TypeReference { .. }
                    | SyntaxData::TypeContainer { .. }
                    | SyntaxData::FunctionType { .. }
            )
        });
        let return_type_span = return_type
            .span
            .byte_range
            .expect("function return type span range");
        self.skip_newlines();
        let body_event_start = self.syntax_events.len();
        let body_statements = self.parse_function_body(start_span)?;
        let body_range = self.last_statement_range_since(body_event_start);
        let end_span = body_statements.last().map_or(start_span, |s| s.span);
        let span = start_span.combine(&end_span);
        let ast_span = span.byte_range.expect("function span range");
        let function_end = self.current;
        self.record_typed_syntax_node(
            SyntaxKind::FunctionDeclaration,
            function_start,
            function_end,
            SyntaxData::Function {
                name: name_range,
                ast_span,
                type_parameters,
                parameters,
                return_type: return_type_range,
                return_type_span,
                where_clause: where_range,
                body: body_range,
                is_common,
            },
        );
        Ok(AstNode::Function(FunctionNode {
            name,
            type_params,
            params,
            return_type,
            body: body_statements,
            span,
            is_common,
            where_clause,
        }))
    }

    pub(super) fn parse_is_type_params(&mut self) -> ParserResult<Vec<(String, Vec<TraitBound>)>> {
        let start = self.current;
        if !self.matches(&[TokenType::Lt]) {
            return Ok(Vec::new());
        }
        let mut params = Vec::new();
        if !self.check(TokenType::Gt) {
            loop {
                let parameter_start = self.current;
                let param = self.consume_identifier("Expected type parameter name")?;
                let name_range = self
                    .previous()
                    .span
                    .byte_range
                    .expect("type parameter name has source range");
                let bounds_start = self.syntax_events.len();
                let trait_bounds = self.parse_ref_trait_bounds()?;
                let bound_ranges = self.syntax_ranges_since(bounds_start, |data| {
                    matches!(data, SyntaxData::TraitBound { .. })
                });
                self.record_typed_syntax_node(
                    SyntaxKind::TypeParameter,
                    parameter_start,
                    self.current,
                    SyntaxData::TypeParameter {
                        name: name_range,
                        bounds: bound_ranges,
                    },
                );
                params.push((param, trait_bounds));
                if !self.matches(&[TokenType::Comma]) {
                    break;
                }
            }
        }
        self.consume_token(TokenType::Gt, "Expected '>' after type parameters")?;
        self.record_syntax_node(SyntaxKind::TypeArguments, start, self.current);
        Ok(params)
    }

    pub(super) fn parse_ref_trait_bounds(&mut self) -> ParserResult<Vec<TraitBound>> {
        if !self.matches(&[TokenType::Is]) {
            return Ok(Vec::new());
        }
        let mut bounds = Vec::new();
        loop {
            let bound_start = self.current;
            let bound_name = self.consume_identifier("Expected trait name in bound")?;
            let bound_span = self.previous().span;
            let name = bound_span.byte_range.expect("trait bound name range");
            let type_arguments_start = self.syntax_events.len();
            let type_args = self.parse_optional_type_args()?;
            let type_arguments = self.syntax_ranges_since(type_arguments_start, |data| {
                matches!(
                    data,
                    SyntaxData::TypeName { .. }
                        | SyntaxData::TypeReference { .. }
                        | SyntaxData::TypeContainer { .. }
                        | SyntaxData::FunctionType { .. }
                )
            });
            self.record_typed_syntax_node(
                SyntaxKind::TraitBound,
                bound_start,
                self.current,
                SyntaxData::TraitBound {
                    name,
                    type_arguments,
                },
            );
            bounds.push(TraitBound {
                name: bound_name,
                type_params: type_args,
                span: bound_span,
            });
            if !self.matches(&[TokenType::Ref]) {
                break;
            }
        }
        Ok(bounds)
    }

    pub(super) fn parse_function_params(&mut self) -> ParserResult<Vec<Param>> {
        let mut params = Vec::new();
        if !self.check(TokenType::CloseParen) {
            let mut has_default = false;
            loop {
                let param = self.parse_single_param(&mut has_default)?;
                params.push(param);
                if !self.matches(&[TokenType::Comma]) {
                    break;
                }
            }
        }
        Ok(params)
    }

    pub(super) fn parse_single_param(&mut self, has_default: &mut bool) -> ParserResult<Param> {
        let parameter_start = self.current;
        let param_type = self.parse_type()?;
        let type_range = param_type.span.byte_range.expect("parameter type range");
        let param_name = self.consume_identifier("Expected parameter name")?;
        let name = self
            .previous()
            .span
            .byte_range
            .expect("parameter name range");
        let default_value = self.parse_param_default(has_default)?;
        let default_value_range = default_value
            .as_ref()
            .and_then(|value| value.span.byte_range);
        self.record_typed_syntax_node(
            SyntaxKind::Parameter,
            parameter_start,
            self.current,
            SyntaxData::Parameter {
                name,
                type_range,
                default_value: default_value_range,
            },
        );
        Ok(Param {
            name: param_name,
            type_: param_type,
            default_value,
        })
    }

    pub(super) fn parse_param_default(
        &mut self,
        has_default: &mut bool,
    ) -> ParserResult<Option<ExpressionNode>> {
        if !self.matches(&[TokenType::Eq]) {
            if *has_default {
                return Err(ParserError::with_help(
                    DiagnosticCode::ParseExpectedToken,
                    "Required parameter cannot follow a parameter with a default value",
                    self.previous().span,
                    "Move all parameters with default values to the end of the parameter list",
                ));
            }
            return Ok(None);
        }
        let default_expr = self.parse_expression()?;
        if !Self::is_literal_expression(&default_expr) {
            return Err(ParserError::with_help(
                DiagnosticCode::ParseExpectedToken,
                "Default parameter values must be literals",
                default_expr.span,
                "Only literal values (int, float, string, bool, char) are allowed as default parameter values. Example: func foo(int x = 10) returns void { ... }",
            ));
        }
        *has_default = true;
        Ok(Some(default_expr))
    }

    /// If the next non-newline token is `token_type`, consume the intervening
    /// newlines and return true; otherwise leave them unconsumed and return
    /// false. Lets a clause continue on a following line without eating
    /// newlines that separate declarations.
    pub(super) fn parse_field_declaration(
        &mut self,
        type_param_names: &[(String, Vec<TraitBound>)],
    ) -> ParserResult<Field> {
        let field_start = self.current;
        // Check if this is a const field
        let is_const = if self.check(TokenType::Const) {
            self.consume();
            true
        } else {
            false
        };

        let field_type = self.parse_type()?;
        let type_range = field_type
            .span
            .byte_range
            .expect("class field type source range");
        let field_name = self.consume_identifier("Expected field name")?;
        let name = self
            .previous()
            .span
            .byte_range
            .expect("class field name source range");

        // Check for optional default value. Any expression is allowed; it is
        // evaluated per instance when the constructor (`.new()`) runs, and its
        // type is checked against the field in semantic analysis.
        let default_value = if self.matches(&[TokenType::Eq]) {
            Some(self.parse_expression()?)
        } else {
            None
        };

        // For const fields, require a default value
        if is_const && default_value.is_none() {
            return Err(ParserError::with_help(
                DiagnosticCode::ParseExpectedToken,
                "Const fields must have a default value",
                self.previous().span,
                "Add a default value to the const field. Example: const int MAX_SIZE = 100",
            ));
        }

        let is_generic_param = Self::is_field_generic_param(&field_type, type_param_names);
        let default_value_range = default_value
            .as_ref()
            .and_then(|value| value.span.byte_range);
        let where_start = self.syntax_events.len();
        let where_clause = if self.check(TokenType::Where) {
            // Fields are newline-separated, so only a same-line `where`
            // belongs to this field.
            self.parse_where_clause()?
        } else {
            None
        };
        let where_range = self.last_syntax_range_since(where_start, |data| {
            matches!(data, SyntaxData::WhereClause { .. })
        });
        self.record_typed_syntax_node(
            SyntaxKind::FieldDeclaration,
            field_start,
            self.current,
            SyntaxData::Field {
                name,
                type_range,
                is_generic_param,
                is_const,
                default_value: default_value_range,
                where_clause: where_range,
            },
        );
        Ok(Field {
            name: field_name,
            type_: field_type,
            is_generic_param,
            is_const,
            default_value,
            where_clause,
        })
    }

    pub(super) fn is_field_generic_param(
        field_type: &TypeNode,
        type_param_names: &[(String, Vec<TraitBound>)],
    ) -> bool {
        match &field_type.kind {
            TypeKind::Named(name, type_args) => {
                // A field is a generic parameter if:
                // 1. It has no type arguments (e.g., T not T<int>)
                // 2. Its name matches a type parameter (e.g., T or U)
                type_args.is_empty()
                    && type_param_names
                        .iter()
                        .any(|(param_name, _)| param_name == name)
            }
            _ => false,
        }
    }

    pub(super) fn is_literal_expression(expr: &ExpressionNode) -> bool {
        matches!(expr.kind, ExpressionKind::Literal(_))
    }
}
