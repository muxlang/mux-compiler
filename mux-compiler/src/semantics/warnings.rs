//! High-confidence warnings derived from the recovered, type-checked AST.
//!
//! Warning collection is deliberately separate from error-producing semantic
//! checks. It runs only after semantic analysis has found no errors, so a
//! malformed program cannot produce a misleading warning cascade.

use super::const_fold::{ConstValue, fold};
use super::error::SemanticError;
use crate::ast::{
    AstNode, BinaryOp, ExpressionKind, ExpressionNode, FunctionNode, StatementKind, StatementNode,
};
use crate::diagnostic::{DiagnosticCode, SpanEdit};

pub(super) fn collect(nodes: &[AstNode]) -> Vec<SemanticError> {
    let mut warnings = Vec::new();
    for node in nodes {
        match node {
            AstNode::Function(function) => collect_function(function, &mut warnings),
            AstNode::Class { methods, .. } | AstNode::Interface { methods, .. } => {
                for method in methods {
                    collect_function(method, &mut warnings);
                }
            }
            AstNode::Statement(statement) => collect_statement(statement, &mut warnings),
            AstNode::Enum { .. } => {}
        }
    }
    warnings
}

fn collect_function(function: &FunctionNode, warnings: &mut Vec<SemanticError>) {
    collect_block(&function.body, warnings);
}

fn collect_block(statements: &[StatementNode], warnings: &mut Vec<SemanticError>) {
    let mut reachable = true;
    for statement in statements {
        if !reachable {
            warnings.push(SemanticError::new(
                DiagnosticCode::UnreachableCode,
                "statement is unreachable",
                statement.span,
            ));
        }

        collect_statement(statement, warnings);
        reachable = statement_can_fall_through(statement);
    }
}

fn collect_unreachable_block(statements: &[StatementNode], warnings: &mut Vec<SemanticError>) {
    for statement in statements {
        warnings.push(SemanticError::new(
            DiagnosticCode::UnreachableCode,
            "statement is unreachable",
            statement.span,
        ));
        collect_statement(statement, warnings);
    }
}

fn collect_statement(statement: &StatementNode, warnings: &mut Vec<SemanticError>) {
    match &statement.kind {
        StatementKind::If {
            cond,
            then_block,
            else_block,
        } => {
            warn_constant_condition(cond, warnings);
            collect_expression(cond, warnings);
            match fold(cond) {
                Some(ConstValue::Bool(true)) => {
                    collect_block(then_block, warnings);
                    if let Some(else_block) = else_block {
                        collect_unreachable_block(else_block, warnings);
                    }
                }
                Some(ConstValue::Bool(false)) => {
                    collect_unreachable_block(then_block, warnings);
                    if let Some(else_block) = else_block {
                        collect_block(else_block, warnings);
                    }
                }
                _ => {
                    collect_block(then_block, warnings);
                    if let Some(else_block) = else_block {
                        collect_block(else_block, warnings);
                    }
                }
            }
        }
        StatementKind::While { cond, body } => {
            warn_constant_condition(cond, warnings);
            collect_expression(cond, warnings);
            if matches!(fold(cond), Some(ConstValue::Bool(false))) {
                collect_unreachable_block(body, warnings);
            } else {
                collect_block(body, warnings);
            }
        }
        StatementKind::For { iter, body, .. } => {
            collect_expression(iter, warnings);
            collect_block(body, warnings);
        }
        StatementKind::Match { expr, arms } => {
            collect_expression(expr, warnings);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    collect_expression(guard, warnings);
                }
                collect_block(&arm.body, warnings);
            }
        }
        StatementKind::Return(Some(expression)) => collect_expression(expression, warnings),
        StatementKind::Expression(expression)
        | StatementKind::AutoDecl(_, _, expression)
        | StatementKind::TypedDecl(_, _, expression)
        | StatementKind::ConstDecl(_, _, expression) => collect_expression(expression, warnings),
        StatementKind::Block(statements) => collect_block(statements, warnings),
        StatementKind::Function(function) => collect_function(function, warnings),
        StatementKind::UninitDecl(_, _)
        | StatementKind::Import { .. }
        | StatementKind::Return(None)
        | StatementKind::Break
        | StatementKind::Continue => {}
    }
}

