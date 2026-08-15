//! Statement generation for the code generator.
//!
//! This module handles:
//! - Variable declarations (auto and typed)
//! - Return statements
//! - If statements with proper block handling
//! - While and for loops
//! - Expression statements

use inkwell::AddressSpace;
use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::{BasicValueEnum, FunctionValue};

use crate::ast::{
    EnumVariantField, ExpressionKind, ExpressionNode, LiteralNode, PatternNode, PrimitiveType,
    StatementKind, StatementNode,
};
use crate::semantics::{Type, Type as ResolvedType};

use super::CodeGenerator;

impl<'a> CodeGenerator<'a> {
    fn declare_variable(
        &mut self,
        name: &str,
        var_type: BasicTypeEnum<'a>,
        value: BasicValueEnum<'a>,
        resolved_type: &ResolvedType,
        function: Option<&FunctionValue<'a>>,
        rhs_owned: bool,
    ) -> Result<(), String> {
        if self.try_declare_closure_variable(name, value, resolved_type, function)? {
            return Ok(());
        }

        // Only a binding in THIS scope is the same variable being redeclared -
        // a loop-local on its second iteration, or a top-level declaration
        // reusing its pre-declared global slot. A binding of the same name
        // further out is a different variable that this one shadows, and
        // writing into its slot would change it from inside a block it does not
        // belong to.
        let existing_var = self.variables.get_in_current_scope(name).cloned();

        if let Some((existing_ptr, existing_slot_type, _)) = existing_var {
            if value.is_struct_value() {
                // Reassigning an inline enum struct into an existing slot (a
                // re-declared local or a pre-declared global) must release the
                // previous value's pointer payloads first, or it leaks them
                // (issue #290), and a borrowed copy is deep-cloned so it owns an
                // independent value (issue #298). The slot holds a valid enum or
                // is zero-initialized, both null-safe for the release.
                self.store_struct_value(existing_ptr, value, resolved_type, rhs_owned, true)?;
            } else {
                // Re-declaring an existing slot (a loop-local declared each
                // iteration, or a pre-declared top-level global) must release the
                // previous occupant, or every iteration but the last leaks.
                // Global slots are zero-initialized and locals hold their prior
                // owned value, so the null-safe release is always correct.
                self.overwrite_slot_of_type(
                    existing_ptr,
                    existing_slot_type,
                    value,
                    resolved_type,
                )?;
            }
        } else if value.is_struct_value() {
            self.declare_struct_variable(
                name,
                var_type,
                value,
                resolved_type,
                function,
                rhs_owned,
            )?;
        } else if let Some(func) = function {
            self.declare_local_in_function(name, value, resolved_type, func)?;
        } else {
            self.declare_local_without_function(name, value, resolved_type)?;
        }
        Ok(())
    }

    /// Bind a fresh local inside a function, in an entry-block slot.
    ///
    /// The slot is hoisted and null-initialized because the store runs on every
    /// pass: a variable declared inside a loop body is stored to on each
    /// iteration. So it goes through `overwrite_slot_of_type`, which releases
    /// the previous occupant, and the first pass sees the null init where that
    /// release is a no-op.
    ///
    /// Which slot depends on the variable: a captured one shares a cell, a
    /// scalar lives in a slot of its own width, everything else in a
    /// `*mut Value` slot. Split out of `declare_variable` to keep that
    /// dispatcher's cognitive complexity down.
    fn declare_local_in_function(
        &mut self,
        name: &str,
        value: BasicValueEnum<'a>,
        resolved_type: &ResolvedType,
        func: &FunctionValue<'a>,
    ) -> Result<(), String> {
        let ptr_slot = self.context.ptr_type(AddressSpace::default()).into();
        let (slot, slot_type) = if self.captured_names.contains(name) {
            let cell = self.create_entry_block_cell(*func, name)?;
            self.track_cell_variable(name, cell);
            (cell, ptr_slot)
        } else {
            let slot_type = self
                .scalar_slot_for_binding(name, resolved_type)
                .unwrap_or(ptr_slot);
            let alloca = self.create_entry_block_alloca(*func, slot_type, name)?;
            // Tracked when the slot owns a box - which a scalar's slot does when
            // its address is taken, since that keeps it boxed.
            if self.slot_owns_boxed_contents(slot_type, resolved_type) {
                self.track_rc_variable(name, alloca);
            }
            (alloca, slot_type)
        };
        self.variables
            .insert(name.to_string(), (slot, slot_type, resolved_type.clone()));
        self.overwrite_slot_of_type(slot, slot_type, value, resolved_type)
    }

    /// Bind a fresh local with no enclosing function, which is module-init and
    /// top-level statement territory.
    ///
    /// The alloca here is at the current position rather than hoisted, so it is
    /// not null-initialized and no previous occupant can be released; the value
    /// is simply stored. Split out of `declare_variable` for the same reason as
    /// the function case above.
    fn declare_local_without_function(
        &mut self,
        name: &str,
        value: BasicValueEnum<'a>,
        resolved_type: &ResolvedType,
    ) -> Result<(), String> {
        let scalar = self.scalar_slot_for_binding(name, resolved_type);
        let slot_type =
            scalar.unwrap_or_else(|| self.context.ptr_type(AddressSpace::default()).into());
        let alloca = self
            .builder
            .build_alloca(slot_type, name)
            .map_err(|e| e.to_string())?;
        let stored = match scalar {
            Some(scalar_type) => self.coerce_to_scalar(value, scalar_type)?,
            None => self.box_value_owned_for_slot(value, resolved_type)?.into(),
        };
        self.builder
            .build_store(alloca, stored)
            .map_err(|e| e.to_string())?;
        self.variables
            .insert(name.to_string(), (alloca, slot_type, resolved_type.clone()));
        Ok(())
    }

    /// Bind a fresh inline-struct local (an enum value, most importantly), which
    /// is stored by value rather than as a boxed pointer. Split out of
    /// `declare_variable` to keep that dispatcher's cognitive complexity down.
    fn declare_struct_variable(
        &mut self,
        name: &str,
        var_type: BasicTypeEnum<'a>,
        value: BasicValueEnum<'a>,
        resolved_type: &ResolvedType,
        function: Option<&FunctionValue<'a>>,
        rhs_owned: bool,
    ) -> Result<(), String> {
        let (alloca, zero_initialized) = if let Some(func) = function {
            (self.create_entry_block_alloca(*func, var_type, name)?, true)
        } else {
            (
                self.builder
                    .build_alloca(var_type, name)
                    .map_err(|e| e.to_string())?,
                false,
            )
        };
        // An entry-block enum slot is created once but a declaration inside a
        // loop body reuses it every iteration; release the previous iteration's
        // value before overwriting, or all but the last leak (issue #290), and
        // deep-clone a borrowed copy so it owns an independent value (issue
        // #298). Releasing the old value is only safe for the zero-initialized
        // entry-block slot (its first-iteration null payload makes the drop a
        // no-op); the rare no-function fallback alloca holds garbage, so it is
        // stored without a release.
        self.store_struct_value(alloca, value, resolved_type, rhs_owned, zero_initialized)?;
        self.variables
            .insert(name.to_string(), (alloca, var_type, resolved_type.clone()));
        // Inline enum structs own the pointer payloads of their active variant;
        // track the slot so those payloads are released when the scope ends.
        // Plain value structs (classes are boxed, not inline) and scalar-only
        // enums own nothing here and are skipped.
        if let Some(enum_name) = self.user_enum_type_name(resolved_type)
            && self.enum_has_rc_payload(&enum_name)
        {
            self.track_enum_variable(name, &enum_name, alloca);
        }
        Ok(())
    }

    /// Bind a closure-typed variable. Closures are not RC `Value`s; they carry
    /// their own refcount and are released with mux_closure_release. If the bound
    /// value is an owned closure temporary, transfer ownership into a tracked
    /// closure variable so the scope releases it; a borrowed closure (a parameter
    /// or an alias of another variable) is stored without tracking. Returns
    /// `true` when it handled the declaration.
    fn try_declare_closure_variable(
        &mut self,
        name: &str,
        value: BasicValueEnum<'a>,
        resolved_type: &ResolvedType,
        function: Option<&FunctionValue<'a>>,
    ) -> Result<bool, String> {
        if !matches!(resolved_type, Type::Function { .. }) || !value.is_pointer_value() {
            return Ok(false);
        }
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        // An entry-block slot is zero-initialized and is re-executed by a loop,
        // so the store below can be overwriting the previous iteration's
        // closure. A slot allocated at the current position is fresh each time
        // control reaches it and holds nothing to release.
        let (alloca, slot_may_hold_previous) = match function {
            Some(func) => (
                self.create_entry_block_alloca(*func, ptr_type.into(), name)?,
                true,
            ),
            None => (
                self.builder
                    .build_alloca(ptr_type, name)
                    .map_err(|e| e.to_string())?,
                false,
            ),
        };
        // Decided before the store, because it decides whether this slot owns
        // what it holds. An owned closure temporary transfers its reference into
        // the slot; a borrowed one (a parameter, or an alias of another
        // variable) is stored without tracking and the slot owns nothing.
        let takes_ownership = self.untrack_closure_temp(value);
        if takes_ownership && slot_may_hold_previous {
            // Release what the previous iteration left here, or every iteration
            // but the last leaks its closure - a loop running n times leaked
            // n - 1. `overwrite_slot_with_owned` does this for reference-counted
            // values and deliberately skips a closure, which needs
            // `mux_closure_release` to walk and release its captures. The slot
            // is zero-initialized and the release is null-safe, so the first
            // iteration releases nothing. Only an owning slot is released: a
            // borrowed alias never held a reference to give back.
            let release = self
                .runtime_function("mux_closure_release")
                .ok_or("mux_closure_release not found")?;
            let previous = self
                .builder
                .build_load(ptr_type, alloca, "old_closure")
                .map_err(|e| e.to_string())?;
            self.builder
                .build_call(release, &[previous.into()], "release_old_closure")
                .map_err(|e| e.to_string())?;
        }
        self.builder
            .build_store(alloca, value)
            .map_err(|e| e.to_string())?;
        self.variables.insert(
            name.to_string(),
            (alloca, ptr_type.into(), resolved_type.clone()),
        );
        if takes_ownership {
            self.track_closure_variable(name, alloca);
        }
        Ok(true)
    }

