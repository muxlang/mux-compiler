//! Semantic analysis for `where { ... }` constraint clauses.
//!
//! Typechecks every predicate as `bool` in the scope of the construct it is
//! attached to (function/method/lambda params, class fields, enum variant
//! payload fields, interface method params) and records the metadata codegen
//! needs: per-class invariants (field-level and class-level predicates, with
//! the fields each references) and interface method preconditions that every
//! implementing class must enforce.

use super::{SemanticAnalyzer, SemanticError, SymbolKind, Type, format_type};
use crate::ast::{
    AstNode, EnumVariant, ExpressionKind, ExpressionNode, Field, FunctionNode, PrimitiveType,
    TraitBound, TraitRef, WhereClause,
};
use std::collections::{HashMap, HashSet};

/// A class constraint from a field-level or class-level `where`, ready for
/// codegen: the predicate plus the class fields it references (used to decide
/// which invariants to re-check when a given field is assigned).
#[derive(Debug, Clone)]
pub struct ClassInvariant {
    pub predicate: ExpressionNode,
    pub referenced_fields: Vec<String>,
}

/// An interface method's `where` predicates, expressed against the interface
/// method's own parameter names. Implementing classes bind these names to
/// their method's parameters positionally at codegen.
#[derive(Debug, Clone)]
pub struct InheritedPrecondition {
    pub interface_param_names: Vec<String>,
    pub predicates: Vec<ExpressionNode>,
}

impl SemanticAnalyzer {
    /// Typecheck every predicate of a where clause as `bool` in the current
    /// scope, accumulating errors instead of aborting on the first.
    pub(super) fn analyze_where_clause(&mut self, clause: &WhereClause) {
        for predicate in &clause.predicates {
            if let Err(e) = self.analyze_expression(predicate) {
                self.errors.push(e);
                continue;
            }
            match self.get_expression_type(predicate) {
                Ok(Type::Primitive(PrimitiveType::Bool)) => {}
                Ok(other) => self.errors.push(SemanticError::with_help(
                    format!(
                        "where predicate must be a bool, got {}",
                        format_type(&other)
                    ),
                    predicate.span,
                    "Each predicate in a where block must evaluate to a bool. Example: where { value > 0 }",
                )),
                Err(e) => self.errors.push(e),
            }
        }
    }

    /// Collect the `where` predicates declared on interface methods, keyed by
    /// interface then method name. Runs before the analysis pass so classes
    /// can inherit preconditions from interfaces in any module.
    pub(super) fn collect_interface_preconditions(&mut self, ast: &[AstNode]) {
        let mut map = HashMap::new();
        for nodes in self.all_module_asts.values() {
            Self::extract_interface_preconditions(nodes, &mut map);
        }
        Self::extract_interface_preconditions(ast, &mut map);
        self.interface_preconditions = map;
    }

    fn extract_interface_preconditions(
        nodes: &[AstNode],
        map: &mut HashMap<String, HashMap<String, InheritedPrecondition>>,
    ) {
        for node in nodes {
            let AstNode::Interface { name, methods, .. } = node else {
                continue;
            };
            for method in methods {
                let Some(clause) = &method.where_clause else {
                    continue;
                };
                map.entry(name.clone()).or_default().insert(
                    method.name.clone(),
                    InheritedPrecondition {
                        interface_param_names: method
                            .params
                            .iter()
                            .map(|p| p.name.clone())
                            .collect(),
                        predicates: clause.predicates.clone(),
                    },
                );
            }
        }
    }

    /// Typecheck the `where` clauses on an interface's method signatures with
    /// each method's parameters in scope. Implementing classes rely on this
    /// single check; signature compatibility guarantees the predicates remain
    /// well-typed against the implementing method's parameters.
    pub(super) fn analyze_interface_where_clauses(
        &mut self,
        type_params: &[(String, Vec<TraitBound>)],
        methods: &[FunctionNode],
    ) -> Result<(), SemanticError> {
        for method in methods {
            let Some(clause) = &method.where_clause else {
                continue;
            };
            self.symbol_table.push_scope()?;
            for (param_name, _) in type_params.iter().chain(method.type_params.iter()) {
                self.symbol_table.add_symbol(
                    param_name,
                    Self::make_symbol(
                        SymbolKind::Type,
                        method.span,
                        Some(Type::Generic(param_name.clone())),
                    ),
                )?;
            }
            for param in &method.params {
                let param_type = self.resolve_type(&param.type_)?;
                self.symbol_table.add_symbol(
                    &param.name,
                    Self::make_symbol(SymbolKind::Variable, param.type_.span, Some(param_type)),
                )?;
            }
            self.analyze_where_clause(clause);
            self.symbol_table.pop_scope()?;
        }
        Ok(())
    }

    /// Typecheck the `where` clauses on an enum's variants with each
    /// variant's named payload fields in scope.
    pub(super) fn analyze_enum_where_clauses(
        &mut self,
        variants: &[EnumVariant],
    ) -> Result<(), SemanticError> {
        for variant in variants {
            let Some(clause) = &variant.where_clause else {
                continue;
            };
            self.symbol_table.push_scope()?;
            for (field_name, field_type) in variant.data.iter().flatten() {
                let Some(field_name) = field_name else {
                    continue;
                };
                let field_type = self.resolve_type(field_type)?;
                self.symbol_table.add_symbol(
                    field_name,
                    Self::make_symbol(SymbolKind::Variable, clause.span, Some(field_type)),
                )?;
            }
            self.analyze_where_clause(clause);
            self.symbol_table.pop_scope()?;
        }
        Ok(())
    }

