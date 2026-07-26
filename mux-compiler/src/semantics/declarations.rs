use super::{
    ClassFieldInfo, GenericBounds, MethodSig, ResolvedInterface, SemanticAnalyzer, SemanticError,
    Symbol, SymbolKind, Type, format_type,
};
use crate::ast::{
    AstNode, EnumVariant, ExpressionKind, ExpressionNode, Field, FunctionNode, LiteralNode,
    Spanned, StatementKind, StatementNode, TraitBound, TraitRef,
};
use crate::diagnostic::Files;
use crate::lexer::Span;
use std::collections::HashMap;

impl SemanticAnalyzer {
    // first pass, collect hoistable declarations like functions and classes.
    //
    // Import statements are also resolved here, in source order, rather than
    // waiting for the second pass (analyze_nodes). A class's `is Interface`
    // clause is resolved during this same hoisting pass (via
    // resolve_implemented_interfaces), so an interface imported from another
    // module must already be in scope by the time hoisting reaches the class
    // that implements it; otherwise the lookup silently finds nothing and the
    // class ends up with no recorded interfaces. Import statements are
    // processed again in the second pass too, but that is harmless: imported
    // symbol/module registration is already idempotent (see
    // add_import_symbol_if_absent).
    pub(super) fn collect_hoistable_declarations(
        &mut self,
        ast: &[AstNode],
        mut files: Option<&mut Files>,
    ) -> Result<(), SemanticError> {
        for node in ast {
            match node {
                AstNode::Function(func) => {
                    self.collect_function_symbol(func)?;
                }
                AstNode::Class {
                    name,
                    traits,
                    fields,
                    methods,
                    type_params,
                    ..
                } => {
                    self.collect_class_symbol(
                        name,
                        traits,
                        fields,
                        methods,
                        type_params,
                        node.span(),
                    )?;
                }
                AstNode::Enum {
                    name,
                    type_params,
                    variants,
                    ..
                } => {
                    self.collect_enum_symbol(name, type_params, variants, node.span());
                }
                AstNode::Interface {
                    name,
                    type_params,
                    fields,
                    methods,
                    ..
                } => {
                    self.collect_interface_symbol(name, type_params, fields, methods, node.span());
                }
                AstNode::Statement(stmt) => {
                    if let StatementKind::Import { module_path, spec } = &stmt.kind {
                        self.analyze_import_statement(
                            module_path,
                            spec,
                            stmt.span,
                            files.as_deref_mut(),
                        )?;
                        self.hoisted_import_spans.insert(stmt.span);
                    }
                }
            }
        }
        Ok(())
    }

    fn collect_function_symbol(&mut self, func: &FunctionNode) -> Result<(), SemanticError> {
        if func.is_common {
            return Err(SemanticError::with_help(
                "Common methods are only allowed in classes",
                func.span,
                "The 'common' modifier creates a static method. Move this function inside a class definition, or remove the 'common' keyword.",
            ));
        }
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

        if let Err(e) = self.symbol_table.add_symbol(
            &func.name,
            self.make_function_symbol(func.span, func_type, &func.type_params, default_count),
        ) {
            self.errors.push(e);
        }
        Ok(())
    }

