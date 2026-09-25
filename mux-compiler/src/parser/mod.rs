//! Parser module for the Mux language.
//!
//! This module contains the parser which converts a stream of tokens into an AST.

mod cursor;
mod declarations;
mod error;
mod expressions;
mod statements;
mod types;

pub use error::{ParserError, ParserResult};

use crate::ast::{
    AstNode, BinaryOp, EnumVariant, EnumVariantField, ExpressionKind, ExpressionNode, Field,
    FunctionNode, ImportSpec, LiteralNode, MatchArm, Param, PatternNode, Precedence, SpanExt,
    Spanned, StatementKind, StatementNode, TraitBound, TraitRef, TypeKind, TypeNode, UnaryOp,
    WhereClause,
};
use crate::diagnostic::DiagnosticCode;
use crate::lexer::{ByteRange, Span, Token, TokenType};
use crate::syntax::{
    SyntaxData, SyntaxImportSpec, SyntaxKind, SyntaxModuleAlias, SyntaxNodeEvent, SyntaxPattern,
    VariableDeclarationKind,
};

#[derive(Debug)]
pub struct Parser<'a> {
    tokens: Vec<&'a Token>,
    current: usize,
    pub errors: Vec<ParserError>,
    recovery_spans: Vec<Span>,
    loop_depth: usize,
    stopped: bool,
    syntax_events: Vec<SyntaxNodeEvent>,
    mode: ParserMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParserMode {
    Compatibility,
    SyntaxOnly,
}

#[derive(Debug, Clone, Copy)]
struct ParserCheckpoint {
    current: usize,
    errors: usize,
    recovery_spans: usize,
    syntax_events: usize,
    loop_depth: usize,
    stopped: bool,
}

/// Why `name` cannot be declared as a class method, if it cannot.
///
/// The compiler synthesizes `new` on every class, so a user-declared method of
/// the same name would be silently replaced by the synthesized constructor.
/// Serialization and deserialization are ordinary user methods: their names
/// and implementations belong to the program, not to the compiler.
fn reserved_class_method_error(name: &str) -> Option<String> {
    let purpose = match name {
        "new" => "class constructors",
        _ => return None,
    };
    Some(format!(
        "'{name}' is reserved for {purpose} and cannot be defined as a class method"
    ))
}

impl<'a> Parser<'a> {
    #[must_use]
    pub fn new(tokens: &'a [Token]) -> Self {
        let mut grammar_tokens = Vec::with_capacity(tokens.len());
        let mut bracket_depth = 0_usize;
        let mut brace_depths = Vec::new();
        for token in tokens {
            match &token.token_type {
                TokenType::OpenBrace => {
                    brace_depths.push(bracket_depth);
                    bracket_depth = 0;
                }
                TokenType::CloseBrace => {
                    bracket_depth = brace_depths.pop().unwrap_or(bracket_depth);
                }
                TokenType::OpenParen | TokenType::OpenBracket => bracket_depth += 1,
                TokenType::CloseParen | TokenType::CloseBracket => {
                    bracket_depth = bracket_depth.saturating_sub(1);
                }
                _ => {}
            }
            if token.token_type == TokenType::NewLine && bracket_depth > 0 {
                continue;
            }
            if matches!(
                token.token_type,
                TokenType::LineComment(_) | TokenType::MultilineComment(_) | TokenType::Whitespace
            ) {
                continue;
            }
            grammar_tokens.push(token);
        }
        Self {
            tokens: grammar_tokens,
            current: 0,
            errors: Vec::new(),
            recovery_spans: Vec::new(),
            loop_depth: 0,
            stopped: false,
            syntax_events: Vec::new(),
            mode: ParserMode::Compatibility,
        }
    }

    pub(crate) fn syntax_events(&self) -> &[SyntaxNodeEvent] {
        &self.syntax_events
    }

    fn checkpoint(&self) -> ParserCheckpoint {
        ParserCheckpoint {
            current: self.current,
            errors: self.errors.len(),
            recovery_spans: self.recovery_spans.len(),
            syntax_events: self.syntax_events.len(),
            loop_depth: self.loop_depth,
            stopped: self.stopped,
        }
    }

    fn rewind(&mut self, checkpoint: ParserCheckpoint) {
        self.current = checkpoint.current;
        self.errors.truncate(checkpoint.errors);
        self.recovery_spans.truncate(checkpoint.recovery_spans);
        self.syntax_events.truncate(checkpoint.syntax_events);
        self.loop_depth = checkpoint.loop_depth;
        self.stopped = checkpoint.stopped;
    }

    fn record_syntax_node(&mut self, kind: SyntaxKind, start: usize, end: usize) {
        self.record_syntax_data(kind, start, end, None);
    }

    fn record_typed_syntax_node(
        &mut self,
        kind: SyntaxKind,
        start: usize,
        end: usize,
        data: crate::syntax::SyntaxData,
    ) {
        self.record_syntax_data(kind, start, end, Some(data));
    }

