//! Token types and token structure for the Mux language lexer.

use ordered_float::OrderedFloat;

use super::span::Span;

/// A token with its type and source location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub token_type: TokenType,
    pub span: Span,
}

impl Token {
    #[must_use]
    pub fn new(token: TokenType, span: Span) -> Token {
        Token {
            token_type: token,
            span,
        }
    }
}

/// All possible token types in the Mux language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenType {
    // Keywords
    Auto,
    Func,
    Returns,
    Return,
    If,
    Else,
    For,
    While,
    Match,
    Const,
    Class,
    Interface,
    Enum,
    Import,
    Is,
    As,
    In,
    Break,
    Continue,
    None,
    Common,
    Where,

    // Delimiters
    OpenParen,    // (
    CloseParen,   // )
    OpenBrace,    // {
    CloseBrace,   // }
    OpenBracket,  // [
    CloseBracket, // ]
    Dot,          // .
    DotDot,       // ..
    Comma,
    Colon,

    // Operators
    Eq,
    Plus,
    Minus,
    Star,
    StarStar,
    Slash,
    Percent,
    Lt,
    Gt,
    Le,
    Ge,
    EqEq,
    NotEq,
    Bang,
    Incr,
    Decr,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    And,
    Or,
    Ref,

    // Literals
    Int(i64),
    Float(OrderedFloat<f64>),
    Bool(bool),
    Char(char),
    Str(String),
    Underscore,

    // Identifiers
    Id(String),

    // Special
    Eof,
    NewLine,

    // Comments
    LineComment(String),
    MultilineComment(String),
}