    fn collect_class_symbol(
        &mut self,
        name: &str,
        traits: &[TraitRef],
        fields: &[Field],
        methods: &[FunctionNode],
        type_params: &[(String, Vec<TraitBound>)],
        span: &Span,
    ) -> Result<(), SemanticError> {
        let implemented_interfaces = self.resolve_implemented_interfaces(traits, span)?;
        let type_param_bounds: Vec<(String, Vec<String>)> = type_params
            .iter()
            .map(|(p, b)| (p.clone(), b.iter().map(|tb| tb.name.clone()).collect()))
            .collect();

        for (param_name, _) in type_params {
            let _ = self.symbol_table.add_symbol(
                param_name,
                Self::make_symbol(
                    SymbolKind::Type,
                    *span,
                    Some(Type::Generic(param_name.clone())),
                ),
            );
        }

        let (fields_map, _) = self.collect_class_fields(name, fields, span)?;
        let methods_map = self.collect_class_methods(methods, name, type_params)?;

        self.validate_interface_implementations(
            name,
            &implemented_interfaces,
            &fields_map,
            &methods_map,
            span,
        );

        if let Err(e) = self.symbol_table.add_symbol(
            name,
            Symbol {
                kind: SymbolKind::Class,
                span: *span,
                type_: Some(Type::Named(name.to_string(), vec![])),
                interfaces: implemented_interfaces,
                methods: methods_map,
                fields: fields_map,
                type_params: type_param_bounds,
                original_name: None,
                llvm_name: None,
                default_param_count: 0,
                variants: None,
            },
        ) {
            self.errors.push(e);
        }
        Ok(())
    }

    fn resolve_implemented_interfaces(
        &self,
        traits: &[TraitRef],
        _span: &Span,
    ) -> Result<HashMap<String, ResolvedInterface>, SemanticError> {
        let mut implemented_interfaces = std::collections::HashMap::new();
        for trait_ref in traits {
            if let Some(interface_symbol) = self.symbol_table.lookup(&trait_ref.name)
                && let Some((_, interface_methods)) =
                    interface_symbol.interfaces.get(&trait_ref.name)
            {
                let resolved_args = trait_ref
                    .type_args
                    .iter()
                    .map(|arg| self.resolve_type(arg))
                    .collect::<Result<Vec<_>, _>>()?;
                let interface_type_params = &interface_symbol.type_params;
                let mut substituted_methods = std::collections::HashMap::new();
                for (method_name, method_sig) in interface_methods {
                    let sub_params = method_sig
                        .params
                        .iter()
                        .map(|p| {
                            self.substitute_type_params(p, interface_type_params, &resolved_args)
                        })
                        .collect();
                    let sub_return = self.substitute_type_params(
                        &method_sig.return_type,
                        interface_type_params,
                        &resolved_args,
                    );
                    substituted_methods.insert(
                        method_name.clone(),
                        MethodSig {
                            params: sub_params,
                            return_type: sub_return,
                            is_static: method_sig.is_static,
                        },
                    );
                }
                implemented_interfaces
                    .insert(trait_ref.name.clone(), (resolved_args, substituted_methods));
            }
        }
        Ok(implemented_interfaces)
    }

    fn collect_class_fields(
        &mut self,
        name: &str,
        fields: &[Field],
        _span: &Span,
    ) -> Result<(HashMap<String, ClassFieldInfo>, Vec<Type>), SemanticError> {
        let mut field_types = Vec::new();
        let mut fields_map = std::collections::HashMap::new();
        for field in fields {
            if fields_map.contains_key(&field.name) {
                return Err(SemanticError::with_help(
                    format!("Duplicate field '{}' in class '{}'", field.name, name),
                    field.type_.span,
                    "Each field name must be unique within a class. Rename or remove the duplicate field.",
                ));
            }
            match self.resolve_type(&field.type_) {
                Ok(t) => {
                    field_types.push(t.clone());
                    fields_map.insert(field.name.clone(), (t, field.is_const));
                }
                Err(e) => self.errors.push(e),
            }
        }
        Ok((fields_map, field_types))
    }

