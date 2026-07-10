//! User-defined type generation (classes, interfaces, enums).
//!
//! This module handles generating LLVM types for classes, interfaces, and enums.

use super::CodeGenerator;
use crate::ast::{
    AstNode, EnumVariant, EnumVariantField, Field, PrimitiveType, TypeKind, TypeNode,
};
use crate::semantics::{MethodSig, Type};
use inkwell::AddressSpace;
use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::{BasicValueEnum, IntValue};
use std::collections::HashMap;

impl<'a> CodeGenerator<'a> {
    fn class_field_llvm_type(
        &self,
        class_type_param_names: &std::collections::HashSet<String>,
        field: &Field,
    ) -> Result<BasicTypeEnum<'a>, String> {
        let ptr_type = self.context.ptr_type(AddressSpace::default());

        if matches!(field.type_.kind, TypeKind::Primitive(_)) {
            return Ok(ptr_type.into());
        }

        if let TypeNode {
            kind: TypeKind::Named(type_name, _),
            ..
        } = &field.type_
            && class_type_param_names.contains(type_name)
        {
            return Ok(ptr_type.into());
        }

        self.llvm_type_from_mux_type(&field.type_)
    }

    pub(super) fn generate_user_defined_types(&mut self, nodes: &[AstNode]) -> Result<(), String> {
        // generate LLVM types for classes, interfaces, enums
        for node in nodes {
            match node {
                AstNode::Class { name, fields, .. } => {
                    let interfaces = self
                        .analyzer
                        .all_symbols()
                        .get(name)
                        .map(|sym| sym.interfaces.clone())
                        .unwrap_or_default();
                    self.classes.insert(name.clone(), fields.clone());
                    self.generate_class_type(name, fields, &interfaces)?;
                }
                AstNode::Interface { name, .. } => {
                    self.generate_interface_type(name)?;
                }
                AstNode::Enum { name, variants, .. } => {
                    self.generate_enum_type(name, variants)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub(super) fn generate_class_type(
        &mut self,
        name: &str,
        fields: &[Field],
        interfaces: &HashMap<String, (Vec<Type>, HashMap<String, MethodSig>)>,
    ) -> Result<(), String> {
        let mut field_types = Vec::new();
        let mut field_indices = HashMap::new();

        // Collect type parameter names for this class from the symbol table
        let type_param_names: std::collections::HashSet<String> =
            if let Some(class_symbol) = self.analyzer.all_symbols().get(name) {
                class_symbol
                    .type_params
                    .iter()
                    .map(|(param_name, _)| param_name.clone())
                    .collect()
            } else {
                std::collections::HashSet::new()
            };

        let ptr_type = self.context.ptr_type(AddressSpace::default());
        for interface_name in interfaces.keys() {
            field_types.push(ptr_type.into());
            field_indices.insert(format!("vtable_{}", interface_name), field_types.len() - 1);
        }

        for field in fields {
            let field_type = self.class_field_llvm_type(&type_param_names, field)?;
            field_types.push(field_type);
            field_indices.insert(field.name.clone(), field_types.len() - 1);
        }

        let struct_type = self.context.struct_type(&field_types, false);
        self.type_map.insert(name.to_string(), struct_type.into());
        self.field_map.insert(name.to_string(), field_indices);
        self.field_types_map.insert(name.to_string(), field_types);

        Ok(())
    }

    /// Generate per-class deep-copy and destructor functions and store
    /// their function pointers in `class_copy_fns` / `class_destructor_fns`
    /// so the constructor body can register them with the runtime.
    ///
    /// The copy function:
    ///   1. Copies the class data bytes from `src` to `dst`.
    ///   2. For each user field, replaces the destination pointer with a
    ///      refcount-isolated clone produced by `mux_value_deep_clone`.
    ///
    /// The destructor function calls `mux_rc_dec` on every user field so
    /// the runtime releases each boxed `Value` when the class is freed.
    pub(super) fn generate_class_copy_and_destructor(
        &mut self,
        name: &str,
        fields: &[Field],
    ) -> Result<(), String> {
        let void_type = self.context.void_type();
        let i8_ptr = self.context.ptr_type(AddressSpace::default());
        let fn_type = void_type.fn_type(&[i8_ptr.into(), i8_ptr.into()], false);
        let copy_fn = self.module.add_function(
            &format!("{}.copy", name),
            fn_type,
            Some(inkwell::module::Linkage::External),
        );
        let destructor_type = void_type.fn_type(&[i8_ptr.into()], false);
        let destructor_fn = self.module.add_function(
            &format!("{}.destructor", name),
            destructor_type,
            Some(inkwell::module::Linkage::External),
        );
        self.class_copy_fns.insert(
            name.to_string(),
            copy_fn.as_global_value().as_pointer_value(),
        );
        self.class_destructor_fns.insert(
            name.to_string(),
            destructor_fn.as_global_value().as_pointer_value(),
        );

        let class_type = *self
            .type_map
            .get(name)
            .ok_or_else(|| format!("Class {} not in type map", name))?;
        let class_size = class_type
            .size_of()
            .ok_or_else(|| format!("Cannot get size of class {}", name))?;

        // Build the copy function body.
        let copy_entry = self.context.append_basic_block(copy_fn, "entry");
        self.builder.position_at_end(copy_entry);
        let src_ptr = copy_fn.get_nth_param(0).unwrap().into_pointer_value();
        let dst_ptr = copy_fn.get_nth_param(1).unwrap().into_pointer_value();
        self.generate_class_copy_body(name, fields, class_type, src_ptr, dst_ptr, class_size)?;
        self.builder.build_return(None).map_err(|e| e.to_string())?;

        // Build the destructor function body.
        let destr_entry = self.context.append_basic_block(destructor_fn, "entry");
        self.builder.position_at_end(destr_entry);
        let obj_ptr = destructor_fn.get_nth_param(0).unwrap().into_pointer_value();
        self.generate_class_destructor_body(name, fields, class_type, obj_ptr)?;
        self.builder.build_return(None).map_err(|e| e.to_string())?;

        Ok(())
    }

    fn generate_class_copy_body(
        &mut self,
        name: &str,
        fields: &[Field],
        class_type: BasicTypeEnum<'a>,
        src_ptr: inkwell::values::PointerValue<'a>,
        dst_ptr: inkwell::values::PointerValue<'a>,
        class_size: inkwell::values::IntValue<'a>,
    ) -> Result<(), String> {
        // Step 1: bulk-copy the class data (vtable + raw field bytes).
        let dst_typed = self
            .builder
            .build_pointer_cast(
                dst_ptr,
                self.context.ptr_type(AddressSpace::default()),
                "dst_typed",
            )
            .map_err(|e| e.to_string())?;
        let src_typed = self
            .builder
            .build_pointer_cast(
                src_ptr,
                self.context.ptr_type(AddressSpace::default()),
                "src_typed",
            )
            .map_err(|e| e.to_string())?;
        self.builder
            .build_memcpy(dst_typed, 1, src_typed, 1, class_size)
            .map_err(|e| e.to_string())?;

        // Step 2: for each user field, deep-clone the boxed value.
        let deep_clone = self
            .runtime_function("mux_value_deep_clone")
            .ok_or("mux_value_deep_clone not found")?;
        let i8_ptr_type = self.context.ptr_type(AddressSpace::default());
        for field in fields.iter() {
            let field_index = self
                .field_map
                .get(name)
                .and_then(|m| m.get(&field.name))
                .copied()
                .ok_or_else(|| format!("Field {} not in field_map for {}", field.name, name))?;
            // Inline fields (e.g. an enum struct) were already duplicated by the
            // bulk memcpy above and hold no boxed pointer to deep-clone.
            if !Self::class_field_is_boxed_pointer(class_type, field_index) {
                continue;
            }
            let field_ptr = self
                .builder
                .build_struct_gep(class_type, dst_typed, field_index as u32, &field.name)
                .map_err(|e| e.to_string())?;
            let field_val = self
                .builder
                .build_load(i8_ptr_type, field_ptr, &field.name)
                .map_err(|e| e.to_string())?;
            let cloned = self
                .builder
                .build_call(deep_clone, &[field_val.into()], &field.name)
                .map_err(|e| e.to_string())?
                .try_as_basic_value()
                .basic()
                .ok_or_else(|| {
                    format!("mux_value_deep_clone returned no value for {}", field.name)
                })?;
            self.builder
                .build_store(field_ptr, cloned)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn generate_class_destructor_body(
        &mut self,
        name: &str,
        fields: &[Field],
        class_type: BasicTypeEnum<'a>,
        obj_ptr: inkwell::values::PointerValue<'a>,
    ) -> Result<(), String> {
        let rc_dec = self
            .runtime_function("mux_rc_dec")
            .ok_or("mux_rc_dec not found")?;
        let i8_ptr_type = self.context.ptr_type(AddressSpace::default());
        let obj_typed = self
            .builder
            .build_pointer_cast(obj_ptr, i8_ptr_type, "obj_typed")
            .map_err(|e| e.to_string())?;
        for field in fields.iter() {
            let field_index = self
                .field_map
                .get(name)
                .and_then(|m| m.get(&field.name))
                .copied()
                .ok_or_else(|| format!("Field {} not in field_map for {}", field.name, name))?;
            // Fields stored inline (e.g. an enum held as a struct) are not boxed
            // `*mut Value` pointers; loading their first word and decrementing it
            // as a refcount would corrupt memory. They own no heap reference to
            // release, so skip them.
            if !Self::class_field_is_boxed_pointer(class_type, field_index) {
                continue;
            }
            let field_ptr = self
                .builder
                .build_struct_gep(class_type, obj_typed, field_index as u32, &field.name)
                .map_err(|e| e.to_string())?;
            let field_val = self
                .builder
                .build_load(i8_ptr_type, field_ptr, &field.name)
                .map_err(|e| e.to_string())?;
            self.builder
                .build_call(rc_dec, &[field_val.into()], &field.name)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Whether class field `field_index` is stored as a boxed `*mut Value`
    /// pointer (and therefore participates in reference counting) rather than
    /// inline data such as an enum struct.
    fn class_field_is_boxed_pointer(class_type: BasicTypeEnum<'a>, field_index: usize) -> bool {
        matches!(
            class_type
                .into_struct_type()
                .get_field_type_at_index(field_index as u32),
            Some(t) if t.is_pointer_type()
        )
    }

    pub(super) fn generate_class_vtables(
        &mut self,
        class_name: &str,
        interfaces: &HashMap<String, (Vec<Type>, HashMap<String, MethodSig>)>,
    ) -> Result<(), String> {
        // Generic classes only ever emit monomorphized method bodies (e.g.
        // "Graph$string.len"), never an unspecialized "Graph.len" definition.
        // A vtable built from the unspecialized name would reference a
        // declaration with no body and fail at link time, and the vtable
        // field is never read for dispatch anyway (interfaces use static
        // dispatch), so skip vtable generation for generic classes entirely.
        let is_generic = self
            .analyzer
            .all_symbols()
            .get(class_name)
            .is_some_and(|sym| !sym.type_params.is_empty());
        if is_generic {
            return Ok(());
        }

        for (interface_name, (_, interface_methods)) in interfaces {
            let mut vtable_values = Vec::new();
            for method_name in interface_methods.keys() {
                let class_method_name = format!("{}.{}", class_name, method_name);
                let func = self.functions.get(&class_method_name).ok_or_else(|| {
                    format!(
                        "Class {} does not implement method {} for interface {}",
                        class_name, method_name, interface_name
                    )
                })?;
                vtable_values.push(func.as_global_value().as_pointer_value().into());
            }
            // get vtable struct type
            let vtable_type = self
                .vtable_type_map
                .get(interface_name)
                .expect("vtable_type should be registered during interface generation");
            let vtable_const = vtable_type.const_named_struct(&vtable_values);
            // create global
            let vtable_name = format!("{}_{}_vtable", class_name, interface_name);
            let global =
                self.module
                    .add_global(vtable_type.as_basic_type_enum(), None, &vtable_name);
            global.set_initializer(&vtable_const);
            self.vtable_map.insert(
                format!("{}_{}", class_name, interface_name),
                global.as_pointer_value(),
            );
        }
        Ok(())
    }

    pub(super) fn generate_interface_type(&mut self, name: &str) -> Result<(), String> {
        // generate LLVM struct for interface: { *mut vtable, field1, field2, ... }
        // for simplicity, vtable is struct of void* function pointers
        let symbol = self
            .analyzer
            .all_symbols()
            .get(name)
            .ok_or_else(|| format!("Interface symbol '{}' not found in symbol table", name))?;
        let (_, interface_methods) = symbol
            .interfaces
            .get(name)
            .ok_or_else(|| format!("Interface methods for '{}' not found", name))?;

        // create vtable as struct of function pointers (all (void*) -> void* for now)
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let fn_ptr_type = ptr_type; // since fn_type.ptr_type deprecated, use ptr_type

        let vtable_types = vec![fn_ptr_type.into(); interface_methods.len()];

        // vtable type: struct of function pointers
        let vtable_struct_type = self.context.struct_type(&vtable_types, false);
        self.vtable_type_map
            .insert(name.to_string(), vtable_struct_type);
        let vtable_ptr_type = self.context.ptr_type(AddressSpace::default());

        // interface struct: { vtable_ptr, field1, field2, ... }
        let mut struct_fields = vec![vtable_ptr_type.into()];

        // Add interface fields to the struct
        for (field_type, _) in symbol.fields.values() {
            let llvm_field_type = self.semantic_type_to_llvm(field_type)?;
            struct_fields.push(llvm_field_type);
        }

        let interface_struct_type = self.context.struct_type(&struct_fields, false);
        self.type_map
            .insert(name.to_string(), interface_struct_type.into());

        Ok(())
    }

    pub(super) fn generate_enum_type(
        &mut self,
        name: &str,
        variants: &[EnumVariant],
    ) -> Result<(), String> {
        // Tagged union: {i32 discriminant, <union fields...>}
        // Union fields are determined by analyzing all variant field types.
        let i32_type = self.context.i32_type();
        let mut variant_names = Vec::new();
        let mut variant_fields = HashMap::new();
        let mut max_fields = 0;
        for variant in variants {
            variant_names.push(variant.name.clone());
            let field_types: Vec<EnumVariantField> = variant.data.clone().unwrap_or_default();
            max_fields = max_fields.max(field_types.len());
            variant_fields.insert(variant.name.clone(), field_types);
        }
        self.enum_variants.insert(name.to_string(), variant_names);
        self.enum_variant_fields
            .insert(name.to_string(), variant_fields);

        // create struct type with discriminant + actual field types from variants
        let mut struct_fields = vec![i32_type.into()]; // discriminant first
        let union_field_types = self.get_enum_union_field_types(name);
        struct_fields.extend(union_field_types);
        let struct_type = self.context.struct_type(&struct_fields, false);
        self.type_map.insert(name.to_string(), struct_type.into());
        Ok(())
    }

    pub(super) fn get_variant_index(
        &self,
        enum_name: &str,
        variant_name: &str,
    ) -> Result<usize, String> {
        // hardcode indices for built-in enums to ensure deterministic behavior
        match (enum_name, variant_name) {
            ("optional", "some") => Ok(0),
            ("optional", "none") => Ok(1),
            ("result", "ok") => Ok(0),
            ("result", "err") => Ok(1),
            _ => {
                // for user-defined enums, use HashMap lookup
                if let Some(variants) = self.enum_variants.get(enum_name) {
                    variants
                        .iter()
                        .position(|v| v == variant_name)
                        .ok_or_else(|| {
                            format!("Variant {} not found in enum {}", variant_name, enum_name)
                        })
                } else {
                    Err(format!("Enum {} not found", enum_name))
                }
            }
        }
    }

    /// load the discriminant from an enum value as an i32
    /// for optional and result, all values are *mut Value -- use the Value-based discriminant functions
    /// for user-defined enums, load the discriminant field directly from the struct
    pub(super) fn load_enum_discriminant(
        &self,
        enum_name: &str,
        enum_value: BasicValueEnum<'a>,
    ) -> Result<IntValue<'a>, String> {
        match enum_name {
            "optional" | "result" => {
                let discriminant_func = if enum_name == "optional" {
                    "mux_value_optional_discriminant"
                } else {
                    "mux_value_result_discriminant"
                };
                let func = self
                    .runtime_function(discriminant_func)
                    .ok_or(format!("{} not found", discriminant_func))?;

                let discriminant_call = self
                    .builder
                    .build_call(func, &[enum_value.into()], "discriminant_call")
                    .map_err(|e| e.to_string())?;

                Ok(discriminant_call
                    .try_as_basic_value()
                    .basic()
                    .expect("discriminant function should return a basic value")
                    .into_int_value())
            }
            _ => {
                // for user-defined enums, load discriminant field directly
                let struct_type = self
                    .type_map
                    .get(enum_name)
                    .ok_or_else(|| format!("Enum {} not found in type map", enum_name))?
                    .into_struct_type();

                // allocate temporary storage for the enum value
                let temp_ptr = self
                    .builder
                    .build_alloca(struct_type, "temp_enum")
                    .map_err(|e| e.to_string())?;

                // store the enum value
                self.builder
                    .build_store(temp_ptr, enum_value)
                    .map_err(|e| e.to_string())?;

                // get pointer to discriminant field (index 0)
                let discriminant_ptr = self
                    .builder
                    .build_struct_gep(struct_type, temp_ptr, 0, "discriminant_ptr")
                    .map_err(|e| e.to_string())?;

                // load discriminant as i32
                let discriminant = self
                    .builder
                    .build_load(self.context.i32_type(), discriminant_ptr, "discriminant")
                    .map_err(|e| e.to_string())?
                    .into_int_value();

                Ok(discriminant)
            }
        }
    }

    /// create a type-safe comparison between discriminant and variant index
    /// this ensures both operands are i32 values and returns a boolean for branching
    pub(super) fn build_discriminant_comparison(
        &self,
        discriminant: IntValue<'a>,
        variant_index: usize,
    ) -> Result<IntValue<'a>, String> {
        let index_val = self
            .context
            .i32_type()
            .const_int(variant_index as u64, false);

        let result = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                discriminant,
                index_val,
                "match_cmp",
            )
            .map_err(|e| e.to_string())?;

        Ok(result)
    }

    /// determine the union field types for an enum based on its variants
    /// this replaces the hardcoded f64 assumption with actual field types
    pub(super) fn get_enum_union_field_types(&self, enum_name: &str) -> Vec<BasicTypeEnum<'a>> {
        let mut union_types = Vec::new();

        if let Some(variant_fields) = self.enum_variant_fields.get(enum_name) {
            // find the maximum number of fields across all variants
            let max_fields = variant_fields
                .values()
                .map(|fields| fields.len())
                .max()
                .unwrap_or(0);

            // for each field position, determine the appropriate union type
            for field_idx in 0..max_fields {
                let mut field_types = Vec::new();

                // collect all types used in this field position across variants
                for field_list in variant_fields.values() {
                    if field_idx < field_list.len() {
                        field_types.push(&field_list[field_idx].1);
                    }
                }

                // determine the union type for this field position
                let union_type = self.determine_union_field_type(&field_types);
                union_types.push(union_type);
            }
        }

        union_types
    }

    /// determine the appropriate LLVM type for a union field position
    /// for now, use the largest common type or pointer for complex types
    pub(super) fn determine_union_field_type(
        &self,
        field_types: &[&TypeNode],
    ) -> BasicTypeEnum<'a> {
        if field_types.is_empty() {
            // no fields in this position, use i32 as default
            return self.context.i32_type().into();
        }

        // for simplicity, check if all types are the same
        let first_type = field_types[0];
        let all_same = field_types.iter().all(|t| t.kind == first_type.kind);

        if all_same {
            // all variants use the same type for this field
            // use the same types as llvm_type_from_mux_type for consistency
            match &first_type.kind {
                TypeKind::Primitive(PrimitiveType::Int) => self.context.i64_type().into(),
                TypeKind::Primitive(PrimitiveType::Float) => self.context.f64_type().into(),
                TypeKind::Primitive(PrimitiveType::Bool) => self.context.bool_type().into(),
                TypeKind::Primitive(PrimitiveType::Str) => {
                    self.context.ptr_type(AddressSpace::default()).into()
                }
                _ => self.context.ptr_type(AddressSpace::default()).into(), // default to pointer
            }
        } else {
            // mixed types - use pointer for now (could be improved with proper union types)
            self.context.ptr_type(AddressSpace::default()).into()
        }
    }
}
