//! Codegen for `where { ... }` constraint checks.
//!
//! Each predicate is evaluated as a bool; a false result panics through the
//! unified runtime panic (`panic: where constraint violated` plus the
//! predicate's `--> file:line:col`). Emission points: function/method/lambda
//! entry (preconditions, including preconditions inherited from interfaces),
//! enum variant construction, class construction (`.new()`, after field
//! defaults are applied), and class field assignment (invariants).

use inkwell::values::PointerValue;

use crate::ast::{ExpressionNode, FunctionNode};
use crate::semantics::Type;
use crate::semantics::const_fold::{self, ConstValue};

use super::CodeGenerator;

impl<'a> CodeGenerator<'a> {
    /// Evaluate each predicate and panic if any is false. Positions the
    /// builder at a fresh continue block after each check.
    pub(super) fn emit_where_checks(
        &mut self,
        predicates: &[ExpressionNode],
        block_prefix: &str,
    ) -> Result<(), String> {
        for (i, predicate) in predicates.iter().enumerate() {
            // A predicate that folds to true from literals alone can never
            // fail; skip the branch. The folder never short-circuits, so no
            // runtime evaluation is dropped.
            if const_fold::fold(predicate) == Some(ConstValue::Bool(true)) {
                continue;
            }
            let value = self.generate_expression(predicate)?;
            let cond = self.get_raw_bool_value(value)?;

            let current_function = self
                .builder
                .get_insert_block()
                .expect("Builder should have an insertion block")
                .get_parent()
                .ok_or("No current function")?;
            let prefix = format!("{}_{}", block_prefix, i);
            let fail_bb = self
                .context
                .append_basic_block(current_function, &format!("{}_fail", prefix));
            let ok_bb = self
                .context
                .append_basic_block(current_function, &format!("{}_ok", prefix));
            self.builder
                .build_conditional_branch(cond, ok_bb, fail_bb)
                .map_err(|e| e.to_string())?;

            self.builder.position_at_end(fail_bb);
            self.emit_runtime_fatal("where constraint violated", Some(&predicate.span), &prefix)?;

            self.builder.position_at_end(ok_bb);
        }
        Ok(())
    }

    /// Emit a function's own `where` preconditions plus any inherited from
    /// interfaces the enclosing class implements. Runs at function entry,
    /// after parameters are bound into the variable table.
    pub(super) fn emit_function_preconditions(
        &mut self,
        func: &FunctionNode,
    ) -> Result<(), String> {
        if let Some(clause) = &func.where_clause {
            self.emit_where_checks(&clause.predicates, "where_pre")?;
        }

        let Some((class_name, method_name)) = func.name.split_once('.') else {
            return Ok(());
        };
        let inherited = self
            .analyzer
            .inherited_preconditions(class_name, method_name)
            .to_vec();
        if inherited.is_empty() {
            return Ok(());
        }

        // Interface predicates are written against the interface method's
        // parameter names; bind those names to this method's parameters
        // positionally, without leaking the aliases into the body scope.
        let snapshot = self.variables.clone();
        for (set_index, precondition) in inherited.iter().enumerate() {
            self.variables = snapshot.clone();
            for (interface_name, param) in precondition
                .interface_param_names
                .iter()
                .zip(func.params.iter())
            {
                if interface_name != &param.name
                    && let Some(entry) = snapshot.get(&param.name)
                {
                    self.variables.insert(interface_name.clone(), entry.clone());
                }
            }
            self.emit_where_checks(
                &precondition.predicates,
                &format!("where_iface_{}", set_index),
            )?;
        }
        self.variables = snapshot;
        Ok(())
    }

