//! LLVM IR code generation for the Mux compiler.
//!
//! This module generates LLVM IR from the AST and semantic analysis results.
//! It has been split into submodules for better organization:
//! - classes: Class, interface, and enum type generation
//! - constructors: Constructor generation for classes and enums
//! - expressions: Expression code generation
//! - functions: Function declaration and generation
//! - generics: Generic type instantiation
//! - memory: Memory management and RC tracking
//! - methods: Method call generation
//! - operators: Binary and logical operators
//! - runtime: Runtime function boxing/unboxing
//! - statements: Statement code generation
//! - types: Type conversion functions

use std::io::Write;

use inkwell::AddressSpace;
use inkwell::OptimizationLevel;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine,
};
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, FunctionValue, PointerValue};
use std::collections::{BTreeMap, HashMap};

use crate::ast::{
    AstNode, EnumVariant, EnumVariantField, Field, FunctionNode, ImportSpec, StatementKind,
    StatementNode, TraitBound, TypeNode,
};
use crate::semantics::{GenericContext, SemanticAnalyzer, Type, Type as ResolvedType};

use scoped_vars::ScopedVars;

type ClassTypeParamBounds = Vec<(String, Vec<(String, Vec<Type>)>)>;
type EnumVariantFieldMap = HashMap<String, HashMap<String, Vec<EnumVariantField>>>;

/// What a tracked RC scope slot holds, so end-of-scope cleanup releases it the
/// right way. A boxed value is a `*mut Value` decremented directly; a custom
/// enum is stored inline as a `{ i32 tag, fields... }` struct, so it needs
/// variant-tag drop-glue to release only the pointer payloads of its active
/// variant (see `emit_enum_drop`).
#[derive(Clone)]
pub(super) enum RcSlot<'a> {
    Boxed(PointerValue<'a>),
    /// The shared storage of a captured variable. Released with
    /// `mux_cell_release`, which drops one holder rather than freeing outright,
    /// because a closure capturing the variable holds the same cell.
    Cell(PointerValue<'a>),
    EnumStruct {
        enum_name: String,
        alloca: PointerValue<'a>,
    },
}

impl<'a> RcSlot<'a> {
    /// The storage slot backing this entry, used to dedupe tracking so one slot
    /// is never released twice within a scope.
    fn alloca(&self) -> PointerValue<'a> {
        match self {
            RcSlot::Boxed(alloca) | RcSlot::Cell(alloca) | RcSlot::EnumStruct { alloca, .. } => {
                *alloca
            }
        }
    }
}

