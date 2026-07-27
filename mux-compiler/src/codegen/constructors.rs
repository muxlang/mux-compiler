//! Constructor generation for classes and enums.
//!
//! This module handles generating constructors and related initialization code.

use super::CodeGenerator;
use crate::ast::{EnumVariant, ExpressionNode, Field, LiteralNode, PrimitiveType, TypeKind};
use crate::semantics::{GenericContext, MethodSig, Type};
use inkwell::AddressSpace;
use inkwell::types::BasicType;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, IntValue, PointerValue};
use std::collections::HashMap;

impl<'a> CodeGenerator<'a> {
    /// Create a new empty collection and wrap it as a Value pointer.
    /// `new_fn` creates the raw collection, `value_fn` wraps it as `*mut Value`.
    fn create_empty_collection_value(
        &mut self,
        new_fn: &str,
        value_fn: &str,
    ) -> BasicValueEnum<'a> {
        let raw_ptr = self
            .generate_runtime_call(new_fn, &[])
            .expect("should always return a value");
        self.generate_runtime_call(value_fn, &[raw_ptr.into()])
            .expect("should always return a value")
    }

    /// Store an initialized field value at a given pointer location.
    /// Handles both explicit default values and type-based initialization.
    fn store_initialized_field(
        &mut self,
        field_ptr: PointerValue<'a>,
        field: &Field,
    ) -> Result<(), String> {
        // The initial value stored here is owned by the field (released by the
        // object's destructor), so any temporary registered while producing it
        // must be dropped from the pending set rather than freed at the
        // constructor's statement boundary - otherwise the field is left
        // dangling.
        let temp_mark = self.temp_mark();
        if let Some(default_expr) = &field.default_value {
            let literal_val = self.generate_expression(default_expr)?;
            let stored_val = if matches!(field.type_.kind, TypeKind::Primitive(_)) {
                self.box_value(literal_val).into()
            } else {
                literal_val
            };
            self.builder
                .build_store(field_ptr, stored_val)
                .map_err(|e| e.to_string())?;
        } else {
            let field_type = self.type_node_to_type(&field.type_);
            self.initialize_field_by_type(field_ptr, &field_type, field.is_generic_param)?;
        }
        self.discard_temps_to(temp_mark);
        Ok(())
    }

    pub(super) fn generate_enum_constructors(
        &mut self,
        name: &str,
        variants: &[EnumVariant],
    ) -> Result<(), String> {
        for variant in variants {
            self.generate_single_enum_constructor(name, variant)?;
        }
        Ok(())
    }

    /// Emit the `Enum!Variant` constructor: zero-initialize the enum struct, set
    /// the discriminant, store each payload field, enforce the variant's where
    /// clause, and return the struct by value.
    fn generate_single_enum_constructor(
        &mut self,
        name: &str,
        variant: &EnumVariant,
    ) -> Result<(), String> {
        let variant_name = &variant.name;
        let full_name = format!("{}!{}", name, variant_name);

        // params: variant.data (extract types, discard field names for codegen)
        let field_count = variant.data.as_ref().map(|d| d.len()).unwrap_or(0);
        let mut param_types = vec![];
        if let Some(ref d) = variant.data {
            for (_, t) in d {
                let llvm_type = self.llvm_type_from_mux_type(t)?;
                param_types.push(llvm_type.into());
            }
        }

        // return type: enum struct
        let enum_type_basic = *self.type_map.get(name).ok_or("Enum type not found")?;
        let struct_type = enum_type_basic.into_struct_type();
        let fn_type = enum_type_basic.fn_type(&param_types, false);
        let function = self.module.add_function(&full_name, fn_type, None);

        // generate the body
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        // build the struct by storing to temp and loading
        let tag_index = self.get_variant_index(name, variant_name)?;
        let tag_val = self.context.i32_type().const_int(tag_index as u64, false);
        let temp_ptr = self
            .builder
            .build_alloca(struct_type, "temp_struct")
            .map_err(|e| e.to_string())?;
        self.builder
            .build_store(temp_ptr, struct_type.const_zero())
            .map_err(|e| e.to_string())?;
        let tag_ptr = self
            .builder
            .build_struct_gep(struct_type, temp_ptr, 0, "tag_ptr")
            .map_err(|e| e.to_string())?;
        self.builder
            .build_store(tag_ptr, tag_val)
            .map_err(|e| e.to_string())?;
        for i in 0..field_count {
            self.store_constructor_field(name, variant, struct_type, temp_ptr, function, i)?;
        }
        // enforce the variant's where clause with its named payload fields
        // readable by name (bound like function parameters)
        if let Some(clause) = &variant.where_clause {
            let predicates = clause.predicates.clone();
            self.enforce_variant_where_clause(variant, function, &predicates)?;
        }
        let struct_val = self
            .builder
            .build_load(struct_type, temp_ptr, "struct")
            .map_err(|e| e.to_string())?;
        self.builder
            .build_return(Some(&struct_val))
            .map_err(|e| e.to_string())?;

        self.constructors
            .insert(format!("{}.{}", name, variant_name), function);
        Ok(())
    }

    /// Store constructor parameter `i` into the enum struct at `temp_ptr`. A
    /// nested user enum is stored inline and its payloads retained (mirroring
    /// `rc_inc_if_pointer` for a direct pointer payload); any other payload is
    /// retained if it is a pointer and stored directly.
    fn store_constructor_field(
        &mut self,
        enum_name: &str,
        variant: &EnumVariant,
        struct_type: inkwell::types::StructType<'a>,
        temp_ptr: PointerValue<'a>,
        function: inkwell::values::FunctionValue<'a>,
        i: usize,
    ) -> Result<(), String> {
        let arg = function
            .get_nth_param(i as u32)
            .expect("function parameter should exist at expected index");
        let data_ptr = self
            .builder
            .build_struct_gep(struct_type, temp_ptr, (i + 1) as u32, "data_ptr")
            .map_err(|e| e.to_string())?;
        let nested_enum = variant
            .data
            .as_ref()
            .and_then(|d| d.get(i))
            .and_then(|field| self.nested_user_enum_name(&field.1));
        let Some(inner) = nested_enum else {
            // The variant takes ownership of a reference-counted payload
            // (string/list/object), so retain it: the caller frees the value it
            // passed in as a statement temporary, and the enum must keep its own
            // reference alive. No-op for unboxed scalar payloads.
            self.rc_inc_if_pointer(arg)?;
            return self
                .builder
                .build_store(data_ptr, arg)
                .map(|_| ())
                .map_err(|e| e.to_string());
        };
        // A nested user enum is stored inline. The union slot is sized to the
        // widest payload at this position (issue #309), so it must be at least as
        // large as this enum's struct. A slot that is too small means the enum
        // could not be laid out inline (a recursive enum falls back to a pointer);
        // error cleanly rather than storing a struct into an undersized slot and
        // corrupting memory (issue #306).
        let slot_fits = match (
            struct_type.get_field_type_at_index((i + 1) as u32),
            self.type_map.get(&inner).copied(),
        ) {
            (Some(slot), Some(inner_ty)) => {
                self.abi_store_size(&slot) >= self.abi_store_size(&inner_ty)
            }
            _ => false,
        };
        if !slot_fits {
            return Err(self.nested_enum_layout_error(enum_name, &variant.name, &inner, i));
        }
        self.builder
            .build_store(data_ptr, arg)
            .map_err(|e| e.to_string())?;
        // Mirror rc_inc_if_pointer for a direct pointer payload: this variant now
        // owns an independent +1 on the nested enum's payloads, so the caller can
        // still release the temporary it passed in.
        self.emit_enum_retain(&inner, data_ptr)
    }

    /// Bind the variant's named payload fields as parameters and run its where
    /// predicates. Binding boxes scalar payloads (mux_int_value, ...); those boxes
    /// are owned temporaries released once the check has run, or every constructed
    /// variant with a where clause leaks them.
    fn enforce_variant_where_clause(
        &mut self,
        variant: &EnumVariant,
        function: inkwell::values::FunctionValue<'a>,
        predicates: &[ExpressionNode],
    ) -> Result<(), String> {
        let temp_mark = self.temp_mark();
        let snapshot = self.variables.clone();
        for (i, (field_name, type_node)) in variant.data.iter().flatten().enumerate() {
            let Some(field_name) = field_name else {
                continue;
            };
            let arg = function
                .get_nth_param(i as u32)
                .expect("function parameter should exist at expected index");
            let semantic_type = self.type_node_to_type(type_node);
            self.store_function_parameter_value(field_name, arg, semantic_type)?;
        }
        self.emit_where_checks(predicates, "where_variant")?;
        self.variables = snapshot;
        self.cleanup_temps_to(temp_mark)
    }

    /// Build the error for a nested enum payload that could not be laid out
    /// inline. The pointer slot has two causes, reported distinctly: a recursive
    /// enum (a self- or mutually-embedding enum has no finite inline layout), or
    /// a heterogeneous union position (another variant places a differently-typed
    /// payload at the same position, so the slot is a generic pointer). Recursion
    /// is checked first because a recursive enum is also heterogeneous, but
    /// recursion is the fundamental blocker. Neither is supported yet.
    fn nested_enum_layout_error(
        &self,
        enum_name: &str,
        variant_name: &str,
        inner: &str,
        position: usize,
    ) -> String {
        let recursive = inner == enum_name || self.enum_embeds(inner, enum_name);
        if recursive {
            format!(
                "Nested enum payload '{}' in {}!{}: enum '{}' embeds '{}' recursively; \
                 recursive or mutually-referential enums cannot be laid out inline",
                inner, enum_name, variant_name, enum_name, inner
            )
        } else {
            format!(
                "Nested enum payload '{}' in {}!{} shares payload position {} with a \
                 differently-typed payload; a nested enum in a heterogeneous union \
                 position is not yet supported",
                inner, enum_name, variant_name, position
            )
        }
    }

    fn register_class_type(
        &mut self,
        name: &str,
        type_name_global: PointerValue<'a>,
        type_size: IntValue<'a>,
    ) -> Result<IntValue<'a>, String> {
        let register_func = self
            .runtime_function("mux_register_object_type")
            .ok_or("mux_register_object_type not found")?;
        let type_id = self
            .builder
            .build_call(
                register_func,
                &[type_name_global.into(), type_size.into()],
                "type_id",
            )
            .map_err(|e| e.to_string())?;
        let type_id_val = type_id
            .try_as_basic_value()
            .basic()
            .ok_or("type_id call should return a basic value")?
            .into_int_value();

        if let (Some(copy_fn), Some(destructor_fn)) = (
            self.class_copy_fns.get(name).copied(),
            self.class_destructor_fns.get(name).copied(),
        ) {
            let register_copy = self
                .runtime_function("mux_register_object_copy")
                .ok_or("mux_register_object_copy not found")?;
            self.builder
                .build_call(
                    register_copy,
                    &[type_id_val.into(), copy_fn.into()],
                    "register_copy",
                )
                .map_err(|e| e.to_string())?;
            let register_destructor = self
                .runtime_function("mux_register_object_destructor")
                .ok_or("mux_register_object_destructor not found")?;
            self.builder
                .build_call(
                    register_destructor,
                    &[type_id_val.into(), destructor_fn.into()],
                    "register_destructor",
                )
                .map_err(|e| e.to_string())?;
        }

        Ok(type_id_val)
    }

    fn allocate_class_object(
        &mut self,
        type_id_val: IntValue<'a>,
    ) -> Result<(PointerValue<'a>, PointerValue<'a>), String> {
        let alloc_func = self
            .runtime_function("mux_alloc_object")
            .ok_or("mux_alloc_object not found")?;
        let obj_ptr = self
            .builder
            .build_call(alloc_func, &[type_id_val.into()], "obj_ptr")
            .map_err(|e| e.to_string())?;
        let obj_value_ptr = obj_ptr
            .try_as_basic_value()
            .basic()
            .ok_or("alloc_object call should return a pointer value")?
            .into_pointer_value();

        let get_ptr_func = self
            .runtime_function("mux_get_object_ptr")
            .ok_or("mux_get_object_ptr not found")?;
        let data_ptr = self
            .builder
            .build_call(get_ptr_func, &[obj_value_ptr.into()], "data_ptr")
            .map_err(|e| e.to_string())?;
        let struct_ptr = data_ptr
            .try_as_basic_value()
            .basic()
            .ok_or("mux_get_object_ptr should return a basic value")?
            .into_pointer_value();

        Ok((obj_value_ptr, struct_ptr))
    }

    pub(super) fn generate_class_constructors(
        &mut self,
        name: &str,
        fields: &[Field],
        interfaces: &HashMap<String, (Vec<Type>, HashMap<String, MethodSig>)>,
    ) -> Result<(), String> {
        let full_name = format!("{}.new", name);

        // Constructor takes no parameters - fields are initialized separately
        let param_types = vec![];

        // return type: *mut Value (boxed object)
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let fn_type = ptr_type.fn_type(&param_types, false);
        let function = self.module.add_function(&full_name, fn_type, None);

        // generate the body
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        // register the object type if not already registered
        let type_name = format!("type_name_{}", name);
        let type_name_global = self
            .builder
            .build_global_string_ptr(name, &type_name)
            .map_err(|e| e.to_string())?;
        if let Some(global) = self.module.get_global(&type_name) {
            global.set_linkage(inkwell::module::Linkage::External);
        }
        let type_size = self
            .type_map
            .get(name)
            .ok_or("Class type not found")?
            .size_of()
            .ok_or("Cannot get type size")?;
        let type_id_val =
            self.register_class_type(name, type_name_global.as_pointer_value(), type_size)?;

        let (obj_value_ptr, struct_ptr) = self.allocate_class_object(type_id_val)?;

        // cast to class struct pointer
        let class_type = self.type_map.get(name).ok_or("Class type not found")?;
        let class_type_clone = *class_type;
        let struct_ptr_typed = self
            .builder
            .build_pointer_cast(
                struct_ptr,
                self.context.ptr_type(AddressSpace::default()),
                "struct_ptr",
            )
            .map_err(|e| e.to_string())?;

        // set fields to default (zero) values
        for field in fields.iter() {
            let field_index = self
                .field_map
                .get(name)
                .expect("class should be in field_map after type generation")
                .get(&field.name)
                .expect("field should exist in class after semantic analysis");
            let field_ptr = self
                .builder
                .build_struct_gep(
                    class_type_clone,
                    struct_ptr_typed,
                    *field_index as u32,
                    &field.name,
                )
                .map_err(|e| e.to_string())?;

            self.store_initialized_field(field_ptr, field)?;
        }

        self.emit_construction_invariants(name, struct_ptr_typed)?;

        // set vtable fields. Generic classes never get a vtable generated
        // (see generate_class_vtables) since the vtable would have to
        // reference an unspecialized method that has no body; skip those
        // fields rather than erroring, since the vtable field is never read
        // for dispatch (interfaces use static dispatch).
        for interface_name in interfaces.keys() {
            let vtable_key = format!("{}_{}", name, interface_name);
            let Some(vtable_ptr) = self.vtable_map.get(&vtable_key) else {
                continue;
            };
            let vtable_field_name = format!("vtable_{}", interface_name);
            let field_index = self
                .field_map
                .get(name)
                .ok_or_else(|| format!("Field map not found for class {}", name))?
                .get(&vtable_field_name)
                .ok_or_else(|| {
                    format!(
                        "Vtable field {} not found in class {}",
                        vtable_field_name, name
                    )
                })?;
            let field_ptr = self
                .builder
                .build_struct_gep(
                    class_type_clone,
                    struct_ptr_typed,
                    *field_index as u32,
                    &vtable_field_name,
                )
                .map_err(|e| e.to_string())?;
            self.builder
                .build_store(field_ptr, *vtable_ptr)
                .map_err(|e| e.to_string())?;
        }

        // return the Value pointer
        self.builder
            .build_return(Some(&obj_value_ptr))
            .map_err(|e| e.to_string())?;

        // store in constructors
        self.constructors.insert(format!("{}.new", name), function);
        Ok(())
    }

    pub(super) fn initialize_field_by_type(
        &mut self,
        field_ptr: PointerValue<'a>,
        field_type: &Type,
        is_generic_param: bool,
    ) -> Result<(), String> {
        // generic parameter fields are boxed, initialize as null pointer
        if is_generic_param {
            let null_ptr = self.context.ptr_type(AddressSpace::default()).const_null();
            self.builder
                .build_store(field_ptr, null_ptr)
                .map_err(|e| e.to_string())?;
            return Ok(());
        }

        let resolved_type = self.resolve_type(field_type)?;

        match resolved_type {
            // Primitive fields are stored boxed (a `*mut Value` pointer), matching
            // how explicitly-defaulted fields and later assignments store them.
            // Storing the raw scalar instead would leave the upper bytes of the
            // pointer-sized slot uninitialized, so a later boxed-pointer read
            // (e.g. `mux_value_get_bool`) would dereference garbage whenever the
            // object landed on non-zero reclaimed heap memory.
            Type::Primitive(PrimitiveType::Bool) => {
                let false_val = self.context.bool_type().const_int(0, false);
                let boxed = self.box_value(false_val.into());
                self.builder
                    .build_store(field_ptr, boxed)
                    .map_err(|e| e.to_string())?;
            }
            Type::Primitive(PrimitiveType::Int) => {
                let zero_val = self.context.i64_type().const_int(0, false);
                let boxed = self.box_value(zero_val.into());
                self.builder
                    .build_store(field_ptr, boxed)
                    .map_err(|e| e.to_string())?;
            }
            Type::Primitive(PrimitiveType::Float) => {
                let zero_val = self.context.f64_type().const_float(0.0);
                let boxed = self.box_value(zero_val.into());
                self.builder
                    .build_store(field_ptr, boxed)
                    .map_err(|e| e.to_string())?;
            }
            Type::Primitive(PrimitiveType::Str) => {
                // A real empty string, not null: string methods (and
                // construction-time where checks that call them) must work
                // on a field before its first assignment.
                let empty: ExpressionNode = LiteralNode::String(String::new()).into();
                let val = self.generate_expression(&empty)?;
                self.builder
                    .build_store(field_ptr, val)
                    .map_err(|e| e.to_string())?;
            }
            Type::List(_) => {
                let val = self.create_empty_collection_value("mux_new_list", "mux_list_value");
                self.builder
                    .build_store(field_ptr, val)
                    .map_err(|e| e.to_string())?;
            }
            Type::Map(_, _) => {
                let val = self.create_empty_collection_value("mux_new_map", "mux_map_value");
                self.builder
                    .build_store(field_ptr, val)
                    .map_err(|e| e.to_string())?;
            }
            Type::Set(_) => {
                let val = self.create_empty_collection_value("mux_new_set", "mux_set_value");
                self.builder
                    .build_store(field_ptr, val)
                    .map_err(|e| e.to_string())?;
            }
            Type::Optional(_) => {
                let optional_ptr = self
                    .generate_runtime_call("mux_optional_none", &[])
                    .expect("mux_optional_none should always return a value");
                self.builder
                    .build_store(field_ptr, optional_ptr)
                    .map_err(|e| e.to_string())?;
            }
            Type::Result(ok_type, _) => {
                let ok_value = self.create_default_value_ptr(&ok_type)?;
                let result_ptr = self
                    .generate_runtime_call("mux_result_ok_value", &[ok_value.into()])
                    .expect("mux_result_ok_value should always return a value");
                self.builder
                    .build_store(field_ptr, result_ptr)
                    .map_err(|e| e.to_string())?;
            }
            Type::Tuple(left_type, right_type) => {
                let tuple_value = self.generate_tuple_constructor(&left_type, &right_type)?;
                self.builder
                    .build_store(field_ptr, tuple_value)
                    .map_err(|e| e.to_string())?;
            }
            Type::Named(class_name, type_args) => {
                // handle built-in types
                if class_name == "string" && type_args.is_empty() {
                    // initialize string field with null pointer
                    let null_ptr = self.context.ptr_type(AddressSpace::default()).const_null();
                    self.builder
                        .build_store(field_ptr, null_ptr)
                        .map_err(|e| e.to_string())?;
                } else if class_name == "bool" && type_args.is_empty() {
                    // initialize bool field with false
                    let false_val = self.context.bool_type().const_int(0, false);
                    self.builder
                        .build_store(field_ptr, false_val)
                        .map_err(|e| e.to_string())?;
                } else if self.is_enum_type(&Type::Named(class_name.clone(), type_args.clone())) {
                    // Enum fields are stored inline as a struct. Default them to a
                    // zeroed enum value (the first variant) - going through the
                    // class constructor path would wrongly allocate a heap object
                    // (mux_alloc_object) for the enum, which then leaks.
                    if let Some(enum_type) = self.type_map.get(&class_name) {
                        let zero = enum_type.into_struct_type().const_zero();
                        self.builder
                            .build_store(field_ptr, zero)
                            .map_err(|e| e.to_string())?;
                    }
                } else {
                    // recursively call constructor for nested classes
                    let nested_obj =
                        self.generate_constructor_call_with_types(&class_name, &type_args, &[])?;
                    self.builder
                        .build_store(field_ptr, nested_obj)
                        .map_err(|e| e.to_string())?;
                }
            }
            _ => return Err(format!("Unsupported field type: {:?}", resolved_type)),
        }
        Ok(())
    }

    pub(super) fn generate_tuple_constructor(
        &mut self,
        left_type: &Type,
        right_type: &Type,
    ) -> Result<BasicValueEnum<'a>, String> {
        let left_ptr = self.create_default_value_ptr(left_type)?;
        let right_ptr = self.create_default_value_ptr(right_type)?;
        // mux_new_tuple clones its arguments, so the owned default values are
        // ours to release; register them for statement-end cleanup.
        self.register_temp(left_ptr.into());
        self.register_temp(right_ptr.into());

        let tuple_value = self
            .generate_runtime_call("mux_new_tuple", &[left_ptr.into(), right_ptr.into()])
            .expect("mux_new_tuple should always return a value");

        let wrapped_value = self
            .generate_runtime_call("mux_tuple_value", &[tuple_value.into()])
            .expect("mux_tuple_value should always return a value");

        // Owned (+1) tuple Value; register so a non-binding use is released and a
        // binding transfers ownership instead of deep-cloning.
        self.register_temp(wrapped_value);
        Ok(wrapped_value)
    }

    pub(super) fn create_default_value_ptr(
        &mut self,
        mux_type: &Type,
    ) -> Result<PointerValue<'a>, String> {
        let resolved_type = self.resolve_type(mux_type)?;
        match resolved_type {
            Type::Primitive(PrimitiveType::Int) => {
                let zero = self.context.i64_type().const_zero();
                Ok(self.box_value(zero.into()))
            }
            Type::Primitive(PrimitiveType::Float) => {
                let zero = self.context.f64_type().const_zero();
                Ok(self.box_value(zero.into()))
            }
            Type::Primitive(PrimitiveType::Bool) => {
                let zero = self.context.bool_type().const_zero();
                Ok(self.box_value(zero.into()))
            }
            Type::Primitive(PrimitiveType::Str) => {
                let str_ptr = self
                    .builder
                    .build_global_string_ptr("", "empty_str")
                    .map_err(|e| e.to_string())?;
                let value_ptr = self
                    .generate_runtime_call(
                        "mux_new_string_from_cstr",
                        &[str_ptr.as_pointer_value().into()],
                    )
                    .expect("mux_new_string_from_cstr should always return a value");
                Ok(value_ptr.into_pointer_value())
            }
            Type::List(_) => {
                let val = self.create_empty_collection_value("mux_new_list", "mux_list_value");
                Ok(val.into_pointer_value())
            }
            Type::Map(_, _) => {
                let val = self.create_empty_collection_value("mux_new_map", "mux_map_value");
                Ok(val.into_pointer_value())
            }
            Type::Set(_) => {
                let val = self.create_empty_collection_value("mux_new_set", "mux_set_value");
                Ok(val.into_pointer_value())
            }
            Type::Tuple(left_type, right_type) => {
                let tuple_value = self.generate_tuple_constructor(&left_type, &right_type)?;
                Ok(tuple_value.into_pointer_value())
            }
            Type::Optional(_) => {
                let optional_ptr = self
                    .generate_runtime_call("mux_optional_none", &[])
                    .expect("mux_optional_none should always return a value");
                Ok(optional_ptr.into_pointer_value())
            }
            Type::Result(ok_type, _) => {
                let ok_value = self.create_default_value_ptr(&ok_type)?;
                let result_ptr = self
                    .generate_runtime_call("mux_result_ok_value", &[ok_value.into()])
                    .expect("mux_result_ok_value should always return a value");
                Ok(result_ptr.into_pointer_value())
            }
            Type::Named(name, type_args) => {
                if name == "optional" {
                    let optional_ptr = self
                        .generate_runtime_call("mux_optional_none", &[])
                        .expect("mux_optional_none should always return a value");
                    return Ok(optional_ptr.into_pointer_value());
                }
                if name == "result" {
                    if let Some(ok_type) = type_args.first() {
                        let ok_value = self.create_default_value_ptr(ok_type)?;
                        let result_ptr = self
                            .generate_runtime_call("mux_result_ok_value", &[ok_value.into()])
                            .expect("mux_result_ok_value should always return a value");
                        return Ok(result_ptr.into_pointer_value());
                    }
                    return Ok(self.context.ptr_type(AddressSpace::default()).const_zero());
                }
                if self.classes.contains_key(&name) {
                    let obj_value =
                        self.generate_constructor_call_with_types(&name, &type_args, &[])?;
                    return Ok(obj_value.into_pointer_value());
                }
                Ok(self.context.ptr_type(AddressSpace::default()).const_zero())
            }
            Type::Instantiated(name, type_args) => {
                if self.classes.contains_key(&name) {
                    let obj_value =
                        self.generate_constructor_call_with_types(&name, &type_args, &[])?;
                    return Ok(obj_value.into_pointer_value());
                }
                Ok(self.context.ptr_type(AddressSpace::default()).const_zero())
            }
            _ => Ok(self.context.ptr_type(AddressSpace::default()).const_zero()),
        }
    }

    pub(super) fn generate_constructor_call_with_types(
        &mut self,
        class_name: &str,
        type_args: &[Type],
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        if class_name == "tuple"
            && type_args.len() == 2
            && let [left_type, right_type] = type_args
        {
            return self.generate_tuple_constructor(left_type, right_type);
        }
        // create generic context for this instantiation
        let context = GenericContext {
            type_params: self.build_type_param_map(class_name, type_args)?,
        };

        // Save whatever generic context was active before this call (e.g. the
        // specialized common/static method this constructor call is nested inside)
        // so it can be restored afterwards, rather than discarded.
        let old_context = self.generic_context.take();
        self.context_stack.push(context.clone());
        self.generic_context = Some(context);

        // generate specialized methods for this class variant if not already generated
        if !type_args.is_empty() {
            self.generate_specialized_methods(class_name, type_args)?;
        }

        // generate constructor with context
        let result = self.generate_constructor_call(class_name, args);

        // restore the context that was active before this call
        self.context_stack.pop();
        self.generic_context = old_context;

        result
    }

    #[allow(clippy::only_used_in_recursion)]
    pub(super) fn sanitize_type_name(&self, type_: &Type) -> String {
        match type_ {
            Type::Primitive(PrimitiveType::Int) => "int".to_string(),
            Type::Primitive(PrimitiveType::Float) => "float".to_string(),
            Type::Primitive(PrimitiveType::Bool) => "bool".to_string(),
            Type::Primitive(PrimitiveType::Str) => "string".to_string(),
            Type::Named(name, type_args) => {
                if type_args.is_empty() {
                    name.clone()
                } else {
                    let args_str = type_args
                        .iter()
                        .map(|arg| self.sanitize_type_name(arg))
                        .collect::<Vec<_>>()
                        .join("_");
                    format!("{}_{}", name, args_str)
                }
            }
            Type::Generic(name) | Type::Variable(name) => name.clone(),
            Type::List(inner) => format!("list_{}", self.sanitize_type_name(inner)),
            Type::Map(k, v) => format!(
                "map_{}_{}",
                self.sanitize_type_name(k),
                self.sanitize_type_name(v)
            ),
            Type::Set(inner) => format!("set_{}", self.sanitize_type_name(inner)),

            Type::Optional(inner) => format!("optional_{}", self.sanitize_type_name(inner)),
            Type::Result(ok, err) => format!(
                "result_{}_{}",
                self.sanitize_type_name(ok),
                self.sanitize_type_name(err)
            ),
            Type::Instantiated(name, type_args) => {
                let args_str = type_args
                    .iter()
                    .map(|arg| self.sanitize_type_name(arg))
                    .collect::<Vec<_>>()
                    .join("$");
                format!("{}${}", name, args_str)
            }
            _ => "unknown".to_string(),
        }
    }

    pub(super) fn create_specialized_method_name(
        &self,
        class_name: &str,
        type_args: &[Type],
        method_name: &str,
    ) -> String {
        if type_args.is_empty() {
            format!("{}.{}", class_name, method_name)
        } else {
            let args_str = type_args
                .iter()
                .map(|t| self.sanitize_type_name(t))
                .collect::<Vec<_>>()
                .join("$");
            format!("{}${}.{}", class_name, args_str, method_name)
        }
    }

    pub(super) fn generate_constructor_call(
        &mut self,
        class_name: &str,
        _args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        // get the class type from our type map
        let class_type = *self
            .type_map
            .get(class_name)
            .ok_or(format!("Class '{}' not found in type map", class_name))?;

        // register the object type if not already registered
        let type_name = format!("type_name_{}", class_name);
        let type_name_global = self
            .builder
            .build_global_string_ptr(class_name, &type_name)
            .map_err(|e| e.to_string())?;
        if let Some(global) = self.module.get_global(&type_name) {
            global.set_linkage(inkwell::module::Linkage::External);
        }
        let type_size = class_type.size_of().ok_or("Cannot get type size")?;
        let type_id_val =
            self.register_class_type(class_name, type_name_global.as_pointer_value(), type_size)?;

        let (obj_value_ptr, struct_ptr) = self.allocate_class_object(type_id_val)?;

        // initialize fields based on their types. Field struct indices come
        // from field_map (keyed by name), not by enumerating self.classes
        // positionally, because classes that implement interfaces have
        // vtable pointer fields inserted ahead of the declared fields in the
        // LLVM struct layout (see generate_class_type).
        if let Some(fields) = self.classes.get(class_name).cloned() {
            for field in &fields {
                let field_index = *self
                    .field_map
                    .get(class_name)
                    .ok_or_else(|| format!("Field map not found for class {}", class_name))?
                    .get(&field.name)
                    .ok_or_else(|| {
                        format!("Field {} not found in class {}", field.name, class_name)
                    })?;
                let field_ptr = self
                    .builder
                    .build_struct_gep(
                        class_type.into_struct_type(),
                        struct_ptr,
                        field_index as u32,
                        &field.name,
                    )
                    .map_err(|e| e.to_string())?;

                self.store_initialized_field(field_ptr, field)?;
            }
        }

        // set vtable fields for any interfaces this class implements.
        // Generic classes never get a vtable generated (see
        // generate_class_vtables) since the vtable would have to reference
        // an unspecialized method that has no body; skip those fields
        // rather than erroring, since the vtable field is never read for
        // dispatch (interfaces use static dispatch).
        let interfaces = self
            .analyzer
            .all_symbols()
            .get(class_name)
            .map(|sym| sym.interfaces.clone())
            .unwrap_or_default();
        for interface_name in interfaces.keys() {
            let vtable_key = format!("{}_{}", class_name, interface_name);
            let Some(vtable_ptr) = self.vtable_map.get(&vtable_key).copied() else {
                continue;
            };
            let vtable_field_name = format!("vtable_{}", interface_name);
            let field_index = *self
                .field_map
                .get(class_name)
                .ok_or_else(|| format!("Field map not found for class {}", class_name))?
                .get(&vtable_field_name)
                .ok_or_else(|| {
                    format!(
                        "Vtable field {} not found in class {}",
                        vtable_field_name, class_name
                    )
                })?;
            let field_ptr = self
                .builder
                .build_struct_gep(
                    class_type.into_struct_type(),
                    struct_ptr,
                    field_index as u32,
                    &vtable_field_name,
                )
                .map_err(|e| e.to_string())?;
            self.builder
                .build_store(field_ptr, vtable_ptr)
                .map_err(|e| e.to_string())?;
        }

        // return the allocated object as a boxed Value pointer
        Ok(obj_value_ptr.into())
    }

    fn get_self_class_info(&mut self) -> Result<(PointerValue<'a>, String, Vec<Type>), String> {
        // get self pointer
        let (self_ptr, _, _) = self
            .variables
            .get("self")
            .or_else(|| self.global_variables.get("self"))
            .ok_or("Self not found in method call")?;

        // get class name and type args from self type
        if let Some((_, _, Type::Named(class_name, type_args))) = self
            .variables
            .get("self")
            .or_else(|| self.global_variables.get("self"))
        {
            Ok((*self_ptr, class_name.clone(), type_args.clone()))
        } else {
            Err("Self type not found".to_string())
        }
    }

    fn resolve_method_function_name(
        &mut self,
        class_name: &str,
        type_args: &[Type],
        method_name: &str,
    ) -> Result<String, String> {
        // resolve method function name, preferring specialized names in generic contexts
        let mut method_func_name =
            self.create_specialized_method_name(class_name, type_args, method_name);
        if !self.functions.contains_key(&method_func_name) {
            if type_args.is_empty()
                && let Some(current_fn) = &self.current_function_name
                && let Some((current_class_part, _)) = current_fn.split_once('.')
                && current_class_part.starts_with(&format!("{}$", class_name))
            {
                let contextual_name = format!("{}.{}", current_class_part, method_name);
                if self.functions.contains_key(&contextual_name) {
                    method_func_name = contextual_name;
                } else {
                    method_func_name = format!("{}.{}", class_name, method_name);
                }
            } else {
                method_func_name = format!("{}.{}", class_name, method_name);
            }
        }
        Ok(method_func_name)
    }

    fn check_method_is_static(
        &mut self,
        class_name: &str,
        method_name: &str,
    ) -> Result<(), String> {
        if let Some(class) = self.analyzer.symbol_table().lookup(class_name)
            && let Some(method) = class.methods.get(method_name)
            && method.is_static
        {
            return Err(format!(
                "Cannot call static method {} with self",
                method_name
            ));
        }
        Ok(())
    }

    fn build_call_arguments_for_method(
        &mut self,
        self_ptr: PointerValue<'a>,
        args: &[ExpressionNode],
        is_specialized: bool,
    ) -> Result<Vec<BasicMetadataValueEnum<'a>>, String> {
        // load the actual object pointer from the alloca first
        let self_loaded = self
            .builder
            .build_load(
                self.context.ptr_type(AddressSpace::default()),
                self_ptr,
                "load_self_for_method_call",
            )
            .map_err(|e| e.to_string())?;
        let mut call_args: Vec<BasicMetadataValueEnum> = vec![self_loaded.into()];
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

    pub(super) fn generate_method_call_on_self(
        &mut self,
        method_name: &str,
        args: &[ExpressionNode],
    ) -> Result<BasicValueEnum<'a>, String> {
        let (self_ptr, class_name, type_args) = self.get_self_class_info()?;

        let method_func_name =
            self.resolve_method_function_name(&class_name, &type_args, method_name)?;

        self.check_method_is_static(&class_name, method_name)?;

        // get the function
        let func_val = *self
            .functions
            .get(&method_func_name)
            .ok_or(format!("Method {} not found", method_func_name))?;

        let is_specialized = method_func_name.contains('$');
        let call_args = self.build_call_arguments_for_method(self_ptr, args, is_specialized)?;

        // call the method
        let call = self
            .builder
            .build_call(func_val, &call_args, &format!("call_{}", method_name))
            .map_err(|e| e.to_string())?;

        if let Some(value) = call.try_as_basic_value().basic() {
            Ok(value)
        } else {
            // Void-returning self-method calls can appear as expression statements.
            // Return a dummy value for the expression path; callers that require a
            // real value should have been rejected by semantic analysis.
            Ok(self.context.i64_type().const_zero().into())
        }
    }
}
