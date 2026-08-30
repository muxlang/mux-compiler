use crate::lexer::{Token, TokenType};
use std::fmt;

use super::error::ParseError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Precedence {
    Assignment,
    Or,
    And,
    Equality,
    Comparison,
    Term,
    Factor,
    Exponent,
    Unary,
    Call,
    Primary,
}

impl Precedence {
    #[must_use]
    pub fn next_higher(self) -> Self {
        match self {
            Precedence::Assignment => Precedence::Or,
            Precedence::Or => Precedence::And,
            Precedence::And => Precedence::Equality,
            Precedence::Equality => Precedence::Comparison,
            Precedence::Comparison => Precedence::Term,
            Precedence::Term => Precedence::Factor,
            Precedence::Factor => Precedence::Exponent,
            Precedence::Exponent => Precedence::Unary,
            Precedence::Unary => Precedence::Call,
            Precedence::Call | Precedence::Primary => Precedence::Primary,
        }
    }
}

impl fmt::Display for Precedence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum BinaryOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    Modulo,
    Exponent,

    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,

    LogicalAnd,
    LogicalOr,

    In,

    Assign,
    AddAssign,
    SubtractAssign,
    MultiplyAssign,
    DivideAssign,
    ModuloAssign,
}

impl BinaryOp {
    #[must_use]
    pub fn is_assignment(&self) -> bool {
        matches!(
            self,
            BinaryOp::Assign
                | BinaryOp::AddAssign
                | BinaryOp::SubtractAssign
                | BinaryOp::MultiplyAssign
                | BinaryOp::DivideAssign
                | BinaryOp::ModuloAssign
        )
    }

    #[must_use]
    pub fn is_right_associative(&self) -> bool {
        matches!(self, BinaryOp::Exponent) || self.is_assignment()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum UnaryOp {
    Neg,
    Not,
    Ref,
    Incr,
    Decr,
    Deref,
}

impl UnaryOp {
    /// Parses a unary operator token.
    ///
    /// # Errors
    ///
    /// Returns an error when `token` is not a unary operator.
    pub fn parse(token: &Token) -> Result<UnaryOp, ParseError> {
        match &token.token_type {
            TokenType::Minus => Ok(UnaryOp::Neg),
            TokenType::Bang => Ok(UnaryOp::Not),
            TokenType::Ref => Ok(UnaryOp::Ref),
            TokenType::Incr => Ok(UnaryOp::Incr),
            TokenType::Decr => Ok(UnaryOp::Decr),
            TokenType::Star => Ok(UnaryOp::Deref),
            _ => Err(ParseError::new(
                format!(
                    "Unexpected token type for unary operation: {:?}",
                    token.token_type
                ),
                token.span,
            )),
        }
    }
}
