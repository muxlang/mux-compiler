//! Lossless concrete syntax produced alongside the compiler AST.
//!
//! Token text is always read from the original source using its byte range. In
//! particular, comments and literal spellings are not reconstructed from the
//! decoded `TokenType` payload.

use std::sync::Arc;

use crate::ast::{
    AstNode, BinaryOp, EnumVariant, EnumVariantField, ExpressionKind, ExpressionNode, Field,
    FunctionNode, ImportSpec, LiteralNode, MatchArm, Param, PatternNode, PrimitiveType,
    StatementKind, StatementNode, TraitBound, TraitRef, TypeKind, TypeNode, UnaryOp, WhereClause,
};
use crate::diagnostic::{Diagnostic, FileId, ToDiagnostic};
use crate::lexer::{ByteRange, LexerError, Span, Token, TokenType};
use crate::parser::{Parser, ParserError};
use crate::source::{Source, SourceText};

/// A context attached to a contiguous region of source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SyntaxKind {
    Root,
    Declaration,
    FunctionDeclaration,
    ClassDeclaration,
    FieldDeclaration,
    ClassMethod,
    TraitReference,
    InterfaceDeclaration,
    EnumDeclaration,
    EnumVariant,
    EnumVariantField,
    ImportDeclaration,
    TestDeclaration,
    ClassBody,
    InterfaceBody,
    EnumBody,
    Statement,
    Block,
    IfStatement,
    WhileStatement,
    ForStatement,
    MatchStatement,
    MatchExpression,
    MatchArm,
    Pattern,
    Parameter,
    TypeParameter,
    TraitBound,
    WhereClause,
    Expression,
    BinaryExpression,
    UnaryExpression,
    LambdaExpression,
    IfExpression,
    CallArguments,
    ParameterList,
    Type,
    TypeArguments,
    ParenthesizedExpression,
    TupleExpression,
    ListLiteral,
    IndexExpression,
    SliceExpression,
    MapLiteral,
    SetLiteral,
    MatchArms,
    Delimited,
    Error,
}

/// The syntax-level form of a simple variable declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VariableDeclarationKind {
    Auto,
    Typed,
    Uninitialized,
    Const,
}

/// Source facts for a module import alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyntaxModuleAlias {
    /// Use the final path segment as the module binding.
    Default,
    /// An explicit `as _` import that creates no binding.
    Hidden,
    /// The source range of an explicit alias token.
    Explicit(ByteRange),
}

/// Lossless import-spec facts used to lower the AST-facing import variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyntaxImportSpec {
    Module {
        alias: SyntaxModuleAlias,
    },
    Item {
        item: ByteRange,
        alias: Option<ByteRange>,
    },
    Items {
        items: Vec<(ByteRange, Option<ByteRange>)>,
    },
    Wildcard,
}

/// Grammar facts attached to typed contexts for AST lowering. Every field is a
/// source range or another range, so spelling and token values remain owned by
/// the lossless token leaves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AstLoweringData {
    Name {
        token: ByteRange,
    },
    Literal {
        token: ByteRange,
    },
    Unary {
        operator: ByteRange,
        operand: ByteRange,
        postfix: bool,
    },
    Lambda {
        parameters: Vec<ByteRange>,
        return_type: ByteRange,
        where_clause: Option<ByteRange>,
        body: ByteRange,
        ast_span: ByteRange,
    },
    IfExpression {
        condition: ByteRange,
        then_expression: ByteRange,
        else_expression: ByteRange,
        ast_span: ByteRange,
    },
    Slice {
        base: ByteRange,
        start: Option<ByteRange>,
        end: Option<ByteRange>,
    },
    Binary {
        operator: ByteRange,
        left: ByteRange,
        right: ByteRange,
    },
    Parenthesized {
        expression: ByteRange,
    },
    Tuple {
        first: ByteRange,
        second: ByteRange,
    },
    List {
        elements: Vec<ByteRange>,
    },
    Map {
        entries: Vec<(ByteRange, ByteRange)>,
        inferred_type_span: ByteRange,
    },
    Set {
        elements: Vec<ByteRange>,
    },
    Call {
        callee: ByteRange,
        arguments: Vec<ByteRange>,
    },
    FieldAccess {
        base: ByteRange,
        field: ByteRange,
    },
    Index {
        base: ByteRange,
        index: ByteRange,
    },
    Generic {
        target: ByteRange,
        arguments: Vec<ByteRange>,
    },
    TypeName {
        name: ByteRange,
        arguments: Vec<ByteRange>,
    },
    TypeReference {
        reference: ByteRange,
    },
    TypeContainer {
        name: ByteRange,
        arguments: Vec<ByteRange>,
    },
    FunctionType {
        parameters: Vec<ByteRange>,
        returns: ByteRange,
    },
    VariableDeclaration {
        kind: VariableDeclarationKind,
        name: ByteRange,
        type_range: Option<ByteRange>,
        value: Option<ByteRange>,
        ast_span: ByteRange,
    },
    ExpressionStatement {
        expression: ByteRange,
        ast_span: ByteRange,
    },
    ReturnStatement {
        value: Option<ByteRange>,
        ast_span: ByteRange,
    },
    BreakStatement {
        ast_span: ByteRange,
    },
    ContinueStatement {
        ast_span: ByteRange,
    },
    Block,
    IfStatement {
        condition: ByteRange,
        then_block: ByteRange,
        else_branch: Option<ByteRange>,
        ast_span: ByteRange,
    },
    WhileStatement {
        condition: ByteRange,
        body: ByteRange,
        ast_span: ByteRange,
    },
    ForStatement {
        variable: ByteRange,
        variable_type: ByteRange,
        iterator: ByteRange,
        body: ByteRange,
        body_is_block: bool,
        ast_span: ByteRange,
    },
    MatchStatement {
        expression: ByteRange,
        arms: Vec<ByteRange>,
        ast_span: ByteRange,
    },
    MatchExpression {
        expression: ByteRange,
        arms: Vec<ByteRange>,
        ast_span: ByteRange,
    },
    MatchArm {
        pattern: ByteRange,
        guard: Option<ByteRange>,
        body: ByteRange,
        body_is_expression: bool,
    },
    Pattern(SyntaxPattern),
    Function {
        name: ByteRange,
        ast_span: ByteRange,
        type_parameters: Vec<ByteRange>,
        parameters: Vec<ByteRange>,
        return_type: Option<ByteRange>,
        return_type_span: ByteRange,
        where_clause: Option<ByteRange>,
        body: Option<ByteRange>,
        is_common: bool,
    },
    Import {
        module_path: ByteRange,
        spec: SyntaxImportSpec,
        ast_span: ByteRange,
    },
    Test {
        name: ByteRange,
        body: ByteRange,
        body_contents: ByteRange,
        ast_span: ByteRange,
    },
    Enum {
        name: ByteRange,
        type_parameters: Vec<ByteRange>,
        variants: Vec<ByteRange>,
        ast_span: ByteRange,
    },
    EnumVariant {
        name: ByteRange,
        fields: Option<Vec<ByteRange>>,
        where_clause: Option<ByteRange>,
    },
    EnumVariantField {
        field_name: Option<ByteRange>,
        type_range: ByteRange,
    },
    Class {
        name: ByteRange,
        type_parameters: Vec<ByteRange>,
        traits: Vec<ByteRange>,
        fields: Vec<ByteRange>,
        methods: Vec<ByteRange>,
        where_clause: Option<ByteRange>,
        ast_span: ByteRange,
    },
    Interface {
        name: ByteRange,
        type_parameters: Vec<ByteRange>,
        fields: Vec<ByteRange>,
        methods: Vec<ByteRange>,
        ast_span: ByteRange,
    },
    Field {
        name: ByteRange,
        type_range: ByteRange,
        is_generic_param: bool,
        is_const: bool,
        default_value: Option<ByteRange>,
        where_clause: Option<ByteRange>,
    },
    ClassMethod {
        function: ByteRange,
    },
    TraitReference {
        name: ByteRange,
        type_arguments: Vec<ByteRange>,
    },
    Parameter {
        name: ByteRange,
        type_range: ByteRange,
        default_value: Option<ByteRange>,
    },
    TypeParameter {
        name: ByteRange,
        bounds: Vec<ByteRange>,
    },
    TraitBound {
        name: ByteRange,
        type_arguments: Vec<ByteRange>,
    },
    WhereClause {
        predicates: Vec<ByteRange>,
    },
}

/// Range-only pattern facts. Pattern spellings are read from their token leaves
/// when lowering to the AST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyntaxPattern {
    Literal {
        token: ByteRange,
    },
    Identifier {
        token: ByteRange,
    },
    Wildcard,
    EnumVariant {
        name: ByteRange,
        args: Vec<ByteRange>,
    },
    List {
        elements: Vec<ByteRange>,
        rest: Option<ByteRange>,
    },
}

/// An AST-lowering failure for an incomplete or unsupported syntax context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyntaxLowerError {
    MissingContext(ByteRange),
    MissingToken(ByteRange),
    UnsupportedContext(SyntaxKind),
}

/// Failure to produce a compilation-unit AST from parsed source.
#[derive(Debug, Clone, PartialEq)]
pub enum ParseOutputLowerError<'a> {
    Frontend(&'a [FrontendError]),
    Syntax(SyntaxLowerError),
}

impl ParseOutputLowerError<'_> {
    /// Iterate the frontend or lowering errors as displayable values.
    pub fn iter(&self) -> Box<dyn Iterator<Item = &dyn std::fmt::Display> + '_> {
        match self {
            Self::Frontend(errors) => {
                Box::new(errors.iter().map(|error| error as &dyn std::fmt::Display))
            }
            Self::Syntax(error) => Box::new(std::iter::once(error as &dyn std::fmt::Display)),
        }
    }
}

impl std::fmt::Display for ParseOutputLowerError<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Frontend(errors) => {
                for (index, error) in errors.iter().enumerate() {
                    if index != 0 {
                        f.write_str("; ")?;
                    }
                    std::fmt::Display::fmt(error, f)?;
                }
                Ok(())
            }
            Self::Syntax(error) => std::fmt::Display::fmt(error, f),
        }
    }
}

impl std::error::Error for ParseOutputLowerError<'_> {}

impl std::fmt::Display for SyntaxLowerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingContext(range) => write!(
                f,
                "no typed expression context covers bytes {}..{}",
                range.start, range.end
            ),
            Self::MissingToken(range) => write!(
                f,
                "no lossless token covers bytes {}..{}",
                range.start, range.end
            ),
            Self::UnsupportedContext(kind) => {
                write!(f, "AST lowering is not implemented for {kind:?}")
            }
        }
    }
}

impl std::error::Error for SyntaxLowerError {}

/// A lossless token leaf. Its text is the exact source spelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxToken {
    token: Token,
}

impl SyntaxToken {
    #[must_use]
    pub fn kind(&self) -> &TokenType {
        &self.token.token_type
    }

    #[must_use]
    pub fn range(&self) -> ByteRange {
        self.token
            .span
            .byte_range
            .expect("validated syntax token range")
    }

    #[must_use]
    pub fn text<'a>(&self, source: &'a str) -> &'a str {
        let range = self.range();
        source
            .get(range.start..range.end)
            .expect("syntax token range must address valid UTF-8 source")
    }

    #[must_use]
    pub fn token(&self) -> &Token {
        &self.token
    }
}

/// An ordered token or nested context node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyntaxElement {
    Node(Box<SyntaxNode>),
    Token(usize),
}

/// A syntax context with ordered children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxNode {
    kind: SyntaxKind,
    range: ByteRange,
    data: Option<AstLoweringData>,
    children: Vec<SyntaxElement>,
}

impl SyntaxNode {
    #[must_use]
    pub fn kind(&self) -> SyntaxKind {
        self.kind
    }

    #[must_use]
    pub fn range(&self) -> ByteRange {
        self.range
    }

    #[must_use]
    pub fn lowering_data(&self) -> Option<&AstLoweringData> {
        self.data.as_ref()
    }

    #[must_use]
    pub fn children(&self) -> &[SyntaxElement] {
        &self.children
    }
}

/// A completed grammar context to be inserted into the lossless token stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SyntaxNodeEvent {
    pub kind: SyntaxKind,
    pub range: ByteRange,
    pub data: Option<AstLoweringData>,
}

/// The lossless syntax tree for one source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxTree {
    source: Arc<SourceText>,
    tokens: Vec<SyntaxToken>,
    root: SyntaxNode,
}

/// Structured source information for a top-level test declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TestDeclaration<'a> {
    range: ByteRange,
    name: &'a str,
    body_contents: ByteRange,
    body_start_line: usize,
    annotation: Option<&'a str>,
}

impl TestDeclaration<'_> {
    #[must_use]
    pub fn range(&self) -> ByteRange {
        self.range
    }

    #[must_use]
    pub fn name(&self) -> &str {
        self.name
    }

    /// Source bytes inside the test body's braces, including original trivia.
    #[must_use]
    pub fn body_contents(&self) -> ByteRange {
        self.body_contents
    }

    #[must_use]
    pub fn body_start_line(&self) -> usize {
        self.body_start_line
    }

    /// An immediately preceding `// mux:test ...` comment, if present.
    #[must_use]
    pub fn annotation(&self) -> Option<&str> {
        self.annotation
    }
}

/// A parser or lexer error produced while building a [`SyntaxTree`].
#[derive(Debug, Clone, PartialEq)]
pub enum FrontendError {
    Lexer(LexerError),
    Parser(ParserError),
}

/// Result of lossless parsing and recoverable front-end diagnostics.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseOutput {
    pub tree: SyntaxTree,
    pub errors: Vec<FrontendError>,
    pub recovery_spans: Vec<Span>,
}

impl ParseOutput {
    #[must_use]
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    /// Return the compiler AST only when lexing and parsing both succeeded.
    pub fn lower(&self) -> Result<Vec<AstNode>, ParseOutputLowerError<'_>> {
        if !self.errors.is_empty() {
            return Err(ParseOutputLowerError::Frontend(&self.errors));
        }
        self.tree
            .lower_compilation_unit()
            .map_err(ParseOutputLowerError::Syntax)
    }

    /// Lower every top-level construct that survived parser recovery.
    ///
    /// This is intended for tools such as `mux fix` that need to analyze valid
    /// regions after reporting syntax errors. Constructs that cannot be lowered
    /// are omitted; callers must report `errors` separately.
    #[must_use]
    pub fn lower_recovered(&self) -> Vec<AstNode> {
        self.tree.lower_compilation_unit_recovered()
    }

    /// Byte ranges skipped or recovered by the parser, preserving source
    /// coordinates without routing edit consumers through display spans.
    pub fn recovery_byte_ranges(&self) -> impl Iterator<Item = ByteRange> + '_ {
        self.recovery_spans
            .iter()
            .filter_map(|span| span.byte_range)
    }
}

