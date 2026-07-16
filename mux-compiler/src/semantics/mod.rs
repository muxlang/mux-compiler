// Module declarations

pub mod const_checks;
pub mod const_fold;
pub mod declarations;
pub mod error;
pub mod expressions;
pub mod format;
pub mod free_vars;
pub mod imports;
pub mod patterns;
pub mod statements;
pub mod std_registry;
pub mod stdlib;
pub mod symbol_table;
pub mod types;
pub mod unifier;
pub mod where_clause;

// Re-exports for public API
pub use error::SemanticError;
pub use format::{format_binary_op, format_type};
pub use symbol_table::SymbolTable;
pub use types::{BuiltInSig, GenericContext, MethodSig, Symbol, SymbolKind, Type};
pub use unifier::Unifier;

// Internal imports
use crate::ast::{
    AstNode, BinaryOp, ExpressionKind, ExpressionNode, Param, PrimitiveType, StatementKind,
    StatementNode, TraitBound, TypeKind, TypeNode, UnaryOp,
};
use crate::diagnostic::Files;
use crate::lexer::Span;
use crate::semantics::std_registry::std_module_registry;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

type GenericBound = (String, Vec<Type>);
type GenericBounds = Vec<GenericBound>;
type ResolvedInterface = (Vec<Type>, HashMap<String, MethodSig>);
type ClassFieldInfo = (Type, bool);

pub struct SemanticAnalyzer {
    pub(super) symbol_table: SymbolTable,
    current_bounds: std::collections::HashMap<String, GenericBounds>,
    errors: Vec<SemanticError>,
    is_in_static_method: bool,
    pub current_self_type: Option<Type>,
    pub module_resolver: Option<Rc<RefCell<crate::module_resolver::ModuleResolver>>>,
    pub imported_symbols:
        std::collections::HashMap<String, std::collections::HashMap<String, Symbol>>,
    pub all_module_asts: std::collections::HashMap<String, Vec<AstNode>>,
    pub module_dependencies: Vec<String>,
    pub(super) required_runtime_features: HashSet<String>,
    pub(super) current_file: Option<std::path::PathBuf>, // Track current file for relative imports
    pub lambda_captures: std::collections::HashMap<Span, Vec<(String, Type)>>, // Track captured variables for each lambda
    pub current_return_type: Option<Type>, // Track current function/lambda return type
    pub current_class_type_params: Option<Vec<(String, GenericBounds)>>, // Track class-level type params with bounds for method analysis
    expression_type_overrides: std::collections::HashMap<Span, Type>, // Override resolved types for expressions (e.g., {} resolved to EmptyMap in map context)
    fresh_type_var_counter: usize, // Generates globally-unique names so a callee's type variables never collide with the caller's
    // Import statements are resolved during the hoisting pass (so interfaces
    // they bring into scope are visible to classes hoisted later in the same
    // pass), then skipped during the second pass by span so they are not
    // processed twice (some import side effects, like registering a
    // submodule namespace symbol, are not safe to repeat).
    pub(super) hoisted_import_spans: HashSet<Span>,
    // Class name -> the where-clause invariants (field-level and class-level)
    // codegen enforces on field assignment.
    pub(super) class_invariants: HashMap<String, Vec<where_clause::ClassInvariant>>,
    // Interface name -> method name -> that method's where-clause
    // precondition, collected before the analysis pass.
    pub(super) interface_preconditions:
        HashMap<String, HashMap<String, where_clause::InheritedPrecondition>>,
    // Class name -> method name -> interface preconditions the class method
    // must enforce at entry (one entry per declaring interface).
    pub(super) inherited_preconditions:
        HashMap<String, HashMap<String, Vec<where_clause::InheritedPrecondition>>>,
    // (class name or "" for free functions, name) -> where preconditions,
    // collected before the analysis pass so call sites can prove violations
    // from literal arguments regardless of declaration order.
    pub(super) function_preconditions: HashMap<(String, String), const_checks::WherePreconditions>,
    // (enum name, variant name) -> where preconditions on the variant payload.
    pub(super) enum_variant_preconditions:
        HashMap<(String, String), const_checks::EnumVariantPreconditions>,
}

impl Default for SemanticAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl SemanticAnalyzer {
    // Helper function to sanitize module paths for use in identifiers
    pub(super) fn sanitize_module_path(module_path: &str) -> String {
        module_path.replace(['.', '/'], "_")
    }

    pub fn new() -> Self {
        let symbol_table = SymbolTable::new();
        Self {
            symbol_table,
            current_bounds: std::collections::HashMap::new(),
            errors: Vec::new(),
            is_in_static_method: false,
            current_self_type: None,
            module_resolver: None,
            imported_symbols: std::collections::HashMap::new(),
            all_module_asts: std::collections::HashMap::new(),
            module_dependencies: Vec::new(),
            required_runtime_features: HashSet::new(),
            current_file: None,
            lambda_captures: std::collections::HashMap::new(),
            current_return_type: None,
            current_class_type_params: None,
            expression_type_overrides: std::collections::HashMap::new(),
            fresh_type_var_counter: 0,
            hoisted_import_spans: HashSet::new(),
            class_invariants: HashMap::new(),
            interface_preconditions: HashMap::new(),
            inherited_preconditions: HashMap::new(),
            function_preconditions: HashMap::new(),
            enum_variant_preconditions: HashMap::new(),
        }
    }

    pub fn new_with_resolver(
        resolver: Rc<RefCell<crate::module_resolver::ModuleResolver>>,
    ) -> Self {
        Self {
            module_resolver: Some(resolver),
            ..Self::new()
        }
    }

    pub(super) fn make_symbol(kind: SymbolKind, span: Span, type_: Option<Type>) -> Symbol {
        Symbol {
            kind,
            span,
            type_,
            interfaces: std::collections::HashMap::new(),
            methods: std::collections::HashMap::new(),
            fields: std::collections::HashMap::new(),
            type_params: Vec::new(),
            original_name: None,
            llvm_name: None,
            default_param_count: 0,
            variants: None,
        }
    }

    pub(super) fn make_module_symbol(&self, module_name: &str, span: Span) -> Symbol {
        Self::make_symbol(
            SymbolKind::Import,
            span,
            Some(Type::Module(module_name.to_string())),
        )
    }

    fn make_function_symbol(
        &self,
        span: Span,
        function_type: Type,
        type_params: &[(String, Vec<crate::ast::TraitBound>)],
        default_param_count: usize,
    ) -> Symbol {
        Symbol {
            kind: SymbolKind::Function,
            span,
            type_: Some(function_type),
            interfaces: std::collections::HashMap::new(),
            methods: std::collections::HashMap::new(),
            fields: std::collections::HashMap::new(),
            type_params: type_params
                .iter()
                .map(|(name, bounds)| {
                    (
                        name.clone(),
                        bounds.iter().map(|bound| bound.name.clone()).collect(),
                    )
                })
                .collect(),
            original_name: None,
            llvm_name: None,
            default_param_count,
            variants: None,
        }
    }

    /// Map of stdlib parent module -> declared nested child modules.
    /// Keys are the short parent name (e.g. "net", "data"). Values are full
    /// child module paths (e.g. "net.http", "data.json"). This is used to
    /// eagerly inject nested stdlib modules when a parent stdlib module is
    /// imported (for example importing `std.net` also makes `net.http` usable).
    fn stdlib_nested_modules_map() -> std::collections::HashMap<String, Vec<String>> {
        let mut m = std::collections::HashMap::new();
        let registry = std_module_registry();
        for full_name in registry.keys() {
            if let Some(rest) = full_name.strip_prefix("std.")
                && let Some(pos) = rest.find('.')
            {
                let parent = &rest[..pos];
                let child = rest; // full child path like "data.json"
                m.entry(parent.to_string())
                    .or_insert_with(Vec::new)
                    .push(child.to_string());
            }
        }
        m
    }

    /// Inject nested stdlib children for a given parent stdlib module into
    /// `self.imported_symbols`. `parent_module` is the short name (e.g. "net").
    pub(super) fn inject_nested_stdlib_children(&mut self, parent_module: &str, span: Span) {
        let map = Self::stdlib_nested_modules_map();
        if let Some(children) = map.get(parent_module) {
            for child in children {
                // collect symbols for the child module (child is a full path like "net.http" or "data.json")
                let child_symbols = self.collect_stdlib_module_symbols(child, span);
                // store under the full child module path so module imports and
                // module-qualified accesses (Module("net.http")) resolve correctly
                self.imported_symbols
                    .insert(child.to_string(), child_symbols.clone());

                // Also expose the short child name (e.g. "json") for backward
                // compatibility so code referencing `json.parse` works after
                // importing the parent (e.g. `import std.data`). Don't overwrite
                // an existing user-provided namespace.
                if let Some(short_name) = child.split('.').next_back()
                    && !self.imported_symbols.contains_key(short_name)
                {
                    self.imported_symbols
                        .insert(short_name.to_string(), child_symbols);
                    // Register module symbol in symbol table so unqualified
                    // module references resolve (e.g., json.parse)
                    let _ = self
                        .symbol_table
                        .add_symbol(short_name, self.make_module_symbol(short_name, span));
                }
            }
        }
    }

    pub fn set_current_file(&mut self, file: std::path::PathBuf) {
        self.current_file = Some(file);
    }

    fn new_for_module(resolver: Rc<RefCell<crate::module_resolver::ModuleResolver>>) -> Self {
        let symbol_table = SymbolTable::new();
        Self {
            symbol_table,
            current_bounds: std::collections::HashMap::new(),
            errors: Vec::new(),
            is_in_static_method: false,
            current_self_type: None,
            module_resolver: Some(resolver),
            imported_symbols: std::collections::HashMap::new(),
            all_module_asts: std::collections::HashMap::new(),
            module_dependencies: Vec::new(),
            required_runtime_features: HashSet::new(),
            current_file: None,
            lambda_captures: std::collections::HashMap::new(),
            current_return_type: None,
            current_class_type_params: None,
            expression_type_overrides: std::collections::HashMap::new(),
            fresh_type_var_counter: 0,
            hoisted_import_spans: HashSet::new(),
            class_invariants: HashMap::new(),
            interface_preconditions: HashMap::new(),
            inherited_preconditions: HashMap::new(),
            function_preconditions: HashMap::new(),
            enum_variant_preconditions: HashMap::new(),
        }
    }

    pub fn symbol_table(&self) -> &SymbolTable {
        &self.symbol_table
    }

    pub fn all_symbols(&self) -> &std::collections::HashMap<String, Symbol> {
        &self.symbol_table.all_symbols
    }

