use crate::lexer::Span;
use ordered_float::OrderedFloat;

use super::types::TypeNode;

#[derive(Debug, Clone, PartialEq)]
pub enum LiteralNode {
    Float(OrderedFloat<f64>),
    Integer(i64),
    String(String),
    Boolean(bool),
    Char(char),
}

impl From<OrderedFloat<f64>> for LiteralNode {
    fn from(f: OrderedFloat<f64>) -> Self {
        LiteralNode::Float(f)
    }
}

impl From<i64> for LiteralNode {
    fn from(i: i64) -> Self {
        LiteralNode::Integer(i)
    }
}

impl From<String> for LiteralNode {
    fn from(s: String) -> Self {
        LiteralNode::String(s)
    }
}

impl From<&str> for LiteralNode {
    fn from(s: &str) -> Self {
        LiteralNode::String(s.to_string())
    }
}

impl From<bool> for LiteralNode {
    fn from(b: bool) -> Self {
        LiteralNode::Boolean(b)
    }
}

impl From<char> for LiteralNode {
    fn from(c: char) -> Self {
        LiteralNode::Char(c)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub type_: TypeNode,
    pub default_value: Option<super::nodes::ExpressionNode>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    pub type_: TypeNode,
    pub is_generic_param: bool,
    pub is_const: bool,
    pub default_value: Option<super::nodes::ExpressionNode>,
    pub where_clause: Option<WhereClause>,
}

/// A `where { ... }` runtime constraint block: boolean predicates that must all
/// hold, separated like set-literal elements (commas, optional newlines).
#[derive(Debug, Clone, PartialEq)]
pub struct WhereClause {
    pub predicates: Vec<super::nodes::ExpressionNode>,
    pub span: Span,
}

impl super::Spanned for WhereClause {
    fn span(&self) -> &Span {
        &self.span
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TraitBound {
    pub name: String,
    pub type_params: Vec<TypeNode>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TraitRef {
    pub name: String,
    pub type_args: Vec<TypeNode>,
}

pub type EnumVariantField = (Option<String>, TypeNode);

#[derive(Debug, Clone, PartialEq)]
pub struct EnumVariant {
    pub name: String,
    pub data: Option<Vec<EnumVariantField>>,
    pub where_clause: Option<WhereClause>,
}

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub struct ConstDeclNode {
    pub name: String,
    pub type_: TypeNode,
    pub value: super::nodes::ExpressionNode,
    pub span: Span,
}

impl super::Spanned for ConstDeclNode {
    fn span(&self) -> &Span {
        &self.span
    }
}
