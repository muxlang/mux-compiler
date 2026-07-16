//! Runtime function calls and value boxing/unboxing.
//!
//! This module handles calling mux runtime functions and converting
//! between LLVM values and boxed Value* pointers.

use super::CodeGenerator;
use crate::ast::PrimitiveType;
use crate::semantics::Type;
use inkwell::AddressSpace;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, PointerType};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, FloatValue, FunctionValue, IntValue, PointerValue,
};

impl<'a> CodeGenerator<'a> {
    pub(super) fn runtime_function(&self, name: &str) -> Option<FunctionValue<'a>> {
        if let Some(func) = self.module.get_function(name) {
            return Some(func);
        }

        let signature = self.runtime_signatures.get_function(name)?;
        Some(self.module.add_function(name, signature.get_type(), None))
    }

    pub(super) fn generate_runtime_call(
        &mut self,
        name: &str,
        args: &[BasicMetadataValueEnum<'a>],
    ) -> Option<BasicValueEnum<'a>> {
        let func = match self.runtime_function(name) {
            Some(f) => f,
            None => {
                panic!("Function '{}' not found in module", name);
            }
        };
        let call = self
            .builder
            .build_call(func, args, "call")
            .expect("build_call should always return Some");
        call.try_as_basic_value().basic()
    }

    /// Declare runtime functions used by codegen.
    pub(super) fn declare_runtime_functions<'b>(module: &Module<'b>, context: &'b Context) {
        // local helpers
        fn add_i8_fn<'c>(
            module: &Module<'c>,
            i8_ptr: PointerType<'c>,
            name: &str,
            params: &[BasicTypeEnum<'c>],
        ) -> FunctionValue<'c> {
            let llvm_params: Vec<BasicMetadataTypeEnum<'c>> =
                params.iter().copied().map(Into::into).collect();
            module.add_function(name, i8_ptr.fn_type(&llvm_params, false), None)
        }

        fn add_conversion_fn<'c>(
            module: &Module<'c>,
            i8_ptr: PointerType<'c>,
            mux_name: &str,
            from: BasicTypeEnum<'c>,
        ) -> FunctionValue<'c> {
            add_i8_fn(module, i8_ptr, mux_name, &[from])
        }

        fn add_typed_getter<'c>(
            module: &Module<'c>,
            i8_ptr: PointerType<'c>,
            name: &str,
            return_type: BasicTypeEnum<'c>,
        ) -> FunctionValue<'c> {
            module.add_function(name, return_type.fn_type(&[i8_ptr.into()], false), None)
        }

        let void_type = context.void_type();
        let i64_type = context.i64_type();
        let i32_type = context.i32_type();
        let f64_type = context.f64_type();
        let i8_ptr = context.ptr_type(AddressSpace::default());
        let list_ptr = i8_ptr;
        let map_ptr = i8_ptr;
        let set_ptr = i8_ptr;

        macro_rules! void_i8ptr_fn {
            ($name:expr) => {
                module.add_function($name, void_type.fn_type(&[i8_ptr.into()], false), None)
            };
        }
        macro_rules! i8ptr_i8ptr_fn {
            ($name:expr) => {
                module.add_function($name, i8_ptr.fn_type(&[i8_ptr.into()], false), None)
            };
        }
        macro_rules! i8ptr_void_fn {
            ($name:expr) => {
                module.add_function($name, i8_ptr.fn_type(&[], false), None)
            };
        }
        macro_rules! i8ptr_i8ptr_i8ptr_fn {
            ($name:expr) => {
                module.add_function(
                    $name,
                    i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
                    None,
                )
            };
        }

        module.add_function(
            "mux_value_from_string",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_new_string_from_cstr",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );
        // Ownership-taking variant: frees the input C string after copying it.
        // Used for conversion functions (`to_string`, concat) whose input is an
        // owned pointer returned by the runtime, so it must be freed once copied.
        module.add_function(
            "mux_new_string_from_owned_cstr",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_print",
            void_type.fn_type(&[i8_ptr.into()], false),
            None,
        );
        i8ptr_void_fn!("mux_read_line");
        module.add_function(
            "exit",
            void_type.fn_type(&[context.i32_type().into()], false),
            None,
        );
        module.add_function(
            "mux_panic_cstr",
            void_type.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_panic_index_oob",
            void_type.fn_type(&[i64_type.into(), i64_type.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_panic_key_not_found",
            void_type.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function("malloc", i8_ptr.fn_type(&[i64_type.into()], false), None);

        // Closure lifetime management (see mux-runtime/src/closure.rs).
        module.add_function(
            "mux_closure_retain",
            void_type.fn_type(&[i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_closure_release",
            void_type.fn_type(&[i8_ptr.into()], false),
            None,
        );

        let params = &[i8_ptr.into(), i8_ptr.into()];
        let fn_type = i8_ptr.fn_type(params, false);
        module.add_function("mux_string_concat", fn_type, None);

        module.add_function(
            "mux_string_length",
            i64_type.fn_type(&[i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_string_contains",
            context
                .bool_type()
                .fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_string_contains_char",
            context
                .bool_type()
                .fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );

        module.add_function(
            "mux_string_equal",
            context
                .i32_type()
                .fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_string_not_equal",
            context
                .i32_type()
                .fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_value_equal",
            context
                .i32_type()
                .fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_value_not_equal",
            context
                .i32_type()
                .fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_value_get_string",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );

        for (name, from_ty) in [
            ("mux_int_to_string", i64_type.into()),
            ("mux_int_to_float", i64_type.into()),
            ("mux_float_to_int", f64_type.into()),
            ("mux_float_to_string", f64_type.into()),
            ("mux_bool_to_string", i32_type.into()),
            ("mux_char_to_int", i64_type.into()),
            ("mux_char_to_string", i64_type.into()),
        ] {
            add_conversion_fn(module, i8_ptr, name, from_ty);
        }

        for name in [
            "mux_bool_to_int",
            "mux_bool_to_float",
            "mux_string_to_string",
            "mux_string_to_int",
            "mux_string_to_float",
            "mux_string_to_char",
            "mux_list_to_string",
            "mux_list_value",
            "mux_map_value",
            "mux_set_value",
            "mux_set_to_list",
            "mux_map_to_string",
        ] {
            add_i8_fn(module, i8_ptr, name, &[i8_ptr.into()]);
        }

        module.add_function(
            "mux_register_object_type",
            i32_type.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );

        module.add_function(
            "mux_register_object_copy",
            void_type.fn_type(&[i32_type.into(), i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_register_object_destructor",
            void_type.fn_type(&[i32_type.into(), i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_alloc_object",
            i8_ptr.fn_type(&[i32_type.into()], false),
            None,
        );

        void_i8ptr_fn!("mux_free_string");
        void_i8ptr_fn!("mux_free_object");
        void_i8ptr_fn!("mux_free_list");
        void_i8ptr_fn!("mux_free_set");
        void_i8ptr_fn!("mux_free_map");
        i8ptr_i8ptr_fn!("mux_get_object_ptr");
        i8ptr_i8ptr_fn!("mux_copy_object");
        i8ptr_i8ptr_fn!("mux_value_deep_clone");

        module.add_function(
            "mux_box_enum",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );

        module.add_function(
            "mux_value_unbox_enum",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );

        i8ptr_i8ptr_fn!("mux_set_to_string");
        i8ptr_i8ptr_fn!("mux_optional_to_string");
        i8ptr_i8ptr_fn!("mux_optional_into_value");

        module.add_function(
            "mux_value_get_list",
            list_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_value_get_map",
            map_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_value_get_set",
            set_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_list_concat",
            list_ptr.fn_type(&[list_ptr.into(), list_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_map_merge",
            map_ptr.fn_type(&[map_ptr.into(), map_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_set_union",
            set_ptr.fn_type(&[set_ptr.into(), set_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_value_to_string",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_range",
            list_ptr.fn_type(&[i64_type.into(), i64_type.into()], false),
            None,
        );

        module.add_function("mux_new_list", list_ptr.fn_type(&[], false), None);

        module.add_function("mux_new_map", list_ptr.fn_type(&[], false), None);

        module.add_function("mux_new_set", list_ptr.fn_type(&[], false), None);

        module.add_function(
            "mux_new_tuple",
            list_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_tuple_value",
            i8_ptr.fn_type(&[list_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_tuple_left",
            i8_ptr.fn_type(&[list_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_tuple_right",
            i8_ptr.fn_type(&[list_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_tuple_eq",
            context
                .bool_type()
                .fn_type(&[list_ptr.into(), list_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_tuple_to_string",
            i8_ptr.fn_type(&[list_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_value_get_tuple",
            list_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );

        i8ptr_i8ptr_i8ptr_fn!("mux_value_add");

        add_typed_getter(module, i8_ptr, "mux_value_get_int", i64_type.into());
        add_typed_getter(module, i8_ptr, "mux_value_get_float", f64_type.into());
        add_typed_getter(module, i8_ptr, "mux_value_get_bool", i32_type.into());
        add_typed_getter(module, i8_ptr, "mux_value_get_type_tag", i32_type.into());

        void_i8ptr_fn!("mux_free_value");

        module.add_function(
            "mux_list_push_back",
            void_type.fn_type(&[list_ptr.into(), i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_list_get",
            i8_ptr.fn_type(&[list_ptr.into(), i64_type.into()], false),
            None,
        );

        module.add_function(
            "mux_list_get_value",
            i8_ptr.fn_type(&[list_ptr.into(), i64_type.into()], false),
            None,
        );

        module.add_function(
            "mux_list_set",
            void_type.fn_type(&[list_ptr.into(), i64_type.into(), i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_list_set_value",
            void_type.fn_type(&[i8_ptr.into(), i64_type.into(), i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_list_length",
            i64_type.fn_type(&[list_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_list_contains",
            context
                .bool_type()
                .fn_type(&[list_ptr.into(), i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_value_list_length",
            i64_type.fn_type(&[i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_value_list_get_value",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );

        module.add_function(
            "mux_value_list_slice",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into(), i64_type.into()], false),
            None,
        );

        module.add_function(
            "mux_list_pop_back",
            i8_ptr.fn_type(&[list_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_list_push",
            void_type.fn_type(&[list_ptr.into(), i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_list_pop",
            i8_ptr.fn_type(&[list_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_list_push_back_value",
            void_type.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_list_push_value",
            void_type.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_list_pop_back_value",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_list_pop_value",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_list_is_empty",
            context.bool_type().fn_type(&[list_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_map_put",
            void_type.fn_type(&[map_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_map_put_value",
            void_type.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_map_get",
            i8_ptr.fn_type(&[list_ptr.into(), i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_value_map_get_value",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_map_contains",
            context
                .bool_type()
                .fn_type(&[map_ptr.into(), i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_map_remove",
            i8_ptr.fn_type(&[map_ptr.into(), i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_map_remove_value",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_set_add",
            void_type.fn_type(&[list_ptr.into(), i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_set_add_value",
            void_type.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_set_contains",
            context
                .bool_type()
                .fn_type(&[list_ptr.into(), i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_set_remove",
            context
                .bool_type()
                .fn_type(&[list_ptr.into(), i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_set_remove_value",
            context
                .bool_type()
                .fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_set_size",
            i64_type.fn_type(&[list_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_set_is_empty",
            context.bool_type().fn_type(&[list_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_map_size",
            i64_type.fn_type(&[list_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_map_is_empty",
            context.bool_type().fn_type(&[list_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_map_keys",
            i8_ptr.fn_type(&[map_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_map_values",
            i8_ptr.fn_type(&[map_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_map_pairs",
            i8_ptr.fn_type(&[map_ptr.into()], false),
            None,
        );

        for (name, from_ty) in [
            ("mux_int_value", i64_type.into()),
            ("mux_float_value", f64_type.into()),
            ("mux_bool_value", i32_type.into()),
        ] {
            add_conversion_fn(module, i8_ptr, name, from_ty);
        }
        add_i8_fn(module, i8_ptr, "mux_string_value", &[i8_ptr.into()]);
        add_typed_getter(module, i8_ptr, "mux_int_from_value", i64_type.into());
        add_typed_getter(module, i8_ptr, "mux_float_from_value", f64_type.into());
        add_typed_getter(module, i8_ptr, "mux_bool_from_value", i32_type.into());
        add_i8_fn(module, i8_ptr, "mux_string_from_value", &[i8_ptr.into()]);

        for (name, from_ty) in [
            ("mux_optional_some_int", i64_type.into()),
            ("mux_optional_some_float", f64_type.into()),
            ("mux_optional_some_bool", i32_type.into()),
            ("mux_optional_some_char", i64_type.into()),
            ("mux_result_ok_int", i64_type.into()),
            ("mux_result_ok_float", f64_type.into()),
            ("mux_result_ok_bool", i32_type.into()),
            ("mux_result_ok_char", i64_type.into()),
        ] {
            add_conversion_fn(module, i8_ptr, name, from_ty);
        }
        for name in [
            "mux_optional_some_string",
            "mux_optional_some_value",
            "mux_result_ok_string",
            "mux_result_ok_value",
            "mux_result_err_str",
            "mux_result_err_value",
            "mux_optional_data",
            "mux_optional_get_value",
            "mux_result_data",
        ] {
            add_i8_fn(module, i8_ptr, name, &[i8_ptr.into()]);
        }
        i8ptr_void_fn!("mux_optional_none");
        add_typed_getter(module, i8_ptr, "mux_optional_discriminant", i32_type.into());
        add_typed_getter(
            module,
            i8_ptr,
            "mux_value_optional_discriminant",
            i32_type.into(),
        );
        add_typed_getter(
            module,
            i8_ptr,
            "mux_optional_is_some",
            context.bool_type().into(),
        );
        add_typed_getter(
            module,
            i8_ptr,
            "mux_optional_is_none",
            context.bool_type().into(),
        );
        void_i8ptr_fn!("mux_free_optional");
        add_typed_getter(module, i8_ptr, "mux_result_discriminant", i32_type.into());
        add_typed_getter(
            module,
            i8_ptr,
            "mux_value_result_discriminant",
            i32_type.into(),
        );
        add_typed_getter(
            module,
            i8_ptr,
            "mux_result_is_ok",
            context.bool_type().into(),
        );
        add_typed_getter(
            module,
            i8_ptr,
            "mux_result_is_err",
            context.bool_type().into(),
        );

        module.add_function(
            "mux_int_pow",
            i64_type.fn_type(&[i64_type.into(), i64_type.into()], false),
            None,
        );

        module.add_function(
            "mux_math_pow",
            f64_type.fn_type(&[f64_type.into(), f64_type.into()], false),
            None,
        );

        macro_rules! declare_extern_batch {
            ($module:expr, $names:expr, $fn_type:expr) => {
                for name in $names {
                    $module.add_function(name, $fn_type, None);
                }
            };
        }

        declare_extern_batch!(
            module,
            &[
                "mux_math_sqrt",
                "mux_math_sin",
                "mux_math_cos",
                "mux_math_tan",
                "mux_math_asin",
                "mux_math_acos",
                "mux_math_atan",
                "mux_math_ln",
                "mux_math_log2",
                "mux_math_log10",
                "mux_math_exp",
                "mux_math_abs",
                "mux_math_floor",
                "mux_math_ceil",
                "mux_math_round",
            ],
            f64_type.fn_type(&[f64_type.into()], false)
        );

        declare_extern_batch!(
            module,
            &[
                "mux_math_atan2",
                "mux_math_log",
                "mux_math_min",
                "mux_math_max",
                "mux_math_hypot",
            ],
            f64_type.fn_type(&[f64_type.into(), f64_type.into()], false)
        );

        declare_extern_batch!(
            module,
            &["mux_math_pi", "mux_math_e"],
            f64_type.fn_type(&[], false)
        );

        module.add_function("mux_read_int", i64_type.fn_type(&[], false), None);

        module.add_function("mux_flush_stdout", void_type.fn_type(&[], false), None);

        for name in [
            "mux_io_read_file",
            "mux_io_exists",
            "mux_io_remove",
            "mux_io_is_file",
            "mux_io_is_dir",
            "mux_io_mkdir",
            "mux_io_listdir",
            "mux_io_basename",
            "mux_io_dirname",
        ] {
            add_i8_fn(module, i8_ptr, name, &[i8_ptr.into()]);
        }
        // Environment access: env.get(key: *const i8) -> Optional(Str)
        add_i8_fn(module, i8_ptr, "mux_env_get", &[i8_ptr.into()]);
        // JSON helpers
        add_i8_fn(module, i8_ptr, "mux_json_parse", &[i8_ptr.into()]);
        module.add_function(
            "mux_json_stringify",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        add_i8_fn(module, i8_ptr, "mux_json_from_map", &[i8_ptr.into()]);
        add_i8_fn(module, i8_ptr, "mux_json_to_map", &[i8_ptr.into()]);
        // CSV helpers
        add_i8_fn(module, i8_ptr, "mux_csv_parse", &[i8_ptr.into()]);
        add_i8_fn(
            module,
            i8_ptr,
            "mux_csv_parse_with_headers",
            &[i8_ptr.into()],
        );
        module.add_function(
            "mux_csv_to_string",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_io_write_file",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_io_join",
            &[i8_ptr.into(), i8_ptr.into()],
        );

        for name in ["mux_datetime_now", "mux_datetime_now_millis"] {
            module.add_function(name, i8_ptr.fn_type(&[], false), None);
        }
        for name in [
            "mux_datetime_year",
            "mux_datetime_month",
            "mux_datetime_day",
            "mux_datetime_hour",
            "mux_datetime_minute",
            "mux_datetime_second",
            "mux_datetime_weekday",
            "mux_datetime_sleep",
            "mux_datetime_sleep_millis",
        ] {
            module.add_function(name, i8_ptr.fn_type(&[i64_type.into()], false), None);
        }
        for name in ["mux_datetime_format", "mux_datetime_format_local"] {
            module.add_function(
                name,
                i8_ptr.fn_type(&[i64_type.into(), i8_ptr.into()], false),
                None,
            );
        }

        module.add_function(
            "mux_sync_spawn",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_sync_sleep",
            void_type.fn_type(&[i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_thread_join",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_thread_detach",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );
        i8ptr_void_fn!("mux_mutex_new");
        i8ptr_i8ptr_fn!("mux_mutex_lock");
        i8ptr_i8ptr_fn!("mux_mutex_unlock");
        i8ptr_void_fn!("mux_rwlock_new");
        i8ptr_i8ptr_fn!("mux_rwlock_read_lock");
        i8ptr_i8ptr_fn!("mux_rwlock_write_lock");
        i8ptr_i8ptr_fn!("mux_rwlock_unlock");
        i8ptr_void_fn!("mux_condvar_new");
        i8ptr_i8ptr_i8ptr_fn!("mux_condvar_wait");
        i8ptr_i8ptr_fn!("mux_condvar_signal");
        module.add_function(
            "mux_condvar_broadcast",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_rc_inc",
            context.void_type().fn_type(&[i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_rc_dec",
            context.bool_type().fn_type(&[i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_rand_init",
            void_type.fn_type(&[i64_type.into()], false),
            None,
        );

        module.add_function("mux_rand_int", i64_type.fn_type(&[], false), None);

        module.add_function(
            "mux_rand_range",
            i64_type.fn_type(&[i64_type.into(), i64_type.into()], false),
            None,
        );

        module.add_function("mux_rand_float", f64_type.fn_type(&[], false), None);

        module.add_function(
            "mux_rand_bool",
            context.bool_type().fn_type(&[], false),
            None,
        );

        module.add_function(
            "mux_assert_assert",
            void_type.fn_type(&[context.i32_type().into(), i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_assert_eq",
            void_type.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_assert_ne",
            void_type.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_assert_true",
            void_type.fn_type(&[context.i32_type().into()], false),
            None,
        );
        module.add_function(
            "mux_assert_false",
            void_type.fn_type(&[context.i32_type().into()], false),
            None,
        );
        void_i8ptr_fn!("mux_assert_some");
        void_i8ptr_fn!("mux_assert_none");
        void_i8ptr_fn!("mux_assert_ok");
        void_i8ptr_fn!("mux_assert_err");

        i8ptr_i8ptr_fn!("mux_net_http_request");
        i8ptr_i8ptr_fn!("mux_net_http_read_request");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_write_response");
        i8ptr_i8ptr_fn!("mux_net_tcp_listener_bind");
        i8ptr_i8ptr_fn!("mux_net_tcp_listener_accept");
        module.add_function(
            "mux_net_tcp_listener_set_nonblocking",
            i8_ptr.fn_type(&[i8_ptr.into(), i32_type.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_net_tcp_listener_local_addr");
        void_i8ptr_fn!("mux_net_tcp_listener_close");
        i8ptr_i8ptr_fn!("mux_net_tcp_connect");
        module.add_function(
            "mux_net_tcp_read",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_i8ptr_fn!("mux_net_tcp_write");
        void_i8ptr_fn!("mux_net_tcp_close");
        module.add_function(
            "mux_net_tcp_set_nonblocking",
            i8_ptr.fn_type(&[i8_ptr.into(), i32_type.into()], false),
            None,
        );
        module.add_function(
            "mux_net_tcp_peer_addr",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_net_tcp_local_addr",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_net_udp_bind",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_net_udp_send_to",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_net_udp_recv_from",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_net_udp_close",
            void_type.fn_type(&[i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_net_udp_set_nonblocking",
            i8_ptr.fn_type(&[i8_ptr.into(), i32_type.into()], false),
            None,
        );
        module.add_function(
            "mux_net_udp_peer_addr",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_net_udp_local_addr",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_sql_connect",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_sql_value_int",
            i8_ptr.fn_type(&[i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_sql_value_float",
            i8_ptr.fn_type(&[f64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_sql_value_bool",
            i8_ptr.fn_type(&[context.bool_type().into()], false),
            None,
        );
        module.add_function(
            "mux_sql_value_string",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_sql_value_bytes",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );
        i8ptr_void_fn!("mux_sql_value_null");
        module.add_function(
            "mux_sql_value_is_null",
            context.bool_type().fn_type(&[i8_ptr.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_sql_value_as_bool");
        i8ptr_i8ptr_fn!("mux_sql_value_as_int");
        i8ptr_i8ptr_fn!("mux_sql_value_as_float");
        i8ptr_i8ptr_fn!("mux_sql_value_as_string");
        i8ptr_i8ptr_fn!("mux_sql_value_as_bytes");
        void_i8ptr_fn!("mux_sql_connection_close");
        i8ptr_i8ptr_i8ptr_fn!("mux_sql_connection_execute");
        module.add_function(
            "mux_sql_connection_execute_params",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        i8ptr_i8ptr_i8ptr_fn!("mux_sql_connection_query");
        module.add_function(
            "mux_sql_connection_query_params",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_sql_connection_begin_transaction");
        i8ptr_i8ptr_fn!("mux_sql_transaction_commit");
        i8ptr_i8ptr_fn!("mux_sql_transaction_rollback");
        i8ptr_i8ptr_i8ptr_fn!("mux_sql_transaction_execute");
        module.add_function(
            "mux_sql_transaction_execute_params",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        i8ptr_i8ptr_i8ptr_fn!("mux_sql_transaction_query");
        module.add_function(
            "mux_sql_transaction_query_params",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_sql_resultset_rows");
        i8ptr_i8ptr_fn!("mux_sql_resultset_next");
        i8ptr_i8ptr_fn!("mux_sql_resultset_columns");
    }

    pub(super) fn box_value(&mut self, val: BasicValueEnum<'a>) -> PointerValue<'a> {
        if val.is_int_value() {
            let int_val = val.into_int_value();
            // Check if this is a bool (i1) - LLVM considers i1 as an int type
            if int_val.get_type().get_bit_width() == 1 {
                // Bool: extend i1 to i32 for mux_bool_value
                let i32_val = self
                    .builder
                    .build_int_z_extend(int_val, self.context.i32_type(), "bool_to_i32")
                    .expect("bool extension should succeed");
                let call = self
                    .generate_runtime_call("mux_bool_value", &[i32_val.into()])
                    .expect("mux_bool_value should always return a value");
                self.register_temp(call);
                call.into_pointer_value()
            } else {
                // Regular int (i64)
                let call = self
                    .generate_runtime_call("mux_int_value", &[int_val.into()])
                    .expect("mux_int_value should always return a value");
                self.register_temp(call);
                call.into_pointer_value()
            }
        } else if val.is_float_value() {
            let call = self
                .generate_runtime_call("mux_float_value", &[val.into()])
                .expect("mux_float_value should always return a value");
            self.register_temp(call);
            call.into_pointer_value()
        } else if val.is_pointer_value() {
            // assume string or already boxed Value (from Map/Set/List literals)
            // map/Set/List literals already return *mut Value pointers, so just return as-is.
            // Already-owned pointers were registered by whatever produced them.
            val.into_pointer_value()
        } else if val.is_struct_value() {
            // user-defined enum values (structs): box into Value::Opaque
            let struct_val = val.into_struct_value();
            let struct_type = struct_val.get_type();
            let temp_ptr = self
                .builder
                .build_alloca(struct_type, "temp_enum_box")
                .expect("alloca should succeed");
            self.builder
                .build_store(temp_ptr, struct_val)
                .expect("store should succeed");
            let size = struct_type
                .size_of()
                .expect("struct type should have a size");
            let call = self
                .generate_runtime_call("mux_box_enum", &[temp_ptr.into(), size.into()])
                .expect("mux_box_enum should always return a value");
            self.register_temp(call);
            call.into_pointer_value()
        } else {
            panic!("Unexpected value type in box_value")
        }
    }

    fn extract_primitive_from_ptr<T: TryFrom<BasicValueEnum<'a>>>(
        &mut self,
        ptr: PointerValue<'a>,
        getter_func_name: &str,
        error_msg: &str,
    ) -> Result<T, String>
    where
        <T as TryFrom<BasicValueEnum<'a>>>::Error: std::fmt::Debug,
    {
        let func = self
            .runtime_function(getter_func_name)
            .ok_or(format!("{} not found", getter_func_name))?;
        let result = self
            .builder
            .build_call(func, &[ptr.into()], "extract")
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or(error_msg)?;
        result
            .try_into()
            .map_err(|_| format!("Conversion failed for {}", getter_func_name))
    }

    pub(super) fn get_raw_int_value(
        &mut self,
        val: BasicValueEnum<'a>,
    ) -> Result<IntValue<'a>, String> {
        if val.is_int_value() {
            Ok(val.into_int_value())
        } else if val.is_pointer_value() {
            let ptr = val.into_pointer_value();
            self.extract_primitive_from_ptr(ptr, "mux_value_get_int", "Call returned no value")
        } else {
            Err("Expected int value or pointer".to_string())
        }
    }

    pub(super) fn get_raw_float_value(
        &mut self,
        val: BasicValueEnum<'a>,
    ) -> Result<FloatValue<'a>, String> {
        if val.is_float_value() {
            Ok(val.into_float_value())
        } else if val.is_pointer_value() {
            let ptr = val.into_pointer_value();
            self.extract_primitive_from_ptr(ptr, "mux_value_get_float", "Call returned no value")
        } else {
            Err("Expected float value or pointer".to_string())
        }
    }

    pub(super) fn get_raw_bool_value(
        &mut self,
        val: BasicValueEnum<'a>,
    ) -> Result<IntValue<'a>, String> {
        if val.is_int_value() {
            let int_val = val.into_int_value();
            // If already i1, return as-is. If i32 (from runtime), truncate to i1
            if int_val.get_type().get_bit_width() == 1 {
                Ok(int_val)
            } else {
                // Truncate i32 to i1
                let i1_val = self
                    .builder
                    .build_int_truncate(int_val, self.context.bool_type(), "trunc_to_i1")
                    .map_err(|e| e.to_string())?;
                Ok(i1_val)
            }
        } else if val.is_pointer_value() {
            // use safe runtime function to extract bool
            let ptr = val.into_pointer_value();
            let get_bool_fn = self
                .runtime_function("mux_value_get_bool")
                .ok_or("mux_value_get_bool not found")?;
            let i32_result = self
                .builder
                .build_call(get_bool_fn, &[ptr.into()], "get_bool")
                .map_err(|e| e.to_string())?
                .try_as_basic_value()
                .basic()
                .ok_or("Call returned no value")?
                .into_int_value();
            // Truncate i32 to i1 so callers get consistent bool type
            let i1_val = self
                .builder
                .build_int_truncate(i32_result, self.context.bool_type(), "trunc_to_i1")
                .map_err(|e| e.to_string())?;
            Ok(i1_val)
        } else {
            Err("Expected bool value or pointer".to_string())
        }
    }

    /// Extracts a value from a *mut Value pointer based on the wrapped type.
    /// Used for unwrapping Optional<T> and Result<T, E> in match statements.
    ///
    /// # Arguments
    /// * `data_ptr` - Pointer to the wrapped value (*mut Value)
    /// * `wrapped_type` - The type of the value being unwrapped
    /// * `variant_name` - Name of the variant ("Some", "Ok", "Err") for error messages
    ///
    /// # Returns
    /// A tuple of (BasicValueEnum, Type) representing the extracted value and its type
    /// Emit `mux_rc_dec` on an owned `*mut Value`. Used to release an
    /// intermediate extraction result that is not otherwise stored or returned.
    pub(super) fn emit_value_decref(&self, ptr: PointerValue<'a>) -> Result<(), String> {
        let rc_dec = self
            .runtime_function("mux_rc_dec")
            .ok_or("mux_rc_dec not found")?;
        self.builder
            .build_call(rc_dec, &[ptr.into()], "rc_dec_extracted")
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Extract the payload of an Optional/Result inner value out of an owned
    /// `data_ptr` (produced by `mux_optional_data`/`mux_result_data`, which clone
    /// the inner value into a fresh allocation). For scalar and string payloads
    /// the boxed `data_ptr` is unwrapped and then released here; for
    /// collection/object/nested-wrapper payloads `data_ptr` *is* the payload and
    /// ownership is handed back to the caller unchanged.
    pub(super) fn extract_value_from_ptr(
        &mut self,
        data_ptr: PointerValue<'a>,
        wrapped_type: &Type,
        variant_name: &str,
    ) -> Result<(BasicValueEnum<'a>, Type), String> {
        match wrapped_type {
            // Primitive types need to be extracted from *mut Value
            Type::Primitive(PrimitiveType::Int) => {
                let get_int_func = self.runtime_function("mux_value_get_int").ok_or(format!(
                    "Failed to extract int from {}: mux_value_get_int not found",
                    variant_name
                ))?;
                let val = self
                    .builder
                    .build_call(get_int_func, &[data_ptr.into()], "get_int")
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("mux_value_get_int returned no value")?;
                self.emit_value_decref(data_ptr)?;
                Ok((val, Type::Primitive(PrimitiveType::Int)))
            }
            Type::Primitive(PrimitiveType::Float) => {
                let get_float_func =
                    self.runtime_function("mux_value_get_float").ok_or(format!(
                        "Failed to extract float from {}: mux_value_get_float not found",
                        variant_name
                    ))?;
                let val = self
                    .builder
                    .build_call(get_float_func, &[data_ptr.into()], "get_float")
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("mux_value_get_float returned no value")?;
                self.emit_value_decref(data_ptr)?;
                Ok((val, Type::Primitive(PrimitiveType::Float)))
            }
            Type::Primitive(PrimitiveType::Bool) => {
                let get_bool_func = self.runtime_function("mux_value_get_bool").ok_or(format!(
                    "Failed to extract bool from {}: mux_value_get_bool not found",
                    variant_name
                ))?;
                let val = self
                    .builder
                    .build_call(get_bool_func, &[data_ptr.into()], "get_bool")
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("mux_value_get_bool returned no value")?
                    .into_int_value();
                let i1_val = self
                    .builder
                    .build_int_truncate(val, self.context.bool_type(), "trunc_to_i1")
                    .map_err(|e| e.to_string())?;
                self.emit_value_decref(data_ptr)?;
                Ok((i1_val.into(), Type::Primitive(PrimitiveType::Bool)))
            }
            Type::Primitive(PrimitiveType::Char) => {
                // Char is stored as int
                let get_int_func = self.runtime_function("mux_value_get_int").ok_or(format!(
                    "Failed to extract char from {}: mux_value_get_int not found",
                    variant_name
                ))?;
                let val = self
                    .builder
                    .build_call(get_int_func, &[data_ptr.into()], "get_int")
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("mux_value_get_int returned no value")?;
                self.emit_value_decref(data_ptr)?;
                Ok((val, Type::Primitive(PrimitiveType::Char)))
            }
            Type::Primitive(PrimitiveType::Str) => {
                // String needs special handling: get C string then wrap in Mux string
                let get_string_func =
                    self.runtime_function("mux_value_get_string")
                        .ok_or(format!(
                            "Failed to extract string from {}: mux_value_get_string not found",
                            variant_name
                        ))?;
                let c_str = self
                    .builder
                    .build_call(get_string_func, &[data_ptr.into()], "get_string")
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("mux_value_get_string returned no value")?
                    .into_pointer_value();

                // Wrap the (owned) C string back into a Mux string, freeing the C
                // string in the process, then release the boxed data pointer.
                let new_string_func = self
                    .runtime_function("mux_new_string_from_owned_cstr")
                    .ok_or("mux_new_string_from_owned_cstr not found")?;
                let mux_string = self
                    .builder
                    .build_call(new_string_func, &[c_str.into()], "new_string")
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("mux_new_string_from_owned_cstr returned no value")?;
                self.emit_value_decref(data_ptr)?;

                Ok((mux_string, Type::Primitive(PrimitiveType::Str)))
            }
            Type::Primitive(PrimitiveType::Void) => Err(format!(
                "Unsupported type Void for extraction from {}",
                variant_name
            )),
            Type::Primitive(PrimitiveType::Auto) => Err(format!(
                "Unsupported type Auto for extraction from {}",
                variant_name
            )),
            // Collections, custom types, and nested Optional/Result stay as *mut Value
            Type::List(_)
            | Type::Map(_, _)
            | Type::Set(_)
            | Type::Tuple(_, _)
            | Type::Named(_, _)
            | Type::Optional(_)
            | Type::Result(_, _)
            | Type::Instantiated(_, _) => {
                // These are already *mut Value pointers, no extraction needed
                Ok((data_ptr.into(), wrapped_type.clone()))
            }
            // Reference types - unwrap the reference
            Type::Reference(inner) => self.extract_value_from_ptr(data_ptr, inner, variant_name),
            // Other types that shouldn't appear in Optional/Result
            Type::Void | Type::Never | Type::EmptyList | Type::EmptyMap | Type::EmptySet => {
                Err(format!(
                    "Unsupported type {:?} for extraction from {}",
                    wrapped_type, variant_name
                ))
            }
            Type::Function { .. } => Err(format!(
                "Unsupported type Function for extraction from {}",
                variant_name
            )),
            Type::Variable(v) | Type::Generic(v) => Err(format!(
                "Unresolved generic type {} for extraction from {}",
                v, variant_name
            )),
            Type::Module(_) => {
                panic!("Module types should not appear in codegen - they are compile-time only")
            }
        }
    }

    /// Call a runtime getter function to extract a pointer from a Value.
    fn call_value_getter(
        &mut self,
        func_name: &str,
        value_ptr: PointerValue<'a>,
        result_name: &str,
    ) -> Result<PointerValue<'a>, String> {
        let func = self
            .runtime_function(func_name)
            .ok_or_else(|| format!("{} not found", func_name))?;
        self.builder
            .build_call(func, &[value_ptr.into()], result_name)
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| format!("{} returned no value", func_name))
            .map(|v| v.into_pointer_value())
    }

    pub(super) fn extract_c_string_from_value(
        &mut self,
        value_ptr: PointerValue<'a>,
    ) -> Result<PointerValue<'a>, String> {
        self.call_value_getter("mux_value_get_string", value_ptr, "get_string")
    }

    pub(super) fn box_string_value(
        &mut self,
        cstr_ptr: PointerValue<'a>,
    ) -> Result<BasicValueEnum<'a>, String> {
        let func = self
            .runtime_function("mux_value_from_string")
            .ok_or("mux_value_from_string not found")?;
        self.builder
            .build_call(func, &[cstr_ptr.into()], "from_string")
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| "mux_value_from_string returned no value".to_string())
    }

    pub(super) fn copy_object_or_error(
        &mut self,
        ptr: PointerValue<'a>,
    ) -> Result<PointerValue<'a>, String> {
        let copy_func = self
            .runtime_function("mux_copy_object")
            .ok_or("mux_copy_object not found")?;
        let copied = self
            .builder
            .build_call(copy_func, &[ptr.into()], "copy_obj")
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or("mux_copy_object returned no value")?
            .into_pointer_value();

        let is_null = self
            .builder
            .build_is_null(copied, "copy_is_null")
            .map_err(|e| e.to_string())?;

        let current_function = self
            .builder
            .get_insert_block()
            .ok_or("No current basic block")?
            .get_parent()
            .ok_or("No current function")?;

        let error_bb = self
            .context
            .append_basic_block(current_function, "copy_error");
        let continue_bb = self
            .context
            .append_basic_block(current_function, "copy_continue");

        self.builder
            .build_conditional_branch(is_null, error_bb, continue_bb)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(error_bb);
        self.emit_runtime_fatal("cannot copy object of this type", None, "copy_error")?;

        self.builder.position_at_end(continue_bb);
        Ok(copied)
    }

    pub(super) fn extract_list_from_value(
        &mut self,
        value_ptr: PointerValue<'a>,
    ) -> Result<PointerValue<'a>, String> {
        self.call_value_getter("mux_value_get_list", value_ptr, "get_list")
    }

    pub(super) fn extract_map_from_value(
        &mut self,
        value_ptr: PointerValue<'a>,
    ) -> Result<PointerValue<'a>, String> {
        self.call_value_getter("mux_value_get_map", value_ptr, "get_map")
    }

    pub(super) fn extract_set_from_value(
        &mut self,
        value_ptr: PointerValue<'a>,
    ) -> Result<PointerValue<'a>, String> {
        self.call_value_getter("mux_value_get_set", value_ptr, "get_set")
    }
}
