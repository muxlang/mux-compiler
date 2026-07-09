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

use inkwell::AddressSpace;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{FunctionValue, PointerValue};
use std::collections::HashMap;

use crate::ast::{
    AstNode, EnumVariantField, Field, FunctionNode, StatementKind, StatementNode, TraitBound,
    TypeNode,
};
use crate::semantics::{GenericContext, SemanticAnalyzer, Type, Type as ResolvedType};

type ClassTypeParamBounds = Vec<(String, Vec<(String, Vec<Type>)>)>;
type EnumVariantFieldMap = HashMap<String, HashMap<String, Vec<EnumVariantField>>>;

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
    enum_variant_fields: EnumVariantFieldMap,
    field_map: HashMap<String, HashMap<String, usize>>,
    field_types_map: HashMap<String, Vec<BasicTypeEnum<'a>>>,
    classes: HashMap<String, Vec<Field>>,
    constructors: HashMap<String, FunctionValue<'a>>,
    lambda_counter: usize,
    string_counter: usize,
    label_counter: usize,
    variables: HashMap<String, (PointerValue<'a>, BasicTypeEnum<'a>, ResolvedType)>,
    global_variables: HashMap<String, (PointerValue<'a>, BasicTypeEnum<'a>, ResolvedType)>,
    functions: HashMap<String, FunctionValue<'a>>,
    function_nodes: HashMap<String, FunctionNode>,
    current_function_name: Option<String>,
    current_function_return_type: Option<ResolvedType>,
    generic_context: Option<GenericContext>,
    context_stack: Vec<GenericContext>,
    generated_methods: HashMap<String, bool>,
    rc_scope_stack: Vec<Vec<(String, PointerValue<'a>)>>,
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
    source_name: String,
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
        let global = self.module.add_global(llvm_type, None, name);
        global.set_initializer(&llvm_type.const_zero());
        self.global_variables.insert(
            name.to_string(),
            (global.as_pointer_value(), llvm_type, resolved_type),
        );
    }

    fn llvm_global_type_for_resolved_type(
        &mut self,
        resolved_type: &ResolvedType,
    ) -> Result<BasicTypeEnum<'a>, String> {
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
        let llvm_type = self.llvm_global_type_for_resolved_type(&resolved_type)?;
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
        let llvm_type = self.llvm_global_type_for_resolved_type(&resolved_type)?;
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

    fn generate_imported_user_functions(
        &mut self,
        imported_functions: &[(String, FunctionNode)],
    ) -> Result<(), String> {
        for (module_name_mangled, func) in imported_functions {
            if func.type_params.is_empty() {
                let mangled_name = format!("{}!{}", module_name_mangled, func.name);
                self.generate_function_with_llvm_name(func, &mangled_name)?;
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

        use std::collections::BTreeMap;
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

        for (name, symbol) in analyzer.all_symbols() {
            if symbol.kind == crate::semantics::SymbolKind::Enum {
                let mut variants = vec![];
                for method_name in symbol.methods.keys() {
                    variants.push(method_name.clone());
                }
                enum_variants.insert(name.clone(), variants);
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
            enum_variant_fields: HashMap::new(),
            field_map: HashMap::new(),
            field_types_map: HashMap::new(),
            classes: HashMap::new(),
            constructors: HashMap::new(),
            lambda_counter: 0,
            string_counter: 0,
            label_counter: 0,
            variables: HashMap::new(),
            global_variables: HashMap::new(),
            functions: HashMap::new(),
            function_nodes: HashMap::new(),
            current_function_name: None,
            current_function_return_type: None,
            generic_context: None,
            context_stack: Vec::new(),
            generated_methods: HashMap::new(),
            rc_scope_stack: Vec::new(),
            temp_values: Vec::new(),
            source_name: source_name.to_string(),
        }
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

        // Pointer locals can be hoisted to the entry block even when their
        // declaration is inside conditional control flow. Initialize them to
        // null so cleanup paths never decrement uninitialized memory.
        if matches!(ty, BasicTypeEnum::PointerType(_)) {
            let null_ptr = self.context.ptr_type(AddressSpace::default()).const_null();
            builder
                .build_store(alloca, null_ptr)
                .map_err(|e| e.to_string())?;
        }

        Ok(alloca)
    }

    /// Create an alloca in the entry block of the current function (inferred from builder position).
    /// If not in a function context, creates alloca at current position.
    fn create_entry_alloca(
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

        self.generate_user_defined_types(nodes)?;

        let imported_functions = self.collect_imported_functions();

        self.declare_imported_functions(&imported_functions)?;
        self.declare_main_functions(main_module_nodes)?;
        self.declare_class_methods(nodes)?;
        self.generate_vtables(nodes)?;
        self.generate_enum_and_class_constructors(nodes)?;

        let top_level_statements = self.collect_top_level_statements(nodes);
        let user_functions = self.collect_non_generic_main_functions(main_module_nodes);
        let main_top_level_statements = self.collect_top_level_statements(main_module_nodes);

        self.declare_top_level_globals(&top_level_statements)?;

        let modules_data = self.collect_module_init_data();

        for (module_name, module_top_level_statements) in modules_data {
            self.generate_module_init(&module_top_level_statements, &module_name)?;
        }

        let module_name = self.get_module_name(main_module_nodes);
        self.generate_module_init(&main_top_level_statements, &module_name)?;
        self.generate_main_function(&module_name)?;

        self.generate_imported_user_functions(&imported_functions)?;
        self.generate_main_user_functions(&user_functions)?;
        self.generate_class_methods_for_all_nodes(nodes)?;

        Ok(())
    }

    pub fn emit_ir_to_file(&self, filename: &str) -> Result<(), String> {
        self.module
            .verify()
            .map_err(|e| format!("LLVM module verification failed: {}", e.to_string()))?;
        self.module
            .print_to_file(filename)
            .map_err(|e| format!("Failed to write IR: {}", e))
    }
}

// Re-export all submodules
mod classes;
mod constructors;
mod expressions;
mod functions;
mod generics;
mod memory;
mod methods;
mod operators;
mod runtime;
mod statements;
mod types;
mod where_clause;
