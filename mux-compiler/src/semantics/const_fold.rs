//! Constant folding for compile-time panic detection.
//!
//! Evaluates an expression to a constant when every input is known, mirroring
//! runtime semantics exactly. Whenever the result could differ from what the
//! compiled program computes (integer overflow, NaN, unsupported operators,
//! unknown identifiers), folding returns `None` and the caller stays silent:
//! Mux has no warning level, so a check built on the folder must only fire
//! when the panic is provable.

use std::collections::HashMap;
use std::fmt;

use crate::ast::{BinaryOp, ExpressionKind, ExpressionNode, LiteralNode, UnaryOp};

/// A compile-time constant value with the same semantics as its runtime type.
#[derive(Debug, Clone, PartialEq)]
pub enum ConstValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Char(char),
}

impl fmt::Display for ConstValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConstValue::Int(v) => write!(f, "{}", v),
            ConstValue::Float(v) => write!(f, "{}", v),
            ConstValue::Bool(v) => write!(f, "{}", v),
            ConstValue::Str(v) => write!(f, "{}", v),
            ConstValue::Char(v) => write!(f, "{}", v),
        }
    }
}

/// Fold an expression with no bindings: only literal-derived values succeed.
pub fn fold(expr: &ExpressionNode) -> Option<ConstValue> {
    fold_with_env(expr, &HashMap::new())
}

/// Fold an expression, resolving bare identifiers through `env`. Identifiers
/// absent from `env` make the whole fold return `None`.
pub fn fold_with_env(
    expr: &ExpressionNode,
    env: &HashMap<String, ConstValue>,
) -> Option<ConstValue> {
    match &expr.kind {
        ExpressionKind::Literal(lit) => Some(match lit {
            LiteralNode::Integer(v) => ConstValue::Int(*v),
            LiteralNode::Float(v) => ConstValue::Float(v.into_inner()),
            LiteralNode::Boolean(v) => ConstValue::Bool(*v),
            LiteralNode::String(v) => ConstValue::Str(v.clone()),
            LiteralNode::Char(v) => ConstValue::Char(*v),
        }),
        ExpressionKind::Identifier(name) => env.get(name).cloned(),
        ExpressionKind::Unary { op, expr, .. } => {
            let value = fold_with_env(expr, env)?;
            fold_unary(op, value)
        }
        ExpressionKind::Binary {
            left, op, right, ..
        } => {
            // No short-circuit folding: both sides must be known, so skipping
            // a fold-true predicate in codegen can never drop an evaluation
            // the runtime would have performed.
            let l = fold_with_env(left, env)?;
            let r = fold_with_env(right, env)?;
            fold_binary(l, op, r)
        }
        _ => None,
    }
}

fn fold_unary(op: &UnaryOp, value: ConstValue) -> Option<ConstValue> {
    match (op, value) {
        (UnaryOp::Neg, ConstValue::Int(v)) => v.checked_neg().map(ConstValue::Int),
        (UnaryOp::Neg, ConstValue::Float(v)) => Some(ConstValue::Float(-v)),
        (UnaryOp::Not, ConstValue::Bool(v)) => Some(ConstValue::Bool(!v)),
        _ => None,
    }
}

fn fold_binary(left: ConstValue, op: &BinaryOp, right: ConstValue) -> Option<ConstValue> {
    use ConstValue::*;
    match (left, right) {
        (Int(l), Int(r)) => fold_int_op(l, op, r),
        (Float(l), Float(r)) => fold_float_op(l, op, r),
        (Bool(l), Bool(r)) => match op {
            BinaryOp::Equal => Some(Bool(l == r)),
            BinaryOp::NotEqual => Some(Bool(l != r)),
            BinaryOp::LogicalAnd => Some(Bool(l && r)),
            BinaryOp::LogicalOr => Some(Bool(l || r)),
            _ => None,
        },
        (Str(l), Str(r)) => match op {
            BinaryOp::Add => Some(Str(l + &r)),
            BinaryOp::Equal => Some(Bool(l == r)),
            BinaryOp::NotEqual => Some(Bool(l != r)),
            _ => None,
        },
        (Char(l), Char(r)) => match op {
            BinaryOp::Equal => Some(Bool(l == r)),
            BinaryOp::NotEqual => Some(Bool(l != r)),
            _ => None,
        },
        _ => None,
    }
}

fn fold_int_op(l: i64, op: &BinaryOp, r: i64) -> Option<ConstValue> {
    use ConstValue::{Bool, Int};
    match op {
        // checked_* bails on overflow (and on division by zero / MIN / -1),
        // so the folder never has to guess wrap behavior. Division by zero is
        // reported by its dedicated check, not through the folder.
        BinaryOp::Add => l.checked_add(r).map(Int),
        BinaryOp::Subtract => l.checked_sub(r).map(Int),
        BinaryOp::Multiply => l.checked_mul(r).map(Int),
        BinaryOp::Divide => l.checked_div(r).map(Int),
        BinaryOp::Modulo => l.checked_rem(r).map(Int),
        BinaryOp::Equal => Some(Bool(l == r)),
        BinaryOp::NotEqual => Some(Bool(l != r)),
        BinaryOp::Less => Some(Bool(l < r)),
        BinaryOp::LessEqual => Some(Bool(l <= r)),
        BinaryOp::Greater => Some(Bool(l > r)),
        BinaryOp::GreaterEqual => Some(Bool(l >= r)),
        // Exponent lowers through a runtime routine; not mirrored here.
        _ => None,
    }
}

