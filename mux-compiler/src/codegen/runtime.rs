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

/// Numeric codes owned by `mux-runtime` for terminating failures. Keep this
/// mirror in lockstep with `mux_runtime::panic::RuntimeErrorCode`; the compiler
/// cannot depend on the runtime crate because it supports separately built
/// runtimes and cross-target compilation.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(super) enum RuntimeErrorCode {
    IndexOutOfBounds = 600,
    KeyNotFound = 601,
    DivisionByZero = 602,
    AssertionFailed = 603,
    WhereConstraintViolation = 604,
    IntegerOverflow = 605,
    InternalRuntime = 699,
}

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
        let Some(func) = self.runtime_function(name) else {
            panic!("Function '{name}' not found in module");
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

        module.add_function(
            "mux_coverage_record",
            context.void_type().fn_type(
                &[
                    context.ptr_type(AddressSpace::default()).into(),
                    context.i64_type().into(),
                    context.i32_type().into(),
                    context.i64_type().into(),
                    context.i32_type().into(),
                ],
                false,
            ),
            None,
        );

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
        macro_rules! i8ptr_i8ptr_i64_fn {
            ($name:expr) => {
                module.add_function(
                    $name,
                    i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
                    None,
                )
            };
        }
        macro_rules! i8ptr_i8ptr_bool_fn {
            ($name:expr) => {
                module.add_function(
                    $name,
                    i8_ptr.fn_type(&[i8_ptr.into(), context.bool_type().into()], false),
                    None,
                )
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
        macro_rules! i8ptr_i8ptr_i8ptr_i8ptr_fn {
            ($name:expr) => {
                module.add_function(
                    $name,
                    i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
                    None,
                )
            };
        }
        macro_rules! i8ptr_i8ptr_i8ptr_i8ptr_i8ptr_fn {
            ($name:expr) => {
                module.add_function(
                    $name,
                    i8_ptr.fn_type(
                        &[i8_ptr.into(), i8_ptr.into(), i8_ptr.into(), i8_ptr.into()],
                        false,
                    ),
                    None,
                )
            };
        }
        macro_rules! i8ptr_i64_i64_fn {
            ($name:expr) => {
                module.add_function(
                    $name,
                    i8_ptr.fn_type(&[i64_type.into(), i64_type.into()], false),
                    None,
                )
            };
        }
        macro_rules! bool_i8ptr_fn {
            ($name:expr) => {
                module.add_function(
                    $name,
                    context.bool_type().fn_type(&[i8_ptr.into()], false),
                    None,
                )
            };
        }
        macro_rules! bool_i8ptr_i8ptr_fn {
            ($name:expr) => {
                module.add_function(
                    $name,
                    context
                        .bool_type()
                        .fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
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
            "mux_panic_cstr_code",
            void_type.fn_type(
                &[context.i32_type().into(), i8_ptr.into(), i8_ptr.into()],
                false,
            ),
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

        // A capture cell is the shared storage of a captured variable, so it is
        // reference counted rather than owned by one closure.
        module.add_function(
            "mux_cell_alloc",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_cell_retain",
            void_type.fn_type(&[i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_cell_release",
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

        // Decomposition. Every position is a character position, matching
        // mux_string_length.
        for owned in [
            "mux_string_trim",
            "mux_string_to_upper",
            "mux_string_to_lower",
        ] {
            module.add_function(owned, i8_ptr.fn_type(&[i8_ptr.into()], false), None);
        }
        module.add_function(
            "mux_string_split",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_string_to_list",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_string_char_at",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_string_slice",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into(), i64_type.into()], false),
            None,
        );
        for pred in ["mux_string_starts_with", "mux_string_ends_with"] {
            module.add_function(
                pred,
                context
                    .bool_type()
                    .fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
                None,
            );
        }
        module.add_function(
            "mux_string_index_of",
            i64_type.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_string_replace",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_list_slice_value",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into(), i64_type.into()], false),
            None,
        );

        // Lexicographic ordering, negative / zero / positive like strcmp. The
        // relational operators on `string` lower to this.
        module.add_function(
            "mux_string_compare",
            context
                .i64_type()
                .fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_string_hash",
            i64_type.fn_type(&[i8_ptr.into()], false),
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
            ("mux_int_to_byte", i64_type.into()),
            ("mux_int_to_float", i64_type.into()),
            ("mux_float_to_int", f64_type.into()),
            ("mux_float_to_string", f64_type.into()),
            ("mux_bool_to_string", i32_type.into()),
            ("mux_char_to_int", i64_type.into()),
            ("mux_char_to_string", i64_type.into()),
        ] {
            add_conversion_fn(module, i8_ptr, name, from_ty);
        }
        module.add_function(
            "mux_char_to_codepoint",
            i64_type.fn_type(&[i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_float_hash",
            i64_type.fn_type(&[f64_type.into()], false),
            None,
        );

        for name in [
            "mux_bool_to_int",
            "mux_bool_to_float",
            "mux_string_to_string",
            "mux_string_to_int",
            "mux_string_to_byte",
            "mux_string_to_float",
            "mux_string_to_bool",
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

        // The equality, ordering and hash a class declares. Unlike the copy and
        // destructor callbacks above, these take the boxed object rather than
        // its data buffer, because they are the class's own methods.
        for name in [
            "mux_register_object_equals",
            "mux_register_object_compare",
            "mux_register_object_hash",
        ] {
            module.add_function(
                name,
                void_type.fn_type(&[i32_type.into(), i8_ptr.into()], false),
                None,
            );
        }

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

        // mux_box_enum_managed(bytes, size, clone_glue, drop_glue, cmp_glue,
        //                      hash_glue) -> *mut Value
        module.add_function(
            "mux_box_enum_managed",
            i8_ptr.fn_type(
                &[
                    i8_ptr.into(),
                    i64_type.into(),
                    i8_ptr.into(),
                    i8_ptr.into(),
                    i8_ptr.into(),
                    i8_ptr.into(),
                ],
                false,
            ),
            None,
        );

        // mux_value_compare(a, b) -> i32 (three-way, for enum compare glue)
        module.add_function(
            "mux_value_compare",
            i32_type.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );

        module.add_function(
            "mux_value_unbox_enum",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );

        // mux_value_hash(value) -> u64, for a pointer payload inside enum hash glue
        module.add_function(
            "mux_value_hash",
            i64_type.fn_type(&[i8_ptr.into()], false),
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

        module.add_function("mux_bytes_new", i8_ptr.fn_type(&[], false), None);
        module.add_function(
            "mux_bytes_from_data",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_bytes_from_list",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_bytes_to_list",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_bytes_length",
            i64_type.fn_type(&[i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_bytes_is_empty",
            context.bool_type().fn_type(&[i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_bytes_get",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_bytes_push_back",
            void_type.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_bytes_push_front",
            void_type.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_bytes_clear",
            void_type.fn_type(&[i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_bytes_extend",
            void_type.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        for name in [
            "mux_bytes_pop_back",
            "mux_bytes_pop_front",
            "mux_bytes_to_utf8",
        ] {
            module.add_function(name, i8_ptr.fn_type(&[i8_ptr.into()], false), None);
        }
        module.add_function(
            "mux_bytes_to_utf8_lossy",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_bytes_reserve",
            void_type.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_bytes_truncate",
            void_type.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_bytes_resize",
            void_type.fn_type(&[i8_ptr.into(), i64_type.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_bytes_fill",
            void_type.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_bytes_contains",
            context
                .bool_type()
                .fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_bytes_find",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_bytes_slice",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_bytes_insert",
            void_type.fn_type(&[i8_ptr.into(), i64_type.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_bytes_remove",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_bytes_copy_within",
            void_type.fn_type(
                &[
                    i8_ptr.into(),
                    i64_type.into(),
                    i64_type.into(),
                    i64_type.into(),
                ],
                false,
            ),
            None,
        );
        module.add_function(
            "mux_bytes_format",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_bytes_read_uint",
            i8_ptr.fn_type(
                &[
                    i8_ptr.into(),
                    i64_type.into(),
                    i64_type.into(),
                    context.bool_type().into(),
                ],
                false,
            ),
            None,
        );
        module.add_function(
            "mux_bytes_write_uint",
            i8_ptr.fn_type(
                &[
                    i8_ptr.into(),
                    i64_type.into(),
                    i64_type.into(),
                    i64_type.into(),
                    context.bool_type().into(),
                ],
                false,
            ),
            None,
        );
        module.add_function(
            "mux_bytes_read_float",
            i8_ptr.fn_type(
                &[i8_ptr.into(), i64_type.into(), context.bool_type().into()],
                false,
            ),
            None,
        );
        module.add_function(
            "mux_bytes_write_float",
            i8_ptr.fn_type(
                &[
                    i8_ptr.into(),
                    i64_type.into(),
                    i8_ptr.into(),
                    context.bool_type().into(),
                ],
                false,
            ),
            None,
        );
        module.add_function(
            "mux_bytes_read_varint",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_bytes_write_varint",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_bytes_cursor_new",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );
        for name in [
            "mux_bytes_cursor_position",
            "mux_bytes_cursor_remaining",
            "mux_bytes_cursor_into_bytes",
        ] {
            module.add_function(name, i8_ptr.fn_type(&[i8_ptr.into()], false), None);
        }
        module.add_function(
            "mux_bytes_cursor_read_bytes",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_bytes_cursor_read_uint",
            i8_ptr.fn_type(
                &[i8_ptr.into(), i64_type.into(), context.bool_type().into()],
                false,
            ),
            None,
        );
        module.add_function(
            "mux_bytes_cursor_write_uint",
            i8_ptr.fn_type(
                &[
                    i8_ptr.into(),
                    i64_type.into(),
                    i64_type.into(),
                    context.bool_type().into(),
                ],
                false,
            ),
            None,
        );
        module.add_function(
            "mux_bytes_cursor_read_float",
            i8_ptr.fn_type(&[i8_ptr.into(), context.bool_type().into()], false),
            None,
        );
        module.add_function(
            "mux_bytes_cursor_write_float",
            i8_ptr.fn_type(
                &[i8_ptr.into(), i8_ptr.into(), context.bool_type().into()],
                false,
            ),
            None,
        );

        module.add_function(
            "mux_rand_bytes",
            i8_ptr.fn_type(&[i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_rand_normal",
            i8_ptr.fn_type(&[f64_type.into(), f64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_rand_exponential",
            i8_ptr.fn_type(&[f64_type.into()], false),
            None,
        );

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
        i8ptr_void_fn!("mux_result_ok_unit");
        i8ptr_void_fn!("mux_optional_none");
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
                "mux_math_trunc",
                "mux_math_fract",
                "mux_math_sinh",
                "mux_math_cosh",
                "mux_math_tanh",
                "mux_math_asinh",
                "mux_math_acosh",
                "mux_math_atanh",
                "mux_math_to_radians",
                "mux_math_to_degrees",
                "mux_math_exp2",
                "mux_math_exp_m1",
                "mux_math_ln_1p",
                "mux_math_cbrt",
                "mux_math_signum",
                "mux_math_erf",
                "mux_math_gamma",
            ],
            f64_type.fn_type(&[f64_type.into()], false)
        );

        module.add_function(
            "mux_math_sum",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_math_product",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
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
            &["mux_math_clamp", "mux_math_lerp"],
            f64_type.fn_type(&[f64_type.into(), f64_type.into(), f64_type.into()], false,)
        );

        declare_extern_batch!(
            module,
            &[
                "mux_math_clamp_checked",
                "mux_math_inverse_lerp",
                "mux_math_smoothstep",
            ],
            i8_ptr.fn_type(&[f64_type.into(), f64_type.into(), f64_type.into()], false,)
        );

        declare_extern_batch!(
            module,
            &[
                "mux_math_is_nan",
                "mux_math_is_infinite",
                "mux_math_is_finite"
            ],
            context.bool_type().fn_type(&[f64_type.into()], false)
        );

        declare_extern_batch!(
            module,
            &["mux_math_gcd", "mux_math_lcm"],
            i64_type.fn_type(&[i64_type.into(), i64_type.into()], false)
        );
        module.add_function(
            "mux_math_isqrt",
            i64_type.fn_type(&[i64_type.into()], false),
            None,
        );

        declare_extern_batch!(
            module,
            &["mux_math_factorial"],
            i8_ptr.fn_type(&[i64_type.into()], false)
        );
        declare_extern_batch!(
            module,
            &["mux_math_combinations", "mux_math_permutations"],
            i8_ptr.fn_type(&[i64_type.into(), i64_type.into()], false)
        );

        declare_extern_batch!(
            module,
            &[
                "mux_byte_checked_add",
                "mux_byte_checked_sub",
                "mux_byte_checked_mul",
                "mux_byte_checked_div",
                "mux_byte_checked_rem",
                "mux_byte_shift_left",
                "mux_byte_shift_right",
            ],
            i8_ptr.fn_type(&[i64_type.into(), i64_type.into()], false)
        );
        declare_extern_batch!(
            module,
            &[
                "mux_byte_wrapping_add",
                "mux_byte_wrapping_sub",
                "mux_byte_wrapping_mul",
                "mux_byte_saturating_add",
                "mux_byte_saturating_sub",
                "mux_byte_saturating_mul",
                "mux_byte_bit_and",
                "mux_byte_bit_or",
                "mux_byte_bit_xor",
                "mux_byte_rotate_left",
                "mux_byte_rotate_right",
            ],
            i64_type.fn_type(&[i64_type.into(), i64_type.into()], false)
        );
        module.add_function(
            "mux_byte_bit_not",
            i64_type.fn_type(&[i64_type.into()], false),
            None,
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
            "mux_io_remove_dir_all",
            "mux_io_is_file",
            "mux_io_is_dir",
            "mux_io_mkdir",
            "mux_io_listdir",
            "mux_io_basename",
            "mux_io_dirname",
            "mux_io_absolute",
            "mux_io_canonical",
            "mux_io_file_size",
            "mux_io_is_symlink",
        ] {
            add_i8_fn(module, i8_ptr, name, &[i8_ptr.into()]);
        }
        add_i8_fn(module, i8_ptr, "mux_io_cwd", &[]);
        for name in ["mux_io_copy", "mux_io_rename", "mux_io_replace_atomic"] {
            add_i8_fn(module, i8_ptr, name, &[i8_ptr.into(), i8_ptr.into()]);
        }
        for name in ["mux_fs_temp_file", "mux_fs_temp_dir"] {
            add_i8_fn(module, i8_ptr, name, &[]);
        }
        add_i8_fn(module, i8_ptr, "mux_fs_is_readonly", &[i8_ptr.into()]);
        add_i8_fn(module, i8_ptr, "mux_fs_read_link", &[i8_ptr.into()]);
        add_i8_fn(module, i8_ptr, "mux_fs_directory_open", &[i8_ptr.into()]);
        add_i8_fn(module, i8_ptr, "mux_fs_directory_next", &[i8_ptr.into()]);
        void_i8ptr_fn!("mux_fs_directory_close");
        add_i8_fn(
            module,
            i8_ptr,
            "mux_fs_set_readonly",
            &[i8_ptr.into(), context.bool_type().into()],
        );
        // Environment access.
        add_i8_fn(module, i8_ptr, "mux_env_get", &[i8_ptr.into()]);
        add_i8_fn(
            module,
            i8_ptr,
            "mux_env_set",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(module, i8_ptr, "mux_env_remove", &[i8_ptr.into()]);
        add_i8_fn(module, i8_ptr, "mux_env_contains", &[i8_ptr.into()]);
        add_i8_fn(module, i8_ptr, "mux_env_entries", &[]);
        // JSON helpers
        add_i8_fn(module, i8_ptr, "mux_json_parse", &[i8_ptr.into()]);
        add_i8_fn(
            module,
            i8_ptr,
            "mux_json_parse_with_policy",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(module, i8_ptr, "mux_json_parse_lines", &[i8_ptr.into()]);
        add_i8_fn(
            module,
            i8_ptr,
            "mux_json_parse_lines_with_policy",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(module, i8_ptr, "mux_json_stringify_lines", &[i8_ptr.into()]);
        module.add_function(
            "mux_json_stringify",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        add_i8_fn(module, i8_ptr, "mux_json_from_map", &[i8_ptr.into()]);
        add_i8_fn(module, i8_ptr, "mux_json_to_map", &[i8_ptr.into()]);
        i8ptr_i8ptr_fn!("mux_json_error_from_message");
        i8ptr_i8ptr_fn!("mux_json_error_kind");
        i8ptr_i8ptr_fn!("mux_json_error_detail");
        i8ptr_i8ptr_fn!("mux_json_error_message");
        i8ptr_i8ptr_fn!("mux_json_error_to_string");
        i8ptr_i8ptr_fn!("mux_byte_error_from_message");
        i8ptr_i8ptr_fn!("mux_byte_error_kind");
        i8ptr_i8ptr_fn!("mux_byte_error_detail");
        i8ptr_i8ptr_fn!("mux_byte_error_message");
        i8ptr_i8ptr_fn!("mux_byte_error_to_string");
        i8ptr_i8ptr_fn!("mux_bytes_error_from_message");
        i8ptr_i8ptr_fn!("mux_bytes_error_kind");
        i8ptr_i8ptr_fn!("mux_bytes_error_detail");
        i8ptr_i8ptr_fn!("mux_bytes_error_message");
        i8ptr_i8ptr_fn!("mux_bytes_error_to_string");
        add_i8_fn(
            module,
            i8_ptr,
            "mux_json_set_field",
            &[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_json_push",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_json_at_pointer",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_json_set_pointer",
            &[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_json_remove_pointer",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_json_merge_patch",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_json_apply_patch",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        // Typed accessors, each returning an owned optional<T>.
        for accessor in [
            "mux_json_as_string",
            "mux_json_as_int",
            "mux_json_as_float",
            "mux_json_as_bool",
            "mux_json_as_list",
            "mux_json_as_map",
            "mux_json_as_number",
        ] {
            add_i8_fn(module, i8_ptr, accessor, &[i8_ptr.into()]);
        }
        module.add_function(
            "mux_json_is_null",
            context.bool_type().fn_type(&[i8_ptr.into()], false),
            None,
        );
        // One field of a JSON object by name, as an owned optional<Json>.
        // `none` means absent or not an object; a field holding null comes back
        // as some(null), which is what keeps the two apart for a deserializer
        // that must reject a missing required field but accept an explicit one.
        add_i8_fn(
            module,
            i8_ptr,
            "mux_json_field",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        // The total renders behind `to_string` on Json and Csv (#405).
        add_i8_fn(module, i8_ptr, "mux_json_to_string", &[i8_ptr.into()]);
        add_i8_fn(module, i8_ptr, "mux_json_canonical", &[i8_ptr.into()]);
        add_i8_fn(
            module,
            i8_ptr,
            "mux_json_number_to_string",
            &[i8_ptr.into()],
        );
        add_i8_fn(module, i8_ptr, "mux_json_number_as_int", &[i8_ptr.into()]);
        add_i8_fn(module, i8_ptr, "mux_json_number_as_float", &[i8_ptr.into()]);
        add_i8_fn(module, i8_ptr, "mux_csv_render", &[i8_ptr.into()]);
        // CSV helpers
        // One map per row keyed by header, so a named column can be read
        // without finding its index per row in generated code.
        add_i8_fn(module, i8_ptr, "mux_csv_rows_as_maps", &[i8_ptr.into()]);
        add_i8_fn(module, i8_ptr, "mux_csv_parse", &[i8_ptr.into()]);
        add_i8_fn(
            module,
            i8_ptr,
            "mux_csv_parse_with_headers",
            &[i8_ptr.into()],
        );
        module.add_function(
            "mux_csv_parse_with_options",
            i8_ptr.fn_type(
                &[
                    i8_ptr.into(),
                    i64_type.into(),
                    i64_type.into(),
                    context.i32_type().into(),
                    context.i32_type().into(),
                    context.i32_type().into(),
                ],
                false,
            ),
            None,
        );
        module.add_function("mux_csv_reader_new", i8_ptr.fn_type(&[], false), None);
        module.add_function(
            "mux_csv_reader_from_bytes",
            i8_ptr.fn_type(&[i8_ptr.into(), context.bool_type().into()], false),
            None,
        );
        module.add_function(
            "mux_csv_reader_from_reader",
            i8_ptr.fn_type(&[i8_ptr.into(), context.bool_type().into()], false),
            None,
        );
        add_i8_fn(module, i8_ptr, "mux_csv_reader_headers", &[i8_ptr.into()]);
        add_i8_fn(module, i8_ptr, "mux_csv_reader_read", &[i8_ptr.into()]);
        module.add_function("mux_csv_writer_new", i8_ptr.fn_type(&[], false), None);
        module.add_function(
            "mux_csv_writer_from_config",
            i8_ptr.fn_type(&[i64_type.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_csv_writer_from_writer",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into(), i64_type.into()], false),
            None,
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_csv_writer_write",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(module, i8_ptr, "mux_csv_writer_flush", &[i8_ptr.into()]);
        add_i8_fn(module, i8_ptr, "mux_csv_writer_bytes", &[i8_ptr.into()]);
        i8ptr_i8ptr_fn!("mux_csv_error_from_message");
        i8ptr_i8ptr_fn!("mux_csv_error_kind");
        i8ptr_i8ptr_fn!("mux_csv_error_detail");
        i8ptr_i8ptr_fn!("mux_csv_error_message");
        i8ptr_i8ptr_fn!("mux_csv_error_to_string");
        module.add_function(
            "mux_csv_to_string",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_csv_to_string_with",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into(), i64_type.into()], false),
            None,
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_io_write_file",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(module, i8_ptr, "mux_io_read_bytes", &[i8_ptr.into()]);
        add_i8_fn(
            module,
            i8_ptr,
            "mux_io_write_bytes",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        i8ptr_i8ptr_fn!("mux_io_error_from_message");
        i8ptr_i8ptr_fn!("mux_io_error_kind");
        i8ptr_i8ptr_fn!("mux_io_error_detail");
        i8ptr_i8ptr_fn!("mux_io_error_operation");
        i8ptr_i8ptr_fn!("mux_io_error_message");
        i8ptr_i8ptr_fn!("mux_io_error_to_string");
        module.add_function("mux_io_reader_new", i8_ptr.fn_type(&[], false), None);
        module.add_function("mux_io_stdin", i8_ptr.fn_type(&[], false), None);
        void_i8ptr_fn!("mux_io_reader_close");
        add_i8_fn(module, i8_ptr, "mux_io_reader_from_bytes", &[i8_ptr.into()]);
        add_i8_fn(module, i8_ptr, "mux_io_reader_from_file", &[i8_ptr.into()]);
        add_i8_fn(module, i8_ptr, "mux_io_reader_from_tcp", &[i8_ptr.into()]);
        add_i8_fn(
            module,
            i8_ptr,
            "mux_io_reader_read",
            &[i8_ptr.into(), i64_type.into()],
        );
        add_i8_fn(module, i8_ptr, "mux_io_reader_read_line", &[i8_ptr.into()]);
        add_i8_fn(
            module,
            i8_ptr,
            "mux_io_reader_limit",
            &[i8_ptr.into(), i64_type.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_io_reader_tee",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(module, i8_ptr, "mux_io_reader_untee", &[i8_ptr.into()]);
        add_i8_fn(
            module,
            i8_ptr,
            "mux_io_reader_read_exact",
            &[i8_ptr.into(), i64_type.into()],
        );
        add_i8_fn(module, i8_ptr, "mux_io_reader_position", &[i8_ptr.into()]);
        add_i8_fn(module, i8_ptr, "mux_io_reader_remaining", &[i8_ptr.into()]);
        add_i8_fn(
            module,
            i8_ptr,
            "mux_io_reader_read_to_end",
            &[i8_ptr.into(), i64_type.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_json_parse_reader",
            &[i8_ptr.into(), i64_type.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_json_parse_reader_with_policy",
            &[i8_ptr.into(), i64_type.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_json_token_reader_from_reader",
            &[i8_ptr.into(), i64_type.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_json_token_reader_next",
            &[i8_ptr.into()],
        );
        void_i8ptr_fn!("mux_json_token_reader_close");
        add_i8_fn(module, i8_ptr, "mux_json_token_kind", &[i8_ptr.into()]);
        add_i8_fn(module, i8_ptr, "mux_json_token_text", &[i8_ptr.into()]);
        add_i8_fn(module, i8_ptr, "mux_json_token_value", &[i8_ptr.into()]);
        add_i8_fn(
            module,
            i8_ptr,
            "mux_io_reader_copy_to",
            &[i8_ptr.into(), i8_ptr.into(), i64_type.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_io_reader_seek",
            &[i8_ptr.into(), i64_type.into()],
        );
        module.add_function("mux_io_writer_new", i8_ptr.fn_type(&[], false), None);
        module.add_function("mux_io_stdout", i8_ptr.fn_type(&[], false), None);
        module.add_function("mux_io_stderr", i8_ptr.fn_type(&[], false), None);
        void_i8ptr_fn!("mux_io_writer_close");
        add_i8_fn(module, i8_ptr, "mux_io_writer_to_file", &[i8_ptr.into()]);
        add_i8_fn(
            module,
            i8_ptr,
            "mux_io_writer_append_file",
            &[i8_ptr.into()],
        );
        add_i8_fn(module, i8_ptr, "mux_io_writer_from_tcp", &[i8_ptr.into()]);
        add_i8_fn(
            module,
            i8_ptr,
            "mux_io_writer_write",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_io_writer_write_all",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(module, i8_ptr, "mux_io_writer_bytes", &[i8_ptr.into()]);
        add_i8_fn(module, i8_ptr, "mux_io_writer_position", &[i8_ptr.into()]);
        add_i8_fn(
            module,
            i8_ptr,
            "mux_io_writer_seek",
            &[i8_ptr.into(), i64_type.into()],
        );
        add_i8_fn(module, i8_ptr, "mux_io_writer_flush", &[i8_ptr.into()]);
        add_i8_fn(
            module,
            i8_ptr,
            "mux_io_stream_from_reader",
            &[i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_io_stream_from_writer",
            &[i8_ptr.into()],
        );
        void_i8ptr_fn!("mux_io_stream_close");
        add_i8_fn(
            module,
            i8_ptr,
            "mux_io_stream_read",
            &[i8_ptr.into(), i64_type.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_io_stream_write",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(module, i8_ptr, "mux_io_stream_flush", &[i8_ptr.into()]);
        add_i8_fn(
            module,
            i8_ptr,
            "mux_io_stream_seek",
            &[i8_ptr.into(), i64_type.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_json_stringify_to",
            &[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_io_join",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        module.add_function("mux_cli_parser_new", i8_ptr.fn_type(&[], false), None);
        add_i8_fn(
            module,
            i8_ptr,
            "mux_cli_parser_set_program",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_cli_parser_set_about",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_cli_parser_set_version",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_cli_parser_set_response_files",
            &[i8_ptr.into(), context.bool_type().into()],
        );
        module.add_function(
            "mux_cli_parser_add_option",
            i8_ptr.fn_type(
                &[
                    i8_ptr.into(),
                    i8_ptr.into(),
                    i8_ptr.into(),
                    context.bool_type().into(),
                    context.bool_type().into(),
                ],
                false,
            ),
            None,
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_cli_parser_set_option_env",
            &[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_cli_parser_set_option_default",
            &[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_cli_parser_set_option_multiple",
            &[i8_ptr.into(), i8_ptr.into(), context.bool_type().into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_cli_parser_set_option_conflicts",
            &[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_cli_parser_set_option_requires",
            &[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_cli_parser_set_option_alias",
            &[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_cli_parser_set_option_group",
            &[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_cli_parser_set_option_parser",
            &[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_cli_parser_add_positional",
            &[i8_ptr.into(), i8_ptr.into(), context.bool_type().into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_cli_parser_add_subcommand",
            &[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_cli_parser_parse",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_cli_parser_parse_process",
            &[i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_cli_parser_parse_or_exit",
            &[i8_ptr.into()],
        );
        add_i8_fn(module, i8_ptr, "mux_cli_parser_help", &[i8_ptr.into()]);
        add_i8_fn(
            module,
            i8_ptr,
            "mux_cli_parser_completion",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(module, i8_ptr, "mux_cli_parser_manpage", &[i8_ptr.into()]);
        add_i8_fn(
            module,
            i8_ptr,
            "mux_cli_matches_has",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_cli_matches_get",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_cli_matches_get_int",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_cli_matches_get_float",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_cli_matches_get_bool",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_cli_matches_values",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_cli_matches_positional",
            &[i8_ptr.into(), i64_type.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_cli_matches_subcommand",
            &[i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_cli_matches_subcommand_matches",
            &[i8_ptr.into()],
        );
        add_i8_fn(module, i8_ptr, "mux_cli_matches_help", &[i8_ptr.into()]);
        module.add_function("mux_fs_path_new", i8_ptr.fn_type(&[], false), None);
        add_i8_fn(module, i8_ptr, "mux_fs_path_from_string", &[i8_ptr.into()]);
        for name in [
            "mux_fs_path_to_string",
            "mux_fs_path_display",
            "mux_fs_path_is_absolute",
            "mux_fs_path_is_relative",
            "mux_fs_path_parent",
            "mux_fs_path_file_name",
            "mux_fs_path_extension",
            "mux_fs_path_stem",
        ] {
            add_i8_fn(module, i8_ptr, name, &[i8_ptr.into()]);
        }
        for name in [
            "mux_fs_path_join",
            "mux_fs_path_with_file_name",
            "mux_fs_path_with_extension",
        ] {
            add_i8_fn(module, i8_ptr, name, &[i8_ptr.into(), i8_ptr.into()]);
        }
        module.add_function(
            "mux_net_endpoint_from_host",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_net_endpoint_from_socket_addr",
            &[i8_ptr.into()],
        );
        for name in [
            "mux_net_endpoint_to_string",
            "mux_net_endpoint_host",
            "mux_net_endpoint_port",
            "mux_net_endpoint_resolve",
        ] {
            add_i8_fn(module, i8_ptr, name, &[i8_ptr.into()]);
        }
        module.add_function(
            "mux_tls_connect",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function("mux_tls_config_new", i8_ptr.fn_type(&[], false), None);
        add_i8_fn(
            module,
            i8_ptr,
            "mux_tls_config_set_protocols",
            &[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_tls_config_set_cipher_suites",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_tls_config_set_alpn_protocols",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        module.add_function(
            "mux_tls_connect_with_config",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_tls_connect_with_roots_config",
            i8_ptr.fn_type(
                &[i8_ptr.into(), i8_ptr.into(), i8_ptr.into(), i8_ptr.into()],
                false,
            ),
            None,
        );
        module.add_function(
            "mux_tls_connect_with_roots",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_tls_accept",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_tls_accept_with_config",
            i8_ptr.fn_type(
                &[i8_ptr.into(), i8_ptr.into(), i8_ptr.into(), i8_ptr.into()],
                false,
            ),
            None,
        );
        module.add_function(
            "mux_tls_connect_with_client_cert",
            i8_ptr.fn_type(
                &[
                    i8_ptr.into(),
                    i8_ptr.into(),
                    i8_ptr.into(),
                    i8_ptr.into(),
                    i8_ptr.into(),
                ],
                false,
            ),
            None,
        );
        module.add_function(
            "mux_tls_connect_with_client_cert_config",
            i8_ptr.fn_type(
                &[
                    i8_ptr.into(),
                    i8_ptr.into(),
                    i8_ptr.into(),
                    i8_ptr.into(),
                    i8_ptr.into(),
                    i8_ptr.into(),
                ],
                false,
            ),
            None,
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_tls_read",
            &[i8_ptr.into(), i64_type.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_tls_write",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(module, i8_ptr, "mux_tls_flush", &[i8_ptr.into()]);
        add_i8_fn(module, i8_ptr, "mux_tls_shutdown", &[i8_ptr.into()]);
        for name in [
            "mux_tls_peer_certificates",
            "mux_tls_protocol_version",
            "mux_tls_cipher_suite",
            "mux_tls_alpn_protocol",
        ] {
            add_i8_fn(module, i8_ptr, name, &[i8_ptr.into()]);
        }
        module.add_function(
            "mux_tls_error_from_message",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );
        for name in [
            "mux_tls_error_kind",
            "mux_tls_error_detail",
            "mux_tls_error_message",
            "mux_tls_error_to_string",
        ] {
            add_i8_fn(module, i8_ptr, name, &[i8_ptr.into()]);
        }

        for name in [
            "mux_datetime_now",
            "mux_datetime_now_millis",
            "mux_datetime_now_micros",
            "mux_datetime_now_nanos",
        ] {
            module.add_function(name, i8_ptr.fn_type(&[], false), None);
        }
        module.add_function(
            "mux_datetime_error_from_message",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );
        for name in [
            "mux_datetime_error_kind",
            "mux_datetime_error_detail",
            "mux_datetime_error_message",
            "mux_datetime_error_to_string",
        ] {
            add_i8_fn(module, i8_ptr, name, &[i8_ptr.into()]);
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
        for name in [
            "mux_datetime_format",
            "mux_datetime_format_local",
            "mux_datetime_parse_timestamp",
            "mux_datetime_parse_datetime",
            "mux_datetime_parse_http_date",
        ] {
            module.add_function(
                name,
                if name == "mux_datetime_parse_timestamp"
                    || name == "mux_datetime_parse_datetime"
                    || name == "mux_datetime_parse_http_date"
                {
                    i8_ptr.fn_type(&[i8_ptr.into()], false)
                } else {
                    i8_ptr.fn_type(&[i64_type.into(), i8_ptr.into()], false)
                },
                None,
            );
        }
        module.add_function(
            "mux_datetime_format_timestamp",
            i8_ptr.fn_type(&[i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_datetime_format_http_date",
            i8_ptr.fn_type(&[i64_type.into()], false),
            None,
        );

        for name in [
            "mux_datetime_date_new",
            "mux_datetime_time_new",
            "mux_datetime_datetime_new",
            "mux_datetime_datetime_now",
            "mux_datetime_zoned_datetime_new",
            "mux_datetime_instant_new",
            "mux_datetime_instant_now",
            "mux_datetime_duration_new",
            "mux_datetime_period_new",
        ] {
            module.add_function(name, i8_ptr.fn_type(&[], false), None);
        }
        for name in [
            "mux_datetime_date_parse",
            "mux_datetime_time_parse",
            "mux_datetime_datetime_parse",
            "mux_datetime_zoned_datetime_parse",
        ] {
            module.add_function(name, i8_ptr.fn_type(&[i8_ptr.into()], false), None);
        }
        for name in [
            "mux_datetime_zoned_datetime_to_string",
            "mux_datetime_zoned_datetime_zone",
            "mux_datetime_zoned_datetime_offset_seconds",
            "mux_datetime_zoned_datetime_date",
            "mux_datetime_zoned_datetime_time",
            "mux_datetime_zoned_datetime_instant",
            "mux_datetime_local_resolution_kind",
            "mux_datetime_local_resolution_earlier",
            "mux_datetime_local_resolution_later",
        ] {
            module.add_function(name, i8_ptr.fn_type(&[i8_ptr.into()], false), None);
        }
        for name in [
            "mux_datetime_instant_from_unix_nanos",
            "mux_datetime_duration_from_seconds",
            "mux_datetime_duration_from_millis",
            "mux_datetime_duration_from_micros",
            "mux_datetime_duration_from_nanos",
        ] {
            module.add_function(name, i8_ptr.fn_type(&[i64_type.into()], false), None);
        }
        for name in [
            "mux_datetime_date_to_string",
            "mux_datetime_time_to_string",
            "mux_datetime_datetime_to_string",
        ] {
            module.add_function(name, i8_ptr.fn_type(&[i8_ptr.into()], false), None);
        }
        for name in [
            "mux_datetime_date_format",
            "mux_datetime_time_format",
            "mux_datetime_datetime_format",
            "mux_datetime_datetime_parse_pattern",
        ] {
            module.add_function(
                name,
                i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
                None,
            );
        }
        for name in [
            "mux_datetime_date_year",
            "mux_datetime_date_month",
            "mux_datetime_date_day",
            "mux_datetime_date_weekday",
            "mux_datetime_time_hour",
            "mux_datetime_time_minute",
            "mux_datetime_time_second",
            "mux_datetime_time_nanosecond",
            "mux_datetime_period_years",
            "mux_datetime_period_months",
            "mux_datetime_period_days",
        ] {
            module.add_function(name, i64_type.fn_type(&[i8_ptr.into()], false), None);
        }
        for name in [
            "mux_datetime_datetime_unix_seconds",
            "mux_datetime_datetime_unix_nanos",
            "mux_datetime_instant_unix_nanos",
            "mux_datetime_duration_to_nanos",
        ] {
            module.add_function(name, i8_ptr.fn_type(&[i8_ptr.into()], false), None);
        }
        module.add_function(
            "mux_datetime_date_from_parts",
            i8_ptr.fn_type(&[i64_type.into(), i64_type.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_datetime_time_from_parts",
            i8_ptr.fn_type(
                &[
                    i64_type.into(),
                    i64_type.into(),
                    i64_type.into(),
                    i64_type.into(),
                ],
                false,
            ),
            None,
        );
        module.add_function(
            "mux_datetime_datetime_from_timestamp",
            i8_ptr.fn_type(&[i64_type.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_datetime_zoned_datetime_from_instant",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        for name in [
            "mux_datetime_zoned_datetime_from_local",
            "mux_datetime_zoned_datetime_resolve_local",
        ] {
            module.add_function(
                name,
                i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
                None,
            );
        }
        module.add_function(
            "mux_datetime_period_from_parts",
            i8_ptr.fn_type(&[i64_type.into(), i64_type.into(), i64_type.into()], false),
            None,
        );
        for name in [
            "mux_datetime_datetime_from_date_time",
            "mux_datetime_datetime_add_duration",
            "mux_datetime_zoned_datetime_add_duration",
            "mux_datetime_instant_add_duration",
            "mux_datetime_instant_duration_since",
            "mux_datetime_duration_add",
            "mux_datetime_duration_sub",
            "mux_datetime_period_add_to_date",
        ] {
            module.add_function(
                name,
                i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
                None,
            );
        }
        module.add_function(
            "mux_datetime_date_add_days",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );

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
        i8ptr_i8ptr_i8ptr_fn!("mux_mutex_with_lock");
        i8ptr_i8ptr_fn!("mux_mutex_with_value");
        i8ptr_void_fn!("mux_rwlock_new");
        i8ptr_i8ptr_fn!("mux_rwlock_read_lock");
        i8ptr_i8ptr_fn!("mux_rwlock_write_lock");
        i8ptr_i8ptr_fn!("mux_rwlock_unlock");
        i8ptr_i8ptr_i8ptr_fn!("mux_rwlock_with_read");
        i8ptr_i8ptr_i8ptr_fn!("mux_rwlock_with_write");
        i8ptr_i8ptr_fn!("mux_rwlock_with_value");
        i8ptr_void_fn!("mux_condvar_new");
        i8ptr_i8ptr_i8ptr_fn!("mux_condvar_wait");
        module.add_function(
            "mux_condvar_wait_timeout",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_condvar_signal");
        module.add_function(
            "mux_condvar_broadcast",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );
        i8ptr_void_fn!("mux_atomic_int_new");
        i8ptr_void_fn!("mux_atomic_bool_new");
        i8ptr_i8ptr_fn!("mux_atomic_int_load");
        i8ptr_i8ptr_fn!("mux_atomic_bool_load");
        i8ptr_i8ptr_i64_fn!("mux_atomic_int_store");
        i8ptr_i8ptr_bool_fn!("mux_atomic_bool_store");
        i8ptr_i8ptr_i64_fn!("mux_atomic_int_add");
        i8ptr_i8ptr_i64_fn!("mux_atomic_int_swap");
        i8ptr_i8ptr_bool_fn!("mux_atomic_bool_swap");
        module.add_function(
            "mux_atomic_int_compare_exchange",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_atomic_bool_compare_exchange",
            i8_ptr.fn_type(
                &[
                    i8_ptr.into(),
                    context.bool_type().into(),
                    context.bool_type().into(),
                ],
                false,
            ),
            None,
        );
        i8ptr_void_fn!("mux_cancellation_new");
        i8ptr_i8ptr_fn!("mux_cancellation_cancel");
        i8ptr_i8ptr_fn!("mux_cancellation_is_cancelled");
        module.add_function(
            "mux_atomic_int_with_value",
            i8_ptr.fn_type(&[i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_atomic_bool_with_value",
            i8_ptr.fn_type(&[context.bool_type().into()], false),
            None,
        );
        module.add_function(
            "mux_semaphore_with_permits",
            i8_ptr.fn_type(&[i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_semaphore_acquire");
        i8ptr_i8ptr_fn!("mux_semaphore_try_acquire");
        module.add_function(
            "mux_semaphore_acquire_timeout",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_semaphore_release");
        module.add_function(
            "mux_barrier_with_size",
            i8_ptr.fn_type(&[i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_barrier_wait");
        i8ptr_void_fn!("mux_channel_new");
        i8ptr_void_fn!("mux_channel_new_unbounded");
        module.add_function(
            "mux_channel_new_bounded",
            i8_ptr.fn_type(&[i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_i8ptr_fn!("mux_channel_send");
        i8ptr_i8ptr_i8ptr_fn!("mux_channel_try_send");
        module.add_function(
            "mux_channel_send_timeout",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_i8ptr_i8ptr_fn!("mux_channel_send_cancelled");
        i8ptr_i8ptr_fn!("mux_channel_recv");
        i8ptr_i8ptr_fn!("mux_channel_try_recv");
        module.add_function(
            "mux_channel_recv_timeout",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_channel_select",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_i8ptr_fn!("mux_channel_recv_cancelled");
        i8ptr_i8ptr_fn!("mux_channel_close");
        i8ptr_i8ptr_fn!("mux_channel_is_closed");
        i8ptr_i8ptr_fn!("mux_channel_capacity");
        i8ptr_void_fn!("mux_once_new");
        i8ptr_i8ptr_i8ptr_fn!("mux_once_call");
        i8ptr_void_fn!("mux_pool_new");
        module.add_function(
            "mux_pool_with_size",
            i8_ptr.fn_type(&[i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_pool_with_config",
            i8_ptr.fn_type(&[i64_type.into(), i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_i8ptr_fn!("mux_pool_submit");
        i8ptr_i8ptr_i8ptr_fn!("mux_pool_try_submit");
        module.add_function(
            "mux_pool_submit_timeout",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_pool_cancel_pending");
        i8ptr_i8ptr_i8ptr_i8ptr_fn!("mux_pool_map");
        i8ptr_i8ptr_fn!("mux_pool_close");

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
            "mux_random_seeded",
            i8_ptr.fn_type(&[i64_type.into()], false),
            None,
        );
        module.add_function("mux_random_system", i8_ptr.fn_type(&[], false), None);
        module.add_function(
            "mux_random_next_int",
            i64_type.fn_type(&[i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_random_next_range",
            i64_type.fn_type(&[i8_ptr.into(), i64_type.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_random_next_float",
            f64_type.fn_type(&[i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_random_next_bool",
            context.bool_type().fn_type(&[i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_random_bytes",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_random_normal",
            i8_ptr.fn_type(&[i8_ptr.into(), f64_type.into(), f64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_random_exponential",
            i8_ptr.fn_type(&[i8_ptr.into(), f64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_random_choose");
        module.add_function(
            "mux_random_shuffle",
            void_type.fn_type(&[i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_random_sample",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_i8ptr_fn!("mux_random_weighted_choice");

        module.add_function(
            "mux_assert",
            void_type.fn_type(&[context.i32_type().into(), i8_ptr.into()], false),
            None,
        );

        module.add_function("mux_net_http_headers_new", i8_ptr.fn_type(&[], false), None);
        module.add_function(
            "mux_net_http_headers_set",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_net_http_headers_append",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_headers_get");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_headers_values");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_headers_remove");
        i8ptr_i8ptr_fn!("mux_http_error_from_message");
        i8ptr_i8ptr_fn!("mux_http_error_kind");
        i8ptr_i8ptr_fn!("mux_http_error_detail");
        i8ptr_i8ptr_fn!("mux_http_error_status");
        i8ptr_i8ptr_fn!("mux_http_error_method");
        i8ptr_i8ptr_fn!("mux_http_error_url");
        i8ptr_i8ptr_fn!("mux_http_error_message");
        i8ptr_i8ptr_fn!("mux_http_error_to_string");
        module.add_function("mux_net_http_request_new", i8_ptr.fn_type(&[], false), None);
        i8ptr_i8ptr_i8ptr_i8ptr_i8ptr_fn!("mux_net_http_request_from_config");
        i8ptr_i8ptr_fn!("mux_net_http_request_method");
        i8ptr_i8ptr_fn!("mux_net_http_request_url");
        i8ptr_i8ptr_fn!("mux_net_http_request_id");
        i8ptr_i8ptr_fn!("mux_net_http_request_proxy");
        i8ptr_i8ptr_fn!("mux_net_http_request_headers_field");
        i8ptr_i8ptr_fn!("mux_net_http_request_body");
        i8ptr_i8ptr_fn!("mux_net_http_request_connect_timeout");
        i8ptr_i8ptr_fn!("mux_net_http_request_timeout");
        i8ptr_i8ptr_fn!("mux_net_http_request_max_redirects");
        i8ptr_i8ptr_fn!("mux_net_http_request_retries");
        i8ptr_i8ptr_fn!("mux_net_http_request_retry_backoff");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_request_set_method_field");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_request_set_url_field");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_request_set_id_field");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_request_set_proxy_field");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_request_set_headers_field");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_request_set_body_field");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_request_set_connect_timeout_field");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_request_set_timeout_field");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_request_set_max_redirects_field");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_request_set_retries_field");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_request_set_retry_backoff_field");
        i8ptr_i8ptr_fn!("mux_net_http_request_set_body_reader");
        i8ptr_i8ptr_fn!("mux_net_http_request_send");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_request_path_param");
        i8ptr_i8ptr_fn!("mux_net_http_request_read");
        module.add_function(
            "mux_net_http_server_config_new",
            i8_ptr.fn_type(&[], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_net_http_server_config_max_header_bytes");
        i8ptr_i8ptr_fn!("mux_net_http_server_config_max_body_bytes");
        i8ptr_i8ptr_fn!("mux_net_http_server_config_max_headers");
        i8ptr_i8ptr_fn!("mux_net_http_server_config_read_timeout_ms");
        i8ptr_i8ptr_fn!("mux_net_http_server_config_access_log");
        i8ptr_i8ptr_fn!("mux_net_http_server_config_cors_origins");
        i8ptr_i8ptr_fn!("mux_net_http_server_config_cors_allow_credentials");
        i8ptr_i8ptr_fn!("mux_net_http_server_config_static_root");
        i8ptr_i8ptr_fn!("mux_net_http_server_config_worker_count");
        i8ptr_i8ptr_fn!("mux_net_http_server_config_heartbeat_interval_ms");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_server_config_set_max_header_bytes");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_server_config_set_max_body_bytes");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_server_config_set_max_headers");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_server_config_set_read_timeout_ms");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_server_config_set_access_log");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_server_config_set_cors_origins");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_server_config_set_cors_allow_credentials");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_server_config_set_static_root");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_server_config_set_worker_count");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_server_config_set_heartbeat_interval_ms");
        i8ptr_i8ptr_i8ptr_i8ptr_fn!("mux_net_http_server_serve_once");
        i8ptr_i8ptr_i8ptr_i8ptr_i8ptr_fn!("mux_net_http_server_serve_until_cancelled");
        module.add_function("mux_net_http_router_new", i8_ptr.fn_type(&[], false), None);
        i8ptr_i8ptr_i8ptr_i8ptr_i8ptr_fn!("mux_net_http_router_route");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_router_use");
        i8ptr_i8ptr_i8ptr_i8ptr_fn!("mux_net_http_router_basic_auth");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_router_bearer_auth");
        i8ptr_i8ptr_i8ptr_i8ptr_i8ptr_fn!("mux_net_http_router_oauth_oidc");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_router_handle");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_next_handle");
        module.add_function("mux_net_oauth_client_new", i8_ptr.fn_type(&[], false), None);
        i8ptr_i8ptr_i8ptr_i8ptr_i8ptr_fn!("mux_net_oauth_client_from_config");
        module.add_function(
            "mux_net_oauth_client_configure",
            i8_ptr.fn_type(
                &[
                    i8_ptr.into(),
                    i8_ptr.into(),
                    i8_ptr.into(),
                    i8_ptr.into(),
                    i8_ptr.into(),
                ],
                false,
            ),
            None,
        );
        i8ptr_i8ptr_fn!("mux_net_oauth_client_discover");
        i8ptr_i8ptr_i8ptr_i8ptr_i8ptr_fn!("mux_net_oauth_client_authorization_url");
        i8ptr_i8ptr_i8ptr_i8ptr_fn!("mux_net_oauth_client_exchange_code");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_oauth_client_refresh");
        i8ptr_i8ptr_i8ptr_i8ptr_fn!("mux_net_oauth_client_revoke");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_oauth_client_introspect");
        i8ptr_i8ptr_fn!("mux_net_oauth_session_from_token_response");
        i8ptr_i8ptr_fn!("mux_net_oauth_session_access_token");
        i8ptr_i8ptr_fn!("mux_net_oauth_session_refresh_token");
        i8ptr_i8ptr_fn!("mux_net_oauth_session_id_token");
        i8ptr_i8ptr_fn!("mux_net_oauth_session_token_type");
        i8ptr_i8ptr_fn!("mux_net_oauth_session_is_expired");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_oauth_session_refresh");
        i8ptr_i8ptr_i8ptr_i8ptr_fn!("mux_net_oauth_session_revoke");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_oauth_session_introspect");
        void_i8ptr_fn!("mux_net_oauth_session_close");
        module.add_function("mux_net_sse_event_new", i8_ptr.fn_type(&[], false), None);
        module.add_function(
            "mux_net_sse_event_from_config",
            i8_ptr.fn_type(
                &[i8_ptr.into(), i8_ptr.into(), i64_type.into(), i8_ptr.into()],
                false,
            ),
            None,
        );
        i8ptr_i8ptr_fn!("mux_net_sse_event_encode");
        i8ptr_i8ptr_fn!("mux_net_sse_event_event");
        i8ptr_i8ptr_fn!("mux_net_sse_event_id");
        i8ptr_i8ptr_fn!("mux_net_sse_event_retry_ms");
        i8ptr_i8ptr_fn!("mux_net_sse_event_data");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_sse_event_set_event");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_sse_event_set_id");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_sse_event_set_retry_ms");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_sse_event_set_data");
        i8ptr_i8ptr_fn!("mux_net_sse_stream_from_tcp");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_sse_stream_send");
        i8ptr_i8ptr_fn!("mux_net_sse_stream_flush");
        void_i8ptr_fn!("mux_net_sse_stream_close");
        module.add_function(
            "mux_net_websocket_frame_new",
            i8_ptr.fn_type(&[], false),
            None,
        );
        module.add_function(
            "mux_net_websocket_frame_from_config",
            i8_ptr.fn_type(
                &[
                    context.i32_type().into(),
                    i64_type.into(),
                    i8_ptr.into(),
                    context.i32_type().into(),
                ],
                false,
            ),
            None,
        );
        i8ptr_i8ptr_fn!("mux_net_websocket_frame_encode");
        i8ptr_i8ptr_fn!("mux_net_websocket_frame_decode");
        i8ptr_i8ptr_fn!("mux_net_websocket_frame_reassemble");
        i8ptr_i8ptr_fn!("mux_net_websocket_frame_fin");
        i8ptr_i8ptr_fn!("mux_net_websocket_frame_opcode");
        i8ptr_i8ptr_fn!("mux_net_websocket_frame_payload");
        i8ptr_i8ptr_fn!("mux_net_websocket_frame_masked");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_websocket_frame_set_fin");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_websocket_frame_set_opcode");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_websocket_frame_set_payload");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_websocket_frame_set_masked");
        module.add_function(
            "mux_net_websocket_handshake_new",
            i8_ptr.fn_type(&[], false),
            None,
        );
        i8ptr_i8ptr_i8ptr_fn!("mux_net_websocket_handshake_from_config");
        i8ptr_void_fn!("mux_net_websocket_handshake_request_key");
        i8ptr_i8ptr_fn!("mux_net_websocket_handshake_accept_key");
        i8ptr_i8ptr_fn!("mux_net_websocket_handshake_response_headers");
        i8ptr_i8ptr_fn!("mux_net_websocket_handshake_key");
        i8ptr_i8ptr_fn!("mux_net_websocket_handshake_protocol");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_websocket_handshake_set_key");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_websocket_handshake_set_protocol");
        i8ptr_i8ptr_fn!("mux_net_websocket_session_from_tcp");
        i8ptr_i8ptr_fn!("mux_net_websocket_session_receive");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_websocket_session_send");
        void_i8ptr_fn!("mux_net_websocket_session_close");
        module.add_function(
            "mux_net_http_server_serve",
            i8_ptr.fn_type(
                &[i8_ptr.into(), i8_ptr.into(), i8_ptr.into(), i64_type.into()],
                false,
            ),
            None,
        );
        module.add_function(
            "mux_net_http_response_new",
            i8_ptr.fn_type(&[], false),
            None,
        );
        module.add_function(
            "mux_net_http_response_from_config",
            i8_ptr.fn_type(&[i64_type.into(), i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_response_write");
        i8ptr_i8ptr_fn!("mux_net_http_response_status_value");
        i8ptr_i8ptr_fn!("mux_net_http_response_headers_value");
        i8ptr_i8ptr_fn!("mux_net_http_response_status_field");
        i8ptr_i8ptr_fn!("mux_net_http_response_headers_field");
        i8ptr_i8ptr_fn!("mux_net_http_response_body_field");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_response_set_status_field");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_response_set_headers_field");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_response_set_body_field");
        i8ptr_i8ptr_fn!("mux_net_http_response_error_for_status");
        module.add_function(
            "mux_net_http_response_read_bytes",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_net_http_response_reader",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        module.add_function("mux_log_logger_new", i8_ptr.fn_type(&[], false), None);
        module.add_function("mux_log_default", i8_ptr.fn_type(&[], false), None);
        i8ptr_i8ptr_fn!("mux_log_set_default");
        i8ptr_i8ptr_i8ptr_fn!("mux_log_logger_set_writer");
        i8ptr_i8ptr_i8ptr_fn!("mux_log_logger_set_level");
        i8ptr_i8ptr_i8ptr_fn!("mux_log_logger_set_name");
        i8ptr_i8ptr_i8ptr_i8ptr_fn!("mux_log_logger_field");
        i8ptr_i8ptr_i8ptr_fn!("mux_log_logger_trace");
        i8ptr_i8ptr_i8ptr_fn!("mux_log_logger_debug");
        i8ptr_i8ptr_i8ptr_fn!("mux_log_logger_info");
        i8ptr_i8ptr_i8ptr_fn!("mux_log_logger_warn");
        i8ptr_i8ptr_i8ptr_fn!("mux_log_logger_error");
        i8ptr_i8ptr_fn!("mux_log_trace");
        i8ptr_i8ptr_fn!("mux_log_debug");
        i8ptr_i8ptr_fn!("mux_log_info");
        i8ptr_i8ptr_fn!("mux_log_warn");
        i8ptr_i8ptr_fn!("mux_log_error");
        module.add_function(
            "mux_net_http_response_read_text",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_net_http_response_read_json",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_i8ptr_fn!("mux_net_http_response_save");
        i8ptr_i8ptr_fn!("mux_env_error_from_message");
        i8ptr_i8ptr_fn!("mux_env_error_kind");
        i8ptr_i8ptr_fn!("mux_env_error_detail");
        i8ptr_i8ptr_fn!("mux_env_error_key");
        i8ptr_i8ptr_fn!("mux_env_error_message");
        i8ptr_i8ptr_fn!("mux_env_error_to_string");
        i8ptr_i8ptr_fn!("mux_fs_error_from_message");
        i8ptr_i8ptr_fn!("mux_fs_error_kind");
        i8ptr_i8ptr_fn!("mux_fs_error_detail");
        i8ptr_i8ptr_fn!("mux_fs_error_path");
        i8ptr_i8ptr_fn!("mux_fs_error_message");
        i8ptr_i8ptr_fn!("mux_fs_error_to_string");
        i8ptr_i8ptr_fn!("mux_net_error_from_message");
        i8ptr_i8ptr_fn!("mux_net_error_kind");
        i8ptr_i8ptr_fn!("mux_net_error_detail");
        i8ptr_i8ptr_fn!("mux_net_error_address");
        i8ptr_i8ptr_fn!("mux_net_error_message");
        i8ptr_i8ptr_fn!("mux_net_error_to_string");
        module.add_function("mux_regex_new", i8_ptr.fn_type(&[], false), None);
        i8ptr_i8ptr_fn!("mux_regex_from_pattern");
        i8ptr_i8ptr_i8ptr_fn!("mux_regex_from_pattern_with_flags");
        i8ptr_i8ptr_fn!("mux_regex_escape");
        i8ptr_i8ptr_i8ptr_fn!("mux_regex_is_match");
        i8ptr_i8ptr_i8ptr_fn!("mux_regex_full_match");
        i8ptr_i8ptr_i8ptr_fn!("mux_regex_find");
        i8ptr_i8ptr_i8ptr_fn!("mux_regex_find_all");
        i8ptr_i8ptr_i8ptr_i8ptr_fn!("mux_regex_replace");
        i8ptr_i8ptr_i8ptr_i8ptr_fn!("mux_regex_replace_first");
        i8ptr_i8ptr_i8ptr_i8ptr_fn!("mux_regex_replace_with");
        i8ptr_i8ptr_i8ptr_fn!("mux_regex_split");
        i8ptr_i8ptr_fn!("mux_regex_group_count");
        i8ptr_i8ptr_fn!("mux_regex_named_groups");
        i8ptr_i8ptr_fn!("mux_regex_match_start");
        i8ptr_i8ptr_fn!("mux_regex_match_end");
        i8ptr_i8ptr_fn!("mux_regex_match_text");
        module.add_function(
            "mux_regex_match_capture",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_i8ptr_fn!("mux_regex_match_capture_named");
        i8ptr_i8ptr_fn!("mux_regex_match_captures");
        i8ptr_i8ptr_fn!("mux_uuid_parse");
        i8ptr_i8ptr_fn!("mux_uuid_parse_uuid");
        i8ptr_i8ptr_fn!("mux_uuid_from_bytes");
        i8ptr_void_fn!("mux_uuid_nil");
        i8ptr_void_fn!("mux_uuid_max");
        i8ptr_void_fn!("mux_uuid_v1");
        i8ptr_i8ptr_i8ptr_fn!("mux_uuid_v3");
        i8ptr_void_fn!("mux_uuid_v4");
        i8ptr_i8ptr_i8ptr_fn!("mux_uuid_v5");
        i8ptr_void_fn!("mux_uuid_v6");
        i8ptr_void_fn!("mux_uuid_v7");
        i8ptr_i8ptr_fn!("mux_uuid_v8");
        i8ptr_i64_i64_fn!("mux_uuid_from_parts");
        i8ptr_i8ptr_fn!("mux_uuid_to_string");
        i8ptr_i8ptr_fn!("mux_uuid_to_compact");
        i8ptr_i8ptr_fn!("mux_uuid_to_braced");
        i8ptr_i8ptr_fn!("mux_uuid_to_urn");
        i8ptr_i8ptr_fn!("mux_uuid_to_bytes");
        i8ptr_i8ptr_fn!("mux_uuid_to_parts");
        bool_i8ptr_fn!("mux_uuid_is_nil");
        bool_i8ptr_fn!("mux_uuid_is_max");
        i8ptr_i8ptr_fn!("mux_uuid_version");
        i8ptr_i8ptr_fn!("mux_uuid_variant");
        i8ptr_i8ptr_fn!("mux_uuid_error_from_message");
        i8ptr_i8ptr_fn!("mux_uuid_error_kind");
        i8ptr_i8ptr_fn!("mux_uuid_error_detail");
        i8ptr_i8ptr_fn!("mux_uuid_error_message");
        i8ptr_i8ptr_fn!("mux_uuid_error_to_string");
        i8ptr_i8ptr_fn!("mux_url_parse");
        i8ptr_i8ptr_fn!("mux_url_from_file");
        i8ptr_i8ptr_fn!("mux_url_to_string");
        i8ptr_i8ptr_fn!("mux_url_scheme");
        i8ptr_i8ptr_fn!("mux_url_username");
        i8ptr_i8ptr_fn!("mux_url_password");
        i8ptr_i8ptr_fn!("mux_url_host");
        i8ptr_i8ptr_fn!("mux_url_host_ascii");
        i8ptr_i8ptr_fn!("mux_url_host_unicode");
        i8ptr_i8ptr_fn!("mux_url_port");
        i8ptr_i8ptr_fn!("mux_url_path");
        i8ptr_i8ptr_fn!("mux_url_query");
        i8ptr_i8ptr_fn!("mux_url_fragment");
        i8ptr_i8ptr_fn!("mux_url_origin");
        i8ptr_i8ptr_fn!("mux_url_redacted");
        i8ptr_i8ptr_i8ptr_fn!("mux_url_join");
        i8ptr_i8ptr_i8ptr_fn!("mux_url_with_path");
        i8ptr_i8ptr_i8ptr_fn!("mux_url_with_query");
        i8ptr_i8ptr_i8ptr_fn!("mux_url_with_fragment");
        i8ptr_i8ptr_i8ptr_fn!("mux_url_with_host");
        module.add_function(
            "mux_url_with_port",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_i8ptr_fn!("mux_url_with_scheme");
        i8ptr_i8ptr_i8ptr_fn!("mux_url_with_username");
        i8ptr_i8ptr_i8ptr_fn!("mux_url_with_password");
        i8ptr_i8ptr_fn!("mux_url_query_pairs");
        i8ptr_i8ptr_i8ptr_fn!("mux_url_with_query_pairs");
        i8ptr_i8ptr_fn!("mux_url_to_file_path");
        bool_i8ptr_fn!("mux_url_is_http");
        bool_i8ptr_fn!("mux_url_is_https");
        i8ptr_i8ptr_fn!("mux_url_error_from_message");
        i8ptr_i8ptr_fn!("mux_url_error_kind");
        i8ptr_i8ptr_fn!("mux_url_error_detail");
        i8ptr_i8ptr_fn!("mux_url_error_url");
        i8ptr_i8ptr_fn!("mux_url_error_message");
        i8ptr_i8ptr_fn!("mux_url_error_to_string");
        i8ptr_i8ptr_fn!("mux_log_error_from_message");
        i8ptr_i8ptr_fn!("mux_log_error_kind");
        i8ptr_i8ptr_fn!("mux_log_error_detail");
        i8ptr_i8ptr_fn!("mux_log_error_message");
        i8ptr_i8ptr_fn!("mux_log_error_to_string");
        i8ptr_i8ptr_fn!("mux_sync_error_from_message");
        i8ptr_i8ptr_fn!("mux_sync_error_kind");
        i8ptr_i8ptr_fn!("mux_sync_error_detail");
        i8ptr_i8ptr_fn!("mux_sync_error_message");
        i8ptr_i8ptr_fn!("mux_sync_error_to_string");
        i8ptr_i8ptr_fn!("mux_random_error_from_message");
        i8ptr_i8ptr_fn!("mux_random_error_kind");
        i8ptr_i8ptr_fn!("mux_random_error_detail");
        i8ptr_i8ptr_fn!("mux_random_error_message");
        i8ptr_i8ptr_fn!("mux_random_error_to_string");
        i8ptr_i8ptr_fn!("mux_crypto_error_from_message");
        i8ptr_i8ptr_fn!("mux_crypto_error_kind");
        i8ptr_i8ptr_fn!("mux_crypto_error_detail");
        i8ptr_i8ptr_fn!("mux_crypto_error_message");
        i8ptr_i8ptr_fn!("mux_crypto_error_to_string");
        i8ptr_i8ptr_fn!("mux_regex_error_from_message");
        i8ptr_i8ptr_fn!("mux_regex_error_kind");
        i8ptr_i8ptr_fn!("mux_regex_error_detail");
        i8ptr_i8ptr_fn!("mux_regex_error_message");
        i8ptr_i8ptr_fn!("mux_regex_error_to_string");
        i8ptr_i8ptr_fn!("mux_math_error_from_message");
        i8ptr_i8ptr_fn!("mux_math_error_kind");
        i8ptr_i8ptr_fn!("mux_math_error_detail");
        i8ptr_i8ptr_fn!("mux_math_error_message");
        i8ptr_i8ptr_fn!("mux_math_error_to_string");
        i8ptr_i8ptr_fn!("mux_crypto_sha256");
        i8ptr_i8ptr_fn!("mux_crypto_sha512");
        i8ptr_i8ptr_fn!("mux_crypto_sha3_256");
        i8ptr_i8ptr_fn!("mux_crypto_sha3_512");
        i8ptr_i8ptr_fn!("mux_crypto_blake3");
        i8ptr_i8ptr_i8ptr_fn!("mux_crypto_hmac_sha256");
        i8ptr_i8ptr_i8ptr_fn!("mux_crypto_hmac_sha512");
        module.add_function(
            "mux_crypto_random_bytes",
            i8_ptr.fn_type(&[i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_crypto_random_token",
            i8_ptr.fn_type(&[i64_type.into()], false),
            None,
        );
        i8ptr_void_fn!("mux_crypto_generate_key");
        i8ptr_i8ptr_i8ptr_i8ptr_fn!("mux_crypto_seal_aes256_gcm");
        i8ptr_i8ptr_i8ptr_i8ptr_fn!("mux_crypto_seal_chacha20_poly1305");
        i8ptr_i8ptr_i8ptr_i8ptr_fn!("mux_crypto_open");
        i8ptr_i8ptr_i8ptr_i8ptr_i8ptr_fn!("mux_crypto_seal_file");
        i8ptr_i8ptr_i8ptr_i8ptr_i8ptr_fn!("mux_crypto_open_file");
        i8ptr_i8ptr_fn!("mux_net_tcp_listener_bind");
        i8ptr_i8ptr_fn!("mux_net_ip_parse");
        i8ptr_i8ptr_fn!("mux_net_socket_addr_parse");
        module.add_function(
            "mux_net_socket_addr_resolve",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_net_cidr_parse");
        i8ptr_i8ptr_fn!("mux_net_ip_to_string");
        bool_i8ptr_fn!("mux_net_ip_is_v4");
        bool_i8ptr_fn!("mux_net_ip_is_v6");
        i8ptr_i8ptr_fn!("mux_net_ip_octets");
        i8ptr_i8ptr_fn!("mux_net_socket_to_string");
        module.add_function(
            "mux_net_socket_port",
            i64_type.fn_type(&[i8_ptr.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_net_socket_ip");
        module.add_function(
            "mux_net_socket_with_port",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_net_cidr_to_string");
        module.add_function(
            "mux_net_cidr_prefix",
            i64_type.fn_type(&[i8_ptr.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_net_cidr_network");
        bool_i8ptr_i8ptr_fn!("mux_net_cidr_contains");
        i8ptr_void_fn!("mux_poller_new");
        module.add_function(
            "mux_poller_register_tcp",
            i8_ptr.fn_type(
                &[
                    i8_ptr.into(),
                    i8_ptr.into(),
                    i32_type.into(),
                    i32_type.into(),
                ],
                false,
            ),
            None,
        );
        i8ptr_i8ptr_fn!("mux_poller_register_listener");
        module.add_function(
            "mux_poller_register_udp",
            i8_ptr.fn_type(
                &[
                    i8_ptr.into(),
                    i8_ptr.into(),
                    i32_type.into(),
                    i32_type.into(),
                ],
                false,
            ),
            None,
        );
        module.add_function(
            "mux_poller_deregister",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_poller_poll",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        for name in [
            "mux_poll_event_token",
            "mux_poll_event_readable",
            "mux_poll_event_writable",
            "mux_poll_event_error",
            "mux_poll_event_closed",
        ] {
            add_i8_fn(module, i8_ptr, name, &[i8_ptr.into()]);
        }
        i8ptr_i8ptr_fn!("mux_net_tcp_listener_accept");
        module.add_function(
            "mux_net_tcp_listener_set_nonblocking",
            i8_ptr.fn_type(&[i8_ptr.into(), i32_type.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_net_tcp_listener_local_addr");
        void_i8ptr_fn!("mux_net_tcp_listener_close");
        i8ptr_i8ptr_fn!("mux_net_local_listener_bind");
        i8ptr_i8ptr_fn!("mux_net_local_connect");
        i8ptr_i8ptr_fn!("mux_net_local_listener_accept");
        module.add_function(
            "mux_net_local_read",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_i8ptr_fn!("mux_net_local_write");
        i8ptr_i8ptr_i64_fn!("mux_net_local_set_read_timeout");
        i8ptr_i8ptr_i64_fn!("mux_net_local_set_write_timeout");
        module.add_function(
            "mux_net_local_set_nonblocking",
            i8_ptr.fn_type(&[i8_ptr.into(), i32_type.into()], false),
            None,
        );
        module.add_function(
            "mux_net_local_listener_set_nonblocking",
            i8_ptr.fn_type(&[i8_ptr.into(), i32_type.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_net_local_shutdown_read");
        i8ptr_i8ptr_fn!("mux_net_local_shutdown_write");
        void_i8ptr_fn!("mux_net_local_close");
        void_i8ptr_fn!("mux_net_local_listener_close");
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
            "mux_net_tcp_set_read_timeout",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_net_tcp_set_write_timeout",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_net_tcp_set_nodelay",
            i8_ptr.fn_type(&[i8_ptr.into(), i32_type.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_net_tcp_nodelay");
        module.add_function(
            "mux_net_tcp_set_keepalive",
            i8_ptr.fn_type(&[i8_ptr.into(), i32_type.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_net_tcp_keepalive");
        module.add_function(
            "mux_net_tcp_set_recv_buffer_size",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_net_tcp_recv_buffer_size");
        module.add_function(
            "mux_net_tcp_set_send_buffer_size",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_net_tcp_send_buffer_size");
        module.add_function(
            "mux_net_tcp_set_ttl",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_net_tcp_ttl");
        i8ptr_i8ptr_fn!("mux_net_tcp_shutdown_read");
        i8ptr_i8ptr_fn!("mux_net_tcp_shutdown_write");
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
        i8ptr_i8ptr_fn!("mux_net_udp_datagram_bytes");
        i8ptr_i8ptr_fn!("mux_net_udp_datagram_address");
        i8ptr_i8ptr_fn!("mux_net_udp_datagram_truncated");
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
            "mux_net_udp_set_read_timeout",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_net_udp_set_write_timeout",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_net_udp_set_ttl",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_net_udp_ttl");
        module.add_function(
            "mux_net_udp_set_recv_buffer_size",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_net_udp_recv_buffer_size");
        module.add_function(
            "mux_net_udp_set_send_buffer_size",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_net_udp_send_buffer_size");
        module.add_function(
            "mux_net_udp_set_broadcast",
            i8_ptr.fn_type(&[i8_ptr.into(), i32_type.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_net_udp_broadcast");
        module.add_function(
            "mux_net_udp_set_multicast_loop_v4",
            i8_ptr.fn_type(&[i8_ptr.into(), i32_type.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_net_udp_multicast_loop_v4");
        module.add_function(
            "mux_net_udp_set_multicast_ttl_v4",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_net_udp_multicast_ttl_v4");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_udp_join_multicast_v4");
        i8ptr_i8ptr_i8ptr_fn!("mux_net_udp_leave_multicast_v4");
        module.add_function(
            "mux_net_udp_set_multicast_loop_v6",
            i8_ptr.fn_type(&[i8_ptr.into(), i32_type.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_net_udp_multicast_loop_v6");
        module.add_function(
            "mux_net_udp_set_multicast_hops_v6",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_net_udp_multicast_hops_v6");
        module.add_function(
            "mux_net_udp_join_multicast_v6",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_net_udp_leave_multicast_v6",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i64_type.into()], false),
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

        // Process metadata and explicit command/child/output handles.
        i8ptr_void_fn!("mux_process_args");
        module.add_function("mux_process_id", i64_type.fn_type(&[], false), None);
        i8ptr_void_fn!("mux_process_parent_id");
        i8ptr_void_fn!("mux_process_executable");
        module.add_function("mux_process_command_new", i8_ptr.fn_type(&[], false), None);
        i8ptr_i8ptr_i8ptr_fn!("mux_process_command_set_program");
        i8ptr_i8ptr_fn!("mux_process_command_shell");
        i8ptr_i8ptr_i8ptr_fn!("mux_process_command_arg");
        module.add_function(
            "mux_process_command_env",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_process_command_cwd");
        i8ptr_i8ptr_fn!("mux_process_command_stdin_piped");
        i8ptr_i8ptr_fn!("mux_process_command_stdout_piped");
        i8ptr_i8ptr_fn!("mux_process_command_stderr_piped");
        i8ptr_i8ptr_fn!("mux_process_command_stdin_null");
        i8ptr_i8ptr_fn!("mux_process_command_stdout_null");
        i8ptr_i8ptr_fn!("mux_process_command_stderr_null");
        i8ptr_i8ptr_fn!("mux_process_command_output");
        i8ptr_i8ptr_fn!("mux_process_command_status");
        i8ptr_i8ptr_fn!("mux_process_command_spawn");
        i8ptr_i8ptr_fn!("mux_process_child_wait");
        i8ptr_i8ptr_fn!("mux_process_child_try_wait");
        module.add_function(
            "mux_process_child_wait_timeout",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_i8ptr_fn!("mux_process_child_write_stdin");
        i8ptr_i8ptr_fn!("mux_process_child_close_stdin");
        module.add_function(
            "mux_process_child_read_stdout",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_process_child_read_stderr",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_process_child_kill");
        i8ptr_i8ptr_fn!("mux_process_child_kill_group");
        i8ptr_i8ptr_fn!("mux_process_output_status");
        i8ptr_i8ptr_fn!("mux_process_output_stdout");
        i8ptr_i8ptr_fn!("mux_process_output_stderr");
        i8ptr_void_fn!("mux_process_pool_new");
        module.add_function(
            "mux_process_pool_with_config",
            i8_ptr.fn_type(&[i64_type.into(), i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_i8ptr_fn!("mux_process_pool_submit");
        i8ptr_i8ptr_i8ptr_fn!("mux_process_pool_try_submit");
        module.add_function(
            "mux_process_pool_submit_timeout",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_process_pool_cancel_pending");
        i8ptr_i8ptr_fn!("mux_process_pool_close");
        module.add_function(
            "mux_process_error_from_message",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_process_error_kind");
        i8ptr_i8ptr_fn!("mux_process_error_detail");
        i8ptr_i8ptr_fn!("mux_process_error_message");
        i8ptr_i8ptr_fn!("mux_process_error_to_string");

        module.add_function(
            "mux_cli_error_from_message",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );
        i8ptr_i8ptr_fn!("mux_cli_error_kind");
        i8ptr_i8ptr_fn!("mux_cli_error_detail");
        i8ptr_i8ptr_fn!("mux_cli_error_message");
        i8ptr_i8ptr_fn!("mux_cli_error_to_string");

        module.add_function(
            "mux_sql_connect",
            i8_ptr.fn_type(&[i8_ptr.into()], false),
            None,
        );
        module.add_function("mux_sql_sqlite_memory", i8_ptr.fn_type(&[], false), None);
        module.add_function(
            "mux_sql_connection_begin_transaction_with_options",
            i8_ptr.fn_type(
                &[i8_ptr.into(), i8_ptr.into(), i8_ptr.into(), i8_ptr.into()],
                false,
            ),
            None,
        );
        module.add_function(
            "mux_sql_pool_from_config",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_sql_migration_from_config",
            i8_ptr.fn_type(
                &[i64_type.into(), i8_ptr.into(), i8_ptr.into(), i8_ptr.into()],
                false,
            ),
            None,
        );
        module.add_function(
            "mux_sql_migrator_from_migrations",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_sql_migrator_from_directory",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        for name in [
            "mux_sql_migrator_up",
            "mux_sql_migrator_down",
            "mux_sql_migrator_status",
            "mux_sql_migrator_validate",
            "mux_sql_migrator_dry_run",
        ] {
            add_i8_fn(module, i8_ptr, name, &[i8_ptr.into()]);
        }
        for name in ["mux_sql_migrator_up_to", "mux_sql_migrator_down_to"] {
            module.add_function(
                name,
                i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
                None,
            );
        }
        for name in ["mux_sql_pool_close", "mux_sql_pool_metrics"] {
            add_i8_fn(module, i8_ptr, name, &[i8_ptr.into()]);
        }
        for name in ["mux_sql_pool_execute", "mux_sql_pool_query"] {
            add_i8_fn(module, i8_ptr, name, &[i8_ptr.into(), i8_ptr.into()]);
        }
        add_i8_fn(
            module,
            i8_ptr,
            "mux_sql_pool_execute_batch",
            &[i8_ptr.into(), i8_ptr.into()],
        );
        add_i8_fn(
            module,
            i8_ptr,
            "mux_sql_pool_execute_many",
            &[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()],
        );
        for name in [
            "mux_sql_pool_execute_params",
            "mux_sql_pool_execute_named",
            "mux_sql_pool_query_params",
            "mux_sql_pool_query_named",
        ] {
            add_i8_fn(
                module,
                i8_ptr,
                name,
                &[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()],
            );
        }
        module.add_function(
            "mux_sql_pool_query_with_timeout",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i64_type.into()], false),
            None,
        );
        for name in [
            "mux_sql_pool_query_params_with_timeout",
            "mux_sql_pool_query_named_with_timeout",
        ] {
            add_i8_fn(
                module,
                i8_ptr,
                name,
                &[i8_ptr.into(), i8_ptr.into(), i8_ptr.into(), i64_type.into()],
            );
        }
        for name in [
            "mux_sql_pool_query_with_cancellation",
            "mux_sql_pool_query_params_with_cancellation",
            "mux_sql_pool_query_named_with_cancellation",
        ] {
            add_i8_fn(
                module,
                i8_ptr,
                name,
                &[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()],
            );
        }
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
        i8ptr_i8ptr_fn!("mux_sql_value_json");
        i8ptr_i8ptr_fn!("mux_sql_value_as_json");
        i8ptr_i8ptr_fn!("mux_sql_value_datetime");
        i8ptr_i8ptr_fn!("mux_sql_value_uuid");
        i8ptr_i8ptr_fn!("mux_sql_value_as_datetime");
        i8ptr_i8ptr_fn!("mux_sql_value_as_uuid");
        void_i8ptr_fn!("mux_sql_connection_close");
        i8ptr_i8ptr_i8ptr_fn!("mux_sql_connection_execute");
        i8ptr_i8ptr_fn!("mux_sql_connection_execute_batch");
        i8ptr_i8ptr_i8ptr_fn!("mux_sql_connection_execute_many");
        module.add_function(
            "mux_sql_connection_execute_params",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_sql_connection_execute_named",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        i8ptr_i8ptr_i8ptr_fn!("mux_sql_connection_query");
        module.add_function(
            "mux_sql_connection_query_params",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_sql_connection_query_named",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_sql_connection_query_with_timeout",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i64_type.into()], false),
            None,
        );
        module.add_function(
            "mux_sql_connection_query_params_with_timeout",
            i8_ptr.fn_type(
                &[i8_ptr.into(), i8_ptr.into(), i8_ptr.into(), i64_type.into()],
                false,
            ),
            None,
        );
        module.add_function(
            "mux_sql_connection_query_named_with_timeout",
            i8_ptr.fn_type(
                &[i8_ptr.into(), i8_ptr.into(), i8_ptr.into(), i64_type.into()],
                false,
            ),
            None,
        );
        for name in [
            "mux_sql_connection_query_with_cancellation",
            "mux_sql_connection_query_params_with_cancellation",
            "mux_sql_connection_query_named_with_cancellation",
        ] {
            add_i8_fn(
                module,
                i8_ptr,
                name,
                &[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()],
            );
        }
        i8ptr_i8ptr_fn!("mux_sql_connection_begin_transaction");
        i8ptr_i8ptr_fn!("mux_sql_transaction_begin_transaction");
        i8ptr_i8ptr_i8ptr_fn!("mux_sql_connection_prepare");
        i8ptr_i8ptr_i8ptr_fn!("mux_sql_transaction_savepoint");
        i8ptr_i8ptr_i8ptr_fn!("mux_sql_transaction_rollback_to");
        i8ptr_i8ptr_i8ptr_fn!("mux_sql_transaction_release_savepoint");
        i8ptr_i8ptr_fn!("mux_sql_transaction_commit");
        i8ptr_i8ptr_fn!("mux_sql_transaction_rollback");
        i8ptr_i8ptr_i8ptr_fn!("mux_sql_transaction_execute");
        i8ptr_i8ptr_fn!("mux_sql_transaction_execute_batch");
        i8ptr_i8ptr_i8ptr_fn!("mux_sql_transaction_execute_many");
        module.add_function(
            "mux_sql_transaction_execute_params",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_sql_transaction_execute_named",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        i8ptr_i8ptr_i8ptr_fn!("mux_sql_transaction_query");
        module.add_function(
            "mux_sql_transaction_query_params",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_sql_transaction_query_named",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()], false),
            None,
        );
        module.add_function(
            "mux_sql_transaction_query_with_timeout",
            i8_ptr.fn_type(&[i8_ptr.into(), i8_ptr.into(), i64_type.into()], false),
            None,
        );
        for name in [
            "mux_sql_transaction_query_params_with_timeout",
            "mux_sql_transaction_query_named_with_timeout",
        ] {
            add_i8_fn(
                module,
                i8_ptr,
                name,
                &[i8_ptr.into(), i8_ptr.into(), i8_ptr.into(), i64_type.into()],
            );
        }
        for name in [
            "mux_sql_transaction_query_with_cancellation",
            "mux_sql_transaction_query_params_with_cancellation",
            "mux_sql_transaction_query_named_with_cancellation",
        ] {
            add_i8_fn(
                module,
                i8_ptr,
                name,
                &[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()],
            );
        }
        i8ptr_i8ptr_i8ptr_fn!("mux_sql_prepared_execute");
        i8ptr_i8ptr_i8ptr_fn!("mux_sql_prepared_execute_named");
        i8ptr_i8ptr_i8ptr_fn!("mux_sql_prepared_query");
        i8ptr_i8ptr_i8ptr_fn!("mux_sql_prepared_query_named");
        for name in [
            "mux_sql_prepared_query_with_timeout",
            "mux_sql_prepared_query_named_with_timeout",
        ] {
            add_i8_fn(
                module,
                i8_ptr,
                name,
                &[i8_ptr.into(), i8_ptr.into(), i64_type.into()],
            );
        }
        for name in [
            "mux_sql_prepared_query_with_cancellation",
            "mux_sql_prepared_query_named_with_cancellation",
        ] {
            add_i8_fn(
                module,
                i8_ptr,
                name,
                &[i8_ptr.into(), i8_ptr.into(), i8_ptr.into()],
            );
        }
        void_i8ptr_fn!("mux_sql_prepared_close");
        i8ptr_i8ptr_fn!("mux_sql_resultset_rows");
        i8ptr_i8ptr_fn!("mux_sql_resultset_close");
        i8ptr_i8ptr_fn!("mux_sql_resultset_next");
        i8ptr_i8ptr_i64_fn!("mux_sql_resultset_next_batch");
        i8ptr_i8ptr_fn!("mux_sql_resultset_columns");
        i8ptr_i8ptr_fn!("mux_sql_row_columns");
        i8ptr_i8ptr_fn!("mux_sql_row_values");
        module.add_function(
            "mux_sql_row_at",
            i8_ptr.fn_type(&[i8_ptr.into(), i64_type.into()], false),
            None,
        );
        i8ptr_i8ptr_i8ptr_fn!("mux_sql_row_get");
        i8ptr_i8ptr_fn!("mux_sql_error_kind");
        i8ptr_i8ptr_fn!("mux_sql_error_detail");
        i8ptr_i8ptr_fn!("mux_sql_error_provider");
        i8ptr_i8ptr_fn!("mux_sql_error_code");
        i8ptr_i8ptr_fn!("mux_sql_error_constraint");
        i8ptr_i8ptr_fn!("mux_sql_error_operation");
        i8ptr_i8ptr_fn!("mux_sql_error_message");
        i8ptr_i8ptr_fn!("mux_sql_error_to_string");
        i8ptr_i8ptr_fn!("mux_sql_error_from_message");
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
            // user-defined enum values (structs): box into Value::Opaque. An enum
            // that carries reference-counted payloads and is being stored into a
            // collection is instead boxed as a managed BoxedEnum via
            // `box_enum_or_value`, whose caller knows the element's enum type; a
            // bare struct value here is not enough to identify the enum, since
            // literal LLVM struct types are shared across enums (issue #309).
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
            .ok_or(format!("{getter_func_name} not found"))?;
        let result = self
            .builder
            .build_call(func, &[ptr.into()], "extract")
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or(error_msg)?;
        result
            .try_into()
            .map_err(|_| format!("Conversion failed for {getter_func_name}"))
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
    /// Used for unwrapping `Optional<T>` and `Result<T, E>` in match statements.
    ///
    /// # Arguments
    /// * `data_ptr` - Pointer to the wrapped value (*mut Value)
    /// * `wrapped_type` - The type of the value being unwrapped
    /// * `variant_name` - Name of the variant ("Some", "Ok", "Err") for error messages
    ///
    /// # Returns
    /// A tuple of (`BasicValueEnum`, Type) representing the extracted value and its type
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
                    "Failed to extract int from {variant_name}: mux_value_get_int not found"
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
                        "Failed to extract float from {variant_name}: mux_value_get_float not found"
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
                    "Failed to extract bool from {variant_name}: mux_value_get_bool not found"
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
                    "Failed to extract char from {variant_name}: mux_value_get_int not found"
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
            Type::Primitive(PrimitiveType::Byte) => {
                let get_int_func = self.runtime_function("mux_value_get_int").ok_or(format!(
                    "Failed to extract byte from {variant_name}: mux_value_get_int not found"
                ))?;
                let val = self
                    .builder
                    .build_call(get_int_func, &[data_ptr.into()], "get_byte")
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("mux_value_get_int returned no value")?;
                self.emit_value_decref(data_ptr)?;
                Ok((val, Type::Primitive(PrimitiveType::Byte)))
            }
            Type::Primitive(PrimitiveType::Str) => {
                // String needs special handling: get C string then wrap in Mux string
                let get_string_func = self
                    .runtime_function("mux_value_get_string")
                    .ok_or(format!(
                    "Failed to extract string from {variant_name}: mux_value_get_string not found"
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
                "Unsupported type Void for extraction from {variant_name}"
            )),
            Type::Primitive(PrimitiveType::Auto) => Err(format!(
                "Unsupported type Auto for extraction from {variant_name}"
            )),
            // Payload-less enum values cross the ABI as an owned opaque Value.
            // Load the inline discriminant struct before releasing that owned
            // wrapper, just as field access does for native typed enums.
            Type::Named(name, _)
                if self.enum_variants.contains_key(name) && self.is_payloadless_enum(name) =>
            {
                let value = self.unbox_enum_subject_value(name, data_ptr)?;
                self.emit_value_decref(data_ptr)?;
                Ok((value, wrapped_type.clone()))
            }
            // Collections, custom types, and nested Optional/Result stay as *mut Value
            Type::List(_)
            | Type::Map(_, _)
            | Type::Set(_)
            | Type::Primitive(PrimitiveType::Bytes)
            | Type::Tuple(_, _)
            | Type::Named(_, _)
            | Type::Optional(_)
            | Type::Result(_, _)
            | Type::Instantiated(_, _)
            | Type::TraitObject(_) => {
                // These are already *mut Value pointers, no extraction needed
                Ok((data_ptr.into(), wrapped_type.clone()))
            }
            // Reference types - unwrap the reference
            Type::Reference(inner) => self.extract_value_from_ptr(data_ptr, inner, variant_name),
            // Other types that shouldn't appear in Optional/Result
            Type::Void | Type::Never | Type::EmptyList | Type::EmptyMap | Type::EmptySet => Err(
                format!("Unsupported type {wrapped_type:?} for extraction from {variant_name}"),
            ),
            Type::Function { .. } => Err(format!(
                "Unsupported type Function for extraction from {variant_name}"
            )),
            // Inside a specialized body the parameter has a concrete binding,
            // so resolve it and extract that instead. Failing here made
            // indexing a generic collection - `d[k]` for a `map<K, V>`
            // parameter - an internal error, while `d.get(k)` worked.
            Type::Variable(v) | Type::Generic(v) => match self.resolve_generic_param(v).cloned() {
                Some(concrete) => self.extract_value_from_ptr(data_ptr, &concrete, variant_name),
                None => Err(format!(
                    "Unresolved generic type {v} for extraction from {variant_name}"
                )),
            },
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
            .ok_or_else(|| format!("{func_name} not found"))?;
        self.builder
            .build_call(func, &[value_ptr.into()], result_name)
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| format!("{func_name} returned no value"))
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
        self.emit_runtime_fatal(
            RuntimeErrorCode::InternalRuntime,
            "cannot copy object of this type",
            None,
            "copy_error",
        )?;

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

#[cfg(test)]
mod tests {
    use inkwell::AddressSpace;
    use inkwell::context::Context;

    use super::CodeGenerator;
    use super::RuntimeErrorCode;

    #[test]
    fn runtime_codes_match_the_runtime_registry() {
        assert_eq!(RuntimeErrorCode::IndexOutOfBounds as i32, 600);
        assert_eq!(RuntimeErrorCode::KeyNotFound as i32, 601);
        assert_eq!(RuntimeErrorCode::DivisionByZero as i32, 602);
        assert_eq!(RuntimeErrorCode::AssertionFailed as i32, 603);
        assert_eq!(RuntimeErrorCode::WhereConstraintViolation as i32, 604);
        assert_eq!(RuntimeErrorCode::IntegerOverflow as i32, 605);
        assert_eq!(RuntimeErrorCode::InternalRuntime as i32, 699);
    }

    #[test]
    fn oauth_oidc_runtime_symbol_has_the_router_and_three_url_arguments() {
        let context = Context::create();
        let module = context.create_module("runtime_api_parity");
        CodeGenerator::declare_runtime_functions(&module, &context);

        let function = module
            .get_function("mux_net_http_router_oauth_oidc")
            .expect("OAuth/OIDC runtime symbol must be declared");
        let function_type = function.get_type();
        let pointer = context.ptr_type(AddressSpace::default());

        assert!(!function_type.is_var_arg());
        assert_eq!(function_type.get_param_types().len(), 4);
        for parameter in function_type.get_param_types() {
            assert_eq!(parameter, pointer.into());
        }
        assert_eq!(function_type.get_return_type(), Some(pointer.into()));
    }

    #[test]
    fn oauth_client_runtime_symbols_have_typed_pointer_signatures() {
        let context = Context::create();
        let module = context.create_module("oauth_client_runtime_api");
        CodeGenerator::declare_runtime_functions(&module, &context);
        let pointer = context.ptr_type(AddressSpace::default());
        for (name, parameter_count) in [
            ("mux_net_oauth_client_new", 0),
            ("mux_net_oauth_client_from_config", 4),
            ("mux_net_oauth_client_discover", 1),
            ("mux_net_oauth_client_authorization_url", 4),
            ("mux_net_oauth_client_exchange_code", 3),
            ("mux_net_oauth_client_refresh", 2),
            ("mux_net_oauth_client_revoke", 3),
            ("mux_net_oauth_client_introspect", 2),
            ("mux_net_oauth_session_from_token_response", 1),
            ("mux_net_oauth_session_access_token", 1),
            ("mux_net_oauth_session_refresh_token", 1),
            ("mux_net_oauth_session_id_token", 1),
            ("mux_net_oauth_session_token_type", 1),
            ("mux_net_oauth_session_is_expired", 1),
            ("mux_net_oauth_session_refresh", 2),
            ("mux_net_oauth_session_revoke", 3),
            ("mux_net_oauth_session_introspect", 2),
        ] {
            let function = module
                .get_function(name)
                .unwrap_or_else(|| panic!("{name} must be declared"));
            let function_type = function.get_type();
            assert!(!function_type.is_var_arg());
            assert_eq!(function_type.get_param_types().len(), parameter_count);
            for parameter in function_type.get_param_types() {
                assert_eq!(parameter, pointer.into());
            }
            assert_eq!(function_type.get_return_type(), Some(pointer.into()));
        }
        let close = module
            .get_function("mux_net_oauth_session_close")
            .expect("OAuthSession.close must be declared");
        assert_eq!(close.get_type().get_param_types().len(), 1);
        assert!(close.get_type().get_return_type().is_none());
    }
}
