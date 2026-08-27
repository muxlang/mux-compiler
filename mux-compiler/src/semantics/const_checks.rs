//! Compile-time detection of guaranteed runtime panics.
//!
//! Every check here fires only when the panic is provable from literals via
//! `const_fold`; anything unknown keeps the runtime check as the fallback.
//! Covered: `where` violations at call sites (functions, methods, enum
//! variants, interface preconditions), literal integer division/modulo by
//! zero, provably out-of-bounds list indices, and missing keys in map
//! literals. Always-false predicates and field defaults that violate class
//! invariants are reported from `where_clause.rs` with the same folder.

use std::collections::HashMap;

use super::const_fold::{self, ConstValue};
use super::{SemanticAnalyzer, SemanticError, SymbolKind, Type};
use crate::ast::{AstNode, BinaryOp, EnumVariant, ExpressionKind, ExpressionNode, FunctionNode};
use crate::diagnostic::DiagnosticCode;
use crate::lexer::Span;

/// A function's or method's `where` predicates with what a call site needs to
/// evaluate them: parameter names and any default argument expressions.
#[derive(Debug, Clone, PartialEq)]
pub struct WherePreconditions {
    pub param_names: Vec<String>,
    pub param_defaults: Vec<Option<ExpressionNode>>,
    pub predicates: Vec<ExpressionNode>,
}

/// An enum variant's `where` predicates keyed to its named payload fields.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumVariantPreconditions {
    pub field_names: Vec<Option<String>>,
    pub predicates: Vec<ExpressionNode>,
}

impl SemanticAnalyzer {
    /// Collect the `where` preconditions of every function, method, and enum
    /// variant across all modules, before the analysis pass, so call sites
    /// can check them regardless of declaration order. A name that maps to
    /// two different declarations across modules is dropped entirely: a
    /// wrong-callee check would be a false positive.
    pub(super) fn collect_where_preconditions(&mut self, ast: &[AstNode]) {
        let mut functions = HashMap::new();
        let mut variants = HashMap::new();
        let mut poisoned = Vec::new();
        for nodes in self.all_module_asts.values() {
            Self::extract_where_preconditions(nodes, &mut functions, &mut variants, &mut poisoned);
        }
        Self::extract_where_preconditions(ast, &mut functions, &mut variants, &mut poisoned);
        for key in poisoned {
            functions.remove(&key);
        }
        self.function_preconditions = functions;
        self.enum_variant_preconditions = variants;
    }

    fn extract_where_preconditions(
        nodes: &[AstNode],
        functions: &mut HashMap<(String, String), WherePreconditions>,
        variants: &mut HashMap<(String, String), EnumVariantPreconditions>,
        poisoned: &mut Vec<(String, String)>,
    ) {
        for node in nodes {
            match node {
                AstNode::Function(func) => {
                    Self::record_function_preconditions(String::new(), func, functions, poisoned);
                }
                AstNode::Class { name, methods, .. } => {
                    for method in methods {
                        Self::record_function_preconditions(
                            name.clone(),
                            method,
                            functions,
                            poisoned,
                        );
                    }
                }
                AstNode::Enum {
                    name,
                    variants: enum_variants,
                    ..
                } => Self::record_enum_preconditions(name, enum_variants, variants),
                _ => {}
            }
        }
    }

    fn record_function_preconditions(
        owner: String,
        func: &FunctionNode,
        functions: &mut HashMap<(String, String), WherePreconditions>,
        poisoned: &mut Vec<(String, String)>,
    ) {
        let Some(clause) = &func.where_clause else {
            return;
        };
        let key = (owner, func.name.clone());
        let value = Self::make_preconditions(&func.params, &clause.predicates);
        if let Some(previous) = functions.insert(key.clone(), value.clone())
            && previous != value
        {
            poisoned.push(key);
        }
    }

