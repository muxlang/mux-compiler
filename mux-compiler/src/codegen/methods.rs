//! Method call generation for the code generator.
//!
//! This module handles:
//! - Instance method calls on user-defined classes
//! - Primitive type method calls (int, float, str, bool, char)
//! - Collection method calls (list, map, set)
//! - Optional method calls

use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};

use crate::ast::{ExpressionNode, PrimitiveType};
use crate::semantics::Type;

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

impl<'a> CodeGenerator<'a> {
    /// Wrap an *owned* C string (returned by a runtime `*_to_string` helper) in
    /// a Mux string Value. Uses the ownership-taking constructor so the input C
    /// string is freed after its contents are copied, rather than leaked.
    fn call_cstr_to_mux_string(
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

    fn call_runtime_function(
        &self,
        func_name: &str,
        args: &[BasicValueEnum<'a>],
    ) -> Result<BasicValueEnum<'a>, String> {
        let func = self
            .runtime_function(func_name)
            .ok_or(format!("Function '{}' not found", func_name))?;
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
            .ok_or_else(|| format!("{} should return a basic value", func_name))
    }

    fn call_runtime_to_string(
        &self,
        value: BasicValueEnum<'a>,
        func_name: &str,
    ) -> Result<BasicValueEnum<'a>, String> {
        let to_cstr = self
            .runtime_function(func_name)
            .ok_or(format!("{} not found", func_name))?;
        let call = self
            .builder
            .build_call(to_cstr, &[value.into()], "to_cstr")
            .map_err(|e| e.to_string())?;
        let cstr = call
            .try_as_basic_value()
            .basic()
            .ok_or(format!("{} should return a basic value", func_name))?;
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
            .ok_or(format!("{} not found", getter_func))?;
        let raw = self
            .builder
            .build_call(getter, &[obj_value.into()], extract_name)
            .map_err(|e| e.to_string())?;
        raw.try_as_basic_value()
            .basic()
            .ok_or_else(|| format!("{} should return a basic value", getter_func))
    }