    /// The where-clause invariants (field-level and class-level) declared on a
    /// class, or an empty slice when it has none.
    pub fn class_invariants(&self, class_name: &str) -> &[where_clause::ClassInvariant] {
        self.class_invariants
            .get(class_name)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// The interface preconditions a class method inherits from the
    /// interfaces its class implements, or an empty slice when none apply.
    pub fn inherited_preconditions(
        &self,
        class_name: &str,
        method_name: &str,
    ) -> &[where_clause::InheritedPrecondition] {
        self.inherited_preconditions
            .get(class_name)
            .and_then(|methods| methods.get(method_name))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn imported_symbols(
        &self,
    ) -> &std::collections::HashMap<String, std::collections::HashMap<String, Symbol>> {
        &self.imported_symbols
    }

    pub fn all_module_asts(&self) -> &std::collections::HashMap<String, Vec<AstNode>> {
        &self.all_module_asts
    }

    pub fn required_runtime_features(&self) -> Vec<String> {
        let mut features: Vec<String> = self.required_runtime_features.iter().cloned().collect();
        features.sort();
        features
    }

    pub(super) fn track_runtime_features_for_std_module_name(&mut self, module_name: &str) {
        let registry = std_module_registry();
        let full_name = if module_name.starts_with("std.") {
            module_name.to_string()
        } else {
            format!("std.{}", module_name)
        };

        if let Some(def) = registry.get(full_name.as_str()) {
            for feature in def.runtime_features {
                self.required_runtime_features
                    .insert((*feature).to_string());
            }
        }
    }

    /// Generate helpful context for binary operator type mismatches.
    fn binary_op_help(left: &Type, right: &Type, op: &crate::ast::BinaryOp) -> String {
        match (left, right) {
            (Type::Primitive(crate::ast::PrimitiveType::Str), Type::Primitive(crate::ast::PrimitiveType::Int))
            | (Type::Primitive(crate::ast::PrimitiveType::Int), Type::Primitive(crate::ast::PrimitiveType::Str)) => {
                "Strings and integers cannot be combined directly. Use int_to_string() to convert the integer first, then use '+' for concatenation.".to_string()
            }
            (Type::Primitive(crate::ast::PrimitiveType::Str), Type::Primitive(crate::ast::PrimitiveType::Float))
            | (Type::Primitive(crate::ast::PrimitiveType::Float), Type::Primitive(crate::ast::PrimitiveType::Str)) => {
                "Strings and floats cannot be combined directly. Use float_to_string() to convert the float first, then use '+' for concatenation.".to_string()
            }
            (Type::Primitive(crate::ast::PrimitiveType::Int), Type::Primitive(crate::ast::PrimitiveType::Float))
            | (Type::Primitive(crate::ast::PrimitiveType::Float), Type::Primitive(crate::ast::PrimitiveType::Int)) => {
                "Cannot mix int and float in arithmetic. Use int_to_float() or float_to_int() to convert one operand.".to_string()
            }
            (Type::Primitive(crate::ast::PrimitiveType::Str), Type::Primitive(crate::ast::PrimitiveType::Str)) => {
                format!("The '{}' operator is not supported between two strings.", format_binary_op(op))
            }
            _ => {
                format!(
                    "Ensure both operands have compatible types. Left is {}, right is {}.",
                    format_type(left),
                    format_type(right)
                )
            }
        }
    }

    /// Build an "undefined symbol" error with a "did you mean?" suggestion if a similar
    /// symbol exists in the current scope.
    fn undefined_symbol_error(&self, kind: &str, name: &str, span: Span) -> SemanticError {
        if let Some(suggestion) = self.symbol_table.find_similar(name) {
            SemanticError::with_help(
                format!("Undefined {} '{}'", kind, name),
                span,
                format!("Did you mean '{}'?", suggestion),
            )
        } else {
            SemanticError::new(format!("Undefined {} '{}'", kind, name), span)
        }
    }

    /// Generic helper for item-not-found errors, suggesting similar names if available.
    fn item_not_found_error<F, M>(
        &self,
        item_type: &str,
        item: &str,
        type_name: &str,
        span: Span,
        get_available: F,
        message_format: M,
    ) -> SemanticError
    where
        F: Fn(&str) -> Vec<String>,
        M: Fn(&str, &str, &str) -> String,
    {
        let available_items = get_available(type_name);
        if available_items.is_empty() {
            SemanticError::new(message_format(item_type, item, type_name), span)
        } else {
            let threshold = calculate_similarity_threshold(item);
            let suggestion = available_items
                .iter()
                .map(|f| (f, levenshtein_distance(item, f)))
                .filter(|(_, dist)| *dist <= threshold)
                .min_by_key(|(_, dist)| *dist)
                .map(|(f, _)| f);
            let available = available_items.join(", ");
            if let Some(similar) = suggestion {
                SemanticError::with_help(
                    message_format(item_type, item, type_name),
                    span,
                    format!(
                        "Did you mean '{}'? Available {}s: {}",
                        similar,
                        item_type.to_lowercase(),
                        available
                    ),
                )
            } else {
                SemanticError::with_help(
                    message_format(item_type, item, type_name),
                    span,
                    format!("Available {}s: {}", item_type.to_lowercase(), available),
                )
            }
        }
    }

    /// Build a field-not-found error, suggesting similar field names if available.
    fn field_not_found_error(&self, field: &str, type_name: &str, span: Span) -> SemanticError {
        self.item_not_found_error(
            "Field",
            field,
            type_name,
            span,
            |t| self.get_available_fields(t),
            |_item_type, item, type_name| {
                format!("Field '{}' not found on type '{}'", item, type_name)
            },
        )
    }

    /// Get a list of field names for a given type.
    fn get_available_fields(&self, type_name: &str) -> Vec<String> {
        if let Some(symbol) = self.symbol_table.lookup(type_name) {
            symbol.fields.keys().cloned().collect()
        } else {
            Vec::new()
        }
    }

    /// Build a method-not-found error, suggesting similar method names if available.
    fn method_not_found_error(&self, method: &str, type_name: &str, span: Span) -> SemanticError {
        self.item_not_found_error(
            "Method",
            method,
            type_name,
            span,
            |t| self.get_available_methods(t),
            |_item_type, item, type_name| {
                format!("Undefined method '{}' on type '{}'", item, type_name)
            },
        )
    }

    /// Get a list of method names for a given type.
    fn get_available_methods(&self, type_name: &str) -> Vec<String> {
        if let Some(symbol) = self.symbol_table.lookup(type_name) {
            symbol.methods.keys().cloned().collect()
        } else {
            Vec::new()
        }
    }

    /// Set class-level type parameters and their bounds for method analysis.
    /// This should be called before analyzing/generating methods of a generic class.
    pub fn set_class_type_params(&mut self, params: Vec<(String, GenericBounds)>) {
        self.current_class_type_params = Some(params.clone());
        // Also add to current_bounds for immediate use in type checking
        for (param, bounds) in params {
            self.current_bounds.insert(param, bounds);
        }
    }

    /// Clear class-level type parameters after finishing with a class.
    pub fn clear_class_type_params(&mut self) {
        if let Some(params) = &self.current_class_type_params {
            for (param, _) in params {
                self.current_bounds.remove(param);
            }
        }
        self.current_class_type_params = None;
    }

    fn normalize_type_for_bound(&self, type_: &Type, known_type_params: &[String]) -> Type {
        match type_ {
            Type::Named(name, args)
                if args.is_empty() && known_type_params.iter().any(|p| p == name) =>
            {
                Type::Variable(name.clone())
            }
            Type::Named(name, args) => Type::Named(
                name.clone(),
                args.iter()
                    .map(|arg| self.normalize_type_for_bound(arg, known_type_params))
                    .collect(),
            ),
            Type::List(inner) => Type::List(Box::new(
                self.normalize_type_for_bound(inner, known_type_params),
            )),
            Type::Set(inner) => Type::Set(Box::new(
                self.normalize_type_for_bound(inner, known_type_params),
            )),
            Type::Map(key, value) => Type::Map(
                Box::new(self.normalize_type_for_bound(key, known_type_params)),
                Box::new(self.normalize_type_for_bound(value, known_type_params)),
            ),
            Type::Optional(inner) => Type::Optional(Box::new(
                self.normalize_type_for_bound(inner, known_type_params),
            )),
            Type::Reference(inner) => Type::Reference(Box::new(
                self.normalize_type_for_bound(inner, known_type_params),
            )),
            Type::Function {
                params,
                returns,
                default_count,
            } => Type::Function {
                params: params
                    .iter()
                    .map(|param| self.normalize_type_for_bound(param, known_type_params))
                    .collect(),
                returns: Box::new(self.normalize_type_for_bound(returns, known_type_params)),
                default_count: *default_count,
            },
            _ => type_.clone(),
        }
    }

    fn resolve_type_param_bounds(
        &self,
        type_params: &[(String, Vec<TraitBound>)],
    ) -> Result<Vec<(String, GenericBounds)>, SemanticError> {
        let mut resolved = Vec::new();
        let mut known_type_params = Vec::new();

        for (param, bounds) in type_params {
            let mut resolved_bounds = Vec::new();
            for bound in bounds {
                let resolved_type_args = bound
                    .type_params
                    .iter()
                    .map(|type_param| self.resolve_type(type_param))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .map(|ty| self.normalize_type_for_bound(&ty, &known_type_params))
                    .collect();
                resolved_bounds.push((bound.name.clone(), resolved_type_args));
            }
            resolved.push((param.clone(), resolved_bounds));
            known_type_params.push(param.clone());
        }

        Ok(resolved)
    }

    fn infer_missing_type_params_from_function_bounds(
        &self,
        func_name: &str,
        substitutions: &mut std::collections::HashMap<String, Type>,
    ) {
        let mut func_node_opt = None;
        for module_nodes in self.all_module_asts.values() {
            for node in module_nodes {
                if let AstNode::Function(func) = node
                    && func.name == func_name
                {
                    func_node_opt = Some(func);
                    break;
                }
            }
            if func_node_opt.is_some() {
                break;
            }
        }

        let Some(func_node) = func_node_opt else {
            return;
        };

        infer_missing_type_params_from_bounds(&func_node.type_params, substitutions);
    }

    pub(super) fn get_builtin_sig(&self, name: &str) -> Option<&BuiltInSig> {
        // Use the canonical BUILT_IN_FUNCTIONS from the stdlib module
        crate::semantics::stdlib::BUILT_IN_FUNCTIONS.get(name)
    }

    pub fn analyze(
        &mut self,
        ast: &[AstNode],
        mut files: Option<&mut Files>,
    ) -> Vec<SemanticError> {
        self.add_builtin_functions();
        if let Err(e) = self.collect_hoistable_declarations(ast, files.as_deref_mut()) {
            self.errors.push(e);
        }
        // Imports are loaded by now, so interface where-clauses from every
        // module are visible for classes to inherit during analysis.
        self.collect_interface_preconditions(ast);
        self.collect_where_preconditions(ast);
        self.analyze_nodes(ast, files);
        std::mem::take(&mut self.errors)
    }

    fn add_builtin_functions(&mut self) {
        // Register built-in functions from the canonical stdlib table.
        let span = Span::new(0, 0);
        for (name, sig) in crate::semantics::stdlib::BUILT_IN_FUNCTIONS.iter() {
            self.register_builtin_function(name, sig, span);
        }

        // Register builtin classes
        self.add_sync_builtin_types();
        self.add_csv_builtin_types();
    }

    fn add_sync_builtin_types(&mut self) {
        let span = Span::new(0, 0);
        // Use canonical class symbols from the stdlib module and register them.
        let classes = crate::semantics::stdlib::sync_module_class_symbols(span);
        for (name, sym) in classes {
            let _ = self.symbol_table.add_symbol(&name, sym);
        }
    }

    fn add_csv_builtin_types(&mut self) {
        let span = Span::new(0, 0);
        let symbol = Self::make_csv_symbol(span);
        let _ = self.symbol_table.add_symbol("Csv", symbol);
    }

    #[allow(clippy::only_used_in_recursion)]
    pub fn resolve_type(&self, type_node: &TypeNode) -> Result<Type, SemanticError> {
        match &type_node.kind {
            TypeKind::Primitive(prim) => match prim {
                crate::ast::PrimitiveType::Int => {
                    Ok(Type::Primitive(crate::ast::PrimitiveType::Int))
                }
                crate::ast::PrimitiveType::Float => {
                    Ok(Type::Primitive(crate::ast::PrimitiveType::Float))
                }
                crate::ast::PrimitiveType::Bool => {
                    Ok(Type::Primitive(crate::ast::PrimitiveType::Bool))
                }
                crate::ast::PrimitiveType::Char => {
                    Ok(Type::Primitive(crate::ast::PrimitiveType::Char))
                }
                crate::ast::PrimitiveType::Str => {
                    Ok(Type::Primitive(crate::ast::PrimitiveType::Str))
                }
                crate::ast::PrimitiveType::Void => Ok(Type::Void),
                crate::ast::PrimitiveType::Auto => Err(SemanticError::with_help(
                    "The 'auto' type is not allowed in this context",
                    type_node.span,
                    "Use an explicit type annotation instead of 'auto'",
                )),
            },
            TypeKind::Named(name, type_args) => {
                // Handle type parameters (generic type variables)
                if type_args.is_empty()
                    && let Some(symbol) = self.symbol_table.lookup(name)
                    && matches!(symbol.kind, SymbolKind::Type)
                {
                    return Ok(Type::Variable(name.clone()));
                }

                // Handle built-in generic types
                if name == "optional" && type_args.len() == 1 {
                    let resolved_arg = self.resolve_type(&type_args[0])?;
                    return Ok(Type::Optional(Box::new(resolved_arg)));
                } else if name == "result" && type_args.len() == 2 {
                    let resolved_ok = self.resolve_type(&type_args[0])?;
                    let resolved_err = self.resolve_type(&type_args[1])?;
                    if !self.type_implements_interface(&resolved_err, "Error") {
                        return Err(SemanticError::with_help(
                            format!(
                                "Result error type must implement Error, but found {}",
                                format_type(&resolved_err)
                            ),
                            type_node.span,
                            "Use an error type that implements Error (requires message() -> string).",
                        ));
                    }
                    return Ok(Type::Result(Box::new(resolved_ok), Box::new(resolved_err)));
                }

                // named types are assumed to be classes, enums, or interfaces
                let resolved_args = type_args
                    .iter()
                    .map(|arg| self.resolve_type(arg))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Type::Named(name.clone(), resolved_args))
            }
            TypeKind::Function { params, returns } => {
                let resolved_params = params
                    .iter()
                    .map(|p| self.resolve_type(p))
                    .collect::<Result<Vec<_>, _>>()?;
                let resolved_return = self.resolve_type(returns)?;
                Ok(Type::Function {
                    params: resolved_params,
                    returns: Box::new(resolved_return),
                    default_count: 0,
                })
            }
            TypeKind::Reference(inner) => {
                let resolved_inner = self.resolve_type(inner)?;
                Ok(Type::Reference(Box::new(resolved_inner)))
            }
            TypeKind::List(inner) => {
                let resolved_inner = self.resolve_type(inner)?;
                Ok(Type::List(Box::new(resolved_inner)))
            }
            TypeKind::Map(key, value) => {
                let resolved_key = self.resolve_type(key)?;
                let resolved_value = self.resolve_type(value)?;
                Ok(Type::Map(Box::new(resolved_key), Box::new(resolved_value)))
            }
            TypeKind::Set(inner) => {
                let resolved_inner = self.resolve_type(inner)?;
                Ok(Type::Set(Box::new(resolved_inner)))
            }
            TypeKind::Tuple(left, right) => {
                let resolved_left = self.resolve_type(left)?;
                let resolved_right = self.resolve_type(right)?;
                Ok(Type::Tuple(
                    Box::new(resolved_left),
                    Box::new(resolved_right),
                ))
            }

            TypeKind::TraitObject(_) => Err(SemanticError::new(
                "Trait objects are not yet supported",
                type_node.span,
            )),
            TypeKind::Auto => Err(SemanticError::with_help(
                "The 'auto' type is not allowed in this context",
                type_node.span,
                "Use an explicit type annotation instead of 'auto'",
            )),
        }
    }

    pub fn get_expression_type(&mut self, expr: &ExpressionNode) -> Result<Type, SemanticError> {
        if let Some(override_type) = self.expression_type_overrides.get(&expr.span) {
            return Ok(override_type.clone());
        }
        match &expr.kind {
            ExpressionKind::Literal(_) => self.infer_literal_type(expr),
            ExpressionKind::None => Ok(Type::Optional(Box::new(Type::Never))),
            ExpressionKind::Identifier(name) => {
                self.resolve_identifier_expression_type(name, expr.span)
            }
            ExpressionKind::Binary {
                left,
                right,
                op,
                op_span,
                ..
            } => self.resolve_binary_expression_type(left, right, op, op_span, expr.span),
            ExpressionKind::Unary {
                expr, op, op_span, ..
            } => self.resolve_unary_expression_type(expr, op, op_span),
            ExpressionKind::Call { func, args } => {
                self.resolve_call_expression_type(func, args, expr.span)
            }
            ExpressionKind::FieldAccess { expr, field } => {
                self.resolve_field_access_type(expr, field, expr.span)
            }
            ExpressionKind::ListAccess { expr, index: _ } => {
                self.resolve_list_access_type(expr, expr.span)
            }
            ExpressionKind::ListLiteral(elements) => self.resolve_list_literal_type(elements),
            ExpressionKind::MapLiteral { entries, .. } => self.resolve_map_literal_type(entries),
            ExpressionKind::If {
                then_expr,
                else_expr,
                ..
            } => self.resolve_if_expression_type(then_expr, else_expr, expr.span),
            ExpressionKind::Lambda {
                params,
                return_type,
                body,
                ..
            } => self.resolve_lambda_type(params, return_type, body, expr.span),
            ExpressionKind::SetLiteral(elements) => self.resolve_set_literal_type(elements),
            ExpressionKind::TupleLiteral(elements) => {
                self.resolve_tuple_literal_type(elements, expr.span)
            }
            ExpressionKind::GenericType(name, type_args) => {
                self.resolve_generic_type(name, type_args, expr.span)
            }
        }
    }

    fn resolve_field_access_type(
        &mut self,
        expr: &ExpressionNode,
        field: &str,
        span: Span,
    ) -> Result<Type, SemanticError> {
        let expr_type_res = self.get_expression_type(expr);
        let expr_type = match expr_type_res {
            Ok(t) => t,
            Err(e) => {
                if let crate::ast::ExpressionKind::Identifier(name) = &expr.kind
                    && let Some(func_type) = self.try_stdlib_method_lookup(name, field)
                {
                    return Ok(func_type);
                }
                return Err(e);
            }
        };

        if let crate::ast::ExpressionKind::Identifier(name) = &expr.kind
            && matches!(expr_type, Type::Function { .. })
            && let Some(func_type) = self.try_stdlib_method_lookup(name, field)
        {
            return Ok(func_type);
        }

        self.resolve_field_access_by_type(&expr_type, field, span)
    }

    fn try_stdlib_method_lookup(&self, name: &str, field: &str) -> Option<Type> {
        if let Some(symbol) = self.symbol_table.lookup(name)
            && matches!(symbol.kind, SymbolKind::Function)
        {
            use crate::semantics::stdlib::{StdlibItem, lookup_stdlib_item};

            let full_name = format!("{}.{}", name, field);
            if let Some(StdlibItem::Function { params, ret, .. }) = lookup_stdlib_item(&full_name) {
                return Some(Type::Function {
                    params: params.clone(),
                    returns: Box::new(ret.clone()),
                    default_count: 0,
                });
            }
        }

        let stdlib_names: std::collections::HashSet<String> = std_module_registry()
            .keys()
            .filter_map(|s| s.strip_prefix("std.").map(|name| name.to_string()))
            .collect();
        for (ns, module_symbols) in &self.imported_symbols {
            if !stdlib_names.contains(ns) {
                continue;
            }
            if let Some(class_sym) = module_symbols.get(name)
                && matches!(class_sym.kind, SymbolKind::Class)
                && let Some(method_sig) = class_sym.methods.get(field)
            {
                return Some(Type::Function {
                    params: method_sig.params.clone(),
                    returns: Box::new(method_sig.return_type.clone()),
                    default_count: 0,
                });
            }
        }
        None
    }

    fn resolve_field_access_by_type(
        &mut self,
        expr_type: &Type,
        field: &str,
        span: Span,
    ) -> Result<Type, SemanticError> {
        if let Type::Module(module_name) = expr_type {
            return self.resolve_module_field(module_name, field, span);
        }
        if let Type::Reference(inner) = expr_type {
            let inner_type = (*inner).clone();
            return self.resolve_reference_field(&inner_type, field, span);
        }
        if let Some(method_sig) = self.get_method_sig(expr_type, field) {
            return Ok(Type::Function {
                params: method_sig.params,
                returns: Box::new(method_sig.return_type),
                default_count: 0,
            });
        }
        if let Type::Named(name, args) = expr_type {
            return self.resolve_named_field(expr_type, name, args, field, span);
        }
        if let Type::Tuple(left_type, right_type) = expr_type {
            return match field {
                "left" => Ok(*left_type.clone()),
                "right" => Ok(*right_type.clone()),
                _ => Err(SemanticError::with_help(
                    format!("Unknown field '{}' on tuple type", field),
                    span,
                    "Tuples only have two fields: 'left' and 'right'. Example: auto pair = (1, 2); print(int_to_string(pair.left))",
                )),
            };
        }
        let type_name = format_type(expr_type);
        Err(self.method_not_found_error(field, &type_name, span))
    }

    fn resolve_list_access_type(
        &mut self,
        expr: &ExpressionNode,
        span: Span,
    ) -> Result<Type, SemanticError> {
        let target_type = self.get_expression_type(expr)?;
        match target_type {
            Type::List(elem_type) => Ok(*elem_type),
            Type::Map(_, value_type) => Ok(*value_type),
            Type::EmptyMap => Err(SemanticError::with_help(
                "Cannot index empty map",
                span,
                "The map type is unknown. Provide type annotations or add entries to the map literal.",
            )),
            _ => Err(SemanticError::with_help(
                "Cannot index non-list type",
                span,
                "Only lists and maps can be indexed with '[]'. Examples: my_list[0], my_map['key']",
            )),
        }
    }

    fn resolve_list_literal_type(
        &mut self,
        elements: &[ExpressionNode],
    ) -> Result<Type, SemanticError> {
        if elements.is_empty() {
            return Ok(Type::EmptyList);
        }
        let first_type = self.get_expression_type(&elements[0])?;
        for (index, element) in elements.iter().enumerate() {
            let element_type = self.get_expression_type(element)?;
            if self
                .check_type_compatibility(&first_type, &element_type, element.span)
                .is_err()
            {
                return Err(SemanticError::with_help(
                    format!(
                        "List element type mismatch: expected {}, but element at index {} has type {}",
                        format_type(&first_type),
                        index,
                        format_type(&element_type)
                    ),
                    element.span,
                    "All elements in a list must have the same type. The list type is inferred from the first element.",
                ));
            }
        }
        Ok(Type::List(Box::new(first_type)))
    }

    fn resolve_map_literal_type(
        &mut self,
        entries: &[(ExpressionNode, ExpressionNode)],
    ) -> Result<Type, SemanticError> {
        if entries.is_empty() {
            return Ok(Type::EmptyMap);
        }
        let (key, value) = &entries[0];
        let key_type = self.get_expression_type(key)?;
        let value_type = self.get_expression_type(value)?;
        Ok(Type::Map(Box::new(key_type), Box::new(value_type)))
    }

    fn resolve_if_expression_type(
        &mut self,
        then_expr: &ExpressionNode,
        else_expr: &ExpressionNode,
        span: Span,
    ) -> Result<Type, SemanticError> {
        let then_type = self.get_expression_type(then_expr)?;
        let else_type = self.get_expression_type(else_expr)?;
        if then_type == else_type {
            Ok(then_type)
        } else {
            Err(SemanticError::with_help(
                "If expression branches must have the same type",
                span,
                format!(
                    "Then branch has type {}, else branch has type {}",
                    format_type(&then_type),
                    format_type(&else_type)
                ),
            ))
        }
    }

    fn resolve_lambda_type(
        &mut self,
        params: &[Param],
        return_type: &TypeNode,
        body: &[StatementNode],
        span: Span,
    ) -> Result<Type, SemanticError> {
        if self.lambda_captures.contains_key(&span) {
            let param_types = params
                .iter()
                .map(|p| self.resolve_type(&p.type_))
                .collect::<Result<Vec<_>, _>>()?;
            let resolved_return_type = self.resolve_type(return_type)?;
            let default_count = params.iter().filter(|p| p.default_value.is_some()).count();
            return Ok(Type::Function {
                params: param_types,
                returns: Box::new(resolved_return_type),
                default_count,
            });
        }

        let mut local_vars = std::collections::HashSet::new();
        for param in params {
            local_vars.insert(param.name.clone());
        }

        self.symbol_table.push_scope()?;
        for param in params {
            let param_type = self.resolve_type(&param.type_)?;
            self.symbol_table.add_symbol(
                &param.name,
                Self::make_symbol(SymbolKind::Variable, param.type_.span, Some(param_type)),
            )?;
        }
        self.analyze_block(body, None)?;

        let captures = self.find_free_variables_in_block(body, &local_vars)?;
        self.lambda_captures.insert(span, captures);

        let param_types = params
            .iter()
            .map(|p| self.resolve_type(&p.type_))
            .collect::<Result<Vec<_>, _>>()?;
        let return_type_resolved = if body.is_empty() {
            Type::Void
        } else {
            match &body.last().expect("body is not empty").kind {
                StatementKind::Expression(expr) => self.get_expression_type(expr)?,
                StatementKind::Return(Some(expr)) => self.get_expression_type(expr)?,
                _ => Type::Void,
            }
        };
        self.symbol_table.pop_scope()?;
        let default_count = params.iter().filter(|p| p.default_value.is_some()).count();
        Ok(Type::Function {
            params: param_types,
            returns: Box::new(return_type_resolved),
            default_count,
        })
    }

    fn resolve_set_literal_type(
        &mut self,
        elements: &[ExpressionNode],
    ) -> Result<Type, SemanticError> {
        if elements.is_empty() {
            return Ok(Type::EmptySet);
        }
        let elem_type = self.get_expression_type(&elements[0])?;
        Ok(Type::Set(Box::new(elem_type)))
    }

    fn resolve_tuple_literal_type(
        &mut self,
        elements: &[ExpressionNode],
        span: Span,
    ) -> Result<Type, SemanticError> {
        if elements.len() != 2 {
            return Err(SemanticError::with_help(
                format!(
                    "Tuple must have exactly 2 elements, found {}",
                    elements.len()
                ),
                span,
                "Tuples in Mux always contain exactly 2 elements: (left, right). Example: auto pair = (1, \"hello\")",
            ));
        }
        let left_type = self.get_expression_type(&elements[0])?;
        let right_type = self.get_expression_type(&elements[1])?;
        Ok(Type::Tuple(Box::new(left_type), Box::new(right_type)))
    }

    fn resolve_generic_type(
        &mut self,
        name: &str,
        type_args: &[TypeNode],
        span: Span,
    ) -> Result<Type, SemanticError> {
        if name == "tuple" {
            return self.resolve_tuple_type_annotation(type_args, span);
        }
        let (lookup_name, symbol) = self.resolve_generic_type_symbol(name, span)?;
        self.validate_type_argument_count(&lookup_name, &symbol, type_args, span)?;
        let resolved_args = type_args
            .iter()
            .map(|arg| self.resolve_type(arg))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Type::Named(lookup_name, resolved_args))
    }

