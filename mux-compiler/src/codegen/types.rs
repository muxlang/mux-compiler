//! Type conversion between Mux types, LLVM types, and type nodes.
//!
//! This module handles all the various type conversions needed during code generation.

use super::CodeGenerator;
use crate::ast::{PrimitiveType, TypeKind, TypeNode};
use crate::lexer::Span;
use crate::semantics::Type;
use inkwell::AddressSpace;
use inkwell::types::BasicTypeEnum;
use std::collections::HashSet;

/// Placeholder span for synthetically-constructed TypeNodes in codegen.
const SYNTHETIC_SPAN: Span = Span {
    row_start: 0,
    col_start: 0,
    row_end: None,
    col_end: None,
};

impl<'a> CodeGenerator<'a> {
    /// Returns a pointer type, used for heap-allocated values.
    fn ptr_type(&self) -> BasicTypeEnum<'a> {
        self.context.ptr_type(AddressSpace::default()).into()
    }

    /// Resolve a generic type parameter name via the current generic context.
    pub(super) fn resolve_generic_param(&self, name: &str) -> Option<&Type> {
        self.generic_context
            .as_ref()
            .and_then(|ctx| ctx.type_params.get(name))
    }

    /// Create a synthetic TypeNode (no source location) from a TypeKind.
    fn synthetic_type_node(kind: TypeKind) -> TypeNode {
        TypeNode {
            kind,
            span: SYNTHETIC_SPAN,
        }
    }

    pub(super) fn llvm_type_from_resolved_type(
        &self,
        resolved_type: &Type,
    ) -> Result<BasicTypeEnum<'a>, String> {
        match resolved_type {
            Type::Primitive(PrimitiveType::Int) => Ok(self.context.i64_type().into()),
            Type::Primitive(PrimitiveType::Float) => Ok(self.context.f64_type().into()),
            Type::Primitive(PrimitiveType::Bool) => Ok(self.context.bool_type().into()),
            Type::Primitive(PrimitiveType::Str) => Ok(self.ptr_type()),
            Type::Primitive(PrimitiveType::Char) => Ok(self.context.i64_type().into()),
            Type::Primitive(PrimitiveType::Void) | Type::Void => {
                Err("Void type not allowed here".to_string())
            }
            Type::Primitive(PrimitiveType::Auto) => Err("Auto type should be resolved".to_string()),
            Type::Named(name, args) => {
                if args.is_empty()
                    && let Some(concrete) = self.resolve_generic_param(name)
                {
                    return self.llvm_type_from_resolved_type(&concrete.clone());
                }
                if self.classes.contains_key(name) {
                    Ok(self.ptr_type())
                } else {
                    Err(format!("Unknown type: {}", name))
                }
            }
            Type::Function { .. }
            | Type::List(_)
            | Type::Map(_, _)
            | Type::Set(_)
            | Type::Tuple(_, _)
            | Type::Optional(_)
            | Type::Result(_, _)
            | Type::Reference(_)
            | Type::EmptyList
            | Type::EmptyMap
            | Type::EmptySet => Ok(self.ptr_type()),
            Type::Generic(_) => Err("Generic types should be resolved".to_string()),
            Type::Instantiated(_, _) => Err("Instantiated types should be resolved".to_string()),
            Type::Variable(name) => {
                if let Some(concrete) = self.resolve_generic_param(name) {
                    return self.llvm_type_from_resolved_type(&concrete.clone());
                }
                Err(format!("Variable type '{}' should be resolved", name))
            }
            Type::Never => Err("Never type not allowed here".to_string()),
            Type::Module(_) => Err(
                "Module types should not appear in codegen - they are compile-time only"
                    .to_string(),
            ),
        }
    }

    pub(super) fn llvm_type_from_mux_type(
        &self,
        type_node: &TypeNode,
    ) -> Result<BasicTypeEnum<'a>, String> {
        self.type_kind_to_llvm_type(&type_node.kind)
    }

    /// The LLVM type a value of `ty` is stored in directly, or `None` when it
    /// lives behind a `*mut Value`.
    ///
    /// A scalar slot holds the value itself, so it is not reference counted,
    /// needs no box on the way in and no unbox on the way out. `string` is
    /// absent on purpose: it is a primitive to the language but a
    /// reference-counted heap value at runtime, so its slot holds a pointer.
    /// This is the same rule `scalar_field_type` applies to a class field.
    /// `scalar_slot_type` for a named binding, which additionally stays boxed
    /// when its address is taken anywhere in the program.
    ///
    /// A reference has to mean one thing, and a reference to a list element is
    /// the element's `*mut Value`, so a referenced variable has to be boxed too.
    /// Deciding it here, before the slot exists, is what keeps `&x` from having
    /// to convert a live variable mid-body (see `codegen::address_taken`).
    pub(super) fn scalar_slot_for_binding(
        &self,
        name: &str,
        ty: &Type,
    ) -> Option<BasicTypeEnum<'a>> {
        // A captured name is never raw: it is either a cell, or copied into a
        // closure-owned cell as a `*mut Value`. Either way its slot is a
        // pointer.
        if self.address_taken.contains(name) || self.captured_names.contains(name) {
            return None;
        }
        self.scalar_slot_type(ty)
    }

    pub(super) fn scalar_slot_type(&self, ty: &Type) -> Option<BasicTypeEnum<'a>> {
        match ty {
            Type::Primitive(PrimitiveType::Int | PrimitiveType::Char) => {
                Some(self.context.i64_type().into())
            }
            Type::Primitive(PrimitiveType::Float) => Some(self.context.f64_type().into()),
            Type::Primitive(PrimitiveType::Bool) => Some(self.context.bool_type().into()),
            _ => None,
        }
    }

    pub(super) fn semantic_type_to_llvm(
        &self,
        sem_type: &Type,
    ) -> Result<BasicTypeEnum<'a>, String> {
        match sem_type {
            Type::Primitive(prim) => match prim {
                PrimitiveType::Int => Ok(self.context.i64_type().into()),
                PrimitiveType::Float => Ok(self.context.f64_type().into()),
                PrimitiveType::Bool => Ok(self.context.bool_type().into()),
                PrimitiveType::Str => Ok(self.ptr_type()),
                PrimitiveType::Char => Ok(self.context.i64_type().into()),
                PrimitiveType::Void => Err("Void type not allowed in fields".to_string()),
                PrimitiveType::Auto => Err("Auto type should be resolved".to_string()),
            },
            Type::Named(_, _)
            | Type::List(_)
            | Type::Map(_, _)
            | Type::Set(_)
            | Type::Optional(_)
            | Type::Reference(_)
            | Type::Function { .. } => Ok(self.ptr_type()),
            _ => Err(format!(
                "Unsupported type in interface fields: {:?}",
                sem_type
            )),
        }
    }

    pub(super) fn type_kind_to_llvm_type(
        &self,
        type_kind: &TypeKind,
    ) -> Result<BasicTypeEnum<'a>, String> {
        match type_kind {
            TypeKind::Primitive(PrimitiveType::Int) => Ok(self.context.i64_type().into()),
            TypeKind::Primitive(PrimitiveType::Float) => Ok(self.context.f64_type().into()),
            TypeKind::Primitive(PrimitiveType::Bool) => Ok(self.context.bool_type().into()),
            TypeKind::Primitive(PrimitiveType::Str) => Ok(self.ptr_type()),
            TypeKind::Primitive(PrimitiveType::Char) => Ok(self.context.i64_type().into()),
            TypeKind::Named(name, args) => {
                if let Some(concrete) = self.resolve_generic_param(name) {
                    return self.llvm_type_from_resolved_type(&concrete.clone());
                }
                if self.enum_variants.contains_key(name) {
                    if name == "optional" || name == "result" {
                        Ok(self.ptr_type())
                    } else {
                        // A generic enum resolves to its instantiation, so
                        // `Tree<int>` is `Tree$int` and not the differently
                        // shaped uninstantiated `Tree` (issue #359).
                        let arg_types: Vec<Type> =
                            args.iter().map(|a| self.type_node_to_type(a)).collect();
                        let resolved = self.mangled_enum_name(name, &arg_types);
                        let struct_type = self.type_map.get(&resolved).ok_or_else(|| {
                            format!("Enum type {} not found in type map", resolved)
                        })?;
                        Ok(*struct_type)
                    }
                } else {
                    Ok(self.ptr_type())
                }
            }
            TypeKind::List(_)
            | TypeKind::Map(_, _)
            | TypeKind::Set(_)
            | TypeKind::Tuple(_, _)
            | TypeKind::Reference(_)
            | TypeKind::Function { .. }
            | TypeKind::TraitObject(_) => Ok(self.ptr_type()),
            TypeKind::Primitive(PrimitiveType::Void) => {
                Err("Void type cannot be used in enum variant fields".to_string())
            }
            TypeKind::Primitive(PrimitiveType::Auto) | TypeKind::Auto => {
                Err("Auto type should be resolved before codegen".to_string())
            }
        }
    }

    #[allow(clippy::only_used_in_recursion)]
    pub(super) fn type_to_type_node(&self, type_: &Type) -> TypeNode {
        let auto_node = || Self::synthetic_type_node(TypeKind::Auto);
        let kind = match type_ {
            Type::Primitive(p) => TypeKind::Primitive(p.clone()),
            Type::List(inner) => TypeKind::List(Box::new(self.type_to_type_node(inner))),
            Type::Map(k, v) => TypeKind::Map(
                Box::new(self.type_to_type_node(k)),
                Box::new(self.type_to_type_node(v)),
            ),
            Type::Set(inner) => TypeKind::Set(Box::new(self.type_to_type_node(inner))),
            Type::Tuple(l, r) => TypeKind::Tuple(
                Box::new(self.type_to_type_node(l)),
                Box::new(self.type_to_type_node(r)),
            ),
            Type::Optional(inner) => {
                TypeKind::Named("optional".to_string(), vec![self.type_to_type_node(inner)])
            }
            Type::Result(ok, err) => TypeKind::Named(
                "result".to_string(),
                vec![self.type_to_type_node(ok), self.type_to_type_node(err)],
            ),
            Type::Reference(inner) => TypeKind::Reference(Box::new(self.type_to_type_node(inner))),
            Type::Void => TypeKind::Primitive(PrimitiveType::Void),
            Type::EmptyList => TypeKind::List(Box::new(auto_node())),
            Type::EmptyMap => TypeKind::Map(Box::new(auto_node()), Box::new(auto_node())),
            Type::EmptySet => TypeKind::Set(Box::new(auto_node())),
            Type::Function {
                params, returns, ..
            } => TypeKind::Function {
                params: params.iter().map(|p| self.type_to_type_node(p)).collect(),
                returns: Box::new(self.type_to_type_node(returns)),
            },
            Type::Named(name, generics) | Type::Instantiated(name, generics) => TypeKind::Named(
                name.clone(),
                generics.iter().map(|g| self.type_to_type_node(g)).collect(),
            ),
            Type::Generic(name) => TypeKind::Named(name.clone(), vec![]),
            Type::Variable(_) | Type::Never => TypeKind::Auto,
            Type::Module(_) => {
                panic!("Module types should not appear in codegen - they are compile-time only")
            }
        };
        Self::synthetic_type_node(kind)
    }

    pub(super) fn type_node_to_type(&self, type_node: &TypeNode) -> Type {
        match &type_node.kind {
            TypeKind::Primitive(p) => Type::Primitive(p.clone()),
            TypeKind::List(inner) => Type::List(Box::new(self.type_node_to_type(inner))),
            TypeKind::Map(k, v) => Type::Map(
                Box::new(self.type_node_to_type(k)),
                Box::new(self.type_node_to_type(v)),
            ),
            TypeKind::Set(inner) => Type::Set(Box::new(self.type_node_to_type(inner))),
            TypeKind::Tuple(l, r) => Type::Tuple(
                Box::new(self.type_node_to_type(l)),
                Box::new(self.type_node_to_type(r)),
            ),

            TypeKind::TraitObject(_) => Type::Variable("trait_object".to_string()),

            TypeKind::Reference(inner) => Type::Reference(Box::new(self.type_node_to_type(inner))),
            TypeKind::Named(name, generics) => {
                if generics.is_empty() {
                    if let Some(concrete_type) = self.resolve_generic_param(name) {
                        concrete_type.clone()
                    } else {
                        Type::Named(name.clone(), vec![])
                    }
                } else {
                    // special handling for optional and result
                    if name == "optional" && generics.len() == 1 {
                        Type::Optional(Box::new(self.type_node_to_type(&generics[0])))
                    } else if name == "result" && generics.len() == 2 {
                        Type::Result(
                            Box::new(self.type_node_to_type(&generics[0])),
                            Box::new(self.type_node_to_type(&generics[1])),
                        )
                    } else {
                        Type::Named(
                            name.clone(),
                            generics.iter().map(|g| self.type_node_to_type(g)).collect(),
                        )
                    }
                }
            }
            TypeKind::Function { params, returns } => Type::Function {
                params: params.iter().map(|p| self.type_node_to_type(p)).collect(),
                returns: Box::new(self.type_node_to_type(returns)),
                default_count: 0,
            },
            TypeKind::Auto => Type::Variable("auto".to_string()),
        }
    }

    fn resolve_type_with_seen(
        &self,
        type_: &Type,
        seen_generic_params: &mut HashSet<String>,
    ) -> Result<Type, String> {
        match type_ {
            Type::Primitive(_)
            | Type::Void
            | Type::Never
            | Type::EmptyList
            | Type::EmptyMap
            | Type::EmptySet => Ok(type_.clone()),
            Type::Generic(name) | Type::Variable(name) => {
                if seen_generic_params.contains(name) {
                    return Err(format!("Cyclic generic resolution for '{}'", name));
                }
                if let Some(concrete) = self.resolve_generic_param(name) {
                    seen_generic_params.insert(name.clone());
                    let resolved =
                        self.resolve_type_with_seen(&concrete.clone(), seen_generic_params);
                    seen_generic_params.remove(name);
                    return resolved;
                }
                Err(format!("Unresolved generic: {}", name))
            }
            Type::Named(name, type_args) => {
                if type_args.is_empty() {
                    if seen_generic_params.contains(name) {
                        return Err(format!("Cyclic generic resolution for '{}'", name));
                    }
                    if let Some(concrete) = self.resolve_generic_param(name) {
                        seen_generic_params.insert(name.clone());
                        let resolved =
                            self.resolve_type_with_seen(&concrete.clone(), seen_generic_params);
                        seen_generic_params.remove(name);
                        return resolved;
                    }
                    Ok(Type::Named(name.clone(), vec![]))
                } else {
                    let resolved_args = type_args
                        .iter()
                        .map(|arg| self.resolve_type_with_seen(arg, seen_generic_params))
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(Type::Named(name.clone(), resolved_args))
                }
            }
            Type::Instantiated(name, type_args) => {
                let resolved_args = type_args
                    .iter()
                    .map(|arg| self.resolve_type_with_seen(arg, seen_generic_params))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Type::Instantiated(name.clone(), resolved_args))
            }
            Type::List(inner) => Ok(Type::List(Box::new(
                self.resolve_type_with_seen(inner, seen_generic_params)?,
            ))),
            Type::Set(inner) => Ok(Type::Set(Box::new(
                self.resolve_type_with_seen(inner, seen_generic_params)?,
            ))),
            Type::Map(k, v) => Ok(Type::Map(
                Box::new(self.resolve_type_with_seen(k, seen_generic_params)?),
                Box::new(self.resolve_type_with_seen(v, seen_generic_params)?),
            )),
            Type::Tuple(l, r) => Ok(Type::Tuple(
                Box::new(self.resolve_type_with_seen(l, seen_generic_params)?),
                Box::new(self.resolve_type_with_seen(r, seen_generic_params)?),
            )),
            Type::Optional(inner) => Ok(Type::Optional(Box::new(
                self.resolve_type_with_seen(inner, seen_generic_params)?,
            ))),
            Type::Result(ok, err) => Ok(Type::Result(
                Box::new(self.resolve_type_with_seen(ok, seen_generic_params)?),
                Box::new(self.resolve_type_with_seen(err, seen_generic_params)?),
            )),
            Type::Reference(inner) => Ok(Type::Reference(Box::new(
                self.resolve_type_with_seen(inner, seen_generic_params)?,
            ))),
            Type::Function {
                params,
                returns,
                default_count,
                ..
            } => Ok(Type::Function {
                params: params
                    .iter()
                    .map(|p| self.resolve_type_with_seen(p, seen_generic_params))
                    .collect::<Result<Vec<_>, _>>()?,
                returns: Box::new(self.resolve_type_with_seen(returns, seen_generic_params)?),
                default_count: *default_count,
            }),
            Type::Module(_) => Err(
                "Module types should not appear in codegen - they are compile-time only"
                    .to_string(),
            ),
        }
    }

    pub(super) fn resolve_type(&self, type_: &Type) -> Result<Type, String> {
        let mut seen_generic_params = HashSet::new();
        self.resolve_type_with_seen(type_, &mut seen_generic_params)
    }

    /// Shared recursive type traversal helper. Walks all Type variants and recursively
    /// processes containers (Named, List, Map, etc.). Delegates to `handle_var_fn` for
    /// Variable/Generic leaf nodes. This reduces duplication between resolve_type_with_seen
    /// and substitute_type_with_map.
    fn traverse_type_recursive<F>(&self, type_: &Type, handle_var_fn: F) -> Result<Type, String>
    where
        F: Fn(&str) -> Result<Type, String> + Copy,
    {
        match type_ {
            Type::Primitive(_)
            | Type::Void
            | Type::Never
            | Type::EmptyList
            | Type::EmptyMap
            | Type::EmptySet
            | Type::Module(_) => Ok(type_.clone()),
            Type::Variable(name) | Type::Generic(name) => handle_var_fn(name),
            Type::Named(name, type_args) => {
                let resolved_args = type_args
                    .iter()
                    .map(|arg| self.traverse_type_recursive(arg, handle_var_fn))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Type::Named(name.clone(), resolved_args))
            }
            Type::Instantiated(name, type_args) => {
                let resolved_args = type_args
                    .iter()
                    .map(|arg| self.traverse_type_recursive(arg, handle_var_fn))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Type::Instantiated(name.clone(), resolved_args))
            }
            Type::List(inner) => Ok(Type::List(Box::new(
                self.traverse_type_recursive(inner, handle_var_fn)?,
            ))),
            Type::Set(inner) => Ok(Type::Set(Box::new(
                self.traverse_type_recursive(inner, handle_var_fn)?,
            ))),
            Type::Map(k, v) => Ok(Type::Map(
                Box::new(self.traverse_type_recursive(k, handle_var_fn)?),
                Box::new(self.traverse_type_recursive(v, handle_var_fn)?),
            )),
            Type::Tuple(l, r) => Ok(Type::Tuple(
                Box::new(self.traverse_type_recursive(l, handle_var_fn)?),
                Box::new(self.traverse_type_recursive(r, handle_var_fn)?),
            )),
            Type::Optional(inner) => Ok(Type::Optional(Box::new(
                self.traverse_type_recursive(inner, handle_var_fn)?,
            ))),
            Type::Result(ok, err) => Ok(Type::Result(
                Box::new(self.traverse_type_recursive(ok, handle_var_fn)?),
                Box::new(self.traverse_type_recursive(err, handle_var_fn)?),
            )),
            Type::Reference(inner) => Ok(Type::Reference(Box::new(
                self.traverse_type_recursive(inner, handle_var_fn)?,
            ))),
            Type::Function {
                params,
                returns,
                default_count,
                ..
            } => Ok(Type::Function {
                params: params
                    .iter()
                    .map(|p| self.traverse_type_recursive(p, handle_var_fn))
                    .collect::<Result<Vec<_>, _>>()?,
                returns: Box::new(self.traverse_type_recursive(returns, handle_var_fn)?),
                default_count: *default_count,
            }),
        }
    }

    /// Substitute any `Type::Variable`/`Type::Generic` named in `map` with its concrete
    /// type, recursively. Used to resolve a class field's declared type (which still
    /// names the class's own type parameters, e.g. `list<T>`) against a specific
    /// instantiation's type arguments, independent of the codegen generic context.
    pub(super) fn substitute_type_with_map(
        &self,
        type_: &Type,
        map: &std::collections::HashMap<String, Type>,
    ) -> Type {
        self.traverse_type_recursive(type_, |name| {
            Ok(map
                .get(name)
                .cloned()
                .unwrap_or_else(|| Type::Variable(name.to_string())))
        })
        .unwrap_or_else(|_| type_.clone())
    }

    /// A distinct name per type, used to key a generic function's
    /// monomorphised instances (`ensure_generic_function_instantiated`), which
    /// is its only caller.
    ///
    /// It must be injective. Every type outside a handful of cases used to
    /// collapse to "unknown", so a generic instantiated with, say, a tuple and
    /// then an enum produced one function under one name: the first
    /// instantiation defined the signature and the second called it with the
    /// wrong argument type. That surfaced as a failed LLVM verification, but the
    /// same collision between two boxed types would have quietly shared one body
    /// instead (issue #371).
    #[allow(clippy::only_used_in_recursion)]
    pub(super) fn type_to_string(&self, type_: &Type) -> String {
        match type_ {
            Type::Primitive(PrimitiveType::Int) => "int".to_string(),
            Type::Primitive(PrimitiveType::Float) => "float".to_string(),
            Type::Primitive(PrimitiveType::Bool) => "bool".to_string(),
            Type::Primitive(PrimitiveType::Str) => "string".to_string(),
            Type::Primitive(PrimitiveType::Char) => "char".to_string(),
            Type::Primitive(PrimitiveType::Void) | Type::Void => "void".to_string(),
            Type::Primitive(PrimitiveType::Auto) => "auto".to_string(),
            Type::List(inner) => format!("list_{}", self.type_to_string(inner)),
            Type::Set(inner) => format!("set_{}", self.type_to_string(inner)),
            Type::Map(key, value) => format!(
                "map_{}_{}",
                self.type_to_string(key),
                self.type_to_string(value)
            ),
            Type::Tuple(left, right) => format!(
                "tuple_{}_{}",
                self.type_to_string(left),
                self.type_to_string(right)
            ),
            Type::Optional(inner) => format!("optional_{}", self.type_to_string(inner)),
            Type::Result(ok, err) => format!(
                "result_{}_{}",
                self.type_to_string(ok),
                self.type_to_string(err)
            ),
            Type::Reference(inner) => format!("ref_{}", self.type_to_string(inner)),
            Type::EmptyList => "empty_list".to_string(),
            Type::EmptyMap => "empty_map".to_string(),
            Type::EmptySet => "empty_set".to_string(),
            // Classes and enums, and their generic instantiations.
            Type::Named(name, args) | Type::Instantiated(name, args) => {
                if args.is_empty() {
                    name.clone()
                } else {
                    let rendered: Vec<String> =
                        args.iter().map(|a| self.type_to_string(a)).collect();
                    format!("{}_{}", name, rendered.join("_"))
                }
            }
            Type::Function {
                params, returns, ..
            } => {
                let rendered: Vec<String> = params.iter().map(|p| self.type_to_string(p)).collect();
                format!(
                    "fn_{}_to_{}",
                    rendered.join("_"),
                    self.type_to_string(returns)
                )
            }
            // A type parameter reaching here means monomorphisation did not
            // substitute, which is a bug elsewhere; keep the name distinct so
            // two of them do not merge on top of it.
            Type::Generic(name) | Type::Variable(name) => format!("generic_{}", name),
            Type::Never => "never".to_string(),
            Type::Module(name) => format!("module_{}", name),
        }
    }
}
