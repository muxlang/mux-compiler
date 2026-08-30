use crate::lexer::{Span, Token, TokenType};

use super::error::ParseError;

#[derive(Debug, Clone, PartialEq)]
pub enum TypeKind {
    Primitive(PrimitiveType),
    Named(String, Vec<TypeNode>),
    TraitObject(Box<TypeNode>),
    Function {
        params: Vec<TypeNode>,
        returns: Box<TypeNode>,
    },
    Reference(Box<TypeNode>),
    List(Box<TypeNode>),
    Map(Box<TypeNode>, Box<TypeNode>),
    Set(Box<TypeNode>),
    Tuple(Box<TypeNode>, Box<TypeNode>),

    Auto,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeNode {
    pub kind: TypeKind,
    pub span: Span,
}

impl super::Spanned for TypeNode {
    fn span(&self) -> &Span {
        &self.span
    }
}

impl From<PrimitiveType> for TypeNode {
    fn from(prim: PrimitiveType) -> Self {
        TypeNode {
            kind: TypeKind::Primitive(prim),
            span: Span::new(0, 0),
        }
    }
}

impl From<&str> for TypeNode {
    fn from(s: &str) -> Self {
        TypeNode {
            kind: TypeKind::Named(s.to_string(), Vec::new()),
            span: Span::new(0, 0),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrimitiveType {
    Int,
    Float,
    Bool,
    Char,
    Str,
    Void,
    Auto,
}

impl PrimitiveType {
    /// Parses a built-in primitive type token.
    ///
    /// # Errors
    ///
    /// Returns an error when `token` is not a recognized primitive type.
    pub fn parse(token: &Token) -> Result<PrimitiveType, ParseError> {
        match &token.token_type {
            TokenType::Id(value) => match value.as_str() {
                "int" => Ok(PrimitiveType::Int),
                "float" => Ok(PrimitiveType::Float),
                "bool" => Ok(PrimitiveType::Bool),
                "char" => Ok(PrimitiveType::Char),
                "string" => Ok(PrimitiveType::Str),
                "void" => Ok(PrimitiveType::Void),
                "auto" => Ok(PrimitiveType::Auto),
                _ => Err(ParseError::new(
                    format!("Unknown primitive type: {value}"),
                    token.span,
                )),
            },
            _ => Err(ParseError::new(
                format!("Expected an identifier, found {:?}", token.token_type),
                token.span,
            )),
        }
    }
}