    fn collect_class_methods(
        &mut self,
        methods: &[FunctionNode],
        class_name: &str,
        type_params: &[(String, Vec<TraitBound>)],
    ) -> Result<HashMap<String, MethodSig>, SemanticError> {
        let mut methods_map = HashMap::new();
        for method in methods {
            let mut param_types = Vec::new();
            for p in &method.params {
                match self.resolve_type(&p.type_) {
                    Ok(t) => param_types.push(t),
                    Err(e) => self.errors.push(e),
                }
            }
            let ret = match self.resolve_type(&method.return_type) {
                Ok(t) => t,
                Err(e) => {
                    self.errors.push(e);
                    continue;
                }
            };
            let method_sig = MethodSig {
                params: param_types,
                return_type: ret,
                is_static: method.is_common,
            };
            methods_map.insert(method.name.clone(), method_sig);
        }

        let type_args: Vec<Type> = type_params
            .iter()
            .map(|(p, _)| Type::Variable(p.clone()))
            .collect();
        let new_sig = MethodSig {
            params: vec![],
            return_type: Type::Named(class_name.to_string(), type_args),
            is_static: true,
        };
        methods_map.insert("new".to_string(), new_sig);
        Ok(methods_map)
    }

    fn validate_interface_implementations(
        &mut self,
        class_name: &str,
        implemented_interfaces: &HashMap<String, ResolvedInterface>,
        fields_map: &HashMap<String, ClassFieldInfo>,
        methods_map: &HashMap<String, MethodSig>,
        span: &Span,
    ) {
        for (interface_name, (_, interface_methods)) in implemented_interfaces {
            self.validate_interface_methods(
                class_name,
                interface_name,
                interface_methods,
                methods_map,
                *span,
            );
            self.validate_interface_fields(class_name, interface_name, fields_map, *span);
        }
    }

    fn validate_interface_methods(
        &mut self,
        class_name: &str,
        interface_name: &str,
        interface_methods: &HashMap<String, MethodSig>,
        methods_map: &HashMap<String, MethodSig>,
        span: Span,
    ) {
        for (method_name, interface_sig) in interface_methods {
            if let Some(class_sig) = methods_map.get(method_name) {
                if let Err(e) = self.check_method_compatibility(interface_sig, class_sig, span) {
                    self.errors.push(e);
                }
            } else {
                self.errors.push(SemanticError::with_help(
                    format!(
                        "Class '{}' does not implement method '{}' required by interface '{}'",
                        class_name, method_name, interface_name
                    ),
                    span,
                    format!("Add a method '{}' to class '{}' with the signature required by interface '{}'", method_name, class_name, interface_name),
                ));
            }
        }
    }

    fn validate_interface_fields(
        &mut self,
        class_name: &str,
        interface_name: &str,
        fields_map: &HashMap<String, ClassFieldInfo>,
        span: Span,
    ) {
        if let Some(interface_symbol) = self.symbol_table.lookup(interface_name) {
            for (field_name, (interface_field_type, interface_is_const)) in &interface_symbol.fields
            {
                if let Some((class_field_type, class_is_const)) = fields_map.get(field_name) {
                    self.validate_field_type(
                        class_name,
                        interface_name,
                        field_name,
                        interface_field_type,
                        class_field_type,
                        span,
                    );
                    self.validate_field_const(
                        class_name,
                        interface_name,
                        field_name,
                        *interface_is_const,
                        *class_is_const,
                        span,
                    );
                } else {
                    self.errors.push(SemanticError::with_help(
                        format!(
                            "Class '{}' is missing required field '{}' from interface '{}'",
                            class_name, field_name, interface_name
                        ),
                        span,
                        format!(
                            "Add field '{}: {}' to class '{}'",
                            field_name,
                            format_type(interface_field_type),
                            class_name
                        ),
                    ));
                }
            }
        }
    }

    fn validate_field_type(
        &mut self,
        class_name: &str,
        interface_name: &str,
        field_name: &str,
        interface_field_type: &Type,
        class_field_type: &Type,
        span: Span,
    ) {
        if !self.types_compatible(class_field_type, interface_field_type) {
            self.errors.push(SemanticError::with_help(
                format!(
                    "Field '{}' type mismatch in class '{}': class has {}, interface '{}' requires {}",
                    field_name,
                    class_name,
                    format_type(class_field_type),
                    interface_name,
                    format_type(interface_field_type)
                ),
                span,
                format!("Change the type of field '{}' to {} to match interface '{}'", field_name, format_type(interface_field_type), interface_name),
            ));
        }
    }

