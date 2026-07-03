use super::{
    SemanticAnalyzer, SemanticError, Symbol, SymbolKind, Type, collection_new_hint, format_type,
};
use crate::ast::{
    ExpressionKind, ExpressionNode, LiteralNode, Param, PrimitiveType, StatementKind,
    StatementNode, TypeNode, UnaryOp,
};
use crate::lexer::Span;
use crate::semantics::std_registry::std_module_registry;
use std::collections::HashMap;

impl SemanticAnalyzer {
    pub(super) fn analyze_expression(
        &mut self,
        expr: &ExpressionNode,
    ) -> Result<(), SemanticError> {
        match &expr.kind {
            ExpressionKind::Identifier(name) => self.analyze_identifier_expr(name, expr),
            ExpressionKind::Literal(_) => Ok(()),
            ExpressionKind::None => Ok(()),
            ExpressionKind::Binary {
                left,
                op,
                op_span,
                right,
            } => {
                // List writes wrap negative indices and extend past the end,
                // so an assignment target is exempt from the provable
                // out-of-bounds check that applies to reads.
                if op.is_assignment()
                    && let ExpressionKind::ListAccess {
                        expr: target,
                        index,
                    } = &left.kind
                {
                    self.analyze_list_access_expr(target, index, true)?;
                } else {
                    self.analyze_expression(left)?;
                }
                self.analyze_expression(right)?;
                let _ = self.get_expression_type(expr)?;
                self.check_const_binary(left, op, op_span, right)
            }
            ExpressionKind::Unary {
                expr,
                op,
                op_span,
                postfix: _,
            } => self.analyze_unary_expr(expr, op, *op_span),
            ExpressionKind::Call { func, args } => self.analyze_call_expr(expr, func, args),
            ExpressionKind::FieldAccess { expr, .. } => {
                self.analyze_expression(expr)?;
                let _ = self.get_expression_type(expr)?;
                Ok(())
            }
            ExpressionKind::ListAccess { expr, index } => {
                self.analyze_list_access_expr(expr, index, false)
            }
            ExpressionKind::ListLiteral(elements) => self.analyze_list_literal_expr(elements),
            ExpressionKind::MapLiteral { entries, .. } => {
                self.analyze_map_literal_expr(expr, entries)
            }
            ExpressionKind::SetOrMapLiteral(elements) => self.analyze_set_literal_expr(elements),
            ExpressionKind::TupleLiteral(elements) => {
                self.analyze_tuple_literal_expr(expr, elements)
            }
            ExpressionKind::If {
                cond,
                then_expr,
                else_expr,
            } => self.analyze_if_expr(cond, then_expr, else_expr),
            ExpressionKind::Lambda {
                params,
                return_type,
                body,
                where_clause,
            } => self.analyze_lambda_expr(expr, params, return_type, body, where_clause.as_ref()),
            ExpressionKind::GenericType(name, type_args) => {
                self.analyze_generic_type_expr(expr, name, type_args)
            }
        }
    }

    fn analyze_identifier_expr(
        &mut self,
        name: &str,
        expr: &ExpressionNode,
    ) -> Result<(), SemanticError> {
        if name == "self" {
            return self.analyze_self_identifier(expr);
        }
        let mut exists_like =
            self.symbol_table.exists(name) || self.get_builtin_sig(name).is_some();
        if !exists_like {
            exists_like = self.check_stdlib_imports_for_class(name);
        }
        if !exists_like {
            if self.symbol_table.find_similar(name).is_none()
                && let Some(help) = collection_new_hint(name)
            {
                return Err(SemanticError::with_help(
                    format!("Undefined variable '{}'", name),
                    expr.span,
                    help,
                ));
            }
            return Err(self.undefined_symbol_error("variable", name, expr.span));
        }
        Ok(())
    }

    fn analyze_self_identifier(&self, expr: &ExpressionNode) -> Result<(), SemanticError> {
        if self.is_in_static_method {
            return Err(SemanticError::with_help(
                "Cannot use 'self' in a common method",
                expr.span,
                "Common (static) methods do not have access to 'self'. Remove the 'common' modifier or access the class through a parameter instead.",
            ));
        }
        if self.current_self_type.is_none() {
            return Err(SemanticError::with_help(
                "Cannot use 'self' outside of a method",
                expr.span,
                "'self' is only available inside instance methods of a class",
            ));
        }
        Ok(())
    }

