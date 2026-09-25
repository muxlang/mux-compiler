use super::statements::WhereClauseFact;
use super::types::TypeFact;
use super::*;

type EnumVariantDataFact = Vec<(Option<String>, TypeFact)>;

struct TestDeclarationFact {
    range: ByteRange,
    span: Span,
    name: Option<String>,
    body: Vec<StatementNode>,
}

impl TestDeclarationFact {
    fn into_compatibility_ast(self) -> AstNode {
        let Self {
            range,
            span,
            name,
            body,
        } = self;
        debug_assert!(span.byte_range.is_some_and(|span_range| {
            range.start <= span_range.start && span_range.end <= range.end
        }));
        AstNode::Test {
            name: name.expect("compatibility test declaration name"),
            body,
            span,
        }
    }
}

struct ImportDeclarationFact {
    range: ByteRange,
    span: Span,
    module_path_range: ByteRange,
    module_path: Option<String>,
    spec: Option<ImportSpec>,
}

impl ImportDeclarationFact {
    fn into_compatibility_ast(self) -> AstNode {
        let Self {
            range,
            span,
            module_path_range,
            module_path,
            spec,
        } = self;
        debug_assert!(span.byte_range.is_some_and(|span_range| {
            range.start <= span_range.start && span_range.end <= range.end
        }));
        debug_assert!(range.start <= module_path_range.start && module_path_range.end <= range.end);
        AstNode::Statement(StatementNode {
            kind: StatementKind::Import {
                module_path: module_path.expect("compatibility import path"),
                spec: spec.expect("compatibility import spec"),
            },
            span,
        })
    }
}

struct EnumDeclarationFact {
    range: ByteRange,
    span: Span,
    name: Option<String>,
    type_params: Vec<TypeParameterFact>,
    variants: Vec<EnumVariantFact>,
}

struct TypeParameterFact {
    range: ByteRange,
    span: Span,
    name: Option<String>,
    name_range: ByteRange,
    bounds: Vec<TraitBoundFact>,
}

impl TypeParameterFact {
    fn into_compatibility(self) -> (String, Vec<TraitBound>) {
        let Self {
            range,
            span,
            name,
            name_range,
            bounds,
        } = self;
        debug_assert!(span.byte_range.is_some_and(|span_range| {
            range.start <= span_range.start && span_range.end <= range.end
        }));
        debug_assert!(range.start <= name_range.start && name_range.end <= range.end);
        (
            name.expect("compatibility type parameter fact has a name"),
            bounds
                .into_iter()
                .map(TraitBoundFact::into_compatibility)
                .collect(),
        )
    }
}

struct TraitBoundFact {
    span: Span,
    name: Option<String>,
    type_arguments: Vec<TypeFact>,
}

impl TraitBoundFact {
    fn into_compatibility(self) -> TraitBound {
        let Self {
            span,
            name,
            type_arguments,
        } = self;
        TraitBound {
            name: name.expect("compatibility trait bound name"),
            type_params: type_arguments
                .into_iter()
                .map(TypeFact::into_compat_type_node)
                .collect(),
            span,
        }
    }
}

struct TraitReferenceFact {
    span: Span,
    name: Option<String>,
    type_arguments: Vec<TypeFact>,
}

impl TraitReferenceFact {
    fn into_compatibility(self) -> TraitRef {
        let Self {
            span,
            name,
            type_arguments,
        } = self;
        TraitRef {
            name: name.expect("compatibility trait reference name"),
            type_args: type_arguments
                .into_iter()
                .map(TypeFact::into_compat_type_node)
                .collect(),
            span,
        }
    }
}

/// Parsed facts for one enum variant. The name, payload and constraint remain
/// available for the compatibility AST, while syntax consumers use the
/// recorded ranges.
struct EnumVariantFact {
    range: ByteRange,
    span: Span,
    name: Option<String>,
    data: Option<EnumVariantDataFact>,
    where_clause: Option<WhereClauseFact>,
}

impl EnumVariantFact {
    fn into_compatibility_variant(self) -> EnumVariant {
        let Self {
            range,
            span,
            name,
            data,
            where_clause,
        } = self;
        debug_assert!(span.byte_range.is_some_and(|span_range| {
            range.start <= span_range.start && span_range.end <= range.end
        }));
        EnumVariant {
            name: name.expect("compatibility enum variant name"),
            data: data.map(|fields| {
                fields
                    .into_iter()
                    .map(|(name, type_fact)| (name, type_fact.into_compat_type_node()))
                    .collect()
            }),
            where_clause: where_clause.map(WhereClauseFact::into_compatibility),
        }
    }
}

impl EnumDeclarationFact {
    fn into_compatibility_ast(self) -> AstNode {
        let Self {
            range,
            span,
            name,
            type_params,
            variants,
        } = self;
        debug_assert!(span.byte_range.is_some_and(|span_range| {
            range.start <= span_range.start && span_range.end <= range.end
        }));
        AstNode::Enum {
            name: name.expect("compatibility enum name"),
            type_params: type_params
                .into_iter()
                .map(TypeParameterFact::into_compatibility)
                .collect(),
            variants: variants
                .into_iter()
                .map(EnumVariantFact::into_compatibility_variant)
                .collect(),
            span,
        }
    }
}

struct FunctionDeclarationFact {
    range: ByteRange,
    span: Span,
    name: Option<String>,
    type_params: Vec<TypeParameterFact>,
    params: Vec<FunctionParameterFact>,
    return_type: TypeFact,
    body: Vec<StatementNode>,
    is_common: bool,
    where_clause: Option<WhereClauseFact>,
}

struct FunctionParameterFact {
    name: Option<String>,
    type_fact: TypeFact,
    default_value: ParameterDefaultFact,
}

pub(super) struct ParameterDefaultFact {
    range: Option<ByteRange>,
    expression: Option<ExpressionNode>,
}

impl FunctionParameterFact {
    fn into_compatibility_param(self) -> Param {
        Param {
            name: self
                .name
                .expect("compatibility parameter fact must retain its name"),
            type_: self.type_fact.into_compat_type_node(),
            default_value: self.default_value.expression,
        }
    }
}

impl FunctionDeclarationFact {
    fn into_compatibility_ast(self) -> AstNode {
        AstNode::Function(self.into_compatibility_function())
    }

    fn into_compatibility_function(self) -> FunctionNode {
        let Self {
            range,
            span,
            name,
            type_params,
            params,
            return_type,
            body,
            is_common,
            where_clause,
        } = self;
        debug_assert!(span.byte_range.is_some_and(|span_range| {
            range.start <= span_range.start && span_range.end <= range.end
        }));
        let params = params
            .into_iter()
            .map(FunctionParameterFact::into_compatibility_param)
            .collect();
        let type_params = type_params
            .into_iter()
            .map(TypeParameterFact::into_compatibility)
            .collect();
        FunctionNode {
            name: name.expect("compatibility function fact must retain its name"),
            type_params,
            params,
            return_type: return_type.into_compat_type_node(),
            body,
            span,
            is_common,
            where_clause: where_clause.map(WhereClauseFact::into_compatibility),
        }
    }
}

