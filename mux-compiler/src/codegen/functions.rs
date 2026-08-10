//! Function declaration and generation for the code generator.
//!
//! This module handles:
//! - Function declaration with proper type signatures
//! - Function generation with parameter handling
//! - Module initialization functions
//! - Main function generation

use inkwell::AddressSpace;
use inkwell::types::BasicMetadataTypeEnum;
use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::FunctionValue;

use crate::ast::{AstNode, FunctionNode, PrimitiveType, StatementNode, TypeKind};
use crate::semantics::Type;

use super::CodeGenerator;
use super::scoped_vars::ScopedVars;

impl<'a> CodeGenerator<'a> {
    fn resolve_base_class_name_for_method(method_name: &str) -> &str {
        let class_name = method_name
            .split('.')
            .next()
            .or_else(|| method_name.split('$').next())
            .expect("class method name should contain '.' or '$'");
        class_name.split('$').next().unwrap_or(class_name)
    }

    fn resolve_self_type_for_method(&self, base_class_name: &str) -> Type {
        if let Some(ref context) = self.generic_context
            && let Some(class_symbol) = self.analyzer.symbol_table().lookup(base_class_name)
        {
            let type_args: Vec<Type> = class_symbol
                .type_params
                .iter()
                .filter_map(|(param_name, _bounds)| context.type_params.get(param_name).cloned())
                .collect();
            return Type::Named(base_class_name.to_string(), type_args);
        }

        Type::Named(base_class_name.to_string(), vec![])
    }

    fn setup_method_self_parameter(
        &mut self,
        func: &FunctionNode,
        function: FunctionValue<'a>,
        param_index: &mut u32,
    ) -> Result<(), String> {
        let base_class_name = Self::resolve_base_class_name_for_method(&func.name);
        let self_type = self.resolve_self_type_for_method(base_class_name);
        // `Box$int.get` reads `self` through the instantiation's layout, where
        // `item` is a real `i64`; the erased `Box` layout it was specialized
        // from has a `*mut Value` in that slot.
        let layout_name = match &self_type {
            Type::Named(name, type_args) => self.ensure_class_instantiated(name, type_args)?,
            _ => base_class_name.to_string(),
        };
        let class_type = *self
            .type_map
            .get(&layout_name)
            .expect("class type should be in type_map after type generation");
        let arg = function
            .get_nth_param(*param_index)
            .expect("self parameter should exist for class methods");
        *param_index += 1;

        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let alloca = self
            .builder
            .build_alloca(ptr_type, "self")
            .map_err(|e| e.to_string())?;
        self.builder
            .build_store(alloca, arg)
            .map_err(|e| e.to_string())?;

        self.variables
            .insert("self".to_string(), (alloca, class_type, self_type.clone()));
        self.analyzer.current_self_type = Some(self_type);
        Ok(())
    }

    pub(super) fn is_enum_type(&self, resolved_type: &Type) -> bool {
        matches!(resolved_type, Type::Named(type_name, _) if self
            .analyzer
            .symbol_table()
            .lookup(type_name)
            .map(|s| s.kind == crate::semantics::SymbolKind::Enum)
            .unwrap_or(false))
    }

    fn store_enum_parameter(
        &mut self,
        param_name: &str,
        arg: inkwell::values::BasicValueEnum<'a>,
        resolved_type: Type,
    ) -> Result<(), String> {
        let struct_type = arg.get_type();
        let alloca = self
            .builder
            .build_alloca(struct_type, param_name)
            .map_err(|e| e.to_string())?;
        self.builder
            .build_store(alloca, arg)
            .map_err(|e| e.to_string())?;
        self.variables
            .insert(param_name.to_string(), (alloca, struct_type, resolved_type));
        Ok(())
    }