    fn check_stdlib_imports_for_class(&self, name: &str) -> bool {
        let stdlib_names: std::collections::HashSet<String> = std_module_registry()
            .keys()
            .filter_map(|s| s.strip_prefix("std.").map(|name| name.to_string()))
            .collect();
        for (ns, module_symbols) in &self.imported_symbols {
            if !stdlib_names.contains(ns) {
                continue;
            }
            if let Some(sym) = module_symbols.get(name)
                && matches!(sym.kind, SymbolKind::Class)
            {
                return true;
            }
        }
        false
    }

    fn analyze_unary_expr(
        &mut self,
        expr: &ExpressionNode,
        op: &UnaryOp,
        op_span: Span,
    ) -> Result<(), SemanticError> {
        self.analyze_expression(expr)?;
        let operand_type = self.get_expression_type(expr)?;
        match op {
            UnaryOp::Not => self.check_not_operator_type(&operand_type, op_span),
            UnaryOp::Neg => self.check_neg_operator_type(&operand_type, op_span),
            UnaryOp::Ref => Ok(()),
            UnaryOp::Incr | UnaryOp::Decr => {
                self.check_incr_decr_operator_type(&operand_type, op_span)?;
                self.check_incr_decr_const_modification(expr, op_span)
            }
            _ => Ok(()),
        }
    }

    fn check_not_operator_type(
        &self,
        operand_type: &Type,
        op_span: Span,
    ) -> Result<(), SemanticError> {
        if !matches!(operand_type, Type::Primitive(PrimitiveType::Bool)) {
            return Err(SemanticError::with_help(
                format!(
                    "Logical 'not' operator requires a boolean operand, found {}",
                    format_type(operand_type)
                ),
                op_span,
                "The '!' operator can only be applied to bool values",
            ));
        }
        Ok(())
    }

    fn check_neg_operator_type(
        &self,
        operand_type: &Type,
        op_span: Span,
    ) -> Result<(), SemanticError> {
        if !matches!(
            operand_type,
            Type::Primitive(PrimitiveType::Int) | Type::Primitive(PrimitiveType::Float)
        ) {
            return Err(SemanticError::with_help(
                format!(
                    "Negation operator '-' requires a numeric operand, found {}",
                    format_type(operand_type)
                ),
                op_span,
                "The unary '-' operator can only be applied to int or float values",
            ));
        }
        Ok(())
    }

    fn check_incr_decr_operator_type(
        &self,
        operand_type: &Type,
        op_span: Span,
    ) -> Result<(), SemanticError> {
        if !matches!(operand_type, Type::Primitive(PrimitiveType::Int)) {
            return Err(SemanticError::with_help(
                format!(
                    "Increment/decrement operators require an int operand, found {}",
                    format_type(operand_type)
                ),
                op_span,
                "The '++' and '--' operators can only be applied to int variables",
            ));
        }
        Ok(())
    }

    fn check_incr_decr_const_modification(
        &mut self,
        expr: &ExpressionNode,
        op_span: Span,
    ) -> Result<(), SemanticError> {
        if let ExpressionKind::Identifier(name) = &expr.kind
            && let Some(symbol) = self.symbol_table.lookup(name)
            && symbol.kind == SymbolKind::Constant
        {
            return Err(SemanticError::with_help(
                format!("Cannot modify constant '{}'", name),
                op_span,
                "Constants cannot be modified after initialization",
            ));
        }

        if let ExpressionKind::FieldAccess {
            expr: obj_expr,
            field,
        } = &expr.kind
        {
            let obj_type = self.get_expression_type(obj_expr)?;
            if let Type::Named(class_name, _) = &obj_type
                && let Some(symbol) = self.symbol_table.lookup(class_name)
                && let Some((_field_type, is_const)) = symbol.fields.get(field)
                && *is_const
            {
                return Err(SemanticError::with_help(
                    format!("Cannot modify const field '{}'", field),
                    op_span,
                    "Const fields cannot be modified after initialization. Remove the 'const' modifier from the field declaration if mutation is needed.",
                ));
            }
        }
        Ok(())
    }

    fn analyze_call_expr(
        &mut self,
        expr: &ExpressionNode,
        func: &ExpressionNode,
        args: &[ExpressionNode],
    ) -> Result<(), SemanticError> {
        if let ExpressionKind::Identifier(name) = &func.kind
            && !self.symbol_table.exists(name)
            && self.get_builtin_sig(name).is_none()
        {
            return Err(self.undefined_symbol_error("function", name, func.span));
        }

        self.analyze_expression(func)?;
        for arg in args {
            self.analyze_expression(arg)?;
        }

        if let ExpressionKind::Identifier(name) = &func.kind
            && name == "some"
        {
            self.check_some_call_args(expr, args)?;
        }

        let _ = self.get_expression_type(expr)?;
        self.check_call_preconditions(expr, func, args)
    }