    /// At the end of `.new()`, after field defaults are applied, check every
    /// class invariant. Together with the per-assignment re-checks this
    /// guarantees an object never holds out-of-range data; it also means a
    /// constrained field needs a default that satisfies its constraints.
    pub(super) fn emit_construction_invariants(
        &mut self,
        class_name: &str,
        struct_ptr: PointerValue<'a>,
    ) -> Result<(), String> {
        let invariants: Vec<ExpressionNode> = self
            .analyzer
            .class_invariants(Self::declared_class_name(class_name))
            .iter()
            .map(|inv| inv.predicate.clone())
            .collect();
        if invariants.is_empty() {
            return Ok(());
        }

        let snapshot = self.variables.clone();
        self.bind_class_fields(class_name, struct_ptr)?;
        self.emit_where_checks(&invariants, "where_new")?;
        self.variables = snapshot;
        Ok(())
    }

    /// After a store to `assigned_field`, re-check the class invariants that
    /// reference it.
    pub(super) fn emit_field_assignment_invariants(
        &mut self,
        class_name: &str,
        assigned_field: &str,
        struct_ptr: PointerValue<'a>,
        block_prefix: &str,
    ) -> Result<(), String> {
        let invariants: Vec<ExpressionNode> = self
            .analyzer
            .class_invariants(Self::declared_class_name(class_name))
            .iter()
            .filter(|inv| inv.referenced_fields.iter().any(|f| f == assigned_field))
            .map(|inv| inv.predicate.clone())
            .collect();
        if invariants.is_empty() {
            return Ok(());
        }

        let snapshot = self.variables.clone();
        self.bind_class_fields(class_name, struct_ptr)?;
        self.emit_where_checks(&invariants, block_prefix)?;
        self.variables = snapshot;
        Ok(())
    }

    /// Bind every field of `class_name` into the variable table as a bare
    /// name backed by its slot in the object at `struct_ptr`, so invariant
    /// predicates can read fields like locals.
    ///
    /// `class_name` is the layout name, so a generic instantiation reads its
    /// own slots; the field's declared semantic type still comes from the
    /// symbol table, which only ever knew the declaration.
    fn bind_class_fields(
        &mut self,
        class_name: &str,
        struct_ptr: PointerValue<'a>,
    ) -> Result<(), String> {
        let struct_type = *self
            .type_map
            .get(class_name)
            .ok_or_else(|| format!("Class {} not found in type map", class_name))?;
        let field_indices = self
            .field_map
            .get(class_name)
            .ok_or_else(|| format!("Class {} not found in field map", class_name))?
            .clone();
        let field_llvm_types = self
            .field_types_map
            .get(class_name)
            .ok_or_else(|| format!("Class {} not found in field types map", class_name))?
            .clone();
        let declared_name = Self::declared_class_name(class_name);
        let mut field_semantic_types: Vec<(String, Type)> = {
            let class_symbol = self
                .analyzer
                .symbol_table()
                .lookup(declared_name)
                .ok_or_else(|| format!("Class {} not found in symbol table", declared_name))?;
            class_symbol
                .fields
                .iter()
                .map(|(name, (ty, _))| (name.clone(), ty.clone()))
                .collect()
        };
        // Bind in struct order. `fields` is a HashMap, so without this the GEPs
        // below came out in a different order between builds of the same file
        // (issue #344). Struct order is also what a reader of the IR expects.
        field_semantic_types.sort_by_key(|(name, _)| field_indices.get(name).copied());

        for (field_name, semantic_type) in field_semantic_types {
            let Some(&index) = field_indices.get(&field_name) else {
                continue;
            };
            let field_ptr = self
                .builder
                .build_struct_gep(
                    struct_type.into_struct_type(),
                    struct_ptr,
                    index as u32,
                    &format!("where_field_{}", field_name),
                )
                .map_err(|e| e.to_string())?;
            let llvm_type = *field_llvm_types
                .get(index)
                .ok_or_else(|| format!("Field {} index out of range", field_name))?;
            self.variables
                .insert(field_name, (field_ptr, llvm_type, semantic_type));
        }
        Ok(())
    }
}