    fn record_syntax_data(
        &mut self,
        kind: SyntaxKind,
        start: usize,
        end: usize,
        data: Option<crate::syntax::SyntaxData>,
    ) {
        if start > end {
            return;
        }
        if start == end {
            let position = self
                .tokens
                .get(start)
                .and_then(|token| token.span.byte_range)
                .map(|range| range.start)
                .or_else(|| {
                    start
                        .checked_sub(1)
                        .and_then(|index| self.tokens.get(index))
                        .and_then(|token| token.span.byte_range)
                        .map(|range| range.end)
                });
            let Some(position) = position else {
                return;
            };
            self.syntax_events.push(SyntaxNodeEvent {
                kind,
                range: ByteRange::empty(position),
                data,
            });
            return;
        }
        let Some(start_range) = self
            .tokens
            .get(start)
            .and_then(|token| token.span.byte_range)
        else {
            return;
        };
        let Some(end_range) = self
            .tokens
            .get(end - 1)
            .and_then(|token| token.span.byte_range)
        else {
            return;
        };
        self.syntax_events.push(SyntaxNodeEvent {
            kind,
            range: crate::lexer::ByteRange::new(start_range.start, end_range.end),
            data,
        });
    }

    fn source_range_for_tokens(&self, start: usize, end: usize) -> Option<ByteRange> {
        let start = self.tokens.get(start)?.span.byte_range?;
        let end = self.tokens.get(end.checked_sub(1)?)?.span.byte_range?;
        Some(ByteRange::new(start.start, end.end))
    }

    fn last_statement_range_since(&self, event_start: usize) -> Option<ByteRange> {
        self.syntax_events
            .iter()
            .skip(event_start)
            .rev()
            .find(|event| {
                matches!(
                    event.data.as_ref(),
                    Some(
                        SyntaxData::VariableDeclaration { .. }
                            | SyntaxData::ExpressionStatement { .. }
                            | SyntaxData::ReturnStatement { .. }
                            | SyntaxData::BreakStatement
                            | SyntaxData::ContinueStatement
                            | SyntaxData::Block
                            | SyntaxData::IfStatement { .. }
                            | SyntaxData::WhileStatement { .. }
                            | SyntaxData::ForStatement { .. }
                            | SyntaxData::MatchStatement { .. }
                    )
                )
            })
            .map(|event| event.range)
    }

    fn syntax_ranges_since(
        &self,
        event_start: usize,
        matches_data: impl Fn(&SyntaxData) -> bool,
    ) -> Vec<ByteRange> {
        self.syntax_events
            .iter()
            .skip(event_start)
            .filter_map(|event| {
                event
                    .data
                    .as_ref()
                    .filter(|data| matches_data(data))
                    .map(|_| event.range)
            })
            .collect()
    }

    fn last_syntax_range_since(
        &self,
        event_start: usize,
        matches_data: impl Fn(&SyntaxData) -> bool,
    ) -> Option<ByteRange> {
        self.syntax_events
            .iter()
            .skip(event_start)
            .rev()
            .find(|event| event.data.as_ref().is_some_and(&matches_data))
            .map(|event| event.range)
    }

    fn token_index_for_span(&self, span: Span) -> usize {
        let Some(range) = span.byte_range else {
            return self.current;
        };
        // Grammar contexts often start at a composite expression span such as
        // `value.field`; no single token has that full range. Resolve the
        // token at the span's start instead of requiring an exact token span.
        let lower = self.tokens.partition_point(|token| {
            token
                .span
                .byte_range
                .is_some_and(|token_range| token_range.start < range.start)
        });
        if self
            .tokens
            .get(lower)
            .and_then(|token| token.span.byte_range)
            .is_some_and(|token_range| token_range.start == range.start)
        {
            return lower;
        }
        lower
            .checked_sub(1)
            .filter(|index| {
                self.tokens
                    .get(*index)
                    .and_then(|token| token.span.byte_range)
                    .is_some_and(|token_range| {
                        token_range.start <= range.start && range.start < token_range.end
                    })
            })
            .unwrap_or(self.current)
    }