    fn check_some_call_args(
        &mut self,
        expr: &ExpressionNode,
        args: &[ExpressionNode],
    ) -> Result<(), SemanticError> {
        if args.len() != 1 {
            return Err(SemanticError::with_help(
                format!("Some() takes exactly 1 argument, got {}", args.len()),
                expr.span,
                "Wrap a single value in Some(), e.g. Some(42)",
            ));
        }
        let arg_type = self.get_expression_type(&args[0])?;
        if let Type::Optional(_) = arg_type {
            return Err(SemanticError::with_help(
                "Some() cannot wrap an Optional value",
                expr.span,
                "The argument to Some() must not be Optional. Remove the nested Some() or unwrap the inner value first.",
            ));
        }
        Ok(())
    }

    fn analyze_list_access_expr(
        &mut self,
        expr: &ExpressionNode,
        index: &ExpressionNode,
        is_assignment_target: bool,
    ) -> Result<(), SemanticError> {
        self.analyze_expression(expr)?;
        self.analyze_expression(index)?;
        let target_type = self.get_expression_type(expr)?;
        let index_type = self.get_expression_type(index)?;
        match &target_type {
            Type::List(_) => {
                if !matches!(index_type, Type::Primitive(PrimitiveType::Int)) {
                    return Err(SemanticError::with_help(
                        format!(
                            "List index must be an integer, found {}",
                            format_type(&index_type)
                        ),
                        index.span,
                        "Lists can only be indexed with integer values, e.g. myList[0]",
                    ));
                }
            }
            Type::Map(expected_key_type, _) => {
                if index_type != **expected_key_type {
                    return Err(SemanticError::with_help(
                        format!(
                            "Map key type mismatch: expected {}, found {}",
                            format_type(expected_key_type),
                            format_type(&index_type)
                        ),
                        index.span,
                        format!(
                            "This map has keys of type {}",
                            format_type(expected_key_type)
                        ),
                    ));
                }
            }
            Type::EmptyMap => {
                return Err(SemanticError::with_help(
                    "Cannot index empty map",
                    expr.span,
                    "The map type is unknown. Provide type annotations or add entries to the map literal.",
                ));
            }
            _ => {
                return Err(SemanticError::with_help(
                    "Cannot index non-list type",
                    expr.span,
                    "Only lists and maps can be indexed with '[]'. Examples: my_list[0], my_map['key']",
                ));
            }
        }
        if is_assignment_target {
            return Ok(());
        }
        self.check_const_index(expr, &target_type, index)
    }

    fn analyze_list_literal_expr(
        &mut self,
        elements: &[ExpressionNode],
    ) -> Result<(), SemanticError> {
        for elem in elements {
            self.analyze_expression(elem)?;
        }
        if !elements.is_empty() {
            let first_type = self.get_expression_type(&elements[0])?;
            for elem in &elements[1..] {
                let elem_type = self.get_expression_type(elem)?;
                if elem_type != first_type {
                    return Err(SemanticError::with_help(
                        format!(
                            "List element type mismatch: expected {}, found {}",
                            format_type(&first_type),
                            format_type(&elem_type)
                        ),
                        elem.span,
                        "All elements in a list literal must have the same type",
                    ));
                }
            }
        }
        Ok(())
    }

    fn analyze_map_literal_expr(
        &mut self,
        _expr: &ExpressionNode,
        entries: &[(ExpressionNode, ExpressionNode)],
    ) -> Result<(), SemanticError> {
        for (key, value) in entries {
            self.analyze_expression(key)?;
            self.analyze_expression(value)?;
        }
        if !entries.is_empty() {
            let (first_key, first_value) = &entries[0];
            let key_type = self.get_expression_type(first_key)?;
            self.check_map_key_hashable(first_key, &key_type)?;

            let value_type = self.get_expression_type(first_value)?;
            for (key, value) in &entries[1..] {
                self.check_map_entry_type_consistency(key, value, &key_type, &value_type)?;
            }
        }
        Ok(())
    }

    fn check_map_key_hashable(
        &self,
        key_expr: &ExpressionNode,
        key_type: &Type,
    ) -> Result<(), SemanticError> {
        let is_hashable = matches!(key_type, Type::Primitive(_));
        if !is_hashable {
            return Err(SemanticError::with_help(
                format!(
                    "Map keys must be a hashable type, found '{}'",
                    format_type(key_type)
                ),
                key_expr.span,
                "Only primitive types (int, float, string, bool, char) can be used as map keys",
            ));
        }
        Ok(())
    }

