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
    /// Decrement the payload's refcount, releasing the value.
    Drop,
    /// Replace the payload with an independent `mux_value_deep_clone` of itself.
    DeepClone,
}

impl EnumPayloadOp {
    /// The runtime function applied to each pointer payload.
    fn runtime_fn_name(self) -> &'static str {
        match self {
            EnumPayloadOp::Drop => "mux_rc_dec",
            EnumPayloadOp::DeepClone => "mux_value_deep_clone",
        }
    }

    /// Short slug used in generated basic-block names.
    fn label(self) -> &'static str {
        match self {
            EnumPayloadOp::Drop => "drop",
            EnumPayloadOp::DeepClone => "clone",
        }
    }
}

impl<'a> CodeGenerator<'a> {
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
    /// by value) owns its result. A ternary owns its result when both arms do -
    /// e.g. `cond ? Status.Active(x) : Status.Inactive(y)` is owned - so it must
    /// not be misclassified as borrowed. Every other form (identifier, field or
    /// index load) is treated as borrowed and deep-cloned, which is always safe.
    pub(super) fn rhs_produces_owned_enum(kind: &crate::ast::ExpressionKind) -> bool {
        match kind {
            crate::ast::ExpressionKind::Call { .. } => true,
            crate::ast::ExpressionKind::If {
                then_expr,
                else_expr,
                ..
            } => {
                Self::rhs_produces_owned_enum(&then_expr.kind)
                    && Self::rhs_produces_owned_enum(&else_expr.kind)
            }
            _ => false,
        }
    }

    /// If `ty` is a user-declared enum (not the built-in `optional`/`result`,
    /// which are boxed `*mut Value`s rather than inline structs), return its
    /// name. Used to decide whether a struct-valued local needs enum drop-glue.
    pub(super) fn user_enum_type_name(&self, ty: &Type) -> Option<String> {
        let Type::Named(name, _) = ty else {
            return None;
        };
        if name == "optional" || name == "result" {
            return None;
        }
        self.enum_variants.contains_key(name).then(|| name.clone())
    }

    /// Whether any variant of `enum_name` carries at least one pointer (RC)
    /// payload field. Enums that only hold inline scalars (a plain C-style enum)
    /// own nothing and need no drop-glue, so tracking and `emit_enum_drop` skip
    /// them entirely.
    pub(super) fn enum_has_rc_payload(&self, enum_name: &str) -> bool {
        let Some(variants) = self.enum_variants.get(enum_name).cloned() else {
            return false;
        };
        variants.iter().any(|variant| {
            self.variant_field_types(enum_name, variant)
                .map(|fields| {
                    (0..fields.len()).any(|i| {
                        self.variant_field_llvm_type(enum_name, variant, &fields, i)
                            .map(|t| t.is_pointer_type())
                            .unwrap_or(false)
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
    fn materialize_owned_enum(
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
        let temp = self.create_entry_alloca(struct_type.into(), "enum_copy_src")?;
        self.builder
            .build_store(temp, value)
            .map_err(|e| e.to_string())?;
        self.emit_enum_deep_clone(enum_name, temp)?;
        let owned = self
            .builder
            .build_load(struct_type, temp, "enum_copy")
            .map_err(|e| e.to_string())?;
        Ok(owned)
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

        // One switch case per variant that actually owns a pointer payload.
        let variants = self
            .enum_variants
            .get(enum_name)
            .cloned()
            .ok_or_else(|| format!("Enum {} has no variant list", enum_name))?;
        let mut cases = Vec::new();
        for variant in &variants {
            let fields = self.variant_field_types(enum_name, variant)?;
            let pointer_fields: Vec<usize> = (0..fields.len())
                .filter(|&i| {
                    self.variant_field_llvm_type(enum_name, variant, &fields, i)
                        .map(|t| t.is_pointer_type())
                        .unwrap_or(false)
                })
                .collect();
            if pointer_fields.is_empty() {
                continue;
            }
            let index = self.get_variant_index(enum_name, variant)?;
            let case_block = self
                .context
                .append_basic_block(function, &format!("enum_{label}_{enum_name}_{variant}"));
            self.builder.position_at_end(case_block);
            for field_index in pointer_fields {
                self.emit_enum_payload_field_op(
                    struct_type,
                    struct_alloca,
                    field_index,
                    runtime_fn,
                    op,
                )?;
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
    /// `mark`, then truncate. The statement-boundary sibling of
    /// `cleanup_temps_to`.
    pub(super) fn cleanup_closure_temps_to(&mut self, mark: usize) -> Result<(), String> {
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
        self.closure_temp_values.truncate(mark);
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

    /// Current number of registered temporaries. Capture this before evaluating
    /// a full expression, then pass it to `cleanup_temps_to` afterwards to
    /// decrement only the temporaries produced by that expression.
    pub(super) fn temp_mark(&self) -> (usize, usize) {
        (self.temp_values.len(), self.closure_temp_values.len())
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

    /// Emit `mux_rc_dec` for every temporary registered since `mark` by loading
    /// its slot (null-safe), then truncate the list back to `mark`. Call at
    /// statement boundaries. Skips emission when the current block already has a
    /// terminator (dead code).
    pub(super) fn cleanup_temps_to(&mut self, mark: (usize, usize)) -> Result<(), String> {
        self.cleanup_closure_temps_to(mark.1)?;
        let mark = mark.0;
        if self.temp_values.len() <= mark {
            return Ok(());
        }
        let live = self
            .builder
            .get_insert_block()
            .is_some_and(|bb| bb.get_terminator().is_none());
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
        self.temp_values.truncate(mark);
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
        let live = self
            .builder
            .get_insert_block()
            .is_some_and(|bb| bb.get_terminator().is_none());
        if !live || self.temp_values.is_empty() {
            return Ok(());
        }
        let rc_dec = self
            .runtime_function("mux_rc_dec")
            .ok_or("mux_rc_dec not found")?;
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let slots: Vec<PointerValue<'a>> = self.temp_values.iter().map(|(_, slot)| *slot).collect();
        for slot in slots {
            let loaded = self
                .builder
                .build_load(ptr_type, slot, "temp_load")
                .map_err(|e| e.to_string())?;
            self.builder
                .build_call(rc_dec, &[loaded.into()], "rc_dec_temp")
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Drop the temporaries registered since `mark` WITHOUT releasing them - used
    /// when their ownership has been transferred somewhere that will free them
    /// (e.g. stored into an object field, which the destructor decrements). They
    /// must not also be decremented at the statement boundary.
    pub(super) fn discard_temps_to(&mut self, mark: (usize, usize)) {
        self.temp_values.truncate(mark.0);
        self.closure_temp_values.truncate(mark.1);
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
    pub(super) fn overwrite_slot_with_owned(
        &mut self,
        slot: PointerValue<'a>,
        value: BasicValueEnum<'a>,
        resolved_type: &Type,
    ) -> Result<(), String> {
        let owned = self.box_value_owned_for_slot(value, resolved_type)?;
        let owns_contents = self.type_needs_rc_tracking(resolved_type)
            && !matches!(resolved_type, Type::Reference(_) | Type::Function { .. });
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
    pub(super) fn type_needs_rc_tracking(&self, ty: &Type) -> bool {
        match ty {
            // Primitives are boxed, so they need RC tracking
            Type::Primitive(_) => true,
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
