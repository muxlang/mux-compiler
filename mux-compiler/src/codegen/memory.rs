//! Reference counting (RC) scope management for CodeGenerator.
//!
//! This module handles tracking RC-allocated variables and generating cleanup code.

use super::{CodeGenerator, RcSlot};
use crate::semantics::Type;
use inkwell::AddressSpace;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, PointerValue};

/// Which per-payload operation `emit_enum_payload_op` performs on each active
/// variant pointer field of an inline enum value.
#[derive(Clone, Copy)]
enum EnumPayloadOp {
    /// Increment the payload's refcount, so a new owner shares it.
    Retain,
    /// Decrement the payload's refcount, releasing the value.
    Drop,
    /// Replace the payload with an independent `mux_value_deep_clone` of itself.
    DeepClone,
}

/// How a variant payload field participates in reference-counting glue.
enum FieldGlue {
    /// A direct pointer payload (string/list/object/closure): apply the op to
    /// the loaded pointer with a single runtime call.
    Pointer,
    /// An inline nested user enum: recurse into the inner enum so its own active
    /// variant's payloads get the same operation.
    InlineEnum(String),
    /// A heap-boxed recursive nested enum (issue #309): unbox and recurse
    /// through the box, then release (drop) or duplicate (clone) the box itself.
    BoxedEnum(String),
}

/// How a variant payload field is compared for structural ordering (issue
/// #309). Unlike `FieldGlue` this covers every field, including inline scalars.
/// FNV-1a constants, used to combine an enum's discriminant and payload field
/// hashes into one value.
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Every NaN hashes to this, because the comparison glue reports two NaNs as
/// equal while their bit patterns can differ.
const NAN_HASH: u64 = 0x7fff_ffff_ffff_ffff;

enum CompareKind<'a> {
    /// An inline scalar (int/bool/char/float): compared directly on its bits.
    Scalar(BasicTypeEnum<'a>),
    /// A pointer payload (string/list/object/...): compared with `mux_value_compare`.
    Pointer,
    /// An inline nested user enum: recurse into the inner enum's compare glue.
    InlineEnum(String),
    /// A heap-boxed recursive nested enum: unbox both sides, then recurse.
    BoxedEnum(String),
}

impl EnumPayloadOp {
    /// The runtime function applied to each pointer payload.
    fn runtime_fn_name(self) -> &'static str {
        match self {
            EnumPayloadOp::Retain => "mux_rc_inc",
            EnumPayloadOp::Drop => "mux_rc_dec",
            EnumPayloadOp::DeepClone => "mux_value_deep_clone",
        }
    }

    /// Short slug used in generated basic-block names.
    fn label(self) -> &'static str {
        match self {
            EnumPayloadOp::Retain => "retain",
            EnumPayloadOp::Drop => "drop",
            EnumPayloadOp::DeepClone => "clone",
        }
    }
}

impl<'a> CodeGenerator<'a> {
    /// Whether the builder's current block can still receive instructions, i.e.
    /// it exists and has no terminator yet. Emitting into a terminated block
    /// (e.g. an `unreachable` match-end block after every arm returned) produces
    /// invalid IR, so cleanup that runs on the fall-through path must guard on
    /// this.
    pub(super) fn current_block_is_live(&self) -> bool {
        self.builder
            .get_insert_block()
            .is_some_and(|bb| bb.get_terminator().is_none())
    }

