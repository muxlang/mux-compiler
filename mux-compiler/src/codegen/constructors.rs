//! Constructor generation for classes and enums.
//!
//! This module handles generating constructors and related initialization code.

use super::CodeGenerator;
use crate::ast::{EnumVariant, ExpressionNode, Field, LiteralNode, PrimitiveType, TypeKind};
use crate::semantics::{GenericContext, MethodSig, Type, mangle_type_name};
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
        // The final value stored here is owned by the field (released by the
        // object's destructor), so it is transferred out of the pending temporary
        // set rather than freed at the constructor's statement boundary -
        // otherwise the field is left dangling. Any *intermediate* temporaries a
        // compound default expression produced (e.g. the operands of `"a" + "b"`,
        // or arguments to a call) are still owned and must be released, or they
        // leak - so only the stored value is untracked; the rest are cleaned up.
        let temp_mark = self.temp_mark();
        if let Some(default_expr) = &field.default_value {
            let value = self.generate_expression(default_expr)?;
            let scalar = self.scalar_field_type(&field.type_);
            let stored_val = match scalar {
                // A scalar slot holds the value itself; the box the default
                // expression produced is a temporary like any other.
                Some(scalar) => self.coerce_to_scalar(value, scalar)?,
                None if matches!(field.type_.kind, TypeKind::Primitive(_)) => {
                    self.box_value(value).into()
                }
                None => value,
            };
            self.builder
                .build_store(field_ptr, stored_val)
                .map_err(|e| e.to_string())?;
            self.untrack_temp(stored_val);
            self.cleanup_temps_to(temp_mark)?;
        } else {
            let field_type = self.type_node_to_type(&field.type_);
            self.initialize_field_by_type(field_ptr, &field_type, field.is_generic_param)?;
            self.discard_temps_to(temp_mark);
        }
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
        let full_name = format!("{name}!{variant_name}");

        // params: variant.data (extract types, discard field names for codegen)
        let field_count = variant.data.as_ref().map_or(0, std::vec::Vec::len);
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
            self.store_constructor_field(variant, struct_type, temp_ptr, function, i)?;
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
            .insert(format!("{name}.{variant_name}"), function);
        Ok(())
    }

    /// Store constructor parameter `i` into the enum struct at `temp_ptr`. A
    /// nested user enum is stored inline and its payloads retained (mirroring
    /// `rc_inc_if_pointer` for a direct pointer payload); any other payload is
    /// retained if it is a pointer and stored directly.
    fn store_constructor_field(
        &mut self,
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
        // A nested user enum is normally stored inline. The union slot is sized
        // to the widest payload at this position (issue #309), so it is at least
        // as large as this enum's struct - unless the enum is recursive or
        // mutually-referential, which has no finite inline layout and falls back
        // to a pointer slot. Such a payload is heap-boxed into a Value::Opaque
        // instead of stored inline (issue #309).
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
            return self.store_boxed_recursive_field(&inner, arg, data_ptr);
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

    /// The global holding a class's runtime type name, created once per class.
    ///
    /// `build_global_string_ptr` mints a fresh global every call and LLVM
    /// uniquifies the clashing name, so a class built from three places carried
    /// three identical `type_name_X` constants. Only the first was ever read:
    /// registration is guarded by the class's shared `type_id` slot, so the rest
    /// were dead constants in the module.
    fn class_type_name_global(&mut self, class_name: &str) -> Result<PointerValue<'a>, String> {
        let global_name = format!("type_name_{class_name}");
        if let Some(existing) = self.module.get_global(&global_name) {
            return Ok(existing.as_pointer_value());
        }
        let global = self
            .builder
            .build_global_string_ptr(class_name, &global_name)
            .map_err(|e| e.to_string())?;
        global.set_linkage(inkwell::module::Linkage::External);
        Ok(global.as_pointer_value())
    }

    /// Register the class with the runtime the first time an instance is built,
    /// and return its type id.
    ///
    /// The runtime hands out a fresh id per call, so registering on every
    /// construction gave each instance a type of its own - two instances of one
    /// class never shared a type id, and the registry grew without bound. A
    /// per-class global holds the id after the first construction; zero means
    /// "not registered yet", which no id ever is.
    fn register_class_type(
        &mut self,
        name: &str,
        type_name_global: PointerValue<'a>,
        type_size: IntValue<'a>,
    ) -> Result<IntValue<'a>, String> {
        let i32_type = self.context.i32_type();
        let slot_name = format!("{name}.type_id");
        let slot = match self.module.get_global(&slot_name) {
            Some(existing) => existing,
            None => {
                let global = self.module.add_global(i32_type, None, &slot_name);
                global.set_initializer(&i32_type.const_zero());
                global
            }
        };
        let slot = slot.as_pointer_value();

        let function = self
            .builder
            .get_insert_block()
            .and_then(inkwell::basic_block::BasicBlock::get_parent)
            .ok_or("register_class_type needs an enclosing function")?;
        let register_block = self.context.append_basic_block(function, "register_type");
        let registered_block = self.context.append_basic_block(function, "type_registered");
        let existing = self
            .builder
            .build_load(i32_type, slot, "existing_type_id")
            .map_err(|e| e.to_string())?
            .into_int_value();
        let unregistered = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                existing,
                i32_type.const_zero(),
                "unregistered",
            )
            .map_err(|e| e.to_string())?;
        self.builder
            .build_conditional_branch(unregistered, register_block, registered_block)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(register_block);
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

        self.register_class_capabilities(name, type_id_val)?;
        self.builder
            .build_store(slot, type_id_val)
            .map_err(|e| e.to_string())?;
        self.builder
            .build_unconditional_branch(registered_block)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(registered_block);
        self.builder
            .build_load(i32_type, slot, "class_type_id")
            .map_err(|e| e.to_string())
            .map(|value| value.into_int_value())
    }

    /// Hand the runtime the class's own equality, ordering and hash, so a map,
    /// a set or `contains` matches instances the way the operators do instead
    /// of comparing addresses. A capability the class did not declare is left
    /// unregistered, and such an instance keeps identity semantics.
    fn register_class_capabilities(
        &mut self,
        name: &str,
        type_id_val: IntValue<'a>,
    ) -> Result<(), String> {
        for (method, register) in [
            ("eq_glue", "mux_register_object_equals"),
            ("cmp_glue", "mux_register_object_compare"),
            // `hash` returns an `int`, already the 64-bit value the runtime
            // wants, so it is registered without a wrapper. Its gate lives in
            // `class_capability_method`, so a class with a `hash` method of its
            // own is not registered as if it had promised the capability.
            ("hash", "mux_register_object_hash"),
        ] {
            // The glue exists only when the capability was both declared and
            // implemented, so its presence is the gate for the two wrappers.
            let func = if let Some(base) = method.strip_suffix("_glue") {
                match self.class_capability_method(name, base) {
                    Some(_) => self.module.get_function(&format!("{name}.{method}")),
                    None => None,
                }
            } else {
                self.class_capability_method(name, method)
            };
            let Some(func) = func else {
                continue;
            };
            let register_func = self
                .runtime_function(register)
                .ok_or_else(|| format!("{register} not found"))?;
            self.builder
                .build_call(
                    register_func,
                    &[
                        type_id_val.into(),
                        func.as_global_value().as_pointer_value().into(),
                    ],
                    register,
                )
                .map_err(|e| e.to_string())?;
        }
        Ok(())
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
        let full_name = format!("{name}.new");

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
        let type_name_global = self.class_type_name_global(name)?;
        let type_size = self
            .type_map
            .get(name)
            .ok_or("Class type not found")?
            .size_of()
            .ok_or("Cannot get type size")?;
        let type_id_val = self.register_class_type(name, type_name_global, type_size)?;

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
            let vtable_key = format!("{name}_{interface_name}");
            let Some(vtable_ptr) = self.vtable_map.get(&vtable_key) else {
                continue;
            };
            let vtable_field_name = format!("vtable_{interface_name}");
            let field_index = self
                .field_map
                .get(name)
                .ok_or_else(|| format!("Field map not found for class {name}"))?
                .get(&vtable_field_name)
                .ok_or_else(|| {
                    format!("Vtable field {vtable_field_name} not found in class {name}")
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
        self.constructors.insert(format!("{name}.new"), function);
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
            // A scalar field holds the value itself, so zero the slot directly.
            // Its width is exactly the scalar's, unlike the pointer-sized slot
            // this used to be, where a raw store would have left the upper bytes
            // uninitialized for a later boxed read to dereference.
            Type::Primitive(PrimitiveType::Bool) => {
                let false_val = self.context.bool_type().const_int(0, false);
                self.builder
                    .build_store(field_ptr, false_val)
                    .map_err(|e| e.to_string())?;
            }
            Type::Primitive(PrimitiveType::Int | PrimitiveType::Char) => {
                let zero_val = self.context.i64_type().const_int(0, false);
                self.builder
                    .build_store(field_ptr, zero_val)
                    .map_err(|e| e.to_string())?;
            }
            Type::Primitive(PrimitiveType::Float) => {
                let zero_val = self.context.f64_type().const_float(0.0);
                self.builder
                    .build_store(field_ptr, zero_val)
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
                } else if !self.type_map.contains_key(&class_name) {
                    // An opaque stdlib type - `Json`, `TcpListener`, `Csv`,
                    // `sql.Value`. These are runtime-registered handles with no
                    // Mux layout, so there is no constructor to call and no
                    // struct to zero; the slot holds a pointer and starts null.
                    //
                    // Constructing them unconditionally made a field of such a
                    // type an internal compiler error on a three-line program
                    // (#408). Null matches what an uninitialized `string` field
                    // already does, and `mux_rc_dec` is null-safe, so teardown
                    // is unaffected.
                    let null_ptr = self.context.ptr_type(AddressSpace::default()).const_null();
                    self.builder
                        .build_store(field_ptr, null_ptr)
                        .map_err(|e| e.to_string())?;
                } else {
                    // recursively call constructor for nested classes
                    let nested_obj =
                        self.generate_constructor_call_with_types(&class_name, &type_args, &[])?;
                    self.builder
                        .build_store(field_ptr, nested_obj)
                        .map_err(|e| e.to_string())?;
                }
            }
            _ => return Err(format!("Unsupported field type: {resolved_type:?}")),
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

        // Build the instantiation's own layout, not the type-erased one shared
        // by the declaration: `Box<int>` allocates `{ i64, i64 }` and registers
        // its own size, copy and destructor with the runtime.
        let layout_name = self.ensure_class_instantiated(class_name, type_args)?;

        // generate constructor with context
        let result = self.generate_constructor_call(&layout_name, args);

        // restore the context that was active before this call
        self.context_stack.pop();
        self.generic_context = old_context;

        result
    }

    pub(super) fn sanitize_type_name(&self, type_: &Type) -> String {
        mangle_type_name(type_)
    }

    pub(super) fn create_specialized_method_name(
        &self,
        class_name: &str,
        type_args: &[Type],
        method_name: &str,
    ) -> String {
        if type_args.is_empty() {
            format!("{class_name}.{method_name}")
        } else {
            let args_str = type_args
                .iter()
                .map(|t| self.sanitize_type_name(t))
                .collect::<Vec<_>>()
                .join("$");
            format!("{class_name}${args_str}.{method_name}")
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
            .ok_or(format!("Class '{class_name}' not found in type map"))?;

        // register the object type if not already registered
        let type_name_global = self.class_type_name_global(class_name)?;
        let type_size = class_type.size_of().ok_or("Cannot get type size")?;
        let type_id_val = self.register_class_type(class_name, type_name_global, type_size)?;

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
                    .ok_or_else(|| format!("Field map not found for class {class_name}"))?
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
            .get(Self::declared_class_name(class_name))
            .map(|sym| sym.interfaces.clone())
            .unwrap_or_default();
        for interface_name in interfaces.keys() {
            let vtable_key = format!("{class_name}_{interface_name}");
            let Some(vtable_ptr) = self.vtable_map.get(&vtable_key).copied() else {
                continue;
            };
            let vtable_field_name = format!("vtable_{interface_name}");
            let field_index = *self
                .field_map
                .get(class_name)
                .ok_or_else(|| format!("Field map not found for class {class_name}"))?
                .get(&vtable_field_name)
                .ok_or_else(|| {
                    format!("Vtable field {vtable_field_name} not found in class {class_name}")
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
                && current_class_part.starts_with(&format!("{class_name}$"))
            {
                let contextual_name = format!("{current_class_part}.{method_name}");
                if self.functions.contains_key(&contextual_name) {
                    method_func_name = contextual_name;
                } else {
                    method_func_name = format!("{class_name}.{method_name}");
                }
            } else {
                method_func_name = format!("{class_name}.{method_name}");
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
            return Err(format!("Cannot call static method {method_name} with self"));
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
            .ok_or(format!("Method {method_func_name} not found"))?;

        let is_specialized = method_func_name.contains('$');
        let call_args = self.build_call_arguments_for_method(self_ptr, args, is_specialized)?;

        // call the method
        let call = self
            .builder
            .build_call(func_val, &call_args, &format!("call_{method_name}"))
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