    fn resolve_tuple_type_annotation(
        &self,
        type_args: &[TypeNode],
        span: Span,
    ) -> Result<Type, SemanticError> {
        if type_args.len() != 2 {
            return Err(SemanticError::with_help(
                format!(
                    "Tuple type requires exactly 2 type arguments, got {}",
                    type_args.len()
                ),
                span,
                "Tuple types always have exactly 2 type parameters. Example: tuple<int, string>",
            ));
        }
        let left_type = Box::new(self.resolve_type(&type_args[0])?);
        let right_type = Box::new(self.resolve_type(&type_args[1])?);
        Ok(Type::Tuple(left_type, right_type))
    }

    fn resolve_generic_type_symbol(
        &mut self,
        name: &str,
        span: Span,
    ) -> Result<(String, Symbol), SemanticError> {
        if let Some((module_name, type_name)) = name.split_once('.') {
            let module_symbols = self
                .imported_symbols
                .get(module_name)
                .ok_or_else(|| self.undefined_symbol_error("module", module_name, span))?;
            let symbol = module_symbols
                .get(type_name)
                .ok_or_else(|| self.undefined_symbol_error("type", type_name, span))?;
            if self.symbol_table.lookup(type_name).is_none() {
                let _ = self.symbol_table.add_symbol(type_name, symbol.clone());
            }
            Ok((type_name.to_string(), symbol.clone()))
        } else if let Some(symbol) = self.symbol_table.lookup(name) {
            Ok((name.to_string(), symbol))
        } else {
            Err(self.undefined_symbol_error("type", name, span))
        }
    }

