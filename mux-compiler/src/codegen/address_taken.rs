//! Which variables have their address taken anywhere in the program.
//!
//! A scalar normally lives in a slot of its own width, but `&x` has to produce
//! something that means the same as `&list[0]`, and a list element is a boxed
//! `*mut Value`. So a variable whose address is taken keeps a boxed slot, and
//! this pass is what decides that before any slot is allocated.
//!
//! Deciding it up front rather than promoting the variable when `&x` is reached
//! is not a detail. A promotion emitted mid-body allocates its cell wherever
//! control happens to be, so `&x` inside an `if` produced an alloca that did not
//! dominate the scope cleanup ("instruction does not dominate all uses"); it
//! re-ran on every pass through a loop, resetting the variable and dropping the
//! previous box; and rebinding a global into the function-local table made
//! writes through the reference land on a copy while the global kept its old
//! value.
//!
//! The set is keyed by bare name and collected program-wide, so two different
//! variables sharing a name are both boxed when either has its address taken.
//! That is deliberate: it costs the optimization on a name, never correctness,
//! and it needs no scope tracking to stay right.

use std::collections::HashSet;

use crate::ast::{AstNode, ExpressionKind, ExpressionNode, StatementKind, StatementNode, UnaryOp};

/// Every name that appears as the operand of `&` anywhere in `nodes`.
pub(super) fn collect(nodes: &[AstNode]) -> HashSet<String> {
    let mut found = HashSet::new();
    for node in nodes {
        match node {
            AstNode::Function(func) => visit_block(&func.body, &mut found),
            AstNode::Class { methods, .. } => {
                for method in methods {
                    visit_block(&method.body, &mut found);
                }
            }
            AstNode::Statement(stmt) => visit_statement(stmt, &mut found),
            AstNode::Enum { .. } | AstNode::Interface { .. } => {}
        }
    }
    found
}

fn visit_block(stmts: &[StatementNode], found: &mut HashSet<String>) {
    for stmt in stmts {
        visit_statement(stmt, found);
    }
}

fn visit_statement(stmt: &StatementNode, found: &mut HashSet<String>) {
    match &stmt.kind {
        StatementKind::AutoDecl(_, _, expr)
        | StatementKind::TypedDecl(_, _, expr)
        | StatementKind::ConstDecl(_, _, expr)
        | StatementKind::Expression(expr) => visit_expression(expr, found),
        // No initializer, so no expression to walk.
        StatementKind::Return(expr) => {
            if let Some(expr) = expr {
                visit_expression(expr, found);
            }
        }
        StatementKind::Function(func) => visit_block(&func.body, found),
        StatementKind::If {
            cond,
            then_block,
            else_block,
        } => {
            visit_expression(cond, found);
            visit_block(then_block, found);
            if let Some(else_block) = else_block {
                visit_block(else_block, found);
            }
        }
        StatementKind::While { cond, body } => {
            visit_expression(cond, found);
            visit_block(body, found);
        }
        StatementKind::For { iter, body, .. } => {
            visit_expression(iter, found);
            visit_block(body, found);
        }
        StatementKind::Match { expr, arms } => {
            visit_expression(expr, found);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    visit_expression(guard, found);
                }
                visit_block(&arm.body, found);
            }
        }
        StatementKind::Block(stmts) => visit_block(stmts, found),
        StatementKind::UninitDecl(_, _)
        | StatementKind::Import { .. }
        | StatementKind::Break
        | StatementKind::Continue => {}
    }
}

fn visit_expression(expr: &ExpressionNode, found: &mut HashSet<String>) {
    match &expr.kind {
        ExpressionKind::Unary {
            op: UnaryOp::Ref,
            expr: operand,
            ..
        } => {
            if let ExpressionKind::Identifier(name) = &operand.kind {
                found.insert(name.clone());
            }
            visit_expression(operand, found);
        }
        ExpressionKind::Unary { expr: operand, .. } => visit_expression(operand, found),
        ExpressionKind::Binary { left, right, .. } => {
            visit_expression(left, found);
            visit_expression(right, found);
        }
        ExpressionKind::Call { func, args } => {
            visit_expression(func, found);
            for arg in args {
                visit_expression(arg, found);
            }
        }
        ExpressionKind::FieldAccess { expr, .. } => visit_expression(expr, found),
        ExpressionKind::ListAccess { expr, index } => {
            visit_expression(expr, found);
            visit_expression(index, found);
        }
        ExpressionKind::Slice { expr, start, end } => {
            visit_expression(expr, found);
            for bound in [start, end].into_iter().flatten() {
                visit_expression(bound, found);
            }
        }
        ExpressionKind::ListLiteral(items)
        | ExpressionKind::SetLiteral(items)
        | ExpressionKind::TupleLiteral(items) => {
            for item in items {
                visit_expression(item, found);
            }
        }
        ExpressionKind::MapLiteral { entries, .. } => {
            for (key, value) in entries {
                visit_expression(key, found);
                visit_expression(value, found);
            }
        }
        ExpressionKind::If {
            cond,
            then_expr,
            else_expr,
        } => {
            visit_expression(cond, found);
            visit_expression(then_expr, found);
            visit_expression(else_expr, found);
        }
        ExpressionKind::Lambda { body, .. } => visit_block(body, found),
        ExpressionKind::Identifier(_)
        | ExpressionKind::Literal(_)
        | ExpressionKind::GenericType(..)
        | ExpressionKind::None => {}
    }
}
