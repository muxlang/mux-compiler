use super::{SemanticAnalyzer, SemanticError, SymbolKind, Type, format_type};
use crate::ast::{
    ExpressionKind, ExpressionNode, ImportSpec, PatternNode, StatementKind, StatementNode, TypeNode,
};
use crate::diagnostic::Files;
use crate::lexer::Span;

impl SemanticAnalyzer {
    pub(super) fn analyze_block(
        &mut self,
        stmts: &[StatementNode],
        mut files: Option<&mut Files>,
    ) -> Result<(), SemanticError> {
        for stmt in stmts {
            if let StatementKind::Function(func) = &stmt.kind {
                let param_types = func
                    .params
                    .iter()
                    .map(|p| self.resolve_type(&p.type_))
                    .collect::<Result<Vec<_>, _>>()?;
                let return_type = self.resolve_type(&func.return_type)?;
                let mut func_type = Type::Function {
                    params: param_types,
                    returns: Box::new(return_type),
                    default_count: 0,
                };

                for (type_param_name, _) in &func.type_params {
                    let var = Type::Variable(type_param_name.clone());
                    func_type = self.substitute_type_param(&func_type, type_param_name, &var);
                }

                let default_count = func
                    .params
                    .iter()
                    .filter(|p| p.default_value.is_some())
                    .count();

                self.symbol_table.add_symbol(
                    &func.name,
                    self.make_function_symbol(
                        stmt.span,
                        func_type,
                        &func.type_params,
                        default_count,
                    ),
                )?;
            }
        }

        for stmt in stmts {
            self.analyze_statement(stmt, files.as_deref_mut())?;
        }

        Ok(())
    }

    fn analyze_auto_decl_statement(
        &mut self,
        name: &str,
        expr: &ExpressionNode,
        span: Span,
    ) -> Result<(), SemanticError> {
        self.analyze_expression(expr)?;
        let expr_type = self.get_expression_type(expr)?;
        if matches!(expr_type, Type::EmptyList | Type::EmptyMap | Type::EmptySet) {
            let (collection_type, example) = match expr_type {
                Type::EmptyList => ("list", "list<int> myVar = []"),
                Type::EmptyMap => ("map", "map<string, int> myVar = {:}"),
                Type::EmptySet => ("set", "set<int> myVar = {}"),
                _ => unreachable!(),
            };
            return Err(SemanticError::with_help(
                format!("Cannot infer type for empty {} literal", collection_type),
                expr.span,
                format!("Use an explicit type annotation, e.g. {}", example),
            ));
        }
        if Self::type_contains_empty_collection(&expr_type) {
            return Err(SemanticError::with_help(
                "Cannot infer type for expression containing an empty collection literal"
                    .to_string(),
                expr.span,
                "Use explicit type annotations for all empty collection literals.",
            ));
        }
        self.symbol_table.add_symbol(
            name,
            Self::make_symbol(SymbolKind::Variable, span, Some(expr_type)),
        )?;
        Ok(())
    }

    fn type_contains_empty_collection(ty: &Type) -> bool {
        match ty {
            Type::EmptySet | Type::EmptyMap | Type::EmptyList => true,
            Type::Map(key_type, value_type) => {
                Self::type_contains_empty_collection(key_type)
                    || Self::type_contains_empty_collection(value_type)
            }
            Type::Set(elem_type) => Self::type_contains_empty_collection(elem_type),
            Type::List(elem_type) => Self::type_contains_empty_collection(elem_type),
            Type::Optional(inner) => Self::type_contains_empty_collection(inner),
            Type::Result(ok, err) => {
                Self::type_contains_empty_collection(ok)
                    || Self::type_contains_empty_collection(err)
            }
            Type::Tuple(left, right) => {
                Self::type_contains_empty_collection(left)
                    || Self::type_contains_empty_collection(right)
            }
            _ => false,
        }
    }