/// Lex and parse source into a lossless tree.
///
/// The tree is retained even when errors occur, so editor and formatter
/// clients can inspect the malformed region and its original spelling.
#[must_use]
pub fn parse_source(input: &str) -> ParseOutput {
    let tree_source = Arc::new(SourceText::new(input.to_owned()));
    let mut source = Source::from_source_text(tree_source.clone());
    let lexed = crate::lexer::Lexer::new(&mut source).lex_all_lossless();
    let mut parser = Parser::new(&lexed.tokens);
    let parser_errors = match parser.parse() {
        Ok(()) => Vec::new(),
        Err(errors) => errors,
    };
    let syntax_events = parser.take_syntax_events();
    let recovery_spans = parser.recovery_spans().to_vec();
    drop(parser);
    let mut errors: Vec<_> = lexed.errors.into_iter().map(FrontendError::Lexer).collect();
    errors.extend(parser_errors.into_iter().map(FrontendError::Parser));
    ParseOutput {
        tree: SyntaxTree::new(tree_source, lexed.tokens, syntax_events),
        errors,
        recovery_spans,
    }
}

impl FrontendError {
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Self::Lexer(error) => error.span,
            Self::Parser(error) => error.span,
        }
    }

    /// Authoritative source bytes associated with this error, if available.
    #[must_use]
    pub fn byte_range(&self) -> Option<ByteRange> {
        self.span().byte_range
    }

    /// Format an error with its location derived from the supplied source.
    #[must_use]
    pub fn display_with_source(&self, source: &str) -> String {
        let Some(range) = self.byte_range() else {
            return self.to_string();
        };
        let source = SourceText::new(source.to_owned());
        let (line, column) = source.line_col(range.start);
        let (kind, message) = match self {
            Self::Lexer(error) => ("Lexer", error.message.as_ref()),
            Self::Parser(error) => ("Parser", error.message.as_ref()),
        };
        format!("{kind} error at {line}:{column} - {message}")
    }
}

impl std::fmt::Display for FrontendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lexer(error) => std::fmt::Display::fmt(error, f),
            Self::Parser(error) => std::fmt::Display::fmt(error, f),
        }
    }
}

impl std::error::Error for FrontendError {}

impl ToDiagnostic for FrontendError {
    fn to_diagnostic(&self, file_id: FileId) -> Diagnostic {
        match self {
            Self::Lexer(error) => error.to_diagnostic(file_id),
            Self::Parser(error) => error.to_diagnostic(file_id),
        }
    }
}

impl SyntaxTree {
    pub(crate) fn new(
        source: Arc<SourceText>,
        tokens: Vec<Token>,
        events: impl Into<Vec<SyntaxNodeEvent>>,
    ) -> Self {
        let text = source.text();
        for token in &tokens {
            let range = token
                .span
                .byte_range
                .expect("lossless lexer tokens must have byte ranges");
            assert!(
                range.start <= range.end
                    && range.end <= text.len()
                    && text.is_char_boundary(range.start)
                    && text.is_char_boundary(range.end),
                "lossless token range must be an ordered UTF-8 source range"
            );
        }
        let tokens: Vec<_> = tokens
            .into_iter()
            .map(|token| SyntaxToken { token })
            .collect();
        let root_range = ByteRange::new(0, text.len());
        let root = build_tree(root_range, &tokens, events.into());
        Self {
            source,
            tokens,
            root,
        }
    }

    #[must_use]
    pub fn source(&self) -> &str {
        self.source.text()
    }

    #[must_use]
    pub fn root(&self) -> &SyntaxNode {
        &self.root
    }

    /// All lexer tokens, including whitespace, comments, invalid text, and EOF.
    #[must_use]
    pub fn tokens(&self) -> &[SyntaxToken] {
        &self.tokens
    }

    /// Return the exact source spelling for a token index.
    #[must_use]
    pub fn token_text(&self, index: usize) -> Option<&str> {
        self.tokens
            .get(index)
            .map(|token| token.text(self.source()))
    }

