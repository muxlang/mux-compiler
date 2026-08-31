//! Ownership tracking for closure values during code generation.
//!
//! Closures are not boxed runtime `Value`s, so they need their own retain and
//! release operations. This module owns the statement-temporary and scope
//! cleanup paths for closure values while `memory` handles ordinary RC values
//! and inline enums.

use super::CodeGenerator;
use inkwell::AddressSpace;
use inkwell::values::{BasicValueEnum, PointerValue};

impl<'a> CodeGenerator<'a> {
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
    /// ownership is transferred to a binding or return value first. Spilled
    /// into a null-initialized entry-block slot exactly like `register_temp`,
    /// so it is dominance-safe and null-safe on paths that never produced it.
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

    /// Transfer ownership of a closure temporary out of the pending set (e.g.
    /// it was stored into a variable or returned). Returns whether it was
    /// tracked.
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
    pub(super) fn emit_closure_temps_since(&mut self, mark: usize) -> Result<(), String> {
        if self.closure_temp_values.len() <= mark {
            return Ok(());
        }
        let live = self.current_block_is_live();
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
        let live = self.current_block_is_live();
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
                .build_load(ptr_type, *alloca, &format!("closure_load_{name}"))
                .map_err(|e| e.to_string())?;
            self.builder
                .build_call(release, &[value.into()], &format!("closure_release_{name}"))
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
}
