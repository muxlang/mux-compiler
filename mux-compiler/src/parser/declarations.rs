use super::types::TypeFact;
use super::*;
use crate::ast::SpanExt;

struct TypeParameterListFact {
    names: Vec<ByteRange>,
}

pub(super) struct ParameterDefaultFact {
    range: Option<ByteRange>,
}

impl<'a> Parser<'a> {
    pub(super) fn declaration(&mut self) -> ParserResult<()> {
        loop {
            let _ = self.skip_newlines();
            if self.is_at_end() {
                return Ok(());
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
                    return Ok(());
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

    pub(super) fn parse_declaration_content(&mut self) -> ParserResult<()> {
        if self.check(TokenType::Auto) {
            self.auto_declaration_fact().map(|_| ())
        } else if self.check(TokenType::Const) {
            self.const_declaration_fact().map(|_| ())
        } else if self.check(TokenType::Common) {
            self.consume();
            self.function_declaration_fact(true)
        } else if self.check(TokenType::Func) {
            self.function_declaration_fact(false)
        } else if let TokenType::Id(_) = &self.peek().token_type {
            self.parse_id_start_declaration()
        } else if self.check(TokenType::Class) {
            self.class_declaration_fact().map(|_| ())
        } else if self.check(TokenType::Interface) {
            self.interface_declaration_fact().map(|_| ())
        } else if self.check(TokenType::Enum) {
            self.enum_declaration()
        } else if self.check(TokenType::Test) {
            self.test_declaration().map(|_| ())
        } else if self.check(TokenType::Import) {
            self.import_declaration()
        } else {
            self.statement()
        }
    }

    pub(super) fn parse_id_start_declaration(&mut self) -> ParserResult<()> {
        let checkpoint = self.checkpoint();
        let parsed_type = self.parse_type_fact().is_ok();
        if parsed_type {
            self.parse_typed_or_statement(checkpoint)
        } else {
            self.rewind(checkpoint);
            self.statement()
        }
    }

    pub(super) fn parse_typed_or_statement(
        &mut self,
        checkpoint: ParserCheckpoint,
    ) -> ParserResult<()> {
        if let TokenType::Id(_) = &self.peek().token_type {
            let next = self.current + 1;
            if next < self.tokens.len() && self.tokens[next].token_type == TokenType::Eq {
                self.rewind(checkpoint);
                self.parse_typed_declaration_with_recovery()
            } else {
                self.rewind(checkpoint);
                self.statement()
            }
        } else {
            self.rewind(checkpoint);
            self.statement()
        }
    }

    pub(super) fn parse_typed_declaration_with_recovery(&mut self) -> ParserResult<()> {
        match self.typed_declaration_fact() {
            Ok(_) => Ok(()),
            Err(e)
                if matches!(
                    e.message.as_ref(),
                    "must be terminated with a newline" | "expected newline after statement"
                ) =>
            {
                self.record_error(e);
                self.synchronize();
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    fn auto_declaration_fact(&mut self) -> ParserResult<()> {
        let start = self.current;
        let start_span = self.peek().span;
        self.advance();

        self.consume_identifier_fact("Expected variable name after 'auto'")?;
        let name_span = self.tokens[self.current - 1].span;

        self.consume_token(TokenType::Eq, "Expected '=' after variable name")?;
        let value = self.parse_expression_parsed()?;

        // Validate that postfix ++ and -- don't appear in declarations
        self.check_no_postfix_increment_decrement_parsed(&value)?;

        self.record_typed_syntax_node(
            SyntaxKind::Statement,
            start,
            self.current,
            AstLoweringData::VariableDeclaration {
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
                ast_span: start_span
                    .combine(&value.span)
                    .byte_range
                    .expect("auto declaration AST range"),
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

        Ok(())
    }

    fn const_declaration_fact(&mut self) -> ParserResult<()> {
        let start = self.current;
        let start_span = self.peek().span;
        self.advance();

        let type_range = self
            .parse_type_fact()?
            .source_range()
            .expect("constant type source range");
        self.consume_identifier_fact("Expected constant name after type")?;
        let name_span = self.previous().span;

        self.consume_token(TokenType::Eq, "Expected '=' after constant name")?;
        let value = self.parse_expression_parsed()?;

        // Validate that postfix ++ and -- don't appear in declarations
        self.check_no_postfix_increment_decrement_parsed(&value)?;

        self.record_typed_syntax_node(
            SyntaxKind::Statement,
            start,
            self.current,
            AstLoweringData::VariableDeclaration {
                kind: VariableDeclarationKind::Const,
                name: name_span
                    .byte_range
                    .expect("constant name has source range"),
                type_range: Some(type_range),
                value: Some(
                    value
                        .span
                        .byte_range
                        .expect("constant initializer has source range"),
                ),
                ast_span: start_span
                    .combine(&value.span)
                    .byte_range
                    .expect("constant declaration AST range"),
            },
        );

        Ok(())
    }

    pub(super) fn typed_declaration_fact(&mut self) -> ParserResult<()> {
        let start = self.current;
        let start_span = self.peek().span;
        let type_range = self
            .parse_type_fact()?
            .source_range()
            .expect("typed declaration type source range");
        self.consume_identifier_fact("Expected variable name after type")?;
        let name_span = self.previous().span;

        // `Type name` with no initializer. Deciding here, after the name, is
        // what lets an initialized and an uninitialized declaration share one
        // entry point (#393).
        if !self.check(TokenType::Eq) {
            self.record_typed_syntax_node(
                SyntaxKind::Statement,
                start,
                self.current,
                AstLoweringData::VariableDeclaration {
                    kind: VariableDeclarationKind::Uninitialized,
                    name: name_span
                        .byte_range
                        .expect("uninitialized declaration name has source range"),
                    type_range: Some(type_range),
                    value: None,
                    ast_span: start_span
                        .combine(&name_span)
                        .byte_range
                        .expect("uninitialized declaration AST range"),
                },
            );
            return Ok(());
        }

        self.consume_token(TokenType::Eq, "Expected '=' after variable name")?;
        let value = self.parse_expression_parsed()?;

        // Validate that postfix ++ and -- don't appear in declarations
        self.check_no_postfix_increment_decrement_parsed(&value)?;

        self.record_typed_syntax_node(
            SyntaxKind::Statement,
            start,
            self.current,
            AstLoweringData::VariableDeclaration {
                kind: VariableDeclarationKind::Typed,
                name: name_span
                    .byte_range
                    .expect("typed declaration name has source range"),
                type_range: Some(type_range),
                value: Some(
                    value
                        .span
                        .byte_range
                        .expect("typed initializer has source range"),
                ),
                ast_span: start_span
                    .combine(&value.span)
                    .byte_range
                    .expect("typed declaration AST range"),
            },
        );

        Ok(())
    }

    fn class_declaration_fact(&mut self) -> ParserResult<()> {
        let class_start = self.current;
        let start_span = self.tokens[self.current].span;
        self.consume_token(TokenType::Class, "Expected 'class' keyword")?;
        self.consume_identifier_fact("Expected class name")?;
        let name_range = self
            .previous()
            .span
            .byte_range
            .expect("class name source range");
        let type_parameters_start = self.syntax_events.len();
        let type_params = self.parse_type_params_list()?;
        let type_parameters = self.syntax_ranges_since(type_parameters_start, |data| {
            matches!(data, AstLoweringData::TypeParameter { .. })
        });
        let traits_start = self.syntax_events.len();
        self.parse_trait_list()?;
        let trait_ranges = self.syntax_ranges_since(traits_start, |data| {
            matches!(data, AstLoweringData::TraitReference { .. })
        });
        let body_start = self.current;
        self.consume_token(TokenType::OpenBrace, "Expected '{' after class header")?;
        let members_start = self.syntax_events.len();
        self.parse_class_body(&type_params.names)?;
        let field_ranges = self.syntax_ranges_since(members_start, |data| {
            matches!(data, AstLoweringData::Field { .. })
        });
        let method_ranges = self.syntax_ranges_since(members_start, |data| {
            matches!(data, AstLoweringData::ClassMethod { .. })
        });
        let end_span =
            self.consume_token(TokenType::CloseBrace, "Expected '}' after class body")?;
        self.record_syntax_node(SyntaxKind::ClassBody, body_start, self.current);
        let where_start = self.syntax_events.len();
        let where_clause = self.parse_where_clause_fact()?;
        let where_range = self.last_syntax_range_since(where_start, |data| {
            matches!(data, AstLoweringData::WhereClause { .. })
        });
        let full_span = match &where_clause {
            Some(clause) => start_span.combine(&clause.span),
            None => start_span.combine(&end_span),
        };
        let lowering_data = AstLoweringData::Class {
            name: name_range,
            type_parameters,
            traits: trait_ranges,
            fields: field_ranges,
            methods: method_ranges,
            where_clause: where_range,
            ast_span: full_span.byte_range.expect("class span source range"),
        };
        self.record_typed_syntax_node(
            SyntaxKind::ClassDeclaration,
            class_start,
            self.current,
            lowering_data,
        );
        Ok(())
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
    fn test_declaration(&mut self) -> ParserResult<()> {
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
        let name_token = self.consume();
        let name_range = {
            match &name_token.token_type {
                TokenType::Str(name) => {
                    if name.is_empty() {
                        return Err(ParserError::new(
                            DiagnosticCode::ParseExpectedToken,
                            "Test name must not be empty".to_string(),
                            name_token.span,
                        ));
                    }
                }
                _ => {
                    return Err(ParserError::with_help(
                        DiagnosticCode::ParseExpectedToken,
                        "Expected a quoted test name after 'test'".to_string(),
                        name_token.span,
                        "Name tests with a string, for example: test \"adds numbers\" { ... }",
                    ));
                }
            };
            name_token
                .span
                .byte_range
                .expect("test name token source range")
        };
        self.skip_newlines();
        let block_start = self.current;
        let block = self.block()?;
        let block_span = self.span_for_byte_range(block.range);
        let body_contents = ByteRange::new(
            self.tokens[block_start]
                .span
                .byte_range
                .expect("test body opening brace range")
                .end,
            self.previous()
                .span
                .byte_range
                .expect("test body closing brace range")
                .start,
        );
        let span = start_span.combine(&block_span);
        let lowering_data = AstLoweringData::Test {
            name: name_range,
            body: block_span.byte_range.expect("test block source range"),
            body_contents,
            ast_span: span.byte_range.expect("test declaration source range"),
        };
        self.record_typed_syntax_node(
            SyntaxKind::TestDeclaration,
            test_start,
            self.current,
            lowering_data,
        );
        Ok(())
    }

    fn parse_type_params_list(&mut self) -> ParserResult<TypeParameterListFact> {
        let start = self.current;
        if !self.matches(&[TokenType::Lt]) {
            return Ok(TypeParameterListFact { names: Vec::new() });
        }
        let mut names = Vec::new();
        loop {
            let parameter_start = self.current;
            self.consume_identifier_fact("Expected type parameter name")?;
            let name = self
                .previous()
                .span
                .byte_range
                .expect("class type parameter name source range");
            let bounds_start = self.syntax_events.len();
            self.parse_trait_bounds()?;
            let bound_ranges = self.syntax_ranges_since(bounds_start, |data| {
                matches!(data, AstLoweringData::TraitBound { .. })
            });
            let lowering_data = AstLoweringData::TypeParameter {
                name,
                bounds: bound_ranges,
            };
            self.record_typed_syntax_node(
                SyntaxKind::TypeParameter,
                parameter_start,
                self.current,
                lowering_data,
            );
            names.push(name);
            if !self.matches(&[TokenType::Comma]) {
                break;
            }
        }
        self.consume_token(TokenType::Gt, "Expected '>' after type parameters")?;
        self.record_syntax_node(SyntaxKind::TypeArguments, start, self.current);
        Ok(TypeParameterListFact { names })
    }

    fn parse_trait_bounds(&mut self) -> ParserResult<()> {
        if !self.matches(&[TokenType::Is]) {
            return Ok(());
        }
        loop {
            let bound_start = self.current;
            self.consume_identifier_fact("Expected trait name in bound")?;
            let bound_span = self.previous().span;
            let type_args = self.parse_optional_type_argument_facts()?;
            let type_arguments = type_args
                .iter()
                .map(|argument| {
                    argument
                        .source_range()
                        .expect("trait bound type argument has source range")
                })
                .collect();
            let lowering_data = AstLoweringData::TraitBound {
                name: bound_span
                    .byte_range
                    .expect("trait bound name has source range"),
                type_arguments,
            };
            self.record_typed_syntax_node(
                SyntaxKind::TraitBound,
                bound_start,
                self.current,
                lowering_data,
            );
            if !self.matches(&[TokenType::Ref]) {
                break;
            }
        }
        Ok(())
    }

    fn parse_trait_list(&mut self) -> ParserResult<Vec<ByteRange>> {
        if !self.matches(&[TokenType::Is]) {
            return Ok(Vec::new());
        }
        let mut trait_ranges = Vec::new();
        loop {
            let trait_start = self.current;
            self.consume_identifier_fact("Expected trait name")?;
            let trait_span = self.previous().span;
            let name = trait_span
                .byte_range
                .expect("trait reference name source range");
            let type_arguments_start = self.syntax_events.len();
            self.parse_optional_type_argument_facts()?;
            let type_arguments = self.syntax_ranges_since(type_arguments_start, |data| {
                matches!(
                    data,
                    AstLoweringData::TypeName { .. }
                        | AstLoweringData::TypeReference { .. }
                        | AstLoweringData::TypeContainer { .. }
                        | AstLoweringData::FunctionType { .. }
                )
            });
            let lowering_data = AstLoweringData::TraitReference {
                name,
                type_arguments,
            };
            self.record_typed_syntax_node(
                SyntaxKind::TraitReference,
                trait_start,
                self.current,
                lowering_data,
            );
            trait_ranges.push(name);
            if !self.matches(&[TokenType::Comma]) {
                break;
            }
        }
        Ok(trait_ranges)
    }

    fn parse_optional_type_argument_facts(&mut self) -> ParserResult<Vec<TypeFact>> {
        let start = self.current;
        if self.matches(&[TokenType::Lt]) {
            let args = self.parse_type_argument_facts()?;
            self.consume_token(TokenType::Gt, "Expected '>' after type arguments")?;
            self.record_syntax_node(SyntaxKind::TypeArguments, start, self.current);
            Ok(args)
        } else {
            Ok(Vec::new())
        }
    }

    fn parse_class_body(&mut self, type_params: &[ByteRange]) -> ParserResult<()> {
        while !self.check(TokenType::CloseBrace) && !self.is_at_end() {
            let checkpoint = self.checkpoint();
            if let Err(error) = self.parse_class_member(type_params) {
                self.rewind(checkpoint);
                self.record_error(error);
                self.recover_class_member();
                if self.current == checkpoint.current {
                    self.advance();
                }
            }
        }
        Ok(())
    }

    /// Skip a malformed class member as a balanced unit, starting from the token
    /// the member began at. Matched brace pairs are consumed together; recovery
    /// ends at the member's terminating newline or the class's closing brace.
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

    fn parse_class_member(&mut self, type_params: &[ByteRange]) -> ParserResult<()> {
        match self.peek().token_type {
            TokenType::Func | TokenType::Common => {
                let member_start = self.current;
                let is_common = self.matches(&[TokenType::Common]);
                let name_span = self.peek_ahead(1).map(|token| token.span);
                let function_event_start = self.syntax_events.len();
                self.function_declaration_fact(is_common)?;
                let name_range = name_span
                    .and_then(|span| span.byte_range)
                    .expect("class method name source range");
                if let Some(message) = self
                    .identifier_at_range(name_range)
                    .and_then(reserved_class_method_error)
                {
                    self.record_error(ParserError::new(
                        DiagnosticCode::ParseExpectedToken,
                        &message,
                        name_span.unwrap_or(self.previous().span),
                    ));
                    return Ok(());
                }
                let function_range = self
                    .last_syntax_range_since(function_event_start, |data| {
                        matches!(data, AstLoweringData::Function { .. })
                    })
                    .expect("parsed class method has a function context");
                self.record_typed_syntax_node(
                    SyntaxKind::ClassMethod,
                    member_start,
                    self.current,
                    AstLoweringData::ClassMethod {
                        function: function_range,
                    },
                );
            }
            TokenType::Id(_) | TokenType::Const => {
                self.parse_field_declaration_fact(type_params)?;
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
                    "Class bodies can only contain field declarations and method declarations",
                ));
            }
        }
        Ok(())
    }

    fn interface_declaration_fact(&mut self) -> ParserResult<()> {
        let interface_start = self.current;
        let start_span = self.tokens[self.current].span;
        self.consume_token(TokenType::Interface, "Expected 'interface' keyword")?;
        self.consume_identifier_fact("Expected interface name")?;
        let name_range = self
            .previous()
            .span
            .byte_range
            .expect("interface name source range");
        let type_parameters_start = self.syntax_events.len();
        let type_params = self.parse_interface_type_params()?;
        let type_parameters = self.syntax_ranges_since(type_parameters_start, |data| {
            matches!(data, AstLoweringData::TypeParameter { .. })
        });
        let body_start = self.current;
        self.consume_token(TokenType::OpenBrace, "Expected '{' after interface header")?;
        let members_start = self.syntax_events.len();
        self.parse_interface_body(&type_params.names)?;
        let field_ranges = self.syntax_ranges_since(members_start, |data| {
            matches!(data, AstLoweringData::Field { .. })
        });
        let method_ranges = self.syntax_ranges_since(members_start, |data| {
            matches!(data, AstLoweringData::Function { .. })
        });
        let end_span =
            self.consume_token(TokenType::CloseBrace, "Expected '}' after interface body")?;
        self.record_syntax_node(SyntaxKind::InterfaceBody, body_start, self.current);
        let full_span = start_span.combine(&end_span);
        self.record_typed_syntax_node(
            SyntaxKind::InterfaceDeclaration,
            interface_start,
            self.current,
            AstLoweringData::Interface {
                name: name_range,
                type_parameters,
                fields: field_ranges,
                methods: method_ranges,
                ast_span: full_span.byte_range.expect("interface span source range"),
            },
        );
        Ok(())
    }

    fn parse_interface_type_params(&mut self) -> ParserResult<TypeParameterListFact> {
        let start = self.current;
        if !self.matches(&[TokenType::Lt]) {
            return Ok(TypeParameterListFact { names: Vec::new() });
        }
        let mut names = Vec::new();
        if !self.check(TokenType::Gt) {
            loop {
                let parameter_start = self.current;
                self.consume_identifier_fact("Expected type parameter name")?;
                let name = self
                    .previous()
                    .span
                    .byte_range
                    .expect("interface type parameter name range");
                let bounds_start = self.syntax_events.len();
                self.parse_colon_trait_bounds()?;
                let bound_ranges = self.syntax_ranges_since(bounds_start, |data| {
                    matches!(data, AstLoweringData::TraitBound { .. })
                });
                self.record_typed_syntax_node(
                    SyntaxKind::TypeParameter,
                    parameter_start,
                    self.current,
                    AstLoweringData::TypeParameter {
                        name,
                        bounds: bound_ranges,
                    },
                );
                names.push(name);
                if !self.matches(&[TokenType::Comma]) {
                    break;
                }
            }
        }
        self.consume_token(TokenType::Gt, "Expected '>' after type parameters")?;
        self.record_syntax_node(SyntaxKind::TypeArguments, start, self.current);
        Ok(TypeParameterListFact { names })
    }

    fn parse_colon_trait_bounds(&mut self) -> ParserResult<()> {
        if !self.matches(&[TokenType::Colon]) {
            return Ok(());
        }
        loop {
            let bound_start = self.current;
            self.consume_identifier_fact("Expected trait name in bound")?;
            let name = self
                .previous()
                .span
                .byte_range
                .expect("trait bound name source range");
            let type_arguments_start = self.syntax_events.len();
            self.parse_optional_type_argument_facts()?;
            let type_arguments = self.syntax_ranges_since(type_arguments_start, |data| {
                matches!(
                    data,
                    AstLoweringData::TypeName { .. }
                        | AstLoweringData::TypeReference { .. }
                        | AstLoweringData::TypeContainer { .. }
                        | AstLoweringData::FunctionType { .. }
                )
            });
            self.record_typed_syntax_node(
                SyntaxKind::TraitBound,
                bound_start,
                self.current,
                AstLoweringData::TraitBound {
                    name,
                    type_arguments,
                },
            );
            if !self.matches(&[TokenType::Plus]) {
                break;
            }
        }
        Ok(())
    }

    fn parse_interface_body(&mut self, type_params: &[ByteRange]) -> ParserResult<()> {
        while !self.is_at_end() {
            self.skip_newlines();
            if self.check(TokenType::CloseBrace) {
                break;
            }
            self.parse_interface_member(type_params)?;
        }
        Ok(())
    }

    fn parse_interface_member(&mut self, type_params: &[ByteRange]) -> ParserResult<()> {
        match self.peek().token_type {
            TokenType::Func => self.parse_interface_method(),
            TokenType::Id(_) | TokenType::Const => self.parse_field_declaration_fact(type_params),
            TokenType::NewLine => self
                .consume_token(TokenType::NewLine, "Expected newline")
                .map(drop),
            _ => Err(ParserError::new(
                DiagnosticCode::ParseExpectedToken,
                "Expected field or function declaration in interface",
                self.peek().span,
            )),
        }
    }

    fn parse_interface_method(&mut self) -> ParserResult<()> {
        let function_start = self.current;
        let function_start_span = self.peek().span;
        self.consume();
        self.consume_identifier_fact("Expected method name")?;
        let name_range = self.previous().span.byte_range.expect("method name range");
        let type_params_start = self.syntax_events.len();
        self.parse_simple_type_params()?;
        let type_parameters = self.syntax_ranges_since(type_params_start, |data| {
            matches!(data, AstLoweringData::TypeParameter { .. })
        });
        self.consume_token(TokenType::OpenParen, "Expected '(' after method name")?;
        let params_start = self.current;
        let params_event_start = self.syntax_events.len();
        self.parse_param_list()?;
        let parameters = self.syntax_ranges_since(params_event_start, |data| {
            matches!(data, AstLoweringData::Parameter { .. })
        });
        self.consume_token(TokenType::CloseParen, "Expected ')' after parameters")?;
        self.record_syntax_node(SyntaxKind::ParameterList, params_start, self.current);
        let where_event_start = self.syntax_events.len();
        let where_clause = self.parse_where_clause_fact()?;
        let where_range = self.last_syntax_range_since(where_event_start, |data| {
            matches!(data, AstLoweringData::WhereClause { .. })
        });
        if where_clause.is_some() {
            self.skip_newlines_before(TokenType::Returns);
        }
        let return_event_start = self.syntax_events.len();
        let return_type = self.parse_optional_return_type()?;
        let return_type_range = self.last_syntax_range_since(return_event_start, |data| {
            matches!(
                data,
                AstLoweringData::TypeName { .. }
                    | AstLoweringData::TypeReference { .. }
                    | AstLoweringData::TypeContainer { .. }
                    | AstLoweringData::FunctionType { .. }
            )
        });
        let return_type_span = return_type
            .source_range()
            .expect("method return type source range");
        let end = self.current;
        let ast_span = self
            .source_range_for_tokens(function_start, end)
            .expect("interface method source range");
        let _ = function_start_span;
        self.record_typed_syntax_node(
            SyntaxKind::FunctionDeclaration,
            function_start,
            end,
            AstLoweringData::Function {
                name: name_range,
                ast_span,
                type_parameters,
                parameters,
                return_type: return_type_range,
                return_type_span,
                where_clause: where_range,
                body: None,
                is_common: false,
            },
        );
        Ok(())
    }

    fn parse_simple_type_params(&mut self) -> ParserResult<()> {
        let start = self.current;
        if !self.matches(&[TokenType::Lt]) {
            return Ok(());
        }
        if !self.check(TokenType::Gt) {
            loop {
                let parameter_start = self.current;
                self.consume_identifier_fact("Expected type parameter name")?;
                let name = self
                    .previous()
                    .span
                    .byte_range
                    .expect("type parameter name range");
                self.record_typed_syntax_node(
                    SyntaxKind::TypeParameter,
                    parameter_start,
                    self.current,
                    AstLoweringData::TypeParameter {
                        name,
                        bounds: Vec::new(),
                    },
                );
                if !self.matches(&[TokenType::Comma]) {
                    break;
                }
            }
        }
        self.consume_token(TokenType::Gt, "Expected '>' after type parameters")?;
        self.record_syntax_node(SyntaxKind::TypeArguments, start, self.current);
        Ok(())
    }

    fn parse_param_list(&mut self) -> ParserResult<()> {
        if !self.check(TokenType::CloseParen) {
            loop {
                let parameter_start = self.current;
                let type_range = self
                    .parse_type_fact()?
                    .source_range()
                    .expect("parameter type range");
                self.consume_identifier_fact("Expected parameter name")?;
                let name = self
                    .previous()
                    .span
                    .byte_range
                    .expect("parameter name range");
                self.record_typed_syntax_node(
                    SyntaxKind::Parameter,
                    parameter_start,
                    self.current,
                    AstLoweringData::Parameter {
                        name,
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
        Ok(())
    }

    fn parse_optional_return_type(&mut self) -> ParserResult<TypeFact> {
        if self.matches(&[TokenType::Minus, TokenType::Gt]) || self.matches(&[TokenType::Returns]) {
            self.parse_type_fact()
        } else {
            Ok(TypeFact::void(self.peek().span))
        }
    }

    fn enum_declaration(&mut self) -> ParserResult<()> {
        let enum_start = self.current;
        let start_span = self.tokens[self.current].span;
        self.consume_token(TokenType::Enum, "Expected 'enum' keyword")?;
        self.consume_identifier_fact("Expected enum name")?;
        let name_range = self
            .previous()
            .span
            .byte_range
            .expect("enum name source range");
        let type_parameters_start = self.syntax_events.len();
        self.parse_colon_type_params()?;
        let type_parameters = self.syntax_ranges_since(type_parameters_start, |data| {
            matches!(data, AstLoweringData::TypeParameter { .. })
        });
        let body_start = self.current;
        self.consume_token(TokenType::OpenBrace, "Expected '{' after enum header")?;
        let variants_start = self.syntax_events.len();
        self.parse_enum_variants()?;
        let variant_ranges = self.syntax_ranges_since(variants_start, |data| {
            matches!(data, AstLoweringData::EnumVariant { .. })
        });
        let end_span =
            self.consume_token(TokenType::CloseBrace, "Expected '}' after enum variants")?;
        self.record_syntax_node(SyntaxKind::EnumBody, body_start, self.current);
        let full_span = start_span.combine(&end_span);
        let lowering_data = AstLoweringData::Enum {
            name: name_range,
            type_parameters,
            variants: variant_ranges,
            ast_span: full_span.byte_range.expect("enum span source range"),
        };
        self.record_typed_syntax_node(
            SyntaxKind::EnumDeclaration,
            enum_start,
            self.current,
            lowering_data,
        );
        Ok(())
    }

    fn parse_colon_type_params(&mut self) -> ParserResult<()> {
        let start = self.current;
        if !self.matches(&[TokenType::Lt]) {
            return Ok(());
        }
        if !self.check(TokenType::Gt) {
            loop {
                let parameter_start = self.current;
                self.consume_identifier_fact("Expected type parameter name")?;
                let name = self
                    .previous()
                    .span
                    .byte_range
                    .expect("enum type parameter name source range");
                let bounds_start = self.syntax_events.len();
                self.parse_colon_trait_bounds()?;
                let bound_ranges = self.syntax_ranges_since(bounds_start, |data| {
                    matches!(data, AstLoweringData::TraitBound { .. })
                });
                let lowering_data = AstLoweringData::TypeParameter {
                    name,
                    bounds: bound_ranges,
                };
                self.record_typed_syntax_node(
                    SyntaxKind::TypeParameter,
                    parameter_start,
                    self.current,
                    lowering_data,
                );
                if !self.matches(&[TokenType::Comma]) {
                    break;
                }
            }
        }
        self.consume_token(TokenType::Gt, "Expected '>' after type parameters")?;
        self.record_syntax_node(SyntaxKind::TypeArguments, start, self.current);
        Ok(())
    }

    fn parse_enum_variants(&mut self) -> ParserResult<()> {
        self.skip_newlines();
        while !self.check(TokenType::CloseBrace) && !self.is_at_end() {
            self.parse_single_enum_variant()?;
            if !self.matches(&[TokenType::Comma]) {
                self.skip_newlines();
                break;
            }
            self.skip_newlines();
            if self.check(TokenType::CloseBrace) {
                break;
            }
        }
        Ok(())
    }

    fn parse_single_enum_variant(&mut self) -> ParserResult<()> {
        let variant_start = self.current;
        self.consume_identifier_fact("Expected variant name")?;
        let name = self
            .previous()
            .span
            .byte_range
            .expect("enum variant name source range");
        let fields_start = self.syntax_events.len();
        let has_fields = self.check(TokenType::OpenParen);
        self.parse_enum_variant_data()?;
        let fields = has_fields.then(|| {
            self.syntax_ranges_since(fields_start, |data| {
                matches!(data, AstLoweringData::EnumVariantField { .. })
            })
        });
        let where_start = self.syntax_events.len();
        if self.check(TokenType::Where) {
            self.parse_where_clause_fact()?;
        } else {
            // Variants are newline-separated, so only a same-line `where`
            // belongs to this variant.
        }
        let where_range = self.last_syntax_range_since(where_start, |data| {
            matches!(data, AstLoweringData::WhereClause { .. })
        });
        let lowering_data = AstLoweringData::EnumVariant {
            name,
            fields,
            where_clause: where_range,
        };
        self.record_typed_syntax_node(
            SyntaxKind::EnumVariant,
            variant_start,
            self.current,
            lowering_data,
        );
        Ok(())
    }

    fn parse_enum_variant_data(&mut self) -> ParserResult<()> {
        if !self.matches(&[TokenType::OpenParen]) {
            return Ok(());
        }
        if !self.check(TokenType::CloseParen) {
            loop {
                let field_start = self.current;
                let field_type = self.parse_type_fact()?;
                let type_range = field_type
                    .source_range()
                    .expect("enum payload field type range");
                let field_name_range = if matches!(&self.peek().token_type, TokenType::Id(_)) {
                    self.advance();
                    self.previous().span.byte_range
                } else {
                    None
                };
                self.record_typed_syntax_node(
                    SyntaxKind::EnumVariantField,
                    field_start,
                    self.current,
                    AstLoweringData::EnumVariantField {
                        field_name: field_name_range,
                        type_range,
                    },
                );
                if !self.matches(&[TokenType::Comma]) {
                    break;
                }
            }
        }
        self.consume_token(TokenType::CloseParen, "Expected ')' after variant data")?;
        Ok(())
    }

    fn import_declaration(&mut self) -> ParserResult<()> {
        let import_start = self.current;
        let start_span = self.consume_token(TokenType::Import, "Expected 'import' keyword")?;
        let path_start = self.current;
        self.parse_module_path()?;
        let path_end = self.current;
        let spec_start = self.current;
        self.parse_import_spec()?;
        let end_span = self.previous().span;
        let span = start_span.combine(&end_span);
        let module_path_range = self
            .source_range_for_tokens(path_start, path_end)
            .expect("import module path source range");
        let syntax_spec = self.import_syntax_spec(spec_start, self.current);
        let lowering_data = AstLoweringData::Import {
            module_path: module_path_range,
            spec: syntax_spec,
            ast_span: span.byte_range.expect("import span range"),
        };
        self.record_typed_syntax_node(
            SyntaxKind::ImportDeclaration,
            import_start,
            self.current,
            lowering_data,
        );
        Ok(())
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

    fn parse_import_spec(&mut self) -> ParserResult<()> {
        if self.matches(&[TokenType::Dot]) {
            if self.matches(&[TokenType::Star]) {
                return Ok(());
            }
            if self.matches(&[TokenType::OpenParen]) {
                if !self.check(TokenType::CloseParen) {
                    loop {
                        self.consume_identifier_fact("Expected item name")?;
                        if self.matches(&[TokenType::As]) {
                            self.consume_identifier_fact("Expected alias after 'as'")?;
                        }
                        if !self.matches(&[TokenType::Comma]) {
                            break;
                        }
                        self.skip_newlines();
                    }
                }
                self.consume_token(TokenType::CloseParen, "Expected ')' after import items")?;
            } else {
                self.consume_identifier_fact("Expected item name after '.'")?;
                if self.matches(&[TokenType::As]) {
                    self.consume_identifier_fact("Expected alias after 'as'")?;
                }
            }
        } else if self.matches(&[TokenType::As]) {
            self.consume_identifier_fact("Expected alias after 'as'")?;
        }
        Ok(())
    }

    // Parse module path with support for dots, relative (./, ../), and absolute (/)
    fn parse_module_path(&mut self) -> ParserResult<()> {
        if self.matches(&[TokenType::Dot]) {
            self.consume_token(TokenType::Slash, "Expected '/' after '.'")?;
        } else if self.matches(&[TokenType::DotDot]) {
            self.consume_token(TokenType::Slash, "Expected '/' after '..'")?;
        } else {
            self.matches(&[TokenType::Slash]);
        }

        self.consume_identifier_fact("Expected module path")?;

        while self.check(TokenType::Dot) {
            let next = self.peek_ahead(1);
            if next.is_some_and(|token| {
                matches!(token.token_type, TokenType::Star | TokenType::OpenParen)
            }) {
                break;
            }
            if next.is_some_and(|token| matches!(token.token_type, TokenType::Id(_))) {
                let after_identifier = self.peek_ahead(2);
                if !after_identifier.is_some_and(|token| matches!(token.token_type, TokenType::Dot))
                {
                    break;
                }
            }

            self.advance();
            self.consume_identifier_fact("Expected module name after '.'")?;
        }

        Ok(())
    }

    pub(super) fn function_declaration_fact(&mut self, is_common: bool) -> ParserResult<()> {
        let function_start = self.current;
        let start_span = self.peek().span;
        self.consume_token(TokenType::Func, "Expected 'func' keyword")?;
        self.consume_identifier_fact("Expected function name")?;
        let name_range = self
            .previous()
            .span
            .byte_range
            .expect("function name range");
        let type_params_start = self.syntax_events.len();
        self.parse_is_type_params()?;
        let type_parameters = self.syntax_ranges_since(type_params_start, |data| {
            matches!(data, AstLoweringData::TypeParameter { .. })
        });
        let params_start = self.current;
        self.consume_token(TokenType::OpenParen, "Expected '(' after function name")?;
        let params_event_start = self.syntax_events.len();
        self.parse_function_params()?;
        let parameters = self.syntax_ranges_since(params_event_start, |data| {
            matches!(data, AstLoweringData::Parameter { .. })
        });
        self.consume_token(TokenType::CloseParen, "Expected ')' after parameters")?;
        self.record_syntax_node(SyntaxKind::ParameterList, params_start, self.current);
        let where_event_start = self.syntax_events.len();
        self.parse_where_clause_fact()?;
        let where_range = self.last_syntax_range_since(where_event_start, |data| {
            matches!(data, AstLoweringData::WhereClause { .. })
        });
        if where_range.is_some() {
            self.skip_newlines();
        }
        let return_event_start = self.syntax_events.len();
        let return_type = self.parse_required_return_type()?;
        let return_type_range = self.last_syntax_range_since(return_event_start, |data| {
            matches!(
                data,
                AstLoweringData::TypeName { .. }
                    | AstLoweringData::TypeReference { .. }
                    | AstLoweringData::TypeContainer { .. }
                    | AstLoweringData::FunctionType { .. }
            )
        });
        let return_type_span = return_type
            .source_range()
            .expect("function return type span range");
        self.skip_newlines();
        let body_event_start = self.syntax_events.len();
        let block = self.block()?;
        let body_range = Some(block.range);
        let start_range = start_span.byte_range.expect("function start range");
        let ast_end = self
            .last_ast_statement_range_since(body_event_start, body_range)
            .map_or(start_range.end, |range| range.end);
        let ast_span = ByteRange::new(start_range.start, ast_end);
        let function_end = self.current;
        let lowering_data = AstLoweringData::Function {
            name: name_range,
            ast_span,
            type_parameters,
            parameters,
            return_type: return_type_range,
            return_type_span,
            where_clause: where_range,
            body: body_range,
            is_common,
        };
        self.record_typed_syntax_node(
            SyntaxKind::FunctionDeclaration,
            function_start,
            function_end,
            lowering_data,
        );
        Ok(())
    }

    fn parse_is_type_params(&mut self) -> ParserResult<()> {
        let start = self.current;
        if !self.matches(&[TokenType::Lt]) {
            return Ok(());
        }
        if !self.check(TokenType::Gt) {
            loop {
                let parameter_start = self.current;
                self.consume_identifier_fact("Expected type parameter name")?;
                let name_range = self
                    .previous()
                    .span
                    .byte_range
                    .expect("type parameter name has source range");
                let bounds_start = self.syntax_events.len();
                self.parse_ref_trait_bounds()?;
                let bound_ranges = self.syntax_ranges_since(bounds_start, |data| {
                    matches!(data, AstLoweringData::TraitBound { .. })
                });
                let lowering_data = AstLoweringData::TypeParameter {
                    name: name_range,
                    bounds: bound_ranges,
                };
                self.record_typed_syntax_node(
                    SyntaxKind::TypeParameter,
                    parameter_start,
                    self.current,
                    lowering_data,
                );
                if !self.matches(&[TokenType::Comma]) {
                    break;
                }
            }
        }
        self.consume_token(TokenType::Gt, "Expected '>' after type parameters")?;
        self.record_syntax_node(SyntaxKind::TypeArguments, start, self.current);
        Ok(())
    }

    fn parse_ref_trait_bounds(&mut self) -> ParserResult<()> {
        if !self.matches(&[TokenType::Is]) {
            return Ok(());
        }
        loop {
            let bound_start = self.current;
            self.consume_identifier_fact("Expected trait name in bound")?;
            let bound_span = self.previous().span;
            let name = bound_span.byte_range.expect("trait bound name range");
            let type_arguments_start = self.syntax_events.len();
            self.parse_optional_type_argument_facts()?;
            let type_arguments = self.syntax_ranges_since(type_arguments_start, |data| {
                matches!(
                    data,
                    AstLoweringData::TypeName { .. }
                        | AstLoweringData::TypeReference { .. }
                        | AstLoweringData::TypeContainer { .. }
                        | AstLoweringData::FunctionType { .. }
                )
            });
            let lowering_data = AstLoweringData::TraitBound {
                name,
                type_arguments,
            };
            self.record_typed_syntax_node(
                SyntaxKind::TraitBound,
                bound_start,
                self.current,
                lowering_data,
            );
            if !self.matches(&[TokenType::Ref]) {
                break;
            }
        }
        Ok(())
    }

    fn parse_function_params(&mut self) -> ParserResult<()> {
        if !self.check(TokenType::CloseParen) {
            let mut has_default = false;
            loop {
                self.parse_single_param(&mut has_default)?;
                if !self.matches(&[TokenType::Comma]) {
                    break;
                }
            }
        }
        Ok(())
    }

    fn parse_single_param(&mut self, has_default: &mut bool) -> ParserResult<()> {
        let parameter_start = self.current;
        let type_fact = self.parse_type_fact()?;
        let type_range = type_fact.source_range().expect("parameter type range");
        self.consume_identifier_fact("Expected parameter name")?;
        let name = self
            .previous()
            .span
            .byte_range
            .expect("parameter name range");
        let default_value = self.parse_param_default(has_default)?;
        self.record_typed_syntax_node(
            SyntaxKind::Parameter,
            parameter_start,
            self.current,
            AstLoweringData::Parameter {
                name,
                type_range,
                default_value: default_value.range,
            },
        );
        Ok(())
    }

    pub(super) fn parse_param_default(
        &mut self,
        has_default: &mut bool,
    ) -> ParserResult<ParameterDefaultFact> {
        if !self.matches(&[TokenType::Eq]) {
            if *has_default {
                return Err(ParserError::with_help(
                    DiagnosticCode::ParseExpectedToken,
                    "Required parameter cannot follow a parameter with a default value",
                    self.previous().span,
                    "Move all parameters with default values to the end of the parameter list",
                ));
            }
            return Ok(ParameterDefaultFact { range: None });
        }
        let default_expr = self.parse_expression_parsed()?;
        let is_literal = self
            .is_literal_expression_range(default_expr.span.byte_range, default_expr.event_start);
        if !is_literal {
            return Err(ParserError::with_help(
                DiagnosticCode::ParseExpectedToken,
                "Default parameter values must be literals",
                default_expr.span,
                "Only literal values (int, float, string, bool, char) are allowed as default parameter values. Example: func foo(int x = 10) returns void { ... }",
            ));
        }
        *has_default = true;
        let range = default_expr.span.byte_range;
        Ok(ParameterDefaultFact { range })
    }

    /// If the next non-newline token is `token_type`, consume the intervening
    /// newlines and return true; otherwise leave them unconsumed and return
    /// false. Lets a clause continue on a following line without eating
    /// newlines that separate declarations.
    fn parse_field_declaration_fact(&mut self, type_param_names: &[ByteRange]) -> ParserResult<()> {
        let field_start = self.current;
        // Check if this is a const field
        let is_const = if self.check(TokenType::Const) {
            self.consume();
            true
        } else {
            false
        };

        let field_type = self.parse_type_fact()?;
        let type_range = field_type
            .source_range()
            .expect("class field type source range");
        self.consume_identifier_fact("Expected field name")?;
        let name = self
            .previous()
            .span
            .byte_range
            .expect("class field name source range");

        // Check for optional default value. Any expression is allowed; it is
        // evaluated per instance when the constructor (`.new()`) runs, and its
        // type is checked against the field in semantic analysis.
        let (default_value_range, has_default_value) = if self.matches(&[TokenType::Eq]) {
            let value = self.parse_expression_parsed()?;
            let value_range = value.span.byte_range;
            let has_default_value = true;
            (value_range, has_default_value)
        } else {
            (None, false)
        };

        // For const fields, require a default value even when syntax parsing
        // parses the expression for syntax and validation.
        if is_const && !has_default_value {
            return Err(ParserError::with_help(
                DiagnosticCode::ParseExpectedToken,
                "Const fields must have a default value",
                self.previous().span,
                "Add a default value to the const field. Example: const int MAX_SIZE = 100",
            ));
        }

        let is_generic_param = self.is_field_generic_param(&field_type, type_param_names);
        let where_start = self.syntax_events.len();
        if self.check(TokenType::Where) {
            // Fields are newline-separated, so only a same-line `where`
            // belongs to this field.
            self.parse_where_clause_fact()?;
        }
        let where_range = self.last_syntax_range_since(where_start, |data| {
            matches!(data, AstLoweringData::WhereClause { .. })
        });
        let lowering_data = AstLoweringData::Field {
            name,
            type_range,
            is_generic_param,
            is_const,
            default_value: default_value_range,
            where_clause: where_range,
        };
        self.record_typed_syntax_node(
            SyntaxKind::FieldDeclaration,
            field_start,
            self.current,
            lowering_data,
        );
        Ok(())
    }

    fn is_field_generic_param(
        &self,
        field_type: &TypeFact,
        type_param_names: &[ByteRange],
    ) -> bool {
        let Some(type_name_range) = field_type.generic_parameter_name_range() else {
            return false;
        };
        let Some(type_name) = self.identifier_at_range(type_name_range) else {
            return false;
        };
        type_param_names
            .iter()
            .any(|parameter| self.identifier_at_range(*parameter) == Some(type_name))
    }

    fn identifier_at_range(&self, range: ByteRange) -> Option<&str> {
        let index = self.tokens.partition_point(|token| {
            token
                .span
                .byte_range
                .is_some_and(|token_range| token_range.start < range.start)
        });
        let token = self.tokens.get(index)?;
        if token.span.byte_range != Some(range) {
            return None;
        }
        match &token.token_type {
            TokenType::Id(name) => Some(name),
            _ => None,
        }
    }

    fn is_literal_expression_range(&self, range: Option<ByteRange>, event_start: usize) -> bool {
        range.is_some_and(|range| {
            self.syntax_events[event_start..].iter().any(|event| {
                event.range == range
                    && matches!(event.data.as_ref(), Some(AstLoweringData::Literal { .. }))
            })
        })
    }
}