    fn store_function_parameter(
        &mut self,
        param_name: &str,
        arg: inkwell::values::BasicValueEnum<'a>,
        resolved_type: Type,
    ) -> Result<(), String> {
        let func_ptr_type = self.context.ptr_type(AddressSpace::default());
        let alloca = self
            .builder
            .build_alloca(func_ptr_type, param_name)
            .map_err(|e| e.to_string())?;
        self.builder
            .build_store(alloca, arg)
            .map_err(|e| e.to_string())?;

        self.variables.insert(
            param_name.to_string(),
            (alloca, func_ptr_type.into(), resolved_type),
        );
        Ok(())
    }

    fn store_boxed_parameter(
        &mut self,
        param_name: &str,
        value_to_store: inkwell::values::PointerValue<'a>,
        resolved_type: Type,
    ) -> Result<(), String> {
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        // A captured parameter's storage is a shared cell, like a captured
        // local's: a closure writing to it has to be writing to the parameter
        // the enclosing body reads, not to a copy of it.
        let slot = if self.captured_names.contains(param_name) {
            let function = self
                .builder
                .get_insert_block()
                .and_then(|block| block.get_parent())
                .ok_or("parameter setup needs an enclosing function")?;
            let cell = self.create_entry_block_cell(function, param_name)?;
            self.track_cell_variable(param_name, cell);
            // The cell releases what it holds, so it needs a reference of its
            // own. A parameter's box belongs to the caller, and a freshly boxed
            // scalar is a statement temporary that is released separately -
            // without this the cell hands back a reference it never took, which
            // showed up as a returned closure losing its capture and a corrupt
            // heap.
            self.rc_inc_if_pointer(value_to_store.into())?;
            cell
        } else {
            self.builder
                .build_alloca(ptr_type, param_name)
                .map_err(|e| e.to_string())?
        };
        self.builder
            .build_store(slot, value_to_store)
            .map_err(|e| e.to_string())?;

        self.variables.insert(
            param_name.to_string(),
            (slot, BasicTypeEnum::PointerType(ptr_type), resolved_type),
        );
        Ok(())
    }

    pub(super) fn store_function_parameter_value(
        &mut self,
        param_name: &str,
        arg: inkwell::values::BasicValueEnum<'a>,
        resolved_type: Type,
    ) -> Result<(), String> {
        if matches!(resolved_type, Type::Reference(_)) {
            return self.store_boxed_parameter(param_name, arg.into_pointer_value(), resolved_type);
        }

        if self.is_enum_type(&resolved_type) {
            return self.store_enum_parameter(param_name, arg, resolved_type);
        }

        if matches!(resolved_type, Type::Function { .. }) {
            return self.store_function_parameter(param_name, arg, resolved_type);
        }

        let boxed_value = self.box_value(arg);
        self.store_boxed_parameter(param_name, boxed_value, resolved_type)
    }

    fn setup_function_parameters(
        &mut self,
        func: &FunctionNode,
        function: FunctionValue<'a>,
        start_param_index: u32,
    ) -> Result<(), String> {
        for (i, param) in func.params.iter().enumerate() {
            let arg = function
                .get_nth_param((i as u32) + start_param_index)
                .expect("function parameter should exist at expected index");
            let resolved_type = self
                .analyzer
                .resolve_type(&param.type_)
                .map_err(|e| e.to_string())?;
            self.store_function_parameter_value(&param.name, arg, resolved_type)?;
        }

        Ok(())
    }

    fn resolve_decl_llvm_name(&self, func: &FunctionNode) -> String {
        if func.name.contains('$') {
            return func.name.clone();
        }

        if let Some(symbol) = self.analyzer.symbol_table().lookup(&func.name)
            && let Some(mangled_name) = &symbol.llvm_name
        {
            return mangled_name.clone();
        }

        for module_syms in self.analyzer.imported_symbols().values() {
            if let Some(func_symbol) = module_syms.get(&func.name)
                && let Some(mangled) = &func_symbol.llvm_name
            {
                return mangled.clone();
            }
        }

        func.name.clone()
    }

