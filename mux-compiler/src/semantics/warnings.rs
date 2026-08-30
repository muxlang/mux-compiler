//! High-confidence warnings derived from the recovered, type-checked AST.
//!
//! Warning collection is deliberately separate from error-producing semantic
//! checks. It runs only after semantic analysis has found no errors, so a
//! malformed program cannot produce a misleading warning cascade.

use super::const_fold::{ConstValue, fold};
use super::error::SemanticError;
use crate::ast::{
    AstNode, BinaryOp, ExpressionKind, ExpressionNode, FunctionNode, MatchArm, PatternNode,
    StatementKind, StatementNode,
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
    warnings.extend(collect_binding_warnings(nodes));
    warnings
}

#[derive(Debug)]
struct Binding {
    name: String,
    span: crate::lexer::Span,
    reads: usize,
    last_assignment: Option<crate::lexer::Span>,
    read_since_assignment: bool,
}

/// A deliberately small lexical-use pass. Semantic analysis has already
/// proven the program valid when this runs, so this pass can focus on facts
/// that are syntax-local and cannot be invalidated by error recovery.
struct BindingWarnings {
    bindings: Vec<Binding>,
    scopes: Vec<Vec<usize>>,
    warnings: Vec<SemanticError>,
}

impl BindingWarnings {
    fn new() -> Self {
        Self {
            bindings: Vec::new(),
            scopes: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn push_scope(&mut self) {
        self.scopes.push(Vec::new());
    }

    fn pop_scope(&mut self) {
        let Some(indices) = self.scopes.pop() else {
            return;
        };
        for index in indices {
            let binding = &self.bindings[index];
            if binding.reads == 0 && binding.name != "_" {
                self.warnings.push(SemanticError::new(
                    DiagnosticCode::UnusedBinding,
                    format!("binding '{}' is never read", binding.name),
                    binding.span,
                ));
            }
        }
    }

    fn declare(&mut self, name: &str, span: crate::lexer::Span) {
        if name == "_" {
            return;
        }
        let Some(scope_index) = self.scopes.len().checked_sub(1) else {
            return;
        };
        let shadowed = self
            .scopes
            .iter()
            .rev()
            .skip(1)
            .flatten()
            .any(|index| self.bindings[*index].name == name);
        if shadowed {
            self.warnings.push(SemanticError::new(
                DiagnosticCode::ShadowedBinding,
                format!("binding '{name}' shadows an outer binding"),
                span,
            ));
        }
        let index = self.bindings.len();
        self.bindings.push(Binding {
            name: name.to_string(),
            span,
            reads: 0,
            last_assignment: None,
            read_since_assignment: false,
        });
        self.scopes[scope_index].push(index);
    }

    fn find(&self, name: &str) -> Option<usize> {
        self.scopes
            .iter()
            .rev()
            .flatten()
            .find_map(|index| (self.bindings[*index].name == name).then_some(*index))
    }

    fn read(&mut self, name: &str) {
        let Some(index) = self.find(name) else {
            return;
        };
        let binding = &mut self.bindings[index];
        binding.reads += 1;
        binding.read_since_assignment = true;
    }

    fn write(&mut self, name: &str, span: crate::lexer::Span) {
        let Some(index) = self.find(name) else {
            return;
        };
        let binding = &mut self.bindings[index];
        if let Some(previous) = binding.last_assignment
            && !binding.read_since_assignment
        {
            self.warnings.push(SemanticError::new(
                DiagnosticCode::DeadAssignment,
                format!("assignment to '{name}' is overwritten before it is read"),
                previous,
            ));
        }
        binding.last_assignment = Some(span);
        binding.read_since_assignment = false;
    }

    /// A write observed inside a branch or loop is not known to execute, so it
    /// must not participate in a later straight-line dead-assignment claim.
    /// Reads remain recorded because they are safe evidence that a binding is
    /// used, regardless of which path executed.
    fn clear_assignment_tracking(&mut self) {
        for binding in &mut self.bindings {
            binding.last_assignment = None;
            binding.read_since_assignment = false;
        }
    }
}

fn collect_binding_warnings(nodes: &[AstNode]) -> Vec<SemanticError> {
    let mut analysis = BindingWarnings::new();
    for node in nodes {
        match node {
            AstNode::Function(function) => collect_function_bindings(function, &mut analysis),
            AstNode::Class { methods, .. } => {
                for method in methods {
                    collect_function_bindings(method, &mut analysis);
                }
            }
            AstNode::Statement(statement) => {
                if let StatementKind::Function(function) = &statement.kind {
                    collect_function_bindings(function, &mut analysis);
                }
            }
            AstNode::Interface { .. } => {}
            AstNode::Enum { .. } => {}
        }
    }
    analysis.warnings
}

fn collect_function_bindings(function: &FunctionNode, analysis: &mut BindingWarnings) {
    analysis.push_scope();
    for parameter in &function.params {
        analysis.declare(&parameter.name, parameter.type_.span);
        if let Some(default) = &parameter.default_value {
            collect_binding_expression(default, analysis);
        }
    }
    collect_binding_block(&function.body, analysis);
    analysis.pop_scope();
}

fn collect_binding_block(statements: &[StatementNode], analysis: &mut BindingWarnings) {
    for statement in statements {
        collect_binding_statement(statement, analysis);
    }
}

fn collect_binding_statement(statement: &StatementNode, analysis: &mut BindingWarnings) {
    match &statement.kind {
        StatementKind::AutoDecl(name, _, expression)
        | StatementKind::TypedDecl(name, _, expression)
        | StatementKind::ConstDecl(name, _, expression) => {
            collect_binding_expression(expression, analysis);
            analysis.declare(name, statement.span);
        }
        StatementKind::UninitDecl(name, _) => analysis.declare(name, statement.span),
        StatementKind::For {
            var, iter, body, ..
        } => {
            collect_binding_expression(iter, analysis);
            analysis.clear_assignment_tracking();
            analysis.push_scope();
            analysis.declare(var, statement.span);
            collect_binding_block(body, analysis);
            analysis.pop_scope();
            analysis.clear_assignment_tracking();
        }
        StatementKind::If {
            cond,
            then_block,
            else_block,
        } => {
            collect_binding_expression(cond, analysis);
            analysis.clear_assignment_tracking();
            analysis.push_scope();
            collect_binding_block(then_block, analysis);
            analysis.pop_scope();
            if let Some(else_block) = else_block {
                analysis.push_scope();
                collect_binding_block(else_block, analysis);
                analysis.pop_scope();
            }
            analysis.clear_assignment_tracking();
        }
        StatementKind::While { cond, body } => {
            collect_binding_expression(cond, analysis);
            analysis.clear_assignment_tracking();
            analysis.push_scope();
            collect_binding_block(body, analysis);
            analysis.pop_scope();
            analysis.clear_assignment_tracking();
        }
        StatementKind::Match { expr, arms } => {
            collect_binding_expression(expr, analysis);
            analysis.clear_assignment_tracking();
            for arm in arms {
                collect_binding_arm(arm, analysis);
                // Arms are mutually exclusive. A store in one arm cannot make
                // a later store in another arm a straight-line dead write.
                analysis.clear_assignment_tracking();
            }
            analysis.clear_assignment_tracking();
        }
        StatementKind::Return(Some(expression)) | StatementKind::Expression(expression) => {
            collect_binding_expression(expression, analysis);
        }
        StatementKind::Block(statements) => {
            analysis.clear_assignment_tracking();
            analysis.push_scope();
            collect_binding_block(statements, analysis);
            analysis.pop_scope();
            analysis.clear_assignment_tracking();
        }
        StatementKind::Function(function) => {
            analysis.declare(&function.name, statement.span);
            collect_function_bindings(function, analysis);
        }
        StatementKind::Import { .. }
        | StatementKind::Return(None)
        | StatementKind::Break
        | StatementKind::Continue => {}
    }
}

fn collect_binding_arm(arm: &MatchArm, analysis: &mut BindingWarnings) {
    analysis.push_scope();
    collect_binding_pattern(&arm.pattern, analysis);
    if let Some(guard) = &arm.guard {
        collect_binding_expression(guard, analysis);
    }
    collect_binding_block(&arm.body, analysis);
    analysis.pop_scope();
}

fn collect_binding_pattern(pattern: &PatternNode, analysis: &mut BindingWarnings) {
    // Pattern identifiers are ambiguous in the recovered AST: `LIMIT` can be
    // a constant pattern while `value` can bind a payload. The semantic
    // pattern checker resolves that distinction, but it does not expose the
    // binding spans to this lint pass. Do not guess here; guessing would turn
    // valid constant patterns into bogus unused/shadowing warnings.
    let _ = (pattern, analysis);
}

fn collect_binding_expression(expression: &ExpressionNode, analysis: &mut BindingWarnings) {
    match &expression.kind {
        ExpressionKind::Identifier(name) => analysis.read(name),
        ExpressionKind::Binary {
            left, op, right, ..
        } if op.is_assignment() => collect_binding_assignment(left, op, right, analysis),
        ExpressionKind::Binary { left, right, .. } => {
            collect_binding_expression(left, analysis);
            collect_binding_expression(right, analysis);
        }
        ExpressionKind::Unary { expr, .. } => collect_binding_expression(expr, analysis),
        ExpressionKind::Call { func, args } => collect_binding_call(func, args, analysis),
        ExpressionKind::FieldAccess { expr, .. } => collect_binding_expression(expr, analysis),
        ExpressionKind::ListAccess { expr, index } => {
            collect_binding_expression(expr, analysis);
            collect_binding_expression(index, analysis);
        }
        ExpressionKind::Slice { expr, start, end } => {
            collect_binding_slice(expr, start.as_deref(), end.as_deref(), analysis);
        }
        ExpressionKind::ListLiteral(elements)
        | ExpressionKind::SetLiteral(elements)
        | ExpressionKind::TupleLiteral(elements) => collect_binding_elements(elements, analysis),
        ExpressionKind::MapLiteral { entries, .. } => collect_binding_map(entries, analysis),
        ExpressionKind::If {
            cond,
            then_expr,
            else_expr,
        } => {
            collect_binding_expression(cond, analysis);
            collect_binding_expression(then_expr, analysis);
            collect_binding_expression(else_expr, analysis);
        }
        ExpressionKind::Lambda { params, body, .. } => {
            collect_binding_lambda(params, body, analysis);
        }
        ExpressionKind::Literal(_) | ExpressionKind::None | ExpressionKind::GenericType(_, _) => {}
    }
}

fn collect_binding_assignment(
    left: &ExpressionNode,
    op: &BinaryOp,
    right: &ExpressionNode,
    analysis: &mut BindingWarnings,
) {
    let ExpressionKind::Identifier(name) = &left.kind else {
        collect_binding_expression(left, analysis);
        collect_binding_expression(right, analysis);
        return;
    };
    if !matches!(op, BinaryOp::Assign) {
        analysis.read(name);
    }
    collect_binding_expression(right, analysis);
    analysis.write(name, left.span);
}

fn collect_binding_call(
    func: &ExpressionNode,
    args: &[ExpressionNode],
    analysis: &mut BindingWarnings,
) {
    collect_binding_expression(func, analysis);
    for arg in args {
        collect_binding_expression(arg, analysis);
    }
}

fn collect_binding_slice(
    expr: &ExpressionNode,
    start: Option<&ExpressionNode>,
    end: Option<&ExpressionNode>,
    analysis: &mut BindingWarnings,
) {
    collect_binding_expression(expr, analysis);
    if let Some(start) = start {
        collect_binding_expression(start, analysis);
    }
    if let Some(end) = end {
        collect_binding_expression(end, analysis);
    }
}

fn collect_binding_elements(elements: &[ExpressionNode], analysis: &mut BindingWarnings) {
    for element in elements {
        collect_binding_expression(element, analysis);
    }
}

fn collect_binding_map(
    entries: &[(ExpressionNode, ExpressionNode)],
    analysis: &mut BindingWarnings,
) {
    for (key, value) in entries {
        collect_binding_expression(key, analysis);
        collect_binding_expression(value, analysis);
    }
}

fn collect_binding_lambda(
    params: &[crate::ast::Param],
    body: &[StatementNode],
    analysis: &mut BindingWarnings,
) {
    // The lambda may execute later, so writes inside it cannot prove anything
    // about the enclosing function's straight-line store order. Reads of
    // captured names are still real uses, however.
    analysis.clear_assignment_tracking();
    analysis.push_scope();
    for parameter in params {
        analysis.declare(&parameter.name, parameter.type_.span);
    }
    collect_binding_block(body, analysis);
    analysis.pop_scope();
    analysis.clear_assignment_tracking();
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
        reachable &= statement_can_fall_through(statement);
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
        } => collect_if_statement(cond, then_block, else_block.as_deref(), warnings),
        StatementKind::While { cond, body } => collect_while_statement(cond, body, warnings),
        StatementKind::For { iter, body, .. } => {
            collect_expression(iter, warnings);
            collect_block(body, warnings);
        }
        StatementKind::Match { expr, arms } => collect_match_statement(expr, arms, warnings),
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

fn collect_if_statement(
    cond: &ExpressionNode,
    then_block: &[StatementNode],
    else_block: Option<&[StatementNode]>,
    warnings: &mut Vec<SemanticError>,
) {
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

fn collect_while_statement(
    cond: &ExpressionNode,
    body: &[StatementNode],
    warnings: &mut Vec<SemanticError>,
) {
    warn_constant_condition(cond, warnings);
    collect_expression(cond, warnings);
    if matches!(fold(cond), Some(ConstValue::Bool(false))) {
        collect_unreachable_block(body, warnings);
    } else {
        collect_block(body, warnings);
    }
}

fn collect_match_statement(
    expr: &ExpressionNode,
    arms: &[crate::ast::MatchArm],
    warnings: &mut Vec<SemanticError>,
) {
    collect_expression(expr, warnings);
    for arm in arms {
        if let Some(guard) = &arm.guard {
            collect_expression(guard, warnings);
        }
        collect_block(&arm.body, warnings);
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
        ExpressionKind::FieldAccess { expr, .. } => collect_expression(expr, warnings),
        ExpressionKind::ListAccess { expr, index } => {
            collect_expression(expr, warnings);
            collect_expression(index, warnings);
        }
        ExpressionKind::Slice { expr, start, end } => {
            collect_expression(expr, warnings);
            if let Some(start) = start {
                collect_expression(start, warnings);
            }
            if let Some(end) = end {
                collect_expression(end, warnings);
            }
        }
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
        reachable &= statement_can_fall_through(statement);
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