    fn record_enum_preconditions(
        enum_name: &str,
        enum_variants: &[EnumVariant],
        variants: &mut HashMap<(String, String), EnumVariantPreconditions>,
    ) {
        for variant in enum_variants {
            let Some(clause) = &variant.where_clause else {
                continue;
            };
            variants.insert(
                (enum_name.to_string(), variant.name.clone()),
                EnumVariantPreconditions {
                    field_names: variant
                        .data
                        .iter()
                        .flatten()
                        .map(|(field_name, _)| field_name.clone())
                        .collect(),
                    predicates: clause.predicates.clone(),
                },
            );
        }
    }

    fn make_preconditions(
        params: &[crate::ast::Param],
        predicates: &[ExpressionNode],
    ) -> WherePreconditions {
        WherePreconditions {
            param_names: params.iter().map(|p| p.name.clone()).collect(),
            param_defaults: params.iter().map(|p| p.default_value.clone()).collect(),
            predicates: predicates.to_vec(),
        }
    }

    /// At a call site, if the callee has `where` preconditions and every value
    /// a predicate needs folds to a constant, report a predicate that folds to
    /// false: the call would panic on every execution.
    pub(super) fn check_call_preconditions(
        &mut self,
        expr: &ExpressionNode,
        func: &ExpressionNode,
        args: &[ExpressionNode],
    ) -> Result<(), SemanticError> {
        match &func.kind {
            ExpressionKind::Identifier(name) => {
                if self
                    .symbol_table
                    .lookup(name)
                    .is_some_and(|s| s.kind == SymbolKind::Function)
                    && let Some(pre) = self
                        .function_preconditions
                        .get(&(String::new(), name.clone()))
                {
                    let pre = pre.clone();
                    return self.check_preconditions_against_args(expr, name, &pre, args);
                }
                Ok(())
            }
            ExpressionKind::FieldAccess { expr: inner, field } => {
                self.check_qualified_call_preconditions(expr, inner, field, args)
            }
            _ => Ok(()),
        }
    }

    fn check_qualified_call_preconditions(
        &mut self,
        expr: &ExpressionNode,
        inner: &ExpressionNode,
        field: &str,
        args: &[ExpressionNode],
    ) -> Result<(), SemanticError> {
        // Type-qualified call: enum variant constructor or common method.
        if let ExpressionKind::Identifier(type_name) = &inner.kind {
            match self.symbol_table.lookup(type_name).map(|s| s.kind.clone()) {
                Some(SymbolKind::Enum) => {
                    let key = (type_name.clone(), field.to_string());
                    if let Some(pre) = self.enum_variant_preconditions.get(&key) {
                        let pre = pre.clone();
                        let display = format!("{}.{}", type_name, field);
                        return self.check_variant_preconditions(expr, &display, &pre, args);
                    }
                    return Ok(());
                }
                Some(SymbolKind::Class) => {
                    return self.check_method_preconditions(expr, type_name, field, args);
                }
                _ => {}
            }
        }
        // Instance method call: resolve the receiver's class or interface.
        match self.get_expression_type(inner) {
            Ok(Type::Named(type_name, _)) => {
                match self.symbol_table.lookup(&type_name).map(|s| s.kind.clone()) {
                    Some(SymbolKind::Class) => {
                        self.check_method_preconditions(expr, &type_name, field, args)
                    }
                    Some(SymbolKind::Interface) => {
                        let inherited = self
                            .interface_preconditions
                            .get(&type_name)
                            .and_then(|methods| methods.get(field))
                            .cloned();
                        if let Some(pre) = inherited {
                            let display = format!("{}.{}", type_name, field);
                            let pre = WherePreconditions {
                                param_defaults: vec![None; pre.interface_param_names.len()],
                                param_names: pre.interface_param_names,
                                predicates: pre.predicates,
                            };
                            return self
                                .check_preconditions_against_args(expr, &display, &pre, args);
                        }
                        Ok(())
                    }
                    _ => Ok(()),
                }
            }
            _ => Ok(()),
        }
    }

