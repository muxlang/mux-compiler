//! Abstract Syntax Tree (AST) definitions for the Mux language.
//!
//! This module contains all the AST node types that are produced by the parser
//! and consumed by the semantic analyzer and code generator.

use crate::lexer::Span;

pub mod error;
pub mod literals;
pub mod nodes;
pub mod operators;
pub mod patterns;
pub mod types;

// Re-export all public types for convenient access
pub use error::ParseError;
pub use literals::{
    EnumVariant, EnumVariantField, Field, LiteralNode, Param, TraitBound, TraitRef, WhereClause,
};
pub use nodes::{
    AstNode, ExpressionKind, ExpressionNode, FunctionNode, ImportSpec, StatementKind, StatementNode,
};
pub use operators::{BinaryOp, Precedence, UnaryOp};
pub use patterns::{MatchArm, PatternNode};
pub use types::{PrimitiveType, TypeKind, TypeNode};

/// Trait for AST nodes that have source location information.
pub trait Spanned {
    fn span(&self) -> &Span;
}

/// Extension trait for combining spans.
pub trait SpanExt {
    fn combine(&self, other: &Span) -> Span;
}

impl SpanExt for Span {
    fn combine(&self, other: &Span) -> Span {
        let mut span = *self;
        if let (Some(end_row), Some(end_col)) = (other.row_end, other.col_end) {
            span.complete(end_row, end_col);
        }
        span
    }
}