    /// Return the test declarations with their exact body source and adjacent
    /// annotation comment, without requiring clients to reconstruct them from
    /// the lossless token stream.
    pub fn test_declarations(&self) -> Result<Vec<TestDeclaration<'_>>, SyntaxLowerError> {
        let mut nodes = Vec::new();
        collect_test_nodes(&self.root, &mut nodes);
        nodes
            .into_iter()
            .map(|node| {
                let Some(AstLoweringData::Test {
                    name,
                    body,
                    body_contents,
                    ..
                }) = node.data.as_ref()
                else {
                    return Err(SyntaxLowerError::UnsupportedContext(node.kind));
                };
                let name_token = self.token_for_range(*name)?;
                let TokenType::Str(name) = name_token.kind() else {
                    return Err(SyntaxLowerError::MissingToken(*name));
                };
                let body_start_line = self.source.line_col(body.start).0;
                Ok(TestDeclaration {
                    range: node.range,
                    name,
                    body_contents: *body_contents,
                    body_start_line,
                    annotation: self.test_annotation_before(node.range.start),
                })
            })
            .collect()
    }

    fn test_annotation_before(&self, start: usize) -> Option<&str> {
        let mut index = self
            .tokens
            .partition_point(|token| token.range().end <= start);
        let comment = loop {
            let token = self.tokens.get(index.checked_sub(1)?)?;
            index -= 1;
            match token.kind() {
                TokenType::Whitespace | TokenType::NewLine => continue,
                TokenType::LineComment(_) => break token,
                _ => return None,
            }
        };
        let comment_range = comment.range();
        let source = self.source();
        let comment_start_line = source[..comment_range.start].rsplit(['\n', '\r']).next()?;
        if !comment_start_line.trim().is_empty() {
            return None;
        }
        let between = source.get(comment_range.end..start)?;
        if !between.chars().all(char::is_whitespace) || count_line_breaks(between) != 1 {
            return None;
        }
        source.get(comment_range.start..comment_range.end)
    }

    /// Lower declarations and statements in source order from the syntax tree.
    pub fn lower_compilation_unit(&self) -> Result<Vec<AstNode>, SyntaxLowerError> {
        let mut declarations = Vec::new();
        for element in self.root.children() {
            let SyntaxElement::Node(node) = element else {
                continue;
            };
            if is_compilation_unit_wrapper(node.kind()) {
                declarations.push(self.lower_top_level_declaration(node)?);
            }
        }
        Ok(declarations)
    }

    /// Lower valid top-level constructs while skipping contexts made
    /// incomplete by parser recovery.
    #[must_use]
    pub fn lower_compilation_unit_recovered(&self) -> Vec<AstNode> {
        self.root
            .children()
            .iter()
            .filter_map(|element| match element {
                SyntaxElement::Node(node) if is_compilation_unit_wrapper(node.kind()) => {
                    self.lower_top_level_declaration(node).ok()
                }
                _ => None,
            })
            .collect()
    }

    /// Lower one typed expression context to the compiler AST.
    pub fn lower_expression(&self, node: &SyntaxNode) -> Result<ExpressionNode, SyntaxLowerError> {
        self.lower_expression_node(node)
    }

    /// Find and lower the typed expression that exactly covers `range`.
    pub fn lower_expression_at(
        &self,
        range: ByteRange,
    ) -> Result<ExpressionNode, SyntaxLowerError> {
        let Some(node) = self.find_expression_node(self.root(), range) else {
            return Err(SyntaxLowerError::MissingContext(range));
        };
        self.lower_expression_node(node)
    }

    /// Lower one typed type context to the compiler AST.
    pub fn lower_type(&self, node: &SyntaxNode) -> Result<TypeNode, SyntaxLowerError> {
        self.lower_type_node(node)
    }

    /// Find and lower the typed type context that exactly covers `range`.
    pub fn lower_type_at(&self, range: ByteRange) -> Result<TypeNode, SyntaxLowerError> {
        let Some(node) = self.find_typed_node(self.root(), range, is_type_data) else {
            return Err(SyntaxLowerError::MissingContext(range));
        };
        self.lower_type_node(node)
    }

    /// Lower one typed statement context to the compiler AST.
    pub fn lower_statement(&self, node: &SyntaxNode) -> Result<StatementNode, SyntaxLowerError> {
        self.lower_statement_node(node)
    }

    /// Find and lower the typed statement that exactly covers `range`.
    pub fn lower_statement_at(&self, range: ByteRange) -> Result<StatementNode, SyntaxLowerError> {
        let Some(node) = self.find_typed_node(self.root(), range, is_statement_data) else {
            return Err(SyntaxLowerError::MissingContext(range));
        };
        self.lower_statement_node(node)
    }

    /// Lower one typed function declaration or bodyless method signature.
    pub fn lower_function(&self, node: &SyntaxNode) -> Result<FunctionNode, SyntaxLowerError> {
        self.lower_function_node(node)
    }

    /// Find and lower the function declaration exactly covering `range`.
    pub fn lower_function_at(&self, range: ByteRange) -> Result<FunctionNode, SyntaxLowerError> {
        let Some(node) = self.find_typed_node(self.root(), range, is_function_data) else {
            return Err(SyntaxLowerError::MissingContext(range));
        };
        self.lower_function_node(node)
    }

    /// Lower one named test declaration while keeping its body as ordinary statements.
    pub fn lower_test(&self, node: &SyntaxNode) -> Result<AstNode, SyntaxLowerError> {
        self.lower_test_node(node)
    }

    /// Find and lower the test declaration exactly covering `range`.
    pub fn lower_test_at(&self, range: ByteRange) -> Result<AstNode, SyntaxLowerError> {
        let Some(node) = self.find_typed_node(self.root(), range, is_test_data) else {
            return Err(SyntaxLowerError::MissingContext(range));
        };
        self.lower_test_node(node)
    }

    /// Lower one typed enum declaration and its payload variants.
    pub fn lower_enum(&self, node: &SyntaxNode) -> Result<AstNode, SyntaxLowerError> {
        self.lower_enum_node(node)
    }

    /// Find and lower the enum declaration exactly covering `range`.
    pub fn lower_enum_at(&self, range: ByteRange) -> Result<AstNode, SyntaxLowerError> {
        let Some(node) = self.find_typed_node(self.root(), range, is_enum_data) else {
            return Err(SyntaxLowerError::MissingContext(range));
        };
        self.lower_enum_node(node)
    }

    /// Lower one typed class declaration, preserving member order within each
    /// AST member collection.
    pub fn lower_class(&self, node: &SyntaxNode) -> Result<AstNode, SyntaxLowerError> {
        self.lower_class_node(node)
    }

    /// Find and lower the class declaration exactly covering `range`.
    pub fn lower_class_at(&self, range: ByteRange) -> Result<AstNode, SyntaxLowerError> {
        let Some(node) = self.find_typed_node(self.root(), range, is_class_data) else {
            return Err(SyntaxLowerError::MissingContext(range));
        };
        self.lower_class_node(node)
    }

    /// Lower one typed interface declaration, including bodyless method
    /// signatures and field members.
    pub fn lower_interface(&self, node: &SyntaxNode) -> Result<AstNode, SyntaxLowerError> {
        self.lower_interface_node(node)
    }

    /// Find and lower the interface declaration exactly covering `range`.
    pub fn lower_interface_at(&self, range: ByteRange) -> Result<AstNode, SyntaxLowerError> {
        let Some(node) = self.find_typed_node(self.root(), range, is_interface_data) else {
            return Err(SyntaxLowerError::MissingContext(range));
        };
        self.lower_interface_node(node)
    }

    /// Lower one structural pattern by its exact source range.
    pub fn lower_pattern_at(&self, range: ByteRange) -> Result<PatternNode, SyntaxLowerError> {
        let Some(node) = self.find_typed_node(self.root(), range, is_pattern_data) else {
            return Err(SyntaxLowerError::MissingContext(range));
        };
        self.lower_pattern_node(node)
    }

    /// Lower one match arm by its exact source range.
    pub fn lower_match_arm_at(&self, range: ByteRange) -> Result<MatchArm, SyntaxLowerError> {
        let Some(node) = self.find_typed_node(self.root(), range, is_match_arm_data) else {
            return Err(SyntaxLowerError::MissingContext(range));
        };
        self.lower_match_arm_node(node)
    }

    fn find_typed_node<'a>(
        &self,
        node: &'a SyntaxNode,
        range: ByteRange,
        accepts: fn(&AstLoweringData) -> bool,
    ) -> Option<&'a SyntaxNode> {
        if range.start < node.range.start || range.end > node.range.end {
            return None;
        }
        if node.range == range && node.data.as_ref().is_some_and(accepts) {
            return Some(node);
        }
        node.children.iter().find_map(|child| match child {
            SyntaxElement::Node(child) => self.find_typed_node(child, range, accepts),
            SyntaxElement::Token(_) => None,
        })
    }

    fn find_expression_node<'a>(
        &self,
        node: &'a SyntaxNode,
        range: ByteRange,
    ) -> Option<&'a SyntaxNode> {
        if range.start < node.range.start || range.end > node.range.end {
            return None;
        }
        if let Some(data) = node.data.as_ref().filter(|data| is_expression_data(data)) {
            let ast_range = match data {
                AstLoweringData::Lambda { ast_span, .. }
                | AstLoweringData::IfExpression { ast_span, .. }
                | AstLoweringData::MatchExpression { ast_span, .. } => *ast_span,
                AstLoweringData::Unary {
                    operator,
                    operand,
                    postfix,
                } => {
                    if *postfix {
                        ByteRange::new(operand.start, operator.end)
                    } else {
                        ByteRange::new(operator.start, operand.end)
                    }
                }
                AstLoweringData::Binary { left, right, .. } => {
                    ByteRange::new(left.start, right.end)
                }
                _ => node.range,
            };
            if ast_range == range {
                return Some(node);
            }
        }
        node.children.iter().find_map(|child| match child {
            SyntaxElement::Node(child) => self.find_expression_node(child, range),
            SyntaxElement::Token(_) => None,
        })
    }

    fn lower_top_level_declaration(&self, node: &SyntaxNode) -> Result<AstNode, SyntaxLowerError> {
        self.find_top_level_ast_node(node)?
            .ok_or(SyntaxLowerError::UnsupportedContext(node.kind()))
    }

    fn find_top_level_ast_node(
        &self,
        node: &SyntaxNode,
    ) -> Result<Option<AstNode>, SyntaxLowerError> {
        if let Some(data) = node.data.as_ref() {
            match data {
                AstLoweringData::Function { .. } => {
                    return self
                        .lower_function_node(node)
                        .map(AstNode::Function)
                        .map(Some);
                }
                AstLoweringData::Class { .. } => return self.lower_class_node(node).map(Some),
                AstLoweringData::Interface { .. } => {
                    return self.lower_interface_node(node).map(Some);
                }
                AstLoweringData::Enum { .. } => return self.lower_enum_node(node).map(Some),
                AstLoweringData::Test { .. } => return self.lower_test_node(node).map(Some),
                data if is_statement_data(data) => {
                    return self
                        .lower_statement_node(node)
                        .map(AstNode::Statement)
                        .map(Some);
                }
                _ => {}
            }
        }
        for child in node.children() {
            if let SyntaxElement::Node(child) = child
                && let Some(declaration) = self.find_top_level_ast_node(child)?
            {
                return Ok(Some(declaration));
            }
        }
        Ok(None)
    }

    fn lower_expression_node(&self, node: &SyntaxNode) -> Result<ExpressionNode, SyntaxLowerError> {
        let data = node
            .data
            .as_ref()
            .ok_or(SyntaxLowerError::UnsupportedContext(node.kind))?;
        let span = match data {
            AstLoweringData::Lambda { ast_span, .. }
            | AstLoweringData::IfExpression { ast_span, .. }
            | AstLoweringData::MatchExpression { ast_span, .. } => {
                self.span_for_range(*ast_span)?
            }
            AstLoweringData::Unary {
                operator,
                operand,
                postfix,
            } => {
                let range = if *postfix {
                    ByteRange::new(operand.start, operator.end)
                } else {
                    ByteRange::new(operator.start, operand.end)
                };
                self.span_for_range(range)?
            }
            AstLoweringData::Binary { left, right, .. } => {
                self.span_for_range(ByteRange::new(left.start, right.end))?
            }
            _ => self.span_for_range(node.range)?,
        };
        match data {
            AstLoweringData::Name { token } => {
                let token = self.token_for_range(*token)?;
                match token.kind() {
                    TokenType::Id(name) => Ok(ExpressionNode {
                        kind: ExpressionKind::Identifier(name.clone()),
                        span,
                    }),
                    _ => Err(SyntaxLowerError::MissingToken(token.range())),
                }
            }
            AstLoweringData::Literal { token } => {
                let token = self.token_for_range(*token)?;
                let kind = match token.kind() {
                    TokenType::Int(value) => ExpressionKind::Literal(LiteralNode::Integer(*value)),
                    TokenType::Float(value) => ExpressionKind::Literal(LiteralNode::Float(*value)),
                    TokenType::Bool(value) => ExpressionKind::Literal(LiteralNode::Boolean(*value)),
                    TokenType::Char(value) => ExpressionKind::Literal(LiteralNode::Char(*value)),
                    TokenType::Str(value) => {
                        ExpressionKind::Literal(LiteralNode::String(value.clone()))
                    }
                    TokenType::Bytes(value) => {
                        ExpressionKind::Literal(LiteralNode::Bytes(value.clone()))
                    }
                    TokenType::None => ExpressionKind::None,
                    _ => return Err(SyntaxLowerError::MissingToken(token.range())),
                };
                Ok(ExpressionNode { kind, span })
            }
            AstLoweringData::Unary {
                operator,
                operand,
                postfix,
            } => {
                let op_token = self.token_for_range(*operator)?.token();
                let op_span = self.span_for_range(*operator)?;
                let expr = self.lower_expression_at(*operand)?;
                let op = UnaryOp::parse(op_token)
                    .map_err(|_| SyntaxLowerError::MissingToken(*operator))?;
                Ok(ExpressionNode {
                    kind: ExpressionKind::Unary {
                        op,
                        op_span,
                        expr: Box::new(expr),
                        postfix: *postfix,
                    },
                    span,
                })
            }
            AstLoweringData::Binary {
                operator,
                left,
                right,
            } => {
                let op_token = self.token_for_range(*operator)?.token();
                let op_span = self.span_for_range(*operator)?;
                let op = binary_operator(&op_token.token_type)
                    .ok_or(SyntaxLowerError::MissingToken(*operator))?;
                Ok(ExpressionNode {
                    kind: ExpressionKind::Binary {
                        left: Box::new(self.lower_expression_at(*left)?),
                        op,
                        op_span,
                        right: Box::new(self.lower_expression_at(*right)?),
                    },
                    span,
                })
            }
            AstLoweringData::Parenthesized { expression } => self.lower_expression_at(*expression),
            AstLoweringData::Tuple { first, second } => Ok(ExpressionNode {
                kind: ExpressionKind::TupleLiteral(vec![
                    self.lower_expression_at(*first)?,
                    self.lower_expression_at(*second)?,
                ]),
                span,
            }),
            AstLoweringData::Lambda {
                parameters,
                return_type,
                where_clause,
                body,
                ..
            } => {
                let body_statement = self.lower_statement_at(*body)?;
                let body = match body_statement {
                    StatementNode {
                        kind: StatementKind::Block(statements),
                        ..
                    } => statements,
                    statement => vec![statement],
                };
                Ok(ExpressionNode {
                    kind: ExpressionKind::Lambda {
                        params: parameters
                            .iter()
                            .map(|range| self.lower_parameter_at(*range))
                            .collect::<Result<_, _>>()?,
                        return_type: self.lower_type_at(*return_type)?,
                        body,
                        where_clause: where_clause
                            .map(|range| self.lower_where_clause_at(range))
                            .transpose()?,
                    },
                    span,
                })
            }
            AstLoweringData::IfExpression {
                condition,
                then_expression,
                else_expression,
                ..
            } => Ok(ExpressionNode {
                kind: ExpressionKind::If {
                    cond: Box::new(self.lower_expression_at(*condition)?),
                    then_expr: Box::new(self.lower_expression_at(*then_expression)?),
                    else_expr: Box::new(self.lower_expression_at(*else_expression)?),
                },
                span,
            }),
            AstLoweringData::MatchExpression {
                expression, arms, ..
            } => Ok(ExpressionNode {
                kind: ExpressionKind::Match {
                    expr: Box::new(self.lower_expression_at(*expression)?),
                    arms: arms
                        .iter()
                        .map(|range| self.lower_match_arm_at(*range))
                        .collect::<Result<_, _>>()?,
                },
                span,
            }),
            AstLoweringData::Slice { base, start, end } => Ok(ExpressionNode {
                kind: ExpressionKind::Slice {
                    expr: Box::new(self.lower_expression_at(*base)?),
                    start: start
                        .map(|range| self.lower_expression_at(range).map(Box::new))
                        .transpose()?,
                    end: end
                        .map(|range| self.lower_expression_at(range).map(Box::new))
                        .transpose()?,
                },
                span,
            }),
            AstLoweringData::List { elements } => Ok(ExpressionNode {
                kind: ExpressionKind::ListLiteral(
                    elements
                        .iter()
                        .map(|range| self.lower_expression_at(*range))
                        .collect::<Result<_, _>>()?,
                ),
                span,
            }),
            AstLoweringData::Map {
                entries,
                inferred_type_span,
            } => {
                let inferred_type_span = self.span_for_range(*inferred_type_span)?;
                Ok(ExpressionNode {
                    kind: ExpressionKind::MapLiteral {
                        key_type: Box::new(TypeNode {
                            kind: TypeKind::Auto,
                            span: inferred_type_span,
                        }),
                        value_type: Box::new(TypeNode {
                            kind: TypeKind::Auto,
                            span: inferred_type_span,
                        }),
                        entries: entries
                            .iter()
                            .map(|(key, value)| {
                                Ok((
                                    self.lower_expression_at(*key)?,
                                    self.lower_expression_at(*value)?,
                                ))
                            })
                            .collect::<Result<_, SyntaxLowerError>>()?,
                    },
                    span,
                })
            }
            AstLoweringData::Set { elements } => Ok(ExpressionNode {
                kind: ExpressionKind::SetLiteral(
                    elements
                        .iter()
                        .map(|range| self.lower_expression_at(*range))
                        .collect::<Result<_, _>>()?,
                ),
                span,
            }),
            AstLoweringData::Call { callee, arguments } => Ok(ExpressionNode {
                kind: ExpressionKind::Call {
                    func: Box::new(self.lower_expression_at(*callee)?),
                    args: arguments
                        .iter()
                        .map(|range| self.lower_expression_at(*range))
                        .collect::<Result<_, _>>()?,
                },
                span,
            }),
            AstLoweringData::FieldAccess { base, field } => {
                let field_token = self.token_for_range(*field)?;
                let TokenType::Id(field_name) = field_token.kind() else {
                    return Err(SyntaxLowerError::MissingToken(*field));
                };
                Ok(ExpressionNode {
                    kind: ExpressionKind::FieldAccess {
                        expr: Box::new(self.lower_expression_at(*base)?),
                        field: field_name.clone(),
                    },
                    span,
                })
            }
            AstLoweringData::Index { base, index } => Ok(ExpressionNode {
                kind: ExpressionKind::ListAccess {
                    expr: Box::new(self.lower_expression_at(*base)?),
                    index: Box::new(self.lower_expression_at(*index)?),
                },
                span,
            }),
            AstLoweringData::Generic { target, arguments } => {
                let target_node = self.lower_expression_at(*target)?;
                let name = generic_target_name(&target_node)
                    .ok_or(SyntaxLowerError::MissingContext(*target))?;
                Ok(ExpressionNode {
                    kind: ExpressionKind::GenericType(
                        name,
                        arguments
                            .iter()
                            .map(|range| self.lower_type_at(*range))
                            .collect::<Result<_, _>>()?,
                    ),
                    span,
                })
            }
            _ => Err(SyntaxLowerError::UnsupportedContext(node.kind)),
        }
    }

    fn lower_type_node(&self, node: &SyntaxNode) -> Result<TypeNode, SyntaxLowerError> {
        let data = node
            .data
            .as_ref()
            .ok_or(SyntaxLowerError::UnsupportedContext(node.kind))?;
        let span = self.span_for_range(node.range)?;
        match data {
            AstLoweringData::TypeName { name, arguments } => {
                let name = self
                    .source
                    .text()
                    .get(name.start..name.end)
                    .ok_or(SyntaxLowerError::MissingToken(*name))?;
                let args = arguments
                    .iter()
                    .map(|range| self.lower_type_at(*range))
                    .collect::<Result<Vec<_>, _>>()?;
                let kind = if name == "dyn" {
                    let Some(inner) = args.into_iter().next() else {
                        return Err(SyntaxLowerError::MissingContext(node.range));
                    };
                    TypeKind::TraitObject(Box::new(inner))
                } else if let Some(primitive) = primitive_type(name) {
                    TypeKind::Primitive(primitive)
                } else {
                    TypeKind::Named(name.to_owned(), args)
                };
                Ok(TypeNode { kind, span })
            }
            AstLoweringData::TypeReference { reference } => Ok(TypeNode {
                kind: TypeKind::Reference(Box::new(self.lower_type_at(*reference)?)),
                span,
            }),
            AstLoweringData::TypeContainer {
                name: name_range,
                arguments,
            } => {
                let name = self
                    .source
                    .text()
                    .get(name_range.start..name_range.end)
                    .ok_or(SyntaxLowerError::MissingToken(*name_range))?;
                let mut args = arguments
                    .iter()
                    .map(|range| self.lower_type_at(*range))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter();
                let first = args
                    .next()
                    .ok_or(SyntaxLowerError::MissingContext(node.range))?;
                let kind = match name {
                    "list" => TypeKind::List(Box::new(first)),
                    "set" => TypeKind::Set(Box::new(first)),
                    "map" => TypeKind::Map(
                        Box::new(first),
                        Box::new(
                            args.next()
                                .ok_or(SyntaxLowerError::MissingContext(node.range))?,
                        ),
                    ),
                    "tuple" => TypeKind::Tuple(
                        Box::new(first),
                        Box::new(
                            args.next()
                                .ok_or(SyntaxLowerError::MissingContext(node.range))?,
                        ),
                    ),
                    _ => return Err(SyntaxLowerError::MissingToken(*name_range)),
                };
                Ok(TypeNode { kind, span })
            }
            AstLoweringData::FunctionType {
                parameters,
                returns,
            } => Ok(TypeNode {
                kind: TypeKind::Function {
                    params: parameters
                        .iter()
                        .map(|range| self.lower_type_at(*range))
                        .collect::<Result<_, _>>()?,
                    returns: Box::new(self.lower_type_at(*returns)?),
                },
                span,
            }),
            _ => Err(SyntaxLowerError::UnsupportedContext(node.kind)),
        }
    }

    fn lower_statement_node(&self, node: &SyntaxNode) -> Result<StatementNode, SyntaxLowerError> {
        let data = node
            .data
            .as_ref()
            .ok_or(SyntaxLowerError::UnsupportedContext(node.kind))?;
        let span = match data {
            AstLoweringData::Import { ast_span, .. } => self.span_for_range(*ast_span)?,
            AstLoweringData::MatchStatement { ast_span, .. } => self.span_for_range(*ast_span)?,
            AstLoweringData::Function { ast_span, .. } => self.span_for_range(*ast_span)?,
            AstLoweringData::IfStatement { ast_span, .. }
            | AstLoweringData::WhileStatement { ast_span, .. }
            | AstLoweringData::ForStatement { ast_span, .. }
            | AstLoweringData::VariableDeclaration { ast_span, .. }
            | AstLoweringData::ExpressionStatement { ast_span, .. }
            | AstLoweringData::ReturnStatement { ast_span, .. }
            | AstLoweringData::BreakStatement { ast_span }
            | AstLoweringData::ContinueStatement { ast_span } => self.span_for_range(*ast_span)?,
            _ => self.span_for_range(node.range)?,
        };
        let kind = match data {
            AstLoweringData::VariableDeclaration {
                kind,
                name,
                type_range,
                value,
                ..
            } => {
                let name_range = *name;
                let name = self.identifier_for_range(*name)?;
                let type_node = match type_range {
                    Some(range) => self.lower_type_at(*range)?,
                    None => TypeNode {
                        kind: TypeKind::Auto,
                        span: self.span_for_range(name_range)?,
                    },
                };
                let value = (*value)
                    .map(|range| self.lower_expression_at(range))
                    .transpose()?;
                match kind {
                    VariableDeclarationKind::Auto => StatementKind::AutoDecl(
                        name.clone(),
                        type_node,
                        value.ok_or(SyntaxLowerError::MissingContext(node.range))?,
                    ),
                    VariableDeclarationKind::Typed => StatementKind::TypedDecl(
                        name.clone(),
                        type_node,
                        value.ok_or(SyntaxLowerError::MissingContext(node.range))?,
                    ),
                    VariableDeclarationKind::Uninitialized => {
                        StatementKind::UninitDecl(name.clone(), type_node)
                    }
                    VariableDeclarationKind::Const => StatementKind::ConstDecl(
                        name.clone(),
                        type_node,
                        value.ok_or(SyntaxLowerError::MissingContext(node.range))?,
                    ),
                }
            }
            AstLoweringData::ExpressionStatement { expression, .. } => {
                StatementKind::Expression(self.lower_expression_at(*expression)?)
            }
            AstLoweringData::Function { .. } => {
                StatementKind::Function(self.lower_function_node(node)?)
            }
            AstLoweringData::ReturnStatement { value, .. } => StatementKind::Return(
                (*value)
                    .map(|range| self.lower_expression_at(range))
                    .transpose()?,
            ),
            AstLoweringData::BreakStatement { .. } => StatementKind::Break,
            AstLoweringData::ContinueStatement { .. } => StatementKind::Continue,
            AstLoweringData::Block => StatementKind::Block(self.lower_statement_children(node)?),
            AstLoweringData::IfStatement {
                condition,
                then_block,
                else_branch,
                ..
            } => {
                let then_statement = self.lower_statement_at(*then_block)?;
                let StatementKind::Block(then_block) = then_statement.kind else {
                    return Err(SyntaxLowerError::MissingContext(*then_block));
                };
                let else_block = else_branch
                    .map(|range| {
                        let statement = self.lower_statement_at(range)?;
                        match statement.kind {
                            StatementKind::Block(statements) => Ok(statements),
                            _ => Ok(vec![statement]),
                        }
                    })
                    .transpose()?;
                StatementKind::If {
                    cond: self.lower_expression_at(*condition)?,
                    then_block,
                    else_block,
                }
            }
            AstLoweringData::WhileStatement {
                condition, body, ..
            } => {
                let statement = self.lower_statement_at(*body)?;
                let StatementKind::Block(body) = statement.kind else {
                    return Err(SyntaxLowerError::MissingContext(*body));
                };
                StatementKind::While {
                    cond: self.lower_expression_at(*condition)?,
                    body,
                }
            }
            AstLoweringData::ForStatement {
                variable,
                variable_type,
                iterator,
                body,
                body_is_block,
                ..
            } => {
                let token = self.token_for_range(*variable)?;
                let variable = match token.kind() {
                    TokenType::Id(variable) => variable.clone(),
                    TokenType::Underscore => "_".to_owned(),
                    _ => return Err(SyntaxLowerError::MissingToken(*variable)),
                };
                let body_statement = self.lower_statement_at(*body)?;
                let body = if *body_is_block {
                    let StatementKind::Block(statements) = body_statement.kind else {
                        return Err(SyntaxLowerError::MissingContext(*body));
                    };
                    statements
                } else {
                    vec![body_statement]
                };
                StatementKind::For {
                    var: variable,
                    var_type: self.lower_type_at(*variable_type)?,
                    iter: self.lower_expression_at(*iterator)?,
                    body,
                }
            }
            AstLoweringData::MatchStatement {
                expression, arms, ..
            } => StatementKind::Match {
                expr: self.lower_expression_at(*expression)?,
                arms: arms
                    .iter()
                    .map(|range| self.lower_match_arm_at(*range))
                    .collect::<Result<_, _>>()?,
            },
            AstLoweringData::Import {
                module_path, spec, ..
            } => {
                let module_path = self.lower_module_path(*module_path)?;
                StatementKind::Import {
                    spec: self.lower_import_spec(spec, &module_path)?,
                    module_path,
                }
            }
            _ => return Err(SyntaxLowerError::UnsupportedContext(node.kind)),
        };
        Ok(StatementNode { kind, span })
    }

    fn lower_test_node(&self, node: &SyntaxNode) -> Result<AstNode, SyntaxLowerError> {
        let Some(AstLoweringData::Test {
            name,
            body,
            ast_span,
            ..
        }) = node.data.as_ref()
        else {
            return Err(SyntaxLowerError::UnsupportedContext(node.kind));
        };
        let name_token = self.token_for_range(*name)?;
        let TokenType::Str(name) = name_token.kind() else {
            return Err(SyntaxLowerError::MissingToken(*name));
        };
        let body_range = *body;
        let body_statement = self.lower_statement_at(body_range)?;
        let StatementKind::Block(body) = body_statement.kind else {
            return Err(SyntaxLowerError::MissingContext(body_range));
        };
        Ok(AstNode::Test {
            name: name.clone(),
            body,
            span: self.span_for_range(*ast_span)?,
        })
    }

    fn lower_enum_node(&self, node: &SyntaxNode) -> Result<AstNode, SyntaxLowerError> {
        let Some(AstLoweringData::Enum {
            name,
            type_parameters,
            variants,
            ast_span,
        }) = node.data.as_ref()
        else {
            return Err(SyntaxLowerError::UnsupportedContext(node.kind));
        };
        let name = self.identifier_for_range(*name)?;
        Ok(AstNode::Enum {
            name,
            type_params: type_parameters
                .iter()
                .map(|range| self.lower_type_parameter_at(*range))
                .collect::<Result<_, _>>()?,
            variants: variants
                .iter()
                .map(|range| self.lower_enum_variant_at(*range))
                .collect::<Result<_, _>>()?,
            span: self.span_for_range(*ast_span)?,
        })
    }

    fn lower_enum_variant_at(&self, range: ByteRange) -> Result<EnumVariant, SyntaxLowerError> {
        let Some(node) = self.find_typed_node(self.root(), range, |data| {
            matches!(data, AstLoweringData::EnumVariant { .. })
        }) else {
            return Err(SyntaxLowerError::MissingContext(range));
        };
        let AstLoweringData::EnumVariant {
            name,
            fields,
            where_clause,
        } = node
            .data
            .as_ref()
            .ok_or(SyntaxLowerError::UnsupportedContext(node.kind))?
        else {
            return Err(SyntaxLowerError::UnsupportedContext(node.kind));
        };
        let name = self.identifier_for_range(*name)?;
        Ok(EnumVariant {
            name,
            data: fields
                .as_ref()
                .map(|fields| {
                    fields
                        .iter()
                        .map(|range| self.lower_enum_variant_field_at(*range))
                        .collect::<Result<_, _>>()
                })
                .transpose()?,
            where_clause: where_clause
                .map(|range| self.lower_where_clause_at(range))
                .transpose()?,
        })
    }

    fn lower_enum_variant_field_at(
        &self,
        range: ByteRange,
    ) -> Result<EnumVariantField, SyntaxLowerError> {
        let Some(node) = self.find_typed_node(self.root(), range, |data| {
            matches!(data, AstLoweringData::EnumVariantField { .. })
        }) else {
            return Err(SyntaxLowerError::MissingContext(range));
        };
        let AstLoweringData::EnumVariantField {
            field_name,
            type_range,
        } = node
            .data
            .as_ref()
            .ok_or(SyntaxLowerError::UnsupportedContext(node.kind))?
        else {
            return Err(SyntaxLowerError::UnsupportedContext(node.kind));
        };
        let field_name = field_name
            .map(|range| {
                let token = self.token_for_range(range)?;
                let TokenType::Id(name) = token.kind() else {
                    return Err(SyntaxLowerError::MissingToken(range));
                };
                Ok(name.clone())
            })
            .transpose()?;
        Ok((field_name, self.lower_type_at(*type_range)?))
    }

    fn lower_class_node(&self, node: &SyntaxNode) -> Result<AstNode, SyntaxLowerError> {
        let Some(AstLoweringData::Class {
            name,
            type_parameters,
            traits,
            fields,
            methods,
            where_clause,
            ast_span,
        }) = node.data.as_ref()
        else {
            return Err(SyntaxLowerError::UnsupportedContext(node.kind));
        };
        Ok(AstNode::Class {
            name: self.identifier_for_range(*name)?,
            type_params: type_parameters
                .iter()
                .map(|range| self.lower_type_parameter_at(*range))
                .collect::<Result<_, _>>()?,
            traits: traits
                .iter()
                .map(|range| self.lower_trait_reference_at(*range))
                .collect::<Result<_, _>>()?,
            fields: fields
                .iter()
                .map(|range| self.lower_class_field_at(*range))
                .collect::<Result<_, _>>()?,
            methods: methods
                .iter()
                .map(|range| self.lower_class_method_at(*range))
                .collect::<Result<_, _>>()?,
            where_clause: where_clause
                .map(|range| self.lower_where_clause_at(range))
                .transpose()?,
            span: self.span_for_range(*ast_span)?,
        })
    }

    fn lower_interface_node(&self, node: &SyntaxNode) -> Result<AstNode, SyntaxLowerError> {
        let Some(AstLoweringData::Interface {
            name,
            type_parameters,
            fields,
            methods,
            ast_span,
        }) = node.data.as_ref()
        else {
            return Err(SyntaxLowerError::UnsupportedContext(node.kind));
        };
        Ok(AstNode::Interface {
            name: self.identifier_for_range(*name)?,
            type_params: type_parameters
                .iter()
                .map(|range| self.lower_type_parameter_at(*range))
                .collect::<Result<_, _>>()?,
            fields: fields
                .iter()
                .map(|range| self.lower_class_field_at(*range))
                .collect::<Result<_, _>>()?,
            methods: methods
                .iter()
                .map(|range| self.lower_function_at(*range))
                .collect::<Result<_, _>>()?,
            span: self.span_for_range(*ast_span)?,
        })
    }

    fn lower_pattern_node(&self, node: &SyntaxNode) -> Result<PatternNode, SyntaxLowerError> {
        let Some(AstLoweringData::Pattern(pattern)) = node.data.as_ref() else {
            return Err(SyntaxLowerError::UnsupportedContext(node.kind));
        };
        match pattern {
            SyntaxPattern::Literal { token: token_range } => {
                let token = self.token_for_range(*token_range)?;
                let literal = match token.kind() {
                    TokenType::Int(value) => LiteralNode::Integer(*value),
                    TokenType::Float(value) => LiteralNode::Float(*value),
                    TokenType::Bool(value) => LiteralNode::Boolean(*value),
                    TokenType::Char(value) => LiteralNode::Char(*value),
                    TokenType::Str(value) => LiteralNode::String(value.clone()),
                    _ => return Err(SyntaxLowerError::MissingToken(*token_range)),
                };
                Ok(PatternNode::Literal(literal))
            }
            SyntaxPattern::Identifier { token: token_range } => {
                let token = self.token_for_range(*token_range)?;
                let TokenType::Id(name) = token.kind() else {
                    return Err(SyntaxLowerError::MissingToken(*token_range));
                };
                Ok(PatternNode::Identifier(name.clone()))
            }
            SyntaxPattern::Wildcard => Ok(PatternNode::Wildcard),
            SyntaxPattern::EnumVariant { name, args } => {
                let name_token = self.token_for_range(*name)?;
                let name = match name_token.kind() {
                    TokenType::Id(name) => name.clone(),
                    TokenType::None => "none".to_owned(),
                    _ => return Err(SyntaxLowerError::MissingToken(*name)),
                };
                Ok(PatternNode::EnumVariant {
                    name,
                    args: args
                        .iter()
                        .map(|range| self.lower_pattern_at(*range))
                        .collect::<Result<_, _>>()?,
                })
            }
            SyntaxPattern::List { elements, rest } => Ok(PatternNode::List {
                elements: elements
                    .iter()
                    .map(|range| self.lower_pattern_at(*range))
                    .collect::<Result<_, _>>()?,
                rest: rest
                    .map(|range| self.lower_pattern_at(range).map(Box::new))
                    .transpose()?,
            }),
        }
    }

    fn lower_match_arm_node(&self, node: &SyntaxNode) -> Result<MatchArm, SyntaxLowerError> {
        let Some(AstLoweringData::MatchArm {
            pattern,
            guard,
            body,
            body_is_expression,
        }) = node.data.as_ref()
        else {
            return Err(SyntaxLowerError::UnsupportedContext(node.kind));
        };
        let body = if *body_is_expression {
            let expression = self.lower_expression_at(*body)?;
            vec![StatementNode {
                span: expression.span,
                kind: StatementKind::Expression(expression),
            }]
        } else {
            let body_statement = self.lower_statement_at(*body)?;
            match body_statement {
                StatementNode {
                    kind: StatementKind::Block(statements),
                    ..
                } => statements,
                statement => vec![statement],
            }
        };
        Ok(MatchArm {
            pattern: self.lower_pattern_at(*pattern)?,
            guard: guard
                .map(|range| self.lower_expression_at(range))
                .transpose()?,
            body,
        })
    }

    fn lower_trait_reference_at(&self, range: ByteRange) -> Result<TraitRef, SyntaxLowerError> {
        let Some(node) = self.find_typed_node(self.root(), range, |data| {
            matches!(data, AstLoweringData::TraitReference { .. })
        }) else {
            return Err(SyntaxLowerError::MissingContext(range));
        };
        let AstLoweringData::TraitReference {
            name,
            type_arguments,
        } = node
            .data
            .as_ref()
            .ok_or(SyntaxLowerError::UnsupportedContext(node.kind))?
        else {
            return Err(SyntaxLowerError::UnsupportedContext(node.kind));
        };
        Ok(TraitRef {
            name: self.identifier_for_range(*name)?,
            type_args: type_arguments
                .iter()
                .map(|range| self.lower_type_at(*range))
                .collect::<Result<_, _>>()?,
            span: self.span_for_range(*name)?,
        })
    }

    fn lower_class_field_at(&self, range: ByteRange) -> Result<Field, SyntaxLowerError> {
        let Some(node) = self.find_typed_node(self.root(), range, |data| {
            matches!(data, AstLoweringData::Field { .. })
        }) else {
            return Err(SyntaxLowerError::MissingContext(range));
        };
        let AstLoweringData::Field {
            name,
            type_range,
            is_generic_param,
            is_const,
            default_value,
            where_clause,
        } = node
            .data
            .as_ref()
            .ok_or(SyntaxLowerError::UnsupportedContext(node.kind))?
        else {
            return Err(SyntaxLowerError::UnsupportedContext(node.kind));
        };
        Ok(Field {
            name: self.identifier_for_range(*name)?,
            type_: self.lower_type_at(*type_range)?,
            is_generic_param: *is_generic_param,
            is_const: *is_const,
            default_value: default_value
                .map(|range| self.lower_expression_at(range))
                .transpose()?,
            where_clause: where_clause
                .map(|range| self.lower_where_clause_at(range))
                .transpose()?,
        })
    }

    fn lower_class_method_at(&self, range: ByteRange) -> Result<FunctionNode, SyntaxLowerError> {
        let Some(node) = self.find_typed_node(self.root(), range, |data| {
            matches!(data, AstLoweringData::ClassMethod { .. })
        }) else {
            return Err(SyntaxLowerError::MissingContext(range));
        };
        let AstLoweringData::ClassMethod { function } = node
            .data
            .as_ref()
            .ok_or(SyntaxLowerError::UnsupportedContext(node.kind))?
        else {
            return Err(SyntaxLowerError::UnsupportedContext(node.kind));
        };
        self.lower_function_at(*function)
    }

    fn lower_module_path(&self, range: ByteRange) -> Result<String, SyntaxLowerError> {
        let mut path = String::new();
        let mut found = false;
        for token in &self.tokens {
            let token_range = token.range();
            if token_range.start < range.start || token_range.end > range.end {
                continue;
            }
            match token.kind() {
                TokenType::Whitespace
                | TokenType::NewLine
                | TokenType::LineComment(_)
                | TokenType::MultilineComment(_)
                | TokenType::Eof => {}
                _ => {
                    path.push_str(token.text(self.source()));
                    found = true;
                }
            }
        }
        if found {
            Ok(path)
        } else {
            Err(SyntaxLowerError::MissingToken(range))
        }
    }

    fn lower_import_spec(
        &self,
        spec: &SyntaxImportSpec,
        module_path: &str,
    ) -> Result<ImportSpec, SyntaxLowerError> {
        let identifier = |range| -> Result<String, SyntaxLowerError> {
            let token = self.token_for_range(range)?;
            let TokenType::Id(name) = token.kind() else {
                return Err(SyntaxLowerError::MissingToken(range));
            };
            Ok(name.clone())
        };
        Ok(match spec {
            SyntaxImportSpec::Module { alias } => ImportSpec::Module {
                alias: match alias {
                    SyntaxModuleAlias::Default => Some(
                        module_path
                            .split('.')
                            .next_back()
                            .unwrap_or(module_path)
                            .to_owned(),
                    ),
                    SyntaxModuleAlias::Hidden => None,
                    SyntaxModuleAlias::Explicit(range) => Some(identifier(*range)?),
                },
            },
            SyntaxImportSpec::Item { item, alias } => ImportSpec::Item {
                item: identifier(*item)?,
                alias: (*alias).map(&identifier).transpose()?,
            },
            SyntaxImportSpec::Items { items } => ImportSpec::Items {
                items: items
                    .iter()
                    .map(|(item, alias)| {
                        Ok((identifier(*item)?, (*alias).map(&identifier).transpose()?))
                    })
                    .collect::<Result<_, SyntaxLowerError>>()?,
            },
            SyntaxImportSpec::Wildcard => ImportSpec::Wildcard,
        })
    }

    fn lower_function_node(&self, node: &SyntaxNode) -> Result<FunctionNode, SyntaxLowerError> {
        let Some(AstLoweringData::Function {
            name,
            ast_span,
            type_parameters,
            parameters,
            return_type,
            return_type_span,
            where_clause,
            body,
            is_common,
        }) = node.data.as_ref()
        else {
            return Err(SyntaxLowerError::UnsupportedContext(node.kind));
        };

        let name_token = self.token_for_range(*name)?;
        let TokenType::Id(name) = name_token.kind() else {
            return Err(SyntaxLowerError::MissingToken(*name));
        };
        let type_params = type_parameters
            .iter()
            .map(|range| self.lower_type_parameter_at(*range))
            .collect::<Result<_, _>>()?;
        let params = parameters
            .iter()
            .map(|range| self.lower_parameter_at(*range))
            .collect::<Result<_, _>>()?;
        let return_type = return_type
            .map(|range| self.lower_type_at(range))
            .transpose()?
            .unwrap_or(TypeNode {
                kind: TypeKind::Primitive(PrimitiveType::Void),
                span: self.span_for_range(*return_type_span)?,
            });
        let where_clause = where_clause
            .map(|range| self.lower_where_clause_at(range))
            .transpose()?;
        let body = if let Some(range) = body {
            let statement = self.lower_statement_at(*range)?;
            let StatementKind::Block(statements) = statement.kind else {
                return Err(SyntaxLowerError::MissingContext(*range));
            };
            statements
        } else {
            Vec::new()
        };

        Ok(FunctionNode {
            name: name.clone(),
            type_params,
            params,
            return_type,
            body,
            span: self.span_for_range(*ast_span)?,
            is_common: *is_common,
            where_clause,
        })
    }

    fn lower_type_parameter_at(
        &self,
        range: ByteRange,
    ) -> Result<(String, Vec<TraitBound>), SyntaxLowerError> {
        let Some(node) = self.find_typed_node(self.root(), range, |data| {
            matches!(data, AstLoweringData::TypeParameter { .. })
        }) else {
            return Err(SyntaxLowerError::MissingContext(range));
        };
        let AstLoweringData::TypeParameter { name, bounds } = node
            .data
            .as_ref()
            .ok_or(SyntaxLowerError::UnsupportedContext(node.kind))?
        else {
            return Err(SyntaxLowerError::UnsupportedContext(node.kind));
        };
        Ok((
            self.identifier_for_range(*name)?,
            bounds
                .iter()
                .map(|range| self.lower_trait_bound_at(*range))
                .collect::<Result<_, _>>()?,
        ))
    }

    fn lower_trait_bound_at(&self, range: ByteRange) -> Result<TraitBound, SyntaxLowerError> {
        let Some(node) = self.find_typed_node(self.root(), range, |data| {
            matches!(data, AstLoweringData::TraitBound { .. })
        }) else {
            return Err(SyntaxLowerError::MissingContext(range));
        };
        let AstLoweringData::TraitBound {
            name,
            type_arguments,
        } = node
            .data
            .as_ref()
            .ok_or(SyntaxLowerError::UnsupportedContext(node.kind))?
        else {
            return Err(SyntaxLowerError::UnsupportedContext(node.kind));
        };
        let span = self.span_for_range(*name)?;
        let name = self.identifier_for_range(*name)?;
        Ok(TraitBound {
            name,
            type_params: type_arguments
                .iter()
                .map(|range| self.lower_type_at(*range))
                .collect::<Result<_, _>>()?,
            span,
        })
    }

    fn lower_parameter_at(&self, range: ByteRange) -> Result<Param, SyntaxLowerError> {
        let Some(node) = self.find_typed_node(self.root(), range, |data| {
            matches!(data, AstLoweringData::Parameter { .. })
        }) else {
            return Err(SyntaxLowerError::MissingContext(range));
        };
        let AstLoweringData::Parameter {
            name,
            type_range,
            default_value,
        } = node
            .data
            .as_ref()
            .ok_or(SyntaxLowerError::UnsupportedContext(node.kind))?
        else {
            return Err(SyntaxLowerError::UnsupportedContext(node.kind));
        };
        Ok(Param {
            name: self.identifier_for_range(*name)?,
            type_: self.lower_type_at(*type_range)?,
            default_value: default_value
                .map(|range| self.lower_expression_at(range))
                .transpose()?,
        })
    }

    fn lower_where_clause_at(&self, range: ByteRange) -> Result<WhereClause, SyntaxLowerError> {
        let Some(node) = self.find_typed_node(self.root(), range, |data| {
            matches!(data, AstLoweringData::WhereClause { .. })
        }) else {
            return Err(SyntaxLowerError::MissingContext(range));
        };
        let AstLoweringData::WhereClause { predicates } = node
            .data
            .as_ref()
            .ok_or(SyntaxLowerError::UnsupportedContext(node.kind))?
        else {
            return Err(SyntaxLowerError::UnsupportedContext(node.kind));
        };
        Ok(WhereClause {
            predicates: predicates
                .iter()
                .map(|range| self.lower_expression_at(*range))
                .collect::<Result<_, _>>()?,
            span: self.span_for_range(node.range)?,
        })
    }

    fn lower_statement_children(
        &self,
        node: &SyntaxNode,
    ) -> Result<Vec<StatementNode>, SyntaxLowerError> {
        let mut statements = Vec::new();
        for child in &node.children {
            let SyntaxElement::Node(child) = child else {
                continue;
            };
            if child.data.as_ref().is_some_and(is_statement_data) {
                statements.push(self.lower_statement_node(child)?);
            } else {
                statements.extend(self.lower_statement_children(child)?);
            }
        }
        Ok(statements)
    }

    fn token_for_range(&self, range: ByteRange) -> Result<&SyntaxToken, SyntaxLowerError> {
        let index = self
            .tokens
            .partition_point(|token| token.range().start < range.start);
        self.tokens
            .get(index)
            .filter(|token| token.range() == range)
            .ok_or(SyntaxLowerError::MissingToken(range))
    }

    fn identifier_for_range(&self, range: ByteRange) -> Result<String, SyntaxLowerError> {
        match self.token_for_range(range)?.kind() {
            TokenType::Id(name) => Ok(name.clone()),
            TokenType::Underscore => Ok("_".to_owned()),
            _ => Err(SyntaxLowerError::MissingToken(range)),
        }
    }

    fn span_for_range(&self, range: ByteRange) -> Result<crate::lexer::Span, SyntaxLowerError> {
        Ok(crate::lexer::Span::new(range.start, range.end))
    }
}