    /// Add a function declaration to the module under `llvm_name` and record
    /// it in the function table. Centralizes the param-type, return-type, and
    /// module-registration logic shared by the two public entry points.
    fn add_function_declaration(
        &mut self,
        func: &FunctionNode,
        llvm_name: &str,
    ) -> Result<(), String> {
        // A signature may be the first mention of a generic enum instantiation,
        // and it is declared before any body that could construct one.
        for param in &func.params {
            self.instantiate_generic_types_in_type_node(&param.type_)?;
        }
        self.instantiate_generic_types_in_type_node(&func.return_type)?;

        let mut param_types: Vec<BasicMetadataTypeEnum> = func
            .params
            .iter()
            .map(|p| self.llvm_type_from_mux_type(&p.type_).map(|t| t.into()))
            .collect::<Result<_, _>>()?;

        let is_class_method = func.name.contains('.');
        if is_class_method && !func.is_common {
            param_types.insert(0, self.context.ptr_type(AddressSpace::default()).into());
        }

        // Boxing applies to a specialized class METHOD, whose caller boxes its
        // arguments to match (`build_method_call_args`). A generic free
        // function passes raw values, so `$` alone must not select this - a
        // generic function instance is `identity$$int`, with no `.` in it.
        let is_specialized_method = func.name.contains('$') && is_class_method;
        let is_static = func.is_common;
        if is_specialized_method && !is_static {
            let ptr_type = self.context.ptr_type(AddressSpace::default());
            param_types = param_types
                .into_iter()
                .enumerate()
                .map(|(i, param_type)| {
                    if i == 0 && is_class_method && !func.is_common {
                        param_type
                    } else {
                        ptr_type.into()
                    }
                })
                .collect();
        }

        let fn_type = if matches!(
            func.return_type.kind,
            TypeKind::Primitive(PrimitiveType::Void)
        ) {
            self.context.void_type().fn_type(&param_types, false)
        } else {
            let return_type = self.llvm_type_from_mux_type(&func.return_type)?;
            return_type.fn_type(&param_types, false)
        };

        let function = self.module.add_function(llvm_name, fn_type, None);
        self.functions.insert(llvm_name.to_string(), function);
        Ok(())
    }

    pub(super) fn declare_function(&mut self, func: &FunctionNode) -> Result<(), String> {
        let llvm_name = self.resolve_decl_llvm_name(func);
        self.add_function_declaration(func, &llvm_name)?;
        self.function_nodes.insert(func.name.clone(), func.clone());
        Ok(())
    }

    pub(super) fn declare_function_with_name(
        &mut self,
        func: &FunctionNode,
        llvm_name: &str,
    ) -> Result<(), String> {
        self.add_function_declaration(func, llvm_name)
    }

    pub(super) fn generate_module_init(
        &mut self,
        top_level_statements: &[StatementNode],
        module_name: &str,
    ) -> Result<(), String> {
        // Use ! prefix to avoid conflicts with user-defined functions
        // (! is used for module-level generated code, $ is used for generic specializations)
        let init_name = format!("!{}!init", module_name.replace(['.', '/'], "_"));

        let init_type = self.context.void_type().fn_type(&[], false);
        let init_func = self.module.add_function(&init_name, init_type, None);
        let entry = self.context.append_basic_block(init_func, "entry");
        self.builder.position_at_end(entry);

        // copy global_variables to variables so statements can access/initialize them
        self.variables = ScopedVars::from_bindings(self.global_variables.clone());

        // Isolate RC scope and statement temporaries for this init body, exactly
        // as generate_function does: nested function generation triggered while
        // emitting top-level statements must not see or clean up init's temps
        // (and vice versa), or cleanup would emit a cross-function instruction
        // reference. Give init its own RC scope so block-local bindings created
        // by top-level statements (loop variables, match-arm bindings) are
        // released at the end of init. Top-level global declarations reuse their
        // pre-declared global slots (the `existing_var` path in declare_variable)
        // and are NOT tracked here, so they survive for a later user `main()`.
        let saved_rc_scope_stack = std::mem::take(&mut self.rc_scope_stack);
        let saved_temp_values = std::mem::take(&mut self.temp_values);
        let saved_enum_temp_values = std::mem::take(&mut self.enum_temp_values);
        let saved_closure_scope_stack = std::mem::take(&mut self.closure_scope_stack);
        let saved_closure_temp_values = std::mem::take(&mut self.closure_temp_values);
        self.push_rc_scope();

        // Execute top-level statements as module initialization
        for stmt in top_level_statements {
            self.generate_statement(stmt, Some(&init_func))?;
        }

        // Release module-init locals before returning. Only runs when the entry
        // block is still open (top-level code cannot early-return).
        if self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_terminator())
            .is_none()
        {
            self.generate_all_scopes_cleanup()?;
        }

