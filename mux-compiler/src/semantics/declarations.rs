use super::{
    ClassFieldInfo, GenericBounds, MethodSig, ResolvedInterface, SemanticAnalyzer, SemanticError,
    Symbol, SymbolKind, Type, Unifier, format_type,
};
use crate::ast::{
    AstNode, EnumVariant, ExpressionKind, ExpressionNode, Field, FunctionNode, LiteralNode,
    Spanned, StatementKind, StatementNode, TraitBound, TraitRef, TypeKind, TypeNode, WhereClause,
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

    /// The (name, trait-bound-names) list for a declaration's type parameters,
    /// recorded on the symbol so a generic use with the wrong number of type
    /// arguments is caught by the arity check. Shared by class and enum
    /// collection.
    fn type_param_bounds(type_params: &[(String, Vec<TraitBound>)]) -> Vec<(String, Vec<String>)> {
        type_params
            .iter()
            .map(|(p, b)| (p.clone(), b.iter().map(|tb| tb.name.clone()).collect()))
            .collect()
    }

    /// Record `type_param_bounds` and register each parameter name as a type
    /// symbol so annotations that reference it resolve to a type variable. Used
    /// by class collection. Enums record their bounds without registering the
    /// names, so a parameter does not leak into the global namespace where an
    /// unrelated later annotation could resolve it.
    fn register_type_param_symbols(
        &mut self,
        type_params: &[(String, Vec<TraitBound>)],
        span: &Span,
    ) -> Vec<(String, Vec<String>)> {
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
        Self::type_param_bounds(type_params)
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
        let type_param_bounds = self.register_type_param_symbols(type_params, span);

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
        // Record the enum's type parameters so a generic enum used with the
        // wrong number of type arguments is caught by the arity check, which
        // reads the symbol's type_params (issue #289 review). Unlike classes the
        // parameter names are NOT registered as global type symbols: a variant
        // payload referencing one resolves leniently (as before), and this avoids
        // leaking the name into the global namespace where an unrelated later
        // annotation could pick it up.
        let type_param_bounds = Self::type_param_bounds(type_params);

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
            AstNode::Enum { variants, .. } => self.analyze_enum_where_clauses(variants),
            AstNode::Interface {
                type_params,
                methods,
                ..
            } => self.analyze_interface_where_clauses(type_params, methods),
            AstNode::Statement(stmt) => self.analyze_statement(stmt, files),
        }
    }

    /// Single comprehensive generic-arity pass (issue #303). Runs after
    /// declaration collection - so every class/enum/interface symbol and its
    /// `type_params` are registered - and walks EVERY `TypeNode` in the program:
    /// class fields, enum variant payloads, interface fields and method
    /// signatures, function/method parameter and return types, and the type
    /// annotations inside bodies (local declarations, `for` variable types, map
    /// literals, lambdas, and generic type instantiations). Each is checked by
    /// `validate_type_arity`, which reports only arity (never resolving a name),
    /// so it is order-independent and never false-positives on a forward
    /// reference or a type parameter. Replaces the per-position checks that
    /// #289's review kept having to extend one hole at a time.
    ///
    /// Overlap with the arity check in `resolve_named_type` is harmless: the two
    /// produce the same message and span, which `analyze` deduplicates.
    pub(super) fn validate_all_type_arities(&mut self, nodes: &[AstNode]) {
        for node in nodes {
            match node {
                AstNode::Class {
                    type_params,
                    traits,
                    fields,
                    methods,
                    where_clause,
                    ..
                } => self.arity_check_class(
                    type_params,
                    traits,
                    fields,
                    methods,
                    where_clause.as_ref(),
                ),
                AstNode::Enum {
                    type_params,
                    variants,
                    ..
                } => self.arity_check_enum_decl(type_params, variants),
                AstNode::Interface {
                    type_params,
                    fields,
                    methods,
                    ..
                } => self.arity_check_interface(type_params, fields, methods),
                AstNode::Function(func) => self.arity_check_function(func, &[]),
                AstNode::Statement(stmt) => self.arity_check_statement(stmt, &[]),
            }
        }
    }

    fn arity_check_class(
        &mut self,
        type_params: &[(String, Vec<TraitBound>)],
        traits: &[TraitRef],
        fields: &[Field],
        methods: &[FunctionNode],
        where_clause: Option<&WhereClause>,
    ) {
        self.arity_check_type_param_bounds(type_params, type_params);
        self.arity_check_trait_refs(traits, type_params);
        self.arity_check_where(where_clause, type_params);
        for field in fields {
            self.arity_check_field(field, type_params);
        }
        for method in methods {
            self.arity_check_function(method, type_params);
        }
    }

    fn arity_check_enum_decl(
        &mut self,
        type_params: &[(String, Vec<TraitBound>)],
        variants: &[EnumVariant],
    ) {
        self.arity_check_type_param_bounds(type_params, type_params);
        for variant in variants {
            for (_, type_node) in variant.data.iter().flatten() {
                self.arity_check_type(type_node, type_params);
            }
            self.arity_check_where(variant.where_clause.as_ref(), type_params);
        }
    }

    fn arity_check_interface(
        &mut self,
        type_params: &[(String, Vec<TraitBound>)],
        fields: &[Field],
        methods: &[FunctionNode],
    ) {
        self.arity_check_type_param_bounds(type_params, type_params);
        for field in fields {
            self.arity_check_field(field, type_params);
        }
        for method in methods {
            // Interface methods declare a signature (and where clause) but no body.
            self.arity_check_signature(method, type_params);
        }
    }

    /// Arity-check `type_node` against the enclosing declaration's type
    /// parameters, recording any mismatch.
    fn arity_check_type(&mut self, type_node: &TypeNode, params: &[(String, Vec<TraitBound>)]) {
        if let Err(e) = self.validate_type_arity(type_node, params) {
            self.errors.push(e);
        }
    }

    /// Arity-check each type parameter's trait bounds - both the bound trait's
    /// own arity (a generic interface `T is Wrapper<int>`) and its type
    /// arguments. `params` are the type parameters in scope, which include these
    /// same parameters, so a bound referencing a sibling parameter is not
    /// mistaken for a generic.
    fn arity_check_type_param_bounds(
        &mut self,
        type_params: &[(String, Vec<TraitBound>)],
        params: &[(String, Vec<TraitBound>)],
    ) {
        for (_, bounds) in type_params {
            for bound in bounds {
                self.arity_check_named_ref(&bound.name, &bound.type_params, bound.span, params);
            }
        }
    }

    /// Arity-check implemented-interface references (`class C is Foo<int>`),
    /// including the interface's own arity.
    fn arity_check_trait_refs(
        &mut self,
        traits: &[TraitRef],
        params: &[(String, Vec<TraitBound>)],
    ) {
        for trait_ref in traits {
            self.arity_check_named_ref(
                &trait_ref.name,
                &trait_ref.type_args,
                trait_ref.span,
                params,
            );
        }
    }

    /// Arity-check a named type reference (`name<args...>`) - a trait bound or
    /// implemented-interface reference - by validating it as the named type it
    /// denotes. `span` is the reference's name span, so the diagnostic underlines
    /// the offending name rather than an argument or the whole declaration.
    fn arity_check_named_ref(
        &mut self,
        name: &str,
        args: &[TypeNode],
        span: Span,
        params: &[(String, Vec<TraitBound>)],
    ) {
        let type_node = TypeNode {
            kind: TypeKind::Named(name.to_string(), args.to_vec()),
            span,
        };
        self.arity_check_type(&type_node, params);
    }

    /// Arity-check the type annotations inside a where clause's predicate
    /// expressions (e.g. a cast or generic instantiation in a constraint).
    fn arity_check_where(
        &mut self,
        where_clause: Option<&WhereClause>,
        params: &[(String, Vec<TraitBound>)],
    ) {
        if let Some(where_clause) = where_clause {
            for predicate in &where_clause.predicates {
                self.arity_check_expr(predicate, params);
            }
        }
    }

    /// Arity-check a class or interface field: its declared type, its
    /// default-value expression (arbitrary since #287), and its where clause.
    fn arity_check_field(&mut self, field: &Field, params: &[(String, Vec<TraitBound>)]) {
        self.arity_check_type(&field.type_, params);
        if let Some(default) = &field.default_value {
            self.arity_check_expr(default, params);
        }
        self.arity_check_where(field.where_clause.as_ref(), params);
    }

    /// Arity-check a function/method signature: its own type-parameter bounds,
    /// parameter types and default-value expressions, return type, and where
    /// clause, in scope of the outer type parameters plus its own.
    fn arity_check_signature(
        &mut self,
        func: &FunctionNode,
        outer_params: &[(String, Vec<TraitBound>)],
    ) {
        let params = Self::extend_params(outer_params, &func.type_params);
        self.arity_check_type_param_bounds(&func.type_params, &params);
        for param in &func.params {
            self.arity_check_type(&param.type_, &params);
            if let Some(default) = &param.default_value {
                self.arity_check_expr(default, &params);
            }
        }
        self.arity_check_type(&func.return_type, &params);
        self.arity_check_where(func.where_clause.as_ref(), &params);
    }

    /// Arity-check a function/method's signature and its body.
    fn arity_check_function(
        &mut self,
        func: &FunctionNode,
        outer_params: &[(String, Vec<TraitBound>)],
    ) {
        self.arity_check_signature(func, outer_params);
        let params = Self::extend_params(outer_params, &func.type_params);
        for stmt in &func.body {
            self.arity_check_statement(stmt, &params);
        }
    }

    /// The type parameters in scope inside a function/method: the enclosing
    /// declaration's plus the function's own.
    fn extend_params(
        outer: &[(String, Vec<TraitBound>)],
        own: &[(String, Vec<TraitBound>)],
    ) -> Vec<(String, Vec<TraitBound>)> {
        outer.iter().cloned().chain(own.iter().cloned()).collect()
    }

    fn arity_check_statement(
        &mut self,
        stmt: &StatementNode,
        params: &[(String, Vec<TraitBound>)],
    ) {
        match &stmt.kind {
            StatementKind::AutoDecl(_, type_node, expr)
            | StatementKind::TypedDecl(_, type_node, expr)
            | StatementKind::ConstDecl(_, type_node, expr) => {
                self.arity_check_type(type_node, params);
                self.arity_check_expr(expr, params);
            }
            StatementKind::Function(func) => self.arity_check_function(func, params),
            StatementKind::For {
                var_type,
                iter,
                body,
                ..
            } => {
                self.arity_check_type(var_type, params);
                self.arity_check_expr(iter, params);
                self.arity_check_statements(body, params);
            }
            StatementKind::If {
                cond,
                then_block,
                else_block,
            } => {
                self.arity_check_expr(cond, params);
                self.arity_check_statements(then_block, params);
                if let Some(else_block) = else_block {
                    self.arity_check_statements(else_block, params);
                }
            }
            StatementKind::While { cond, body } => {
                self.arity_check_expr(cond, params);
                self.arity_check_statements(body, params);
            }
            StatementKind::Match { expr, arms } => {
                self.arity_check_expr(expr, params);
                for arm in arms {
                    self.arity_check_statements(&arm.body, params);
                }
            }
            StatementKind::Return(Some(expr)) | StatementKind::Expression(expr) => {
                self.arity_check_expr(expr, params);
            }
            StatementKind::Block(body) => self.arity_check_statements(body, params),
            StatementKind::Return(None)
            | StatementKind::Import { .. }
            | StatementKind::Break
            | StatementKind::Continue => {}
        }
    }

    fn arity_check_statements(
        &mut self,
        stmts: &[StatementNode],
        params: &[(String, Vec<TraitBound>)],
    ) {
        for stmt in stmts {
            self.arity_check_statement(stmt, params);
        }
    }

    fn arity_check_expr(&mut self, expr: &ExpressionNode, params: &[(String, Vec<TraitBound>)]) {
        match &expr.kind {
            ExpressionKind::GenericType(name, args) => {
                // A generic type instantiation in value position (e.g.
                // `Box<int>.new()`); validate it as the named type it denotes.
                let type_node = TypeNode {
                    kind: TypeKind::Named(name.clone(), args.clone()),
                    span: expr.span,
                };
                self.arity_check_type(&type_node, params);
            }
            ExpressionKind::MapLiteral {
                key_type,
                value_type,
                entries,
            } => {
                self.arity_check_type(key_type, params);
                self.arity_check_type(value_type, params);
                for (key, value) in entries {
                    self.arity_check_expr(key, params);
                    self.arity_check_expr(value, params);
                }
            }
            ExpressionKind::Lambda {
                params: lambda_params,
                return_type,
                body,
                where_clause,
            } => {
                for param in lambda_params {
                    self.arity_check_type(&param.type_, params);
                    if let Some(default) = &param.default_value {
                        self.arity_check_expr(default, params);
                    }
                }
                self.arity_check_type(return_type, params);
                self.arity_check_where(where_clause.as_ref(), params);
                self.arity_check_statements(body, params);
            }
            ExpressionKind::Binary { left, right, .. } => {
                self.arity_check_expr(left, params);
                self.arity_check_expr(right, params);
            }
            ExpressionKind::Unary { expr, .. } => self.arity_check_expr(expr, params),
            ExpressionKind::Call { func, args } => {
                self.arity_check_expr(func, params);
                for arg in args {
                    self.arity_check_expr(arg, params);
                }
            }
            ExpressionKind::FieldAccess { expr, .. } => self.arity_check_expr(expr, params),
            ExpressionKind::ListAccess { expr, index } => {
                self.arity_check_expr(expr, params);
                self.arity_check_expr(index, params);
            }
            ExpressionKind::ListLiteral(items)
            | ExpressionKind::SetLiteral(items)
            | ExpressionKind::TupleLiteral(items) => {
                for item in items {
                    self.arity_check_expr(item, params);
                }
            }
            ExpressionKind::If {
                cond,
                then_expr,
                else_expr,
            } => {
                self.arity_check_expr(cond, params);
                self.arity_check_expr(then_expr, params);
                self.arity_check_expr(else_expr, params);
            }
            ExpressionKind::Literal(_) | ExpressionKind::None | ExpressionKind::Identifier(_) => {}
        }
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

    /// Type-check each field's default value against the field's declared type.
    /// Defaults are arbitrary expressions evaluated per instance at construction
    /// (issue #287), typed here in the class's type-parameter scope. There is no
    /// `self` or sibling field in scope, so a default cannot reference instance
    /// state; such a reference surfaces as the usual unresolved-name error.
    fn check_field_default_types(&mut self, fields: &[Field]) {
        for field in fields {
            let Some(default_expr) = &field.default_value else {
                continue;
            };
            let Ok(field_type) = self.resolve_type(&field.type_) else {
                continue;
            };
            let default_type = match self.get_expression_type(default_expr) {
                Ok(t) => t,
                Err(e) => {
                    self.errors.push(e);
                    continue;
                }
            };
            let mut unifier = Unifier::new();
            if unifier
                .unify(&field_type, &default_type, default_expr.span)
                .is_err()
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

        // Type-check field default expressions against their declared types.
        self.check_field_default_types(fields);

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
