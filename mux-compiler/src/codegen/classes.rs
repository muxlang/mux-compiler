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
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue};
use std::collections::HashMap;

impl<'a> CodeGenerator<'a> {
    /// The LLVM type of a field the class stores inline as a scalar, or `None`
    /// when the slot holds a `*mut Value`.
    ///
    /// `string` is absent on purpose: it is a primitive to the language but a
    /// heap value at runtime, so the field holds a pointer to it like any other
    /// reference-counted value.
    pub(super) fn scalar_field_type(&self, type_node: &TypeNode) -> Option<BasicTypeEnum<'a>> {
        match type_node.kind {
            TypeKind::Primitive(PrimitiveType::Int | PrimitiveType::Char) => {
                Some(self.context.i64_type().into())
            }
            TypeKind::Primitive(PrimitiveType::Float) => Some(self.context.f64_type().into()),
            TypeKind::Primitive(PrimitiveType::Bool) => Some(self.context.bool_type().into()),
            _ => None,
        }
    }

    fn class_field_llvm_type(
        &self,
        class_type_param_names: &std::collections::HashSet<String>,
        field: &Field,
    ) -> Result<BasicTypeEnum<'a>, String> {
        if let Some(scalar) = self.scalar_field_type(&field.type_) {
            return Ok(scalar);
        }

        let ptr_type = self.context.ptr_type(AddressSpace::default());

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
        // A nested user-enum payload is laid out inline (issue #306), so an enum
        // must be generated after any enum it embeds. Generate enum types in that
        // dependency order first, independent of source order, so a nested enum
        // works regardless of which enum is declared first. Class fields of enum
        // type are likewise stored inline, so classes are generated afterwards
        // once every enum type exists.
        for &idx in &self.enum_generation_order(nodes) {
            if let AstNode::Enum { name, variants, .. } = &nodes[idx] {
                // Retained so a generic enum can be stamped out per
                // instantiation later, when the type arguments are known.
                self.enum_asts.insert(name.clone(), variants.clone());
                // A variant payload may name an instantiation of another
                // generic enum, and this enum's layout needs it to exist first.
                // `enum_generation_order` already puts the embedded enum ahead
                // of this one, so its AST is available to stamp from.
                for variant in variants {
                    for (_, type_node) in variant.data.iter().flatten() {
                        self.instantiate_generic_types_in_type_node(type_node)?;
                    }
                }
                self.generate_enum_type(name, variants)?;
            }
        }
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
                _ => {}
            }
        }
        Ok(())
    }

    /// Indices of the `AstNode::Enum` nodes in `nodes`, ordered so that an enum
    /// that embeds another user enum as an inline payload is generated after that
    /// embedded enum. A cycle (a self- or mutually-embedding enum, which cannot be
    /// laid out inline) is emitted last in source order; those fall back to a
    /// pointer slot and are rejected with a clear error at construction.
    fn enum_generation_order(&self, nodes: &[AstNode]) -> Vec<usize> {
        let enum_indices: Vec<usize> = nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| matches!(n, AstNode::Enum { .. }))
            .map(|(i, _)| i)
            .collect();
        let enum_names: std::collections::HashSet<&str> = enum_indices
            .iter()
            .filter_map(|&i| Self::enum_node_name(&nodes[i]))
            .collect();

        let mut generated: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut order = Vec::with_capacity(enum_indices.len());
        // Repeatedly emit every enum whose embedded-enum dependencies are already
        // emitted, until no more make progress.
        loop {
            let ready: Vec<usize> = enum_indices
                .iter()
                .copied()
                .filter(|&idx| Self::enum_ready(&nodes[idx], &enum_names, &generated))
                .collect();
            if ready.is_empty() {
                break;
            }
            for idx in ready {
                order.push(idx);
                if let Some(name) = Self::enum_node_name(&nodes[idx]) {
                    generated.insert(name);
                }
            }
        }
        // Any enum left over is part of a cycle; append it in source order.
        for &idx in &enum_indices {
            if Self::enum_node_name(&nodes[idx]).is_some_and(|name| !generated.contains(name)) {
                order.push(idx);
            }
        }
        order
    }

    /// The declared name of an `AstNode::Enum`, or `None` for any other node.
    fn enum_node_name(node: &AstNode) -> Option<&str> {
        match node {
            AstNode::Enum { name, .. } => Some(name.as_str()),
            _ => None,
        }
    }

    /// Whether the enum at `node` is not yet generated but all of its embedded
    /// nested-enum dependencies are, so it can be generated now.
    fn enum_ready(
        node: &AstNode,
        enum_names: &std::collections::HashSet<&str>,
        generated: &std::collections::HashSet<&str>,
    ) -> bool {
        let Some(name) = Self::enum_node_name(node) else {
            return false;
        };
        !generated.contains(name)
            && Self::embedded_enum_deps(node, enum_names)
                .iter()
                .all(|dep| generated.contains(dep))
    }

    /// The user-enum names that `node`'s variants embed as inline payloads (a
    /// self-reference is excluded; it is handled as a cycle).
    fn embedded_enum_deps<'n>(
        node: &'n AstNode,
        enum_names: &std::collections::HashSet<&str>,
    ) -> Vec<&'n str> {
        let AstNode::Enum { name, variants, .. } = node else {
            return Vec::new();
        };
        let mut deps = Vec::new();
        for variant in variants {
            for (_, type_node) in variant.data.iter().flatten() {
                if let TypeKind::Named(dep, _) = &type_node.kind
                    && dep != name
                    && enum_names.contains(dep.as_str())
                {
                    deps.push(dep.as_str());
                }
            }
        }
        deps
    }

    pub(super) fn generate_class_type(
        &mut self,
        name: &str,
        fields: &[Field],
        interfaces: &HashMap<String, (Vec<Type>, HashMap<String, MethodSig>)>,
    ) -> Result<(), String> {
        // A field may be the first mention of a generic enum instantiation, and
        // class types are built before any body that could construct one.
        for field in fields {
            self.instantiate_generic_types_in_type_node(&field.type_)?;
        }

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
            field_indices.insert(format!("vtable_{interface_name}"), field_types.len() - 1);
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
            &format!("{name}.copy"),
            fn_type,
            Some(inkwell::module::Linkage::External),
        );
        let destructor_type = void_type.fn_type(&[i8_ptr.into()], false);
        let destructor_fn = self.module.add_function(
            &format!("{name}.destructor"),
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
            .ok_or_else(|| format!("Class {name} not in type map"))?;
        let class_size = class_type
            .size_of()
            .ok_or_else(|| format!("Cannot get size of class {name}"))?;

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

    /// The method each glue wraps, and the suffix it is emitted under.
    ///
    /// `Hashable` needs no glue: its `hash` returns an `int`, which is already
    /// the 64-bit value the runtime wants, so the method is registered as is.
    const CAPABILITY_GLUE: [(&'static str, &'static str); 2] =
        [("eq", "eq_glue"), ("cmp", "cmp_glue")];

    /// Whether the class both declares a capability that needs `method` and
    /// actually defines it.
    ///
    /// Equality is not gated on a literal `is Equatable`: `Comparable` and
    /// `Hashable` grant it too, and `Hashable` requires the class to write
    /// `eq`. Gating on the declaration alone left a `Hashable` class hashing
    /// by field and matching by address, so a key never found its own entry.
    pub(super) fn class_capability_method(
        &self,
        name: &str,
        method: &str,
    ) -> Option<FunctionValue<'a>> {
        // A capability is declared by the class, not by one of its layouts, so
        // `Box$int` has to ask about `Box`.
        let declared_name = Self::declared_class_name(name);
        let declared = match method {
            "eq" => self.analyzer.class_declares_equality_method(declared_name),
            "cmp" => self
                .analyzer
                .type_implements_named_interface(declared_name, "Comparable"),
            _ => self
                .analyzer
                .type_implements_named_interface(declared_name, "Hashable"),
        };
        if !declared {
            return None;
        }
        // A generic class only ever emits monomorphized method bodies, so the
        // unspecialized name is a declaration that never gets one - calling it
        // fails at link time. The operators reach such a class through its
        // instantiation instead; the runtime callbacks, which are registered
        // once per class, have no instantiation to name and are skipped. The
        // body is not emitted yet when this runs, so genericity is the test,
        // not whether the function has one - the same test the vtables use.
        let is_generic = self
            .analyzer
            .all_symbols()
            .get(declared_name)
            .is_some_and(|symbol| !symbol.type_params.is_empty());
        if is_generic {
            return None;
        }
        self.module.get_function(&format!("{name}.{method}"))
    }

    /// Generate the wrappers that let the runtime call a class's `eq` and `cmp`
    /// when it needs to match or order an instance - as a map key, a set member
    /// or a list element.
    ///
    /// Both wrappers exist to narrow the method's return value to what the
    /// runtime expects: `eq`'s `i1` to a byte holding 0 or 1, and `cmp`'s `int`
    /// to -1, 0 or 1. Truncating the `int` instead would turn a difference of
    /// exactly 2^32 into "equal".
    pub(super) fn generate_class_capability_glue(&mut self, name: &str) -> Result<(), String> {
        for (method, suffix) in Self::CAPABILITY_GLUE {
            let Some(target) = self.class_capability_method(name, method) else {
                continue;
            };
            let is_equality = method == "eq";
            let ptr_type = self.context.ptr_type(AddressSpace::default());
            let return_type = if is_equality {
                self.context.i8_type()
            } else {
                self.context.i32_type()
            };
            let glue = self.module.add_function(
                &format!("{name}.{suffix}"),
                return_type.fn_type(&[ptr_type.into(), ptr_type.into()], false),
                Some(inkwell::module::Linkage::External),
            );
            let entry = self.context.append_basic_block(glue, "entry");
            self.builder.position_at_end(entry);
            let left = glue.get_nth_param(0).expect("glue takes two objects");
            let right = glue.get_nth_param(1).expect("glue takes two objects");
            let result = self
                .builder
                .build_call(target, &[left.into(), right.into()], "capability_call")
                .map_err(|e| e.to_string())?
                .try_as_basic_value()
                .basic()
                .ok_or_else(|| format!("{name}.{method} should return a value"))?
                .into_int_value();
            let narrowed = if is_equality {
                self.builder
                    .build_int_z_extend(result, return_type, "equal")
                    .map_err(|e| e.to_string())?
            } else {
                self.narrow_ordering_to_sign(result, return_type)?
            };
            self.builder
                .build_return(Some(&narrowed))
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Reduce a `cmp` result to -1, 0 or 1 as `(value > 0) - (value < 0)`.
    fn narrow_ordering_to_sign(
        &mut self,
        value: IntValue<'a>,
        result_type: inkwell::types::IntType<'a>,
    ) -> Result<IntValue<'a>, String> {
        let zero = value.get_type().const_zero();
        let greater = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGT, value, zero, "greater")
            .map_err(|e| e.to_string())?;
        let less = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, value, zero, "less")
            .map_err(|e| e.to_string())?;
        let greater = self
            .builder
            .build_int_z_extend(greater, result_type, "greater_bit")
            .map_err(|e| e.to_string())?;
        let less = self
            .builder
            .build_int_z_extend(less, result_type, "less_bit")
            .map_err(|e| e.to_string())?;
        self.builder
            .build_int_sub(greater, less, "sign")
            .map_err(|e| e.to_string())
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
            let llvm_field_index = Self::class_field_llvm_index(field_index)?;
            // Inline fields were bulk-copied above. A plain scalar is fully
            // duplicated by that memcpy, but an inline enum field's copy still
            // aliases the source's RC payloads (a string, a nested or boxed
            // recursive enum), so deep-clone them in place to make the copy
            // independent; the memcpy alone would double-free.
            if !Self::class_field_is_boxed_pointer(class_type, llvm_field_index) {
                if let Some(enum_name) = self.nested_user_enum_name(&field.type_) {
                    let field_ptr = self
                        .builder
                        .build_struct_gep(class_type, dst_typed, llvm_field_index, &field.name)
                        .map_err(|e| e.to_string())?;
                    self.emit_enum_deep_clone(&enum_name, field_ptr)?;
                }
                continue;
            }
            let field_ptr = self
                .builder
                .build_struct_gep(class_type, dst_typed, llvm_field_index, &field.name)
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
            let llvm_field_index = Self::class_field_llvm_index(field_index)?;
            // Fields stored inline (e.g. an enum held as a struct) are not boxed
            // `*mut Value` pointers; loading their first word and decrementing it
            // as a refcount would corrupt memory. An inline enum field still owns
            // its active variant's RC payloads (a string, a nested or boxed
            // recursive enum), so release those with the enum drop glue; other
            // inline scalars own nothing and are skipped.
            if !Self::class_field_is_boxed_pointer(class_type, llvm_field_index) {
                if let Some(enum_name) = self.nested_user_enum_name(&field.type_) {
                    let field_ptr = self
                        .builder
                        .build_struct_gep(class_type, obj_typed, llvm_field_index, &field.name)
                        .map_err(|e| e.to_string())?;
                    self.emit_enum_drop(&enum_name, field_ptr)?;
                }
                continue;
            }
            let field_ptr = self
                .builder
                .build_struct_gep(class_type, obj_typed, llvm_field_index, &field.name)
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
    fn class_field_llvm_index(field_index: usize) -> Result<u32, String> {
        u32::try_from(field_index)
            .map_err(|_| format!("class field index {field_index} exceeds LLVM's u32 limit"))
    }

    fn class_field_is_boxed_pointer(class_type: BasicTypeEnum<'a>, field_index: u32) -> bool {
        matches!(
            class_type
                .into_struct_type()
                .get_field_type_at_index(field_index),
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
            // The built-in capabilities are not declared interfaces, so no
            // vtable type was ever registered for them. Skipped for the same
            // reason as a generic class above: the vtable field is never read,
            // since interfaces dispatch statically.
            if !self.vtable_type_map.contains_key(interface_name)
                && matches!(
                    interface_name.as_str(),
                    "Stringable" | "Equatable" | "Comparable" | "Hashable" | "Error"
                )
            {
                continue;
            }
            let mut vtable_values = Vec::new();
            for method_name in interface_methods.keys() {
                let class_method_name = format!("{class_name}.{method_name}");
                let func = self.functions.get(&class_method_name).ok_or_else(|| {
                    format!(
                        "Class {class_name} does not implement method {method_name} for interface {interface_name}"
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
            let vtable_name = format!("{class_name}_{interface_name}_vtable");
            let global =
                self.module
                    .add_global(vtable_type.as_basic_type_enum(), None, &vtable_name);
            global.set_initializer(&vtable_const);
            self.vtable_map.insert(
                format!("{class_name}_{interface_name}"),
                global.as_pointer_value(),
            );
        }
        Ok(())
    }

    pub(super) fn generate_interface_type(&mut self, name: &str) -> Result<(), String> {
        // generate LLVM struct for interface: { *mut vtable, field1, field2, ... }
        // for simplicity, vtable is struct of void* function pointers
        // A class can name an interface that is not in scope. Importing a type
        // now brings its interfaces with it, and semantics reports what the
        // module cannot supply before codegen runs (#391), so reaching here
        // means both of those missed something.
        //
        // It stays a message the user can act on rather than an assertion. The
        // failure it guards is a missing import, which is theirs to fix, and
        // reporting it as a compiler crash sends them to file a bug about their
        // own program - which is exactly what #391 was.
        let symbol = self.analyzer.all_symbols().get(name).ok_or_else(|| {
            format!(
                "interface '{name}' is implemented by a type in use here but was never imported.\n\
                 note: importing a type does not import the interfaces it implements\n\
                 help: add an import for '{name}' - for a standard library interface that is \
                 usually `import std.<module>.<submodule>.{name}`, e.g. \
                 `import std.dsa.collection.Collection`"
            )
        })?;
        let (_, interface_methods) = symbol
            .interfaces
            .get(name)
            .ok_or_else(|| format!("Interface methods for '{name}' not found"))?;

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
                            format!("Variant {variant_name} not found in enum {enum_name}")
                        })
                } else {
                    Err(format!("Enum {enum_name} not found"))
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
                    .ok_or(format!("{discriminant_func} not found"))?;

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
                    .ok_or_else(|| format!("Enum {enum_name} not found in type map"))?
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
                .map(std::vec::Vec::len)
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

    /// Determine the LLVM type for a union field position (one payload slot shared
    /// across variants).
    ///
    /// A position that includes a nested user enum is stored inline (issue #306),
    /// but different variants may put differently-sized payloads there (a nested
    /// enum in one variant, a scalar or another enum in another - issue #309). The
    /// slot must fit them all, so it takes the widest candidate type; each variant
    /// addresses the slot with its own type, so the slot's identity does not matter
    /// beyond its size and alignment. A recursive enum cannot be sized inline and
    /// falls back to a pointer, which construction rejects with a clear error.
    ///
    /// Positions with no nested enum keep the historical single-type-or-pointer
    /// behavior, so ordinary scalar/pointer enums are laid out exactly as before.
    pub(super) fn determine_union_field_type(
        &self,
        field_types: &[&TypeNode],
    ) -> BasicTypeEnum<'a> {
        let ptr_slot = || self.context.ptr_type(AddressSpace::default()).into();
        let Some(first_type) = field_types.first() else {
            // no fields in this position, use i32 as default
            return self.context.i32_type().into();
        };

        if field_types
            .iter()
            .any(|t| self.nested_user_enum_name(t).is_some())
        {
            return self
                .widest_payload_type(field_types)
                .unwrap_or_else(ptr_slot);
        }

        // No nested enum: keep the original single-type-or-pointer layout.
        let all_same = field_types.iter().all(|t| t.kind == first_type.kind);
        if all_same {
            match &first_type.kind {
                TypeKind::Primitive(PrimitiveType::Int) => self.context.i64_type().into(),
                TypeKind::Primitive(PrimitiveType::Float) => self.context.f64_type().into(),
                TypeKind::Primitive(PrimitiveType::Bool) => self.context.bool_type().into(),
                _ => ptr_slot(),
            }
        } else {
            ptr_slot()
        }
    }

    /// A union slot wide enough for every variant's payload at a position, or
    /// `None` if any candidate cannot be laid out (a recursive enum whose struct
    /// is not yet registered).
    ///
    /// The slot is an `i64` array sized to the widest candidate, not the widest
    /// candidate type itself: a struct-typed slot has interior padding, and a
    /// variant whose (differently-typed) payload straddles that padding would be
    /// corrupted when the enum is copied field-by-field, because LLVM's aggregate
    /// load/store does not preserve padding bytes. An `i64` array has no padding
    /// and is 8-byte aligned, so any variant's payload survives a copy verbatim;
    /// each variant still reads and writes the slot through its own type.
    fn widest_payload_type(&self, field_types: &[&TypeNode]) -> Option<BasicTypeEnum<'a>> {
        let mut max_size = 0u64;
        for type_node in field_types {
            let candidate = self.type_kind_to_llvm_type(&type_node.kind).ok()?;
            max_size = max_size.max(self.abi_store_size(&candidate));
        }
        let words = max_size.div_ceil(8).max(1);
        let words = u32::try_from(words).ok()?;
        Some(self.context.i64_type().array_type(words).into())
    }
}

#[cfg(test)]
mod tests {
    use super::super::llvm_index;
    use super::CodeGenerator;

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn class_field_index_accepts_the_largest_representable_index() {
        let largest = usize::try_from(u32::MAX).expect("u32 fits in a 64-bit usize");
        assert_eq!(CodeGenerator::class_field_llvm_index(largest), Ok(u32::MAX));
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn class_field_index_reports_unrepresentable_indices() {
        let unrepresentable = usize::try_from(u32::MAX).expect("u32 fits in a 64-bit usize") + 1;
        let error = CodeGenerator::class_field_llvm_index(unrepresentable)
            .expect_err("an index wider than LLVM's API must be rejected");
        assert!(error.contains("exceeds LLVM's u32 limit"));
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn llvm_index_accepts_the_largest_representable_index() {
        let largest = usize::try_from(u32::MAX).expect("u32 fits in a 64-bit usize");
        assert_eq!(llvm_index(largest), u32::MAX);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    #[should_panic(expected = "LLVM index exceeds u32::MAX")]
    fn llvm_index_rejects_unrepresentable_indices() {
        let unrepresentable = usize::try_from(u32::MAX).expect("u32 fits in a 64-bit usize") + 1;
        llvm_index(unrepresentable);
    }
}
