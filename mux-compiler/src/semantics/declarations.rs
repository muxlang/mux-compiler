use super::{
    ClassFieldInfo, GenericBounds, MethodSig, ResolvedInterface, SemanticAnalyzer, SemanticError,
    Symbol, SymbolKind, Type, Unifier, format_type,
};
use crate::ast::{
    AstNode, EnumVariant, ExpressionKind, ExpressionNode, Field, FunctionNode, LiteralNode,
    Spanned, StatementKind, StatementNode, TraitBound, TraitRef, TypeKind, TypeNode, WhereClause,
};
use crate::diagnostic::DiagnosticCode;
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
                DiagnosticCode::UnknownMember,
                "Common methods are only allowed in classes",
                func.span,
                "The 'common' modifier creates a static method. Move this function inside a class definition, or remove the 'common' keyword.",
            ));
        }
        // Name this declaration's type parameters while its signature is
        // resolved, so a bare `E` in `result<T, E>` is known to be one.
        self.signature_type_params = func
            .type_params
            .iter()
            .map(|(name, _)| name.clone())
            .collect();
        let signature = (|analyzer: &mut Self| {
            let param_types = func
                .params
                .iter()
                .map(|p| analyzer.resolve_type(&p.type_))
                .collect::<Result<Vec<_>, _>>()?;
            let return_type = analyzer.resolve_type(&func.return_type)?;
            Ok::<_, SemanticError>((param_types, return_type))
        })(self);
        self.signature_type_params.clear();
        let (param_types, return_type) = signature?;
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

    /// Reject a declaration that reuses a built-in type name.
    ///
    /// `optional` and `result` are types the compiler and runtime implement
    /// together: they are boxed heap values with their own construction,
    /// discriminant and payload calls, not inline structs. A user declaration
    /// under either name is accepted by the symbol table and then overwrites the
    /// built-in registration in codegen, so the program gets a type that looks
    /// like its own and behaves like neither (issue #369).
    ///
    /// Rejecting the name is consistent with `none` already being a keyword.
    fn reject_builtin_type_name(name: &str, kind: &str, span: &Span) -> Option<SemanticError> {
        if !matches!(name, "optional" | "result") {
            return None;
        }
        Some(SemanticError::with_help(
            DiagnosticCode::InvalidOperation,
            format!("Cannot declare {kind} named '{name}'"),
            *span,
            format!("'{name}' is a built-in type. Choose another name."),
        ))
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
        if let Some(e) = Self::reject_builtin_type_name(name, "a class", span) {
            return Err(e);
        }
        let implemented_interfaces =
            self.resolve_implemented_interfaces(name, type_params, traits, span)?;
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

    /// The methods a built-in capability requires of a class that declares
    /// `is Stringable` / `is Equatable` / `is Comparable` / `is Hashable`.
    ///
    /// `Hashable` requires equality as well as `hash`, the way Rust spells a
    /// map key as `Hash + Eq`: a hash alone cannot answer whether two keys in
    /// the same bucket are the same key. It is also what makes `Hashable`
    /// honestly satisfy an `Equatable` bound, which the bound system already
    /// promises it does.
    ///
    /// `None` for anything else, including a user-declared interface that
    /// happens to share the name - a real declaration always wins.
    fn builtin_interface_methods(
        &self,
        name: &str,
        implementor: &str,
        implementor_type_params: &[(String, Vec<TraitBound>)],
    ) -> Option<HashMap<String, MethodSig>> {
        if self.symbol_table.lookup(name).is_some() {
            return None;
        }
        let methods: &[&str] = match name {
            "Stringable" => &["to_string"],
            "Equatable" => &["eq"],
            "Comparable" => &["cmp"],
            "Hashable" => &["hash", "eq"],
            // Absent, a class implementing Error registered nothing, so
            // `result<int, MyErr>` rejected its own error type and only a
            // `string` error ever worked.
            "Error" => &["message"],
            _ => return None,
        };
        // The built-in signatures are written against `Self`, so `eq` and `cmp`
        // take the implementing type rather than a type literally named Self.
        // A generic class's own type includes its parameters: substituting the
        // bare name asked `class Ranked<T> is Comparable` for `cmp(Ranked)`,
        // which then failed as a generic type missing its type argument, so no
        // generic class could implement a built-in capability at all.
        let own_type = Type::Named(
            implementor.to_string(),
            implementor_type_params
                .iter()
                .map(|(param, _)| Type::Generic(param.clone()))
                .collect(),
        );
        let substitute = |t: &Type| match t {
            Type::Generic(n) if n == "Self" => own_type.clone(),
            other => other.clone(),
        };
        let mut required = HashMap::new();
        for method in methods {
            // `eq` belongs to Equatable, whichever capability asked for it.
            let owner = if *method == "eq" { "Equatable" } else { name };
            let sig = self.get_builtin_interface_method(owner, method)?;
            required.insert(
                (*method).to_string(),
                MethodSig {
                    params: sig.params.iter().map(&substitute).collect(),
                    return_type: substitute(&sig.return_type),
                    is_static: sig.is_static,
                },
            );
        }
        Some(required)
    }

    /// Reject `Equatable`, `Comparable` or `Hashable` on a generic class.
    ///
    /// Those three are handed to the runtime as callbacks on the class's object
    /// type, and a generic class registers one object type shared by every
    /// instantiation - one layout, one copy, one destructor. There is no
    /// per-instantiation type to hang `Ranked$int.cmp` on, and registering it
    /// against the shared type would hand `Ranked<string>` the `int` version.
    ///
    /// Without this the operators worked (they monomorphize) while every
    /// collection silently fell back to comparing addresses, so a generic
    /// `Hashable` key never found its own entry and `contains` answered false.
    ///
    /// `Stringable` is unaffected: it registers nothing and `to_string` is
    /// resolved statically like any other method.
    fn reject_runtime_capability_on_generic_class(
        interface: &str,
        class_name: &str,
        class_type_params: &[(String, Vec<TraitBound>)],
        span: Span,
    ) -> Option<SemanticError> {
        if class_type_params.is_empty()
            || !matches!(interface, "Equatable" | "Comparable" | "Hashable")
        {
            return None;
        }
        Some(SemanticError::with_help(
            DiagnosticCode::TypeMismatch,
            format!("Generic class '{class_name}' cannot implement '{interface}'"),
            span,
            format!(
                "'{interface}' is registered with the runtime per class, and a generic class shares one                  registration across every instantiation. Drop the type parameter, or compare                  through a method you call directly."
            ),
        ))
    }

    fn resolve_implemented_interfaces(
        &self,
        class_name: &str,
        class_type_params: &[(String, Vec<TraitBound>)],
        traits: &[TraitRef],
        _span: &Span,
    ) -> Result<HashMap<String, ResolvedInterface>, SemanticError> {
        let mut implemented_interfaces = std::collections::HashMap::new();
        for trait_ref in traits {
            // The built-in capabilities are not declared symbols - they are
            // answered structurally for primitives and collections - so a class
            // saying `is Comparable` registered nothing at all, and then failed
            // to satisfy a bound it had genuinely implemented.
            if let Some(error) = Self::reject_runtime_capability_on_generic_class(
                &trait_ref.name,
                class_name,
                class_type_params,
                trait_ref.span,
            ) {
                return Err(error);
            }
            if let Some(methods) =
                self.builtin_interface_methods(&trait_ref.name, class_name, class_type_params)
            {
                implemented_interfaces.insert(trait_ref.name.clone(), (Vec::new(), methods));
                continue;
            }
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
                    DiagnosticCode::DuplicateDeclaration,
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

        // Deserializers, synthesized for every class the way `new` is.
        //
        // The name states the shape it returns: `from_json` gives one object,
        // `list_from_json` gives a JSON array, `list_from_csv` gives the rows of
        // a table. There is deliberately no singular `from_csv` - a CSV document
        // IS a table, so a singular form would only work for a file with exactly
        // one row and would read as a promise the format cannot keep.
        //
        // Whether a class can actually be deserialized (every field a type with
        // a JSON representation) is reported by codegen against the specific
        // field, not by withholding the method - a missing method says nothing
        // about which field is the problem.
        let self_type = Type::Named(
            class_name.to_string(),
            type_params
                .iter()
                .map(|(p, _)| Type::Variable(p.clone()))
                .collect(),
        );
        let str_type = Type::Primitive(crate::ast::PrimitiveType::Str);
        for (name, ok_type) in [
            ("from_json", self_type.clone()),
            ("list_from_json", Type::List(Box::new(self_type.clone()))),
            ("list_from_csv", Type::List(Box::new(self_type.clone()))),
        ] {
            methods_map.insert(
                name.to_string(),
                MethodSig {
                    params: vec![str_type.clone()],
                    return_type: Type::Result(Box::new(ok_type), Box::new(str_type.clone())),
                    is_static: true,
                },
            );
        }

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
                DiagnosticCode::UnknownMember,
                    format!(
                        "Class '{class_name}' does not implement method '{method_name}' required by interface '{interface_name}'"
                    ),
                    span,
                    format!("Add a method '{method_name}' to class '{class_name}' with the signature required by interface '{interface_name}'"),
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
                        DiagnosticCode::UnknownMember,
                        format!(
                            "Class '{class_name}' is missing required field '{field_name}' from interface '{interface_name}'"
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
                DiagnosticCode::TypeMismatch,
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
                DiagnosticCode::UnknownMember,
                format!(
                    "Field '{field_name}' must be const in class '{class_name}' to implement interface '{interface_name}'"
                ),
                span,
                format!(
                    "Add the 'const' modifier to field '{field_name}' in class '{class_name}'"
                ),
            ));
        }
    }

    /// Require a name on every payload field.
    ///
    /// Both `Code(int)` and `Code(int value)` used to be legal, and most
    /// declarations skipped the name, so a reader could not tell what
    /// `Cons(int, IntList)` held (mux-context#39, issue #370).
    ///
    /// The reason is readability, and parity with function parameters, which are
    /// always named even though calls are positional. It is NOT about access:
    /// a payload is read through the pattern, which binds positionally, so
    /// `match s { Circ(radius) { .. } }` works either way. There is deliberately
    /// no field access on an enum payload - the field does not exist on the
    /// other variants - so naming is for the reader, not the compiler.
    fn reject_unnamed_payload_fields(&mut self, enum_name: &str, variant: &EnumVariant) {
        let Some(fields) = &variant.data else {
            return;
        };
        for (field_name, type_node) in fields {
            if field_name.is_some() {
                continue;
            }
            self.errors.push(SemanticError::with_help(
                DiagnosticCode::UnknownMember,
                format!(
                    "Payload field of '{}.{}' needs a name",
                    enum_name, variant.name
                ),
                type_node.span,
                match self.resolve_type(type_node) {
                    Ok(ty) => format!(
                        "Write it as '{} <name>'. Every payload field is named, so a reader can tell what a variant holds.",
                        format_type(&ty)
                    ),
                    Err(_) => "Give the field a name, so a reader can tell what the variant holds."
                        .to_string(),
                },
            ));
        }
    }

    /// Reject an enum that contains itself with different type arguments.
    ///
    /// Ordinary recursion is fine - the type argument stays the same, so one
    /// instantiation covers every level:
    ///
    /// ```text
    /// enum Tree<T> { Leaf, Node(T value, Tree<T> rest) }
    /// ```
    ///
    /// Growing recursion cannot be built, because each level needs a new type
    /// and the sequence never repeats:
    ///
    /// ```text
    /// enum Nest<T> { Leaf(T v), N(Nest<list<T>> inner) }
    /// Nest<int> -> Nest<list<int>> -> Nest<list<list<int>>> -> ...
    /// ```
    ///
    /// Rust rejects the same shape while expanding `Vec<Vec<Vec<...>>>`. Caught
    /// at the declaration rather than at a use, so the error names the line that
    /// is actually wrong and appears even if nobody instantiates the enum.
    fn reject_growing_self_reference(
        &mut self,
        enum_name: &str,
        type_params: &[(String, Vec<TraitBound>)],
        variant: &EnumVariant,
    ) {
        if type_params.is_empty() {
            return;
        }
        for (_, type_node) in variant.data.iter().flatten() {
            let TypeKind::Named(nested, nested_args) = &type_node.kind else {
                continue;
            };
            if nested != enum_name {
                continue;
            }
            // Self-reference is only safe when every argument is the enum's own
            // parameter, in the same position.
            let unchanged = nested_args.len() == type_params.len()
                && nested_args.iter().zip(type_params).all(|(arg, (param, _))| {
                    matches!(&arg.kind, TypeKind::Named(n, a) if n == param && a.is_empty())
                });
            if unchanged {
                continue;
            }
            self.errors.push(SemanticError::with_help(
                DiagnosticCode::InvalidTypeArguments,
                format!(
                    "Enum '{enum_name}' contains itself with different type arguments"
                ),
                type_node.span,
                format!(
                    "Each level would need a new type, so '{}' can never be built. Recursion is allowed when the argument stays the same, as in '{}<{}>'.",
                    enum_name,
                    enum_name,
                    type_params
                        .iter()
                        .map(|(p, _)| p.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
            return;
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
        if let Some(e) = Self::reject_builtin_type_name(name, "an enum", span) {
            self.errors.push(e);
            return;
        }
        let type_param_bounds = Self::type_param_bounds(type_params);

        let mut methods = std::collections::HashMap::new();
        let mut variant_names = Vec::new();
        for variant in variants {
            self.reject_unnamed_payload_fields(name, variant);
            self.reject_growing_self_reference(name, type_params, variant);
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
        if let Some(e) = Self::reject_builtin_type_name(name, "an interface", span) {
            self.errors.push(e);
            return;
        }
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
                        DiagnosticCode::TypeMismatch,
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
                        DiagnosticCode::UnknownMember,
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
                            DiagnosticCode::MissingReturn,
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
            StatementKind::UninitDecl(_, type_node) => {
                self.arity_check_type(type_node, params);
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
            ExpressionKind::Slice { expr, start, end } => {
                self.arity_check_expr(expr, params);
                for bound in [start, end].iter().copied().flatten() {
                    self.arity_check_expr(bound, params);
                }
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
        Err(SemanticError::with_help(
            DiagnosticCode::InvalidOperation,
            msg,
            func.span,
            help,
        ))
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
                    DiagnosticCode::TypeMismatch,
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
