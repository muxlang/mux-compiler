//! Unit tests for the AST data types in `mux_lang::ast`.
//!
//! The snapshot/integration suites drive the parser and analyzer end to end but
//! never touch the small pure helpers on the AST directly: the `From`
//! conversions, `Spanned` impls, precedence/operator helpers, and the
//! primitive/operator token parsers. These tests cover those.

use mux_lang::ast::literals::ConstDeclNode;
use mux_lang::ast::{
    AstNode, BinaryOp, ExpressionKind, ExpressionNode, FunctionNode, LiteralNode, Precedence,
    PrimitiveType, Spanned, StatementKind, StatementNode, TypeKind, TypeNode, UnaryOp,
};
use mux_lang::lexer::{Span, Token, TokenType};
use ordered_float::OrderedFloat;

fn span() -> Span {
    Span::new(1, 1)
}

#[test]
fn literal_node_from_conversions() {
    assert_eq!(
        LiteralNode::from(OrderedFloat(3.5_f64)),
        LiteralNode::Float(OrderedFloat(3.5))
    );
    assert_eq!(LiteralNode::from(42_i64), LiteralNode::Integer(42));
    assert_eq!(
        LiteralNode::from(String::from("hi")),
        LiteralNode::String("hi".to_string())
    );
    assert_eq!(
        LiteralNode::from("hi"),
        LiteralNode::String("hi".to_string())
    );
    assert_eq!(LiteralNode::from(true), LiteralNode::Boolean(true));
    assert_eq!(LiteralNode::from('z'), LiteralNode::Char('z'));
}

#[test]
fn type_node_from_conversions_and_span() {
    let t: TypeNode = PrimitiveType::Int.into();
    assert_eq!(t.kind, TypeKind::Primitive(PrimitiveType::Int));

    let named: TypeNode = "Widget".into();
    assert_eq!(
        named.kind,
        TypeKind::Named("Widget".to_string(), Vec::new())
    );

    // Spanned is implemented for TypeNode.
    assert_eq!(named.span().row_start, 0);
}

#[test]
fn primitive_type_parse_all_keywords() {
    let cases = [
        ("int", PrimitiveType::Int),
        ("float", PrimitiveType::Float),
        ("bool", PrimitiveType::Bool),
        ("char", PrimitiveType::Char),
        ("string", PrimitiveType::Str),
        ("void", PrimitiveType::Void),
        ("auto", PrimitiveType::Auto),
    ];
    for (text, expected) in cases {
        let tok = Token::new(TokenType::Id(text.to_string()), span());
        assert_eq!(PrimitiveType::parse(&tok), Ok(expected));
    }
}

#[test]
fn primitive_type_parse_rejects_unknown_and_non_identifier() {
    let unknown = Token::new(TokenType::Id("widget".to_string()), span());
    assert!(PrimitiveType::parse(&unknown).is_err());

    let non_id = Token::new(TokenType::Plus, span());
    assert!(PrimitiveType::parse(&non_id).is_err());
}

#[test]
fn precedence_next_higher_climbs_and_saturates() {
    assert_eq!(Precedence::Assignment.next_higher(), Precedence::Or);
    assert_eq!(Precedence::Or.next_higher(), Precedence::And);
    assert_eq!(Precedence::And.next_higher(), Precedence::Equality);
    assert_eq!(Precedence::Equality.next_higher(), Precedence::Comparison);
    assert_eq!(Precedence::Comparison.next_higher(), Precedence::Term);
    assert_eq!(Precedence::Term.next_higher(), Precedence::Factor);
    assert_eq!(Precedence::Factor.next_higher(), Precedence::Exponent);
    assert_eq!(Precedence::Exponent.next_higher(), Precedence::Unary);
    assert_eq!(Precedence::Unary.next_higher(), Precedence::Call);
    assert_eq!(Precedence::Call.next_higher(), Precedence::Primary);
    // Primary is the ceiling and stays put.
    assert_eq!(Precedence::Primary.next_higher(), Precedence::Primary);
}

#[test]
fn precedence_display_and_ordering() {
    assert_eq!(Precedence::Term.to_string(), "Term");
    assert!(Precedence::Assignment < Precedence::Primary);
}