    fn validate_type_argument_count(
        &self,
        lookup_name: &str,
        symbol: &Symbol,
        type_args: &[TypeNode],
        span: Span,
    ) -> Result<(), SemanticError> {
        let expected_count = symbol.type_params.len();
        let actual_count = type_args.len();
        if expected_count != actual_count {
            return Err(SemanticError::with_help(
                format!(
                    "Generic type '{}' requires {} type argument(s), got {}",
                    lookup_name, expected_count, actual_count
                ),
                span,
                format!(
                    "Provide exactly {} type argument{} in angle brackets, e.g. {}<{}>",
                    expected_count,
                    if expected_count == 1 { "" } else { "s" },
                    lookup_name,
                    symbol
                        .type_params
                        .iter()
                        .map(|(p, _)| p.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
        Ok(())
    }

    fn resolve_unary_expression_type(
        &mut self,
        expr: &ExpressionNode,
        op: &UnaryOp,
        op_span: &Span,
    ) -> Result<Type, SemanticError> {
        match op {
            UnaryOp::Not => Ok(Type::Primitive(crate::ast::PrimitiveType::Bool)),
            UnaryOp::Neg => {
                let operand_type = self.get_expression_type(expr)?;
                match operand_type {
                    Type::Primitive(crate::ast::PrimitiveType::Int)
                    | Type::Primitive(crate::ast::PrimitiveType::Float) => Ok(operand_type),
                    _ => Err(SemanticError::with_help(
                        format!(
                            "Negation operator '-' requires a numeric operand, found {}",
                            format_type(&operand_type)
                        ),
                        *op_span,
                        "The unary '-' operator can only be applied to int or float values",
                    )),
                }
            }
            UnaryOp::Ref => {
                let operand_type = self.get_expression_type(expr)?;
                Ok(Type::Reference(Box::new(operand_type)))
            }
            UnaryOp::Deref => {
                let operand_type = self.get_expression_type(expr)?;
                if let Type::Reference(inner) = operand_type {
                    Ok(*inner)
                } else {
                    Err(SemanticError::with_help(
                        format!(
                            "Cannot dereference type {}, which is not a reference",
                            format_type(&operand_type)
                        ),
                        *op_span,
                        "The dereference operator '*' can only be applied to reference types (e.g., ref int)",
                    ))
                }
            }
            UnaryOp::Incr | UnaryOp::Decr => {
                self.check_not_modifying_constant(expr, op_span)?;
                let operand_type = self.get_expression_type(expr)?;
                match operand_type {
                    Type::Primitive(crate::ast::PrimitiveType::Int) => Ok(operand_type),
                    _ => Err(SemanticError::with_help(
                        format!(
                            "Increment/decrement operators require an int operand, found {}",
                            format_type(&operand_type)
                        ),
                        *op_span,
                        "The '++' and '--' operators can only be applied to int variables",
                    )),
                }
            }
        }
    }

    fn resolve_call_expression_type(
        &mut self,
        func: &ExpressionNode,
        args: &[ExpressionNode],
        expr_span: Span,
    ) -> Result<Type, SemanticError> {
        let func_type = self.resolve_called_function_type(func)?;

        match func_type {
            Type::Function {
                params,
                returns,
                default_count,
                ..
            } => self.resolve_function_call_type(
                func,
                args,
                params,
                returns,
                default_count,
                expr_span,
            ),
            _ => Err(SemanticError::with_help(
                "Cannot call non-function type",
                expr_span,
                "Only functions can be called with '()'. Ensure the expression before '()' is a function.",
            )),
        }
    }

    fn resolve_called_function_type(
        &mut self,
        func: &ExpressionNode,
    ) -> Result<Type, SemanticError> {
        match &func.kind {
            ExpressionKind::GenericType(name, type_args) => {
                self.get_instantiated_constructor_type(name, type_args, func.span)
            }
            ExpressionKind::Identifier(name) => match self.get_expression_type(func) {
                Ok(t) => Ok(t),
                Err(e) if e.message.contains("Undefined variable") => {
                    Err(self.undefined_symbol_error("function", name, func.span))
                }
                Err(e) => Err(e),
            },
            _ => self.get_expression_type(func),
        }
    }

    fn resolve_function_call_type(
        &mut self,
        func: &ExpressionNode,
        args: &[ExpressionNode],
        params: Vec<Type>,
        returns: Box<Type>,
        default_count: usize,
        expr_span: Span,
    ) -> Result<Type, SemanticError> {
        let actual_default_count = self.call_default_param_count(func, default_count);
        let min_args = params.len() - actual_default_count;
        let max_args = params.len();

        if args.len() < min_args || args.len() > max_args {
            self.report_call_arity_error(
                func,
                args.len(),
                min_args,
                max_args,
                actual_default_count,
                expr_span,
            )?;
        }

        // Alpha-rename the callee's own type variables to globally-unique names before
        // unifying. Without this, a callee whose type parameter happens to share a name
        // with one already in scope at the call site (e.g. both called "T") can make the
        // unifier's occurs check misfire: it would see the in-scope "T" nested inside an
        // argument type (like list<T>) and mistake it for the callee's own "T", reporting
        // a false recursive-type error even though the two are unrelated.
        //
        // This is scoped to plain identifier calls (free functions and the built-in
        // constructors `some`/`ok`/`err`) rather than method calls on a field/receiver.
        // Method calls on an interface-bound type parameter (e.g. `collection.to_list()`
        // where `E is Collection<T>`) have no arguments to unify the receiver's own type
        // parameter against, so their return type is resolved by the literal type
        // parameter name matching the caller's in-scope name; renaming here would break
        // that resolution.
        let should_rename = matches!(func.kind, ExpressionKind::Identifier(_));
        let mut rename_map = std::collections::HashMap::new();
        let renamed_params: Vec<Type> = if should_rename {
            params
                .iter()
                .map(|p| self.rename_type_vars(p, &mut rename_map))
                .collect()
        } else {
            params.clone()
        };
        let renamed_returns = if should_rename {
            self.rename_type_vars(&returns, &mut rename_map)
        } else {
            *returns.clone()
        };

        let mut unifier = Unifier::new();
        for (param, arg) in renamed_params.iter().zip(args.iter()) {
            let arg_type = self.get_expression_type(arg)?;
            unifier.unify(param, &arg_type, expr_span)?;
        }

        if let Some(func_name) = self.call_function_name(func) {
            let reverse_rename_map: std::collections::HashMap<String, String> = rename_map
                .iter()
                .map(|(original, fresh)| (fresh.clone(), original.clone()))
                .collect();
            let mut substitutions_by_original_name: std::collections::HashMap<String, Type> =
                unifier
                    .substitutions
                    .iter()
                    .map(|(fresh, ty)| {
                        let original = reverse_rename_map
                            .get(fresh)
                            .cloned()
                            .unwrap_or_else(|| fresh.clone());
                        (original, ty.clone())
                    })
                    .collect();

            self.infer_missing_type_params_from_function_bounds(
                func_name,
                &mut substitutions_by_original_name,
            );

            for (original, ty) in substitutions_by_original_name {
                let fresh = rename_map.get(&original).cloned().unwrap_or(original);
                unifier.substitutions.entry(fresh).or_insert(ty);
            }
        }

        Ok(unifier.apply(&renamed_returns))
    }

    // Renames every distinct `Type::Variable`/`Type::Generic` found in `t` to a fresh,
    // globally-unique name, reusing the same fresh name for repeated occurrences within
    // this call (so e.g. both occurrences of a callee's own "T" still unify with each
    // other). See `resolve_function_call_type` for why this is needed.
    fn rename_type_vars(
        &mut self,
        t: &Type,
        mapping: &mut std::collections::HashMap<String, String>,
    ) -> Type {
        match t {
            Type::Variable(name) => Type::Variable(self.fresh_name_for(name, mapping)),
            Type::Generic(name) => Type::Generic(self.fresh_name_for(name, mapping)),
            Type::List(inner) => Type::List(Box::new(self.rename_type_vars(inner, mapping))),
            Type::Set(inner) => Type::Set(Box::new(self.rename_type_vars(inner, mapping))),
            Type::Optional(inner) => {
                Type::Optional(Box::new(self.rename_type_vars(inner, mapping)))
            }
            Type::Reference(inner) => {
                Type::Reference(Box::new(self.rename_type_vars(inner, mapping)))
            }
            Type::Map(key, value) => Type::Map(
                Box::new(self.rename_type_vars(key, mapping)),
                Box::new(self.rename_type_vars(value, mapping)),
            ),
            Type::Tuple(left, right) => Type::Tuple(
                Box::new(self.rename_type_vars(left, mapping)),
                Box::new(self.rename_type_vars(right, mapping)),
            ),
            Type::Result(ok, err) => Type::Result(
                Box::new(self.rename_type_vars(ok, mapping)),
                Box::new(self.rename_type_vars(err, mapping)),
            ),
            Type::Named(name, args) => Type::Named(
                name.clone(),
                args.iter()
                    .map(|arg| self.rename_type_vars(arg, mapping))
                    .collect(),
            ),
            Type::Instantiated(name, args) => Type::Instantiated(
                name.clone(),
                args.iter()
                    .map(|arg| self.rename_type_vars(arg, mapping))
                    .collect(),
            ),
            Type::Function {
                params,
                returns,
                default_count,
            } => Type::Function {
                params: params
                    .iter()
                    .map(|p| self.rename_type_vars(p, mapping))
                    .collect(),
                returns: Box::new(self.rename_type_vars(returns, mapping)),
                default_count: *default_count,
            },
            other => other.clone(),
        }
    }

    fn fresh_name_for(
        &mut self,
        original: &str,
        mapping: &mut std::collections::HashMap<String, String>,
    ) -> String {
        if let Some(existing) = mapping.get(original) {
            return existing.clone();
        }
        self.fresh_type_var_counter += 1;
        let fresh = format!("{}#{}", original, self.fresh_type_var_counter);
        mapping.insert(original.to_string(), fresh.clone());
        fresh
    }

    fn call_default_param_count(&self, func: &ExpressionNode, default_count: usize) -> usize {
        match &func.kind {
            ExpressionKind::Identifier(name) => {
                let symbol_default = self
                    .symbol_table
                    .lookup(name)
                    .map(|s| s.default_param_count)
                    .unwrap_or(0);
                std::cmp::max(default_count, symbol_default)
            }
            _ => default_count,
        }
    }

    fn call_function_name<'a>(&self, func: &'a ExpressionNode) -> Option<&'a str> {
        match &func.kind {
            ExpressionKind::Identifier(name) => Some(name.as_str()),
            ExpressionKind::FieldAccess { field, .. } => Some(field.as_str()),
            _ => None,
        }
    }

    fn report_call_arity_error(
        &self,
        func: &ExpressionNode,
        arg_count: usize,
        min_args: usize,
        max_args: usize,
        actual_default_count: usize,
        expr_span: Span,
    ) -> Result<(), SemanticError> {
        let func_name = match &func.kind {
            ExpressionKind::Identifier(name) => format!("'{}'", name),
            ExpressionKind::FieldAccess { field, .. } => format!("'{}'", field),
            _ => "this function".to_string(),
        };

        if actual_default_count > 0 {
            Err(SemanticError::with_help(
                format!(
                    "{} expects {} to {} arguments, but {} {} provided",
                    func_name,
                    min_args,
                    max_args,
                    arg_count,
                    if arg_count == 1 { "was" } else { "were" }
                ),
                expr_span,
                format!(
                    "{} has {} required parameter(s) and {} optional parameter(s) with defaults",
                    func_name, min_args, actual_default_count
                ),
            ))
        } else {
            Err(SemanticError::with_help(
                format!(
                    "{} expects {} argument(s), but {} {} provided",
                    func_name,
                    max_args,
                    arg_count,
                    if arg_count == 1 { "was" } else { "were" }
                ),
                expr_span,
                if arg_count > max_args {
                    "Too many arguments. Remove the extra argument(s).".to_string()
                } else {
                    format!(
                        "Not enough arguments. {} requires {} argument(s).",
                        func_name, max_args
                    )
                },
            ))
        }
    }

    fn resolve_identifier_expression_type(
        &self,
        name: &str,
        span: Span,
    ) -> Result<Type, SemanticError> {
        if name == "self" {
            if let Some(self_type) = &self.current_self_type {
                return Ok(self_type.clone());
            }
            return Ok(Type::Named("Unknown".to_string(), vec![]));
        }

        let symbol = self
            .symbol_table
            .get_cloned(name)
            .or_else(|| self.symbol_table.lookup(name));

        if let Some(symbol) = symbol {
            let type_ = symbol.type_.clone().ok_or_else(|| {
                SemanticError::new(format!("Symbol '{}' has no type information", name), span)
            })?;
            let type_ = match &type_ {
                Type::Generic(n) if n == name => Type::Variable(name.to_string()),
                _ => type_,
            };
            return Ok(type_);
        }

        if let Some(sig) = self.get_builtin_sig(name) {
            return Ok(Type::Function {
                params: sig.params.clone(),
                returns: Box::new(sig.return_type.clone()),
                default_count: 0,
            });
        }

        let stdlib_names: std::collections::HashSet<String> = std_module_registry()
            .keys()
            .filter_map(|s| s.strip_prefix("std.").map(|name| name.to_string()))
            .collect();

        for (module_ns, module_symbols) in &self.imported_symbols {
            if !stdlib_names.contains(module_ns) {
                continue;
            }
            if let Some(sym) = module_symbols.get(name)
                && matches!(sym.kind, SymbolKind::Class)
            {
                return Ok(Type::Named(name.to_string(), Vec::new()));
            }
        }

        Err(self.undefined_symbol_error("variable", name, span))
    }

    fn resolve_binary_expression_type(
        &mut self,
        left: &ExpressionNode,
        right: &ExpressionNode,
        op: &crate::ast::BinaryOp,
        op_span: &Span,
        expr_span: Span,
    ) -> Result<Type, SemanticError> {
        let left_type = self.get_expression_type(left)?;
        let right_type = self.get_expression_type(right)?;

        if *op == crate::ast::BinaryOp::Assign {
            self.resolve_empty_collection_types(&left_type, right)?;
            let right_type = self.get_expression_type(right)?;
            self.validate_assignment_target(left, &right_type, expr_span)?;
            return Ok(right_type);
        }

        if matches!(
            op,
            crate::ast::BinaryOp::AddAssign
                | crate::ast::BinaryOp::SubtractAssign
                | crate::ast::BinaryOp::MultiplyAssign
                | crate::ast::BinaryOp::DivideAssign
                | crate::ast::BinaryOp::ModuloAssign
        ) {
            self.validate_compound_assignment_target(
                left,
                &left_type,
                &right_type,
                op,
                expr_span,
                op_span,
            )?;
            let base_op = Self::compound_base_op(op);
            if let Some(result_type) =
                self.resolve_binary_operator(&left_type, &right_type, &base_op)
            {
                return Ok(result_type);
            }
            return Err(SemanticError::with_help(
                format!(
                    "Operator '{}' is not supported between types {} and {}",
                    format_binary_op(&base_op),
                    format_type(&left_type),
                    format_type(&right_type)
                ),
                *op_span,
                Self::binary_op_help(&left_type, &right_type, &base_op),
            ));
        }

        if let Some(result_type) = self.resolve_binary_operator(&left_type, &right_type, op) {
            return Ok(result_type);
        }

        Err(SemanticError::with_help(
            format!(
                "Operator '{}' is not supported between types {} and {}",
                format_binary_op(op),
                format_type(&left_type),
                format_type(&right_type)
            ),
            *op_span,
            Self::binary_op_help(&left_type, &right_type, op),
        ))
    }

    fn validate_assignment_target(
        &mut self,
        left: &ExpressionNode,
        right_type: &Type,
        expr_span: Span,
    ) -> Result<(), SemanticError> {
        match &left.kind {
            crate::ast::ExpressionKind::Identifier(name) => {
                self.validate_identifier_assignment_target(name, left.span, right_type, expr_span)
            }
            crate::ast::ExpressionKind::FieldAccess {
                expr: obj_expr,
                field,
            } => self.validate_field_assignment_target(
                obj_expr, field, left.span, right_type, expr_span,
            ),
            crate::ast::ExpressionKind::Unary {
                op: crate::ast::UnaryOp::Deref,
                ..
            } => Ok(()),
            crate::ast::ExpressionKind::ListAccess {
                expr: target_expr, ..
            } => self.validate_index_assignment_target(target_expr, right_type, expr_span),
            _ => Err(SemanticError::with_help(
                "Cannot assign to this expression",
                expr_span,
                "Only variables, fields, dereferences, and indexed expressions can be assigned to",
            )),
        }
    }

    fn resolve_lvalue_class_symbol(
        &mut self,
        obj_expr: &ExpressionNode,
    ) -> Result<Option<(String, crate::semantics::Symbol)>, SemanticError> {
        let obj_type = self.get_expression_type(obj_expr)?;
        if let Type::Named(class_name, _) = &obj_type
            && let Some(symbol) = self.symbol_table.lookup(class_name)
        {
            return Ok(Some((class_name.clone(), symbol)));
        }
        Ok(None)
    }

    fn check_const_field_assignment(
        &self,
        field: &str,
        fields: &std::collections::HashMap<String, (Type, bool)>,
        expr_span: Span,
    ) -> Result<(), SemanticError> {
        if let Some((_field_type, is_const)) = fields.get(field)
            && *is_const
        {
            return Err(SemanticError::with_help(
                format!("Cannot assign to const field '{}'", field),
                expr_span,
                "Const fields cannot be modified after initialization. Remove the 'const' modifier from the field declaration if mutation is needed.",
            ));
        }
        Ok(())
    }

    fn validate_identifier_assignment_target(
        &mut self,
        name: &str,
        left_span: Span,
        right_type: &Type,
        expr_span: Span,
    ) -> Result<(), SemanticError> {
        let symbol = self
            .symbol_table
            .lookup(name)
            .ok_or_else(|| self.undefined_symbol_error("variable", name, left_span))?;

        if symbol.kind == SymbolKind::Constant {
            return Err(SemanticError::with_help(
                format!("Cannot assign to constant '{}'", name),
                expr_span,
                "Constants cannot be modified after initialization",
            ));
        }

        let var_type = symbol.type_.as_ref().ok_or_else(|| {
            SemanticError::new(
                format!("Variable '{}' has no type information", name),
                left_span,
            )
        })?;
        if let Type::Reference(inner) = var_type {
            self.check_type_compatibility(inner, right_type, expr_span)
        } else {
            self.check_type_compatibility(var_type, right_type, expr_span)
        }
    }

    fn validate_field_assignment_target(
        &mut self,
        obj_expr: &ExpressionNode,
        field: &str,
        left_span: Span,
        right_type: &Type,
        expr_span: Span,
    ) -> Result<(), SemanticError> {
        let Some((class_name, symbol)) = self.resolve_lvalue_class_symbol(obj_expr)? else {
            return Ok(());
        };

        self.check_const_field_assignment(field, &symbol.fields, expr_span)?;

        if let Some((field_type, _)) = symbol.fields.get(field) {
            self.check_type_compatibility(field_type, right_type, expr_span)?;
            return Ok(());
        }
        Err(self.field_not_found_error(field, &class_name, left_span))
    }

    fn validate_index_assignment_target(
        &mut self,
        target_expr: &ExpressionNode,
        right_type: &Type,
        expr_span: Span,
    ) -> Result<(), SemanticError> {
        let target_type = self.get_expression_type(target_expr)?;
        match target_type {
            Type::List(ref elem_type) => {
                self.check_type_compatibility(elem_type, right_type, expr_span)
            }
            Type::Map(_, ref value_type) => {
                self.check_type_compatibility(value_type, right_type, expr_span)
            }
            _ => Err(SemanticError::with_help(
                format!(
                    "Cannot assign to index on type {}",
                    format_type(&target_type)
                ),
                expr_span,
                "Only lists and maps support index assignment. Example: my_list[0] = value, my_map[\"key\"] = value",
            )),
        }
    }

    fn validate_compound_assignment_target(
        &mut self,
        left: &ExpressionNode,
        left_type: &Type,
        right_type: &Type,
        op: &crate::ast::BinaryOp,
        expr_span: Span,
        op_span: &Span,
    ) -> Result<(), SemanticError> {
        match &left.kind {
            crate::ast::ExpressionKind::Identifier(name) => {
                self.validate_identifier_compound_target(name, left.span, expr_span)?;
            }
            crate::ast::ExpressionKind::FieldAccess { .. } => {
                self.validate_field_compound_target(
                    left, left_type, right_type, op, expr_span, op_span,
                )?;
            }
            crate::ast::ExpressionKind::Unary {
                op: crate::ast::UnaryOp::Deref,
                ..
            } => {}
            _ => {}
        }

        Ok(())
    }

    fn validate_identifier_compound_target(
        &self,
        name: &str,
        left_span: Span,
        expr_span: Span,
    ) -> Result<(), SemanticError> {
        let symbol = self
            .symbol_table
            .lookup(name)
            .ok_or_else(|| self.undefined_symbol_error("variable", name, left_span))?;

        if symbol.kind == SymbolKind::Constant {
            return Err(SemanticError::with_help(
                format!("Cannot modify constant '{}'", name),
                expr_span,
                "Constants cannot be modified after initialization. Declare the variable with 'auto' instead of 'const' if you need to change its value.",
            ));
        }

        Ok(())
    }

    fn validate_field_compound_target(
        &mut self,
        left: &ExpressionNode,
        left_type: &Type,
        right_type: &Type,
        op: &crate::ast::BinaryOp,
        expr_span: Span,
        op_span: &Span,
    ) -> Result<(), SemanticError> {
        let crate::ast::ExpressionKind::FieldAccess {
            expr: obj_expr,
            field,
        } = &left.kind
        else {
            return Ok(());
        };

        let Some((class_name, symbol)) = self.resolve_lvalue_class_symbol(obj_expr)? else {
            return Ok(());
        };

        if let Some((_field_type, is_const)) = symbol.fields.get(field) {
            if *is_const {
                return Err(SemanticError::with_help(
                    format!("Cannot modify const field '{}'", field),
                    expr_span,
                    "Const fields cannot be modified after initialization. Remove the 'const' modifier from the field declaration if mutation is needed.",
                ));
            }

            let base_op = Self::compound_base_op(op);
            self.resolve_binary_operator(left_type, right_type, &base_op)
                .ok_or_else(|| {
                    SemanticError::with_help(
                        format!(
                            "Operator '{}' is not supported between types {} and {}",
                            format_binary_op(&base_op),
                            format_type(left_type),
                            format_type(right_type)
                        ),
                        *op_span,
                        format!(
                            "The '{}' operator cannot be applied to {} and {}. Ensure both operands have compatible types.",
                            format_binary_op(&base_op),
                            format_type(left_type),
                            format_type(right_type)
                        ),
                    )
                })?;
        } else {
            return Err(self.field_not_found_error(field, &class_name, left.span));
        }

        Ok(())
    }

    fn compound_base_op(op: &crate::ast::BinaryOp) -> crate::ast::BinaryOp {
        match op {
            crate::ast::BinaryOp::AddAssign => crate::ast::BinaryOp::Add,
            crate::ast::BinaryOp::SubtractAssign => crate::ast::BinaryOp::Subtract,
            crate::ast::BinaryOp::MultiplyAssign => crate::ast::BinaryOp::Multiply,
            crate::ast::BinaryOp::DivideAssign => crate::ast::BinaryOp::Divide,
            crate::ast::BinaryOp::ModuloAssign => crate::ast::BinaryOp::Modulo,
            _ => unreachable!(),
        }
    }

    fn resolve_module_field(
        &self,
        module_name: &str,
        field: &str,
        span: Span,
    ) -> Result<Type, SemanticError> {
        let module_symbols = self.imported_symbols.get(module_name).ok_or_else(|| {
            SemanticError::with_help(
                format!("Module '{}' not found in imports", module_name),
                span,
                format!(
                    "Make sure you have imported '{}' at the top of your file, e.g. import {}",
                    module_name, module_name
                ),
            )
        })?;
        let symbol = module_symbols.get(field).ok_or_else(|| {
            let available: Vec<&String> = module_symbols.keys().collect();
            if available.is_empty() {
                SemanticError::new(
                    format!(
                        "Module '{}' has no exported symbol '{}'",
                        module_name, field
                    ),
                    span,
                )
            } else {
                SemanticError::with_help(
                    format!(
                        "Module '{}' has no exported symbol '{}'",
                        module_name, field
                    ),
                    span,
                    format!(
                        "Available exports: {}",
                        available
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )
            }
        })?;
        symbol.type_.clone().ok_or_else(|| {
            SemanticError::new(
                format!(
                    "Symbol '{}' in module '{}' has no type information",
                    field, module_name
                ),
                span,
            )
        })
    }

    fn resolve_reference_field(
        &mut self,
        inner_type: &Type,
        field: &str,
        span: Span,
    ) -> Result<Type, SemanticError> {
        if let Type::Named(name, args) = inner_type {
            if let Some(symbol) = self.symbol_table.lookup(name) {
                if let Some((field_type, _)) = symbol.fields.get(field) {
                    return Ok(self.substitute_type_params(field_type, &symbol.type_params, args));
                }
                if let Some(method_sig) = self.get_method_sig(inner_type, field) {
                    return Ok(Type::Function {
                        params: method_sig.params,
                        returns: Box::new(method_sig.return_type),
                        default_count: 0,
                    });
                }
                return Err(self.field_not_found_error(field, name, span));
            }
            return Err(self.undefined_symbol_error("type", name, span));
        }
        if let Some(method_sig) = self.get_method_sig(inner_type, field) {
            return Ok(Type::Function {
                params: method_sig.params,
                returns: Box::new(method_sig.return_type),
                default_count: 0,
            });
        }
        Err(SemanticError::with_help(
            format!(
                "Cannot access field '{}' on type {}",
                field,
                format_type(inner_type)
            ),
            span,
            format!(
                "The type {} does not have a field or method named '{}'",
                format_type(inner_type),
                field
            ),
        ))
    }

    fn resolve_named_field(
        &mut self,
        expr_type: &Type,
        name: &str,
        args: &[Type],
        field: &str,
        span: Span,
    ) -> Result<Type, SemanticError> {
        if let Some(symbol) = self.symbol_table.lookup(name) {
            return self
                .resolve_field_from_local_symbol(&symbol, args, expr_type, field, name, span);
        }

        self.resolve_field_from_imported_module(name, field, expr_type, args, span)
    }

    fn resolve_field_from_local_symbol(
        &mut self,
        symbol: &Symbol,
        args: &[Type],
        expr_type: &Type,
        field: &str,
        name: &str,
        span: Span,
    ) -> Result<Type, SemanticError> {
        if let Some((field_type, _)) = symbol.fields.get(field) {
            return Ok(self.substitute_type_params(field_type, &symbol.type_params, args));
        }

        if let Some(method_sig) = self.get_method_sig(expr_type, field) {
            return Ok(self.wrap_method_signature(&method_sig));
        }

        Err(self.field_not_found_error(field, name, span))
    }

    fn resolve_field_from_imported_module(
        &mut self,
        name: &str,
        field: &str,
        expr_type: &Type,
        args: &[Type],
        span: Span,
    ) -> Result<Type, SemanticError> {
        for module_symbols in self.imported_symbols.values() {
            if let Some(class_symbol) = module_symbols.get(name)
                && let Some(method_sig) = class_symbol.methods.get(field)
            {
                let resolved_sig =
                    self.resolve_method_sig_for_field(method_sig, &class_symbol.type_params, args);
                return Ok(self.wrap_method_signature(&resolved_sig));
            }
        }

        Err(self.method_not_found_error(field, &format_type(expr_type), span))
    }

    fn resolve_method_sig_for_field(
        &self,
        method_sig: &MethodSig,
        type_params: &[(String, Vec<String>)],
        args: &[Type],
    ) -> MethodSig {
        if args.is_empty() {
            method_sig.clone()
        } else {
            self.substitute_method_sig(method_sig, type_params, args)
        }
    }

    fn wrap_method_signature(&self, method_sig: &MethodSig) -> Type {
        Type::Function {
            params: method_sig.params.clone(),
            returns: Box::new(method_sig.return_type.clone()),
            default_count: 0,
        }
    }

    fn check_type_compatibility(
        &self,
        expected: &Type,
        actual: &Type,
        span: Span,
    ) -> Result<(), SemanticError> {
        let mut temp_unifier = Unifier::new();
        temp_unifier.unify(expected, actual, span).map_err(|_| {
            SemanticError::new(
                format!(
                    "Type mismatch: expected {}, got {}",
                    format_type(expected),
                    format_type(actual)
                ),
                span,
            )
        })
    }

    #[allow(clippy::only_used_in_recursion)]
    fn substitute_type_param(&self, type_: &Type, param: &str, replacement: &Type) -> Type {
        match type_ {
            Type::Variable(var) if var == param => replacement.clone(),
            Type::Generic(var) if var == param => replacement.clone(),
            Type::Named(name, args) if name == param && args.is_empty() => replacement.clone(),
            Type::Named(name, args) => Type::Named(
                name.clone(),
                args.iter()
                    .map(|a| self.substitute_type_param(a, param, replacement))
                    .collect(),
            ),
            Type::List(inner) => Type::List(Box::new(self.substitute_type_param(
                inner,
                param,
                replacement,
            ))),
            Type::Set(inner) => Type::Set(Box::new(self.substitute_type_param(
                inner,
                param,
                replacement,
            ))),
            Type::Map(key, value) => Type::Map(
                Box::new(self.substitute_type_param(key, param, replacement)),
                Box::new(self.substitute_type_param(value, param, replacement)),
            ),
            Type::Optional(inner) => Type::Optional(Box::new(self.substitute_type_param(
                inner,
                param,
                replacement,
            ))),
            Type::Function {
                params,
                returns,
                default_count,
            } => Type::Function {
                params: params
                    .iter()
                    .map(|p| self.substitute_type_param(p, param, replacement))
                    .collect(),
                returns: Box::new(self.substitute_type_param(returns, param, replacement)),
                default_count: *default_count,
            },
            Type::Reference(inner) => Type::Reference(Box::new(self.substitute_type_param(
                inner,
                param,
                replacement,
            ))),
            _ => type_.clone(),
        }
    }

    fn substitute_type_params(
        &self,
        type_: &Type,
        params: &[(String, Vec<String>)],
        replacements: &[Type],
    ) -> Type {
        let mut result = type_.clone();
        for ((param_name, _), replacement) in params.iter().zip(replacements) {
            result = self.substitute_type_param(&result, param_name, replacement);
        }
        result
    }

    fn check_method_compatibility(
        &self,
        interface_sig: &MethodSig,
        class_sig: &MethodSig,
        span: Span,
    ) -> Result<(), SemanticError> {
        let mut unifier = Unifier::new();
        // Unify return types
        unifier
            .unify(&interface_sig.return_type, &class_sig.return_type, span)
            .map_err(|e| {
                SemanticError::with_help(
                    format!(
                        "Return type mismatch in interface implementation: {}",
                        e.message
                    ),
                    span,
                    "The class method's return type must match the interface method's return type",
                )
            })?;
        // Unify params
        if interface_sig.params.len() != class_sig.params.len() {
            return Err(SemanticError::with_help(
                format!(
                    "Parameter count mismatch: interface expects {} parameter{}, class provides {}",
                    interface_sig.params.len(),
                    if interface_sig.params.len() == 1 {
                        ""
                    } else {
                        "s"
                    },
                    class_sig.params.len()
                ),
                span,
                "The class method must have the same number of parameters as the interface method",
            ));
        }
        for (i, (int_param, class_param)) in interface_sig
            .params
            .iter()
            .zip(&class_sig.params)
            .enumerate()
        {
            unifier.unify(int_param, class_param, span).map_err(|e| {
                SemanticError::with_help(
                    format!(
                        "Parameter {} type mismatch in interface implementation: {}",
                        i, e.message
                    ),
                    span,
                    format!(
                        "Parameter {} must have type {} to match the interface",
                        i,
                        format_type(int_param)
                    ),
                )
            })?;
        }
        Ok(())
    }

    fn get_instantiated_constructor_type(
        &mut self,
        name: &str,
        type_args: &[TypeNode],
        span: Span,
    ) -> Result<Type, SemanticError> {
        let symbol = self
            .symbol_table
            .get_cloned(name)
            .ok_or_else(|| self.undefined_symbol_error("type", name, span))?;
        if symbol.kind != SymbolKind::Class {
            return Err(SemanticError::with_help(
                format!("'{}' is not a class", name),
                span,
                format!(
                    "'{}' is a {}not a class. Only classes can be instantiated with .new()",
                    name,
                    match symbol.kind {
                        SymbolKind::Function => "a function, ",
                        SymbolKind::Variable => "a variable, ",
                        SymbolKind::Interface => "an interface, ",
                        SymbolKind::Enum => "an enum, ",
                        SymbolKind::Constant => "a constant, ",
                        SymbolKind::Import => "an import, ",
                        SymbolKind::Type => "a type parameter, ",
                        _ => "",
                    }
                ),
            ));
        }
        let resolved_args = type_args
            .iter()
            .map(|arg| self.resolve_type(arg))
            .collect::<Result<Vec<_>, _>>()?;
        if resolved_args.len() != symbol.type_params.len() {
            return Err(SemanticError::with_help(
                format!(
                    "Expected {} type argument(s) for '{}', got {}",
                    symbol.type_params.len(),
                    name,
                    resolved_args.len()
                ),
                span,
                format!(
                    "Class '{}' requires {} type parameter(s). Example: {}<{}>",
                    name,
                    symbol.type_params.len(),
                    name,
                    symbol
                        .type_params
                        .iter()
                        .map(|(p, _)| p.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
        let new_sig = symbol.methods.get("new").ok_or_else(|| SemanticError::with_help(
            format!("Class '{}' has no constructor", name),
            span,
            format!(
                "Class '{}' does not have a .new() method. Ensure the class has fields or a constructor defined.",
                name
            ),
        ))?;
        let substituted_params = new_sig
            .params
            .iter()
            .map(|p| self.substitute_type_params(p, &symbol.type_params, &resolved_args))
            .collect();
        let substituted_returns =
            self.substitute_type_params(&new_sig.return_type, &symbol.type_params, &resolved_args);
        Ok(Type::Function {
            params: substituted_params,
            returns: Box::new(substituted_returns),
            default_count: 0,
        })
    }

    fn type_implements_interface(&self, type_: &Type, interface_name: &str) -> bool {
        self.type_implements_interface_with_args(type_, interface_name, &[])
    }

    fn type_implements_interface_with_args(
        &self,
        type_: &Type,
        interface_name: &str,
        interface_args: &[Type],
    ) -> bool {
        match type_ {
            Type::Named(name, _) => {
                self.type_implements_interface_with_named(name, interface_name, interface_args)
            }
            Type::Primitive(prim) => {
                self.type_implements_interface_with_primitive(prim, interface_name)
            }
            Type::Variable(var) | Type::Generic(var) => {
                self.type_implements_interface_with_variable(var, interface_name, interface_args)
            }
            _ => false,
        }
    }

    fn type_implements_interface_with_named(
        &self,
        name: &str,
        interface_name: &str,
        interface_args: &[Type],
    ) -> bool {
        if let Some(symbol) = self.symbol_table.lookup(name) {
            if let Some((stored_args, _)) = symbol.interfaces.get(interface_name) {
                if interface_args.is_empty() {
                    return true;
                }
                // Verify the interface exists and arity matches
                if let Some(interface_symbol) = self.symbol_table.lookup(interface_name)
                    && interface_symbol.type_params.len() != interface_args.len()
                {
                    return false;
                }
                // Compare stored concrete type arguments with provided interface_args
                stored_args == interface_args
            } else {
                false
            }
        } else {
            false
        }
    }

    fn type_implements_interface_with_primitive(
        &self,
        prim: &PrimitiveType,
        interface_name: &str,
    ) -> bool {
        match prim {
            PrimitiveType::Str => {
                interface_name == "Stringable"
                    || interface_name == "Equatable"
                    || interface_name == "Comparable"
                    || interface_name == "Hashable"
                    || interface_name == "Error"
            }
            PrimitiveType::Int => {
                matches!(
                    interface_name,
                    "Stringable" | "Equatable" | "Comparable" | "Hashable"
                )
            }
            PrimitiveType::Float => {
                matches!(
                    interface_name,
                    "Stringable" | "Equatable" | "Comparable" | "Hashable"
                )
            }
            PrimitiveType::Bool => {
                matches!(interface_name, "Stringable" | "Equatable" | "Hashable")
            }
            PrimitiveType::Char => {
                matches!(
                    interface_name,
                    "Stringable" | "Equatable" | "Comparable" | "Hashable"
                )
            }
            _ => false,
        }
    }

    fn type_implements_interface_with_variable(
        &self,
        var: &str,
        interface_name: &str,
        interface_args: &[Type],
    ) -> bool {
        if let Some(bounds) = self.current_bounds.get(var) {
            bounds.iter().any(|(bound_name, bound_args)| {
                bound_name == interface_name
                    && (interface_args.is_empty() || bound_args == interface_args)
            })
        } else {
            false
        }
    }

    fn get_builtin_interface_method(
        &self,
        interface_name: &str,
        method_name: &str,
    ) -> Option<MethodSig> {
        match interface_name {
            "Stringable" => match method_name {
                "to_string" => Some(MethodSig {
                    params: vec![],
                    return_type: Type::Primitive(PrimitiveType::Str),
                    is_static: false,
                }),
                _ => None,
            },
            "Equatable" => match method_name {
                "eq" => Some(MethodSig {
                    params: vec![Type::Generic("Self".to_string())],
                    return_type: Type::Primitive(PrimitiveType::Bool),
                    is_static: false,
                }),
                _ => None,
            },
            "Comparable" => match method_name {
                "cmp" => Some(MethodSig {
                    params: vec![Type::Generic("Self".to_string())],
                    return_type: Type::Primitive(PrimitiveType::Int),
                    is_static: false,
                }),
                _ => None,
            },
            "Hashable" => match method_name {
                "hash" => Some(MethodSig {
                    params: vec![],
                    return_type: Type::Primitive(PrimitiveType::Int),
                    is_static: false,
                }),
                _ => None,
            },
            "Error" => match method_name {
                "message" => Some(MethodSig {
                    params: vec![],
                    return_type: Type::Primitive(PrimitiveType::Str),
                    is_static: false,
                }),
                _ => None,
            },
            _ => None,
        }
    }

    fn get_named_method_sig(
        &self,
        name: &str,
        args: &[Type],
        method_name: &str,
    ) -> Option<MethodSig> {
        let symbol = self.symbol_table.lookup(name)?;
        if let Some(sig) = symbol.methods.get(method_name) {
            return Some(if args.is_empty() {
                sig.clone()
            } else {
                self.substitute_method_sig(sig, &symbol.type_params, args)
            });
        }
        self.find_interface_method(&symbol, method_name)
    }

    fn get_variable_generic_method_sig(&self, var: &str, method_name: &str) -> Option<MethodSig> {
        let bounds = self.current_bounds.get(var)?;
        for (bound_name, bound_args) in bounds {
            if let Some(sig) = self.get_builtin_interface_method(bound_name, method_name) {
                return Some(sig);
            }
            if let Some(interface_symbol) = self.symbol_table.lookup(bound_name)
                && let Some(sig) = self.find_interface_method(&interface_symbol, method_name)
            {
                if bound_args.is_empty() {
                    return Some(sig);
                }

                return Some(self.substitute_method_sig(
                    &sig,
                    &interface_symbol.type_params,
                    bound_args,
                ));
            }
        }
        None
    }

    fn get_primitive_method_sig(
        &self,
        prim: &PrimitiveType,
        method_name: &str,
    ) -> Option<MethodSig> {
        use PrimitiveType::{Bool, Char, Float, Int, Str};
        let resolver = match prim {
            Int => Some(Self::get_int_method_sig as fn(&Self, &str) -> Option<MethodSig>),
            Float => Some(Self::get_float_method_sig as fn(&Self, &str) -> Option<MethodSig>),
            Str => Some(Self::get_string_method_sig as fn(&Self, &str) -> Option<MethodSig>),
            Bool => Some(Self::get_bool_method_sig as fn(&Self, &str) -> Option<MethodSig>),
            Char => Some(Self::get_char_method_sig as fn(&Self, &str) -> Option<MethodSig>),
            PrimitiveType::Void | PrimitiveType::Auto => None,
        };
        resolver.and_then(|resolve| resolve(self, method_name))
    }

    fn make_instance_method_sig(params: Vec<Type>, return_type: Type) -> MethodSig {
        MethodSig {
            params,
            return_type,
            is_static: false,
        }
    }

    fn make_eq_method_sig(param_type: PrimitiveType) -> MethodSig {
        Self::make_instance_method_sig(
            vec![Type::Primitive(param_type)],
            Type::Primitive(PrimitiveType::Bool),
        )
    }

    fn make_cmp_method_sig(param_type: PrimitiveType) -> MethodSig {
        Self::make_instance_method_sig(
            vec![Type::Primitive(param_type)],
            Type::Primitive(PrimitiveType::Int),
        )
    }

    fn make_hash_method_sig() -> MethodSig {
        Self::make_instance_method_sig(vec![], Type::Primitive(PrimitiveType::Int))
    }

    fn make_to_string_method_sig() -> MethodSig {
        Self::make_instance_method_sig(vec![], Type::Primitive(PrimitiveType::Str))
    }

    fn make_str_parse_result_method_sig(value_type: PrimitiveType) -> MethodSig {
        Self::make_instance_method_sig(
            vec![],
            Type::Result(
                Box::new(Type::Primitive(value_type)),
                Box::new(Type::Primitive(PrimitiveType::Str)),
            ),
        )
    }

    fn get_int_method_sig(&self, method_name: &str) -> Option<MethodSig> {
        match method_name {
            "to_string" => Some(Self::make_to_string_method_sig()),
            "to_float" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Float),
            )),
            "to_int" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Int),
            )),
            "to_char" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Char),
            )),
            "eq" => Some(Self::make_eq_method_sig(PrimitiveType::Int)),
            "cmp" => Some(Self::make_cmp_method_sig(PrimitiveType::Int)),
            "hash" => Some(Self::make_hash_method_sig()),
            _ => None,
        }
    }

