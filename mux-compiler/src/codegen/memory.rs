//! Reference counting (RC) scope management for CodeGenerator.
//!
//! This module handles tracking RC-allocated variables and generating cleanup code.

use super::CodeGenerator;
use crate::semantics::Type;
use inkwell::AddressSpace;
use inkwell::values::{BasicValueEnum, PointerValue};

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
        let all_vars: Vec<(String, PointerValue<'a>)> = self
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

    /// Generate mux_rc_dec calls for a list of variables.
    pub(super) fn generate_cleanup_for_vars(
        &mut self,
        vars: &[(String, PointerValue<'a>)],
    ) -> Result<(), String> {
        let rc_dec = self
            .runtime_function("mux_rc_dec")
            .ok_or("mux_rc_dec not found")?;
        let ptr_type = self.context.ptr_type(AddressSpace::default());

        for (name, alloca) in vars {
            // Load the pointer value from the alloca
            let value = self
                .builder
                .build_load(ptr_type, *alloca, &format!("rc_load_{}", name))
                .map_err(|e| e.to_string())?;

            // Call mux_rc_dec
            self.builder
                .build_call(rc_dec, &[value.into()], &format!("rc_dec_{}", name))
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Track an RC-allocated variable in the current scope.
    /// The variable will have mux_rc_dec called on it when the scope ends.
    pub(super) fn track_rc_variable(&mut self, name: &str, alloca: PointerValue<'a>) {
        if let Some(current_scope) = self.rc_scope_stack.last_mut() {
            // Avoid duplicate tracking of the same storage slot inside a scope.
            // Duplicates can lead to double decrements during cleanup.
            if current_scope.iter().any(|(_, p)| *p == alloca) {
                return;
            }
            current_scope.push((name.to_string(), alloca));
        }
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
    fn deep_clone_value(&mut self, ptr: PointerValue<'a>) -> Result<PointerValue<'a>, String> {
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