        self.rc_scope_stack = saved_rc_scope_stack;
        self.temp_values = saved_temp_values;
        self.enum_temp_values = saved_enum_temp_values;
        self.closure_scope_stack = saved_closure_scope_stack;
        self.closure_temp_values = saved_closure_temp_values;

        self.builder.build_return(None).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub(super) fn generate_main_function(&mut self, module_name: &str) -> Result<(), String> {
        let main_type = self.context.i32_type().fn_type(&[], false);
        let main_func = self.module.add_function("main", main_type, None);
        let entry = self.context.append_basic_block(main_func, "entry");
        self.builder.position_at_end(entry);

        // Call imported module init functions in dependency order
        // This ensures modules are initialized before use
        for module_path in &self.analyzer.module_dependencies {
            let init_name = format!("!{}!init", Self::sanitize_module_path(module_path));
            if let Some(init_func) = self.module.get_function(&init_name) {
                self.builder
                    .build_call(
                        init_func,
                        &[],
                        &format!("{}_init_call", module_path.replace('.', "_")),
                    )
                    .map_err(|e| e.to_string())?;
            }
        }

        // Call main module init function
        let init_name = format!("!{}!init", Self::sanitize_module_path(module_name));
        if let Some(init_func) = self.module.get_function(&init_name) {
            self.builder
                .build_call(init_func, &[], "init_call")
                .map_err(|e| e.to_string())?;
        }

        // Call user-defined main function if it exists
        if let Some(user_main) = self.module.get_function("!user!main") {
            self.builder
                .build_call(user_main, &[], "user_main_call")
                .map_err(|e| e.to_string())?;
        }

        self.emit_global_teardown()?;

        // return 0 from main
        self.builder
            .build_return(Some(&self.context.i32_type().const_int(0, false)))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Release every owned global variable once at program exit so persistent
    /// globals are not reported as leaked. Reference and function globals borrow
    /// their target and must not be decremented.
    ///
    /// Deduplicated by slot, not by name: a directly-imported constant is
    /// aliased into the importing module's view under its local name, so one
    /// slot can be reachable under several names (`import cfg.*` plus
    /// `import cfg.LIMIT as CAP` binds the same storage twice). Decrementing per
    /// name would over-release it and free the value out from under the
    /// remaining references. Sorted so the emitted IR stays deterministic.
    ///
    /// Covers the module tables as well as the main-module view. A module's
    /// constants live only in its own table unless the main module imported
    /// them directly, so draining just `global_variables` would leave every
    /// module-private constant (one reached as `cfg.NAME`, never imported) at a
    /// permanent refcount of 1. Valgrind reports those as "still reachable"
    /// rather than lost, because the global slot still points at them, so the
    /// leak legs of CI stay green while the memory is never freed.
    fn emit_global_teardown(&mut self) -> Result<(), String> {
        let mut owned_globals = self.collect_teardown_slots();
        owned_globals.sort_by(|a, b| a.0.cmp(&b.0));
        let mut released = std::collections::HashSet::new();
        owned_globals.retain(|(_, slot)| released.insert(slot.alloca()));
        // Reuse the local-scope cleanup dispatch: boxed globals are decremented
        // directly and inline enum-struct globals get variant-tag drop-glue.
        self.generate_cleanup_for_vars(&owned_globals)
    }

    /// Every RC-owned global slot that program exit is responsible for
    /// releasing, gathered from the main-module view and every module table.
    /// Names are qualified by module so the emitted IR labels stay unique;
    /// callers dedupe by slot, since one slot can appear under several names.
    fn collect_teardown_slots(&self) -> Vec<(String, super::RcSlot<'a>)> {
        let main_table = std::iter::once((None, &self.global_variables));
        let module_tables = self
            .module_globals
            .iter()
            .map(|(module, table)| (Some(module.as_str()), table));

        main_table
            .chain(module_tables)
            .flat_map(|(module, table)| {
                table
                    .iter()
                    .filter_map(move |(name, (alloca, llvm_type, ty))| {
                        let label = match module {
                            Some(module) => format!("{}_{}", module, name),
                            None => name.clone(),
                        };
                        // Inline custom-enum globals are value-semantic structs,
                        // not boxed RC pointers, so they need variant-tag drop-glue
                        // to release their active variant's pointer payloads. Only
                        // enums that actually own a pointer payload are worth a
                        // slot.
                        if let Some(enum_name) = self.user_enum_type_name(ty) {
                            if self.enum_has_rc_payload(&enum_name) {
                                return Some((
                                    label,
                                    super::RcSlot::EnumStruct {
                                        enum_name,
                                        alloca: *alloca,
                                    },
                                ));
                            }
                            return None;
                        }
                        // Otherwise only globals whose storage is a boxed RC
                        // pointer can be decremented; loading and decrementing an
                        // inline non-pointer would dereference a non-pointer.
                        // References and functions borrow their target and are
                        // managed elsewhere. Driven by the slot, so a scalar
                        // global kept boxed because its address is taken is
                        // still released.
                        if !self.slot_owns_boxed_contents(*llvm_type, ty) {
                            return None;
                        }
                        Some((label, super::RcSlot::Boxed(*alloca)))
                    })
            })
            .collect()
    }

    pub(super) fn get_module_name(&self, nodes: &[AstNode]) -> String {
        // Try to get module name from first class or function
        for node in nodes {
            match node {
                AstNode::Class { name, .. } => {
                    return name.split('.').next().unwrap_or("main").to_string();
                }
                AstNode::Function(func) => {
                    return func.name.split('.').next().unwrap_or("main").to_string();
                }
                _ => {}
            }
        }
        "main".to_string()
    }

    pub(super) fn generate_function(&mut self, func: &FunctionNode) -> Result<(), String> {
        self.generate_function_recorded(func, &func.name)
    }

    /// Generate `func`'s body while recording `record_name` as the current
    /// function name. For a plain function this is `func.name`; for an imported
    /// module function it is the mangled `module!name`, so a call to a sibling in
    /// the same module (including a recursive self-call) resolves through
    /// `find_nested_mangled_function_name` to `module!sibling` instead of the
    /// bare, non-existent LLVM symbol.
    fn generate_function_recorded(
        &mut self,
        func: &FunctionNode,
        record_name: &str,
    ) -> Result<(), String> {
        // Save state that might be overwritten by nested function generation
        // (e.g., when generating specialized methods for generic classes used in this function)
        let saved_function_name = self.current_function_name.take();
        let saved_return_type = self.current_function_return_type.take();
        let saved_self_type = self.analyzer.current_self_type.take();

        // Save the RC scope stack from any parent function context.
        // Each function has its own isolated RC scope - nested function generation
        // (specialized methods, generic instantiation, etc.) should not see or clean up
        // variables from the calling function's scope.
        let saved_rc_scope_stack = std::mem::take(&mut self.rc_scope_stack);
        // Statement temporaries are likewise per-function: a temporary produced
        // while generating this body must never be cleaned up in another
        // function (which would emit a cross-function instruction reference).
        let saved_temp_values = std::mem::take(&mut self.temp_values);
        // Inline-enum temporaries are per-function for the same reason (a value
        // returned by an enum-returning call is tracked here, issue #309).
        let saved_enum_temp_values = std::mem::take(&mut self.enum_temp_values);
        // Closure temporaries/scopes are per-function for the same reason.
        let saved_closure_scope_stack = std::mem::take(&mut self.closure_scope_stack);
        let saved_closure_temp_values = std::mem::take(&mut self.closure_temp_values);

        self.current_function_name = Some(record_name.to_string());
        self.current_function_return_type = Some(
            self.analyzer
                .resolve_type(&func.return_type)
                .map_err(|e| e.to_string())?,
        );

        // Open the scope this function's locals are published into, so the
        // analyzer types expressions against what is in scope here rather than
        // through its flat program-wide index. Nested generation opens its own
        // and closes it, so an inner function shadows this one while it runs.
        self.analyzer.symbol_table_mut().open_codegen_scope();

        let function = *self
            .functions
            .get(&func.name)
            .ok_or("Function not declared")?;

        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        // clear variables for new scope
        self.variables.clear();

        // Push RC scope for this function (on a fresh stack)
        self.push_rc_scope();

        // set up parameter variables
        let is_class_method = func.name.contains('.');
        let mut param_index = 0u32;
        if is_class_method && !func.is_common {
            self.setup_method_self_parameter(func, function, &mut param_index)?;
        }
        self.setup_function_parameters(func, function, param_index)?;

        // enforce where-clause preconditions before the body runs
        self.emit_function_preconditions(func)?;

        // generate function body
        for stmt in &func.body {
            self.generate_statement(stmt, Some(&function))?;
        }

        // if void return, add return void if not already terminated
        if matches!(
            func.return_type.kind,
            TypeKind::Primitive(PrimitiveType::Void)
        ) && let Some(block) = self.builder.get_insert_block()
            && block.get_terminator().is_none()
        {
            // Generate cleanup for all RC variables before returning
            self.generate_all_scopes_cleanup()?;
            self.builder.build_return(None).map_err(|e| e.to_string())?;
        } else if let Some(block) = self.builder.get_insert_block()
            && block.get_terminator().is_none()
        {
            // Non-void: the analyzer guarantees every path returns, so an
            // open block here is unreachable join-point fallout (e.g. a
            // tail-position match whose arms return through nested if/else).
            // Terminate it so the module verifies.
            self.builder
                .build_unreachable()
                .map_err(|e| e.to_string())?;
        }

        // Pop the function's RC scope (no cleanup needed here since we already
        // cleaned up before returns, and non-void functions must have explicit returns)
        self.rc_scope_stack.pop();
        self.closure_scope_stack.pop();

        self.analyzer.symbol_table_mut().close_codegen_scope();

        // Restore previous function context
        self.current_function_name = saved_function_name;
        self.current_function_return_type = saved_return_type;
        self.analyzer.current_self_type = saved_self_type;

        // Restore the parent function's RC scope stack and temporaries. Any
        // temporaries still pending here belonged to this body and have already
        // been decremented on its return paths.
        self.rc_scope_stack = saved_rc_scope_stack;
        self.temp_values = saved_temp_values;
        self.enum_temp_values = saved_enum_temp_values;
        self.closure_scope_stack = saved_closure_scope_stack;
        self.closure_temp_values = saved_closure_temp_values;

        Ok(())
    }

    // Generate function with explicit LLVM name (for imported module functions)
    pub(super) fn generate_function_with_llvm_name(
        &mut self,
        func: &FunctionNode,
        llvm_name: &str,
    ) -> Result<(), String> {
        // Look up by LLVM name instead of source name
        let function = *self.functions.get(llvm_name).ok_or_else(|| {
            format!(
                "Function {} not declared (LLVM name: {})",
                func.name, llvm_name
            )
        })?;

        // Delegate to the regular implementation
        // but first we need to temporarily store it under the source name too
        self.functions.insert(func.name.clone(), function);
        let result = self.generate_function_recorded(func, llvm_name);
        // Remove the temporary entry
        self.functions.remove(&func.name);
        result
    }
}