    fn validate_field_const(
        &mut self,
        class_name: &str,
        interface_name: &str,
        field_name: &str,
        interface_is_const: bool,
        class_is_const: bool,
        span: Span,
    ) {
        if interface_is_const && !class_is_const {
            self.errors.push(SemanticError::with_help(
                format!(
                    "Field '{}' must be const in class '{}' to implement interface '{}'",
                    field_name, class_name, interface_name
                ),
                span,
                format!(
                    "Add the 'const' modifier to field '{}' in class '{}'",
                    field_name, class_name
                ),
            ));
        }
    }

    fn collect_enum_symbol(
        &mut self,
        name: &str,
        type_params: &[(String, Vec<TraitBound>)],
        variants: &[EnumVariant],
        span: &Span,
    ) {
        // Record the enum's type parameters like classes do, so a generic enum
        // used with the wrong number of type arguments is caught by the arity
        // check (which reads the symbol's type_params) rather than silently
        // accepted (issue #289 review). Also register each parameter name as a
        // type symbol so variant payloads that reference it resolve to a type
        // variable, matching collect_class_symbol.
        let type_param_bounds: Vec<(String, Vec<String>)> = type_params
            .iter()
            .map(|(p, b)| (p.clone(), b.iter().map(|tb| tb.name.clone()).collect()))
            .collect();
        for (param_name, _) in type_params {
            let _ = self.symbol_table.add_symbol(
                param_name,
                Self::make_symbol(
                    SymbolKind::Type,
                    *span,
                    Some(Type::Generic(param_name.clone())),
                ),
            );
        }

        let mut methods = std::collections::HashMap::new();
        let mut variant_names = Vec::new();
        for variant in variants {
            variant_names.push(variant.name.clone());
            let params = variant
                .data
                .clone()
                .unwrap_or_default()
                .into_iter()
                .map(|(_, t)| self.resolve_type(&t).unwrap_or(Type::Void))
                .collect();
            let return_type = Type::Named(name.to_string(), vec![]);
            methods.insert(
                variant.name.clone(),
                MethodSig {
                    params,
                    return_type,
                    is_static: true,
                },
            );
        }
        if let Err(e) = self.symbol_table.add_symbol(
            name,
            Symbol {
                kind: SymbolKind::Enum,
                span: *span,
                type_: Some(Type::Named(name.to_string(), vec![])),
                interfaces: std::collections::HashMap::new(),
                methods,
                fields: std::collections::HashMap::new(),
                type_params: type_param_bounds,
                original_name: None,
                llvm_name: None,
                default_param_count: 0,
                variants: Some(variant_names),
            },
        ) {
            self.errors.push(e);
        }
    }