    /// Push a new RC scope onto the stack. Call this when entering a new scope
    /// (function, if/else block, loop body, match arm, etc.)
    pub(super) fn push_rc_scope(&mut self) {
        self.rc_scope_stack.push(Vec::new());
        // Closure-typed variables are tracked in a parallel per-scope stack that
        // is pushed/popped in lock-step with the RC scope stack.
        self.closure_scope_stack.push(Vec::new());
    }
    /// Generate cleanup code for all scopes (used before return statements).
    /// This doesn't pop the scopes - just generates the cleanup code.
    ///
    /// Every caller is a return/terminator path, and on value-returning paths
    /// the returned value has already been retained with `mux_rc_inc`. So we
    /// also decrement any pending statement temporaries here: an unbound
    /// temporary is freed, and a temporary that happens to be the return value
    /// is brought back down to the caller-owned +1 by the earlier retain.
    pub(super) fn generate_all_scopes_cleanup(&mut self) -> Result<(), String> {
        self.cleanup_all_temps()?;
        self.cleanup_all_closure_temps()?;

        // Collect all variables from all scopes to avoid borrow issues
        let all_vars: Vec<(String, RcSlot<'a>)> = self
            .rc_scope_stack
            .iter()
            .rev()
            .flat_map(|scope| scope.iter().cloned())
            .collect();

        self.generate_cleanup_for_vars(&all_vars)?;

        let all_closures: Vec<(String, PointerValue<'a>)> = self
            .closure_scope_stack
            .iter()
            .rev()
            .flat_map(|scope| scope.iter().cloned())
            .collect();
        self.generate_closure_cleanup_for_vars(&all_closures)
    }

    /// Release a list of tracked scope slots. Boxed `*mut Value` slots are
    /// decremented directly; inline enum-struct slots are released with
    /// `emit_enum_drop`, which decrements only the pointer payloads of the
    /// value's active variant.
    pub(super) fn generate_cleanup_for_vars(
        &mut self,
        vars: &[(String, RcSlot<'a>)],
    ) -> Result<(), String> {
        let rc_dec = self
            .runtime_function("mux_rc_dec")
            .ok_or("mux_rc_dec not found")?;
        let ptr_type = self.context.ptr_type(AddressSpace::default());

        for (name, slot) in vars {
            match slot {
                RcSlot::Boxed(alloca) => {
                    let value = self
                        .builder
                        .build_load(ptr_type, *alloca, &format!("rc_load_{}", name))
                        .map_err(|e| e.to_string())?;
                    self.builder
                        .build_call(rc_dec, &[value.into()], &format!("rc_dec_{}", name))
                        .map_err(|e| e.to_string())?;
                }
                RcSlot::Cell(cell) => {
                    let release = self
                        .runtime_function("mux_cell_release")
                        .ok_or("mux_cell_release not found")?;
                    self.builder
                        .build_call(
                            release,
                            &[(*cell).into()],
                            &format!("cell_release_{}", name),
                        )
                        .map_err(|e| e.to_string())?;
                }
                RcSlot::EnumStruct { enum_name, alloca } => {
                    self.emit_enum_drop(enum_name, *alloca)?;
                }
            }
        }
        Ok(())
    }

    /// Add a cleanup slot to the current scope, skipping it if the same storage
    /// slot is already tracked there - a duplicate would be decremented twice at
    /// cleanup. Shared by the boxed and enum tracking entry points.
    fn track_slot(&mut self, name: &str, slot: RcSlot<'a>) {
        if let Some(current_scope) = self.rc_scope_stack.last_mut() {
            if current_scope
                .iter()
                .any(|(_, s)| s.alloca() == slot.alloca())
            {
                return;
            }
            current_scope.push((name.to_string(), slot));
        }
    }

    /// Track an RC-allocated variable in the current scope.
    /// The variable will have mux_rc_dec called on it when the scope ends.
    pub(super) fn track_rc_variable(&mut self, name: &str, alloca: PointerValue<'a>) {
        self.track_slot(name, RcSlot::Boxed(alloca));
    }

    /// Track an inline enum-struct local so its active variant's pointer payloads
    /// are released when the scope ends. Enums are value-semantic structs, not
    /// boxed `*mut Value`s, so they need `emit_enum_drop` rather than a plain
    /// `mux_rc_dec` - see `RcSlot`.
    pub(super) fn track_enum_variable(
        &mut self,
        name: &str,
        enum_name: &str,
        alloca: PointerValue<'a>,
    ) {
        self.track_slot(
            name,
            RcSlot::EnumStruct {
                enum_name: enum_name.to_string(),
                alloca,
            },
        );
    }

    /// Whether an assignment/binding right-hand side produces a freshly-owned
    /// enum value - one whose pointer payloads have already been retained and
    /// whose ownership transfers into the slot. An owned value is stored
    /// directly; a borrowed one (an identifier or field load, which still points
    /// at its source's payloads) is deep-cloned first so the binding owns an
    /// independent value (`store_struct_value`, issue #298).
    ///
    /// A call (a variant constructor, or a function/method that returns an enum
    /// by value) owns its result. A ternary is also owned: `generate_if_expression`
    /// normalizes each arm to an owned value (deep-cloning any borrowed arm) so
    /// the result is uniformly owned, which avoids leaking the owned arm of a
    /// mixed-ownership ternary (issue #298 review). Every other form (identifier,
    /// field or index load) is treated as borrowed and deep-cloned, which is
    /// always safe.
    ///
    /// `FieldAccess` is deliberately in that borrowed group even though a bare
    /// variant (`Color.Red`, mux-context#39) is a construction and therefore
    /// owned. Treating it as borrowed costs a deep clone that does nothing: a
    /// variant written without parentheses has no payload by definition, so
    /// `emit_enum_payload_op` skips it as scalar-only. Widening this to include
    /// FieldAccess would be an optimization, not a fix, and would need to
    /// distinguish a variant construction from an ordinary field load first.
    pub(super) fn rhs_produces_owned_enum(kind: &crate::ast::ExpressionKind) -> bool {
        matches!(
            kind,
            crate::ast::ExpressionKind::Call { .. } | crate::ast::ExpressionKind::If { .. }
        )
    }

    /// If `ty` is a user-declared enum (not the built-in `optional`/`result`,
    /// which are boxed `*mut Value`s rather than inline structs), return its
    /// name. Used to decide whether a struct-valued local needs enum drop-glue.
    pub(super) fn user_enum_type_name(&self, ty: &Type) -> Option<String> {
        let Type::Named(name, args) = ty else {
            return None;
        };
        if name == "optional" || name == "result" {
            return None;
        }
        // A generic enum answers under its instantiation name, so callers get
        // `Box$int` and reach the stamped-out struct and glue (issue #359).
        self.enum_variants
            .contains_key(name)
            .then(|| self.mangled_enum_name(name, args))
    }

    /// Convert a user-enum value to the inline struct every consumer expects.
    ///
    /// A user enum is normally an inline `{ i32 tag, fields... }` struct. When
    /// one arrives as a POINTER it is boxed - a managed BoxedEnum, or an Opaque
    /// read out of a collection (`items[0]`, `m[k]`) - and the struct has to be
    /// loaded back out before anything can use it.
    ///
    /// Matching and comparison already did this, via `generate_enum_match` and
    /// `enum_struct_pointer`. The paths that did not are why `match items[0]`
    /// worked while passing the same element to a function raised an internal
    /// error, and assigning it to a class field compiled and then segfaulted
    /// (issue #363). Anything consuming an enum by value must go through here.
    ///
    /// `optional`/`result` are excluded by `user_enum_type_name`: they are heap
    /// values by design and their own runtime calls operate on the pointer.
    pub(super) fn coerce_boxed_enum_to_inline(
        &mut self,
        value: BasicValueEnum<'a>,
        ty: &Type,
    ) -> Result<BasicValueEnum<'a>, String> {
        if !value.is_pointer_value() {
            return Ok(value);
        }
        let Some(enum_name) = self.user_enum_type_name(ty) else {
            return Ok(value);
        };
        self.unbox_enum_subject_value(&enum_name, value.into_pointer_value())
    }

    /// If the enum-variant field `type_node` denotes a user-declared enum stored
    /// inline (not the built-in `optional`/`result`, which are boxed `*mut Value`),
    /// return that enum's name. A nested user enum is laid out inline in the
    /// containing enum's union slot, so its own payloads need recursive
    /// drop/clone/retain glue.
    pub(super) fn nested_user_enum_name(&self, type_node: &crate::ast::TypeNode) -> Option<String> {
        let crate::ast::TypeKind::Named(name, _) = &type_node.kind else {
            return None;
        };
        if name == "optional" || name == "result" {
            return None;
        }
        if !self.enum_variants.contains_key(name) {
            return None;
        }
        // A nested generic payload names the instantiation, so `Tree<int>`
        // inside `Tree$int` embeds `Tree$int` and not the uninstantiated
        // `Tree`, whose layout is a different shape (issue #359).
        let args = self.type_node_args_as_types(type_node);
        Some(self.mangled_enum_name(name, &args))
    }

    /// The type arguments of a named `TypeNode`, resolved to semantic types.
    /// Empty for a non-generic reference.
    fn type_node_args_as_types(&self, type_node: &crate::ast::TypeNode) -> Vec<Type> {
        let crate::ast::TypeKind::Named(_, args) = &type_node.kind else {
            return Vec::new();
        };
        args.iter().map(|arg| self.type_node_to_type(arg)).collect()
    }

    /// Whether `from` embeds `target` as a nested user-enum payload, directly or
    /// transitively. Used to tell a recursive enum (no finite inline layout) from
    /// a merely heterogeneous union position when reporting a layout error.
    pub(super) fn enum_embeds(&self, from: &str, target: &str) -> bool {
        self.enum_embeds_rec(from, target, &mut std::collections::HashSet::new())
    }

    fn enum_embeds_rec(
        &self,
        from: &str,
        target: &str,
        visited: &mut std::collections::HashSet<String>,
    ) -> bool {
        if !visited.insert(from.to_string()) {
            return false;
        }
        let Some(variant_fields) = self.enum_variant_fields.get(from) else {
            return false;
        };
        variant_fields.values().flatten().any(|(_, type_node)| {
            self.nested_user_enum_name(type_node).is_some_and(|inner| {
                inner == target || self.enum_embeds_rec(&inner, target, visited)
            })
        })
    }

    /// If the `field_index`-th payload of `enum_name`'s `variant` is a nested
    /// user enum whose struct is too large for the union slot it was assigned,
    /// return the inner enum's name. That happens only for a recursive or
    /// mutually-referential enum, whose slot falls back to a bare pointer: the
    /// payload is stored as a heap-boxed `Value::Opaque` (issue #309) rather than
    /// inline, so it needs box/unbox glue. Returns `None` for an inline nested
    /// enum, a scalar, or a plain pointer payload.
    pub(super) fn boxed_recursive_field(
        &self,
        enum_name: &str,
        variant: &str,
        field_index: usize,
    ) -> Option<String> {
        let fields = self.variant_field_types(enum_name, variant).ok()?;
        let inner = self.nested_user_enum_name(&fields.get(field_index)?.1)?;
        let slot = match self.type_map.get(enum_name) {
            Some(BasicTypeEnum::StructType(st)) => {
                st.get_field_type_at_index((field_index + 1) as u32)?
            }
            _ => return None,
        };
        let inner_ty = self.type_map.get(&inner).copied()?;
        (self.abi_store_size(&slot) < self.abi_store_size(&inner_ty)).then_some(inner)
    }

    /// Whether `enum_name` is part of an embedding cycle (embeds itself directly
    /// or transitively). Such an enum cannot have its drop/clone glue
    /// inline-expanded without looping forever at compile time, so it gets an
    /// out-of-line, memoized glue function that recurses at runtime instead
    /// (issue #309).
    pub(super) fn enum_is_recursive(&self, enum_name: &str) -> bool {
        self.enum_embeds(enum_name, enum_name)
    }

    /// Whether any variant of `enum_name` carries at least one pointer (RC)
    /// payload field - either directly, or nested inside an inline user-enum
    /// payload. Enums that only hold inline scalars (a plain C-style enum) own
    /// nothing and need no drop-glue, so tracking and `emit_enum_drop` skip them
    /// entirely.
    pub(super) fn enum_has_rc_payload(&self, enum_name: &str) -> bool {
        self.enum_has_rc_payload_rec(enum_name, &mut std::collections::HashSet::new())
    }

    /// `enum_has_rc_payload` with cycle protection: `visited` breaks the
    /// recursion on a self-referential enum (which cannot be laid out inline and
    /// is rejected at construction) instead of looping forever.
    fn enum_has_rc_payload_rec(
        &self,
        enum_name: &str,
        visited: &mut std::collections::HashSet<String>,
    ) -> bool {
        if !visited.insert(enum_name.to_string()) {
            return false;
        }
        let Some(variants) = self.enum_variants.get(enum_name).cloned() else {
            return false;
        };
        variants.iter().any(|variant| {
            self.variant_field_types(enum_name, variant)
                .map(|fields| {
                    fields.iter().enumerate().any(|(i, field)| {
                        if let Some(inner) = self.nested_user_enum_name(&field.1) {
                            // A boxed recursive field owns a heap `Value::Opaque`
                            // box that must be released, so it is itself an RC
                            // payload regardless of what the inner enum carries.
                            self.boxed_recursive_field(enum_name, variant, i).is_some()
                                || self.enum_has_rc_payload_rec(&inner, visited)
                        } else {
                            self.variant_field_llvm_type(enum_name, variant, &fields, i)
                                .map(|t| t.is_pointer_type())
                                .unwrap_or(false)
                        }
                    })
                })
                .unwrap_or(false)
        })
    }

    /// Store a struct `value` into `slot`, releasing the slot's previous
    /// occupant when `release_old` is set. For a payload-bearing user enum this
    /// applies value semantics: an owned right-hand side (a constructor or call
    /// result, `rhs_owned`) transfers its payloads directly, while a borrowed
    /// copy (an identifier or field load, e.g. `auto t = s`) is deep-cloned so
    /// the binding owns an independent value (issue #298). The independent new
    /// value is produced before the old one is released, so a self-assignment
    /// (`t = t`) clones the payloads before they are freed (issue #290).
    ///
    /// Any other struct (a scalar-only enum, the built-in optional/result, a
    /// tuple) owns nothing inline here and is stored directly. `release_old`
    /// must be false only when the slot has no valid prior value to release (a
    /// fresh, non-zero-initialized binding).
    pub(super) fn store_struct_value(
        &mut self,
        slot: PointerValue<'a>,
        value: BasicValueEnum<'a>,
        resolved_type: &Type,
        rhs_owned: bool,
        release_old: bool,
    ) -> Result<(), String> {
        let enum_name = self
            .user_enum_type_name(resolved_type)
            .filter(|name| self.enum_has_rc_payload(name));
        let to_store = if let Some(enum_name) = &enum_name {
            let owned = self.materialize_owned_enum(value, enum_name, rhs_owned)?;
            // Ownership of an owned RHS transfers into this slot, so it must no
            // longer be released as a statement temporary. (A borrowed RHS was
            // never tracked, so this is a no-op there.)
            self.untrack_enum_temp(value);
            if release_old {
                // The slot holds a valid enum or is zero-initialized; emit_enum_drop
                // is null-safe on the zeroed case (the first loop pass / a skipped
                // conditional).
                self.emit_enum_drop(enum_name, slot)?;
            }
            owned
        } else {
            value
        };
        self.builder
            .build_store(slot, to_store)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Produce an owned enum value from `value`: an already-owned right-hand side
    /// is returned unchanged, while a borrowed copy is deep-cloned so the caller
    /// can store an independent value (issue #298). The clone spills the value to
    /// a temporary, deep-clones the active variant's pointer payloads in place
    /// (leaving the source untouched), and reloads it. A no-op for an owned value
    /// or an enum with no pointer payloads.
    pub(super) fn materialize_owned_enum(
        &mut self,
        value: BasicValueEnum<'a>,
        enum_name: &str,
        rhs_owned: bool,
    ) -> Result<BasicValueEnum<'a>, String> {
        if rhs_owned || !self.enum_has_rc_payload(enum_name) {
            return Ok(value);
        }
        let struct_type = match self.type_map.get(enum_name) {
            Some(BasicTypeEnum::StructType(struct_type)) => *struct_type,
            Some(_) => {
                return Err(format!(
                    "Enum {} type map entry is not a struct type",
                    enum_name
                ));
            }
            None => return Err(format!("Enum {} not found in type map", enum_name)),
        };
        let temp = self.spill_enum_to_temp(value, enum_name, "enum_copy_src")?;
        self.emit_enum_deep_clone(enum_name, temp)?;
        let owned = self
            .builder
            .build_load(struct_type, temp, "enum_copy")
            .map_err(|e| e.to_string())?;
        Ok(owned)
    }

    /// Spill an inline enum struct `value` into a fresh entry-block alloca so its
    /// payloads can be reached by GEP (for `emit_enum_drop` / `emit_enum_deep_clone`,
    /// which operate on a slot pointer rather than an SSA struct value).
    pub(super) fn spill_enum_to_temp(
        &mut self,
        value: BasicValueEnum<'a>,
        enum_name: &str,
        label: &str,
    ) -> Result<PointerValue<'a>, String> {
        let struct_type = match self.type_map.get(enum_name) {
            Some(BasicTypeEnum::StructType(struct_type)) => *struct_type,
            Some(_) => {
                return Err(format!(
                    "Enum {} type map entry is not a struct type",
                    enum_name
                ));
            }
            None => return Err(format!("Enum {} not found in type map", enum_name)),
        };
        let temp = self.create_entry_alloca(struct_type.into(), label)?;
        self.builder
            .build_store(temp, value)
            .map_err(|e| e.to_string())?;
        Ok(temp)
    }

    /// Release an inline enum value held at `struct_alloca` by decrementing only
    /// the pointer payloads of its active variant (see `emit_enum_payload_op` for
    /// why the variant is selected at runtime). No-op for enums with no pointer
    /// payloads.
    pub(super) fn emit_enum_drop(
        &mut self,
        enum_name: &str,
        struct_alloca: PointerValue<'a>,
    ) -> Result<(), String> {
        self.emit_enum_payload_op(enum_name, struct_alloca, EnumPayloadOp::Drop)
    }

    /// Deep-clone the pointer payloads of the enum value at `struct_alloca` in
    /// place, so a copy binding owns an independent value instead of aliasing the
    /// source (issue #298). Each active-variant pointer payload is replaced with
    /// a fresh `mux_value_deep_clone` of itself; the source is left untouched.
    pub(super) fn emit_enum_deep_clone(
        &mut self,
        enum_name: &str,
        struct_alloca: PointerValue<'a>,
    ) -> Result<(), String> {
        self.emit_enum_payload_op(enum_name, struct_alloca, EnumPayloadOp::DeepClone)
    }

    /// Retain (bump the refcount of) the pointer payloads of the enum value at
    /// `struct_alloca`. Used when a constructor stores an inline user-enum payload
    /// into a containing enum: the containing enum needs its own +1 on the nested
    /// value's payloads, mirroring `rc_inc_if_pointer` for a direct pointer
    /// payload (issue #306).
    pub(super) fn emit_enum_retain(
        &mut self,
        enum_name: &str,
        struct_alloca: PointerValue<'a>,
    ) -> Result<(), String> {
        self.emit_enum_payload_op(enum_name, struct_alloca, EnumPayloadOp::Retain)
    }

    /// Classify the payload fields of `variant` that need RC glue. Fields that
    /// carry only inline scalars produce nothing.
    fn variant_glue_fields(
        &self,
        enum_name: &str,
        variant: &str,
    ) -> Result<Vec<(usize, FieldGlue)>, String> {
        let fields = self.variant_field_types(enum_name, variant)?;
        let mut glue = Vec::new();
        for (i, field) in fields.iter().enumerate() {
            if let Some(inner) = self.nested_user_enum_name(&field.1) {
                if self.boxed_recursive_field(enum_name, variant, i).is_some() {
                    glue.push((i, FieldGlue::BoxedEnum(inner)));
                } else if self.enum_has_rc_payload(&inner) {
                    glue.push((i, FieldGlue::InlineEnum(inner)));
                }
                continue;
            }
            let is_pointer = self
                .variant_field_llvm_type(enum_name, variant, &fields, i)
                .map(|t| t.is_pointer_type())
                .unwrap_or(false);
            if is_pointer {
                glue.push((i, FieldGlue::Pointer));
            }
        }
        Ok(glue)
    }

    /// Apply `op` to every pointer payload of the active variant of the enum at
    /// `struct_alloca`, selecting the variant at runtime by switching on the
    /// discriminant. This mirrors construction, which retains a payload only when
    /// it is a pointer (`rc_inc_if_pointer`): a union slot typed as a pointer can
    /// hold a raw scalar for a different variant, so touching it unconditionally
    /// would corrupt memory. Scalar-only variants fall through to the merge block
    /// untouched. No-op for enums with no pointer payloads.
    fn emit_enum_payload_op(
        &mut self,
        enum_name: &str,
        struct_alloca: PointerValue<'a>,
        op: EnumPayloadOp,
    ) -> Result<(), String> {
        if !self.enum_has_rc_payload(enum_name) {
            return Ok(());
        }
        // A recursive enum cannot be inline-expanded - its glue would nest
        // forever at compile time - so route it through an out-of-line function
        // that recurses at runtime, following the box chain to a base variant
        // that carries no boxed payload (issue #309).
        if self.enum_is_recursive(enum_name) {
            let glue_fn = self.get_or_create_enum_glue_fn(enum_name, op)?;
            self.builder
                .build_call(glue_fn, &[struct_alloca.into()], "enum_glue_call")
                .map_err(|e| e.to_string())?;
            return Ok(());
        }
        self.emit_enum_payload_op_inline(enum_name, struct_alloca, op)
    }

    /// Inline body of `emit_enum_payload_op` for a non-recursive enum: switch on
    /// the discriminant and apply `op` to each glue field of the active variant.
    /// Terminates because the inline nested-enum graph is acyclic; boxed
    /// recursive fields route back through out-of-line glue functions.
    fn emit_enum_payload_op_inline(
        &mut self,
        enum_name: &str,
        struct_alloca: PointerValue<'a>,
        op: EnumPayloadOp,
    ) -> Result<(), String> {
        // generate_enum_type always stores a StructType here; match rather than
        // `into_struct_type()` so a violated invariant surfaces as a diagnostic
        // instead of panicking the compiler.
        let struct_type = match self.type_map.get(enum_name) {
            Some(BasicTypeEnum::StructType(struct_type)) => *struct_type,
            Some(_) => {
                return Err(format!(
                    "Enum {} type map entry is not a struct type",
                    enum_name
                ));
            }
            None => return Err(format!("Enum {} not found in type map", enum_name)),
        };
        let runtime_fn = self
            .runtime_function(op.runtime_fn_name())
            .ok_or_else(|| format!("{} not found", op.runtime_fn_name()))?;
        let label = op.label();

        let current_block = self
            .builder
            .get_insert_block()
            .ok_or("emit_enum_payload_op: builder has no insertion block")?;
        let function = current_block
            .get_parent()
            .ok_or("emit_enum_payload_op: insertion block has no parent function")?;

        // Load the discriminant (struct field 0).
        let tag_ptr = self
            .builder
            .build_struct_gep(struct_type, struct_alloca, 0, "enum_op_tag_ptr")
            .map_err(|e| e.to_string())?;
        let discriminant = self
            .builder
            .build_load(self.context.i32_type(), tag_ptr, "enum_op_tag")
            .map_err(|e| e.to_string())?
            .into_int_value();

        let merge_block = self
            .context
            .append_basic_block(function, &format!("enum_{label}_merge"));

        // One switch case per variant that actually owns an RC payload (a direct
        // pointer, or one nested inside an inline user-enum payload).
        let variants = self
            .enum_variants
            .get(enum_name)
            .cloned()
            .ok_or_else(|| format!("Enum {} has no variant list", enum_name))?;
        let mut cases = Vec::new();
        for variant in &variants {
            let glue_fields = self.variant_glue_fields(enum_name, variant)?;
            if glue_fields.is_empty() {
                continue;
            }
            let index = self.get_variant_index(enum_name, variant)?;
            let case_block = self
                .context
                .append_basic_block(function, &format!("enum_{label}_{enum_name}_{variant}"));
            self.builder.position_at_end(case_block);
            for (field_index, glue) in glue_fields {
                match glue {
                    // A nested inline user enum: recurse so its own active
                    // variant's payloads get the same operation.
                    FieldGlue::InlineEnum(inner) => {
                        let field_ptr = self
                            .builder
                            .build_struct_gep(
                                struct_type,
                                struct_alloca,
                                (field_index + 1) as u32,
                                "enum_nested_ptr",
                            )
                            .map_err(|e| e.to_string())?;
                        self.emit_enum_payload_op(&inner, field_ptr, op)?;
                    }
                    // A heap-boxed recursive nested enum: unbox and recurse
                    // through the box, then release or duplicate the box itself.
                    FieldGlue::BoxedEnum(inner) => {
                        self.emit_boxed_enum_field_op(
                            struct_type,
                            struct_alloca,
                            field_index,
                            &inner,
                            op,
                        )?;
                    }
                    FieldGlue::Pointer => {
                        self.emit_enum_payload_field_op(
                            struct_type,
                            struct_alloca,
                            field_index,
                            runtime_fn,
                            op,
                        )?;
                    }
                }
            }
            self.builder
                .build_unconditional_branch(merge_block)
                .map_err(|e| e.to_string())?;
            cases.push((
                self.context.i32_type().const_int(index as u64, false),
                case_block,
            ));
        }

        // Emit the switch on the original block, then continue at the merge.
        self.builder.position_at_end(current_block);
        self.builder
            .build_switch(discriminant, merge_block, &cases)
            .map_err(|e| e.to_string())?;
        self.builder.position_at_end(merge_block);
        Ok(())
    }

    /// Apply `op` to one pointer payload field (`field_index`) of an enum value:
    /// load the payload, call the op's runtime function, and for a deep clone
    /// store the returned independent copy back into the field.
    fn emit_enum_payload_field_op(
        &mut self,
        struct_type: inkwell::types::StructType<'a>,
        struct_alloca: PointerValue<'a>,
        field_index: usize,
        runtime_fn: inkwell::values::FunctionValue<'a>,
        op: EnumPayloadOp,
    ) -> Result<(), String> {
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let data_ptr = self
            .builder
            .build_struct_gep(
                struct_type,
                struct_alloca,
                (field_index + 1) as u32,
                "enum_op_field_ptr",
            )
            .map_err(|e| e.to_string())?;
        let payload = self
            .builder
            .build_load(ptr_type, data_ptr, "enum_op_payload")
            .map_err(|e| e.to_string())?;
        let call = self
            .builder
            .build_call(runtime_fn, &[payload.into()], "enum_op_call")
            .map_err(|e| e.to_string())?;
        if matches!(op, EnumPayloadOp::DeepClone) {
            let cloned = call
                .try_as_basic_value()
                .basic()
                .ok_or("mux_value_deep_clone returned no value")?;
            self.builder
                .build_store(data_ptr, cloned)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Emit, up front and in a clean builder state, the out-of-line glue an
    /// RC-payload enum needs to be boxed as a self-managing value (issue #309):
    /// drop and deep-clone (used as the managed box's drop/clone glue) plus
    /// retain for a recursive enum's internal inline-field retain. Generating
    /// this lazily from inside a function body would interleave the generated
    /// blocks with the caller's and corrupt the insertion point.
    pub(super) fn generate_enum_object_support(&mut self, enum_name: &str) -> Result<(), String> {
        self.get_or_create_enum_glue_fn(enum_name, EnumPayloadOp::Drop)?;
        self.get_or_create_enum_glue_fn(enum_name, EnumPayloadOp::DeepClone)?;
        if self.enum_is_recursive(enum_name) {
            self.get_or_create_enum_glue_fn(enum_name, EnumPayloadOp::Retain)?;
        }
        // Structural comparison glue, so a payload-carrying enum orders correctly
        // as a map key or set member. Built up front for the same reason as the
        // RC glue: a lazy build mid-body would corrupt the insertion point.
        self.get_or_create_enum_cmp_fn(enum_name)?;
        // Hash glue for the same reason: map and set are hash tables, so a
        // payload-carrying enum needs a hash that agrees with its comparison.
        self.get_or_create_enum_hash_fn(enum_name)?;
        Ok(())
    }

    /// Box `value` for storage into a collection, using its known semantic
    /// `elem_type` to decide the representation (issue #309): an enum that owns
    /// reference-counted payloads becomes a managed `BoxedEnum` so the runtime
    /// clones and drops it with value semantics as the collection copies and
    /// releases elements; everything else (including a payload-less enum) goes
    /// through `box_value`. The semantic type is required because a bare LLVM
    /// struct value cannot identify its enum - literal struct types are shared.
    pub(super) fn box_enum_or_value(
        &mut self,
        value: BasicValueEnum<'a>,
        elem_type: &Type,
    ) -> Result<PointerValue<'a>, String> {
        if value.is_struct_value()
            && let Some(enum_name) = self.user_enum_type_name(elem_type)
        {
            // Every user enum, not only one with a reference-counted payload.
            // A plain `box_value` makes an `Opaque`, which the runtime compares
            // and hashes byte for byte - correct only because constructors
            // zero-initialize the struct, so the padding and the unused bytes
            // of the union slot happen to match. That invariant lived in codegen
            // and was depended on by the runtime, with neither side saying so,
            // and deleting the "redundant" zero-store would have silently broken
            // enum map keys. A managed box carries the compare and hash glue, so
            // equality rests on the enum's fields rather than on its bytes.
            return self.box_enum_managed(value.into_struct_value(), &enum_name);
        }
        Ok(self.box_value(value))
    }

    /// Box an RC-payload enum value into a managed `BoxedEnum` (issue #309) so
    /// its deep-clone and drop glue run wherever the runtime later copies or
    /// releases it - notably inside collections, whose insert/read helpers
    /// `clone()` their elements. The box owns an independent deep copy of the
    /// enum, so the source temporary the caller still holds is released as usual.
    pub(super) fn box_enum_managed(
        &mut self,
        struct_val: inkwell::values::StructValue<'a>,
        enum_name: &str,
    ) -> Result<PointerValue<'a>, String> {
        let clone_glue = *self
            .enum_glue_fns
            .get(&(enum_name.to_string(), EnumPayloadOp::DeepClone.label()))
            .ok_or_else(|| format!("Enum {} deep-clone glue missing", enum_name))?;
        let drop_glue = *self
            .enum_glue_fns
            .get(&(enum_name.to_string(), EnumPayloadOp::Drop.label()))
            .ok_or_else(|| format!("Enum {} drop glue missing", enum_name))?;
        let cmp_glue = self.get_or_create_enum_cmp_fn(enum_name)?;
        let hash_glue = self.get_or_create_enum_hash_fn(enum_name)?;
        let struct_type = struct_val.get_type();
        let temp = self
            .builder
            .build_alloca(struct_type, "enum_box_src")
            .map_err(|e| e.to_string())?;
        self.builder
            .build_store(temp, struct_val)
            .map_err(|e| e.to_string())?;
        let size = struct_type
            .size_of()
            .ok_or("enum struct has no size for boxing")?;
        let box_fn = self
            .runtime_function("mux_box_enum_managed")
            .ok_or("mux_box_enum_managed not found")?;
        let boxed = self
            .builder
            .build_call(
                box_fn,
                &[
                    temp.into(),
                    size.into(),
                    clone_glue.as_global_value().as_pointer_value().into(),
                    drop_glue.as_global_value().as_pointer_value().into(),
                    cmp_glue.as_global_value().as_pointer_value().into(),
                    hash_glue.as_global_value().as_pointer_value().into(),
                ],
                "managed_enum_box",
            )
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or("mux_box_enum_managed returned no value")?
            .into_pointer_value();
        self.register_temp(boxed.into());
        Ok(boxed)
    }

    /// Return (building it on first use) the out-of-line RC-glue function for a
    /// recursive enum: `void @mux_enum_glue_<op>_<Enum>(ptr)`. Its body switches
    /// on the active variant and applies `op` to the variant's payloads, and it
    /// recurses into itself at runtime through boxed fields - so unlike inline
    /// expansion it terminates, following the box chain to a base variant.
    /// Memoized and inserted before its body is built so a variant that
    /// re-embeds the enum resolves the self-call (issue #309).
    fn get_or_create_enum_glue_fn(
        &mut self,
        enum_name: &str,
        op: EnumPayloadOp,
    ) -> Result<inkwell::values::FunctionValue<'a>, String> {
        let key = (enum_name.to_string(), op.label());
        if let Some(existing) = self.enum_glue_fns.get(&key) {
            return Ok(*existing);
        }
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let fn_type = self.context.void_type().fn_type(&[ptr_type.into()], false);
        let function = self.module.add_function(
            &format!("mux_enum_glue_{}_{}", op.label(), enum_name),
            fn_type,
            None,
        );
        self.enum_glue_fns.insert(key, function);

        let saved_block = self.builder.get_insert_block();
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);
        let param = function
            .get_nth_param(0)
            .ok_or("enum glue function is missing its parameter")?
            .into_pointer_value();
        self.emit_enum_payload_op_inline(enum_name, param, op)?;
        self.builder.build_return(None).map_err(|e| e.to_string())?;
        if let Some(block) = saved_block {
            self.builder.position_at_end(block);
        }
        Ok(function)
    }