fn fold_float_op(l: f64, op: &BinaryOp, r: f64) -> Option<ConstValue> {
    use ConstValue::{Bool, Float};
    let arith = |v: f64| {
        // NaN comparison semantics are not worth mirroring; bail instead.
        if v.is_nan() { None } else { Some(Float(v)) }
    };
    match op {
        BinaryOp::Add => arith(l + r),
        BinaryOp::Subtract => arith(l - r),
        BinaryOp::Multiply => arith(l * r),
        BinaryOp::Divide => arith(l / r),
        BinaryOp::Modulo => arith(l % r),
        // Literals are never NaN and arithmetic bails on NaN, so these
        // ordered comparisons match LLVM's ordered fcmp on every input
        // that can reach them.
        BinaryOp::Equal => Some(Bool(l == r)),
        BinaryOp::NotEqual => Some(Bool(l != r)),
        BinaryOp::Less => Some(Bool(l < r)),
        BinaryOp::LessEqual => Some(Bool(l <= r)),
        BinaryOp::Greater => Some(Bool(l > r)),
        BinaryOp::GreaterEqual => Some(Bool(l >= r)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{BinaryOp, ExpressionKind, ExpressionNode, LiteralNode, UnaryOp};
    use crate::lexer::Span;

    fn lit(l: LiteralNode) -> ExpressionNode {
        l.into()
    }

    fn int(v: i64) -> ExpressionNode {
        lit(LiteralNode::Integer(v))
    }

    fn ident(name: &str) -> ExpressionNode {
        name.into()
    }

    fn binary(left: ExpressionNode, op: BinaryOp, right: ExpressionNode) -> ExpressionNode {
        ExpressionNode {
            kind: ExpressionKind::Binary {
                left: Box::new(left),
                op,
                op_span: Span::new(0, 0),
                right: Box::new(right),
            },
            span: Span::new(0, 0),
        }
    }

    fn unary(op: UnaryOp, expr: ExpressionNode) -> ExpressionNode {
        ExpressionNode {
            kind: ExpressionKind::Unary {
                op,
                op_span: Span::new(0, 0),
                expr: Box::new(expr),
                postfix: false,
            },
            span: Span::new(0, 0),
        }
    }

    #[test]
    fn folds_int_arithmetic_and_comparisons() {
        let e = binary(int(2), BinaryOp::Add, int(3));
        assert_eq!(fold(&e), Some(ConstValue::Int(5)));
        let e = binary(int(1), BinaryOp::Greater, int(2));
        assert_eq!(fold(&e), Some(ConstValue::Bool(false)));
    }

    #[test]
    fn bails_on_int_overflow_and_zero_division() {
        let e = binary(int(i64::MAX), BinaryOp::Add, int(1));
        assert_eq!(fold(&e), None);
        let e = binary(int(i64::MIN), BinaryOp::Divide, int(-1));
        assert_eq!(fold(&e), None);
        let e = binary(int(10), BinaryOp::Divide, int(0));
        assert_eq!(fold(&e), None);
        let e = binary(int(10), BinaryOp::Modulo, int(0));
        assert_eq!(fold(&e), None);
    }

    #[test]
    fn bails_on_exponent() {
        let e = binary(int(2), BinaryOp::Exponent, int(3));
        assert_eq!(fold(&e), None);
    }

    #[test]
    fn folds_float_ops_and_bails_on_nan() {
        use ordered_float::OrderedFloat;
        let f = |v: f64| lit(LiteralNode::Float(OrderedFloat(v)));
        let e = binary(f(1.0), BinaryOp::NotEqual, f(0.0));
        assert_eq!(fold(&e), Some(ConstValue::Bool(true)));
        // 0.0 / 0.0 is NaN: the folder must stay silent.
        let e = binary(f(0.0), BinaryOp::Divide, f(0.0));
        assert_eq!(fold(&e), None);
        // float / 0.0 is inf at runtime, not a panic; folding keeps that value.
        let e = binary(f(1.0), BinaryOp::Divide, f(0.0));
        assert_eq!(fold(&e), Some(ConstValue::Float(f64::INFINITY)));
    }

    #[test]
    fn folds_logical_string_char_ops() {
        let e = binary(lit(true.into()), BinaryOp::LogicalAnd, lit(false.into()));
        assert_eq!(fold(&e), Some(ConstValue::Bool(false)));
        let e = binary(lit("a".into()), BinaryOp::Add, lit("b".into()));
        assert_eq!(fold(&e), Some(ConstValue::Str("ab".to_string())));
        let e = binary(lit('x'.into()), BinaryOp::Equal, lit('x'.into()));
        assert_eq!(fold(&e), Some(ConstValue::Bool(true)));
    }

    #[test]
    fn folds_unary_and_env_identifiers() {
        let e = unary(UnaryOp::Neg, int(5));
        assert_eq!(fold(&e), Some(ConstValue::Int(-5)));
        let e = unary(UnaryOp::Not, lit(true.into()));
        assert_eq!(fold(&e), Some(ConstValue::Bool(false)));

        let mut env = HashMap::new();
        env.insert("b".to_string(), ConstValue::Int(0));
        let e = binary(ident("b"), BinaryOp::NotEqual, int(0));
        assert_eq!(fold_with_env(&e, &env), Some(ConstValue::Bool(false)));
        // Unknown identifiers poison the whole fold.
        let e = binary(ident("unknown"), BinaryOp::NotEqual, int(0));
        assert_eq!(fold_with_env(&e, &env), None);
    }
}
