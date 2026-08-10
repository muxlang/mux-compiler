//! Expression generation for the code generator.
//!
//! This module handles:
//! - All expression types (literals, identifiers, binary ops, function calls, etc.)
//! - If expressions with phi nodes
//! - Lambda expressions with capture handling
//! - List/map/set literals
//! - Field access and method calls
//! - Unary operators (ref, deref, incr, decr, not, neg)
//! - Index access
//! - Match expressions

#![allow(clippy::collapsible_if, clippy::let_and_return)]

use inkwell::AddressSpace;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, PointerValue};

use crate::ast::{
    BinaryOp, ExpressionKind, ExpressionNode, FunctionNode, LiteralNode, Param, PrimitiveType,
    Spanned, StatementNode, TypeKind, TypeNode, UnaryOp,
};
use crate::lexer::Span;
use crate::semantics::{GenericContext, SymbolKind, Type};

use super::CodeGenerator;

/// Stdlib module prefixes that map to runtime functions via the "mux_" prefix.
/// Adding a new stdlib module only requires adding its prefix here and
/// declaring the corresponding LLVM functions in codegen/mod.rs.
impl<'a> CodeGenerator<'a> {
    fn build_import_call_args(
        &mut self,
        args: &[ExpressionNode],
        func_type: Option<&Type>,
        llvm_func: Option<inkwell::values::FunctionValue<'a>>,
    ) -> Result<(Vec<BasicMetadataValueEnum<'a>>, Vec<PointerValue<'a>>), String> {
        let param_types = match func_type {
            Some(Type::Function { params, .. }) => Some(params.as_slice()),
            _ => None,
        };
        let llvm_param_types = llvm_func.map(|f| f.get_type().get_param_types());

        let mut call_args = Vec::with_capacity(args.len());
        // C strings extracted for `string` parameters are owned copies
        // (mux_value_get_string returns an into_raw pointer). Imported/runtime
        // functions borrow their `*const c_char` arguments, so the caller must
        // free these once the call returns.
        let mut owned_cstrings = Vec::new();
        for (idx, arg) in args.iter().enumerate() {
            let arg_val = self.generate_expression(arg)?;
            let mut coerced = arg_val;
            let mut coerced_owned_cstr = false;
            if let Some(params) = param_types
                && let Some(param_type) = params.get(idx)
            {
                coerced_owned_cstr = matches!(param_type, Type::Primitive(PrimitiveType::Str))
                    && coerced.is_pointer_value();
                coerced = self.coerce_import_arg(coerced, param_type)?;
            }

            if let Some(ref param_types) = llvm_param_types
                && let Some(expected) = param_types.get(idx)
            {
                coerced = self.coerce_to_llvm_param_type(coerced, *expected)?;
            }
            if coerced_owned_cstr && coerced.is_pointer_value() {
                owned_cstrings.push(coerced.into_pointer_value());
            }
            call_args.push(coerced.into());
        }
        Ok((call_args, owned_cstrings))
    }

    /// Free owned C strings that were extracted for imported/runtime string
    /// arguments after the call that borrowed them has returned.
    fn free_import_arg_cstrings(
        &mut self,
        owned_cstrings: &[PointerValue<'a>],
    ) -> Result<(), String> {
        if owned_cstrings.is_empty() {
            return Ok(());
        }
        let free_fn = self
            .runtime_function("mux_free_string")
            .ok_or("mux_free_string not found")?;
        for cstr in owned_cstrings {
            self.builder
                .build_call(free_fn, &[(*cstr).into()], "free_import_cstr")
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn coerce_to_llvm_param_type(
        &self,
        value: BasicValueEnum<'a>,
        expected: BasicMetadataTypeEnum<'a>,
    ) -> Result<BasicValueEnum<'a>, String> {
        if value.is_int_value()
            && let BasicMetadataTypeEnum::IntType(expected_int) = expected
        {
            let actual_int = value.into_int_value();
            let actual_width = actual_int.get_type().get_bit_width();
            let expected_width = expected_int.get_bit_width();

            if actual_width < expected_width {
                let widened = self
                    .builder
                    .build_int_z_extend(actual_int, expected_int, "ffi_int_widen")
                    .map_err(|e| e.to_string())?;
                return Ok(widened.into());
            }
            if actual_width > expected_width {
                let narrowed = self
                    .builder
                    .build_int_truncate(actual_int, expected_int, "ffi_int_narrow")
                    .map_err(|e| e.to_string())?;
                return Ok(narrowed.into());
            }
        }
        Ok(value)
    }

    /// Coerce arguments for FFI/calling convention compatibility.
    /// This is NOT type coercion (types are already verified by semantic analysis).
    /// Instead, this converts from Mux representation to C/FFI representation:
    /// - Strings: extract raw *mut c_char from boxed Mux string
    /// - Complex types (Variable, Optional, Named): box the value
    ///
    /// This is necessary infrastructure for calling external/runtime functions.
    fn coerce_import_arg(
        &mut self,
        arg_val: BasicValueEnum<'a>,
        param_type: &Type,
    ) -> Result<BasicValueEnum<'a>, String> {
        match param_type {
            Type::Primitive(PrimitiveType::Str) => {
                if !arg_val.is_pointer_value() {
                    return Err("String arguments must be boxed values".to_string());
                }
                let cstr = self.extract_c_string_from_value(arg_val.into_pointer_value())?;
                Ok(cstr.into())
            }
            Type::Variable(_) | Type::Optional(_) | Type::Named(..) => {
                let boxed = self.box_value(arg_val);
                Ok(boxed.into())
            }
            _ => Ok(arg_val),
        }
    }

    /// Helper function to resolve the class/struct name from any expression
    /// Uses the semantic analyzer to get the expression type
    /// The codegen layout an expression's object is laid out with, which for a
    /// generic class is the instantiation rather than the declaration: a
    /// `Box<int>` reads its fields out of `Box$int`, whose `item` slot is a real
    /// `i64`, not the `*mut Value` the erased `Box` shares with every other
    /// instantiation. A non-generic class is its own layout, so this is the
    /// class name unchanged.
    fn resolve_expression_class_name(&mut self, expr: &ExpressionNode) -> Option<String> {
        let object_type = if let ExpressionKind::Identifier(name) = &expr.kind
            && let Some((_, _, var_type)) = self
                .variables
                .get(name)
                .or_else(|| self.global_variables.get(name))
        {
            var_type.clone()
        } else {
            self.get_resolved_expression_type(expr).ok()?
        };
        let named = match &object_type {
            Type::Named(..) => &object_type,
            Type::Reference(inner) => inner.as_ref(),
            _ => return None,
        };
        let Type::Named(type_name, type_args) = named else {
            return None;
        };
        // Only a class has a stamped-out layout to name. Every other named type
        // reaching here - an enum, a built-in like `Csv` - keeps its own name,
        // which is what the callers that fall through to non-class handling
        // match on.
        if !self.classes.contains_key(type_name) {
            return Some(type_name.clone());
        }
        Some(self.class_layout_name(type_name, type_args))
    }

    fn generate_csv_field_access(
        &mut self,
        expr: &ExpressionNode,
        field: &str,
    ) -> Result<BasicValueEnum<'a>, String> {
        if field != "headers" && field != "rows" {
            return Err(format!("Csv has no field '{}'", field));
        }
        let csv_value = self.generate_expression(expr)?;
        let raw_map = self
            .generate_runtime_call("mux_value_get_map", &[csv_value.into()])
            .ok_or_else(|| "mux_value_get_map should return a value".to_string())?;
        let key_value = self.csv_field_key_value(field)?;
        let map_get = self
            .generate_runtime_call("mux_map_get", &[raw_map.into(), key_value.into()])
            .ok_or_else(|| "mux_map_get should return a value".to_string())?
            .into_pointer_value();
        // mux_map_get borrows the key, so the boxed key string is ours to free.
        if key_value.is_pointer_value() {
            self.emit_value_decref(key_value.into_pointer_value())?;
        }
        let value = self
            .generate_runtime_call("mux_optional_get_value", &[map_get.into()])
            .ok_or_else(|| "mux_optional_get_value should return a value".to_string())?;
        // mux_map_get returns an owned (+1) optional. mux_free_optional is a
        // runtime no-op, so release it with the refcount decrement.
        self.emit_value_decref(map_get)?;
        let free_map = self
            .runtime_function("mux_free_map")
            .ok_or("mux_free_map not found")?;
        let _ = self
            .builder
            .build_call(free_map, &[raw_map.into()], "free_csv_map");
        // mux_optional_get_value returns an owned (+1) copy of the payload;
        // register it so it is released at statement end unless transferred.
        self.register_temp(value);
        Ok(value)
    }