fn binary_operator(token: &TokenType) -> Option<BinaryOp> {
    Some(match token {
        TokenType::Plus => BinaryOp::Add,
        TokenType::Minus => BinaryOp::Subtract,
        TokenType::Star => BinaryOp::Multiply,
        TokenType::Slash => BinaryOp::Divide,
        TokenType::Percent => BinaryOp::Modulo,
        TokenType::StarStar => BinaryOp::Exponent,
        TokenType::Eq => BinaryOp::Assign,
        TokenType::EqEq => BinaryOp::Equal,
        TokenType::NotEq => BinaryOp::NotEqual,
        TokenType::Lt => BinaryOp::Less,
        TokenType::Gt => BinaryOp::Greater,
        TokenType::Le => BinaryOp::LessEqual,
        TokenType::Ge => BinaryOp::GreaterEqual,
        TokenType::And => BinaryOp::LogicalAnd,
        TokenType::Or => BinaryOp::LogicalOr,
        TokenType::In => BinaryOp::In,
        TokenType::PlusEq => BinaryOp::AddAssign,
        TokenType::MinusEq => BinaryOp::SubtractAssign,
        TokenType::StarEq => BinaryOp::MultiplyAssign,
        TokenType::SlashEq => BinaryOp::DivideAssign,
        TokenType::PercentEq => BinaryOp::ModuloAssign,
        _ => return None,
    })
}