    fn matching_delimiter_end(&self, start: usize, open: TokenType, close: TokenType) -> usize {
        let mut depth = 0_usize;
        for (index, token) in self.tokens.iter().enumerate().skip(start) {
            if token.token_type == open {
                depth += 1;
            } else if token.token_type == close {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return index + 1;
                }
            }
        }
        self.current
    }

    fn classify_declaration(&self, token: &TokenType) -> SyntaxKind {
        match token {
            TokenType::Func | TokenType::Common => SyntaxKind::FunctionDeclaration,
            TokenType::Class => SyntaxKind::ClassDeclaration,
            TokenType::Interface => SyntaxKind::InterfaceDeclaration,
            TokenType::Enum => SyntaxKind::EnumDeclaration,
            TokenType::Import => SyntaxKind::ImportDeclaration,
            TokenType::Test => SyntaxKind::TestDeclaration,
            _ => SyntaxKind::Declaration,
        }
    }

    const MAX_ERRORS: usize = 100;

    fn record_error(&mut self, error: ParserError) {
        if self.stopped {
            return;
        }
        if let Some(range) = error.span.byte_range {
            self.syntax_events.push(SyntaxNodeEvent {
                kind: SyntaxKind::Error,
                range: ByteRange::empty(range.start),
                data: None,
            });
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
                | TokenType::Test
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
            let error_count_before_declaration = self.errors.len();

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
                    let declaration_start = self
                        .tokens
                        .get(start_position)
                        .and_then(|token| token.span.byte_range)
                        .map(|range| range.start);
                    let is_syntax_only_statement = self.mode == ParserMode::SyntaxOnly
                        && self.errors.len() == error_count_before_declaration
                        && declaration_start.is_some_and(|start| {
                            self.syntax_events.iter().any(|event| {
                                event.range.start == start
                                    && matches!(
                                        event.data.as_ref(),
                                        Some(
                                            SyntaxData::Import { .. }
                                                | SyntaxData::VariableDeclaration { .. }
                                                | SyntaxData::ExpressionStatement { .. }
                                                | SyntaxData::ReturnStatement { .. }
                                                | SyntaxData::BreakStatement
                                                | SyntaxData::ContinueStatement
                                        )
                                    )
                            })
                        });
                    if is_syntax_only_statement && let Err(e) = self.check_statement_termination() {
                        self.record_error(e);
                        self.synchronize();
                    }
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

    /// Parse declarations for lossless syntax consumers. Enum declarations
    /// retain their syntax facts without materializing enum compatibility AST
    /// nodes; other declarations keep the ordinary parser path.
    pub(crate) fn parse_for_syntax(
        &mut self,
    ) -> Result<Vec<AstNode>, (Vec<AstNode>, Vec<ParserError>)> {
        self.mode = ParserMode::SyntaxOnly;
        self.parse()
    }

    /// Spans consumed while skipping malformed input during recovery.
    ///
    /// Callers use these intervals to suppress diagnostics derived from
    /// tokens the parser deliberately discarded.
    #[must_use]
    pub fn recovery_spans(&self) -> &[Span] {
        &self.recovery_spans
    }
}

impl<'a> Parser<'a> {
    fn synchronize(&mut self) {
        let start = self.current;
        while !self.is_at_end() {
            match self.peek().token_type {
                TokenType::NewLine => {
                    self.advance();
                    break;
                }
                TokenType::Func
                | TokenType::Auto
                | TokenType::Const
                | TokenType::Class
                | TokenType::Interface
                | TokenType::Enum
                | TokenType::Import
                | TokenType::If
                | TokenType::Else
                | TokenType::While
                | TokenType::For
                | TokenType::Match
                | TokenType::Return
                | TokenType::OpenBrace
                | TokenType::CloseBrace
                | TokenType::OpenParen
                | TokenType::CloseParen
                | TokenType::OpenBracket
                | TokenType::CloseBracket => {
                    break;
                }
                _ => {
                    self.advance();
                }
            }
        }
        if self.current > start {
            let span = self.tokens[start]
                .span
                .combine(&self.tokens[self.current - 1].span);
            self.recovery_spans.push(span);
            self.record_syntax_node(SyntaxKind::Error, start, self.current);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::PrimitiveType;
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
        let input = source.text().to_owned();
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
    fn parses_named_top_level_test_block() {
        let mut parser = create_parser(
            "test \"adds numbers\" {\n  auto value = 1 + 2\n}\nfunc main() returns void {\n  return\n}\n",
        );
        let nodes = parser.parse().expect("test block should parse");
        assert!(matches!(
            nodes.first(),
            Some(AstNode::Test { name, body, .. })
                if name == "adds numbers" && body.len() == 1
        ));
        assert!(matches!(nodes.get(1), Some(AstNode::Function(_))));
    }

    #[test]
    fn rejects_nested_test_block() {
        let mut parser = create_parser("func main() returns void {\n  test \"nested\" {}\n}\n");
        let Err((_, errors)) = parser.parse() else {
            panic!("nested test block should be rejected");
        };
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("top level"))
        );
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
    fn if_block_destructuring_preserves_nested_statement_and_span() {
        let stmts = parse_stmts("if true {\n  auto y = 1\n}\n");
        let StatementKind::If {
            cond,
            then_block,
            else_block,
        } = &stmts[0].kind
        else {
            panic!("expected an if statement, got {:?}", stmts[0].kind);
        };

        assert!(matches!(cond.kind, ExpressionKind::Literal(_)));
        assert_eq!(then_block.len(), 1);
        assert!(matches!(then_block[0].kind, StatementKind::AutoDecl(..)));
        assert!(else_block.is_none());
        assert_eq!(stmts[0].span.row_start, 1);
        assert_eq!(stmts[0].span.row_end, Some(2));
    }

    #[test]
    fn if_without_block_reports_the_expected_diagnostic() {
        let mut parser = create_parser("if true\nauto y = 1\n");
        let result = parser.parse();
        let (_, errors) = result.expect_err("an if without a block must fail");

        assert!(
            errors
                .iter()
                .any(|error| { error.message.contains("Expected '{' before block") })
        );
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
