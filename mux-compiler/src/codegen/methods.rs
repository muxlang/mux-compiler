//! Method call generation for the code generator.
//!
//! This module handles:
//! - Instance method calls on user-defined classes
//! - Primitive type method calls (int, float, str, bool, char)
//! - Collection method calls (list, map, set)
//! - Optional method calls

use inkwell::AddressSpace;
use inkwell::types::BasicType;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};

use crate::ast::{ExpressionNode, PrimitiveType};
use crate::semantics::Type;
use crate::semantics::format::format_type;

use super::CodeGenerator;

fn gen_one_expr<'a>(
    s: &mut CodeGenerator<'a>,
    args: &[ExpressionNode],
) -> Result<BasicValueEnum<'a>, String> {
    if args.len() != 1 {
        return Err("method takes exactly 1 argument".to_string());
    }
    s.generate_expression(&args[0])
}

fn gen_two_expr<'a>(
    s: &mut CodeGenerator<'a>,
    args: &[ExpressionNode],
) -> Result<(BasicValueEnum<'a>, BasicValueEnum<'a>), String> {
    if args.len() != 2 {
        return Err("method takes exactly 2 arguments".to_string());
    }
    let a = s.generate_expression(&args[0])?;
    let b = s.generate_expression(&args[1])?;
    Ok((a, b))
}

fn gen_three_expr<'a>(
    s: &mut CodeGenerator<'a>,
    args: &[ExpressionNode],
) -> Result<(BasicValueEnum<'a>, BasicValueEnum<'a>, BasicValueEnum<'a>), String> {
    if args.len() != 3 {
        return Err("method takes exactly 3 arguments".to_string());
    }
    let a = s.generate_expression(&args[0])?;
    let b = s.generate_expression(&args[1])?;
    let c = s.generate_expression(&args[2])?;
    Ok((a, b, c))
}