    /// Give a `for` loop's variable its slot and report the slot back, so the
    /// per-iteration store can be driven by the storage rather than the type.
    ///
    /// A scalar element lives in a slot of its own width. Anything else gets a
    /// null-initialized entry-block slot: the per-iteration overwrite decrements
    /// the previous occupant, so the first iteration must see a null (the
    /// null-safe dec is then a no-op), and a zero-iteration loop must leave a
    /// null the scope cleanup can safely decrement.
    fn bind_loop_variable(
        &mut self,
        function: &FunctionValue<'a>,
        var: &str,
        resolved_var_type: &ResolvedType,
    ) -> Result<(inkwell::values::PointerValue<'a>, BasicTypeEnum<'a>), String> {
        let ptr_slot = self.context.ptr_type(AddressSpace::default()).into();
        // A captured loop variable gets the shared cell too, so a closure built
        // in the body writes to the variable the body reads.
        if self.captured_names.contains(var) {
            let cell = self.create_entry_block_cell(*function, var)?;
            self.track_cell_variable(var, cell);
            self.variables
                .insert(var.to_string(), (cell, ptr_slot, resolved_var_type.clone()));
            return Ok((cell, ptr_slot));
        }
        let slot_type = self
            .scalar_slot_for_binding(var, resolved_var_type)
            .unwrap_or(ptr_slot);
        let var_alloca = self.create_entry_block_alloca(*function, slot_type, var)?;
        self.variables.insert(
            var.to_string(),
            (var_alloca, slot_type, resolved_var_type.clone()),
        );
        // The loop variable owns its element for the whole loop when the slot is
        // a boxed one; release that final occupant at function return or
        // scope end.
        if self.slot_owns_boxed_contents(slot_type, resolved_var_type) {
            self.track_rc_variable(var, var_alloca);
        }
        Ok((var_alloca, slot_type))
    }

    /// Emit a `for` loop's body, then the index increment and the jump back to
    /// the header. Shared by the counting and list-iterating loops, which differ
    /// only in how they test the index and fetch the element.
    fn close_loop_iteration(
        &mut self,
        function: &FunctionValue<'a>,
        body: &[StatementNode],
        index_alloca: inkwell::values::PointerValue<'a>,
        index_load: BasicValueEnum<'a>,
        header_bb: inkwell::basic_block::BasicBlock<'a>,
    ) -> Result<(), String> {
        for stmt in body {
            self.generate_statement(stmt, Some(function))?;
        }
        let one = self.context.i64_type().const_int(1, false);
        let new_index = self
            .builder
            .build_int_add(index_load.into_int_value(), one, "inc")
            .map_err(|e| e.to_string())?;
        self.builder
            .build_store(index_alloca, new_index)
            .map_err(|e| e.to_string())?;
        self.builder
            .build_unconditional_branch(header_bb)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn generate_for_statement_inner(
        &mut self,
        function: &FunctionValue<'a>,
        var: &str,
        var_type: &crate::ast::TypeNode,
        iter: &ExpressionNode,
        body: &[StatementNode],
    ) -> Result<(), String> {
        // `range(a, b)` counts rather than materializing a list, so it keeps its
        // own lowering. Everything else is iterated as a list, whatever
        // expression produced it - `generate_list_for_loop` evaluates the
        // operand like any other. Restricting it to a bare identifier meant a
        // method call, a field or a function result could not be iterated at
        // all, and failed as an internal error rather than a diagnostic.
        if let ExpressionKind::Call { func, args } = &iter.kind
            && let ExpressionKind::Identifier(name) = &func.kind
            && name == "range"
            && args.len() == 2
        {
            return self.generate_range_for_loop(function, var, &args[0], &args[1], body);
        }
        self.generate_list_for_loop(function, var, var_type, iter, body)
    }

    /// `for <var> in range(<start>, <end>)`: iterate the integers [start, end).
    fn generate_range_for_loop(
        &mut self,
        function: &FunctionValue<'a>,
        var: &str,
        start: &ExpressionNode,
        end: &ExpressionNode,
        body: &[StatementNode],
    ) -> Result<(), String> {
        let resolved_var_type = Type::Primitive(PrimitiveType::Int);
        let start_val = self.generate_expression(start)?;
        let end_val = self.generate_expression(end)?;
        let index_type = self.context.i64_type();
        let index_alloca = self
            .builder
            .build_alloca(index_type, "index")
            .map_err(|e| e.to_string())?;
        self.builder
            .build_store(index_alloca, start_val)
            .map_err(|e| e.to_string())?;
        let (var_alloca, slot_type) = self.bind_loop_variable(function, var, &resolved_var_type)?;
        let label_id = self.label_counter;
        self.label_counter += 1;
        let header_bb = self
            .context
            .append_basic_block(*function, &format!("for_header_{}", label_id));
        let body_bb = self
            .context
            .append_basic_block(*function, &format!("for_body_{}", label_id));
        let exit_bb = self
            .context
            .append_basic_block(*function, &format!("for_exit_{}", label_id));
        self.builder
            .build_unconditional_branch(header_bb)
            .map_err(|e| e.to_string())?;
        self.builder.position_at_end(header_bb);
        let index_load = self
            .builder
            .build_load(index_type, index_alloca, "index_load")
            .map_err(|e| e.to_string())?;
        let cmp = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLT,
                index_load.into_int_value(),
                end_val.into_int_value(),
                "cmp",
            )
            .map_err(|e| e.to_string())?;
        self.builder
            .build_conditional_branch(cmp, body_bb, exit_bb)
            .map_err(|e| e.to_string())?;
        self.builder.position_at_end(body_bb);
        let index_load2 = self
            .builder
            .build_load(index_type, index_alloca, "index_load2")
            .map_err(|e| e.to_string())?;
        // Box the current index and transfer it into the loop slot,
        // releasing the previous iteration's boxed index so the loop
        // does not accumulate leaks.
        self.overwrite_slot_of_type(var_alloca, slot_type, index_load2, &resolved_var_type)?;
        self.close_loop_iteration(function, body, index_alloca, index_load2, header_bb)?;
        self.builder.position_at_end(exit_bb);
        Ok(())
    }

