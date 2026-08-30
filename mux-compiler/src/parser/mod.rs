//! Parser module for the Mux language.
//!
//! This module contains the parser which converts a stream of tokens into an AST.

mod error;

pub use error::{ParserError, ParserResult};

use crate::ast::{
    AstNode, BinaryOp, EnumVariant, EnumVariantField, ExpressionKind, ExpressionNode, Field,
    FunctionNode, ImportSpec, LiteralNode, MatchArm, Param, PatternNode, Precedence, PrimitiveType,
    SpanExt, Spanned, StatementKind, StatementNode, TraitBound, TraitRef, TypeKind, TypeNode,
    UnaryOp, WhereClause,
};
use crate::diagnostic::DiagnosticCode;
use crate::lexer::{Span, Token, TokenType};

#[derive(Debug)]
pub struct Parser<'a> {
    tokens: Vec<&'a Token>,
    current: usize,
    pub errors: Vec<ParserError>,
    recovery_spans: Vec<Span>,
    loop_depth: usize,
    stopped: bool,
}

/// Why `name` cannot be declared as a class method, if it cannot.
///
/// The compiler synthesizes these on every class - `new` builds an instance,
/// the deserializers build one from a document - so a user-declared method of
/// the same name would be silently replaced by the synthesized one. Rejecting
/// it at the declaration says so, rather than letting a method that looks
/// defined behave as something else entirely.
fn reserved_class_method_error(name: &str) -> Option<String> {
    let purpose = match name {
        "new" => "class constructors",
        "from_json" => "building an instance from a JSON object",
        "list_from_json" => "building a list of instances from a JSON array",
        "list_from_csv" => "building a list of instances from CSV rows",
        _ => return None,
    };
    Some(format!(
        "'{name}' is reserved for {purpose} and cannot be defined as a class method"
    ))
}

impl<'a> Parser<'a> {
    #[must_use]
    pub fn new(tokens: &'a [Token]) -> Self {
        Self {
            tokens: tokens
                .iter()
                .filter(|&token| {
                    !matches!(
                        token.token_type,
                        TokenType::LineComment(_) | TokenType::MultilineComment(_)
                    )
                })
                .collect(),
            current: 0,
            errors: Vec::new(),
            recovery_spans: Vec::new(),
            loop_depth: 0,
            stopped: false,
        }
    }

    const MAX_ERRORS: usize = 100;

    fn record_error(&mut self, error: ParserError) {
        if self.stopped {
            return;
        }
        if self.errors.len() + 1 == Self::MAX_ERRORS {
            self.errors.push(ParserError::new(
                DiagnosticCode::ParseErrorLimit,
                "too many parse errors; recovery stopped",
                error.span,
            ));
            self.stopped = true;
        } else {
            self.errors.push(error);
        }
    }

    fn advance(&mut self) -> &Token {
        if !self.is_at_end() {
            self.current += 1;
        }
        self.previous()
    }

    fn previous(&self) -> &Token {
        self.tokens[(self.current - 1).max(0)]
    }

    fn is_statement_starter(&self, token_type: &TokenType) -> bool {
        matches!(
            token_type,
            TokenType::Auto
                | TokenType::Const
                | TokenType::Func
                | TokenType::Class
                | TokenType::Interface
                | TokenType::Enum
                | TokenType::If
                | TokenType::While
                | TokenType::For
                | TokenType::Match
                | TokenType::Break
                | TokenType::Continue
                | TokenType::Return
                | TokenType::OpenBrace
                | TokenType::CloseBrace
        )
    }

    fn check_statement_termination(&self) -> ParserResult<()> {
        if self.current >= self.tokens.len() {
            return Ok(());
        }

        let token = &self.tokens[self.current];

        // statements can end at eof, closing braces, or newlines
        if matches!(
            token.token_type,
            TokenType::CloseBrace | TokenType::Eof | TokenType::NewLine
        ) {
            return Ok(());
        }

        // check if next token starts a new statement
        if self.current + 1 < self.tokens.len() {
            let next_token = &self.tokens[self.current + 1];
            if self.is_statement_starter(&next_token.token_type)
                && !matches!(token.token_type, TokenType::NewLine)
                && next_token.token_type != TokenType::Eof
            {
                return Err(ParserError::new(
                    DiagnosticCode::ParseExpectedToken,
                    "Expected newline before statement".to_string(),
                    next_token.span,
                ));
            }
        }
        Ok(())
    }

    pub fn parse(&mut self) -> Result<Vec<AstNode>, (Vec<AstNode>, Vec<ParserError>)> {
        let mut nodes = Vec::new();
        while !self.is_at_end() && !self.stopped {
            if self.is_at_end() {
                break;
            }

            let start_position = self.current;

            match self.declaration() {
                Ok(Some(decl)) => {
                    nodes.push(decl);

                    // check statement termination for top-level, non-control flow statements.
                    let should_check_termination = matches!(
                        nodes.last().expect("nodes should not be empty after push"),
                        AstNode::Statement(stmt) if !matches!(
                            stmt.kind,
                            StatementKind::If { .. } | StatementKind::While { .. } |
                            StatementKind::For { .. } | StatementKind::Match { .. } |
                            StatementKind::Block(_)
                        )
                    );

                    if should_check_termination && let Err(e) = self.check_statement_termination() {
                        self.record_error(e);
                        self.synchronize();
                    }

                    let _ = self.skip_newlines();
                }
                Ok(None) => {
                    if self.current == start_position {
                        self.advance();
                    }
                }
                Err(e) => {
                    self.record_error(e);
                    self.synchronize();
                }
            }

            let _ = self.skip_newlines();
        }

        let all_errors = std::mem::take(&mut self.errors);
        if all_errors.is_empty() {
            Ok(nodes)
        } else {
            Err((nodes, all_errors))
        }
    }

    /// Spans consumed while skipping malformed input during recovery.
    ///
    /// Callers use these intervals to suppress diagnostics derived from
    /// tokens the parser deliberately discarded.
    #[must_use]
    pub fn recovery_spans(&self) -> &[Span] {
        &self.recovery_spans
    }

    fn declaration(&mut self) -> ParserResult<Option<AstNode>> {
        loop {
            let _ = self.skip_newlines();
            if self.is_at_end() {
                return Ok(None);
            }

            let start_position = self.current;
            let result = self.parse_declaration_content();

            while self.matches(&[TokenType::NewLine]) {}

            if let Err(ref e) = result {
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

            return result;
        }
    }

    fn parse_declaration_content(&mut self) -> ParserResult<Option<AstNode>> {
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
        } else if self.check(TokenType::Import) {
            self.import_declaration().map(Some)
        } else {
            self.statement().map(Some)
        }
    }

    fn parse_id_start_declaration(&mut self) -> ParserResult<Option<AstNode>> {
        let start = self.current;
        if self.parse_type().is_ok() {
            self.parse_typed_or_statement(start)
        } else {
            self.current = start;
            self.statement().map(Some)
        }
    }

    fn parse_typed_or_statement(&mut self, start: usize) -> ParserResult<Option<AstNode>> {
        if let TokenType::Id(_) = &self.peek().token_type {
            let next = self.current + 1;
            if next < self.tokens.len() && self.tokens[next].token_type == TokenType::Eq {
                self.current = start;
                self.parse_typed_declaration_with_recovery()
            } else {
                self.current = start;
                self.statement().map(Some)
            }
        } else {
            self.current = start;
            self.statement().map(Some)
        }
    }