    fn collect_interface_symbol(
        &mut self,
        name: &str,
        type_params: &[(String, Vec<TraitBound>)],
        fields: &[Field],
        methods: &[FunctionNode],
        span: &Span,
    ) {
        let mut interface_methods = std::collections::HashMap::new();
        for method in methods {
            let param_types = method
                .params
                .iter()
                .map(|p| self.resolve_type(&p.type_))
                .collect::<Result<Vec<_>, _>>();
            if let Ok(param_types) = param_types {
                let return_type = self.resolve_type(&method.return_type);
                if let Ok(return_type) = return_type {
                    let method_sig = MethodSig {
                        params: param_types,
                        return_type,
                        is_static: false,
                    };
                    interface_methods.insert(method.name.clone(), method_sig);
                }
            }
        }

        let mut interface_fields = std::collections::HashMap::new();
        for field in fields {
            if let Ok(field_type) = self.resolve_type(&field.type_) {
                if let Some(default_expr) = &field.default_value
                    && let Ok(default_type) = self.infer_literal_type(default_expr)
                    && !self.types_compatible(&default_type, &field_type)
                {
                    self.errors.push(SemanticError::with_help(
                        format!(
                            "Default value type mismatch for field '{}': expected {}, got {}",
                            field.name,
                            format_type(&field_type),
                            format_type(&default_type)
                        ),
                        default_expr.span,
                        format!(
                            "The default value must match the field's declared type of {}",
                            format_type(&field_type)
                        ),
                    ));
                }
                interface_fields.insert(field.name.clone(), (field_type, field.is_const));
            }
        }

        let mut interfaces_map = std::collections::HashMap::new();
        interfaces_map.insert(name.to_string(), (vec![], interface_methods));
        if let Err(e) = self.symbol_table.add_symbol(
            name,
            Symbol {
                kind: SymbolKind::Interface,
                span: *span,
                type_: None,
                interfaces: interfaces_map,
                methods: std::collections::HashMap::new(),
                fields: interface_fields,
                type_params: type_params
                    .iter()
                    .map(|(p, b)| (p.clone(), b.iter().map(|tb| tb.name.clone()).collect()))
                    .collect::<Vec<(String, Vec<String>)>>(),
                original_name: None,
                llvm_name: None,
                default_param_count: 0,
                variants: None,
            },
        ) {
            self.errors.push(e);
        }
    }

    // second pass, analyze all nodes with full symbol information.
    pub(super) fn analyze_nodes(&mut self, nodes: &[AstNode], mut files: Option<&mut Files>) {
        for node in nodes {
            if let Err(e) = self.analyze_node(node, files.as_deref_mut()) {
                self.errors.push(e);
            }
        }
    }

    fn analyze_node(
        &mut self,
        node: &AstNode,
        files: Option<&mut Files>,
    ) -> Result<(), SemanticError> {
        match node {
            AstNode::Function(func) => {
                if func.is_common {
                    return Err(SemanticError::with_help(
                        "Common methods are only allowed inside class definitions",
                        func.span,
                        "The 'common' modifier creates static methods on a class. Remove 'common' for standalone functions.",
                    ));
                }

                // Check that main() returns void
                if func.name == "main" {
                    let return_type = self.resolve_type(&func.return_type)?;
                    if !matches!(return_type, Type::Void) {
                        return Err(SemanticError::with_help(
                            format!(
                                "Function 'main' must return void, not '{}'",
                                format_type(&return_type)
                            ),
                            func.return_type.span,
                            "The entry point 'main' function must be declared as 'returns void'. Change the return type to void.",
                        ));
                    }
                }

                self.analyze_function(func, None)
            }
            AstNode::Class {
                name,
                fields,
                methods,
                type_params,
                traits,
                where_clause,
                ..
            } => {
                let type_param_bounds = self.resolve_type_param_bounds(type_params)?;
                self.analyze_class(
                    name,
                    fields,
                    methods,
                    &type_param_bounds,
                    traits,
                    where_clause.as_ref(),
                )
            }
            AstNode::Enum { variants, .. } => {
                self.validate_enum_variant_type_arities(variants);
                self.analyze_enum_where_clauses(variants)
            }
            AstNode::Interface {
                type_params,
                fields,
                methods,
                ..
            } => {
                self.validate_interface_member_type_arities(fields, methods);
                self.analyze_interface_where_clauses(type_params, methods)
            }
            AstNode::Statement(stmt) => self.analyze_statement(stmt, files),
        }
    }

    /// Arity-check every enum variant payload type in the second pass, where all
    /// type symbols are registered. Collection resolves these with errors
    /// swallowed (`unwrap_or`), so a bad generic arity in a payload - e.g.
    /// `V(list)` or a user generic with the wrong argument count - would
    /// otherwise be silently accepted (issue #289).
    fn validate_enum_variant_type_arities(&mut self, variants: &[EnumVariant]) {
        let mut errors = Vec::new();
        for variant in variants {
            for (_, type_node) in variant.data.iter().flatten() {
                if let Err(e) = self.validate_type_arity(type_node) {
                    errors.push(e);
                }
            }
        }
        self.errors.extend(errors);
    }