    /// `for <var> in <list-identifier>`: iterate a list value's elements.
    fn generate_list_for_loop(
        &mut self,
        function: &FunctionValue<'a>,
        var: &str,
        var_type: &crate::ast::TypeNode,
        iter: &ExpressionNode,
        body: &[StatementNode],
    ) -> Result<(), String> {
        let resolved_var_type = self
            .analyzer
            .resolve_type(var_type)
            .map_err(|e| e.message)?;
        let mut list_val = self.generate_expression(iter)?;
        // A string iterates as its characters. Converting the subject here
        // keeps the loop itself list-only, so nothing below needs a string
        // case (#389).
        if matches!(
            self.resolve_expression_type_with_fallback(iter),
            Ok(crate::semantics::Type::Primitive(
                crate::ast::PrimitiveType::Str
            ))
        ) {
            let cstr = self.string_value_to_cstr(list_val)?;
            list_val = self.call_runtime_function("mux_string_to_list", &[cstr])?;
            self.free_cstrings(&[cstr])?;
            self.register_temp(list_val);
        }
        let len_call = self
            .builder
            .build_call(
                self.runtime_function("mux_value_list_length")
                    .expect("mux_value_list_length must be declared in runtime"),
                &[list_val.into()],
                "list_len",
            )
            .map_err(|e| e.to_string())?;
        let len_val = len_call
            .try_as_basic_value()
            .basic()
            .expect("mux_value_list_length should return a basic value")
            .into_int_value();
        let index_type = self.context.i64_type();
        let index_alloca = self
            .builder
            .build_alloca(index_type, "index")
            .map_err(|e| e.to_string())?;
        let zero = self.context.i64_type().const_int(0, false);
        self.builder
            .build_store(index_alloca, zero)
            .map_err(|e| e.to_string())?;
        let (var_alloca, slot_type) = self.bind_loop_variable(function, var, &resolved_var_type)?;
        let label_id = self.label_counter;
        self.label_counter += 1;
        let header_bb = self
            .context
            .append_basic_block(*function, &format!("for_header_{}", label_id));
        let body_bb = self
            .context
            .append_basic_block(*function, &format!("for_body_{}", label_id));
        let exit_bb = self
            .context
            .append_basic_block(*function, &format!("for_exit_{}", label_id));
        self.builder
            .build_unconditional_branch(header_bb)
            .map_err(|e| e.to_string())?;
        self.builder.position_at_end(header_bb);
        let index_load = self
            .builder
            .build_load(index_type, index_alloca, "index_load")
            .map_err(|e| e.to_string())?;
        let cmp = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLT,
                index_load.into_int_value(),
                len_val,
                "cmp",
            )
            .map_err(|e| e.to_string())?;
        self.builder
            .build_conditional_branch(cmp, body_bb, exit_bb)
            .map_err(|e| e.to_string())?;
        self.builder.position_at_end(body_bb);
        let index_load2 = self
            .builder
            .build_load(index_type, index_alloca, "index_load2")
            .map_err(|e| e.to_string())?;
        let get_call = self
            .builder
            .build_call(
                self.runtime_function("mux_value_list_get_value")
                    .expect("mux_value_list_get_value must be declared in runtime"),
                &[list_val.into(), index_load2.into()],
                "list_get_value",
            )
            .map_err(|e| e.to_string())?;
        let value_ptr = get_call
            .try_as_basic_value()
            .basic()
            .expect("mux_value_list_get_value should return a basic value")
            .into_pointer_value();
        // `mux_value_list_get_value` returns an owned (+1) copy of the
        // element. Registering it lets `overwrite_slot_with_owned` transfer
        // that ownership into the slot (rather than deep-cloning it, which
        // would leak the copy) while releasing the previous iteration's
        // element so long-running loops do not accumulate leaks.
        self.register_temp(value_ptr.into());
        self.overwrite_slot_of_type(var_alloca, slot_type, value_ptr.into(), &resolved_var_type)?;
        self.close_loop_iteration(function, body, index_alloca, index_load2, header_bb)?;
        let continue_bb = self
            .context
            .append_basic_block(*function, &format!("for_continue_{}", label_id));
        self.builder.position_at_end(exit_bb);
        self.builder
            .build_unconditional_branch(continue_bb)
            .map_err(|e| e.to_string())?;
        self.builder.position_at_end(continue_bb);
        Ok(())
    }

    fn prepare_match_expression(
        &mut self,
        expr: &ExpressionNode,
    ) -> Result<(BasicValueEnum<'a>, ExpressionNode), String> {
        if matches!(
            &expr.kind,
            ExpressionKind::Identifier(_) | ExpressionKind::FieldAccess { .. }
        ) || matches!(&expr.kind, ExpressionKind::Call { func, .. } if matches!(func.kind, ExpressionKind::Identifier(_)))
        {
            return Ok((self.generate_expression(expr)?, expr.clone()));
        }

        let temp_val = self.generate_expression(expr)?;
        // A non-trivial match subject (method/module call like json.parse(...),
        // binary expression, etc.) produces an owned (+1) value. Register it so
        // it is released at the end of the match statement; the arms extract
        // their bindings by cloning from the subject, so releasing it after the
        // match is safe. (Identifier/field-access subjects are borrowed and take
        // the early-return path above without registration.)
        self.register_temp(temp_val);
        let temp_name = format!("match_temp_{}", self.label_counter);
        self.label_counter += 1;
        let actual_type = self
            .get_resolved_expression_type(expr)
            .map_err(|e| format!("Type inference failed: {}", e))?;
        // The spill slot holds whatever the subject is. A scalar subject is a
        // raw value, and a slot typed `*mut Value` would have the arms read it
        // as a pointer.
        let temp_type = self
            .scalar_slot_type(&actual_type)
            .unwrap_or_else(|| self.context.ptr_type(AddressSpace::default()).into());
        let temp_alloca = self
            .builder
            .build_alloca(temp_type, &temp_name)
            .map_err(|e| e.to_string())?;
        let spilled = match self.scalar_slot_type(&actual_type) {
            Some(scalar) => self.coerce_to_scalar(temp_val, scalar)?,
            None => temp_val,
        };
        self.builder
            .build_store(temp_alloca, spilled)
            .map_err(|e| e.to_string())?;
        self.variables
            .insert(temp_name.clone(), (temp_alloca, temp_type, actual_type));
        let temp_expr = ExpressionNode {
            kind: ExpressionKind::Identifier(temp_name),
            span: expr.span,
        };
        Ok((temp_val, temp_expr))
    }

    fn resolve_match_expression_type(
        &mut self,
        match_expr: &ExpressionNode,
    ) -> Result<Type, String> {
        if let Some(t) = self.resolve_match_identifier_type(match_expr) {
            return t;
        }

        if let Some(t) = self.resolve_match_self_field_type(match_expr) {
            return t;
        }

        self.get_resolved_expression_type(match_expr)
            .map_err(|e| format!("Type inference failed: {}", e))
    }

    fn resolve_match_identifier_type(
        &mut self,
        match_expr: &ExpressionNode,
    ) -> Option<Result<Type, String>> {
        let ExpressionKind::Identifier(name) = &match_expr.kind else {
            return None;
        };

        if name == "self" || name.starts_with("match_temp_") {
            return Some(
                self.variables
                    .get(name)
                    .or_else(|| self.global_variables.get(name))
                    .map(|(_, _, var_type)| var_type.clone())
                    .ok_or_else(|| {
                        let label = if name == "self" {
                            "Self"
                        } else {
                            "Temporary variable"
                        };
                        format!("{} {} not found", label, name)
                    }),
            );
        }

        None
    }

    fn resolve_match_self_field_type(
        &mut self,
        match_expr: &ExpressionNode,
    ) -> Option<Result<Type, String>> {
        let ExpressionKind::FieldAccess { expr, field } = &match_expr.kind else {
            return None;
        };
        let ExpressionKind::Identifier(obj) = &expr.kind else {
            return None;
        };
        if obj != "self" {
            return None;
        }

        let self_type = match self
            .variables
            .get("self")
            .or_else(|| self.global_variables.get("self"))
        {
            Some((_, _, t)) => t.clone(),
            None => return Some(Err("Self not found".to_string())),
        };

        let result = match &self_type {
            // Through the receiver's own type arguments: a specialized method of
            // `Slot<Color>` still declares its field as `T`, and an unsubstituted
            // `T` is not recognised as an enum, so `match self.item` fell through
            // to the value-comparison path and silently took the first arm.
            Type::Named(class_name, class_args) => {
                self.class_field_type_for_receiver(class_name, class_args, field)
            }
            _ => self
                .get_resolved_expression_type(match_expr)
                .map_err(|e| format!("Type inference failed: {}", e)),
        };
        Some(result)
    }

    /// Run `body` in its own variable scope, so a name it binds stops being
    /// visible when the block ends and does not disturb an outer binding of the
    /// same name.
    ///
    /// The table used to be flat for the whole function, which broke two ways.
    /// A binding made inside a block outlived it, so `auto x = 42` followed by
    /// `for int x in nums` left `x` naming the loop variable for the rest of the
    /// function. And because `declare_variable` reuses the slot of a live
    /// binding of the same name - which is how a loop-local reuses its storage
    /// each iteration instead of leaking one allocation per pass - a block that
    /// shadowed an outer name also wrote through to the outer variable's
    /// storage. A real scope answers both: the redeclaration that should reuse
    /// its slot is the one already bound in this same scope, and a shadowing
    /// declaration in an inner scope is a different variable that gets its own.
    ///
    /// RC cleanup is unaffected: it tracks allocas through the RC scope stack,
    /// not this table. The scope is closed on the error path too, so the table
    /// is never left half-open if error recovery is ever added.
    fn in_block_scope<R>(
        &mut self,
        body: impl FnOnce(&mut Self) -> Result<R, String>,
    ) -> Result<R, String> {
        self.variables.push_scope();
        let result = body(self);
        self.variables.pop_scope();
        result
    }

    fn generate_match_statement_inner(
        &mut self,
        function: &FunctionValue<'a>,
        expr: &ExpressionNode,
        arms: &[crate::ast::MatchArm],
    ) -> Result<(), String> {
        // Pattern bindings (e.g. `n` in `n if n % 2 == 0`) and any arm-body
        // locals are scoped to the match, like any other block. Left visible
        // afterwards, a later `auto n` would reuse the arm-local slot - an
        // alloca created inside a conditional arm block that does not dominate
        // the store, producing invalid LLVM IR ("instruction does not dominate
        // all uses").
        self.in_block_scope(|me| me.generate_match_body(function, expr, arms))
    }

    fn generate_match_body(
        &mut self,
        function: &FunctionValue<'a>,
        expr: &ExpressionNode,
        arms: &[crate::ast::MatchArm],
    ) -> Result<(), String> {
        let (expr_val, match_expr) = self.prepare_match_expression(expr)?;
        let match_expr_type = self.resolve_match_expression_type(&match_expr)?;
        let is_enum = self.is_enum_match_type(&match_expr_type);

        if is_enum {
            // An owned inline enum subject was tracked as an enum temporary when
            // produced (register_enum_temp at the constructor), and the arms only
            // borrow its payloads, so statement cleanup (fall-through) and return
            // cleanup (an arm that returns) release it - including when every arm
            // returns and the match-end block is unreachable. Nothing to do here.
            self.generate_enum_match(function, &match_expr, &match_expr_type, expr_val, arms)?;
        } else {
            self.generate_switch_match(function, &match_expr_type, expr_val, arms)?;
        }

        Ok(())
    }

    fn generate_auto_decl_statement(
        &mut self,
        name: &str,
        expr: &ExpressionNode,
        function: Option<&FunctionValue<'a>>,
    ) -> Result<(), String> {
        let value = self.generate_expression(expr)?;
        let resolved_type = self
            .resolve_expression_type_with_fallback(expr)
            .map_err(|e| format!("Failed to get type for {}: {}", name, e))?;
        let concrete_type = self
            .resolve_type(&resolved_type)
            .unwrap_or_else(|_| resolved_type.clone());
        let var_type = value.get_type();
        let rhs_owned = Self::rhs_produces_owned_enum(&expr.kind);
        self.declare_variable(name, var_type, value, &concrete_type, function, rhs_owned)
    }

    /// `Type name` with no initializer: allocate the slot and bind the name,
    /// but store nothing.
    ///
    /// The slot is zeroed rather than left as whatever the stack held, so a
    /// reference-counted type sees a null pointer rather than a wild one. That
    /// is defence in depth, not the guarantee - semantics rejects a read before
    /// the assignment (#393), so a well-formed program never observes it. But
    /// if that check is ever wrong, a null is a clean crash instead of a
    /// corrupted refcount on an arbitrary address.
    fn generate_uninit_decl_statement(
        &mut self,
        name: &str,
        type_node: &crate::ast::TypeNode,
        function: Option<&FunctionValue<'a>>,
    ) -> Result<(), String> {
        self.instantiate_generic_types_in_type_node(type_node)?;
        let var_type = self.llvm_type_from_mux_type(type_node)?;
        let resolved_type = self
            .analyzer
            .resolve_type(type_node)
            .map_err(|e| e.to_string())?;

        let zero = match var_type {
            BasicTypeEnum::IntType(t) => t.const_zero().into(),
            BasicTypeEnum::FloatType(t) => t.const_zero().into(),
            BasicTypeEnum::PointerType(t) => t.const_null().into(),
            BasicTypeEnum::StructType(t) => t.const_zero().into(),
            BasicTypeEnum::ArrayType(t) => t.const_zero().into(),
            BasicTypeEnum::VectorType(t) => t.const_zero().into(),
            BasicTypeEnum::ScalableVectorType(t) => t.const_zero().into(),
        };

        self.declare_variable(name, var_type, zero, &resolved_type, function, false)
    }

    fn generate_typed_decl_statement(
        &mut self,
        name: &str,
        type_node: &crate::ast::TypeNode,
        expr: &ExpressionNode,
        function: Option<&FunctionValue<'a>>,
    ) -> Result<(), String> {
        // A declaration naming a generic enum is the fourth place an
        // instantiation can first appear, after a construction site, a match
        // site and a function signature. Without this, `Box<int> b = ...`
        // resolved the LLVM type before anything had stamped out `Box$int`,
        // while the same line written with `auto` worked.
        self.instantiate_generic_types_in_type_node(type_node)?;
        let var_type = self.llvm_type_from_mux_type(type_node)?;
        let value = self.generate_expression(expr)?;
        let resolved_type = self
            .analyzer
            .resolve_type(type_node)
            .map_err(|e| format!("Failed to resolve type for {}: {}", name, e.message))?;
        let rhs_owned = Self::rhs_produces_owned_enum(&expr.kind);
        self.declare_variable(name, var_type, value, &resolved_type, function, rhs_owned)
    }

    fn generate_const_decl_statement(
        &mut self,
        name: &str,
        expr: &ExpressionNode,
        function: Option<&FunctionValue<'a>>,
    ) -> Result<(), String> {
        let value = self.generate_expression(expr)?;
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let resolved_type = self
            .resolve_expression_type_with_fallback(expr)
            .map_err(|e| format!("Failed to get type for {}: {}", name, e))?;

        // A scalar constant lives in its slot like any other scalar. Boxing it
        // wrote a pointer into a slot the readers load as a raw value, and left
        // the box owned by nobody.
        let scalar = self.scalar_slot_for_binding(name, &resolved_type);
        let stored: BasicValueEnum<'a> = match scalar {
            Some(scalar_type) => self.coerce_to_scalar(value, scalar_type)?,
            None => {
                let boxed = self.box_value(value);
                // Ownership of the boxed value transfers into the constant's
                // storage slot; do not also free it as a statement temporary.
                self.untrack_temp(boxed.into());
                boxed.into()
            }
        };
        let slot_type = scalar.unwrap_or(BasicTypeEnum::PointerType(ptr_type));

        // Only a slot in THIS scope is the same constant being initialized - a
        // module-level constant reusing its pre-declared global. A binding of
        // the same name further out is a different constant this one shadows,
        // and writing into its slot both clobbered it and, when the two had
        // different storage, wrote a raw scalar where readers load a pointer.
        let existing = self
            .variables
            .get_in_current_scope(name)
            .map(|(p, t, _)| (*p, *t));
        if let Some((existing_ptr, existing_slot_type)) = existing
            && existing_slot_type == slot_type
        {
            self.builder
                .build_store(existing_ptr, stored)
                .map_err(|e| e.to_string())?;
            return Ok(());
        }

        let alloca = if let Some(func) = function {
            self.create_entry_block_alloca(*func, slot_type, name)?
        } else {
            self.builder
                .build_alloca(slot_type, name)
                .map_err(|e| e.to_string())?
        };
        self.builder
            .build_store(alloca, stored)
            .map_err(|e| e.to_string())?;
        self.variables
            .insert(name.to_string(), (alloca, slot_type, resolved_type.clone()));
        if function.is_some() && self.type_needs_rc_tracking(&resolved_type) {
            self.track_rc_variable(name, alloca);
        }

        Ok(())
    }

    fn try_generate_bool_literal_return(&mut self, expr: &ExpressionNode) -> Result<bool, String> {
        if let ExpressionKind::Literal(LiteralNode::Boolean(b)) = &expr.kind
            && let Some(ResolvedType::Primitive(PrimitiveType::Bool)) =
                &self.current_function_return_type
        {
            self.generate_all_scopes_cleanup()?;
            let bool_val = self
                .context
                .bool_type()
                .const_int(if *b { 1 } else { 0 }, false);
            self.builder
                .build_return(Some(&bool_val))
                .map_err(|e| e.to_string())?;
            return Ok(true);
        }

        Ok(false)
    }

    fn return_bool_int_value(
        &mut self,
        int_val: inkwell::values::IntValue<'a>,
    ) -> Result<(), String> {
        self.generate_all_scopes_cleanup()?;
        if int_val.get_type().get_bit_width() == 1 {
            self.builder
                .build_return(Some(&int_val))
                .map_err(|e| e.to_string())?;
            return Ok(());
        }

        let bool_val = self
            .builder
            .build_int_truncate(int_val, self.context.bool_type(), "int_to_i1")
            .map_err(|e| e.to_string())?;
        self.builder
            .build_return(Some(&bool_val))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn return_bool_ptr_value(
        &mut self,
        ptr: inkwell::values::PointerValue<'a>,
    ) -> Result<(), String> {
        let get_bool_fn = self
            .runtime_function("mux_value_get_bool")
            .ok_or("mux_value_get_bool not found")?;
        let result = self
            .builder
            .build_call(get_bool_fn, &[ptr.into()], "get_bool")
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or("Call returned no value")?;
        self.generate_all_scopes_cleanup()?;
        let bool_val = self
            .builder
            .build_int_truncate(
                result.into_int_value(),
                self.context.bool_type(),
                "i32_to_i1",
            )
            .map_err(|e| e.to_string())?;
        self.builder
            .build_return(Some(&bool_val))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn return_bool_value(&mut self, value: BasicValueEnum<'a>) -> Result<(), String> {
        if value.is_int_value() {
            return self.return_bool_int_value(value.into_int_value());
        }
        if value.is_pointer_value() {
            return self.return_bool_ptr_value(value.into_pointer_value());
        }
        Err("Expected bool value or pointer".to_string())
    }

    fn return_list_value(&mut self, value: BasicValueEnum<'a>) -> Result<(), String> {
        if !value.is_pointer_value() {
            return Err("Expected pointer value for list return".to_string());
        }
        self.rc_inc_if_pointer(value)?;
        self.generate_all_scopes_cleanup()?;
        self.builder
            .build_return(Some(&value))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn return_boxed_complex_value(&mut self, value: BasicValueEnum<'a>) -> Result<(), String> {
        let boxed = match value {
            BasicValueEnum::PointerValue(_) => value,
            _ => self.box_value(value).into(),
        };
        self.rc_inc_if_pointer(boxed)?;
        self.generate_all_scopes_cleanup()?;
        self.builder
            .build_return(Some(&boxed))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Return a user enum by value. The function's LLVM return type is the enum
    /// struct itself (`{ i32, ... }`), so the inline struct is returned directly
    /// rather than boxed - boxing produced a `ret ptr` that failed module
    /// verification against the struct return type (issue #304). An owned result
    /// (a constructor/call) transfers ownership to the caller and is untracked so
    /// scope cleanup does not release its payloads; a borrowed value (`return s`)
    /// is deep-cloned first so the caller owns an independent value while cleanup
    /// still releases the local it came from.
    fn return_enum_value(
        &mut self,
        value: BasicValueEnum<'a>,
        enum_name: &str,
        rhs_owned: bool,
    ) -> Result<(), String> {
        let owned = self.materialize_owned_enum(value, enum_name, rhs_owned)?;
        self.untrack_enum_temp(value);
        self.generate_all_scopes_cleanup()?;
        self.builder
            .build_return(Some(&owned))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn generate_typed_return(
        &mut self,
        return_type: ResolvedType,
        value: BasicValueEnum<'a>,
        rhs_owned: bool,
    ) -> Result<(), String> {
        if let Some(enum_name) = self.user_enum_type_name(&return_type) {
            return self.return_enum_value(value, &enum_name, rhs_owned);
        }
        match return_type {
            ResolvedType::Primitive(PrimitiveType::Int) => {
                let raw = self.get_raw_int_value(value)?;
                self.generate_all_scopes_cleanup()?;
                self.builder
                    .build_return(Some(&raw))
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
            ResolvedType::Primitive(PrimitiveType::Float) => {
                let raw = self.get_raw_float_value(value)?;
                self.generate_all_scopes_cleanup()?;
                self.builder
                    .build_return(Some(&raw))
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
            ResolvedType::Primitive(PrimitiveType::Bool) => self.return_bool_value(value),
            ResolvedType::List(_) => self.return_list_value(value),
            ResolvedType::Function { .. } => {
                // A closure is not an RC Value: retain it (bump its own refcount)
                // so it survives this scope's cleanup, which releases the closure
                // temporary/variable that currently holds it.
                self.retain_closure(value)?;
                self.generate_all_scopes_cleanup()?;
                self.builder
                    .build_return(Some(&value))
                    .map_err(|e| e.to_string())?;
                Ok(())
            }
            _ => self.return_boxed_complex_value(value),
        }
    }

    fn generate_return_with_value(&mut self, expr: &ExpressionNode) -> Result<(), String> {
        if self.try_generate_bool_literal_return(expr)? {
            return Ok(());
        }

        let value = self.generate_expression(expr)?;
        if let Some(return_type) = self.current_function_return_type.clone() {
            // Returning an enum read out of a collection hands back the boxed
            // pointer where the signature says inline struct (issue #363).
            let value = self.coerce_boxed_enum_to_inline(value, &return_type)?;
            let rhs_owned = Self::rhs_produces_owned_enum(&expr.kind);
            return self.generate_typed_return(return_type, value, rhs_owned);
        }

        let boxed = self.box_value(value);
        self.rc_inc_if_pointer(boxed.into())?;
        self.generate_all_scopes_cleanup()?;
        self.builder
            .build_return(Some(&boxed))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn generate_if_statement(
        &mut self,
        function: &FunctionValue<'a>,
        cond: &ExpressionNode,
        then_block: &[StatementNode],
        else_block: &Option<Vec<StatementNode>>,
    ) -> Result<(), String> {
        let cond_val = self.generate_expression(cond)?;
        let cond_int = cond_val.into_int_value();
        let if_id = self.label_counter;
        self.label_counter += 1;
        let then_bb = self
            .context
            .append_basic_block(*function, &format!("if_then_{}", if_id));
        let else_bb = self
            .context
            .append_basic_block(*function, &format!("if_else_{}", if_id));
        let then_ends_with_return = then_block
            .last()
            .is_some_and(|s| matches!(s.kind, StatementKind::Return(_)));
        let else_ends_with_return = if let Some(else_stmts) = else_block {
            else_stmts
                .last()
                .is_some_and(|s| matches!(s.kind, StatementKind::Return(_)))
        } else {
            false
        };
        let needs_merge = !then_ends_with_return || !else_ends_with_return;
        let merge_bb = if needs_merge {
            Some(
                self.context
                    .append_basic_block(*function, &format!("if_merge_{}", if_id)),
            )
        } else {
            None
        };
        self.builder
            .build_conditional_branch(cond_int, then_bb, else_bb)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(then_bb);
        for stmt in then_block {
            self.generate_statement(stmt, Some(function))?;
        }
        if !then_ends_with_return && let Some(merge_bb) = merge_bb {
            self.builder
                .build_unconditional_branch(merge_bb)
                .map_err(|e| e.to_string())?;
        }

        self.builder.position_at_end(else_bb);
        if let Some(else_stmts) = else_block {
            for stmt in else_stmts {
                self.generate_statement(stmt, Some(function))?;
            }
        }
        if !else_ends_with_return && let Some(merge_bb) = merge_bb {
            self.builder
                .build_unconditional_branch(merge_bb)
                .map_err(|e| e.to_string())?;
        }

        if let Some(merge_bb) = merge_bb {
            self.builder.position_at_end(merge_bb);
        }

        Ok(())
    }

    fn generate_while_statement(
        &mut self,
        function: &FunctionValue<'a>,
        cond: &ExpressionNode,
        body: &[StatementNode],
    ) -> Result<(), String> {
        let header_bb = self.context.append_basic_block(*function, "while_header");
        let body_bb = self.context.append_basic_block(*function, "while_body");
        let exit_bb = self.context.append_basic_block(*function, "while_exit");
        self.builder
            .build_unconditional_branch(header_bb)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(header_bb);
        let cond_val = self.generate_expression(cond)?;
        let cond_int = cond_val.into_int_value();
        self.builder
            .build_conditional_branch(cond_int, body_bb, exit_bb)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(body_bb);
        for stmt in body {
            self.generate_statement(stmt, Some(function))?;
        }
        self.builder
            .build_unconditional_branch(header_bb)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(exit_bb);
        Ok(())
    }

    fn generate_nested_function_statement(
        &mut self,
        func: &crate::ast::FunctionNode,
        function: Option<&FunctionValue<'a>>,
    ) -> Result<(), String> {
        let parent_name = self
            .current_function_name
            .as_ref()
            .ok_or("Nested function outside of parent function")?;
        let mangled_name = format!("{}!{}", parent_name, func.name);

        self.function_nodes
            .insert(mangled_name.clone(), func.clone());

        if !self.functions.contains_key(&mangled_name) {
            let saved_insert_block = self.builder.get_insert_block();

            let return_type = self
                .analyzer
                .resolve_type(&func.return_type)
                .map_err(|e| e.to_string())?;
            let llvm_return_type = if matches!(return_type, Type::Void) {
                None
            } else {
                Some(self.llvm_type_from_mux_type(&func.return_type)?)
            };

            let mut param_types = vec![];
            for param in &func.params {
                param_types.push(self.llvm_type_from_mux_type(&param.type_)?.into());
            }

            let fn_type = if let Some(ret_type) = llvm_return_type {
                ret_type.fn_type(&param_types, false)
            } else {
                self.context.void_type().fn_type(&param_types, false)
            };

            let declared_function = self.module.add_function(&mangled_name, fn_type, None);
            self.functions
                .insert(mangled_name.clone(), declared_function);

            if let Some(block) = saved_insert_block {
                self.builder.position_at_end(block);
            }
        }

        let saved_function_name = self.current_function_name.clone();
        let saved_function_return_type = self.current_function_return_type.clone();
        let saved_variables = self.variables.clone();

        let mut mangled_func = func.clone();
        mangled_func.name = mangled_name.clone();
        self.generate_function(&mangled_func)?;

        self.current_function_name = saved_function_name;
        self.current_function_return_type = saved_function_return_type;
        self.variables = saved_variables;

        if let Some(current_fn) = function
            && let Some(block) = current_fn.get_last_basic_block()
        {
            self.builder.position_at_end(block);
        }

        Ok(())
    }

    pub(super) fn generate_statement(
        &mut self,
        stmt: &StatementNode,
        function: Option<&FunctionValue<'a>>,
    ) -> Result<(), String> {
        // The current block already ends in a terminator when the previous
        // statement made this point unreachable (e.g. a match whose arms all
        // return leaves its end block terminated with `unreachable`). Emitting
        // here would place instructions after the terminator and fail module
        // verification, so skip the dead statement instead.
        if self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_terminator())
            .is_some()
        {
            return Ok(());
        }
        // Statement boundary: any owned RC temporaries produced while evaluating
        // this statement's expressions and not transferred to a binding are
        // decremented here. `Return` manages its own temporaries (it must retain
        // the returned value across cleanup), so it is exempt.
        let temp_mark = self.temp_mark();
        let is_return = matches!(stmt.kind, StatementKind::Return(_));
        match &stmt.kind {
            StatementKind::AutoDecl(name, _, expr) => {
                self.generate_auto_decl_statement(name, expr, function)?;
            }
            StatementKind::TypedDecl(name, type_node, expr) => {
                self.generate_typed_decl_statement(name, type_node, expr, function)?;
            }
            StatementKind::UninitDecl(name, type_node) => {
                self.generate_uninit_decl_statement(name, type_node, function)?;
            }
            StatementKind::ConstDecl(name, _, expr) => {
                self.generate_const_decl_statement(name, expr, function)?;
            }
            StatementKind::Return(Some(expr)) => {
                self.generate_return_with_value(expr)?;
            }
            StatementKind::Return(None) => {
                self.generate_all_scopes_cleanup()?;
                self.builder.build_return(None).map_err(|e| e.to_string())?;
            }
            StatementKind::If {
                cond,
                then_block,
                else_block,
            } => {
                let function = *function.ok_or("If statement not in function")?;
                self.in_block_scope(|me| {
                    me.generate_if_statement(&function, cond, then_block, else_block)
                })?;
            }
            StatementKind::While { cond, body } => {
                let function = *function.ok_or("While statement not in function")?;
                self.in_block_scope(|me| me.generate_while_statement(&function, cond, body))?;
            }
            StatementKind::For {
                var,
                var_type,
                iter,
                body,
            } => {
                let function = *function.ok_or("For statement not in function")?;
                self.in_block_scope(|me| {
                    me.generate_for_statement_inner(&function, var, var_type, iter, body)
                })?;
            }
            StatementKind::Match { expr, arms } => {
                let function = function.ok_or("Match not in function")?;
                self.generate_match_statement_inner(function, expr, arms)?;
            }
            StatementKind::Expression(expr) => {
                // A discarded owned enum value (a bare `Enum.Variant(x)`
                // statement) is released by statement cleanup: it was tracked as
                // an enum temporary when produced (register_enum_temp at the
                // constructor) and never transferred to a slot.
                self.generate_expression(expr)?;
            }
            StatementKind::Function(func) => {
                self.generate_nested_function_statement(func, function)?;
            }
            _ => {} // skip other statement types for now
        }
        if !is_return {
            self.cleanup_temps_to(temp_mark)?;
        }
        Ok(())
    }

    /// Determines if a type should use enum-based (discriminant) matching.
    fn is_enum_match_type(&self, match_type: &Type) -> bool {
        match match_type {
            Type::Optional(_) | Type::Result(_, _) => true,
            Type::Named(name, _) => {
                // Check if it's an enum in the enum_variants map or symbol table
                if self.enum_variants.contains_key(name) {
                    return true;
                }
                if let Some(symbol) = self.analyzer.symbol_table().lookup(name) {
                    return symbol.kind == crate::semantics::SymbolKind::Enum;
                }
                false
            }
            _ => false,
        }
    }

    fn resolve_enum_match_name(&self, match_expr: &ExpressionNode) -> Result<String, String> {
        match &match_expr.kind {
            ExpressionKind::Identifier(name) => self.resolve_enum_match_name_from_identifier(name),
            ExpressionKind::FieldAccess { expr, field } => {
                self.resolve_enum_match_name_from_field_access(expr, field)
            }
            ExpressionKind::Call { func, .. } => {
                if let ExpressionKind::Identifier(constructor_name) = &func.kind {
                    return self.resolve_enum_constructor_match_name(constructor_name);
                }

                Err("Match expression constructor calls must be simple identifiers".to_string())
            }
            _ => Err(
                "Match expression must be identifier, field access, or constructor call"
                    .to_string(),
            ),
        }
    }

    fn enum_name_from_type(
        &self,
        value_type: &Type,
        unknown_type_msg: &str,
    ) -> Result<String, String> {
        match value_type {
            // A generic enum's codegen name carries its type arguments, so
            // `Box<int>` resolves to the `Box$int` instantiation (issue #359).
            Type::Named(n, args) => Ok(self.mangled_enum_name(n, args)),
            Type::Optional(_) => Ok("optional".to_string()),
            Type::Result(_, _) => Ok("result".to_string()),
            _ => Err(unknown_type_msg.to_string()),
        }
    }

    fn resolve_enum_match_name_from_identifier(&self, name: &str) -> Result<String, String> {
        if let Some((_, _, var_type)) = self.variables.get(name) {
            let unknown_msg = if name.starts_with("match_temp_") {
                format!("Match expression must be an enum type, got {:?}", var_type)
            } else {
                "Match expression must be an enum type".to_string()
            };
            return self.enum_name_from_type(var_type, &unknown_msg);
        }

        if name.starts_with("match_temp_") {
            return Err(format!("Temporary variable {} not found", name));
        }

        if let Some(symbol) = self.analyzer.symbol_table().lookup(name) {
            if let Some(symbol_type) = &symbol.type_ {
                return self
                    .enum_name_from_type(symbol_type, "Match expression must be an enum type");
            }
            return Err("Match expression must be an enum type".to_string());
        }

        Err(format!("Symbol {} not found", name))
    }

    fn resolve_enum_constructor_match_name(
        &self,
        constructor_name: &str,
    ) -> Result<String, String> {
        match constructor_name {
            "some" | "none" => Ok("optional".to_string()),
            "ok" | "err" => Ok("result".to_string()),
            _ => {
                if let Some(symbol) = self.analyzer.symbol_table().lookup(constructor_name) {
                    if let Some(Type::Named(type_name, _)) = &symbol.type_ {
                        Ok(type_name.clone())
                    } else {
                        Err("Constructor must be enum type".to_string())
                    }
                } else {
                    Err(format!("Constructor {} not found", constructor_name))
                }
            }
        }
    }

    fn resolve_enum_match_name_from_field_access(
        &self,
        expr: &ExpressionNode,
        field: &str,
    ) -> Result<String, String> {
        let ExpressionKind::Identifier(obj) = &expr.kind else {
            return Err(
                "Match expression must be identifier, self.field, or obj.field".to_string(),
            );
        };

        if obj == "self" {
            if let Some((_, _, Type::Named(class_name, class_args))) = self
                .variables
                .get("self")
                .or_else(|| self.global_variables.get("self"))
            {
                return self.resolve_enum_name_from_class_field(class_name, class_args, field);
            }
            return Err("Self not found".to_string());
        }

        if let Some((_, _, Type::Named(class_name, class_args))) = self
            .variables
            .get(obj)
            .or_else(|| self.global_variables.get(obj))
        {
            return self.resolve_enum_name_from_class_field(class_name, class_args, field);
        }

        if self
            .variables
            .get(obj)
            .or_else(|| self.global_variables.get(obj))
            .is_some()
        {
            return Err(format!("Variable {} is not a class instance", obj));
        }

        Err(format!("Variable {} not found", obj))
    }

    /// Name the enum a matched class field holds, as seen through the receiver.
    fn resolve_enum_name_from_class_field(
        &self,
        class_name: &str,
        class_args: &[Type],
        field: &str,
    ) -> Result<String, String> {
        let field_type = self.class_field_type_for_receiver(class_name, class_args, field)?;
        // The type arguments are carried into the name, so matching a `Box<int>`
        // field binds against the `Box$int` instantiation and not the base enum,
        // whose payload is still the type parameter (issue #359).
        self.enum_name_from_type(&field_type, "Match field must be enum type")
    }

    /// A class field's type as an instance of `class_name<class_args>` holds it.
    ///
    /// The field type comes from the class declaration, so in a generic class it
    /// is written in the class's type parameters: `Slot<T>.item` is declared `T`
    /// whatever the instance is. Left unsubstituted, `T` names neither an enum in
    /// the type map nor a type recognised as an enum at all, which is how
    /// matching a type-parameter field both failed outright and, from a
    /// specialized method's `self`, silently took the first arm. Substituting the
    /// receiver's arguments recovers the type the instance actually holds, and
    /// leaves a concrete field type untouched.
    fn class_field_type_for_receiver(
        &self,
        class_name: &str,
        class_args: &[Type],
        field: &str,
    ) -> Result<Type, String> {
        let field_type = self.class_field_type(class_name, field)?;
        Ok(match self.build_type_param_map(class_name, class_args) {
            Ok(type_param_map) => self.substitute_type_with_map(&field_type, &type_param_map),
            Err(_) => field_type,
        })
    }

    /// A class field's declared type, as the semantic analyzer recorded it.
    ///
    /// The analyzer's copy keeps a field written in a type parameter as a type
    /// variable, which is what makes it substitutable; the parsed declaration
    /// cannot distinguish the type parameter `T` from a class named `T`, so
    /// reading the field type from there leaves nothing for a receiver's type
    /// arguments to replace. The parsed declaration stays as the fallback for a
    /// class the analyzer has no symbol for.
    fn class_field_type(&self, class_name: &str, field: &str) -> Result<Type, String> {
        if let Some(class_symbol) = self.lookup_class_symbol(class_name)
            && let Some((field_type, _)) = class_symbol.fields.get(field)
        {
            return Ok(field_type.clone());
        }

        let fields = self
            .classes
            .get(class_name)
            .ok_or_else(|| format!("Class {} not found", class_name))?;
        let field_info = fields
            .iter()
            .find(|f| f.name == field)
            .ok_or_else(|| format!("Field {} not found in class {}", field, class_name))?;
        Ok(self.type_node_to_type(&field_info.type_))
    }

    fn emit_match_guard_and_body(
        &mut self,
        function: &FunctionValue<'a>,
        arm: &crate::ast::MatchArm,
        next_bb: inkwell::basic_block::BasicBlock<'a>,
        end_bb: inkwell::basic_block::BasicBlock<'a>,
        arm_index: usize,
    ) -> Result<(), String> {
        if let Some(guard) = &arm.guard {
            let guard_val = self.generate_expression(guard)?;
            let guard_pass_bb = self
                .context
                .append_basic_block(*function, &format!("match_guard_pass_{}", arm_index));
            self.builder
                .build_conditional_branch(guard_val.into_int_value(), guard_pass_bb, next_bb)
                .map_err(|e| e.to_string())?;
            self.builder.position_at_end(guard_pass_bb);
        }

        for stmt in &arm.body {
            self.generate_statement(stmt, Some(function))?;
        }
        if self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_terminator())
            .is_none()
        {
            self.builder
                .build_unconditional_branch(end_bb)
                .map_err(|e| e.to_string())?;
        }

        Ok(())
    }

    fn evaluate_switch_pattern_condition(
        &mut self,
        match_val: BasicValueEnum<'a>,
        match_expr_type: &Type,
        pattern: &PatternNode,
    ) -> Result<inkwell::values::IntValue<'a>, String> {
        match pattern {
            PatternNode::Literal(lit) => {
                let pattern_val = self.generate_literal(lit)?;
                self.generate_value_equality(match_val, pattern_val, match_expr_type)
            }
            PatternNode::Identifier(name) => {
                let is_constant = self
                    .analyzer
                    .symbol_table()
                    .lookup(name)
                    .map(|s| s.kind == crate::semantics::SymbolKind::Constant)
                    .unwrap_or(false);

                if is_constant {
                    let (const_ptr, const_slot_type, _) = self
                        .variables
                        .get(name)
                        .or_else(|| self.global_variables.get(name))
                        .ok_or_else(|| format!("Constant {} not found", name))?
                        .clone();

                    // Read through the constant's own slot: a scalar constant
                    // holds its value, so loading it as a pointer and unboxing
                    // dereferenced the value itself.
                    let loaded = self
                        .builder
                        .build_load(const_slot_type, const_ptr, &format!("load_{}", name))
                        .map_err(|e| e.to_string())?;

                    let const_val: BasicValueEnum<'a> = match match_expr_type {
                        Type::Primitive(PrimitiveType::Int)
                        | Type::Primitive(PrimitiveType::Char) => {
                            self.get_raw_int_value(loaded)?.into()
                        }
                        Type::Primitive(PrimitiveType::Bool) => {
                            self.get_raw_bool_value(loaded)?.into()
                        }
                        Type::Primitive(PrimitiveType::Float) => {
                            self.get_raw_float_value(loaded)?.into()
                        }
                        _ => loaded,
                    };

                    self.generate_value_equality(match_val, const_val, match_expr_type)
                } else {
                    let boxed = self.box_value(match_val);
                    let ptr_type = self.context.ptr_type(AddressSpace::default());
                    let alloca = self
                        .builder
                        .build_alloca(ptr_type, name)
                        .map_err(|e| e.to_string())?;
                    self.builder
                        .build_store(alloca, boxed)
                        .map_err(|e| e.to_string())?;
                    self.variables.insert(
                        name.clone(),
                        (alloca, ptr_type.into(), match_expr_type.clone()),
                    );
                    Ok(self.context.bool_type().const_int(1, false))
                }
            }
            PatternNode::Wildcard => Ok(self.context.bool_type().const_int(1, false)),
            PatternNode::List { elements, rest } => self.generate_list_pattern_check(
                match_val,
                match_expr_type,
                elements,
                rest.as_deref(),
            ),
            PatternNode::EnumVariant { .. } => {
                Err("Enum variant patterns are not valid in non-enum match".to_string())
            }
        }
    }

    fn enum_expr_ptr_for_payload_access(
        &mut self,
        enum_name: &str,
        expr_val: BasicValueEnum<'a>,
    ) -> Result<Option<inkwell::values::PointerValue<'a>>, String> {
        if enum_name != "optional" && enum_name != "result" {
            return Ok(None);
        }

        if expr_val.is_pointer_value() {
            return Ok(Some(expr_val.into_pointer_value()));
        }

        let struct_val = expr_val.into_struct_value();
        let alloca = self
            .builder
            .build_alloca(struct_val.get_type(), "temp_enum")
            .map_err(|e| e.to_string())?;
        self.builder
            .build_store(alloca, struct_val)
            .map_err(|e| e.to_string())?;
        Ok(Some(alloca))
    }

    fn enum_temp_struct_ptr(
        &mut self,
        enum_name: &str,
        expr_val: BasicValueEnum<'a>,
    ) -> Result<Option<inkwell::values::PointerValue<'a>>, String> {
        if expr_val.is_pointer_value() {
            return Ok(Some(expr_val.into_pointer_value()));
        }

        let struct_type = self
            .type_map
            .get(enum_name)
            .ok_or_else(|| format!("Enum {} not found in type map", enum_name))?
            .into_struct_type();
        let temp_ptr = self
            .builder
            .build_alloca(struct_type, "temp_enum_struct")
            .map_err(|e| e.to_string())?;
        self.builder
            .build_store(temp_ptr, expr_val)
            .map_err(|e| e.to_string())?;
        Ok(Some(temp_ptr))
    }

    fn enum_pattern_matches(
        &mut self,
        enum_name: &str,
        discriminant: inkwell::values::IntValue<'a>,
        pattern: &PatternNode,
    ) -> Result<inkwell::values::IntValue<'a>, String> {
        match pattern {
            PatternNode::EnumVariant { name, args: _ } => {
                let variant_index = self.get_variant_index(enum_name, name)?;
                self.build_discriminant_comparison(discriminant, variant_index)
            }
            // A bare identifier that names a variant of the matched enum is a
            // payload-less variant pattern, not a catch-all binding - a pure
            // C-style enum is matched with `Red`, `Green`, ... and each must
            // compare the discriminant rather than always match the first arm
            // (issue #307). A non-variant identifier is a genuine catch-all.
            PatternNode::Identifier(name) => match self.get_variant_index(enum_name, name) {
                Ok(variant_index) => {
                    self.build_discriminant_comparison(discriminant, variant_index)
                }
                Err(_) => Ok(self.context.bool_type().const_int(1, false)),
            },
            PatternNode::Literal(_) | PatternNode::Wildcard | PatternNode::List { .. } => {
                Ok(self.context.bool_type().const_int(1, false))
            }
        }
    }

    fn bind_builtin_enum_variant_args(
        &mut self,
        enum_name: &str,
        variant_name: &str,
        args: &[PatternNode],
        expr_ptr: inkwell::values::PointerValue<'a>,
        match_expr_type: &Type,
    ) -> Result<(), String> {
        if !matches!(variant_name, "some" | "ok" | "err") || args.is_empty() {
            return Ok(());
        }

        let PatternNode::Identifier(var) = &args[0] else {
            return Ok(());
        };

        let data_func = self.enum_data_function_name(enum_name)?;

        let func = self
            .runtime_function(data_func)
            .ok_or(format!("{} not found", data_func))?;
        let data_call = self
            .builder
            .build_call(func, &[expr_ptr.into()], "data_call")
            .map_err(|e| e.to_string())?;
        let data_ptr = data_call
            .try_as_basic_value()
            .basic()
            .expect("data function should return a basic value")
            .into_pointer_value();

        let (data_val, resolved_type) = self.extract_builtin_enum_variant_value(
            enum_name,
            variant_name,
            match_expr_type,
            data_ptr,
        )?;

        let boxed = self.box_value(data_val);
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let alloca = self.create_entry_alloca(ptr_type.into(), var)?;
        self.builder
            .build_store(alloca, boxed)
            .map_err(|e| e.to_string())?;

        // The bound payload is an owned (+1) value: scalar payloads are freshly
        // boxed here, and complex payloads (string/list/...) were cloned by the
        // `mux_*_data` extraction. `box_value` already registers freshly boxed
        // scalars as statement temporaries, but returns already-boxed pointers
        // untracked - so register the binding to guarantee it is released at the
        // end of the match statement (or brought down to caller-owned on a
        // returning arm). Dedups if already tracked.
        self.register_temp(boxed.into());

        self.variables
            .insert(var.clone(), (alloca, ptr_type.into(), resolved_type));
        Ok(())
    }

    fn enum_data_function_name(&self, enum_name: &str) -> Result<&'static str, String> {
        match enum_name {
            "optional" => Ok("mux_optional_data"),
            "result" => Ok("mux_result_data"),
            _ => Err(format!("Unknown enum {}", enum_name)),
        }
    }

    fn extract_builtin_enum_variant_value(
        &mut self,
        enum_name: &str,
        variant_name: &str,
        match_expr_type: &Type,
        data_ptr: inkwell::values::PointerValue<'a>,
    ) -> Result<(BasicValueEnum<'a>, Type), String> {
        if enum_name == "optional" {
            return self.extract_optional_variant_value(match_expr_type, data_ptr);
        }
        if enum_name == "result" {
            return self.extract_result_variant_value(variant_name, match_expr_type, data_ptr);
        }
        Err(format!("Unknown enum {}", enum_name))
    }

    fn extract_optional_variant_value(
        &mut self,
        match_expr_type: &Type,
        data_ptr: inkwell::values::PointerValue<'a>,
    ) -> Result<(BasicValueEnum<'a>, Type), String> {
        if let Type::Optional(inner_type) = match_expr_type {
            return self.extract_value_from_ptr(data_ptr, inner_type, "some");
        }
        Err(format!(
            "Type mismatch: expected Optional, got {:?}",
            match_expr_type
        ))
    }

    fn extract_result_variant_value(
        &mut self,
        variant_name: &str,
        match_expr_type: &Type,
        data_ptr: inkwell::values::PointerValue<'a>,
    ) -> Result<(BasicValueEnum<'a>, Type), String> {
        if let Type::Result(ok_type, err_type) = match_expr_type {
            let (target_type, variant) = if variant_name == "ok" {
                (ok_type, "ok")
            } else {
                (err_type, "err")
            };
            return self.extract_value_from_ptr(data_ptr, target_type, variant);
        }
        Err(format!(
            "Type mismatch: expected Result, got {:?}",
            match_expr_type
        ))
    }

    /// Bind a boxed recursive nested-enum payload in a match arm (issue #309):
    /// the slot at `data_ptr` holds a `Value::Opaque` box, so load it, unbox to
    /// the inner enum struct, and copy that into a fresh local of type
    /// `inner_struct`. The binding is a borrow - the matched enum still owns the
    /// box - so it is not tracked for cleanup, exactly like an inline nested-enum
    /// payload.
    fn bind_boxed_recursive_payload(
        &mut self,
        inner_struct: BasicTypeEnum<'a>,
        data_ptr: inkwell::values::PointerValue<'a>,
        var: &str,
    ) -> Result<(inkwell::values::PointerValue<'a>, BasicTypeEnum<'a>), String> {
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let box_ptr = self
            .builder
            .build_load(ptr_type, data_ptr, "boxed_ptr")
            .map_err(|e| e.to_string())?;
        let unbox_fn = self
            .runtime_function("mux_value_unbox_enum")
            .ok_or("mux_value_unbox_enum not found")?;
        let inner_data = self
            .builder
            .build_call(unbox_fn, &[box_ptr.into()], "unbox_match")
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or("mux_value_unbox_enum returned no value")?
            .into_pointer_value();
        let loaded = self
            .builder
            .build_load(inner_struct, inner_data, "unboxed")
            .map_err(|e| e.to_string())?;
        let alloca = self
            .builder
            .build_alloca(inner_struct, var)
            .map_err(|e| e.to_string())?;
        self.builder
            .build_store(alloca, loaded)
            .map_err(|e| e.to_string())?;
        Ok((alloca, inner_struct))
    }

    fn bind_custom_enum_variant_args(
        &mut self,
        enum_name: &str,
        variant_name: &str,
        args: &[PatternNode],
        temp_ptr: inkwell::values::PointerValue<'a>,
    ) -> Result<(), String> {
        let struct_type = self
            .type_map
            .get(enum_name)
            .ok_or_else(|| format!("Enum {} not found in type map", enum_name))?
            .into_struct_type();

        let field_types_clone = self.variant_field_types(enum_name, variant_name)?;

        for (i, arg) in args.iter().enumerate() {
            if let PatternNode::Identifier(var) = arg {
                let data_index = i + 1;
                let data_ptr = self
                    .builder
                    .build_struct_gep(struct_type, temp_ptr, data_index as u32, "data_ptr")
                    .map_err(|e| e.to_string())?;

                let field_type: BasicTypeEnum<'_> =
                    self.variant_field_llvm_type(enum_name, variant_name, &field_types_clone, i)?;

                let resolved_type = self.variant_field_resolved_type(
                    enum_name,
                    variant_name,
                    &field_types_clone,
                    i,
                )?;

                // A nested user-enum payload is kept as its inline struct value so
                // downstream uses see a real enum (loadable, re-matchable), not a
                // boxed Opaque (issue #306). A recursive nested enum lives behind a
                // heap box, so it is unboxed to its inner struct first (issue #309).
                // Any other payload is boxed to a *mut Value the way enum locals
                // expect.
                let (alloca, stored_type) = if self
                    .boxed_recursive_field(enum_name, variant_name, i)
                    .is_some()
                {
                    self.bind_boxed_recursive_payload(field_type, data_ptr, var)?
                } else if self
                    .nested_user_enum_name(&field_types_clone[i].1)
                    .is_some()
                {
                    let data_val = self
                        .builder
                        .build_load(field_type, data_ptr, "data")
                        .map_err(|e| e.to_string())?;
                    let alloca = self
                        .builder
                        .build_alloca(field_type, var)
                        .map_err(|e| e.to_string())?;
                    self.builder
                        .build_store(alloca, data_val)
                        .map_err(|e| e.to_string())?;
                    (alloca, field_type)
                } else {
                    let data_val = self
                        .builder
                        .build_load(field_type, data_ptr, "data")
                        .map_err(|e| e.to_string())?;
                    let boxed = self.box_value(data_val);
                    let ptr_type = self.context.ptr_type(AddressSpace::default());
                    let alloca = self
                        .builder
                        .build_alloca(ptr_type, var)
                        .map_err(|e| e.to_string())?;
                    self.builder
                        .build_store(alloca, boxed)
                        .map_err(|e| e.to_string())?;
                    (alloca, ptr_type.into())
                };

                // Note: for custom enums the payload is loaded directly from the
                // enum struct (a borrow), so it is intentionally NOT registered
                // as an owned temporary here - the enum owns it. Only scalar
                // payloads, which `box_value` freshly boxes and self-registers,
                // are released as temporaries.
                self.variables
                    .insert(var.clone(), (alloca, stored_type, resolved_type));
            }
        }

        Ok(())
    }

    pub(super) fn variant_field_types(
        &self,
        enum_name: &str,
        variant_name: &str,
    ) -> Result<Vec<EnumVariantField>, String> {
        let variant_fields = self
            .enum_variant_fields
            .get(enum_name)
            .ok_or_else(|| format!("No field information found for enum {}", enum_name))?;
        variant_fields
            .get(variant_name)
            .cloned()
            .ok_or_else(|| format!("Variant {} not found in enum {}", variant_name, enum_name))
    }

    pub(super) fn variant_field_llvm_type(
        &self,
        enum_name: &str,
        variant_name: &str,
        field_types: &[EnumVariantField],
        index: usize,
    ) -> Result<BasicTypeEnum<'a>, String> {
        if index >= field_types.len() {
            return Err(format!(
                "Field index {} out of bounds for enum variant {}.{} (has {} fields)",
                index,
                enum_name,
                variant_name,
                field_types.len()
            ));
        }
        self.type_kind_to_llvm_type(&field_types[index].1.kind)
    }

    fn variant_field_resolved_type(
        &mut self,
        enum_name: &str,
        variant_name: &str,
        field_types: &[EnumVariantField],
        index: usize,
    ) -> Result<Type, String> {
        if index >= field_types.len() {
            return Err(format!(
                "Field index {} out of bounds for enum variant {}.{} during type resolution (has {} fields)",
                index,
                enum_name,
                variant_name,
                field_types.len()
            ));
        }
        self.analyzer
            .resolve_type(&field_types[index].1)
            .map_err(|e| e.to_string())
    }

    fn bind_enum_arm_variables(
        &mut self,
        arm: &crate::ast::MatchArm,
        enum_name: &str,
        match_expr_type: &Type,
        expr_ptr_opt: Option<inkwell::values::PointerValue<'a>>,
        temp_ptr_opt: Option<inkwell::values::PointerValue<'a>>,
    ) -> Result<(), String> {
        let PatternNode::EnumVariant { name, args } = &arm.pattern else {
            return Ok(());
        };

        if let Some(expr_ptr) = expr_ptr_opt {
            return self.bind_builtin_enum_variant_args(
                enum_name,
                name,
                args,
                expr_ptr,
                match_expr_type,
            );
        }

        let temp_ptr = temp_ptr_opt.ok_or_else(|| "Temp pointer should be Some".to_string())?;
        self.bind_custom_enum_variant_args(enum_name, name, args, temp_ptr)
    }

    /// Unbox a boxed user-enum match subject (a `*mut Value`) into its inline
    /// struct value, so the discriminant load and payload binding operate on a
    /// real enum rather than a heap pointer (issue #309). Handles both a managed
    /// BoxedEnum and a raw Opaque via `mux_value_unbox_enum`.
    pub(super) fn unbox_enum_subject_value(
        &mut self,
        enum_name: &str,
        boxed: inkwell::values::PointerValue<'a>,
    ) -> Result<BasicValueEnum<'a>, String> {
        let struct_type = self
            .type_map
            .get(enum_name)
            .ok_or_else(|| format!("Enum {} not found in type map", enum_name))?
            .into_struct_type();
        let unbox_fn = self
            .runtime_function("mux_value_unbox_enum")
            .ok_or("mux_value_unbox_enum not found")?;
        let buf = self
            .builder
            .build_call(unbox_fn, &[boxed.into()], "unbox_subject")
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or("mux_value_unbox_enum returned no value")?
            .into_pointer_value();
        self.builder
            .build_load(struct_type, buf, "unboxed_subject")
            .map_err(|e| e.to_string())
    }

    /// Generate match code for enum types using discriminant-based comparison.
    fn generate_enum_match(
        &mut self,
        function: &FunctionValue<'a>,
        match_expr: &ExpressionNode,
        match_expr_type: &Type,
        expr_val: BasicValueEnum<'a>,
        arms: &[crate::ast::MatchArm],
    ) -> Result<(), String> {
        let enum_name = self.resolve_enum_match_name(match_expr).or_else(|_| {
            self.enum_name_from_type(match_expr_type, "Match expression must be an enum type")
        })?;
        // Matching a generic enum needs its instantiation to exist, since the
        // subject may have been constructed in another function (issue #359).
        if let Type::Named(base, args) = match_expr_type
            && !args.is_empty()
        {
            self.ensure_enum_instantiated(base, args)?;
        }
        // A user-enum subject is normally an inline struct value. When it arrives
        // as a pointer it is a boxed enum (a managed BoxedEnum or Opaque from a
        // collection element, e.g. `match items[i] { ... }`), so unbox it to the
        // inline struct the discriminant load and payload binding expect (issue
        // #309). Builtin optional/result stay boxed - they use their own
        // discriminant/payload runtime calls on the pointer.
        let expr_val = if !matches!(enum_name.as_str(), "optional" | "result")
            && expr_val.is_pointer_value()
        {
            self.unbox_enum_subject_value(&enum_name, expr_val.into_pointer_value())?
        } else {
            expr_val
        };
        let expr_ptr_opt = self.enum_expr_ptr_for_payload_access(&enum_name, expr_val)?;

        let discriminant = self.load_enum_discriminant(&enum_name, expr_val)?;
        let temp_ptr_opt = self.enum_temp_struct_ptr(&enum_name, expr_val)?;

        let mut current_bb = self
            .builder
            .get_insert_block()
            .expect("Builder should have an insertion block");
        let all_arms_return = arms.iter().all(|arm| {
            arm.body
                .last()
                .is_some_and(|s| matches!(s.kind, StatementKind::Return(_)))
        });
        let match_id = self.label_counter;
        self.label_counter += 1;
        let end_bb = self
            .context
            .append_basic_block(*function, &format!("match_end_{}", match_id));

        for (i, arm) in arms.iter().enumerate() {
            let arm_bb = self
                .context
                .append_basic_block(*function, &format!("match_arm_{}_{}", match_id, i));
            let next_bb = if i < arms.len() - 1 {
                self.context
                    .append_basic_block(*function, &format!("match_next_{}_{}", match_id, i))
            } else {
                end_bb
            };

            self.builder.position_at_end(current_bb);
            if current_bb.get_terminator().is_some() {
                current_bb = next_bb;
                continue;
            }

            let pattern_matches =
                self.enum_pattern_matches(&enum_name, discriminant, &arm.pattern)?;

            self.builder
                .build_conditional_branch(pattern_matches, arm_bb, next_bb)
                .map_err(|e| e.to_string())?;

            self.builder.position_at_end(arm_bb);

            // Each arm gets its own scope, so a pattern binding belongs to the
            // arm that introduced it. The match as a whole already opened one,
            // but that is not enough: without a per-arm scope, `ok(v)` was still
            // bound when the `err` arm was generated, so a later arm reading an
            // outer `v` resolved to the earlier arm's slot - which holds nothing
            // on the path where that arm did not run.
            self.in_block_scope(|me| {
                me.bind_enum_arm_variables(
                    arm,
                    &enum_name,
                    match_expr_type,
                    expr_ptr_opt,
                    temp_ptr_opt,
                )?;
                me.emit_match_guard_and_body(function, arm, next_bb, end_bb, i)
            })?;

            current_bb = next_bb;
        }

        self.builder.position_at_end(end_bb);
        if all_arms_return && end_bb.get_terminator().is_none() {
            self.builder
                .build_unreachable()
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Generate match code for non-enum types using equality-based comparison.
    fn generate_switch_match(
        &mut self,
        function: &FunctionValue<'a>,
        match_expr_type: &Type,
        match_val: BasicValueEnum<'a>,
        arms: &[crate::ast::MatchArm],
    ) -> Result<(), String> {
        let mut current_bb = self
            .builder
            .get_insert_block()
            .expect("Builder should have an insertion block");
        let all_arms_return = arms.iter().all(|arm| {
            arm.body
                .last()
                .is_some_and(|s| matches!(s.kind, StatementKind::Return(_)))
        });
        let match_id = self.label_counter;
        self.label_counter += 1;
        let end_bb = self
            .context
            .append_basic_block(*function, &format!("match_end_{}", match_id));

        for (i, arm) in arms.iter().enumerate() {
            let arm_bb = self
                .context
                .append_basic_block(*function, &format!("match_arm_{}_{}", match_id, i));
            let next_bb = if i < arms.len() - 1 {
                self.context
                    .append_basic_block(*function, &format!("match_next_{}_{}", match_id, i))
            } else {
                end_bb
            };

            self.builder.position_at_end(current_bb);
            if current_bb.get_terminator().is_some() {
                current_bb = next_bb;
                continue;
            }

            let condition =
                self.evaluate_switch_pattern_condition(match_val, match_expr_type, &arm.pattern)?;

            self.builder
                .build_conditional_branch(condition, arm_bb, next_bb)
                .map_err(|e| e.to_string())?;

            self.builder.position_at_end(arm_bb);

            // Per-arm scope, for the same reason as the enum path above: a
            // pattern binding must not still be visible when the next arm is
            // generated.
            self.in_block_scope(|me| {
                // For list patterns, bind variables after the condition check succeeds
                if let PatternNode::List { elements, rest } = &arm.pattern {
                    me.bind_list_pattern_variables(
                        match_val,
                        match_expr_type,
                        elements,
                        rest.as_deref(),
                    )?;
                }

                me.emit_match_guard_and_body(function, arm, next_bb, end_bb, i)
            })?;

            current_bb = next_bb;
        }

        self.builder.position_at_end(end_bb);
        if all_arms_return && end_bb.get_terminator().is_none() {
            self.builder
                .build_unreachable()
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Call a runtime function that returns an i32 and convert the result to an
    /// i1 bool (non-zero = true).
    fn call_runtime_bool(
        &mut self,
        left: inkwell::values::PointerValue<'a>,
        right: inkwell::values::PointerValue<'a>,
        func_name: &str,
        label: &str,
    ) -> Result<inkwell::values::IntValue<'a>, String> {
        let func = self
            .runtime_function(func_name)
            .ok_or_else(|| format!("{} not found", func_name))?;
        let result = self
            .builder
            .build_call(func, &[left.into(), right.into()], label)
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| format!("{} returned no value", func_name))?;
        let int_val = match result {
            BasicValueEnum::IntValue(v) => v,
            _ => return Err(format!("runtime function {} must return i32", func_name)),
        };
        self.i32_to_bool(int_val).map(|v| v.into_int_value())
    }

    /// Generate an equality comparison between two values based on their type.
    fn generate_value_equality(
        &mut self,
        left: BasicValueEnum<'a>,
        right: BasicValueEnum<'a>,
        expr_type: &Type,
    ) -> Result<inkwell::values::IntValue<'a>, String> {
        match expr_type {
            Type::Primitive(PrimitiveType::Int) | Type::Primitive(PrimitiveType::Char) => {
                let left_raw = self.get_raw_int_value(left)?;
                let right_raw = self.get_raw_int_value(right)?;
                self.builder
                    .build_int_compare(inkwell::IntPredicate::EQ, left_raw, right_raw, "eq")
                    .map_err(|e| e.to_string())
            }
            Type::Primitive(PrimitiveType::Bool) => {
                let left_raw = self.get_raw_bool_value(left)?;
                let right_raw = self.get_raw_bool_value(right)?;
                self.builder
                    .build_int_compare(inkwell::IntPredicate::EQ, left_raw, right_raw, "eq")
                    .map_err(|e| e.to_string())
            }
            Type::Primitive(PrimitiveType::Float) => {
                let left_float = self.get_raw_float_value(left)?;
                let right_float = self.get_raw_float_value(right)?;
                self.builder
                    .build_float_compare(
                        inkwell::FloatPredicate::OEQ,
                        left_float,
                        right_float,
                        "feq",
                    )
                    .map_err(|e| e.to_string())
            }
            Type::Primitive(PrimitiveType::Str) => {
                let left_ptr = self.ensure_pointer(left);
                let right_ptr = self.ensure_pointer(right);
                let left_cstr = self.extract_c_string_from_value(left_ptr)?;
                let right_cstr = self.extract_c_string_from_value(right_ptr)?;
                let result = self.call_runtime_bool(
                    left_cstr,
                    right_cstr,
                    "mux_string_equal",
                    "string_equal",
                );
                // The getters return owned C strings; free them after comparison.
                let free_fn = self
                    .runtime_function("mux_free_string")
                    .ok_or("mux_free_string not found")?;
                for cstr in [left_cstr, right_cstr] {
                    self.builder
                        .build_call(free_fn, &[cstr.into()], "free_cstr")
                        .map_err(|e| e.to_string())?;
                }
                result
            }
            Type::List(_)
            | Type::Map(_, _)
            | Type::Set(_)
            | Type::Tuple(_, _)
            | Type::EmptyList
            | Type::EmptyMap
            | Type::EmptySet => {
                let left_ptr = self.ensure_pointer(left);
                let right_ptr = self.ensure_pointer(right);
                self.call_runtime_bool(left_ptr, right_ptr, "mux_value_equal", "value_equal")
            }
            _ => Err(format!(
                "Equality comparison not supported for match type: {:?}",
                expr_type
            )),
        }
    }

    /// Generate check for a list structural pattern.
    /// Returns an i1 condition that is true if the pattern matches.
    fn generate_list_pattern_check(
        &mut self,
        match_val: BasicValueEnum<'a>,
        match_expr_type: &Type,
        elements: &[PatternNode],
        rest: Option<&PatternNode>,
    ) -> Result<inkwell::values::IntValue<'a>, String> {
        let val_ptr = if match_val.is_pointer_value() {
            match_val.into_pointer_value()
        } else {
            self.box_value(match_val)
        };

        let len_fn = self
            .runtime_function("mux_value_list_length")
            .ok_or("mux_value_list_length not found")?;
        let len_result = self
            .builder
            .build_call(len_fn, &[val_ptr.into()], "list_len")
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or("mux_value_list_length returned no value")?;
        let list_len = len_result.into_int_value();
        let required_len = self
            .context
            .i64_type()
            .const_int(elements.len() as u64, false);

        let len_check = if rest.is_some() {
            self.builder
                .build_int_compare(inkwell::IntPredicate::SGE, list_len, required_len, "len_ge")
                .map_err(|e| e.to_string())?
        } else {
            self.builder
                .build_int_compare(inkwell::IntPredicate::EQ, list_len, required_len, "len_eq")
                .map_err(|e| e.to_string())?
        };

        if elements.is_empty() && rest.is_none() {
            return Ok(len_check);
        }

        let inner_type = match match_expr_type {
            Type::List(inner) => (**inner).clone(),
            _ => return Ok(len_check),
        };

        let get_fn = self
            .runtime_function("mux_value_list_get_value")
            .ok_or("mux_value_list_get_value not found")?;

        let mut combined = len_check;
        for (i, elem) in elements.iter().enumerate() {
            if let PatternNode::Literal(lit) = elem {
                let idx = self.context.i64_type().const_int(i as u64, false);
                let elem_result = self
                    .builder
                    .build_call(get_fn, &[val_ptr.into(), idx.into()], "list_elem")
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("mux_value_list_get_value returned no value")?;

                // mux_value_list_get_value returns an owned element copy; the
                // equality check only reads it, so register it for release at the
                // end of the match statement.
                self.register_temp(elem_result);
                let pattern_val = self.generate_literal(lit)?;
                let elem_eq =
                    self.generate_value_equality(elem_result, pattern_val, &inner_type)?;
                combined = self
                    .builder
                    .build_and(combined, elem_eq, "and_check")
                    .map_err(|e| e.to_string())?;
            }
        }

        Ok(combined)
    }

    /// Bind variables for list pattern elements after condition check succeeds.
    fn bind_list_pattern_variables(
        &mut self,
        match_val: BasicValueEnum<'a>,
        match_expr_type: &Type,
        elements: &[PatternNode],
        rest: Option<&PatternNode>,
    ) -> Result<(), String> {
        let inner_type = match match_expr_type {
            Type::List(inner) => (**inner).clone(),
            Type::EmptyList => return Ok(()),
            _ => return Ok(()),
        };

        let val_ptr = if match_val.is_pointer_value() {
            match_val.into_pointer_value()
        } else {
            self.box_value(match_val)
        };

        let get_fn = self
            .runtime_function("mux_value_list_get_value")
            .ok_or("mux_value_list_get_value not found")?;

        let ptr_type = self.context.ptr_type(AddressSpace::default());

        for (i, elem) in elements.iter().enumerate() {
            if let PatternNode::Identifier(var) = elem {
                let idx = self.context.i64_type().const_int(i as u64, false);
                let elem_result = self
                    .builder
                    .build_call(get_fn, &[val_ptr.into(), idx.into()], "list_elem")
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("mux_value_list_get_value returned no value")?;

                let alloca = self
                    .builder
                    .build_alloca(ptr_type, var)
                    .map_err(|e| e.to_string())?;
                let elem_ptr = elem_result.into_pointer_value();
                self.builder
                    .build_store(alloca, elem_ptr)
                    .map_err(|e| e.to_string())?;
                // Owned element copy bound to the pattern variable; register it
                // for release at the end of the match statement.
                self.register_temp(elem_ptr.into());
                self.variables
                    .insert(var.clone(), (alloca, ptr_type.into(), inner_type.clone()));
            }
        }

        if let Some(PatternNode::Identifier(rest_var)) = rest {
            let start_idx = self
                .context
                .i64_type()
                .const_int(elements.len() as u64, false);

            let len_fn = self
                .runtime_function("mux_value_list_length")
                .ok_or("mux_value_list_length not found")?;
            let len_result = self
                .builder
                .build_call(len_fn, &[val_ptr.into()], "list_len")
                .map_err(|e| e.to_string())?
                .try_as_basic_value()
                .basic()
                .ok_or("mux_value_list_length returned no value")?;
            let end_idx = len_result.into_int_value();

            let slice_fn = self
                .runtime_function("mux_value_list_slice")
                .ok_or("mux_value_list_slice not found")?;
            let rest_result = self
                .builder
                .build_call(
                    slice_fn,
                    &[val_ptr.into(), start_idx.into(), end_idx.into()],
                    "rest_list",
                )
                .map_err(|e| e.to_string())?
                .try_as_basic_value()
                .basic()
                .ok_or("mux_value_list_slice returned no value")?;

            let alloca = self
                .builder
                .build_alloca(ptr_type, rest_var)
                .map_err(|e| e.to_string())?;
            let rest_ptr = rest_result.into_pointer_value();
            self.builder
                .build_store(alloca, rest_ptr)
                .map_err(|e| e.to_string())?;
            let rest_type = Type::List(Box::new(inner_type));
            self.variables
                .insert(rest_var.clone(), (alloca, ptr_type.into(), rest_type));
        }

        Ok(())
    }
}