    fn analyze_typed_decl_statement(
        &mut self,
        name: &str,
        type_node: &TypeNode,
        expr: &ExpressionNode,
        span: Span,
    ) -> Result<(), SemanticError> {
        let declared_type = self.resolve_type(type_node)?;
        self.analyze_expression(expr)?;
        self.resolve_empty_collection_types(&declared_type, expr)?;
        let expr_type = self.get_expression_type(expr)?;
        self.check_type_compatibility(&declared_type, &expr_type, expr.span)?;
        self.symbol_table.add_symbol(
            name,
            Self::make_symbol(SymbolKind::Variable, span, Some(declared_type)),
        )?;
        Ok(())
    }

    pub(super) fn resolve_empty_collection_types(
        &mut self,
        expected_type: &Type,
        expr: &ExpressionNode,
    ) -> Result<(), SemanticError> {
        match &expr.kind {
            ExpressionKind::SetLiteral(elements) if elements.is_empty() => {
                Self::reject_empty_set_as_map(expected_type, expr.span)?;
            }
            ExpressionKind::MapLiteral { entries, .. } if entries.is_empty() => {
                Self::reject_empty_map_as_set(expected_type, expr.span)?;
            }
            ExpressionKind::MapLiteral { entries, .. } => {
                self.resolve_map_literal_children(expected_type, entries)?;
            }
            ExpressionKind::SetLiteral(elements) => {
                self.resolve_typed_collection_elements(expected_type, elements)?;
            }
            ExpressionKind::ListLiteral(elements) => {
                self.resolve_typed_collection_elements(expected_type, elements)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn reject_empty_set_as_map(expected_type: &Type, span: Span) -> Result<(), SemanticError> {
        if matches!(expected_type, Type::Map(_, _)) {
            return Err(SemanticError::with_help(
                format!(
                    "Expected {}, found an empty set literal",
                    format_type(expected_type)
                ),
                span,
                "The empty map literal is `{:}`; `{}` is the empty set.",
            ));
        }
        Ok(())
    }

    fn reject_empty_map_as_set(expected_type: &Type, span: Span) -> Result<(), SemanticError> {
        if matches!(expected_type, Type::Set(_)) {
            return Err(SemanticError::with_help(
                format!(
                    "Expected {}, found an empty map literal",
                    format_type(expected_type)
                ),
                span,
                "The empty set literal is `{}`; `{:}` is the empty map.",
            ));
        }
        Ok(())
    }

    fn resolve_map_literal_children(
        &mut self,
        expected_type: &Type,
        entries: &[(ExpressionNode, ExpressionNode)],
    ) -> Result<(), SemanticError> {
        if let Type::Map(_, value_type) = expected_type {
            for (_, value_expr) in entries {
                self.resolve_empty_collection_types(value_type, value_expr)?;
            }
        }
        Ok(())
    }

    fn resolve_typed_collection_elements(
        &mut self,
        expected_type: &Type,
        elements: &[ExpressionNode],
    ) -> Result<(), SemanticError> {
        let elem_type = match expected_type {
            Type::Set(elem) => elem,
            Type::List(elem) => elem,
            _ => return Ok(()),
        };
        for element in elements {
            self.resolve_empty_collection_types(elem_type, element)?;
        }
        Ok(())
    }

    fn analyze_const_decl_statement(
        &mut self,
        name: &str,
        type_node: &TypeNode,
        expr: &ExpressionNode,
        span: Span,
    ) -> Result<(), SemanticError> {
        let declared_type = self.resolve_type(type_node)?;
        self.analyze_expression(expr)?;
        self.resolve_empty_collection_types(&declared_type, expr)?;
        let expr_type = self.get_expression_type(expr)?;
        self.check_type_compatibility(&declared_type, &expr_type, expr.span)?;
        self.symbol_table.add_symbol(
            name,
            Self::make_symbol(SymbolKind::Constant, span, Some(declared_type)),
        )?;
        Ok(())
    }

    fn analyze_if_statement(
        &mut self,
        cond: &ExpressionNode,
        then_block: &[StatementNode],
        else_block: &Option<Vec<StatementNode>>,
        mut files: Option<&mut Files>,
    ) -> Result<(), SemanticError> {
        self.analyze_expression(cond)?;

        self.symbol_table.push_scope()?;
        self.analyze_block(then_block, files.as_deref_mut())?;
        self.symbol_table.pop_scope()?;

        if let Some(else_block) = else_block {
            self.symbol_table.push_scope()?;
            self.analyze_block(else_block, files)?;
            self.symbol_table.pop_scope()?;
        }

        Ok(())
    }

    fn analyze_for_statement(
        &mut self,
        var: &str,
        iter: &ExpressionNode,
        body: &[StatementNode],
        span: Span,
        files: Option<&mut Files>,
    ) -> Result<(), SemanticError> {
        self.symbol_table.push_scope()?;
        self.analyze_expression(iter)?;
        let iter_type = self.get_expression_type(iter)?;
        let var_type = match iter_type {
            Type::List(elem_type) => *elem_type,
            _ => {
                return Err(SemanticError::with_help(
                    format!("Cannot iterate over type {}", format_type(&iter_type)),
                    iter.span,
                    "The 'for' loop can only iterate over list types. Use .to_list() for sets. For maps, use .get_keys(), .get_values(), or .get_pairs() to get a list. For numeric ranges, use range(start, end).",
                ));
            }
        };
        self.symbol_table.add_symbol(
            var,
            Self::make_symbol(SymbolKind::Variable, span, Some(var_type)),
        )?;
        self.analyze_block(body, files)?;
        self.symbol_table.pop_scope()?;
        Ok(())
    }

    fn analyze_while_statement(
        &mut self,
        cond: &ExpressionNode,
        body: &[StatementNode],
        files: Option<&mut Files>,
    ) -> Result<(), SemanticError> {
        self.analyze_expression(cond)?;
        self.symbol_table.push_scope()?;
        self.analyze_block(body, files)?;
        self.symbol_table.pop_scope()?;
        Ok(())
    }

    /// Record the type of each arm's top-level return so the arms can be
    /// cross-checked. A match used as a statement does not require returning
    /// arms: whole-function return coverage is enforced by
    /// `all_paths_return`, which counts a match as returning only when every
    /// arm does (including through nested if/else).
    fn collect_match_arm_returns(
        &mut self,
        arm: &crate::ast::MatchArm,
        expecting_return: bool,
        arm_return_types: &mut Vec<(Type, Span)>,
    ) -> Result<(), SemanticError> {
        if !expecting_return {
            return Ok(());
        }
        for stmt in &arm.body {
            if let StatementKind::Return(Some(ret_expr)) = &stmt.kind {
                let ret_type = self.get_expression_type(ret_expr)?;
                arm_return_types.push((ret_type, stmt.span));
                break;
            }
        }
        Ok(())
    }

    fn validate_match_arm_return_types(
        &self,
        arm_return_types: &[(Type, Span)],
    ) -> Result<(), SemanticError> {
        if arm_return_types.is_empty() {
            return Ok(());
        }

        let (first_type, _) = &arm_return_types[0];
        for (i, (arm_type, arm_span)) in arm_return_types.iter().enumerate().skip(1) {
            if self
                .check_type_compatibility(first_type, arm_type, *arm_span)
                .is_err()
            {
                return Err(SemanticError::with_help(
                    format!(
                        "Match arms have incompatible return types: first arm returns '{}', but arm {} returns '{}'",
                        format_type(first_type),
                        i + 1,
                        format_type(arm_type)
                    ),
                    *arm_span,
                    "All match arms must return the same type when used as an expression or in a returning context",
                ));
            }
        }

        Ok(())
    }

    fn analyze_match_statement(
        &mut self,
        expr: &ExpressionNode,
        arms: &[crate::ast::MatchArm],
        mut files: Option<&mut Files>,
    ) -> Result<(), SemanticError> {
        self.analyze_expression(expr)?;
        let expr_type = self.get_expression_type(expr)?;

        let expecting_return = self.current_return_type.is_some()
            && !matches!(self.current_return_type, Some(Type::Void));
        let mut arm_return_types = Vec::new();

        for arm in arms {
            self.symbol_table.push_scope()?;
            self.set_pattern_types(&arm.pattern, &expr_type, expr.span)?;
            self.analyze_pattern(&arm.pattern)?;
            if let Some(guard) = &arm.guard {
                self.analyze_expression(guard)?;
            }
            self.analyze_block(&arm.body, files.as_deref_mut())?;
            self.collect_match_arm_returns(arm, expecting_return, &mut arm_return_types)?;
            self.symbol_table.pop_scope()?;
        }

        self.validate_match_arm_return_types(&arm_return_types)?;
        self.check_match_exhaustiveness(&expr_type, arms, expr.span)
    }

    fn analyze_return_with_value(&mut self, expr: &ExpressionNode) -> Result<(), SemanticError> {
        if self.current_return_type.is_none() {
            return Err(SemanticError::with_help(
                "Cannot use 'return' outside of a function",
                expr.span,
                "'return' can only be used inside a function body",
            ));
        }

        self.analyze_expression(expr)?;

        if matches!(self.current_return_type, Some(Type::Void)) {
            return Err(SemanticError::with_help(
                "Cannot return a value from a void function",
                expr.span,
                "This function is declared as 'returns void'. Either remove the return value or change the function's return type.",
            ));
        }

        if let Some(expected_type) = self.current_return_type.clone() {
            let actual_type = self.get_expression_type(expr)?;
            self.check_type_compatibility(&expected_type, &actual_type, expr.span)?;
        }
        Ok(())
    }

    fn analyze_return_without_value(&mut self, span: Span) -> Result<(), SemanticError> {
        if self.current_return_type.is_none() {
            return Err(SemanticError::with_help(
                "Cannot use 'return' outside of a function",
                span,
                "'return' can only be used inside a function body",
            ));
        }

        if !matches!(self.current_return_type, Some(Type::Void))
            && let Some(expected_type) = &self.current_return_type
        {
            return Err(SemanticError::with_help(
                format!(
                    "Missing return value; function expects '{}', but return has no value",
                    format_type(expected_type)
                ),
                span,
                format!(
                    "Add a value after 'return' that matches the function's return type '{}'. Example: return some_value",
                    format_type(expected_type)
                ),
            ));
        }
        Ok(())
    }

    pub(super) fn analyze_import_statement(
        &mut self,
        module_path: &str,
        spec: &ImportSpec,
        span: Span,
        files: Option<&mut Files>,
    ) -> Result<(), SemanticError> {
        if module_path == "std" || module_path.starts_with("std.") {
            self.handle_std_import(module_path, spec, span, files)?;
            return Ok(());
        }

        let files = files.ok_or_else(|| {
            SemanticError::new(
                "Files registry must be available for import processing",
                span,
            )
        })?;

        self.import_module_from_resolver(module_path, spec, span, files)
    }

    pub(super) fn analyze_statement(
        &mut self,
        stmt: &StatementNode,
        mut files: Option<&mut Files>,
    ) -> Result<(), SemanticError> {
        match &stmt.kind {
            StatementKind::AutoDecl(name, _, expr) => {
                self.analyze_auto_decl_statement(name, expr, stmt.span)?;
            }
            StatementKind::TypedDecl(name, type_node, expr) => {
                self.analyze_typed_decl_statement(name, type_node, expr, stmt.span)?;
            }
            StatementKind::Expression(expr) => {
                self.analyze_expression(expr)?;
            }
            StatementKind::Block(stmts) => {
                self.symbol_table.push_scope()?;
                self.analyze_block(stmts, files.as_deref_mut())?;
                self.symbol_table.pop_scope()?;
            }
            StatementKind::ConstDecl(name, type_node, expr) => {
                self.analyze_const_decl_statement(name, type_node, expr, stmt.span)?;
            }
            StatementKind::If {
                cond,
                then_block,
                else_block,
            } => {
                self.analyze_if_statement(cond, then_block, else_block, files.as_deref_mut())?;
            }
            StatementKind::For {
                var, iter, body, ..
            } => {
                self.analyze_for_statement(var, iter, body, stmt.span, files.as_deref_mut())?;
            }
            StatementKind::While { cond, body } => {
                self.analyze_while_statement(cond, body, files.as_deref_mut())?;
            }
            StatementKind::Match { expr, arms } => {
                self.analyze_match_statement(expr, arms, files.as_deref_mut())?;
            }
            StatementKind::Return(Some(expr)) => {
                self.analyze_return_with_value(expr)?;
            }
            StatementKind::Return(None) => {
                self.analyze_return_without_value(stmt.span)?;
            }
            StatementKind::Import { module_path, spec } => {
                // Already resolved during the hoisting pass (see
                // collect_hoistable_declarations) so that classes hoisted
                // later in that same pass can see interfaces this import
                // brings into scope. Skip re-running it here.
                if !self.hoisted_import_spans.contains(&stmt.span) {
                    self.analyze_import_statement(module_path, spec, stmt.span, files)?;
                }
            }
            StatementKind::Function(func) => {
                self.analyze_function(func, None)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn check_match_exhaustiveness(
        &self,
        expr_type: &Type,
        arms: &[crate::ast::MatchArm],
        expr_span: Span,
    ) -> Result<(), SemanticError> {
        match expr_type {
            Type::Result(_, _) => self.check_result_exhaustiveness(arms, expr_span),
            Type::Named(type_name, _) => {
                self.check_named_enum_exhaustiveness(type_name, arms, expr_type, expr_span)
            }
            Type::Optional(_) => self.check_optional_exhaustiveness(arms, expr_span),
            _ => self.require_wildcard_pattern(arms, expr_type, expr_span),
        }
    }

    fn check_result_exhaustiveness(
        &self,
        arms: &[crate::ast::MatchArm],
        expr_span: Span,
    ) -> Result<(), SemanticError> {
        let has_ok = arms.iter().any(|arm| {
            arm.guard.is_none()
                && matches!(&arm.pattern, PatternNode::EnumVariant { name, .. } if name == "ok")
        });
        let has_err = arms.iter().any(|arm| {
            arm.guard.is_none()
                && matches!(&arm.pattern, PatternNode::EnumVariant { name, .. } if name == "err")
        });
        let has_wildcard = arms
            .iter()
            .any(|arm| arm.guard.is_none() && matches!(&arm.pattern, PatternNode::Wildcard));

        if has_wildcard || (has_ok && has_err) {
            return Ok(());
        }

        let mut missing = Vec::new();
        if !has_ok {
            missing.push("ok");
        }
        if !has_err {
            missing.push("err");
        }
        Err(SemanticError::with_help(
            format!(
                "Non-exhaustive match: missing pattern{} for Result: {}",
                if missing.len() > 1 { "s" } else { "" },
                missing.join(", ")
            ),
            expr_span,
            format!(
                "Add match arm{} for: {}, or add a wildcard '_' pattern to cover all remaining cases",
                if missing.len() > 1 { "s" } else { "" },
                missing.join(", ")
            ),
        ))
    }

    fn check_optional_exhaustiveness(
        &self,
        arms: &[crate::ast::MatchArm],
        expr_span: Span,
    ) -> Result<(), SemanticError> {
        let has_some = arms.iter().any(|arm| {
            arm.guard.is_none()
                && matches!(&arm.pattern, PatternNode::EnumVariant { name, .. } if name == "some")
        });
        let has_none = arms.iter().any(|arm| {
            arm.guard.is_none()
                && matches!(&arm.pattern, PatternNode::EnumVariant { name, .. } if name == "none")
        });
        let has_wildcard = arms
            .iter()
            .any(|arm| arm.guard.is_none() && matches!(&arm.pattern, PatternNode::Wildcard));

        if has_wildcard || (has_some && has_none) {
            return Ok(());
        }

        let mut missing = Vec::new();
        if !has_some {
            missing.push("some");
        }
        if !has_none {
            missing.push("none");
        }
        Err(SemanticError::with_help(
            format!(
                "Non-exhaustive match: missing pattern{} for Optional: {}",
                if missing.len() > 1 { "s" } else { "" },
                missing.join(", ")
            ),
            expr_span,
            format!(
                "Add match arm{} for: {}, or add a wildcard '_' pattern",
                if missing.len() > 1 { "s" } else { "" },
                missing.join(", ")
            ),
        ))
    }

    fn check_named_enum_exhaustiveness(
        &self,
        type_name: &str,
        arms: &[crate::ast::MatchArm],
        expr_type: &Type,
        expr_span: Span,
    ) -> Result<(), SemanticError> {
        let Some(symbol) = self.symbol_table.lookup(type_name) else {
            return self.require_wildcard_pattern(arms, expr_type, expr_span);
        };
        let Some(variant_names) = &symbol.variants else {
            return self.require_wildcard_pattern(arms, expr_type, expr_span);
        };

        let (covered, has_wildcard) = self.collect_enum_covered_variants(arms, variant_names);

        if has_wildcard {
            return Ok(());
        }

        let uncovered: Vec<&String> = variant_names
            .iter()
            .filter(|v| !covered.contains(*v))
            .collect();

        if !uncovered.is_empty() {
            let uncovered_list = uncovered
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(SemanticError::with_help(
                format!(
                    "Non-exhaustive match: missing variant{} of '{}': {}",
                    if uncovered.len() > 1 { "s" } else { "" },
                    type_name,
                    uncovered_list
                ),
                expr_span,
                format!(
                    "Add match arm{} for: {}, or add a wildcard '_' pattern",
                    if uncovered.len() > 1 { "s" } else { "" },
                    uncovered_list
                ),
            ));
        }
        Ok(())
    }

    fn collect_enum_covered_variants(
        &self,
        arms: &[crate::ast::MatchArm],
        variant_names: &[String],
    ) -> (std::collections::HashSet<String>, bool) {
        let mut covered: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut has_wildcard = false;

        for arm in arms {
            match &arm.pattern {
                PatternNode::Wildcard => {
                    if arm.guard.is_none() {
                        has_wildcard = true;
                        break;
                    }
                }
                PatternNode::EnumVariant { name, .. } => {
                    if arm.guard.is_none() {
                        covered.insert(name.clone());
                    }
                }
                PatternNode::Identifier(name) => {
                    if arm.guard.is_none() && variant_names.contains(name) {
                        covered.insert(name.clone());
                    }
                }
                _ => {}
            }
        }

        (covered, has_wildcard)
    }

    fn require_wildcard_pattern(
        &self,
        arms: &[crate::ast::MatchArm],
        expr_type: &Type,
        expr_span: Span,
    ) -> Result<(), SemanticError> {
        let has_wildcard = arms
            .iter()
            .any(|arm| arm.guard.is_none() && matches!(arm.pattern, PatternNode::Wildcard));
        if !has_wildcard {
            return Err(SemanticError::with_help(
                format!("Non-exhaustive match on type '{}'", format_type(expr_type)),
                expr_span,
                "Add an unguarded wildcard '_' pattern as the last match arm to handle all remaining cases; a guarded arm can fail its guard at runtime",
            ));
        }
        Ok(())
    }
}