    fn check_map_entry_type_consistency(
        &mut self,
        key: &ExpressionNode,
        value: &ExpressionNode,
        expected_key: &Type,
        expected_value: &Type,
    ) -> Result<(), SemanticError> {
        let k_type = self.get_expression_type(key)?;
        self.check_map_key_hashable(key, &k_type)?;

        let v_type = self.get_expression_type(value)?;
        if k_type != *expected_key {
            return Err(SemanticError::with_help(
                format!(
                    "Map key type mismatch: expected {}, found {}",
                    format_type(expected_key),
                    format_type(&k_type)
                ),
                key.span,
                "All keys in a map literal must have the same type",
            ));
        }
        if v_type != *expected_value {
            return Err(SemanticError::with_help(
                format!(
                    "Map value type mismatch: expected {}, found {}",
                    format_type(expected_value),
                    format_type(&v_type)
                ),
                value.span,
                "All values in a map literal must have the same type",
            ));
        }
        Ok(())
    }

    fn analyze_set_literal_expr(
        &mut self,
        elements: &[ExpressionNode],
    ) -> Result<(), SemanticError> {
        for elem in elements {
            self.analyze_expression(elem)?;
        }
        if !elements.is_empty() {
            let first_type = self.get_expression_type(&elements[0])?;
            for elem in &elements[1..] {
                let elem_type = self.get_expression_type(elem)?;
                if elem_type != first_type {
                    return Err(SemanticError::with_help(
                        format!(
                            "Set element type mismatch: expected {}, found {}",
                            format_type(&first_type),
                            format_type(&elem_type)
                        ),
                        elem.span,
                        "All elements in a set literal must have the same type",
                    ));
                }
            }
        }
        Ok(())
    }

    fn analyze_tuple_literal_expr(
        &mut self,
        expr: &ExpressionNode,
        elements: &[ExpressionNode],
    ) -> Result<(), SemanticError> {
        if elements.len() != 2 {
            return Err(SemanticError::with_help(
                format!(
                    "Tuple must have exactly 2 elements, found {}",
                    elements.len()
                ),
                expr.span,
                "Tuples in Mux are pairs with exactly two elements, e.g. (1, 2)",
            ));
        }
        for elem in elements {
            self.analyze_expression(elem)?;
        }
        Ok(())
    }

    fn analyze_if_expr(
        &mut self,
        cond: &ExpressionNode,
        then_expr: &ExpressionNode,
        else_expr: &ExpressionNode,
    ) -> Result<(), SemanticError> {
        self.analyze_expression(cond)?;
        self.analyze_expression(then_expr)?;
        self.analyze_expression(else_expr)?;
        let cond_type = self.get_expression_type(cond)?;
        if !matches!(cond_type, Type::Primitive(PrimitiveType::Bool)) {
            return Err(SemanticError::with_help(
                format!(
                    "If condition must be boolean, found {}",
                    format_type(&cond_type)
                ),
                cond.span,
                "The condition in an if expression must evaluate to a bool value",
            ));
        }
        Ok(())
    }

    fn analyze_lambda_expr(
        &mut self,
        expr: &ExpressionNode,
        params: &[Param],
        return_type: &TypeNode,
        body: &[StatementNode],
        where_clause: Option<&crate::ast::WhereClause>,
    ) -> Result<(), SemanticError> {
        let mut local_vars = std::collections::HashSet::new();
        for param in params {
            local_vars.insert(param.name.clone());
        }

        self.symbol_table.push_scope()?;

        let lambda_return_type = self.resolve_type(return_type)?;
        let prev_return_type = self.current_return_type.clone();
        self.current_return_type = Some(lambda_return_type.clone());

        for param in params {
            let param_type = self.resolve_type(&param.type_)?;
            self.symbol_table.add_symbol(
                &param.name,
                Symbol {
                    kind: SymbolKind::Variable,
                    span: param.type_.span,
                    type_: Some(param_type),
                    interfaces: HashMap::new(),
                    methods: HashMap::new(),
                    fields: HashMap::new(),
                    type_params: Vec::new(),
                    original_name: None,
                    llvm_name: None,
                    default_param_count: 0,
                    variants: None,
                },
            )?;
        }

        // typecheck where-clause preconditions with the params in scope.
        if let Some(clause) = where_clause {
            self.analyze_where_clause(clause);
        }

        self.analyze_block(body, None)?;

        self.check_lambda_return_paths(expr, body, &lambda_return_type)?;

        self.current_return_type = prev_return_type;
        let mut captures = self.find_free_variables_in_block(body, &local_vars)?;
        // Where predicates run inside the lambda, so outer variables they
        // reference must be captured too.
        if let Some(clause) = where_clause {
            let predicate_captures =
                self.find_free_variables_in_exprs(&clause.predicates, &local_vars)?;
            for capture in predicate_captures {
                if !captures.iter().any(|(name, _)| *name == capture.0) {
                    captures.push(capture);
                }
            }
        }
        self.lambda_captures.insert(expr.span, captures);

        self.symbol_table.pop_scope()?;
        Ok(())
    }