    fn get_float_method_sig(&self, method_name: &str) -> Option<MethodSig> {
        match method_name {
            "to_string" => Some(Self::make_to_string_method_sig()),
            "to_int" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Int),
            )),
            "to_float" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Float),
            )),
            "eq" => Some(Self::make_eq_method_sig(PrimitiveType::Float)),
            "cmp" => Some(Self::make_cmp_method_sig(PrimitiveType::Float)),
            "hash" => Some(Self::make_hash_method_sig()),
            _ => None,
        }
    }

    fn get_string_method_sig(&self, method_name: &str) -> Option<MethodSig> {
        match method_name {
            "to_string" | "message" => Some(Self::make_to_string_method_sig()),
            "length" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Int),
            )),
            "to_int" => Some(Self::make_str_parse_result_method_sig(PrimitiveType::Int)),
            "to_float" => Some(Self::make_str_parse_result_method_sig(PrimitiveType::Float)),
            "to_char" => Some(Self::make_str_parse_result_method_sig(PrimitiveType::Char)),
            "eq" => Some(Self::make_eq_method_sig(PrimitiveType::Str)),
            "cmp" => Some(Self::make_cmp_method_sig(PrimitiveType::Str)),
            "hash" => Some(Self::make_hash_method_sig()),
            _ => None,
        }
    }

    fn get_bool_method_sig(&self, method_name: &str) -> Option<MethodSig> {
        match method_name {
            "to_string" => Some(Self::make_to_string_method_sig()),
            "to_int" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Int),
            )),
            "to_float" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Float),
            )),
            "eq" => Some(Self::make_eq_method_sig(PrimitiveType::Bool)),
            "hash" => Some(Self::make_hash_method_sig()),
            _ => None,
        }
    }

    fn get_char_method_sig(&self, method_name: &str) -> Option<MethodSig> {
        match method_name {
            "to_string" => Some(Self::make_to_string_method_sig()),
            "to_int" => Some(Self::make_str_parse_result_method_sig(PrimitiveType::Int)),
            "to_char" => Some(Self::make_instance_method_sig(
                vec![],
                Type::Primitive(PrimitiveType::Char),
            )),
            "eq" => Some(Self::make_eq_method_sig(PrimitiveType::Char)),
            "cmp" => Some(Self::make_cmp_method_sig(PrimitiveType::Char)),
            "hash" => Some(Self::make_hash_method_sig()),
            _ => None,
        }
    }

    fn get_list_method_sig(&self, elem_type: &Type, method_name: &str) -> Option<MethodSig> {
        match method_name {
            "push_back" => Some(MethodSig {
                params: vec![elem_type.clone()],
                return_type: Type::Void,
                is_static: false,
            }),
            "pop_back" => Some(MethodSig {
                params: vec![],
                return_type: Type::Optional(Box::new(elem_type.clone())),
                is_static: false,
            }),
            "push" => Some(MethodSig {
                params: vec![elem_type.clone()],
                return_type: Type::Void,
                is_static: false,
            }),
            "pop" => Some(MethodSig {
                params: vec![],
                return_type: Type::Optional(Box::new(elem_type.clone())),
                is_static: false,
            }),
            "get" => Some(MethodSig {
                params: vec![Type::Primitive(PrimitiveType::Int)],
                return_type: Type::Optional(Box::new(elem_type.clone())),
                is_static: false,
            }),
            "is_empty" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Bool),
                is_static: false,
            }),
            "size" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Int),
                is_static: false,
            }),
            "to_string" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Str),
                is_static: false,
            }),
            _ => None,
        }
    }

    fn get_map_method_sig(
        &self,
        key_type: &Type,
        value_type: &Type,
        method_name: &str,
    ) -> Option<MethodSig> {
        match method_name {
            "put" => Some(MethodSig {
                params: vec![key_type.clone(), value_type.clone()],
                return_type: Type::Void,
                is_static: false,
            }),
            "get" => Some(MethodSig {
                params: vec![key_type.clone()],
                return_type: Type::Optional(Box::new(value_type.clone())),
                is_static: false,
            }),
            "get_keys" => Some(MethodSig {
                params: vec![],
                return_type: Type::List(Box::new(key_type.clone())),
                is_static: false,
            }),
            "get_values" => Some(MethodSig {
                params: vec![],
                return_type: Type::List(Box::new(value_type.clone())),
                is_static: false,
            }),
            "get_pairs" => Some(MethodSig {
                params: vec![],
                return_type: Type::List(Box::new(Type::Tuple(
                    Box::new(key_type.clone()),
                    Box::new(value_type.clone()),
                ))),
                is_static: false,
            }),
            "contains" => Some(MethodSig {
                params: vec![key_type.clone()],
                return_type: Type::Primitive(PrimitiveType::Bool),
                is_static: false,
            }),
            "remove" => Some(MethodSig {
                params: vec![key_type.clone()],
                return_type: Type::Optional(Box::new(value_type.clone())),
                is_static: false,
            }),
            "size" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Int),
                is_static: false,
            }),
            "is_empty" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Bool),
                is_static: false,
            }),
            "to_string" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Str),
                is_static: false,
            }),
            _ => None,
        }
    }

    fn get_set_method_sig(&self, elem_type: &Type, method_name: &str) -> Option<MethodSig> {
        match method_name {
            "add" => Some(MethodSig {
                params: vec![elem_type.clone()],
                return_type: Type::Void,
                is_static: false,
            }),
            "remove" => Some(MethodSig {
                params: vec![elem_type.clone()],
                return_type: Type::Primitive(PrimitiveType::Bool),
                is_static: false,
            }),
            "contains" => Some(MethodSig {
                params: vec![elem_type.clone()],
                return_type: Type::Primitive(PrimitiveType::Bool),
                is_static: false,
            }),
            "size" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Int),
                is_static: false,
            }),
            "is_empty" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Bool),
                is_static: false,
            }),
            "to_string" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Str),
                is_static: false,
            }),
            "to_list" => Some(MethodSig {
                params: vec![],
                return_type: Type::List(Box::new(elem_type.clone())),
                is_static: false,
            }),
            _ => None,
        }
    }

    fn get_optional_method_sig(&self, method_name: &str) -> Option<MethodSig> {
        fn bool_method_sig() -> MethodSig {
            MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Bool),
                is_static: false,
            }
        }

        match method_name {
            "to_string" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Str),
                is_static: false,
            }),
            "is_some" | "is_none" => Some(bool_method_sig()),
            _ => None,
        }
    }

    fn get_result_method_sig(&self, method_name: &str) -> Option<MethodSig> {
        fn bool_method_sig() -> MethodSig {
            MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Bool),
                is_static: false,
            }
        }

        match method_name {
            "to_string" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Str),
                is_static: false,
            }),
            "is_ok" | "is_err" => Some(bool_method_sig()),
            _ => None,
        }
    }

    fn get_tuple_method_sig(&self, method_name: &str) -> Option<MethodSig> {
        match method_name {
            "to_string" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Str),
                is_static: false,
            }),
            "new" => Some(MethodSig {
                params: vec![],
                return_type: Type::Tuple(
                    Box::new(Type::Primitive(PrimitiveType::Int)),
                    Box::new(Type::Primitive(PrimitiveType::Str)),
                ),
                is_static: true,
            }),
            _ => None,
        }
    }

    pub(crate) fn get_method_sig(&self, type_: &Type, method_name: &str) -> Option<MethodSig> {
        match type_ {
            Type::Named(name, args) => self.get_named_method_sig(name, args, method_name),
            Type::Variable(var) | Type::Generic(var) => {
                self.get_variable_generic_method_sig(var, method_name)
            }
            Type::Primitive(prim) => self.get_primitive_method_sig(prim, method_name),
            Type::List(elem_type) => self.get_list_method_sig(elem_type, method_name),
            Type::Map(key_type, value_type) => {
                self.get_map_method_sig(key_type, value_type, method_name)
            }
            Type::Set(elem_type) => self.get_set_method_sig(elem_type, method_name),
            Type::Optional(_) => self.get_optional_method_sig(method_name),
            Type::Result(_, _) => self.get_result_method_sig(method_name),
            Type::Tuple(_, _) => self.get_tuple_method_sig(method_name),
            Type::Reference(inner) => self.get_method_sig(inner, method_name),
            _ => None,
        }
    }

    fn find_interface_method(
        &self,
        symbol: &crate::semantics::Symbol,
        method_name: &str,
    ) -> Option<MethodSig> {
        for (_, interface_methods) in symbol.interfaces.values() {
            if let Some(sig) = interface_methods.get(method_name) {
                return Some(sig.clone());
            }
        }
        None
    }

    fn substitute_method_sig(
        &self,
        sig: &MethodSig,
        type_params: &[(String, Vec<String>)],
        args: &[Type],
    ) -> MethodSig {
        let substituted_params = sig
            .params
            .iter()
            .map(|p| self.substitute_type_params(p, type_params, args))
            .collect();
        let substituted_return = self.substitute_type_params(&sig.return_type, type_params, args);
        MethodSig {
            params: substituted_params,
            return_type: substituted_return,
            is_static: false,
        }
    }

    fn resolve_binary_operator(
        &self,
        left_type: &Type,
        right_type: &Type,
        op: &BinaryOp,
    ) -> Option<Type> {
        if matches!(op, BinaryOp::In) {
            return self.resolve_in_binary_operator(left_type, right_type);
        }

        if left_type != right_type {
            return None;
        }

        self.resolve_equal_type_binary_operator(left_type, right_type, op)
    }

    fn resolve_in_binary_operator(&self, left_type: &Type, right_type: &Type) -> Option<Type> {
        match right_type {
            Type::List(_) | Type::Set(_) => Some(Type::Primitive(crate::ast::PrimitiveType::Bool)),
            Type::Map(key_type, _) => {
                if left_type == key_type.as_ref() {
                    Some(Type::Primitive(crate::ast::PrimitiveType::Bool))
                } else {
                    None
                }
            }
            Type::Primitive(PrimitiveType::Str) => {
                if matches!(
                    left_type,
                    Type::Primitive(PrimitiveType::Char | PrimitiveType::Str)
                ) {
                    Some(Type::Primitive(crate::ast::PrimitiveType::Bool))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn resolve_equal_type_binary_operator(
        &self,
        left_type: &Type,
        right_type: &Type,
        op: &BinaryOp,
    ) -> Option<Type> {
        match op {
            BinaryOp::Add => self.resolve_add_binary_operator(left_type),
            BinaryOp::Subtract
            | BinaryOp::Multiply
            | BinaryOp::Divide
            | BinaryOp::Modulo
            | BinaryOp::Exponent => self.resolve_numeric_binary_operator(left_type),
            BinaryOp::Equal | BinaryOp::NotEqual => {
                Some(Type::Primitive(crate::ast::PrimitiveType::Bool))
            }
            BinaryOp::Less | BinaryOp::LessEqual | BinaryOp::Greater | BinaryOp::GreaterEqual => {
                self.resolve_comparison_binary_operator(left_type)
            }
            BinaryOp::LogicalAnd | BinaryOp::LogicalOr => {
                self.resolve_logical_binary_operator(left_type, right_type)
            }
            _ => None,
        }
    }

    fn resolve_add_binary_operator(&self, left_type: &Type) -> Option<Type> {
        if matches!(
            left_type,
            Type::Primitive(PrimitiveType::Str | PrimitiveType::Int | PrimitiveType::Float)
                | Type::List(_)
                | Type::Map(_, _)
                | Type::Set(_)
        ) {
            Some(left_type.clone())
        } else {
            None
        }
    }

    fn resolve_numeric_binary_operator(&self, left_type: &Type) -> Option<Type> {
        if matches!(
            left_type,
            Type::Primitive(PrimitiveType::Int | PrimitiveType::Float)
        ) {
            Some(left_type.clone())
        } else {
            None
        }
    }

    fn resolve_comparison_binary_operator(&self, left_type: &Type) -> Option<Type> {
        if matches!(
            left_type,
            Type::Primitive(PrimitiveType::Int | PrimitiveType::Float | PrimitiveType::Str)
        ) || self.type_implements_interface(left_type, "Comparable")
        {
            Some(Type::Primitive(crate::ast::PrimitiveType::Bool))
        } else {
            None
        }
    }

    fn resolve_logical_binary_operator(&self, left_type: &Type, right_type: &Type) -> Option<Type> {
        if matches!(left_type, Type::Primitive(PrimitiveType::Bool))
            && matches!(right_type, Type::Primitive(PrimitiveType::Bool))
        {
            Some(Type::Primitive(crate::ast::PrimitiveType::Bool))
        } else {
            None
        }
    }
}

/// Infer missing type parameters from already-inferred parameters' trait bounds.
///
/// This implements the three-level nested loop that scans each missing type parameter's
/// bounds to see if one of the already-inferred type parameters' bounds contains a
/// reference to the missing parameter at a specific position, allowing inference of the
/// concrete type argument at that position.
pub(crate) fn infer_missing_type_params_from_bounds(
    type_params: &[(String, Vec<crate::ast::TraitBound>)],
    substitutions: &mut std::collections::HashMap<String, Type>,
) {
    for (missing_param_name, _) in type_params {
        if substitutions.contains_key(missing_param_name) {
            continue;
        }

        if let Some(inferred_type) =
            infer_missing_param_from_bounds(missing_param_name, type_params, substitutions)
        {
            substitutions.insert(missing_param_name.clone(), inferred_type);
        }
    }
}

fn infer_missing_param_from_bounds(
    missing_param_name: &str,
    type_params: &[(String, Vec<crate::ast::TraitBound>)],
    substitutions: &std::collections::HashMap<String, Type>,
) -> Option<Type> {
    type_params
        .iter()
        .find_map(|(owner_param_name, owner_bounds)| {
            substitutions
                .get(owner_param_name)
                .and_then(owner_concrete_type_args)
                .filter(|owner_type_args| !owner_type_args.is_empty())
                .and_then(|owner_type_args| {
                    infer_bound_type_arg(missing_param_name, owner_bounds, owner_type_args)
                })
        })
}

fn owner_concrete_type_args(owner_concrete_type: &Type) -> Option<&[Type]> {
    match owner_concrete_type {
        Type::Named(_, args) => Some(args.as_slice()),
        Type::Reference(inner) => match inner.as_ref() {
            Type::Named(_, args) => Some(args.as_slice()),
            _ => None,
        },
        _ => None,
    }
}

fn infer_bound_type_arg(
    missing_param_name: &str,
    owner_bounds: &[crate::ast::TraitBound],
    owner_type_args: &[Type],
) -> Option<Type> {
    for bound in owner_bounds {
        for (idx, bound_type_arg) in bound.type_params.iter().enumerate() {
            if let TypeKind::Named(bound_name, _) = &bound_type_arg.kind
                && bound_name == missing_param_name
                && let Some(concrete_arg) = owner_type_args.get(idx)
            {
                return Some(concrete_arg.clone());
            }
        }
    }
    None
}

/// Compute the Levenshtein edit distance between two strings.
// Use the existing edit_distance from symbol_table instead of duplicating
use crate::semantics::symbol_table::{
    calculate_similarity_threshold, edit_distance as levenshtein_distance,
};

/// Suggest a fix for `<name>.new()`-style usage on built-in collection-like types.
/// Returns a help message if `name` is a built-in container type that has no
/// `.new()` constructor and should be created with a literal instead.
fn collection_new_hint(name: &str) -> Option<&'static str> {
    match name {
        "list" => Some(
            "List has no '.new()' constructor. Use '[]' for an empty list or '[1, 2, 3]' for elements. For example: list<int> my_list = []",
        ),
        "map" => Some(
            "Map has no '.new()' constructor. Use '{}' for an empty map or '{\"key\": \"value\"}' for entries. For example: map<string, int> my_map = {}",
        ),
        "set" => Some(
            "Set has no '.new()' constructor. Use '{}' for an empty set or '{1, 2, 3}' for elements. For example: set<int> my_set = {}",
        ),
        "optional" => Some(
            "Optional has no '.new()' constructor. Use 'none' for an empty value or 'some(value)' for a value. For example: optional<int> my_opt = none",
        ),
        "result" => Some(
            "Result has no '.new()' constructor. Use 'ok(value)' or 'err(message)' to construct one. For example: result<int, string> my_res = ok(42)",
        ),
        _ => None,
    }
}