    /// Arity-check interface field and method-signature types in the second
    /// pass, for the same reason as enum payloads: collection swallows their
    /// resolution errors (issue #289).
    fn validate_interface_member_type_arities(
        &mut self,
        fields: &[Field],
        methods: &[FunctionNode],
    ) {
        let mut errors = Vec::new();
        for field in fields {
            if let Err(e) = self.validate_type_arity(&field.type_) {
                errors.push(e);
            }
        }
        for method in methods {
            for param in &method.params {
                if let Err(e) = self.validate_type_arity(&param.type_) {
                    errors.push(e);
                }
            }
            if let Err(e) = self.validate_type_arity(&method.return_type) {
                errors.push(e);
            }
        }
        self.errors.extend(errors);
    }

    pub(super) fn analyze_function(
        &mut self,
        func: &FunctionNode,
        self_type: Option<Type>,
    ) -> Result<(), SemanticError> {
        let was_static = self.is_in_static_method;
        let old_self_type = self.current_self_type.clone();
        let old_return_type = self.current_return_type.clone();
        self.is_in_static_method = func.is_common;
        self.current_self_type = self_type.clone();

        // Set return type context
        let return_type = self.resolve_type(&func.return_type)?;
        self.current_return_type = Some(return_type.clone());

        // set current bounds
        self.current_bounds.clear();
        for (param, bounds) in self.resolve_type_param_bounds(&func.type_params)? {
            self.current_bounds.insert(param, bounds);
        }

        // Also add class-level type params (for methods in generic classes)
        if let Some(class_type_params) = &self.current_class_type_params {
            for (param, bounds) in class_type_params {
                // Don't override function-level type params
                if !self.current_bounds.contains_key(param) {
                    self.current_bounds.insert(param.clone(), bounds.clone());
                }
            }
        }

        // create new scope for function parameters and body.
        self.symbol_table.push_scope()?;

        // add generic type parameters to symbol table
        for (param_name, _) in &func.type_params {
            self.symbol_table.add_symbol(
                param_name,
                Symbol {
                    kind: SymbolKind::Type,
                    span: func.span,
                    type_: Some(Type::Generic(param_name.clone())),
                    interfaces: std::collections::HashMap::new(),
                    methods: std::collections::HashMap::new(),
                    fields: std::collections::HashMap::new(),
                    type_params: Vec::new(),
                    original_name: None,
                    llvm_name: None,
                    default_param_count: 0,
                    variants: None,
                },
            )?;
        }

        // add self if provided
        if let Some(self_type) = self_type {
            self.symbol_table.add_symbol(
                "self",
                Self::make_symbol(SymbolKind::Variable, func.span, Some(self_type)),
            )?;
        }

        for param in &func.params {
            let param_type = self.resolve_type(&param.type_)?;
            self.symbol_table.add_symbol(
                &param.name,
                Self::make_symbol(SymbolKind::Variable, param.type_.span, Some(param_type)),
            )?;
        }

        // typecheck where-clause preconditions with the params in scope.
        if let Some(clause) = &func.where_clause {
            self.analyze_where_clause(clause);
        }

        // analyze function body with new scope.
        self.analyze_block(&func.body, None)?;

        // clean up function scope.
        self.symbol_table.pop_scope()?;
        self.current_bounds.clear();
        self.is_in_static_method = was_static;
        self.current_self_type = old_self_type;
        self.current_return_type = old_return_type;

        self.ensure_all_paths_return(func, &return_type)
    }