struct ClassDeclarationFact {
    range: ByteRange,
    span: Span,
    name: Option<String>,
    type_params: Vec<TypeParameterFact>,
    traits: Vec<TraitReferenceFact>,
    fields: Vec<FieldDeclarationFact>,
    methods: Vec<FunctionDeclarationFact>,
    where_clause: Option<WhereClauseFact>,
}

impl ClassDeclarationFact {
    fn into_compatibility_ast(self) -> AstNode {
        let Self {
            range,
            span,
            name,
            type_params,
            traits,
            fields,
            methods,
            where_clause,
        } = self;
        debug_assert!(span.byte_range.is_some_and(|span_range| {
            range.start <= span_range.start && span_range.end <= range.end
        }));
        let type_params = type_params
            .into_iter()
            .map(TypeParameterFact::into_compatibility)
            .collect();
        let traits = traits
            .into_iter()
            .map(TraitReferenceFact::into_compatibility)
            .collect();
        let fields = fields
            .into_iter()
            .map(FieldDeclarationFact::into_compatibility_field)
            .collect();
        let methods = methods
            .into_iter()
            .map(FunctionDeclarationFact::into_compatibility_function)
            .collect();
        AstNode::Class {
            name: name.expect("compatibility class fact must retain its name"),
            type_params,
            traits,
            fields,
            methods,
            where_clause: where_clause.map(WhereClauseFact::into_compatibility),
            span,
        }
    }
}

struct InterfaceDeclarationFact {
    range: ByteRange,
    span: Span,
    name: Option<String>,
    type_params: Vec<TypeParameterFact>,
    fields: Vec<FieldDeclarationFact>,
    methods: Vec<InterfaceMethodFact>,
}

struct InterfaceMethodFact {
    span: Span,
    name: Option<String>,
    type_params: Vec<TypeParameterFact>,
    params: Vec<FunctionParameterFact>,
    return_type: TypeFact,
    where_clause: Option<WhereClauseFact>,
}

impl InterfaceMethodFact {
    fn into_compatibility_function(self) -> FunctionNode {
        let Self {
            span,
            name,
            type_params,
            params,
            return_type,
            where_clause,
        } = self;
        let type_params = type_params
            .into_iter()
            .map(TypeParameterFact::into_compatibility)
            .collect();
        FunctionNode {
            name: name.expect("compatibility interface method fact must retain its name"),
            type_params,
            params: params
                .into_iter()
                .map(FunctionParameterFact::into_compatibility_param)
                .collect(),
            return_type: return_type.into_compat_type_node(),
            body: Vec::new(),
            span,
            is_common: false,
            where_clause: where_clause.map(WhereClauseFact::into_compatibility),
        }
    }
}

impl InterfaceDeclarationFact {
    fn into_compatibility_ast(self) -> AstNode {
        let Self {
            range,
            span,
            name,
            type_params,
            fields,
            methods,
        } = self;
        debug_assert!(span.byte_range.is_some_and(|span_range| {
            range.start <= span_range.start && span_range.end <= range.end
        }));
        let type_params = type_params
            .into_iter()
            .map(TypeParameterFact::into_compatibility)
            .collect();
        let fields = fields
            .into_iter()
            .map(FieldDeclarationFact::into_compatibility_field)
            .collect();
        let methods = methods
            .into_iter()
            .map(InterfaceMethodFact::into_compatibility_function)
            .collect();
        AstNode::Interface {
            name: name.expect("compatibility interface fact must retain its name"),
            type_params,
            fields,
            methods,
            span,
        }
    }
}

struct FieldDeclarationFact {
    range: ByteRange,
    span: Span,
    name: Option<String>,
    type_fact: TypeFact,
    is_generic_param: bool,
    is_const: bool,
    default_value: Option<ExpressionNode>,
    where_clause: Option<WhereClauseFact>,
}

impl FieldDeclarationFact {
    fn into_compatibility_field(self) -> Field {
        let Self {
            range,
            span,
            name,
            type_fact,
            is_generic_param,
            is_const,
            default_value,
            where_clause,
        } = self;
        debug_assert!(span.byte_range.is_some_and(|span_range| {
            range.start <= span_range.start && span_range.end <= range.end
        }));
        Field {
            name: name.expect("compatibility field fact must retain its name"),
            type_: type_fact.into_compat_type_node(),
            is_generic_param,
            is_const,
            default_value,
            where_clause: where_clause.map(WhereClauseFact::into_compatibility),
        }
    }
}