    fn check_lambda_return_paths(
        &mut self,
        expr: &ExpressionNode,
        body: &[StatementNode],
        lambda_return_type: &Type,
    ) -> Result<(), SemanticError> {
        if body.is_empty() || !self.all_paths_return(body) {
            let (msg, help) = if matches!(lambda_return_type, Type::Void) {
                (
                    "Lambda must end with an explicit 'return' statement on all code paths"
                        .to_string(),
                    "Add a 'return' statement at the end of every code path in the lambda body"
                        .to_string(),
                )
            } else {
                (
                    format!(
                        "Lambda must return a value of type '{}' on all code paths",
                        format_type(lambda_return_type)
                    ),
                    "Add a return statement at the end of every branch in the lambda body"
                        .to_string(),
                )
            };
            return Err(SemanticError::with_help(msg, expr.span, help));
        }
        if let Some(last_stmt) = body.last()
            && let StatementKind::Return(Some(ret_expr)) = &last_stmt.kind
        {
            let actual_type = self.get_expression_type(ret_expr)?;
            self.check_type_compatibility(lambda_return_type, &actual_type, ret_expr.span)?;
        }
        Ok(())
    }

    fn analyze_generic_type_expr(
        &mut self,
        expr: &ExpressionNode,
        name: &str,
        type_args: &[TypeNode],
    ) -> Result<(), SemanticError> {
        if name == "tuple" {
            return self.check_tuple_type_args(expr, type_args);
        }
        if self.symbol_table.find_similar(name).is_none()
            && let Some(help) = collection_new_hint(name)
        {
            return Err(SemanticError::with_help(
                format!("Undefined type '{}'", name),
                expr.span,
                help,
            ));
        }
        if let Some((module_name, type_name)) = name.split_once('.') {
            let module_symbols = self
                .imported_symbols
                .get(module_name)
                .ok_or_else(|| self.undefined_symbol_error("module", module_name, expr.span))?;
            let _ = module_symbols
                .get(type_name)
                .ok_or_else(|| self.undefined_symbol_error("type", type_name, expr.span))?;
        } else if !self.symbol_table.exists(name) {
            return Err(self.undefined_symbol_error("type", name, expr.span));
        }
        Ok(())
    }

    fn check_tuple_type_args(
        &self,
        expr: &ExpressionNode,
        type_args: &[TypeNode],
    ) -> Result<(), SemanticError> {
        if type_args.len() != 2 {
            return Err(SemanticError::with_help(
                format!(
                    "Tuple type requires exactly 2 type arguments, got {}",
                    type_args.len()
                ),
                expr.span,
                "Tuples in Mux are pairs, e.g. tuple<int, string>",
            ));
        }
        for arg in type_args {
            self.resolve_type(arg)?;
        }
        Ok(())
    }

    pub(super) fn infer_literal_type(&self, expr: &ExpressionNode) -> Result<Type, SemanticError> {
        match &expr.kind {
            ExpressionKind::Literal(lit) => match lit {
                LiteralNode::Integer(_) => Ok(Type::Primitive(PrimitiveType::Int)),
                LiteralNode::Float(_) => Ok(Type::Primitive(PrimitiveType::Float)),
                LiteralNode::String(_) => Ok(Type::Primitive(PrimitiveType::Str)),
                LiteralNode::Boolean(_) => Ok(Type::Primitive(PrimitiveType::Bool)),
                LiteralNode::Char(_) => Ok(Type::Primitive(PrimitiveType::Char)),
            },
            _ => Err(SemanticError::with_help(
                "Expected a literal expression",
                expr.span,
                "Only literal values (integers, floats, strings, booleans, chars) are allowed here",
            )),
        }
    }

    pub(super) fn types_compatible(&self, type1: &Type, type2: &Type) -> bool {
        match (type1, type2) {
            (Type::Variable(v1), Type::Variable(v2)) => v1 == v2,
            (Type::Generic(g1), Type::Generic(g2)) => g1 == g2,
            _ => type1 == type2,
        }
    }
}