    /// Ensure every code path in the function body ends in a return, with a
    /// diagnostic tailored to whether the function returns void or a value.
    fn ensure_all_paths_return(
        &self,
        func: &FunctionNode,
        return_type: &Type,
    ) -> Result<(), SemanticError> {
        if !func.body.is_empty() && self.all_paths_return(&func.body) {
            return Ok(());
        }
        let (msg, help): (String, String) = if matches!(return_type, Type::Void) {
            (
                "Function must end with an explicit 'return' statement on all code paths"
                    .to_string(),
                "Add a 'return' statement at the end of every code path".to_string(),
            )
        } else {
            (
                format!(
                    "Function must return a value of type '{}' on all code paths",
                    format_type(return_type)
                ),
                "Add a return statement at the end of every branch (if/else, match, etc.)"
                    .to_string(),
            )
        };
        Err(SemanticError::with_help(msg, func.span, help))
    }

    #[allow(clippy::only_used_in_recursion)]
    /// Returns true if the given expression is a statically-known infinite loop
    /// condition, currently the literal `true`. This lets `while true { ... }`
    /// be recognized as always returning when its body always returns.
    fn is_infinite_loop_condition(cond: &ExpressionNode) -> bool {
        matches!(
            &cond.kind,
            ExpressionKind::Literal(LiteralNode::Boolean(true))
        )
    }

    #[allow(clippy::only_used_in_recursion)]
    pub(super) fn all_paths_return(&self, stmts: &[StatementNode]) -> bool {
        stmts.iter().any(|stmt| self.statement_returns(stmt))
    }

    /// Returns true if the given statement terminates this code path with a
    /// return. Used by `all_paths_return` to keep the recursive function
    /// under SonarQube's cognitive complexity threshold.
    #[allow(clippy::only_used_in_recursion)]
    fn statement_returns(&self, stmt: &StatementNode) -> bool {
        match &stmt.kind {
            StatementKind::Return(_) => true,
            StatementKind::If {
                then_block,
                else_block,
                ..
            } => {
                let else_returns = else_block
                    .as_ref()
                    .is_some_and(|b| self.all_paths_return(b));
                self.all_paths_return(then_block) && else_returns
            }
            StatementKind::Block(block_stmts) => self.all_paths_return(block_stmts),
            StatementKind::While { cond, body: _ } => Self::is_infinite_loop_condition(cond),
            StatementKind::Match { arms, .. } => {
                arms.iter().all(|arm| self.all_paths_return(&arm.body))
            }
            _ => false,
        }
    }

    fn analyze_class(
        &mut self,
        name: &str,
        fields: &[Field],
        methods: &[FunctionNode],
        type_params: &[(String, GenericBounds)],
        traits: &[TraitRef],
        where_clause: Option<&crate::ast::WhereClause>,
    ) -> Result<(), SemanticError> {
        // Methods were already added to the class symbol during first pass (collect_hoistable_declarations)
        // Here we just need to analyze method bodies with proper self type

        // Create self type for this class
        let self_type = if type_params.is_empty() {
            Type::Named(name.to_string(), vec![])
        } else {
            // For generic classes, use type variables for self
            Type::Named(
                name.to_string(),
                type_params
                    .iter()
                    .map(|(param_name, _)| Type::Variable(param_name.clone()))
                    .collect(),
            )
        };

        // Set current class type params for method analysis
        self.set_class_type_params(type_params.to_vec());

        // Typecheck field-level and class-level where clauses with the fields
        // in scope, and record the invariants and inherited interface
        // preconditions codegen enforces.
        self.analyze_class_where_clauses(name, fields, methods, traits, where_clause)?;

        // Analyze each method body with proper self type
        for method in methods {
            // Static methods (common) don't have self
            let method_self_type = if method.is_common {
                None
            } else {
                Some(self_type.clone())
            };

            self.analyze_function(method, method_self_type)?;
        }

        // Clear class type params after analyzing class methods
        self.clear_class_type_params();

        Ok(())
    }
}