pub struct CodeGenerator<'a> {
    context: &'a Context,
    module: Module<'a>,
    runtime_signatures: Module<'a>,
    builder: Builder<'a>,
    analyzer: &'a mut SemanticAnalyzer,
    type_map: HashMap<String, BasicTypeEnum<'a>>,
    vtable_map: HashMap<String, PointerValue<'a>>,
    vtable_type_map: HashMap<String, inkwell::types::StructType<'a>>,
    class_copy_fns: HashMap<String, PointerValue<'a>>,
    class_destructor_fns: HashMap<String, PointerValue<'a>>,
    enum_variants: HashMap<String, Vec<String>>,
    /// Each enum's declared variants, kept so a generic enum can be stamped out
    /// per instantiation on demand (`ensure_enum_instantiated`, issue #359).
    /// Ordered, so the instances an ordered walk creates stay deterministic
    /// (issue #344).
    enum_asts: BTreeMap<String, Vec<EnumVariant>>,
    enum_variant_fields: EnumVariantFieldMap,
    field_map: HashMap<String, HashMap<String, usize>>,
    field_types_map: HashMap<String, Vec<BasicTypeEnum<'a>>>,
    classes: HashMap<String, Vec<Field>>,
    constructors: HashMap<String, FunctionValue<'a>>,
    /// Memoized out-of-line RC-glue functions for recursive nested enums (issue
    /// #309). A self- or mutually-referential enum cannot have its drop/clone
    /// glue inline-expanded (it would recurse forever at compile time), so each
    /// `(enum name, op label)` gets one function that recurses at runtime,
    /// terminating on a variant with no boxed payload. Declared before its body
    /// is built so a variant that re-embeds the enum resolves the self-call.
    enum_glue_fns: HashMap<(String, &'static str), FunctionValue<'a>>,
    lambda_counter: usize,
    string_counter: usize,
    label_counter: usize,
    variables: ScopedVars<(PointerValue<'a>, BasicTypeEnum<'a>, ResolvedType)>,
    /// Names whose address is taken somewhere in the program. Their slots stay
    /// boxed even when the type is a scalar, because a reference has to mean
    /// one thing and a reference to a list element is a `*mut Value`.
    address_taken: std::collections::HashSet<String>,
    /// Names captured by some closure. Their storage is a reference-counted
    /// cell shared with every closure that captures them, so a write through
    /// one is visible to the others for as long as any holder lives.
    captured_names: std::collections::HashSet<String>,
    /// The slots that really are reference-counted cells: a captured local,
    /// parameter or loop variable gets one at its binding, and a captured
    /// global is emitted as a static cell. Anything else a closure captures is
    /// copied into a cell the closure owns. Recorded by slot rather than by
    /// name, because one name can denote either depending on where it was
    /// bound, and retaining a non-cell as a cell corrupts memory.
    cell_slots: std::collections::HashSet<PointerValue<'a>>,
    /// Globals visible to the code currently being generated, keyed by their
    /// bare source name. Swapped per module (see `module_globals`) so every
    /// lookup site can keep using the unqualified name.
    global_variables: HashMap<String, (PointerValue<'a>, BasicTypeEnum<'a>, ResolvedType)>,
    /// Per-module global tables, keyed by sanitized module name, each mapping a
    /// bare global name to its slot. Module-level globals are emitted as
    /// `module!name` in LLVM so two modules declaring the same constant no
    /// longer collide; this table is what makes the right set visible while a
    /// given module's init and functions are generated.
    module_globals:
        HashMap<String, HashMap<String, (PointerValue<'a>, BasicTypeEnum<'a>, ResolvedType)>>,
    /// Sanitized name of the module whose globals are being declared/generated,
    /// or `None` for the main module (whose globals keep unqualified symbols).
    current_module_prefix: Option<String>,
    functions: HashMap<String, FunctionValue<'a>>,
    /// Every function AST node, keyed by name. Ordered, because
    /// `generate_specialized_methods` walks it to decide which methods to
    /// monomorphize and emits them in the order it finds them. A `HashMap`
    /// there made the emitted IR differ byte-for-byte between back-to-back
    /// builds of the same file, since Rust randomizes hash iteration per
    /// process (issue #344).
    function_nodes: BTreeMap<String, FunctionNode>,
    current_function_name: Option<String>,
    current_function_return_type: Option<ResolvedType>,
    generic_context: Option<GenericContext>,
    context_stack: Vec<GenericContext>,
    generated_methods: HashMap<String, bool>,
    rc_scope_stack: Vec<Vec<(String, RcSlot<'a>)>>,
    /// Owned RC temporaries produced during the current statement's expression
    /// evaluation that have not been bound to a variable. They are decremented
    /// at the statement boundary so intermediate values (string literals,
    /// `to_string()`/concat results, call results, collection literals) do not
    /// leak. Ownership transfer (binding to a variable, returning) removes the
    /// pointer from this list so it is not double-freed.
    ///
    /// Each entry is `(value, slot)`: the SSA pointer of the temporary and a
    /// null-initialized entry-block alloca it was spilled into. Cleanup loads
    /// the slot (which dominates all blocks) and decrements it via the null-safe
    /// `mux_rc_dec`, so temporaries born in conditionally executed blocks
    /// (short-circuit operands, ternary arms, loop bodies) are freed correctly
    /// regardless of control flow. `value` is retained so ownership transfer can
    /// find and null the right slot.
    temp_values: Vec<(PointerValue<'a>, PointerValue<'a>)>,
    /// Owned closure temporaries produced during the current statement, kept
    /// separate from `temp_values` because closures are freed with
    /// `mux_closure_release` (which walks and releases their captures) rather
    /// than `mux_rc_dec`. Same `(value, slot)` spill-slot discipline.
    closure_temp_values: Vec<(PointerValue<'a>, PointerValue<'a>)>,
    /// Owned inline-enum temporaries produced during the current statement that
    /// were not bound to a variable (a discarded `Enum.Variant(x)` statement, or
    /// an owned enum match subject). Enums are value-semantic structs, not boxed
    /// pointers, so they are released with `emit_enum_drop` on their active
    /// variant rather than `mux_rc_dec`. Each entry is `(value, slot, enum_name)`:
    /// the SSA struct value and a zero-initialized entry-block spill alloca it was
    /// stored into, so cleanup can drop it from any later block (null-safe on
    /// paths that never produced it, exactly like `temp_values`).
    enum_temp_values: Vec<(BasicValueEnum<'a>, PointerValue<'a>, String)>,
    /// Closure-typed variables tracked per scope, released with
    /// `mux_closure_release` when the scope ends. Pushed/popped in lock-step
    /// with `rc_scope_stack`.
    closure_scope_stack: Vec<Vec<(String, PointerValue<'a>)>>,
    source_name: String,
    /// ABI type sizing used to pick a union slot large enough for every variant
    /// at a heterogeneous enum payload position (issue #309). Built from LLVM's
    /// default data layout; only relative size/alignment comparisons are used, so
    /// the exact target layout is irrelevant (clang lays out the real struct from
    /// the field types the slot ultimately holds).
    target_data: inkwell::targets::TargetData,
}

impl<'a> CodeGenerator<'a> {
    fn collect_imported_functions(&self) -> Vec<(String, FunctionNode)> {
        self.analyzer
            .all_module_asts()
            .iter()
            .flat_map(|(module_path, module_nodes)| {
                let module_name_for_mangling = Self::sanitize_module_path(module_path);
                module_nodes
                    .iter()
                    .filter_map(|node| {
                        if let AstNode::Function(func) = node {
                            Some((module_name_for_mangling.clone(), func.clone()))
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn collect_non_generic_main_functions(&self, nodes: &[AstNode]) -> Vec<FunctionNode> {
        nodes
            .iter()
            .filter_map(|node| {
                if let AstNode::Function(func) = node
                    && func.type_params.is_empty()
                {
                    return Some(func.clone());
                }
                None
            })
            .collect()
    }

    fn collect_top_level_statements(&self, nodes: &[AstNode]) -> Vec<StatementNode> {
        nodes
            .iter()
            .filter_map(|node| {
                if let AstNode::Statement(stmt) = node {
                    Some(stmt.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    fn collect_class_methods_to_declare(&self, nodes: &[AstNode]) -> Vec<FunctionNode> {
        let mut methods = Vec::new();
        for node in nodes {
            if let AstNode::Class {
                name,
                methods: class_methods,
                ..
            } = node
            {
                for method in class_methods {
                    let mut method_copy = method.clone();
                    method_copy.name = format!("{}.{}", name, method.name);
                    methods.push(method_copy);
                }
            }
        }
        methods
    }

    fn declare_imported_functions(
        &mut self,
        imported_functions: &[(String, FunctionNode)],
    ) -> Result<(), String> {
        for (module_name, func) in imported_functions {
            self.function_nodes.insert(func.name.clone(), func.clone());
            if func.type_params.is_empty() {
                let mangled_name = format!("{}!{}", module_name, func.name);
                self.declare_function_with_name(func, &mangled_name)?;
            }
        }
        Ok(())
    }

    fn declare_main_functions(&mut self, main_module_nodes: &[AstNode]) -> Result<(), String> {
        for node in main_module_nodes {
            if let AstNode::Function(func) = node {
                self.function_nodes.insert(func.name.clone(), func.clone());

                if func.type_params.is_empty() {
                    let llvm_name = if func.name == "main" {
                        "!user!main".to_string()
                    } else {
                        func.name.clone()
                    };
                    self.declare_function_with_name(func, &llvm_name)?;
                }
            }
        }
        Ok(())
    }

    fn declare_class_methods(&mut self, nodes: &[AstNode]) -> Result<(), String> {
        for method in self.collect_class_methods_to_declare(nodes) {
            self.declare_function(&method)?;
        }
        Ok(())
    }

    /// Emit, before any function or constructor body, the out-of-line support
    /// every RC-payload enum needs (issue #309): its drop/clone glue functions,
    /// an object copy callback, and the global that will hold its runtime object
    /// type id. Doing it here, with no active function, keeps each generated
    /// function self-contained; a lazy build mid-body would splice its blocks
    /// into the caller. `main` later registers the object types and fills in the
    /// globals (see `register_enum_object_types`).
    fn generate_all_enum_object_support(&mut self, nodes: &[AstNode]) -> Result<(), String> {
        for node in nodes {
            if let AstNode::Enum { name, .. } = node {
                // Every enum, because every enum can now be boxed as a managed
                // value and needs its compare and hash glue. The drop and clone
                // glue for one without a reference-counted payload are trivial.
                self.generate_enum_object_support(name)?;
            }
        }
        Ok(())
    }

    fn generate_vtables(&mut self, nodes: &[AstNode]) -> Result<(), String> {
        for node in nodes {
            if let AstNode::Class { name, .. } = node {
                let interfaces = self
                    .analyzer
                    .all_symbols()
                    .get(name)
                    .map(|sym| sym.interfaces.clone())
                    .unwrap_or_default();
                self.generate_class_vtables(name, &interfaces)?;
            }
        }
        Ok(())
    }

    fn generate_enum_and_class_constructors(&mut self, nodes: &[AstNode]) -> Result<(), String> {
        for node in nodes {
            match node {
                AstNode::Enum { name, variants, .. } => {
                    self.generate_enum_constructors(name, variants)?;
                }
                AstNode::Class { name, fields, .. } => {
                    // A generic class has nothing to build here: it is laid out
                    // per instantiation, and `ensure_class_instantiated` gives
                    // each one its own constructor, copy and destructor over its
                    // own layout. Emitting the unspecialized set as well would
                    // put a second, type-erased layout of the same class in the
                    // module, whose copy and destructor read a slot the
                    // instantiations store a raw scalar in.
                    if self.class_is_generic(name) {
                        continue;
                    }
                    let interfaces = self
                        .analyzer
                        .all_symbols()
                        .get(name)
                        .map(|sym| sym.interfaces.clone())
                        .unwrap_or_default();
                    // Generate the copy and destructor functions first so
                    // the constructor body can register them as runtime
                    // callbacks. The function definitions are emitted at
                    // module level, so the order only affects the lookup
                    // in `class_copy_fns` / `class_destructor_fns`.
                    self.generate_class_copy_and_destructor(name, fields)?;
                    self.generate_class_capability_glue(name)?;
                    self.generate_class_constructors(name, fields, &interfaces)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn declare_global_variable(
        &mut self,
        name: &str,
        llvm_type: BasicTypeEnum<'a>,
        resolved_type: ResolvedType,
    ) {
        // Module globals are emitted as `module!name` so same-named constants in
        // two modules get distinct LLVM symbols; the map stays keyed by the bare
        // name because only one module's globals are visible at a time.
        let llvm_name = match &self.current_module_prefix {
            Some(prefix) => format!("{}!{}", prefix, name),
            None => name.to_string(),
        };
        // A captured global is emitted as a statically allocated capture cell,
        // `{ i64 refcount, *mut Value }`, and its slot is that cell's payload.
        // A closure then shares the global itself rather than a copy of it, so
        // writing through the closure is a write to the global.
        //
        // The refcount starts at 1 for the global's own permanent reference, so
        // the closures that retain and release it can never take it to zero and
        // try to free static storage. Teardown releases the value the cell
        // holds, never the cell.
        if self.captured_names.contains(name) {
            let i64_type = self.context.i64_type();
            let ptr_type = self.context.ptr_type(AddressSpace::default());
            let cell_type = self
                .context
                .struct_type(&[i64_type.into(), ptr_type.into()], false);
            let global = self.module.add_global(cell_type, None, &llvm_name);
            global.set_initializer(&cell_type.const_named_struct(&[
                i64_type.const_int(1, false).into(),
                ptr_type.const_null().into(),
            ]));
            // A constant GEP: globals are declared before the builder is
            // positioned anywhere, so this cannot go through the builder.
            let i32_type = self.context.i32_type();
            let payload = unsafe {
                global.as_pointer_value().const_in_bounds_gep(
                    cell_type,
                    &[i32_type.const_zero(), i32_type.const_int(1, false)],
                )
            };
            self.global_variables
                .insert(name.to_string(), (payload, ptr_type.into(), resolved_type));
            self.cell_slots.insert(payload);
            return;
        }

        let global = self.module.add_global(llvm_type, None, &llvm_name);
        global.set_initializer(&llvm_type.const_zero());
        self.global_variables.insert(
            name.to_string(),
            (global.as_pointer_value(), llvm_type, resolved_type),
        );
    }

    fn llvm_global_type_for_resolved_type(
        &mut self,
        name: &str,
        resolved_type: &ResolvedType,
    ) -> Result<BasicTypeEnum<'a>, String> {
        // A global holds what a local of the same type holds. Module init seeds
        // the variable table from these slots, so a scalar global that stayed a
        // `*mut Value` would be written with a raw scalar by the initializing
        // assignment and then read back as a pointer.
        if let Some(scalar) = self.scalar_slot_for_binding(name, resolved_type) {
            return Ok(scalar);
        }
        match resolved_type {
            Type::Primitive(_) => Ok(self.context.ptr_type(AddressSpace::default()).into()),
            _ => {
                let type_node = self.type_to_type_node(resolved_type);
                self.llvm_type_from_mux_type(&type_node)
            }
        }
    }

    fn declare_typed_or_const_global(
        &mut self,
        name: &str,
        type_node: &TypeNode,
    ) -> Result<(), String> {
        let resolved_type = self
            .analyzer
            .resolve_type(type_node)
            .map_err(|e| e.to_string())?;
        self.instantiate_generic_types_in_type(&resolved_type)?;
        let llvm_type = self.llvm_global_type_for_resolved_type(name, &resolved_type)?;
        self.declare_global_variable(name, llvm_type, resolved_type);
        Ok(())
    }

    fn declare_auto_global(
        &mut self,
        name: &str,
        expr: &crate::ast::ExpressionNode,
    ) -> Result<(), String> {
        let resolved_type = self
            .resolve_expression_type_with_fallback(expr)
            .map_err(|e| format!("Failed to get type for {}: {}", name, e))?;
        self.instantiate_generic_types_in_type(&resolved_type)?;
        let llvm_type = self.llvm_global_type_for_resolved_type(name, &resolved_type)?;
        self.declare_global_variable(name, llvm_type, resolved_type);
        Ok(())
    }

    fn declare_top_level_globals(
        &mut self,
        top_level_statements: &[StatementNode],
    ) -> Result<(), String> {
        for stmt in top_level_statements {
            match &stmt.kind {
                StatementKind::TypedDecl(name, type_, _)
                | StatementKind::ConstDecl(name, type_, _) => {
                    self.declare_typed_or_const_global(name, type_)?;
                }
                StatementKind::AutoDecl(name, _, expr) => {
                    self.declare_auto_global(name, expr)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn collect_module_init_data(&self) -> Vec<(String, Vec<StatementNode>)> {
        self.analyzer
            .all_module_asts()
            .iter()
            .map(|(module_path, module_nodes)| {
                let module_top_level_statements = module_nodes
                    .iter()
                    .filter_map(|node| {
                        if let AstNode::Statement(stmt) = node {
                            Some(stmt.clone())
                        } else {
                            None
                        }
                    })
                    .collect();
                (module_path.replace('/', "_"), module_top_level_statements)
            })
            .collect()
    }

    /// Make constants imported directly into the current scope reachable by
    /// their bare name. Their storage stays in the owning module's table; this
    /// adds an alias so `import cfg.*` followed by a bare `err_code` resolves,
    /// while a constant only reachable as `cfg.err_code` stays out of the view.
    fn alias_directly_imported_constants(&mut self) {
        let aliases: Vec<(String, String, String)> = self
            .analyzer
            .all_symbols()
            .iter()
            .filter(|(_, symbol)| symbol.kind == crate::semantics::SymbolKind::Constant)
            .filter_map(|(local_name, symbol)| {
                // `llvm_name` is `module!original_name`. Both halves matter: a
                // renamed import (`... as MY_CONST`) is referenced by the local
                // name but stored in the module's table under the original one,
                // so keying the lookup by the local name would silently miss it.
                let (module, original_name) = symbol.llvm_name.as_ref()?.split_once('!')?;
                Some((
                    local_name.clone(),
                    module.to_string(),
                    original_name.to_string(),
                ))
            })
            .collect();

        for (local_name, module, original_name) in aliases {
            if let Some(entry) = self
                .module_globals
                .get(&module)
                .and_then(|globals| globals.get(&original_name))
                .cloned()
            {
                self.global_variables.insert(local_name, entry);
            }
        }
    }

    /// The `(source module, spec)` of every import in a module's own AST that
    /// brings names directly into scope. A bare `import logger` is excluded: it
    /// is reached as `logger.NAME` field access, not by bare name.
    fn collect_direct_imports(nodes: &[AstNode]) -> Vec<(String, ImportSpec)> {
        nodes
            .iter()
            .filter_map(|node| match node {
                AstNode::Statement(StatementNode {
                    kind: StatementKind::Import { module_path, spec },
                    ..
                }) if !matches!(spec, ImportSpec::Module { .. }) => {
                    Some((Self::sanitize_module_path(module_path), spec.clone()))
                }
                _ => None,
            })
            .collect()
    }

    /// Resolve one import spec against the source module's globals into
    /// `(local name, original name)` pairs. A wildcard takes every global the
    /// source module declared; the named forms take only what they list, under
    /// their alias when renamed.
    fn resolve_import_aliases(
        spec: &ImportSpec,
        source_globals: &HashMap<String, (PointerValue<'a>, BasicTypeEnum<'a>, ResolvedType)>,
    ) -> Vec<(String, String)> {
        match spec {
            ImportSpec::Wildcard => source_globals
                .keys()
                .map(|name| (name.clone(), name.clone()))
                .collect(),
            ImportSpec::Item { item, alias } => {
                vec![(alias.clone().unwrap_or_else(|| item.clone()), item.clone())]
            }
            ImportSpec::Items { items } => items
                .iter()
                .map(|(item, alias)| (alias.clone().unwrap_or_else(|| item.clone()), item.clone()))
                .collect(),
            ImportSpec::Module { .. } => Vec::new(),
        }
    }

    /// Alias constants a non-main module imported directly into that module's
    /// own global table. `alias_directly_imported_constants` handles the main
    /// module from the analyzer's flattened symbol table, but that table says
    /// nothing about what a non-main module pulled into its own scope, so a
    /// module function referencing a wildcard-imported constant by bare name
    /// failed to resolve. Storage stays in the owning module's table; this only
    /// makes it reachable from the importing module's view.
    fn alias_imported_constants_into_modules(&mut self) {
        let module_paths: Vec<String> = self.analyzer.all_module_asts().keys().cloned().collect();
        for module_path in module_paths {
            let nodes = match self.analyzer.all_module_asts().get(&module_path) {
                Some(nodes) => nodes.clone(),
                None => continue,
            };
            let target = Self::sanitize_module_path(&module_path);
            let mut aliases = Vec::new();
            for (source, spec) in Self::collect_direct_imports(&nodes) {
                let Some(source_globals) = self.module_globals.get(&source) else {
                    continue;
                };
                for (local_name, original) in Self::resolve_import_aliases(&spec, source_globals) {
                    if let Some(entry) = source_globals.get(&original).cloned() {
                        aliases.push((local_name, entry));
                    }
                }
            }
            if let Some(target_globals) = self.module_globals.get_mut(&target) {
                target_globals.extend(aliases);
            }
        }
    }

    /// Run `f` with `module_name`'s globals installed as the visible set,
    /// restoring the previous set afterwards (on success or error). This is what
    /// lets a module's own code refer to its globals by their bare names while
    /// they live under `module!name` in LLVM.
    /// Every module reachable here was seeded into `module_globals` from the
    /// same `all_module_asts` set, so a missing key means the tables have
    /// drifted apart. Defaulting to an empty set would turn that into a
    /// confusing "Undefined variable" far from the cause, so fail loudly.
    fn with_module_globals<T>(
        &mut self,
        module_name: &str,
        f: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        let sanitized = Self::sanitize_module_path(module_name);
        let globals = self
            .module_globals
            .get(&sanitized)
            .cloned()
            .ok_or_else(|| {
                format!("internal error: no global table registered for module '{sanitized}'")
            })?;
        // The prefix must travel with the table: anything that declares a
        // global while `f` runs has to mangle it into this module, not leak an
        // unprefixed name into the swapped-in view.
        let saved = std::mem::replace(&mut self.global_variables, globals);
        let saved_prefix = self.current_module_prefix.replace(sanitized);
        let result = f(self);
        self.global_variables = saved;
        self.current_module_prefix = saved_prefix;
        result
    }

    fn generate_imported_user_functions(
        &mut self,
        imported_functions: &[(String, FunctionNode)],
    ) -> Result<(), String> {
        for (module_name_mangled, func) in imported_functions {
            if func.type_params.is_empty() {
                let mangled_name = format!("{}!{}", module_name_mangled, func.name);
                // A module function reads its own module's globals by bare name.
                self.with_module_globals(module_name_mangled, |me| {
                    me.generate_function_with_llvm_name(func, &mangled_name)
                })?;
            }
        }
        Ok(())
    }

    fn generate_main_user_functions(
        &mut self,
        user_functions: &[FunctionNode],
    ) -> Result<(), String> {
        for func in user_functions {
            if func.name == "main" {
                self.generate_function_with_llvm_name(func, "!user!main")?;
            } else {
                self.generate_function(func)?;
            }
        }
        Ok(())
    }

    fn generate_class_methods_for_all_nodes(&mut self, nodes: &[AstNode]) -> Result<(), String> {
        for node in nodes {
            if let AstNode::Class {
                name,
                methods,
                type_params,
                ..
            } = node
            {
                self.generate_class_methods_for_node(name, methods, type_params)?;
            }
        }
        Ok(())
    }

    fn generate_class_methods_for_node(
        &mut self,
        name: &str,
        methods: &[FunctionNode],
        type_params: &[(String, Vec<TraitBound>)],
    ) -> Result<(), String> {
        if !type_params.is_empty() {
            let bounds: ClassTypeParamBounds = type_params
                .iter()
                .map(|(p, b)| {
                    (
                        p.clone(),
                        b.iter().map(|tb| (tb.name.clone(), Vec::new())).collect(),
                    )
                })
                .collect();
            self.analyzer.set_class_type_params(bounds);
        }

        for method in methods {
            let prefixed_name = format!("{}.{}", name, method.name);
            if type_params.is_empty() {
                let mut method_copy = method.clone();
                method_copy.name = prefixed_name;
                self.generate_function(&method_copy)?;
                continue;
            }

            let class_type_param_names: Vec<&str> =
                type_params.iter().map(|(p, _)| p.as_str()).collect();
            if method.is_common
                && method.type_params.is_empty()
                && !Self::method_uses_type_params(method, &class_type_param_names)
            {
                let mut method_copy = method.clone();
                method_copy.name = prefixed_name;
                self.generate_function(&method_copy)?;
            }
        }

        if !type_params.is_empty() {
            self.analyzer.clear_class_type_params();
        }

        Ok(())
    }

    // Helper function to sanitize module paths for use in LLVM identifiers
    fn sanitize_module_path(module_path: &str) -> String {
        module_path.replace(['.', '/'], "_")
    }

    // small helpers for runtime declarations were moved to runtime.rs

    pub fn new(
        context: &'a Context,
        analyzer: &'a mut SemanticAnalyzer,
        source_name: &str,
    ) -> Self {
        let module = context.create_module("mux_module");
        let runtime_signatures = context.create_module("mux_runtime_signatures");
        let builder = context.create_builder();

        Self::declare_runtime_functions(&runtime_signatures, context);

        let mut type_map = HashMap::new();
        let mut enum_variants = HashMap::new();

        let i32_type = context.i32_type();
        let i8_ptr = context.ptr_type(AddressSpace::default());
        let struct_type = context.struct_type(&[i32_type.into(), i8_ptr.into()], false);
        type_map.insert("optional".to_string(), struct_type.into());
        type_map.insert("result".to_string(), struct_type.into());

        let mut ordered_variants = BTreeMap::new();
        ordered_variants.insert(
            "optional".to_string(),
            vec!["some".to_string(), "none".to_string()],
        );
        ordered_variants.insert(
            "result".to_string(),
            vec!["ok".to_string(), "err".to_string()],
        );

        for (enum_name, variants) in ordered_variants {
            enum_variants.insert(enum_name, variants);
        }

        // Seed from the symbol's declared variant order, not from `methods`.
        // Position in this vector is the discriminant (`get_variant_index`), and
        // `methods` is a HashMap, so seeding from its keys made discriminants
        // depend on hash order. `generate_enum_type` overwrites each entry with
        // declaration order before anything reads it, which is why that was
        // never observable - but it left a hash-ordered vector feeding
        // discriminants one refactor away from mattering (issue #344).
        for (name, symbol) in analyzer.all_symbols() {
            if symbol.kind == crate::semantics::SymbolKind::Enum {
                enum_variants.insert(name.clone(), symbol.variants.clone().unwrap_or_default());
            }
        }

        Self {
            context,
            module,
            runtime_signatures,
            builder,
            analyzer,
            type_map,
            vtable_map: HashMap::new(),
            vtable_type_map: HashMap::new(),
            class_copy_fns: HashMap::new(),
            class_destructor_fns: HashMap::new(),
            enum_variants,
            enum_asts: BTreeMap::new(),
            enum_variant_fields: HashMap::new(),
            field_map: HashMap::new(),
            field_types_map: HashMap::new(),
            classes: HashMap::new(),
            constructors: HashMap::new(),
            enum_glue_fns: HashMap::new(),
            lambda_counter: 0,
            string_counter: 0,
            label_counter: 0,
            variables: ScopedVars::new(),
            address_taken: std::collections::HashSet::new(),
            captured_names: std::collections::HashSet::new(),
            cell_slots: std::collections::HashSet::new(),
            global_variables: HashMap::new(),
            module_globals: HashMap::new(),
            current_module_prefix: None,
            functions: HashMap::new(),
            function_nodes: BTreeMap::new(),
            current_function_name: None,
            current_function_return_type: None,
            generic_context: None,
            context_stack: Vec::new(),
            generated_methods: HashMap::new(),
            rc_scope_stack: Vec::new(),
            temp_values: Vec::new(),
            closure_temp_values: Vec::new(),
            enum_temp_values: Vec::new(),
            closure_scope_stack: Vec::new(),
            source_name: source_name.to_string(),
            target_data: inkwell::targets::TargetData::create(""),
        }
    }

    /// ABI store size (in bytes) of an LLVM type, used to size enum union slots.
    pub(super) fn abi_store_size(&self, ty: &BasicTypeEnum<'a>) -> u64 {
        self.target_data.get_store_size(ty)
    }

    /// Render a `file:line:col` location for runtime panic messages, matching
    /// the compiler diagnostic emitter's `--> file:line:col` locator.
    fn panic_location(&self, span: &crate::lexer::Span) -> String {
        format!("{}:{}:{}", self.source_name, span.row_start, span.col_start)
    }

    // Runtime declarations are implemented in the `runtime` submodule to keep
    // the code generator file smaller and data-driven. The real implementation
    // is an associated function on `CodeGenerator` defined in
    // `codegen/runtime.rs`. Calling `Self::declare_runtime_functions` here will
    // resolve to that implementation after the file is compiled.
    //
    // Note: We intentionally leave this method as an empty wrapper by relying on
    // the method provided in the `runtime` module; keeping the call site in
    // `new` unchanged avoids changing call sites elsewhere.
    // runtime declarations moved to `codegen::runtime` impl for CodeGenerator
    /// Create an alloca instruction in the entry block of the current function.
    /// This ensures proper LLVM dominance - allocas must be in the entry block
    /// to be used throughout the function, including in match arms and loops.
    /// Allocate a captured variable's storage cell in the entry block.
    ///
    /// The cell replaces the entry-block alloca a boxed local would get, and is
    /// emitted in the same place for the same reason: it has to dominate every
    /// use, including the scope cleanup that releases it on a path the
    /// declaration may not dominate. Being on the heap is what lets a closure
    /// keep the variable alive after the declaring function returns, and being
    /// reference counted is what lets the variable and several closures share
    /// one location (see `mux_cell_alloc` in mux-runtime).
    fn create_entry_block_cell(
        &mut self,
        function: FunctionValue<'a>,
        name: &str,
    ) -> Result<PointerValue<'a>, String> {
        let alloc = self
            .runtime_function("mux_cell_alloc")
            .ok_or("mux_cell_alloc not found")?;
        let entry = function
            .get_first_basic_block()
            .expect("function should have entry block after creation");
        let saved = self.builder.get_insert_block();
        match entry.get_first_instruction() {
            Some(first) => self.builder.position_before(&first),
            None => self.builder.position_at_end(entry),
        }
        let null = self.context.ptr_type(AddressSpace::default()).const_null();
        let cell = self
            .builder
            .build_call(alloc, &[null.into()], &format!("{}_cell", name))
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or("mux_cell_alloc should return a pointer")?
            .into_pointer_value();
        if let Some(block) = saved {
            self.builder.position_at_end(block);
        }
        self.cell_slots.insert(cell);
        Ok(cell)
    }

    fn create_entry_block_alloca(
        &self,
        function: FunctionValue<'a>,
        ty: BasicTypeEnum<'a>,
        name: &str,
    ) -> Result<PointerValue<'a>, String> {
        let builder = self.context.create_builder();

        let entry = function
            .get_first_basic_block()
            .expect("function should have entry block after creation");
        match entry.get_first_instruction() {
            Some(first_instr) => builder.position_before(&first_instr),
            None => builder.position_at_end(entry),
        }

        let alloca = builder.build_alloca(ty, name).map_err(|e| e.to_string())?;

        // Pointer and inline-struct locals can be hoisted to the entry block
        // even when their declaration is inside conditional control flow, or
        // reused across loop iterations. Zero-initialize them so cleanup and
        // drop-before-store paths never read uninitialized memory: a null
        // pointer is a no-op for `mux_rc_dec`, and a zeroed enum struct has a
        // null active payload so `emit_enum_drop` is a no-op before the first
        // real store.
        match ty {
            BasicTypeEnum::PointerType(_) => {
                let null_ptr = self.context.ptr_type(AddressSpace::default()).const_null();
                builder
                    .build_store(alloca, null_ptr)
                    .map_err(|e| e.to_string())?;
            }
            BasicTypeEnum::StructType(struct_type) => {
                builder
                    .build_store(alloca, struct_type.const_zero())
                    .map_err(|e| e.to_string())?;
            }
            _ => {}
        }

        Ok(alloca)
    }

    /// Create an alloca in the entry block of the current function (inferred from builder position).
    /// If not in a function context, creates alloca at current position.
    pub(super) fn create_entry_alloca(
        &self,
        ty: BasicTypeEnum<'a>,
        name: &str,
    ) -> Result<PointerValue<'a>, String> {
        // try to get the current function from the builder's insert block
        if let Some(block) = self.builder.get_insert_block()
            && let Some(function) = block.get_parent()
        {
            return self.create_entry_block_alloca(function, ty, name);
        }

        // fallback: create alloca at current position (shouldn't happen in normal code)
        self.builder
            .build_alloca(ty, name)
            .map_err(|e| e.to_string())
    }

    pub fn generate(&mut self, nodes: &[AstNode]) -> Result<(), String> {
        let main_module_nodes = nodes;
        let mut all_nodes = Vec::new();
        for module_nodes in self.analyzer.all_module_asts().values() {
            all_nodes.extend(module_nodes.clone());
        }
        all_nodes.extend(nodes.to_vec());
        let nodes = &all_nodes;

        // Decided before any slot is allocated: a variable whose address is
        // taken keeps a boxed slot, because `&x` has to mean what `&list[0]`
        // means and a list element is a `*mut Value`.
        self.address_taken = address_taken::collect(nodes);
        // A captured variable's storage is a shared cell, decided here for the
        // same reason: it has to exist before the variable's slot does.
        self.captured_names = self
            .analyzer
            .lambda_captures
            .values()
            .flatten()
            .map(|(name, _)| name.clone())
            .collect();

        self.generate_user_defined_types(nodes)?;
        self.generate_all_enum_object_support(nodes)?;

        let imported_functions = self.collect_imported_functions();

        self.declare_imported_functions(&imported_functions)?;
        self.declare_main_functions(main_module_nodes)?;
        self.declare_class_methods(nodes)?;
        self.generate_vtables(nodes)?;
        self.generate_enum_and_class_constructors(nodes)?;

        let user_functions = self.collect_non_generic_main_functions(main_module_nodes);
        let main_top_level_statements = self.collect_top_level_statements(main_module_nodes);
        let modules_data = self.collect_module_init_data();

        // Declare each module's globals into its own table, emitted as
        // `module!name`, then the main module's under bare names. Only the
        // module being generated has its globals visible, so two modules
        // declaring the same constant no longer share one slot.
        for (module_name, module_top_level_statements) in &modules_data {
            let sanitized = Self::sanitize_module_path(module_name);
            self.global_variables = HashMap::new();
            self.current_module_prefix = Some(sanitized.clone());
            self.declare_top_level_globals(module_top_level_statements)?;
            self.current_module_prefix = None;
            self.module_globals
                .insert(sanitized, std::mem::take(&mut self.global_variables));
        }
        // Every module's table exists now, so cross-module aliases can be
        // resolved in either direction.
        self.alias_imported_constants_into_modules();
        self.declare_top_level_globals(&main_top_level_statements)?;
        // A constant brought directly into scope (`import cfg.*`, or a selective
        // import) is referenced by its bare name from the importing module, so
        // alias it into that module's view. The slot still lives in the owning
        // module's table - this only makes it reachable, it does not duplicate it.
        self.alias_directly_imported_constants();
        let main_globals = self.global_variables.clone();

        for (module_name, module_top_level_statements) in &modules_data {
            self.with_module_globals(module_name, |me| {
                me.generate_module_init(module_top_level_statements, module_name)
            })?;
        }

        self.global_variables = main_globals.clone();
        let module_name = self.get_module_name(main_module_nodes);
        self.generate_module_init(&main_top_level_statements, &module_name)?;
        self.generate_main_function(&module_name)?;

        self.generate_imported_user_functions(&imported_functions)?;
        self.global_variables = main_globals;
        self.generate_main_user_functions(&user_functions)?;

        // Class methods need the same per-module scoping as free functions: a
        // method on a module's class reads that module's globals by bare name.
        // Generating every class against the main module's table would leave a
        // module constant referenced by one of its methods unresolvable.
        let module_asts: Vec<(String, Vec<AstNode>)> = self
            .analyzer
            .all_module_asts()
            .iter()
            .map(|(path, nodes)| (path.clone(), nodes.clone()))
            .collect();
        for (module_path, module_nodes) in &module_asts {
            self.with_module_globals(module_path, |me| {
                me.generate_class_methods_for_all_nodes(module_nodes)
            })?;
        }
        self.generate_class_methods_for_all_nodes(main_module_nodes)?;

        Ok(())
    }

    /// Compile the module to a native object file.
    ///
    /// The alternative is writing `.ll` and having clang parse it back, which
    /// couples every install to a clang whose major version matches the LLVM
    /// this compiler links - textual IR is not stable across versions. Emitting
    /// the object here removes that coupling: whatever links the result only has
    /// to understand object files.
    ///
    /// LLVM emits into memory and the bytes go to an already-open handle, so no
    /// path is resolved here at all. The caller created that file exclusively
    /// (`create_new`), so the only filesystem entry involved is one it made -
    /// there is no pathname for anything to swap between deciding to write and
    /// writing.
    pub fn emit_object(&self, out: &mut impl Write) -> Result<(), String> {
        self.module
            .verify()
            .map_err(|e| format!("LLVM module verification failed: {}", e.to_string()))?;

        Target::initialize_native(&InitializationConfig::default())
            .map_err(|e| format!("failed to initialize the native target: {}", e))?;

        let triple = TargetMachine::get_default_triple();
        let target = Target::from_triple(&triple)
            .map_err(|e| format!("no LLVM target for {}: {}", triple, e.to_string()))?;

        // PIC because distributions default to position-independent
        // executables; a non-PIC object fails to link against them.
        let machine = target
            .create_target_machine(
                &triple,
                &TargetMachine::get_host_cpu_name().to_string(),
                &TargetMachine::get_host_cpu_features().to_string(),
                OptimizationLevel::None,
                RelocMode::PIC,
                CodeModel::Default,
            )
            .ok_or_else(|| format!("failed to create a target machine for {}", triple))?;

        // Stamp the module with the machine's own triple and layout. Nothing set
        // them before, which is why clang reported "overriding the module target
        // triple"; emitting directly, a mismatch would mean wrong ABI decisions
        // rather than a warning.
        self.module.set_triple(&triple);
        self.module
            .set_data_layout(&machine.get_target_data().get_data_layout());

        let object = machine
            .write_to_memory_buffer(&self.module, FileType::Object)
            .map_err(|e| format!("failed to emit object code: {}", e.to_string()))?;

        out.write_all(object.as_slice())
            .map_err(|e| format!("failed to write the object file: {}", e))
    }

    pub fn emit_ir_to_file(&self, filename: &str) -> Result<(), String> {
        // Write the IR before verifying: when codegen produces an invalid module
        // the verification error is exactly what the emitted `.ll` is needed to
        // debug, so `-i` must still leave the file behind rather than bail first.
        self.module
            .print_to_file(filename)
            .map_err(|e| format!("Failed to write IR: {}", e))?;
        self.module
            .verify()
            .map_err(|e| format!("LLVM module verification failed: {}", e.to_string()))
    }
}

// Re-export all submodules
mod address_taken;
mod classes;
mod constructors;
mod deserialize;
mod expressions;
mod functions;
mod generics;
mod memory;
mod methods;
mod operators;
mod runtime;
mod scoped_vars;
mod statements;
mod types;
mod where_clause;
