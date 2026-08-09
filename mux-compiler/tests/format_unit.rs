//! Unit tests for the semantic type/diagnostic formatting helpers in
//! `mux_lang::semantics::format`.
//!
//! These pure functions render `Type` values and operators into the strings
//! shown in diagnostics. They are easy to get subtly wrong and were largely
//! uncovered, so every `Type` variant and operator is exercised here.

use mux_lang::ast::{BinaryOp, PrimitiveType};
use mux_lang::lexer::Span;
use mux_lang::semantics::Type;
use mux_lang::semantics::format::{format_binary_op, format_type};

fn b(t: Type) -> Box<Type> {
    Box::new(t)
}

fn int() -> Type {
    Type::Primitive(PrimitiveType::Int)
}

#[test]
fn format_primitive_types() {
    assert_eq!(format_type(&Type::Primitive(PrimitiveType::Int)), "int");
    assert_eq!(format_type(&Type::Primitive(PrimitiveType::Float)), "float");
    assert_eq!(format_type(&Type::Primitive(PrimitiveType::Bool)), "bool");
    assert_eq!(format_type(&Type::Primitive(PrimitiveType::Char)), "char");
    assert_eq!(format_type(&Type::Primitive(PrimitiveType::Str)), "string");
    assert_eq!(format_type(&Type::Primitive(PrimitiveType::Void)), "void");
    assert_eq!(format_type(&Type::Primitive(PrimitiveType::Auto)), "auto");
}

#[test]
fn format_collection_and_wrapper_types() {
    assert_eq!(format_type(&Type::List(b(int()))), "list<int>");
    assert_eq!(
        format_type(&Type::Map(b(int()), b(Type::Primitive(PrimitiveType::Str)))),
        "map<int, string>"
    );
    assert_eq!(format_type(&Type::Set(b(int()))), "set<int>");
    // Spelled the way the parser accepts it, like every other composite above.
    // "(int, bool)" is not writable syntax, so a diagnostic naming that type
    // handed the reader something they could not type back.
    assert_eq!(
        format_type(&Type::Tuple(
            b(int()),
            b(Type::Primitive(PrimitiveType::Bool))
        )),
        "tuple<int, bool>"
    );
    assert_eq!(format_type(&Type::Optional(b(int()))), "optional<int>");
    assert_eq!(
        format_type(&Type::Result(
            b(int()),
            b(Type::Primitive(PrimitiveType::Str))
        )),
        "result<int, string>"
    );
    assert_eq!(format_type(&Type::Reference(b(int()))), "&int");
}

#[test]
fn format_unit_and_empty_types() {
    assert_eq!(format_type(&Type::Void), "void");
    assert_eq!(format_type(&Type::Never), "never");
    assert_eq!(format_type(&Type::EmptyList), "list<?>");
    assert_eq!(format_type(&Type::EmptyMap), "map<?, ?>");
    assert_eq!(format_type(&Type::EmptySet), "set<?>");
}

#[test]
fn format_function_type() {
    let f = Type::Function {
        params: vec![int(), Type::Primitive(PrimitiveType::Bool)],
        returns: b(Type::Primitive(PrimitiveType::Str)),
        default_count: 0,
    };
    assert_eq!(format_type(&f), "fn(int, bool) -> string");

    let no_params = Type::Function {
        params: vec![],
        returns: b(Type::Void),
        default_count: 0,
    };
    assert_eq!(format_type(&no_params), "fn() -> void");
}

#[test]
fn format_named_variable_generic_instantiated_module() {
    assert_eq!(
        format_type(&Type::Named("Widget".to_string(), vec![])),
        "Widget"
    );
    assert_eq!(
        format_type(&Type::Named("Pair".to_string(), vec![int(), int()])),
        "Pair<int, int>"
    );
    assert_eq!(format_type(&Type::Variable("T".to_string())), "T");
    assert_eq!(format_type(&Type::Generic("U".to_string())), "U");
    assert_eq!(
        format_type(&Type::Instantiated("Box".to_string(), vec![int()])),
        "Box<int>"
    );
    assert_eq!(
        format_type(&Type::Module("shapes".to_string())),
        "module:shapes"
    );
}

#[test]
fn format_binary_op_all_operators() {
    let cases = [
        (BinaryOp::Add, "+"),
        (BinaryOp::Subtract, "-"),
        (BinaryOp::Multiply, "*"),
        (BinaryOp::Divide, "/"),
        (BinaryOp::Modulo, "%"),
        (BinaryOp::Exponent, "**"),
        (BinaryOp::Equal, "=="),
        (BinaryOp::NotEqual, "!="),
        (BinaryOp::Less, "<"),
        (BinaryOp::LessEqual, "<="),
        (BinaryOp::Greater, ">"),
        (BinaryOp::GreaterEqual, ">="),
        (BinaryOp::LogicalAnd, "&&"),
        (BinaryOp::LogicalOr, "||"),
        (BinaryOp::In, "in"),
        (BinaryOp::Assign, "="),
        (BinaryOp::AddAssign, "+="),
        (BinaryOp::SubtractAssign, "-="),
        (BinaryOp::MultiplyAssign, "*="),
        (BinaryOp::DivideAssign, "/="),
        (BinaryOp::ModuloAssign, "%="),
    ];
    for (op, expected) in cases {
        assert_eq!(format_binary_op(&op), expected, "mismatch for {op:?}");
    }
}

#[test]
fn format_span_location_renders_row_and_col() {
    let span = Span::new(12, 5);
    assert_eq!(
        mux_lang::semantics::format::format_span_location(&span),
        "12:5"
    );
}