impl<'a> CodeGenerator<'a> {
    /// Wrap an *owned* C string (returned by a runtime `*_to_string` helper) in
    /// a Mux string Value. Uses the ownership-taking constructor so the input C
    /// string is freed after its contents are copied, rather than leaked.
    pub(super) fn call_cstr_to_mux_string(
        &self,
        cstr_ptr: BasicValueEnum<'a>,
    ) -> Result<BasicValueEnum<'a>, String> {
        let new_string = self
            .runtime_function("mux_new_string_from_owned_cstr")
            .ok_or("mux_new_string_from_owned_cstr not found")?;
        let call = self
            .builder
            .build_call(new_string, &[cstr_ptr.into()], "new_string")
            .map_err(|e| e.to_string())?;
        Ok(call
            .try_as_basic_value()
            .basic()
            .expect("mux_new_string_from_owned_cstr should return a basic value"))
    }

    pub(super) fn call_runtime_function(
        &self,
        func_name: &str,
        args: &[BasicValueEnum<'a>],
    ) -> Result<BasicValueEnum<'a>, String> {
        let func = self
            .runtime_function(func_name)
            .ok_or(format!("Function '{func_name}' not found"))?;
        let call = self
            .builder
            .build_call(
                func,
                &args.iter().map(|v| (*v).into()).collect::<Vec<_>>(),
                "call",
            )
            .map_err(|e| e.to_string())?;
        call.try_as_basic_value()
            .basic()
            .ok_or_else(|| format!("{func_name} should return a basic value"))
    }

    fn call_runtime_to_string(
        &self,
        value: BasicValueEnum<'a>,
        func_name: &str,
    ) -> Result<BasicValueEnum<'a>, String> {
        let to_cstr = self
            .runtime_function(func_name)
            .ok_or(format!("{func_name} not found"))?;
        let call = self
            .builder
            .build_call(to_cstr, &[value.into()], "to_cstr")
            .map_err(|e| e.to_string())?;
        let cstr = call
            .try_as_basic_value()
            .basic()
            .ok_or(format!("{func_name} should return a basic value"))?;
        self.call_cstr_to_mux_string(cstr)
    }

    fn call_runtime_to_string_from_call(
        &self,
        call: inkwell::values::CallSiteValue<'a>,
    ) -> Result<BasicValueEnum<'a>, String> {
        let cstr = call
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| "Function should return a basic value".to_string())?;
        self.call_cstr_to_mux_string(cstr)
    }

    fn generate_to_string_call(
        &self,
        obj_value: BasicValueEnum<'a>,
    ) -> Result<BasicValueEnum<'a>, String> {
        let func = self
            .runtime_function("mux_value_to_string")
            .ok_or("mux_value_to_string not found")?;
        let call = self
            .builder
            .build_call(func, &[obj_value.into()], "val_to_str")
            .map_err(|e| e.to_string())?;
        let cstr = call
            .try_as_basic_value()
            .basic()
            .expect("mux_value_to_string should return a basic value");
        self.call_cstr_to_mux_string(cstr)
    }

    fn extract_raw_pointer(
        &self,
        obj_value: BasicValueEnum<'a>,
        getter_func: &str,
        extract_name: &str,
    ) -> Result<BasicValueEnum<'a>, String> {
        let getter = self
            .runtime_function(getter_func)
            .ok_or(format!("{getter_func} not found"))?;
        let raw = self
            .builder
            .build_call(getter, &[obj_value.into()], extract_name)
            .map_err(|e| e.to_string())?;
        raw.try_as_basic_value()
            .basic()
            .ok_or_else(|| format!("{getter_func} should return a basic value"))
    }

    fn free_raw_pointer(&self, raw_ptr: BasicValueEnum<'a>, free_func: &str) -> Result<(), String> {
        let free_fn = self
            .runtime_function(free_func)
            .ok_or(format!("{free_func} not found"))?;
        self.builder
            .build_call(free_fn, &[raw_ptr.into()], "free_raw")
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn with_extracted<F>(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        get_func: &str,
        free_func: &str,
        f: F,
    ) -> Result<BasicValueEnum<'a>, String>
    where
        F: FnOnce(&mut Self, BasicValueEnum<'a>) -> Result<BasicValueEnum<'a>, String>,
    {
        let raw = self.extract_raw_pointer(obj_value, get_func, "extract")?;
        let result = f(self, raw)?;
        self.free_raw_pointer(raw, free_func)?;
        Ok(result)
    }

    fn with_extracted_list<F>(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        f: F,
    ) -> Result<BasicValueEnum<'a>, String>
    where
        F: FnOnce(&mut Self, BasicValueEnum<'a>) -> Result<BasicValueEnum<'a>, String>,
    {
        self.with_extracted(obj_value, "mux_value_get_list", "mux_free_list", f)
    }

    fn with_extracted_map<F>(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        f: F,
    ) -> Result<BasicValueEnum<'a>, String>
    where
        F: FnOnce(&mut Self, BasicValueEnum<'a>) -> Result<BasicValueEnum<'a>, String>,
    {
        self.with_extracted(obj_value, "mux_value_get_map", "mux_free_map", f)
    }

    fn with_extracted_set<F>(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        f: F,
    ) -> Result<BasicValueEnum<'a>, String>
    where
        F: FnOnce(&mut Self, BasicValueEnum<'a>) -> Result<BasicValueEnum<'a>, String>,
    {
        self.with_extracted(obj_value, "mux_value_get_set", "mux_free_set", f)
    }

    /// Unwrap a Mux string Value to the raw C string the runtime's string
    /// helpers take. Same step `call_string_conversion_func` performs, split out
    /// for the helpers that need two operands rather than one.
    pub(super) fn string_value_to_cstr(
        &self,
        value: BasicValueEnum<'a>,
    ) -> Result<BasicValueEnum<'a>, String> {
        let func = self
            .runtime_function("mux_value_to_string")
            .ok_or("mux_value_to_string not found")?;
        self.builder
            .build_call(func, &[value.into()], "str_to_cstr")
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| "mux_value_to_string should return a basic value".to_string())
    }

    /// Free the owned C strings `string_value_to_cstr` handed back.
    ///
    /// `mux_value_to_string` returns an owned copy, so every operand unwrapped
    /// for a runtime string call has to be released or the call leaks one
    /// allocation per operand per invocation.
    pub(super) fn free_cstrings(&self, cstrs: &[BasicValueEnum<'a>]) -> Result<(), String> {
        let free_fn = self
            .runtime_function("mux_free_string")
            .ok_or("mux_free_string not found")?;
        for cstr in cstrs {
            self.builder
                .build_call(free_fn, &[(*cstr).into()], "free_cstr")
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn call_string_conversion_func(
        &self,
        obj_value: BasicValueEnum<'a>,
        conversion_func: &str,
    ) -> Result<BasicValueEnum<'a>, String> {
        let func_to_cstr = self
            .runtime_function("mux_value_to_string")
            .ok_or("mux_value_to_string not found")?;
        let cstr = self
            .builder
            .build_call(func_to_cstr, &[obj_value.into()], "str_to_cstr")
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .expect("mux_value_to_string should return a basic value");
        let func = self
            .runtime_function(conversion_func)
            .ok_or(format!("{conversion_func} not found"))?;
        let call = self
            .builder
            .build_call(func, &[cstr.into()], "str_conv")
            .map_err(|e| e.to_string())?;
        let result = call
            .try_as_basic_value()
            .basic()
            .unwrap_or_else(|| panic!("{conversion_func} should return a basic value"));
        // `mux_value_to_string` returned an owned C string that the conversion
        // only borrows; free it so string `.length()`/`.to_int()` etc. do not leak.
        let free_fn = self
            .runtime_function("mux_free_string")
            .ok_or("mux_free_string not found")?;
        self.builder
            .build_call(free_fn, &[cstr.into()], "free_cstr")
            .map_err(|e| e.to_string())?;
        Ok(result)
    }

    // Validate argument counts for methods
    fn ensure_arg_count(
        &self,
        method: &str,
        args: &[ExpressionNode],
        expected: usize,
    ) -> Result<(), String> {
        if args.len() != expected {
            Err(format!(
                "{method}() method takes exactly {expected} argument(s)"
            ))
        } else {
            Ok(())
        }
    }

    fn ensure_no_args(&self, method: &str, args: &[ExpressionNode]) -> Result<(), String> {
        if !args.is_empty() {
            Err(format!("{method}() method takes no arguments"))
        } else {
            Ok(())
        }
    }

    /// Invoke a class instance method (user-defined class).
    /// Centralizes the logic that chooses specialized method names, boxes args when
    /// calling specialized functions, issues the call and handles void/non-void returns.
    fn invoke_class_instance_method(
        &mut self,
        class_name: &str,
        type_args: &[Type],
        obj_value: BasicValueEnum<'a>,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        let class = self
            .analyzer
            .symbol_table()
            .lookup(class_name)
            .ok_or_else(|| format!("Class {class_name} not found"))?;

        let method = class
            .methods
            .get(method_name)
            .ok_or_else(|| format!("Method {method_name} not found on class {class_name}"))?;

        if method.is_static {
            return Err(format!(
                "Cannot call static method {method_name} on instance"
            ));
        }

        let method_func_name = self.resolve_method_func_name(class_name, type_args, method_name)?;
        let is_specialized = method_func_name.contains('$');

        let call_args = self.build_method_call_args(obj_value, args, is_specialized)?;

        let func = self
            .module
            .get_function(&method_func_name)
            .ok_or_else(|| format!("Method '{method_func_name}' not found"))?;

        let call = self
            .builder
            .build_call(
                func,
                &call_args,
                &format!("{}_call", method_func_name.replace('.', "_")),
            )
            .map_err(|e| e.to_string())?;

        self.handle_method_return_value(call, &method.return_type)
    }

    fn invoke_trait_object_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        target: &Type,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        let Type::Named(interface_name, interface_args) = target else {
            return Err("dynamic interface target must be a named interface".to_string());
        };
        if !interface_args.is_empty() {
            return Err("generic dynamic interface calls are not supported yet".to_string());
        }
        let symbol = self
            .analyzer
            .symbol_table()
            .lookup(interface_name)
            .ok_or_else(|| format!("Interface {interface_name} not found"))?;
        let (_, methods) = symbol
            .interfaces
            .get(interface_name)
            .ok_or_else(|| format!("Interface methods for {interface_name} not found"))?;
        let method = methods.get(method_name).ok_or_else(|| {
            format!("Method {method_name} not found on interface {interface_name}")
        })?;
        let mut method_names: Vec<&String> = methods.keys().collect();
        method_names.sort();
        let method_index = method_names
            .iter()
            .position(|name| *name == method_name)
            .ok_or_else(|| format!("Method {method_name} has no vtable slot"))?;

        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let layout = *self
            .trait_object_layouts
            .get(interface_name)
            .ok_or_else(|| format!("dynamic interface {interface_name} has no layout"))?;
        let data = self
            .builder
            .build_call(
                self.runtime_function("mux_get_object_ptr")
                    .ok_or("mux_get_object_ptr not found")?,
                &[obj_value.into_pointer_value().into()],
                "trait_object_data",
            )
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or("mux_get_object_ptr returned no value")?
            .into_pointer_value();
        let typed_data = self
            .builder
            .build_pointer_cast(
                data,
                self.context.ptr_type(AddressSpace::default()),
                "trait_object",
            )
            .map_err(|e| e.to_string())?;
        let object = self
            .builder
            .build_load(
                ptr_type,
                self.builder
                    .build_struct_gep(layout, typed_data, 0, "trait_object_object")
                    .map_err(|e| e.to_string())?,
                "trait_object_object_load",
            )
            .map_err(|e| e.to_string())?
            .into_pointer_value();
        let vtable = self
            .builder
            .build_load(
                ptr_type,
                self.builder
                    .build_struct_gep(layout, typed_data, 1, "trait_object_vtable")
                    .map_err(|e| e.to_string())?,
                "trait_object_vtable_load",
            )
            .map_err(|e| e.to_string())?
            .into_pointer_value();
        let vtable_type = *self
            .vtable_type_map
            .get(interface_name)
            .ok_or_else(|| format!("interface {interface_name} has no vtable type"))?;
        let method_slot = self
            .builder
            .build_struct_gep(
                vtable_type,
                vtable,
                super::llvm_index(method_index),
                method_name,
            )
            .map_err(|e| e.to_string())?;
        let method_ptr = self
            .builder
            .build_load(ptr_type, method_slot, "trait_method")
            .map_err(|e| e.to_string())?
            .into_pointer_value();

        let mut param_types = Vec::with_capacity(method.params.len() + 1);
        param_types.push(ptr_type.into());
        for param in &method.params {
            param_types.push(self.semantic_type_to_llvm(param)?.into());
        }
        let function_type = if matches!(method.return_type, Type::Void) {
            self.context.void_type().fn_type(&param_types, false)
        } else {
            self.semantic_type_to_llvm(&method.return_type)?
                .fn_type(&param_types, false)
        };
        let mut call_args = vec![object.into()];
        for arg in args {
            call_args.push(self.generate_expression(arg)?.into());
        }
        let call = self
            .builder
            .build_indirect_call(function_type, method_ptr, &call_args, "trait_method_call")
            .map_err(|e| e.to_string())?;
        self.handle_method_return_value(call, &method.return_type)
    }

    fn resolve_method_func_name(
        &mut self,
        class_name: &str,
        type_args: &[Type],
        method_name: &str,
    ) -> Result<String, String> {
        if type_args.is_empty()
            && let Some(current_fn) = &self.current_function_name
            && let Some((current_class_part, _)) = current_fn.split_once('.')
            && current_class_part.starts_with(&format!("{class_name}$"))
        {
            let contextual_name = format!("{current_class_part}.{method_name}");
            if self.module.get_function(&contextual_name).is_some() {
                return Ok(contextual_name);
            }
        }

        // Stamp out the instantiation before looking for it. Bodies are emitted
        // in source order, so a function reaching a generic class's method
        // before any construction site found no `Pair$int.describe` and fell
        // back to `Pair.describe` - a declaration that never gets a body, which
        // fails at link time rather than at the call. Generating on demand is
        // what the enum path and the generic static-method path already do, and
        // it is idempotent per instantiation.
        if !type_args.is_empty() {
            self.generate_specialized_methods(class_name, type_args)?;
        }

        let specialized_method_name =
            self.create_specialized_method_name(class_name, type_args, method_name);

        if self.module.get_function(&specialized_method_name).is_some() {
            Ok(specialized_method_name)
        } else {
            Ok(format!("{class_name}.{method_name}"))
        }
    }

    fn build_method_call_args(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        args: &[ExpressionNode],
        is_specialized: bool,
    ) -> Result<Vec<BasicMetadataValueEnum<'a>>, String> {
        let mut call_args: Vec<BasicMetadataValueEnum<'a>> = vec![obj_value.into()];

        for arg in args {
            let arg_val = self.generate_expression(arg)?;
            if is_specialized {
                call_args.push(self.box_value(arg_val).into());
            } else {
                call_args.push(arg_val.into());
            }
        }

        Ok(call_args)
    }

    fn handle_method_return_value(
        &self,
        call: inkwell::values::CallSiteValue<'a>,
        return_type: &Type,
    ) -> Result<BasicValueEnum<'a>, String> {
        if let Some(value) = call.try_as_basic_value().basic() {
            Ok(value)
        } else if *return_type == Type::Void {
            Ok(self.context.i32_type().const_int(0, false).into())
        } else {
            Err("Method call failed to return value".to_string())
        }
    }

    pub(super) fn build_net_call(
        &mut self,
        func_name: &str,
        args: &[BasicValueEnum<'a>],
    ) -> Result<BasicValueEnum<'a>, String> {
        let func = self
            .runtime_function(func_name)
            .ok_or(format!("{func_name} not found"))?;
        let metadata_args = args.iter().map(|v| (*v).into()).collect::<Vec<_>>();
        let call = self
            .builder
            .build_call(
                func,
                &metadata_args,
                &format!("{}_call", func_name.replace('.', "_")),
            )
            .map_err(|e| e.to_string())?;
        if let Some(value) = call.try_as_basic_value().basic() {
            Ok(value)
        } else {
            Ok(self.context.i32_type().const_int(0, false).into())
        }
    }

    pub(super) fn bool_to_i32(
        &mut self,
        value: BasicValueEnum<'a>,
    ) -> Result<BasicValueEnum<'a>, String> {
        let int_value = if value.is_int_value() {
            value.into_int_value()
        } else if value.is_pointer_value() {
            self.extract_raw_pointer(value, "mux_value_get_bool", "extract_bool")?
                .into_int_value()
        } else {
            return Err("Expected boolean argument".to_string());
        };
        let extended = self
            .builder
            .build_int_z_extend(int_value, self.context.i32_type(), "bool_to_i32")
            .map_err(|e| e.to_string())?;
        Ok(extended.into())
    }

    pub(super) fn try_generate_net_static_method_call(
        &mut self,
        class_name: &str,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        // small helper to generate single arg with validation
        fn gen_one_arg<'a>(
            s: &mut CodeGenerator<'a>,
            args: &[ExpressionNode],
        ) -> Result<BasicValueEnum<'a>, String> {
            if args.len() != 1 {
                return Err("method takes exactly 1 argument".to_string());
            }
            s.generate_expression(&args[0])
        }

        match (class_name, method_name) {
            ("TlsConfig", "new") => {
                self.ensure_no_args(method_name, args)?;
                self.build_net_call("mux_tls_config_new", &[]).map(Some)
            }
            ("Headers", "new") => {
                self.ensure_no_args(method_name, args)?;
                let call = self.call_runtime_function("mux_net_http_headers_new", &[])?;
                Ok(Some(call))
            }
            ("HttpRequest", "new") => {
                self.ensure_no_args(method_name, args)?;
                let call = self.call_runtime_function("mux_net_http_request_new", &[])?;
                Ok(Some(call))
            }
            ("HttpRequest", "from_config") => {
                if args.len() != 4 {
                    return Err("from_config() method takes exactly 4 arguments".to_string());
                }
                let args = args
                    .iter()
                    .map(|arg| self.generate_expression(arg))
                    .collect::<Result<Vec<_>, _>>()?;
                self.build_net_call("mux_net_http_request_from_config", &args)
                    .map(Some)
            }
            ("HttpRequest", "read") => {
                let stream = gen_one_expr(self, args)?;
                self.build_net_call("mux_net_http_request_read", &[stream])
                    .map(Some)
            }
            ("HttpServerConfig", "new") => {
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function("mux_net_http_server_config_new", &[])
                    .map(Some)
            }
            ("HttpResponse", "new") => {
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function("mux_net_http_response_new", &[])
                    .map(Some)
            }
            ("HttpRouter", "new") => {
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function("mux_net_http_router_new", &[])
                    .map(Some)
            }
            ("OAuthClient", "new") => {
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function("mux_net_oauth_client_new", &[])
                    .map(Some)
            }
            ("OAuthClient", "from_config") => {
                if args.len() != 4 {
                    return Err("from_config() method takes exactly 4 arguments".to_string());
                }
                let args = args
                    .iter()
                    .map(|arg| self.generate_expression(arg))
                    .collect::<Result<Vec<_>, _>>()?;
                self.build_net_call("mux_net_oauth_client_from_config", &args)
                    .map(Some)
            }
            ("OAuthSession", "from_token_response") => {
                let response = gen_one_expr(self, args)?;
                self.build_net_call("mux_net_oauth_session_from_token_response", &[response])
                    .map(Some)
            }
            ("SseEvent", "new") => {
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function("mux_net_sse_event_new", &[])
                    .map(Some)
            }
            ("SseEvent", "from_config") => {
                if args.len() != 4 {
                    return Err("from_config() method takes exactly 4 arguments".to_string());
                }
                let args = args
                    .iter()
                    .map(|arg| self.generate_expression(arg))
                    .collect::<Result<Vec<_>, _>>()?;
                self.build_net_call("mux_net_sse_event_from_config", &args)
                    .map(Some)
            }
            ("SseStream", "from_tcp") => {
                let stream = gen_one_expr(self, args)?;
                self.build_net_call("mux_net_sse_stream_from_tcp", &[stream])
                    .map(Some)
            }
            ("WebSocketFrame", "new") => {
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function("mux_net_websocket_frame_new", &[])
                    .map(Some)
            }
            ("WebSocketFrame", "from_config") => {
                if args.len() != 4 {
                    return Err("from_config() method takes exactly 4 arguments".to_string());
                }
                let fin_value = self.generate_expression(&args[0])?;
                let fin = self.bool_to_i32(fin_value)?;
                let opcode = self.generate_expression(&args[1])?;
                let payload = self.generate_expression(&args[2])?;
                let masked_value = self.generate_expression(&args[3])?;
                let masked = self.bool_to_i32(masked_value)?;
                self.build_net_call(
                    "mux_net_websocket_frame_from_config",
                    &[fin, opcode, payload, masked],
                )
                .map(Some)
            }
            ("WebSocketFrame", "decode") => {
                let bytes = gen_one_expr(self, args)?;
                self.build_net_call("mux_net_websocket_frame_decode", &[bytes])
                    .map(Some)
            }
            ("WebSocketFrame", "reassemble") => {
                let frames = gen_one_expr(self, args)?;
                self.build_net_call("mux_net_websocket_frame_reassemble", &[frames])
                    .map(Some)
            }
            ("WebSocketHandshake", "new") => {
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function("mux_net_websocket_handshake_new", &[])
                    .map(Some)
            }
            ("WebSocketHandshake", "from_config") => {
                if args.len() != 2 {
                    return Err("from_config() method takes exactly 2 arguments".to_string());
                }
                let key = self.generate_expression(&args[0])?;
                let protocol = self.generate_expression(&args[1])?;
                self.build_net_call("mux_net_websocket_handshake_from_config", &[key, protocol])
                    .map(Some)
            }
            ("WebSocketHandshake", "request_key") => {
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function("mux_net_websocket_handshake_request_key", &[])
                    .map(Some)
            }
            ("WebSocketSession", "from_tcp") => {
                let stream = gen_one_expr(self, args)?;
                self.build_net_call("mux_net_websocket_session_from_tcp", &[stream])
                    .map(Some)
            }
            ("HttpServer", "serve_once") => {
                if args.len() != 3 {
                    return Err("serve_once() method takes exactly 3 arguments".to_string());
                }
                let listener = self.generate_expression(&args[0])?;
                let config = self.generate_expression(&args[1])?;
                let handler = self.generate_expression(&args[2])?;
                self.build_net_call(
                    "mux_net_http_server_serve_once",
                    &[listener, config, handler],
                )
                .map(Some)
            }
            ("HttpServer", "serve") => {
                if args.len() != 4 {
                    return Err("serve() method takes exactly 4 arguments".to_string());
                }
                let listener = self.generate_expression(&args[0])?;
                let config = self.generate_expression(&args[1])?;
                let handler = self.generate_expression(&args[2])?;
                let max_requests = self.generate_expression(&args[3])?;
                self.build_net_call(
                    "mux_net_http_server_serve",
                    &[listener, config, handler, max_requests],
                )
                .map(Some)
            }
            ("HttpServer", "serve_until_cancelled") => {
                if args.len() != 4 {
                    return Err(
                        "serve_until_cancelled() method takes exactly 4 arguments".to_string()
                    );
                }
                let listener = self.generate_expression(&args[0])?;
                let config = self.generate_expression(&args[1])?;
                let handler = self.generate_expression(&args[2])?;
                let cancellation = self.generate_expression(&args[3])?;
                self.build_net_call(
                    "mux_net_http_server_serve_until_cancelled",
                    &[listener, config, handler, cancellation],
                )
                .map(Some)
            }
            ("HttpResponse", "from_config") => {
                if args.len() != 3 {
                    return Err("from_config() method takes exactly 3 arguments".to_string());
                }
                let args = args
                    .iter()
                    .map(|arg| self.generate_expression(arg))
                    .collect::<Result<Vec<_>, _>>()?;
                self.build_net_call("mux_net_http_response_from_config", &args)
                    .map(Some)
            }
            ("HttpError", "from_message") => {
                let message = gen_one_arg(self, args)?;
                self.build_net_call("mux_http_error_from_message", &[message])
                    .map(Some)
            }
            ("EnvError", "from_message") => {
                let message = gen_one_arg(self, args)?;
                self.build_net_call("mux_env_error_from_message", &[message])
                    .map(Some)
            }
            ("NetError", "from_message") => {
                let message = gen_one_arg(self, args)?;
                self.build_net_call("mux_net_error_from_message", &[message])
                    .map(Some)
            }
            ("FsError", "from_message") => {
                let message = gen_one_arg(self, args)?;
                self.build_net_call("mux_fs_error_from_message", &[message])
                    .map(Some)
            }
            ("TcpListener", "bind") => {
                let addr = gen_one_arg(self, args)?;
                let call = self.build_net_call("mux_net_tcp_listener_bind", &[addr])?;
                Ok(Some(call))
            }
            ("TcpStream", "connect") => {
                let addr = gen_one_arg(self, args)?;
                let call = self.build_net_call("mux_net_tcp_connect", &[addr])?;
                Ok(Some(call))
            }
            ("LocalListener", "bind") => {
                let path = gen_one_arg(self, args)?;
                let call = self.build_net_call("mux_net_local_listener_bind", &[path])?;
                Ok(Some(call))
            }
            ("LocalStream", "connect") => {
                let path = gen_one_arg(self, args)?;
                let call = self.build_net_call("mux_net_local_connect", &[path])?;
                Ok(Some(call))
            }
            ("UdpSocket", "bind") => {
                let addr = gen_one_arg(self, args)?;
                let call = self.build_net_call("mux_net_udp_bind", &[addr])?;
                Ok(Some(call))
            }
            ("Poller", "new") => {
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function("mux_poller_new", &[]).map(Some)
            }
            ("IpAddr", "parse") => {
                let value = gen_one_arg(self, args)?;
                self.build_net_call("mux_net_ip_parse", &[value]).map(Some)
            }
            ("SocketAddr", "parse") => {
                let value = gen_one_arg(self, args)?;
                self.build_net_call("mux_net_socket_addr_parse", &[value])
                    .map(Some)
            }
            ("SocketAddr", "resolve") => {
                if args.len() != 2 {
                    return Err("resolve() method takes exactly 2 arguments".to_string());
                }
                let values = args
                    .iter()
                    .map(|arg| self.generate_expression(arg))
                    .collect::<Result<Vec<_>, _>>()?;
                self.build_net_call("mux_net_socket_addr_resolve", &values)
                    .map(Some)
            }
            ("Cidr", "parse") => {
                let value = gen_one_arg(self, args)?;
                self.build_net_call("mux_net_cidr_parse", &[value])
                    .map(Some)
            }
            ("Endpoint", "from_host") => {
                if args.len() != 2 {
                    return Err("from_host() method takes exactly 2 arguments".to_string());
                }
                let values = args
                    .iter()
                    .map(|arg| self.generate_expression(arg))
                    .collect::<Result<Vec<_>, _>>()?;
                self.build_net_call("mux_net_endpoint_from_host", &values)
                    .map(Some)
            }
            ("Endpoint", "from_socket_addr") => {
                let value = gen_one_arg(self, args)?;
                self.build_net_call("mux_net_endpoint_from_socket_addr", &[value])
                    .map(Some)
            }
            _ => Ok(None),
        }
    }

    pub(super) fn try_generate_tls_static_method_call(
        &mut self,
        class_name: &str,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if class_name == "TlsError" && method_name == "from_message" {
            let message = gen_one_expr(self, args)?;
            return self
                .build_net_call("mux_tls_error_from_message", &[message])
                .map(Some);
        }
        if class_name != "TlsStream" {
            return Ok(None);
        }
        match method_name {
            "connect" => {
                let (server_name, address) = gen_two_expr(self, args)?;
                self.build_net_call("mux_tls_connect", &[server_name, address])
                    .map(Some)
            }
            "connect_with_roots" => {
                if args.len() != 3 {
                    return Err("connect_with_roots() method takes exactly 3 arguments".to_string());
                }
                let values = args
                    .iter()
                    .map(|arg| self.generate_expression(arg))
                    .collect::<Result<Vec<_>, _>>()?;
                self.build_net_call("mux_tls_connect_with_roots", &values)
                    .map(Some)
            }
            "connect_with_config" => {
                if args.len() != 3 {
                    return Err(
                        "connect_with_config() method takes exactly 3 arguments".to_string()
                    );
                }
                let values = args
                    .iter()
                    .map(|arg| self.generate_expression(arg))
                    .collect::<Result<Vec<_>, _>>()?;
                self.build_net_call("mux_tls_connect_with_config", &values)
                    .map(Some)
            }
            "connect_with_roots_config" => {
                if args.len() != 4 {
                    return Err(
                        "connect_with_roots_config() method takes exactly 4 arguments".to_string(),
                    );
                }
                let values = args
                    .iter()
                    .map(|arg| self.generate_expression(arg))
                    .collect::<Result<Vec<_>, _>>()?;
                self.build_net_call("mux_tls_connect_with_roots_config", &values)
                    .map(Some)
            }
            "accept" => {
                if args.len() != 3 {
                    return Err("accept() method takes exactly 3 arguments".to_string());
                }
                let values = args
                    .iter()
                    .map(|arg| self.generate_expression(arg))
                    .collect::<Result<Vec<_>, _>>()?;
                self.build_net_call("mux_tls_accept", &values).map(Some)
            }
            "accept_with_config" => {
                if args.len() != 4 {
                    return Err("accept_with_config() method takes exactly 4 arguments".to_string());
                }
                let values = args
                    .iter()
                    .map(|arg| self.generate_expression(arg))
                    .collect::<Result<Vec<_>, _>>()?;
                self.build_net_call("mux_tls_accept_with_config", &values)
                    .map(Some)
            }
            "connect_with_client_cert" => {
                if args.len() != 5 {
                    return Err(
                        "connect_with_client_cert() method takes exactly 5 arguments".to_string(),
                    );
                }
                let values = args
                    .iter()
                    .map(|arg| self.generate_expression(arg))
                    .collect::<Result<Vec<_>, _>>()?;
                self.build_net_call("mux_tls_connect_with_client_cert", &values)
                    .map(Some)
            }
            "connect_with_client_cert_config" => {
                if args.len() != 6 {
                    return Err(
                        "connect_with_client_cert_config() method takes exactly 6 arguments"
                            .to_string(),
                    );
                }
                let values = args
                    .iter()
                    .map(|arg| self.generate_expression(arg))
                    .collect::<Result<Vec<_>, _>>()?;
                self.build_net_call("mux_tls_connect_with_client_cert_config", &values)
                    .map(Some)
            }
            _ => Ok(None),
        }
    }

    pub(super) fn try_generate_tls_config_instance_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        obj_type: &Type,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if !matches!(obj_type, Type::Named(name, _) if name == "TlsConfig") {
            return Ok(None);
        }
        let runtime_name = match method_name {
            "set_protocols" => "mux_tls_config_set_protocols",
            "set_cipher_suites" => "mux_tls_config_set_cipher_suites",
            "set_alpn_protocols" => "mux_tls_config_set_alpn_protocols",
            _ => return Ok(None),
        };
        let values = args
            .iter()
            .map(|arg| self.generate_expression(arg))
            .collect::<Result<Vec<_>, _>>()?;
        let expected = match method_name {
            "set_protocols" => 2,
            _ => 1,
        };
        if values.len() != expected {
            return Err(format!(
                "{method_name}() method takes exactly {expected} argument{}",
                if expected == 1 { "" } else { "s" }
            ));
        }
        let mut call_args = vec![obj_value];
        call_args.extend(values);
        self.build_net_call(runtime_name, &call_args).map(Some)
    }

    pub(super) fn try_generate_tls_instance_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        obj_type: &Type,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if matches!(obj_type, Type::Named(name, _) if name == "TlsError") {
            let runtime_name = match method_name {
                "message" => "mux_tls_error_message",
                "to_string" => "mux_tls_error_to_string",
                _ => return Ok(None),
            };
            self.ensure_no_args(method_name, args)?;
            return self.build_net_call(runtime_name, &[obj_value]).map(Some);
        }
        if !matches!(obj_type, Type::Named(name, _) if name == "TlsStream") {
            return Ok(None);
        }
        let runtime_name = match method_name {
            "read" => "mux_tls_read",
            "write" => "mux_tls_write",
            "flush" => "mux_tls_flush",
            "shutdown" => "mux_tls_shutdown",
            "peer_certificates" => "mux_tls_peer_certificates",
            "protocol_version" => "mux_tls_protocol_version",
            "cipher_suite" => "mux_tls_cipher_suite",
            "alpn_protocol" => "mux_tls_alpn_protocol",
            _ => return Ok(None),
        };
        let expected = usize::from(matches!(method_name, "read" | "write"));
        if args.len() != expected {
            return Err(format!(
                "{method_name}() method takes exactly {expected} argument{}",
                if expected == 1 { "" } else { "s" }
            ));
        }
        let mut values = vec![obj_value];
        values.extend(
            args.iter()
                .map(|arg| self.generate_expression(arg))
                .collect::<Result<Vec<_>, _>>()?,
        );
        self.build_net_call(runtime_name, &values).map(Some)
    }

    pub(super) fn try_generate_datetime_static_method_call(
        &mut self,
        class_name: &str,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if class_name == "DateTimeError" && method_name == "from_message" {
            let message = gen_one_expr(self, args)?;
            return self
                .build_net_call("mux_datetime_error_from_message", &[message])
                .map(Some);
        }
        let runtime_name = match (class_name, method_name) {
            ("Date", "new") => "mux_datetime_date_new",
            ("Date", "from_parts") => "mux_datetime_date_from_parts",
            ("Date", "parse") => "mux_datetime_date_parse",
            ("Time", "new") => "mux_datetime_time_new",
            ("Time", "from_parts") => "mux_datetime_time_from_parts",
            ("Time", "parse") => "mux_datetime_time_parse",
            ("DateTime", "new") => "mux_datetime_datetime_new",
            ("DateTime", "now") => "mux_datetime_datetime_now",
            ("DateTime", "from_timestamp") => "mux_datetime_datetime_from_timestamp",
            ("DateTime", "from_date_time") => "mux_datetime_datetime_from_date_time",
            ("DateTime", "parse_pattern") => "mux_datetime_datetime_parse_pattern",
            ("ZonedDateTime", "new") => "mux_datetime_zoned_datetime_new",
            ("ZonedDateTime", "from_local") => "mux_datetime_zoned_datetime_from_local",
            ("ZonedDateTime", "resolve_local") => "mux_datetime_zoned_datetime_resolve_local",
            ("ZonedDateTime", "from_instant") => "mux_datetime_zoned_datetime_from_instant",
            ("ZonedDateTime", "parse") => "mux_datetime_zoned_datetime_parse",
            ("Instant", "new") => "mux_datetime_instant_new",
            ("Instant", "now") => "mux_datetime_instant_now",
            ("Instant", "from_unix_nanos") => "mux_datetime_instant_from_unix_nanos",
            ("Duration", "new") => "mux_datetime_duration_new",
            ("Duration", "from_parts") => "mux_datetime_duration_from_parts",
            ("Duration", "from_seconds") => "mux_datetime_duration_from_seconds",
            ("Duration", "from_millis") => "mux_datetime_duration_from_millis",
            ("Duration", "from_micros") => "mux_datetime_duration_from_micros",
            ("Duration", "from_nanos") => "mux_datetime_duration_from_nanos",
            ("Period", "new") => "mux_datetime_period_new",
            ("Period", "from_parts") => "mux_datetime_period_from_parts",
            _ => return Ok(None),
        };
        let expected = match (class_name, method_name) {
            ("Date" | "Time" | "Duration" | "Period", "new")
            | ("DateTime" | "Instant", "new" | "now") => 0,
            ("Duration", "from_parts") | ("DateTime", "from_timestamp" | "from_date_time") => 2,
            ("Date" | "Period", "from_parts") => 3,
            ("Time", "from_parts") => 4,
            ("DateTime", "parse_pattern") => 2,
            ("ZonedDateTime", "from_local" | "resolve_local") => 3,
            ("ZonedDateTime", "from_instant") => 2,
            ("ZonedDateTime", "parse") => 1,
            ("Date" | "Time" | "DateTime", "parse")
            | ("Instant", "from_unix_nanos")
            | ("Duration", "from_seconds" | "from_millis" | "from_micros" | "from_nanos") => 1,
            _ => 0,
        };
        if args.len() != expected {
            return Err(format!(
                "{method_name}() method takes exactly {expected} argument{}",
                if expected == 1 { "" } else { "s" }
            ));
        }
        if expected == 0 {
            return self.call_runtime_function(runtime_name, &[]).map(Some);
        }
        let values = args
            .iter()
            .map(|arg| self.generate_expression(arg))
            .collect::<Result<Vec<_>, _>>()?;
        self.build_net_call(runtime_name, &values).map(Some)
    }

    pub(super) fn try_generate_datetime_instance_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        obj_type: &Type,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if matches!(obj_type, Type::Named(name, _) if name == "DateTimeError") {
            let runtime_name = match method_name {
                "message" => "mux_datetime_error_message",
                "to_string" => "mux_datetime_error_to_string",
                _ => return Ok(None),
            };
            self.ensure_no_args(method_name, args)?;
            return self.build_net_call(runtime_name, &[obj_value]).map(Some);
        }
        let Type::Named(class_name, _) = obj_type else {
            return Ok(None);
        };
        let runtime_name = match (class_name.as_str(), method_name) {
            ("Date", "to_string") => "mux_datetime_date_to_string",
            ("Date", "year") => "mux_datetime_date_year",
            ("Date", "month") => "mux_datetime_date_month",
            ("Date", "day") => "mux_datetime_date_day",
            ("Date", "weekday") => "mux_datetime_date_weekday",
            ("Date", "format") => "mux_datetime_date_format",
            ("Date", "add_days") => "mux_datetime_date_add_days",
            ("Time", "to_string") => "mux_datetime_time_to_string",
            ("Time", "hour") => "mux_datetime_time_hour",
            ("Time", "minute") => "mux_datetime_time_minute",
            ("Time", "second") => "mux_datetime_time_second",
            ("Time", "nanosecond") => "mux_datetime_time_nanosecond",
            ("Time", "format") => "mux_datetime_time_format",
            ("DateTime", "to_string") => "mux_datetime_datetime_to_string",
            ("DateTime", "format") => "mux_datetime_datetime_format",
            ("DateTime", "unix_seconds") => "mux_datetime_datetime_unix_seconds",
            ("DateTime", "unix_nanos") => "mux_datetime_datetime_unix_nanos",
            ("DateTime", "date") => "mux_datetime_datetime_date",
            ("DateTime", "time") => "mux_datetime_datetime_time",
            ("DateTime", "add_duration") => "mux_datetime_datetime_add_duration",
            ("ZonedDateTime", "to_string") => "mux_datetime_zoned_datetime_to_string",
            ("ZonedDateTime", "zone") => "mux_datetime_zoned_datetime_zone",
            ("ZonedDateTime", "offset_seconds") => "mux_datetime_zoned_datetime_offset_seconds",
            ("ZonedDateTime", "date") => "mux_datetime_zoned_datetime_date",
            ("ZonedDateTime", "time") => "mux_datetime_zoned_datetime_time",
            ("ZonedDateTime", "instant") => "mux_datetime_zoned_datetime_instant",
            ("ZonedDateTime", "add_duration") => "mux_datetime_zoned_datetime_add_duration",
            ("LocalResolution", "kind") => "mux_datetime_local_resolution_kind",
            ("LocalResolution", "earlier") => "mux_datetime_local_resolution_earlier",
            ("LocalResolution", "later") => "mux_datetime_local_resolution_later",
            ("Instant", "unix_nanos") => "mux_datetime_instant_unix_nanos",
            ("Instant", "add_duration") => "mux_datetime_instant_add_duration",
            ("Instant", "duration_since") => "mux_datetime_instant_duration_since",
            ("Duration", "to_nanos") => "mux_datetime_duration_to_nanos",
            ("Duration", "add") => "mux_datetime_duration_add",
            ("Duration", "sub") => "mux_datetime_duration_sub",
            ("Period", "years") => "mux_datetime_period_years",
            ("Period", "months") => "mux_datetime_period_months",
            ("Period", "days") => "mux_datetime_period_days",
            ("Period", "add_to_date") => "mux_datetime_period_add_to_date",
            _ => return Ok(None),
        };
        let expected = match (class_name.as_str(), method_name) {
            ("Date" | "Time" | "DateTime", "format")
            | ("Date", "add_days")
            | ("DateTime" | "Instant" | "ZonedDateTime", "add_duration")
            | ("Instant", "duration_since")
            | ("Duration", "add" | "sub")
            | ("Period", "add_to_date") => 1,
            _ => 0,
        };
        if args.len() != expected {
            return Err(format!(
                "{method_name}() method takes exactly {expected} argument{}",
                if expected == 1 { "" } else { "s" }
            ));
        }
        let mut values = Vec::with_capacity(expected + 1);
        values.push(obj_value);
        values.extend(
            args.iter()
                .map(|arg| self.generate_expression(arg))
                .collect::<Result<Vec<_>, _>>()?,
        );
        self.build_net_call(runtime_name, &values).map(Some)
    }

    pub(super) fn try_generate_process_static_method_call(
        &mut self,
        class_name: &str,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if class_name == "ProcessError" && method_name == "from_message" {
            let message = gen_one_expr(self, args)?;
            return self
                .build_net_call("mux_process_error_from_message", &[message])
                .map(Some);
        }
        if class_name == "ProcessPool" {
            let runtime_name = match method_name {
                "new" => {
                    self.ensure_no_args(method_name, args)?;
                    return self
                        .call_runtime_function("mux_process_pool_new", &[])
                        .map(Some);
                }
                "with_config" => "mux_process_pool_with_config",
                _ => return Ok(None),
            };
            if args.len() != 2 {
                return Err(format!("{method_name}() method takes exactly 2 arguments"));
            }
            let values = args
                .iter()
                .map(|arg| self.generate_expression(arg))
                .collect::<Result<Vec<_>, _>>()?;
            return self.build_net_call(runtime_name, &values).map(Some);
        }
        if class_name != "Command" || !matches!(method_name, "new" | "shell") {
            return Ok(None);
        }
        if method_name == "new" {
            self.ensure_no_args(method_name, args)?;
            return self
                .call_runtime_function("mux_process_command_new", &[])
                .map(Some);
        }
        let commandline = gen_one_expr(self, args)?;
        let commandline_cstr = self.string_value_to_cstr(commandline)?;
        let call = self.build_net_call("mux_process_command_shell", &[commandline_cstr])?;
        self.free_cstrings(&[commandline_cstr])?;
        Ok(Some(call))
    }

    pub(super) fn try_generate_log_static_method_call(
        &mut self,
        class_name: &str,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if class_name == "LogError" && method_name == "from_message" {
            let message = gen_one_expr(self, args)?;
            return self
                .build_net_call("mux_log_error_from_message", &[message])
                .map(Some);
        }
        if class_name != "Logger" {
            return Ok(None);
        }
        match method_name {
            "new" => {
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function("mux_log_logger_new", &[])
                    .map(Some)
            }
            "default" => {
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function("mux_log_default", &[]).map(Some)
            }
            "set_default" => {
                let logger = gen_one_expr(self, args)?;
                self.build_net_call("mux_log_set_default", &[logger])
                    .map(Some)
            }
            _ => Ok(None),
        }
    }

    pub(super) fn try_generate_log_instance_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        obj_type: &Type,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if matches!(obj_type, Type::Named(name, _) if name == "LogError") {
            let runtime_name = match method_name {
                "message" => "mux_log_error_message",
                "to_string" => "mux_log_error_to_string",
                _ => return Ok(None),
            };
            self.ensure_no_args(method_name, args)?;
            return self.build_net_call(runtime_name, &[obj_value]).map(Some);
        }
        if !matches!(obj_type, Type::Named(name, _) if name == "Logger") {
            return Ok(None);
        }
        let runtime_name = match method_name {
            "set_writer" => "mux_log_logger_set_writer",
            "set_level" => "mux_log_logger_set_level",
            "set_name" => "mux_log_logger_set_name",
            "field" => "mux_log_logger_field",
            "trace" => "mux_log_logger_trace",
            "debug" => "mux_log_logger_debug",
            "info" => "mux_log_logger_info",
            "warn" => "mux_log_logger_warn",
            "error" => "mux_log_logger_error",
            _ => return Ok(None),
        };
        let expected = match method_name {
            "field" => 2,
            _ => 1,
        };
        if args.len() != expected {
            return Err(format!(
                "{method_name}() method takes exactly {expected} argument{}",
                if expected == 1 { "" } else { "s" }
            ));
        }
        let mut values = Vec::with_capacity(args.len() + 1);
        values.push(obj_value);
        for arg in args {
            values.push(self.generate_expression(arg)?);
        }
        self.build_net_call(runtime_name, &values).map(Some)
    }

    pub(super) fn try_generate_random_static_method_call(
        &mut self,
        class_name: &str,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if class_name == "RandomError" && method_name == "from_message" {
            let message = gen_one_expr(self, args)?;
            return self
                .build_net_call("mux_random_error_from_message", &[message])
                .map(Some);
        }
        if class_name != "Random" {
            return Ok(None);
        }
        match method_name {
            "seeded" => {
                let seed = gen_one_expr(self, args)?;
                self.call_runtime_function("mux_random_seeded", &[seed])
                    .map(Some)
            }
            "system" => {
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function("mux_random_system", &[])
                    .map(Some)
            }
            _ => Ok(None),
        }
    }

    pub(super) fn try_generate_crypto_static_method_call(
        &mut self,
        class_name: &str,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if class_name != "CryptoError" || method_name != "from_message" {
            return Ok(None);
        }
        let message = gen_one_expr(self, args)?;
        self.build_net_call("mux_crypto_error_from_message", &[message])
            .map(Some)
    }

    pub(super) fn try_generate_crypto_instance_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        obj_type: &Type,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if !matches!(obj_type, Type::Named(name, _) if name == "CryptoError") {
            return Ok(None);
        }
        let runtime_name = match method_name {
            "message" => "mux_crypto_error_message",
            "to_string" => "mux_crypto_error_to_string",
            _ => return Ok(None),
        };
        self.ensure_no_args(method_name, args)?;
        self.build_net_call(runtime_name, &[obj_value]).map(Some)
    }

    pub(super) fn try_generate_math_static_method_call(
        &mut self,
        class_name: &str,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if class_name != "MathError" || method_name != "from_message" {
            return Ok(None);
        }
        let message = gen_one_expr(self, args)?;
        self.build_net_call("mux_math_error_from_message", &[message])
            .map(Some)
    }

    pub(super) fn try_generate_math_instance_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        obj_type: &Type,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if !matches!(obj_type, Type::Named(name, _) if name == "MathError") {
            return Ok(None);
        }
        let runtime_name = match method_name {
            "message" => "mux_math_error_message",
            "to_string" => "mux_math_error_to_string",
            _ => return Ok(None),
        };
        self.ensure_no_args(method_name, args)?;
        self.build_net_call(runtime_name, &[obj_value]).map(Some)
    }

    pub(super) fn try_generate_random_instance_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        obj_type: &Type,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if matches!(obj_type, Type::Named(name, _) if name == "RandomError") {
            let runtime_name = match method_name {
                "message" => "mux_random_error_message",
                "to_string" => "mux_random_error_to_string",
                _ => return Ok(None),
            };
            self.ensure_no_args(method_name, args)?;
            return self.build_net_call(runtime_name, &[obj_value]).map(Some);
        }
        if !matches!(obj_type, Type::Named(name, _) if name == "Random") {
            return Ok(None);
        }
        let runtime_name = match method_name {
            "next_int" => "mux_random_next_int",
            "next_range" => "mux_random_next_range",
            "next_float" => "mux_random_next_float",
            "next_bool" => "mux_random_next_bool",
            "bytes" => "mux_random_bytes",
            "normal" => "mux_random_normal",
            "exponential" => "mux_random_exponential",
            _ => return Ok(None),
        };
        let expected = match method_name {
            "next_range" => 2,
            "bytes" => 1,
            "normal" => 2,
            "exponential" => 1,
            _ => 0,
        };
        if args.len() != expected {
            return Err(format!(
                "{method_name}() method takes exactly {expected} argument{}",
                if expected == 1 { "" } else { "s" }
            ));
        }
        let mut values = Vec::with_capacity(args.len() + 1);
        values.push(obj_value);
        for arg in args {
            values.push(self.generate_expression(arg)?);
        }
        if matches!(method_name, "next_int" | "next_float" | "next_bool") {
            self.call_runtime_function(runtime_name, &values).map(Some)
        } else {
            self.build_net_call(runtime_name, &values).map(Some)
        }
    }

    pub(super) fn try_generate_io_static_method_call(
        &mut self,
        class_name: &str,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        match (class_name, method_name) {
            ("IoError", "from_message") => {
                let message = gen_one_expr(self, args)?;
                self.build_net_call("mux_io_error_from_message", &[message])
                    .map(Some)
            }
            ("Path", "new") => {
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function("mux_fs_path_new", &[]).map(Some)
            }
            ("Path", "from_string") => {
                let value = gen_one_expr(self, args)?;
                self.build_net_call("mux_fs_path_from_string", &[value])
                    .map(Some)
            }
            ("Reader", "new") => {
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function("mux_io_reader_new", &[])
                    .map(Some)
            }
            ("Reader", "from_bytes") => {
                let bytes = gen_one_expr(self, args)?;
                self.build_net_call("mux_io_reader_from_bytes", &[bytes])
                    .map(Some)
            }
            ("Reader", "from_file") => {
                let path = gen_one_expr(self, args)?;
                self.build_net_call("mux_io_reader_from_file", &[path])
                    .map(Some)
            }
            ("Reader", "from_tcp") => {
                let stream = gen_one_expr(self, args)?;
                self.build_net_call("mux_io_reader_from_tcp", &[stream])
                    .map(Some)
            }
            ("Writer", "new") => {
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function("mux_io_writer_new", &[])
                    .map(Some)
            }
            ("Writer", "to_file") => {
                let path = gen_one_expr(self, args)?;
                self.build_net_call("mux_io_writer_to_file", &[path])
                    .map(Some)
            }
            ("Writer", "append_file") => {
                let path = gen_one_expr(self, args)?;
                self.build_net_call("mux_io_writer_append_file", &[path])
                    .map(Some)
            }
            ("Writer", "from_tcp") => {
                let stream = gen_one_expr(self, args)?;
                self.build_net_call("mux_io_writer_from_tcp", &[stream])
                    .map(Some)
            }
            ("Stream", "from_reader") => {
                let reader = gen_one_expr(self, args)?;
                self.build_net_call("mux_io_stream_from_reader", &[reader])
                    .map(Some)
            }
            ("Stream", "from_writer") => {
                let writer = gen_one_expr(self, args)?;
                self.build_net_call("mux_io_stream_from_writer", &[writer])
                    .map(Some)
            }
            ("Directory", "open") => {
                let path = gen_one_expr(self, args)?;
                self.build_net_call("mux_fs_directory_open", &[path])
                    .map(Some)
            }
            _ => Ok(None),
        }
    }

    pub(super) fn try_generate_csv_static_method_call(
        &mut self,
        class_name: &str,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if method_name == "from_message"
            && matches!(
                class_name,
                "JsonError" | "CsvError" | "ByteError" | "BytesError"
            )
        {
            let message = gen_one_expr(self, args)?;
            let runtime_name = match class_name {
                "JsonError" => "mux_json_error_from_message",
                "CsvError" => "mux_csv_error_from_message",
                "BytesError" => "mux_bytes_error_from_message",
                _ => "mux_byte_error_from_message",
            };
            return self.build_net_call(runtime_name, &[message]).map(Some);
        }
        let (runtime_name, expected) = match (class_name, method_name) {
            ("CsvReader", "new") => ("mux_csv_reader_new", 0),
            ("CsvReader", "from_bytes") => ("mux_csv_reader_from_bytes", 2),
            ("CsvReader", "from_reader") => ("mux_csv_reader_from_reader", 2),
            ("CsvWriter", "new") => ("mux_csv_writer_new", 0),
            ("CsvWriter", "from_config") => ("mux_csv_writer_from_config", 2),
            ("CsvWriter", "from_writer") => ("mux_csv_writer_from_writer", 3),
            _ => return Ok(None),
        };
        if args.len() != expected {
            return Err(format!(
                "{method_name}() method takes exactly {expected} argument{}",
                if expected == 1 { "" } else { "s" }
            ));
        }
        if expected == 0 {
            return self.call_runtime_function(runtime_name, &[]).map(Some);
        }
        let values = args
            .iter()
            .map(|arg| self.generate_expression(arg))
            .collect::<Result<Vec<_>, _>>()?;
        self.build_net_call(runtime_name, &values).map(Some)
    }

    pub(super) fn try_generate_csv_instance_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        obj_type: &Type,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let Type::Named(class_name, _) = obj_type else {
            return Ok(None);
        };
        let runtime_name = match (class_name.as_str(), method_name) {
            ("JsonError", "message") => "mux_json_error_message",
            ("JsonError", "to_string") => "mux_json_error_to_string",
            ("CsvError", "message") => "mux_csv_error_message",
            ("CsvError", "to_string") => "mux_csv_error_to_string",
            ("ByteError", "message") => "mux_byte_error_message",
            ("ByteError", "to_string") => "mux_byte_error_to_string",
            ("BytesError", "message") => "mux_bytes_error_message",
            ("BytesError", "to_string") => "mux_bytes_error_to_string",
            ("CsvReader", "headers") => "mux_csv_reader_headers",
            ("CsvReader", "read") => "mux_csv_reader_read",
            ("CsvWriter", "write") => "mux_csv_writer_write",
            ("CsvWriter", "flush") => "mux_csv_writer_flush",
            ("CsvWriter", "bytes") => "mux_csv_writer_bytes",
            _ => return Ok(None),
        };
        let expected = usize::from(matches!(
            (class_name.as_str(), method_name),
            ("CsvWriter", "write")
        ));
        if args.len() != expected {
            return Err(format!(
                "{method_name}() method takes exactly {expected} argument{}",
                if expected == 1 { "" } else { "s" }
            ));
        }
        let mut values = Vec::with_capacity(expected + 1);
        values.push(obj_value);
        values.extend(
            args.iter()
                .map(|arg| self.generate_expression(arg))
                .collect::<Result<Vec<_>, _>>()?,
        );
        self.build_net_call(runtime_name, &values).map(Some)
    }

    pub(super) fn try_generate_json_token_instance_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        obj_type: &Type,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let Type::Named(class_name, _) = obj_type else {
            return Ok(None);
        };
        let runtime_name = match (class_name.as_str(), method_name) {
            ("JsonTokenReader", "next") => "mux_json_token_reader_next",
            ("JsonTokenReader", "close") => "mux_json_token_reader_close",
            ("JsonToken", "kind") => "mux_json_token_kind",
            ("JsonToken", "text") => "mux_json_token_text",
            ("JsonToken", "value") => "mux_json_token_value",
            _ => return Ok(None),
        };
        self.ensure_no_args(method_name, args)?;
        self.build_net_call(runtime_name, &[obj_value]).map(Some)
    }

    pub(super) fn try_generate_cli_static_method_call(
        &mut self,
        class_name: &str,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if class_name == "CliError" && method_name == "from_message" {
            let message = gen_one_expr(self, args)?;
            return self
                .build_net_call("mux_cli_error_from_message", &[message])
                .map(Some);
        }
        if class_name != "CliParser" || method_name != "new" {
            return Ok(None);
        }
        self.ensure_no_args(method_name, args)?;
        self.call_runtime_function("mux_cli_parser_new", &[])
            .map(Some)
    }

    pub(super) fn try_generate_cli_instance_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        obj_type: &Type,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let Type::Named(class_name, _) = obj_type else {
            return Ok(None);
        };
        if class_name == "CliError" {
            let runtime_name = match method_name {
                "message" => "mux_cli_error_message",
                "to_string" => "mux_cli_error_to_string",
                _ => return Ok(None),
            };
            self.ensure_no_args(method_name, args)?;
            return self.build_net_call(runtime_name, &[obj_value]).map(Some);
        }
        let runtime_name = match (class_name.as_str(), method_name) {
            ("CliParser", "set_program") => "mux_cli_parser_set_program",
            ("CliParser", "set_about") => "mux_cli_parser_set_about",
            ("CliParser", "set_version") => "mux_cli_parser_set_version",
            ("CliParser", "set_response_files") => "mux_cli_parser_set_response_files",
            ("CliParser", "add_option") => "mux_cli_parser_add_option",
            ("CliParser", "set_option_env") => "mux_cli_parser_set_option_env",
            ("CliParser", "set_option_default") => "mux_cli_parser_set_option_default",
            ("CliParser", "set_option_multiple") => "mux_cli_parser_set_option_multiple",
            ("CliParser", "set_option_conflicts") => "mux_cli_parser_set_option_conflicts",
            ("CliParser", "set_option_requires") => "mux_cli_parser_set_option_requires",
            ("CliParser", "set_option_alias") => "mux_cli_parser_set_option_alias",
            ("CliParser", "set_option_group") => "mux_cli_parser_set_option_group",
            ("CliParser", "set_option_parser") => "mux_cli_parser_set_option_parser",
            ("CliParser", "add_positional") => "mux_cli_parser_add_positional",
            ("CliParser", "add_subcommand") => "mux_cli_parser_add_subcommand",
            ("CliParser", "parse") => "mux_cli_parser_parse",
            ("CliParser", "parse_process") => "mux_cli_parser_parse_process",
            ("CliParser", "parse_or_exit") => "mux_cli_parser_parse_or_exit",
            ("CliParser", "help") => "mux_cli_parser_help",
            ("CliParser", "completion") => "mux_cli_parser_completion",
            ("CliParser", "manpage") => "mux_cli_parser_manpage",
            ("CliMatches", "has") => "mux_cli_matches_has",
            ("CliMatches", "get") => "mux_cli_matches_get",
            ("CliMatches", "get_int") => "mux_cli_matches_get_int",
            ("CliMatches", "get_float") => "mux_cli_matches_get_float",
            ("CliMatches", "get_bool") => "mux_cli_matches_get_bool",
            ("CliMatches", "values") => "mux_cli_matches_values",
            ("CliMatches", "positional") => "mux_cli_matches_positional",
            ("CliMatches", "subcommand") => "mux_cli_matches_subcommand",
            ("CliMatches", "subcommand_matches") => "mux_cli_matches_subcommand_matches",
            ("CliMatches", "help") => "mux_cli_matches_help",
            _ => return Ok(None),
        };
        let expected = match (class_name.as_str(), method_name) {
            ("CliParser", "add_option") => 4,
            ("CliParser", "set_option_env" | "set_option_default") => 2,
            (
                "CliParser",
                "set_option_multiple"
                | "set_option_conflicts"
                | "set_option_requires"
                | "set_option_alias"
                | "set_option_group"
                | "set_option_parser",
            ) => 2,
            ("CliParser", "add_positional") => 2,
            ("CliParser", "add_subcommand") => 2,
            ("CliParser", "parse") => 1,
            (
                "CliParser",
                "set_program" | "set_about" | "set_version" | "set_response_files" | "completion",
            ) => 1,
            (
                "CliMatches",
                "has" | "get" | "get_int" | "get_float" | "get_bool" | "values" | "positional",
            ) => 1,
            _ => 0,
        };
        if args.len() != expected {
            return Err(format!(
                "{method_name}() method takes exactly {expected} argument{}",
                if expected == 1 { "" } else { "s" }
            ));
        }
        let mut values = Vec::with_capacity(expected + 1);
        values.push(obj_value);
        values.extend(
            args.iter()
                .map(|arg| self.generate_expression(arg))
                .collect::<Result<Vec<_>, _>>()?,
        );
        self.build_net_call(runtime_name, &values).map(Some)
    }

    pub(super) fn try_generate_io_instance_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        obj_type: &Type,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let class_name = match obj_type {
            Type::Named(name, _)
                if matches!(
                    name.as_str(),
                    "IoError" | "Reader" | "Writer" | "Stream" | "Path" | "Directory"
                ) =>
            {
                name.as_str()
            }
            _ => return Ok(None),
        };
        if class_name == "IoError" {
            let runtime_name = match method_name {
                "message" => "mux_io_error_message",
                "to_string" => "mux_io_error_to_string",
                _ => return Ok(None),
            };
            self.ensure_no_args(method_name, args)?;
            return self.build_net_call(runtime_name, &[obj_value]).map(Some);
        }
        let runtime_name = match (class_name, method_name) {
            ("Path", "to_string") => "mux_fs_path_to_string",
            ("Path", "display") => "mux_fs_path_display",
            ("Path", "is_absolute") => "mux_fs_path_is_absolute",
            ("Path", "is_relative") => "mux_fs_path_is_relative",
            ("Path", "parent") => "mux_fs_path_parent",
            ("Path", "file_name") => "mux_fs_path_file_name",
            ("Path", "extension") => "mux_fs_path_extension",
            ("Path", "stem") => "mux_fs_path_stem",
            ("Path", "join") => "mux_fs_path_join",
            ("Path", "with_file_name") => "mux_fs_path_with_file_name",
            ("Path", "with_extension") => "mux_fs_path_with_extension",
            ("Reader", "read") => "mux_io_reader_read",
            ("Reader", "read_line") => "mux_io_reader_read_line",
            ("Reader", "limit") => "mux_io_reader_limit",
            ("Reader", "tee") => "mux_io_reader_tee",
            ("Reader", "untee") => "mux_io_reader_untee",
            ("Reader", "read_exact") => "mux_io_reader_read_exact",
            ("Reader", "position") => "mux_io_reader_position",
            ("Reader", "remaining") => "mux_io_reader_remaining",
            ("Reader", "read_to_end") => "mux_io_reader_read_to_end",
            ("Reader", "copy_to") => "mux_io_reader_copy_to",
            ("Reader", "seek") => "mux_io_reader_seek",
            ("Reader", "close") => "mux_io_reader_close",
            ("Writer", "write") => "mux_io_writer_write",
            ("Writer", "write_all") => "mux_io_writer_write_all",
            ("Writer", "bytes") => "mux_io_writer_bytes",
            ("Writer", "position") => "mux_io_writer_position",
            ("Writer", "seek") => "mux_io_writer_seek",
            ("Writer", "flush") => "mux_io_writer_flush",
            ("Writer", "close") => "mux_io_writer_close",
            ("Stream", "read") => "mux_io_stream_read",
            ("Stream", "write") => "mux_io_stream_write",
            ("Stream", "flush") => "mux_io_stream_flush",
            ("Stream", "seek") => "mux_io_stream_seek",
            ("Stream", "close") => "mux_io_stream_close",
            ("Directory", "next") => "mux_fs_directory_next",
            ("Directory", "close") => "mux_fs_directory_close",
            _ => return Ok(None),
        };
        let expected = match method_name {
            "join" | "with_file_name" | "with_extension" => 1,
            "read" | "read_exact" | "read_to_end" | "seek" | "write" | "write_all" => 1,
            "copy_to" => 2,
            _ => 0,
        };
        if args.len() != expected {
            return Err(format!(
                "{method_name}() method takes exactly {expected} argument{}",
                if expected == 1 { "" } else { "s" }
            ));
        }
        let mut values = Vec::with_capacity(args.len() + 1);
        values.push(obj_value);
        for arg in args {
            values.push(self.generate_expression(arg)?);
        }
        self.build_net_call(runtime_name, &values).map(Some)
    }

    pub(super) fn try_generate_regex_static_method_call(
        &mut self,
        class_name: &str,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if class_name == "RegexError" && method_name == "from_message" {
            let message = gen_one_expr(self, args)?;
            return self
                .build_net_call("mux_regex_error_from_message", &[message])
                .map(Some);
        }
        if class_name != "Regex" {
            return Ok(None);
        }
        match method_name {
            "new" => {
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function("mux_regex_new", &[]).map(Some)
            }
            "from_pattern" | "escape" => {
                let arg = gen_one_expr(self, args)?;
                let runtime_name = if method_name == "from_pattern" {
                    "mux_regex_from_pattern"
                } else {
                    "mux_regex_escape"
                };
                self.build_net_call(runtime_name, &[arg]).map(Some)
            }
            "from_pattern_with_flags" => {
                let (pattern, flags) = gen_two_expr(self, args)?;
                self.build_net_call("mux_regex_from_pattern_with_flags", &[pattern, flags])
                    .map(Some)
            }
            _ => Ok(None),
        }
    }

    pub(super) fn try_generate_uuid_static_method_call(
        &mut self,
        class_name: &str,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if class_name == "UuidError" && method_name == "from_message" {
            let message = gen_one_expr(self, args)?;
            return self
                .build_net_call("mux_uuid_error_from_message", &[message])
                .map(Some);
        }
        if class_name != "Uuid" {
            return Ok(None);
        }
        match method_name {
            "from_bytes" | "v8" => {
                let arg = gen_one_expr(self, args)?;
                let runtime_name = if method_name == "from_bytes" {
                    "mux_uuid_from_bytes"
                } else {
                    "mux_uuid_v8"
                };
                self.build_net_call(runtime_name, &[arg]).map(Some)
            }
            "v3" | "v5" => {
                let (namespace, name) = gen_two_expr(self, args)?;
                let runtime_name = if method_name == "v3" {
                    "mux_uuid_v3"
                } else {
                    "mux_uuid_v5"
                };
                self.build_net_call(runtime_name, &[namespace, name])
                    .map(Some)
            }
            "from_parts" => {
                let (high, low) = gen_two_expr(self, args)?;
                self.build_net_call("mux_uuid_from_parts", &[high, low])
                    .map(Some)
            }
            "nil" | "max" | "v1" | "v4" | "v6" | "v7" => {
                self.ensure_no_args(method_name, args)?;
                let runtime_name = match method_name {
                    "nil" => "mux_uuid_nil",
                    "max" => "mux_uuid_max",
                    "v1" => "mux_uuid_v1",
                    "v4" => "mux_uuid_v4",
                    "v6" => "mux_uuid_v6",
                    "v7" => "mux_uuid_v7",
                    _ => unreachable!(),
                };
                self.build_net_call(runtime_name, &[]).map(Some)
            }
            _ => Ok(None),
        }
    }

    pub(super) fn try_generate_url_static_method_call(
        &mut self,
        class_name: &str,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if class_name == "UrlError" && method_name == "from_message" {
            let message = gen_one_expr(self, args)?;
            return self
                .build_net_call("mux_url_error_from_message", &[message])
                .map(Some);
        }
        if class_name != "Url" {
            return Ok(None);
        }
        match method_name {
            "parse" | "from_file" => {
                let arg = gen_one_expr(self, args)?;
                let runtime_name = if method_name == "parse" {
                    "mux_url_parse"
                } else {
                    "mux_url_from_file"
                };
                self.build_net_call(runtime_name, &[arg]).map(Some)
            }
            _ => Ok(None),
        }
    }

    pub(super) fn try_generate_net_instance_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        obj_type: &Type,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let Type::Named(type_name, _) = obj_type else {
            return Ok(None);
        };
        match type_name.as_str() {
            "HttpError" | "EnvError" | "FsError" | "NetError" => {
                self.generate_net_error_method(obj_value, type_name, method_name, args)
            }
            "Headers" | "HttpRequest" | "HttpResponse" | "HttpRouter" => {
                self.generate_net_http_method(obj_value, type_name, method_name, args)
            }
            "OAuthClient" | "OAuthSession" => {
                self.generate_net_oauth_method(obj_value, type_name, method_name, args)
            }
            "HttpNext" | "SseEvent" | "SseStream" | "WebSocketFrame" | "WebSocketHandshake"
            | "WebSocketSession" => {
                self.generate_net_realtime_method(obj_value, type_name, method_name, args)
            }
            "Poller" | "PollEvent" => {
                self.generate_net_poller_method(obj_value, type_name, method_name, args)
            }
            "IpAddr" | "SocketAddr" | "Cidr" | "Endpoint" => {
                self.generate_net_address_method(obj_value, type_name, method_name, args)
            }
            "TcpStream" | "TcpListener" | "LocalStream" | "LocalListener" => {
                self.generate_net_stream_method(obj_value, type_name, method_name, args)
            }
            "UdpSocket" | "UdpDatagram" => {
                self.generate_net_udp_method(obj_value, type_name, method_name, args)
            }
            _ => Ok(None),
        }
    }

    fn generate_net_error_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        type_name: &str,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        match type_name {
            "HttpError" => match method_name {
                "message" => {
                    self.ensure_no_args(method_name, args)?;
                    self.build_net_call("mux_http_error_message", &[obj_value])
                        .map(Some)
                }
                "to_string" => {
                    self.ensure_no_args(method_name, args)?;
                    self.build_net_call("mux_http_error_to_string", &[obj_value])
                        .map(Some)
                }
                _ => Ok(None),
            },
            "EnvError" => match method_name {
                "message" => {
                    self.ensure_no_args(method_name, args)?;
                    self.build_net_call("mux_env_error_message", &[obj_value])
                        .map(Some)
                }
                "to_string" => {
                    self.ensure_no_args(method_name, args)?;
                    self.build_net_call("mux_env_error_to_string", &[obj_value])
                        .map(Some)
                }
                _ => Ok(None),
            },
            "FsError" => match method_name {
                "message" => {
                    self.ensure_no_args(method_name, args)?;
                    self.build_net_call("mux_fs_error_message", &[obj_value])
                        .map(Some)
                }
                "to_string" => {
                    self.ensure_no_args(method_name, args)?;
                    self.build_net_call("mux_fs_error_to_string", &[obj_value])
                        .map(Some)
                }
                _ => Ok(None),
            },
            "NetError" => match method_name {
                "message" => {
                    self.ensure_no_args(method_name, args)?;
                    self.build_net_call("mux_net_error_message", &[obj_value])
                        .map(Some)
                }
                "to_string" => {
                    self.ensure_no_args(method_name, args)?;
                    self.build_net_call("mux_net_error_to_string", &[obj_value])
                        .map(Some)
                }
                _ => Ok(None),
            },
            _ => Ok(None),
        }
    }

    fn generate_net_http_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        type_name: &str,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        match type_name {
            "Headers" => match method_name {
                "set" | "append" => {
                    let (name, value) = gen_two_expr(self, args)?;
                    let runtime_name = if method_name == "set" {
                        "mux_net_http_headers_set"
                    } else {
                        "mux_net_http_headers_append"
                    };
                    self.build_net_call(runtime_name, &[obj_value, name, value])
                        .map(Some)
                }
                "get" | "values" | "remove" => {
                    let name = gen_one_expr(self, args)?;
                    let runtime_name = match method_name {
                        "get" => "mux_net_http_headers_get",
                        "values" => "mux_net_http_headers_values",
                        "remove" => "mux_net_http_headers_remove",
                        _ => unreachable!(),
                    };
                    self.build_net_call(runtime_name, &[obj_value, name])
                        .map(Some)
                }
                _ => Ok(None),
            },
            "HttpRequest" => match method_name {
                "send" => {
                    self.ensure_no_args(method_name, args)?;
                    self.build_net_call("mux_net_http_request_send", &[obj_value])
                        .map(Some)
                }
                "path_param" => {
                    let name = gen_one_expr(self, args)?;
                    self.build_net_call("mux_net_http_request_path_param", &[obj_value, name])
                        .map(Some)
                }
                "set_body_reader" => {
                    let reader = gen_one_expr(self, args)?;
                    self.build_net_call(
                        "mux_net_http_request_set_body_reader",
                        &[obj_value, reader],
                    )
                    .map(Some)
                }
                _ => Ok(None),
            },
            "HttpResponse" => match method_name {
                "write" => {
                    let stream = gen_one_expr(self, args)?;
                    self.build_net_call("mux_net_http_response_write", &[stream, obj_value])
                        .map(Some)
                }
                "error_for_status" => {
                    self.ensure_no_args(method_name, args)?;
                    let runtime_name = match method_name {
                        "error_for_status" => "mux_net_http_response_error_for_status",
                        _ => unreachable!(),
                    };
                    self.build_net_call(runtime_name, &[obj_value]).map(Some)
                }
                "read_bytes" | "read_text" | "read_json" => {
                    let limit = gen_one_expr(self, args)?;
                    let runtime_name = match method_name {
                        "read_bytes" => "mux_net_http_response_read_bytes",
                        "read_text" => "mux_net_http_response_read_text",
                        "read_json" => "mux_net_http_response_read_json",
                        _ => unreachable!(),
                    };
                    self.build_net_call(runtime_name, &[obj_value, limit])
                        .map(Some)
                }
                "reader" => {
                    let limit = gen_one_expr(self, args)?;
                    self.build_net_call("mux_net_http_response_reader", &[obj_value, limit])
                        .map(Some)
                }
                "save" => {
                    let path = gen_one_expr(self, args)?;
                    self.build_net_call("mux_net_http_response_save", &[obj_value, path])
                        .map(Some)
                }
                _ => Ok(None),
            },
            "HttpRouter" => match method_name {
                "route" => {
                    if args.len() != 3 {
                        return Err("route() method takes exactly 3 arguments".to_string());
                    }
                    let method = self.generate_expression(&args[0])?;
                    let path = self.generate_expression(&args[1])?;
                    let handler = self.generate_expression(&args[2])?;
                    self.build_net_call(
                        "mux_net_http_router_route",
                        &[obj_value, method, path, handler],
                    )
                    .map(Some)
                }
                "middleware" => {
                    let middleware = gen_one_expr(self, args)?;
                    self.build_net_call("mux_net_http_router_use", &[obj_value, middleware])
                        .map(Some)
                }
                "basic_auth" => {
                    if args.len() != 2 {
                        return Err("basic_auth() method takes exactly 2 arguments".to_string());
                    }
                    let username = self.generate_expression(&args[0])?;
                    let password = self.generate_expression(&args[1])?;
                    self.build_net_call(
                        "mux_net_http_router_basic_auth",
                        &[obj_value, username, password],
                    )
                    .map(Some)
                }
                "bearer_auth" => {
                    let token = gen_one_expr(self, args)?;
                    self.build_net_call("mux_net_http_router_bearer_auth", &[obj_value, token])
                        .map(Some)
                }
                "oauth_oidc" => {
                    if args.len() != 3 {
                        return Err("oauth_oidc() method takes exactly 3 arguments".to_string());
                    }
                    let issuer = self.generate_expression(&args[0])?;
                    let audience = self.generate_expression(&args[1])?;
                    let jwks_url = self.generate_expression(&args[2])?;
                    self.build_net_call(
                        "mux_net_http_router_oauth_oidc",
                        &[obj_value, issuer, audience, jwks_url],
                    )
                    .map(Some)
                }
                "handle" => {
                    let request = gen_one_expr(self, args)?;
                    self.build_net_call("mux_net_http_router_handle", &[obj_value, request])
                        .map(Some)
                }
                _ => Ok(None),
            },
            _ => Ok(None),
        }
    }

    fn generate_net_oauth_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        type_name: &str,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        match type_name {
            "OAuthClient" => match method_name {
                "discover" => {
                    self.ensure_no_args(method_name, args)?;
                    self.build_net_call("mux_net_oauth_client_discover", &[obj_value])
                        .map(Some)
                }
                "authorization_url" => {
                    if args.len() != 3 {
                        return Err(
                            "authorization_url() method takes exactly 3 arguments".to_string()
                        );
                    }
                    let mut values = vec![obj_value];
                    values.extend(
                        args.iter()
                            .map(|arg| self.generate_expression(arg))
                            .collect::<Result<Vec<_>, _>>()?,
                    );
                    self.build_net_call("mux_net_oauth_client_authorization_url", &values)
                        .map(Some)
                }
                "exchange_code" => {
                    if args.len() != 2 {
                        return Err("exchange_code() method takes exactly 2 arguments".to_string());
                    }
                    let mut values = vec![obj_value];
                    values.extend(
                        args.iter()
                            .map(|arg| self.generate_expression(arg))
                            .collect::<Result<Vec<_>, _>>()?,
                    );
                    self.build_net_call("mux_net_oauth_client_exchange_code", &values)
                        .map(Some)
                }
                "refresh" => {
                    let token = gen_one_expr(self, args)?;
                    self.build_net_call("mux_net_oauth_client_refresh", &[obj_value, token])
                        .map(Some)
                }
                "revoke" => {
                    if args.len() != 2 {
                        return Err("revoke() method takes exactly 2 arguments".to_string());
                    }
                    let mut values = vec![obj_value];
                    values.extend(
                        args.iter()
                            .map(|arg| self.generate_expression(arg))
                            .collect::<Result<Vec<_>, _>>()?,
                    );
                    self.build_net_call("mux_net_oauth_client_revoke", &values)
                        .map(Some)
                }
                "introspect" => {
                    let token = gen_one_expr(self, args)?;
                    self.build_net_call("mux_net_oauth_client_introspect", &[obj_value, token])
                        .map(Some)
                }
                _ => Ok(None),
            },
            "OAuthSession" => match method_name {
                "access_token" => {
                    self.ensure_no_args(method_name, args)?;
                    self.build_net_call("mux_net_oauth_session_access_token", &[obj_value])
                        .map(Some)
                }
                "refresh_token" => {
                    self.ensure_no_args(method_name, args)?;
                    self.build_net_call("mux_net_oauth_session_refresh_token", &[obj_value])
                        .map(Some)
                }
                "id_token" => {
                    self.ensure_no_args(method_name, args)?;
                    self.build_net_call("mux_net_oauth_session_id_token", &[obj_value])
                        .map(Some)
                }
                "token_type" => {
                    self.ensure_no_args(method_name, args)?;
                    self.build_net_call("mux_net_oauth_session_token_type", &[obj_value])
                        .map(Some)
                }
                "is_expired" => {
                    self.ensure_no_args(method_name, args)?;
                    self.build_net_call("mux_net_oauth_session_is_expired", &[obj_value])
                        .map(Some)
                }
                "refresh" => {
                    let client = gen_one_expr(self, args)?;
                    self.build_net_call("mux_net_oauth_session_refresh", &[obj_value, client])
                        .map(Some)
                }
                "revoke" => {
                    if args.len() != 2 {
                        return Err("revoke() method takes exactly 2 arguments".to_string());
                    }
                    let mut values = vec![obj_value];
                    values.extend(
                        args.iter()
                            .map(|arg| self.generate_expression(arg))
                            .collect::<Result<Vec<_>, _>>()?,
                    );
                    self.build_net_call("mux_net_oauth_session_revoke", &values)
                        .map(Some)
                }
                "introspect" => {
                    let client = gen_one_expr(self, args)?;
                    self.build_net_call("mux_net_oauth_session_introspect", &[obj_value, client])
                        .map(Some)
                }
                "close" => {
                    self.ensure_no_args(method_name, args)?;
                    self.build_net_call("mux_net_oauth_session_close", &[obj_value])
                        .map(Some)
                }
                _ => Ok(None),
            },
            _ => Ok(None),
        }
    }

    fn generate_net_realtime_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        type_name: &str,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        match type_name {
            "HttpNext" => match method_name {
                "handle" => {
                    let request = gen_one_expr(self, args)?;
                    self.build_net_call("mux_net_http_next_handle", &[obj_value, request])
                        .map(Some)
                }
                _ => Ok(None),
            },
            "SseEvent" => match method_name {
                "encode" => {
                    self.ensure_no_args(method_name, args)?;
                    self.build_net_call("mux_net_sse_event_encode", &[obj_value])
                        .map(Some)
                }
                _ => Ok(None),
            },
            "SseStream" => match method_name {
                "send" => {
                    let event = gen_one_expr(self, args)?;
                    self.build_net_call("mux_net_sse_stream_send", &[obj_value, event])
                        .map(Some)
                }
                "flush" => {
                    self.ensure_no_args(method_name, args)?;
                    self.build_net_call("mux_net_sse_stream_flush", &[obj_value])
                        .map(Some)
                }
                "close" => {
                    self.ensure_no_args(method_name, args)?;
                    self.build_net_call("mux_net_sse_stream_close", &[obj_value])
                        .map(Some)
                }
                _ => Ok(None),
            },
            "WebSocketFrame" => match method_name {
                "encode" => {
                    self.ensure_no_args(method_name, args)?;
                    self.build_net_call("mux_net_websocket_frame_encode", &[obj_value])
                        .map(Some)
                }
                _ => Ok(None),
            },
            "WebSocketHandshake" => match method_name {
                "accept_key" => {
                    self.ensure_no_args(method_name, args)?;
                    self.build_net_call("mux_net_websocket_handshake_accept_key", &[obj_value])
                        .map(Some)
                }
                "response_headers" => {
                    self.ensure_no_args(method_name, args)?;
                    self.build_net_call(
                        "mux_net_websocket_handshake_response_headers",
                        &[obj_value],
                    )
                    .map(Some)
                }
                _ => Ok(None),
            },
            "WebSocketSession" => match method_name {
                "receive" => {
                    self.ensure_no_args(method_name, args)?;
                    self.build_net_call("mux_net_websocket_session_receive", &[obj_value])
                        .map(Some)
                }
                "send" => {
                    let frame = gen_one_expr(self, args)?;
                    self.build_net_call("mux_net_websocket_session_send", &[obj_value, frame])
                        .map(Some)
                }
                "close" => {
                    self.ensure_no_args(method_name, args)?;
                    self.build_net_call("mux_net_websocket_session_close", &[obj_value])
                        .map(Some)
                }
                _ => Ok(None),
            },
            _ => Ok(None),
        }
    }

    fn generate_net_poller_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        type_name: &str,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        match type_name {
            "Poller" => match method_name {
                "register_tcp" => {
                    if args.len() != 3 {
                        return Err("register_tcp() method takes exactly 3 arguments".to_string());
                    }
                    let stream = self.generate_expression(&args[0])?;
                    let readable_value = self.generate_expression(&args[1])?;
                    let readable = self.bool_to_i32(readable_value)?;
                    let writable_value = self.generate_expression(&args[2])?;
                    let writable = self.bool_to_i32(writable_value)?;
                    self.build_net_call(
                        "mux_poller_register_tcp",
                        &[obj_value, stream, readable, writable],
                    )
                    .map(Some)
                }
                "register_listener" => {
                    let listener = gen_one_expr(self, args)?;
                    self.build_net_call("mux_poller_register_listener", &[obj_value, listener])
                        .map(Some)
                }
                "register_udp" => {
                    if args.len() != 3 {
                        return Err("register_udp() method takes exactly 3 arguments".to_string());
                    }
                    let socket = self.generate_expression(&args[0])?;
                    let readable_value = self.generate_expression(&args[1])?;
                    let readable = self.bool_to_i32(readable_value)?;
                    let writable_value = self.generate_expression(&args[2])?;
                    let writable = self.bool_to_i32(writable_value)?;
                    self.build_net_call(
                        "mux_poller_register_udp",
                        &[obj_value, socket, readable, writable],
                    )
                    .map(Some)
                }
                "deregister" | "poll" => {
                    let value = gen_one_expr(self, args)?;
                    let runtime_name = if method_name == "deregister" {
                        "mux_poller_deregister"
                    } else {
                        "mux_poller_poll"
                    };
                    self.build_net_call(runtime_name, &[obj_value, value])
                        .map(Some)
                }
                _ => Ok(None),
            },
            "PollEvent" => match method_name {
                "token" | "readable" | "writable" | "error" | "closed" => {
                    self.ensure_no_args(method_name, args)?;
                    let runtime_name = format!(
                        "mux_poll_event_{}",
                        if method_name == "token" {
                            "token"
                        } else if method_name == "readable" {
                            "readable"
                        } else if method_name == "writable" {
                            "writable"
                        } else if method_name == "error" {
                            "error"
                        } else {
                            "closed"
                        }
                    );
                    self.build_net_call(&runtime_name, &[obj_value]).map(Some)
                }
                _ => Ok(None),
            },
            _ => Ok(None),
        }
    }

    fn generate_net_address_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        type_name: &str,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        match type_name {
            "IpAddr" => {
                let runtime_name = match method_name {
                    "to_string" => "mux_net_ip_to_string",
                    "is_v4" => "mux_net_ip_is_v4",
                    "is_v6" => "mux_net_ip_is_v6",
                    "octets" => "mux_net_ip_octets",
                    _ => return Ok(None),
                };
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function(runtime_name, &[obj_value])
                    .map(Some)
            }
            "SocketAddr" => {
                let runtime_name = match method_name {
                    "to_string" => "mux_net_socket_to_string",
                    "port" => "mux_net_socket_port",
                    "ip" => "mux_net_socket_ip",
                    "with_port" => "mux_net_socket_with_port",
                    _ => return Ok(None),
                };
                if method_name == "with_port" {
                    let port = gen_one_expr(self, args)?;
                    self.build_net_call(runtime_name, &[obj_value, port])
                        .map(Some)
                } else {
                    self.ensure_no_args(method_name, args)?;
                    self.call_runtime_function(runtime_name, &[obj_value])
                        .map(Some)
                }
            }
            "Cidr" => {
                let runtime_name = match method_name {
                    "to_string" => "mux_net_cidr_to_string",
                    "prefix_len" => "mux_net_cidr_prefix",
                    "network" => "mux_net_cidr_network",
                    "contains" => "mux_net_cidr_contains",
                    _ => return Ok(None),
                };
                if method_name == "contains" {
                    let ip = gen_one_expr(self, args)?;
                    self.call_runtime_function(runtime_name, &[obj_value, ip])
                        .map(Some)
                } else {
                    self.ensure_no_args(method_name, args)?;
                    self.call_runtime_function(runtime_name, &[obj_value])
                        .map(Some)
                }
            }
            "Endpoint" => {
                let runtime_name = match method_name {
                    "to_string" => "mux_net_endpoint_to_string",
                    "host" => "mux_net_endpoint_host",
                    "port" => "mux_net_endpoint_port",
                    "resolve" => "mux_net_endpoint_resolve",
                    _ => return Ok(None),
                };
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function(runtime_name, &[obj_value])
                    .map(Some)
            }
            _ => Ok(None),
        }
    }

    fn generate_net_stream_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        type_name: &str,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        match type_name {
            "TcpStream" => match method_name {
                "read" => {
                    let size = gen_one_expr(self, args)?;
                    let call = self.build_net_call("mux_net_tcp_read", &[obj_value, size])?;
                    Ok(Some(call))
                }
                "write" => {
                    let data = gen_one_expr(self, args)?;
                    let call = self.build_net_call("mux_net_tcp_write", &[obj_value, data])?;
                    Ok(Some(call))
                }
                "close" => {
                    self.ensure_no_args("close", args)?;
                    let call = self.build_net_call("mux_net_tcp_close", &[obj_value])?;
                    Ok(Some(call))
                }
                "set_nonblocking" => {
                    let bool_val = gen_one_expr(self, args)?;
                    let converted = self.bool_to_i32(bool_val)?;
                    let call = self
                        .build_net_call("mux_net_tcp_set_nonblocking", &[obj_value, converted])?;
                    Ok(Some(call))
                }
                "set_read_timeout" | "set_write_timeout" => {
                    let timeout = gen_one_expr(self, args)?;
                    let runtime_name = if method_name == "set_read_timeout" {
                        "mux_net_tcp_set_read_timeout"
                    } else {
                        "mux_net_tcp_set_write_timeout"
                    };
                    let call = self.build_net_call(runtime_name, &[obj_value, timeout])?;
                    Ok(Some(call))
                }
                "shutdown_read" | "shutdown_write" => {
                    self.ensure_no_args(method_name, args)?;
                    let runtime_name = if method_name == "shutdown_read" {
                        "mux_net_tcp_shutdown_read"
                    } else {
                        "mux_net_tcp_shutdown_write"
                    };
                    self.build_net_call(runtime_name, &[obj_value]).map(Some)
                }
                "set_nodelay" => {
                    let enabled = gen_one_expr(self, args)?;
                    let converted = self.bool_to_i32(enabled)?;
                    self.build_net_call("mux_net_tcp_set_nodelay", &[obj_value, converted])
                        .map(Some)
                }
                "nodelay" => {
                    self.ensure_no_args("nodelay", args)?;
                    self.build_net_call("mux_net_tcp_nodelay", &[obj_value])
                        .map(Some)
                }
                "set_keepalive" => {
                    let enabled = gen_one_expr(self, args)?;
                    let converted = self.bool_to_i32(enabled)?;
                    self.build_net_call("mux_net_tcp_set_keepalive", &[obj_value, converted])
                        .map(Some)
                }
                "keepalive" => {
                    self.ensure_no_args("keepalive", args)?;
                    self.build_net_call("mux_net_tcp_keepalive", &[obj_value])
                        .map(Some)
                }
                "set_ttl" => {
                    let ttl = gen_one_expr(self, args)?;
                    self.build_net_call("mux_net_tcp_set_ttl", &[obj_value, ttl])
                        .map(Some)
                }
                "ttl" => {
                    self.ensure_no_args("ttl", args)?;
                    self.build_net_call("mux_net_tcp_ttl", &[obj_value])
                        .map(Some)
                }
                "peer_addr" => {
                    self.ensure_no_args("peer_addr", args)?;
                    let call = self.build_net_call("mux_net_tcp_peer_addr", &[obj_value])?;
                    Ok(Some(call))
                }
                "local_addr" => {
                    self.ensure_no_args("local_addr", args)?;
                    let call = self.build_net_call("mux_net_tcp_local_addr", &[obj_value])?;
                    Ok(Some(call))
                }
                "set_recv_buffer_size" | "set_send_buffer_size" => {
                    let size = gen_one_expr(self, args)?;
                    let runtime_name = if method_name == "set_recv_buffer_size" {
                        "mux_net_tcp_set_recv_buffer_size"
                    } else {
                        "mux_net_tcp_set_send_buffer_size"
                    };
                    self.build_net_call(runtime_name, &[obj_value, size])
                        .map(Some)
                }
                "recv_buffer_size" | "send_buffer_size" => {
                    self.ensure_no_args(method_name, args)?;
                    let runtime_name = if method_name == "recv_buffer_size" {
                        "mux_net_tcp_recv_buffer_size"
                    } else {
                        "mux_net_tcp_send_buffer_size"
                    };
                    self.build_net_call(runtime_name, &[obj_value]).map(Some)
                }
                _ => Ok(None),
            },
            "TcpListener" => match method_name {
                "accept" => {
                    self.ensure_no_args("accept", args)?;
                    let call = self.build_net_call("mux_net_tcp_listener_accept", &[obj_value])?;
                    Ok(Some(call))
                }
                "close" => {
                    self.ensure_no_args("close", args)?;
                    let call = self.build_net_call("mux_net_tcp_listener_close", &[obj_value])?;
                    Ok(Some(call))
                }
                "set_nonblocking" => {
                    let bool_val = gen_one_expr(self, args)?;
                    let converted = self.bool_to_i32(bool_val)?;
                    let call = self.build_net_call(
                        "mux_net_tcp_listener_set_nonblocking",
                        &[obj_value, converted],
                    )?;
                    Ok(Some(call))
                }
                "local_addr" => {
                    self.ensure_no_args("local_addr", args)?;
                    let call =
                        self.build_net_call("mux_net_tcp_listener_local_addr", &[obj_value])?;
                    Ok(Some(call))
                }
                _ => Ok(None),
            },
            "LocalStream" => match method_name {
                "read" => {
                    let size = gen_one_expr(self, args)?;
                    self.build_net_call("mux_net_local_read", &[obj_value, size])
                        .map(Some)
                }
                "write" => {
                    let data = gen_one_expr(self, args)?;
                    self.build_net_call("mux_net_local_write", &[obj_value, data])
                        .map(Some)
                }
                "set_read_timeout" | "set_write_timeout" => {
                    let timeout = gen_one_expr(self, args)?;
                    let runtime_name = if method_name == "set_read_timeout" {
                        "mux_net_local_set_read_timeout"
                    } else {
                        "mux_net_local_set_write_timeout"
                    };
                    self.build_net_call(runtime_name, &[obj_value, timeout])
                        .map(Some)
                }
                "set_nonblocking" => {
                    let enabled = gen_one_expr(self, args)?;
                    let converted = self.bool_to_i32(enabled)?;
                    self.build_net_call("mux_net_local_set_nonblocking", &[obj_value, converted])
                        .map(Some)
                }
                "shutdown_read" | "shutdown_write" => {
                    self.ensure_no_args(method_name, args)?;
                    let runtime_name = if method_name == "shutdown_read" {
                        "mux_net_local_shutdown_read"
                    } else {
                        "mux_net_local_shutdown_write"
                    };
                    self.build_net_call(runtime_name, &[obj_value]).map(Some)
                }
                "close" => {
                    self.ensure_no_args("close", args)?;
                    self.build_net_call("mux_net_local_close", &[obj_value])
                        .map(Some)
                }
                _ => Ok(None),
            },
            "LocalListener" => match method_name {
                "accept" => {
                    self.ensure_no_args("accept", args)?;
                    self.build_net_call("mux_net_local_listener_accept", &[obj_value])
                        .map(Some)
                }
                "set_nonblocking" => {
                    let enabled = gen_one_expr(self, args)?;
                    let converted = self.bool_to_i32(enabled)?;
                    self.build_net_call(
                        "mux_net_local_listener_set_nonblocking",
                        &[obj_value, converted],
                    )
                    .map(Some)
                }
                "close" => {
                    self.ensure_no_args("close", args)?;
                    self.build_net_call("mux_net_local_listener_close", &[obj_value])
                        .map(Some)
                }
                _ => Ok(None),
            },
            _ => Ok(None),
        }
    }

    fn generate_net_udp_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        type_name: &str,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        match type_name {
            "UdpSocket" => match method_name {
                "send_to" => {
                    let (data, addr) = gen_two_expr(self, args)?;
                    let call =
                        self.build_net_call("mux_net_udp_send_to", &[obj_value, data, addr])?;
                    Ok(Some(call))
                }
                "recv_from" => {
                    let size = gen_one_expr(self, args)?;
                    let call = self.build_net_call("mux_net_udp_recv_from", &[obj_value, size])?;
                    Ok(Some(call))
                }
                "close" => {
                    self.ensure_no_args("close", args)?;
                    let call = self.build_net_call("mux_net_udp_close", &[obj_value])?;
                    Ok(Some(call))
                }
                "set_nonblocking" => {
                    let bool_val = gen_one_expr(self, args)?;
                    let converted = self.bool_to_i32(bool_val)?;
                    let call = self
                        .build_net_call("mux_net_udp_set_nonblocking", &[obj_value, converted])?;
                    Ok(Some(call))
                }
                "set_read_timeout" | "set_write_timeout" => {
                    let timeout = gen_one_expr(self, args)?;
                    let runtime_name = if method_name == "set_read_timeout" {
                        "mux_net_udp_set_read_timeout"
                    } else {
                        "mux_net_udp_set_write_timeout"
                    };
                    let call = self.build_net_call(runtime_name, &[obj_value, timeout])?;
                    Ok(Some(call))
                }
                "set_ttl" => {
                    let ttl = gen_one_expr(self, args)?;
                    self.build_net_call("mux_net_udp_set_ttl", &[obj_value, ttl])
                        .map(Some)
                }
                "ttl" => {
                    self.ensure_no_args("ttl", args)?;
                    self.build_net_call("mux_net_udp_ttl", &[obj_value])
                        .map(Some)
                }
                "set_broadcast" => {
                    let enabled = gen_one_expr(self, args)?;
                    let converted = self.bool_to_i32(enabled)?;
                    self.build_net_call("mux_net_udp_set_broadcast", &[obj_value, converted])
                        .map(Some)
                }
                "broadcast" => {
                    self.ensure_no_args("broadcast", args)?;
                    self.build_net_call("mux_net_udp_broadcast", &[obj_value])
                        .map(Some)
                }
                "set_multicast_loop_v4" => {
                    let enabled = gen_one_expr(self, args)?;
                    let converted = self.bool_to_i32(enabled)?;
                    self.build_net_call(
                        "mux_net_udp_set_multicast_loop_v4",
                        &[obj_value, converted],
                    )
                    .map(Some)
                }
                "multicast_loop_v4" => {
                    self.ensure_no_args("multicast_loop_v4", args)?;
                    self.build_net_call("mux_net_udp_multicast_loop_v4", &[obj_value])
                        .map(Some)
                }
                "set_multicast_ttl_v4" => {
                    let ttl = gen_one_expr(self, args)?;
                    self.build_net_call("mux_net_udp_set_multicast_ttl_v4", &[obj_value, ttl])
                        .map(Some)
                }
                "multicast_ttl_v4" => {
                    self.ensure_no_args("multicast_ttl_v4", args)?;
                    self.build_net_call("mux_net_udp_multicast_ttl_v4", &[obj_value])
                        .map(Some)
                }
                "join_multicast_v4" | "leave_multicast_v4" => {
                    let (group, interface) = gen_two_expr(self, args)?;
                    let runtime_name = if method_name == "join_multicast_v4" {
                        "mux_net_udp_join_multicast_v4"
                    } else {
                        "mux_net_udp_leave_multicast_v4"
                    };
                    self.build_net_call(runtime_name, &[obj_value, group, interface])
                        .map(Some)
                }
                "set_multicast_loop_v6" => {
                    let enabled = gen_one_expr(self, args)?;
                    let converted = self.bool_to_i32(enabled)?;
                    self.build_net_call(
                        "mux_net_udp_set_multicast_loop_v6",
                        &[obj_value, converted],
                    )
                    .map(Some)
                }
                "multicast_loop_v6" => {
                    self.ensure_no_args("multicast_loop_v6", args)?;
                    self.build_net_call("mux_net_udp_multicast_loop_v6", &[obj_value])
                        .map(Some)
                }
                "set_multicast_hops_v6" => {
                    let hops = gen_one_expr(self, args)?;
                    self.build_net_call("mux_net_udp_set_multicast_hops_v6", &[obj_value, hops])
                        .map(Some)
                }
                "multicast_hops_v6" => {
                    self.ensure_no_args("multicast_hops_v6", args)?;
                    self.build_net_call("mux_net_udp_multicast_hops_v6", &[obj_value])
                        .map(Some)
                }
                "join_multicast_v6" | "leave_multicast_v6" => {
                    let (group, interface) = gen_two_expr(self, args)?;
                    let runtime_name = if method_name == "join_multicast_v6" {
                        "mux_net_udp_join_multicast_v6"
                    } else {
                        "mux_net_udp_leave_multicast_v6"
                    };
                    self.build_net_call(runtime_name, &[obj_value, group, interface])
                        .map(Some)
                }
                "peer_addr" => {
                    self.ensure_no_args("peer_addr", args)?;
                    let call = self.build_net_call("mux_net_udp_peer_addr", &[obj_value])?;
                    Ok(Some(call))
                }
                "local_addr" => {
                    self.ensure_no_args("local_addr", args)?;
                    let call = self.build_net_call("mux_net_udp_local_addr", &[obj_value])?;
                    Ok(Some(call))
                }
                "set_recv_buffer_size" | "set_send_buffer_size" => {
                    let size = gen_one_expr(self, args)?;
                    let runtime_name = if method_name == "set_recv_buffer_size" {
                        "mux_net_udp_set_recv_buffer_size"
                    } else {
                        "mux_net_udp_set_send_buffer_size"
                    };
                    self.build_net_call(runtime_name, &[obj_value, size])
                        .map(Some)
                }
                "recv_buffer_size" | "send_buffer_size" => {
                    self.ensure_no_args(method_name, args)?;
                    let runtime_name = if method_name == "recv_buffer_size" {
                        "mux_net_udp_recv_buffer_size"
                    } else {
                        "mux_net_udp_send_buffer_size"
                    };
                    self.build_net_call(runtime_name, &[obj_value]).map(Some)
                }
                _ => Ok(None),
            },
            "UdpDatagram" => {
                let runtime_name = match method_name {
                    "bytes" => "mux_net_udp_datagram_bytes",
                    "address" => "mux_net_udp_datagram_address",
                    "truncated" => "mux_net_udp_datagram_truncated",
                    _ => return Ok(None),
                };
                self.ensure_no_args(method_name, args)?;
                self.build_net_call(runtime_name, &[obj_value]).map(Some)
            }
            _ => Ok(None),
        }
    }

    pub(super) fn try_generate_process_instance_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        obj_type: &Type,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let Type::Named(type_name, _) = obj_type else {
            return Ok(None);
        };
        match type_name.as_str() {
            "ProcessError" => self.generate_process_error_method(obj_value, method_name, args),
            "Command" => self.generate_process_command_method(obj_value, method_name, args),
            "ProcessPool" => self.generate_process_pool_method(obj_value, method_name, args),
            "Child" | "process.Child" => {
                self.generate_process_child_method(obj_value, method_name, args)
            }
            "Output" => self.generate_process_output_method(obj_value, method_name, args),
            _ => Ok(None),
        }
    }

    fn generate_process_error_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let runtime_name = match method_name {
            "message" => "mux_process_error_message",
            "to_string" => "mux_process_error_to_string",
            _ => return Ok(None),
        };
        self.ensure_no_args(method_name, args)?;
        self.build_net_call(runtime_name, &[obj_value]).map(Some)
    }

    fn generate_process_command_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        match method_name {
            "set_program" => {
                let program = gen_one_expr(self, args)?;
                let program_cstr = self.string_value_to_cstr(program)?;
                let call = self.build_net_call(
                    "mux_process_command_set_program",
                    &[obj_value, program_cstr],
                )?;
                self.free_cstrings(&[program_cstr])?;
                Ok(Some(call))
            }
            "arg" => {
                let arg = gen_one_expr(self, args)?;
                let arg_cstr = self.string_value_to_cstr(arg)?;
                let call =
                    self.build_net_call("mux_process_command_arg", &[obj_value, arg_cstr])?;
                self.free_cstrings(&[arg_cstr])?;
                Ok(Some(call))
            }
            "env" => {
                let (key, value) = gen_two_expr(self, args)?;
                let key_cstr = self.string_value_to_cstr(key)?;
                let value_cstr = self.string_value_to_cstr(value)?;
                let call = self.build_net_call(
                    "mux_process_command_env",
                    &[obj_value, key_cstr, value_cstr],
                )?;
                self.free_cstrings(&[key_cstr, value_cstr])?;
                Ok(Some(call))
            }
            "cwd" => {
                let path = gen_one_expr(self, args)?;
                let path_cstr = self.string_value_to_cstr(path)?;
                let call =
                    self.build_net_call("mux_process_command_cwd", &[obj_value, path_cstr])?;
                self.free_cstrings(&[path_cstr])?;
                Ok(Some(call))
            }
            "stdin_piped" | "stdout_piped" | "stderr_piped" | "stdin_null" | "stdout_null"
            | "stderr_null" | "output" | "status" | "spawn" => {
                self.ensure_no_args(method_name, args)?;
                let runtime_name = match method_name {
                    "stdin_piped" => "mux_process_command_stdin_piped",
                    "stdout_piped" => "mux_process_command_stdout_piped",
                    "stderr_piped" => "mux_process_command_stderr_piped",
                    "stdin_null" => "mux_process_command_stdin_null",
                    "stdout_null" => "mux_process_command_stdout_null",
                    "stderr_null" => "mux_process_command_stderr_null",
                    "output" => "mux_process_command_output",
                    "status" => "mux_process_command_status",
                    "spawn" => "mux_process_command_spawn",
                    _ => unreachable!(),
                };
                self.build_net_call(runtime_name, &[obj_value]).map(Some)
            }
            _ => Ok(None),
        }
    }

    fn generate_process_pool_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        match method_name {
            "submit" | "try_submit" => {
                let command = gen_one_expr(self, args)?;
                let runtime_name = if method_name == "submit" {
                    "mux_process_pool_submit"
                } else {
                    "mux_process_pool_try_submit"
                };
                self.build_net_call(runtime_name, &[obj_value, command])
                    .map(Some)
            }
            "submit_timeout" => {
                let (command, timeout) = gen_two_expr(self, args)?;
                self.build_net_call(
                    "mux_process_pool_submit_timeout",
                    &[obj_value, command, timeout],
                )
                .map(Some)
            }
            "cancel_pending" | "close" => {
                self.ensure_no_args(method_name, args)?;
                let runtime_name = if method_name == "cancel_pending" {
                    "mux_process_pool_cancel_pending"
                } else {
                    "mux_process_pool_close"
                };
                self.build_net_call(runtime_name, &[obj_value]).map(Some)
            }
            _ => Ok(None),
        }
    }

    fn generate_process_child_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        match method_name {
            "wait" | "try_wait" | "kill" | "kill_group" => {
                self.ensure_no_args(method_name, args)?;
                let runtime_name = match method_name {
                    "wait" => "mux_process_child_wait",
                    "try_wait" => "mux_process_child_try_wait",
                    "kill" => "mux_process_child_kill",
                    "kill_group" => "mux_process_child_kill_group",
                    _ => unreachable!(),
                };
                self.build_net_call(runtime_name, &[obj_value]).map(Some)
            }
            "wait_timeout" => {
                let timeout = gen_one_expr(self, args)?;
                self.build_net_call("mux_process_child_wait_timeout", &[obj_value, timeout])
                    .map(Some)
            }
            "write_stdin" => {
                let input = gen_one_expr(self, args)?;
                self.build_net_call("mux_process_child_write_stdin", &[obj_value, input])
                    .map(Some)
            }
            "close_stdin" => {
                self.ensure_no_args(method_name, args)?;
                self.build_net_call("mux_process_child_close_stdin", &[obj_value])
                    .map(Some)
            }
            "read_stdout" | "read_stderr" => {
                let size = gen_one_expr(self, args)?;
                let runtime_name = match method_name {
                    "read_stdout" => "mux_process_child_read_stdout",
                    "read_stderr" => "mux_process_child_read_stderr",
                    _ => unreachable!(),
                };
                self.build_net_call(runtime_name, &[obj_value, size])
                    .map(Some)
            }
            _ => Ok(None),
        }
    }

    fn generate_process_output_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        match method_name {
            "status" | "stdout" | "stderr" => {
                self.ensure_no_args(method_name, args)?;
                let runtime_name = match method_name {
                    "status" => "mux_process_output_status",
                    "stdout" => "mux_process_output_stdout",
                    "stderr" => "mux_process_output_stderr",
                    _ => unreachable!(),
                };
                self.build_net_call(runtime_name, &[obj_value]).map(Some)
            }
            _ => Ok(None),
        }
    }

    pub(super) fn try_generate_regex_instance_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        obj_type: &Type,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let Type::Named(type_name, _) = obj_type else {
            return Ok(None);
        };
        if type_name == "RegexError" {
            let runtime_name = match method_name {
                "message" => "mux_regex_error_message",
                "to_string" => "mux_regex_error_to_string",
                _ => return Ok(None),
            };
            self.ensure_no_args(method_name, args)?;
            return self.build_net_call(runtime_name, &[obj_value]).map(Some);
        }
        if type_name == "Regex" {
            let runtime_name = match method_name {
                "is_match" => "mux_regex_is_match",
                "full_match" => "mux_regex_full_match",
                "find" => "mux_regex_find",
                "find_all" => "mux_regex_find_all",
                "replace" => "mux_regex_replace",
                "replace_first" => "mux_regex_replace_first",
                "replace_with" => "mux_regex_replace_with",
                "split" => "mux_regex_split",
                "group_count" => "mux_regex_group_count",
                "named_groups" => "mux_regex_named_groups",
                _ => return Ok(None),
            };
            let call = match method_name {
                "group_count" | "named_groups" => {
                    self.ensure_no_args(method_name, args)?;
                    self.build_net_call(runtime_name, &[obj_value])?
                }
                "replace" | "replace_first" => {
                    let (text, replacement) = gen_two_expr(self, args)?;
                    self.build_net_call(runtime_name, &[obj_value, text, replacement])?
                }
                "replace_with" => {
                    let (text, callback) = gen_two_expr(self, args)?;
                    self.build_net_call(runtime_name, &[obj_value, text, callback])?
                }
                _ => {
                    let text = gen_one_expr(self, args)?;
                    self.build_net_call(runtime_name, &[obj_value, text])?
                }
            };
            return Ok(Some(call));
        }
        if type_name != "RegexMatch" {
            return Ok(None);
        }
        let runtime_name = match method_name {
            "start" => "mux_regex_match_start",
            "end" => "mux_regex_match_end",
            "text" => "mux_regex_match_text",
            "capture" => "mux_regex_match_capture",
            "capture_named" => "mux_regex_match_capture_named",
            "captures" => "mux_regex_match_captures",
            _ => return Ok(None),
        };
        let call = match method_name {
            "start" | "end" | "text" | "captures" => {
                self.ensure_no_args(method_name, args)?;
                self.build_net_call(runtime_name, &[obj_value])?
            }
            "capture" | "capture_named" => {
                let argument = gen_one_expr(self, args)?;
                self.build_net_call(runtime_name, &[obj_value, argument])?
            }
            _ => unreachable!(),
        };
        Ok(Some(call))
    }

    pub(super) fn try_generate_uuid_instance_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        obj_type: &Type,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if matches!(obj_type, Type::Named(name, _) if name == "UuidError") {
            let runtime_name = match method_name {
                "message" => "mux_uuid_error_message",
                "to_string" => "mux_uuid_error_to_string",
                _ => return Ok(None),
            };
            self.ensure_no_args(method_name, args)?;
            return self.build_net_call(runtime_name, &[obj_value]).map(Some);
        }
        if !matches!(obj_type, Type::Named(name, _) if name == "Uuid") {
            return Ok(None);
        }
        self.ensure_no_args(method_name, args)?;
        let runtime_name = match method_name {
            "to_string" => "mux_uuid_to_string",
            "to_compact" => "mux_uuid_to_compact",
            "to_braced" => "mux_uuid_to_braced",
            "to_urn" => "mux_uuid_to_urn",
            "to_bytes" => "mux_uuid_to_bytes",
            "to_parts" => "mux_uuid_to_parts",
            "is_nil" => "mux_uuid_is_nil",
            "is_max" => "mux_uuid_is_max",
            "version" => "mux_uuid_version",
            "variant" => "mux_uuid_variant",
            _ => return Ok(None),
        };
        self.build_net_call(runtime_name, &[obj_value]).map(Some)
    }

    pub(super) fn try_generate_url_instance_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        obj_type: &Type,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if matches!(obj_type, Type::Named(name, _) if name == "UrlError") {
            let runtime_name = match method_name {
                "message" => "mux_url_error_message",
                "to_string" => "mux_url_error_to_string",
                _ => return Ok(None),
            };
            self.ensure_no_args(method_name, args)?;
            return self.build_net_call(runtime_name, &[obj_value]).map(Some);
        }
        if !matches!(obj_type, Type::Named(name, _) if name == "Url") {
            return Ok(None);
        }
        let runtime_name = match method_name {
            "to_string" => "mux_url_to_string",
            "scheme" => "mux_url_scheme",
            "username" => "mux_url_username",
            "password" => "mux_url_password",
            "host" => "mux_url_host",
            "host_ascii" => "mux_url_host_ascii",
            "host_unicode" => "mux_url_host_unicode",
            "port" => "mux_url_port",
            "path" => "mux_url_path",
            "query" => "mux_url_query",
            "fragment" => "mux_url_fragment",
            "origin" => "mux_url_origin",
            "redacted" => "mux_url_redacted",
            "query_pairs" => "mux_url_query_pairs",
            "to_file_path" => "mux_url_to_file_path",
            "is_http" => "mux_url_is_http",
            "is_https" => "mux_url_is_https",
            "join" => "mux_url_join",
            "with_path" => "mux_url_with_path",
            "with_query" => "mux_url_with_query",
            "with_fragment" => "mux_url_with_fragment",
            "with_scheme" => "mux_url_with_scheme",
            "with_username" => "mux_url_with_username",
            "with_password" => "mux_url_with_password",
            "with_host" => "mux_url_with_host",
            "with_port" => "mux_url_with_port",
            "with_query_pairs" => "mux_url_with_query_pairs",
            _ => return Ok(None),
        };
        let call = match method_name {
            "join" | "with_path" | "with_query" | "with_fragment" | "with_host" | "with_port"
            | "with_scheme" | "with_username" | "with_password" | "with_query_pairs" => {
                let arg = gen_one_expr(self, args)?;
                self.build_net_call(runtime_name, &[obj_value, arg])?
            }
            _ => {
                self.ensure_no_args(method_name, args)?;
                self.build_net_call(runtime_name, &[obj_value])?
            }
        };
        Ok(Some(call))
    }

    pub(super) fn try_generate_sync_instance_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        obj_type: &Type,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let Type::Named(type_name, _) = obj_type else {
            return Ok(None);
        };

        match type_name.as_str() {
            "SyncError" => self.generate_sync_error_method(obj_value, method_name, args),
            "AtomicInt" | "AtomicBool" => {
                self.generate_sync_atomic_method(type_name, obj_value, method_name, args)
            }
            "CancellationToken" => {
                self.generate_sync_cancellation_method(obj_value, method_name, args)
            }
            "Semaphore" => self.generate_sync_semaphore_method(obj_value, method_name, args),
            "Barrier" => self.generate_sync_barrier_method(obj_value, method_name, args),
            "Channel" => self.generate_sync_channel_method(obj_value, method_name, args),
            "WorkerPool" => self.generate_sync_worker_pool_method(obj_value, method_name, args),
            "Once" => self.generate_sync_once_method(obj_value, method_name, args),
            "Thread" => self.generate_sync_thread_method(obj_value, method_name, args),
            "Mutex" => self.generate_sync_mutex_method(obj_value, method_name, args),
            "RwLock" => self.generate_sync_rwlock_method(obj_value, method_name, args),
            "CondVar" => self.generate_sync_condvar_method(obj_value, method_name, args),
            _ => Ok(None),
        }
    }

    fn generate_sync_error_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let runtime_name = match method_name {
            "message" => "mux_sync_error_message",
            "to_string" => "mux_sync_error_to_string",
            _ => return Ok(None),
        };
        self.ensure_no_args(method_name, args)?;
        self.build_net_call(runtime_name, &[obj_value]).map(Some)
    }

    fn generate_sync_atomic_method(
        &mut self,
        type_name: &str,
        obj_value: BasicValueEnum<'a>,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let prefix = if type_name == "AtomicInt" {
            "mux_atomic_int_"
        } else {
            "mux_atomic_bool_"
        };
        let runtime_name = match method_name {
            "load" => format!("{prefix}load"),
            "store" => format!("{prefix}store"),
            "add" if type_name == "AtomicInt" => format!("{prefix}add"),
            "swap" => format!("{prefix}swap"),
            "compare_exchange" => format!("{prefix}compare_exchange"),
            _ => return Ok(None),
        };
        let expected = match method_name {
            "load" => 0,
            "compare_exchange" => 2,
            _ => 1,
        };
        if args.len() != expected {
            return Err(format!(
                "{method_name}() method takes exactly {expected} argument{}",
                if expected == 1 { "" } else { "s" }
            ));
        }
        let mut values = vec![obj_value];
        values.extend(
            args.iter()
                .map(|arg| self.generate_expression(arg))
                .collect::<Result<Vec<_>, _>>()?,
        );
        self.build_net_call(&runtime_name, &values).map(Some)
    }

    fn generate_sync_cancellation_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let runtime_name = match method_name {
            "cancel" => "mux_cancellation_cancel",
            "is_cancelled" => "mux_cancellation_is_cancelled",
            _ => return Ok(None),
        };
        self.ensure_no_args(method_name, args)?;
        self.build_net_call(runtime_name, &[obj_value]).map(Some)
    }

    fn generate_sync_semaphore_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let runtime_name = match method_name {
            "acquire" => "mux_semaphore_acquire",
            "try_acquire" => "mux_semaphore_try_acquire",
            "acquire_timeout" => "mux_semaphore_acquire_timeout",
            "release" => "mux_semaphore_release",
            _ => return Ok(None),
        };
        if method_name == "acquire_timeout" {
            let timeout = gen_one_expr(self, args)?;
            self.build_net_call(runtime_name, &[obj_value, timeout])
                .map(Some)
        } else {
            self.ensure_no_args(method_name, args)?;
            self.build_net_call(runtime_name, &[obj_value]).map(Some)
        }
    }

    fn generate_sync_barrier_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if method_name != "wait" {
            return Ok(None);
        }
        self.ensure_no_args(method_name, args)?;
        self.build_net_call("mux_barrier_wait", &[obj_value])
            .map(Some)
    }

    fn generate_sync_channel_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let runtime_name = match method_name {
            "send" => "mux_channel_send",
            "try_send" => "mux_channel_try_send",
            "send_timeout" => "mux_channel_send_timeout",
            "send_cancelled" => "mux_channel_send_cancelled",
            "recv" => "mux_channel_recv",
            "try_recv" => "mux_channel_try_recv",
            "recv_timeout" => "mux_channel_recv_timeout",
            "recv_cancelled" => "mux_channel_recv_cancelled",
            "close" => "mux_channel_close",
            "is_closed" => "mux_channel_is_closed",
            "capacity" => "mux_channel_capacity",
            _ => return Ok(None),
        };
        match method_name {
            "send" | "try_send" => {
                self.ensure_arg_count(method_name, args, 1)?;
                let generated = self.generate_expression(&args[0])?;
                let value = self.box_value(generated);
                self.call_runtime_function(runtime_name, &[obj_value, value.into()])
                    .map(Some)
            }
            "send_cancelled" => {
                self.ensure_arg_count(method_name, args, 2)?;
                let generated = self.generate_expression(&args[0])?;
                let value = self.box_value(generated);
                let cancellation = self.generate_expression(&args[1])?;
                self.call_runtime_function(runtime_name, &[obj_value, value.into(), cancellation])
                    .map(Some)
            }
            "send_timeout" => {
                self.ensure_arg_count(method_name, args, 2)?;
                let generated = self.generate_expression(&args[0])?;
                let value = self.box_value(generated);
                let generated_timeout = self.generate_expression(&args[1])?;
                let timeout = self.get_raw_int_value(generated_timeout)?;
                self.call_runtime_function(runtime_name, &[obj_value, value.into(), timeout.into()])
                    .map(Some)
            }
            "recv_timeout" => {
                self.ensure_arg_count(method_name, args, 1)?;
                let generated_timeout = self.generate_expression(&args[0])?;
                let timeout = self.get_raw_int_value(generated_timeout)?;
                self.call_runtime_function(runtime_name, &[obj_value, timeout.into()])
                    .map(Some)
            }
            "recv_cancelled" => {
                self.ensure_arg_count(method_name, args, 1)?;
                let cancellation = self.generate_expression(&args[0])?;
                self.call_runtime_function(runtime_name, &[obj_value, cancellation])
                    .map(Some)
            }
            _ => {
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function(runtime_name, &[obj_value])
                    .map(Some)
            }
        }
    }

    fn generate_sync_worker_pool_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let runtime_name = match method_name {
            "submit" => "mux_pool_submit",
            "map" => "mux_pool_map",
            "try_submit" => "mux_pool_try_submit",
            "submit_timeout" => "mux_pool_submit_timeout",
            "cancel_pending" => "mux_pool_cancel_pending",
            "close" => "mux_pool_close",
            _ => return Ok(None),
        };
        match method_name {
            "map" => {
                self.ensure_arg_count(method_name, args, 2)?;
                let values = self.generate_expression(&args[0])?;
                let callback = self.generate_expression(&args[1])?;
                self.call_runtime_function(runtime_name, &[obj_value, values, callback])
                    .map(Some)
            }
            "submit" | "try_submit" => {
                self.ensure_arg_count(method_name, args, 1)?;
                let callback = self.generate_expression(&args[0])?;
                self.call_runtime_function(runtime_name, &[obj_value, callback])
                    .map(Some)
            }
            "submit_timeout" => {
                self.ensure_arg_count(method_name, args, 2)?;
                let callback = self.generate_expression(&args[0])?;
                let generated_timeout = self.generate_expression(&args[1])?;
                let timeout = self.get_raw_int_value(generated_timeout)?;
                self.call_runtime_function(runtime_name, &[obj_value, callback, timeout.into()])
                    .map(Some)
            }
            "cancel_pending" | "close" => {
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function(runtime_name, &[obj_value])
                    .map(Some)
            }
            _ => Ok(None),
        }
    }

    fn generate_sync_once_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if method_name != "call" {
            return Ok(None);
        }
        self.ensure_arg_count(method_name, args, 1)?;
        let callback = self.generate_expression(&args[0])?;
        self.call_runtime_function("mux_once_call", &[obj_value, callback])
            .map(Some)
    }

    fn generate_sync_thread_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        self.ensure_no_args("Thread", args)?;
        let runtime_name = match method_name {
            "join" => "mux_thread_join",
            "detach" => "mux_thread_detach",
            _ => return Ok(None),
        };
        self.call_runtime_function(runtime_name, &[obj_value])
            .map(Some)
    }

    fn generate_sync_mutex_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if method_name != "with_lock" {
            return Ok(None);
        }
        if args.len() != 1 {
            return Err("with_lock() method takes exactly 1 argument".to_string());
        }
        let callback = self.generate_expression(&args[0])?;
        self.call_runtime_function("mux_mutex_with_lock", &[obj_value, callback])
            .map(Some)
    }

    fn generate_sync_rwlock_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if !matches!(method_name, "with_read" | "with_write") {
            return Ok(None);
        }
        if args.len() != 1 {
            return Err(format!("{method_name}() method takes exactly 1 argument"));
        }
        let callback = self.generate_expression(&args[0])?;
        let runtime_name = if method_name == "with_read" {
            "mux_rwlock_with_read"
        } else {
            "mux_rwlock_with_write"
        };
        self.call_runtime_function(runtime_name, &[obj_value, callback])
            .map(Some)
    }

    fn generate_sync_condvar_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        match method_name {
            "wait" => {
                if args.len() != 1 {
                    return Err("wait() method takes exactly 1 argument".to_string());
                }
                let generated_mutex = self.generate_expression(&args[0])?;
                let mutex = self.box_value(generated_mutex);
                self.call_runtime_function("mux_condvar_wait", &[obj_value, mutex.into()])
                    .map(Some)
            }
            "wait_timeout" => {
                if args.len() != 2 {
                    return Err("wait_timeout() method takes exactly 2 arguments".to_string());
                }
                let generated_mutex = self.generate_expression(&args[0])?;
                let mutex = self.box_value(generated_mutex);
                let generated_timeout = self.generate_expression(&args[1])?;
                let timeout = self.get_raw_int_value(generated_timeout)?;
                self.call_runtime_function(
                    "mux_condvar_wait_timeout",
                    &[obj_value, mutex.into(), timeout.into()],
                )
                .map(Some)
            }
            "signal" | "broadcast" => {
                self.ensure_no_args(method_name, args)?;
                let runtime_name = if method_name == "signal" {
                    "mux_condvar_signal"
                } else {
                    "mux_condvar_broadcast"
                };
                self.call_runtime_function(runtime_name, &[obj_value])
                    .map(Some)
            }
            _ => Ok(None),
        }
    }

    pub(super) fn try_generate_sync_static_method_call(
        &mut self,
        class_name: &str,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if class_name == "SyncError" && method_name == "from_message" {
            let message = gen_one_expr(self, args)?;
            return self
                .build_net_call("mux_sync_error_from_message", &[message])
                .map(Some);
        }
        match (class_name, method_name) {
            ("Channel", "select") => {
                if args.len() != 2 {
                    return Err("select() method takes exactly 2 arguments".to_string());
                }
                let values = args
                    .iter()
                    .map(|arg| self.generate_expression(arg))
                    .collect::<Result<Vec<_>, _>>()?;
                self.call_runtime_function("mux_channel_select", &values)
                    .map(Some)
            }
            ("AtomicInt", "new") => {
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function("mux_atomic_int_new", &[])
                    .map(Some)
            }
            ("AtomicInt", "with_value") => {
                let arg = gen_one_expr(self, args)?;
                self.call_runtime_function("mux_atomic_int_with_value", &[arg])
                    .map(Some)
            }
            ("AtomicBool", "new") => {
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function("mux_atomic_bool_new", &[])
                    .map(Some)
            }
            ("AtomicBool", "with_value") => {
                let arg = gen_one_expr(self, args)?;
                self.call_runtime_function("mux_atomic_bool_with_value", &[arg])
                    .map(Some)
            }
            ("CancellationToken", "new") => {
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function("mux_cancellation_new", &[])
                    .map(Some)
            }
            ("Semaphore", "with_permits") => {
                let arg = gen_one_expr(self, args)?;
                self.build_net_call("mux_semaphore_with_permits", &[arg])
                    .map(Some)
            }
            ("Barrier", "with_size") => {
                let arg = gen_one_expr(self, args)?;
                self.build_net_call("mux_barrier_with_size", &[arg])
                    .map(Some)
            }
            ("Channel", "new" | "new_unbounded") => {
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function("mux_channel_new_unbounded", &[])
                    .map(Some)
            }
            ("Channel", "new_bounded") => {
                let capacity = gen_one_expr(self, args)?;
                self.call_runtime_function("mux_channel_new_bounded", &[capacity])
                    .map(Some)
            }
            ("Once", "new") => {
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function("mux_once_new", &[]).map(Some)
            }
            ("Mutex", "with_value") => {
                let value = gen_one_expr(self, args)?;
                let value = self.box_value(value);
                self.call_runtime_function("mux_mutex_with_value", &[value.into()])
                    .map(Some)
            }
            ("RwLock", "with_value") => {
                let value = gen_one_expr(self, args)?;
                let value = self.box_value(value);
                self.call_runtime_function("mux_rwlock_with_value", &[value.into()])
                    .map(Some)
            }
            ("WorkerPool", "new") => {
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function("mux_pool_new", &[]).map(Some)
            }
            ("WorkerPool", "with_size") => {
                let workers = gen_one_expr(self, args)?;
                self.call_runtime_function("mux_pool_with_size", &[workers])
                    .map(Some)
            }
            ("WorkerPool", "with_config") => {
                if args.len() != 2 {
                    return Err("with_config() method takes exactly 2 arguments".to_string());
                }
                let workers = self.generate_expression(&args[0])?;
                let capacity = self.generate_expression(&args[1])?;
                self.call_runtime_function("mux_pool_with_config", &[workers, capacity])
                    .map(Some)
            }
            _ => Ok(None),
        }
    }

    pub(super) fn try_generate_sql_instance_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        obj_type: &Type,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let Type::Named(type_name, _) = obj_type else {
            return Ok(None);
        };

        match type_name.as_str() {
            "Connection" => match method_name {
                "close" => {
                    self.ensure_no_args("close", args)?;
                    self.build_net_call("mux_sql_connection_close", &[obj_value])
                        .map(Some)
                }
                "execute" => {
                    let sql = gen_one_expr(self, args)?;
                    self.build_net_call("mux_sql_connection_execute", &[obj_value, sql])
                        .map(Some)
                }
                "execute_batch" => {
                    let sql = gen_one_expr(self, args)?;
                    self.build_net_call("mux_sql_connection_execute_batch", &[obj_value, sql])
                        .map(Some)
                }
                "execute_many" => {
                    let (sql, rows) = gen_two_expr(self, args)?;
                    self.build_net_call("mux_sql_connection_execute_many", &[obj_value, sql, rows])
                        .map(Some)
                }
                "execute_params" => {
                    let (sql, params) = gen_two_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_connection_execute_params",
                        &[obj_value, sql, params],
                    )
                    .map(Some)
                }
                "execute_named" => {
                    let (sql, params) = gen_two_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_connection_execute_named",
                        &[obj_value, sql, params],
                    )
                    .map(Some)
                }
                "query" => {
                    let sql = gen_one_expr(self, args)?;
                    self.build_net_call("mux_sql_connection_query", &[obj_value, sql])
                        .map(Some)
                }
                "query_params" => {
                    let (sql, params) = gen_two_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_connection_query_params",
                        &[obj_value, sql, params],
                    )
                    .map(Some)
                }
                "query_named" => {
                    let (sql, params) = gen_two_expr(self, args)?;
                    self.build_net_call("mux_sql_connection_query_named", &[obj_value, sql, params])
                        .map(Some)
                }
                "query_with_timeout" => {
                    let (sql, timeout) = gen_two_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_connection_query_with_timeout",
                        &[obj_value, sql, timeout],
                    )
                    .map(Some)
                }
                "query_params_with_timeout" => {
                    let (sql, params, timeout) = gen_three_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_connection_query_params_with_timeout",
                        &[obj_value, sql, params, timeout],
                    )
                    .map(Some)
                }
                "query_named_with_timeout" => {
                    let (sql, params, timeout) = gen_three_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_connection_query_named_with_timeout",
                        &[obj_value, sql, params, timeout],
                    )
                    .map(Some)
                }
                "query_with_cancellation" => {
                    let (sql, token) = gen_two_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_connection_query_with_cancellation",
                        &[obj_value, sql, token],
                    )
                    .map(Some)
                }
                "query_params_with_cancellation" => {
                    let (sql, params, token) = gen_three_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_connection_query_params_with_cancellation",
                        &[obj_value, sql, params, token],
                    )
                    .map(Some)
                }
                "query_named_with_cancellation" => {
                    let (sql, params, token) = gen_three_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_connection_query_named_with_cancellation",
                        &[obj_value, sql, params, token],
                    )
                    .map(Some)
                }
                "begin_transaction" => {
                    self.ensure_no_args("begin_transaction", args)?;
                    self.build_net_call("mux_sql_connection_begin_transaction", &[obj_value])
                        .map(Some)
                }
                "begin_transaction_with_options" => {
                    let values = args
                        .iter()
                        .map(|arg| self.generate_expression(arg))
                        .collect::<Result<Vec<_>, _>>()?;
                    if values.len() != 3 {
                        return Err(
                            "begin_transaction_with_options() method takes exactly 3 arguments"
                                .to_string(),
                        );
                    }
                    let mut call_args = vec![obj_value];
                    call_args.extend(values);
                    self.build_net_call(
                        "mux_sql_connection_begin_transaction_with_options",
                        &call_args,
                    )
                    .map(Some)
                }
                "prepare" => {
                    let sql = gen_one_expr(self, args)?;
                    self.build_net_call("mux_sql_connection_prepare", &[obj_value, sql])
                        .map(Some)
                }
                _ => Ok(None),
            },
            "Pool" => match method_name {
                "close" => {
                    self.ensure_no_args("close", args)?;
                    self.build_net_call("mux_sql_pool_close", &[obj_value])
                        .map(Some)
                }
                "metrics" => {
                    self.ensure_no_args("metrics", args)?;
                    self.build_net_call("mux_sql_pool_metrics", &[obj_value])
                        .map(Some)
                }
                "execute" => {
                    let sql = gen_one_expr(self, args)?;
                    self.build_net_call("mux_sql_pool_execute", &[obj_value, sql])
                        .map(Some)
                }
                "execute_batch" => {
                    let sql = gen_one_expr(self, args)?;
                    self.build_net_call("mux_sql_pool_execute_batch", &[obj_value, sql])
                        .map(Some)
                }
                "execute_many" => {
                    let (sql, rows) = gen_two_expr(self, args)?;
                    self.build_net_call("mux_sql_pool_execute_many", &[obj_value, sql, rows])
                        .map(Some)
                }
                "execute_params" => {
                    let (sql, params) = gen_two_expr(self, args)?;
                    self.build_net_call("mux_sql_pool_execute_params", &[obj_value, sql, params])
                        .map(Some)
                }
                "execute_named" => {
                    let (sql, params) = gen_two_expr(self, args)?;
                    self.build_net_call("mux_sql_pool_execute_named", &[obj_value, sql, params])
                        .map(Some)
                }
                "query" => {
                    let sql = gen_one_expr(self, args)?;
                    self.build_net_call("mux_sql_pool_query", &[obj_value, sql])
                        .map(Some)
                }
                "query_params" => {
                    let (sql, params) = gen_two_expr(self, args)?;
                    self.build_net_call("mux_sql_pool_query_params", &[obj_value, sql, params])
                        .map(Some)
                }
                "query_named" => {
                    let (sql, params) = gen_two_expr(self, args)?;
                    self.build_net_call("mux_sql_pool_query_named", &[obj_value, sql, params])
                        .map(Some)
                }
                "query_with_timeout" => {
                    let (sql, timeout) = gen_two_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_pool_query_with_timeout",
                        &[obj_value, sql, timeout],
                    )
                    .map(Some)
                }
                "query_params_with_timeout" => {
                    let (sql, params, timeout) = gen_three_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_pool_query_params_with_timeout",
                        &[obj_value, sql, params, timeout],
                    )
                    .map(Some)
                }
                "query_named_with_timeout" => {
                    let (sql, params, timeout) = gen_three_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_pool_query_named_with_timeout",
                        &[obj_value, sql, params, timeout],
                    )
                    .map(Some)
                }
                "query_with_cancellation" => {
                    let (sql, token) = gen_two_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_pool_query_with_cancellation",
                        &[obj_value, sql, token],
                    )
                    .map(Some)
                }
                "query_params_with_cancellation" => {
                    let (sql, params, token) = gen_three_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_pool_query_params_with_cancellation",
                        &[obj_value, sql, params, token],
                    )
                    .map(Some)
                }
                "query_named_with_cancellation" => {
                    let (sql, params, token) = gen_three_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_pool_query_named_with_cancellation",
                        &[obj_value, sql, params, token],
                    )
                    .map(Some)
                }
                _ => Ok(None),
            },
            "Migrator" => match method_name {
                "up" => {
                    self.ensure_no_args("up", args)?;
                    self.build_net_call("mux_sql_migrator_up", &[obj_value])
                        .map(Some)
                }
                "up_to" => {
                    let version = gen_one_expr(self, args)?;
                    self.build_net_call("mux_sql_migrator_up_to", &[obj_value, version])
                        .map(Some)
                }
                "down" => {
                    self.ensure_no_args("down", args)?;
                    self.build_net_call("mux_sql_migrator_down", &[obj_value])
                        .map(Some)
                }
                "down_to" => {
                    let version = gen_one_expr(self, args)?;
                    self.build_net_call("mux_sql_migrator_down_to", &[obj_value, version])
                        .map(Some)
                }
                "status" => {
                    self.ensure_no_args("status", args)?;
                    self.build_net_call("mux_sql_migrator_status", &[obj_value])
                        .map(Some)
                }
                "validate" => {
                    self.ensure_no_args("validate", args)?;
                    self.build_net_call("mux_sql_migrator_validate", &[obj_value])
                        .map(Some)
                }
                "dry_run" => {
                    self.ensure_no_args("dry_run", args)?;
                    self.build_net_call("mux_sql_migrator_dry_run", &[obj_value])
                        .map(Some)
                }
                _ => Ok(None),
            },
            "Transaction" => match method_name {
                "begin_transaction" => {
                    self.ensure_no_args(method_name, args)?;
                    self.build_net_call("mux_sql_transaction_begin_transaction", &[obj_value])
                        .map(Some)
                }
                "savepoint" => {
                    let name = gen_one_expr(self, args)?;
                    self.build_net_call("mux_sql_transaction_savepoint", &[obj_value, name])
                        .map(Some)
                }
                "rollback_to" => {
                    let name = gen_one_expr(self, args)?;
                    self.build_net_call("mux_sql_transaction_rollback_to", &[obj_value, name])
                        .map(Some)
                }
                "release_savepoint" => {
                    let name = gen_one_expr(self, args)?;
                    self.build_net_call("mux_sql_transaction_release_savepoint", &[obj_value, name])
                        .map(Some)
                }
                "commit" => {
                    self.ensure_no_args("commit", args)?;
                    self.build_net_call("mux_sql_transaction_commit", &[obj_value])
                        .map(Some)
                }
                "rollback" => {
                    self.ensure_no_args("rollback", args)?;
                    self.build_net_call("mux_sql_transaction_rollback", &[obj_value])
                        .map(Some)
                }
                "execute" => {
                    let sql = gen_one_expr(self, args)?;
                    self.build_net_call("mux_sql_transaction_execute", &[obj_value, sql])
                        .map(Some)
                }
                "execute_batch" => {
                    let sql = gen_one_expr(self, args)?;
                    self.build_net_call("mux_sql_transaction_execute_batch", &[obj_value, sql])
                        .map(Some)
                }
                "execute_many" => {
                    let (sql, rows) = gen_two_expr(self, args)?;
                    self.build_net_call("mux_sql_transaction_execute_many", &[obj_value, sql, rows])
                        .map(Some)
                }
                "execute_params" => {
                    let (sql, params) = gen_two_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_transaction_execute_params",
                        &[obj_value, sql, params],
                    )
                    .map(Some)
                }
                "execute_named" => {
                    let (sql, params) = gen_two_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_transaction_execute_named",
                        &[obj_value, sql, params],
                    )
                    .map(Some)
                }
                "query" => {
                    let sql = gen_one_expr(self, args)?;
                    self.build_net_call("mux_sql_transaction_query", &[obj_value, sql])
                        .map(Some)
                }
                "query_params" => {
                    let (sql, params) = gen_two_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_transaction_query_params",
                        &[obj_value, sql, params],
                    )
                    .map(Some)
                }
                "query_named" => {
                    let (sql, params) = gen_two_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_transaction_query_named",
                        &[obj_value, sql, params],
                    )
                    .map(Some)
                }
                "query_with_timeout" => {
                    let (sql, timeout) = gen_two_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_transaction_query_with_timeout",
                        &[obj_value, sql, timeout],
                    )
                    .map(Some)
                }
                "query_params_with_timeout" => {
                    let (sql, params, timeout) = gen_three_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_transaction_query_params_with_timeout",
                        &[obj_value, sql, params, timeout],
                    )
                    .map(Some)
                }
                "query_named_with_timeout" => {
                    let (sql, params, timeout) = gen_three_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_transaction_query_named_with_timeout",
                        &[obj_value, sql, params, timeout],
                    )
                    .map(Some)
                }
                "query_with_cancellation" => {
                    let (sql, token) = gen_two_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_transaction_query_with_cancellation",
                        &[obj_value, sql, token],
                    )
                    .map(Some)
                }
                "query_params_with_cancellation" => {
                    let (sql, params, token) = gen_three_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_transaction_query_params_with_cancellation",
                        &[obj_value, sql, params, token],
                    )
                    .map(Some)
                }
                "query_named_with_cancellation" => {
                    let (sql, params, token) = gen_three_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_transaction_query_named_with_cancellation",
                        &[obj_value, sql, params, token],
                    )
                    .map(Some)
                }
                _ => Ok(None),
            },
            "ResultSet" => match method_name {
                "close" => {
                    self.ensure_no_args("close", args)?;
                    self.build_net_call("mux_sql_resultset_close", &[obj_value])
                        .map(Some)
                }
                "rows" => {
                    self.ensure_no_args("rows", args)?;
                    self.build_net_call("mux_sql_resultset_rows", &[obj_value])
                        .map(Some)
                }
                "next" => {
                    self.ensure_no_args("next", args)?;
                    self.build_net_call("mux_sql_resultset_next", &[obj_value])
                        .map(Some)
                }
                "next_batch" => {
                    let limit = gen_one_expr(self, args)?;
                    self.build_net_call("mux_sql_resultset_next_batch", &[obj_value, limit])
                        .map(Some)
                }
                "columns" => {
                    self.ensure_no_args("columns", args)?;
                    self.build_net_call("mux_sql_resultset_columns", &[obj_value])
                        .map(Some)
                }
                _ => Ok(None),
            },
            "Row" => match method_name {
                "columns" | "values" => {
                    self.ensure_no_args(method_name, args)?;
                    let runtime_name = if method_name == "columns" {
                        "mux_sql_row_columns"
                    } else {
                        "mux_sql_row_values"
                    };
                    self.build_net_call(runtime_name, &[obj_value]).map(Some)
                }
                "at" => {
                    let index = gen_one_expr(self, args)?;
                    self.build_net_call("mux_sql_row_at", &[obj_value, index])
                        .map(Some)
                }
                "get" => {
                    let name = gen_one_expr(self, args)?;
                    self.build_net_call("mux_sql_row_get", &[obj_value, name])
                        .map(Some)
                }
                _ => Ok(None),
            },
            "PreparedStatement" => match method_name {
                "close" => {
                    self.ensure_no_args("close", args)?;
                    self.build_net_call("mux_sql_prepared_close", &[obj_value])
                        .map(Some)
                }
                "execute" | "execute_named" | "query" | "query_named" => {
                    let params = gen_one_expr(self, args)?;
                    let runtime_name = match method_name {
                        "execute" => "mux_sql_prepared_execute",
                        "execute_named" => "mux_sql_prepared_execute_named",
                        "query" => "mux_sql_prepared_query",
                        "query_named" => "mux_sql_prepared_query_named",
                        _ => unreachable!(),
                    };
                    self.build_net_call(runtime_name, &[obj_value, params])
                        .map(Some)
                }
                "query_with_timeout" => {
                    let (params, timeout) = gen_two_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_prepared_query_with_timeout",
                        &[obj_value, params, timeout],
                    )
                    .map(Some)
                }
                "query_named_with_timeout" => {
                    let (params, timeout) = gen_two_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_prepared_query_named_with_timeout",
                        &[obj_value, params, timeout],
                    )
                    .map(Some)
                }
                "query_with_cancellation" => {
                    let (params, token) = gen_two_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_prepared_query_with_cancellation",
                        &[obj_value, params, token],
                    )
                    .map(Some)
                }
                "query_named_with_cancellation" => {
                    let (params, token) = gen_two_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_prepared_query_named_with_cancellation",
                        &[obj_value, params, token],
                    )
                    .map(Some)
                }
                _ => Ok(None),
            },
            "SqlError" => match method_name {
                "message" => {
                    self.ensure_no_args("message", args)?;
                    self.build_net_call("mux_sql_error_message", &[obj_value])
                        .map(Some)
                }
                "to_string" => {
                    self.ensure_no_args("to_string", args)?;
                    self.build_net_call("mux_sql_error_to_string", &[obj_value])
                        .map(Some)
                }
                _ => Ok(None),
            },
            _ => Ok(None),
        }
    }

    pub(super) fn try_generate_sql_static_method_call(
        &mut self,
        class_name: &str,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let (runtime_name, expected_args) = match (class_name, method_name) {
            ("Pool", "from_config") => ("mux_sql_pool_from_config", 3),
            ("Migration", "from_config") => ("mux_sql_migration_from_config", 4),
            ("Migrator", "from_migrations") => ("mux_sql_migrator_from_migrations", 2),
            ("Migrator", "from_directory") => ("mux_sql_migrator_from_directory", 2),
            ("SqlError", "from_message") => ("mux_sql_error_from_message", 1),
            _ => return Ok(None),
        };
        if args.len() != expected_args {
            return Err(format!(
                "{method_name}() method takes exactly {expected_args} argument(s)"
            ));
        }
        let values = args
            .iter()
            .map(|arg| self.generate_expression(arg))
            .collect::<Result<Vec<_>, _>>()?;
        self.build_net_call(runtime_name, &values).map(Some)
    }

    pub(super) fn generate_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        obj_type: &Type,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        let resolved_obj_type = self
            .resolve_type(obj_type)
            .map_err(|e| format!("Unresolved receiver type for method '{method_name}': {e}"))?;

        if let Some(call) = self.try_generate_bytes_cursor_instance_method_call(
            obj_value,
            &resolved_obj_type,
            method_name,
            args,
        )? {
            return Ok(call);
        }

        if let Some(call) = self.try_generate_process_instance_method_call(
            obj_value,
            &resolved_obj_type,
            method_name,
            args,
        )? {
            return Ok(call);
        }

        if let Some(call) = self.try_generate_regex_instance_method_call(
            obj_value,
            &resolved_obj_type,
            method_name,
            args,
        )? {
            return Ok(call);
        }

        if let Some(call) = self.try_generate_uuid_instance_method_call(
            obj_value,
            &resolved_obj_type,
            method_name,
            args,
        )? {
            return Ok(call);
        }

        if let Some(call) = self.try_generate_url_instance_method_call(
            obj_value,
            &resolved_obj_type,
            method_name,
            args,
        )? {
            return Ok(call);
        }

        if let Some(call) = self.try_generate_log_instance_method_call(
            obj_value,
            &resolved_obj_type,
            method_name,
            args,
        )? {
            return Ok(call);
        }

        if let Some(call) = self.try_generate_random_instance_method_call(
            obj_value,
            &resolved_obj_type,
            method_name,
            args,
        )? {
            return Ok(call);
        }

        if let Some(call) = self.try_generate_crypto_instance_method_call(
            obj_value,
            &resolved_obj_type,
            method_name,
            args,
        )? {
            return Ok(call);
        }

        if let Some(call) = self.try_generate_math_instance_method_call(
            obj_value,
            &resolved_obj_type,
            method_name,
            args,
        )? {
            return Ok(call);
        }

        if let Some(call) = self.try_generate_io_instance_method_call(
            obj_value,
            &resolved_obj_type,
            method_name,
            args,
        )? {
            return Ok(call);
        }

        if let Some(call) = self.try_generate_csv_instance_method_call(
            obj_value,
            &resolved_obj_type,
            method_name,
            args,
        )? {
            return Ok(call);
        }

        if let Some(call) = self.try_generate_json_token_instance_method_call(
            obj_value,
            &resolved_obj_type,
            method_name,
            args,
        )? {
            return Ok(call);
        }

        if let Some(call) = self.try_generate_cli_instance_method_call(
            obj_value,
            &resolved_obj_type,
            method_name,
            args,
        )? {
            return Ok(call);
        }

        if let Some(call) = self.try_generate_net_instance_method_call(
            obj_value,
            &resolved_obj_type,
            method_name,
            args,
        )? {
            return Ok(call);
        }

        if let Some(call) = self.try_generate_tls_instance_method_call(
            obj_value,
            &resolved_obj_type,
            method_name,
            args,
        )? {
            return Ok(call);
        }

        if let Some(call) = self.try_generate_tls_config_instance_method_call(
            obj_value,
            &resolved_obj_type,
            method_name,
            args,
        )? {
            return Ok(call);
        }

        if let Some(call) = self.try_generate_datetime_instance_method_call(
            obj_value,
            &resolved_obj_type,
            method_name,
            args,
        )? {
            return Ok(call);
        }

        if let Some(call) = self.try_generate_sync_instance_method_call(
            obj_value,
            &resolved_obj_type,
            method_name,
            args,
        )? {
            return Ok(call);
        }

        if let Some(call) = self.try_generate_sql_instance_method_call(
            obj_value,
            &resolved_obj_type,
            method_name,
            args,
        )? {
            return Ok(call);
        }

        match &resolved_obj_type {
            Type::Primitive(prim) => {
                self.generate_primitive_method_call(obj_value, prim, method_name, args)
            }
            Type::List(_) => self.generate_list_method_call(obj_value, method_name, args),
            Type::Map(key_type, value_type) => {
                self.generate_map_method_call(obj_value, key_type, value_type, method_name, args)
            }
            Type::Set(elem_type) => {
                self.generate_set_method_call(obj_value, elem_type, method_name, args)
            }
            Type::Tuple(_, _) => self.generate_tuple_method_call(obj_value, method_name, args),
            Type::Named(name, _type_args) if name == "Csv" => {
                self.generate_csv_method_call(obj_value, method_name, args)
            }
            Type::Named(name, _type_args) if name == "Json" => {
                self.generate_json_method_call(obj_value, method_name, args)
            }
            Type::Named(name, _type_args) if name == "JsonNumber" => {
                self.generate_json_number_method_call(obj_value, method_name, args)
            }
            Type::Named(name, _type_args) if name == "SqlValue" => {
                self.generate_sql_value_method_call(obj_value, method_name, args)
            }
            Type::Named(name, type_args) => {
                self.invoke_class_instance_method(name, type_args, obj_value, method_name, args)
            }
            Type::Optional(inner) => {
                self.generate_optional_method_call(obj_value, inner, method_name, args)
            }
            Type::Result(ok, error) => {
                self.generate_result_method_call(obj_value, ok, error, method_name, args)
            }
            Type::Reference(inner) => {
                // Load the *mut Value box through the reference slot, then
                // extract the raw scalar for primitives (matching the
                // generate_deref_unary_expression pattern).
                let ptr_type = self.context.ptr_type(AddressSpace::default());
                let boxed_ptr = self
                    .builder
                    .build_load(ptr_type, obj_value.into_pointer_value(), "ref_load")
                    .map_err(|e| e.to_string())?;
                let loaded = match inner.as_ref() {
                    Type::Primitive(PrimitiveType::Int | PrimitiveType::Byte) => {
                        self.get_raw_int_value(boxed_ptr).map(Into::into)?
                    }
                    Type::Primitive(PrimitiveType::Float) => {
                        self.get_raw_float_value(boxed_ptr).map(Into::into)?
                    }
                    Type::Primitive(PrimitiveType::Bool) => {
                        self.get_raw_bool_value(boxed_ptr).map(Into::into)?
                    }
                    _ => boxed_ptr,
                };
                self.generate_method_call(loaded, inner, method_name, args)
            }
            Type::TraitObject(target) => {
                self.invoke_trait_object_method(obj_value, target, method_name, args)
            }
            _ => Err(format!(
                "Method {} not implemented for type {}",
                method_name,
                format_type(&resolved_obj_type)
            )),
        }
    }

    fn generate_primitive_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        prim: &PrimitiveType,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        match prim {
            PrimitiveType::Int => match method_name {
                "to_string" => self.call_runtime_to_string(obj_value, "mux_int_to_string"),
                "to_float" => {
                    let float_val = self
                        .builder
                        .build_signed_int_to_float(
                            obj_value.into_int_value(),
                            self.context.f64_type(),
                            "int_to_float",
                        )
                        .map_err(|e| e.to_string())?;
                    Ok(float_val.into())
                }
                "to_int" | "to_char" => Ok(obj_value),
                "to_byte" => self.call_runtime_function("mux_int_to_byte", &[obj_value]),
                "eq" => self.generate_int_equality_method(obj_value, args),
                "cmp" => self.generate_int_order_method(obj_value, args, true),
                "hash" => {
                    self.ensure_no_args("hash", args)?;
                    Ok(obj_value)
                }
                _ => Err(format!(
                    "Method {method_name} not implemented for integer scalar"
                )),
            },
            PrimitiveType::Byte => match method_name {
                "eq" => self.generate_int_equality_method(obj_value, args),
                "cmp" => self.generate_int_order_method(obj_value, args, false),
                "hash" => {
                    self.ensure_no_args("hash", args)?;
                    Ok(obj_value)
                }
                _ => self.generate_byte_method_call(obj_value, method_name, args),
            },
            PrimitiveType::Bytes => self.generate_bytes_method_call(obj_value, method_name, args),
            PrimitiveType::Float => match method_name {
                "to_string" => self.call_runtime_to_string(obj_value, "mux_float_to_string"),
                "to_int" => {
                    let float_val = obj_value.into_float_value();
                    let int_val = self
                        .builder
                        .build_float_to_signed_int(
                            float_val,
                            self.context.i64_type(),
                            "float_to_int",
                        )
                        .map_err(|e| e.to_string())?;
                    Ok(int_val.into())
                }
                "to_float" => Ok(obj_value),
                "eq" => self.generate_float_equality_method(obj_value, args),
                "cmp" => self.generate_float_order_method(obj_value, args),
                "hash" => {
                    self.ensure_no_args("hash", args)?;
                    self.call_runtime_function("mux_float_hash", &[obj_value])
                }
                _ => Err(format!("Method {method_name} not implemented for float")),
            },
            PrimitiveType::Str => match method_name {
                "to_string" => self.call_runtime_to_string(obj_value, "mux_value_to_string"),
                "message" => Ok(obj_value),
                "to_int" => self.call_string_conversion_func(obj_value, "mux_string_to_int"),
                "to_float" => self.call_string_conversion_func(obj_value, "mux_string_to_float"),
                "to_char" => self.call_string_conversion_func(obj_value, "mux_string_to_char"),
                "to_byte" => self.call_string_conversion_func(obj_value, "mux_string_to_byte"),
                "eq" => self.generate_string_equality_method(obj_value, args),
                "hash" => self.generate_string_hash_method(obj_value, args),
                // mux_string_length takes a raw C string, so unwrap the
                // boxed value first like the conversion functions do.
                "length" => self.call_string_conversion_func(obj_value, "mux_string_length"),
                // Declared in `get_string_method_sig` and, until now, never
                // implemented - so `"a".cmp("b")` was an internal compiler
                // error, while the declaration alone was enough to let `string`
                // satisfy the `Comparable` bound and reach a broken `sort`.
                "cmp" => {
                    let other = gen_one_expr(self, args)?;
                    let left = self.string_value_to_cstr(obj_value)?;
                    let right = self.string_value_to_cstr(other)?;
                    let func = self
                        .runtime_function("mux_string_compare")
                        .ok_or("mux_string_compare not found".to_string())?;
                    let ordering = self
                        .builder
                        .build_call(func, &[left.into(), right.into()], "str_cmp")
                        .map_err(|e| e.to_string())?
                        .try_as_basic_value()
                        .basic()
                        .ok_or_else(|| "mux_string_compare should return a value".to_string())?;
                    // Both C strings are owned copies from mux_value_to_string.
                    let free_fn = self
                        .runtime_function("mux_free_string")
                        .ok_or("mux_free_string not found".to_string())?;
                    for cstr in [left, right] {
                        self.builder
                            .build_call(free_fn, &[cstr.into()], "free_cstr")
                            .map_err(|e| e.to_string())?;
                    }
                    Ok(ordering)
                }
                // Transforms: take the receiver's C string, hand back an owned
                // one, wrap it as a Mux string.
                "trim" | "to_upper" | "to_lower" => {
                    self.ensure_no_args(method_name, args)?;
                    let recv = self.string_value_to_cstr(obj_value)?;
                    let out =
                        self.call_runtime_function(&format!("mux_string_{method_name}"), &[recv])?;
                    self.free_cstrings(&[recv])?;
                    self.call_cstr_to_mux_string(out)
                }
                "split" => {
                    let sep = gen_one_expr(self, args)?;
                    let recv = self.string_value_to_cstr(obj_value)?;
                    let sep_cstr = self.string_value_to_cstr(sep)?;
                    let list = self.call_runtime_function("mux_string_split", &[recv, sep_cstr])?;
                    self.free_cstrings(&[recv, sep_cstr])?;
                    self.register_temp(list);
                    Ok(list)
                }
                "to_list" => {
                    self.ensure_no_args("to_list", args)?;
                    let recv = self.string_value_to_cstr(obj_value)?;
                    let list = self.call_runtime_function("mux_string_to_list", &[recv])?;
                    self.free_cstrings(&[recv])?;
                    self.register_temp(list);
                    Ok(list)
                }
                "char_at" => {
                    let index = gen_one_expr(self, args)?;
                    let recv = self.string_value_to_cstr(obj_value)?;
                    let raw_index = self.get_raw_int_value(index)?;
                    let opt = self
                        .call_runtime_function("mux_string_char_at", &[recv, raw_index.into()])?;
                    self.free_cstrings(&[recv])?;
                    self.register_temp(opt);
                    Ok(opt)
                }
                "substring" => {
                    let (start, end) = gen_two_expr(self, args)?;
                    let recv = self.string_value_to_cstr(obj_value)?;
                    let raw_start = self.get_raw_int_value(start)?;
                    let raw_end = self.get_raw_int_value(end)?;
                    let out = self.call_runtime_function(
                        "mux_string_slice",
                        &[recv, raw_start.into(), raw_end.into()],
                    )?;
                    self.free_cstrings(&[recv])?;
                    self.call_cstr_to_mux_string(out)
                }
                // mux_string_contains takes Values rather than C strings, so
                // this one does not unwrap its operands.
                "contains" => {
                    let needle = gen_one_expr(self, args)?;
                    self.call_runtime_function("mux_string_contains", &[obj_value, needle])
                }
                "starts_with" | "ends_with" => {
                    let other = gen_one_expr(self, args)?;
                    let recv = self.string_value_to_cstr(obj_value)?;
                    let other_cstr = self.string_value_to_cstr(other)?;
                    let result = self.call_runtime_function(
                        &format!("mux_string_{method_name}"),
                        &[recv, other_cstr],
                    )?;
                    self.free_cstrings(&[recv, other_cstr])?;
                    Ok(result)
                }
                "index_of" => {
                    let needle = gen_one_expr(self, args)?;
                    let recv = self.string_value_to_cstr(obj_value)?;
                    let needle_cstr = self.string_value_to_cstr(needle)?;
                    let result =
                        self.call_runtime_function("mux_string_index_of", &[recv, needle_cstr])?;
                    self.free_cstrings(&[recv, needle_cstr])?;
                    Ok(result)
                }
                "replace" => {
                    let (from, to) = gen_two_expr(self, args)?;
                    let recv = self.string_value_to_cstr(obj_value)?;
                    let from_cstr = self.string_value_to_cstr(from)?;
                    let to_cstr = self.string_value_to_cstr(to)?;
                    let out = self
                        .call_runtime_function("mux_string_replace", &[recv, from_cstr, to_cstr])?;
                    self.free_cstrings(&[recv, from_cstr, to_cstr])?;
                    self.call_cstr_to_mux_string(out)
                }
                _ => Err(format!("Method {method_name} not implemented for string")),
            },
            PrimitiveType::Bool => match method_name {
                "to_string" => {
                    let bool_i32 = self.bool_to_i32(obj_value)?;
                    let bool_func = self
                        .runtime_function("mux_bool_to_string")
                        .ok_or("mux_bool_to_string not found".to_string())?;
                    let call = self
                        .builder
                        .build_call(bool_func, &[bool_i32.into()], "bool_to_str")
                        .map_err(|e| e.to_string())?;
                    self.call_runtime_to_string_from_call(call)
                }
                "to_int" => {
                    let bool_i32 = self.bool_to_i32(obj_value)?.into_int_value();
                    let int_val = self
                        .builder
                        .build_int_z_extend(bool_i32, self.context.i64_type(), "bool_to_int")
                        .map_err(|e| e.to_string())?;
                    Ok(int_val.into())
                }
                "to_float" => {
                    let bool_i32 = self.bool_to_i32(obj_value)?.into_int_value();
                    let float_val = self
                        .builder
                        .build_unsigned_int_to_float(
                            bool_i32,
                            self.context.f64_type(),
                            "bool_to_float",
                        )
                        .map_err(|e| e.to_string())?;
                    Ok(float_val.into())
                }
                "eq" => {
                    let other = gen_one_expr(self, args)?;
                    let left = self.get_raw_bool_value(obj_value)?;
                    let right = self.get_raw_bool_value(other)?;
                    self.builder
                        .build_int_compare(inkwell::IntPredicate::EQ, left, right, "bool_eq")
                        .map(Into::into)
                        .map_err(|e| e.to_string())
                }
                "hash" => {
                    self.ensure_no_args("hash", args)?;
                    let value = self.get_raw_bool_value(obj_value)?;
                    self.builder
                        .build_int_z_extend(value, self.context.i64_type(), "bool_hash")
                        .map(Into::into)
                        .map_err(|e| e.to_string())
                }
                _ => Err(format!("Method {method_name} not implemented for bool")),
            },
            PrimitiveType::Char => match method_name {
                "to_string" => self.call_runtime_to_string(obj_value, "mux_char_to_string"),
                "to_int" => self.call_runtime_function("mux_char_to_int", &[obj_value]),
                "to_codepoint" => {
                    self.ensure_no_args("to_codepoint", args)?;
                    self.call_runtime_function("mux_char_to_codepoint", &[obj_value])
                }
                "to_char" => Ok(obj_value),
                "eq" => self.generate_int_equality_method(obj_value, args),
                "cmp" => self.generate_int_order_method(obj_value, args, false),
                "hash" => {
                    self.ensure_no_args("hash", args)?;
                    Ok(obj_value)
                }
                _ => Err(format!("Method {method_name} not implemented for char")),
            },
            _ => Err(format!(
                "Method {method_name} not implemented for primitive type {prim:?}"
            )),
        }
    }

    fn generate_int_equality_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        let other = gen_one_expr(self, args)?;
        let left = self.get_raw_int_value(obj_value)?;
        let right = self.get_raw_int_value(other)?;
        self.builder
            .build_int_compare(inkwell::IntPredicate::EQ, left, right, "int_eq")
            .map(Into::into)
            .map_err(|e| e.to_string())
    }

    fn generate_int_order_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        args: &[ExpressionNode],
        signed: bool,
    ) -> Result<BasicValueEnum<'a>, String> {
        let other = gen_one_expr(self, args)?;
        let left = self.get_raw_int_value(obj_value)?;
        let right = self.get_raw_int_value(other)?;
        let (less_pred, greater_pred) = if signed {
            (inkwell::IntPredicate::SLT, inkwell::IntPredicate::SGT)
        } else {
            (inkwell::IntPredicate::ULT, inkwell::IntPredicate::UGT)
        };
        let less = self
            .builder
            .build_int_compare(less_pred, left, right, "int_cmp_less")
            .map_err(|e| e.to_string())?;
        let greater = self
            .builder
            .build_int_compare(greater_pred, left, right, "int_cmp_greater")
            .map_err(|e| e.to_string())?;
        self.generate_three_way_result(less, greater)
    }

    fn generate_float_equality_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        let other = gen_one_expr(self, args)?;
        let left = self.get_raw_float_value(obj_value)?;
        let right = self.get_raw_float_value(other)?;
        self.builder
            .build_float_compare(inkwell::FloatPredicate::OEQ, left, right, "float_eq")
            .map(Into::into)
            .map_err(|e| e.to_string())
    }

    fn generate_float_order_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        let other = gen_one_expr(self, args)?;
        let left = self.get_raw_float_value(obj_value)?;
        let right = self.get_raw_float_value(other)?;
        let less = self
            .builder
            .build_float_compare(inkwell::FloatPredicate::OLT, left, right, "float_cmp_less")
            .map_err(|e| e.to_string())?;
        let greater = self
            .builder
            .build_float_compare(
                inkwell::FloatPredicate::OGT,
                left,
                right,
                "float_cmp_greater",
            )
            .map_err(|e| e.to_string())?;
        self.generate_three_way_result(less, greater)
    }

    fn generate_three_way_result(
        &mut self,
        less: inkwell::values::IntValue<'a>,
        greater: inkwell::values::IntValue<'a>,
    ) -> Result<BasicValueEnum<'a>, String> {
        let int_type = self.context.i64_type();
        let neg_one = int_type.const_int((-1i64) as u64, true);
        let zero = int_type.const_zero();
        let one = int_type.const_int(1, false);
        let positive = self
            .builder
            .build_select(greater, one, zero, "cmp_positive")
            .map_err(|e| e.to_string())?;
        self.builder
            .build_select(less, neg_one, positive.into_int_value(), "cmp_result")
            .map_err(|e| e.to_string())
    }

    fn generate_string_equality_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        let other = gen_one_expr(self, args)?;
        let left = self.string_value_to_cstr(obj_value)?;
        let right = self.string_value_to_cstr(other)?;
        let result = self.call_runtime_function("mux_string_equal", &[left, right])?;
        self.free_cstrings(&[left, right])?;
        self.i32_to_bool(result.into_int_value())
    }

    fn generate_string_hash_method(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        self.ensure_no_args("hash", args)?;
        let value = self.string_value_to_cstr(obj_value)?;
        let result = self.call_runtime_function("mux_string_hash", &[value])?;
        self.free_cstrings(&[value])?;
        Ok(result)
    }

    fn generate_byte_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        match method_name {
            "to_string" => self.call_runtime_to_string(obj_value, "mux_int_to_string"),
            "to_int" | "to_byte" => Ok(obj_value),
            "bit_not" => {
                self.ensure_arg_count(method_name, args, 0)?;
                self.call_runtime_function("mux_byte_bit_not", &[obj_value])
            }
            "checked_add" | "checked_sub" | "checked_mul" | "checked_div" | "checked_rem"
            | "wrapping_add" | "wrapping_sub" | "wrapping_mul" | "saturating_add"
            | "saturating_sub" | "saturating_mul" | "bit_and" | "bit_or" | "bit_xor"
            | "rotate_left" | "rotate_right" | "shift_left" | "shift_right" => {
                self.ensure_arg_count(method_name, args, 1)?;
                let other = self.generate_expression(&args[0])?;
                let runtime_name = format!("mux_byte_{method_name}");
                self.call_runtime_function(&runtime_name, &[obj_value, other])
            }
            _ => Err(format!("Method {method_name} not implemented for byte")),
        }
    }

    fn generate_bytes_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        match method_name {
            "to_string" => {
                self.ensure_no_args("to_string", args)?;
                self.call_runtime_to_string(obj_value, "mux_value_to_string")
            }
            "to_list" => {
                self.ensure_no_args("to_list", args)?;
                self.call_runtime_function("mux_bytes_to_list", &[obj_value])
            }
            "to_utf8" => {
                self.ensure_no_args("to_utf8", args)?;
                self.call_runtime_function("mux_bytes_to_utf8", &[obj_value])
            }
            "to_utf8_lossy" => {
                self.ensure_no_args("to_utf8_lossy", args)?;
                self.call_runtime_function("mux_bytes_to_utf8_lossy", &[obj_value])
            }
            "cursor" => {
                self.ensure_no_args("cursor", args)?;
                self.call_runtime_function("mux_bytes_cursor_new", &[obj_value])
            }
            "len" | "size" => {
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function("mux_bytes_length", &[obj_value])
            }
            "is_empty" => {
                self.ensure_no_args("is_empty", args)?;
                self.call_runtime_function("mux_bytes_is_empty", &[obj_value])
            }
            "get" => {
                self.ensure_arg_count("get", args, 1)?;
                let index_value = self.generate_expression(&args[0])?;
                let index = self.get_raw_int_value(index_value)?;
                self.call_runtime_function("mux_bytes_get", &[obj_value, index.into()])
            }
            "push_back" | "push_front" => {
                self.ensure_arg_count(method_name, args, 1)?;
                let byte_value = self.generate_expression(&args[0])?;
                let byte = self.box_value(byte_value);
                let runtime_name = if method_name == "push_back" {
                    "mux_bytes_push_back"
                } else {
                    "mux_bytes_push_front"
                };
                self.generate_runtime_call(runtime_name, &[obj_value.into(), byte.into()]);
                Ok(self.context.i32_type().const_zero().into())
            }
            "pop_back" | "pop_front" => {
                self.ensure_no_args(method_name, args)?;
                let runtime_name = if method_name == "pop_back" {
                    "mux_bytes_pop_back"
                } else {
                    "mux_bytes_pop_front"
                };
                self.call_runtime_function(runtime_name, &[obj_value])
            }
            "clear" => {
                self.ensure_no_args("clear", args)?;
                self.generate_runtime_call("mux_bytes_clear", &[obj_value.into()]);
                Ok(self.context.i32_type().const_zero().into())
            }
            "reserve" | "truncate" => {
                self.ensure_arg_count(method_name, args, 1)?;
                let length_value = self.generate_expression(&args[0])?;
                let length = self.get_raw_int_value(length_value)?;
                let runtime_name = if method_name == "reserve" {
                    "mux_bytes_reserve"
                } else {
                    "mux_bytes_truncate"
                };
                self.generate_runtime_call(runtime_name, &[obj_value.into(), length.into()]);
                Ok(self.context.i32_type().const_zero().into())
            }
            "resize" => {
                self.ensure_arg_count(method_name, args, 2)?;
                let length_value = self.generate_expression(&args[0])?;
                let length = self.get_raw_int_value(length_value)?;
                let fill_value = self.generate_expression(&args[1])?;
                let fill = self.box_value(fill_value);
                self.generate_runtime_call(
                    "mux_bytes_resize",
                    &[obj_value.into(), length.into(), fill.into()],
                );
                Ok(self.context.i32_type().const_zero().into())
            }
            "fill" => {
                self.ensure_arg_count("fill", args, 1)?;
                let fill_value = self.generate_expression(&args[0])?;
                let fill = self.box_value(fill_value);
                self.generate_runtime_call("mux_bytes_fill", &[obj_value.into(), fill.into()]);
                Ok(self.context.i32_type().const_zero().into())
            }
            "extend" => {
                self.ensure_arg_count("extend", args, 1)?;
                let other = self.generate_expression(&args[0])?;
                self.generate_runtime_call("mux_bytes_extend", &[obj_value.into(), other.into()]);
                Ok(self.context.i32_type().const_zero().into())
            }
            "contains" => {
                self.ensure_arg_count("contains", args, 1)?;
                let byte_value = self.generate_expression(&args[0])?;
                let byte = self.box_value(byte_value);
                self.call_runtime_function("mux_bytes_contains", &[obj_value, byte.into()])
            }
            "find" => {
                self.ensure_arg_count("find", args, 1)?;
                let byte_value = self.generate_expression(&args[0])?;
                let byte = self.box_value(byte_value);
                self.call_runtime_function("mux_bytes_find", &[obj_value, byte.into()])
            }
            "insert" => {
                self.ensure_arg_count("insert", args, 2)?;
                let index_value = self.generate_expression(&args[0])?;
                let index = self.get_raw_int_value(index_value)?;
                let byte_value = self.generate_expression(&args[1])?;
                let byte = self.box_value(byte_value);
                self.generate_runtime_call(
                    "mux_bytes_insert",
                    &[obj_value.into(), index.into(), byte.into()],
                );
                Ok(self.context.i32_type().const_zero().into())
            }
            "remove" => {
                self.ensure_arg_count("remove", args, 1)?;
                let index_value = self.generate_expression(&args[0])?;
                let index = self.get_raw_int_value(index_value)?;
                self.call_runtime_function("mux_bytes_remove", &[obj_value, index.into()])
            }
            "copy_within" => {
                self.ensure_arg_count("copy_within", args, 3)?;
                let destination_value = self.generate_expression(&args[0])?;
                let destination = self.get_raw_int_value(destination_value)?;
                let source_value = self.generate_expression(&args[1])?;
                let source = self.get_raw_int_value(source_value)?;
                let length_value = self.generate_expression(&args[2])?;
                let length = self.get_raw_int_value(length_value)?;
                self.generate_runtime_call(
                    "mux_bytes_copy_within",
                    &[
                        obj_value.into(),
                        destination.into(),
                        source.into(),
                        length.into(),
                    ],
                );
                Ok(self.context.i32_type().const_zero().into())
            }
            "format" | "to_binary" | "to_octal" | "to_decimal" | "to_hex" => {
                let (radix, width) = match method_name {
                    "to_binary" => (2, 8),
                    "to_octal" => (8, 3),
                    "to_decimal" => (10, 3),
                    "to_hex" => (16, 2),
                    _ => {
                        self.ensure_arg_count("format", args, 2)?;
                        let radix_value = self.generate_expression(&args[0])?;
                        let radix = self.get_raw_int_value(radix_value)?;
                        let width_value = self.generate_expression(&args[1])?;
                        let width = self.get_raw_int_value(width_value)?;
                        return self.call_runtime_function(
                            "mux_bytes_format",
                            &[obj_value, radix.into(), width.into()],
                        );
                    }
                };
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function(
                    "mux_bytes_format",
                    &[
                        obj_value,
                        self.context.i64_type().const_int(radix, false).into(),
                        self.context.i64_type().const_int(width, false).into(),
                    ],
                )
            }
            "read_uint_le" | "read_uint_be" => {
                self.ensure_arg_count(method_name, args, 2)?;
                let offset_value = self.generate_expression(&args[0])?;
                let offset = self.get_raw_int_value(offset_value)?;
                let width_value = self.generate_expression(&args[1])?;
                let width = self.get_raw_int_value(width_value)?;
                let little = method_name.ends_with("_le");
                self.call_runtime_function(
                    "mux_bytes_read_uint",
                    &[
                        obj_value,
                        offset.into(),
                        width.into(),
                        self.context
                            .bool_type()
                            .const_int(u64::from(little), false)
                            .into(),
                    ],
                )
            }
            "write_uint_le" | "write_uint_be" => {
                self.ensure_arg_count(method_name, args, 3)?;
                let offset_value = self.generate_expression(&args[0])?;
                let offset = self.get_raw_int_value(offset_value)?;
                let number_value = self.generate_expression(&args[1])?;
                let number = self.get_raw_int_value(number_value)?;
                let width_value = self.generate_expression(&args[2])?;
                let width = self.get_raw_int_value(width_value)?;
                let little = method_name.ends_with("_le");
                let little_value = self.context.bool_type().const_int(u64::from(little), false);
                self.call_runtime_function(
                    "mux_bytes_write_uint",
                    &[
                        obj_value,
                        offset.into(),
                        number.into(),
                        width.into(),
                        little_value.into(),
                    ],
                )
            }
            "read_float_le" | "read_float_be" => {
                self.ensure_arg_count(method_name, args, 1)?;
                let offset_value = self.generate_expression(&args[0])?;
                let offset = self.get_raw_int_value(offset_value)?;
                let little = method_name.ends_with("_le");
                self.call_runtime_function(
                    "mux_bytes_read_float",
                    &[
                        obj_value,
                        offset.into(),
                        self.context
                            .bool_type()
                            .const_int(u64::from(little), false)
                            .into(),
                    ],
                )
            }
            "write_float_le" | "write_float_be" => {
                self.ensure_arg_count(method_name, args, 2)?;
                let offset_value = self.generate_expression(&args[0])?;
                let offset = self.get_raw_int_value(offset_value)?;
                let number_value = self.generate_expression(&args[1])?;
                let number = self.box_value(number_value);
                let little = method_name.ends_with("_le");
                let little_value = self.context.bool_type().const_int(u64::from(little), false);
                self.call_runtime_function(
                    "mux_bytes_write_float",
                    &[obj_value, offset.into(), number.into(), little_value.into()],
                )
            }
            "read_varint" => {
                self.ensure_arg_count("read_varint", args, 1)?;
                let offset_value = self.generate_expression(&args[0])?;
                let offset = self.get_raw_int_value(offset_value)?;
                self.call_runtime_function("mux_bytes_read_varint", &[obj_value, offset.into()])
            }
            "write_varint" => {
                self.ensure_arg_count("write_varint", args, 2)?;
                let offset_value = self.generate_expression(&args[0])?;
                let offset = self.get_raw_int_value(offset_value)?;
                let number_value = self.generate_expression(&args[1])?;
                let number = self.get_raw_int_value(number_value)?;
                self.call_runtime_function(
                    "mux_bytes_write_varint",
                    &[obj_value, offset.into(), number.into()],
                )
            }
            _ => Err(format!("Method {method_name} not implemented for bytes")),
        }
    }

    pub(super) fn try_generate_bytes_cursor_instance_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        obj_type: &Type,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let Type::Named(type_name, _) = obj_type else {
            return Ok(None);
        };
        if type_name != "BytesCursor" {
            return Ok(None);
        }
        fn one_arg<'b>(
            args: &'b [ExpressionNode],
            name: &str,
        ) -> Result<&'b ExpressionNode, String> {
            if args.len() != 1 {
                Err(format!("{name}() method takes exactly 1 argument"))
            } else {
                Ok(&args[0])
            }
        }
        let call = match method_name {
            "position" | "remaining" | "into_bytes" => {
                self.ensure_no_args(method_name, args)?;
                let runtime_name = match method_name {
                    "position" => "mux_bytes_cursor_position",
                    "remaining" => "mux_bytes_cursor_remaining",
                    "into_bytes" => "mux_bytes_cursor_into_bytes",
                    _ => unreachable!(),
                };
                self.build_net_call(runtime_name, &[obj_value])?
            }
            "read_bytes" => {
                let expr = one_arg(args, method_name)?;
                let generated = self.generate_expression(expr)?;
                let length = self.get_raw_int_value(generated)?;
                self.build_net_call("mux_bytes_cursor_read_bytes", &[obj_value, length.into()])?
            }
            "read_uint_le" | "read_uint_be" => {
                let expr = one_arg(args, method_name)?;
                let generated = self.generate_expression(expr)?;
                let width = self.get_raw_int_value(generated)?;
                let little = method_name.ends_with("_le");
                self.build_net_call(
                    "mux_bytes_cursor_read_uint",
                    &[
                        obj_value,
                        width.into(),
                        self.context
                            .bool_type()
                            .const_int(u64::from(little), false)
                            .into(),
                    ],
                )?
            }
            "write_uint_le" | "write_uint_be" => {
                if args.len() != 2 {
                    return Err(format!("{method_name}() method takes exactly 2 arguments"));
                }
                let number_value = self.generate_expression(&args[0])?;
                let number = self.get_raw_int_value(number_value)?;
                let width_value = self.generate_expression(&args[1])?;
                let width = self.get_raw_int_value(width_value)?;
                let little = method_name.ends_with("_le");
                self.build_net_call(
                    "mux_bytes_cursor_write_uint",
                    &[
                        obj_value,
                        number.into(),
                        width.into(),
                        self.context
                            .bool_type()
                            .const_int(u64::from(little), false)
                            .into(),
                    ],
                )?
            }
            "read_float_le" | "read_float_be" => {
                self.ensure_no_args(method_name, args)?;
                let little = method_name.ends_with("_le");
                self.build_net_call(
                    "mux_bytes_cursor_read_float",
                    &[
                        obj_value,
                        self.context
                            .bool_type()
                            .const_int(u64::from(little), false)
                            .into(),
                    ],
                )?
            }
            "write_float_le" | "write_float_be" => {
                let expr = one_arg(args, method_name)?;
                let generated = self.generate_expression(expr)?;
                let number = self.box_value(generated);
                let little = method_name.ends_with("_le");
                self.build_net_call(
                    "mux_bytes_cursor_write_float",
                    &[
                        obj_value,
                        number.into(),
                        self.context
                            .bool_type()
                            .const_int(u64::from(little), false)
                            .into(),
                    ],
                )?
            }
            _ => return Ok(None),
        };
        Ok(Some(call))
    }

    fn generate_list_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        match method_name {
            "get" => {
                self.ensure_arg_count("get", args, 1)?;
                let index_val = self.generate_expression(&args[0])?;
                self.with_extracted_list(obj_value, |me, raw_list| {
                    let call = me
                        .builder
                        .build_call(
                            me.runtime_function("mux_list_get")
                                .expect("mux_list_get must be declared in runtime"),
                            &[raw_list.into(), index_val.into()],
                            "list_get",
                        )
                        .map_err(|e| e.to_string())?;
                    Ok(call
                        .try_as_basic_value()
                        .basic()
                        .expect("mux_list_get should return a basic value"))
                })
            }
            "push_back" => {
                self.ensure_arg_count("push_back", args, 1)?;
                let elem_val = self.generate_expression(&args[0])?;
                let elem_ptr = self.box_value(elem_val);

                self.generate_runtime_call(
                    "mux_list_push_back_value",
                    &[obj_value.into(), elem_ptr.into()],
                );
                Ok(self.context.i32_type().const_int(0, false).into())
            }
            "pop_back" => {
                self.ensure_no_args("pop_back", args)?;
                self.call_runtime_function("mux_list_pop_back_value", &[obj_value])
            }
            "push" => {
                self.ensure_arg_count("push", args, 1)?;
                let elem_val = self.generate_expression(&args[0])?;
                let elem_ptr = self.box_value(elem_val);

                self.generate_runtime_call(
                    "mux_list_push_value",
                    &[obj_value.into(), elem_ptr.into()],
                );
                Ok(self.context.i32_type().const_int(0, false).into())
            }
            "pop" => {
                self.ensure_no_args("pop", args)?;
                self.call_runtime_function("mux_list_pop_value", &[obj_value])
            }
            "is_empty" => {
                self.ensure_no_args("is_empty", args)?;
                self.with_extracted_list(obj_value, |me, raw_list| {
                    me.call_runtime_function("mux_list_is_empty", &[raw_list])
                })
            }
            "size" | "len" => {
                self.ensure_no_args(method_name, args)?;
                self.with_extracted_list(obj_value, |me, raw_list| {
                    me.call_runtime_function("mux_list_length", &[raw_list])
                })
            }
            "contains" => {
                self.ensure_arg_count("contains", args, 1)?;
                let arg_val = self.generate_expression(&args[0])?;
                let boxed = self.box_value(arg_val);
                self.with_extracted_list(obj_value, |me, raw_list| {
                    me.call_runtime_function("mux_list_contains", &[raw_list, boxed.into()])
                })
            }
            // `Collection<T>` member. A list is already a list, but Mux is
            // value-semantic, so this must hand back an independent deep copy
            // rather than an alias of the receiver - every other implementor
            // allocates (e.g. mux_set_to_list), and returning the receiver
            // would let a mutation through the result be observed on the
            // original. The clone is owned (+1), matching the return
            // convention, so the caller's release cannot underflow.
            "to_list" => {
                self.ensure_no_args("to_list", args)?;
                let cloned = self.deep_clone_value(obj_value.into_pointer_value())?;
                Ok(cloned.into())
            }
            "to_bytes" => {
                self.ensure_no_args("to_bytes", args)?;
                self.call_runtime_function("mux_bytes_from_list", &[obj_value])
            }
            "to_string" => {
                self.ensure_no_args("to_string", args)?;
                self.with_extracted_list(obj_value, |me, raw_list| {
                    let cstr = me.call_runtime_function("mux_list_to_string", &[raw_list])?;
                    me.call_cstr_to_mux_string(cstr)
                })
            }
            _ => Err(format!("Method {method_name} not implemented for lists")),
        }
    }

    fn generate_csv_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        match method_name {
            "stringify" => {
                if !args.is_empty() {
                    return Err("stringify() expects no arguments".to_string());
                }
                self.call_runtime_function("mux_csv_to_string", &[obj_value])
            }
            "stringify_with" => {
                self.ensure_arg_count("stringify_with", args, 2)?;
                let delimiter = self.generate_expression(&args[0])?;
                let quote = self.generate_expression(&args[1])?;
                self.call_runtime_function("mux_csv_to_string_with", &[obj_value, delimiter, quote])
            }
            "to_string" => {
                self.ensure_no_args("to_string", args)?;
                self.call_runtime_function("mux_csv_render", &[obj_value])
            }
            _ => Err(format!("Method {method_name} not implemented for Csv")),
        }
    }

    fn generate_json_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        match method_name {
            "stringify" => {
                if args.len() != 1 {
                    return Err(
                        "stringify() expects exactly 1 argument (optional indent)".to_string()
                    );
                }
                let indent_arg = self.generate_expression(&args[0])?;
                let result =
                    self.call_runtime_function("mux_json_stringify", &[obj_value, indent_arg])?;
                // mux_json_stringify returns an owned result<string,...>; register
                // it for statement-end release unless ownership is transferred.
                self.register_temp(result);
                Ok(result)
            }
            // The total render. Returns an owned string, released at the end of
            // the statement like any other temporary.
            "to_string" => {
                self.ensure_no_args("to_string", args)?;
                let result = self.call_runtime_function("mux_json_to_string", &[obj_value])?;
                self.register_temp(result);
                Ok(result)
            }
            "canonical" => {
                self.ensure_no_args("canonical", args)?;
                let result = self.call_runtime_function("mux_json_canonical", &[obj_value])?;
                self.register_temp(result);
                Ok(result)
            }
            // Typed accessors. Each returns an owned optional<T>, so it is
            // registered for statement-end release the same way stringify's
            // result is.
            "as_string" | "as_int" | "as_float" | "as_bool" | "as_list" | "as_map"
            | "as_number" => {
                self.ensure_no_args(method_name, args)?;
                let result =
                    self.call_runtime_function(&format!("mux_json_{method_name}"), &[obj_value])?;
                self.register_temp(result);
                Ok(result)
            }
            // Returns a bare bool rather than an optional, so nothing to
            // release.
            "is_null" => {
                self.ensure_no_args("is_null", args)?;
                self.call_runtime_function("mux_json_is_null", &[obj_value])
            }
            "set_field" => {
                self.ensure_arg_count("set_field", args, 2)?;
                let key = self.generate_expression(&args[0])?;
                let value = self.generate_expression(&args[1])?;
                self.call_runtime_function("mux_json_set_field", &[obj_value, key, value])
            }
            "push" => {
                self.ensure_arg_count("push", args, 1)?;
                let value = self.generate_expression(&args[0])?;
                self.call_runtime_function("mux_json_push", &[obj_value, value])
            }
            "at_pointer" => {
                self.ensure_arg_count("at_pointer", args, 1)?;
                let pointer_value = self.generate_expression(&args[0])?;
                let pointer = self.string_value_to_cstr(pointer_value)?;
                let result =
                    self.call_runtime_function("mux_json_at_pointer", &[obj_value, pointer]);
                self.free_cstrings(&[pointer])?;
                let result = result?;
                self.register_temp(result);
                Ok(result)
            }
            "set_pointer" => {
                self.ensure_arg_count("set_pointer", args, 2)?;
                let pointer_value = self.generate_expression(&args[0])?;
                let pointer = self.string_value_to_cstr(pointer_value)?;
                let value = self.generate_expression(&args[1])?;
                let result = self
                    .call_runtime_function("mux_json_set_pointer", &[obj_value, pointer, value]);
                self.free_cstrings(&[pointer])?;
                result
            }
            "remove_pointer" => {
                self.ensure_arg_count("remove_pointer", args, 1)?;
                let pointer_value = self.generate_expression(&args[0])?;
                let pointer = self.string_value_to_cstr(pointer_value)?;
                let result =
                    self.call_runtime_function("mux_json_remove_pointer", &[obj_value, pointer]);
                self.free_cstrings(&[pointer])?;
                let result = result?;
                self.register_temp(result);
                Ok(result)
            }
            "merge_patch" => {
                self.ensure_arg_count("merge_patch", args, 1)?;
                let patch = self.generate_expression(&args[0])?;
                self.call_runtime_function("mux_json_merge_patch", &[obj_value, patch])
            }
            "apply_patch" => {
                self.ensure_arg_count("apply_patch", args, 1)?;
                let patch = self.generate_expression(&args[0])?;
                self.call_runtime_function("mux_json_apply_patch", &[obj_value, patch])
            }
            _ => Err(format!("Method {method_name} not implemented for Json")),
        }
    }

    fn generate_json_number_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        self.ensure_no_args(method_name, args)?;
        let runtime_name = match method_name {
            "to_string" => "mux_json_number_to_string",
            "as_int" => "mux_json_number_as_int",
            "as_float" => "mux_json_number_as_float",
            _ => {
                return Err(format!(
                    "Method {method_name} not implemented for JsonNumber"
                ));
            }
        };
        let result = self.call_runtime_function(runtime_name, &[obj_value])?;
        self.register_temp(result);
        Ok(result)
    }

    fn generate_sql_value_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        match method_name {
            "is_null" => {
                self.ensure_no_args("is_null", args)?;
                self.call_runtime_function("mux_sql_value_is_null", &[obj_value])
            }
            "as_bool" => {
                self.ensure_no_args("as_bool", args)?;
                self.call_runtime_function("mux_sql_value_as_bool", &[obj_value])
            }
            "as_int" => {
                self.ensure_no_args("as_int", args)?;
                self.call_runtime_function("mux_sql_value_as_int", &[obj_value])
            }
            "as_float" => {
                self.ensure_no_args("as_float", args)?;
                self.call_runtime_function("mux_sql_value_as_float", &[obj_value])
            }
            "as_string" => {
                self.ensure_no_args("as_string", args)?;
                self.call_runtime_function("mux_sql_value_as_string", &[obj_value])
            }
            "as_bytes" => {
                self.ensure_no_args("as_bytes", args)?;
                self.call_runtime_function("mux_sql_value_as_bytes", &[obj_value])
            }
            "as_json" => {
                self.ensure_no_args("as_json", args)?;
                self.call_runtime_function("mux_sql_value_as_json", &[obj_value])
            }
            "as_datetime" => {
                self.ensure_no_args("as_datetime", args)?;
                self.call_runtime_function("mux_sql_value_as_datetime", &[obj_value])
            }
            "as_uuid" => {
                self.ensure_no_args("as_uuid", args)?;
                self.call_runtime_function("mux_sql_value_as_uuid", &[obj_value])
            }
            "to_string" => {
                self.ensure_no_args("to_string", args)?;
                self.generate_to_string_call(obj_value)
            }
            _ => Err(format!("Method {method_name} not implemented for SqlValue")),
        }
    }

    fn generate_map_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        key_type: &Type,
        value_type: &Type,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        match method_name {
            "to_string" => {
                self.ensure_no_args("to_string", args)?;
                self.generate_to_string_call(obj_value)
            }
            "put" => {
                if args.len() != 2 {
                    return Err("put() method takes exactly 2 arguments".to_string());
                }
                let key_val = self.generate_expression(&args[0])?;
                let value_val = self.generate_expression(&args[1])?;
                // Use mux_map_put_value which modifies the boxed Value directly.
                // Enum keys/values are boxed as managed so they compare and copy
                // correctly (issue #309).
                let boxed_key = self.box_enum_or_value(key_val, key_type)?;
                let boxed_value = self.box_enum_or_value(value_val, value_type)?;
                self.builder
                    .build_call(
                        self.runtime_function("mux_map_put_value")
                            .expect("mux_map_put_value must be declared in runtime"),
                        &[obj_value.into(), boxed_key.into(), boxed_value.into()],
                        "map_put_value",
                    )
                    .map_err(|e| e.to_string())?;
                // put() returns nothing (void), return a dummy value
                Ok(self.context.i64_type().const_int(0, false).into())
            }
            "get" => {
                if args.len() != 1 {
                    return Err("get() method takes exactly 1 argument".to_string());
                }
                let key_val = self.generate_expression(&args[0])?;
                let key_ptr = self.box_enum_or_value(key_val, key_type)?;
                self.with_extracted_map(obj_value, |me, extract_map| {
                    let result = me
                        .builder
                        .build_call(
                            me.runtime_function("mux_map_get")
                                .expect("mux_map_get must be declared in runtime"),
                            &[extract_map.into(), key_ptr.into()],
                            "map_get",
                        )
                        .map_err(|e| e.to_string())?
                        .try_as_basic_value()
                        .basic()
                        .expect("mux_map_get should return a basic value");
                    Ok(result)
                })
            }
            "get_keys" => {
                self.ensure_no_args("get_keys", args)?;
                self.with_extracted_map(obj_value, |me, extract_map| {
                    me.call_runtime_function("mux_map_keys", &[extract_map])
                })
            }
            "get_values" => {
                self.ensure_no_args("get_values", args)?;
                self.with_extracted_map(obj_value, |me, extract_map| {
                    me.call_runtime_function("mux_map_values", &[extract_map])
                })
            }
            // A map's elements are its key/value pairs, so `to_list` is `get_pairs`.
            "get_pairs" | "to_list" => {
                self.ensure_no_args(method_name, args)?;
                self.with_extracted_map(obj_value, |me, extract_map| {
                    me.call_runtime_function("mux_map_pairs", &[extract_map])
                })
            }
            "contains" => {
                if args.len() != 1 {
                    return Err("contains() method takes exactly 1 argument".to_string());
                }
                let key_val = self.generate_expression(&args[0])?;
                let key_ptr = self.box_enum_or_value(key_val, key_type)?;
                self.with_extracted_map(obj_value, |me, extract_map| {
                    let call = me
                        .builder
                        .build_call(
                            me.runtime_function("mux_map_contains")
                                .expect("mux_map_contains must be declared in runtime"),
                            &[extract_map.into(), key_ptr.into()],
                            "map_contains",
                        )
                        .map_err(|e| e.to_string())?;
                    Ok(call
                        .try_as_basic_value()
                        .basic()
                        .expect("mux_map_contains should return a basic value"))
                })
            }
            "size" | "len" => {
                self.ensure_no_args(method_name, args)?;
                self.with_extracted_map(obj_value, |me, extract_map| {
                    me.call_runtime_function("mux_map_size", &[extract_map])
                })
            }
            "is_empty" => {
                self.ensure_no_args("is_empty", args)?;
                self.with_extracted_map(obj_value, |me, extract_map| {
                    me.call_runtime_function("mux_map_is_empty", &[extract_map])
                })
            }
            "remove" => {
                self.ensure_arg_count("remove", args, 1)?;
                let key_val = self.generate_expression(&args[0])?;
                let key_ptr = self.box_enum_or_value(key_val, key_type)?;

                let optional_ptr = self
                    .generate_runtime_call(
                        "mux_map_remove_value",
                        &[obj_value.into(), key_ptr.into()],
                    )
                    .ok_or("mux_map_remove_value should return a value")?;

                Ok(optional_ptr)
            }
            _ => Err(format!("Method {method_name} not implemented for maps")),
        }
    }

    fn generate_set_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        elem_type: &Type,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        match method_name {
            "to_string" => {
                self.ensure_no_args("to_string", args)?;
                self.generate_to_string_call(obj_value)
            }
            "add" => {
                self.ensure_arg_count("add", args, 1)?;
                let elem_val = self.generate_expression(&args[0])?;
                let elem_ptr = self.box_enum_or_value(elem_val, elem_type)?;

                // Use mux_set_add_value which modifies the boxed Value directly
                self.generate_runtime_call(
                    "mux_set_add_value",
                    &[obj_value.into(), elem_ptr.into()],
                );
                Ok(self.context.i32_type().const_int(0, false).into())
            }
            "remove" => {
                self.ensure_arg_count("remove", args, 1)?;
                let elem_val = self.generate_expression(&args[0])?;
                let elem_ptr = self.box_enum_or_value(elem_val, elem_type)?;
                self.call_runtime_function("mux_set_remove_value", &[obj_value, elem_ptr.into()])
            }
            "contains" => {
                self.ensure_arg_count("contains", args, 1)?;
                let elem_val = self.generate_expression(&args[0])?;
                let elem_ptr = self.box_enum_or_value(elem_val, elem_type)?;
                self.with_extracted_set(obj_value, |me, extract_set| {
                    me.call_runtime_function("mux_set_contains", &[extract_set, elem_ptr.into()])
                })
            }
            "size" | "len" => {
                self.ensure_no_args(method_name, args)?;
                self.with_extracted_set(obj_value, |me, extract_set| {
                    me.call_runtime_function("mux_set_size", &[extract_set])
                })
            }
            "is_empty" => {
                self.ensure_no_args("is_empty", args)?;
                self.with_extracted_set(obj_value, |me, extract_set| {
                    me.call_runtime_function("mux_set_is_empty", &[extract_set])
                })
            }
            "to_list" => {
                self.ensure_no_args("to_list", args)?;
                self.with_extracted_set(obj_value, |me, extract_set| {
                    me.call_runtime_function("mux_set_to_list", &[extract_set])
                })
            }
            _ => Err(format!("Method {method_name} not implemented for sets")),
        }
    }

    fn generate_optional_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        inner: &Type,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        match method_name {
            "is_some" | "is_none" => {
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function(&format!("mux_optional_{method_name}"), &[obj_value])
            }
            "value" => {
                self.ensure_no_args(method_name, args)?;
                let data = self.call_runtime_function("mux_optional_data", &[obj_value])?;
                self.extract_value_from_ptr(data.into_pointer_value(), inner, "some")
                    .map(|(value, _)| value)
            }
            "to_string" => {
                self.ensure_no_args("to_string", args)?;
                self.generate_to_string_call(obj_value)
            }
            _ => Err(format!(
                "Method {method_name} not implemented for Optionals"
            )),
        }
    }

    fn generate_result_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        ok: &Type,
        error: &Type,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        match method_name {
            "is_ok" | "is_err" => {
                self.ensure_no_args(method_name, args)?;
                self.call_runtime_function(&format!("mux_result_{method_name}"), &[obj_value])
            }
            "value" | "error" => {
                self.ensure_no_args(method_name, args)?;
                let data = self.call_runtime_function("mux_result_data", &[obj_value])?;
                let payload = if method_name == "value" { ok } else { error };
                self.extract_value_from_ptr(data.into_pointer_value(), payload, method_name)
                    .map(|(value, _)| value)
            }
            "to_string" => {
                self.ensure_no_args("to_string", args)?;
                self.generate_to_string_call(obj_value)
            }
            _ => Err(format!("Method {method_name} not implemented for Results")),
        }
    }

    fn generate_tuple_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        match method_name {
            "to_string" => {
                self.ensure_no_args("to_string", args)?;
                let tuple_ptr = if obj_value.is_pointer_value() {
                    self.extract_raw_pointer(obj_value, "mux_value_get_tuple", "get_tuple")?
                        .into_pointer_value()
                } else {
                    return Err("Tuple method receiver must be a pointer value".to_string());
                };
                let cstr =
                    self.call_runtime_function("mux_tuple_to_string", &[tuple_ptr.into()])?;
                self.call_cstr_to_mux_string(cstr)
            }
            _ => Err(format!("Method {method_name} not implemented for tuples")),
        }
    }
}