#[test]
fn binary_op_assignment_and_associativity() {
    for op in [
        BinaryOp::Assign,
        BinaryOp::AddAssign,
        BinaryOp::SubtractAssign,
        BinaryOp::MultiplyAssign,
        BinaryOp::DivideAssign,
        BinaryOp::ModuloAssign,
    ] {
        assert!(op.is_assignment(), "{op:?} should be an assignment");
        assert!(op.is_right_associative(), "{op:?} should be right assoc");
    }

    assert!(!BinaryOp::Add.is_assignment());
    // Exponent is right-associative but not an assignment.
    assert!(BinaryOp::Exponent.is_right_associative());
    assert!(!BinaryOp::Add.is_right_associative());
}

#[test]
fn unary_op_parse_all_and_error() {
    let cases = [
        (TokenType::Minus, UnaryOp::Neg),
        (TokenType::Bang, UnaryOp::Not),
        (TokenType::Ref, UnaryOp::Ref),
        (TokenType::Incr, UnaryOp::Incr),
        (TokenType::Decr, UnaryOp::Decr),
        (TokenType::Star, UnaryOp::Deref),
    ];
    for (tt, expected) in cases {
        let tok = Token::new(tt, span());
        assert_eq!(UnaryOp::parse(&tok), Ok(expected));
    }

    let bad = Token::new(TokenType::Plus, span());
    assert!(UnaryOp::parse(&bad).is_err());
}

#[test]
fn expression_node_from_conversions() {
    let from_lit: ExpressionNode = LiteralNode::Integer(7).into();
    assert_eq!(
        from_lit.kind,
        ExpressionKind::Literal(LiteralNode::Integer(7))
    );

    let from_ident: ExpressionNode = "name".into();
    assert_eq!(
        from_ident.kind,
        ExpressionKind::Identifier("name".to_string())
    );
}

#[test]
fn const_decl_node_is_spanned() {
    let node = ConstDeclNode {
        name: "MAX".to_string(),
        type_: PrimitiveType::Int.into(),
        value: LiteralNode::Integer(100).into(),
        span: Span::new(3, 9),
    };
    assert_eq!(node.span().row_start, 3);
    assert_eq!(node.span().col_start, 9);
}

#[test]
fn statement_node_from_expression() {
    let expr: ExpressionNode = LiteralNode::Boolean(true).into();
    let stmt: StatementNode = expr.into();
    assert!(matches!(stmt.kind, StatementKind::Expression(_)));
    assert_eq!(stmt.span().row_start, 0);
}

fn sample_function() -> FunctionNode {
    FunctionNode {
        name: "f".to_string(),
        type_params: Vec::new(),
        params: Vec::new(),
        return_type: PrimitiveType::Void.into(),
        body: Vec::new(),
        span: Span::new(4, 2),
        is_common: false,
        where_clause: None,
    }
}

#[test]
fn ast_node_span_covers_all_variants() {
    let func = AstNode::Function(sample_function());
    assert_eq!(func.span().row_start, 4);

    let class = AstNode::Class {
        name: "C".to_string(),
        type_params: Vec::new(),
        traits: Vec::new(),
        fields: Vec::new(),
        methods: Vec::new(),
        where_clause: None,
        span: Span::new(5, 0),
    };
    assert_eq!(class.span().row_start, 5);

    let interface = AstNode::Interface {
        name: "I".to_string(),
        type_params: Vec::new(),
        fields: Vec::new(),
        methods: Vec::new(),
        span: Span::new(6, 0),
    };
    assert_eq!(interface.span().row_start, 6);

    let enum_node = AstNode::Enum {
        name: "E".to_string(),
        type_params: Vec::new(),
        variants: Vec::new(),
        span: Span::new(7, 0),
    };
    assert_eq!(enum_node.span().row_start, 7);

    let stmt = AstNode::Statement(StatementNode {
        kind: StatementKind::Break,
        span: Span::new(8, 0),
    });
    assert_eq!(stmt.span().row_start, 8);
}

#[test]
fn ast_node_into_statement() {
    // A statement node unwraps to itself.
    let stmt = AstNode::Statement(StatementNode {
        kind: StatementKind::Continue,
        span: span(),
    });
    assert!(matches!(
        stmt.into_statement(),
        Some(StatementNode {
            kind: StatementKind::Continue,
            ..
        })
    ));

    // A function becomes a Function statement.
    let func = AstNode::Function(sample_function());
    assert!(matches!(
        func.into_statement(),
        Some(StatementNode {
            kind: StatementKind::Function(_),
            ..
        })
    ));

    // Top-level declarations are not statements.
    let enum_node = AstNode::Enum {
        name: "E".to_string(),
        type_params: Vec::new(),
        variants: Vec::new(),
        span: span(),
    };
    assert!(enum_node.into_statement().is_none());
}