    /// Classify every payload field of `variant` for structural comparison
    /// (issue #309): unlike `variant_glue_fields`, this includes inline scalars,
    /// since they are compared even though they own no reference-counted memory.
    fn variant_compare_fields(
        &self,
        enum_name: &str,
        variant: &str,
    ) -> Result<Vec<(usize, CompareKind<'a>)>, String> {
        let fields = self.variant_field_types(enum_name, variant)?;
        let mut out = Vec::new();
        for (i, field) in fields.iter().enumerate() {
            if let Some(inner) = self.nested_user_enum_name(&field.1) {
                if self.boxed_recursive_field(enum_name, variant, i).is_some() {
                    out.push((i, CompareKind::BoxedEnum(inner)));
                } else {
                    out.push((i, CompareKind::InlineEnum(inner)));
                }
                continue;
            }
            let ty = self.variant_field_llvm_type(enum_name, variant, &fields, i)?;
            if ty.is_pointer_type() {
                out.push((i, CompareKind::Pointer));
            } else {
                out.push((i, CompareKind::Scalar(ty)));
            }
        }
        Ok(out)
    }

    /// Return (building it on first use) the structural comparison function for
    /// an RC-payload enum: `i32 @mux_enum_cmp_<Enum>(ptr a, ptr b)`. It compares
    /// discriminants, then each payload field of the matching variant in order,
    /// yielding the first non-zero three-way result (like `Ord::cmp`). Pointer
    /// payloads defer to `mux_value_compare` and nested enums recurse into their
    /// own compare function (following box chains at runtime, so it terminates).
    /// Memoized and declared before its body so a self-embedding enum resolves
    /// the recursive call (issue #309).
    pub(super) fn get_or_create_enum_cmp_fn(
        &mut self,
        enum_name: &str,
    ) -> Result<inkwell::values::FunctionValue<'a>, String> {
        let key = (enum_name.to_string(), "cmp");
        if let Some(existing) = self.enum_glue_fns.get(&key) {
            return Ok(*existing);
        }
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let i32_type = self.context.i32_type();
        let fn_type = i32_type.fn_type(&[ptr_type.into(), ptr_type.into()], false);
        let function =
            self.module
                .add_function(&format!("mux_enum_cmp_{}", enum_name), fn_type, None);
        self.enum_glue_fns.insert(key, function);