fn collect_expression(expression: &ExpressionNode, warnings: &mut Vec<SemanticError>) {
    match &expression.kind {
        ExpressionKind::Binary { left, right, .. } => {
            warn_redundant_boolean(expression, left, right, warnings);
            collect_expression(left, warnings);
            collect_expression(right, warnings);
        }
        ExpressionKind::Unary { expr, .. } => collect_expression(expr, warnings),
        ExpressionKind::Call { func, args } => {
            collect_expression(func, warnings);
            for arg in args {
                collect_expression(arg, warnings);
            }
        }
        ExpressionKind::FieldAccess { expr, .. }
        | ExpressionKind::ListAccess { expr, .. }
        | ExpressionKind::Slice { expr, .. } => collect_expression(expr, warnings),
        ExpressionKind::ListLiteral(elements)
        | ExpressionKind::SetLiteral(elements)
        | ExpressionKind::TupleLiteral(elements) => {
            for element in elements {
                collect_expression(element, warnings);
            }
        }
        ExpressionKind::MapLiteral { entries, .. } => {
            for (key, value) in entries {
                collect_expression(key, warnings);
                collect_expression(value, warnings);
            }
        }
        ExpressionKind::If {
            cond,
            then_expr,
            else_expr,
        } => {
            warn_constant_condition(cond, warnings);
            collect_expression(cond, warnings);
            collect_expression(then_expr, warnings);
            collect_expression(else_expr, warnings);
        }
        ExpressionKind::Lambda { body, .. } => collect_block(body, warnings),
        ExpressionKind::Literal(_)
        | ExpressionKind::None
        | ExpressionKind::Identifier(_)
        | ExpressionKind::GenericType(_, _) => {}
    }
}

fn warn_redundant_boolean(
    expression: &ExpressionNode,
    left: &ExpressionNode,
    right: &ExpressionNode,
    warnings: &mut Vec<SemanticError>,
) {
    let ExpressionKind::Binary { op, .. } = &expression.kind else {
        return;
    };
    let keep = match op {
        BinaryOp::LogicalAnd if is_boolean_literal(right, true) => Some(left),
        BinaryOp::LogicalAnd if is_boolean_literal(left, true) => Some(right),
        BinaryOp::LogicalOr if is_boolean_literal(right, false) => Some(left),
        BinaryOp::LogicalOr if is_boolean_literal(left, false) => Some(right),
        _ => None,
    };
    let Some(keep) = keep else {
        return;
    };

    let mut warning = SemanticError::new(
        DiagnosticCode::RedundantConstruct,
        "boolean expression contains a redundant operand",
        expression.span,
    );
    warning = warning.with_span_edit(SpanEdit::machine_applicable_source(
        expression.span,
        keep.span,
        DiagnosticCode::RedundantConstruct,
    ));
    warnings.push(warning);
}

fn is_boolean_literal(expression: &ExpressionNode, value: bool) -> bool {
    matches!(
        expression.kind,
        ExpressionKind::Literal(crate::ast::LiteralNode::Boolean(actual)) if actual == value
    )
}

fn warn_constant_condition(condition: &ExpressionNode, warnings: &mut Vec<SemanticError>) {
    let Some(ConstValue::Bool(value)) = fold(condition) else {
        return;
    };
    warnings.push(SemanticError::new(
        DiagnosticCode::ConstantCondition,
        format!("condition is always {value}"),
        condition.span,
    ));
}

fn statement_can_fall_through(statement: &StatementNode) -> bool {
    match &statement.kind {
        StatementKind::Return(_) | StatementKind::Break | StatementKind::Continue => false,
        StatementKind::Block(statements) => block_can_fall_through(statements),
        StatementKind::If {
            then_block,
            else_block: Some(else_block),
            ..
        } => block_can_fall_through(then_block) || block_can_fall_through(else_block),
        StatementKind::Match { arms, .. } if !arms.is_empty() => {
            arms.iter().any(|arm| block_can_fall_through(&arm.body))
        }
        StatementKind::While { cond, body } => {
            !matches!(fold(cond), Some(ConstValue::Bool(true))) || block_may_break_loop(body)
        }
        _ => true,
    }
}

fn block_can_fall_through(statements: &[StatementNode]) -> bool {
    statements.iter().all(statement_can_fall_through)
}

fn block_may_break_loop(statements: &[StatementNode]) -> bool {
    let mut reachable = true;
    for statement in statements {
        if reachable && statement_may_break_loop(statement) {
            return true;
        }
        reachable = statement_can_fall_through(statement);
    }
    false
}

fn statement_may_break_loop(statement: &StatementNode) -> bool {
    match &statement.kind {
        StatementKind::Break => true,
        StatementKind::Block(statements) => block_may_break_loop(statements),
        StatementKind::If {
            cond,
            then_block,
            else_block,
        } => match fold(cond) {
            Some(ConstValue::Bool(true)) => block_may_break_loop(then_block),
            Some(ConstValue::Bool(false)) => else_block
                .as_ref()
                .is_some_and(|else_block| block_may_break_loop(else_block)),
            _ => {
                block_may_break_loop(then_block)
                    || else_block
                        .as_ref()
                        .is_some_and(|else_block| block_may_break_loop(else_block))
            }
        },
        StatementKind::Match { arms, .. } => arms.iter().any(|arm| block_may_break_loop(&arm.body)),
        // A break in a nested loop targets that loop, not this one.
        StatementKind::While { .. } | StatementKind::For { .. } | StatementKind::Function(_) => {
            false
        }
        _ => false,
    }
}
