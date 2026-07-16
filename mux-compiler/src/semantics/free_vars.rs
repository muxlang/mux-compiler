use super::{SemanticAnalyzer, SemanticError, SymbolKind, Type};
use crate::ast::{ExpressionKind, ExpressionNode, Param, StatementKind, StatementNode};
use crate::lexer::Span;

impl SemanticAnalyzer {
    // Helper to find free variables in a block of statements
    // Returns variables that are used but not declared in the local scope
    pub(super) fn find_free_variables_in_block(
        &self,
        body: &[StatementNode],
        local_vars: &std::collections::HashSet<String>,
    ) -> Result<Vec<(String, Type)>, SemanticError> {
        let mut free_vars = std::collections::HashMap::new();
        let mut local_vars = local_vars.clone();

        for stmt in body {
            self.find_free_variables_in_statement(stmt, &mut local_vars, &mut free_vars)?;
        }

        Ok(free_vars.into_iter().collect())
    }

    /// Free variables referenced by a list of bare expressions (e.g. the
    /// predicates of a lambda's where clause).
    pub(super) fn find_free_variables_in_exprs(
        &self,
        exprs: &[ExpressionNode],
        local_vars: &std::collections::HashSet<String>,
    ) -> Result<Vec<(String, Type)>, SemanticError> {
        let mut free_vars = std::collections::HashMap::new();

        for expr in exprs {
            self.find_free_variables_in_expression(expr, local_vars, &mut free_vars)?;
        }

        Ok(free_vars.into_iter().collect())
    }