        let struct_type = match self.type_map.get(enum_name) {
            Some(BasicTypeEnum::StructType(st)) => *st,
            _ => return Err(format!("Enum {} is not a struct type", enum_name)),
        };
        let saved_block = self.builder.get_insert_block();
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);
        let a = function
            .get_nth_param(0)
            .ok_or("enum compare function missing parameter a")?
            .into_pointer_value();
        let b = function
            .get_nth_param(1)
            .ok_or("enum compare function missing parameter b")?
            .into_pointer_value();

        let result = self
            .builder
            .build_alloca(i32_type, "cmp_result")
            .map_err(|e| e.to_string())?;
        // Discriminants first: a difference is the whole answer, and every field
        // comparison below is guarded on `result == 0` so it is then skipped.
        let da = self.load_enum_discriminant_i32(struct_type, a)?;
        let db = self.load_enum_discriminant_i32(struct_type, b)?;
        let disc_cmp = self.build_three_way_int(da, db, false)?;
        self.builder
            .build_store(result, disc_cmp)
            .map_err(|e| e.to_string())?;

        let merge = self.context.append_basic_block(function, "cmp_merge");
        let variants = self
            .enum_variants
            .get(enum_name)
            .cloned()
            .ok_or_else(|| format!("Enum {} has no variant list", enum_name))?;
        let mut cases = Vec::new();
        for variant in &variants {
            let fields = self.variant_compare_fields(enum_name, variant)?;
            if fields.is_empty() {
                continue;
            }
            let index = self.get_variant_index(enum_name, variant)?;
            let case_block = self
                .context
                .append_basic_block(function, &format!("cmp_{enum_name}_{variant}"));
            self.builder.position_at_end(case_block);
            self.emit_variant_field_compares(struct_type, a, b, result, &fields)?;
            self.builder
                .build_unconditional_branch(merge)
                .map_err(|e| e.to_string())?;
            cases.push((i32_type.const_int(index as u64, false), case_block));
        }
        self.builder.position_at_end(entry);
        self.builder
            .build_switch(da, merge, &cases)
            .map_err(|e| e.to_string())?;
        self.builder.position_at_end(merge);
        let final_result = self
            .builder
            .build_load(i32_type, result, "cmp_final")
            .map_err(|e| e.to_string())?;
        self.builder
            .build_return(Some(&final_result))
            .map_err(|e| e.to_string())?;
        if let Some(block) = saved_block {
            self.builder.position_at_end(block);
        }
        Ok(function)
    }

    /// Load an enum's i32 discriminant (struct field 0) through `ptr`.
    fn load_enum_discriminant_i32(
        &self,
        struct_type: inkwell::types::StructType<'a>,
        ptr: PointerValue<'a>,
    ) -> Result<inkwell::values::IntValue<'a>, String> {
        let disc_ptr = self
            .builder
            .build_struct_gep(struct_type, ptr, 0, "cmp_disc_ptr")
            .map_err(|e| e.to_string())?;
        Ok(self
            .builder
            .build_load(self.context.i32_type(), disc_ptr, "cmp_disc")
            .map_err(|e| e.to_string())?
            .into_int_value())
    }

    /// Return (building it on first use) the structural hash function for an
    /// enum: `u64 @mux_enum_hash_<Enum>(ptr a)`.
    ///
    /// Mirrors `get_or_create_enum_cmp_fn` field for field, and has to: the
    /// runtime's map and set are hash tables, so two values `cmp_glue` calls
    /// equal must hash equally. Hashing the raw bytes would not do - the inline
    /// struct has padding between the discriminant and the payload, and that
    /// padding is not guaranteed equal for two otherwise equal values.
    ///
    /// Combines with FNV-1a, which is deterministic and needs no state.
    pub(super) fn get_or_create_enum_hash_fn(
        &mut self,
        enum_name: &str,
    ) -> Result<inkwell::values::FunctionValue<'a>, String> {
        let key = (enum_name.to_string(), "hash");
        if let Some(existing) = self.enum_glue_fns.get(&key) {
            return Ok(*existing);
        }
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let i64_type = self.context.i64_type();
        let fn_type = i64_type.fn_type(&[ptr_type.into()], false);
        let function =
            self.module
                .add_function(&format!("mux_enum_hash_{}", enum_name), fn_type, None);
        // Memoized before the body is built, so a self-embedding enum resolves
        // the recursive call rather than expanding forever (issue #309).
        self.enum_glue_fns.insert(key, function);

        let struct_type = match self.type_map.get(enum_name) {
            Some(BasicTypeEnum::StructType(st)) => *st,
            _ => return Err(format!("Enum {} is not a struct type", enum_name)),
        };
        let saved_block = self.builder.get_insert_block();
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);
        let a = function
            .get_nth_param(0)
            .ok_or("enum hash function missing its parameter")?
            .into_pointer_value();

        let acc = self
            .builder
            .build_alloca(i64_type, "hash_acc")
            .map_err(|e| e.to_string())?;
        self.builder
            .build_store(acc, i64_type.const_int(FNV_OFFSET_BASIS, false))
            .map_err(|e| e.to_string())?;

        // The discriminant always contributes, so two variants with the same
        // payload bits still hash differently.
        let disc = self.load_enum_discriminant_i32(struct_type, a)?;
        let disc_wide = self
            .builder
            .build_int_z_extend(disc, i64_type, "hash_disc")
            .map_err(|e| e.to_string())?;
        self.mix_into_hash(acc, disc_wide)?;

        let merge = self.context.append_basic_block(function, "hash_merge");
        let variants = self
            .enum_variants
            .get(enum_name)
            .cloned()
            .ok_or_else(|| format!("Enum {} has no variant list", enum_name))?;
        let mut cases = Vec::new();
        for variant in &variants {
            let fields = self.variant_compare_fields(enum_name, variant)?;
            if fields.is_empty() {
                continue;
            }
            let index = self.get_variant_index(enum_name, variant)?;
            let case_block = self
                .context
                .append_basic_block(function, &format!("hash_{enum_name}_{variant}"));
            self.builder.position_at_end(case_block);
            for (idx, kind) in &fields {
                let field_hash = self.emit_hash_field(struct_type, a, *idx, kind)?;
                self.mix_into_hash(acc, field_hash)?;
            }
            self.builder
                .build_unconditional_branch(merge)
                .map_err(|e| e.to_string())?;
            cases.push((
                self.context.i32_type().const_int(index as u64, false),
                case_block,
            ));
        }
        self.builder.position_at_end(entry);
        self.builder
            .build_switch(disc, merge, &cases)
            .map_err(|e| e.to_string())?;
        self.builder.position_at_end(merge);
        let final_hash = self
            .builder
            .build_load(i64_type, acc, "hash_final")
            .map_err(|e| e.to_string())?;
        self.builder
            .build_return(Some(&final_hash))
            .map_err(|e| e.to_string())?;
        if let Some(block) = saved_block {
            self.builder.position_at_end(block);
        }
        Ok(function)
    }

    /// `acc = (acc XOR value) * FNV_PRIME`, the FNV-1a step.
    fn mix_into_hash(
        &mut self,
        acc: PointerValue<'a>,
        value: inkwell::values::IntValue<'a>,
    ) -> Result<(), String> {
        let i64_type = self.context.i64_type();
        let current = self
            .builder
            .build_load(i64_type, acc, "hash_cur")
            .map_err(|e| e.to_string())?
            .into_int_value();
        let xored = self
            .builder
            .build_xor(current, value, "hash_xor")
            .map_err(|e| e.to_string())?;
        let mixed = self
            .builder
            .build_int_mul(xored, i64_type.const_int(FNV_PRIME, false), "hash_mul")
            .map_err(|e| e.to_string())?;
        self.builder
            .build_store(acc, mixed)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Hash a float by its bits, after canonicalizing the two cases where equal
    /// values have different bit patterns.
    ///
    /// The comparison glue uses float semantics, so `0.0 == -0.0` and two NaNs
    /// come out equal (neither `OLT` nor `OGT` holds, so the three-way result is
    /// zero). Their bit patterns differ, so hashing the bits directly breaks the
    /// rule that equal values hash equally - a set built from `M.At(0.0)` and
    /// `M.At(-0.0)` ended up holding both.
    ///
    /// Adding zero collapses `-0.0` to `+0.0` and leaves every other value
    /// alone, and NaN takes a fixed hash so all NaNs agree.
    fn hash_float_canonically(
        &mut self,
        value: inkwell::values::FloatValue<'a>,
    ) -> Result<inkwell::values::IntValue<'a>, String> {
        let i64_type = self.context.i64_type();
        let float_type = value.get_type();
        let normalized = self
            .builder
            .build_float_add(value, float_type.const_zero(), "hash_fnorm")
            .map_err(|e| e.to_string())?;
        let bits = self
            .builder
            .build_bit_cast(normalized, i64_type, "hash_fbits")
            .map_err(|e| e.to_string())?
            .into_int_value();
        // `UNO` is true when either operand is NaN, and both are this value.
        let is_nan = self
            .builder
            .build_float_compare(inkwell::FloatPredicate::UNO, value, value, "hash_fisnan")
            .map_err(|e| e.to_string())?;
        Ok(self
            .builder
            .build_select(
                is_nan,
                i64_type.const_int(NAN_HASH, false),
                bits,
                "hash_fcanon",
            )
            .map_err(|e| e.to_string())?
            .into_int_value())
    }

    /// Hash one payload field to `u64`, by the same four cases the comparison
    /// uses, so the two stay in step.
    fn emit_hash_field(
        &mut self,
        struct_type: inkwell::types::StructType<'a>,
        a: PointerValue<'a>,
        idx: usize,
        kind: &CompareKind<'a>,
    ) -> Result<inkwell::values::IntValue<'a>, String> {
        let i64_type = self.context.i64_type();
        let gep = self
            .builder
            .build_struct_gep(struct_type, a, (idx + 1) as u32, "hash_fa")
            .map_err(|e| e.to_string())?;
        match kind {
            CompareKind::Scalar(ty) => {
                let loaded = self
                    .builder
                    .build_load(*ty, gep, "hash_la")
                    .map_err(|e| e.to_string())?;
                if loaded.is_float_value() {
                    return self.hash_float_canonically(loaded.into_float_value());
                }
                let as_int = loaded.into_int_value();
                if as_int.get_type().get_bit_width() < 64 {
                    return self
                        .builder
                        .build_int_z_extend(as_int, i64_type, "hash_widen")
                        .map_err(|e| e.to_string());
                }
                Ok(as_int)
            }
            CompareKind::Pointer => {
                let ptr_type = self.context.ptr_type(AddressSpace::default());
                let loaded = self
                    .builder
                    .build_load(ptr_type, gep, "hash_pa")
                    .map_err(|e| e.to_string())?;
                let hash_fn = self
                    .runtime_function("mux_value_hash")
                    .ok_or("mux_value_hash not found")?;
                Ok(self
                    .builder
                    .build_call(hash_fn, &[loaded.into()], "hash_value_call")
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("mux_value_hash returned no value")?
                    .into_int_value())
            }
            CompareKind::InlineEnum(inner) => {
                let inner_hash = self.get_or_create_enum_hash_fn(inner)?;
                Ok(self
                    .builder
                    .build_call(inner_hash, &[gep.into()], "hash_inline")
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("nested enum hash returned no value")?
                    .into_int_value())
            }
            CompareKind::BoxedEnum(inner) => {
                let ptr_type = self.context.ptr_type(AddressSpace::default());
                let unbox = self
                    .runtime_function("mux_value_unbox_enum")
                    .ok_or("mux_value_unbox_enum not found")?;
                let boxed = self
                    .builder
                    .build_load(ptr_type, gep, "hash_ba")
                    .map_err(|e| e.to_string())?;
                let unboxed = self
                    .builder
                    .build_call(unbox, &[boxed.into()], "hash_ua")
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("unbox returned no value")?;
                let inner_hash = self.get_or_create_enum_hash_fn(inner)?;
                Ok(self
                    .builder
                    .build_call(inner_hash, &[unboxed.into()], "hash_boxed")
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("boxed enum hash returned no value")?
                    .into_int_value())
            }
        }
    }

    /// Emit the guarded, in-order field comparisons for one variant's case
    /// block: each field's three-way result overwrites `result` only while
    /// `result` is still zero, so the first difference wins (lexicographic).
    fn emit_variant_field_compares(
        &mut self,
        struct_type: inkwell::types::StructType<'a>,
        a: PointerValue<'a>,
        b: PointerValue<'a>,
        result: PointerValue<'a>,
        fields: &[(usize, CompareKind<'a>)],
    ) -> Result<(), String> {
        let i32_type = self.context.i32_type();
        let function = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or("enum compare: no current function")?;
        for (idx, kind) in fields {
            let r = self
                .builder
                .build_load(i32_type, result, "cmp_r")
                .map_err(|e| e.to_string())?
                .into_int_value();
            let is_zero = self
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::EQ,
                    r,
                    i32_type.const_zero(),
                    "cmp_r0",
                )
                .map_err(|e| e.to_string())?;
            let do_bb = self.context.append_basic_block(function, "cmp_do");
            let cont_bb = self.context.append_basic_block(function, "cmp_cont");
            self.builder
                .build_conditional_branch(is_zero, do_bb, cont_bb)
                .map_err(|e| e.to_string())?;
            self.builder.position_at_end(do_bb);
            let c = self.emit_compare_field(struct_type, a, b, *idx, kind)?;
            self.builder
                .build_store(result, c)
                .map_err(|e| e.to_string())?;
            self.builder
                .build_unconditional_branch(cont_bb)
                .map_err(|e| e.to_string())?;
            self.builder.position_at_end(cont_bb);
        }
        Ok(())
    }

    /// Emit the three-way comparison (`i32`) of field `idx` between `a` and `b`.
    fn emit_compare_field(
        &mut self,
        struct_type: inkwell::types::StructType<'a>,
        a: PointerValue<'a>,
        b: PointerValue<'a>,
        idx: usize,
        kind: &CompareKind<'a>,
    ) -> Result<inkwell::values::IntValue<'a>, String> {
        let gep_a = self
            .builder
            .build_struct_gep(struct_type, a, (idx + 1) as u32, "cmp_fa")
            .map_err(|e| e.to_string())?;
        let gep_b = self
            .builder
            .build_struct_gep(struct_type, b, (idx + 1) as u32, "cmp_fb")
            .map_err(|e| e.to_string())?;
        match kind {
            CompareKind::Scalar(ty) => {
                let la = self
                    .builder
                    .build_load(*ty, gep_a, "cmp_la")
                    .map_err(|e| e.to_string())?;
                let lb = self
                    .builder
                    .build_load(*ty, gep_b, "cmp_lb")
                    .map_err(|e| e.to_string())?;
                self.build_scalar_three_way(la, lb)
            }
            CompareKind::Pointer => {
                let ptr_type = self.context.ptr_type(AddressSpace::default());
                let pa = self
                    .builder
                    .build_load(ptr_type, gep_a, "cmp_pa")
                    .map_err(|e| e.to_string())?;
                let pb = self
                    .builder
                    .build_load(ptr_type, gep_b, "cmp_pb")
                    .map_err(|e| e.to_string())?;
                let cmp_fn = self
                    .runtime_function("mux_value_compare")
                    .ok_or("mux_value_compare not found")?;
                Ok(self
                    .builder
                    .build_call(cmp_fn, &[pa.into(), pb.into()], "cmp_val")
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("mux_value_compare returned no value")?
                    .into_int_value())
            }
            CompareKind::InlineEnum(inner) => {
                let inner_cmp = self.get_or_create_enum_cmp_fn(inner)?;
                Ok(self
                    .builder
                    .build_call(inner_cmp, &[gep_a.into(), gep_b.into()], "cmp_inner")
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("inner enum compare returned no value")?
                    .into_int_value())
            }
            CompareKind::BoxedEnum(inner) => {
                let ptr_type = self.context.ptr_type(AddressSpace::default());
                let unbox = self
                    .runtime_function("mux_value_unbox_enum")
                    .ok_or("mux_value_unbox_enum not found")?;
                let ba = self
                    .builder
                    .build_load(ptr_type, gep_a, "cmp_ba")
                    .map_err(|e| e.to_string())?;
                let bb = self
                    .builder
                    .build_load(ptr_type, gep_b, "cmp_bb")
                    .map_err(|e| e.to_string())?;
                let ua = self
                    .builder
                    .build_call(unbox, &[ba.into()], "cmp_ua")
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("unbox returned no value")?;
                let ub = self
                    .builder
                    .build_call(unbox, &[bb.into()], "cmp_ub")
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("unbox returned no value")?;
                let inner_cmp = self.get_or_create_enum_cmp_fn(inner)?;
                Ok(self
                    .builder
                    .build_call(inner_cmp, &[ua.into(), ub.into()], "cmp_boxed")
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("boxed enum compare returned no value")?
                    .into_int_value())
            }
        }
    }

    /// Three-way compare two scalars (int/bool/char or float) to `i32`
    /// (-1/0/1). 64-bit ints are signed (Mux `int`); narrower ints (bool, char)
    /// are unsigned.
    fn build_scalar_three_way(
        &self,
        la: BasicValueEnum<'a>,
        lb: BasicValueEnum<'a>,
    ) -> Result<inkwell::values::IntValue<'a>, String> {
        if la.is_int_value() {
            let signed = la.into_int_value().get_type().get_bit_width() == 64;
            self.build_three_way_int(la.into_int_value(), lb.into_int_value(), signed)
        } else if la.is_float_value() {
            let (lt, gt) = (inkwell::FloatPredicate::OLT, inkwell::FloatPredicate::OGT);
            let a = la.into_float_value();
            let b = lb.into_float_value();
            let is_lt = self
                .builder
                .build_float_compare(lt, a, b, "cmp_flt")
                .map_err(|e| e.to_string())?;
            let is_gt = self
                .builder
                .build_float_compare(gt, a, b, "cmp_fgt")
                .map_err(|e| e.to_string())?;
            self.select_three_way(is_lt, is_gt)
        } else {
            Err("unsupported scalar type in enum comparison".to_string())
        }
    }

    /// Three-way compare two same-width integers to `i32` (-1/0/1).
    fn build_three_way_int(
        &self,
        a: inkwell::values::IntValue<'a>,
        b: inkwell::values::IntValue<'a>,
        signed: bool,
    ) -> Result<inkwell::values::IntValue<'a>, String> {
        let (lt_pred, gt_pred) = if signed {
            (inkwell::IntPredicate::SLT, inkwell::IntPredicate::SGT)
        } else {
            (inkwell::IntPredicate::ULT, inkwell::IntPredicate::UGT)
        };
        let is_lt = self
            .builder
            .build_int_compare(lt_pred, a, b, "cmp_ilt")
            .map_err(|e| e.to_string())?;
        let is_gt = self
            .builder
            .build_int_compare(gt_pred, a, b, "cmp_igt")
            .map_err(|e| e.to_string())?;
        self.select_three_way(is_lt, is_gt)
    }

    /// Fold `is_lt`/`is_gt` booleans into an `i32` of -1/1/0.
    fn select_three_way(
        &self,
        is_lt: inkwell::values::IntValue<'a>,
        is_gt: inkwell::values::IntValue<'a>,
    ) -> Result<inkwell::values::IntValue<'a>, String> {
        let i32_type = self.context.i32_type();
        let neg_one = i32_type.const_int((-1i64) as u64, true);
        let one = i32_type.const_int(1, false);
        let zero = i32_type.const_zero();
        let gt_or_zero = self
            .builder
            .build_select(is_gt, one, zero, "cmp_gt0")
            .map_err(|e| e.to_string())?
            .into_int_value();
        Ok(self
            .builder
            .build_select(is_lt, neg_one, gt_or_zero, "cmp_sel")
            .map_err(|e| e.to_string())?
            .into_int_value())
    }

    /// Apply `op` to a heap-boxed recursive nested-enum payload (issue #309). The
    /// field slot holds a `Value::Opaque` box that exclusively owns one inner
    /// enum value - boxes are never shared, so each stays at refcount 1:
    /// - Drop: unbox, recurse the drop into the inner enum's own payloads, then
    ///   release the box (freeing it, since it was the sole owner).
    /// - Retain/DeepClone: box a fresh independent copy and deep-clone its
    ///   payloads so it shares nothing with the source, then store it back. Both
    ///   ops behave identically: a shared retain would break the unshared-box
    ///   invariant the unconditional recursive drop relies on.
    fn emit_boxed_enum_field_op(
        &mut self,
        struct_type: inkwell::types::StructType<'a>,
        struct_alloca: PointerValue<'a>,
        field_index: usize,
        inner: &str,
        op: EnumPayloadOp,
    ) -> Result<(), String> {
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let inner_struct = match self.type_map.get(inner) {
            Some(BasicTypeEnum::StructType(st)) => *st,
            _ => return Err(format!("Boxed nested enum {} is not a struct type", inner)),
        };
        let field_ptr = self
            .builder
            .build_struct_gep(
                struct_type,
                struct_alloca,
                (field_index + 1) as u32,
                "boxed_field_ptr",
            )
            .map_err(|e| e.to_string())?;
        let box_ptr = self
            .builder
            .build_load(ptr_type, field_ptr, "boxed_ptr")
            .map_err(|e| e.to_string())?
            .into_pointer_value();
        let unbox_fn = self
            .runtime_function("mux_value_unbox_enum")
            .ok_or("mux_value_unbox_enum not found")?;
        if matches!(op, EnumPayloadOp::Drop) {
            let inner_data = self
                .builder
                .build_call(unbox_fn, &[box_ptr.into()], "unbox_drop")
                .map_err(|e| e.to_string())?
                .try_as_basic_value()
                .basic()
                .ok_or("mux_value_unbox_enum returned no value")?
                .into_pointer_value();
            self.emit_enum_payload_op(inner, inner_data, EnumPayloadOp::Drop)?;
            let rc_dec = self
                .runtime_function("mux_rc_dec")
                .ok_or("mux_rc_dec not found")?;
            self.builder
                .build_call(rc_dec, &[box_ptr.into()], "box_release")
                .map_err(|e| e.to_string())?;
            return Ok(());
        }
        // Retain or DeepClone: produce a fresh, fully independent box.
        let src_data = self
            .builder
            .build_call(unbox_fn, &[box_ptr.into()], "unbox_src")
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or("mux_value_unbox_enum returned no value")?
            .into_pointer_value();
        let box_fn = self
            .runtime_function("mux_box_enum")
            .ok_or("mux_box_enum not found")?;
        let size = inner_struct
            .size_of()
            .ok_or("boxed nested enum struct has no size")?;
        let new_box = self
            .builder
            .build_call(box_fn, &[src_data.into(), size.into()], "rebox")
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or("mux_box_enum returned no value")?
            .into_pointer_value();
        let new_data = self
            .builder
            .build_call(unbox_fn, &[new_box.into()], "unbox_new")
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or("mux_value_unbox_enum returned no value")?
            .into_pointer_value();
        // Always a deep clone (never a shared retain) so the fresh box owns
        // independent copies of the inner enum's own boxed children.
        self.emit_enum_payload_op(inner, new_data, EnumPayloadOp::DeepClone)?;
        self.builder
            .build_store(field_ptr, new_box)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Store constructor argument `arg` (an inline nested-enum value) into a
    /// boxed recursive field slot at `data_ptr` (issue #309): heap-box it into a
    /// `Value::Opaque` and deep-clone the box's payloads so it owns an
    /// independent copy. The caller still drops the temporary it passed in, and
    /// the box stays the sole (refcount-1) owner of its contents. `inner` names
    /// the boxed enum.
    pub(super) fn store_boxed_recursive_field(
        &mut self,
        inner: &str,
        arg: BasicValueEnum<'a>,
        data_ptr: PointerValue<'a>,
    ) -> Result<(), String> {
        let inner_struct = match self.type_map.get(inner) {
            Some(BasicTypeEnum::StructType(st)) => *st,
            _ => return Err(format!("Boxed nested enum {} is not a struct type", inner)),
        };
        let temp_ptr = self
            .builder
            .build_alloca(inner_struct, "box_src")
            .map_err(|e| e.to_string())?;
        self.builder
            .build_store(temp_ptr, arg)
            .map_err(|e| e.to_string())?;
        let box_fn = self
            .runtime_function("mux_box_enum")
            .ok_or("mux_box_enum not found")?;
        let size = inner_struct
            .size_of()
            .ok_or("boxed nested enum struct has no size")?;
        let new_box = self
            .builder
            .build_call(box_fn, &[temp_ptr.into(), size.into()], "box_recursive")
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or("mux_box_enum returned no value")?
            .into_pointer_value();
        let unbox_fn = self
            .runtime_function("mux_value_unbox_enum")
            .ok_or("mux_value_unbox_enum not found")?;
        let new_data = self
            .builder
            .build_call(unbox_fn, &[new_box.into()], "unbox_box_src")
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or("mux_value_unbox_enum returned no value")?
            .into_pointer_value();
        // Deep-clone so the box shares nothing with `arg`, which the caller still
        // drops as a statement temporary.
        self.emit_enum_payload_op(inner, new_data, EnumPayloadOp::DeepClone)?;
        self.builder
            .build_store(data_ptr, new_box)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Track a closure-typed variable in the current scope. Its slot will have
    /// `mux_closure_release` called on it when the scope ends. Mirrors
    /// `track_rc_variable` but for closures (which are not RC `Value`s).
    pub(super) fn track_closure_variable(&mut self, name: &str, alloca: PointerValue<'a>) {
        if let Some(current_scope) = self.closure_scope_stack.last_mut() {
            if current_scope.iter().any(|(_, p)| *p == alloca) {
                return;
            }
            current_scope.push((name.to_string(), alloca));
        }
    }

    /// Register a freshly produced, owned closure temporary so it is released
    /// with `mux_closure_release` at the end of the current statement unless
    /// ownership is transferred to a binding or return value first. Spilled into
    /// a null-initialized entry-block slot exactly like `register_temp`, so it is
    /// dominance-safe and null-safe on paths that never produced it.
    pub(super) fn register_closure_temp(&mut self, value: BasicValueEnum<'a>) {
        if !value.is_pointer_value() {
            return;
        }
        let ptr = value.into_pointer_value();
        if self.closure_temp_values.iter().any(|(v, _)| *v == ptr) {
            return;
        }
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let Ok(slot) = self.create_entry_alloca(ptr_type.into(), "closure_temp_slot") else {
            return;
        };
        if self.builder.build_store(slot, value).is_err() {
            return;
        }
        self.closure_temp_values.push((ptr, slot));
    }

    /// Transfer ownership of a closure temporary out of the pending set (e.g. it
    /// was stored into a variable or returned). Returns whether it was tracked.
    pub(super) fn untrack_closure_temp(&mut self, value: BasicValueEnum<'a>) -> bool {
        if value.is_pointer_value() {
            let ptr = value.into_pointer_value();
            if let Some(pos) = self
                .closure_temp_values
                .iter()
                .rposition(|(p, _)| *p == ptr)
            {
                self.closure_temp_values.remove(pos);
                return true;
            }
        }
        false
    }

    /// Emit `mux_closure_release` for each closure temporary registered since
    /// `mark`, without mutating the pending-temporary list.
    fn emit_closure_temps_since(&mut self, mark: usize) -> Result<(), String> {
        if self.closure_temp_values.len() <= mark {
            return Ok(());
        }
        let live = self
            .builder
            .get_insert_block()
            .is_some_and(|bb| bb.get_terminator().is_none());
        if live {
            let release = self
                .runtime_function("mux_closure_release")
                .ok_or("mux_closure_release not found")?;
            let ptr_type = self.context.ptr_type(AddressSpace::default());
            let null_ptr = ptr_type.const_null();
            let slots: Vec<PointerValue<'a>> = self.closure_temp_values[mark..]
                .iter()
                .map(|(_, slot)| *slot)
                .collect();
            for slot in slots {
                let loaded = self
                    .builder
                    .build_load(ptr_type, slot, "closure_temp_load")
                    .map_err(|e| e.to_string())?;
                self.builder
                    .build_call(release, &[loaded.into()], "closure_release_temp")
                    .map_err(|e| e.to_string())?;
                self.builder
                    .build_store(slot, null_ptr)
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    /// Release every pending closure temporary without truncating (return-path
    /// sibling of `cleanup_all_temps`).
    pub(super) fn cleanup_all_closure_temps(&mut self) -> Result<(), String> {
        let live = self
            .builder
            .get_insert_block()
            .is_some_and(|bb| bb.get_terminator().is_none());
        if !live || self.closure_temp_values.is_empty() {
            return Ok(());
        }
        let release = self
            .runtime_function("mux_closure_release")
            .ok_or("mux_closure_release not found")?;
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let slots: Vec<PointerValue<'a>> = self
            .closure_temp_values
            .iter()
            .map(|(_, slot)| *slot)
            .collect();
        for slot in slots {
            let loaded = self
                .builder
                .build_load(ptr_type, slot, "closure_temp_load")
                .map_err(|e| e.to_string())?;
            self.builder
                .build_call(release, &[loaded.into()], "closure_release_temp")
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Emit `mux_closure_release` for a list of closure-typed variable slots.
    pub(super) fn generate_closure_cleanup_for_vars(
        &mut self,
        vars: &[(String, PointerValue<'a>)],
    ) -> Result<(), String> {
        if vars.is_empty() {
            return Ok(());
        }
        let release = self
            .runtime_function("mux_closure_release")
            .ok_or("mux_closure_release not found")?;
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        for (name, alloca) in vars {
            let value = self
                .builder
                .build_load(ptr_type, *alloca, &format!("closure_load_{}", name))
                .map_err(|e| e.to_string())?;
            self.builder
                .build_call(
                    release,
                    &[value.into()],
                    &format!("closure_release_{}", name),
                )
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Retain a closure value (increment its refcount) - used when a closure is
    /// returned so it survives the producing scope's cleanup.
    pub(super) fn retain_closure(&mut self, value: BasicValueEnum<'a>) -> Result<(), String> {
        if !value.is_pointer_value() {
            return Ok(());
        }
        let retain = self
            .runtime_function("mux_closure_retain")
            .ok_or("mux_closure_retain not found")?;
        self.builder
            .build_call(retain, &[value.into()], "closure_retain")
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Register a freshly produced, owned RC temporary so it will be
    /// decremented at the end of the current statement unless ownership is
    /// transferred first. No-op for non-pointer (unboxed scalar) values.
    ///
    /// The temporary is spilled into a null-initialized entry-block alloca (its
    /// "slot"). Because the slot dominates the whole function, cleanup can load
    /// and decrement it from any later block regardless of the control flow that
    /// produced the value; on paths that never produced it the slot is still
    /// null and `mux_rc_dec` (null-safe) is a no-op. `mem2reg` promotes these
    /// slots back to SSA/phi form, so this is the standard way to let LLVM place
    /// dominance-correct cleanups for values born inside conditional control
    /// flow (short-circuit operands, ternary arms, loop bodies).
    ///
    /// Tracking is a best-effort optimization: it is called from `box_value`,
    /// which is on the hot path of nearly every expression and does not return a
    /// `Result`. If the slot cannot be materialized (no active function, or a
    /// builder error) the temporary is simply left untracked - it will not be
    /// auto-released (a leak), which is preferable to aborting codegen. So this
    /// is infallible by design.
    pub(super) fn register_temp(&mut self, value: BasicValueEnum<'a>) {
        if !value.is_pointer_value() {
            return;
        }
        let ptr = value.into_pointer_value();
        // A pointer identifies a unique allocation, so it must be tracked (and
        // thus decremented) at most once. The same pointer legitimately flows
        // out of several owned-returning calls when a function returns its own
        // argument (e.g. an in-place `sort(list)` whose `auto out = items`
        // aliases its input): registering it twice would free the single
        // reference twice, dangling whatever bound the value.
        if self.temp_values.iter().any(|(v, _)| *v == ptr) {
            return;
        }
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let Ok(slot) = self.create_entry_alloca(ptr_type.into(), "temp_slot") else {
            return;
        };
        if self.builder.build_store(slot, value).is_err() {
            return;
        }
        self.temp_values.push((ptr, slot));
    }

    /// Register an owned inline-enum temporary that the current statement
    /// produced but did not bind to a variable (a discarded `Enum.Variant(x)`
    /// statement, or an owned enum match subject), so it is released at the
    /// statement boundary and on any early-return path rather than leaking its
    /// constructor-retained payload (issue #298 review). Spilled into a
    /// zero-initialized entry-block struct alloca, mirroring `register_temp`, so
    /// cleanup can drop it from any later block (null-safe on paths that never
    /// produced it). No-op for a non-struct value or an enum with no pointer
    /// payload; infallible by design (a spill failure just leaves it untracked).
    pub(super) fn register_enum_temp(&mut self, value: BasicValueEnum<'a>, enum_name: &str) {
        if !value.is_struct_value() || !self.enum_has_rc_payload(enum_name) {
            return;
        }
        if self.enum_temp_values.iter().any(|(v, _, _)| *v == value) {
            return;
        }
        let Some(BasicTypeEnum::StructType(struct_type)) = self.type_map.get(enum_name).copied()
        else {
            return;
        };
        let Ok(slot) = self.create_entry_alloca(struct_type.into(), "enum_temp_slot") else {
            return;
        };
        if self.builder.build_store(slot, value).is_err() {
            return;
        }
        self.enum_temp_values
            .push((value, slot, enum_name.to_string()));
    }

    /// Remove an inline-enum value from the pending-temporary set because its
    /// ownership has been transferred (stored into a variable or field slot, or
    /// merged into a ternary's phi result). After this it is not dropped at the
    /// statement boundary. Returns whether it was tracked.
    pub(super) fn untrack_enum_temp(&mut self, value: BasicValueEnum<'a>) -> bool {
        if let Some(pos) = self
            .enum_temp_values
            .iter()
            .rposition(|(v, _, _)| *v == value)
        {
            self.enum_temp_values.remove(pos);
            return true;
        }
        false
    }

    /// Current number of registered temporaries. Capture this before evaluating
    /// a full expression, then pass it to `cleanup_temps_to` afterwards to
    /// decrement only the temporaries produced by that expression.
    pub(super) fn temp_mark(&self) -> (usize, usize, usize) {
        (
            self.temp_values.len(),
            self.closure_temp_values.len(),
            self.enum_temp_values.len(),
        )
    }

    /// Remove a value from the pending-temporary list because its ownership has
    /// been transferred (e.g. stored into a variable slot or returned). After
    /// this the value is no longer decremented at the statement boundary.
    /// Returns `true` if the value was a tracked owned temporary, `false`
    /// otherwise (e.g. a borrowed identifier/parameter load or a non-pointer).
    pub(super) fn untrack_temp(&mut self, value: BasicValueEnum<'a>) -> bool {
        if value.is_pointer_value() {
            let ptr = value.into_pointer_value();
            // Remove the most recent matching entry; the transferred value is
            // typically the last temporary produced. Removing it from the list
            // is sufficient - cleanup only ever iterates `temp_values`, so the
            // now-untracked slot is never loaded or decremented again.
            if let Some(pos) = self.temp_values.iter().rposition(|(p, _)| *p == ptr) {
                self.temp_values.remove(pos);
                return true;
            }
        }
        false
    }

    /// Emit `mux_rc_dec` for every pointer temporary registered since `mark` by
    /// loading its slot, then null the slot so later path cleanup is harmless.
    /// This does not mutate the pending-temporary list: branch paths can emit
    /// the same cleanup from the saved slots, while fallthrough statement
    /// cleanup decides when the compile-time list is truncated.
    fn emit_pointer_temps_since(&mut self, mark: usize) -> Result<(), String> {
        if self.temp_values.len() > mark {
            let live = self.current_block_is_live();
            if live {
                let rc_dec = self
                    .runtime_function("mux_rc_dec")
                    .ok_or("mux_rc_dec not found")?;
                let ptr_type = self.context.ptr_type(AddressSpace::default());
                let null_ptr = ptr_type.const_null();
                let slots: Vec<PointerValue<'a>> = self.temp_values[mark..]
                    .iter()
                    .map(|(_, slot)| *slot)
                    .collect();
                for slot in slots {
                    let loaded = self
                        .builder
                        .build_load(ptr_type, slot, "temp_load")
                        .map_err(|e| e.to_string())?;
                    self.builder
                        .build_call(rc_dec, &[loaded.into()], "rc_dec_temp")
                        .map_err(|e| e.to_string())?;
                    // Null the slot so a later blanket cleanup (or the next loop
                    // iteration reusing this slot) does not decrement it again.
                    self.builder
                        .build_store(slot, null_ptr)
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        Ok(())
    }

    /// Emit all temporary cleanup registered since `mark` without mutating the
    /// pending lists. Used by non-fallthrough loop exits (`break`/`continue`):
    /// those branches jump around the surrounding statement's fallthrough
    /// cleanup, but still have to release temporaries created earlier in that
    /// statement, such as a `match accept()` result wrapper.
    pub(super) fn emit_temps_since_mark(
        &mut self,
        mark: (usize, usize, usize),
    ) -> Result<(), String> {
        self.emit_closure_temps_since(mark.1)?;
        self.emit_pointer_temps_since(mark.0)?;
        self.emit_enum_temps_since(mark.2)
    }

    /// Emit `mux_rc_dec` for every temporary registered since `mark` by loading
    /// its slot (null-safe), then truncate the list back to `mark`. Call at
    /// statement boundaries. Skips emission when the current block already has a
    /// terminator (dead code).
    pub(super) fn cleanup_temps_to(&mut self, mark: (usize, usize, usize)) -> Result<(), String> {
        self.emit_temps_since_mark(mark)?;
        self.closure_temp_values.truncate(mark.1);
        self.temp_values.truncate(mark.0);
        self.enum_temp_values.truncate(mark.2);
        Ok(())
    }

    /// Release all per-iteration temporaries accumulated during a single loop
    /// body pass. Cleans up statement temporaries (pointer and enum), and
    /// closure temporaries registered since `mark`, then truncates so the next
    /// iteration starts with a clean set. Skips emission in a dead block.
    pub(super) fn emit_loop_iteration_cleanup(
        &mut self,
        mark: (usize, usize, usize),
    ) -> Result<(), String> {
        self.cleanup_temps_to(mark)
    }

    /// Release the inline-enum temporaries registered since `mark`, dropping each
    /// via `emit_enum_drop` and then zeroing its spill slot so a later blanket
    /// cleanup or a loop iteration reusing the slot does not drop it again. Skips
    /// emission in an already-terminated block, mirroring the pointer path.
    fn emit_enum_temps_since(&mut self, mark: usize) -> Result<(), String> {
        if self.enum_temp_values.len() <= mark {
            return Ok(());
        }
        if self.current_block_is_live() {
            let entries: Vec<(PointerValue<'a>, String)> = self.enum_temp_values[mark..]
                .iter()
                .map(|(_, slot, name)| (*slot, name.clone()))
                .collect();
            for (slot, enum_name) in entries {
                self.emit_enum_drop(&enum_name, slot)?;
                if let Some(BasicTypeEnum::StructType(struct_type)) =
                    self.type_map.get(&enum_name).copied()
                {
                    self.builder
                        .build_store(slot, struct_type.const_zero())
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        Ok(())
    }

    /// Decrement every registered temporary (used on the return path, after the
    /// returned value has been retained).
    ///
    /// Unlike `cleanup_temps_to`, this does NOT truncate the pending set. A
    /// function can return from several alternative branches (e.g. an `if`/`else`
    /// where each arm returns, or an early `return` inside a loop); each branch
    /// is a distinct runtime path that must release the same still-live
    /// temporaries, so the set has to survive for the sibling branches' returns.
    /// Emitting the same decrement on more than one branch is safe: each
    /// temporary's slot is a null-initialized entry-block alloca, so on a path
    /// that never produced the value the load yields null and the null-safe
    /// `mux_rc_dec` is a no-op. Skips emission in an already-terminated block.
    pub(super) fn cleanup_all_temps(&mut self) -> Result<(), String> {
        if !self.current_block_is_live() {
            return Ok(());
        }
        if !self.temp_values.is_empty() {
            let rc_dec = self
                .runtime_function("mux_rc_dec")
                .ok_or("mux_rc_dec not found")?;
            let ptr_type = self.context.ptr_type(AddressSpace::default());
            let slots: Vec<PointerValue<'a>> =
                self.temp_values.iter().map(|(_, slot)| *slot).collect();
            for slot in slots {
                let loaded = self
                    .builder
                    .build_load(ptr_type, slot, "temp_load")
                    .map_err(|e| e.to_string())?;
                self.builder
                    .build_call(rc_dec, &[loaded.into()], "rc_dec_temp")
                    .map_err(|e| e.to_string())?;
            }
        }
        // Drop every inline-enum temporary too (no truncate: sibling return
        // branches share the set; each slot is zero-initialized so a path that
        // never produced the value drops a null payload, a no-op).
        let enum_entries: Vec<(PointerValue<'a>, String)> = self
            .enum_temp_values
            .iter()
            .map(|(_, slot, name)| (*slot, name.clone()))
            .collect();
        for (slot, enum_name) in enum_entries {
            self.emit_enum_drop(&enum_name, slot)?;
        }
        Ok(())
    }

    /// Drop the temporaries registered since `mark` WITHOUT releasing them - used
    /// when their ownership has been transferred somewhere that will free them
    /// (e.g. stored into an object field, which the destructor decrements). They
    /// must not also be decremented at the statement boundary.
    pub(super) fn discard_temps_to(&mut self, mark: (usize, usize, usize)) {
        self.temp_values.truncate(mark.0);
        self.closure_temp_values.truncate(mark.1);
        self.enum_temp_values.truncate(mark.2);
    }

    /// Deep-clone a reference-counted value, returning a fresh, uniquely-owned,
    /// refcount-isolated copy (`mux_value_deep_clone`).
    pub(super) fn deep_clone_value(
        &mut self,
        ptr: PointerValue<'a>,
    ) -> Result<PointerValue<'a>, String> {
        let clone_fn = self
            .runtime_function("mux_value_deep_clone")
            .ok_or("mux_value_deep_clone not found")?;
        let cloned = self
            .builder
            .build_call(clone_fn, &[ptr.into()], "value_copy")
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or("mux_value_deep_clone returned no value")?;
        Ok(cloned.into_pointer_value())
    }

    /// Produce the boxed pointer to store into a variable/constant slot, taking
    /// ownership of it. Three cases:
    ///  - a freshly produced owned temporary is transferred as-is (untracked so
    ///    it is not also freed at the statement boundary);
    ///  - a borrowed reference-counted value of a copy type (an identifier or
    ///    field load bound with `auto x = y`) is deep-cloned, giving the binding
    ///    value semantics: an independent, uniquely-owned copy that can be freed
    ///    by scope cleanup without touching the source and cannot alias-mutate
    ///    it. References and function values are stored by handle, not copied;
    ///  - a scalar is boxed into a fresh owned value.
    pub(super) fn box_value_owned_for_slot(
        &mut self,
        value: BasicValueEnum<'a>,
        resolved_type: &Type,
    ) -> Result<PointerValue<'a>, String> {
        let boxed = self.box_value(value);
        if self.untrack_temp(boxed.into()) {
            // Was a tracked owned temporary: transfer ownership unchanged.
            return Ok(boxed);
        }
        let is_copy_type = self.type_needs_rc_tracking(resolved_type)
            && !matches!(resolved_type, Type::Reference(_) | Type::Function { .. });
        if value.is_pointer_value() && is_copy_type {
            // Borrowed value-type binding: copy so the new slot owns it.
            return self.deep_clone_value(boxed);
        }
        Ok(boxed)
    }

    /// Overwrite a variable/reference slot with a new value under value
    /// semantics, releasing the previous occupant so reassignment (`x = ...`,
    /// `x++`, `*r = ...`) does not leak it.
    ///
    /// The new owned value is produced first (a fresh temporary is transferred,
    /// a borrowed value type is deep-cloned) and only then is the old value
    /// decremented. Computing the copy before the release makes self-assignment
    /// (`x = x`) and aliasing assignment (`x = y`) safe: the independent copy
    /// already exists before the old reference is dropped. The old value is only
    /// released for value-type slots that uniquely own their contents; reference
    /// and function slots hold a borrowed handle and must not be decremented.
    /// Overwrite a slot under value semantics, driven by the slot's own
    /// storage rather than by the value's type.
    ///
    /// The two can disagree, and did: a closure capture cell always holds a
    /// `*mut Value` so the runtime can release it, and a variable whose address
    /// is taken keeps a boxed slot so `&x` means what `&list[0]` means - in both
    /// cases the type says scalar while the storage is a pointer. Writing a raw
    /// scalar into those cells because the type said `int` is how `count++`
    /// inside a lambda came to store an integer where a pointer was expected.
    ///
    pub(super) fn overwrite_slot_of_type(
        &mut self,
        slot: PointerValue<'a>,
        slot_type: BasicTypeEnum<'a>,
        value: BasicValueEnum<'a>,
        resolved_type: &Type,
    ) -> Result<(), String> {
        // A scalar slot holds the value itself: nothing to release, nothing to
        // box. Reaching the boxed path below would allocate a `Value` per
        // assignment and then decrement the slot's raw contents as if it were a
        // pointer.
        if let Some(scalar) = self
            .scalar_slot_type(resolved_type)
            .filter(|_| !slot_type.is_pointer_type())
        {
            let narrowed = self.coerce_to_scalar(value, scalar)?;
            // Read out of the box first, then give its reference back. A caller
            // may hand over an owned box - a list element copy, a call result -
            // expecting the slot to take ownership of it, which is what boxing
            // into the slot used to do. A scalar slot holds the value itself and
            // cannot keep the box, so the transfer ends here instead. Only a
            // tracked temporary is released; a borrowed load owns nothing.
            if value.is_pointer_value() && self.untrack_temp(value) {
                self.emit_value_decref(value.into_pointer_value())?;
            }
            self.builder
                .build_store(slot, narrowed)
                .map_err(|e| e.to_string())?;
            return Ok(());
        }

        let owned = self.box_value_owned_for_slot(value, resolved_type)?;
        let owns_contents = self.slot_owns_boxed_contents(slot_type, resolved_type);
        if owns_contents {
            let ptr_type = self.context.ptr_type(AddressSpace::default());
            let rc_dec = self
                .runtime_function("mux_rc_dec")
                .ok_or("mux_rc_dec not found")?;
            let old = self
                .builder
                .build_load(ptr_type, slot, "old_slot_val")
                .map_err(|e| e.to_string())?;
            self.builder
                .build_call(rc_dec, &[old.into()], "rc_dec_old")
                .map_err(|e| e.to_string())?;
        }
        self.builder
            .build_store(slot, owned)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Replace the contents of a slot that uniquely owns a boxed value.
    ///
    /// The caller must already have transferred ownership of `boxed` out of
    /// temporary tracking. Releasing the old occupant here is safe because this
    /// slot, unlike a borrowed reference slot, owns exactly one reference.
    pub(super) fn overwrite_owned_boxed_slot(
        &self,
        slot: PointerValue<'a>,
        boxed: PointerValue<'a>,
    ) -> Result<(), String> {
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let old = self
            .builder
            .build_load(ptr_type, slot, "old_owned_boxed_slot")
            .map_err(|e| e.to_string())?
            .into_pointer_value();
        self.emit_value_decref(old)?;
        self.builder
            .build_store(slot, boxed)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Overwrite a variable slot that holds an owned boxed pointer with a new
    /// boxed value, transferring the new value's ownership into the slot so it
    /// is not also freed as a statement temporary. Only valid for slots that
    /// store boxed `*mut Value` pointers (not inline struct/enum storage).
    ///
    /// The previous occupant is intentionally NOT decremented here. The compiler
    /// still has borrow-without-retain sites (an alias can hold a slot's value
    /// without owning a reference), so eagerly releasing the old value can free
    /// something still in use. Leaking the overwritten value is recoverable;
    /// a use-after-free is not. Reclaiming overwritten values needs the same
    /// dominance-aware ownership tracking as the rest of the ARC work.
    pub(super) fn store_boxed_into_slot(
        &mut self,
        slot: PointerValue<'a>,
        boxed: PointerValue<'a>,
    ) -> Result<(), String> {
        self.builder
            .build_store(slot, boxed)
            .map_err(|e| e.to_string())?;
        self.untrack_temp(boxed.into());
        Ok(())
    }

    /// Check if a type requires RC tracking.
    /// Currently all boxed values (primitives, strings, objects) use RC.
    /// Whether a slot of this storage holding a value of this type owns the box
    /// inside it, and so must release the previous occupant when overwritten
    /// and its final occupant at scope end.
    ///
    /// Storage rather than type, because the two disagree wherever a scalar is
    /// kept boxed: a closure capture cell, and a variable whose address is
    /// taken. A scalar in its own slot owns nothing; the same scalar in a
    /// pointer slot owns a box. Reference and function slots hold a borrowed
    /// handle and own nothing either way.
    /// Register a captured variable's cell for release at scope end.
    ///
    /// The variable holds one reference to its own storage; each closure
    /// capturing it holds another, so the cell outlives whichever goes first.
    pub(super) fn track_cell_variable(&mut self, name: &str, cell: PointerValue<'a>) {
        self.track_slot(name, RcSlot::Cell(cell));
    }

    pub(super) fn slot_owns_boxed_contents(
        &self,
        slot_type: BasicTypeEnum<'a>,
        resolved_type: &Type,
    ) -> bool {
        slot_type.is_pointer_type()
            && (self.type_needs_rc_tracking(resolved_type)
                || self.scalar_slot_type(resolved_type).is_some())
            && !matches!(resolved_type, Type::Reference(_) | Type::Function { .. })
    }

    pub(super) fn type_needs_rc_tracking(&self, ty: &Type) -> bool {
        match ty {
            // A scalar lives in its slot, so there is no reference to count.
            // `string` is the exception among the primitives: it is a
            // reference-counted heap value, so its slot holds a pointer.
            Type::Primitive(_) => self.scalar_slot_type(ty).is_none(),
            // Named types (classes) are RC-allocated
            Type::Named(_, _) => true,
            // Generic types that resolve to RC types
            Type::Generic(_) | Type::Variable(_) => true,
            // Collections contain Values which are RC-allocated
            Type::List(_) | Type::Map(_, _) | Type::Set(_) => true,
            // Tuples contain Values which are RC-allocated
            Type::Tuple(_, _) => true,
            // Optional contains boxed values
            Type::Optional(_) => true,
            // Result contains boxed values
            Type::Result(_, _) => true,
            // References are pointers to RC values
            Type::Reference(_) => true,
            // Function types are pointers, not RC
            Type::Function { .. } => false,
            // Void doesn't need tracking
            Type::Void | Type::Never => false,
            // Empty collections don't need tracking
            Type::EmptyList | Type::EmptyMap | Type::EmptySet => false,
            // Instantiated types (like Pair<string, bool>) need RC
            Type::Instantiated(_, _) => true,
            // Module references don't need RC
            Type::Module(_) => false,
        }
    }

    /// Increment the RC of a value if it's an RC-allocated pointer.
    /// Returns the same value. Use this before cleanup when returning a value.
    pub(super) fn rc_inc_if_pointer(
        &mut self,
        value: BasicValueEnum<'a>,
    ) -> Result<BasicValueEnum<'a>, String> {
        if value.is_pointer_value() {
            let rc_inc = self
                .runtime_function("mux_rc_inc")
                .ok_or("mux_rc_inc not found")?;
            self.builder
                .build_call(rc_inc, &[value.into()], "rc_inc_return")
                .map_err(|e| e.to_string())?;
        }
        Ok(value)
    }
}
