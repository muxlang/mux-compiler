//! Definite-assignment analysis for declarations without an initializer.
//!
//! `Type name` (issue #393) separates declaring a variable from assigning it,
//! so that the flat error-handling idiom works for a class type:
//!
//! ```text
//! TcpStream stream
//! match listener.accept() {
//!     ok(s) { stream = s }
//!     err(e) { return err(e) }
//! }
//! // stream is definitely assigned here
//! ```
//!
//! Separating them is only safe if reading before the assignment is rejected -
//! otherwise the slot holds whatever was there, which for a class type means a
//! segfault rather than a diagnostic.
//!
//! The analysis is deliberately flow-SENSITIVE. A flow-insensitive version
//! ("was there an assignment anywhere") would accept
//! `Thing t` / `if cond { t = ... }` / `use(t)`, which is exactly the case that
//! crashes. So a branch counts only when EVERY path through it either assigns
//! or leaves - which is what makes the `match` above accept while an `if` with
//! no `else` does not.
//!
//! Loops never count: a `while` or `for` body may run zero times.

use crate::ast::{ExpressionKind, ExpressionNode, StatementKind, StatementNode};

/// What a statement guarantees about a name by the time control leaves it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Flow {
    /// Falls through without assigning.
    FallsThrough,
    /// Falls through, and the name is assigned on every path that gets here.
    Assigns,
    /// Control does not reach the following statement (return, break,
    /// continue). Vacuously satisfies the requirement, since nothing after it
    /// can read the name on this path.
    Diverges,
}

impl Flow {
    fn satisfied(self) -> bool {
        matches!(self, Flow::Assigns | Flow::Diverges)
    }
}

/// Whether `name` is definitely assigned by the end of `stmts`, and whether a
/// read of it appears first.
pub(super) fn analyze_block(stmts: &[StatementNode], name: &str) -> Flow {
    let mut state = Flow::FallsThrough;
    for stmt in stmts {
        if state == Flow::Diverges {
            break;
        }
        let step = analyze_statement(stmt, name);
        state = match (state, step) {
            (_, Flow::Diverges) => Flow::Diverges,
            (Flow::Assigns, _) | (_, Flow::Assigns) => Flow::Assigns,
            _ => Flow::FallsThrough,
        };
    }
    state
}

fn analyze_statement(stmt: &StatementNode, name: &str) -> Flow {
    match &stmt.kind {
        StatementKind::Return(_) | StatementKind::Break | StatementKind::Continue => Flow::Diverges,

        StatementKind::Expression(expr) => {
            if assigns_name(expr, name) {
                Flow::Assigns
            } else {
                Flow::FallsThrough
            }
        }

        StatementKind::Block(inner) => analyze_block(inner, name),

        // Both arms must satisfy it. An `if` with no `else` never can, which is
        // the case a flow-insensitive check would wrongly accept.
        StatementKind::If {
            then_block,
            else_block,
            ..
        } => {
            let then_flow = analyze_block(then_block, name);
            let Some(else_block) = else_block else {
                return Flow::FallsThrough;
            };
            let else_flow = analyze_block(else_block, name);
            combine_branches(then_flow, else_flow)
        }

        // Every arm must satisfy it. Exhaustiveness is checked separately, so
        // "every arm" is the same as "every path".
        StatementKind::Match { arms, .. } => {
            if arms.is_empty() {
                return Flow::FallsThrough;
            }
            arms.iter()
                .map(|arm| analyze_block(&arm.body, name))
                .reduce(combine_branches)
                .unwrap_or(Flow::FallsThrough)
        }

        // A loop body may not run, so it never establishes an assignment.
        _ => Flow::FallsThrough,
    }
}

/// Two alternative paths: the weaker guarantee wins, except that a diverging
/// path imposes nothing on the other.
fn combine_branches(a: Flow, b: Flow) -> Flow {
    match (a, b) {
        (Flow::Diverges, other) | (other, Flow::Diverges) => other,
        (Flow::Assigns, Flow::Assigns) => Flow::Assigns,
        _ => Flow::FallsThrough,
    }
}

/// Whether this expression assigns to `name` at its top level.
///
/// A compound assignment (`x += 1`) READS before it writes, so it does not
/// count - the read is the thing being rejected.
fn assigns_name(expr: &ExpressionNode, name: &str) -> bool {
    let ExpressionKind::Binary { left, op, .. } = &expr.kind else {
        return false;
    };
    if !matches!(op, crate::ast::BinaryOp::Assign) {
        return false;
    }
    matches!(&left.kind, ExpressionKind::Identifier(n) if n == name)
}