    fn find_free_variables_in_statement(
        &self,
        stmt: &StatementNode,
        local_vars: &mut std::collections::HashSet<String>,
        free_vars: &mut std::collections::HashMap<String, Type>,
    ) -> Result<(), SemanticError> {
        match &stmt.kind {
            StatementKind::Expression(expr) | StatementKind::Return(Some(expr)) => {
                self.find_free_variables_in_expression(expr, local_vars, free_vars)?;
            }
            StatementKind::AutoDecl(name, _, expr) => {
                self.handle_variable_declaration(name, expr, local_vars, free_vars)?;
            }
            StatementKind::TypedDecl(name, _, expr) => {
                self.handle_variable_declaration(name, expr, local_vars, free_vars)?;
            }
            StatementKind::ConstDecl(name, _, expr) => {
                self.handle_variable_declaration(name, expr, local_vars, free_vars)?;
            }
            StatementKind::If {
                cond,
                then_block,
                else_block,
            } => {
                self.handle_if_statement(cond, then_block, else_block, local_vars, free_vars)?;
            }
            StatementKind::While { cond, body } => {
                self.handle_while_statement(cond, body, local_vars, free_vars)?;
            }
            StatementKind::For {
                var, iter, body, ..
            } => {
                self.handle_for_statement(var, iter, body, local_vars, free_vars)?;
            }
            StatementKind::Block(stmts) => {
                self.handle_block_statement(stmts, local_vars, free_vars)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_variable_declaration(
        &self,
        name: &str,
        expr: &ExpressionNode,
        local_vars: &mut std::collections::HashSet<String>,
        free_vars: &mut std::collections::HashMap<String, Type>,
    ) -> Result<(), SemanticError> {
        // First analyze the expression (uses happen before the decl is in scope)
        self.find_free_variables_in_expression(expr, local_vars, free_vars)?;
        // Then add the variable to local scope
        local_vars.insert(name.to_string());
        Ok(())
    }

    fn handle_if_statement(
        &self,
        cond: &ExpressionNode,
        then_block: &[StatementNode],
        else_block: &Option<Vec<StatementNode>>,
        local_vars: &mut std::collections::HashSet<String>,
        free_vars: &mut std::collections::HashMap<String, Type>,
    ) -> Result<(), SemanticError> {
        self.find_free_variables_in_expression(cond, local_vars, free_vars)?;
        for s in then_block {
            self.find_free_variables_in_statement(s, local_vars, free_vars)?;
        }
        if let Some(else_stmts) = else_block {
            for s in else_stmts {
                self.find_free_variables_in_statement(s, local_vars, free_vars)?;
            }
        }
        Ok(())
    }

    fn handle_while_statement(
        &self,
        cond: &ExpressionNode,
        body: &[StatementNode],
        local_vars: &mut std::collections::HashSet<String>,
        free_vars: &mut std::collections::HashMap<String, Type>,
    ) -> Result<(), SemanticError> {
        self.find_free_variables_in_expression(cond, local_vars, free_vars)?;
        for s in body {
            self.find_free_variables_in_statement(s, local_vars, free_vars)?;
        }
        Ok(())
    }

    fn handle_for_statement(
        &self,
        var: &str,
        iter: &ExpressionNode,
        body: &[StatementNode],
        local_vars: &mut std::collections::HashSet<String>,
        free_vars: &mut std::collections::HashMap<String, Type>,
    ) -> Result<(), SemanticError> {
        self.find_free_variables_in_expression(iter, local_vars, free_vars)?;
        // Iterator variable is local to the for loop
        local_vars.insert(var.to_string());
        for s in body {
            self.find_free_variables_in_statement(s, local_vars, free_vars)?;
        }
        Ok(())
    }

    fn handle_block_statement(
        &self,
        stmts: &[StatementNode],
        local_vars: &mut std::collections::HashSet<String>,
        free_vars: &mut std::collections::HashMap<String, Type>,
    ) -> Result<(), SemanticError> {
        for s in stmts {
            self.find_free_variables_in_statement(s, local_vars, free_vars)?;
        }
        Ok(())
    }

    fn find_free_variables_in_expression(
        &self,
        expr: &ExpressionNode,
        local_vars: &std::collections::HashSet<String>,
        free_vars: &mut std::collections::HashMap<String, Type>,
    ) -> Result<(), SemanticError> {
        match &expr.kind {
            ExpressionKind::Identifier(name) => {
                self.handle_identifier_expression(name, local_vars, free_vars)?;
            }
            ExpressionKind::Binary { left, right, .. } => {
                self.handle_binary_expression(left, right, local_vars, free_vars)?;
            }
            ExpressionKind::Unary {
                expr: inner,
                op_span: _,
                ..
            } => {
                self.handle_unary_expression(inner, local_vars, free_vars)?;
            }
            ExpressionKind::Call { func, args } => {
                self.handle_call_expression(func, args, local_vars, free_vars)?;
            }
            ExpressionKind::FieldAccess { expr: inner, .. } => {
                self.handle_field_access_expression(inner, local_vars, free_vars)?;
            }
            ExpressionKind::ListAccess { expr: inner, index } => {
                self.handle_list_access_expression(inner, index, local_vars, free_vars)?;
            }
            ExpressionKind::ListLiteral(elements) => {
                self.handle_list_literal_expression(elements, local_vars, free_vars)?;
            }
            ExpressionKind::MapLiteral { entries, .. } => {
                self.handle_map_literal_expression(entries, local_vars, free_vars)?;
            }
            ExpressionKind::SetLiteral(elements) => {
                self.handle_set_literal_expression(elements, local_vars, free_vars)?;
            }
            ExpressionKind::TupleLiteral(elements) => {
                self.handle_tuple_literal_expression(elements, local_vars, free_vars)?;
            }
            ExpressionKind::If {
                cond,
                then_expr,
                else_expr,
            } => {
                self.handle_if_expression(cond, then_expr, else_expr, local_vars, free_vars)?;
            }
            ExpressionKind::Lambda { params, body, .. } => {
                self.handle_lambda_expression(params, body, local_vars, free_vars)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_identifier_expression(
        &self,
        name: &str,
        local_vars: &std::collections::HashSet<String>,
        free_vars: &mut std::collections::HashMap<String, Type>,
    ) -> Result<(), SemanticError> {
        // Check if it's a local variable (parameter or declared in lambda body)
        if !local_vars.contains(name) {
            // Check if it exists in outer scopes
            if let Some(symbol) = self.symbol_table.lookup(name)
                && matches!(symbol.kind, SymbolKind::Variable)
                && let Some(var_type) = &symbol.type_
            {
                free_vars.insert(name.to_string(), var_type.clone());
            }
        }
        Ok(())
    }

    fn handle_binary_expression(
        &self,
        left: &ExpressionNode,
        right: &ExpressionNode,
        local_vars: &std::collections::HashSet<String>,
        free_vars: &mut std::collections::HashMap<String, Type>,
    ) -> Result<(), SemanticError> {
        self.find_free_variables_in_expression(left, local_vars, free_vars)?;
        self.find_free_variables_in_expression(right, local_vars, free_vars)?;
        Ok(())
    }

    fn handle_unary_expression(
        &self,
        inner: &ExpressionNode,
        local_vars: &std::collections::HashSet<String>,
        free_vars: &mut std::collections::HashMap<String, Type>,
    ) -> Result<(), SemanticError> {
        self.find_free_variables_in_expression(inner, local_vars, free_vars)?;
        Ok(())
    }

    fn handle_call_expression(
        &self,
        func: &ExpressionNode,
        args: &[ExpressionNode],
        local_vars: &std::collections::HashSet<String>,
        free_vars: &mut std::collections::HashMap<String, Type>,
    ) -> Result<(), SemanticError> {
        self.find_free_variables_in_expression(func, local_vars, free_vars)?;
        for arg in args {
            self.find_free_variables_in_expression(arg, local_vars, free_vars)?;
        }
        Ok(())
    }

    fn handle_field_access_expression(
        &self,
        inner: &ExpressionNode,
        local_vars: &std::collections::HashSet<String>,
        free_vars: &mut std::collections::HashMap<String, Type>,
    ) -> Result<(), SemanticError> {
        self.find_free_variables_in_expression(inner, local_vars, free_vars)?;
        Ok(())
    }

    fn handle_list_access_expression(
        &self,
        inner: &ExpressionNode,
        index: &ExpressionNode,
        local_vars: &std::collections::HashSet<String>,
        free_vars: &mut std::collections::HashMap<String, Type>,
    ) -> Result<(), SemanticError> {
        self.find_free_variables_in_expression(inner, local_vars, free_vars)?;
        self.find_free_variables_in_expression(index, local_vars, free_vars)?;
        Ok(())
    }

    fn handle_list_literal_expression(
        &self,
        elements: &[ExpressionNode],
        local_vars: &std::collections::HashSet<String>,
        free_vars: &mut std::collections::HashMap<String, Type>,
    ) -> Result<(), SemanticError> {
        for elem in elements {
            self.find_free_variables_in_expression(elem, local_vars, free_vars)?;
        }
        Ok(())
    }

    fn handle_map_literal_expression(
        &self,
        entries: &[(ExpressionNode, ExpressionNode)],
        local_vars: &std::collections::HashSet<String>,
        free_vars: &mut std::collections::HashMap<String, Type>,
    ) -> Result<(), SemanticError> {
        for (key, val) in entries {
            self.find_free_variables_in_expression(key, local_vars, free_vars)?;
            self.find_free_variables_in_expression(val, local_vars, free_vars)?;
        }
        Ok(())
    }

    fn handle_set_literal_expression(
        &self,
        elements: &[ExpressionNode],
        local_vars: &std::collections::HashSet<String>,
        free_vars: &mut std::collections::HashMap<String, Type>,
    ) -> Result<(), SemanticError> {
        for elem in elements {
            self.find_free_variables_in_expression(elem, local_vars, free_vars)?;
        }
        Ok(())
    }

    fn handle_tuple_literal_expression(
        &self,
        elements: &[ExpressionNode],
        local_vars: &std::collections::HashSet<String>,
        free_vars: &mut std::collections::HashMap<String, Type>,
    ) -> Result<(), SemanticError> {
        for elem in elements {
            self.find_free_variables_in_expression(elem, local_vars, free_vars)?;
        }
        Ok(())
    }

    fn handle_if_expression(
        &self,
        cond: &ExpressionNode,
        then_expr: &ExpressionNode,
        else_expr: &ExpressionNode,
        local_vars: &std::collections::HashSet<String>,
        free_vars: &mut std::collections::HashMap<String, Type>,
    ) -> Result<(), SemanticError> {
        self.find_free_variables_in_expression(cond, local_vars, free_vars)?;
        self.find_free_variables_in_expression(then_expr, local_vars, free_vars)?;
        self.find_free_variables_in_expression(else_expr, local_vars, free_vars)?;
        Ok(())
    }

    fn handle_lambda_expression(
        &self,
        params: &[Param],
        body: &[StatementNode],
        local_vars: &std::collections::HashSet<String>,
        free_vars: &mut std::collections::HashMap<String, Type>,
    ) -> Result<(), SemanticError> {
        let mut inner_local_vars = local_vars.clone();
        for param in params {
            inner_local_vars.insert(param.name.clone());
        }
        for stmt in body {
            self.find_free_variables_in_statement(stmt, &mut inner_local_vars, free_vars)?;
        }
        Ok(())
    }

    pub(super) fn check_not_modifying_constant(
        &mut self,
        expr: &ExpressionNode,
        op_span: &Span,
    ) -> Result<(), SemanticError> {
        if let ExpressionKind::Identifier(name) = &expr.kind {
            if let Some(symbol) = self.symbol_table.lookup(name)
                && symbol.kind == SymbolKind::Constant
            {
                return Err(SemanticError::with_help(
                    format!("Cannot modify constant '{}'", name),
                    *op_span,
                    "Constants cannot be modified after initialization",
                ));
            }
        } else if let ExpressionKind::FieldAccess {
            expr: obj_expr,
            field,
        } = &expr.kind
        {
            let obj_type = self.get_expression_type(obj_expr)?;
            if let Type::Named(class_name, _) = &obj_type
                && let Some(symbol) = self.symbol_table.lookup(class_name)
                && let Some((_, is_const)) = symbol.fields.get(field)
                && *is_const
            {
                return Err(SemanticError::with_help(
                    format!("Cannot modify const field '{}'", field),
                    *op_span,
                    "Const fields cannot be modified after initialization. Remove the 'const' modifier from the field declaration if mutation is needed.",
                ));
            }
        }
        Ok(())
    }
}