    fn parse_typed_declaration_with_recovery(&mut self) -> ParserResult<Option<AstNode>> {
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

    fn auto_declaration(&mut self) -> ParserResult<AstNode> {
        let start_span = self.peek().span;
        self.current += 1;

        let name = self.consume_identifier("Expected variable name after 'auto'")?;
        let name_span = self.tokens[self.current - 1].span;

        self.consume_token(TokenType::Eq, "Expected '=' after variable name")?;
        let value = self.parse_expression()?;

        // Validate that postfix ++ and -- don't appear in declarations
        self.check_no_postfix_increment_decrement(&value)?;

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

        Ok(AstNode::Statement(StatementNode {
            kind: StatementKind::AutoDecl(
                name,
                TypeNode {
                    kind: TypeKind::Auto,
                    span: name_span,
                },
                value,
            ),
            span,
        }))
    }

    fn const_declaration(&mut self) -> ParserResult<AstNode> {
        let start_span = self.peek().span;
        self.current += 1;

        let type_node = self.parse_type()?;
        let name = self.consume_identifier("Expected constant name after type")?;

        self.consume_token(TokenType::Eq, "Expected '=' after constant name")?;
        let value = self.parse_expression()?;

        // Validate that postfix ++ and -- don't appear in declarations
        self.check_no_postfix_increment_decrement(&value)?;

        let span = start_span.combine(&value.span);

        Ok(AstNode::Statement(StatementNode {
            kind: StatementKind::ConstDecl(name, type_node, value),
            span,
        }))
    }

    fn typed_declaration(&mut self) -> ParserResult<AstNode> {
        let start_span = self.peek().span;
        let type_node = self.parse_type()?;
        let name = self.consume_identifier("Expected variable name after type")?;

        // `Type name` with no initializer. Deciding here, after the name, is
        // what lets an initialized and an uninitialized declaration share one
        // entry point (#393).
        if !self.check(TokenType::Eq) {
            let name_span = self.tokens[self.current - 1].span;
            return Ok(AstNode::Statement(StatementNode {
                kind: StatementKind::UninitDecl(name, type_node),
                span: start_span.combine(&name_span),
            }));
        }

        self.consume_token(TokenType::Eq, "Expected '=' after variable name")?;
        let value = self.parse_expression()?;

        // Validate that postfix ++ and -- don't appear in declarations
        self.check_no_postfix_increment_decrement(&value)?;

        let span = start_span.combine(&value.span);
        Ok(AstNode::Statement(StatementNode {
            kind: StatementKind::TypedDecl(name, type_node, value),
            span,
        }))
    }

    fn class_declaration(&mut self) -> ParserResult<AstNode> {
        let start_span = self.tokens[self.current].span;
        self.consume_token(TokenType::Class, "Expected 'class' keyword")?;
        let name = self.consume_identifier("Expected class name")?;
        let type_params = self.parse_type_params_list()?;
        let traits = self.parse_trait_list()?;
        self.consume_token(TokenType::OpenBrace, "Expected '{' after class header")?;
        let (fields, methods) = self.parse_class_body(&type_params, start_span)?;
        let end_span =
            self.consume_token(TokenType::CloseBrace, "Expected '}' after class body")?;
        let where_clause = self.parse_where_clause()?;
        let full_span = match &where_clause {
            Some(clause) => start_span.combine(&clause.span),
            None => start_span.combine(&end_span),
        };
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

    fn parse_type_params_list(&mut self) -> ParserResult<Vec<(String, Vec<TraitBound>)>> {
        if !self.matches(&[TokenType::Lt]) {
            return Ok(Vec::new());
        }
        let mut params = Vec::new();
        loop {
            let param = self.consume_identifier("Expected type parameter name")?;
            let bounds = self.parse_trait_bounds()?;
            params.push((param, bounds));
            if !self.matches(&[TokenType::Comma]) {
                break;
            }
        }
        self.consume_token(TokenType::Gt, "Expected '>' after type parameters")?;
        Ok(params)
    }

    fn parse_trait_bounds(&mut self) -> ParserResult<Vec<TraitBound>> {
        if !self.matches(&[TokenType::Is]) {
            return Ok(Vec::new());
        }
        let mut bounds = Vec::new();
        loop {
            let bound_name = self.consume_identifier("Expected trait name in bound")?;
            let bound_span = self.previous().span;
            let type_args = self.parse_optional_type_args()?;
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

    fn parse_trait_list(&mut self) -> ParserResult<Vec<TraitRef>> {
        if !self.matches(&[TokenType::Is]) {
            return Ok(Vec::new());
        }
        let mut traits_list = Vec::new();
        loop {
            let trait_name = self.consume_identifier("Expected trait name")?;
            let trait_span = self.previous().span;
            let type_args = self.parse_optional_type_args()?;
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

    fn parse_optional_type_args(&mut self) -> ParserResult<Vec<TypeNode>> {
        if self.matches(&[TokenType::Lt]) {
            let args = self.parse_type_arguments()?;
            self.consume_token(TokenType::Gt, "Expected '>' after type arguments")?;
            Ok(args)
        } else {
            Ok(Vec::new())
        }
    }

    fn parse_class_body(
        &mut self,
        type_params: &[(String, Vec<TraitBound>)],
        start_span: Span,
    ) -> ParserResult<(Vec<Field>, Vec<FunctionNode>)> {
        let mut fields = Vec::new();
        let mut methods = Vec::new();
        while !self.check(TokenType::CloseBrace) && !self.is_at_end() {
            let member_start = self.current;
            // Recover within the class body so a malformed member does not abort
            // the whole class and leave '}' to be misparsed as a stray top-level
            // token (issue #288). Skipping from the member start treats the member
            // as a balanced unit, so inner braces - a collection default or a
            // method body - are consumed with their matching open and never
            // mistaken for the class terminator.
            if let Err(e) =
                self.parse_class_member(type_params, start_span, &mut fields, &mut methods)
            {
                self.record_error(e);
                self.current = member_start;
                self.recover_class_member();
                if self.current == member_start {
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
    fn recover_class_member(&mut self) {
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
        type_params: &[(String, Vec<TraitBound>)],
        start_span: Span,
        fields: &mut Vec<Field>,
        methods: &mut Vec<FunctionNode>,
    ) -> ParserResult<()> {
        match self.peek().token_type {
            TokenType::Func => {
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
                    methods.push(func);
                } else {
                    return Err(ParserError::new(
                        DiagnosticCode::ParseExpectedToken,
                        "Expected function in class",
                        start_span,
                    ));
                }
            }
            TokenType::Common => {
                self.consume();
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
                    methods.push(func);
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

    fn interface_declaration(&mut self) -> ParserResult<AstNode> {
        let start_span = self.tokens[self.current].span;
        self.consume_token(TokenType::Interface, "Expected 'interface' keyword")?;
        let name = self.consume_identifier("Expected interface name")?;
        let type_params = self.parse_interface_type_params()?;
        self.consume_token(TokenType::OpenBrace, "Expected '{' after interface header")?;
        let (fields, methods) = self.parse_interface_body(&type_params, start_span)?;
        let end_span =
            self.consume_token(TokenType::CloseBrace, "Expected '}' after interface body")?;
        let full_span = start_span.combine(&end_span);
        Ok(AstNode::Interface {
            name,
            type_params,
            fields,
            methods,
            span: full_span,
        })
    }

    fn parse_interface_type_params(&mut self) -> ParserResult<Vec<(String, Vec<TraitBound>)>> {
        if !self.matches(&[TokenType::Lt]) {
            return Ok(Vec::new());
        }
        let mut params = Vec::new();
        if !self.check(TokenType::Gt) {
            loop {
                let param = self.consume_identifier("Expected type parameter name")?;
                let bounds = self.parse_colon_trait_bounds()?;
                params.push((param, bounds));
                if !self.matches(&[TokenType::Comma]) {
                    break;
                }
            }
        }
        self.consume_token(TokenType::Gt, "Expected '>' after type parameters")?;
        Ok(params)
    }

    fn parse_colon_trait_bounds(&mut self) -> ParserResult<Vec<TraitBound>> {
        if !self.matches(&[TokenType::Colon]) {
            return Ok(Vec::new());
        }
        let mut bounds = Vec::new();
        loop {
            let bound_name = self.consume_identifier("Expected trait name in bound")?;
            let bound_span = self.previous().span;
            let type_args = self.parse_optional_type_args()?;
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

    fn parse_interface_body(
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

    fn parse_interface_member(
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

    fn parse_interface_method(&mut self, start_span: Span) -> ParserResult<FunctionNode> {
        self.consume();
        let name = self.consume_identifier("Expected method name")?;
        let type_params = self.parse_simple_type_params()?;
        self.consume_token(TokenType::OpenParen, "Expected '(' after method name")?;
        let params = self.parse_param_list()?;
        self.consume_token(TokenType::CloseParen, "Expected ')' after parameters")?;
        let where_clause = self.parse_where_clause()?;
        if where_clause.is_some() {
            // The return type may continue on the next line after the block,
            // but do not eat the newline separating this member from the next.
            self.skip_newlines_before(TokenType::Returns);
        }
        let return_type = self.parse_optional_return_type()?;
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

    fn parse_simple_type_params(&mut self) -> ParserResult<Vec<(String, Vec<TraitBound>)>> {
        if !self.matches(&[TokenType::Lt]) {
            return Ok(Vec::new());
        }
        let mut params = Vec::new();
        if !self.check(TokenType::Gt) {
            loop {
                let param = self.consume_identifier("Expected type parameter name")?;
                params.push((param, Vec::new()));
                if !self.matches(&[TokenType::Comma]) {
                    break;
                }
            }
        }
        self.consume_token(TokenType::Gt, "Expected '>' after type parameters")?;
        Ok(params)
    }

    fn parse_param_list(&mut self) -> ParserResult<Vec<Param>> {
        let mut params = Vec::new();
        if !self.check(TokenType::CloseParen) {
            loop {
                let param_type = self.parse_type()?;
                let param_name = self.consume_identifier("Expected parameter name")?;
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

    fn parse_optional_return_type(&mut self) -> ParserResult<TypeNode> {
        if self.matches(&[TokenType::Minus, TokenType::Gt]) || self.matches(&[TokenType::Returns]) {
            self.parse_type()
        } else {
            Ok(TypeNode {
                kind: TypeKind::Primitive(PrimitiveType::Void),
                span: self.peek().span,
            })
        }
    }

    fn enum_declaration(&mut self) -> ParserResult<AstNode> {
        let start_span = self.tokens[self.current].span;
        self.consume_token(TokenType::Enum, "Expected 'enum' keyword")?;
        let name = self.consume_identifier("Expected enum name")?;
        let type_params = self.parse_colon_type_params()?;
        self.consume_token(TokenType::OpenBrace, "Expected '{' after enum header")?;
        let variants = self.parse_enum_variants()?;
        let end_span =
            self.consume_token(TokenType::CloseBrace, "Expected '}' after enum variants")?;
        let full_span = start_span.combine(&end_span);
        Ok(AstNode::Enum {
            name,
            type_params,
            variants,
            span: full_span,
        })
    }

    fn parse_colon_type_params(&mut self) -> ParserResult<Vec<(String, Vec<TraitBound>)>> {
        if !self.matches(&[TokenType::Lt]) {
            return Ok(Vec::new());
        }
        let mut params = Vec::new();
        if !self.check(TokenType::Gt) {
            loop {
                let param = self.consume_identifier("Expected type parameter name")?;
                let bounds = self.parse_colon_trait_bounds()?;
                params.push((param, bounds));
                if !self.matches(&[TokenType::Comma]) {
                    break;
                }
            }
        }
        self.consume_token(TokenType::Gt, "Expected '>' after type parameters")?;
        Ok(params)
    }

    fn parse_enum_variants(&mut self) -> ParserResult<Vec<EnumVariant>> {
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

    fn parse_single_enum_variant(&mut self) -> ParserResult<EnumVariant> {
        let variant_name = self.consume_identifier("Expected variant name")?;
        let data = self.parse_enum_variant_data()?;
        let where_clause = if self.check(TokenType::Where) {
            self.parse_where_clause()?
        } else {
            // Variants are newline-separated, so only a same-line `where`
            // belongs to this variant.
            None
        };
        Ok(EnumVariant {
            name: variant_name,
            data,
            where_clause,
        })
    }

    fn parse_enum_variant_data(&mut self) -> ParserResult<Option<Vec<EnumVariantField>>> {
        if !self.matches(&[TokenType::OpenParen]) {
            return Ok(None);
        }
        let mut fields = Vec::new();
        if !self.check(TokenType::CloseParen) {
            loop {
                let field_type = self.parse_type()?;
                let field_name = if let TokenType::Id(name) = self.peek().token_type.clone() {
                    self.advance();
                    Some(name)
                } else {
                    None
                };
                fields.push((field_name, field_type));
                if !self.matches(&[TokenType::Comma]) {
                    break;
                }
            }
        }
        self.consume_token(TokenType::CloseParen, "Expected ')' after variant data")?;
        Ok(Some(fields))
    }

    fn import_declaration(&mut self) -> ParserResult<AstNode> {
        let start_span = self.consume_token(TokenType::Import, "Expected 'import' keyword")?;
        let module_path = self.parse_module_path()?;
        let spec = self.parse_import_spec(&module_path)?;
        let end_span = self.previous().span;
        Ok(AstNode::Statement(StatementNode {
            kind: StatementKind::Import { module_path, spec },
            span: start_span.combine(&end_span),
        }))
    }

    /// Extracted: Parses the import spec following an import path.
    fn parse_import_spec(&mut self, module_path: &str) -> ParserResult<ImportSpec> {
        if self.matches(&[TokenType::Dot]) {
            self.parse_dot_import_spec()
        } else {
            self.parse_module_import_spec(module_path)
        }
    }

    /// Extracted: Handles dot import: .*, .(items), .item (with optional alias)
    fn parse_dot_import_spec(&mut self) -> ParserResult<ImportSpec> {
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
    fn parse_module_import_spec(&mut self, module_path: &str) -> ParserResult<ImportSpec> {
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
    fn parse_module_path(&mut self) -> ParserResult<String> {
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
    fn parse_import_items(&mut self) -> ParserResult<ImportSpec> {
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

    // Look ahead n tokens without consuming
    fn peek_ahead(&self, n: usize) -> Option<&Token> {
        self.tokens.get(self.current + n).copied()
    }

    fn function_declaration(&mut self, is_common: bool) -> ParserResult<AstNode> {
        let start_span = self.peek().span;
        self.consume_token(TokenType::Func, "Expected 'func' keyword")?;
        let name = self.consume_identifier("Expected function name")?;
        let type_params = self.parse_is_type_params()?;
        self.consume_token(TokenType::OpenParen, "Expected '(' after function name")?;
        let params = self.parse_function_params()?;
        self.consume_token(TokenType::CloseParen, "Expected ')' after parameters")?;
        let where_clause = self.parse_where_clause()?;
        if where_clause.is_some() {
            self.skip_newlines();
        }
        let return_type = self.parse_required_return_type()?;
        self.skip_newlines();
        let body_statements = self.parse_function_body(start_span)?;
        let end_span = body_statements.last().map_or(start_span, |s| s.span);
        let span = start_span.combine(&end_span);
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

    fn parse_is_type_params(&mut self) -> ParserResult<Vec<(String, Vec<TraitBound>)>> {
        if !self.matches(&[TokenType::Lt]) {
            return Ok(Vec::new());
        }
        let mut params = Vec::new();
        if !self.check(TokenType::Gt) {
            loop {
                let param = self.consume_identifier("Expected type parameter name")?;
                let bounds = self.parse_ref_trait_bounds()?;
                params.push((param, bounds));
                if !self.matches(&[TokenType::Comma]) {
                    break;
                }
            }
        }
        self.consume_token(TokenType::Gt, "Expected '>' after type parameters")?;
        Ok(params)
    }

    fn parse_ref_trait_bounds(&mut self) -> ParserResult<Vec<TraitBound>> {
        if !self.matches(&[TokenType::Is]) {
            return Ok(Vec::new());
        }
        let mut bounds = Vec::new();
        loop {
            let bound_name = self.consume_identifier("Expected trait name in bound")?;
            let bound_span = self.previous().span;
            let type_args = self.parse_optional_type_args()?;
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

    fn parse_function_params(&mut self) -> ParserResult<Vec<Param>> {
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

    fn parse_single_param(&mut self, has_default: &mut bool) -> ParserResult<Param> {
        let param_type = self.parse_type()?;
        let param_name = self.consume_identifier("Expected parameter name")?;
        let default_value = self.parse_param_default(has_default)?;
        Ok(Param {
            name: param_name,
            type_: param_type,
            default_value,
        })
    }

    fn parse_param_default(
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
    fn skip_newlines_before(&mut self, token_type: TokenType) -> bool {
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
    fn parse_where_clause(&mut self) -> ParserResult<Option<WhereClause>> {
        if !self.skip_newlines_before(TokenType::Where) {
            return Ok(None);
        }
        let start_span = self.peek().span;
        self.consume_token(TokenType::Where, "Expected 'where' keyword")?;
        self.skip_newlines();
        self.consume_token(TokenType::OpenBrace, "Expected '{' after 'where'")?;
        let mut predicates = Vec::new();
        loop {
            self.skip_newlines();
            if self.check(TokenType::CloseBrace) {
                break;
            }
            predicates.push(self.parse_expression()?);
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
        if predicates.is_empty() {
            return Err(ParserError::with_help(
                DiagnosticCode::ParseExpectedToken,
                "Empty 'where' block",
                span,
                "A where block must contain at least one boolean predicate. Example: where { value > 0 }",
            ));
        }
        Ok(Some(WhereClause { predicates, span }))
    }

    fn parse_required_return_type(&mut self) -> ParserResult<TypeNode> {
        if self.matches(&[TokenType::Returns]) {
            self.parse_type()
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

    fn parse_function_body(&mut self, start_span: Span) -> ParserResult<Vec<StatementNode>> {
        let body = self.block()?;
        match body {
            AstNode::Statement(StatementNode {
                kind: StatementKind::Block(block),
                ..
            }) => Ok(block),
            _ => Err(ParserError::new(
                DiagnosticCode::ParseExpectedToken,
                "Expected block statement for function body",
                start_span,
            )),
        }
    }

    fn statement(&mut self) -> ParserResult<AstNode> {
        let result = if self.matches(&[TokenType::If]) {
            self.if_statement()
        } else if self.matches(&[TokenType::While]) {
            self.while_statement()
        } else if self.matches(&[TokenType::For]) {
            self.for_statement()
        } else if self.matches(&[TokenType::Match]) {
            self.match_statement()
        } else if self.matches(&[TokenType::Break]) {
            self.break_statement()
        } else if self.matches(&[TokenType::Continue]) {
            self.continue_statement()
        } else if self.matches(&[TokenType::Return]) {
            self.return_statement()
        } else if self.matches(&[TokenType::Func]) {
            self.function_declaration(false)
        } else if self.looks_like_typed_decl() {
            self.typed_declaration()
        } else if self.check(TokenType::OpenBrace) {
            self.block()
        } else {
            self.expression_statement()
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

    fn looks_like_typed_decl(&self) -> bool {
        let n = self.tokens.len();
        let i = self.current;

        if let Some(j) = self.skip_type(i)
            && j < n
            && let TokenType::Id(_) = self.tokens[j].token_type
            && j + 1 < n
        {
            return match &self.tokens[j + 1].token_type {
                TokenType::Eq => true,
                // `Type name` with no initializer (#393). A type followed by a
                // name followed by the end of the statement is unambiguously a
                // declaration - Mux has no expression form where two
                // identifiers sit side by side.
                TokenType::NewLine | TokenType::CloseBrace | TokenType::Eof => true,
                _ => false,
            };
        }
        false
    }

    fn skip_type(&self, mut i: usize) -> Option<usize> {
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

    fn skip_function_type(&self, mut i: usize, n: usize) -> Option<usize> {
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

    fn skip_identifier_type(&self, mut i: usize, n: usize) -> Option<usize> {
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

    fn if_statement(&mut self) -> ParserResult<AstNode> {
        let start_span = self.tokens[self.current - 1].span;
        let condition = self.parse_expression()?;
        self.check_no_postfix_increment_decrement(&condition)?;
        self.skip_newlines();

        // Parse then block using the block() function directly
        let then_block = match self.block()? {
            AstNode::Statement(StatementNode {
                kind: StatementKind::Block(block),
                ..
            }) => block,
            _ => {
                return Err(ParserError::new(
                    DiagnosticCode::ParseExpectedToken,
                    "Expected block after if condition",
                    self.peek().span,
                ));
            }
        };

        self.skip_newlines();
        let (else_block, end_span) = self.parse_else_branch(start_span, &then_block)?;
        let span = start_span.combine(&end_span);
        Ok(AstNode::Statement(StatementNode {
            kind: StatementKind::If {
                cond: condition,
                then_block,
                else_block,
            },
            span,
        }))
    }

    fn parse_else_branch(
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

    fn parse_else_if_branch(&mut self) -> ParserResult<(Option<Vec<StatementNode>>, Span)> {
        let nested = self.if_statement()?;
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

    fn parse_else_block(&mut self) -> ParserResult<(Option<Vec<StatementNode>>, Span)> {
        // Check for opening brace first with proper error message
        if !self.check(TokenType::OpenBrace) {
            return Err(ParserError::new(
                DiagnosticCode::ParseExpectedToken,
                "Expected '{' after else",
                self.peek().span,
            ));
        }
        // Parse the else block using block() which handles the opening brace
        let else_block = match self.block()? {
            AstNode::Statement(StatementNode {
                kind: StatementKind::Block(block),
                ..
            }) => block,
            _ => {
                return Err(ParserError::new(
                    DiagnosticCode::ParseExpectedToken,
                    "Expected block after else",
                    self.peek().span,
                ));
            }
        };
        let end_span = else_block
            .last()
            .map_or_else(|| self.tokens[self.current - 1].span, |s| s.span);
        Ok((Some(else_block), end_span))
    }

    fn while_statement(&mut self) -> ParserResult<AstNode> {
        let start_span = self.tokens[self.current].span;
        let condition = self.parse_expression()?;

        // Validate that postfix ++ and -- don't appear in condition
        self.check_no_postfix_increment_decrement(&condition)?;

        // allow newline(s) before body.
        self.skip_newlines();
        self.loop_depth += 1;
        let body = self.block()?;
        self.loop_depth -= 1;

        let body_statements = match body {
            AstNode::Statement(stmt) => match stmt.kind {
                StatementKind::Block(block) => block,
                _ => vec![stmt],
            },
            _ => {
                return Err(ParserError::new(
                    DiagnosticCode::ParseExpectedToken,
                    "Expected statement after while condition",
                    start_span,
                ));
            }
        };

        let end_span = body_statements.last().map_or(start_span, |s| s.span);
        let span = start_span.combine(&end_span);

        Ok(AstNode::Statement(StatementNode {
            kind: StatementKind::While {
                cond: condition,
                body: body_statements,
            },
            span,
        }))
    }

    fn for_statement(&mut self) -> ParserResult<AstNode> {
        let start_span = self.tokens[self.current].span;

        // parse the variable type.
        let var_type = self.parse_type()?;

        let var = self.consume_identifier("Expected variable name")?;
        self.consume_token(TokenType::In, "Expected 'in' after variable")?;
        let iter = self.parse_expression()?;

        // Validate that postfix ++ and -- don't appear in iterator expression
        self.check_no_postfix_increment_decrement(&iter)?;

        // allow newline(s) before body.
        self.skip_newlines();
        self.loop_depth += 1;
        let body = if self.check(TokenType::OpenBrace) {
            self.block()?
        } else {
            self.statement()?
        };
        self.loop_depth -= 1;

        let body_statements = match body {
            AstNode::Statement(stmt) => match stmt.kind {
                StatementKind::Block(block) => block,
                _ => vec![stmt],
            },
            _ => {
                return Err(ParserError::new(
                    DiagnosticCode::ParseExpectedToken,
                    "Expected statement after for loop",
                    start_span,
                ));
            }
        };

        let end_span = body_statements.last().map_or(start_span, |s| s.span);
        let span = start_span.combine(&end_span);
        Ok(AstNode::Statement(StatementNode {
            kind: StatementKind::For {
                var,
                var_type,
                iter,
                body: body_statements,
            },
            span,
        }))
    }

    fn match_statement(&mut self) -> ParserResult<AstNode> {
        let start_span = self.tokens[self.current].span;
        let expr = self.parse_expression()?;

        // Validate that postfix ++ and -- don't appear in match expression
        self.check_no_postfix_increment_decrement(&expr)?;

        self.consume_token(TokenType::OpenBrace, "Expected '{' after match expression")?;
        self.skip_newlines();

        let mut arms = Vec::new();
        while !self.check(TokenType::CloseBrace) && !self.is_at_end() {
            arms.push(self.parse_match_arm(start_span)?);
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

        Ok(AstNode::Statement(StatementNode {
            kind: StatementKind::Match { expr, arms },
            span: start_span.combine(&end_span),
        }))
    }

    /// Helper: Parses a single match arm, including pattern, optional guard, and arm body.
    fn parse_match_arm(&mut self, start_span: Span) -> ParserResult<MatchArm> {
        let pattern = self.parse_pattern()?;
        let guard = if self.matches(&[TokenType::If]) {
            Some(self.parse_expression()?)
        } else {
            None
        };
        self.skip_newlines();
        let body = self.parse_match_arm_body(start_span)?;
        Ok(MatchArm {
            pattern,
            guard,
            body,
        })
    }

    /// Helper: Parses the body of a match arm, returning a vector of statement nodes.
    fn parse_match_arm_body(&mut self, start_span: Span) -> ParserResult<Vec<StatementNode>> {
        let node = if self.check(TokenType::OpenBrace) {
            self.block()?
        } else if self.matches(&[TokenType::Colon]) {
            self.skip_newlines();
            if self.check(TokenType::OpenBrace) {
                self.block()?
            } else {
                self.statement()?
            }
        } else {
            self.statement()?
        };
        match node {
            AstNode::Statement(stmt) => match stmt.kind {
                StatementKind::Block(block) => Ok(block),
                _ => Ok(vec![stmt]),
            },
            _ => Err(ParserError::new(
                DiagnosticCode::ParseExpectedToken,
                "Expected statement for match arm body",
                start_span,
            )),
        }
    }

    fn parse_pattern(&mut self) -> ParserResult<PatternNode> {
        match &self.peek().token_type {
            TokenType::None => {
                self.current += 1; // consume none
                Ok(PatternNode::EnumVariant {
                    name: "none".to_string(),
                    args: vec![],
                })
            }
            TokenType::Id(name) => {
                let name_clone = name.clone();
                self.current += 1; // consume the identifier
                self.parse_pattern_identifier_or_variant(name_clone)
            }
            TokenType::Underscore => {
                self.current += 1; // consume the underscore
                Ok(PatternNode::Wildcard)
            }
            TokenType::OpenBracket => self.parse_list_pattern(),

            _ => self.parse_literal_pattern(),
        }
    }

    fn return_statement(&mut self) -> ParserResult<AstNode> {
        let start_span = self.tokens[self.current].span;

        // Check if there's an expression after return
        let value = if self.is_at_end()
            || self.check(TokenType::NewLine)
            || self.check(TokenType::CloseBrace)
        {
            // return at end of input, or followed by newline/closing brace - void return
            None
        } else {
            let expr = self.parse_expression()?;

            // Validate that postfix ++ and -- don't appear in return value
            self.check_no_postfix_increment_decrement(&expr)?;

            Some(expr)
        };

        let end_span = value.as_ref().map_or(start_span, |v| v.span);

        Ok(AstNode::Statement(StatementNode {
            kind: StatementKind::Return(value),
            span: start_span.combine(&end_span),
        }))
    }

    fn break_statement(&mut self) -> ParserResult<AstNode> {
        let start_span = self.tokens[self.current].span;

        if self.loop_depth == 0 {
            return Err(ParserError::with_help(
                DiagnosticCode::ParseControlFlowOutsideLoop,
                "Cannot use 'break' outside of a loop",
                start_span,
                "'break' can only be used inside a 'for' or 'while' loop",
            ));
        }

        Ok(AstNode::Statement(StatementNode {
            kind: StatementKind::Break,
            span: start_span,
        }))
    }

    fn continue_statement(&mut self) -> ParserResult<AstNode> {
        let start_span = self.tokens[self.current].span;

        if self.loop_depth == 0 {
            return Err(ParserError::with_help(
                DiagnosticCode::ParseControlFlowOutsideLoop,
                "Cannot use 'continue' outside of a loop",
                start_span,
                "'continue' can only be used inside a 'for' or 'while' loop",
            ));
        }

        Ok(AstNode::Statement(StatementNode {
            kind: StatementKind::Continue,
            span: start_span,
        }))
    }

    fn skip_newlines(&mut self) -> usize {
        let mut count = 0;
        while self.matches(&[TokenType::NewLine]) {
            count += 1;
        }
        count
    }

    fn block(&mut self) -> ParserResult<AstNode> {
        let start_span = self.consume_token(TokenType::OpenBrace, "Expected '{' before block")?;
        self.skip_newlines();

        if self.matches(&[TokenType::CloseBrace]) {
            return Ok(AstNode::Statement(StatementNode {
                kind: StatementKind::Block(Vec::new()),
                span: start_span.combine(&self.previous().span),
            }));
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
            .filter_map(super::ast::nodes::AstNode::into_statement)
            .collect();

        Ok(AstNode::Statement(StatementNode {
            kind: StatementKind::Block(stmts),
            span: start_span.combine(&end_span),
        }))
    }

    fn parse_block_statements_loop(&mut self) -> ParserResult<Vec<AstNode>> {
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

    fn parse_block_statement(&mut self, statements: &mut Vec<AstNode>) -> ParserResult<()> {
        match self.declaration() {
            Ok(Some(decl)) => {
                statements.push(decl);
                self.skip_newlines();
            }
            Ok(None) => {
                // `declaration` parsed nothing (e.g. it recovered from an error and
                // stopped at the block's closing brace). Advance only when there is
                // a real token to skip, so the block loop can see '}' and finish
                // rather than consuming it and running past the block (issue #288).
                if !self.check(TokenType::CloseBrace) && !self.is_at_end() {
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

    fn is_in_block(&self) -> bool {
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

    fn expression_statement(&mut self) -> ParserResult<AstNode> {
        let expr = self.parse_expression()?;
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
        let span = *expr.span();

        // Validate that postfix ++ and -- only appear at statement level
        self.validate_postfix_in_statement(&expr)?;

        Ok(AstNode::Statement(StatementNode {
            kind: StatementKind::Expression(expr),
            span,
        }))
    }

    fn validate_postfix_in_statement(&self, expr: &ExpressionNode) -> ParserResult<()> {
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

    #[allow(clippy::only_used_in_recursion)]
    fn check_no_postfix_increment_decrement(&self, expr: &ExpressionNode) -> ParserResult<()> {
        self.check_no_postfix_increment_decrement_kind(&expr.kind, expr.span)
    }

    fn check_no_postfix_increment_decrement_kind(
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

    fn check_unary_postfix_nesting(
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

    fn check_no_postfix_increment_decrement_all(
        &self,
        expressions: &[ExpressionNode],
    ) -> ParserResult<()> {
        for expression in expressions {
            self.check_no_postfix_increment_decrement(expression)?;
        }
        Ok(())
    }

    fn check_no_postfix_increment_decrement_map_entries(
        &self,
        entries: &[(ExpressionNode, ExpressionNode)],
    ) -> ParserResult<()> {
        for (key, value) in entries {
            self.check_no_postfix_increment_decrement(key)?;
            self.check_no_postfix_increment_decrement(value)?;
        }
        Ok(())
    }

    fn check_no_postfix_increment_decrement_lambda_body(
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

    fn parse_named_type_with_builtin_support(
        &mut self,
        mut name: String,
        start_span: Span,
    ) -> ParserResult<TypeNode> {
        // A module-qualified type, `graph.Graph<string>`. This is the form
        // `import std.dsa.*` naturally leads you to write - the same path that
        // already works in expression position - and it was rejected outright
        // in a type position, so a function could not take or return one (#391).
        //
        // A type is never followed by a field access, so a dot here is
        // unambiguous and consuming it greedily costs nothing. The qualifier is
        // kept in the name and resolved by the analyzer, which is the only
        // place that knows which module namespaces are in scope.
        while self.check(TokenType::Dot) {
            self.current += 1;
            let segment = self.consume_identifier("Expected a type name after '.'")?;
            name.push('.');
            name.push_str(&segment);
        }

        if let Ok(prim_type) =
            PrimitiveType::parse(&Token::new(TokenType::Id(name.clone()), start_span))
        {
            return Ok(TypeNode {
                kind: TypeKind::Primitive(prim_type),
                span: start_span,
            });
        }

        if let Some(node) = self.parse_container_type(&name, start_span)? {
            return Ok(node);
        }

        let type_args = if self.matches(&[TokenType::Lt]) {
            let args = self.parse_type_arguments()?;
            self.consume_token(TokenType::Gt, "Expected '>' after type arguments")?;
            args
        } else {
            Vec::new()
        };

        if name == "dyn" && !type_args.is_empty() {
            return Ok(TypeNode {
                kind: TypeKind::TraitObject(Box::new(type_args[0].clone())),
                span: start_span,
            });
        }

        Ok(TypeNode {
            kind: TypeKind::Named(name, type_args),
            span: start_span,
        })
    }

    fn parse_container_type(
        &mut self,
        name: &str,
        start_span: Span,
    ) -> ParserResult<Option<TypeNode>> {
        if !matches!(name, "list" | "map" | "set" | "tuple") {
            return Ok(None);
        }

        if !self.matches(&[TokenType::Lt]) {
            return Ok(None);
        }

        let node = match name {
            "list" => {
                let element_type = self.parse_type()?;
                self.consume_token(TokenType::Gt, "Expected '>' after list element type")?;
                TypeNode {
                    kind: TypeKind::List(Box::new(element_type)),
                    span: start_span,
                }
            }
            "map" => {
                let key_type = self.parse_type()?;
                self.consume_token(
                    TokenType::Comma,
                    "Expected ',' between key and value types in map",
                )?;
                let value_type = self.parse_type()?;
                self.consume_token(TokenType::Gt, "Expected '>' after map value type")?;
                TypeNode {
                    kind: TypeKind::Map(Box::new(key_type), Box::new(value_type)),
                    span: start_span,
                }
            }
            "set" => {
                let element_type = self.parse_type()?;
                self.consume_token(TokenType::Gt, "Expected '>' after set element type")?;
                TypeNode {
                    kind: TypeKind::Set(Box::new(element_type)),
                    span: start_span,
                }
            }
            "tuple" => {
                let left_type = self.parse_type()?;
                self.consume_token(TokenType::Comma, "Expected ',' in tuple type")?;
                let right_type = self.parse_type()?;
                self.consume_token(TokenType::Gt, "Expected '>' after tuple type")?;
                TypeNode {
                    kind: TypeKind::Tuple(Box::new(left_type), Box::new(right_type)),
                    span: start_span,
                }
            }
            _ => unreachable!("guarded by matches!"),
        };

        Ok(Some(node))
    }

    fn parse_pattern_identifier_or_variant(&mut self, name: String) -> ParserResult<PatternNode> {
        if self.matches(&[TokenType::OpenParen]) {
            let mut args = Vec::new();
            if !self.check(TokenType::CloseParen) {
                loop {
                    args.push(self.parse_pattern()?);
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
            return Ok(PatternNode::EnumVariant { name, args });
        }

        Ok(PatternNode::Identifier(name))
    }

    fn parse_list_pattern(&mut self) -> ParserResult<PatternNode> {
        self.current += 1; // consume '['
        self.skip_newlines();
        let mut elements = Vec::new();
        let mut rest = None;

        if !self.check(TokenType::CloseBracket) {
            loop {
                self.skip_newlines();
                if self.check(TokenType::DotDot) {
                    self.current += 1; // consume '..'
                    rest = Some(Box::new(self.parse_pattern()?));
                    self.skip_newlines();
                    break;
                }
                elements.push(self.parse_pattern()?);
                self.skip_newlines();
                if !self.matches(&[TokenType::Comma]) {
                    break;
                }
                self.skip_newlines();
            }
        }

        self.consume_token(TokenType::CloseBracket, "Expected ']' after list pattern")?;
        Ok(PatternNode::List { elements, rest })
    }

    fn parse_literal_pattern(&mut self) -> ParserResult<PatternNode> {
        let token = self.consume();
        match &token.token_type {
            TokenType::Int(n) => Ok(PatternNode::Literal(LiteralNode::Integer(*n))),
            TokenType::Float(f) => Ok(PatternNode::Literal(LiteralNode::Float(*f))),
            TokenType::Bool(b) => Ok(PatternNode::Literal(LiteralNode::Boolean(*b))),
            TokenType::Char(c) => Ok(PatternNode::Literal(LiteralNode::Char(*c))),
            TokenType::Str(s) => Ok(PatternNode::Literal(LiteralNode::String(s.clone()))),
            _ => Err(ParserError::from_token(
                DiagnosticCode::InvalidPattern,
                "Expected pattern",
                token,
            )),
        }
    }

    fn parse_type(&mut self) -> ParserResult<TypeNode> {
        if self.matches(&[TokenType::Ref]) {
            let start_span = self.previous().span;
            let referenced_type = self.parse_type()?;
            return Ok(TypeNode {
                kind: TypeKind::Reference(Box::new(referenced_type)),
                span: Span {
                    row_start: start_span.row_start,
                    col_start: start_span.col_start,
                    row_end: self.previous().span.row_end,
                    col_end: self.previous().span.col_end,
                },
            });
        }

        // we are essentially doing a consume here, but without borrowing the parser again so we dont have to clone it.
        let token = &self.tokens[self.current];
        let start_span = token.span;
        self.current += 1;

        match token.token_type {
            TokenType::Id(ref name) => {
                self.parse_named_type_with_builtin_support(name.clone(), start_span)
            }

            TokenType::Func => {
                self.consume_token(
                    TokenType::OpenParen,
                    "Expected '(' after 'func' in function type",
                )?;
                let mut param_types = Vec::new();

                if !self.check(TokenType::CloseParen) {
                    loop {
                        // Parse parameter types only (no parameter names for function types)
                        param_types.push(self.parse_type()?);

                        if !self.matches(&[TokenType::Comma]) {
                            break;
                        }
                        self.skip_newlines();
                    }
                }

                self.consume_token(TokenType::CloseParen, "Expected ')' after parameter types")?;
                self.consume_token(TokenType::Returns, "Expected 'returns' in function type")?;

                let return_type = Box::new(self.parse_type()?);

                Ok(TypeNode {
                    kind: TypeKind::Function {
                        params: param_types,
                        returns: return_type,
                    },
                    span: start_span,
                })
            }
            _ => Err(ParserError::from_token(
                DiagnosticCode::ParseExpectedType,
                "Expected type",
                token,
            )),
        }
    }

    fn parse_type_arguments(&mut self) -> ParserResult<Vec<TypeNode>> {
        let mut args = Vec::new();
        while !self.check(TokenType::Gt) && !self.is_at_end() {
            let arg = self.parse_type()?;
            args.push(arg);

            if !self.matches(&[TokenType::Comma]) {
                break;
            }
            self.skip_newlines();
        }
        Ok(args)
    }

    fn synchronize(&mut self) {
        let start = self.current;
        while !self.is_at_end() {
            match self.peek().token_type {
                TokenType::NewLine => {
                    self.current += 1;
                    break;
                }
                TokenType::Func
                | TokenType::Auto
                | TokenType::Const
                | TokenType::Class
                | TokenType::Interface
                | TokenType::Enum
                | TokenType::Import => {
                    break;
                }
                TokenType::If
                | TokenType::Else
                | TokenType::While
                | TokenType::For
                | TokenType::Match
                | TokenType::Return => {
                    break;
                }
                TokenType::OpenBrace | TokenType::CloseBrace => {
                    break;
                }
                TokenType::OpenParen | TokenType::CloseParen => {
                    break;
                }
                TokenType::OpenBracket | TokenType::CloseBracket => {
                    break;
                }
                _ => {
                    self.current += 1;
                }
            }
        }
        if self.current > start {
            let span = self.tokens[start]
                .span
                .combine(&self.tokens[self.current - 1].span);
            self.recovery_spans.push(span);
        }
    }

    #[must_use]
    pub fn is_at_end(&self) -> bool {
        self.current >= self.tokens.len()
    }

    fn peek(&self) -> &Token {
        if self.is_at_end() {
            // Return the last token if available, otherwise use a default EOF token
            // This prevents "line 0" errors
            if let Some(last_token) = self.tokens.last() {
                last_token
            } else {
                static EOF_TOKEN: Token = Token {
                    token_type: TokenType::Eof,
                    span: Span {
                        row_start: 1,
                        row_end: None,
                        col_start: 1,
                        col_end: None,
                    },
                };
                &EOF_TOKEN
            }
        } else {
            self.tokens[self.current]
        }
    }

    fn consume(&mut self) -> &Token {
        let ret = &self.tokens[self.current];
        self.current += 1;
        ret
    }

    pub fn parse_expression(&mut self) -> ParserResult<ExpressionNode> {
        self.parse_precedence(Precedence::Assignment)
    }

    fn parse_precedence(&mut self, min_precedence: Precedence) -> ParserResult<ExpressionNode> {
        let left = self.parse_unary()?;
        let mut value = Box::new(left);

        // important, do not consume the operator until after checking precedence.
        // otherwise we may consume a lower-precedence operator in a recursive call and lose it.
        while let Some(op_token) = self.peek_operator() {
            let op_precedence = self.get_operator_precedence(&op_token)?;
            if op_precedence < min_precedence {
                break;
            }

            // now it is safe to consume the operator
            let _ = self.consume_operator();

            let next_precedence = if op_token.is_right_associative() {
                op_precedence
            } else {
                op_precedence.next_higher()
            };

            let right = self.parse_precedence(next_precedence)?;

            let left_span = *value.span();
            let right_span = *right.span();
            let left_expr = value.clone();
            let op_span = self.previous().span;
            let new_value = ExpressionNode {
                kind: ExpressionKind::Binary {
                    left: left_expr,
                    op: op_token,
                    op_span,
                    right: Box::new(right),
                },
                span: left_span.combine(&right_span),
            };
            *value = new_value;
        }

        Ok(*value)
    }

    fn parse_unary(&mut self) -> ParserResult<ExpressionNode> {
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
            Ok(ExpressionNode {
                kind: ExpressionKind::Unary {
                    op: UnaryOp::parse(&op_token)?,
                    op_span: op_token.span,
                    expr: Box::new(expr),
                    postfix: false,
                },
                span: op_token.span.combine(&expr_span),
            })
        } else {
            let expr = self.parse_primary()?;
            self.parse_postfix_operators(expr)
        }
    }

    fn parse_collection_literal(&mut self, start_span: Span) -> ParserResult<ExpressionNode> {
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

    fn parse_empty_set(&mut self, start_span: Span) -> ParserResult<ExpressionNode> {
        let end_span =
            self.consume_token(TokenType::CloseBrace, "Expected '}' after collection")?;
        Ok(ExpressionNode {
            kind: ExpressionKind::SetLiteral(vec![]),
            span: start_span.combine(&end_span),
        })
    }

    fn parse_empty_map(&mut self, start_span: Span) -> ParserResult<ExpressionNode> {
        self.consume_token(TokenType::Colon, "Expected ':' in empty map literal")?;
        self.skip_newlines();
        let end_span = self.consume_token(TokenType::CloseBrace, "Expected '}' after '{:'")?;
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

    fn parse_map_literal(
        &mut self,
        start_span: Span,
        first_key: ExpressionNode,
    ) -> ParserResult<ExpressionNode> {
        let first_value = self.parse_expression()?;
        let mut entries = vec![(first_key, first_value)];
        self.parse_collection_entries(&mut entries, true)?;
        let end_span =
            self.consume_token(TokenType::CloseBrace, "Expected '}' after collection")?;
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

    fn parse_set_literal(
        &mut self,
        start_span: Span,
        first_elem: ExpressionNode,
    ) -> ParserResult<ExpressionNode> {
        let mut elements = vec![first_elem];
        self.parse_set_entries(&mut elements)?;
        let end_span =
            self.consume_token(TokenType::CloseBrace, "Expected '}' after collection")?;
        Ok(ExpressionNode {
            kind: ExpressionKind::SetLiteral(elements),
            span: start_span.combine(&end_span),
        })
    }

    fn parse_set_entries(&mut self, elements: &mut Vec<ExpressionNode>) -> ParserResult<()> {
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

    fn parse_collection_entries(
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

    fn has_comma_or_entry(&mut self) -> ParserResult<bool> {
        if self.matches(&[TokenType::Comma]) {
            self.skip_newlines();
            if self.check(TokenType::CloseBrace) {
                return Ok(false);
            }
            return Ok(true);
        }
        self.skip_newlines_after_comma()
    }

    fn skip_newlines_after_comma(&mut self) -> ParserResult<bool> {
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
                self.current += 1;
            }
            self.skip_newlines();
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn parse_parenthesized_or_tuple_expression(
        &mut self,
        start_span: Span,
    ) -> ParserResult<ExpressionNode> {
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
            self.skip_newlines();
            self.consume_token(TokenType::CloseParen, "Expected ')' after tuple elements")?;

            let tuple_expr = ExpressionNode {
                kind: ExpressionKind::TupleLiteral(vec![first_expr, second_expr]),
                span: start_span.combine(&self.previous().span),
            };
            return self.parse_postfix_operators(tuple_expr);
        }

        self.consume_token(TokenType::CloseParen, "Expected ')' after expression")?;
        self.parse_postfix_operators(first_expr)
    }

    fn parse_list_literal_expression(&mut self, start_span: Span) -> ParserResult<ExpressionNode> {
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

        let expr = ExpressionNode {
            kind: ExpressionKind::ListLiteral(elements),
            span: start_span.combine(&self.consume_list_literal_close_span()?),
        };
        self.parse_postfix_operators(expr)
    }

    fn consume_list_literal_close_span(&mut self) -> ParserResult<Span> {
        self.consume_token(TokenType::CloseBracket, "Expected ']' after list elements")
    }

    fn consume_newline_delimited_comma(&mut self) -> bool {
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
            self.current += 1;
        }
        self.skip_newlines();
        true
    }

    fn parse_lambda_expression(&mut self, start_span: Span) -> ParserResult<ExpressionNode> {
        self.consume_token(TokenType::OpenParen, "Expected '(' after 'func' in lambda")?;
        let mut params = Vec::new();

        if !self.check(TokenType::CloseParen) {
            loop {
                let param_type = self.parse_type()?;
                let param_name = self.consume_identifier("Expected parameter name")?;
                if self.matches(&[TokenType::Eq]) {
                    return Err(ParserError::with_help(
                        DiagnosticCode::ParseExpectedToken,
                        "Default arguments are not supported in lambda expressions",
                        self.previous().span,
                        "Lambda parameters cannot have default values. Define a named function instead if you need default parameters.",
                    ));
                }
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

        self.consume_token(TokenType::CloseParen, "Expected ')' after parameters")?;

        let where_clause = self.parse_where_clause()?;
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

        let body = self.block()?;
        let body_statements = match body {
            AstNode::Statement(stmt) => match stmt.kind {
                StatementKind::Block(block) => block,
                _ => vec![stmt],
            },
            _ => {
                return Err(ParserError::new(
                    DiagnosticCode::ParseExpectedToken,
                    "Expected block statement for lambda body",
                    start_span,
                ));
            }
        };

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

        self.parse_postfix_operators(expr)
    }

    fn parse_if_expression(&mut self, token_span: Span) -> ParserResult<ExpressionNode> {
        let cond = self.parse_expression()?;
        let then_expr = self.parse_if_branch_expression("then expression")?;
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
        let span = token_span.combine(&self.previous().span);
        Ok(ExpressionNode {
            kind: ExpressionKind::If {
                cond: Box::new(cond),
                then_expr: Box::new(then_expr),
                else_expr: Box::new(else_expr),
            },
            span,
        })
    }

    /// Parse one branch of an if-expression: `{ <expr> }`. Newlines are allowed
    /// around the value expression so a branch can span multiple lines; the branch
    /// is still a single value, not a statement block.
    fn parse_if_branch_expression(&mut self, what: &str) -> ParserResult<ExpressionNode> {
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

    fn parse_postfixed_primary(
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

    fn unexpected_primary_error(&self, token_type: TokenType, token_span: Span) -> ParserError {
        let token_desc = Self::describe_token(&token_type);

        if matches!(token_type, TokenType::Match) {
            return ParserError::with_help(
                DiagnosticCode::ParseExpectedToken,
                "match cannot be used as an expression; it can only be used as a statement",
                token_span,
                "Use 'match' as a standalone statement with 'return' in each arm, or use an if/else expression for inline conditionals.",
            );
        }

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

    fn parse_primary(&mut self) -> ParserResult<ExpressionNode> {
        if self.is_at_end() {
            // Use the last token's span to show where we expected an expression
            let error_span = self.tokens.last().map_or_else(
                || Span {
                    row_start: 1,
                    row_end: None,
                    col_start: 1,
                    col_end: None,
                },
                |t| t.span,
            );
            return Err(ParserError::new(
                DiagnosticCode::ParseExpectedExpression,
                "Expected expression, found end of input",
                error_span,
            ));
        }

        let token = self.consume();
        let token_type = token.token_type.clone();
        let token_span = token.span;

        match token_type {
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

            TokenType::Float(f) => self.parse_postfixed_primary(
                ExpressionKind::Literal(LiteralNode::Float(f)),
                token_span,
            ),

            TokenType::OpenBracket => self.parse_list_literal_expression(token_span),

            TokenType::OpenBrace => self.parse_collection_literal(token_span),

            TokenType::Func => self.parse_lambda_expression(token_span),

            TokenType::If => self.parse_if_expression(token_span),

            TokenType::Id(id) => {
                // defer handling of '<' to binary operator parsing or parse_postfix_operators, which can disambiguate generics more safely.
                self.parse_postfixed_primary(ExpressionKind::Identifier(id.clone()), token_span)
            }

            _ => Err(self.unexpected_primary_error(token_type, token_span)),
        }
    }

    fn parse_call_postfix(&mut self, expr: ExpressionNode) -> ParserResult<ExpressionNode> {
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
        Ok(ExpressionNode {
            kind: ExpressionKind::Call {
                func: Box::new(expr),
                args,
            },
            span: expr_span.combine(&end_span),
        })
    }

    fn parse_field_access_postfix(&mut self, expr: ExpressionNode) -> ParserResult<ExpressionNode> {
        let field = self.consume_identifier("Expected field name after '.'")?;
        let expr_span = *expr.span();
        let field_span = self.tokens[self.current - 1].span;
        Ok(ExpressionNode {
            kind: ExpressionKind::FieldAccess {
                expr: Box::new(expr),
                field,
            },
            span: expr_span.combine(&field_span),
        })
    }

    fn parse_index_postfix(&mut self, expr: ExpressionNode) -> ParserResult<ExpressionNode> {
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
        Ok(ExpressionNode {
            kind: ExpressionKind::ListAccess {
                expr: Box::new(expr),
                index: Box::new(index),
            },
            span: expr_span.combine(&end_span),
        })
    }

    /// Finish `xs[start:` - the colon is consumed, the end bound is optional.
    fn finish_slice(
        &mut self,
        expr: ExpressionNode,
        start: Option<ExpressionNode>,
    ) -> ParserResult<ExpressionNode> {
        let end = if self.check(TokenType::CloseBracket) {
            None
        } else {
            Some(Box::new(self.parse_expression()?))
        };
        let end_span = self.consume_token(TokenType::CloseBracket, "Expected ']' after slice")?;
        let expr_span = *expr.span();
        Ok(ExpressionNode {
            kind: ExpressionKind::Slice {
                expr: Box::new(expr),
                start: start.map(Box::new),
                end,
            },
            span: expr_span.combine(&end_span),
        })
    }

    fn should_consume_generic_type_args(&self, expr: &ExpressionNode) -> Option<(String, bool)> {
        let generic_target_name = self.generic_target_name(expr)?;

        let gt_idx = self.find_matching_generic_gt_index()?;
        let should_consume = self.should_consume_generics_for_target(&generic_target_name, gt_idx);

        Some((generic_target_name, should_consume))
    }

    fn generic_target_name(&self, expr: &ExpressionNode) -> Option<String> {
        match &expr.kind {
            ExpressionKind::Identifier(id) => Some(id.clone()),
            ExpressionKind::FieldAccess { expr, field } => {
                if let ExpressionKind::Identifier(base) = &expr.kind {
                    Some(format!("{base}.{field}"))
                } else {
                    Some(field.clone())
                }
            }
            _ => None,
        }
    }

    fn find_matching_generic_gt_index(&self) -> Option<usize> {
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

    fn should_consume_generics_for_target(&self, generic_target_name: &str, gt_idx: usize) -> bool {
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

    fn parse_generic_postfix(
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
        Ok(Some(ExpressionNode {
            kind: ExpressionKind::GenericType(name, type_args),
            span: expr.span.combine(&end_span),
        }))
    }

    fn parse_postfix_update(&mut self, expr: ExpressionNode, op: UnaryOp) -> ExpressionNode {
        let expr_span = *expr.span();
        let op_span = self.previous().span;
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

    fn skip_newline_gap_for_postfix(&mut self) -> bool {
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
                    self.current = lookahead;
                    return true;
                }
                _ => {}
            }
        }

        false
    }

    fn try_parse_postfix_operator(
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

    fn parse_postfix_operators(
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

    fn matches(&mut self, types: &[TokenType]) -> bool {
        if self.is_at_end() {
            return false;
        }

        let token = self.peek();
        for ty in types {
            if token.token_type == *ty {
                if token.token_type == TokenType::Eof {
                    return false;
                }
                self.current += 1;
                return true;
            }
        }
        false
    }

    fn check(&self, ty: TokenType) -> bool {
        if self.is_at_end() {
            return false;
        }
        self.peek().token_type == ty
    }

    fn consume_token(&mut self, expected: TokenType, error_msg: &str) -> ParserResult<Span> {
        if self.is_at_end() {
            return Err(ParserError::new(
                DiagnosticCode::ParseExpectedToken,
                format!("{error_msg}, but reached end of file"),
                self.tokens.last().map_or_else(
                    || Span {
                        row_start: 1,
                        row_end: None,
                        col_start: 1,
                        col_end: None,
                    },
                    |t| t.span,
                ),
            ));
        }

        let token = &self.tokens[self.current];
        if token.token_type == expected {
            self.current += 1;
            Ok(token.span)
        } else {
            let found_desc = Self::describe_token(&token.token_type);
            Err(ParserError::new(
                DiagnosticCode::ParseExpectedToken,
                format!("{error_msg}, found {found_desc}"),
                token.span,
            ))
        }
    }

    fn consume_identifier(&mut self, error_msg: &str) -> ParserResult<String> {
        if self.is_at_end() {
            return Err(ParserError::new(
                DiagnosticCode::ParseExpectedToken,
                format!("{error_msg}, but reached end of file"),
                self.peek().span,
            ));
        }

        match &self.peek().token_type {
            TokenType::Id(name) => {
                let name_clone = name.clone();
                self.current += 1;
                Ok(name_clone)
            }
            TokenType::Underscore => {
                let name_clone = "_".to_string();
                self.current += 1;
                Ok(name_clone)
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

    /// Format a human-readable description of a token type for use in error messages.
    fn describe_token(token_type: &TokenType) -> String {
        match token_type {
            TokenType::Auto => "'auto' keyword".to_string(),
            TokenType::Func => "'func' keyword".to_string(),
            TokenType::Returns => "'returns' keyword".to_string(),
            TokenType::Return => "'return' keyword".to_string(),
            TokenType::Class => "'class' keyword".to_string(),
            TokenType::Interface => "'interface' keyword".to_string(),
            TokenType::Enum => "'enum' keyword".to_string(),
            TokenType::If => "'if' keyword".to_string(),
            TokenType::Else => "'else' keyword".to_string(),
            TokenType::For => "'for' keyword".to_string(),
            TokenType::While => "'while' keyword".to_string(),
            TokenType::Match => "'match' keyword".to_string(),
            TokenType::Const => "'const' keyword".to_string(),
            TokenType::Import => "'import' keyword".to_string(),
            TokenType::Break => "'break' keyword".to_string(),
            TokenType::Continue => "'continue' keyword".to_string(),
            TokenType::In => "'in' keyword".to_string(),
            TokenType::Is => "'is' keyword".to_string(),
            TokenType::As => "'as' keyword".to_string(),
            TokenType::Common => "'common' keyword".to_string(),
            TokenType::Where => "'where' keyword".to_string(),
            TokenType::None => "'none' keyword".to_string(),
            TokenType::OpenBrace => "'{'".to_string(),
            TokenType::CloseBrace => "'}'".to_string(),
            TokenType::OpenParen => "'('".to_string(),
            TokenType::CloseParen => "')'".to_string(),
            TokenType::OpenBracket => "'['".to_string(),
            TokenType::CloseBracket => "']'".to_string(),
            TokenType::Dot => "'.'".to_string(),
            TokenType::DotDot => "'..'".to_string(),
            TokenType::Comma => "','".to_string(),
            TokenType::Colon => "':'".to_string(),
            TokenType::Eq => "'='".to_string(),
            TokenType::Plus => "'+'".to_string(),
            TokenType::Minus => "'-'".to_string(),
            TokenType::Star => "'*'".to_string(),
            TokenType::Slash => "'/'".to_string(),
            TokenType::Percent => "'%'".to_string(),
            TokenType::Lt => "'<'".to_string(),
            TokenType::Gt => "'>'".to_string(),
            TokenType::EqEq => "'=='".to_string(),
            TokenType::NotEq => "'!='".to_string(),
            TokenType::Bang => "'!'".to_string(),
            TokenType::And => "'&&'".to_string(),
            TokenType::Or => "'||'".to_string(),
            TokenType::Id(name) => format!("identifier '{name}'"),
            TokenType::Int(n) => format!("integer literal '{n}'"),
            TokenType::Float(n) => format!("float literal '{n}'"),
            TokenType::Str(s) => format!("string literal \"{s}\""),
            TokenType::Bool(b) => format!("boolean literal '{b}'"),
            TokenType::Char(c) => format!("character literal '{c}'"),
            TokenType::Eof => "end of file".to_string(),
            TokenType::NewLine => "newline".to_string(),
            TokenType::StarStar => "'**'".to_string(),
            TokenType::Le => "'<='".to_string(),
            TokenType::Ge => "'>='".to_string(),
            TokenType::Incr => "'++'".to_string(),
            TokenType::Decr => "'--'".to_string(),
            TokenType::PlusEq => "'+='".to_string(),
            TokenType::MinusEq => "'-='".to_string(),
            TokenType::StarEq => "'*='".to_string(),
            TokenType::SlashEq => "'/='".to_string(),
            TokenType::PercentEq => "'%='".to_string(),
            TokenType::Ref => "'&'".to_string(),
            TokenType::Underscore => "'_'".to_string(),
            _ => format!("{token_type:?}"),
        }
    }

    fn consume_operator(&mut self) -> Option<BinaryOp> {
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
            self.current += 1;
        }
        result
    }

    fn peek_operator(&self) -> Option<BinaryOp> {
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

    fn consume_if_unary_operator(&mut self) -> Option<Token> {
        if self.is_at_end() {
            return None;
        }

        let token = self.peek();
        match &token.token_type {
            TokenType::Minus
            | TokenType::Bang
            | TokenType::Ref
            | TokenType::Incr
            | TokenType::Decr
            | TokenType::Star => {
                let token_clone = token.clone();
                self.current += 1;
                Some(token_clone)
            }
            _ => None,
        }
    }

    fn get_operator_precedence(&self, op: &BinaryOp) -> ParserResult<Precedence> {
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

    /// Check if a field type is a direct generic parameter (e.g., T, U, not `List<T>`)
    fn parse_field_declaration(
        &mut self,
        type_param_names: &[(String, Vec<TraitBound>)],
    ) -> ParserResult<Field> {
        // Check if this is a const field
        let is_const = if self.check(TokenType::Const) {
            self.consume();
            true
        } else {
            false
        };

        let field_type = self.parse_type()?;
        let field_name = self.consume_identifier("Expected field name")?;

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
        let where_clause = if self.check(TokenType::Where) {
            // Fields are newline-separated, so only a same-line `where`
            // belongs to this field.
            self.parse_where_clause()?
        } else {
            None
        };
        Ok(Field {
            name: field_name,
            type_: field_type,
            is_generic_param,
            is_const,
            default_value,
            where_clause,
        })
    }

    fn is_field_generic_param(
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

    fn is_literal_expression(expr: &ExpressionNode) -> bool {
        matches!(expr.kind, ExpressionKind::Literal(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::{Lexer, Token};
    use crate::source::Source;
    use std::rc::Rc;

    #[derive(Debug)]
    struct TestParser {
        parser: Parser<'static>,
    }

    impl TestParser {
        fn new(source: &str) -> Self {
            let mut src = Source::from_test_str(source);
            let tokens = collect_tokens(&mut src);

            let tokens_rc = Rc::new(tokens);

            let tokens_ptr = Rc::into_raw(tokens_rc.clone());

            let tokens_ref = unsafe { &*tokens_ptr };

            let parser = Parser::new(tokens_ref);

            Self { parser }
        }

        fn parse(&mut self) -> Result<Vec<AstNode>, (Vec<AstNode>, Vec<ParserError>)> {
            self.parser.parse()
        }
    }

    fn collect_tokens(source: &mut Source) -> Vec<Token> {
        let input = source.input.clone();
        println!("Collecting tokens from source:\n{input}");

        let mut lexer = Lexer::new(source);
        let mut tokens = Vec::new();

        loop {
            match lexer.next_token() {
                Ok(token) => {
                    if token.token_type == TokenType::Eof {
                        break;
                    }
                    tokens.push(token);
                }
                Err(e) => {
                    panic!("Lexer error: {e}");
                }
            }
        }

        tokens
    }

    #[test]
    fn parser_caps_recovery_errors() {
        let source = (0..110).map(|_| "func main( {\n").collect::<String>();
        let mut parser = create_parser(&source);
        let Err((_, errors)) = parser.parse() else {
            panic!("malformed source unexpectedly parsed");
        };

        assert_eq!(errors.len(), Parser::MAX_ERRORS);
        assert_eq!(
            errors.last().map(|error| error.code),
            Some(DiagnosticCode::ParseErrorLimit)
        );
    }

    #[test]
    fn parser_recovery_progress_is_iterative() {
        let source = ")".repeat(10_000);
        let mut parser = create_parser(&source);
        let Err((_, errors)) = parser.parse() else {
            panic!("malformed source unexpectedly parsed");
        };

        assert_eq!(errors.len(), Parser::MAX_ERRORS);
        assert_eq!(
            errors.last().map(|error| error.code),
            Some(DiagnosticCode::ParseErrorLimit)
        );
    }

    #[test]
    fn module_path_fixture_preserves_relative_and_absolute_prefixes() {
        for (source, expected_path) in [
            ("import ./module\n", "./module"),
            ("import ../module\n", "../module"),
            ("import /module\n", "/module"),
        ] {
            let mut parser = create_parser(source);
            let nodes = parser.parse().expect("module path fixture should parse");
            let AstNode::Statement(statement) = &nodes[0] else {
                panic!("module path fixture should produce an import statement");
            };
            let StatementKind::Import { module_path, .. } = &statement.kind else {
                panic!("module path fixture should produce an import");
            };
            assert_eq!(module_path, expected_path, "source: {source}");
        }
    }

    #[test]
    fn recovery_records_tokens_skipped_between_errors() {
        let mut parser = create_parser("auto x = 1 ) auto y = 2\n");
        parser.parser.current = parser
            .parser
            .tokens
            .iter()
            .position(|token| token.token_type == TokenType::CloseParen)
            .expect("test source should contain a closing parenthesis")
            .saturating_sub(1);
        parser.parser.synchronize();
        assert!(
            !parser.parser.recovery_spans().is_empty(),
            "parser recovery must expose discarded source intervals"
        );
    }

    fn create_parser(source: &str) -> TestParser {
        TestParser::new(source)
    }

    fn parse_expr(source: &str) -> ExpressionNode {
        let mut test_parser = create_parser(source);
        test_parser.parser.parse_expression().unwrap()
    }

    fn parse_stmts(source: &str) -> Vec<StatementNode> {
        let mut test_parser = create_parser(source);
        match test_parser.parse() {
            Ok(nodes) => collect_statements(nodes),
            Err((nodes, errors)) => {
                eprintln!("Parse errors: {errors:?}");
                collect_statements(nodes)
            }
        }
    }

    fn collect_statements(nodes: Vec<AstNode>) -> Vec<StatementNode> {
        nodes
            .into_iter()
            .filter_map(AstNode::into_statement)
            .collect()
    }

    fn assert_auto_decl_literal_statement(
        stmt: &StatementNode,
        expected_row: usize,
        expected_value: &LiteralNode,
    ) {
        if let StatementKind::AutoDecl(_, _, expr) = &stmt.kind {
            assert_eq!(stmt.span.row_start, expected_row);
            assert!(stmt.span.col_start > 0);
            assert_eq!(expr.span.row_start, expected_row);
            assert!(expr.span.col_start > 0);

            if let ExpressionKind::Literal(lit) = &expr.kind {
                assert_eq!(lit, expected_value);
            } else {
                panic!("Expected literal expression, got {:?}", expr.kind);
            }
        } else {
            panic!("Expected AutoDecl statement, got {:?}", stmt.kind);
        }
    }

    #[test]
    fn test_span_propagation() {
        // using a simpler input since the function parsing is more complex.
        let input = r#"
        auto x = 42
        auto y = "hello"
        "#;

        let stmts = parse_stmts(input);
        assert!(!stmts.is_empty(), "No statements were parsed");
        assert!(stmts.len() >= 2, "Expected at least 2 statements");

        assert_auto_decl_literal_statement(&stmts[0], 2, &LiteralNode::Integer(42));
        assert_auto_decl_literal_statement(&stmts[1], 3, &LiteralNode::String("hello".to_string()));
    }

    #[test]
    fn test_binary_expression_span() {
        let input = "1 + 2 * 3";
        let expr = parse_expr(input);

        // The binary expression should span the entire input
        if let ExpressionKind::Binary {
            left, op: _, right, ..
        } = &expr.kind
        {
            // The entire expression should span from the start of the first token to the end of the last token
            assert_eq!(expr.span.row_start, 1);
            assert_eq!(expr.span.col_start, 1);
            assert_eq!(expr.span.col_end, Some(10)); // 1-based, inclusive of last character

            // The left and right operands should have their own spans
            assert_eq!(left.span.row_start, 1);
            assert_eq!(left.span.col_start, 1);
            assert_eq!(right.span.row_start, 1);
            assert!(right.span.col_start > left.span.col_start);
        } else {
            panic!("Expected binary expression, got {:?}", expr.kind);
        }
    }

    #[test]
    fn test_function_call_span() {
        let input = "add(1, 2 + 3)";
        let expr = parse_expr(input);

        // The function call should span the entire input
        if let ExpressionKind::Call { func, args } = &expr.kind {
            assert_eq!(expr.span.row_start, 1);
            assert_eq!(expr.span.col_start, 1);

            // The function name should have its own span
            assert_eq!(func.span.row_start, 1);
            assert_eq!(func.span.col_start, 1);

            // Arguments should have their own spans
            assert_eq!(args.len(), 2);
            assert_eq!(args[0].span.row_start, 1);
            assert_eq!(args[0].span.col_start, 5);

            // The second argument is a binary expression
            if let ExpressionKind::Binary {
                left, op: _, right, ..
            } = &args[1].kind
            {
                assert_eq!(left.span.row_start, 1);
                assert_eq!(left.span.col_start, 8);
                assert_eq!(right.span.row_start, 1);
                assert_eq!(right.span.col_start, 12);
            } else {
                panic!(
                    "Expected binary expression as second argument, got {:?}",
                    args[1].kind
                );
            }
        } else {
            panic!("Expected function call expression, got {:?}", expr.kind);
        }
    }

    #[test]
    fn test_newline_termination() {
        let stmts = parse_stmts("auto x = 1\n");
        assert_eq!(stmts.len(), 1);

        let source = "auto x = 1\nauto y = 2\n";
        let mut test_parser = create_parser(source);
        let result = test_parser.parse();

        match result {
            Ok(nodes) => {
                println!("Successfully parsed {} nodes", nodes.len());
                for (i, node) in nodes.iter().enumerate() {
                    println!("Node {i}: {node:?}");
                }
                assert_eq!(nodes.len(), 2);
            }
            Err((nodes, errors)) => {
                println!("Parse failed with nodes: {nodes:?} and errors: {errors:?}");
                panic!("Failed to parse multiple statements with newlines");
            }
        }

        let stmts = parse_stmts("auto x = 1\n\nauto y = 2\n");
        assert_eq!(stmts.len(), 2);
    }

    #[test]
    fn test_missing_newline_error() {
        let source = "auto x = 1 auto y = 2";
        let mut test_parser = create_parser(source);
        let result = test_parser.parse();

        match result {
            Ok(nodes) => {
                panic!(
                    "Expected an error about missing newline, but got successful parse with nodes: {nodes:?}"
                );
            }
            Err((_, errors)) => {
                assert!(!errors.is_empty(), "Expected errors but got none");
                let has_newline_error = errors.iter().any(|e| {
                    let msg_lower = e.message.to_lowercase();
                    msg_lower.contains("expected newline after statement")
                        || msg_lower.contains("missing newline")
                        || msg_lower.contains("expected newline")
                });

                if !has_newline_error {
                    panic!("Expected an error about missing newline, but got: {errors:?}");
                }
            }
        }
    }
    #[test]
    fn test_variable_declaration() {
        let stmts = parse_stmts("auto x = 42\n");
        assert_eq!(stmts.len(), 1);
        match &stmts[0].kind {
            StatementKind::AutoDecl(name, _, expr) => {
                assert_eq!(name, "x");
                assert!(matches!(expr.kind, ExpressionKind::Literal(_)));
            }
            _ => panic!("Expected auto variable declaration"),
        }

        let stmts = parse_stmts("int x = 42\n");
        assert_eq!(stmts.len(), 1);

        let stmts = parse_stmts("const int PI = 3.14159\n");
        assert_eq!(stmts.len(), 1);
    }

    #[test]
    fn test_expressions() {
        let stmts = parse_stmts("1 + 2 * 3\n");
        assert_eq!(stmts.len(), 1);
        assert!(matches!(&stmts[0].kind, StatementKind::Expression { .. }));

        let expr = parse_expr("(1 + 2) * 3");
        assert!(matches!(expr.kind, ExpressionKind::Binary { .. }));
    }

    #[test]
    fn test_block_statements() {
        let block_expr = parse_stmts("{\n  auto x = 1\n  auto y = 2\n}\n");
        assert!(
            !block_expr.is_empty(),
            "Failed to parse block expression with newlines"
        );

        let single_stmt = parse_stmts("auto x = 1\n");
        assert!(
            !single_stmt.is_empty(),
            "Failed to parse single statement with newline"
        );

        let multi_stmt = parse_stmts("auto x = 1\n\nauto y = 2\n");
        assert_eq!(
            multi_stmt.len(),
            2,
            "Expected 2 statements with newline separation"
        );

        let block_with_empty_lines = parse_stmts("{\n  auto x = 1\n  \n  auto y = 2\n}\n");
        assert!(
            !block_with_empty_lines.is_empty(),
            "Failed to parse block with empty lines"
        );
    }

    #[test]
    fn test_control_flow() {
        let stmts = parse_stmts("if x {\n  auto y = 1\n}\n");
        assert!(!stmts.is_empty());
    }

    #[test]
    fn test_function_declaration() {
        let mut test_parser = create_parser("func add(int a, int b) returns int {\n  a + b\n}\n");
        let result = test_parser.parse();

        match result {
            Ok(nodes) => {
                assert!(!nodes.is_empty());

                if let AstNode::Function(func) = &nodes[0] {
                    assert_eq!(func.name, "add");
                    assert_eq!(func.params.len(), 2);
                    assert_eq!(
                        func.return_type.kind,
                        TypeKind::Primitive(PrimitiveType::Int)
                    );
                } else {
                    panic!("Expected a function node, got {:?}", nodes[0]);
                }
            }
            Err((nodes, errors)) => {
                if !nodes.is_empty()
                    && let AstNode::Function(func) = &nodes[0]
                {
                    assert_eq!(func.name, "add");
                    assert_eq!(func.params.len(), 2);
                    assert_eq!(
                        func.return_type.kind,
                        TypeKind::Primitive(PrimitiveType::Int)
                    );
                    return;
                }
                panic!("Failed to parse function: {errors:?}");
            }
        }

        let mut test_parser = create_parser("fn double(int x) {\n  return x * 2\n}\n");
        let result = test_parser.parse();

        match result {
            Ok(nodes) => {
                assert!(!nodes.is_empty(), "Expected at least one statement");
            }
            Err((nodes, errors)) => {
                if nodes.is_empty() {
                    panic!("Failed to parse function: {errors:?}");
                }
            }
        }

        let mut test_parser =
            create_parser("fn greet(string name, int times = 1) {\n  return 0\n}\n");
        let result = test_parser.parse();

        match result {
            Ok(nodes) => {
                assert!(!nodes.is_empty(), "Expected at least one statement");
            }
            Err((nodes, errors)) => {
                if nodes.is_empty() {
                    panic!("Failed to parse function: {errors:?}");
                }
            }
        }
    }

    #[test]
    fn test_error_recovery() {
        let source = r"
            let x = 5
            auto y = 10

            func foo() {
                return 42
            }

            = 99

            let z = 20 30

            auto valid1 = 100

            let result = (5 + 10

            func bar() {
                return
                auto x = 42
            }

            auto valid2 = 200

            struct BadStruct {
                field1: InvalidType
                field2: int
            }

            auto valid3 = 300
        ";

        let mut parser = create_parser(source);
        let result = parser.parse();

        // We expect an error due to the invalid number format
        if let Err((nodes, errors)) = result {
            assert!(!errors.is_empty(), "Expected at least one error");

            // Check that we have valid nodes despite the errors
            assert!(!nodes.is_empty(), "Expected some valid nodes to be parsed");
            println!("Successfully parsed {} nodes despite errors", nodes.len());

            // Check that we have at least some valid nodes
            assert!(
                nodes.len() >= 2,
                "Expected at least 2 valid nodes to be parsed"
            );

            let mut found_expected_error = false;
            for error in &errors {
                println!("Found error: {} at {:?}", error.message, error.span);

                if error.message.contains("unknown escape sequence")
                    || error.message.contains("Expected expression")
                {
                    found_expected_error = true;
                }

                assert!(
                    !error.message.is_empty(),
                    "Expected a non-empty error message"
                );
            }

            assert!(
                found_expected_error,
                "Expected to find an error about invalid syntax"
            );
        } else {
            panic!("Expected parsing to fail with errors");
        }

        // Test recovery in the middle of expressions
        let source = "auto x = 10 + * 5 - / 3\nauto y = 20\n";
        let mut parser = create_parser(source);
        let result = parser.parse();

        // This should also fail but not panic
        assert!(result.is_err());
    }
}