    /// Typecheck a class's field-level and class-level `where` clauses with
    /// every field in scope as a bare name, record the resulting invariants,
    /// and record the interface preconditions its methods inherit.
    pub(super) fn analyze_class_where_clauses(
        &mut self,
        name: &str,
        fields: &[Field],
        methods: &[FunctionNode],
        traits: &[TraitRef],
        class_where: Option<&WhereClause>,
    ) -> Result<(), SemanticError> {
        let field_names: HashSet<String> = fields.iter().map(|f| f.name.clone()).collect();

        self.symbol_table.push_scope()?;
        for field in fields {
            let field_type = self.resolve_type(&field.type_)?;
            // A duplicate field name is already reported by the hoisting pass;
            // do not report it a second time from this scope.
            let _ = self.symbol_table.add_symbol(
                &field.name,
                Self::make_symbol(SymbolKind::Variable, field.type_.span, Some(field_type)),
            );
        }

        let mut invariants = Vec::new();
        let clauses = fields
            .iter()
            .filter_map(|f| f.where_clause.as_ref())
            .chain(class_where);
        for clause in clauses {
            self.analyze_where_clause(clause);
            for predicate in &clause.predicates {
                invariants.push(ClassInvariant {
                    referenced_fields: referenced_field_names(predicate, &field_names),
                    predicate: predicate.clone(),
                });
            }
        }
        self.symbol_table.pop_scope()?;

        if !invariants.is_empty() {
            self.class_invariants.insert(name.to_string(), invariants);
        }

        self.record_inherited_preconditions(name, methods, traits);
        Ok(())
    }

    /// For each interface the class implements, record the preconditions of
    /// the interface methods this class provides. Codegen enforces them at
    /// method entry alongside the method's own `where` clause.
    fn record_inherited_preconditions(
        &mut self,
        class_name: &str,
        methods: &[FunctionNode],
        traits: &[TraitRef],
    ) {
        let mut inherited: HashMap<String, Vec<InheritedPrecondition>> = HashMap::new();
        for trait_ref in traits {
            let Some(interface_methods) = self.interface_preconditions.get(&trait_ref.name) else {
                continue;
            };
            for (method_name, precondition) in interface_methods {
                if methods.iter().any(|m| m.name == *method_name) {
                    inherited
                        .entry(method_name.clone())
                        .or_default()
                        .push(precondition.clone());
                }
            }
        }
        if !inherited.is_empty() {
            self.inherited_preconditions
                .insert(class_name.to_string(), inherited);
        }
    }
}

/// The class fields a predicate references as bare identifiers. A predicate
/// containing a lambda conservatively references every field, since the
/// lambda body is not walked; this keeps assignment-time re-checks sound.
fn referenced_field_names(predicate: &ExpressionNode, fields: &HashSet<String>) -> Vec<String> {
    let mut found = HashSet::new();
    let mut contains_lambda = false;
    collect_identifiers(predicate, fields, &mut found, &mut contains_lambda);
    if contains_lambda {
        return fields.iter().cloned().collect();
    }
    found.into_iter().collect()
}

fn collect_identifiers(
    expr: &ExpressionNode,
    fields: &HashSet<String>,
    found: &mut HashSet<String>,
    contains_lambda: &mut bool,
) {
    match &expr.kind {
        ExpressionKind::Identifier(name) => {
            if fields.contains(name) {
                found.insert(name.clone());
            }
        }
        ExpressionKind::Binary { left, right, .. } => {
            collect_identifiers(left, fields, found, contains_lambda);
            collect_identifiers(right, fields, found, contains_lambda);
        }
        ExpressionKind::Unary { expr, .. } => {
            collect_identifiers(expr, fields, found, contains_lambda);
        }
        ExpressionKind::Call { func, args } => {
            collect_identifiers(func, fields, found, contains_lambda);
            for arg in args {
                collect_identifiers(arg, fields, found, contains_lambda);
            }
        }
        ExpressionKind::FieldAccess { expr, .. } => {
            collect_identifiers(expr, fields, found, contains_lambda);
        }
        ExpressionKind::ListAccess { expr, index } => {
            collect_identifiers(expr, fields, found, contains_lambda);
            collect_identifiers(index, fields, found, contains_lambda);
        }
        ExpressionKind::ListLiteral(elements)
        | ExpressionKind::SetOrMapLiteral(elements)
        | ExpressionKind::TupleLiteral(elements) => {
            for element in elements {
                collect_identifiers(element, fields, found, contains_lambda);
            }
        }
        ExpressionKind::MapLiteral { entries, .. } => {
            for (key, value) in entries {
                collect_identifiers(key, fields, found, contains_lambda);
                collect_identifiers(value, fields, found, contains_lambda);
            }
        }
        ExpressionKind::If {
            cond,
            then_expr,
            else_expr,
        } => {
            collect_identifiers(cond, fields, found, contains_lambda);
            collect_identifiers(then_expr, fields, found, contains_lambda);
            collect_identifiers(else_expr, fields, found, contains_lambda);
        }
        ExpressionKind::Lambda { .. } => {
            *contains_lambda = true;
        }
        ExpressionKind::Literal(_) | ExpressionKind::None | ExpressionKind::GenericType(..) => {}
    }
}