    fn free_raw_pointer(&self, raw_ptr: BasicValueEnum<'a>, free_func: &str) -> Result<(), String> {
        let free_fn = self
            .runtime_function(free_func)
            .ok_or(format!("{} not found", free_func))?;
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
            .ok_or(format!("{} not found", conversion_func))?;
        let call = self
            .builder
            .build_call(func, &[cstr.into()], "str_conv")
            .map_err(|e| e.to_string())?;
        let result = call
            .try_as_basic_value()
            .basic()
            .unwrap_or_else(|| panic!("{} should return a basic value", conversion_func));
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

    fn call_unary_predicate(
        &self,
        obj_value: BasicValueEnum<'a>,
        func_name: &str,
    ) -> Result<BasicValueEnum<'a>, String> {
        let func = self
            .runtime_function(func_name)
            .ok_or(format!("{} not found", func_name))?;
        let call = self
            .builder
            .build_call(func, &[obj_value.into()], "predicate_call")
            .map_err(|e| e.to_string())?;
        Ok(call
            .try_as_basic_value()
            .basic()
            .unwrap_or_else(|| panic!("{} should return a basic value", func_name)))
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
                "{}() method takes exactly {} argument(s)",
                method, expected
            ))
        } else {
            Ok(())
        }
    }

    fn ensure_no_args(&self, method: &str, args: &[ExpressionNode]) -> Result<(), String> {
        if !args.is_empty() {
            Err(format!("{}() method takes no arguments", method))
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
            .ok_or_else(|| format!("Class {} not found", class_name))?;

        let method = class
            .methods
            .get(method_name)
            .ok_or_else(|| format!("Method {} not found on class {}", method_name, class_name))?;

        if method.is_static {
            return Err(format!(
                "Cannot call static method {} on instance",
                method_name
            ));
        }

        let method_func_name = self.resolve_method_func_name(class_name, type_args, method_name)?;
        let is_specialized = method_func_name.contains('$');

        let call_args = self.build_method_call_args(obj_value, args, is_specialized)?;

        let func = self
            .module
            .get_function(&method_func_name)
            .ok_or_else(|| format!("Method '{}' not found", method_func_name))?;

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

    fn resolve_method_func_name(
        &self,
        class_name: &str,
        type_args: &[Type],
        method_name: &str,
    ) -> Result<String, String> {
        if type_args.is_empty()
            && let Some(current_fn) = &self.current_function_name
            && let Some((current_class_part, _)) = current_fn.split_once('.')
            && current_class_part.starts_with(&format!("{}$", class_name))
        {
            let contextual_name = format!("{}.{}", current_class_part, method_name);
            if self.module.get_function(&contextual_name).is_some() {
                return Ok(contextual_name);
            }
        }

        let specialized_method_name =
            self.create_specialized_method_name(class_name, type_args, method_name);

        if self.module.get_function(&specialized_method_name).is_some() {
            Ok(specialized_method_name)
        } else {
            Ok(format!("{}.{}", class_name, method_name))
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
            .ok_or(format!("{} not found", func_name))?;
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
            ("UdpSocket", "bind") => {
                let addr = gen_one_arg(self, args)?;
                let call = self.build_net_call("mux_net_udp_bind", &[addr])?;
                Ok(Some(call))
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
        let type_name = if let Type::Named(name, _) = obj_type {
            name
        } else {
            return Ok(None);
        };

        match type_name.as_str() {
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
                _ => Ok(None),
            },
            _ => Ok(None),
        }
    }

    pub(super) fn try_generate_sync_instance_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        obj_type: &Type,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let type_name = if let Type::Named(name, _) = obj_type {
            name
        } else {
            return Ok(None);
        };

        match type_name.as_str() {
            "Thread" => {
                self.ensure_no_args("Thread", args)?;
                match method_name {
                    "join" => self
                        .call_runtime_function("mux_thread_join", &[obj_value])
                        .map(Some),
                    "detach" => self
                        .call_runtime_function("mux_thread_detach", &[obj_value])
                        .map(Some),
                    _ => Ok(None),
                }
            }
            "Mutex" => {
                self.ensure_no_args("Mutex", args)?;
                match method_name {
                    "lock" => self
                        .call_runtime_function("mux_mutex_lock", &[obj_value])
                        .map(Some),
                    "unlock" => self
                        .call_runtime_function("mux_mutex_unlock", &[obj_value])
                        .map(Some),
                    _ => Ok(None),
                }
            }
            "RwLock" => {
                self.ensure_no_args("RwLock", args)?;
                match method_name {
                    "read_lock" => self
                        .call_runtime_function("mux_rwlock_read_lock", &[obj_value])
                        .map(Some),
                    "write_lock" => self
                        .call_runtime_function("mux_rwlock_write_lock", &[obj_value])
                        .map(Some),
                    "unlock" => self
                        .call_runtime_function("mux_rwlock_unlock", &[obj_value])
                        .map(Some),
                    _ => Ok(None),
                }
            }
            "CondVar" => match method_name {
                "wait" => {
                    if args.len() != 1 {
                        return Err("wait() method takes exactly 1 argument".to_string());
                    }
                    let mutex_val = self.generate_expression(&args[0])?;
                    let mutex_boxed = self.box_value(mutex_val);
                    self.call_runtime_function("mux_condvar_wait", &[obj_value, mutex_boxed.into()])
                        .map(Some)
                }
                "signal" => {
                    self.ensure_no_args("signal", args)?;
                    self.call_runtime_function("mux_condvar_signal", &[obj_value])
                        .map(Some)
                }
                "broadcast" => {
                    self.ensure_no_args("broadcast", args)?;
                    self.call_runtime_function("mux_condvar_broadcast", &[obj_value])
                        .map(Some)
                }
                _ => Ok(None),
            },
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
        let type_name = if let Type::Named(name, _) = obj_type {
            name
        } else {
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
                "execute_params" => {
                    let (sql, params) = gen_two_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_connection_execute_params",
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
                "begin_transaction" => {
                    self.ensure_no_args("begin_transaction", args)?;
                    self.build_net_call("mux_sql_connection_begin_transaction", &[obj_value])
                        .map(Some)
                }
                _ => Ok(None),
            },
            "Transaction" => match method_name {
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
                "execute_params" => {
                    let (sql, params) = gen_two_expr(self, args)?;
                    self.build_net_call(
                        "mux_sql_transaction_execute_params",
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
                _ => Ok(None),
            },
            "ResultSet" => match method_name {
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
                "columns" => {
                    self.ensure_no_args("columns", args)?;
                    self.build_net_call("mux_sql_resultset_columns", &[obj_value])
                        .map(Some)
                }
                _ => Ok(None),
            },
            _ => Ok(None),
        }
    }

    pub(super) fn generate_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        obj_type: &Type,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        let resolved_obj_type = self.resolve_type(obj_type).map_err(|e| {
            format!(
                "Unresolved receiver type for method '{}': {}",
                method_name, e
            )
        })?;

        if let Some(call) = self.try_generate_net_instance_method_call(
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
                self.generate_primitive_method_call(obj_value, prim, method_name)
            }
            Type::List(_) => self.generate_list_method_call(obj_value, method_name, args),
            Type::Map(_, _) => self.generate_map_method_call(obj_value, method_name, args),
            Type::Set(_) => self.generate_set_method_call(obj_value, method_name, args),
            Type::Tuple(_, _) => self.generate_tuple_method_call(obj_value, method_name, args),
            Type::Named(name, _type_args) if name == "Csv" => {
                self.generate_csv_method_call(obj_value, method_name, args)
            }
            Type::Named(name, _type_args) if name == "Json" => {
                self.generate_json_method_call(obj_value, method_name, args)
            }
            Type::Named(name, _type_args) if name == "SqlValue" => {
                self.generate_sql_value_method_call(obj_value, method_name, args)
            }
            Type::Named(name, type_args) => {
                self.invoke_class_instance_method(name, type_args, obj_value, method_name, args)
            }
            Type::Optional(_) => self.generate_optional_method_call(obj_value, method_name, args),
            Type::Result(_, _) => self.generate_result_method_call(obj_value, method_name, args),
            _ => Err(format!(
                "Method {} not implemented for type {:?}",
                method_name, resolved_obj_type
            )),
        }
    }

    fn generate_primitive_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        prim: &PrimitiveType,
        method_name: &str,
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
                "to_int" => Ok(obj_value),
                "to_char" => Ok(obj_value),
                _ => Err(format!("Method {} not implemented for int", method_name)),
            },
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
                _ => Err(format!("Method {} not implemented for float", method_name)),
            },
            PrimitiveType::Str => match method_name {
                "to_string" => self.call_runtime_to_string(obj_value, "mux_value_to_string"),
                "message" => Ok(obj_value),
                "to_int" => self.call_string_conversion_func(obj_value, "mux_string_to_int"),
                "to_float" => self.call_string_conversion_func(obj_value, "mux_string_to_float"),
                "to_char" => self.call_string_conversion_func(obj_value, "mux_string_to_char"),
                // mux_string_length takes a raw C string, so unwrap the
                // boxed value first like the conversion functions do.
                "length" => self.call_string_conversion_func(obj_value, "mux_string_length"),
                _ => Err(format!("Method {} not implemented for string", method_name)),
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
                _ => Err(format!("Method {} not implemented for bool", method_name)),
            },
            PrimitiveType::Char => match method_name {
                "to_string" => self.call_runtime_to_string(obj_value, "mux_char_to_string"),
                "to_int" => self.call_runtime_function("mux_char_to_int", &[obj_value]),
                "to_char" => Ok(obj_value),
                _ => Err(format!("Method {} not implemented for char", method_name)),
            },
            _ => Err(format!(
                "Method {} not implemented for primitive type {:?}",
                method_name, prim
            )),
        }
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
            "size" => {
                self.ensure_no_args("size", args)?;
                self.with_extracted_list(obj_value, |me, raw_list| {
                    me.call_runtime_function("mux_list_length", &[raw_list])
                })
            }
            "to_string" => {
                self.ensure_no_args("to_string", args)?;
                self.with_extracted_list(obj_value, |me, raw_list| {
                    let cstr = me.call_runtime_function("mux_list_to_string", &[raw_list])?;
                    me.call_cstr_to_mux_string(cstr)
                })
            }
            _ => Err(format!("Method {} not implemented for lists", method_name)),
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
            _ => Err(format!("Method {} not implemented for Csv", method_name)),
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
                self.call_runtime_function("mux_json_stringify", &[obj_value, indent_arg])
            }
            _ => Err(format!("Method {} not implemented for Json", method_name)),
        }
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
            "to_string" => {
                self.ensure_no_args("to_string", args)?;
                self.generate_to_string_call(obj_value)
            }
            _ => Err(format!(
                "Method {} not implemented for SqlValue",
                method_name
            )),
        }
    }

    fn generate_map_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
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
                // Use mux_map_put_value which modifies the boxed Value directly
                let boxed_key = self.box_value(key_val);
                let boxed_value = self.box_value(value_val);
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
                self.with_extracted_map(obj_value, |me, extract_map| {
                    let result = me
                        .builder
                        .build_call(
                            me.runtime_function("mux_map_get")
                                .expect("mux_map_get must be declared in runtime"),
                            &[extract_map.into(), key_val.into()],
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
            "get_pairs" => {
                self.ensure_no_args("get_pairs", args)?;
                self.with_extracted_map(obj_value, |me, extract_map| {
                    me.call_runtime_function("mux_map_pairs", &[extract_map])
                })
            }
            "contains" => {
                if args.len() != 1 {
                    return Err("contains() method takes exactly 1 argument".to_string());
                }
                let key_val = self.generate_expression(&args[0])?;
                self.with_extracted_map(obj_value, |me, extract_map| {
                    let call = me
                        .builder
                        .build_call(
                            me.runtime_function("mux_map_contains")
                                .expect("mux_map_contains must be declared in runtime"),
                            &[extract_map.into(), key_val.into()],
                            "map_contains",
                        )
                        .map_err(|e| e.to_string())?;
                    Ok(call
                        .try_as_basic_value()
                        .basic()
                        .expect("mux_map_contains should return a basic value"))
                })
            }
            "size" => {
                self.ensure_no_args("size", args)?;
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
                let key_ptr = self.box_value(key_val);

                let optional_ptr = self
                    .generate_runtime_call(
                        "mux_map_remove_value",
                        &[obj_value.into(), key_ptr.into()],
                    )
                    .ok_or("mux_map_remove_value should return a value")?;

                Ok(optional_ptr)
            }
            _ => Err(format!("Method {} not implemented for maps", method_name)),
        }
    }

    fn generate_set_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
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
                let elem_ptr = self.box_value(elem_val);

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
                let elem_ptr = self.box_value(elem_val);
                self.call_runtime_function("mux_set_remove_value", &[obj_value, elem_ptr.into()])
            }
            "contains" => {
                self.ensure_arg_count("contains", args, 1)?;
                let elem_val = self.generate_expression(&args[0])?;
                let elem_ptr = self.box_value(elem_val);
                self.with_extracted_set(obj_value, |me, extract_set| {
                    me.call_runtime_function("mux_set_contains", &[extract_set, elem_ptr.into()])
                })
            }
            "size" => {
                self.ensure_no_args("size", args)?;
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
            _ => Err(format!("Method {} not implemented for sets", method_name)),
        }
    }

    fn generate_optional_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        match method_name {
            "to_string" => {
                self.ensure_no_args("to_string", args)?;
                self.generate_to_string_call(obj_value)
            }
            "is_some" => {
                self.ensure_no_args("is_some", args)?;
                self.call_unary_predicate(obj_value, "mux_optional_is_some")
            }
            "is_none" => {
                self.ensure_no_args("is_none", args)?;
                self.call_unary_predicate(obj_value, "mux_optional_is_none")
            }
            _ => Err(format!(
                "Method {} not implemented for Optionals",
                method_name
            )),
        }
    }

    fn generate_result_method_call(
        &mut self,
        obj_value: BasicValueEnum<'a>,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        match method_name {
            "to_string" => {
                self.ensure_no_args("to_string", args)?;
                self.generate_to_string_call(obj_value)
            }
            "is_ok" => {
                self.ensure_no_args("is_ok", args)?;
                self.call_unary_predicate(obj_value, "mux_result_is_ok")
            }
            "is_err" => {
                self.ensure_no_args("is_err", args)?;
                self.call_unary_predicate(obj_value, "mux_result_is_err")
            }
            _ => Err(format!(
                "Method {} not implemented for Results",
                method_name
            )),
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
            _ => Err(format!("Method {} not implemented for tuples", method_name)),
        }
    }
}