    /// Check a class method's own preconditions plus the interface
    /// preconditions it inherits, exactly the set codegen enforces at entry.
    fn check_method_preconditions(
        &mut self,
        expr: &ExpressionNode,
        class_name: &str,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<(), SemanticError> {
        let display = format!("{}.{}", class_name, method_name);
        let key = (class_name.to_string(), method_name.to_string());
        if let Some(pre) = self.function_preconditions.get(&key) {
            let pre = pre.clone();
            self.check_preconditions_against_args(expr, &display, &pre, args)?;
        }
        for inherited in self
            .inherited_preconditions(class_name, method_name)
            .to_vec()
        {
            let pre = WherePreconditions {
                param_defaults: vec![None; inherited.interface_param_names.len()],
                param_names: inherited.interface_param_names,
                predicates: inherited.predicates,
            };
            self.check_preconditions_against_args(expr, &display, &pre, args)?;
        }
        Ok(())
    }

    fn check_preconditions_against_args(
        &self,
        expr: &ExpressionNode,
        display_name: &str,
        pre: &WherePreconditions,
        args: &[ExpressionNode],
    ) -> Result<(), SemanticError> {
        if args.len() > pre.param_names.len() {
            return Ok(());
        }
        let mut env = HashMap::new();
        for (i, name) in pre.param_names.iter().enumerate() {
            let value = if i < args.len() {
                const_fold::fold(&args[i])
            } else {
                pre.param_defaults[i].as_ref().and_then(const_fold::fold)
            };
            if let Some(value) = value {
                env.insert(name.clone(), value);
            }
        }
        Self::report_false_predicate(&pre.predicates, &env, expr.span, || {
            format!(
                "arguments to '{}' violate its where constraint",
                display_name
            )
        })
    }

    fn check_variant_preconditions(
        &self,
        expr: &ExpressionNode,
        display_name: &str,
        pre: &EnumVariantPreconditions,
        args: &[ExpressionNode],
    ) -> Result<(), SemanticError> {
        if args.len() != pre.field_names.len() {
            return Ok(());
        }
        let mut env = HashMap::new();
        for (field_name, arg) in pre.field_names.iter().zip(args) {
            if let Some(name) = field_name
                && let Some(value) = const_fold::fold(arg)
            {
                env.insert(name.clone(), value);
            }
        }
        Self::report_false_predicate(&pre.predicates, &env, expr.span, || {
            format!(
                "arguments to '{}' violate its where constraint",
                display_name
            )
        })
    }

    fn report_false_predicate(
        predicates: &[ExpressionNode],
        env: &HashMap<String, ConstValue>,
        span: Span,
        message: impl Fn() -> String,
    ) -> Result<(), SemanticError> {
        for predicate in predicates {
            // A predicate that is false with no bindings at all is already
            // reported at its declaration ("where predicate is always
            // false"); repeating it per call site would be noise.
            if const_fold::fold(predicate) != Some(ConstValue::Bool(false))
                && const_fold::fold_with_env(predicate, env) == Some(ConstValue::Bool(false))
            {
                return Err(SemanticError::with_help(
                    DiagnosticCode::WrongArgumentCount,
                    message(),
                    span,
                    format!(
                        "The where predicate at {}:{} is always false for these argument values, so this call would panic on every execution",
                        predicate.span.row_start, predicate.span.col_start
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Report integer division/modulo whose divisor folds to zero, and field
    /// assignments whose folded value violates a class invariant. Called from
    /// the Binary analysis path after operand types have been validated.
    pub(super) fn check_const_binary(
        &mut self,
        left: &ExpressionNode,
        op: &BinaryOp,
        op_span: &Span,
        right: &ExpressionNode,
    ) -> Result<(), SemanticError> {
        match op {
            BinaryOp::Divide
            | BinaryOp::DivideAssign
            | BinaryOp::Modulo
            | BinaryOp::ModuloAssign => {
                if const_fold::fold(right) == Some(ConstValue::Int(0)) {
                    let operation = if matches!(op, BinaryOp::Divide | BinaryOp::DivideAssign) {
                        "division"
                    } else {
                        "modulo"
                    };
                    return Err(SemanticError::with_help(
                        DiagnosticCode::DivisionByZero,
                        format!("{} by zero", operation),
                        *op_span,
                        "The divisor is always zero, so this operation would panic on every execution",
                    ));
                }
                Ok(())
            }
            BinaryOp::Assign => self.check_field_assignment_invariants(left, right),
            _ => Ok(()),
        }
    }

    /// A literal assigned to a constrained field is checked against every
    /// invariant that references exactly that field; invariants involving
    /// other fields depend on unknown state and stay runtime checks.
    fn check_field_assignment_invariants(
        &mut self,
        left: &ExpressionNode,
        right: &ExpressionNode,
    ) -> Result<(), SemanticError> {
        let ExpressionKind::FieldAccess { expr: obj, field } = &left.kind else {
            return Ok(());
        };
        let Some(value) = const_fold::fold(right) else {
            return Ok(());
        };
        let Ok(Type::Named(class_name, _)) = self.get_expression_type(obj) else {
            return Ok(());
        };
        let invariants: Vec<ExpressionNode> = self
            .class_invariants
            .get(&class_name)
            .map(|invariants| {
                invariants
                    .iter()
                    .filter(|inv| inv.referenced_fields == [field.clone()])
                    .map(|inv| inv.predicate.clone())
                    .collect()
            })
            .unwrap_or_default();
        let mut env = HashMap::new();
        env.insert(field.clone(), value);
        for predicate in &invariants {
            if const_fold::fold_with_env(predicate, &env) == Some(ConstValue::Bool(false)) {
                return Err(SemanticError::with_help(
                    DiagnosticCode::CannotAssign,
                    format!(
                        "assignment violates where constraint of '{}.{}'",
                        class_name, field
                    ),
                    right.span,
                    format!(
                        "The where predicate at {}:{} is always false for this value, so this assignment would panic on every execution",
                        predicate.span.row_start, predicate.span.col_start
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Report list indices that are provably out of bounds and lookups of keys
    /// provably absent from a map literal. Negative indices count from the end
    /// (Python-style), so on a list literal an index is out of bounds only when
    /// it lands before the start or past the end; other targets are checked at
    /// runtime. Called after the index expression has been typechecked.
    pub(super) fn check_const_index(
        &self,
        target: &ExpressionNode,
        target_type: &Type,
        index: &ExpressionNode,
    ) -> Result<(), SemanticError> {
        match target_type {
            Type::List(_) => {
                let Some(ConstValue::Int(value)) = const_fold::fold(index) else {
                    return Ok(());
                };
                // Negative indexes count from the end (Python-style), so they are
                // only invalid when they reach before the start. The bounds are
                // only provable when the list length is known (a list literal);
                // for any other target the index is validated at runtime, where a
                // negative index wraps and a still-out-of-bounds index panics.
                if let ExpressionKind::ListLiteral(elements) = &target.kind {
                    let len = elements.len() as i64;
                    let effective = if value < 0 { len + value } else { value };
                    if effective < 0 || effective >= len {
                        return Err(SemanticError::with_help(
                            DiagnosticCode::InvalidOperation,
                            format!("list index out of bounds: index {}, length {}", value, len),
                            index.span,
                            "The index is always outside this list, so the access would panic on every execution",
                        ));
                    }
                }
                Ok(())
            }
            Type::Map(_, _) => self.check_map_literal_key(target, index),
            _ => Ok(()),
        }
    }

    fn check_map_literal_key(
        &self,
        target: &ExpressionNode,
        index: &ExpressionNode,
    ) -> Result<(), SemanticError> {
        let ExpressionKind::MapLiteral { entries, .. } = &target.kind else {
            return Ok(());
        };
        // Float keys are skipped: their equality is not worth mirroring.
        let foldable_key = |expr: &ExpressionNode| match const_fold::fold(expr) {
            Some(ConstValue::Float(_)) | None => None,
            Some(value) => Some(value),
        };
        let Some(lookup) = foldable_key(index) else {
            return Ok(());
        };
        let mut keys = Vec::with_capacity(entries.len());
        for (key, _) in entries {
            let Some(key) = foldable_key(key) else {
                return Ok(());
            };
            keys.push(key);
        }
        if keys.contains(&lookup) {
            return Ok(());
        }
        Err(SemanticError::with_help(
            DiagnosticCode::InvalidOperation,
            format!("key not found in map: key {}", lookup),
            index.span,
            "The key is never present in this map literal, so the lookup would panic on every execution",
        ))
    }
}
