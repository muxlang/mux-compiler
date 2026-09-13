//! Facts about Result/Optional variants on paths that can reach an expression.
//!
//! Saved predicates refer to a particular binding revision. A write invalidates
//! that revision, so an old boolean cannot authorize extraction from a new value.

use std::collections::{HashMap, HashSet};

use super::{SemanticAnalyzer, SemanticError, Type};
use crate::ast::{
    BinaryOp, ExpressionKind, ExpressionNode, LiteralNode, PatternNode, StatementKind,
    StatementNode, UnaryOp,
};
use crate::diagnostic::DiagnosticCode;
use crate::lexer::Span;

type Binding = (String, Span);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Variant {
    Present,
    Absent,
}

impl Variant {
    fn opposite(self) -> Self {
        match self {
            Self::Present => Self::Absent,
            Self::Absent => Self::Present,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Guard {
    when_true: HashMap<Binding, (u64, Variant)>,
    when_false: HashMap<Binding, (u64, Variant)>,
}

fn common<K: Eq + std::hash::Hash + Clone, V: Eq + Clone>(
    left: &HashMap<K, V>,
    right: &HashMap<K, V>,
) -> HashMap<K, V> {
    left.iter()
        .filter(|(key, value)| right.get(*key) == Some(*value))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

impl Guard {
    fn negated(mut self) -> Self {
        std::mem::swap(&mut self.when_true, &mut self.when_false);
        self
    }

    fn and(self, other: Self) -> Self {
        let mut when_true = self.when_true.clone();
        when_true.extend(other.when_true);
        let mut right_false = self.when_true;
        right_false.extend(other.when_false);
        Self {
            when_true,
            when_false: common(&self.when_false, &right_false),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct FlowState {
    facts: HashMap<Binding, Variant>,
    revisions: HashMap<Binding, u64>,
    booleans: HashMap<Binding, Guard>,
    escaped: HashSet<Binding>,
    pub(super) reachable: bool,
}

impl Default for FlowState {
    fn default() -> Self {
        Self {
            facts: HashMap::new(),
            revisions: HashMap::new(),
            booleans: HashMap::new(),
            escaped: HashSet::new(),
            reachable: true,
        }
    }
}

impl FlowState {
    pub(crate) fn unreachable() -> Self {
        Self {
            reachable: false,
            ..Self::default()
        }
    }

    pub(super) fn join(left: Self, right: Self) -> Self {
        if !left.reachable {
            return right;
        }
        if !right.reachable {
            return left;
        }
        let facts = common(&left.facts, &right.facts);
        let booleans = common(&left.booleans, &right.booleans);
        let mut revisions = left.revisions;
        for (binding, revision) in right.revisions {
            revisions
                .entry(binding)
                .and_modify(|value| *value = (*value).max(revision))
                .or_insert(revision);
        }
        let mut escaped = left.escaped;
        escaped.extend(right.escaped);
        Self {
            facts,
            revisions,
            booleans,
            escaped,
            reachable: true,
        }
    }
}

impl SemanticAnalyzer {
    fn flow_binding(&self, expression: &ExpressionNode) -> Option<Binding> {
        let ExpressionKind::Identifier(name) = &expression.kind else {
            return None;
        };
        self.symbol_table
            .get_cloned(name)
            .map(|symbol| (name.clone(), symbol.span))
    }

    fn flow_name(&self, name: &str) -> Option<Binding> {
        self.symbol_table
            .get_cloned(name)
            .map(|symbol| (name.to_string(), symbol.span))
    }

    fn flow_revision(&self, binding: &Binding) -> u64 {
        self.flow.revisions.get(binding).copied().unwrap_or(0)
    }

    fn invalidate_binding(&mut self, binding: Binding) {
        self.flow_generation += 1;
        self.flow
            .revisions
            .insert(binding.clone(), self.flow_generation);
        self.flow.facts.remove(&binding);
        self.flow.booleans.remove(&binding);
    }

    pub(super) fn invalidate_flow_target(&mut self, target: &ExpressionNode) {
        if let Some(binding) = self.flow_binding(target) {
            self.invalidate_binding(binding);
        } else {
            // A dereference or field write may modify a wrapper reachable by a
            // reference. No unproven alias relationship preserves a fact.
            self.invalidate_escaped_flow();
        }
    }

    pub(super) fn escape_flow_target(&mut self, target: &ExpressionNode) {
        if let Some(binding) = self.flow_binding(target) {
            self.flow.escaped.insert(binding.clone());
            self.invalidate_binding(binding);
        }
    }

    fn invalidate_escaped_flow(&mut self) {
        let mut affected = self.flow.escaped.clone();
        for (name, symbol) in self.symbol_table.global_scope_symbols() {
            affected.insert((name, symbol.span));
        }
        for binding in self.flow.facts.keys().chain(self.flow.booleans.keys()) {
            if self
                .symbol_table
                .get_cloned(&binding.0)
                .is_some_and(|symbol| matches!(symbol.type_, Some(Type::Reference(_))))
            {
                affected.insert(binding.clone());
            }
        }
        for binding in affected {
            self.invalidate_binding(binding);
        }
    }

    fn wrapper_type(&mut self, expression: &ExpressionNode) -> Option<Type> {
        let mut type_ = self.get_expression_type(expression).ok()?;
        while let Type::Reference(inner) = type_ {
            type_ = *inner;
        }
        matches!(type_, Type::Optional(_) | Type::Result(_, _)).then_some(type_)
    }

    fn inspection_receiver<'e>(
        &mut self,
        expression: &'e ExpressionNode,
    ) -> Option<(&'e ExpressionNode, &'e str)> {
        let ExpressionKind::Call { func, args } = &expression.kind else {
            return None;
        };
        let ExpressionKind::FieldAccess {
            expr: receiver,
            field,
        } = &func.kind
        else {
            return None;
        };
        if !args.is_empty() || self.wrapper_type(receiver).is_none() {
            return None;
        }
        Some((receiver, field.as_str()))
    }

    pub(super) fn finish_flow_call(
        &mut self,
        expression: &ExpressionNode,
    ) -> Result<(), SemanticError> {
        if let Some((receiver, method)) = self.inspection_receiver(expression) {
            match method {
                "value" | "error" => {
                    let expected = if method == "value" {
                        Variant::Present
                    } else {
                        Variant::Absent
                    };
                    if self.flow.reachable && self.known_variant(receiver) != Some(expected) {
                        return Err(SemanticError::with_help(
                            DiagnosticCode::InvalidOperation,
                            format!(
                                "Cannot call {method}(): the wrapper variant is not established on this path"
                            ),
                            expression.span,
                            "Check is_ok()/is_err() or is_some()/is_none() first; the opposite branch must exit before extraction afterward. Use `use` to propagate failure.",
                        ));
                    }
                    return Ok(());
                }
                "is_ok" | "is_err" | "is_some" | "is_none" | "to_string" => return Ok(()),
                _ => {}
            }
        }
        if let ExpressionKind::Call { func, .. } = &expression.kind
            && let ExpressionKind::Identifier(name) = &func.kind
            && matches!(name.as_str(), "ok" | "err" | "some")
        {
            return Ok(());
        }
        self.invalidate_escaped_flow();
        Ok(())
    }

    fn known_variant(&self, expression: &ExpressionNode) -> Option<Variant> {
        match &expression.kind {
            ExpressionKind::None => Some(Variant::Absent),
            ExpressionKind::Call { func, .. } => match &func.kind {
                ExpressionKind::Identifier(name) if name == "ok" || name == "some" => {
                    Some(Variant::Present)
                }
                ExpressionKind::Identifier(name) if name == "err" => Some(Variant::Absent),
                _ => None,
            },
            ExpressionKind::Identifier(_) => self
                .flow_binding(expression)
                .and_then(|binding| self.flow.facts.get(&binding).copied()),
            _ => None,
        }
    }

    pub(super) fn record_flow_binding(&mut self, name: &str, expression: &ExpressionNode) {
        let variant = self.known_variant(expression);
        let guard = self.expression_guards.get(&expression.span).cloned();
        if let Some(binding) = self.flow_name(name) {
            self.invalidate_binding(binding.clone());
            if let Some(variant) = variant {
                self.flow.facts.insert(binding.clone(), variant);
            }
            if let Some(guard) = guard {
                self.flow.booleans.insert(binding, guard);
            }
        }
    }

    pub(super) fn record_flow_assignment(
        &mut self,
        target: &ExpressionNode,
        value: &ExpressionNode,
    ) {
        if let ExpressionKind::Identifier(name) = &target.kind {
            self.record_flow_binding(name, value);
        } else {
            self.invalidate_flow_target(target);
        }
    }

    fn cached_guard(&self, expression: &ExpressionNode) -> Guard {
        self.expression_guards
            .get(&expression.span)
            .cloned()
            .unwrap_or_default()
    }

    pub(super) fn record_expression_guard(&mut self, expression: &ExpressionNode) {
        let guard = match &expression.kind {
            ExpressionKind::Identifier(_) => self
                .flow_binding(expression)
                .and_then(|binding| self.flow.booleans.get(&binding).cloned())
                .unwrap_or_default(),
            ExpressionKind::Unary {
                op: UnaryOp::Not,
                expr,
                ..
            } => self.cached_guard(expr).negated(),
            ExpressionKind::Binary {
                left,
                op: BinaryOp::LogicalAnd,
                right,
                ..
            } => self.cached_guard(left).and(self.cached_guard(right)),
            ExpressionKind::Binary {
                left,
                op: BinaryOp::LogicalOr,
                right,
                ..
            } => self
                .cached_guard(left)
                .negated()
                .and(self.cached_guard(right).negated())
                .negated(),
            ExpressionKind::Binary {
                left,
                op: BinaryOp::Equal | BinaryOp::NotEqual,
                right,
                ..
            } => {
                let (value, literal) =
                    if matches!(left.kind, ExpressionKind::Literal(LiteralNode::Boolean(_))) {
                        (right, left)
                    } else {
                        (left, right)
                    };
                if let ExpressionKind::Literal(LiteralNode::Boolean(boolean)) = literal.kind {
                    let guard = self.cached_guard(value);
                    let equal = matches!(
                        expression.kind,
                        ExpressionKind::Binary {
                            op: BinaryOp::Equal,
                            ..
                        }
                    );
                    if boolean == equal {
                        guard
                    } else {
                        guard.negated()
                    }
                } else {
                    Guard::default()
                }
            }
            _ => self.predicate_guard(expression).unwrap_or_default(),
        };
        self.expression_guards.insert(expression.span, guard);
    }

    fn predicate_guard(&mut self, expression: &ExpressionNode) -> Option<Guard> {
        let (receiver, method) = self.inspection_receiver(expression)?;
        let variant = match method {
            "is_ok" | "is_some" => Variant::Present,
            "is_err" | "is_none" => Variant::Absent,
            _ => return None,
        };
        let binding = self.flow_binding(receiver)?;
        let revision = self.flow_revision(&binding);
        Some(Guard {
            when_true: HashMap::from([(binding.clone(), (revision, variant))]),
            when_false: HashMap::from([(binding, (revision, variant.opposite()))]),
        })
    }

    pub(super) fn assume_flow_condition(&mut self, condition: &ExpressionNode, truth: bool) {
        let guard = self.cached_guard(condition);
        let facts = if truth {
            guard.when_true
        } else {
            guard.when_false
        };
        for (binding, (revision, variant)) in facts {
            if self.flow_revision(&binding) != revision {
                continue;
            }
            if self
                .flow
                .facts
                .get(&binding)
                .is_some_and(|known| *known != variant)
            {
                self.flow.reachable = false;
            }
            self.flow.facts.insert(binding, variant);
        }
    }

    pub(super) fn assume_flow_pattern(
        &mut self,
        expression: &ExpressionNode,
        pattern: &PatternNode,
    ) {
        let variant = match pattern {
            PatternNode::EnumVariant { name, .. } if name == "some" || name == "ok" => {
                Some(Variant::Present)
            }
            PatternNode::EnumVariant { name, .. } if name == "none" || name == "err" => {
                Some(Variant::Absent)
            }
            _ => None,
        };
        if let (Some(binding), Some(variant)) = (self.flow_binding(expression), variant) {
            self.flow.facts.insert(binding, variant);
        }
    }

    /// Kill facts before entering a loop if any iteration can replace their
    /// values. This prevents the first iteration's facts proving later reads.
    pub(super) fn invalidate_loop_writes(&mut self, statements: &[StatementNode]) {
        for statement in statements {
            match &statement.kind {
                StatementKind::Expression(expr)
                | StatementKind::AutoDecl(_, _, expr)
                | StatementKind::TypedDecl(_, _, expr)
                | StatementKind::ConstDecl(_, _, expr)
                | StatementKind::Return(Some(expr)) => self.invalidate_expression_writes(expr),
                StatementKind::Block(body) | StatementKind::For { body, .. } => {
                    self.invalidate_loop_writes(body)
                }
                StatementKind::While { cond, body } => {
                    self.invalidate_expression_writes(cond);
                    self.invalidate_loop_writes(body);
                }
                StatementKind::If {
                    cond,
                    then_block,
                    else_block,
                } => {
                    self.invalidate_expression_writes(cond);
                    self.invalidate_loop_writes(then_block);
                    if let Some(body) = else_block {
                        self.invalidate_loop_writes(body);
                    }
                }
                StatementKind::Match { expr, arms } => {
                    self.invalidate_expression_writes(expr);
                    for arm in arms {
                        if let Some(guard) = &arm.guard {
                            self.invalidate_expression_writes(guard);
                        }
                        self.invalidate_loop_writes(&arm.body);
                    }
                }
                _ => {}
            }
        }
    }

    pub(super) fn invalidate_expression_writes(&mut self, expression: &ExpressionNode) {
        match &expression.kind {
            ExpressionKind::Binary {
                left, op, right, ..
            } => {
                self.invalidate_expression_writes(left);
                self.invalidate_expression_writes(right);
                if op.is_assignment() {
                    self.invalidate_flow_target(left);
                }
            }
            ExpressionKind::Unary { expr, op, .. } => {
                self.invalidate_expression_writes(expr);
                match op {
                    UnaryOp::Ref => self.escape_flow_target(expr),
                    UnaryOp::Incr | UnaryOp::Decr => self.invalidate_flow_target(expr),
                    _ => {}
                }
            }
            ExpressionKind::Call { func, args } => {
                self.invalidate_expression_writes(func);
                for argument in args {
                    self.invalidate_expression_writes(argument);
                }
                self.invalidate_escaped_flow();
            }
            ExpressionKind::FieldAccess { expr, .. } => self.invalidate_expression_writes(expr),
            ExpressionKind::ListAccess { expr, index } => {
                self.invalidate_expression_writes(expr);
                self.invalidate_expression_writes(index);
            }
            ExpressionKind::Slice { expr, start, end } => {
                self.invalidate_expression_writes(expr);
                for value in [start, end].into_iter().flatten() {
                    self.invalidate_expression_writes(value);
                }
            }
            ExpressionKind::ListLiteral(elements)
            | ExpressionKind::SetLiteral(elements)
            | ExpressionKind::TupleLiteral(elements) => {
                for value in elements {
                    self.invalidate_expression_writes(value);
                }
            }
            ExpressionKind::MapLiteral { entries, .. } => {
                for (key, value) in entries {
                    self.invalidate_expression_writes(key);
                    self.invalidate_expression_writes(value);
                }
            }
            ExpressionKind::If {
                cond,
                then_expr,
                else_expr,
            } => {
                self.invalidate_expression_writes(cond);
                self.invalidate_expression_writes(then_expr);
                self.invalidate_expression_writes(else_expr);
            }
            ExpressionKind::Match { expr, arms } => {
                self.invalidate_expression_writes(expr);
                for arm in arms {
                    self.invalidate_loop_writes(&arm.body);
                }
            }
            _ => {}
        }
    }
}