fn primitive_type(name: &str) -> Option<PrimitiveType> {
    Some(match name {
        "int" => PrimitiveType::Int,
        "float" => PrimitiveType::Float,
        "bool" => PrimitiveType::Bool,
        "char" => PrimitiveType::Char,
        "byte" => PrimitiveType::Byte,
        "bytes" => PrimitiveType::Bytes,
        "string" => PrimitiveType::Str,
        "void" => PrimitiveType::Void,
        "auto" => PrimitiveType::Auto,
        _ => return None,
    })
}

fn generic_target_name(expression: &ExpressionNode) -> Option<String> {
    match &expression.kind {
        ExpressionKind::Identifier(name) => Some(name.clone()),
        ExpressionKind::FieldAccess { expr, field } => {
            generic_target_name(expr).map(|base| format!("{base}.{field}"))
        }
        _ => None,
    }
}

fn is_statement_data(data: &AstLoweringData) -> bool {
    matches!(
        data,
        AstLoweringData::VariableDeclaration { .. }
            | AstLoweringData::Function { .. }
            | AstLoweringData::ExpressionStatement { .. }
            | AstLoweringData::ReturnStatement { .. }
            | AstLoweringData::BreakStatement { .. }
            | AstLoweringData::ContinueStatement { .. }
            | AstLoweringData::Block
            | AstLoweringData::IfStatement { .. }
            | AstLoweringData::WhileStatement { .. }
            | AstLoweringData::ForStatement { .. }
            | AstLoweringData::MatchStatement { .. }
            | AstLoweringData::Import { .. }
    )
}

fn is_compilation_unit_wrapper(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::Declaration
            | SyntaxKind::FunctionDeclaration
            | SyntaxKind::ClassDeclaration
            | SyntaxKind::InterfaceDeclaration
            | SyntaxKind::EnumDeclaration
            | SyntaxKind::ImportDeclaration
            | SyntaxKind::TestDeclaration
    )
}

fn is_function_data(data: &AstLoweringData) -> bool {
    matches!(data, AstLoweringData::Function { .. })
}

fn is_test_data(data: &AstLoweringData) -> bool {
    matches!(data, AstLoweringData::Test { .. })
}

fn is_enum_data(data: &AstLoweringData) -> bool {
    matches!(data, AstLoweringData::Enum { .. })
}

fn is_class_data(data: &AstLoweringData) -> bool {
    matches!(data, AstLoweringData::Class { .. })
}

fn is_interface_data(data: &AstLoweringData) -> bool {
    matches!(data, AstLoweringData::Interface { .. })
}

fn is_pattern_data(data: &AstLoweringData) -> bool {
    matches!(data, AstLoweringData::Pattern(_))
}

fn is_match_arm_data(data: &AstLoweringData) -> bool {
    matches!(data, AstLoweringData::MatchArm { .. })
}

fn is_expression_data(data: &AstLoweringData) -> bool {
    matches!(
        data,
        AstLoweringData::Name { .. }
            | AstLoweringData::Literal { .. }
            | AstLoweringData::Unary { .. }
            | AstLoweringData::Binary { .. }
            | AstLoweringData::Lambda { .. }
            | AstLoweringData::IfExpression { .. }
            | AstLoweringData::MatchExpression { .. }
            | AstLoweringData::Parenthesized { .. }
            | AstLoweringData::Tuple { .. }
            | AstLoweringData::List { .. }
            | AstLoweringData::Map { .. }
            | AstLoweringData::Set { .. }
            | AstLoweringData::Call { .. }
            | AstLoweringData::FieldAccess { .. }
            | AstLoweringData::Index { .. }
            | AstLoweringData::Slice { .. }
            | AstLoweringData::Generic { .. }
    )
}

fn is_type_data(data: &AstLoweringData) -> bool {
    matches!(
        data,
        AstLoweringData::TypeName { .. }
            | AstLoweringData::TypeReference { .. }
            | AstLoweringData::TypeContainer { .. }
            | AstLoweringData::FunctionType { .. }
    )
}

fn count_line_breaks(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut count = 0;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\n' => count += 1,
            b'\r' => {
                count += 1;
                if bytes.get(index + 1) == Some(&b'\n') {
                    index += 1;
                }
            }
            _ => {}
        }
        index += 1;
    }
    count
}

fn collect_test_nodes<'a>(node: &'a SyntaxNode, found: &mut Vec<&'a SyntaxNode>) {
    if node
        .data
        .as_ref()
        .is_some_and(|data| matches!(data, AstLoweringData::Test { .. }))
    {
        found.push(node);
        return;
    }
    for child in &node.children {
        if let SyntaxElement::Node(child) = child {
            collect_test_nodes(child, found);
        }
    }
}

#[derive(Clone, Copy)]
enum TreeMarker {
    Open(usize),
    Token(usize),
    Close(usize),
}

struct PendingNode {
    kind: SyntaxKind,
    range: ByteRange,
    event_index: Option<usize>,
    data: Option<AstLoweringData>,
    children: Vec<PendingElement>,
}

enum PendingElement {
    Node(usize),
    Token(usize),
}