    fn csv_field_key_value(&mut self, field: &str) -> Result<BasicValueEnum<'a>, String> {
        let key_global = self
            .builder
            .build_global_string_ptr(field, &format!("csv_field_key_{}", field))
            .map_err(|e| e.to_string())?;
        let key_ptr = key_global.as_pointer_value();
        self.generate_runtime_call("mux_new_string_from_cstr", &[key_ptr.into()])
            .ok_or_else(|| "mux_new_string_from_cstr should return a value".to_string())
    }

    /// Helper to generate a call to the mux_print runtime function.
    /// Used for both direct print() calls and std.print() static method calls.
    pub(super) fn generate_print_call(
        &mut self,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        if args.len() != 1 {
            return Err("print takes 1 argument".to_string());
        }
        let arg_val = self.generate_expression(&args[0])?;
        let func_print = self
            .runtime_function("mux_print")
            .ok_or("mux_print not found")?;
        self.builder
            .build_call(func_print, &[arg_val.into()], "print_call")
            .map_err(|e| e.to_string())?;
        // Return void, but since BasicValueEnum is required, return a dummy
        Ok(self.context.i32_type().const_int(0, false).into())
    }

    /// Helper to generate a call to the mux_read_line runtime function.
    /// Used for both direct read_line() calls and std.read_line() static method calls.
    pub(super) fn generate_read_line_call(
        &mut self,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        if !args.is_empty() {
            return Err("read_line takes 0 arguments".to_string());
        }
        let func_read_line = self
            .runtime_function("mux_read_line")
            .ok_or("mux_read_line not found")?;
        let call = self
            .builder
            .build_call(func_read_line, &[], "read_line_call")
            .map_err(|e| e.to_string())?;
        let cstr_ptr = call
            .try_as_basic_value()
            .basic()
            .ok_or("mux_read_line returned no value")?
            .into_pointer_value();
        self.box_string_value(cstr_ptr)
    }

    fn try_resolve_module_class_static_call(
        &mut self,
        module_name: &str,
        class_name: &str,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let symbol = match self.analyzer.symbol_table().lookup(module_name) {
            Some(s) => s,
            None => return Ok(None),
        };

        if symbol.kind != crate::semantics::SymbolKind::Import {
            return Ok(None);
        }

        let module_syms = match self.analyzer.imported_symbols().get(module_name) {
            Some(syms) => syms,
            None => return Ok(None),
        };

        let class_symbol = match module_syms.get(class_name) {
            Some(sym) => sym,
            None => return Ok(None),
        };

        // An imported enum reaches its variants the same way a class reaches a
        // static method, and imported enums are registered under their bare name,
        // so the construction is the ordinary one from here (issue #368).
        if class_symbol.kind == crate::semantics::SymbolKind::Enum {
            if !class_symbol.methods.contains_key(method_name) {
                return Ok(None);
            }
            return self.generate_enum_symbol_method_call(class_name, method_name, args);
        }

        if class_symbol.kind != crate::semantics::SymbolKind::Class {
            return Ok(None);
        }

        if !class_symbol.methods.contains_key(method_name) {
            return Ok(None);
        }

        if let Some(call) =
            self.try_generate_net_static_method_call(class_name, method_name, args)?
        {
            return Ok(Some(call));
        }

        let mut call_args = vec![];
        for arg in args {
            call_args.push(self.generate_expression(arg)?.into());
        }

        let func_name = format!("{}.{}", class_name, method_name);
        match self.module.get_function(&func_name) {
            Some(func) => {
                let call = self
                    .builder
                    .build_call(func, &call_args, &format!("{}_call", func_name))
                    .map_err(|e| e.to_string())?;
                Ok(Some(
                    call.try_as_basic_value()
                        .basic()
                        .ok_or("static method call should return a basic value")?,
                ))
            }
            None => Err(format!(
                "Static method {} not found for class {}",
                method_name, class_name
            )),
        }
    }

    fn call_import_or_runtime_function(
        &mut self,
        llvm_function_name: &str,
        display_name: &str,
        args: &[ExpressionNode],
        func_type: Option<&Type>,
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        match llvm_function_name {
            "mux_print" => return self.generate_print_call(args).map(Some),
            "mux_read_line" => return self.generate_read_line_call(args).map(Some),
            _ => {}
        }

        let Some(func) = self.runtime_function(llvm_function_name) else {
            return Ok(None);
        };

        let (call_args, owned_cstrings) =
            self.build_import_call_args(args, func_type, Some(func))?;
        let call = self
            .builder
            .build_call(func, &call_args, &format!("{}_call", display_name))
            .map_err(|e| e.to_string())?;
        self.free_import_arg_cstrings(&owned_cstrings)?;
        let result = self.call_result_or_default_i32(call);
        // Value-returning stdlib/runtime functions (json.parse, env.get, csv.*,
        // ...) hand back a freshly allocated owned value; register it so it is
        // released at statement end unless ownership is transferred. No-op for
        // the i32/void default result.
        self.register_temp(result);
        Ok(Some(result))
    }

    fn call_imported_symbol_function(
        &mut self,
        function_symbol: Option<crate::semantics::Symbol>,
        fallback_name: &str,
        display_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let func_type = function_symbol.as_ref().and_then(|s| s.type_.clone());
        let llvm_function_name = function_symbol
            .as_ref()
            .and_then(|s| s.llvm_name.clone())
            .unwrap_or_else(|| fallback_name.to_string());
        self.call_import_or_runtime_function(
            &llvm_function_name,
            display_name,
            args,
            func_type.as_ref(),
        )
    }

    fn build_call_args_from_expressions(
        &mut self,
        args: &[ExpressionNode],
    ) -> Result<Vec<BasicMetadataValueEnum<'a>>, String> {
        let mut call_args = vec![];
        for arg in args {
            let value = self.generate_expression(arg)?;
            // An enum read out of a collection arrives boxed, but a function
            // taking one by value expects the inline struct (issue #363).
            let value = match self.resolve_expression_type_with_fallback(arg) {
                Ok(ty) => self.coerce_boxed_enum_to_inline(value, &ty)?,
                Err(_) => value,
            };
            call_args.push(value.into());
        }
        Ok(call_args)
    }

    /// Normalize one arm of a ternary that yields an inline enum value so the
    /// whole expression is uniformly owned. A borrowed arm (an identifier or
    /// field load) is deep-cloned; an owned arm (a constructor/call result)
    /// already owns its payload and is left as-is. Without this a mixed-ownership
    /// ternary - `cond ? Enum.Variant(x) : borrowed` - is classified borrowed
    /// and deep-cloned by the store, orphaning the owned arm's payload and
    /// leaking it (issue #298 review). Once both arms are owned the ternary is
    /// treated as owned (see `rhs_produces_owned_enum`) and transferred.
    fn normalize_enum_ternary_arm(
        &mut self,
        value: BasicValueEnum<'a>,
        arm: &ExpressionNode,
    ) -> Result<BasicValueEnum<'a>, String> {
        if !value.is_struct_value() || Self::rhs_produces_owned_enum(&arm.kind) {
            return Ok(value);
        }
        let Ok(arm_type) = self.resolve_expression_type_with_fallback(arm) else {
            return Ok(value);
        };
        match self.user_enum_type_name(&arm_type) {
            Some(enum_name) if self.enum_has_rc_payload(&enum_name) => {
                // rhs_owned = false -> deep-clone this borrowed arm.
                self.materialize_owned_enum(value, &enum_name, false)
            }
            _ => Ok(value),
        }
    }

    fn generate_if_expression(
        &mut self,
        cond: &ExpressionNode,
        then_expr: &ExpressionNode,
        else_expr: &ExpressionNode,
    ) -> Result<BasicValueEnum<'a>, String> {
        let cond_val = self.generate_expression(cond)?;

        // get current function
        let current_bb = self
            .builder
            .get_insert_block()
            .expect("builder should have an insert block during if generation");
        let function = current_bb
            .get_parent()
            .expect("basic block should have a parent function");

        // create blocks
        let then_bb = self.context.append_basic_block(function, "if_then");
        let else_bb = self.context.append_basic_block(function, "if_else");
        let merge_bb = self.context.append_basic_block(function, "if_merge");

        // conditional branch
        self.builder
            .build_conditional_branch(cond_val.into_int_value(), then_bb, else_bb)
            .map_err(|e| e.to_string())?;

        // then block
        self.builder.position_at_end(then_bb);
        let then_val = self.generate_expression(then_expr)?;
        let then_val = self.normalize_enum_ternary_arm(then_val, then_expr)?;
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| e.to_string())?;
        let then_bb_end = self
            .builder
            .get_insert_block()
            .expect("builder should have insert block after then block");

        // else block
        self.builder.position_at_end(else_bb);
        let else_val = self.generate_expression(else_expr)?;
        let else_val = self.normalize_enum_ternary_arm(else_val, else_expr)?;
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| e.to_string())?;
        let else_bb_end = self
            .builder
            .get_insert_block()
            .expect("builder should have insert block after else block");

        // merge with phi
        self.builder.position_at_end(merge_bb);
        let phi = self
            .builder
            .build_phi(then_val.get_type(), "if_result")
            .map_err(|e| e.to_string())?;
        phi.add_incoming(&[(&then_val, then_bb_end), (&else_val, else_bb_end)]);
        let result = phi.as_basic_value();

        // If this ternary yields an owned inline enum, its arms were tracked enum
        // temporaries (a constructor) or fresh deep-clones; they merge into the
        // phi, so untrack them and track the phi instead - the phi is the single
        // owned value the rest of the statement sees, released or transferred as
        // one (issue #298 review).
        if result.is_struct_value()
            && let Ok(arm_type) = self.resolve_expression_type_with_fallback(then_expr)
            && let Some(enum_name) = self.user_enum_type_name(&arm_type)
            && self.enum_has_rc_payload(&enum_name)
        {
            self.untrack_enum_temp(then_val);
            self.untrack_enum_temp(else_val);
            self.register_enum_temp(result, &enum_name);
        }
        Ok(result)
    }

    fn generate_lambda_expression(
        &mut self,
        expr: &ExpressionNode,
        params: &[Param],
        return_type: &TypeNode,
        body: &[StatementNode],
        where_clause: Option<&crate::ast::WhereClause>,
    ) -> Result<BasicValueEnum<'a>, String> {
        let old_bb = self.builder.get_insert_block();

        let func_name = format!("lambda_{}", self.lambda_counter);
        self.lambda_counter += 1;

        let captures = self
            .analyzer
            .lambda_captures
            .get(&expr.span)
            .cloned()
            .unwrap_or_default();
        let has_captures = !captures.is_empty();

        let old_function_name = self.current_function_name.take();
        self.current_function_name = Some(func_name.clone());

        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let param_types = self.build_lambda_param_types(params, has_captures, ptr_type)?;

        let resolved_return = self
            .analyzer
            .resolve_type(return_type)
            .map_err(|e| e.to_string())?;
        let return_type_opt: Option<BasicTypeEnum<'a>> = if matches!(resolved_return, Type::Void) {
            None
        } else {
            Some(self.llvm_type_from_mux_type(return_type)?)
        };

        let old_return_type = self.current_function_return_type.take();
        self.current_function_return_type = Some(resolved_return.clone());

        let fn_type = if let Some(rt) = return_type_opt {
            rt.fn_type(&param_types, false)
        } else {
            self.context.void_type().fn_type(&param_types, false)
        };

        let function = self.module.add_function(&func_name, fn_type, None);

        let param_offset = self.setup_lambda_param_names(function, params, has_captures);

        let entry_bb = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry_bb);

        let old_variables = self.variables.clone();
        self.variables.clear();

        let saved_rc_scope_stack = std::mem::take(&mut self.rc_scope_stack);
        // Isolate statement temporaries to this lambda body (see generate_function).
        let saved_temp_values = std::mem::take(&mut self.temp_values);
        let saved_closure_scope_stack = std::mem::take(&mut self.closure_scope_stack);
        let saved_closure_temp_values = std::mem::take(&mut self.closure_temp_values);
        self.push_rc_scope();

        if has_captures {
            self.extract_captures_from_struct(function, &captures, ptr_type)?;
        }

        self.setup_lambda_user_params(function, params, param_offset, ptr_type)?;

        // enforce where-clause preconditions before the body runs
        if let Some(clause) = where_clause {
            self.emit_where_checks(&clause.predicates, "where_lambda")?;
        }

        for stmt in body {
            self.generate_statement(stmt, Some(&function))?;
        }

        self.handle_lambda_void_return(return_type_opt)?;

        self.rc_scope_stack.pop();
        self.closure_scope_stack.pop();
        self.rc_scope_stack = saved_rc_scope_stack;
        self.temp_values = saved_temp_values;
        self.closure_scope_stack = saved_closure_scope_stack;
        self.closure_temp_values = saved_closure_temp_values;

        self.variables = old_variables;
        self.current_function_return_type = old_return_type;
        self.current_function_name = old_function_name;

        if let Some(bb) = old_bb {
            self.builder.position_at_end(bb);
        }

        if has_captures {
            self.create_closure_with_captures(function, &captures, ptr_type)
        } else {
            self.create_closure_without_captures(function, ptr_type)
        }
    }

    fn build_lambda_param_types(
        &self,
        params: &[Param],
        has_captures: bool,
        ptr_type: inkwell::types::PointerType<'a>,
    ) -> Result<Vec<BasicMetadataTypeEnum<'a>>, String> {
        let mut param_types: Vec<BasicMetadataTypeEnum> = Vec::new();

        if has_captures {
            param_types.push(ptr_type.into());
        }

        for param in params {
            let param_type = self.llvm_type_from_mux_type(&param.type_)?;
            param_types.push(param_type.into());
        }

        Ok(param_types)
    }

    fn setup_lambda_param_names(
        &self,
        function: inkwell::values::FunctionValue<'a>,
        params: &[Param],
        has_captures: bool,
    ) -> u32 {
        let param_offset = if has_captures { 1 } else { 0 };
        if has_captures {
            function
                .get_nth_param(0)
                .expect("captures parameter should exist")
                .set_name("captures");
        }
        for (i, param) in params.iter().enumerate() {
            let arg = function
                .get_nth_param((i + param_offset) as u32)
                .expect("function parameter should exist at expected index");
            arg.set_name(&param.name);
        }
        param_offset as u32
    }

    fn extract_captures_from_struct(
        &mut self,
        function: inkwell::values::FunctionValue<'a>,
        captures: &[(String, Type)],
        ptr_type: inkwell::types::PointerType<'a>,
    ) -> Result<(), String> {
        let captures_ptr = function
            .get_nth_param(0)
            .expect("captures parameter should exist")
            .into_pointer_value();

        let capture_struct_type = self
            .context
            .struct_type(&vec![ptr_type.into(); captures.len()], false);

        for (i, (name, var_type)) in captures.iter().enumerate() {
            let field_ptr = self
                .builder
                .build_struct_gep(
                    capture_struct_type,
                    captures_ptr,
                    i as u32,
                    &format!("cap_{}_ptr", name),
                )
                .map_err(|e| e.to_string())?;

            let var_ptr = self
                .builder
                .build_load(ptr_type, field_ptr, &format!("cap_{}", name))
                .map_err(|e| e.to_string())?
                .into_pointer_value();

            self.variables.insert(
                name.clone(),
                (
                    var_ptr,
                    BasicTypeEnum::PointerType(ptr_type),
                    var_type.clone(),
                ),
            );
        }
        Ok(())
    }

    fn setup_lambda_user_params(
        &mut self,
        function: inkwell::values::FunctionValue<'a>,
        params: &[Param],
        param_offset: u32,
        ptr_type: inkwell::types::PointerType<'a>,
    ) -> Result<(), String> {
        for (i, param) in params.iter().enumerate() {
            let arg = function
                .get_nth_param(i as u32 + param_offset)
                .expect("function parameter should exist at expected index");
            let boxed = self.box_value(arg);
            let alloca = self
                .builder
                .build_alloca(ptr_type, &param.name)
                .map_err(|e| e.to_string())?;
            self.builder
                .build_store(alloca, boxed)
                .map_err(|e| e.to_string())?;
            let resolved_type = self
                .analyzer
                .resolve_type(&param.type_)
                .map_err(|e| e.to_string())?;
            self.variables.insert(
                param.name.clone(),
                (
                    alloca,
                    BasicTypeEnum::PointerType(ptr_type),
                    resolved_type.clone(),
                ),
            );
        }
        Ok(())
    }

    fn handle_lambda_void_return(
        &mut self,
        return_type_opt: Option<BasicTypeEnum<'a>>,
    ) -> Result<(), String> {
        if let Some(block) = self.builder.get_insert_block()
            && block.get_terminator().is_none()
        {
            if return_type_opt.is_none() {
                self.generate_all_scopes_cleanup()?;
                self.builder.build_return(None).map_err(|e| e.to_string())?;
            } else {
                // Non-void: the analyzer guarantees every path returns, so an
                // open block here is unreachable join-point fallout (e.g. a
                // tail-position match whose arms return through nested
                // if/else). Terminate it so the module verifies.
                self.builder
                    .build_unreachable()
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    fn create_closure_with_captures(
        &mut self,
        function: inkwell::values::FunctionValue<'a>,
        captures: &[(String, Type)],
        ptr_type: inkwell::types::PointerType<'a>,
    ) -> Result<BasicValueEnum<'a>, String> {
        let capture_struct_type = self
            .context
            .struct_type(&vec![ptr_type.into(); captures.len()], false);

        let struct_size = capture_struct_type
            .size_of()
            .ok_or("Failed to get capture struct size")?;
        let malloc_fn = self.runtime_function("malloc").ok_or("malloc not found")?;
        let capture_mem = self
            .builder
            .build_call(malloc_fn, &[struct_size.into()], "capture_alloc")
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or("malloc didn't return a value")?
            .into_pointer_value();

        for (i, (name, var_type)) in captures.iter().enumerate() {
            let (var_ptr, llvm_type, _) = self
                .variables
                .get(name)
                .or_else(|| self.global_variables.get(name))
                .ok_or_else(|| format!("Captured variable '{}' not found", name))?
                .clone();

            let ptr_size = self
                .context
                .ptr_type(AddressSpace::default())
                .size_of()
                .into();
            let heap_storage = self
                .builder
                .build_call(malloc_fn, &[ptr_size], &format!("cap_{}_heap", name))
                .map_err(|e| e.to_string())?
                .try_as_basic_value()
                .basic()
                .ok_or("malloc didn't return a value")?
                .into_pointer_value();

            let current_value = self
                .builder
                .build_load(ptr_type, var_ptr, &format!("cap_{}_val", name))
                .map_err(|e| e.to_string())?;
            // The closure keeps its own reference to the captured value: a
            // returned closure outlives the function whose scope cleanup would
            // otherwise free the captured binding, leaving a dangling capture.
            self.rc_inc_if_pointer(current_value)?;
            self.builder
                .build_store(heap_storage, current_value)
                .map_err(|e| e.to_string())?;

            self.variables
                .insert(name.clone(), (heap_storage, llvm_type, var_type.clone()));

            let field_ptr = self
                .builder
                .build_struct_gep(
                    capture_struct_type,
                    capture_mem,
                    i as u32,
                    &format!("cap_field_{}", i),
                )
                .map_err(|e| e.to_string())?;

            self.builder
                .build_store(field_ptr, heap_storage)
                .map_err(|e| e.to_string())?;
        }

        let (closure_mem, captures_field, count_field) =
            self.allocate_closure(function, ptr_type)?;
        self.builder
            .build_store(captures_field, capture_mem)
            .map_err(|e| e.to_string())?;
        let count = self
            .context
            .i64_type()
            .const_int(captures.len() as u64, false);
        self.builder
            .build_store(count_field, count)
            .map_err(|e| e.to_string())?;

        // The closure owns one reference to each captured value plus its heap
        // allocations; register it so mux_closure_release runs at statement end
        // unless ownership is transferred to a binding or return value.
        self.register_closure_temp(closure_mem);
        Ok(closure_mem)
    }

    fn create_closure_without_captures(
        &mut self,
        function: inkwell::values::FunctionValue<'a>,
        ptr_type: inkwell::types::PointerType<'a>,
    ) -> Result<BasicValueEnum<'a>, String> {
        let (closure_mem, captures_field, count_field) =
            self.allocate_closure(function, ptr_type)?;
        let null_ptr = ptr_type.const_null();
        self.builder
            .build_store(captures_field, null_ptr)
            .map_err(|e| e.to_string())?;
        self.builder
            .build_store(count_field, self.context.i64_type().const_zero())
            .map_err(|e| e.to_string())?;

        self.register_closure_temp(closure_mem);
        Ok(closure_mem)
    }

    /// Allocate a closure struct (fn_ptr + captures + capture_count) with a
    /// refcount header. The closure is allocated as
    /// `[RefHeader (i64) | fn_ptr | captures | capture_count (i64)]`.
    /// The `capture_count` lets the runtime teardown (`mux_closure_release`)
    /// walk and free the capture array. Returns the pointer to the closure
    /// struct (after the header), the captures field pointer, and the
    /// capture-count field pointer so the caller can populate them.
    fn allocate_closure(
        &self,
        function: inkwell::values::FunctionValue<'a>,
        ptr_type: inkwell::types::PointerType<'a>,
    ) -> Result<
        (
            BasicValueEnum<'a>,
            inkwell::values::PointerValue<'a>,
            inkwell::values::PointerValue<'a>,
        ),
        String,
    > {
        let closure_struct_type = self.context.struct_type(
            &[
                ptr_type.into(),
                ptr_type.into(),
                self.context.i64_type().into(),
            ],
            false,
        );

        // Allocate RefHeader (8-byte u64) + closure struct
        let refcount_header_type = self.context.i64_type();
        let full_struct_type = self.context.struct_type(
            &[refcount_header_type.into(), closure_struct_type.into()],
            false,
        );

        let malloc_fn = self.runtime_function("malloc").ok_or("malloc not found")?;
        let full_size = full_struct_type
            .size_of()
            .ok_or("Failed to get full allocation size")?;

        let alloc_mem = self
            .builder
            .build_call(malloc_fn, &[full_size.into()], "closure_alloc")
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or("malloc didn't return a value")?
            .into_pointer_value();

        // Initialize refcount header to 1
        let header_ptr = self
            .builder
            .build_struct_gep(full_struct_type, alloc_mem, 0, "header_ptr")
            .map_err(|e| e.to_string())?;
        let one = refcount_header_type.const_int(1, false);
        self.builder
            .build_store(header_ptr, one)
            .map_err(|e| e.to_string())?;

        // Get the closure struct pointer (after header)
        let closure_mem = self
            .builder
            .build_struct_gep(full_struct_type, alloc_mem, 1, "closure_struct_ptr")
            .map_err(|e| e.to_string())?;

        let fn_ptr_field = self
            .builder
            .build_struct_gep(closure_struct_type, closure_mem, 0, "closure_fn_ptr")
            .map_err(|e| e.to_string())?;
        self.builder
            .build_store(fn_ptr_field, function.as_global_value().as_pointer_value())
            .map_err(|e| e.to_string())?;

        let captures_field = self
            .builder
            .build_struct_gep(closure_struct_type, closure_mem, 1, "closure_captures")
            .map_err(|e| e.to_string())?;

        let count_field = self
            .builder
            .build_struct_gep(closure_struct_type, closure_mem, 2, "closure_capture_count")
            .map_err(|e| e.to_string())?;

        Ok((closure_mem.into(), captures_field, count_field))
    }

    /// check if a method's parameters or return type reference any of the given type parameters
    pub(super) fn method_uses_type_params(
        method: &FunctionNode,
        type_param_names: &[&str],
    ) -> bool {
        for param in &method.params {
            if Self::type_node_contains_names(&param.type_, type_param_names) {
                return true;
            }
        }
        if Self::type_node_contains_names(&method.return_type, type_param_names) {
            return true;
        }
        false
    }

    /// check if a TypeNode contains any of the given names (for generic type parameters)
    fn type_node_contains_names(type_node: &TypeNode, names: &[&str]) -> bool {
        match &type_node.kind {
            TypeKind::Named(n, args) => {
                if names.contains(&n.as_str()) {
                    return true;
                }
                for arg in args {
                    if Self::type_node_contains_names(arg, names) {
                        return true;
                    }
                }
                false
            }
            TypeKind::List(inner) => Self::type_node_contains_names(inner, names),
            TypeKind::Map(k, v) => {
                Self::type_node_contains_names(k, names) || Self::type_node_contains_names(v, names)
            }
            TypeKind::Set(inner) => Self::type_node_contains_names(inner, names),
            _ => false,
        }
    }

    fn generate_none_expression(&mut self) -> Result<BasicValueEnum<'a>, String> {
        let none_call = self
            .builder
            .build_call(
                self.runtime_function("mux_optional_none")
                    .expect("mux_optional_none must be declared in runtime"),
                &[],
                "none_literal",
            )
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .expect("mux_optional_none should return a basic value");
        // mux_optional_none returns an owned (+1) optional value; register it so
        // it is released at the end of the statement unless ownership is
        // transferred (e.g. to a binding or return value) first.
        self.register_temp(none_call);
        Ok(none_call)
    }

    fn generate_non_assignment_binary_expression(
        &mut self,
        left: &ExpressionNode,
        op: &BinaryOp,
        right: &ExpressionNode,
    ) -> Result<BasicValueEnum<'a>, String> {
        if matches!(op, BinaryOp::LogicalAnd | BinaryOp::LogicalOr) {
            return self.generate_short_circuit_logical_op(left, op, right);
        }

        let left_val = self.generate_expression(left)?;
        let right_val = self.generate_expression(right)?;
        self.generate_binary_op(left, left_val, op, right, right_val)
    }

    fn generate_list_literal_expression(
        &mut self,
        elements: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        let list_ptr = self
            .generate_runtime_call("mux_new_list", &[])
            .expect("mux_new_list should always return a value")
            .into_pointer_value();
        for element in elements {
            let elem_val = self.generate_expression(element)?;
            let elem_type = self.resolve_expression_type_with_fallback(element)?;
            let elem_ptr = self.box_enum_or_value(elem_val, &elem_type)?;
            self.generate_runtime_call("mux_list_push_back", &[list_ptr.into(), elem_ptr.into()]);
        }
        let list_value = self
            .generate_runtime_call("mux_list_value", &[list_ptr.into()])
            .expect("mux_list_value should always return a value");
        Ok(list_value)
    }

    fn generate_map_literal_expression(
        &mut self,
        entries: &[(ExpressionNode, ExpressionNode)],
    ) -> Result<BasicValueEnum<'a>, String> {
        let map_ptr = self
            .generate_runtime_call("mux_new_map", &[])
            .expect("mux_new_map should always return a value")
            .into_pointer_value();
        for (key, value) in entries {
            let key_val = self.generate_expression(key)?;
            let key_type = self.resolve_expression_type_with_fallback(key)?;
            let key_ptr = self.box_enum_or_value(key_val, &key_type)?;
            let value_val = self.generate_expression(value)?;
            let value_type = self.resolve_expression_type_with_fallback(value)?;
            let value_ptr = self.box_enum_or_value(value_val, &value_type)?;
            self.generate_runtime_call(
                "mux_map_put",
                &[map_ptr.into(), key_ptr.into(), value_ptr.into()],
            );
        }
        let map_value = self
            .generate_runtime_call("mux_map_value", &[map_ptr.into()])
            .expect("mux_map_value should always return a value");
        Ok(map_value)
    }

    fn generate_set_literal_expression(
        &mut self,
        expr: &ExpressionNode,
        elements: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        if elements.is_empty() {
            let resolved_type = self.resolve_expression_type_with_fallback(expr)?;
            match resolved_type {
                Type::Map(_, _) | Type::EmptyMap => {
                    let map_ptr = self
                        .generate_runtime_call("mux_new_map", &[])
                        .expect("mux_new_map should always return a value")
                        .into_pointer_value();
                    let map_value = self
                        .generate_runtime_call("mux_map_value", &[map_ptr.into()])
                        .expect("mux_map_value should always return a value");
                    return Ok(map_value);
                }
                _ => {
                    let set_ptr = self
                        .generate_runtime_call("mux_new_set", &[])
                        .expect("mux_new_set should always return a value")
                        .into_pointer_value();
                    let set_value = self
                        .generate_runtime_call("mux_set_value", &[set_ptr.into()])
                        .expect("mux_set_value should always return a value");
                    return Ok(set_value);
                }
            }
        }
        let set_ptr = self
            .generate_runtime_call("mux_new_set", &[])
            .expect("mux_new_set should always return a value")
            .into_pointer_value();
        for element in elements {
            let elem_val = self.generate_expression(element)?;
            let elem_type = self.resolve_expression_type_with_fallback(element)?;
            let elem_ptr = self.box_enum_or_value(elem_val, &elem_type)?;
            self.generate_runtime_call("mux_set_add", &[set_ptr.into(), elem_ptr.into()]);
        }
        let set_value = self
            .generate_runtime_call("mux_set_value", &[set_ptr.into()])
            .expect("mux_set_value should always return a value");
        Ok(set_value)
    }

    fn generate_tuple_literal_expression(
        &mut self,
        elements: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        if elements.len() != 2 {
            return Err(format!(
                "Tuple must have exactly 2 elements, got {}",
                elements.len()
            ));
        }
        let left_val = self.generate_expression(&elements[0])?;
        let right_val = self.generate_expression(&elements[1])?;
        let left_ptr = self.box_value(left_val);
        let right_ptr = self.box_value(right_val);
        let tuple_value = self
            .generate_runtime_call("mux_new_tuple", &[left_ptr.into(), right_ptr.into()])
            .expect("mux_new_tuple should always return a value");
        let wrapped_value = self
            .generate_runtime_call("mux_tuple_value", &[tuple_value.into()])
            .expect("mux_tuple_value should always return a value");
        // mux_tuple_value returns an owned (+1) tuple Value; register it so it is
        // released at statement end unless a binding transfers ownership.
        self.register_temp(wrapped_value);
        Ok(wrapped_value)
    }

    fn generate_unary_expression(
        &mut self,
        op: &UnaryOp,
        expr: &ExpressionNode,
    ) -> Result<BasicValueEnum<'a>, String> {
        match op {
            UnaryOp::Ref => self.generate_ref_unary_expression(expr),
            UnaryOp::Deref => self.generate_deref_unary_expression(expr),
            UnaryOp::Incr => self.generate_update_unary_expression(expr, true, true),
            UnaryOp::Decr => self.generate_update_unary_expression(expr, false, true),
            UnaryOp::Not => self.generate_not_unary_expression(expr),
            UnaryOp::Neg => self.generate_neg_unary_expression(expr),
        }
    }

    fn generate_ref_unary_expression(
        &mut self,
        expr: &ExpressionNode,
    ) -> Result<BasicValueEnum<'a>, String> {
        if let ExpressionKind::Identifier(name) = &expr.kind {
            if let Some((ptr, _, _)) = self
                .variables
                .get(name)
                .or_else(|| self.global_variables.get(name))
            {
                return Ok((*ptr).into());
            }
            return Err(format!("Undefined variable {}", name));
        }

        let expr_val = self.generate_expression(expr)?;
        let boxed_val = if expr_val.is_pointer_value() {
            expr_val.into_pointer_value()
        } else {
            self.box_value(expr_val)
        };
        // Taking a reference to a temporary value (e.g. `&list[0]`) keeps that
        // value alive through the reference, so it must not be freed at the
        // statement boundary or the reference would dangle. `owned` is true when
        // this reference now solely owns a freshly produced value (a boxed scalar
        // or a transferred owned temporary) rather than borrowing an existing
        // binding's value.
        let owned = self.untrack_temp(boxed_val.into());
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        // Null-initialized entry-block slot so scope cleanup of the owned target
        // is dominance- and null-safe (the reference may be produced inside
        // conditional control flow).
        let temp = self.create_entry_alloca(ptr_type.into(), "ref_temp")?;
        self.builder
            .build_store(temp, boxed_val)
            .map_err(|e| e.to_string())?;
        // The temporary that this reference owns is released when the enclosing
        // scope ends (after all uses of the reference). Borrowed targets are not
        // owned here and are freed by whatever binding owns them.
        if owned {
            self.track_rc_variable("ref_temp", temp);
        }
        Ok(temp.into())
    }

    fn generate_deref_unary_expression(
        &mut self,
        expr: &ExpressionNode,
    ) -> Result<BasicValueEnum<'a>, String> {
        let ref_val = self.generate_expression(expr)?;
        let boxed_ptr = self
            .builder
            .build_load(
                self.context.ptr_type(AddressSpace::default()),
                ref_val.into_pointer_value(),
                "boxed_ptr",
            )
            .map_err(|e| e.to_string())?;

        if let ExpressionKind::Identifier(name) = &expr.kind
            && let Some((_, _, Type::Reference(inner_type))) = self
                .variables
                .get(name)
                .or_else(|| self.global_variables.get(name))
        {
            return match inner_type.as_ref() {
                Type::Primitive(PrimitiveType::Int) => {
                    self.get_raw_int_value(boxed_ptr).map(Into::into)
                }
                Type::Primitive(PrimitiveType::Float) => {
                    self.get_raw_float_value(boxed_ptr).map(Into::into)
                }
                Type::Primitive(PrimitiveType::Bool) => {
                    self.get_raw_bool_value(boxed_ptr).map(Into::into)
                }
                _ => Ok(boxed_ptr),
            };
        }

        Ok(boxed_ptr)
    }

    fn generate_update_unary_expression(
        &mut self,
        expr: &ExpressionNode,
        increment: bool,
        allow_global: bool,
    ) -> Result<BasicValueEnum<'a>, String> {
        if let ExpressionKind::FieldAccess { expr: obj, field } = &expr.kind {
            return self.generate_field_update_unary_expression(obj, field, increment);
        }

        let ExpressionKind::Identifier(name) = &expr.kind else {
            return Err(format!(
                "Cannot {} on non-identifier",
                if increment { "increment" } else { "decrement" }
            ));
        };

        let ptr = if allow_global {
            self.variables
                .get(name)
                .or_else(|| self.global_variables.get(name))
                .map(|(p, _, _)| *p)
        } else {
            self.variables.get(name).map(|(p, _, _)| *p)
        }
        .ok_or_else(|| format!("Undefined variable {}", name))?;

        let value_ptr = self
            .builder
            .build_load(
                self.context.ptr_type(AddressSpace::default()),
                ptr,
                &format!("{}_load", name),
            )
            .map_err(|e| e.to_string())?
            .into_pointer_value();
        let get_int_func = self
            .runtime_function("mux_value_get_int")
            .ok_or("mux_value_get_int not found")?;
        let current_val = self
            .builder
            .build_call(
                get_int_func,
                &[value_ptr.into()],
                &format!("{}_get_int", name),
            )
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .expect("mux_value_get_int should return a basic value")
            .into_int_value();

        let one = self.context.i64_type().const_int(1, false);
        let new_val = if increment {
            self.builder
                .build_int_add(current_val, one, "incr_result")
                .map_err(|e| e.to_string())?
        } else {
            self.builder
                .build_int_sub(current_val, one, "decr_result")
                .map_err(|e| e.to_string())?
        };
        // The incremented value is freshly boxed; releasing the previous boxed
        // integer avoids leaking it on every `++`/`--`.
        let int_type = Type::Primitive(PrimitiveType::Int);
        self.overwrite_slot_with_owned(ptr, new_val.into(), &int_type)?;
        Ok(new_val.into())
    }

    /// `obj.field++` / `obj.field--`. Reads the current int field value, adjusts
    /// it by one, and writes it back through the same field-store path as a plain
    /// `obj.field = ...` assignment (so reference counting and where-invariants are
    /// handled). Semantics already guarantees the field is int-typed.
    fn generate_field_update_unary_expression(
        &mut self,
        obj: &ExpressionNode,
        field: &str,
        increment: bool,
    ) -> Result<BasicValueEnum<'a>, String> {
        let current = self.generate_field_access_expression(obj, field)?;
        let current_val = self.get_raw_int_value(current)?;
        let one = self.context.i64_type().const_int(1, false);
        let new_val = if increment {
            self.builder
                .build_int_add(current_val, one, "field_incr_result")
        } else {
            self.builder
                .build_int_sub(current_val, one, "field_decr_result")
        }
        .map_err(|e| e.to_string())?;
        // The incremented value is a fresh computed result (owned).
        self.assign_to_field_access(obj, field, new_val.into(), true)?;
        Ok(new_val.into())
    }

    fn generate_not_unary_expression(
        &mut self,
        expr: &ExpressionNode,
    ) -> Result<BasicValueEnum<'a>, String> {
        let expr_val = self.generate_expression(expr)?;
        let bool_val = self.get_raw_bool_value(expr_val)?;
        let not_result = self
            .builder
            .build_not(bool_val, "not")
            .map_err(|e| e.to_string())?;
        Ok(not_result.into())
    }

    fn generate_neg_unary_expression(
        &mut self,
        expr: &ExpressionNode,
    ) -> Result<BasicValueEnum<'a>, String> {
        let expr_val = self.generate_expression(expr)?;
        if expr_val.is_int_value() {
            let int_val = expr_val.into_int_value();
            let neg = self
                .builder
                .build_int_neg(int_val, "neg")
                .map_err(|e| e.to_string())?;
            return Ok(neg.into());
        }
        if expr_val.is_float_value() {
            let float_val = expr_val.into_float_value();
            let neg = self
                .builder
                .build_float_neg(float_val, "neg")
                .map_err(|e| e.to_string())?;
            return Ok(neg.into());
        }
        if let Ok(int_val) = self.get_raw_int_value(expr_val) {
            let neg = self
                .builder
                .build_int_neg(int_val, "neg")
                .map_err(|e| e.to_string())?;
            return Ok(neg.into());
        }
        if let Ok(float_val) = self.get_raw_float_value(expr_val) {
            let neg = self
                .builder
                .build_float_neg(float_val, "neg")
                .map_err(|e| e.to_string())?;
            return Ok(neg.into());
        }
        Err("Negation only works on int or float values".to_string())
    }

    fn ok_builtin_constructor_name(arg_type: &Type) -> Option<&'static str> {
        boxed_value_constructor_name(arg_type, "mux_result_ok")
    }

    fn some_builtin_constructor_name(arg_type: &Type) -> Option<&'static str> {
        boxed_value_constructor_name(arg_type, "mux_optional_some")
    }

    fn call_single_arg_builtin(
        &mut self,
        func_name: &str,
        arg_val: BasicValueEnum<'a>,
        call_name: &str,
    ) -> Result<BasicValueEnum<'a>, String> {
        let func = self
            .runtime_function(func_name)
            .ok_or(format!("{} not found", func_name))?;
        let call = self
            .builder
            .build_call(func, &[arg_val.into()], call_name)
            .map_err(|e| e.to_string())?;
        let result = call
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| format!("{} should return a basic value", func_name))?;
        // some(...)/ok(...)/err(...) return an owned (+1) optional/result value;
        // register it so it is released at statement end unless transferred.
        self.register_temp(result);
        Ok(result)
    }

    /// Box a payload that a `_value` wrapper constructor takes as a
    /// `*mut Value`.
    ///
    /// `optional` and `result` are heap values, so `some(x)` on anything the
    /// runtime stores generically wants a boxed argument. An enum is stored
    /// inline, so `some(Color.Red)` was handing an inline struct to a function
    /// expecting a pointer. `box_enum_or_value` is used rather than `box_value`
    /// because an enum with a reference-counted payload has to become a managed
    /// `BoxedEnum` for its clone and drop glue to run (issue #309).
    fn box_wrapper_payload(
        &mut self,
        constructor: &str,
        value: BasicValueEnum<'a>,
        value_type: &Type,
    ) -> Result<BasicValueEnum<'a>, String> {
        if !constructor.ends_with("_value") || value.is_pointer_value() {
            return Ok(value);
        }
        Ok(self.box_enum_or_value(value, value_type)?.into())
    }

    fn generate_ok_builtin_call(
        &mut self,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        if args.len() != 1 {
            return Err("Ok takes 1 argument".to_string());
        }
        let arg_expr = &args[0];
        let arg_type = self
            .get_resolved_expression_type(arg_expr)
            .map_err(|e| format!("Type inference failed: {}", e))?;
        let func_name = Self::ok_builtin_constructor_name(&arg_type)
            .ok_or_else(|| format!("Ok() not supported for type {:?}", arg_type))?;
        let arg_val = self.generate_expression(arg_expr)?;
        let arg_val = self.box_wrapper_payload(func_name, arg_val, &arg_type)?;
        self.call_single_arg_builtin(func_name, arg_val, "ok_call")
    }

    fn generate_some_builtin_call(
        &mut self,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        if args.len() != 1 {
            return Err("Some takes 1 argument".to_string());
        }
        let arg_expr = &args[0];
        let arg_type = match self.get_resolved_expression_type(arg_expr) {
            Ok(ty) => ty,
            Err(_) => {
                if let ExpressionKind::Identifier(name) = &arg_expr.kind
                    && let Some((_, _, ty)) = self
                        .variables
                        .get(name)
                        .or_else(|| self.global_variables.get(name))
                {
                    ty.clone()
                } else {
                    return Err("Type inference failed for Some() argument".to_string());
                }
            }
        };
        let func_name = Self::some_builtin_constructor_name(&arg_type)
            .ok_or_else(|| format!("Some() not supported for type {:?}", arg_type))?;
        let arg_val = self.generate_expression(arg_expr)?;
        let arg_val = self.box_wrapper_payload(func_name, arg_val, &arg_type)?;
        self.call_single_arg_builtin(func_name, arg_val, "some_call")
    }

    fn generate_none_builtin_call(
        &mut self,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        if !args.is_empty() {
            return Err("None takes 0 arguments".to_string());
        }
        let func = self
            .runtime_function("mux_optional_none")
            .ok_or("mux_optional_none not found")?;
        let call = self
            .builder
            .build_call(func, &[], "none_call")
            .map_err(|e| e.to_string())?;
        let result = call
            .try_as_basic_value()
            .basic()
            .ok_or("optional constructor should return a basic value".to_string())?;
        // Owned (+1) optional value; register for statement-end release unless
        // ownership is transferred first.
        self.register_temp(result);
        Ok(result)
    }

    fn generate_err_builtin_call(
        &mut self,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        if args.len() != 1 {
            return Err("Err takes 1 argument".to_string());
        }
        let arg_val = self.generate_expression(&args[0])?;
        let boxed_arg = self.box_value(arg_val);
        self.call_single_arg_builtin("mux_result_err_value", boxed_arg.into(), "err_call")
    }

    fn call_result_or_default_i32(
        &self,
        call: inkwell::values::CallSiteValue<'a>,
    ) -> BasicValueEnum<'a> {
        match call.try_as_basic_value().basic() {
            Some(val) => val,
            None => self.context.i32_type().const_int(0, false).into(),
        }
    }

    fn build_args_with_class_copy(
        &mut self,
        args: &[ExpressionNode],
    ) -> Result<Vec<BasicMetadataValueEnum<'a>>, String> {
        let mut call_args = vec![];
        for arg in args {
            let arg_type = self.get_resolved_expression_type(arg)?;

            let needs_copy = if let Type::Named(class_name, _) = &arg_type {
                if let Some(symbol) = self.analyzer.symbol_table().lookup(class_name) {
                    symbol.kind == crate::semantics::SymbolKind::Class
                } else {
                    false
                }
            } else {
                false
            };

            if needs_copy {
                let arg_value = self.generate_expression(arg)?;
                let ptr = arg_value.into_pointer_value();
                // Objects are passed by value: copy the argument so the callee
                // cannot mutate the caller's instance. The copy is owned by this
                // statement (the callee only borrows it), so track it for release
                // at the statement boundary rather than leaking it.
                let copied_ptr = self.copy_object_or_error(ptr)?;
                self.register_temp(copied_ptr.into());
                call_args.push(copied_ptr.into());
            } else {
                let value = self.generate_expression(arg)?;
                // An enum read out of a collection arrives boxed; a function
                // taking one by value expects the inline struct (issue #363).
                let value = self.coerce_boxed_enum_to_inline(value, &arg_type)?;
                call_args.push(value.into());
            }
        }
        Ok(call_args)
    }

    fn append_default_call_args(
        &mut self,
        function_name: &str,
        provided_args_len: usize,
        call_args: &mut Vec<BasicMetadataValueEnum<'a>>,
    ) -> Result<(), String> {
        let default_exprs: Vec<Option<ExpressionNode>> = self
            .function_nodes
            .get(function_name)
            .map(|func_node| {
                let total_params = func_node.params.len();
                if provided_args_len < total_params {
                    func_node.params[provided_args_len..total_params]
                        .iter()
                        .map(|p| p.default_value.clone())
                        .collect()
                } else {
                    Vec::new()
                }
            })
            .unwrap_or_default();

        for default_expr_opt in default_exprs {
            if let Some(default_expr) = default_expr_opt {
                let default_val = self.generate_expression(&default_expr)?;
                call_args.push(default_val.into());
            } else {
                return Err(format!(
                    "Missing argument for parameter in function '{}'",
                    function_name
                ));
            }
        }
        Ok(())
    }

    fn call_closure_with_optional_captures(
        &mut self,
        closure_ptr: PointerValue<'a>,
        params: &[Type],
        returns: &Type,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let closure_struct_type = self
            .context
            .struct_type(&[ptr_type.into(), ptr_type.into()], false);

        let fn_ptr_field = self
            .builder
            .build_struct_gep(closure_struct_type, closure_ptr, 0, "fn_ptr_field")
            .map_err(|e| e.to_string())?;
        let func_ptr = self
            .builder
            .build_load(ptr_type, fn_ptr_field, "fn_ptr")
            .map_err(|e| e.to_string())?
            .into_pointer_value();

        let captures_field = self
            .builder
            .build_struct_gep(closure_struct_type, closure_ptr, 1, "captures_field")
            .map_err(|e| e.to_string())?;
        let captures_ptr = self
            .builder
            .build_load(ptr_type, captures_field, "captures_ptr")
            .map_err(|e| e.to_string())?
            .into_pointer_value();

        let is_null = self
            .builder
            .build_is_null(captures_ptr, "captures_is_null")
            .map_err(|e| e.to_string())?;

        let mut user_args: Vec<BasicMetadataValueEnum> = vec![];
        for arg in args {
            user_args.push(self.generate_expression(arg)?.into());
        }

        let mut param_types_without: Vec<BasicMetadataTypeEnum> = Vec::new();
        for param in params {
            let type_node = self.type_to_type_node(param);
            param_types_without.push(self.llvm_type_from_mux_type(&type_node)?.into());
        }

        let mut param_types_with: Vec<BasicMetadataTypeEnum> = vec![ptr_type.into()];
        param_types_with.extend(param_types_without.clone());

        let fn_type_with = if matches!(returns, Type::Void) {
            self.context.void_type().fn_type(&param_types_with, false)
        } else {
            let return_type_node = self.type_to_type_node(returns);
            let return_type = self.llvm_type_from_mux_type(&return_type_node)?;
            return_type.fn_type(&param_types_with, false)
        };

        let fn_type_without = if matches!(returns, Type::Void) {
            self.context
                .void_type()
                .fn_type(&param_types_without, false)
        } else {
            let return_type_node = self.type_to_type_node(returns);
            let return_type = self.llvm_type_from_mux_type(&return_type_node)?;
            return_type.fn_type(&param_types_without, false)
        };

        let current_fn = self
            .builder
            .get_insert_block()
            .and_then(|b| b.get_parent())
            .ok_or("No current function")?;
        let with_captures_bb = self.context.append_basic_block(current_fn, "with_captures");
        let without_captures_bb = self
            .context
            .append_basic_block(current_fn, "without_captures");
        let merge_bb = self.context.append_basic_block(current_fn, "call_merge");

        self.builder
            .build_conditional_branch(is_null, without_captures_bb, with_captures_bb)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(with_captures_bb);
        let mut args_with_captures: Vec<BasicMetadataValueEnum> = vec![captures_ptr.into()];
        args_with_captures.extend(user_args.clone());
        let call_with = self
            .builder
            .build_indirect_call(
                fn_type_with,
                func_ptr,
                &args_with_captures,
                "call_with_captures",
            )
            .map_err(|e| e.to_string())?;
        let result_with = call_with.try_as_basic_value().basic();
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| e.to_string())?;
        let with_captures_end_bb = self
            .builder
            .get_insert_block()
            .expect("Builder should have an insertion block after positioning and building branch");

        self.builder.position_at_end(without_captures_bb);
        let call_without = self
            .builder
            .build_indirect_call(
                fn_type_without,
                func_ptr,
                &user_args,
                "call_without_captures",
            )
            .map_err(|e| e.to_string())?;
        let result_without = call_without.try_as_basic_value().basic();
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| e.to_string())?;
        let without_captures_end_bb = self
            .builder
            .get_insert_block()
            .expect("Builder should have an insertion block after positioning and building branch");

        self.builder.position_at_end(merge_bb);
        match (result_with, result_without) {
            (Some(r1), Some(r2)) => {
                let phi = self
                    .builder
                    .build_phi(r1.get_type(), "call_result")
                    .map_err(|e| e.to_string())?;
                phi.add_incoming(&[(&r1, with_captures_end_bb), (&r2, without_captures_end_bb)]);
                Ok(phi.as_basic_value())
            }
            _ => Ok(self.context.i32_type().const_int(0, false).into()),
        }
    }
    pub(super) fn generate_expression(
        &mut self,
        expr: &ExpressionNode,
    ) -> Result<BasicValueEnum<'a>, String> {
        let value = self.generate_expression_impl(expr)?;
        // Register freshly produced, owned RC values so unbound temporaries are
        // decremented at the statement boundary. Only expression kinds that
        // return a newly allocated (+1) value are registered; kinds that return
        // a borrowed pointer (an identifier/parameter load, a `None`, a function
        // reference/lambda) must not be, or the shared value would be freed out
        // from under its owner.
        if Self::expr_produces_owned_temp(&expr.kind) {
            // Closures are not RC `Value`s: a closure-returning call already
            // registered its result as a closure temporary (released with
            // mux_closure_release). Registering it as an RC temporary too would
            // free it with mux_rc_dec, corrupting the still-live closure.
            let is_closure = value.is_pointer_value()
                && matches!(
                    self.get_resolved_expression_type(expr),
                    Ok(Type::Function { .. })
                );
            if !is_closure {
                self.register_temp(value);
            }
        }
        Ok(value)
    }

    /// Whether an expression kind yields a freshly allocated, caller-owned RC
    /// value (as opposed to a borrowed pointer into an existing binding). Used
    /// to decide whether the result should be tracked as a statement temporary.
    fn expr_produces_owned_temp(kind: &ExpressionKind) -> bool {
        match kind {
            // String literals allocate a new Value; other literals are unboxed
            // scalars and are filtered out by the pointer check in register_temp.
            ExpressionKind::Literal(_) => true,
            // A non-assignment binary op builds a new value (string/list concat)
            // or an unboxed scalar (arithmetic/comparison, ignored by
            // register_temp). An assignment op instead yields the assigned value,
            // which is already owned by the target slot (and separately tracked),
            // so tracking it here would double-free it.
            ExpressionKind::Binary { op, .. } => !op.is_assignment(),
            // These return a newly allocated, owned value (or an unboxed scalar
            // that register_temp ignores): calls (functions and methods return
            // +1), collection literals, and indexed access (the runtime getters
            // clone the element out).
            ExpressionKind::Call { .. }
            | ExpressionKind::ListAccess { .. }
            | ExpressionKind::ListLiteral(_)
            | ExpressionKind::MapLiteral { .. }
            | ExpressionKind::SetLiteral(_)
            | ExpressionKind::TupleLiteral(_) => true,
            // Everything else is either borrowed or not reference counted, and
            // must never be tracked — freeing a borrowed value (a field load, a
            // dereference, an identifier, a ternary arm that yields one of its
            // borrowed operands) double-frees the owner. A missed owned value
            // only leaks, which is recoverable; a double free corrupts the heap.
            // Notably excluded: Identifier, FieldAccess, Unary (incl. Deref),
            // If, None, Lambda.
            _ => false,
        }
    }

    fn generate_identifier_expression(&mut self, name: &str) -> Result<BasicValueEnum<'a>, String> {
        if let Some((ptr, var_type, type_node)) = self
            .variables
            .get(name)
            .or_else(|| self.global_variables.get(name))
        {
            let ptr_copy = *ptr;
            let var_type_copy = *var_type;
            let type_node_copy = type_node.clone();
            return self.generate_identifier_from_binding(
                name,
                ptr_copy,
                var_type_copy,
                &type_node_copy,
            );
        }

        if self
            .analyzer
            .symbol_table()
            .lookup(name)
            .map(|s| s.kind == crate::semantics::SymbolKind::Enum)
            .unwrap_or(false)
        {
            return Err(format!("Enums cannot be used as values: {}", name));
        }

        if let Some(value) = self.try_generate_method_enum_field_identifier(name)? {
            return Ok(value);
        }

        if let Some(value) = self.try_generate_method_field_identifier(name)? {
            return Ok(value);
        }

        self.generate_identifier_function_reference(name)
    }

    fn load_boxed_ptr_from_alloca(
        &self,
        ptr: PointerValue<'a>,
        name: &str,
    ) -> Result<PointerValue<'a>, String> {
        self.builder
            .build_load(
                self.context.ptr_type(AddressSpace::default()),
                ptr,
                &format!("load_{}", name),
            )
            .map_err(|e| e.to_string())
            .map(|v| v.into_pointer_value())
    }

    fn generate_identifier_from_binding(
        &mut self,
        name: &str,
        ptr: PointerValue<'a>,
        var_type: BasicTypeEnum<'a>,
        type_node: &Type,
    ) -> Result<BasicValueEnum<'a>, String> {
        match type_node {
            Type::Optional(_) | Type::Result(_, _) => {
                self.load_boxed_ptr_from_alloca(ptr, name).map(|v| v.into())
            }
            Type::Named(type_name, _) => {
                self.generate_named_identifier_from_binding(name, ptr, var_type, type_name)
            }
            Type::Primitive(prim) => {
                self.generate_primitive_identifier_from_binding(name, ptr, var_type, prim)
            }
            Type::Function { .. } => self.load_boxed_ptr_from_alloca(ptr, name).map(|v| v.into()),
            _ => self.load_boxed_ptr_from_alloca(ptr, name).map(|v| v.into()),
        }
    }

    fn generate_named_identifier_from_binding(
        &mut self,
        name: &str,
        ptr: PointerValue<'a>,
        var_type: BasicTypeEnum<'a>,
        type_name: &str,
    ) -> Result<BasicValueEnum<'a>, String> {
        if type_name == "optional" || type_name == "result" {
            return self.load_boxed_ptr_from_alloca(ptr, name).map(|v| v.into());
        }

        let is_enum = self
            .analyzer
            .symbol_table()
            .lookup(type_name)
            .map(|s| s.kind == crate::semantics::SymbolKind::Enum)
            .unwrap_or(false);
        if is_enum {
            if let BasicTypeEnum::StructType(st) = var_type {
                return self
                    .builder
                    .build_load(st, ptr, &format!("load_{}", name))
                    .map_err(|e| e.to_string());
            }
            if var_type.is_pointer_type() {
                let boxed_ptr = self.load_boxed_ptr_from_alloca(ptr, name)?;
                let struct_type = self
                    .type_map
                    .get(type_name)
                    .ok_or_else(|| format!("Enum {} not found in type map", type_name))?
                    .into_struct_type();
                let unbox_fn = self
                    .runtime_function("mux_value_unbox_enum")
                    .ok_or("mux_value_unbox_enum not found")?;
                let raw_ptr = self
                    .builder
                    .build_call(unbox_fn, &[boxed_ptr.into()], "unbox_enum")
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("Expected basic value from mux_value_unbox_enum")?
                    .into_pointer_value();
                return self
                    .builder
                    .build_load(struct_type, raw_ptr, &format!("load_{}", name))
                    .map_err(|e| e.to_string());
            }
            return Err(format!("Expected struct type for enum variable {}", name));
        }

        self.load_boxed_ptr_from_alloca(ptr, name).map(|v| v.into())
    }

    fn generate_primitive_identifier_from_binding(
        &mut self,
        name: &str,
        ptr: PointerValue<'a>,
        var_type: BasicTypeEnum<'a>,
        prim: &PrimitiveType,
    ) -> Result<BasicValueEnum<'a>, String> {
        // A local holds a boxed value, but a binding may point straight at a
        // slot that holds the scalar itself - a class field bound by name for a
        // where clause is one. The recorded type says which.
        if !var_type.is_pointer_type() {
            return self
                .builder
                .build_load(var_type, ptr, &format!("load_{}", name))
                .map_err(|e| e.to_string());
        }
        let boxed_ptr = self.load_boxed_ptr_from_alloca(ptr, name)?;
        match prim {
            PrimitiveType::Int => self.get_raw_int_value(boxed_ptr.into()).map(|v| v.into()),
            PrimitiveType::Float => self.get_raw_float_value(boxed_ptr.into()).map(|v| v.into()),
            PrimitiveType::Bool => self.get_raw_bool_value(boxed_ptr.into()).map(|v| v.into()),
            PrimitiveType::Str => Ok(boxed_ptr.into()),
            PrimitiveType::Char => self.get_raw_int_value(boxed_ptr.into()).map(|v| v.into()),
            PrimitiveType::Void | PrimitiveType::Auto => {
                Err(format!("Unsupported primitive type {:?}", prim))
            }
        }
    }

    fn try_generate_method_enum_field_identifier(
        &mut self,
        name: &str,
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let Some(func_name) = self.current_function_name.as_ref() else {
            return Ok(None);
        };
        if !func_name.contains('.') {
            return Ok(None);
        }

        let class_name = func_name
            .split('.')
            .next()
            .ok_or_else(|| format!("Invalid function name format: {}", func_name))?
            .to_string();
        let has_field = self
            .field_map
            .get(&class_name)
            .and_then(|fields| fields.get(name))
            .is_some();
        if !has_field {
            return Ok(None);
        }

        let Some(self_value_ptr) = self.load_self_ptr("self_value")? else {
            return Ok(None);
        };
        let get_ptr_func = self
            .runtime_function("mux_get_object_ptr")
            .ok_or("mux_get_object_ptr not found")?;
        let object_data_ptr = self
            .builder
            .build_call(
                get_ptr_func,
                &[self_value_ptr.into()],
                "get_object_ptr_call",
            )
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or("Invalid return from mux_get_object_ptr")?
            .into_pointer_value();
        let class_type = self
            .type_map
            .get(&class_name)
            .ok_or("Class type not found")?;
        let struct_ptr_typed = self
            .builder
            .build_pointer_cast(
                object_data_ptr,
                self.context.ptr_type(AddressSpace::default()),
                "struct_ptr_typed",
            )
            .map_err(|e| e.to_string())?;
        let field_indices = self
            .field_map
            .get(&class_name)
            .ok_or_else(|| format!("Field map not found for class {}", class_name))?;
        let field_index = field_indices
            .get(name)
            .ok_or_else(|| format!("Field {} not found in class {}", name, class_name))?;
        let field_ptr = self
            .builder
            .build_struct_gep(
                *class_type,
                struct_ptr_typed,
                *field_index as u32,
                "field_ptr",
            )
            .map_err(|e| e.to_string())?;
        let class_fields = self.classes.get(&class_name).ok_or("Class not found")?;
        let field = class_fields
            .iter()
            .find(|f| f.name == name)
            .ok_or("Field not found")?;
        let field_type = self.llvm_type_from_mux_type(&field.type_)?;
        let enum_val = self
            .builder
            .build_load(field_type, field_ptr, "field_enum")
            .map_err(|e| e.to_string())?;
        Ok(Some(enum_val))
    }

    fn try_generate_method_field_identifier(
        &mut self,
        name: &str,
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let Some(func_name) = self.current_function_name.as_ref() else {
            return Ok(None);
        };
        if !func_name.contains('.') {
            return Ok(None);
        }

        let class_name = func_name
            .split('.')
            .next()
            .expect("function name contains '.' so next() should return Some")
            .to_string();
        if !self.classes.contains_key(&class_name) {
            return Ok(None);
        }

        let Some(field_index) = self
            .field_map
            .get(&class_name)
            .and_then(|fields| fields.get(name))
        else {
            return Ok(None);
        };
        let field_index = *field_index;
        let Some(self_value_ptr) = self.load_self_ptr("load_self_for_field_access")? else {
            return Ok(None);
        };
        let get_ptr_func = self
            .runtime_function("mux_get_object_ptr")
            .ok_or("mux_get_object_ptr not found")?;
        let data_ptr = self
            .builder
            .build_call(get_ptr_func, &[self_value_ptr.into()], "get_data_ptr")
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .expect("mux_get_object_ptr should return a basic value")
            .into_pointer_value();
        let struct_type = self
            .type_map
            .get(&class_name)
            .ok_or_else(|| format!("Class {} not found in type map", class_name))?;
        let field_ptr = self
            .builder
            .build_struct_gep(
                *struct_type,
                data_ptr,
                field_index as u32,
                &format!("{}_ptr", name),
            )
            .map_err(|e| e.to_string())?;
        let field_types = self
            .field_types_map
            .get(&class_name)
            .expect("class should be in field_types_map");
        let field_type = field_types[field_index];
        let loaded = self
            .builder
            .build_load(field_type, field_ptr, name)
            .map_err(|e| e.to_string())?;
        Ok(Some(loaded))
    }

    fn generate_identifier_function_reference(
        &self,
        name: &str,
    ) -> Result<BasicValueEnum<'a>, String> {
        if let Some(func) = self.module.get_function(name) {
            return Ok(func.as_global_value().as_pointer_value().into());
        }
        Err(format!("Undefined variable: {}", name))
    }

    fn generate_binary_expression(
        &mut self,
        left: &ExpressionNode,
        op: &BinaryOp,
        right: &ExpressionNode,
    ) -> Result<BasicValueEnum<'a>, String> {
        if !op.is_assignment() {
            return self.generate_non_assignment_binary_expression(left, op, right);
        }

        match op {
            BinaryOp::Assign => self.generate_simple_assignment_expression(left, right),
            _ => self.generate_compound_assignment_expression(left, op, right),
        }
    }

    fn generate_simple_assignment_expression(
        &mut self,
        left: &ExpressionNode,
        right: &ExpressionNode,
    ) -> Result<BasicValueEnum<'a>, String> {
        let right_val = self.generate_expression(right)?;
        let rhs_owned = Self::rhs_produces_owned_enum(&right.kind);
        match &left.kind {
            ExpressionKind::Identifier(name) => {
                self.assign_to_identifier(name, right_val, rhs_owned)
            }
            ExpressionKind::Unary {
                op: UnaryOp::Deref,
                expr: deref_expr,
                ..
            } => self.assign_to_deref(deref_expr, right_val),
            ExpressionKind::FieldAccess { expr, field } => {
                self.assign_to_field_access(expr, field, right_val, rhs_owned)
            }
            ExpressionKind::ListAccess {
                expr: target_expr,
                index,
            } => self.assign_to_list_access(target_expr, index, left, right),
            _ => Err("Assignment to non-identifier/deref/field not implemented".to_string()),
        }
    }

    fn assign_to_identifier(
        &mut self,
        name: &str,
        right_val: BasicValueEnum<'a>,
        rhs_owned: bool,
    ) -> Result<BasicValueEnum<'a>, String> {
        if let Some(result) =
            self.try_assign_identifier_to_method_field(name, right_val, rhs_owned)?
        {
            return Ok(result);
        }

        let Some((ptr, _, type_node)) = self
            .variables
            .get(name)
            .or_else(|| self.global_variables.get(name))
        else {
            return Err(format!("Undefined variable {}", name));
        };
        let ptr_copy = *ptr;
        let type_node_copy = type_node.clone();

        let is_enum = if let Type::Named(type_name, _) = &type_node_copy {
            self.analyzer
                .symbol_table()
                .lookup(type_name)
                .map(|s| s.kind == crate::semantics::SymbolKind::Enum)
                .unwrap_or(false)
        } else {
            false
        };

        if is_enum {
            // Enum values are stored inline (not as owned boxed pointers).
            // Release the slot's previous value's payloads before overwriting so
            // reassignment does not leak them (issue #290), and deep-clone a
            // borrowed copy so it owns an independent value (issue #298). A plain
            // store for the built-in optional/result (not user enums).
            self.store_struct_value(ptr_copy, right_val, &type_node_copy, rhs_owned, true)?;
        } else {
            // Value-semantic overwrite: copy a borrowed value type so `x = y`
            // does not alias, release the previous occupant, then store.
            self.overwrite_slot_with_owned(ptr_copy, right_val, &type_node_copy)?;
        }
        Ok(right_val)
    }

    /// If `class_name.field` is a user enum that owns a pointer payload, return
    /// its enum name. Used to route field assignment through the enum
    /// deep-clone/release path rather than a raw inline store (issue #298).
    fn field_user_enum_name(&self, class_name: &str, field: &str) -> Option<String> {
        let fields = self.classes.get(class_name)?;
        let field_decl = fields.iter().find(|f| f.name == field)?;
        let TypeKind::Named(name, _) = &field_decl.type_.kind else {
            return None;
        };
        (self.enum_variants.contains_key(name) && self.enum_has_rc_payload(name))
            .then(|| name.clone())
    }

    /// Store `value` into a class field slot, releasing the field's previous
    /// occupant. A boxed-pointer field retains the new value then releases the
    /// old pointer; an inline enum field is deep-cloned when it is a borrowed
    /// copy and its previous payload released, via `store_struct_value` (issues
    /// #290/#298). Retain/clone happens before the release so `self.x = self.x`
    /// stays safe. Other inline structs own nothing here and are stored directly.
    /// The LLVM type of `field` when the class stores it inline as a scalar,
    /// or `None` when the slot holds a `*mut Value` or an inline enum struct.
    pub(super) fn class_field_scalar_type(
        &self,
        class_name: &str,
        field: &str,
    ) -> Option<BasicTypeEnum<'a>> {
        let index = *self.field_map.get(class_name)?.get(field)?;
        let field_type = *self.field_types_map.get(class_name)?.get(index)?;
        match field_type {
            BasicTypeEnum::IntType(_) | BasicTypeEnum::FloatType(_) => Some(field_type),
            _ => None,
        }
    }

    /// Narrow a value to a scalar slot, extracting it from its box when the
    /// expression that produced it handed back a `*mut Value`.
    pub(super) fn coerce_to_scalar(
        &mut self,
        value: BasicValueEnum<'a>,
        scalar: BasicTypeEnum<'a>,
    ) -> Result<BasicValueEnum<'a>, String> {
        match scalar {
            BasicTypeEnum::FloatType(_) => Ok(self.get_raw_float_value(value)?.into()),
            BasicTypeEnum::IntType(int_type) if int_type.get_bit_width() == 1 => {
                Ok(self.get_raw_bool_value(value)?.into())
            }
            _ => Ok(self.get_raw_int_value(value)?.into()),
        }
    }

    fn store_class_field(
        &mut self,
        class_name: &str,
        field: &str,
        field_ptr: PointerValue<'a>,
        value: BasicValueEnum<'a>,
        rhs_owned: bool,
    ) -> Result<(), String> {
        if value.is_struct_value() {
            if let Some(enum_name) = self.field_user_enum_name(class_name, field) {
                let field_type = Type::Named(enum_name, Vec::new());
                return self.store_struct_value(field_ptr, value, &field_type, rhs_owned, true);
            }
            return self
                .builder
                .build_store(field_ptr, value)
                .map(|_| ())
                .map_err(|e| e.to_string());
        }
        // A scalar field holds the value itself, not a pointer to it, so the
        // reference counting below has nothing to count and the incoming value
        // has to be narrowed to what the slot holds.
        if let Some(scalar) = self.class_field_scalar_type(class_name, field) {
            let value = self.coerce_to_scalar(value, scalar)?;
            return self
                .builder
                .build_store(field_ptr, value)
                .map(|_| ())
                .map_err(|e| e.to_string());
        }
        self.rc_inc_if_pointer(value)?;
        if value.is_pointer_value() {
            let ptr_type = self.context.ptr_type(AddressSpace::default());
            let old = self
                .builder
                .build_load(ptr_type, field_ptr, "old_field_val")
                .map_err(|e| e.to_string())?;
            let rc_dec = self
                .runtime_function("mux_rc_dec")
                .ok_or("mux_rc_dec not found")?;
            self.builder
                .build_call(rc_dec, &[old.into()], "rc_dec_old_field")
                .map_err(|e| e.to_string())?;
        }
        self.builder
            .build_store(field_ptr, value)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn try_assign_identifier_to_method_field(
        &mut self,
        name: &str,
        right_val: BasicValueEnum<'a>,
        rhs_owned: bool,
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let Some(func_name) = self.current_function_name.as_ref() else {
            return Ok(None);
        };
        let func_name = func_name.clone();
        if !func_name.contains('.') {
            return Ok(None);
        }

        let class_name = func_name
            .split('.')
            .next()
            .expect("function name contains '.' so next() should return Some")
            .to_string();
        let Some(field_index) = self
            .field_map
            .get(class_name.as_str())
            .and_then(|fields| fields.get(name))
        else {
            return Ok(None);
        };
        let field_index = *field_index;
        let Some((self_ptr, _, _)) = self
            .variables
            .get("self")
            .or_else(|| self.global_variables.get("self"))
        else {
            return Ok(None);
        };

        let data_ptr = self.load_object_data_ptr_from_self_alloca(*self_ptr, "get_data_ptr")?;
        let struct_type = self
            .type_map
            .get(class_name.as_str())
            .ok_or_else(|| format!("Class {} not found in type map", class_name))?;
        let field_ptr = self
            .builder
            .build_struct_gep(
                *struct_type,
                data_ptr,
                field_index as u32,
                &format!("{}_ptr", name),
            )
            .map_err(|e| e.to_string())?;
        self.store_class_field(&class_name, name, field_ptr, right_val, rhs_owned)?;
        self.emit_field_assignment_invariants(&class_name, name, data_ptr, "where_inv_self")?;
        Ok(Some(right_val))
    }

    fn assign_to_deref(
        &mut self,
        deref_expr: &ExpressionNode,
        right_val: BasicValueEnum<'a>,
    ) -> Result<BasicValueEnum<'a>, String> {
        let ref_val = self.generate_expression(deref_expr)?;
        let ptr = ref_val.into_pointer_value();
        // Resolve the referenced value's type so the slot's previous occupant can
        // be released under value semantics. References point at the slot, not at
        // the value it currently holds, so releasing the old value is safe. Fall
        // back to a transfer-only store if the type cannot be resolved.
        let target_type = match self.resolve_expression_type_with_fallback(deref_expr) {
            Ok(Type::Reference(inner)) => Some(*inner),
            _ => None,
        };
        if let Some(t) = target_type {
            self.overwrite_slot_with_owned(ptr, right_val, &t)?;
        } else {
            let boxed = self.box_value(right_val);
            self.store_boxed_into_slot(ptr, boxed)?;
        }
        Ok(right_val)
    }

    fn assign_to_field_access(
        &mut self,
        expr: &ExpressionNode,
        field: &str,
        right_val: BasicValueEnum<'a>,
        rhs_owned: bool,
    ) -> Result<BasicValueEnum<'a>, String> {
        let struct_ptr = self.resolve_struct_pointer_for_field_access(expr, "data_ptr_assign")?;
        let class_name = self
            .resolve_expression_class_name(expr)
            .ok_or("Named type expected")?;
        let field_ptr = self.resolve_struct_field_pointer(&class_name, field, struct_ptr)?;
        let value_to_store = self.compute_field_store_value(&class_name, field, right_val)?;

        self.store_class_field(&class_name, field, field_ptr, value_to_store, rhs_owned)?;
        self.emit_field_assignment_invariants(&class_name, field, struct_ptr, "where_inv")?;
        Ok(right_val)
    }

    fn resolve_struct_pointer_for_field_access(
        &mut self,
        expr: &ExpressionNode,
        call_name: &str,
    ) -> Result<PointerValue<'a>, String> {
        let mut struct_ptr = if let ExpressionKind::Identifier(obj_name) = &expr.kind {
            if obj_name == "self" {
                let Some((self_ptr, _, _)) = self
                    .variables
                    .get("self")
                    .or_else(|| self.global_variables.get("self"))
                else {
                    return Err("Self not found in field assignment".to_string());
                };
                self.load_object_data_ptr_from_self_alloca(*self_ptr, "self_data_ptr_assign")?
            } else {
                self.generate_expression(expr)?.into_pointer_value()
            }
        } else {
            self.generate_expression(expr)?.into_pointer_value()
        };

        let is_self = matches!(expr.kind, ExpressionKind::Identifier(ref name) if name == "self");
        if is_self || self.resolve_expression_class_name(expr).is_none() {
            return Ok(struct_ptr);
        }

        let get_ptr_func = self
            .runtime_function("mux_get_object_ptr")
            .ok_or("mux_get_object_ptr not found")?;
        let is_ref = self.is_reference_expression(expr);
        let ptr_to_use = if is_ref {
            self.builder
                .build_load(
                    self.context.ptr_type(AddressSpace::default()),
                    struct_ptr,
                    "load_ref_ptr_assign",
                )
                .map_err(|e| e.to_string())?
                .into_pointer_value()
        } else {
            struct_ptr
        };
        struct_ptr = self
            .builder
            .build_call(get_ptr_func, &[ptr_to_use.into()], call_name)
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .expect("mux_get_object_ptr should return a basic value")
            .into_pointer_value();
        Ok(struct_ptr)
    }

    fn is_reference_expression(&mut self, expr: &ExpressionNode) -> bool {
        if let ExpressionKind::Identifier(obj_name) = &expr.kind {
            return self
                .variables
                .get(obj_name)
                .or_else(|| self.global_variables.get(obj_name))
                .map(|(_, _, t)| matches!(t, Type::Reference(_)))
                .unwrap_or(false);
        }
        self.get_resolved_expression_type(expr)
            .map(|t| matches!(t, Type::Reference(_)))
            .unwrap_or(false)
    }

    fn load_object_data_ptr_from_self_alloca(
        &mut self,
        self_ptr: PointerValue<'a>,
        call_name: &str,
    ) -> Result<PointerValue<'a>, String> {
        let self_value_ptr = self
            .builder
            .build_load(
                self.context.ptr_type(AddressSpace::default()),
                self_ptr,
                "load_self_for_field_assign",
            )
            .map_err(|e| e.to_string())?
            .into_pointer_value();
        let get_ptr_func = self
            .runtime_function("mux_get_object_ptr")
            .ok_or("mux_get_object_ptr not found")?;
        self.builder
            .build_call(get_ptr_func, &[self_value_ptr.into()], call_name)
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| "mux_get_object_ptr should return a basic value".to_string())
            .map(|v| v.into_pointer_value())
    }

    fn resolve_struct_field_pointer(
        &mut self,
        class_name: &str,
        field: &str,
        struct_ptr: PointerValue<'a>,
    ) -> Result<PointerValue<'a>, String> {
        let field_indices = self
            .field_map
            .get(class_name)
            .ok_or("Field map not found")?;
        let index = field_indices
            .get(field)
            .copied()
            .ok_or_else(|| format!("Field {} not found", field))?;
        let struct_type = self
            .type_map
            .get(class_name)
            .ok_or("Class type not found")?;
        if let BasicTypeEnum::StructType(st) = *struct_type {
            return self
                .builder
                .build_struct_gep(st, struct_ptr, index as u32, field)
                .map_err(|e| e.to_string());
        }
        Err("Struct type expected".to_string())
    }

    fn compute_field_store_value(
        &mut self,
        class_name: &str,
        field_name: &str,
        right_val: BasicValueEnum<'a>,
    ) -> Result<BasicValueEnum<'a>, String> {
        let field_info = self
            .classes
            .get(class_name)
            .and_then(|fields| fields.iter().find(|f| f.name == field_name));
        let Some(field) = field_info else {
            return Ok(self.box_value(right_val).into());
        };
        let field_type = field.type_.clone();
        let is_generic_param = field.is_generic_param;

        // A scalar field holds the value itself, so hand the store the raw
        // value. Boxing here and unboxing at the store would allocate and free
        // a `*mut Value` for every assignment to an `int` field.
        if let Some(scalar) = self.scalar_field_type(&field_type) {
            return self.coerce_to_scalar(right_val, scalar);
        }
        if is_generic_param {
            return Ok(self.compute_generic_field_store_value(&field_type, right_val));
        }
        if let TypeNode {
            kind: TypeKind::Named(field_type_name, _),
            ..
        } = &field_type
        {
            let is_enum = self
                .analyzer
                .symbol_table()
                .lookup(field_type_name)
                .map(|s| s.kind == crate::semantics::SymbolKind::Enum)
                .unwrap_or(false);
            if is_enum {
                // The field slot is an inline struct. An enum read out of a
                // collection arrives boxed, and storing the pointer into that
                // slot compiles and then segfaults on the next read (#363).
                let field_semantic_type = Type::Named(field_type_name.clone(), vec![]);
                return self.coerce_boxed_enum_to_inline(right_val, &field_semantic_type);
            }
        }
        Ok(self.box_value(right_val).into())
    }

    fn compute_generic_field_store_value(
        &mut self,
        field_type: &TypeNode,
        right_val: BasicValueEnum<'a>,
    ) -> BasicValueEnum<'a> {
        let TypeNode {
            kind: TypeKind::Named(param_name, _),
            ..
        } = field_type
        else {
            return self.box_value(right_val).into();
        };

        let Some(context) = &self.generic_context else {
            return self.box_value(right_val).into();
        };
        let Some(concrete_type) = context.type_params.get(param_name) else {
            return self.box_value(right_val).into();
        };

        match concrete_type {
            Type::Primitive(_) => self.box_value(right_val).into(),
            Type::Named(_, _) => right_val,
            _ => self.box_value(right_val).into(),
        }
    }

    fn assign_to_list_access(
        &mut self,
        target_expr: &ExpressionNode,
        index: &ExpressionNode,
        left: &ExpressionNode,
        right: &ExpressionNode,
    ) -> Result<BasicValueEnum<'a>, String> {
        if let ExpressionKind::ListAccess { .. } = &target_expr.kind {
            let (base_expr, all_indices) = self.collect_list_access_chain(left);
            if all_indices.len() <= 1 {
                return Err("Unexpected single-level nesting in nested path".to_string());
            }
            let right_val = self.generate_expression(right)?;
            let boxed_value = self.box_value(right_val);
            self.generate_nested_collection_assignment(
                base_expr,
                &all_indices,
                boxed_value.into(),
            )?;
            return Ok(right_val);
        }

        let target_type = self.resolve_expression_type_with_fallback(target_expr)?;
        match target_type {
            crate::semantics::Type::List(_) => {
                let target_val = self.generate_expression(target_expr)?;
                let index_val = self.generate_expression(index)?;
                let right_val = self.generate_expression(right)?;
                let boxed_value = self.box_value(right_val);
                self.builder
                    .build_call(
                        self.runtime_function("mux_list_set_value")
                            .expect("mux_list_set_value must be declared in runtime"),
                        &[target_val.into(), index_val.into(), boxed_value.into()],
                        "list_set_value",
                    )
                    .map_err(|e| e.to_string())?;
                Ok(right_val)
            }
            crate::semantics::Type::Map(key_type, value_type) => {
                let target_val = self.generate_expression(target_expr)?;
                let key_val = self.generate_expression(index)?;
                let right_val = self.generate_expression(right)?;
                // An enum key must be a managed value so its structural
                // comparison matches the map's stored keys (issue #309).
                let boxed_key = self.box_enum_or_value(key_val, &key_type)?;
                let boxed_value = self.box_enum_or_value(right_val, &value_type)?;
                self.builder
                    .build_call(
                        self.runtime_function("mux_map_put_value")
                            .expect("mux_map_put_value must be declared in runtime"),
                        &[target_val.into(), boxed_key.into(), boxed_value.into()],
                        "map_put_value",
                    )
                    .map_err(|e| e.to_string())?;
                Ok(right_val)
            }
            _ => Err(format!(
                "Cannot assign to index on non-list/map type: {:?}",
                target_type
            )),
        }
    }

    /// Compute the scalar result of a compound assignment (`+=` / `-=`) for
    /// numeric operands. `is_add` selects addition vs subtraction.
    /// The operator `x op= y` applies, so the result comes from the same code
    /// that generates `x op y` - including its overflow and divide-by-zero
    /// checks, and string concatenation for `+=`.
    fn compound_base_op(op: &BinaryOp) -> Result<BinaryOp, String> {
        match op {
            BinaryOp::AddAssign => Ok(BinaryOp::Add),
            BinaryOp::SubtractAssign => Ok(BinaryOp::Subtract),
            BinaryOp::MultiplyAssign => Ok(BinaryOp::Multiply),
            BinaryOp::DivideAssign => Ok(BinaryOp::Divide),
            BinaryOp::ModuloAssign => Ok(BinaryOp::Modulo),
            other => Err(format!("{:?} is not a compound assignment", other)),
        }
    }

    fn generate_compound_assignment_expression(
        &mut self,
        left: &ExpressionNode,
        op: &BinaryOp,
        right: &ExpressionNode,
    ) -> Result<BasicValueEnum<'a>, String> {
        let base_op = Self::compound_base_op(op)?;
        let left_val = self.generate_expression(left)?;
        let right_val = self.generate_expression(right)?;
        let result = self.generate_binary_op(left, left_val, &base_op, right, right_val)?;
        // `x op y` reaching here through `generate_expression` would have been
        // registered as a statement temporary; calling the operator directly
        // skips that. Arithmetic hands back an unboxed scalar, which
        // `register_temp` ignores, but a `+=` on a string builds a new value
        // that the store below transfers - and leaks if it was never tracked.
        self.register_temp(result);

        match &left.kind {
            ExpressionKind::Identifier(name) => {
                let Some((ptr, _, ty)) = self
                    .variables
                    .get(name)
                    .or_else(|| self.global_variables.get(name))
                else {
                    return Err(format!("Undefined variable {}", name));
                };
                let ptr_copy = *ptr;
                let ty_copy = ty.clone();
                // `result` is a freshly computed scalar; release the previous
                // boxed value rather than leaking it.
                self.overwrite_slot_with_owned(ptr_copy, result, &ty_copy)?;
                Ok(result)
            }
            ExpressionKind::Unary {
                op: UnaryOp::Deref,
                expr: deref_expr,
                ..
            } => {
                let ref_val = self.generate_expression(deref_expr)?;
                let ptr = ref_val.into_pointer_value();
                let target_type = match self.resolve_expression_type_with_fallback(deref_expr) {
                    Ok(Type::Reference(inner)) => Some(*inner),
                    _ => None,
                };
                if let Some(t) = target_type {
                    self.overwrite_slot_with_owned(ptr, result, &t)?;
                } else {
                    // The referenced type could not be resolved, so the slot
                    // takes the value as-is. `store_boxed_into_slot` untracks it,
                    // transferring ownership, so the statement boundary does not
                    // free what the slot now points at.
                    let boxed = self.box_value(result);
                    self.store_boxed_into_slot(ptr, boxed)?;
                }
                Ok(result)
            }
            ExpressionKind::FieldAccess { expr, field } => {
                // A compound-assignment result is a fresh computed value (owned).
                self.assign_to_field_access(expr, field, result, true)?;
                Ok(result)
            }
            _ => Err("Assignment to non-identifier/deref/field not implemented".to_string()),
        }
    }

    fn generate_call_expression(
        &mut self,
        call_expr: &ExpressionNode,
        func: &ExpressionNode,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        let result = match &func.kind {
            ExpressionKind::FieldAccess { expr, field } => {
                self.generate_method_style_call(call_expr, expr, field, args)
            }
            ExpressionKind::Identifier(name) => self.generate_identifier_function_call(name, args),
            _ => self.generate_callable_expression_call(func, args),
        }?;
        // A function that returns a user enum by value hands back an owned value
        // whose payloads it retained or deep-cloned. Track it as a temporary so
        // an unbound result (e.g. used directly as a call argument) is released
        // at the statement boundary instead of leaking, exactly like a freshly
        // constructed enum. `register_enum_temp` dedups and no-ops on values
        // without an RC payload, so a constructor result (already tracked) or a
        // payload-less enum is unaffected.
        if result.is_struct_value()
            && let Ok(result_type) = self.resolve_expression_type_with_fallback(call_expr)
            && let Some(enum_name) = self.user_enum_type_name(&result_type)
        {
            self.register_enum_temp(result, &enum_name);
        }
        Ok(result)
    }

    fn generate_method_style_call(
        &mut self,
        call_expr: &ExpressionNode,
        expr: &ExpressionNode,
        field: &str,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        if matches!(&expr.kind, ExpressionKind::Identifier(obj_name) if obj_name == "self") {
            return self.generate_method_call_on_self(field, args);
        }
        if let Some(result) =
            self.try_generate_implicit_self_field_method_call(call_expr, expr, field, args)?
        {
            return Ok(result);
        }
        if let Some(result) = self.try_generate_nested_module_method_call(expr, field, args)? {
            return Ok(result);
        }
        if let Some(result) = self.try_generate_identifier_method_target_call(expr, field, args)? {
            return Ok(result);
        }
        if let Some(result) = self.try_generate_generic_type_method_call(expr, field, args)? {
            return Ok(result);
        }
        self.generate_general_method_call(expr, field, args)
    }

    fn try_generate_implicit_self_field_method_call(
        &mut self,
        call_expr: &ExpressionNode,
        expr: &ExpressionNode,
        method: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let ExpressionKind::Identifier(field_name) = &expr.kind else {
            return Ok(None);
        };
        let Some(current_function) = &self.current_function_name else {
            return Ok(None);
        };
        if !current_function.contains('.') {
            return Ok(None);
        }
        let class_name = current_function
            .split('.')
            .next()
            .expect("function name contains '.' so next() should return Some");
        let Some(class_fields) = self.classes.get(class_name) else {
            return Ok(None);
        };
        if !class_fields.iter().any(|f| f.name == *field_name) {
            return Ok(None);
        }

        let field_type = class_fields
            .iter()
            .find(|f| f.name == *field_name)
            .expect("field should exist in class after semantic analysis")
            .type_
            .clone();
        let resolved_field_type = self
            .analyzer
            .resolve_type(&field_type)
            .map_err(|e| format!("Type resolution failed: {}", e))?;
        let self_field_expr = ExpressionNode {
            kind: ExpressionKind::FieldAccess {
                expr: Box::new(ExpressionNode {
                    kind: ExpressionKind::Identifier("self".to_string()),
                    span: expr.span,
                }),
                field: field_name.clone(),
            },
            span: call_expr.span,
        };
        let obj_value = self.generate_expression(&self_field_expr)?;
        self.generate_method_call(obj_value, &resolved_field_type, method, args)
            .map(Some)
    }

    fn try_generate_nested_module_method_call(
        &mut self,
        expr: &ExpressionNode,
        field: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let ExpressionKind::FieldAccess {
            expr: inner_expr,
            field: submodule_name,
        } = &expr.kind
        else {
            return Ok(None);
        };
        if let ExpressionKind::Identifier(module_name) = &inner_expr.kind
            && let Some(symbol) = self.analyzer.symbol_table().lookup(module_name)
            && symbol.kind == crate::semantics::SymbolKind::Import
            && self
                .analyzer
                .imported_symbols()
                .get(module_name)
                .and_then(|module_syms| module_syms.get(submodule_name))
                .is_some()
        {
            let qualified_submodule_name = format!("{}.{}", module_name, submodule_name);
            let function_symbol = self
                .analyzer
                .imported_symbols()
                .get(&qualified_submodule_name)
                .and_then(|submodule_syms| submodule_syms.get(field))
                .or_else(|| {
                    self.analyzer
                        .imported_symbols()
                        .get(submodule_name)
                        .and_then(|submodule_syms| submodule_syms.get(field))
                });
            if let Some(function_symbol) = function_symbol
                && let Some(call_result) = self.call_imported_symbol_function(
                    Some(function_symbol.clone()),
                    field,
                    field,
                    args,
                )?
            {
                return Ok(Some(call_result));
            }
        }

        if let ExpressionKind::Identifier(module_name) = &inner_expr.kind
            && let Some(result) =
                self.try_resolve_module_class_static_call(module_name, submodule_name, field, args)?
        {
            return Ok(Some(result));
        }
        Ok(None)
    }

    fn try_generate_identifier_method_target_call(
        &mut self,
        expr: &ExpressionNode,
        field: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let ExpressionKind::Identifier(name) = &expr.kind else {
            return Ok(None);
        };
        if let Some((_, _, var_type)) = self
            .variables
            .get(name)
            .or_else(|| self.global_variables.get(name))
        {
            let var_type_clone = var_type.clone();
            let obj_value = self.generate_expression(expr)?;
            return self
                .generate_method_call(obj_value, &var_type_clone, field, args)
                .map(Some);
        }
        let Some(symbol) = self.analyzer.symbol_table().lookup(name) else {
            return Ok(None);
        };

        if symbol.kind == crate::semantics::SymbolKind::Function {
            let full_name = format!("{}.{}", name, field);
            if let Some(crate::semantics::stdlib::StdlibItem::Function { llvm_name, .. }) =
                crate::semantics::stdlib::lookup_stdlib_item(&full_name)
            {
                return self
                    .generate_named_function_call(field, &llvm_name, args)
                    .map(Some);
            }
        }

        match symbol.kind {
            crate::semantics::SymbolKind::Import => {
                self.generate_import_symbol_method_call(name, field, args)
            }
            crate::semantics::SymbolKind::Class => {
                self.generate_class_symbol_method_call(name, field, args)
            }
            crate::semantics::SymbolKind::Enum => {
                self.generate_enum_symbol_method_call(name, field, args)
            }
            _ => Ok(None),
        }
    }

    fn generate_import_symbol_method_call(
        &mut self,
        module_name: &str,
        field: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let function_symbol = self
            .analyzer
            .imported_symbols()
            .get(module_name)
            .and_then(|module_syms| module_syms.get(field));
        if let Some(call_result) =
            self.call_imported_symbol_function(function_symbol.cloned(), field, field, args)?
        {
            return Ok(Some(call_result));
        }
        if let Some(generic_func) = self.function_nodes.get(field)
            && !generic_func.type_params.is_empty()
        {
            return self.generate_generic_function_call(field, args).map(Some);
        }
        Err(format!(
            "Function {} not found in module {}",
            field, module_name
        ))
    }

    /// The type arguments a bare `Class.new()` inherits from the instantiation
    /// it is written inside.
    ///
    /// Written outside any instantiation there is nothing to inherit, and no
    /// layout can be chosen. That is an error rather than a fallback to the
    /// erased layout, because such an object cannot have a field read or
    /// written without disagreeing with the layout its readers use.
    fn class_type_args_from_generic_context(
        &self,
        class_name: &str,
        type_params: &[(String, Vec<String>)],
    ) -> Result<Vec<Type>, String> {
        type_params
            .iter()
            .map(|(param, _)| {
                self.generic_context
                    .as_ref()
                    .and_then(|context| context.type_params.get(param))
                    .cloned()
                    .ok_or_else(|| {
                        format!(
                            "Cannot infer type argument '{}' for '{}.new()'. Name it, as in {}<...>.new()",
                            param, class_name, class_name
                        )
                    })
            })
            .collect()
    }

    fn generate_class_symbol_method_call(
        &mut self,
        class_name: &str,
        field: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if let Some(created) = self.try_generate_lock_new_call(class_name, field, args)? {
            return Ok(Some(created));
        }
        let class_symbol = self
            .analyzer
            .symbol_table()
            .lookup(class_name)
            .ok_or_else(|| format!("Class {} not found", class_name))?;
        // `Slot.new()` written inside the class's own generic method means
        // `Slot<T>.new()`, with T bound by the specialization it appears in. It
        // has to build the instantiation: the unspecialized `Slot.new` allocates
        // the shared type-erased layout and registers the erased copy and
        // destructor, while every read of the resulting object goes through the
        // instantiation's layout instead, so the two disagree the moment a field
        // is touched.
        if field == "new" && !class_symbol.type_params.is_empty() {
            let type_args =
                self.class_type_args_from_generic_context(class_name, &class_symbol.type_params)?;
            return self
                .generate_constructor_call_with_types(class_name, &type_args, args)
                .map(Some);
        }
        let Some(method) = class_symbol.methods.get(field) else {
            return Err(format!(
                "Method {} not found on class {}",
                field, class_name
            ));
        };
        if let Some(call) = self.try_generate_net_static_method_call(class_name, field, args)? {
            return Ok(Some(call));
        }
        if !method.is_static {
            return Err(format!(
                "Method {} on class {} is not static",
                field, class_name
            ));
        }
        let call_args = self.build_call_args_from_expressions(args)?;
        let call = self
            .builder
            .build_call(
                self.module
                    .get_function(&format!("{}.{}", class_name, field))
                    .expect("static method should be in module"),
                &call_args,
                &format!("{}.{}_call", class_name, field),
            )
            .map_err(|e| e.to_string())?;
        Ok(Some(
            call.try_as_basic_value()
                .basic()
                .expect("static method call should return a basic value"),
        ))
    }

    fn try_generate_lock_new_call(
        &mut self,
        class_name: &str,
        field: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if !matches!(class_name, "Mutex" | "RwLock" | "CondVar") || field != "new" {
            return Ok(None);
        }
        if !args.is_empty() {
            return Err(format!("{}.new() takes no arguments", class_name));
        }
        let runtime_fn = match class_name {
            "Mutex" => "mux_mutex_new",
            "RwLock" => "mux_rwlock_new",
            "CondVar" => "mux_condvar_new",
            _ => unreachable!(),
        };
        let created = self
            .builder
            .build_call(
                self.runtime_function(runtime_fn)
                    .ok_or(format!("{} not found", runtime_fn))?,
                &[],
                &format!("{}_new_call", class_name),
            )
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| format!("{} should return a value", runtime_fn))?;
        Ok(Some(created))
    }

    fn generate_enum_symbol_method_call(
        &mut self,
        enum_name: &str,
        variant: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let constructor_name = format!("{}!{}", enum_name, variant);
        let Some(constructor_func) = self.module.get_function(&constructor_name) else {
            return Err(format!(
                "Enum variant {} not found in enum {}",
                variant, enum_name
            ));
        };
        let mut call_args = self.build_call_args_from_expressions(args)?;
        // A payload declared as a type parameter has a pointer slot, since the
        // layout cannot know what T is. A primitive argument therefore has to be
        // boxed to reach it - `Box<int>.Full(5)` passes an i64 into a ptr slot
        // otherwise (issue #359).
        let param_types = constructor_func.get_type().get_param_types();
        for (arg, expected) in call_args.iter_mut().zip(param_types.iter()) {
            if expected.is_pointer_type()
                && let Ok(value) = BasicValueEnum::try_from(*arg)
                && !value.is_pointer_value()
            {
                *arg = self.box_value(value).into();
            }
        }
        let call = self
            .builder
            .build_call(
                constructor_func,
                &call_args,
                &format!("{}_call", constructor_name),
            )
            .map_err(|e| e.to_string())?;
        let value = call
            .try_as_basic_value()
            .basic()
            .expect("constructor call should return a basic value");
        // A freshly constructed enum owns its payload (the constructor retained
        // it). Track it as a temporary so it is released at the statement
        // boundary or on an early-return path unless its ownership is transferred
        // into a variable/field slot first, which untracks it (issue #298
        // review). This covers every consumption site - a discarded statement, a
        // match subject, a call argument, a collection element - uniformly.
        self.register_enum_temp(value, enum_name);
        Ok(Some(value))
    }

    fn try_generate_generic_type_method_call(
        &mut self,
        expr: &ExpressionNode,
        field: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let ExpressionKind::GenericType(class_name, type_args) = &expr.kind else {
            return Ok(None);
        };
        let resolved_class_name = class_name
            .split('.')
            .next_back()
            .unwrap_or(class_name.as_str())
            .to_string();
        let resolved_type_args = type_args
            .iter()
            .map(|arg| self.type_node_to_type(arg))
            .collect::<Vec<_>>();
        if field == "new" {
            let concrete_type_args = resolved_type_args
                .iter()
                .map(|arg| self.resolve_type(arg))
                .collect::<Result<Vec<_>, _>>()?;
            return self
                .generate_constructor_call_with_types(
                    &resolved_class_name,
                    &concrete_type_args,
                    args,
                )
                .map(Some);
        }
        let Some(class_symbol) = self.analyzer.symbol_table().lookup(&resolved_class_name) else {
            return Err(format!(
                "Method {} not found on class {}",
                field, class_name
            ));
        };
        // `Box<int>.Full(5)`. A variant constructor is named `Box!Full`, not
        // `Box.Full`, so the class path below would look for a function that
        // never exists - which is the "Method 'Box.Full' not found" in issue
        // #359.
        if class_symbol.kind == crate::semantics::SymbolKind::Enum {
            let concrete: Vec<Type> = resolved_type_args
                .iter()
                .map(|arg| self.resolve_type(arg))
                .collect::<Result<Vec<_>, _>>()?;
            let instance = self.ensure_enum_instantiated(&resolved_class_name, &concrete)?;
            return self.generate_enum_symbol_method_call(&instance, field, args);
        }
        let Some(method) = class_symbol.methods.get(field) else {
            return Err(format!(
                "Method {} not found on class {}",
                field, class_name
            ));
        };
        if !method.is_static {
            return Err(format!(
                "Method {} on class {} is not static",
                field, class_name
            ));
        }
        self.generate_generic_static_method_call(
            &resolved_class_name,
            &resolved_type_args,
            field,
            args,
        )
        .map(Some)
    }

    fn generate_generic_static_method_call(
        &mut self,
        resolved_class_name: &str,
        resolved_type_args: &[Type],
        field: &str,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        // The arguments belong to the CALLER, so they are generated before the
        // callee's context is installed. `Pair<T, U>.swap` calling
        // `Pair<U, T>.from(self.second, self.first)` needs `self.second` typed
        // by the caller's map; resolving it under the callee's swapped map read
        // a `string` field as an `int` and emitted a call whose arguments did
        // not match the signature.
        let call_args = self.build_call_args_from_expressions(args)?;

        let context = GenericContext {
            type_params: self.build_type_param_map(resolved_class_name, resolved_type_args)?,
        };
        let old_context = self.generic_context.take();
        self.generic_context = Some(context);
        let call_result = (|| {
            let saved_variables = self.variables.clone();
            let saved_insert_block = self.builder.get_insert_block();
            if !resolved_type_args.is_empty() {
                self.generate_specialized_methods(resolved_class_name, resolved_type_args)?;
            }
            self.variables = saved_variables;
            if let Some(block) = saved_insert_block {
                self.builder.position_at_end(block);
            }
            // Use the resolved (module-prefix-stripped) class name here: that's the
            // name `generate_specialized_methods` actually registered the specialized
            // method under. Using the original, possibly module-qualified `class_name`
            // (e.g. "box.Box" rather than "Box") would look up a name that was never
            // declared, silently falling through to the unspecialized method instead.
            let specialized_method_name =
                self.create_specialized_method_name(resolved_class_name, resolved_type_args, field);
            let function_name = if self.module.get_function(&specialized_method_name).is_some() {
                specialized_method_name
            } else {
                format!("{}.{}", resolved_class_name, field)
            };
            let call = self
                .builder
                .build_call(
                    self.module
                        .get_function(&function_name)
                        .ok_or(format!("Method '{}' not found", function_name))?,
                    &call_args,
                    &format!("{}_call", function_name.replace('.', "_")),
                )
                .map_err(|e| e.to_string())?;
            Ok(call
                .try_as_basic_value()
                .basic()
                .expect("generic method call should return a basic value"))
        })();
        self.generic_context = old_context;
        call_result
    }

    fn generate_general_method_call(
        &mut self,
        expr: &ExpressionNode,
        field: &str,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        let obj_value = self.generate_expression(expr)?;
        let obj_type = if let ExpressionKind::Identifier(name) = &expr.kind {
            self.variables
                .get(name)
                .map(|(_, _, t)| t.clone())
                .ok_or_else(|| format!("Unknown variable: {}", name))?
        } else {
            self.resolve_expression_type_with_fallback(expr)
                .map_err(|e| format!("Type inference failed: {}", e))?
        };
        self.generate_method_call(obj_value, &obj_type, field, args)
    }

    fn generate_identifier_function_call(
        &mut self,
        name: &str,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        if let Some(result) = self.try_generate_builtin_call(name, args)? {
            return Ok(result);
        }
        if let Some(result) = self.try_generate_variable_or_direct_function_call(name, args)? {
            return Ok(result);
        }
        if self.should_generate_generic_function_call(name) {
            return self.generate_generic_function_call(name, args);
        }
        let lookup_name = self.resolve_function_lookup_name(name);
        self.generate_named_function_call(name, &lookup_name, args)
    }

    fn try_generate_builtin_call(
        &mut self,
        name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let value = match name {
            "print" => self.generate_print_call(args)?,
            "read_line" => self.generate_read_line_call(args)?,
            "err" => self.generate_err_builtin_call(args)?,
            "ok" => self.generate_ok_builtin_call(args)?,
            "some" => self.generate_some_builtin_call(args)?,
            "none" => self.generate_none_builtin_call(args)?,
            "range" => self.generate_range_builtin_call(args)?,
            _ => return Ok(None),
        };
        Ok(Some(value))
    }

    /// `range(start, end)` as a value: the integers [start, end) as a `list<int>`.
    ///
    /// The for-loop lowers `for i in range(a, b)` to a counter instead, which
    /// allocates nothing - but `range` is an ordinary function everywhere else,
    /// and without this it was only callable in that one position.
    fn generate_range_builtin_call(
        &mut self,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        if args.len() != 2 {
            return Err(format!("range expects 2 arguments, got {}", args.len()));
        }
        let start = self.generate_expression(&args[0])?;
        let start = self.get_raw_int_value(start)?;
        let end = self.generate_expression(&args[1])?;
        let end = self.get_raw_int_value(end)?;
        let list_ptr = self
            .generate_runtime_call("mux_range", &[start.into(), end.into()])
            .ok_or("mux_range returned no value")?;
        self.generate_runtime_call("mux_list_value", &[list_ptr.into()])
            .ok_or_else(|| "mux_list_value returned no value".to_string())
    }

    fn try_generate_variable_or_direct_function_call(
        &mut self,
        name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let Some((ptr, _, var_type)) = self
            .variables
            .get(name)
            .or_else(|| self.global_variables.get(name))
        else {
            return Ok(None);
        };
        let ptr_copy = *ptr;
        let var_type_copy = var_type.clone();
        if let Type::Function {
            params, returns, ..
        } = var_type_copy
        {
            let closure_ptr = self
                .builder
                .build_load(
                    self.context.ptr_type(AddressSpace::default()),
                    ptr_copy,
                    name,
                )
                .map_err(|e| e.to_string())?
                .into_pointer_value();
            return self
                .call_closure_with_optional_captures(closure_ptr, &params, &returns, args)
                .map(Some);
        }
        if let Some(func) = self.module.get_function(name) {
            let mut call_args = self.build_args_with_class_copy(args)?;
            self.append_default_call_args(name, args.len(), &mut call_args)?;
            let call = self
                .builder
                .build_call(func, &call_args, "user_func_call")
                .map_err(|e| e.to_string())?;
            return Ok(Some(self.call_result_or_default_i32(call)));
        }
        Err(format!("Undefined function: {}", name))
    }

    fn should_generate_generic_function_call(&mut self, name: &str) -> bool {
        if let Some(func_node) = self.function_nodes.get(name)
            && !func_node.type_params.is_empty()
        {
            return true;
        }
        if let Some(func_symbol) = self.analyzer.symbol_table().lookup(name)
            && let SymbolKind::Function = func_symbol.kind
            && let Some(func_node) = self.function_nodes.get(name)
        {
            return !func_node.type_params.is_empty()
                && func_node.type_params.iter().any(|(param_name, _)| {
                    param_name.chars().next().unwrap_or(' ').is_uppercase() || param_name.len() > 3
                });
        }
        false
    }

    fn resolve_function_lookup_name(&mut self, name: &str) -> String {
        let mut lookup_name = name.to_string();
        if let Some(nested_name) = self.find_nested_mangled_function_name(name) {
            lookup_name = nested_name;
        } else if name == "main" && self.module.get_function("!user!main").is_some() {
            lookup_name = "!user!main".to_string();
        } else if let Some(import_llvm_name) = self.resolve_imported_llvm_name(name) {
            lookup_name = import_llvm_name;
        }
        lookup_name
    }

    fn find_nested_mangled_function_name(&self, name: &str) -> Option<String> {
        let parent_fn = self.current_function_name.as_ref()?;
        let direct = format!("{}!{}", parent_fn, name);
        if self.module.get_function(&direct).is_some() {
            return Some(direct);
        }
        let parts: Vec<&str> = parent_fn.split('!').collect();
        if parts.len() <= 1 {
            return None;
        }
        for i in 1..parts.len() {
            let prefix = parts[0..i].join("!");
            let nested = format!("{}!{}", prefix, name);
            if self.module.get_function(&nested).is_some() {
                return Some(nested);
            }
        }
        None
    }

    fn resolve_imported_llvm_name(&mut self, name: &str) -> Option<String> {
        let symbol = self.analyzer.symbol_table().lookup(name)?;
        symbol
            .llvm_name
            .as_ref()
            .cloned()
            .or_else(|| Some(name.to_string()))
    }

    fn generate_named_function_call(
        &mut self,
        display_name: &str,
        lookup_name: &str,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        let Some(func) = self.runtime_function(lookup_name) else {
            return Err(format!(
                "Undefined function: {} (looked for LLVM name: {})",
                display_name, lookup_name
            ));
        };
        let llvm_param_types = func.get_type().get_param_types();
        let mut call_args = Vec::with_capacity(args.len());
        for (idx, arg) in args.iter().enumerate() {
            // Resolve every argument's type through the fallback resolver. This
            // applies to all named calls, not only imported-module ones: the
            // analyzer's scope for a function body may already be popped by the
            // time codegen runs (always so for imported-module bodies), which
            // breaks a non-identifier argument like `n - 1` in a recursive call.
            // The fallback consults codegen's own parameter/variable tables and is
            // otherwise equivalent, so it is the correct default here.
            let arg_type = self.resolve_expression_type_with_fallback(arg)?;

            let needs_copy = if let Type::Named(class_name, _) = &arg_type {
                if let Some(symbol) = self.analyzer.symbol_table().lookup(class_name) {
                    symbol.kind == crate::semantics::SymbolKind::Class
                } else {
                    false
                }
            } else {
                false
            };

            let arg_val = if needs_copy {
                let original_val = self.generate_expression(arg)?;
                let ptr = original_val.into_pointer_value();
                // Pass-by-value copy owned by this statement (see the sibling
                // path in build_call_args_from_expressions); track for release.
                let copied = self.copy_object_or_error(ptr)?;
                self.register_temp(copied.into());
                copied.into()
            } else {
                // An owned inline enum argument was tracked as an enum temporary
                // when produced; the callee borrows its parameter, so statement
                // cleanup releases the argument (issue #298 review).
                let value = self.generate_expression(arg)?;
                // An enum read out of a collection arrives boxed, and the
                // parameter is an inline struct (issue #363).
                self.coerce_boxed_enum_to_inline(value, &arg_type)?
            };

            let coerced = if let Some(expected) = llvm_param_types.get(idx) {
                self.coerce_to_llvm_param_type(arg_val, *expected)?
            } else {
                arg_val
            };
            call_args.push(coerced.into());
        }
        self.append_default_call_args(display_name, args.len(), &mut call_args)?;
        let call = self
            .builder
            .build_call(func, &call_args, "user_func_call")
            .map_err(|e| e.to_string())?;
        Ok(self.call_result_or_default_i32(call))
    }

    fn generate_callable_expression_call(
        &mut self,
        func: &ExpressionNode,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        let closure_ptr = self.generate_expression(func)?.into_pointer_value();
        let func_type = self.get_resolved_expression_type(func)?;
        if let Type::Function {
            params, returns, ..
        } = func_type
        {
            return self.call_closure_with_optional_captures(closure_ptr, &params, &returns, args);
        }
        Err(format!("Cannot call non-function type: {:?}", func_type))
    }

    fn generate_list_access_expression(
        &mut self,
        target_expr: &ExpressionNode,
        index: &ExpressionNode,
    ) -> Result<BasicValueEnum<'a>, String> {
        let target_val = self.generate_expression(target_expr)?;
        let index_val = self.generate_expression(index)?;

        // Get the target type to determine if it's a list or map
        let target_type = self.resolve_expression_type_with_fallback(target_expr)?;

        match &target_type {
            crate::semantics::Type::List(element_type) => {
                // Read the element directly from the list Value. Indexing the
                // live list in O(1) avoids the old `mux_value_get_list` path,
                // which cloned the entire backing vector on every access (O(n)
                // per read, O(n^2) building/iterating a list by index in a loop).
                let list_length = self
                    .builder
                    .build_call(
                        self.runtime_function("mux_value_list_length")
                            .ok_or("mux_value_list_length not found")?,
                        &[target_val.into()],
                        "index_len",
                    )
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("mux_value_list_length should return a basic value")?;

                // Normalize the index to handle negative wraparound.
                let normalized_index =
                    self.normalize_index_with_length(index_val, list_length.into_int_value())?;

                // mux_value_list_get_value returns a direct owned value or null
                // (out of bounds); it borrows the list Value without cloning it.
                let raw_result = self
                    .builder
                    .build_call(
                        self.runtime_function("mux_value_list_get_value")
                            .ok_or("mux_value_list_get_value not found")?,
                        &[target_val.into(), normalized_index.into()],
                        "list_raw",
                    )
                    .map_err(|e| e.to_string())?;

                let result_ptr = raw_result
                    .try_as_basic_value()
                    .basic()
                    .ok_or("mux_value_list_get_value should return a basic value")?
                    .into_pointer_value();

                // Bounds-check the result pointer. Report the user's original
                // index (pre-normalization) so `values[-5]` reads "-5", not the
                // wrapped value.
                self.check_list_bounds(
                    result_ptr,
                    index_val,
                    list_length,
                    Some(index.span()),
                    "index",
                )?;

                // Use extract_value_from_ptr to properly extract based on type
                let (extracted_val, _) =
                    self.extract_value_from_ptr(result_ptr, element_type, "list_element")?;
                // For scalar elements extract_value_from_ptr already released the
                // owned clone; for string/complex elements it returns an owned
                // value, so register it for statement-end release.
                self.register_temp(extracted_val);
                Ok(extracted_val)
            }
            crate::semantics::Type::Map(key_type, value_type) => {
                // Look up the key directly in the map Value. This avoids the old
                // `mux_value_get_map` path, which cloned the entire map on every
                // access (O(n) per read, O(n^2) reading a map by key in a loop).
                let value_ptr = self
                    .map_value_get_or_panic(
                        target_val,
                        index_val,
                        key_type,
                        Some(index.span()),
                        "map",
                    )?
                    .into_pointer_value();

                // Use extract_value_from_ptr to properly extract based on type
                let (extracted_val, _) =
                    self.extract_value_from_ptr(value_ptr, value_type, "map_element")?;
                // Register owned string/complex results for statement-end release
                // (scalars were already released inside extract_value_from_ptr).
                self.register_temp(extracted_val);
                Ok(extracted_val)
            }
            _ => Err(format!(
                "ListAccess target must be a list or map, found {:?}",
                target_type
            )),
        }
    }

    fn generate_field_access_expression(
        &mut self,
        expr: &ExpressionNode,
        field: &str,
    ) -> Result<BasicValueEnum<'a>, String> {
        // `Color.Red` constructs the variant (mux-context#39). Checked first
        // because the paths below evaluate `expr` as a value, and an enum name
        // is not one - which is how this used to surface as the internal error
        // "Enums cannot be used as values: Color".
        if let Some(value) = self.try_generate_enum_variant_field_access(expr, field)? {
            return Ok(value);
        }
        if let Some(value) = self.try_generate_stdlib_constant_field_access(expr, field)? {
            return Ok(value);
        }
        if let Some(value) = self.try_generate_module_constant_field_access(expr, field)? {
            return Ok(value);
        }
        if let Some(value) = self.try_generate_tuple_field_access(expr, field)? {
            return Ok(value);
        }
        if self.is_csv_expression(expr)? {
            return self.generate_csv_field_access(expr, field);
        }
        if let Some(value) = self.try_generate_class_field_access(expr, field)? {
            return Ok(value);
        }
        if let Some(value) = self.try_generate_primitive_field_method_access(expr, field)? {
            return Ok(value);
        }
        Err(format!(
            "Field access not supported for expression type {:?}",
            expr.kind
        ))
    }

    /// Construct a payload-less enum variant written without parentheses.
    ///
    /// `Color.Red` is a value, so it goes through the same generated
    /// constructor as `Shape.Circ(5.0)` does, just with no arguments. Type
    /// checking has already established the variant exists and takes no
    /// payload; anything else here is a mismatch between the two phases.
    fn try_generate_enum_variant_field_access(
        &mut self,
        expr: &ExpressionNode,
        field: &str,
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let Some(enum_name) = self.enum_name_from_type_reference(expr) else {
            return Ok(None);
        };
        // `Tree<int>.Leaf` must build the instantiation's constructor, not the
        // uninstantiated one, or the value carries the wrong layout (issue #359).
        let enum_name = if let ExpressionKind::GenericType(_, type_args) = &expr.kind {
            let concrete = type_args
                .iter()
                .map(|arg| {
                    let ty = self.type_node_to_type(arg);
                    self.resolve_type(&ty)
                })
                .collect::<Result<Vec<_>, _>>()?;
            self.ensure_enum_instantiated(&enum_name, &concrete)?
        } else {
            enum_name
        };
        self.generate_enum_symbol_method_call(&enum_name, field, &[])
    }

    /// The enum named by a type reference: a bare name (`Color`) or one reached
    /// through a module namespace (`palette.Color`). Imported enums are
    /// registered under their bare name, so both forms yield the same key
    /// (issue #368).
    fn enum_name_from_type_reference(&self, expr: &ExpressionNode) -> Option<String> {
        match &expr.kind {
            // `Box<int>.Empty` - the instantiation is carried by the value's
            // type, not by the constructor's name.
            ExpressionKind::GenericType(name, _) => self
                .analyzer
                .symbol_table()
                .lookup(name)
                .filter(|s| s.kind == crate::semantics::SymbolKind::Enum)
                .map(|_| name.clone()),
            ExpressionKind::Identifier(name) => self
                .analyzer
                .symbol_table()
                .lookup(name)
                .filter(|s| s.kind == crate::semantics::SymbolKind::Enum)
                .map(|_| name.clone()),
            ExpressionKind::FieldAccess { expr: base, field } => {
                let ExpressionKind::Identifier(module) = &base.kind else {
                    return None;
                };
                let module_sym = self.analyzer.symbol_table().lookup(module)?;
                if module_sym.kind != crate::semantics::SymbolKind::Import {
                    return None;
                }
                self.analyzer
                    .imported_symbols()
                    .get(module)?
                    .get(field)
                    .filter(|s| s.kind == crate::semantics::SymbolKind::Enum)
                    .map(|_| field.clone())
            }
            _ => None,
        }
    }

    /// If `expr.field` names a constant imported from a module, return the module
    /// name. Shared preamble for the stdlib and user-module constant field-access
    /// paths: `expr` must be an `Identifier` bound to an `Import`, and `field` must
    /// be a `Constant` exported by that module.
    fn imported_constant_module(&self, expr: &ExpressionNode, field: &str) -> Option<String> {
        let ExpressionKind::Identifier(module_name) = &expr.kind else {
            return None;
        };
        let symbol = self.analyzer.symbol_table().lookup(module_name)?;
        if symbol.kind != crate::semantics::SymbolKind::Import {
            return None;
        }
        let field_sym = self
            .analyzer
            .imported_symbols()
            .get(module_name)?
            .get(field)?;
        if field_sym.kind != crate::semantics::SymbolKind::Constant {
            return None;
        }
        Some(module_name.clone())
    }

    fn try_generate_stdlib_constant_field_access(
        &mut self,
        expr: &ExpressionNode,
        field: &str,
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let Some(module_name) = self.imported_constant_module(expr, field) else {
            return Ok(None);
        };

        use crate::semantics::stdlib::{ConstantValue, lookup_stdlib_item};
        let full_name = format!("{}.{}", module_name, field);
        let Some(crate::semantics::stdlib::StdlibItem::Constant { value, .. }) =
            lookup_stdlib_item(&full_name)
        else {
            return Ok(None);
        };
        let generated = match value {
            ConstantValue::Float(f) => self.context.f64_type().const_float(f).into(),
            ConstantValue::Int(i) => self.context.i64_type().const_int(i as u64, false).into(),
            ConstantValue::Bool(b) => self.context.bool_type().const_int(b as u64, false).into(),
        };
        Ok(Some(generated))
    }

    /// `module.CONST` for a user-defined imported module (as opposed to a stdlib
    /// module, handled just above). A module-level `const` lives in that module's
    /// own global table under its bare name, emitted in LLVM as `module!CONST`.
    /// The importing namespace can be an alias (`import shapes.circle as c`), so
    /// the owning module is taken from the symbol's mangled `llvm_name` rather
    /// than from the namespace, and the value is read from that module's table -
    /// never from locals, so a same-named local in the caller cannot shadow it,
    /// and never from another module, so two modules may share a constant name.
    fn try_generate_module_constant_field_access(
        &mut self,
        expr: &ExpressionNode,
        field: &str,
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let Some(module_name) = self.imported_constant_module(expr, field) else {
            return Ok(None);
        };
        // `llvm_name` is `module!field`; the module part identifies the owning
        // module's table even when the import is aliased.
        let owning_module = self
            .analyzer
            .imported_symbols()
            .get(&module_name)
            .and_then(|syms| syms.get(field))
            .and_then(|sym| sym.llvm_name.as_ref())
            .and_then(|mangled| mangled.split('!').next())
            .map(str::to_string);

        // When the owning module is known, its table is the only correct source:
        // falling back to `global_variables` here would read whichever module's
        // globals happen to be swapped in, silently yielding a same-named
        // constant from the *calling* module - exactly the bug this scoping
        // fixes. Only an unmangled symbol (no `llvm_name`, e.g. a main-module
        // constant) legitimately falls back.
        let entry = match owning_module.as_ref() {
            Some(module) => self
                .module_globals
                .get(module)
                .and_then(|globals| globals.get(field))
                .ok_or_else(|| {
                    format!(
                        "internal error: module constant '{}' has no global slot in module '{}'",
                        field, module
                    )
                })?,
            None => self.global_variables.get(field).ok_or_else(|| {
                format!(
                    "internal error: module constant '{}' has no global slot in codegen",
                    field
                )
            })?,
        };
        let (ptr, var_type, type_node) = entry;
        let (ptr_copy, var_type_copy, type_node_copy) = (*ptr, *var_type, type_node.clone());
        self.generate_identifier_from_binding(field, ptr_copy, var_type_copy, &type_node_copy)
            .map(Some)
    }

    fn try_generate_tuple_field_access(
        &mut self,
        expr: &ExpressionNode,
        field: &str,
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let expr_type = self.get_resolved_expression_type(expr)?;
        let Type::Tuple(left_type, right_type) = expr_type else {
            return Ok(None);
        };

        let tuple_value_ptr = self.generate_expression(expr)?.into_pointer_value();
        let get_tuple_fn = self
            .runtime_function("mux_value_get_tuple")
            .ok_or("mux_value_get_tuple not found")?;
        let tuple_ptr = self
            .builder
            .build_call(get_tuple_fn, &[tuple_value_ptr.into()], "get_tuple")
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or("mux_value_get_tuple should return a value")?
            .into_pointer_value();
        let (field_index, field_type) = match field {
            "left" => (0, *left_type),
            "right" => (1, *right_type),
            _ => return Err(format!("Unknown field '{}' for tuple type", field)),
        };
        let get_field_func = self
            .runtime_function(if field_index == 0 {
                "mux_tuple_left"
            } else {
                "mux_tuple_right"
            })
            .ok_or(if field_index == 0 {
                "mux_tuple_left not found"
            } else {
                "mux_tuple_right not found"
            })?;
        let field_value = self
            .builder
            .build_call(get_field_func, &[tuple_ptr.into()], field)
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or("mux_tuple_left/right should return a value")?;
        let unboxed = self.unbox_value_for_type(field_value, &field_type)?;
        // mux_tuple_left/right return an owned (+1) clone of the element. For
        // scalar fields the raw value has been extracted into `unboxed`, so the
        // owned box is now dead and must be released; for string/complex fields
        // the box IS the returned value, so register it as a statement
        // temporary to be released at the end of the statement.
        if field_value.is_pointer_value() {
            match field_type {
                Type::Primitive(PrimitiveType::Int)
                | Type::Primitive(PrimitiveType::Float)
                | Type::Primitive(PrimitiveType::Bool)
                | Type::Primitive(PrimitiveType::Char) => {
                    self.emit_value_decref(field_value.into_pointer_value())?;
                }
                _ => {
                    self.register_temp(field_value);
                }
            }
        }
        Ok(Some(unboxed))
    }

    fn is_csv_expression(&mut self, expr: &ExpressionNode) -> Result<bool, String> {
        let expr_type = self.get_resolved_expression_type(expr)?;
        Ok(matches!(expr_type, Type::Named(name, _) if name == "Csv"))
    }

    fn try_generate_class_field_access(
        &mut self,
        expr: &ExpressionNode,
        field: &str,
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let Some(class_name) = self.resolve_expression_class_name(expr) else {
            return Ok(None);
        };
        let struct_ptr = self.resolve_struct_pointer_for_field_access(expr, "data_ptr")?;
        let field_indices = self
            .field_map
            .get(class_name.as_str())
            .ok_or("Field map not found")?;
        let index = field_indices
            .get(field)
            .copied()
            .ok_or_else(|| format!("Field {} not found", field))?;
        let field_ptr = self.resolve_struct_field_pointer(&class_name, field, struct_ptr)?;
        self.load_class_field_value(expr, &class_name, field, index, field_ptr)
            .map(Some)
    }

    fn load_class_field_value(
        &mut self,
        expr: &ExpressionNode,
        class_name: &str,
        field: &str,
        index: usize,
        field_ptr: PointerValue<'a>,
    ) -> Result<BasicValueEnum<'a>, String> {
        let field_types = self
            .field_types_map
            .get(class_name)
            .ok_or("Field types not found for class")?;
        let field_type = *field_types
            .get(index)
            .ok_or("Field type index out of bounds")?;
        if let BasicTypeEnum::StructType(struct_type) = field_type {
            return self
                .builder
                .build_load(struct_type, field_ptr, field)
                .map_err(|e| e.to_string());
        }
        let field_def = self
            .classes
            .get(class_name)
            .and_then(|fields| fields.iter().find(|f| f.name == field))
            .cloned()
            .ok_or("Field not found")?;
        let resolved_field_type = self
            .analyzer
            .resolve_type(&field_def.type_)
            .map_err(|e| e.to_string())?;
        let loaded = self
            .builder
            .build_load(field_type, field_ptr, field)
            .map_err(|e| e.to_string())?;
        if let Some(unboxed) =
            self.try_unbox_generic_field_value(expr, class_name, &field_def, loaded)?
        {
            return Ok(unboxed);
        }
        if let Some(unboxed) =
            self.try_unbox_named_substituted_field_value(loaded, &resolved_field_type)?
        {
            return Ok(unboxed);
        }
        self.unbox_value_for_type_or_identity(loaded, &resolved_field_type)
    }

    fn try_unbox_generic_field_value(
        &mut self,
        expr: &ExpressionNode,
        class_name: &str,
        field_def: &crate::ast::Field,
        loaded: BasicValueEnum<'a>,
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if !field_def.is_generic_param {
            return Ok(None);
        }
        let TypeNode {
            kind: TypeKind::Named(param_name, _),
            ..
        } = &field_def.type_
        else {
            return Ok(None);
        };
        let Some(concrete_type) =
            self.resolve_generic_field_concrete_type(expr, class_name, param_name.as_str())
        else {
            return Ok(None);
        };
        self.unbox_value_for_type_or_identity(loaded, &concrete_type)
            .map(Some)
    }

    fn resolve_generic_field_concrete_type(
        &self,
        expr: &ExpressionNode,
        class_name: &str,
        param_name: &str,
    ) -> Option<Type> {
        if let Some(context) = &self.generic_context
            && let Some(concrete) = context.type_params.get(param_name)
        {
            return Some(concrete.clone());
        }
        let ExpressionKind::Identifier(obj_name) = &expr.kind else {
            return None;
        };
        let (_, _, obj_type) = self
            .variables
            .get(obj_name)
            .or_else(|| self.global_variables.get(obj_name))?;
        let type_args = match obj_type {
            Type::Named(_, type_args) | Type::Instantiated(_, type_args) => type_args,
            _ => return None,
        };
        if type_args.is_empty() {
            return None;
        }
        let class_symbol = self.analyzer.all_symbols().get(class_name)?;
        let param_index = class_symbol
            .type_params
            .iter()
            .position(|(p, _)| p == param_name)?;
        type_args.get(param_index).cloned()
    }

    fn try_unbox_named_substituted_field_value(
        &mut self,
        loaded: BasicValueEnum<'a>,
        resolved_field_type: &Type,
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let Type::Named(name, _) = resolved_field_type else {
            return Ok(None);
        };
        let Some(context) = &self.generic_context else {
            return Ok(None);
        };
        let Some(concrete_type) = context.type_params.get(name).cloned() else {
            return Ok(None);
        };
        self.unbox_value_for_type_or_identity(loaded, &concrete_type)
            .map(Some)
    }

    fn unbox_value_for_type_or_identity(
        &mut self,
        loaded: BasicValueEnum<'a>,
        ty: &Type,
    ) -> Result<BasicValueEnum<'a>, String> {
        match ty {
            Type::Primitive(PrimitiveType::Int)
            | Type::Primitive(PrimitiveType::Float)
            | Type::Primitive(PrimitiveType::Bool)
            | Type::Primitive(PrimitiveType::Char)
            | Type::Primitive(PrimitiveType::Str) => self.unbox_value_for_type(loaded, ty),
            _ => Ok(loaded),
        }
    }

    fn unbox_value_for_type(
        &mut self,
        value: BasicValueEnum<'a>,
        ty: &Type,
    ) -> Result<BasicValueEnum<'a>, String> {
        match ty {
            Type::Primitive(PrimitiveType::Int) => self.get_raw_int_value(value).map(|v| v.into()),
            Type::Primitive(PrimitiveType::Float) => {
                self.get_raw_float_value(value).map(|v| v.into())
            }
            Type::Primitive(PrimitiveType::Bool) => {
                self.get_raw_bool_value(value).map(|v| v.into())
            }
            Type::Primitive(PrimitiveType::Char) => self.get_raw_int_value(value).map(|v| v.into()),
            Type::Primitive(PrimitiveType::Str) => Ok(value),
            Type::Primitive(PrimitiveType::Void) | Type::Primitive(PrimitiveType::Auto) => {
                Err(format!("Unsupported tuple field type {:?}", ty))
            }
            _ => Ok(value),
        }
    }

    fn try_generate_primitive_field_method_access(
        &mut self,
        expr: &ExpressionNode,
        field: &str,
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let ExpressionKind::Identifier(obj_name) = &expr.kind else {
            return Ok(None);
        };
        let Some((_, _, type_node)) = self
            .variables
            .get(obj_name)
            .or_else(|| self.global_variables.get(obj_name))
        else {
            return Ok(None);
        };
        let type_node = type_node.clone();
        match type_node {
            Type::Primitive(PrimitiveType::Int) => self.generate_int_field_method(expr, field),
            Type::Primitive(PrimitiveType::Float) => self.generate_float_field_method(expr, field),
            Type::Primitive(PrimitiveType::Bool) => self.generate_bool_field_method(expr, field),
            Type::Primitive(PrimitiveType::Str) => self.generate_string_field_method(expr, field),
            Type::Primitive(PrimitiveType::Char) => self.generate_char_field_method(expr, field),
            _ => Ok(None),
        }
    }

    fn generate_int_field_method(
        &mut self,
        expr: &ExpressionNode,
        field: &str,
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        match field {
            "to_string" => {
                let value = self.generate_expression(expr)?;
                self.value_to_runtime_string(value).map(Some)
            }
            "to_float" => {
                let value = self.generate_expression(expr)?;
                let raw_int = self.get_raw_int_value(value)?;
                let float_val = self
                    .builder
                    .build_signed_int_to_float(raw_int, self.context.f64_type(), "int_to_float")
                    .map_err(|e| e.to_string())?;
                Ok(Some(float_val.into()))
            }
            "to_int" => self.generate_expression(expr).map(Some),
            "to_char" => self.generate_expression(expr).map(Some),
            _ => Ok(None),
        }
    }

    fn generate_float_field_method(
        &mut self,
        expr: &ExpressionNode,
        field: &str,
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        match field {
            "to_string" => {
                let float_val = self.generate_expression(expr)?;
                let func = self
                    .runtime_function("mux_float_to_string")
                    .ok_or("mux_float_to_string not found")?;
                let cstr = self
                    .builder
                    .build_call(func, &[float_val.into()], "float_to_str")
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("mux_float_to_string should return a basic value")?;
                self.cstr_to_mux_string(cstr).map(Some)
            }
            "to_int" => {
                let value = self.generate_expression(expr)?;
                let raw_float = self.get_raw_float_value(value)?;
                let int_val = self
                    .builder
                    .build_float_to_signed_int(raw_float, self.context.i64_type(), "float_to_int")
                    .map_err(|e| e.to_string())?;
                Ok(Some(int_val.into()))
            }
            "to_float" => self.generate_expression(expr).map(Some),
            _ => Ok(None),
        }
    }

    fn generate_bool_field_method(
        &mut self,
        expr: &ExpressionNode,
        field: &str,
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        match field {
            "to_string" => {
                let value = self.generate_expression(expr)?;
                self.value_to_runtime_string(value).map(Some)
            }
            "to_int" => {
                let value = self.generate_expression(expr)?;
                let raw_bool = self.get_raw_bool_value(value)?;
                let int_val = self
                    .builder
                    .build_int_z_extend(raw_bool, self.context.i64_type(), "bool_to_int")
                    .map_err(|e| e.to_string())?;
                Ok(Some(int_val.into()))
            }
            "to_float" => {
                let value = self.generate_expression(expr)?;
                let raw_bool = self.get_raw_bool_value(value)?;
                let float_val = self
                    .builder
                    .build_unsigned_int_to_float(raw_bool, self.context.f64_type(), "bool_to_float")
                    .map_err(|e| e.to_string())?;
                Ok(Some(float_val.into()))
            }
            _ => Ok(None),
        }
    }

    fn generate_string_field_method(
        &mut self,
        expr: &ExpressionNode,
        field: &str,
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        match field {
            "to_string" => {
                let value = self.generate_expression(expr)?;
                self.value_to_runtime_string(value).map(Some)
            }
            "to_int" => self.generate_string_conversion_field_method(
                expr,
                "mux_string_to_int",
                "str_to_int",
            ),
            "to_float" => self.generate_string_conversion_field_method(
                expr,
                "mux_string_to_float",
                "str_to_float",
            ),
            "to_char" => self.generate_string_conversion_field_method(
                expr,
                "mux_string_to_char",
                "str_to_char",
            ),
            _ => Ok(None),
        }
    }

    /// Codegen for the `string.to_X()` field-method family (`to_int`,
    /// `to_float`, `to_char`). Generates the expression, converts the
    /// value to a C string, calls the runtime conversion function, and
    /// returns the boxed result.
    fn generate_string_conversion_field_method(
        &mut self,
        expr: &ExpressionNode,
        func_name: &str,
        call_name: &str,
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let value = self.generate_expression(expr)?;
        let cstr = self.value_to_cstr(value)?;
        let func = self
            .runtime_function(func_name)
            .ok_or(format!("{} not found", func_name))?;
        let result_ptr = self
            .builder
            .build_call(func, &[cstr.into()], call_name)
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or(format!("{} should return a basic value", func_name))?;
        Ok(Some(result_ptr))
    }

    fn generate_char_field_method(
        &mut self,
        expr: &ExpressionNode,
        field: &str,
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        match field {
            "to_int" => {
                let char_val = self.generate_expression(expr)?;
                let func = self
                    .runtime_function("mux_char_to_int")
                    .ok_or("mux_char_to_int not found")?;
                let result_ptr = self
                    .builder
                    .build_call(func, &[char_val.into()], "char_to_int")
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("mux_char_to_int should return a basic value")?;
                Ok(Some(result_ptr))
            }
            "to_string" => {
                let char_val = self.generate_expression(expr)?;
                let func = self
                    .runtime_function("mux_char_to_string")
                    .ok_or("mux_char_to_string not found")?;
                let cstr = self
                    .builder
                    .build_call(func, &[char_val.into()], "char_to_cstr")
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("mux_char_to_string should return a basic value")?;
                self.cstr_to_mux_string(cstr).map(Some)
            }
            "to_char" => self.generate_expression(expr).map(Some),
            _ => Ok(None),
        }
    }

    fn value_to_cstr(&mut self, value: BasicValueEnum<'a>) -> Result<BasicValueEnum<'a>, String> {
        let func = self
            .runtime_function("mux_value_to_string")
            .ok_or("mux_value_to_string not found")?;
        self.builder
            .build_call(func, &[value.into()], "val_to_cstr")
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or("mux_value_to_string should return a basic value".to_string())
    }

    /// Emit a unified runtime panic terminator: the message and an optional
    /// `--> file:line:col` location go to stderr and the process exits with
    /// code 1. The current block is consumed and left unreachable.
    pub(super) fn emit_runtime_fatal(
        &mut self,
        message: &str,
        span: Option<&Span>,
        block_name: &str,
    ) -> Result<(), String> {
        let error_msg = self
            .builder
            .build_global_string_ptr(message, &format!("{}_msg", block_name))
            .map_err(|e| e.to_string())?;
        let loc = self.panic_location_arg(span, block_name)?;
        self.generate_runtime_call(
            "mux_panic_cstr",
            &[error_msg.as_pointer_value().into(), loc.into()],
        );
        self.builder
            .build_unreachable()
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Build the `file:line:col` location argument for the runtime panic
    /// functions: a global string pointer, or null when no span is available.
    pub(super) fn panic_location_arg(
        &mut self,
        span: Option<&Span>,
        block_name: &str,
    ) -> Result<BasicValueEnum<'a>, String> {
        match span {
            Some(span) => {
                let loc = self.panic_location(span);
                let global = self
                    .builder
                    .build_global_string_ptr(&loc, &format!("{}_loc", block_name))
                    .map_err(|e| e.to_string())?;
                Ok(global.as_pointer_value().into())
            }
            None => Ok(self
                .context
                .ptr_type(AddressSpace::default())
                .const_null()
                .into()),
        }
    }

    /// Look up `key` in the raw `map` and return the unwrapped value, freeing
    /// the raw map pointer. If the key is missing, emits a key-not-found panic
    /// block and branches to a new "continue" block where the caller can
    /// continue.
    ///
    /// Returns the unwrapped value (`BasicValueEnum`).
    fn map_get_or_panic(
        &mut self,
        map: PointerValue<'a>,
        key: BasicValueEnum<'a>,
        key_type: &Type,
        span: Option<&Span>,
        block_prefix: &str,
    ) -> Result<BasicValueEnum<'a>, String> {
        // Box an enum key as a managed value so its structural comparison matches
        // the map's stored keys (issue #309); other keys box normally.
        let boxed_key = self.box_enum_or_value(key, key_type)?;

        let optional_ptr = self
            .builder
            .build_call(
                self.runtime_function("mux_map_get")
                    .expect("mux_map_get must be declared in runtime"),
                &[map.into(), boxed_key.into()],
                &format!("{}_get", block_prefix),
            )
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .expect("mux_map_get should return a basic value")
            .into_pointer_value();

        self.unwrap_map_lookup_or_panic(optional_ptr, boxed_key, Some(map), span, block_prefix)
    }

    /// Look up `key` directly in a map `Value` and return the unwrapped value.
    /// Unlike `map_get_or_panic`, this reads the live map without extracting a
    /// cloned copy first, so indexing a map does not clone the whole map on
    /// every access. Emits the same key-not-found panic on a missing key.
    fn map_value_get_or_panic(
        &mut self,
        map_value: BasicValueEnum<'a>,
        key: BasicValueEnum<'a>,
        key_type: &Type,
        span: Option<&Span>,
        block_prefix: &str,
    ) -> Result<BasicValueEnum<'a>, String> {
        // Box an enum key as a managed value so its structural comparison matches
        // the map's stored keys (issue #309); other keys box normally.
        let boxed_key = self.box_enum_or_value(key, key_type)?;

        let optional_ptr = self
            .builder
            .build_call(
                self.runtime_function("mux_value_map_get_value")
                    .ok_or("mux_value_map_get_value not found")?,
                &[map_value.into(), boxed_key.into()],
                &format!("{}_get", block_prefix),
            )
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or("mux_value_map_get_value should return a basic value")?
            .into_pointer_value();

        self.unwrap_map_lookup_or_panic(optional_ptr, boxed_key, None, span, block_prefix)
    }

    /// Shared tail for map lookups: given the `Optional` result and the boxed
    /// key, branch to a key-not-found panic when it is `None`, otherwise (in the
    /// continue block) free `map_to_free` if present, unwrap the optional, and
    /// return the contained value.
    fn unwrap_map_lookup_or_panic(
        &mut self,
        optional_ptr: PointerValue<'a>,
        boxed_key: PointerValue<'a>,
        map_to_free: Option<PointerValue<'a>>,
        span: Option<&Span>,
        block_prefix: &str,
    ) -> Result<BasicValueEnum<'a>, String> {
        let is_some = self
            .builder
            .build_call(
                self.runtime_function("mux_optional_is_some")
                    .expect("mux_optional_is_some must be declared in runtime"),
                &[optional_ptr.into()],
                &format!("{}_has_key", block_prefix),
            )
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .expect("mux_optional_is_some should return a basic value")
            .into_int_value();

        let current_function = self
            .builder
            .get_insert_block()
            .expect("Builder should have an insertion block")
            .get_parent()
            .ok_or("No current function")?;

        let error_bb = self
            .context
            .append_basic_block(current_function, &format!("{}_error", block_prefix));
        let continue_bb = self
            .context
            .append_basic_block(current_function, &format!("{}_continue", block_prefix));

        self.builder
            .build_conditional_branch(is_some, continue_bb, error_bb)
            .map_err(|e| e.to_string())?;

        // Error block: panic with the missing key and source location
        self.builder.position_at_end(error_bb);
        let loc = self.panic_location_arg(span, &format!("{}_error", block_prefix))?;
        self.generate_runtime_call("mux_panic_key_not_found", &[boxed_key.into(), loc.into()]);
        self.builder
            .build_unreachable()
            .map_err(|e| e.to_string())?;

        // Continue block: free the raw map (if the caller extracted one), then
        // unwrap the optional.
        self.builder.position_at_end(continue_bb);
        if let Some(map) = map_to_free {
            let free_map_fn = self
                .runtime_function("mux_free_map")
                .ok_or("mux_free_map not found")?;
            self.builder
                .build_call(free_map_fn, &[map.into()], "free_map")
                .map_err(|e| e.to_string())?;
        }

        let unwrapped = self
            .builder
            .build_call(
                self.runtime_function("mux_optional_get_value")
                    .expect("mux_optional_get_value must be declared in runtime"),
                &[optional_ptr.into()],
                &format!("{}_unwrap", block_prefix),
            )
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or("mux_optional_get_value should return a basic value".to_string())?;
        // The optional wrapper is an owned value; get_value clones the inner
        // value out, so release the wrapper to avoid leaking it.
        let rc_dec = self
            .runtime_function("mux_rc_dec")
            .ok_or("mux_rc_dec not found")?;
        self.builder
            .build_call(rc_dec, &[optional_ptr.into()], "rc_dec_optional")
            .map_err(|e| e.to_string())?;
        Ok(unwrapped)
    }

    /// Look up `index` in the raw `list` (after negative-index normalization)
    /// and return the value, freeing the raw list pointer. If the index is
    /// out of bounds, emits an index-out-of-bounds panic block and branches
    /// to a new "continue" block.
    ///
    /// Returns the unwrapped value (`BasicValueEnum`).
    fn list_get_or_panic(
        &mut self,
        list: PointerValue<'a>,
        index: BasicValueEnum<'a>,
        span: Option<&Span>,
        block_prefix: &str,
    ) -> Result<BasicValueEnum<'a>, String> {
        let normalized_index = self.normalize_list_index(index, list)?;

        let length = self
            .builder
            .build_call(
                self.runtime_function("mux_list_length")
                    .ok_or("mux_list_length not found")?,
                &[list.into()],
                &format!("{}_len", block_prefix),
            )
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or("mux_list_length should return a basic value")?;

        let next = self
            .builder
            .build_call(
                self.runtime_function("mux_list_get_value")
                    .expect("mux_list_get_value must be declared in runtime"),
                &[list.into(), normalized_index.into()],
                &format!("{}_get", block_prefix),
            )
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .expect("mux_list_get_value should return a basic value");

        // Bounds check: a null result means the index was out of range. Report
        // the user's original index (pre-normalization).
        self.check_list_bounds(next.into_pointer_value(), index, length, span, block_prefix)?;

        // Free the raw list.
        let free_list_fn = self
            .runtime_function("mux_free_list")
            .ok_or("mux_free_list not found")?;
        self.builder
            .build_call(free_list_fn, &[list.into()], "free_list")
            .map_err(|e| e.to_string())?;

        Ok(next)
    }

    /// If `ptr` is null (an out-of-bounds `mux_list_get_value` result), emit
    /// an index-out-of-bounds panic block carrying the index, list length,
    /// and source location, then position the builder at a new "continue"
    /// block.
    fn check_list_bounds(
        &mut self,
        ptr: inkwell::values::PointerValue<'a>,
        index: BasicValueEnum<'a>,
        length: BasicValueEnum<'a>,
        span: Option<&Span>,
        block_prefix: &str,
    ) -> Result<(), String> {
        let is_null = self
            .builder
            .build_is_null(ptr, &format!("{}_is_null", block_prefix))
            .map_err(|e| e.to_string())?;

        let current_function = self
            .builder
            .get_insert_block()
            .expect("Builder should have an insertion block")
            .get_parent()
            .ok_or("No current function")?;

        let error_bb = self
            .context
            .append_basic_block(current_function, &format!("{}_error", block_prefix));
        let continue_bb = self
            .context
            .append_basic_block(current_function, &format!("{}_continue", block_prefix));

        self.builder
            .build_conditional_branch(is_null, error_bb, continue_bb)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(error_bb);
        let loc = self.panic_location_arg(span, &format!("{}_error", block_prefix))?;
        self.generate_runtime_call(
            "mux_panic_index_oob",
            &[index.into(), length.into(), loc.into()],
        );
        self.builder
            .build_unreachable()
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(continue_bb);
        Ok(())
    }

    fn cstr_to_mux_string(
        &mut self,
        cstr: BasicValueEnum<'a>,
    ) -> Result<BasicValueEnum<'a>, String> {
        let func_new = self
            .runtime_function("mux_new_string_from_cstr")
            .ok_or("mux_new_string_from_cstr not found")?;
        self.builder
            .build_call(func_new, &[cstr.into()], "new_str")
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or("mux_new_string_from_cstr should return a basic value".to_string())
    }

    /// Load the current `self` pointer from the variable table (or global
    /// table) and return it as a `PointerValue` ready to be passed to
    /// `mux_get_object_ptr`. Returns `Ok(None)` when no `self` is in scope.
    fn load_self_ptr(&mut self, load_name: &str) -> Result<Option<PointerValue<'a>>, String> {
        let Some((self_ptr, _, _)) = self
            .variables
            .get("self")
            .or_else(|| self.global_variables.get("self"))
        else {
            return Ok(None);
        };
        let ptr = self
            .builder
            .build_load(
                self.context.ptr_type(AddressSpace::default()),
                *self_ptr,
                load_name,
            )
            .map_err(|e| e.to_string())?
            .into_pointer_value();
        Ok(Some(ptr))
    }

    fn value_to_runtime_string(
        &mut self,
        value: BasicValueEnum<'a>,
    ) -> Result<BasicValueEnum<'a>, String> {
        let cstr = self.value_to_cstr(value)?;
        self.cstr_to_mux_string(cstr)
    }

    fn generate_expression_impl(
        &mut self,
        expr: &ExpressionNode,
    ) -> Result<BasicValueEnum<'a>, String> {
        match &expr.kind {
            ExpressionKind::Literal(lit) => self.generate_literal(lit),
            ExpressionKind::None => self.generate_none_expression(),
            ExpressionKind::Identifier(name) => self.generate_identifier_expression(name),
            ExpressionKind::Binary {
                left, op, right, ..
            } => self.generate_binary_expression(left, op, right),
            ExpressionKind::Call { func, args } => {
                let result = self.generate_call_expression(expr, func, args)?;
                // A call that returns a closure hands back an owned (+1) closure
                // (the callee retained it). Register it so the caller releases it
                // at statement end unless a binding/return transfers ownership.
                if result.is_pointer_value()
                    && matches!(
                        self.get_resolved_expression_type(expr),
                        Ok(Type::Function { .. })
                    )
                {
                    self.register_closure_temp(result);
                }
                Ok(result)
            }
            ExpressionKind::ListAccess {
                expr: target_expr,
                index,
            } => self.generate_list_access_expression(target_expr, index),
            ExpressionKind::ListLiteral(elements) => {
                self.generate_list_literal_expression(elements)
            }
            ExpressionKind::MapLiteral { entries, .. } => {
                self.generate_map_literal_expression(entries)
            }
            ExpressionKind::SetLiteral(elements) => {
                self.generate_set_literal_expression(expr, elements)
            }
            ExpressionKind::TupleLiteral(elements) => {
                self.generate_tuple_literal_expression(elements)
            }
            ExpressionKind::If {
                cond,
                then_expr,
                else_expr,
            } => Ok(self.generate_if_expression(cond, then_expr, else_expr)?),
            ExpressionKind::Lambda {
                params,
                return_type,
                body,
                where_clause,
            } => Ok(self.generate_lambda_expression(
                expr,
                params,
                return_type,
                body,
                where_clause.as_ref(),
            )?),
            ExpressionKind::FieldAccess { expr, field } => {
                self.generate_field_access_expression(expr, field)
            }
            ExpressionKind::Unary { op, expr, .. } => self.generate_unary_expression(op, expr),
            _ => Err("Expression type not implemented".to_string()),
        }
    }
    pub(super) fn generate_literal(
        &mut self,
        lit: &LiteralNode,
    ) -> Result<BasicValueEnum<'a>, String> {
        match lit {
            LiteralNode::Integer(i) => {
                let val = self.context.i64_type().const_int(*i as u64, true);
                Ok(val.into())
            }
            LiteralNode::Float(f) => {
                let val = self.context.f64_type().const_float(f.into_inner());
                Ok(val.into())
            }
            LiteralNode::Boolean(b) => {
                // Generate i1 (bool) value directly, like int/float literals
                let val = self
                    .context
                    .bool_type()
                    .const_int(if *b { 1 } else { 0 }, false);
                Ok(val.into())
            }
            LiteralNode::String(s) => {
                let name = format!("str_{}", self.string_counter);
                self.string_counter += 1;
                let bytes = s.as_bytes();
                let mut values = vec![];
                for &b in bytes {
                    values.push(self.context.i8_type().const_int(b as u64, false));
                }
                values.push(self.context.i8_type().const_int(0, false));
                let array_type = self.context.i8_type().array_type(values.len() as u32);
                let const_array = self.context.i8_type().const_array(&values);
                let global =
                    self.module
                        .add_global(array_type, Some(AddressSpace::default()), &name);
                global.set_linkage(inkwell::module::Linkage::External);
                global.set_initializer(&const_array);
                let ptr = unsafe {
                    self.builder.build_in_bounds_gep(
                        array_type,
                        global.as_pointer_value(),
                        &[
                            self.context.i32_type().const_int(0, false),
                            self.context.i32_type().const_int(0, false),
                        ],
                        &name,
                    )
                }
                .map_err(|e| e.to_string())?;
                let call = self
                    .generate_runtime_call("mux_new_string_from_cstr", &[ptr.into()])
                    .expect("mux_new_string_from_cstr should always return a value");
                // A string literal is a freshly allocated owned Mux string.
                // Register it so uses that do not bind it (e.g. a string pattern
                // compared in a match) release it at statement end; a binding
                // transfers it out of the temp set instead of deep-cloning.
                self.register_temp(call);
                Ok(call)
            }
            LiteralNode::Char(c) => {
                // Chars are stored as i64 for compatibility with int comparisons
                let val = self.context.i64_type().const_int(*c as u64, false);
                Ok(val.into())
            }
        }
    }

    /// Normalizes a list index to handle negative wraparound.
    ///
    /// Given an index value and the list length, returns a normalized index:
    /// - If index >= 0, returns index as-is
    /// - If index < 0, returns length + index (wraparound from end)
    ///
    /// This matches the behavior of mux_list_set_value in the runtime.
    fn normalize_list_index(
        &mut self,
        index_val: BasicValueEnum<'a>,
        list_ptr: PointerValue<'a>,
    ) -> Result<BasicValueEnum<'a>, String> {
        // Get list length from the raw List pointer, then normalize.
        let list_length = self
            .builder
            .build_call(
                self.runtime_function("mux_list_length")
                    .expect("mux_list_length must be declared in runtime"),
                &[list_ptr.into()],
                "list_len",
            )
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .expect("mux_list_length should return a basic value")
            .into_int_value();

        self.normalize_index_with_length(index_val, list_length)
    }

    /// Normalize a (possibly negative) index against a precomputed list length:
    /// negative indices wrap from the end, non-negative pass through unchanged.
    fn normalize_index_with_length(
        &mut self,
        index_val: BasicValueEnum<'a>,
        list_length: inkwell::values::IntValue<'a>,
    ) -> Result<BasicValueEnum<'a>, String> {
        let index_int = index_val.into_int_value();

        // Check if index < 0
        let is_negative = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLT,
                index_int,
                self.context.i64_type().const_zero(),
                "is_negative",
            )
            .map_err(|e| e.to_string())?;

        // If negative, compute (length + index), otherwise use index as-is
        let wrapped_index = self
            .builder
            .build_int_add(list_length, index_int, "wrapped_index")
            .map_err(|e| e.to_string())?;

        let normalized_index = self
            .builder
            .build_select(is_negative, wrapped_index, index_int, "normalized_index")
            .map_err(|e| e.to_string())?;

        Ok(normalized_index)
    }

    /// Extracts the base expression and all indices from a nested ListAccess chain.
    ///
    /// For example, `matrix[0][1]` returns `(matrix, [0, 1])`
    /// For `hypercube[0][1][2][3]` returns `(hypercube, [0, 1, 2, 3])`
    fn collect_list_access_chain<'b>(
        &self,
        expr: &'b ExpressionNode,
    ) -> (&'b ExpressionNode, Vec<&'b ExpressionNode>) {
        let mut indices = Vec::new();
        let mut current = expr;

        // Walk the chain backwards, collecting indices
        while let ExpressionKind::ListAccess { expr: inner, index } = &current.kind {
            indices.push(index.as_ref());
            current = inner.as_ref();
        }

        // We collected from innermost to outermost, so reverse
        indices.reverse();
        (current, indices)
    }

    /// Helper function to handle nested collection assignments of arbitrary depth.
    ///
    /// This handles assignments for any depth of nesting with Lists and Maps:
    /// - `matrix[0][1] = value` (2 levels, List)
    /// - `cube[0][1][2] = value` (3 levels, List)
    /// - `map_of_lists["key"][0] = value` (2 levels, Map then List)
    /// - `list_of_maps[0]["key"] = value` (2 levels, List then Map)
    ///
    /// Algorithm (recursive get-modify-writeback):
    /// For base[i1][i2]...[iN] = value:
    /// 1. If N == 1: base[i1] = value (base case, direct assignment)
    /// 2. If N > 1:
    ///    a. Get base[i1] -> temp (copy)
    ///    b. Recursively: temp[i2]...[iN] = value
    ///    c. Write temp back: base[i1] = temp
    fn generate_nested_collection_assignment(
        &mut self,
        base_expr: &ExpressionNode,
        indices: &[&ExpressionNode],
        value: BasicValueEnum<'a>,
    ) -> Result<(), String> {
        if indices.is_empty() {
            return Err("generate_nested_collection_assignment called with no indices".to_string());
        }

        if indices.len() == 1 {
            self.assign_single_collection_index(base_expr, indices[0], value)
        } else {
            // RECURSIVE CASE: base[i1][i2]...[iN] = value where N > 1
            // Strategy:
            // 1. Get base[i1] -> temp (copy)
            // 2. Recursively handle temp[i2]...[iN] = value
            // 3. Write temp back to base[i1]

            // Determine if base is a List or Map
            let base_type = self.get_resolved_expression_type(base_expr)?;

            let base_val = self.generate_expression(base_expr)?;
            let first_index_val = self.generate_expression(indices[0])?;

            // Get base[i1] - different logic for List vs Map
            let intermediate_val = match base_type {
                crate::semantics::Type::List(_) => {
                    // Extract raw List from base
                    let raw_base_list = self
                        .builder
                        .build_call(
                            self.runtime_function("mux_value_get_list")
                                .expect("mux_value_get_list must be declared in runtime"),
                            &[base_val.into()],
                            "extract_list",
                        )
                        .map_err(|e| e.to_string())?
                        .try_as_basic_value()
                        .basic()
                        .expect("mux_value_get_list should return a basic value")
                        .into_pointer_value();

                    self.list_get_or_panic(
                        raw_base_list,
                        first_index_val,
                        Some(indices[0].span()),
                        "nested_assign_list",
                    )?
                }
                crate::semantics::Type::Map(_, _) => {
                    let key_type = match &base_type {
                        crate::semantics::Type::Map(k, _) => k.as_ref().clone(),
                        _ => unreachable!(),
                    };
                    // Extract raw Map from base
                    let raw_base_map = self
                        .builder
                        .build_call(
                            self.runtime_function("mux_value_get_map")
                                .expect("mux_value_get_map must be declared in runtime"),
                            &[base_val.into()],
                            "extract_map",
                        )
                        .map_err(|e| e.to_string())?
                        .try_as_basic_value()
                        .basic()
                        .expect("mux_value_get_map should return a basic value")
                        .into_pointer_value();

                    self.map_get_or_panic(
                        raw_base_map,
                        first_index_val,
                        &key_type,
                        Some(indices[0].span()),
                        "nested_assign_map",
                    )?
                }
                _ => {
                    return Err(format!(
                        "Cannot use nested indexing on non-list/map type: {:?}",
                        base_type
                    ));
                }
            };

            // list_get_or_panic / map_get_or_panic already panicked on a
            // missing element, so intermediate_val is guaranteed non-null.

            // Now we need to handle: intermediate_val[i2]...[iN] = value
            // But intermediate_val is a *mut Value, not an ExpressionNode
            // We need to handle the remaining indices iteratively

            // Get the element type of the intermediate collection
            let intermediate_type = match &base_type {
                crate::semantics::Type::List(elem_type) => elem_type.as_ref().clone(),
                crate::semantics::Type::Map(_, value_type) => value_type.as_ref().clone(),
                _ => unreachable!(),
            };

            let remaining_indices = &indices[1..];
            self.apply_indices_and_set(
                intermediate_val,
                &intermediate_type,
                remaining_indices,
                value,
            )?;

            // Write the modified intermediate value back to base[i1]
            match base_type {
                crate::semantics::Type::List(_) => {
                    self.builder
                        .build_call(
                            self.runtime_function("mux_list_set_value")
                                .expect("mux_list_set_value must be declared in runtime"),
                            &[
                                base_val.into(),
                                first_index_val.into(),
                                intermediate_val.into(),
                            ],
                            "list_writeback",
                        )
                        .map_err(|e| e.to_string())?;
                }
                crate::semantics::Type::Map(_, _) => {
                    let boxed_key = self.box_value(first_index_val);
                    self.builder
                        .build_call(
                            self.runtime_function("mux_map_put_value")
                                .expect("mux_map_put_value must be declared in runtime"),
                            &[base_val.into(), boxed_key.into(), intermediate_val.into()],
                            "map_writeback",
                        )
                        .map_err(|e| e.to_string())?;
                }
                _ => unreachable!(),
            }

            // list_get_or_panic / map_get_or_panic returned an owned (+1) clone of
            // the intermediate collection, and the writeback clones it again, so
            // the intermediate is ours to release.
            if intermediate_val.is_pointer_value() {
                self.emit_value_decref(intermediate_val.into_pointer_value())?;
            }

            Ok(())
        }
    }

    /// Base case of a nested collection assignment: a single-index write into a
    /// list or map. An enum map key is boxed as a managed value so it matches the
    /// map's stored keys (issue #309).
    fn assign_single_collection_index(
        &mut self,
        base_expr: &ExpressionNode,
        index: &ExpressionNode,
        value: BasicValueEnum<'a>,
    ) -> Result<(), String> {
        let base_type = self.get_resolved_expression_type(base_expr)?;
        let base_val = self.generate_expression(base_expr)?;
        let index_val = self.generate_expression(index)?;
        match base_type {
            crate::semantics::Type::List(_) => {
                self.builder
                    .build_call(
                        self.runtime_function("mux_list_set_value")
                            .expect("mux_list_set_value must be declared in runtime"),
                        &[base_val.into(), index_val.into(), value.into()],
                        "nested_list_set_direct",
                    )
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
            crate::semantics::Type::Map(key_type, _) => {
                let boxed_key = self.box_enum_or_value(index_val, &key_type)?;
                self.builder
                    .build_call(
                        self.runtime_function("mux_map_put_value")
                            .expect("mux_map_put_value must be declared in runtime"),
                        &[base_val.into(), boxed_key.into(), value.into()],
                        "nested_map_set_direct",
                    )
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
            other => Err(format!(
                "Cannot assign to index on non-list/map type: {:?}",
                other
            )),
        }
    }

    /// Helper to apply a chain of indices to a value and set the final element.
    /// This handles: value[i1][i2]...[iN] = new_value
    /// where `value` is a BasicValueEnum (*mut Value), not an ExpressionNode.
    ///
    /// Requires the type of `current_val` to determine if it's a List or Map.
    fn apply_indices_and_set(
        &mut self,
        current_val: BasicValueEnum<'a>,
        current_type: &crate::semantics::Type,
        indices: &[&ExpressionNode],
        final_value: BasicValueEnum<'a>,
    ) -> Result<(), String> {
        if indices.is_empty() {
            return Err("apply_indices_and_set called with no indices".to_string());
        }

        if indices.len() == 1 {
            // Base case: current_val[index] = final_value
            let index_val = self.generate_expression(indices[0])?;

            match current_type {
                crate::semantics::Type::List(_) => {
                    self.builder
                        .build_call(
                            self.runtime_function("mux_list_set_value")
                                .expect("mux_list_set_value must be declared in runtime"),
                            &[current_val.into(), index_val.into(), final_value.into()],
                            "apply_list_set",
                        )
                        .map_err(|e| e.to_string())?;
                }
                crate::semantics::Type::Map(_, _) => {
                    let boxed_key = self.box_value(index_val);
                    self.builder
                        .build_call(
                            self.runtime_function("mux_map_put_value")
                                .expect("mux_map_put_value must be declared in runtime"),
                            &[current_val.into(), boxed_key.into(), final_value.into()],
                            "apply_map_set",
                        )
                        .map_err(|e| e.to_string())?;
                }
                _ => {
                    return Err(format!(
                        "Cannot apply index to non-list/map type: {:?}",
                        current_type
                    ));
                }
            }
            Ok(())
        } else {
            // Recursive case: get current_val[i1], recurse on remaining, write back
            let first_index_val = self.generate_expression(indices[0])?;

            // Get current_val[i1] - different logic for List vs Map
            let next_val = match current_type {
                crate::semantics::Type::List(_) => {
                    // Extract raw List
                    let raw_list = self
                        .builder
                        .build_call(
                            self.runtime_function("mux_value_get_list")
                                .expect("mux_value_get_list must be declared in runtime"),
                            &[current_val.into()],
                            "extract_for_apply",
                        )
                        .map_err(|e| e.to_string())?
                        .try_as_basic_value()
                        .basic()
                        .expect("mux_value_get_list should return a basic value")
                        .into_pointer_value();

                    self.list_get_or_panic(
                        raw_list,
                        first_index_val,
                        Some(indices[0].span()),
                        "apply_list",
                    )?
                }
                crate::semantics::Type::Map(_, _) => {
                    let key_type = match &current_type {
                        crate::semantics::Type::Map(k, _) => k.as_ref().clone(),
                        _ => unreachable!(),
                    };
                    // Extract raw Map
                    let raw_map = self
                        .builder
                        .build_call(
                            self.runtime_function("mux_value_get_map")
                                .expect("mux_value_get_map must be declared in runtime"),
                            &[current_val.into()],
                            "extract_map_for_apply",
                        )
                        .map_err(|e| e.to_string())?
                        .try_as_basic_value()
                        .basic()
                        .expect("mux_value_get_map should return a basic value")
                        .into_pointer_value();

                    self.map_get_or_panic(
                        raw_map,
                        first_index_val,
                        &key_type,
                        Some(indices[0].span()),
                        "apply_map",
                    )?
                }
                _ => {
                    return Err(format!(
                        "Cannot apply nested index to non-list/map type: {:?}",
                        current_type
                    ));
                }
            };

            // Get the element type for recursion
            let next_type = match current_type {
                crate::semantics::Type::List(elem_type) => elem_type.as_ref().clone(),
                crate::semantics::Type::Map(_, value_type) => value_type.as_ref().clone(),
                _ => unreachable!(),
            };

            // Recurse on the remaining indices
            self.apply_indices_and_set(next_val, &next_type, &indices[1..], final_value)?;

            // Write back
            match current_type {
                crate::semantics::Type::List(_) => {
                    self.builder
                        .build_call(
                            self.runtime_function("mux_list_set_value")
                                .expect("mux_list_set_value must be declared in runtime"),
                            &[current_val.into(), first_index_val.into(), next_val.into()],
                            "apply_list_writeback",
                        )
                        .map_err(|e| e.to_string())?;
                }
                crate::semantics::Type::Map(_, _) => {
                    let boxed_key = self.box_value(first_index_val);
                    self.builder
                        .build_call(
                            self.runtime_function("mux_map_put_value")
                                .expect("mux_map_put_value must be declared in runtime"),
                            &[current_val.into(), boxed_key.into(), next_val.into()],
                            "apply_map_writeback",
                        )
                        .map_err(|e| e.to_string())?;
                }
                _ => unreachable!(),
            }

            Ok(())
        }
    }
}

/// Map a Mux `Type` to the runtime function name used to wrap a value of
/// that type in a `result` or `optional` (e.g. `mux_result_ok_int`,
/// `mux_optional_some_value`). Returns `None` for types that have no
/// specialized constructor at the runtime layer.
fn boxed_value_constructor_name(arg_type: &Type, prefix: &str) -> Option<&'static str> {
    let suffix = match arg_type {
        Type::Primitive(PrimitiveType::Int) => "int",
        Type::Primitive(PrimitiveType::Float) => "float",
        Type::Primitive(PrimitiveType::Bool) => "bool",
        Type::Primitive(PrimitiveType::Char) => "char",
        // Everything already represented as a `*mut Value` shares the generic
        // constructor. `optional`, `result` and `tuple` belong here for the
        // same reason as a list or a class: nesting the built-in wrappers
        // (`optional<result<int, string>>`) is an ordinary shape, and leaving
        // them out rejected it as an unsupported type.
        Type::Primitive(PrimitiveType::Str)
        | Type::List(_)
        | Type::Map(_, _)
        | Type::Set(_)
        | Type::Optional(_)
        | Type::Result(_, _)
        | Type::Tuple(_, _)
        | Type::Named(_, _)
        | Type::Instantiated(_, _) => "value",
        _ => return None,
    };
    Some(match prefix {
        "mux_result_ok" => match suffix {
            "int" => "mux_result_ok_int",
            "float" => "mux_result_ok_float",
            "bool" => "mux_result_ok_bool",
            "char" => "mux_result_ok_char",
            _ => "mux_result_ok_value",
        },
        "mux_optional_some" => match suffix {
            "int" => "mux_optional_some_int",
            "float" => "mux_optional_some_float",
            "bool" => "mux_optional_some_bool",
            "char" => "mux_optional_some_char",
            _ => "mux_optional_some_value",
        },
        _ => return None,
    })
}
