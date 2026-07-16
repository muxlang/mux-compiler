use crate::lexer::Span;

use super::Spanned;
use super::literals::{EnumVariant, Field, LiteralNode, Param, TraitBound, TraitRef, WhereClause};
use super::operators::BinaryOp;
use super::operators::UnaryOp;
use super::patterns::MatchArm;
use super::types::TypeNode;

#[derive(Debug, Clone, PartialEq)]
pub enum AstNode {
    Function(FunctionNode),
    Class {
        name: String,
        type_params: Vec<(String, Vec<TraitBound>)>,
        traits: Vec<TraitRef>,
        fields: Vec<Field>,
        methods: Vec<FunctionNode>,
        where_clause: Option<WhereClause>,
        span: Span,
    },
    Interface {
        name: String,
        type_params: Vec<(String, Vec<TraitBound>)>,
        fields: Vec<Field>,
        methods: Vec<FunctionNode>,
        span: Span,
    },
    Enum {
        name: String,
        type_params: Vec<(String, Vec<TraitBound>)>,
        variants: Vec<EnumVariant>,
        span: Span,
    },
    Statement(StatementNode),
}

impl Spanned for AstNode {
    fn span(&self) -> &Span {
        match self {
            AstNode::Function(func) => &func.span,
            AstNode::Class { span, .. } => span,
            AstNode::Interface { span, .. } => span,
            AstNode::Enum { span, .. } => span,
            AstNode::Statement(stmt) => stmt.span(),
        }
    }
}

impl AstNode {
    // note, we only convert statement and function variants to statements.
    // class, interface, and enum are top-level declarations in the language
    // and cannot appear as statements, they must be handled separately.
    pub fn into_statement(self) -> Option<StatementNode> {
        match self {
            AstNode::Statement(stmt) => Some(stmt),
            AstNode::Function(func) => {
                let span = func.span;
                Some(StatementNode {
                    kind: StatementKind::Function(func),
                    span,
                })
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImportSpec {
    // import logger (as alias)
    Module {
        alias: Option<String>,
    },
    // import logger.log (as alias)
    Item {
        item: String,
        alias: Option<String>,
    },
    // import logger.(log, error, warn as w)
    Items {
        items: Vec<(String, Option<String>)>,
    },
    // import logger.*
    Wildcard,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StatementKind {
    AutoDecl(String, TypeNode, ExpressionNode),
    TypedDecl(String, TypeNode, ExpressionNode),
    ConstDecl(String, TypeNode, ExpressionNode),
    Function(FunctionNode),
    Import {
        module_path: String,
        spec: ImportSpec,
    },
    Return(Option<ExpressionNode>),
    If {
        cond: ExpressionNode,
        then_block: Vec<StatementNode>,
        else_block: Option<Vec<StatementNode>>,
    },
    While {
        cond: ExpressionNode,
        body: Vec<StatementNode>,
    },
    For {
        var: String,
        var_type: TypeNode,
        iter: ExpressionNode,
        body: Vec<StatementNode>,
    },
    Match {
        expr: ExpressionNode,
        arms: Vec<MatchArm>,
    },
    Break,
    Continue,
    Expression(ExpressionNode),
    Block(Vec<StatementNode>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct StatementNode {
    pub kind: StatementKind,
    pub span: Span,
}

impl Spanned for StatementNode {
    fn span(&self) -> &Span {
        &self.span
    }
}

impl From<ExpressionNode> for StatementNode {
    fn from(expr: ExpressionNode) -> Self {
        StatementNode {
            kind: StatementKind::Expression(expr),
            span: Span::new(0, 0),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExpressionKind {
    Literal(LiteralNode),
    None,
    Identifier(String),
    Binary {
        left: Box<ExpressionNode>,
        op: BinaryOp,
        op_span: Span,
        right: Box<ExpressionNode>,
    },
    Unary {
        op: UnaryOp,
        op_span: Span,
        expr: Box<ExpressionNode>,
        postfix: bool,
    },
    Call {
        func: Box<ExpressionNode>,
        args: Vec<ExpressionNode>,
    },
    FieldAccess {
        expr: Box<ExpressionNode>,
        field: String,
    },
    ListAccess {
        expr: Box<ExpressionNode>,
        index: Box<ExpressionNode>,
    },
    ListLiteral(Vec<ExpressionNode>),
    MapLiteral {
        key_type: Box<TypeNode>,
        value_type: Box<TypeNode>,
        entries: Vec<(ExpressionNode, ExpressionNode)>,
    },
    SetLiteral(Vec<ExpressionNode>),
    TupleLiteral(Vec<ExpressionNode>),
    If {
        cond: Box<ExpressionNode>,
        then_expr: Box<ExpressionNode>,
        else_expr: Box<ExpressionNode>,
    },
    Lambda {
        params: Vec<Param>,
        return_type: TypeNode,
        body: Vec<StatementNode>,
        where_clause: Option<WhereClause>,
    },
    GenericType(String, Vec<TypeNode>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExpressionNode {
    pub kind: ExpressionKind,
    pub span: Span,
}

impl Spanned for ExpressionNode {
    fn span(&self) -> &Span {
        &self.span
    }
}

impl From<LiteralNode> for ExpressionNode {
    fn from(lit: LiteralNode) -> Self {
        ExpressionNode {
            kind: ExpressionKind::Literal(lit),
            span: Span::new(0, 0),
        }
    }
}

impl From<&str> for ExpressionNode {
    fn from(ident: &str) -> Self {
        ExpressionNode {
            kind: ExpressionKind::Identifier(ident.to_string()),
            span: Span::new(0, 0),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionNode {
    pub name: String,
    pub type_params: Vec<(String, Vec<TraitBound>)>,
    pub params: Vec<Param>,
    pub return_type: TypeNode,
    pub body: Vec<StatementNode>,
    pub span: Span,
    pub is_common: bool,
    pub where_clause: Option<WhereClause>,
}