fn build_tree(
    root_range: ByteRange,
    tokens: &[SyntaxToken],
    mut events: Vec<SyntaxNodeEvent>,
) -> SyntaxNode {
    let mut markers = Vec::with_capacity(events.len() * 2 + tokens.len());
    for (index, event) in events.iter().enumerate() {
        if event.range.start <= event.range.end && event.range.end <= root_range.end {
            if event.range.start == event.range.end {
                markers.push((event.range.start, 1_u8, index, TreeMarker::Open(index)));
                markers.push((event.range.end, 2_u8, index, TreeMarker::Close(index)));
            } else {
                markers.push((event.range.start, 1_u8, index, TreeMarker::Open(index)));
                markers.push((event.range.end, 0_u8, index, TreeMarker::Close(index)));
            }
        }
    }
    for (index, token) in tokens.iter().enumerate() {
        let range = token.range();
        if range.start <= root_range.end {
            // Tokens at a zero-width insertion point follow the empty context's
            // close marker, so they do not become children of a missing node.
            markers.push((range.start, 3_u8, index, TreeMarker::Token(index)));
        }
    }
    markers.sort_by(|left, right| {
        let position = left.0.cmp(&right.0);
        if position != std::cmp::Ordering::Equal {
            return position;
        }
        let priority = left.1.cmp(&right.1);
        if priority != std::cmp::Ordering::Equal {
            return priority;
        }
        match (left.3, right.3) {
            // Completed grammar events arrive inner-first. At identical ranges,
            // the later event is the outer wrapper and opens first.
            (TreeMarker::Open(_), TreeMarker::Open(_)) => right.2.cmp(&left.2),
            // Close equal-range wrappers in the opposite order.
            (TreeMarker::Close(_), TreeMarker::Close(_)) => left.2.cmp(&right.2),
            _ => left.2.cmp(&right.2),
        }
    });

    let mut nodes = vec![PendingNode {
        kind: SyntaxKind::Root,
        range: root_range,
        event_index: None,
        data: None,
        children: Vec::new(),
    }];
    let mut stack = vec![0_usize];
    let mut active_position = vec![None; events.len()];
    for (_, _, _, marker) in markers {
        match marker {
            TreeMarker::Open(event_index) => {
                let (kind, range) = {
                    let event = &events[event_index];
                    (event.kind, event.range)
                };
                // A malformed overlapping annotation is retained under the
                // smallest active node that contains it. The grammar should
                // normally emit properly nested ranges.
                while stack.len() > 1
                    && range.end > nodes[*stack.last().expect("root remains")].range.end
                {
                    let popped = stack.pop().expect("non-root stack node");
                    if let Some(open_event) = nodes[popped].event_index {
                        active_position[open_event] = None;
                    }
                }
                let node_index = nodes.len();
                let data = events[event_index].data.take();
                nodes.push(PendingNode {
                    kind,
                    range,
                    event_index: Some(event_index),
                    data,
                    children: Vec::new(),
                });
                nodes[*stack.last().expect("root remains")]
                    .children
                    .push(PendingElement::Node(node_index));
                active_position[event_index] = Some(stack.len());
                stack.push(node_index);
            }
            TreeMarker::Token(token_index) => {
                let range = tokens[token_index].range();
                while stack.len() > 1 {
                    let active = nodes[*stack.last().expect("root remains")].range;
                    if range.start < active.end && range.end <= active.end {
                        break;
                    }
                    let popped = stack.pop().expect("non-root stack node");
                    if let Some(open_event) = nodes[popped].event_index {
                        active_position[open_event] = None;
                    }
                }
                nodes[*stack.last().expect("root remains")]
                    .children
                    .push(PendingElement::Token(token_index));
            }
            TreeMarker::Close(event_index) => {
                if let Some(position) = active_position[event_index] {
                    while stack.len() > position {
                        let popped = stack.pop().expect("active syntax stack node");
                        if let Some(open_event) = nodes[popped].event_index {
                            active_position[open_event] = None;
                        }
                    }
                }
            }
        }
    }

    // Building the public tree recursively from the node arena preserves event
    // nesting while making token traversal linear in the output size.
    fn finish(index: usize, nodes: &mut [PendingNode]) -> SyntaxNode {
        let kind = nodes[index].kind;
        let range = nodes[index].range;
        let data = nodes[index].data.take();
        let pending_children = std::mem::take(&mut nodes[index].children);
        let children = pending_children
            .into_iter()
            .map(|child| match child {
                PendingElement::Node(child_index) => {
                    SyntaxElement::Node(Box::new(finish(child_index, nodes)))
                }
                PendingElement::Token(token_index) => SyntaxElement::Token(token_index),
            })
            .collect();
        SyntaxNode {
            kind,
            range,
            data,
            children,
        }
    }
    finish(0, &mut nodes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Span;

    #[test]
    fn syntax_tree_keeps_raw_token_text_and_nested_context() {
        let source = Arc::new(SourceText::new("x + 2 // note\n".to_owned()));
        let tokens = vec![
            Token::new(TokenType::Id("x".into()), Span::new(0, 1)),
            Token::new(TokenType::Whitespace, Span::new(1, 2)),
            Token::new(TokenType::Plus, Span::new(2, 3)),
            Token::new(TokenType::Whitespace, Span::new(3, 4)),
            Token::new(TokenType::Int(2), Span::new(4, 5)),
            Token::new(TokenType::Whitespace, Span::new(5, 6)),
            Token::new(TokenType::LineComment("note".into()), Span::new(6, 13)),
            Token::new(TokenType::NewLine, Span::new(13, 14)),
        ];
        let tree = SyntaxTree::new(
            source,
            tokens,
            &[SyntaxNodeEvent {
                kind: SyntaxKind::BinaryExpression,
                range: ByteRange::new(0, 5),
                data: None,
            }],
        );

        assert_eq!(tree.token_text(6), Some("// note"));
        assert!(matches!(tree.root.children()[0], SyntaxElement::Node(_)));
        assert_eq!(tree.tokens().len(), 8);
    }

    #[test]
    fn parse_source_retains_all_bytes_and_expression_context() {
        let source = "auto value = add((1 + 2), [3, 4]) // keep\n";
        let output = parse_source(source);
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        let reconstructed: String = output
            .tree
            .tokens()
            .iter()
            .map(|token| token.text(output.tree.source()))
            .collect();
        assert_eq!(reconstructed, source);

        fn contains(node: &SyntaxNode, kind: SyntaxKind) -> bool {
            node.kind() == kind
                || node.children().iter().any(|child| match child {
                    SyntaxElement::Node(child) => contains(child, kind),
                    SyntaxElement::Token(_) => false,
                })
        }
        assert!(contains(output.tree.root(), SyntaxKind::CallArguments));
        assert!(contains(
            output.tree.root(),
            SyntaxKind::ParenthesizedExpression
        ));
        assert!(contains(output.tree.root(), SyntaxKind::ListLiteral));
    }

    #[test]
    fn compilation_unit_lowering_dispatches_every_top_level_ast_kind() {
        let source = "import std.io\nfunc read() returns int { return 1 }\nclass Box {}\ninterface Reader {}\nenum State { Ready }\ntest \"works\" {}\nauto count = 2\n";
        let output = parse_source(source);
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        let lowered = output.lower().expect("lower compilation unit from syntax");
        assert_eq!(lowered.len(), 7);
        assert!(matches!(
            &lowered[0],
            AstNode::Statement(StatementNode {
                kind: StatementKind::Import { module_path, .. },
                ..
            }) if module_path == "std"
        ));
        assert!(matches!(&lowered[1], AstNode::Function(function) if function.name == "read"));
        assert!(matches!(&lowered[2], AstNode::Class { name, .. } if name == "Box"));
        assert!(matches!(&lowered[3], AstNode::Interface { name, .. } if name == "Reader"));
        assert!(matches!(&lowered[4], AstNode::Enum { name, .. } if name == "State"));
        assert!(matches!(&lowered[5], AstNode::Test { name, .. } if name == "works"));
        assert!(matches!(
            &lowered[6],
            AstNode::Statement(StatementNode {
                kind: StatementKind::AutoDecl(name, ..),
                ..
            }) if name == "count"
        ));
    }

    #[test]
    fn compilation_unit_lowering_rejects_untyped_top_level_contexts() {
        let tree = SyntaxTree::new(
            Arc::new(SourceText::new("x".to_owned())),
            vec![Token::new(TokenType::Id("x".into()), Span::new(0, 1))],
            &[SyntaxNodeEvent {
                kind: SyntaxKind::Declaration,
                range: ByteRange::new(0, 1),
                data: None,
            }],
        );
        assert_eq!(
            tree.lower_compilation_unit(),
            Err(SyntaxLowerError::UnsupportedContext(
                SyntaxKind::Declaration
            ))
        );
    }

    fn find_data<'a>(
        node: &'a SyntaxNode,
        predicate: &impl Fn(&AstLoweringData) -> bool,
    ) -> Option<&'a SyntaxNode> {
        if node.lowering_data().is_some_and(predicate) {
            return Some(node);
        }
        node.children().iter().find_map(|child| match child {
            SyntaxElement::Node(child) => find_data(child, predicate),
            SyntaxElement::Token(_) => None,
        })
    }

    fn lower_data(source: &str, predicate: impl Fn(&AstLoweringData) -> bool) -> ExpressionNode {
        let output = parse_source(source);
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        let node = find_data(output.tree.root(), &predicate).expect("typed expression context");
        output
            .tree
            .lower_expression(node)
            .expect("lower typed context")
    }

    fn lower_type_data(source: &str, predicate: impl Fn(&AstLoweringData) -> bool) -> TypeNode {
        let output = parse_source(source);
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        let node = find_data(output.tree.root(), &predicate).expect("typed type context");
        output.tree.lower_type(node).expect("lower typed type")
    }

    fn lower_statement_data(
        source: &str,
        predicate: impl Fn(&AstLoweringData) -> bool,
    ) -> StatementNode {
        let output = parse_source(source);
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        let node = find_data(output.tree.root(), &predicate).expect("typed statement context");
        output.tree.lower_statement(node).unwrap_or_else(|error| {
            panic!(
                "lower statement from {source:?}, data {:?}: {error}",
                node.lowering_data()
            )
        })
    }

    #[test]
    fn typed_expression_contexts_lower_to_existing_ast_nodes() {
        let binary = lower_data("auto x = 1 + 2", |data| {
            matches!(data, AstLoweringData::Binary { .. })
        });
        assert!(matches!(binary.kind, ExpressionKind::Binary { .. }));

        let tuple = lower_data("auto x = (1, 2)", |data| {
            matches!(data, AstLoweringData::Tuple { .. })
        });
        assert!(matches!(tuple.kind, ExpressionKind::TupleLiteral(values) if values.len() == 2));

        let list = lower_data("auto x = [1, 2]", |data| {
            matches!(data, AstLoweringData::List { .. })
        });
        assert!(matches!(list.kind, ExpressionKind::ListLiteral(values) if values.len() == 2));

        let empty_list = lower_data("auto x = []", |data| {
            matches!(data, AstLoweringData::List { .. })
        });
        assert!(
            matches!(empty_list.kind, ExpressionKind::ListLiteral(values) if values.is_empty())
        );

        let set = lower_data("auto x = {1, 2}", |data| {
            matches!(data, AstLoweringData::Set { .. })
        });
        assert!(matches!(set.kind, ExpressionKind::SetLiteral(values) if values.len() == 2));

        let map = lower_data("auto x = {1: 2}", |data| {
            matches!(data, AstLoweringData::Map { .. })
        });
        assert!(
            matches!(map.kind, ExpressionKind::MapLiteral { entries, .. } if entries.len() == 1)
        );
    }

    #[test]
    fn typed_lambda_and_conditional_expressions_lower_recursively() {
        let lambda_source =
            "auto increment = func(int value) where { value > 0 } returns int { return value + 1 }";
        let lambda_output = parse_source(lambda_source);
        assert!(
            lambda_output.errors.is_empty(),
            "{:?}",
            lambda_output.errors
        );
        let lambda_node = find_data(lambda_output.tree.root(), &|data| {
            matches!(data, AstLoweringData::Lambda { .. })
        })
        .expect("lambda syntax data");
        let lambda = lambda_output
            .tree
            .lower_expression(lambda_node)
            .expect("lower lambda expression");
        let ExpressionKind::Lambda {
            params,
            return_type,
            body,
            where_clause,
        } = &lambda.kind
        else {
            panic!("expected lowered lambda expression");
        };
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].name, "value");
        assert!(matches!(
            &params[0].type_.kind,
            TypeKind::Primitive(PrimitiveType::Int)
        ));
        assert!(matches!(
            &return_type.kind,
            TypeKind::Primitive(PrimitiveType::Int)
        ));
        assert_eq!(where_clause.as_ref().unwrap().predicates.len(), 1);
        assert!(matches!(
            body.as_slice(),
            [StatementNode {
                kind: StatementKind::Return(Some(ExpressionNode {
                    kind: ExpressionKind::Binary { .. },
                    ..
                })),
                ..
            }]
        ));
        let lowered_lambda_unit = lambda_output
            .lower()
            .expect("lower compilation unit containing a lambda");
        assert!(matches!(
            &lowered_lambda_unit[..],
            [AstNode::Statement(StatementNode {
                kind: StatementKind::AutoDecl(
                    _,
                    _,
                    ExpressionNode {
                        kind: ExpressionKind::Lambda { .. },
                        ..
                    }
                ),
                ..
            })]
        ));

        let if_source = "auto selected = if ready { 1 } else if retry { 2 } else { 3 }";
        let if_output = parse_source(if_source);
        assert!(if_output.errors.is_empty(), "{:?}", if_output.errors);
        let if_node = find_data(if_output.tree.root(), &|data| {
            matches!(data, AstLoweringData::IfExpression { .. })
        })
        .expect("conditional expression syntax data");
        let conditional = if_output
            .tree
            .lower_expression(if_node)
            .expect("lower conditional expression");
        let ExpressionKind::If {
            cond,
            then_expr,
            else_expr,
        } = &conditional.kind
        else {
            panic!("expected lowered conditional expression");
        };
        assert!(matches!(&cond.kind, ExpressionKind::Identifier(name) if name == "ready"));
        assert!(matches!(
            &then_expr.kind,
            ExpressionKind::Literal(LiteralNode::Integer(1))
        ));
        assert!(matches!(
            &else_expr.kind,
            ExpressionKind::If {
                cond,
                then_expr,
                else_expr,
            } if matches!(&cond.kind, ExpressionKind::Identifier(name) if name == "retry")
                && matches!(&then_expr.kind, ExpressionKind::Literal(LiteralNode::Integer(2)))
                && matches!(&else_expr.kind, ExpressionKind::Literal(LiteralNode::Integer(3)))
        ));
        let lowered_if_unit = if_output
            .lower()
            .expect("lower compilation unit containing a conditional expression");
        assert!(matches!(
            &lowered_if_unit[..],
            [AstNode::Statement(StatementNode {
                kind: StatementKind::AutoDecl(
                    _,
                    _,
                    ExpressionNode {
                        kind: ExpressionKind::If { .. },
                        ..
                    }
                ),
                ..
            })]
        ));
    }

    #[test]
    fn typed_postfix_contexts_lower_recursively() {
        let call = lower_data("auto x = foo(1, 2)", |data| {
            matches!(data, AstLoweringData::Call { .. })
        });
        assert!(matches!(call.kind, ExpressionKind::Call { args, .. } if args.len() == 2));

        let field = lower_data("auto x = value.field", |data| {
            matches!(data, AstLoweringData::FieldAccess { .. })
        });
        assert!(
            matches!(field.kind, ExpressionKind::FieldAccess { field, .. } if field == "field")
        );

        let index = lower_data("auto x = values[0]", |data| {
            matches!(data, AstLoweringData::Index { .. })
        });
        assert!(matches!(index.kind, ExpressionKind::ListAccess { .. }));

        let postfix = lower_data("value++", |data| {
            matches!(data, AstLoweringData::Unary { postfix: true, .. })
        });
        assert!(matches!(
            postfix.kind,
            ExpressionKind::Unary { postfix: true, .. }
        ));
    }

    #[test]
    fn composite_field_callee_lowers_inside_call() {
        let output = parse_source("auto result = sum.to_string()");
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        let lowered = output.lower().expect("lower compilation unit");
        assert!(matches!(
            &lowered[..],
            [AstNode::Statement(StatementNode {
                kind: StatementKind::AutoDecl(
                    _,
                    _,
                    ExpressionNode {
                        kind: ExpressionKind::Call { func, args },
                        ..
                    }
                ),
                ..
            })] if args.is_empty()
                && matches!(
                    &func.kind,
                    ExpressionKind::FieldAccess { field, .. } if field == "to_string"
                )
        ));
        let AstNode::Statement(StatementNode {
            kind: StatementKind::AutoDecl(_, _, lowered_call),
            ..
        }) = &lowered[0]
        else {
            panic!("expected lowered auto declaration");
        };
        assert_eq!(lowered_call.span.byte_range, Some(ByteRange::new(14, 29)));
        let ExpressionKind::Call { func, .. } = &lowered_call.kind else {
            panic!("expected lowered call");
        };
        assert_eq!(func.span.byte_range, Some(ByteRange::new(14, 27)));
    }

    #[test]
    fn unary_and_binary_contexts_match_ast_ranges_around_parentheses() {
        let output = parse_source("auto result = !(5 > 1)");
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        let lowered = output.lower().expect("lower compilation unit");
        assert!(matches!(
            &lowered[..],
            [AstNode::Statement(StatementNode {
                kind: StatementKind::AutoDecl(
                    _,
                    _,
                    ExpressionNode {
                        kind: ExpressionKind::Unary {
                            expr,
                            postfix: false,
                            ..
                        },
                        ..
                    }
                ),
                ..
            })] if matches!(
                &expr.kind,
                ExpressionKind::Binary {
                    op: BinaryOp::Greater,
                    ..
                }
            )
        ));
    }

    #[test]
    fn typed_match_expressions_and_slices_lower_with_optional_ranges() {
        let match_source = "func inspect(Box value) returns int {\n    auto result = match value {\n        Some(item) if item > 0 { item }\n        none { return 0 }\n        _ { 1 }\n    }\n    return result\n}";
        let match_output = parse_source(match_source);
        assert!(match_output.errors.is_empty(), "{:?}", match_output.errors);
        let match_node = find_data(match_output.tree.root(), &|data| {
            matches!(data, AstLoweringData::MatchExpression { .. })
        })
        .expect("match expression syntax data");
        let lowered = match_output
            .tree
            .lower_expression(match_node)
            .expect("lower match expression");
        let ExpressionKind::Match { expr, arms } = &lowered.kind else {
            panic!("expected lowered match expression");
        };
        assert!(matches!(&expr.kind, ExpressionKind::Identifier(name) if name == "value"));
        assert_eq!(arms.len(), 3);
        assert!(matches!(
            &arms[0].pattern,
            PatternNode::EnumVariant { name, args }
                if name == "Some" && matches!(args.as_slice(), [PatternNode::Identifier(item)] if item == "item")
        ));
        assert!(arms[0].guard.is_some());
        assert!(matches!(
            arms[0].body.as_slice(),
            [StatementNode {
                kind: StatementKind::Expression(ExpressionNode {
                    kind: ExpressionKind::Identifier(value),
                    ..
                }),
                ..
            }] if value == "item"
        ));
        assert!(matches!(
            arms[1].body.as_slice(),
            [StatementNode {
                kind: StatementKind::Return(Some(ExpressionNode {
                    kind: ExpressionKind::Literal(LiteralNode::Integer(0)),
                    ..
                })),
                ..
            }]
        ));
        assert!(matches!(
            arms[2].body.as_slice(),
            [StatementNode {
                kind: StatementKind::Expression(ExpressionNode {
                    kind: ExpressionKind::Literal(LiteralNode::Integer(1)),
                    ..
                }),
                ..
            }]
        ));
        assert!(lowered.span.byte_range.is_some());

        for (source, expected_start, expected_end) in [
            ("values[:end]", false, true),
            ("values[start:]", true, false),
            ("values[start:end]", true, true),
        ] {
            let output = parse_source(&format!("auto result = {source}"));
            assert!(output.errors.is_empty(), "{:?}", output.errors);
            let node = find_data(output.tree.root(), &|data| {
                matches!(data, AstLoweringData::Slice { .. })
            })
            .expect("slice syntax data");
            let lowered = output
                .tree
                .lower_expression(node)
                .expect("lower slice expression");
            let ExpressionKind::Slice { expr, start, end } = &lowered.kind else {
                panic!("expected lowered slice expression");
            };
            assert!(matches!(&expr.kind, ExpressionKind::Identifier(name) if name == "values"));
            assert_eq!(start.is_some(), expected_start);
            assert_eq!(end.is_some(), expected_end);
            assert!(lowered.span.byte_range.is_some());
        }
    }

    #[test]
    fn typed_generic_and_type_contexts_lower_recursively() {
        let generic = lower_data("auto x = Box<list<int>>(1)", |data| {
            matches!(data, AstLoweringData::Generic { .. })
        });
        assert!(
            matches!(generic.kind, ExpressionKind::GenericType(name, args)
            if name == "Box" && matches!(args.as_slice(), [TypeNode { kind: TypeKind::List(element), .. }]
                if matches!(element.kind, TypeKind::Primitive(PrimitiveType::Int))))
        );

        let named = lower_type_data(
            "func f(Box<int> x) returns void {}",
            |data| matches!(data, AstLoweringData::TypeName { arguments, .. } if !arguments.is_empty()),
        );
        assert!(matches!(named.kind, TypeKind::Named(name, args)
            if name == "Box" && matches!(args.as_slice(), [TypeNode { kind: TypeKind::Primitive(PrimitiveType::Int), .. }])));

        let container =
            lower_type_data("func f(map<string, list<int>> x) returns void {}", |data| {
                matches!(data, AstLoweringData::TypeContainer { .. })
            });
        assert!(matches!(container.kind, TypeKind::Map(key, value)
            if matches!(&key.kind, TypeKind::Primitive(PrimitiveType::Str))
                && matches!(&value.kind, TypeKind::List(element)
                    if matches!(&element.kind, TypeKind::Primitive(PrimitiveType::Int)))));

        let reference_function = lower_type_data(
            "func f(&func(int, string) returns bool x) returns void {}",
            |data| matches!(data, AstLoweringData::TypeReference { .. }),
        );
        assert!(matches!(reference_function.kind, TypeKind::Reference(inner)
            if matches!(&inner.kind, TypeKind::Function { params, returns }
                if params.len() == 2 && matches!(&returns.kind, TypeKind::Primitive(PrimitiveType::Bool)))));
    }

    #[test]
    fn typed_leaf_statements_lower_to_existing_ast_nodes() {
        let expression = lower_statement_data("value", |data| {
            matches!(data, AstLoweringData::ExpressionStatement { .. })
        });
        assert!(matches!(expression.kind, StatementKind::Expression(_)));

        let returned = lower_statement_data("return 1", |data| {
            matches!(data, AstLoweringData::ReturnStatement { .. })
        });
        assert!(matches!(returned.kind, StatementKind::Return(Some(_))));

        let auto = lower_statement_data("auto x = 1", |data| {
            matches!(
                data,
                AstLoweringData::VariableDeclaration {
                    kind: VariableDeclarationKind::Auto,
                    ..
                }
            )
        });
        assert!(matches!(auto.kind, StatementKind::AutoDecl(name, _, _)
            if name == "x"));

        let typed = lower_statement_data("int x = 1", |data| {
            matches!(
                data,
                AstLoweringData::VariableDeclaration {
                    kind: VariableDeclarationKind::Typed,
                    ..
                }
            )
        });
        assert!(matches!(typed.kind, StatementKind::TypedDecl(name, _, _)
            if name == "x"));

        let uninitialized = lower_statement_data("int x", |data| {
            matches!(
                data,
                AstLoweringData::VariableDeclaration {
                    kind: VariableDeclarationKind::Uninitialized,
                    ..
                }
            )
        });
        assert!(
            matches!(uninitialized.kind, StatementKind::UninitDecl(name, _)
            if name == "x")
        );

        let constant = lower_statement_data("const int x = 1", |data| {
            matches!(
                data,
                AstLoweringData::VariableDeclaration {
                    kind: VariableDeclarationKind::Const,
                    ..
                }
            )
        });
        assert!(matches!(constant.kind, StatementKind::ConstDecl(name, _, _)
            if name == "x"));

        let broke = lower_statement_data("while true { break }", |data| {
            matches!(data, AstLoweringData::BreakStatement { .. })
        });
        assert!(matches!(broke.kind, StatementKind::Break));

        let continued = lower_statement_data("while true { continue }", |data| {
            matches!(data, AstLoweringData::ContinueStatement { .. })
        });
        assert!(matches!(continued.kind, StatementKind::Continue));
    }

    #[test]
    fn wildcard_binding_lowers_through_the_syntax_frontend() {
        let output = parse_source("auto _ = 1");
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        let lowered = output.lower().expect("wildcard binding should lower");
        assert!(matches!(
            &lowered[..],
            [AstNode::Statement(StatementNode {
                kind: StatementKind::AutoDecl(name, ..),
                ..
            })] if name == "_"
        ));
    }

    #[test]
    fn nested_block_syntax_lowers_in_source_order() {
        let output = parse_source("{ { auto x = 1 } }");
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        let node = find_data(output.tree.root(), &|data| {
            matches!(data, AstLoweringData::Block)
        })
        .expect("outer block context");
        let block = output
            .tree
            .lower_statement(node)
            .expect("lower nested block");
        let StatementKind::Block(statements) = block.kind else {
            panic!("expected outer block");
        };
        assert_eq!(statements.len(), 1);
        let StatementKind::Block(nested) = &statements[0].kind else {
            panic!("expected nested block");
        };
        assert_eq!(nested.len(), 1);
        assert!(matches!(&nested[0].kind, StatementKind::AutoDecl(name, _, _) if name == "x"));
    }

    #[test]
    fn typed_if_while_and_for_lower_with_existing_block_shapes() {
        let if_statement =
            lower_statement_data("if true { auto x = 1 } else { return 2 }", |data| {
                matches!(data, AstLoweringData::IfStatement { .. })
            });
        let StatementKind::If {
            then_block,
            else_block,
            ..
        } = if_statement.kind
        else {
            panic!("expected if statement");
        };
        assert!(matches!(
            then_block.as_slice(),
            [StatementNode {
                kind: StatementKind::AutoDecl(..),
                ..
            }]
        ));
        assert!(matches!(
            else_block.as_deref(),
            Some([StatementNode {
                kind: StatementKind::Return(Some(_)),
                ..
            }])
        ));

        let else_if = lower_statement_data("if true {} else if false { return 1 }", |data| {
            matches!(data, AstLoweringData::IfStatement { .. })
        });
        let StatementKind::If {
            else_block: Some(else_block),
            ..
        } = else_if.kind
        else {
            panic!("expected else-if branch");
        };
        assert!(matches!(
            else_block.as_slice(),
            [StatementNode {
                kind: StatementKind::If { .. },
                ..
            }]
        ));

        let while_statement = lower_statement_data("while true { break }", |data| {
            matches!(data, AstLoweringData::WhileStatement { .. })
        });
        assert!(
            matches!(while_statement.kind, StatementKind::While { body, .. }
            if matches!(body.as_slice(), [StatementNode { kind: StatementKind::Break, .. }]))
        );

        let for_block = lower_statement_data("for int x in values { auto y = x }", |data| {
            matches!(data, AstLoweringData::ForStatement { .. })
        });
        assert!(
            matches!(for_block.kind, StatementKind::For { var, body, .. }
            if var == "x" && matches!(body.as_slice(), [StatementNode { kind: StatementKind::AutoDecl(..), .. }]))
        );

        let for_single = lower_statement_data("for int x in values return x", |data| {
            matches!(data, AstLoweringData::ForStatement { .. })
        });
        assert!(matches!(for_single.kind, StatementKind::For { body, .. }
            if matches!(body.as_slice(), [StatementNode { kind: StatementKind::Return(Some(_)), .. }])));
    }

    #[test]
    fn for_loop_wildcard_binding_lowers_to_underscore_name() {
        let statement = lower_statement_data("for int _ in range(0, 6) {}", |data| {
            matches!(data, AstLoweringData::ForStatement { .. })
        });
        assert!(matches!(statement.kind, StatementKind::For { var, .. } if var == "_"));
    }

    #[test]
    fn typed_function_signatures_lower_with_defaults_bounds_where_and_body() {
        let source =
            "func identity<T is Eq>(int value = 1) where { value > 0 } returns T { return value }";
        let output = parse_source(source);
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        let syntax_function = find_data(output.tree.root(), &|data| {
            matches!(data, AstLoweringData::Function { .. })
        })
        .expect("function syntax data");
        let lowered = output
            .tree
            .lower_function(syntax_function)
            .expect("lower function from syntax");
        assert_eq!(lowered.name, "identity");
        assert!(!lowered.is_common);
        assert_eq!(lowered.type_params.len(), 1);
        assert_eq!(lowered.type_params[0].0, "T");
        assert_eq!(lowered.type_params[0].1.len(), 1);
        assert_eq!(lowered.type_params[0].1[0].name, "Eq");
        assert!(lowered.type_params[0].1[0].type_params.is_empty());
        assert_eq!(lowered.params.len(), 1);
        assert_eq!(lowered.params[0].name, "value");
        assert!(matches!(
            &lowered.params[0].type_.kind,
            TypeKind::Primitive(PrimitiveType::Int)
        ));
        assert!(matches!(
            lowered.params[0]
                .default_value
                .as_ref()
                .map(|value| &value.kind),
            Some(ExpressionKind::Literal(LiteralNode::Integer(1)))
        ));
        assert!(matches!(
            &lowered.return_type.kind,
            TypeKind::Named(name, args) if name == "T" && args.is_empty()
        ));
        let where_clause = lowered.where_clause.as_ref().expect("where clause");
        assert_eq!(where_clause.predicates.len(), 1);
        assert!(matches!(
            &where_clause.predicates[0].kind,
            ExpressionKind::Binary {
                left,
                op: BinaryOp::Greater,
                right,
                ..
            } if matches!(&left.kind, ExpressionKind::Identifier(name) if name == "value")
                && matches!(&right.kind, ExpressionKind::Literal(LiteralNode::Integer(0)))
        ));
        assert!(matches!(
            lowered.body.as_slice(),
            [StatementNode {
                kind: StatementKind::Return(Some(ExpressionNode {
                    kind: ExpressionKind::Identifier(name),
                    ..
                })),
                ..
            }] if name == "value"
        ));
    }

    #[test]
    fn nested_function_declaration_lowers_as_statement() {
        let source = "func outer() returns void { func inner() returns void { return } inner() }";
        let output = parse_source(source);
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        let lowered = output.lower().expect("lower compilation unit");
        let AstNode::Function(lowered_outer) = &lowered[0] else {
            panic!("expected lowered outer function");
        };
        let [
            StatementNode {
                kind: StatementKind::Function(lowered_inner),
                ..
            },
            StatementNode {
                kind: StatementKind::Expression(_),
                ..
            },
        ] = lowered_outer.body.as_slice()
        else {
            panic!("expected nested function followed by call");
        };
        assert_eq!(lowered_inner.name, "inner");
        assert!(matches!(
            lowered_inner.body.as_slice(),
            [StatementNode {
                kind: StatementKind::Return(None),
                ..
            }]
        ));
        assert!(lowered_inner.span.byte_range.is_some());
    }

    #[test]
    fn underscore_parameter_lowers_as_ignored_name() {
        let output = parse_source("func ignore(int _) returns void { return }");
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        let lowered = output
            .lower()
            .expect("lower function with underscore parameter");
        let AstNode::Function(function) = &lowered[0] else {
            panic!("expected lowered function");
        };
        assert_eq!(function.params.len(), 1);
        assert_eq!(function.params[0].name, "_");
    }

    #[test]
    fn typed_common_function_and_bodyless_interface_signature_lower() {
        let common_output = parse_source("common func run() returns void {}");
        assert!(
            common_output.errors.is_empty(),
            "{:?}",
            common_output.errors
        );
        let common_node = find_data(common_output.tree.root(), &|data| {
            matches!(
                data,
                AstLoweringData::Function {
                    is_common: true,
                    ..
                }
            )
        })
        .expect("common function syntax data");
        let common = common_output
            .tree
            .lower_function(common_node)
            .expect("lower common function");
        assert_eq!(common.name, "run");
        assert!(common.is_common);
        assert!(matches!(
            &common.return_type.kind,
            TypeKind::Primitive(PrimitiveType::Void)
        ));
        assert!(common.body.is_empty());

        let interface_output =
            parse_source("interface Reader { func read<T>(Box<T> value) returns T }");
        assert!(
            interface_output.errors.is_empty(),
            "{:?}",
            interface_output.errors
        );
        let method_node = find_data(interface_output.tree.root(), &|data| {
            matches!(data, AstLoweringData::Function { body: None, .. })
        })
        .expect("bodyless method syntax data");
        let lowered_method = interface_output
            .tree
            .lower_function(method_node)
            .expect("lower bodyless method");
        assert_eq!(lowered_method.name, "read");
        assert!(lowered_method.body.is_empty());
        assert_eq!(lowered_method.type_params.len(), 1);
        assert_eq!(lowered_method.type_params[0].0, "T");
        assert_eq!(lowered_method.params.len(), 1);
        assert_eq!(lowered_method.params[0].name, "value");
        assert!(matches!(
            &lowered_method.params[0].type_.kind,
            TypeKind::Named(name, args)
                if name == "Box" && matches!(args.as_slice(), [TypeNode {
                    kind: TypeKind::Named(type_name, inner_args),
                    ..
                }] if type_name == "T" && inner_args.is_empty())
        ));
        assert!(matches!(
            &lowered_method.return_type.kind,
            TypeKind::Named(name, args) if name == "T" && args.is_empty()
        ));
        assert!(matches!(
            &interface_output.lower().expect("lower interface compilation unit")[..],
            [AstNode::Interface { methods, .. }]
                if methods.len() == 1 && methods[0].name == "read" && methods[0].body.is_empty()
        ));
    }

    #[test]
    fn typed_import_declarations_lower_all_spec_shapes() {
        fn lower_import(source: &str) -> (String, ImportSpec) {
            let output = parse_source(source);
            assert!(output.errors.is_empty(), "{source}: {:?}", output.errors);
            let node = find_data(output.tree.root(), &|data| {
                matches!(data, AstLoweringData::Import { .. })
            })
            .expect("import syntax data");
            let lowered = output
                .tree
                .lower_statement(node)
                .unwrap_or_else(|error| panic!("{source}: lower import statement: {error}"));
            let StatementKind::Import { module_path, spec } = lowered.kind else {
                panic!("expected import statement");
            };
            (module_path, spec)
        }

        assert_eq!(
            lower_import("import std"),
            (
                "std".into(),
                ImportSpec::Module {
                    alias: Some("std".into())
                }
            )
        );
        assert_eq!(
            lower_import("import std.io"),
            (
                "std".into(),
                ImportSpec::Item {
                    item: "io".into(),
                    alias: None,
                }
            )
        );
        assert_eq!(
            lower_import("import std as io2"),
            (
                "std".into(),
                ImportSpec::Module {
                    alias: Some("io2".into())
                }
            )
        );
        assert_eq!(
            lower_import("import std as _"),
            ("std".into(), ImportSpec::Module { alias: None })
        );
        assert_eq!(
            lower_import("import std.io.*"),
            ("std.io".into(), ImportSpec::Wildcard)
        );
        assert_eq!(
            lower_import("import std.io.print as output"),
            (
                "std.io".into(),
                ImportSpec::Item {
                    item: "print".into(),
                    alias: Some("output".into())
                }
            )
        );
        assert_eq!(
            lower_import("import std.io.(print, println as output)"),
            (
                "std.io".into(),
                ImportSpec::Items {
                    items: vec![
                        ("print".into(), None),
                        ("println".into(), Some("output".into()))
                    ]
                }
            )
        );
    }

    #[test]
    fn typed_test_declaration_lowers_name_body_and_span() {
        let output = parse_source("test \"addition\" { auto result = 1 + 2 }");
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        let declarations = output
            .tree
            .test_declarations()
            .expect("test declaration metadata should be available");
        let [declaration] = declarations.as_slice() else {
            panic!("expected one test declaration");
        };
        assert_eq!(declaration.name(), "addition");
        assert_eq!(
            &output.tree.source()
                [declaration.body_contents().start..declaration.body_contents().end],
            " auto result = 1 + 2 "
        );
        assert_eq!(declaration.body_start_line(), 1);
        assert_eq!(declaration.annotation(), None);

        let node = find_data(output.tree.root(), &|data| {
            matches!(data, AstLoweringData::Test { .. })
        })
        .expect("test declaration syntax data");
        let lowered = output
            .tree
            .lower_test(node)
            .expect("lower test declaration");
        let AstNode::Test { name, body, span } = lowered else {
            panic!("expected lowered test declaration");
        };
        assert_eq!(name, "addition");
        assert!(matches!(
            body.as_slice(),
            [StatementNode {
                kind: StatementKind::AutoDecl(name, _, ExpressionNode {
                    kind: ExpressionKind::Binary { .. },
                    ..
                }),
                ..
            }] if name == "result"
        ));
        assert!(span.byte_range.is_some());
    }

    #[test]
    fn test_declaration_metadata_reads_adjacent_indented_crlf_annotation() {
        let source = "  // mux:test timeout=3\r\ntest \"annotated\" {\r\n    return\r\n}\r\n";
        let output = parse_source(source);
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        let declarations = output
            .tree
            .test_declarations()
            .expect("test declaration metadata should be available");
        assert_eq!(declarations.len(), 1);
        assert_eq!(declarations[0].annotation(), Some("// mux:test timeout=3"));
        assert_eq!(declarations[0].body_start_line(), 2);
        assert_eq!(
            &source[declarations[0].body_contents().start..declarations[0].body_contents().end],
            "\r\n    return\r\n"
        );
    }

    #[test]
    fn test_declaration_metadata_does_not_attach_non_adjacent_comment() {
        let source = "// mux:test timeout=3\n\ntest \"plain\" {}";
        let output = parse_source(source);
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        let declarations = output
            .tree
            .test_declarations()
            .expect("test declaration metadata should be available");
        assert_eq!(declarations.len(), 1);
        assert_eq!(declarations[0].annotation(), None);
    }

    #[test]
    fn recovery_tree_keeps_missing_type_and_zero_width_error_contexts() {
        let incomplete_type = parse_source("func broken() returns");
        assert!(!incomplete_type.errors.is_empty());

        let missing_expression = parse_source(")");
        assert!(!missing_expression.errors.is_empty());

        let insertion_tree = SyntaxTree::new(
            Arc::new(SourceText::new(")".to_owned())),
            vec![Token::new(TokenType::CloseParen, Span::new(0, 1))],
            &[SyntaxNodeEvent {
                kind: SyntaxKind::Error,
                range: ByteRange::empty(0),
                data: None,
            }],
        );

        fn has_zero_width_error(node: &SyntaxNode) -> bool {
            (node.kind() == SyntaxKind::Error && node.range().start == node.range().end)
                || node.children().iter().any(|child| match child {
                    SyntaxElement::Node(child) => has_zero_width_error(child),
                    SyntaxElement::Token(_) => false,
                })
        }
        assert!(has_zero_width_error(missing_expression.tree.root()));
        assert!(has_zero_width_error(insertion_tree.root()));
    }

    #[test]
    fn typed_enum_declaration_lowers_bounds_variants_payloads_and_where() {
        let source = "enum Result<T: Eq> { Ok(T value) where { true }, Err(string), Empty() }";
        let output = parse_source(source);
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        let node = find_data(output.tree.root(), &|data| {
            matches!(data, AstLoweringData::Enum { .. })
        })
        .expect("enum syntax data");
        let lowered = output
            .tree
            .lower_enum(node)
            .expect("lower enum declaration");
        let AstNode::Enum {
            name,
            type_params,
            variants,
            span,
        } = lowered
        else {
            panic!("expected lowered enum declaration");
        };
        assert_eq!(name, "Result");
        assert_eq!(type_params.len(), 1);
        assert_eq!(type_params[0].0, "T");
        assert_eq!(type_params[0].1.len(), 1);
        assert_eq!(type_params[0].1[0].name, "Eq");
        assert_eq!(variants.len(), 3);
        assert_eq!(variants[0].name, "Ok");
        assert!(matches!(
            variants[0].data.as_deref(),
            Some([(Some(name), TypeNode { kind: TypeKind::Named(type_name, args), .. })])
                if name == "value" && type_name == "T" && args.is_empty()
        ));
        assert!(matches!(
            variants[0]
                .where_clause
                .as_ref()
                .and_then(|clause| clause.predicates.first())
                .map(|predicate| &predicate.kind),
            Some(ExpressionKind::Literal(LiteralNode::Boolean(true)))
        ));
        assert_eq!(variants[1].name, "Err");
        assert!(matches!(
            variants[1].data.as_deref(),
            Some([(
                None,
                TypeNode {
                    kind: TypeKind::Primitive(PrimitiveType::Str),
                    ..
                }
            )])
        ));
        assert_eq!(variants[2].name, "Empty");
        assert!(matches!(variants[2].data.as_deref(), Some([])));

        assert!(span.byte_range.is_some());
    }

    #[test]
    fn typed_class_declaration_lowers_fields_traits_methods_and_constraints() {
        let source = "class Box<T is Eq> is Comparable<T> {\n    T value where { true }\n    const int count = 1\n    list<T> values = []\n    func get(int index = 0) returns T { return value }\n    common func reset() returns void {}\n} where { true }";
        let output = parse_source(source);
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        let node = find_data(output.tree.root(), &|data| {
            matches!(data, AstLoweringData::Class { .. })
        })
        .expect("class syntax data");
        let lowered = output
            .tree
            .lower_class(node)
            .expect("lower class declaration");
        let AstNode::Class {
            name,
            type_params,
            traits,
            fields,
            methods,
            where_clause,
            span,
        } = lowered
        else {
            panic!("expected lowered class declaration");
        };
        assert_eq!(name, "Box");
        assert_eq!(type_params.len(), 1);
        assert_eq!(type_params[0].0, "T");
        assert_eq!(type_params[0].1[0].name, "Eq");
        assert_eq!(traits.len(), 1);
        assert_eq!(traits[0].name, "Comparable");
        assert!(matches!(
            traits[0].type_args.as_slice(),
            [TypeNode {
                kind: TypeKind::Named(name, args),
                ..
            }] if name == "T" && args.is_empty()
        ));
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].name, "value");
        assert!(fields[0].is_generic_param);
        assert!(!fields[0].is_const);
        assert!(fields[0].default_value.is_none());
        assert_eq!(fields[0].where_clause.as_ref().unwrap().predicates.len(), 1);
        assert_eq!(fields[1].name, "count");
        assert!(fields[1].is_const);
        assert!(matches!(
            fields[1].default_value.as_ref().map(|value| &value.kind),
            Some(ExpressionKind::Literal(LiteralNode::Integer(1)))
        ));
        assert_eq!(fields[2].name, "values");
        assert!(matches!(
            fields[2].default_value.as_ref().map(|value| &value.kind),
            Some(ExpressionKind::ListLiteral(values)) if values.is_empty()
        ));
        assert_eq!(methods.len(), 2);
        assert_eq!(methods[0].name, "get");
        assert!(!methods[0].is_common);
        assert_eq!(methods[0].params.len(), 1);
        assert_eq!(methods[0].params[0].name, "index");
        assert!(methods[0].params[0].default_value.is_some());
        assert!(matches!(
            &methods[0].return_type.kind,
            TypeKind::Named(name, args) if name == "T" && args.is_empty()
        ));
        assert!(matches!(
            methods[0].body.as_slice(),
            [StatementNode {
                kind: StatementKind::Return(Some(ExpressionNode {
                    kind: ExpressionKind::Identifier(name),
                    ..
                })),
                ..
            }] if name == "value"
        ));
        assert_eq!(methods[1].name, "reset");
        assert!(methods[1].is_common);
        assert!(methods[1].body.is_empty());
        assert_eq!(where_clause.as_ref().unwrap().predicates.len(), 1);

        assert!(span.byte_range.is_some());
    }

    #[test]
    fn typed_interface_declaration_lowers_bounds_fields_and_bodyless_methods() {
        let source = "interface Reader<T: Eq> {\n    T last\n    const int version = 1\n    func read(Box<T> source, int limit) returns T\n    func close() returns void\n}";
        let output = parse_source(source);
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        let node = find_data(output.tree.root(), &|data| {
            matches!(data, AstLoweringData::Interface { .. })
        })
        .expect("interface syntax data");
        let lowered = output
            .tree
            .lower_interface(node)
            .expect("lower interface declaration");
        let AstNode::Interface {
            name,
            type_params,
            fields,
            methods,
            span,
        } = lowered
        else {
            panic!("expected lowered interface declaration");
        };
        assert_eq!(name, "Reader");
        assert_eq!(type_params.len(), 1);
        assert_eq!(type_params[0].0, "T");
        assert_eq!(type_params[0].1.len(), 1);
        assert_eq!(type_params[0].1[0].name, "Eq");
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name, "last");
        assert!(fields[0].is_generic_param);
        assert_eq!(fields[1].name, "version");
        assert!(fields[1].is_const);
        assert!(matches!(
            fields[1].default_value.as_ref().map(|value| &value.kind),
            Some(ExpressionKind::Literal(LiteralNode::Integer(1)))
        ));
        assert_eq!(methods.len(), 2);
        assert_eq!(methods[0].name, "read");
        assert!(methods[0].body.is_empty());
        assert_eq!(methods[0].params.len(), 2);
        assert!(methods[0].params[1].default_value.is_none());
        assert!(matches!(
            &methods[0].return_type.kind,
            TypeKind::Named(name, args) if name == "T" && args.is_empty()
        ));
        assert_eq!(methods[1].name, "close");
        assert!(methods[1].body.is_empty());
        assert!(span.byte_range.is_some());
    }

    #[test]
    fn typed_match_statement_lowers_nested_patterns_guards_and_arm_bodies() {
        let source = "func inspect(Box input) returns void { match input {\n    Some([first, ..rest]) if first > 0 { auto saved = first }\n    none { return }\n    _ { return }\n    0 { return }\n} }";
        let output = parse_source(source);
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        let node = find_data(output.tree.root(), &|data| {
            matches!(data, AstLoweringData::MatchStatement { .. })
        })
        .expect("match statement syntax data");
        let lowered = output
            .tree
            .lower_statement(node)
            .expect("lower match statement");
        let StatementKind::Match { expr, arms } = &lowered.kind else {
            panic!("expected lowered match statement");
        };
        assert!(matches!(&expr.kind, ExpressionKind::Identifier(name) if name == "input"));
        assert_eq!(arms.len(), 4);
        assert!(matches!(
            &arms[0].pattern,
            PatternNode::EnumVariant { name, args }
                if name == "Some"
                    && matches!(args.as_slice(), [PatternNode::List { elements, rest: Some(rest) }]
                        if matches!(elements.as_slice(), [PatternNode::Identifier(name)] if name == "first")
                            && matches!(rest.as_ref(), PatternNode::Identifier(name) if name == "rest"))
        ));
        assert!(matches!(
            arms[0].guard.as_ref().map(|guard| &guard.kind),
            Some(ExpressionKind::Binary {
                op: BinaryOp::Greater,
                ..
            })
        ));
        assert!(matches!(
            arms[0].body.as_slice(),
            [StatementNode {
                kind: StatementKind::AutoDecl(name, _, ExpressionNode {
                    kind: ExpressionKind::Identifier(value),
                    ..
                }),
                ..
            }] if name == "saved" && value == "first"
        ));
        assert!(matches!(
            &arms[1].pattern,
            PatternNode::EnumVariant { name, args } if name == "none" && args.is_empty()
        ));
        assert!(matches!(
            arms[1].body.as_slice(),
            [StatementNode {
                kind: StatementKind::Return(None),
                ..
            }]
        ));
        assert!(matches!(&arms[2].pattern, PatternNode::Wildcard));
        assert!(matches!(
            &arms[3].pattern,
            PatternNode::Literal(LiteralNode::Integer(0))
        ));
        assert!(lowered.span.byte_range.is_some());
    }
}