/// Source ranges recorded by the grammar for a variable declaration. The
/// expressions and type are retained only to materialize the legacy AST at
/// parser call sites; syntax consumers use the recorded ranges.
pub(super) struct VariableDeclarationFact {
    range: ByteRange,
    span: Span,
    kind: VariableDeclarationKind,
    name_range: ByteRange,
    name_span: Span,
    name: Option<String>,
    type_range: Option<ByteRange>,
    type_fact: Option<TypeFact>,
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
            type_fact,
            value,
        } = self;
        debug_assert_eq!(name_span.byte_range, Some(name_range));
        debug_assert_eq!(
            type_range,
            type_fact.as_ref().and_then(TypeFact::source_range)
        );
        debug_assert!(span.byte_range.is_some_and(|span_range| {
            range.start <= span_range.start && span_range.end <= range.end
        }));
        let type_node = type_fact.map(TypeFact::into_compat_type_node);
        let statement = match kind {
            VariableDeclarationKind::Auto => StatementKind::AutoDecl(
                name.expect("compatibility auto declaration name"),
                TypeNode {
                    kind: TypeKind::Auto,
                    span: name_span,
                },
                value.expect("auto declaration has initializer"),
            ),
            VariableDeclarationKind::Const => StatementKind::ConstDecl(
                name.expect("compatibility constant declaration name"),
                type_node.expect("constant declaration has type"),
                value.expect("constant declaration has initializer"),
            ),
            VariableDeclarationKind::Typed => StatementKind::TypedDecl(
                name.expect("compatibility typed declaration name"),
                type_node.expect("typed declaration has type"),
                value.expect("typed declaration has initializer"),
            ),
            VariableDeclarationKind::Uninitialized => StatementKind::UninitDecl(
                name.expect("compatibility uninitialized declaration name"),
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
            if self.mode == ParserMode::SyntaxOnly {
                self.auto_declaration_fact().map(|_| None)
            } else {
                self.auto_declaration().map(Some)
            }
        } else if self.check(TokenType::Const) {
            if self.mode == ParserMode::SyntaxOnly {
                self.const_declaration_fact().map(|_| None)
            } else {
                self.const_declaration().map(Some)
            }
        } else if self.check(TokenType::Common) {
            self.consume();
            if self.mode == ParserMode::SyntaxOnly {
                self.function_declaration_fact(true, false).map(|_| None)
            } else {
                self.function_declaration(true).map(Some)
            }
        } else if self.check(TokenType::Func) {
            if self.mode == ParserMode::SyntaxOnly {
                self.function_declaration_fact(false, false).map(|_| None)
            } else {
                self.function_declaration(false).map(Some)
            }
        } else if let TokenType::Id(_) = &self.peek().token_type {
            self.parse_id_start_declaration()
        } else if self.check(TokenType::Class) {
            if self.mode == ParserMode::SyntaxOnly {
                self.class_declaration_fact().map(|_| None)
            } else {
                self.class_declaration().map(Some)
            }
        } else if self.check(TokenType::Interface) {
            if self.mode == ParserMode::SyntaxOnly {
                self.interface_declaration_fact().map(|_| None)
            } else {
                self.interface_declaration().map(Some)
            }
        } else if self.check(TokenType::Enum) {
            if self.mode == ParserMode::SyntaxOnly {
                self.enum_declaration().map(|_| None)
            } else {
                self.enum_declaration()
                    .map(EnumDeclarationFact::into_compatibility_ast)
                    .map(Some)
            }
        } else if self.check(TokenType::Test) {
            if self.mode == ParserMode::SyntaxOnly {
                self.test_declaration().map(|_| None)
            } else {
                self.test_declaration()
                    .map(TestDeclarationFact::into_compatibility_ast)
                    .map(Some)
            }
        } else if self.check(TokenType::Import) {
            if self.mode == ParserMode::SyntaxOnly {
                self.import_declaration().map(|_| None)
            } else {
                self.import_declaration()
                    .map(ImportDeclarationFact::into_compatibility_ast)
                    .map(Some)
            }
        } else if self.mode == ParserMode::SyntaxOnly {
            self.syntax_only_statement().map(|()| None)
        } else {
            self.statement().map(Some)
        }
    }

    pub(super) fn parse_id_start_declaration(&mut self) -> ParserResult<Option<AstNode>> {
        let checkpoint = self.checkpoint();
        let parsed_type = if self.mode == ParserMode::SyntaxOnly {
            self.parse_type_fact().is_ok()
        } else {
            self.parse_type().is_ok()
        };
        if parsed_type {
            self.parse_typed_or_statement(checkpoint)
        } else {
            self.rewind(checkpoint);
            if self.mode == ParserMode::SyntaxOnly {
                self.syntax_only_statement().map(|()| None)
            } else {
                self.statement().map(Some)
            }
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
                if self.mode == ParserMode::SyntaxOnly {
                    self.syntax_only_statement().map(|()| None)
                } else {
                    self.statement().map(Some)
                }
            }
        } else {
            self.rewind(checkpoint);
            if self.mode == ParserMode::SyntaxOnly {
                self.syntax_only_statement().map(|()| None)
            } else {
                self.statement().map(Some)
            }
        }
    }

    pub(super) fn parse_typed_declaration_with_recovery(
        &mut self,
    ) -> ParserResult<Option<AstNode>> {
        if self.mode == ParserMode::SyntaxOnly {
            return match self.typed_declaration_fact() {
                Ok(_) => Ok(None),
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
            };
        }

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

        let name = self.consume_identifier_fact(
            "Expected variable name after 'auto'",
            self.mode == ParserMode::Compatibility,
        )?;
        let name_span = self.tokens[self.current - 1].span;

        self.consume_token(TokenType::Eq, "Expected '=' after variable name")?;
        let value = self.parse_expression_parsed()?;

        // Validate that postfix ++ and -- don't appear in declarations
        self.check_no_postfix_increment_decrement_parsed(&value)?;

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
            type_fact: None,
            value: value.node,
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

        let type_fact = self.parse_type_fact()?;
        let type_range = type_fact
            .source_range()
            .expect("constant type source range");
        let name = self.consume_identifier_fact(
            "Expected constant name after type",
            self.mode == ParserMode::Compatibility,
        )?;
        let name_span = self.previous().span;

        self.consume_token(TokenType::Eq, "Expected '=' after constant name")?;
        let value = self.parse_expression_parsed()?;

        // Validate that postfix ++ and -- don't appear in declarations
        self.check_no_postfix_increment_decrement_parsed(&value)?;

        self.record_typed_syntax_node(
            SyntaxKind::Statement,
            start,
            self.current,
            SyntaxData::VariableDeclaration {
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
            type_range: Some(type_range),
            type_fact: Some(type_fact),
            value: value.node,
        })
    }

    pub(super) fn typed_declaration(&mut self) -> ParserResult<AstNode> {
        self.typed_declaration_fact()
            .map(VariableDeclarationFact::into_compatibility_ast)
    }

    pub(super) fn typed_declaration_fact(&mut self) -> ParserResult<VariableDeclarationFact> {
        let start = self.current;
        let start_span = self.peek().span;
        let type_fact = self.parse_type_fact()?;
        let type_range = type_fact
            .source_range()
            .expect("typed declaration type source range");
        let name = self.consume_identifier_fact(
            "Expected variable name after type",
            self.mode == ParserMode::Compatibility,
        )?;
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
                    type_range: Some(type_range),
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
                type_range: Some(type_range),
                type_fact: Some(type_fact),
                value: None,
            });
        }

        self.consume_token(TokenType::Eq, "Expected '=' after variable name")?;
        let value = self.parse_expression_parsed()?;

        // Validate that postfix ++ and -- don't appear in declarations
        self.check_no_postfix_increment_decrement_parsed(&value)?;

        self.record_typed_syntax_node(
            SyntaxKind::Statement,
            start,
            self.current,
            SyntaxData::VariableDeclaration {
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
            type_range: Some(type_range),
            type_fact: Some(type_fact),
            value: value.node,
        })
    }

    pub(super) fn class_declaration(&mut self) -> ParserResult<AstNode> {
        self.class_declaration_fact()
            .map(ClassDeclarationFact::into_compatibility_ast)
    }

    fn class_declaration_fact(&mut self) -> ParserResult<ClassDeclarationFact> {
        let class_start = self.current;
        let start_span = self.tokens[self.current].span;
        self.consume_token(TokenType::Class, "Expected 'class' keyword")?;
        let name = self.consume_identifier_fact(
            "Expected class name",
            self.mode == ParserMode::Compatibility,
        )?;
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
        let (fields, methods) = self.parse_class_body(&type_params)?;
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
        let where_clause = self.parse_where_clause_fact()?;
        let where_range = self.last_syntax_range_since(where_start, |data| {
            matches!(data, SyntaxData::WhereClause { .. })
        });
        let full_span = match &where_clause {
            Some(clause) => start_span.combine(&clause.span),
            None => start_span.combine(&end_span),
        };
        let syntax_data = SyntaxData::Class {
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
            syntax_data,
        );
        Ok(ClassDeclarationFact {
            range: self
                .source_range_for_tokens(class_start, self.current)
                .expect("class declaration source range"),
            span: full_span,
            name,
            type_params,
            traits,
            fields,
            methods,
            where_clause,
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
    fn test_declaration(&mut self) -> ParserResult<TestDeclarationFact> {
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
        let compatibility = self.mode == ParserMode::Compatibility;
        let (name, name_range) = {
            let name_token = self.consume();
            let name = match &name_token.token_type {
                TokenType::Str(name) => {
                    if name.is_empty() {
                        return Err(ParserError::new(
                            DiagnosticCode::ParseExpectedToken,
                            "Test name must not be empty".to_string(),
                            name_token.span,
                        ));
                    }
                    compatibility.then(|| name.clone())
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
            (
                name,
                name_token
                    .span
                    .byte_range
                    .expect("test name token source range"),
            )
        };
        self.skip_newlines();
        let block = self.block()?;
        let body = block.statements;
        let block_span = block.span;
        let span = start_span.combine(&block_span);
        let syntax_data = SyntaxData::Test {
            name: name_range,
            body: block_span.byte_range.expect("test block source range"),
            ast_span: span.byte_range.expect("test declaration source range"),
        };
        self.record_typed_syntax_node(
            SyntaxKind::TestDeclaration,
            test_start,
            self.current,
            syntax_data,
        );
        Ok(TestDeclarationFact {
            range: self
                .source_range_for_tokens(test_start, self.current)
                .expect("test declaration source range"),
            span,
            name,
            body,
        })
    }

    fn parse_type_params_list(&mut self) -> ParserResult<Vec<TypeParameterFact>> {
        let start = self.current;
        if !self.matches(&[TokenType::Lt]) {
            return Ok(Vec::new());
        }
        let mut params = Vec::new();
        loop {
            let parameter_start = self.current;
            let parameter_span = self.peek().span;
            let param = self.consume_identifier_fact(
                "Expected type parameter name",
                self.mode == ParserMode::Compatibility,
            )?;
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
            let syntax_data = SyntaxData::TypeParameter {
                name,
                bounds: bound_ranges,
            };
            self.record_typed_syntax_node(
                SyntaxKind::TypeParameter,
                parameter_start,
                self.current,
                syntax_data,
            );
            params.push(TypeParameterFact {
                range: self
                    .source_range_for_tokens(parameter_start, self.current)
                    .expect("class type parameter source range"),
                span: parameter_span.combine(&self.previous().span),
                name: param,
                name_range: name,
                bounds,
            });
            if !self.matches(&[TokenType::Comma]) {
                break;
            }
        }
        self.consume_token(TokenType::Gt, "Expected '>' after type parameters")?;
        self.record_syntax_node(SyntaxKind::TypeArguments, start, self.current);
        Ok(params)
    }

    fn parse_trait_bounds(&mut self) -> ParserResult<Vec<TraitBoundFact>> {
        if !self.matches(&[TokenType::Is]) {
            return Ok(Vec::new());
        }
        let mut bounds = Vec::new();
        loop {
            let bound_start = self.current;
            let bound_name = self.consume_identifier_fact(
                "Expected trait name in bound",
                self.mode == ParserMode::Compatibility,
            )?;
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
            let syntax_data = SyntaxData::TraitBound {
                name: bound_span
                    .byte_range
                    .expect("trait bound name has source range"),
                type_arguments,
            };
            self.record_typed_syntax_node(
                SyntaxKind::TraitBound,
                bound_start,
                self.current,
                syntax_data,
            );
            bounds.push(TraitBoundFact {
                span: bound_span,
                name: bound_name,
                type_arguments: type_args,
            });
            if !self.matches(&[TokenType::Ref]) {
                break;
            }
        }
        Ok(bounds)
    }

    fn parse_trait_list(&mut self) -> ParserResult<Vec<TraitReferenceFact>> {
        if !self.matches(&[TokenType::Is]) {
            return Ok(Vec::new());
        }
        let mut traits_list = Vec::new();
        loop {
            let trait_start = self.current;
            let trait_name = self.consume_identifier_fact(
                "Expected trait name",
                self.mode == ParserMode::Compatibility,
            )?;
            let trait_span = self.previous().span;
            let name = trait_span
                .byte_range
                .expect("trait reference name source range");
            let type_arguments_start = self.syntax_events.len();
            let type_args = self.parse_optional_type_argument_facts()?;
            let type_arguments = self.syntax_ranges_since(type_arguments_start, |data| {
                matches!(
                    data,
                    SyntaxData::TypeName { .. }
                        | SyntaxData::TypeReference { .. }
                        | SyntaxData::TypeContainer { .. }
                        | SyntaxData::FunctionType { .. }
                )
            });
            let syntax_data = SyntaxData::TraitReference {
                name,
                type_arguments,
            };
            self.record_typed_syntax_node(
                SyntaxKind::TraitReference,
                trait_start,
                self.current,
                syntax_data,
            );
            traits_list.push(TraitReferenceFact {
                span: trait_span,
                name: trait_name,
                type_arguments: type_args,
            });
            if !self.matches(&[TokenType::Comma]) {
                break;
            }
        }
        Ok(traits_list)
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

    fn parse_class_body(
        &mut self,
        type_params: &[TypeParameterFact],
    ) -> ParserResult<(Vec<FieldDeclarationFact>, Vec<FunctionDeclarationFact>)> {
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
            if let Err(e) = self.parse_class_member(type_params, &mut fields, &mut methods) {
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

    fn parse_class_member(
        &mut self,
        type_params: &[TypeParameterFact],
        fields: &mut Vec<FieldDeclarationFact>,
        methods: &mut Vec<FunctionDeclarationFact>,
    ) -> ParserResult<()> {
        match self.peek().token_type {
            TokenType::Func => {
                let member_start = self.current;
                let function_event_start = self.syntax_events.len();
                let name_span = self.peek_ahead(1).map(|t| t.span);
                let func = self.function_declaration_fact(false, true)?;
                if let Some(message) = reserved_class_method_error(
                    func.name
                        .as_deref()
                        .expect("class method facts retain names for reserved-name checks"),
                ) {
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
            }
            TokenType::Common => {
                let member_start = self.current;
                self.consume();
                let function_event_start = self.syntax_events.len();
                let name_span = self.peek_ahead(1).map(|t| t.span);
                let func = self.function_declaration_fact(true, true)?;
                if let Some(message) = reserved_class_method_error(
                    func.name
                        .as_deref()
                        .expect("class method facts retain names for reserved-name checks"),
                ) {
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
            }
            TokenType::Id(_) | TokenType::Const => {
                let field = self.parse_field_declaration_fact(type_params)?;
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
        self.interface_declaration_fact()
            .map(InterfaceDeclarationFact::into_compatibility_ast)
    }

    fn interface_declaration_fact(&mut self) -> ParserResult<InterfaceDeclarationFact> {
        let interface_start = self.current;
        let start_span = self.tokens[self.current].span;
        self.consume_token(TokenType::Interface, "Expected 'interface' keyword")?;
        let name = self.consume_identifier_fact(
            "Expected interface name",
            self.mode == ParserMode::Compatibility,
        )?;
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
        let syntax_data = SyntaxData::Interface {
            name: name_range,
            type_parameters,
            fields: field_ranges,
            methods: method_ranges,
            ast_span: full_span.byte_range.expect("interface span source range"),
        };
        self.record_typed_syntax_node(
            SyntaxKind::InterfaceDeclaration,
            interface_start,
            self.current,
            syntax_data,
        );
        Ok(InterfaceDeclarationFact {
            range: self
                .source_range_for_tokens(interface_start, self.current)
                .expect("interface declaration source range"),
            span: full_span,
            name,
            type_params,
            fields,
            methods,
        })
    }

    fn parse_interface_type_params(&mut self) -> ParserResult<Vec<TypeParameterFact>> {
        let start = self.current;
        if !self.matches(&[TokenType::Lt]) {
            return Ok(Vec::new());
        }
        let mut params = Vec::new();
        if !self.check(TokenType::Gt) {
            loop {
                let parameter_start = self.current;
                let parameter_span = self.peek().span;
                let param = self.consume_identifier_fact(
                    "Expected type parameter name",
                    self.mode == ParserMode::Compatibility,
                )?;
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
                let syntax_data = SyntaxData::TypeParameter {
                    name,
                    bounds: bound_ranges,
                };
                self.record_typed_syntax_node(
                    SyntaxKind::TypeParameter,
                    parameter_start,
                    self.current,
                    syntax_data,
                );
                params.push(TypeParameterFact {
                    range: self
                        .source_range_for_tokens(parameter_start, self.current)
                        .expect("interface type parameter source range"),
                    span: parameter_span.combine(&self.previous().span),
                    name: param,
                    name_range: name,
                    bounds,
                });
                if !self.matches(&[TokenType::Comma]) {
                    break;
                }
            }
        }
        self.consume_token(TokenType::Gt, "Expected '>' after type parameters")?;
        self.record_syntax_node(SyntaxKind::TypeArguments, start, self.current);
        Ok(params)
    }

    fn parse_colon_trait_bounds(&mut self) -> ParserResult<Vec<TraitBoundFact>> {
        if !self.matches(&[TokenType::Colon]) {
            return Ok(Vec::new());
        }
        let mut bounds = Vec::new();
        loop {
            let bound_start = self.current;
            let bound_name = self.consume_identifier_fact(
                "Expected trait name in bound",
                self.mode == ParserMode::Compatibility,
            )?;
            let bound_span = self.previous().span;
            let name = bound_span
                .byte_range
                .expect("trait bound name source range");
            let type_arguments_start = self.syntax_events.len();
            let type_args = self.parse_optional_type_argument_facts()?;
            let type_arguments = self.syntax_ranges_since(type_arguments_start, |data| {
                matches!(
                    data,
                    SyntaxData::TypeName { .. }
                        | SyntaxData::TypeReference { .. }
                        | SyntaxData::TypeContainer { .. }
                        | SyntaxData::FunctionType { .. }
                )
            });
            let syntax_data = SyntaxData::TraitBound {
                name,
                type_arguments,
            };
            self.record_typed_syntax_node(
                SyntaxKind::TraitBound,
                bound_start,
                self.current,
                syntax_data,
            );
            bounds.push(TraitBoundFact {
                span: bound_span,
                name: bound_name,
                type_arguments: type_args,
            });
            if !self.matches(&[TokenType::Plus]) {
                break;
            }
        }
        Ok(bounds)
    }

    fn parse_interface_body(
        &mut self,
        type_params: &[TypeParameterFact],
        start_span: Span,
    ) -> ParserResult<(Vec<FieldDeclarationFact>, Vec<InterfaceMethodFact>)> {
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

    fn parse_interface_member(
        &mut self,
        type_params: &[TypeParameterFact],
        start_span: Span,
        fields: &mut Vec<FieldDeclarationFact>,
        methods: &mut Vec<InterfaceMethodFact>,
    ) -> ParserResult<()> {
        match self.peek().token_type {
            TokenType::Func => {
                let method = self.parse_interface_method(start_span)?;
                methods.push(method);
            }
            TokenType::Id(_) | TokenType::Const => {
                let field = self.parse_field_declaration_fact(type_params)?;
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

    fn parse_interface_method(&mut self, start_span: Span) -> ParserResult<InterfaceMethodFact> {
        let function_start = self.current;
        self.consume();
        let name = self.consume_identifier_fact(
            "Expected method name",
            self.mode == ParserMode::Compatibility,
        )?;
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
        let where_clause = self.parse_where_clause_fact()?;
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
            .source_range()
            .expect("method return type span range");
        let end = self.current;
        let syntax_data = SyntaxData::Function {
            name: name_range,
            ast_span: start_span.byte_range.expect("interface method span range"),
            type_parameters,
            parameters,
            return_type: return_type_range,
            return_type_span,
            where_clause: where_range,
            body: None,
            is_common: false,
        };
        self.record_typed_syntax_node(
            SyntaxKind::FunctionDeclaration,
            function_start,
            end,
            syntax_data,
        );
        Ok(InterfaceMethodFact {
            span: start_span,
            name,
            type_params,
            params,
            return_type,
            where_clause,
        })
    }

    fn parse_simple_type_params(&mut self) -> ParserResult<Vec<TypeParameterFact>> {
        let start = self.current;
        if !self.matches(&[TokenType::Lt]) {
            return Ok(Vec::new());
        }
        let mut params = Vec::new();
        if !self.check(TokenType::Gt) {
            loop {
                let parameter_start = self.current;
                let parameter_span = self.peek().span;
                let param = self.consume_identifier_fact(
                    "Expected type parameter name",
                    self.mode == ParserMode::Compatibility,
                )?;
                let name = self
                    .previous()
                    .span
                    .byte_range
                    .expect("type parameter name range");
                let syntax_data = SyntaxData::TypeParameter {
                    name,
                    bounds: Vec::new(),
                };
                self.record_typed_syntax_node(
                    SyntaxKind::TypeParameter,
                    parameter_start,
                    self.current,
                    syntax_data,
                );
                params.push(TypeParameterFact {
                    range: self
                        .source_range_for_tokens(parameter_start, self.current)
                        .expect("method type parameter source range"),
                    span: parameter_span.combine(&self.previous().span),
                    name: param,
                    name_range: name,
                    bounds: Vec::new(),
                });
                if !self.matches(&[TokenType::Comma]) {
                    break;
                }
            }
        }
        self.consume_token(TokenType::Gt, "Expected '>' after type parameters")?;
        self.record_syntax_node(SyntaxKind::TypeArguments, start, self.current);
        Ok(params)
    }

    fn parse_param_list(&mut self) -> ParserResult<Vec<FunctionParameterFact>> {
        let mut params = Vec::new();
        if !self.check(TokenType::CloseParen) {
            loop {
                let parameter_start = self.current;
                let type_fact = self.parse_type_fact()?;
                let type_range = type_fact.source_range().expect("parameter type range");
                let param_name = self.consume_identifier_fact(
                    "Expected parameter name",
                    self.mode == ParserMode::Compatibility,
                )?;
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
                params.push(FunctionParameterFact {
                    name: param_name,
                    type_fact,
                    default_value: ParameterDefaultFact {
                        range: None,
                        expression: None,
                    },
                });
                if !self.matches(&[TokenType::Comma]) {
                    break;
                }
            }
        }
        Ok(params)
    }

    fn parse_optional_return_type(&mut self) -> ParserResult<TypeFact> {
        if self.matches(&[TokenType::Minus, TokenType::Gt]) || self.matches(&[TokenType::Returns]) {
            self.parse_type_fact()
        } else {
            Ok(TypeFact::void(self.peek().span))
        }
    }

    fn enum_declaration(&mut self) -> ParserResult<EnumDeclarationFact> {
        let enum_start = self.current;
        let start_span = self.tokens[self.current].span;
        self.consume_token(TokenType::Enum, "Expected 'enum' keyword")?;
        let name = self.consume_identifier_fact(
            "Expected enum name",
            self.mode == ParserMode::Compatibility,
        )?;
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
        let syntax_data = SyntaxData::Enum {
            name: name_range,
            type_parameters,
            variants: variant_ranges,
            ast_span: full_span.byte_range.expect("enum span source range"),
        };
        self.record_typed_syntax_node(
            SyntaxKind::EnumDeclaration,
            enum_start,
            self.current,
            syntax_data,
        );
        Ok(EnumDeclarationFact {
            range: self
                .source_range_for_tokens(enum_start, self.current)
                .expect("enum declaration source range"),
            span: full_span,
            name,
            type_params,
            variants,
        })
    }

    fn parse_colon_type_params(&mut self) -> ParserResult<Vec<TypeParameterFact>> {
        let start = self.current;
        if !self.matches(&[TokenType::Lt]) {
            return Ok(Vec::new());
        }
        let mut params = Vec::new();
        if !self.check(TokenType::Gt) {
            loop {
                let parameter_start = self.current;
                let parameter_span = self.peek().span;
                let param = self.consume_identifier_fact(
                    "Expected type parameter name",
                    self.mode == ParserMode::Compatibility,
                )?;
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
                let syntax_data = SyntaxData::TypeParameter {
                    name,
                    bounds: bound_ranges,
                };
                self.record_typed_syntax_node(
                    SyntaxKind::TypeParameter,
                    parameter_start,
                    self.current,
                    syntax_data,
                );
                params.push(TypeParameterFact {
                    range: self
                        .source_range_for_tokens(parameter_start, self.current)
                        .expect("enum type parameter source range"),
                    span: parameter_span.combine(&self.previous().span),
                    name: param,
                    name_range: name,
                    bounds,
                });
                if !self.matches(&[TokenType::Comma]) {
                    break;
                }
            }
        }
        self.consume_token(TokenType::Gt, "Expected '>' after type parameters")?;
        self.record_syntax_node(SyntaxKind::TypeArguments, start, self.current);
        Ok(params)
    }

    fn parse_enum_variants(&mut self) -> ParserResult<Vec<EnumVariantFact>> {
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

    fn parse_single_enum_variant(&mut self) -> ParserResult<EnumVariantFact> {
        let variant_start = self.current;
        let start_span = self.peek().span;
        let variant_name = self.consume_identifier_fact(
            "Expected variant name",
            self.mode == ParserMode::Compatibility,
        )?;
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
            self.parse_where_clause_fact()?
        } else {
            // Variants are newline-separated, so only a same-line `where`
            // belongs to this variant.
            None
        };
        let where_range = self.last_syntax_range_since(where_start, |data| {
            matches!(data, SyntaxData::WhereClause { .. })
        });
        let syntax_data = SyntaxData::EnumVariant {
            name,
            fields,
            where_clause: where_range,
        };
        let range = self
            .source_range_for_tokens(variant_start, self.current)
            .expect("enum variant source range");
        let end_span = self.previous().span;
        let span = start_span.combine(&end_span);
        self.record_typed_syntax_node(
            SyntaxKind::EnumVariant,
            variant_start,
            self.current,
            syntax_data,
        );
        Ok(EnumVariantFact {
            range,
            span,
            name: variant_name,
            data,
            where_clause,
        })
    }

    fn parse_enum_variant_data(&mut self) -> ParserResult<Option<EnumVariantDataFact>> {
        if !self.matches(&[TokenType::OpenParen]) {
            return Ok(None);
        }
        let mut fields = Vec::new();
        if !self.check(TokenType::CloseParen) {
            loop {
                let field_start = self.current;
                let field_type = self.parse_type_fact()?;
                let type_range = field_type
                    .source_range()
                    .expect("enum payload field type range");
                let has_field_name = matches!(&self.peek().token_type, TokenType::Id(_));
                let field_name = if let TokenType::Id(name) = &self.peek().token_type {
                    let compatibility_name =
                        (self.mode == ParserMode::Compatibility).then(|| name.clone());
                    self.advance();
                    compatibility_name
                } else {
                    None
                };
                let field_name_range = self.previous().span.byte_range.filter(|_| has_field_name);
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

    fn import_declaration(&mut self) -> ParserResult<ImportDeclarationFact> {
        let import_start = self.current;
        let start_span = self.consume_token(TokenType::Import, "Expected 'import' keyword")?;
        let path_start = self.current;
        let compatibility = self.mode == ParserMode::Compatibility;
        let module_path = self.parse_module_path_fact(compatibility)?;
        let path_end = self.current;
        let spec_start = self.current;
        let spec = self.parse_import_spec_fact(module_path.as_deref(), compatibility)?;
        let end_span = self.previous().span;
        let span = start_span.combine(&end_span);
        let module_path_range = self
            .source_range_for_tokens(path_start, path_end)
            .expect("import module path source range");
        let syntax_spec = self.import_syntax_spec(spec_start, self.current);
        let syntax_data = SyntaxData::Import {
            module_path: module_path_range,
            spec: syntax_spec,
            ast_span: span.byte_range.expect("import span range"),
        };
        self.record_typed_syntax_node(
            SyntaxKind::ImportDeclaration,
            import_start,
            self.current,
            syntax_data,
        );
        Ok(ImportDeclarationFact {
            range: self
                .source_range_for_tokens(import_start, self.current)
                .expect("import declaration source range"),
            span,
            module_path_range,
            module_path,
            spec,
        })
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

    fn parse_import_spec_fact(
        &mut self,
        module_path: Option<&str>,
        compatibility: bool,
    ) -> ParserResult<Option<ImportSpec>> {
        let spec = if self.matches(&[TokenType::Dot]) {
            self.parse_dot_import_spec_fact(compatibility)?
        } else {
            self.parse_module_import_spec_fact(module_path, compatibility)?
        };
        Ok(spec)
    }

    fn parse_dot_import_spec_fact(
        &mut self,
        compatibility: bool,
    ) -> ParserResult<Option<ImportSpec>> {
        if self.matches(&[TokenType::Star]) {
            Ok(if compatibility {
                Some(ImportSpec::Wildcard)
            } else {
                None
            })
        } else if self.matches(&[TokenType::OpenParen]) {
            self.parse_import_items_fact(compatibility)
        } else {
            let item =
                self.consume_identifier_fact("Expected item name after '.'", compatibility)?;
            let alias = if self.matches(&[TokenType::As]) {
                self.consume_identifier_fact("Expected alias after 'as'", compatibility)?
            } else {
                None
            };
            if compatibility {
                Ok(Some(ImportSpec::Item {
                    item: item.expect("compatibility import item"),
                    alias,
                }))
            } else {
                Ok(None)
            }
        }
    }

    fn parse_module_import_spec_fact(
        &mut self,
        module_path: Option<&str>,
        compatibility: bool,
    ) -> ParserResult<Option<ImportSpec>> {
        let alias = if self.matches(&[TokenType::As]) {
            let alias_name =
                self.consume_identifier_fact("Expected alias after 'as'", compatibility)?;
            alias_name.filter(|name| name != "_")
        } else if compatibility {
            Some(
                module_path
                    .expect("compatibility import path")
                    .split('.')
                    .next_back()
                    .unwrap_or(module_path.expect("compatibility import path"))
                    .to_string(),
            )
        } else {
            None
        };
        Ok(if compatibility {
            Some(ImportSpec::Module { alias })
        } else {
            None
        })
    }

    fn parse_import_items_fact(&mut self, compatibility: bool) -> ParserResult<Option<ImportSpec>> {
        let mut items = Vec::new();

        if !self.check(TokenType::CloseParen) {
            loop {
                let item = self.consume_identifier_fact("Expected item name", compatibility)?;
                let alias = if self.matches(&[TokenType::As]) {
                    self.consume_identifier_fact("Expected alias after 'as'", compatibility)?
                } else {
                    None
                };
                if compatibility {
                    items.push((item.expect("compatibility import item"), alias));
                }

                if !self.matches(&[TokenType::Comma]) {
                    break;
                }
                self.skip_newlines();
            }
        }

        self.consume_token(TokenType::CloseParen, "Expected ')' after import items")?;
        Ok(if compatibility {
            Some(ImportSpec::Items { items })
        } else {
            None
        })
    }

    pub(super) fn consume_identifier_fact(
        &mut self,
        error_msg: &str,
        compatibility: bool,
    ) -> ParserResult<Option<String>> {
        if self.is_at_end() {
            return Err(ParserError::new(
                DiagnosticCode::ParseExpectedToken,
                format!("{error_msg}, but reached end of file"),
                self.peek().span,
            ));
        }

        match &self.peek().token_type {
            TokenType::Id(name) => {
                let name = compatibility.then(|| name.clone());
                self.current += 1;
                Ok(name)
            }
            TokenType::Underscore => {
                let name = compatibility.then(|| "_".to_string());
                self.current += 1;
                Ok(name)
            }
            _ => {
                let found_desc = Self::describe_token(&self.peek().token_type);
                Err(ParserError::new(
                    DiagnosticCode::ParseExpectedToken,
                    format!("{error_msg}, found {found_desc}"),
                    self.peek().span,
                ))
            }
        }
    }

    // Parse module path with support for dots, relative (./, ../), and absolute (/)
    fn parse_module_path_fact(&mut self, compatibility: bool) -> ParserResult<Option<String>> {
        let mut module_path = compatibility.then(String::new);

        if self.matches(&[TokenType::Dot]) {
            self.consume_token(TokenType::Slash, "Expected '/' after '.'")?;
            if let Some(module_path) = &mut module_path {
                module_path.push_str("./");
            }
        } else if self.matches(&[TokenType::DotDot]) {
            self.consume_token(TokenType::Slash, "Expected '/' after '..'")?;
            if let Some(module_path) = &mut module_path {
                module_path.push_str("../");
            }
        } else if self.matches(&[TokenType::Slash])
            && let Some(module_path) = &mut module_path
        {
            module_path.push('/');
        }

        if let Some(module_path) = &mut module_path {
            module_path.push_str(
                &self
                    .consume_identifier_fact("Expected module path", true)?
                    .expect("compatibility module path component"),
            );
        } else {
            self.consume_identifier_fact("Expected module path", false)?;
        }

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
            if let Some(module_path) = &mut module_path {
                module_path.push('.');
                module_path.push_str(
                    &self
                        .consume_identifier_fact("Expected module name after '.'", true)?
                        .expect("compatibility module path component"),
                );
            } else {
                self.consume_identifier_fact("Expected module name after '.'", false)?;
            }
        }

        Ok(module_path)
    }

    pub(super) fn function_declaration(&mut self, is_common: bool) -> ParserResult<AstNode> {
        self.function_declaration_fact(is_common, true)
            .map(FunctionDeclarationFact::into_compatibility_ast)
    }

    pub(super) fn syntax_only_function_declaration(&mut self, is_common: bool) -> ParserResult<()> {
        self.function_declaration_fact(is_common, false).map(drop)
    }

    fn function_declaration_fact(
        &mut self,
        is_common: bool,
        retain_name_for_validation: bool,
    ) -> ParserResult<FunctionDeclarationFact> {
        let function_start = self.current;
        let start_span = self.peek().span;
        self.consume_token(TokenType::Func, "Expected 'func' keyword")?;
        let name = self.consume_identifier_fact(
            "Expected function name",
            retain_name_for_validation || self.mode == ParserMode::Compatibility,
        )?;
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
        let where_clause = self.parse_where_clause_fact()?;
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
            .source_range()
            .expect("function return type span range");
        self.skip_newlines();
        let body_event_start = self.syntax_events.len();
        let body_statements = self.parse_function_body(start_span)?;
        let body_range = self.last_statement_range_since(body_event_start);
        let end_span = body_statements.last().map_or(start_span, |s| s.span);
        let span = start_span.combine(&end_span);
        let ast_span = span.byte_range.expect("function span range");
        let function_end = self.current;
        let syntax_data = SyntaxData::Function {
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
            syntax_data,
        );
        Ok(FunctionDeclarationFact {
            range: self
                .source_range_for_tokens(function_start, function_end)
                .expect("function declaration source range"),
            span,
            name,
            type_params,
            params,
            return_type,
            body: body_statements,
            is_common,
            where_clause,
        })
    }

    fn parse_is_type_params(&mut self) -> ParserResult<Vec<TypeParameterFact>> {
        let start = self.current;
        if !self.matches(&[TokenType::Lt]) {
            return Ok(Vec::new());
        }
        let mut params = Vec::new();
        if !self.check(TokenType::Gt) {
            loop {
                let parameter_start = self.current;
                let parameter_span = self.peek().span;
                let param = self.consume_identifier_fact(
                    "Expected type parameter name",
                    self.mode == ParserMode::Compatibility,
                )?;
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
                let syntax_data = SyntaxData::TypeParameter {
                    name: name_range,
                    bounds: bound_ranges,
                };
                self.record_typed_syntax_node(
                    SyntaxKind::TypeParameter,
                    parameter_start,
                    self.current,
                    syntax_data,
                );
                params.push(TypeParameterFact {
                    range: self
                        .source_range_for_tokens(parameter_start, self.current)
                        .expect("function type parameter source range"),
                    span: parameter_span.combine(&self.previous().span),
                    name: param,
                    name_range,
                    bounds: trait_bounds,
                });
                if !self.matches(&[TokenType::Comma]) {
                    break;
                }
            }
        }
        self.consume_token(TokenType::Gt, "Expected '>' after type parameters")?;
        self.record_syntax_node(SyntaxKind::TypeArguments, start, self.current);
        Ok(params)
    }

    fn parse_ref_trait_bounds(&mut self) -> ParserResult<Vec<TraitBoundFact>> {
        if !self.matches(&[TokenType::Is]) {
            return Ok(Vec::new());
        }
        let mut bounds = Vec::new();
        loop {
            let bound_start = self.current;
            let bound_name = self.consume_identifier_fact(
                "Expected trait name in bound",
                self.mode == ParserMode::Compatibility,
            )?;
            let bound_span = self.previous().span;
            let name = bound_span.byte_range.expect("trait bound name range");
            let type_arguments_start = self.syntax_events.len();
            let type_args = self.parse_optional_type_argument_facts()?;
            let type_arguments = self.syntax_ranges_since(type_arguments_start, |data| {
                matches!(
                    data,
                    SyntaxData::TypeName { .. }
                        | SyntaxData::TypeReference { .. }
                        | SyntaxData::TypeContainer { .. }
                        | SyntaxData::FunctionType { .. }
                )
            });
            let syntax_data = SyntaxData::TraitBound {
                name,
                type_arguments,
            };
            self.record_typed_syntax_node(
                SyntaxKind::TraitBound,
                bound_start,
                self.current,
                syntax_data,
            );
            bounds.push(TraitBoundFact {
                span: bound_span,
                name: bound_name,
                type_arguments: type_args,
            });
            if !self.matches(&[TokenType::Ref]) {
                break;
            }
        }
        Ok(bounds)
    }

    fn parse_function_params(&mut self) -> ParserResult<Vec<FunctionParameterFact>> {
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

    fn parse_single_param(
        &mut self,
        has_default: &mut bool,
    ) -> ParserResult<FunctionParameterFact> {
        let parameter_start = self.current;
        let type_fact = self.parse_type_fact()?;
        let type_range = type_fact.source_range().expect("parameter type range");
        let param_name = self.consume_identifier_fact(
            "Expected parameter name",
            self.mode == ParserMode::Compatibility,
        )?;
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
            SyntaxData::Parameter {
                name,
                type_range,
                default_value: default_value.range,
            },
        );
        Ok(FunctionParameterFact {
            name: param_name,
            type_fact,
            default_value,
        })
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
            return Ok(ParameterDefaultFact {
                range: None,
                expression: None,
            });
        }
        let default_expr = self.parse_expression_parsed()?;
        let is_literal = match self.mode {
            ParserMode::SyntaxOnly => {
                self.is_literal_expression_range(default_expr.span.byte_range)
            }
            ParserMode::Compatibility => default_expr
                .node
                .as_ref()
                .is_some_and(|expr| matches!(expr.kind, ExpressionKind::Literal(_))),
        };
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
        let expression = default_expr.node;
        Ok(ParameterDefaultFact { range, expression })
    }

    /// If the next non-newline token is `token_type`, consume the intervening
    /// newlines and return true; otherwise leave them unconsumed and return
    /// false. Lets a clause continue on a following line without eating
    /// newlines that separate declarations.
    fn parse_field_declaration_fact(
        &mut self,
        type_param_names: &[TypeParameterFact],
    ) -> ParserResult<FieldDeclarationFact> {
        let field_start = self.current;
        let start_span = self.peek().span;
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
        let field_name = self.consume_identifier_fact(
            "Expected field name",
            self.mode == ParserMode::Compatibility,
        )?;
        let name = self
            .previous()
            .span
            .byte_range
            .expect("class field name source range");

        // Check for optional default value. Any expression is allowed; it is
        // evaluated per instance when the constructor (`.new()`) runs, and its
        // type is checked against the field in semantic analysis.
        let (default_value, default_value_range, has_default_value) =
            if self.matches(&[TokenType::Eq]) {
                let value = self.parse_expression_parsed()?;
                let value_range = value.span.byte_range;
                let has_default_value = true;
                let value = value.node;
                (value, value_range, has_default_value)
            } else {
                (None, None, false)
            };

        // For const fields, require a default value even when syntax parsing
        // discards its compatibility expression node.
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
        let where_clause = if self.check(TokenType::Where) {
            // Fields are newline-separated, so only a same-line `where`
            // belongs to this field.
            self.parse_where_clause_fact()?
        } else {
            None
        };
        let where_range = self.last_syntax_range_since(where_start, |data| {
            matches!(data, SyntaxData::WhereClause { .. })
        });
        let syntax_data = SyntaxData::Field {
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
            syntax_data,
        );
        Ok(FieldDeclarationFact {
            range: self
                .source_range_for_tokens(field_start, self.current)
                .expect("field declaration source range"),
            span: start_span.combine(&self.previous().span),
            name: field_name,
            type_fact: field_type,
            is_generic_param,
            is_const,
            default_value,
            where_clause,
        })
    }

    fn is_field_generic_param(
        &self,
        field_type: &TypeFact,
        type_param_names: &[TypeParameterFact],
    ) -> bool {
        let Some(type_name_range) = field_type.generic_parameter_name_range() else {
            return false;
        };
        let Some(type_name) = self.identifier_at_range(type_name_range) else {
            return false;
        };
        type_param_names
            .iter()
            .any(|parameter| self.identifier_at_range(parameter.name_range) == Some(type_name))
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

    fn is_literal_expression_range(&self, range: Option<ByteRange>) -> bool {
        range.is_some_and(|range| {
            self.syntax_events.iter().any(|event| {
                event.range == range
                    && matches!(event.data.as_ref(), Some(SyntaxData::Literal { .. }))
            })
        })
    }
}
