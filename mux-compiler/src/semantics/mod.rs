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

/// The number of type arguments a built-in generic type requires, or `None` for
/// a name that is not a built-in generic. Used to reject a built-in collection
/// or wrapper written without its type arguments (issue #289).
fn builtin_generic_arity(name: &str) -> Option<usize> {
    match name {
        "list" | "set" | "optional" => Some(1),
        "map" | "result" | "tuple" => Some(2),
        _ => None,
    }
}

/// A `help` line showing a correctly parameterized form of a built-in generic,
/// for the "requires type argument(s)" diagnostic (issue #289).
fn missing_type_args_help(name: &str) -> String {
    let example = match name {
        "list" => "list<int>",
        "set" => "set<int>",
        "map" => "map<string, int>",
        "tuple" => "tuple<int, string>",
        "optional" => "optional<int>",
        "result" => "result<int, Error>",
        _ => "Type<...>",
    };
    format!("Add the type argument(s) in angle brackets, e.g. {example}")
}

pub struct SemanticAnalyzer {
    pub(super) symbol_table: SymbolTable,
    current_bounds: std::collections::HashMap<String, GenericBounds>,
    /// Type parameters of the declaration whose signature is being resolved.
    /// A signature is resolved before its parameters become type variables, so
    /// this is how a name in it is known to be one rather than an unknown type.
    signature_type_params: std::collections::HashSet<String>,
    errors: Vec<SemanticError>,
    is_in_static_method: bool,
    pub current_self_type: Option<Type>,
    pub module_resolver: Option<Rc<RefCell<crate::module_resolver::ModuleResolver>>>,
    /// Symbols exported by each imported module, keyed by module name.
    /// Ordered: codegen walks it when resolving a class or function that came
    /// from an import, and the order it finds them in reaches the emitted IR.
    pub imported_symbols:
        std::collections::BTreeMap<String, std::collections::HashMap<String, Symbol>>,
    /// Every module's AST, keyed by module name. Ordered, because codegen
    /// concatenates these to build the compilation unit, so this map's
    /// iteration order is the order module code is emitted in (issue #344).
    pub all_module_asts: std::collections::BTreeMap<String, Vec<AstNode>>,
    pub module_dependencies: Vec<String>,
    pub(super) current_file: Option<std::path::PathBuf>, // Track current file for relative imports
    pub lambda_captures: std::collections::HashMap<Span, Vec<(String, Type)>>, // Track captured variables for each lambda
    pub current_return_type: Option<Type>, // Track current function/lambda return type
    pub current_class_type_params: Option<Vec<(String, GenericBounds)>>, // Track class-level type params with bounds for method analysis
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
            signature_type_params: std::collections::HashSet::new(),
            errors: Vec::new(),
            is_in_static_method: false,
            current_self_type: None,
            module_resolver: None,
            imported_symbols: std::collections::BTreeMap::new(),
            all_module_asts: std::collections::BTreeMap::new(),
            module_dependencies: Vec::new(),
            current_file: None,
            lambda_captures: std::collections::HashMap::new(),
            current_return_type: None,
            current_class_type_params: None,
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
    ) -> &std::collections::BTreeMap<String, std::collections::HashMap<String, Symbol>> {
        &self.imported_symbols
    }

    pub fn all_module_asts(&self) -> &std::collections::BTreeMap<String, Vec<AstNode>> {
        &self.all_module_asts
    }

    /// Generate helpful context for binary operator type mismatches.
    fn binary_op_help(&self, left: &Type, right: &Type, op: &crate::ast::BinaryOp) -> String {
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
            // Matching types that still failed to resolve `==`/`!=`: the type
            // has no comparison rather than the two sides disagreeing, so the
            // "make the types match" advice below would be misleading.
            _ if left == right
                && matches!(op, crate::ast::BinaryOp::Equal | crate::ast::BinaryOp::NotEqual) =>
            {
                self.equality_op_help(left, op)
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

    /// Suggest a way forward for `==`/`!=` on a type that has no comparison,
    /// rather than restating the two operand types the user can already see.
    fn equality_op_help(&self, type_: &Type, op: &crate::ast::BinaryOp) -> String {
        let op = format_binary_op(op);
        match type_ {
            // Only suggest dereferencing when it would actually compile. For a
            // `&Point` it would just produce a second error, so say what is
            // really wrong instead.
            Type::Reference(inner) if self.resolve_equality_binary_operator(inner).is_some() => {
                format!(
                    "References cannot be compared with '{}'. Dereference both sides to compare the {} values they point at, as in '*a {} *b'.",
                    op,
                    format_type(inner),
                    op
                )
            }
            Type::Reference(inner) => format!(
                "References cannot be compared with '{}', and neither can the {} values they point at.",
                op,
                format_type(inner)
            ),
            // A user enum declared as `optional`/`result` collides with the
            // built-in of that name, which is a heap value rather than an inline
            // struct, so it never gets the structural compare a user enum has.
            // Saying "class or interface" here would be plainly wrong.
            Type::Named(name, _) if matches!(name.as_str(), "optional" | "result") => format!(
                "'{}' is the name of a built-in type. An enum declared with that name does not get the structural comparison user enums have; rename it.",
                name
            ),
            Type::Named(name, _) => format!(
                "'{}' is a class or an interface, and only enums compare structurally. Give '{}' a method that compares two values and call it directly, or model the type as an enum if it is a fixed set of alternatives.",
                name, name
            ),
            Type::Generic(name) | Type::Variable(name) => format!(
                "'{}' is a type parameter with no 'Equatable' bound, so '{}' is not available on it. Declare the bound, as in <{} is Equatable>.",
                name, op, name
            ),
            Type::Function { .. } => {
                format!("Functions cannot be compared with '{}'.", op)
            }
            Type::Void | Type::Primitive(crate::ast::PrimitiveType::Void) => format!(
                "A call that returns void produces no value, so there is nothing for '{}' to compare.",
                op
            ),
            Type::Never => format!(
                "This expression never produces a value, so there is nothing for '{}' to compare.",
                op
            ),
            _ => format!(
                "Values of type {} cannot be compared with '{}'.",
                format_type(type_),
                op
            ),
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
        let Some(func_node) = self.function_node_by_name(func_name) else {
            return;
        };

        infer_missing_type_params_from_bounds(&func_node.type_params, substitutions);
    }

    /// The AST node of a top-level function by name, across every loaded module.
    fn function_node_by_name(&self, func_name: &str) -> Option<&crate::ast::FunctionNode> {
        self.all_module_asts
            .values()
            .flatten()
            .find_map(|node| match node {
                AstNode::Function(func) if func.name == func_name => Some(func),
                _ => None,
            })
    }

    /// Check each inferred type argument against the bounds its parameter
    /// declares.
    ///
    /// Nothing did this before: `<T is Stringable>` accepted any type at all and
    /// the missing capability surfaced in codegen as an internal error, or - for
    /// `Comparable` - as code that compiled and produced a nonsense answer
    /// (issue #361).
    fn check_declared_bounds(
        &self,
        func_name: &str,
        substitutions: &std::collections::HashMap<String, Type>,
        span: Span,
    ) -> Result<(), SemanticError> {
        // Read the bounds off the function symbol rather than the AST: the AST
        // map holds imported modules only, so a generic declared in the file
        // being compiled would never be found.
        let Some(symbol) = self.symbol_table.lookup(func_name) else {
            return Ok(());
        };
        if symbol.kind != SymbolKind::Function {
            return Ok(());
        }
        for (param_name, bound_names) in &symbol.type_params {
            let Some(concrete) = substitutions.get(param_name) else {
                continue;
            };
            // A parameter still standing for a type variable is not yet known;
            // the check belongs to whatever instantiation resolves it.
            if matches!(concrete, Type::Generic(_) | Type::Variable(_)) {
                continue;
            }
            for bound in bound_names {
                if self.type_implements_interface(concrete, bound) {
                    continue;
                }
                return Err(SemanticError::with_help(
                    format!("'{}' does not satisfy '{}'", format_type(concrete), bound),
                    span,
                    Self::unsatisfied_bound_help(bound, concrete, param_name),
                ));
            }
        }
        Ok(())
    }

    /// Say what the type is missing, for the four built-in capabilities where
    /// that is knowable, rather than only restating the bound.
    fn unsatisfied_bound_help(bound: &str, concrete: &Type, param_name: &str) -> String {
        let detail = match bound {
            "Stringable" => "has no 'to_string' method",
            "Equatable" => "cannot be compared with '=='",
            "Comparable" => "cannot be ordered with '<'",
            "Hashable" => "cannot be used as a map key or set member",
            _ => {
                return format!("'{}' is required to satisfy '{}' here.", param_name, bound);
            }
        };
        format!(
            "'{}' is bound by '{}', but {} {}.",
            param_name,
            bound,
            format_type(concrete),
            detail
        )
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
        // One comprehensive generic-arity pass over every type annotation, now
        // that all type symbols and their type parameters are registered (#303).
        self.validate_all_type_arities(ast);
        self.analyze_nodes(ast, files);
        // Two-pass analysis (hoist/collection, then body analysis) can resolve
        // the same field or signature type twice, so a type error on it is
        // recorded twice. Collapse exact duplicates - identical message and
        // span - so each distinct problem is reported once.
        let mut errors = std::mem::take(&mut self.errors);
        let mut seen = HashSet::new();
        errors.retain(|e| seen.insert((e.message.clone(), e.span)));
        errors
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
                self.resolve_named_type(name, type_args, type_node.span)
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

    /// Resolve a `TypeKind::Named` annotation: a generic type parameter, a
    /// built-in wrapper (`optional`/`result`), or a user class/enum/interface.
    /// Split out of `resolve_type` to keep that dispatcher's cognitive
    /// complexity within the gate (SonarQube rust:S3776).
    fn resolve_named_type(
        &self,
        name: &str,
        type_args: &[TypeNode],
        span: Span,
    ) -> Result<Type, SemanticError> {
        // A generic type parameter (e.g. `T`) resolves to a type variable.
        if type_args.is_empty()
            && let Some(symbol) = self.symbol_table.lookup(name)
            && matches!(symbol.kind, SymbolKind::Type)
        {
            return Ok(Type::Variable(name.to_string()));
        }

        // Correctly-parameterized built-in wrappers.
        if name == "optional" && type_args.len() == 1 {
            let resolved_arg = self.resolve_type(&type_args[0])?;
            return Ok(Type::Optional(Box::new(resolved_arg)));
        } else if name == "result" && type_args.len() == 2 {
            let resolved_ok = self.resolve_type(&type_args[0])?;
            let resolved_err = self.resolve_type(&type_args[1])?;
            // A type parameter reaches here as a bare name: the signature is
            // resolved before the parameters are substituted for type
            // variables, so `result<T, E>` would be rejected for E not
            // implementing Error even when the declaration says `E is Error`.
            // Its bound is enforced at the call, where E is bound to a real
            // type, so defer for one - but only for a name the declaration
            // actually lists, or a misspelled type would be waved through.
            let err_is_type_param = matches!(
                &resolved_err,
                Type::Named(name, args)
                    if args.is_empty() && self.signature_type_params.contains(name)
            );
            if !err_is_type_param && !self.type_implements_interface(&resolved_err, "Error") {
                return Err(SemanticError::with_help(
                    format!(
                        "Result error type must implement Error, but found {}",
                        format_type(&resolved_err)
                    ),
                    span,
                    "Use an error type that implements Error (requires message() -> string).",
                ));
            }
            return Ok(Type::Result(Box::new(resolved_ok), Box::new(resolved_err)));
        }

        // Built-in generic types always require their type arguments. The
        // correctly-arg'd forms are handled before reaching here (list/map/set/
        // tuple become dedicated TypeKind variants in the parser; optional/result
        // are matched just above), so any of these names arriving here is missing
        // or has the wrong number of type arguments (issue #289).
        if let Some(required) = builtin_generic_arity(name) {
            return Err(SemanticError::with_help(
                format!(
                    "'{}' requires {} type argument{}, got {}",
                    name,
                    required,
                    if required == 1 { "" } else { "s" },
                    type_args.len()
                ),
                span,
                missing_type_args_help(name),
            ));
        }

        // A user-declared generic type used without (or with the wrong number
        // of) type arguments; non-generic named types have no type parameters
        // and are unaffected (issue #289).
        if let Some(symbol) = self.symbol_table.lookup(name)
            && !symbol.type_params.is_empty()
        {
            self.validate_type_argument_count(name, &symbol, type_args, span)?;
        }

        // An interface names a capability, not a value. Mux dispatches
        // interfaces statically, so an interface-typed slot has no way to find
        // the method body for whatever it happens to hold - the vtable each
        // object carries is never read. Taking the interface as a bound
        // monomorphizes the call instead, which is the form that works.
        //
        // Rejecting here rather than at the call keeps the error on the
        // declaration the reader can fix: it used to be accepted and then fail
        // with "Type mismatch: expected Shape, got Rect" at every caller.
        // The built-in capabilities are answered structurally and are never
        // declared symbols, so a symbol-kind test alone let `func f(Comparable c)`
        // through to fail at the caller with the very message this replaces.
        let is_builtin_capability = self.symbol_table.lookup(name).is_none()
            && matches!(
                name,
                "Stringable" | "Equatable" | "Comparable" | "Hashable" | "Error"
            );
        if is_builtin_capability
            || self
                .symbol_table
                .lookup(name)
                .is_some_and(|symbol| matches!(symbol.kind, SymbolKind::Interface))
        {
            return Err(SemanticError::with_help(
                format!(
                    "'{}' is an interface and cannot be used as a value type",
                    name
                ),
                span,
                format!(
                    "Take it as a bound instead, e.g. 'func f<T is {}>(T value)'. A class still implements it with 'is {}'.",
                    name, name
                ),
            ));
        }

        // Named types are otherwise assumed to be classes or enums.
        let resolved_args = type_args
            .iter()
            .map(|arg| self.resolve_type(arg))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Type::Named(name.to_string(), resolved_args))
    }

    /// Validate only the generic arity of every named type inside `type_node`,
    /// recursing into type arguments and element types. Unlike `resolve_type`
    /// this reports nothing but arity mismatches (issue #289): it does not
    /// resolve names, so it never false-positives on a forward reference to a
    /// not-yet-registered type, and it skips the Result-error-implements-Error
    /// check. Used in the second analysis pass to arity-check enum variant
    /// payloads and interface member signatures, whose types the collection pass
    /// resolves with errors swallowed (unlike class fields, which are
    /// re-resolved in `analyze_class`).
    ///
    /// `local_params` are the enclosing declaration's type parameters (e.g. `T`
    /// in `enum E<T>`). They are type variables, not generic types, so a name
    /// matching one is skipped - otherwise a payload like `V(T)` could be
    /// mistaken for an unrelated global generic that happens to be named `T`.
    pub(super) fn validate_type_arity(
        &self,
        type_node: &TypeNode,
        local_params: &[(String, Vec<TraitBound>)],
    ) -> Result<(), SemanticError> {
        match &type_node.kind {
            TypeKind::Named(name, args) => {
                let is_local_param = local_params.iter().any(|(p, _)| p == name);
                if !is_local_param {
                    if let Some(required) = builtin_generic_arity(name) {
                        if args.len() != required {
                            return Err(SemanticError::with_help(
                                format!(
                                    "'{}' requires {} type argument{}, got {}",
                                    name,
                                    required,
                                    if required == 1 { "" } else { "s" },
                                    args.len()
                                ),
                                type_node.span,
                                missing_type_args_help(name),
                            ));
                        }
                    } else if let Some(symbol) = self.symbol_table.lookup(name)
                        && !symbol.type_params.is_empty()
                    {
                        self.validate_type_argument_count(name, &symbol, args, type_node.span)?;
                    }
                }
                for arg in args {
                    self.validate_type_arity(arg, local_params)?;
                }
                Ok(())
            }
            TypeKind::List(inner) | TypeKind::Set(inner) | TypeKind::Reference(inner) => {
                self.validate_type_arity(inner, local_params)
            }
            TypeKind::Map(key, value) => {
                self.validate_type_arity(key, local_params)?;
                self.validate_type_arity(value, local_params)
            }
            TypeKind::Tuple(left, right) => {
                self.validate_type_arity(left, local_params)?;
                self.validate_type_arity(right, local_params)
            }
            TypeKind::Function { params, returns } => {
                for param in params {
                    self.validate_type_arity(param, local_params)?;
                }
                self.validate_type_arity(returns, local_params)
            }
            TypeKind::TraitObject(inner) => self.validate_type_arity(inner, local_params),
            TypeKind::Primitive(_) | TypeKind::Auto => Ok(()),
        }
    }

    pub fn get_expression_type(&mut self, expr: &ExpressionNode) -> Result<Type, SemanticError> {
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

        // The parenthesis rule (mux-context#39), applied only to a qualified
        // access on the enum's own name. Keying off the base's *type* instead
        // would also match a value of that type, so `c.Green` on a `Color c`
        // would type-check as a construction and then fail in codegen.
        //
        // Not reached for the callee of a call: `resolve_call_expression_type`
        // intercepts `Enum.Variant(...)` first, so rejecting a missing payload
        // here does not affect a real construction.
        if let Some((enum_name, arity)) = self.enum_variant_arity(&expr_type, field) {
            if !self.names_a_type(expr) {
                // Reached through a value rather than the enum's name. A variant
                // is not a field, and matching is how you ask which one a value
                // holds. Rejected here because the alternative is resolving to
                // the variant's constructor and failing in codegen, where there
                // is no span to point at.
                return Err(SemanticError::with_help(
                    format!(
                        "'{}' is a variant of enum '{}', not a field on a value of it",
                        field, enum_name
                    ),
                    span,
                    format!(
                        "Construct it from the enum name, as in {}.{}, or use 'match' to test which variant a value holds.",
                        enum_name, field
                    ),
                ));
            }
            if arity == 0 {
                // A payload-less variant is a value, so it has the enum's type -
                // including its type arguments, so `Tree<int>.Leaf` is a
                // `Tree<int>` and not a bare `Tree` (issue #359).
                let type_args = match &expr_type {
                    Type::Named(_, args) | Type::Instantiated(_, args) => args.clone(),
                    _ => Vec::new(),
                };
                return Ok(Type::Named(enum_name, type_args));
            }
            return Err(Self::enum_variant_needs_arguments(
                &enum_name, field, arity, span,
            ));
        }

        self.resolve_field_access_by_type(&expr_type, field, span)
    }

    /// If `field` names a variant of the user enum `expr_type`, return the enum's
    /// name and how many payload values the variant takes.
    ///
    /// Variants are recorded as static methods whose parameters are the payload
    /// types, so the arity is the parameter count: 0 for `Red`, 1 for
    /// `Circ(float r)`. Drives the parenthesis rule in mux-context#39 - zero
    /// means parentheses are wrong, non-zero means they are required.
    pub(super) fn enum_variant_arity(
        &self,
        expr_type: &Type,
        field: &str,
    ) -> Option<(String, usize)> {
        let Type::Named(name, _) = expr_type else {
            return None;
        };
        let symbol = self.symbol_table.lookup(name)?;
        if symbol.kind != SymbolKind::Enum {
            return None;
        }
        let sig = symbol.methods.get(field)?;
        Some((name.clone(), sig.params.len()))
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

    /// "This variant carries a payload, so it needs its arguments." Shared by
    /// the value-position and call-position paths so both word it the same way.
    fn enum_variant_needs_arguments(
        enum_name: &str,
        variant: &str,
        arity: usize,
        span: Span,
    ) -> SemanticError {
        let plural = if arity == 1 { "" } else { "s" };
        SemanticError::with_help(
            format!(
                "Enum variant '{}.{}' carries a payload and cannot be used on its own",
                enum_name, variant
            ),
            span,
            format!(
                "'{}.{}' takes {} argument{}. Construct a value by passing them, e.g. {}.{}(...).",
                enum_name, variant, arity, plural, enum_name, variant
            ),
        )
    }

    /// The parenthesis rule for a variant in call position (mux-context#39).
    ///
    /// `Enum.Variant(...)` is intercepted before the callee is resolved as a
    /// value, because `resolve_field_access_by_type` gives a payload-less
    /// variant the enum's type rather than a callable one - which is the point,
    /// but would otherwise surface as "Cannot call non-function type".
    fn resolve_enum_variant_call(
        &mut self,
        func: &ExpressionNode,
        expr_span: Span,
    ) -> Option<Result<Type, SemanticError>> {
        let ExpressionKind::FieldAccess { expr: base, field } = &func.kind else {
            return None;
        };
        // Only when qualified by the enum's own name (bare or instantiated),
        // matching the value-position rule above: `c.Green(...)` on a value is a
        // method call that does not exist, not a construction, and must keep its
        // normal diagnostic.
        if !self.names_a_type(base) {
            return None;
        }
        let base_type = self.get_expression_type(base).ok()?;
        let (enum_name, arity) = self.enum_variant_arity(&base_type, field)?;

        if arity == 0 {
            return Some(Err(SemanticError::with_help(
                format!(
                    "Enum variant '{}.{}' carries no payload and is not called",
                    enum_name, field
                ),
                expr_span,
                format!(
                    "Parentheses pass arguments, and '{}.{}' takes none. Write it as a value: {}.{}",
                    enum_name, field, enum_name, field
                ),
            )));
        }

        // A real construction. Build the constructor signature directly rather
        // than routing back through field-access resolution, which rejects a
        // payload variant used without its arguments.
        //
        // The base's type arguments are substituted into the variant's declared
        // payload types, so `Box<int>.Full(5)` checks 5 against int rather than
        // against T (issue #359). They are also carried on the result, so the
        // constructed value is a `Box<int>` and not a bare `Box`.
        let type_args = match &base_type {
            Type::Named(_, args) | Type::Instantiated(_, args) => args.clone(),
            _ => Vec::new(),
        };
        let symbol = self.symbol_table.lookup(&enum_name)?;
        let sig = symbol.methods.get(field)?;
        let sig = if type_args.is_empty() {
            sig.clone()
        } else {
            self.substitute_method_sig(sig, &symbol.type_params, &type_args)
        };
        Some(Ok(Type::Function {
            params: sig.params,
            returns: Box::new(Type::Named(enum_name, type_args)),
            default_count: 0,
        }))
    }

    fn resolve_call_expression_type(
        &mut self,
        func: &ExpressionNode,
        args: &[ExpressionNode],
        expr_span: Span,
    ) -> Result<Type, SemanticError> {
        let func_type = match self.resolve_enum_variant_call(func, expr_span) {
            Some(Err(e)) => return Err(e),
            Some(Ok(t)) => t,
            None => self.resolve_called_function_type(func)?,
        };

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

            self.check_declared_bounds(func_name, &substitutions_by_original_name, expr_span)?;

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
                self.binary_op_help(&left_type, &right_type, &base_op),
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
            self.binary_op_help(&left_type, &right_type, op),
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
            // Tuple and Result are composites like the rest. Falling through to
            // the catch-all left their members as `Named("A")` rather than a
            // type variable, so `func f<A, B>(tuple<A, B> t)` could not be
            // called at all - unification had nothing to bind `A` to.
            Type::Tuple(left, right) => Type::Tuple(
                Box::new(self.substitute_type_param(left, param, replacement)),
                Box::new(self.substitute_type_param(right, param, replacement)),
            ),
            Type::Result(ok, err) => Type::Result(
                Box::new(self.substitute_type_param(ok, param, replacement)),
                Box::new(self.substitute_type_param(err, param, replacement)),
            ),
            Type::Instantiated(name, args) => Type::Instantiated(
                name.clone(),
                args.iter()
                    .map(|a| self.substitute_type_param(a, param, replacement))
                    .collect(),
            ),
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

    /// Whether `type_` has one of the built-in capabilities, or `None` if
    /// `interface_name` is not one of them.
    ///
    /// Each answer delegates to the check that governs the operation itself, so
    /// a bound cannot promise something the operator then refuses. Without this
    /// the bounds answered only for primitives and for named symbols with a
    /// declared `interfaces` entry, so `<T is Equatable>` would have rejected a
    /// `list<int>` that `==` compares perfectly well (issue #361).
    fn satisfies_builtin_capability(&self, type_: &Type, interface_name: &str) -> Option<bool> {
        match interface_name {
            // Exactly what `==` accepts. A type parameter is deliberately not
            // answered here: it falls through to the declared bounds in
            // `current_bounds`, which is also what stops this recursing, since
            // `resolve_equality_binary_operator` asks this question back for one.
            "Equatable" => match type_ {
                Type::Generic(_) | Type::Variable(_) => None,
                _ => Some(self.resolve_equality_binary_operator(type_).is_some()),
            },
            // Exactly what may be a map key or set member. A type parameter
            // is deliberately not answered here, for the same reason as
            // `Equatable` above: it falls through to its declared bounds.
            "Hashable" => match type_ {
                Type::Generic(_) | Type::Variable(_) => None,
                _ => Some(self.is_hashable_type(type_)),
            },
            // Exactly what `<` accepts. Not delegated, because
            // `resolve_comparison_binary_operator` asks this question back.
            "Comparable" => match type_ {
                Type::Primitive(PrimitiveType::Int | PrimitiveType::Float | PrimitiveType::Str) => {
                    Some(true)
                }
                Type::Named(_, _) | Type::Variable(_) | Type::Generic(_) => None,
                _ => Some(false),
            },
            // Collections, tuples and the built-in wrappers render themselves.
            // User enums deliberately do not - only the author knows whether
            // HTTPCode.Ok should print as "Ok" or "200" - so they fall through
            // to the declared-interface check like any other named type.
            "Stringable" => match type_ {
                Type::List(_)
                | Type::Map(_, _)
                | Type::Set(_)
                | Type::Tuple(_, _)
                | Type::Optional(_)
                | Type::Result(_, _)
                | Type::EmptyList
                | Type::EmptyMap
                | Type::EmptySet => Some(true),
                _ => None,
            },
            _ => None,
        }
    }

    fn type_implements_interface_with_args(
        &self,
        type_: &Type,
        interface_name: &str,
        interface_args: &[Type],
    ) -> bool {
        // The four built-in capability interfaces answer for every type that
        // actually has the capability, not only for symbols with a declared
        // `interfaces` entry. Consulted first, and only when no interface
        // arguments were supplied, since none of them are generic.
        if interface_args.is_empty()
            && let Some(answer) = self.satisfies_builtin_capability(type_, interface_name)
        {
            return answer;
        }
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
            // Builtin collections satisfy `Collection<T>` structurally: they
            // provide len/is_empty/to_list/contains over their element type, so
            // a plain `list` or `set` can be passed to the generic algorithms.
            // A map is excluded on purpose - its `contains` takes a key while
            // `to_list` yields pairs, so there is no single coherent T.
            Type::List(elem) | Type::Set(elem) => {
                interface_name == "Collection"
                    && (interface_args.is_empty()
                        || interface_args == std::slice::from_ref(elem.as_ref()))
            }
            _ => false,
        }
    }

    /// Whether the named class or enum declares `interface_name`. Exposed for
    /// codegen, which needs it to decide whether an operator dispatches to a
    /// user method.
    pub fn type_implements_named_interface(&self, name: &str, interface_name: &str) -> bool {
        self.type_implements_interface_with_named(name, interface_name, &[])
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
                Self::bound_grants(bound_name, interface_name)
                    && (interface_args.is_empty() || bound_args == interface_args)
            })
        } else {
            false
        }
    }

    /// Whether declaring `declared` also grants `wanted`.
    ///
    /// Two implications hold, and both are true by construction rather than
    /// convention:
    ///
    /// - `Comparable` grants `Equatable`. A type whose values can be ordered can
    ///   be told apart, and every type `<` accepts is one `==` accepts.
    /// - `Hashable` grants `Equatable`. A map lookup compares keys, so a type
    ///   usable as a key already supports equality - and `is_hashable_type`
    ///   admits exactly primitives and user enums, all of which `==` accepts.
    ///
    /// Without these, every ordered or keyed generic would have to spell out
    /// `<T is Comparable & Equatable>` to use both operators, which is what the
    /// standard library's own containers would have needed: a search tree orders
    /// on `<` and stops on `==`, and a graph keys on `T` and compares it
    /// (issue #361).
    /// Whether a class can answer `==`, through any capability that implies
    /// equality. This is `bound_grants` applied to a declaration rather than to
    /// a type parameter's bound: a class that orders itself can say whether two
    /// instances are the same one, and one that hashes itself has to.
    pub fn class_supports_equality(&self, name: &str) -> bool {
        ["Equatable", "Comparable", "Hashable"]
            .iter()
            .any(|declared| self.type_implements_named_interface(name, declared))
    }

    /// Whether the class declares a capability that *requires* an `eq` method,
    /// which is what makes its signature checked.
    ///
    /// `Comparable` is absent on purpose: it requires only `cmp`, so a class
    /// declaring it may also have an unrelated method named `eq` that nothing
    /// validated. Treating that as the equality method emitted a wrapper
    /// calling it with the wrong argument and return types.
    pub fn class_declares_equality_method(&self, name: &str) -> bool {
        self.type_implements_named_interface(name, "Equatable")
            || self.type_implements_named_interface(name, "Hashable")
    }

    fn bound_grants(declared: &str, wanted: &str) -> bool {
        if declared == wanted {
            return true;
        }
        wanted == "Equatable" && matches!(declared, "Comparable" | "Hashable")
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
            // `len` is the `Collection<T>` spelling of `size`; both are kept so
            // existing code and the interface agree.
            "size" | "len" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Int),
                is_static: false,
            }),
            "contains" => Some(MethodSig {
                params: vec![elem_type.clone()],
                return_type: Type::Primitive(PrimitiveType::Bool),
                is_static: false,
            }),
            // Identity for a list, but required by `Collection<T>` so a list can
            // be passed to the generic algorithms.
            "to_list" => Some(MethodSig {
                params: vec![],
                return_type: Type::List(Box::new(elem_type.clone())),
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
            "size" | "len" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Int),
                is_static: false,
            }),
            "is_empty" => Some(MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Bool),
                is_static: false,
            }),
            // A map's elements are its key/value pairs, so `to_list` matches
            // `get_pairs`. (A map is deliberately not a `Collection<T>`: its
            // `contains` takes a key while this yields pairs, so T is ambiguous.)
            "to_list" => Some(MethodSig {
                params: vec![],
                return_type: Type::List(Box::new(Type::Tuple(
                    Box::new(key_type.clone()),
                    Box::new(value_type.clone()),
                ))),
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
            "size" | "len" => Some(MethodSig {
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
                self.resolve_equality_binary_operator(left_type)
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

    /// `==` and `!=` accept exactly the types codegen knows how to compare -
    /// see `generate_equality_op` in `codegen/operators.rs`, which this must be
    /// kept in step with.
    ///
    /// Returning `Some` for everything is how comparing a class, a reference, a
    /// function value or a `void` call used to reach codegen and be reported as
    /// an internal compiler error, telling users to file a compiler bug about
    /// their own program. A codegen error that describes user code is a bug in
    /// type checking (issue #360), so the rejection belongs here, where there is
    /// a span to point at.
    fn resolve_equality_binary_operator(&self, left_type: &Type) -> Option<Type> {
        let comparable = match left_type {
            // Void and Auto are deliberately absent: neither is a value.
            Type::Primitive(
                PrimitiveType::Int
                | PrimitiveType::Float
                | PrimitiveType::Bool
                | PrimitiveType::Char
                | PrimitiveType::Str,
            ) => true,
            // Compared structurally by the runtime, which is the same
            // comparison that already makes them usable as map keys.
            Type::List(_)
            | Type::Map(_, _)
            | Type::Set(_)
            | Type::Tuple(_, _)
            | Type::Optional(_)
            | Type::Result(_, _)
            | Type::EmptyList
            | Type::EmptyMap
            | Type::EmptySet => true,
            // Classes and interfaces also land in Named. Only enums have the
            // generated structural compare, but a class may opt in by declaring
            // `is Equatable` and writing `eq`.
            Type::Named(name, _) => self.is_enum_name(name) || self.class_supports_equality(name),
            // A type parameter must declare `Equatable` to be compared. Using
            // the operator imposes the bound rather than inferring it, so the
            // error lands on the declaration the reader can fix instead of on
            // each instantiation. `<` already works this way via
            // `resolve_comparison_binary_operator`, and a method call already
            // imposes its own bound, so this makes `==` consistent with both
            // (issue #361).
            //
            // `Instantiated` is absent: semantics never builds one (only
            // codegen's substitution helpers do), and `generate_equality_op` has
            // no arm for it, so accepting it could only ever admit an internal
            // compiler error.
            Type::Generic(_) | Type::Variable(_) => {
                self.type_implements_interface(left_type, "Equatable")
            }
            _ => false,
        };

        comparable.then_some(Type::Primitive(crate::ast::PrimitiveType::Bool))
    }

    /// Whether `name` names a user enum - one with the generated structural
    /// compare - as opposed to a class or an interface.
    ///
    /// `optional` and `result` are excluded to mirror codegen, which excludes
    /// them by name because they are heap `*mut Value`s rather than inline
    /// structs despite being seeded into `enum_variants`. Declaring a type under
    /// either name is now rejected outright (issue #369), so this is defence in
    /// depth against the two sides drifting rather than a reachable case.
    fn is_enum_name(&self, name: &str) -> bool {
        if matches!(name, "optional" | "result") {
            return false;
        }
        self.symbol_table
            .lookup(name)
            .is_some_and(|symbol| symbol.kind == SymbolKind::Enum)
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
        // Builtin collections carry their element type the same way a class
        // carries its type arguments, so `E is Collection<T>` can infer `T` from
        // `E = list<int>` exactly as it does from `E = Stack<int>`. Without this,
        // a signature whose T appears only in the bound (e.g.
        // `reverse<T, E is Collection<T>>(E c) returns list<T>`) is uninferable
        // for a plain list or set.
        Type::List(elem) | Type::Set(elem) => Some(std::slice::from_ref(elem.as_ref())),
        Type::Reference(inner) => match inner.as_ref() {
            Type::Named(_, args) => Some(args.as_slice()),
            Type::List(elem) | Type::Set(elem) => Some(std::slice::from_ref(elem.as_ref())),
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