/// Whether `stmts` reads `name` before any statement that assigns it.
///
/// Reported separately from the flow result so the error can point at the read
/// rather than at the declaration.
pub(super) fn first_read_before_assignment<'a>(
    stmts: &'a [StatementNode],
    name: &str,
) -> Option<&'a ExpressionNode> {
    for stmt in stmts {
        if let Some(found) = read_in_statement(stmt, name) {
            return Some(found);
        }
        if analyze_statement(stmt, name).satisfied() {
            return None;
        }
    }
    None
}

fn read_in_statement<'a>(stmt: &'a StatementNode, name: &str) -> Option<&'a ExpressionNode> {
    match &stmt.kind {
        StatementKind::Expression(expr) => read_in_expression_skipping_assignment(expr, name),
        StatementKind::Return(Some(expr))
        | StatementKind::AutoDecl(_, _, expr)
        | StatementKind::TypedDecl(_, _, expr)
        | StatementKind::ConstDecl(_, _, expr) => read_in_expression(expr, name),
        StatementKind::Block(inner) => first_read_in_block(inner, name),
        StatementKind::If {
            cond,
            then_block,
            else_block,
        } => read_in_expression(cond, name)
            .or_else(|| first_read_in_block(then_block, name))
            .or_else(|| {
                else_block
                    .as_ref()
                    .and_then(|b| first_read_in_block(b, name))
            }),
        StatementKind::While { cond, body } => {
            read_in_expression(cond, name).or_else(|| first_read_in_block(body, name))
        }
        StatementKind::For { iter, body, .. } => {
            read_in_expression(iter, name).or_else(|| first_read_in_block(body, name))
        }
        StatementKind::Match { expr, arms } => read_in_expression(expr, name).or_else(|| {
            arms.iter()
                .find_map(|arm| first_read_in_block(&arm.body, name))
        }),
        _ => None,
    }
}

fn first_read_in_block<'a>(stmts: &'a [StatementNode], name: &str) -> Option<&'a ExpressionNode> {
    stmts.iter().find_map(|s| read_in_statement(s, name))
}

/// A read anywhere in `expr`, ignoring the target of a direct assignment to
/// `name` - `x = f()` writes `x`, it does not read it.
fn read_in_expression_skipping_assignment<'a>(
    expr: &'a ExpressionNode,
    name: &str,
) -> Option<&'a ExpressionNode> {
    if let ExpressionKind::Binary {
        left, op, right, ..
    } = &expr.kind
        && matches!(op, crate::ast::BinaryOp::Assign)
        && matches!(&left.kind, ExpressionKind::Identifier(n) if n == name)
    {
        return read_in_expression(right, name);
    }
    read_in_expression(expr, name)
}

fn read_in_expression<'a>(expr: &'a ExpressionNode, name: &str) -> Option<&'a ExpressionNode> {
    match &expr.kind {
        ExpressionKind::Identifier(n) if n == name => Some(expr),
        ExpressionKind::Binary { left, right, .. } => {
            read_in_expression(left, name).or_else(|| read_in_expression(right, name))
        }
        ExpressionKind::Unary { expr: inner, .. }
        | ExpressionKind::FieldAccess { expr: inner, .. }
        | ExpressionKind::Slice { expr: inner, .. } => read_in_expression(inner, name),
        ExpressionKind::Call { func, args } => read_in_expression(func, name)
            .or_else(|| args.iter().find_map(|a| read_in_expression(a, name))),
        ExpressionKind::ListAccess { expr: inner, index } => {
            read_in_expression(inner, name).or_else(|| read_in_expression(index, name))
        }
        ExpressionKind::ListLiteral(items)
        | ExpressionKind::SetLiteral(items)
        | ExpressionKind::TupleLiteral(items) => {
            items.iter().find_map(|i| read_in_expression(i, name))
        }
        ExpressionKind::If {
            cond,
            then_expr,
            else_expr,
        } => read_in_expression(cond, name)
            .or_else(|| read_in_expression(then_expr, name))
            .or_else(|| read_in_expression(else_expr, name)),
        _ => None,
    }
}
