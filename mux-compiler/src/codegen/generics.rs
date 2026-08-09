//! Generic type instantiation and specialization for the code generator.
//!
//! This module handles:
//! - Specialized method generation for generic classes
//! - Generic function instantiation and calls
//! - Type substitution in AST nodes

use std::collections::HashMap;

use super::ClassTypeParamBounds;
use super::CodeGenerator;
use super::GenericContext;
use crate::ast::{
    ExpressionKind, ExpressionNode, Param, PrimitiveType, StatementKind, StatementNode, TypeKind,
    TypeNode, WhereClause,
};
use crate::lexer::Span;
use crate::semantics::{Type, infer_missing_type_params_from_bounds};

impl<'a> CodeGenerator<'a> {
    /// Stamp out every concrete generic enum a type annotation mentions.
    ///
    /// Function signatures are declared before any body runs, so a parameter
    /// typed `Tree<int>` needs its instantiation to exist before the first
    /// `Tree<int>.Leaf` is ever generated (issue #359).
    /// `instantiate_generic_enums_in_type_node` for a resolved semantic type.
    ///
    /// A module-level global is declared from a `Type` rather than a
    /// `TypeNode` - `declare_auto_global` infers one from the initializer - so
    /// the TypeNode walker never sees it, and `auto b = Box<int>.Full(42)` at
    /// module scope resolved its LLVM type before anything stamped out
    /// `Box$int`. Inside a function the same line works.
    pub(super) fn instantiate_generic_enums_in_type(&mut self, ty: &Type) -> Result<(), String> {
        match ty {
            Type::Named(name, args) | Type::Instantiated(name, args) => {
                for arg in args {
                    self.instantiate_generic_enums_in_type(arg)?;
                }
                if self.enum_asts.contains_key(name) && self.mangled_enum_name(name, args) != *name
                {
                    self.ensure_enum_instantiated(name, args)?;
                }
            }
            Type::List(inner) | Type::Set(inner) | Type::Reference(inner) => {
                self.instantiate_generic_enums_in_type(inner)?;
            }
            Type::Optional(inner) => self.instantiate_generic_enums_in_type(inner)?,
            Type::Map(k, v) | Type::Tuple(k, v) | Type::Result(k, v) => {
                self.instantiate_generic_enums_in_type(k)?;
                self.instantiate_generic_enums_in_type(v)?;
            }
            Type::Function {
                params, returns, ..
            } => {
                for p in params {
                    self.instantiate_generic_enums_in_type(p)?;
                }
                self.instantiate_generic_enums_in_type(returns)?;
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn instantiate_generic_enums_in_type_node(
        &mut self,
        type_node: &TypeNode,
    ) -> Result<(), String> {
        match &type_node.kind {
            TypeKind::Named(name, args) => {
                for arg in args {
                    self.instantiate_generic_enums_in_type_node(arg)?;
                }
                if self.enum_asts.contains_key(name) {
                    let arg_types: Vec<Type> =
                        args.iter().map(|a| self.type_node_to_type(a)).collect();
                    if self.mangled_enum_name(name, &arg_types) != *name {
                        self.ensure_enum_instantiated(name, &arg_types)?;
                    }
                }
            }
            TypeKind::List(inner)
            | TypeKind::Set(inner)
            | TypeKind::Reference(inner)
            | TypeKind::TraitObject(inner) => {
                self.instantiate_generic_enums_in_type_node(inner)?;
            }
            TypeKind::Map(k, v) | TypeKind::Tuple(k, v) => {
                self.instantiate_generic_enums_in_type_node(k)?;
                self.instantiate_generic_enums_in_type_node(v)?;
            }
            TypeKind::Function { params, returns } => {
                for param in params {
                    self.instantiate_generic_enums_in_type_node(param)?;
                }
                self.instantiate_generic_enums_in_type_node(returns)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Reject a generic enum that embeds itself with *different* type
    /// arguments, which cannot be monomorphised.
    ///
    /// Ordinary recursion is fine, because the type argument stays the same and
    /// one instantiation covers every level:
    ///
    /// ```mux
    /// enum Tree<T> { Leaf, Node(T value, Tree<T> rest) }   // Tree<int> embeds Tree<int>
    /// ```
    ///
    /// Growing recursion is not, because each level needs a new instantiation
    /// and the sequence never repeats:
    ///
    /// ```mux
    /// enum Nest<T> { Leaf(T v), N(Nest<list<T>> inner) }
    /// // Nest<int> -> Nest<list<int>> -> Nest<list<list<int>>> -> ...
    /// ```
    ///
    /// Rust rejects the same shape, reporting an overflow while expanding
    /// `Vec<Vec<Vec<...>>>`. Caught here, before expanding, so the compiler says
    /// what is wrong instead of failing to find a type it never built.
    fn reject_growing_recursion(
        &self,
        enum_name: &str,
        key: &str,
        substituted: &[crate::ast::EnumVariant],
    ) -> Result<(), String> {
        for variant in substituted {
            for (_, type_node) in variant.data.iter().flatten() {
                let TypeKind::Named(nested, nested_args) = &type_node.kind else {
                    continue;
                };
                if nested != enum_name {
                    continue;
                }
                let nested_types: Vec<Type> = nested_args
                    .iter()
                    .map(|a| self.type_node_to_type(a))
                    .collect();
                let nested_key = self.mangled_enum_name(nested, &nested_types);
                if nested_key != key {
                    return Err(format!(
                        "enum '{}' cannot be built: variant '{}' contains '{}', so each level needs a new type. Recursion is allowed when the type argument stays the same, as in Tree<T> holding Tree<T>",
                        key, variant.name, nested_key
                    ));
                }
            }
        }
        Ok(())
    }

    /// The codegen name of a generic enum instantiation: `Box` with `[int]`
    /// becomes `Box$int`. A non-generic enum keeps its own name.
    ///
    /// Pure, so the many places that map a `Type` to an enum name can use it
    /// without needing to generate anything.
    pub(super) fn mangled_enum_name(&self, enum_name: &str, type_args: &[Type]) -> String {
        // Only a fully concrete instantiation has its own stamped-out type. The
        // enum's own declaration still refers to itself as `Tree<T>`, and that
        // must keep meaning `Tree` rather than a `Tree$T` that never exists.
        if type_args.is_empty() || !type_args.iter().all(|arg| self.is_concrete_type_arg(arg)) {
            return enum_name.to_string();
        }
        self.build_variant_key(enum_name, type_args)
    }

    /// Whether a type argument names a real type rather than a type parameter.
    ///
    /// A parameter reaches codegen as `Type::Named("T", [])` - indistinguishable
    /// from a class or enum called `T` by shape alone - so the symbol table is
    /// what separates them.
    fn is_concrete_type_arg(&self, arg: &Type) -> bool {
        match arg {
            Type::Generic(_) | Type::Variable(_) => false,
            Type::Named(name, args) => {
                let names_a_type =
                    self.analyzer
                        .symbol_table()
                        .lookup(name)
                        .is_some_and(|symbol| {
                            matches!(
                                symbol.kind,
                                crate::semantics::SymbolKind::Class
                                    | crate::semantics::SymbolKind::Enum
                                    | crate::semantics::SymbolKind::Interface
                            )
                        });
                names_a_type && args.iter().all(|a| self.is_concrete_type_arg(a))
            }
            Type::List(inner) | Type::Set(inner) | Type::Optional(inner) => {
                self.is_concrete_type_arg(inner)
            }
            Type::Map(k, v) | Type::Tuple(k, v) | Type::Result(k, v) => {
                self.is_concrete_type_arg(k) && self.is_concrete_type_arg(v)
            }
            _ => true,
        }
    }

    /// Stamp out a generic enum for one set of type arguments, if it does not
    /// exist yet, and return the instantiation's name.
    ///
    /// Enums are monomorphised rather than type-erased: `Box<int>` gets its own
    /// struct holding a real `i64`, its own constructors, and its own RC glue.
    /// Unlike a class, none of that needs the runtime - an enum is an inline
    /// tagged struct with compiler-generated glue and no object registration -
    /// so this is entirely a compile-time expansion (issue #359).
    pub(super) fn ensure_enum_instantiated(
        &mut self,
        enum_name: &str,
        type_args: &[Type],
    ) -> Result<String, String> {
        if type_args.is_empty() {
            return Ok(enum_name.to_string());
        }
        // Stamp out only what a reader will ask for. Every lookup goes through
        // `mangled_enum_name`, which keeps the bare name whenever an argument
        // is still a type parameter, so instantiating on the key alone built a
        // whole `Box$T` - type, constructors and glue - under a name nothing
        // would ever resolve to.
        let key = self.mangled_enum_name(enum_name, type_args);
        if key == enum_name {
            return Ok(key);
        }
        if self.type_map.contains_key(&key) {
            return Ok(key);
        }

        let Some(variants) = self.enum_asts.get(enum_name).cloned() else {
            // Not a generic enum we have an AST for; leave the name alone.
            return Ok(enum_name.to_string());
        };
        let param_names: Vec<String> = self
            .analyzer
            .symbol_table()
            .lookup(enum_name)
            .map(|symbol| {
                symbol
                    .type_params
                    .iter()
                    .map(|(name, _)| name.clone())
                    .collect()
            })
            .unwrap_or_default();
        if param_names.len() != type_args.len() {
            return Err(format!(
                "Enum {} takes {} type argument(s), got {}",
                enum_name,
                param_names.len(),
                type_args.len()
            ));
        }
        let substitution: std::collections::HashMap<String, Type> = param_names
            .into_iter()
            .zip(type_args.iter().cloned())
            .collect();

        let substituted: Vec<crate::ast::EnumVariant> = variants
            .iter()
            .map(|variant| crate::ast::EnumVariant {
                name: variant.name.clone(),
                data: variant.data.as_ref().map(|fields| {
                    fields
                        .iter()
                        .map(|(field_name, type_node)| {
                            (
                                field_name.clone(),
                                self.substitute_types_in_type_node(type_node, &substitution),
                            )
                        })
                        .collect()
                }),
                where_clause: variant.where_clause.clone(),
            })
            .collect();

        self.reject_growing_recursion(enum_name, &key, &substituted)?;

        // Registered before generating, so a variant that embeds the same
        // instantiation - `Tree<int>` inside `Tree<int>` - resolves to this one
        // rather than expanding again.
        self.enum_asts.insert(key.clone(), substituted.clone());
        // Generating constructors moves the builder into their bodies, and this
        // runs mid-expression from a construction or match site, so the caller's
        // insertion point has to be put back - the same reason
        // `generate_specialized_methods` saves it.
        let saved_block = self.builder.get_insert_block();
        let result = self
            .generate_enum_type(&key, &substituted)
            .and_then(|()| self.generate_enum_constructors(&key, &substituted))
            // An instantiation needs its own drop, deep-clone, retain and
            // compare glue, exactly as a declared enum does. Without it a
            // `Box<string>` in a collection fails with "deep-clone glue
            // missing", and comparing two of them reads the payload as the
            // wrong representation.
            .and_then(|()| self.generate_enum_object_support(&key));
        if let Some(block) = saved_block {
            self.builder.position_at_end(block);
        }
        result?;
        Ok(key)
    }

    fn build_variant_key(&self, class_name: &str, type_args: &[Type]) -> String {
        let variant_suffix = type_args
            .iter()
            .map(|t| self.sanitize_type_name(t))
            .collect::<Vec<_>>()
            .join("$");
        format!("{}${}", class_name, variant_suffix)
    }

    fn mark_variant_processing(&mut self, variant_key: &str) -> bool {
        if self.generated_methods.contains_key(variant_key) {
            return false;
        }
        self.generated_methods.insert(variant_key.to_string(), true);
        true
    }

    fn build_type_param_map_for_class(
        &self,
        class_symbol: &crate::semantics::Symbol,
        type_args: &[Type],
    ) -> Result<HashMap<String, Type>, String> {
        let mut type_param_map = HashMap::new();
        for (i, param) in class_symbol.type_params.iter().enumerate() {
            let resolved_type_arg = self.resolve_type(&type_args[i])?;
            type_param_map.insert(param.0.clone(), resolved_type_arg);
        }
        Ok(type_param_map)
    }

    fn set_class_type_param_bounds_if_needed(&mut self, class_symbol: &crate::semantics::Symbol) {
        if !class_symbol.type_params.is_empty() {
            let bounds: ClassTypeParamBounds = class_symbol
                .type_params
                .iter()
                .map(|(param, bound_names)| {
                    (
                        param.clone(),
                        bound_names
                            .iter()
                            .map(|bound_name| (bound_name.clone(), Vec::new()))
                            .collect(),
                    )
                })
                .collect();
            self.analyzer.set_class_type_params(bounds);
        }
    }

    fn handle_named_signature_type(
        &self,
        name: &str,
        type_args: &[TypeNode],
        param_type: &TypeNode,
        arg_type: &Type,
        type_map: &mut std::collections::HashMap<String, Type>,
    ) -> Result<(), String> {
        if type_args.is_empty() {
            return self
                .infer_or_validate_simple_named_signature(name, param_type, arg_type, type_map);
        }

        self.infer_named_type_args_signature(name, type_args, arg_type, type_map)
    }

    fn infer_or_validate_simple_named_signature(
        &self,
        name: &str,
        param_type: &TypeNode,
        arg_type: &Type,
        type_map: &mut std::collections::HashMap<String, Type>,
    ) -> Result<(), String> {
        if name.chars().next().unwrap_or(' ').is_uppercase() || name.len() <= 3 {
            if let Some(existing) = type_map.get(name) {
                if existing != arg_type {
                    return Err(format!(
                        "Type mismatch for generic parameter {}: expected {:?}, got {:?}",
                        name, existing, arg_type
                    ));
                }
            } else {
                type_map.insert(name.to_string(), arg_type.clone());
            }
            return Ok(());
        }

        let expected_concrete = self.type_node_to_type(param_type);
        if expected_concrete != *arg_type {
            return Err(format!(
                "Type mismatch: expected {:?}, got {:?}",
                expected_concrete, arg_type
            ));
        }

        Ok(())
    }

    fn infer_named_type_args_signature(
        &self,
        name: &str,
        type_args: &[TypeNode],
        arg_type: &Type,
        type_map: &mut std::collections::HashMap<String, Type>,
    ) -> Result<(), String> {
        match arg_type {
            Type::Named(arg_name, arg_type_args) => {
                if name != arg_name {
                    return Err(format!(
                        "Type name mismatch: expected {}, got {}",
                        name, arg_name
                    ));
                }
                if type_args.len() != arg_type_args.len() {
                    return Err(format!(
                        "Type argument count mismatch for {}: expected {}, got {}",
                        name,
                        type_args.len(),
                        arg_type_args.len()
                    ));
                }
                for (param_arg, arg_arg) in type_args.iter().zip(arg_type_args.iter()) {
                    self.infer_types_from_signature(param_arg, arg_arg, type_map)?;
                }
                Ok(())
            }
            _ => Err(format!("Expected named type with args, got {:?}", arg_type)),
        }
    }

    pub(super) fn lookup_class_symbol(&self, class_name: &str) -> Option<crate::semantics::Symbol> {
        if let Some(symbol) = self.analyzer.symbol_table().lookup(class_name) {
            return Some(symbol);
        }

        for module_symbols in self.analyzer.imported_symbols().values() {
            if let Some(symbol) = module_symbols.get(class_name) {
                return Some(symbol.clone());
            }
        }

        None
    }

    pub(super) fn generate_specialized_methods(
        &mut self,
        class_name: &str,
        type_args: &[Type],
    ) -> Result<(), String> {
        // save the current builder position so we can restore it after generating specialized methods
        let saved_insert_block = self.builder.get_insert_block();

        // check if we need to generate specialized methods for this variant
        let variant_key = self.build_variant_key(class_name, type_args);

        // skip if we've already generated methods for this variant
        if !self.mark_variant_processing(&variant_key) {
            return Ok(());
        }

        // get the class symbol to access methods
        let class_symbol = self
            .lookup_class_symbol(class_name)
            .ok_or(format!("Class {} not found", class_name))?;

        // Set class-level type parameter bounds for specialized method generation
        self.set_class_type_param_bounds_if_needed(&class_symbol);

        let type_param_map = self.build_type_param_map_for_class(&class_symbol, type_args)?;

        let mut specialized_methods = Vec::new();

        let method_prefix = format!("{}.", class_name);
        let original_methods: Vec<crate::ast::FunctionNode> = self
            .function_nodes
            .iter()
            .filter_map(|(name, node)| {
                if name.starts_with(&method_prefix) && !name.contains('$') {
                    Some(node.clone())
                } else {
                    None
                }
            })
            .collect();

        // Pass 1: prepare all specialized method AST nodes.
        for method_node in original_methods {
            let method_name = method_node
                .name
                .strip_prefix(&method_prefix)
                .unwrap_or(&method_node.name)
                .to_string();
            let specialized_method_name =
                self.create_specialized_method_name(class_name, type_args, &method_name);

            if self
                .generated_methods
                .contains_key(&specialized_method_name)
            {
                continue;
            }

            let mut specialized_method = method_node.clone();
            specialized_method.name = specialized_method_name;

            for param in &mut specialized_method.params {
                param.type_ = self.substitute_types_in_type_node(&param.type_, &type_param_map);
            }
            specialized_method.return_type = self
                .substitute_types_in_type_node(&specialized_method.return_type, &type_param_map);

            let mut substituted_body = Vec::new();
            for stmt in &specialized_method.body {
                substituted_body.push(self.substitute_types_in_statement(stmt, &type_param_map));
            }
            specialized_method.body = substituted_body;

            specialized_methods.push(specialized_method);
        }

        // set up generic context for specialized method generation
        let specialized_context = GenericContext {
            type_params: type_param_map,
        };
        let old_context = self.generic_context.take();
        self.generic_context = Some(specialized_context);

        // Save variables table because method generation clears self.variables.
        let saved_variables = self.variables.clone();

        // Pass 2: declare all specialized methods first so cross-method calls resolve.
        for method in &specialized_methods {
            self.declare_function(method)?;
        }

        // Pass 3: generate all specialized method bodies.
        for method in &specialized_methods {
            self.generate_function(method)?;
            self.generated_methods.insert(method.name.clone(), true);
        }

        // restore original context and variables
        self.generic_context = old_context;
        self.variables = saved_variables;

        // restore the builder position to where we were before generating specialized methods
        if let Some(block) = saved_insert_block {
            self.builder.position_at_end(block);
        }

        // Clear class-level type params after generating specialized methods
        self.analyzer.clear_class_type_params();

        Ok(())
    }

    pub(super) fn build_type_param_map(
        &self,
        class_name: &str,
        type_args: &[Type],
    ) -> Result<HashMap<String, Type>, String> {
        let mut type_params = HashMap::new();

        // get the class symbol to find generic parameter names
        if let Some(class_symbol) = self.lookup_class_symbol(class_name) {
            if class_symbol.type_params.len() == type_args.len() {
                for (i, param) in class_symbol.type_params.iter().enumerate() {
                    // param is (String, Vec<String>) - first element is the parameter name
                    let resolved_type_arg = self.resolve_type(&type_args[i])?;
                    type_params.insert(param.0.clone(), resolved_type_arg);
                }
            } else {
                return Err(format!(
                    "Type argument count mismatch for class {}",
                    class_name
                ));
            }
        } else {
            return Err(format!("Class {} not found", class_name));
        }

        Ok(type_params)
    }

    pub(super) fn generate_generic_function_call(
        &mut self,
        func_name: &str,
        args: &[ExpressionNode],
    ) -> Result<inkwell::values::BasicValueEnum<'a>, String> {
        // get the generic function AST node
        let func_node = self
            .function_nodes
            .get(func_name)
            .cloned()
            .ok_or(format!("Generic function {} not found", func_name))?;

        let concrete_types = self.infer_concrete_types_for_generic_function(&func_node, args)?;
        let instance_name =
            self.ensure_generic_function_instantiated(func_name, &concrete_types)?;

        // call the instantiated function
        let func = self
            .module
            .get_function(&instance_name)
            .ok_or(format!("Instantiated function {} not found", instance_name))?;

        let mut call_args = vec![];
        for arg in args {
            call_args.push(self.generate_expression(arg)?.into());
        }

        let call = self
            .builder
            .build_call(func, &call_args, "generic_func_call")
            .map_err(|e| e.to_string())?;

        match call.try_as_basic_value().basic() {
            Some(val) => Ok(val),
            None => Ok(self.context.i32_type().const_int(0, false).into()),
        }
    }

    fn instantiate_generic_function(
        &mut self,
        func_name: &str,
        concrete_types: &[Type],
        instance_name: &str,
    ) -> Result<(), String> {
        let func_node = self
            .function_nodes
            .get(func_name)
            .ok_or(format!("Generic function {} not found", func_name))?;

        // create type substitution map
        let mut type_map = std::collections::HashMap::new();
        for (i, type_param) in func_node.type_params.iter().enumerate() {
            if i < concrete_types.len() {
                type_map.insert(type_param.0.clone(), concrete_types[i].clone());
            }
        }

        // clone and substitute the function
        let mut substituted_func = func_node.clone();
        substituted_func.name = instance_name.to_string();
        substituted_func.type_params.clear(); // no longer generic

        // substitute types in parameters and return type
        for param in &mut substituted_func.params {
            param.type_ = self.substitute_types_in_type_node(&param.type_, &type_map);
        }
        substituted_func.return_type =
            self.substitute_types_in_type_node(&substituted_func.return_type, &type_map);

        // substitute types in the function body
        let mut substituted_body = Vec::new();
        for stmt in &substituted_func.body {
            substituted_body.push(self.substitute_types_in_statement(stmt, &type_map));
        }
        substituted_func.body = substituted_body;

        // substitute types in the where-clause predicates
        if let Some(clause) = &mut substituted_func.where_clause {
            clause.predicates = clause
                .predicates
                .iter()
                .map(|p| self.substitute_types_in_expression(p, &type_map))
                .collect();
        }

        // save current context (from calling context)
        let saved_variables = self.variables.clone();
        let saved_current_function_name = self.current_function_name.take();
        let saved_current_function_return_type = self.current_function_return_type.take();
        let saved_builder_position = self.builder.get_insert_block();

        // set up generic context for the instantiated function
        let context = GenericContext {
            type_params: type_map.clone(),
        };
        self.generic_context = Some(context);

        // declare the instantiated function
        self.declare_function(&substituted_func)?;

        // generate the instantiated function
        self.generate_function(&substituted_func)?;

        // clear generic context
        self.generic_context = None;

        // restore context (back to calling context)
        self.variables = saved_variables;
        self.current_function_name = saved_current_function_name;
        self.current_function_return_type = saved_current_function_return_type;
        if let Some(block) = saved_builder_position {
            self.builder.position_at_end(block);
        }

        Ok(())
    }

    pub(super) fn substitute_types_in_type_node(
        &self,
        type_node: &TypeNode,
        type_map: &std::collections::HashMap<String, Type>,
    ) -> TypeNode {
        match &type_node.kind {
            TypeKind::Named(name, args) => {
                if type_map.contains_key(name) {
                    // this is a generic type parameter - substitute it
                    let concrete_type = &type_map[name];
                    self.type_to_type_node(concrete_type)
                } else {
                    // not a generic parameter, substitute arguments recursively
                    let substituted_args = args
                        .iter()
                        .map(|arg| self.substitute_types_in_type_node(arg, type_map))
                        .collect();
                    TypeNode {
                        kind: TypeKind::Named(name.clone(), substituted_args),
                        span: type_node.span,
                    }
                }
            }
            TypeKind::List(inner) => TypeNode {
                kind: TypeKind::List(Box::new(
                    self.substitute_types_in_type_node(inner, type_map),
                )),
                span: Span::new(0, 0),
            },
            TypeKind::Function { params, returns } => {
                let substituted_params = params
                    .iter()
                    .map(|p| self.substitute_types_in_type_node(p, type_map))
                    .collect();
                let substituted_returns = self.substitute_types_in_type_node(returns, type_map);
                TypeNode {
                    kind: TypeKind::Function {
                        params: substituted_params,
                        returns: Box::new(substituted_returns),
                    },
                    span: Span::new(0, 0),
                }
            }
            // for other types, return as-is (they don't contain generic parameters)
            _ => type_node.clone(),
        }
    }

    pub(super) fn substitute_types_in_statement(
        &self,
        stmt: &StatementNode,
        type_map: &std::collections::HashMap<String, Type>,
    ) -> StatementNode {
        match &stmt.kind {
            StatementKind::TypedDecl(name, type_node, expr) => {
                let substituted_type = self.substitute_types_in_type_node(type_node, type_map);
                let substituted_expr = self.substitute_types_in_expression(expr, type_map);
                StatementNode {
                    kind: StatementKind::TypedDecl(
                        name.clone(),
                        substituted_type,
                        substituted_expr,
                    ),
                    span: stmt.span,
                }
            }
            StatementKind::AutoDecl(name, type_node, expr) => {
                let substituted_type = self.substitute_types_in_type_node(type_node, type_map);
                let substituted_expr = self.substitute_types_in_expression(expr, type_map);
                StatementNode {
                    kind: StatementKind::AutoDecl(name.clone(), substituted_type, substituted_expr),
                    span: stmt.span,
                }
            }
            StatementKind::For {
                var,
                var_type,
                iter,
                body,
            } => {
                let substituted_var_type = self.substitute_types_in_type_node(var_type, type_map);
                let substituted_iter = self.substitute_types_in_expression(iter, type_map);
                let substituted_body = body
                    .iter()
                    .map(|s| self.substitute_types_in_statement(s, type_map))
                    .collect();
                StatementNode {
                    kind: StatementKind::For {
                        var: var.clone(),
                        var_type: substituted_var_type,
                        iter: substituted_iter,
                        body: substituted_body,
                    },
                    span: stmt.span,
                }
            }
            StatementKind::Return(expr) => {
                let substituted_expr = expr
                    .as_ref()
                    .map(|e| self.substitute_types_in_expression(e, type_map));
                StatementNode {
                    kind: StatementKind::Return(substituted_expr),
                    span: stmt.span,
                }
            }
            StatementKind::Expression(expr) => {
                let substituted_expr = self.substitute_types_in_expression(expr, type_map);
                StatementNode {
                    kind: StatementKind::Expression(substituted_expr),
                    span: stmt.span,
                }
            }
            StatementKind::If {
                cond,
                then_block,
                else_block,
            } => {
                let substituted_cond = self.substitute_types_in_expression(cond, type_map);
                let substituted_then = then_block
                    .iter()
                    .map(|s| self.substitute_types_in_statement(s, type_map))
                    .collect();
                let substituted_else = else_block.as_ref().map(|b| {
                    b.iter()
                        .map(|s| self.substitute_types_in_statement(s, type_map))
                        .collect()
                });
                StatementNode {
                    kind: StatementKind::If {
                        cond: substituted_cond,
                        then_block: substituted_then,
                        else_block: substituted_else,
                    },
                    span: stmt.span,
                }
            }
            // for other statement types, return unchanged for now
            _ => stmt.clone(),
        }
    }

    pub(super) fn substitute_types_in_expression(
        &self,
        expr: &ExpressionNode,
        type_map: &std::collections::HashMap<String, Type>,
    ) -> ExpressionNode {
        match &expr.kind {
            ExpressionKind::GenericType(name, type_args) => {
                let substituted_args = type_args
                    .iter()
                    .map(|arg| self.substitute_types_in_type_node(arg, type_map))
                    .collect();
                ExpressionNode {
                    kind: ExpressionKind::GenericType(name.clone(), substituted_args),
                    span: expr.span,
                }
            }
            ExpressionKind::Call { func, args } => {
                let substituted_func = self.substitute_types_in_expression(func, type_map);
                let substituted_args = args
                    .iter()
                    .map(|a| self.substitute_types_in_expression(a, type_map))
                    .collect();
                ExpressionNode {
                    kind: ExpressionKind::Call {
                        func: Box::new(substituted_func),
                        args: substituted_args,
                    },
                    span: expr.span,
                }
            }
            ExpressionKind::FieldAccess {
                expr: inner_expr,
                field,
            } => {
                let substituted_expr = self.substitute_types_in_expression(inner_expr, type_map);
                ExpressionNode {
                    kind: ExpressionKind::FieldAccess {
                        expr: Box::new(substituted_expr),
                        field: field.clone(),
                    },
                    span: expr.span,
                }
            }
            ExpressionKind::Lambda {
                params,
                return_type,
                body,
                where_clause,
            } => {
                // for lambda parameters, substitute their types
                let substituted_params = params
                    .iter()
                    .map(|p| Param {
                        name: p.name.clone(),
                        type_: self.substitute_types_in_type_node(&p.type_, type_map),
                        default_value: p
                            .default_value
                            .as_ref()
                            .map(|dv| self.substitute_types_in_expression(dv, type_map)),
                    })
                    .collect();
                // substitute return type
                let substituted_return_type =
                    self.substitute_types_in_type_node(return_type, type_map);
                // for lambda body, substitute statements
                let substituted_body = body
                    .iter()
                    .map(|s| self.substitute_types_in_statement(s, type_map))
                    .collect();
                let substituted_where = where_clause.as_ref().map(|clause| WhereClause {
                    predicates: clause
                        .predicates
                        .iter()
                        .map(|p| self.substitute_types_in_expression(p, type_map))
                        .collect(),
                    span: clause.span,
                });
                ExpressionNode {
                    kind: ExpressionKind::Lambda {
                        params: substituted_params,
                        return_type: substituted_return_type,
                        body: substituted_body,
                        where_clause: substituted_where,
                    },
                    span: expr.span,
                }
            }
            // for other expression types, return unchanged for now
            _ => expr.clone(),
        }
    }

    /// recursively match a parameter type pattern against a concrete argument type to infer generic parameters
    pub(super) fn infer_types_from_signature(
        &self,
        param_type: &TypeNode,
        arg_type: &Type,
        type_map: &mut std::collections::HashMap<String, Type>,
    ) -> Result<(), String> {
        match &param_type.kind {
            TypeKind::Named(name, type_args) => {
                self.handle_named_signature_type(name, type_args, param_type, arg_type, type_map)?;
            }
            TypeKind::List(inner_param_type) => match arg_type {
                Type::List(inner_arg_type) => {
                    self.infer_types_from_signature(inner_param_type, inner_arg_type, type_map)?;
                }
                _ => return Err(format!("Expected list type, got {:?}", arg_type)),
            },
            // The other composites infer the same way, one component at a time.
            // Only `list` was handled, so `func lookup<K, V>(map<K, V> d, K k)`
            // could not be called at all - the parameter fell through to the
            // catch-all and reported an internal error.
            TypeKind::Set(inner_param_type) => match arg_type {
                Type::Set(inner_arg_type) => {
                    self.infer_types_from_signature(inner_param_type, inner_arg_type, type_map)?;
                }
                _ => return Err(format!("Expected set type, got {:?}", arg_type)),
            },
            TypeKind::Map(param_key, param_value) => match arg_type {
                Type::Map(arg_key, arg_value) => {
                    self.infer_types_from_signature(param_key, arg_key, type_map)?;
                    self.infer_types_from_signature(param_value, arg_value, type_map)?;
                }
                _ => return Err(format!("Expected map type, got {:?}", arg_type)),
            },
            TypeKind::Tuple(param_left, param_right) => match arg_type {
                Type::Tuple(arg_left, arg_right) => {
                    self.infer_types_from_signature(param_left, arg_left, type_map)?;
                    self.infer_types_from_signature(param_right, arg_right, type_map)?;
                }
                _ => return Err(format!("Expected tuple type, got {:?}", arg_type)),
            },
            TypeKind::Reference(inner_param_type) => match arg_type {
                Type::Reference(inner_arg_type) => {
                    self.infer_types_from_signature(inner_param_type, inner_arg_type, type_map)?;
                }
                _ => return Err(format!("Expected reference type, got {:?}", arg_type)),
            },
            TypeKind::Function {
                params: param_params,
                returns: param_returns,
            } => {
                match arg_type {
                    Type::Function {
                        params: arg_params,
                        returns: arg_returns,
                        ..
                    } => {
                        if param_params.len() != arg_params.len() {
                            return Err(format!(
                                "Function parameter count mismatch: expected {}, got {}",
                                param_params.len(),
                                arg_params.len()
                            ));
                        }
                        // match parameter types
                        for (param_param, arg_param) in param_params.iter().zip(arg_params.iter()) {
                            self.infer_types_from_signature(param_param, arg_param, type_map)?;
                        }
                        // match return type
                        self.infer_types_from_signature(param_returns, arg_returns, type_map)?;
                    }
                    _ => return Err(format!("Expected function type, got {:?}", arg_type)),
                }
            }
            TypeKind::Primitive(primitive) => {
                let expected = match primitive {
                    PrimitiveType::Int => Type::Primitive(PrimitiveType::Int),
                    PrimitiveType::Float => Type::Primitive(PrimitiveType::Float),
                    PrimitiveType::Bool => Type::Primitive(PrimitiveType::Bool),
                    PrimitiveType::Str => Type::Primitive(PrimitiveType::Str),
                    PrimitiveType::Char => Type::Primitive(PrimitiveType::Char),
                    _ => return Err(format!("Unsupported primitive type {:?}", primitive)),
                };
                if expected != *arg_type {
                    return Err(format!(
                        "Primitive type mismatch: expected {:?}, got {:?}",
                        expected, arg_type
                    ));
                }
            }
            _ => {
                return Err(format!(
                    "Unsupported type kind in signature matching: {:?}",
                    param_type.kind
                ));
            }
        }
        Ok(())
    }

    fn infer_concrete_types_for_generic_function(
        &mut self,
        func_node: &crate::ast::FunctionNode,
        args: &[ExpressionNode],
    ) -> Result<Vec<Type>, String> {
        let mut type_map = std::collections::HashMap::new();

        // for each parameter in the function signature, match against the corresponding argument
        for (param_idx, param) in func_node.params.iter().enumerate() {
            if param_idx >= args.len() {
                break;
            }

            let arg_type = self
                .resolve_expression_type_with_fallback(&args[param_idx])
                .map_err(|e| format!("Failed to get argument type: {}", e))?;

            // recursively match the parameter type against the argument type to infer generic parameters
            self.infer_types_from_signature(&param.type_, &arg_type, &mut type_map)?;
        }
        // Infer missing type params from already-inferred params' bounds
        infer_missing_type_params_from_bounds(&func_node.type_params, &mut type_map);

        // convert to concrete types list in the order of type parameters
        let mut concrete_types = Vec::new();
        for (type_param_name, _) in &func_node.type_params {
            if let Some(concrete_type) = type_map.get(type_param_name) {
                concrete_types.push(concrete_type.clone());
                continue;
            }

            // Bound-driven inference (e.g. `E = list<int>` giving `T = int` via
            // `E is Collection<T>`) already ran above via
            // `infer_missing_type_params_from_bounds`. The only remaining source
            // is an enclosing generic instantiation's own type parameters.
            if let Some(concrete_type) = self
                .generic_context
                .as_ref()
                .and_then(|context| context.type_params.get(type_param_name))
            {
                concrete_types.push(concrete_type.clone());
                continue;
            }
            return Err(format!(
                "Could not infer concrete type for generic parameter {} in function {}",
                type_param_name, func_node.name
            ));
        }
        Ok(concrete_types)
    }

    fn ensure_generic_function_instantiated(
        &mut self,
        func_name: &str,
        concrete_types: &[Type],
    ) -> Result<String, String> {
        // `$` separates a generic instantiation from its type arguments, the
        // same as `Box$int` for an enum and `Pair$int.method` for a class. It
        // matters that a user cannot spell it: joining with `_` put
        // `identity$$int` in the same namespace as a global someone wrote as
        // `identity_int`, and `max<int>` next to a variable named `max_int`
        // took the symbol out from under the instantiation.
        let type_names: Vec<String> = concrete_types
            .iter()
            .map(|t| self.type_to_string(t))
            .collect();
        let instance_name = format!("{}$${}", func_name, type_names.join("$"));

        // check if already instantiated
        if self.module.get_function(&instance_name).is_none() {
            // instantiate the generic function
            self.instantiate_generic_function(func_name, concrete_types, &instance_name)?;
        }
        Ok(instance_name)
    }
}
