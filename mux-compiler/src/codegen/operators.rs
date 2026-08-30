//! Binary and logical operator generation for the code generator.
//!
//! This module handles:
//! - Short-circuit logical operators (&&, ||)
//! - Binary arithmetic operators (+, -, *, /, %, **)
//! - Comparison operators (==, !=, <, >, <=, >=)
//! - The 'in' operator for containment checks

use inkwell::intrinsics::Intrinsic;
use inkwell::values::{BasicValueEnum, IntValue, PointerValue};

use crate::ast::{
    BinaryOp, ExpressionKind, ExpressionNode, LiteralNode, PrimitiveType, Spanned, UnaryOp,
};
use crate::lexer::Span;
use crate::semantics::{SemanticAnalyzer, Symbol, Type};

use super::CodeGenerator;

/// Which direction an equality comparison runs.
///
/// `==` and `!=` share one dispatch (`generate_equality_op`) and differ only in
/// what this returns. Previously they were two parallel matches, which is how
/// `Type::Tuple` came to be handled by `==` and not by `!=` - a divergence this
/// makes unrepresentable (issue #360).
#[derive(Clone, Copy)]
enum EqualityKind {
    Equal,
    NotEqual,
}

#[derive(Clone, Copy)]
enum CheckedArithmeticKind {
    Subtract,
    Multiply,
}

#[derive(Clone, Copy)]
enum DivisionKind {
    Divide,
    Modulo,
}

impl EqualityKind {
    fn int_predicate(self) -> inkwell::IntPredicate {
        match self {
            Self::Equal => inkwell::IntPredicate::EQ,
            Self::NotEqual => inkwell::IntPredicate::NE,
        }
    }

    fn float_predicate(self) -> inkwell::FloatPredicate {
        match self {
            Self::Equal => inkwell::FloatPredicate::OEQ,
            Self::NotEqual => inkwell::FloatPredicate::ONE,
        }
    }

    fn string_runtime(self) -> &'static str {
        match self {
            Self::Equal => "mux_string_equal",
            Self::NotEqual => "mux_string_not_equal",
        }
    }

    fn value_runtime(self) -> &'static str {
        match self {
            Self::Equal => "mux_value_equal",
            Self::NotEqual => "mux_value_not_equal",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Equal => "eq",
            Self::NotEqual => "ne",
        }
    }

    fn float_label(self) -> &'static str {
        match self {
            Self::Equal => "feq",
            Self::NotEqual => "fne",
        }
    }

    fn string_label(self) -> &'static str {
        match self {
            Self::Equal => "string_equal",
            Self::NotEqual => "string_not_equal",
        }
    }

    fn value_label(self) -> &'static str {
        match self {
            Self::Equal => "value_equal",
            Self::NotEqual => "value_not_equal",
        }
    }

    fn enum_label(self) -> &'static str {
        match self {
            Self::Equal => "enum_eq",
            Self::NotEqual => "enum_ne",
        }
    }

    /// The operator as written, for the operand-type resolution error.
    fn operator(self) -> &'static str {
        match self {
            Self::Equal => "==",
            Self::NotEqual => "!=",
        }
    }

    /// Leading noun of the unsupported-type error, preserving the existing
    /// "Equality/Inequality comparison not supported for type" wording.
    fn noun(self) -> &'static str {
        match self {
            Self::Equal => "Equality",
            Self::NotEqual => "Inequality",
        }
    }
}

impl<'a> CodeGenerator<'a> {
    pub(super) fn checked_int_operation(
        &mut self,
        left: IntValue<'a>,
        right: IntValue<'a>,
        intrinsic_name: &str,
        message: &str,
        span: Option<&Span>,
        block_prefix: &str,
    ) -> Result<IntValue<'a>, String> {
        let intrinsic = Intrinsic::find(intrinsic_name)
            .ok_or_else(|| format!("LLVM intrinsic {intrinsic_name} is unavailable"))?;
        let function = intrinsic
            .get_declaration(&self.module, &[left.get_type().into()])
            .ok_or_else(|| format!("cannot declare LLVM intrinsic {intrinsic_name}"))?;
        let aggregate = self
            .builder
            .build_call(function, &[left.into(), right.into()], "checked_int_op")
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or("checked integer intrinsic returned no value")?
            .into_struct_value();
        let result = self
            .builder
            .build_extract_value(aggregate, 0, "checked_result")
            .map_err(|e| e.to_string())?
            .into_int_value();
        let overflow = self
            .builder
            .build_extract_value(aggregate, 1, "checked_overflow")
            .map_err(|e| e.to_string())?
            .into_int_value();
        self.emit_checked_failure(
            overflow,
            super::runtime::RuntimeErrorCode::IntegerOverflow,
            message,
            span,
            block_prefix,
        )?;
        Ok(result)
    }

    fn emit_checked_failure(
        &mut self,
        condition: IntValue<'a>,
        code: super::runtime::RuntimeErrorCode,
        message: &str,
        span: Option<&Span>,
        block_prefix: &str,
    ) -> Result<(), String> {
        let current_function = self
            .builder
            .get_insert_block()
            .ok_or("No current basic block")?
            .get_parent()
            .ok_or("No current function")?;
        let failure_bb = self
            .context
            .append_basic_block(current_function, &format!("{block_prefix}_overflow"));
        let continue_bb = self
            .context
            .append_basic_block(current_function, &format!("{block_prefix}_continue"));
        self.builder
            .build_conditional_branch(condition, failure_bb, continue_bb)
            .map_err(|e| e.to_string())?;
        self.builder.position_at_end(failure_bb);
        self.emit_runtime_fatal(code, message, span, &format!("{block_prefix}_overflow"))?;
        self.builder.position_at_end(continue_bb);
        Ok(())
    }

    /// Ensure a value is a pointer, boxing it if necessary.
    pub(super) fn ensure_pointer(&mut self, val: BasicValueEnum<'a>) -> PointerValue<'a> {
        if val.is_pointer_value() {
            val.into_pointer_value()
        } else {
            self.box_value(val)
        }
    }

    /// Emit `<`, `>`, `<=` or `>=`: a class that declares `is Comparable` orders
    /// through its own `cmp`, everything else through the numeric path.
    fn generate_ordering_op(
        &mut self,
        left_expr: &ExpressionNode,
        left: BasicValueEnum<'a>,
        right: BasicValueEnum<'a>,
        int_pred: inkwell::IntPredicate,
        float_pred: inkwell::FloatPredicate,
        label: &str,
    ) -> Result<BasicValueEnum<'a>, String> {
        if let Some(result) =
            self.try_generate_comparable_class_compare(left_expr, left, right, int_pred, label)?
        {
            return Ok(result);
        }
        if let Some(result) = self.try_generate_string_compare(left_expr, left, right, int_pred)? {
            return Ok(result);
        }
        self.generate_numeric_compare(left, right, int_pred, float_pred, label)
    }

    /// Order two strings lexicographically, via the runtime's `strcmp`-shaped
    /// comparison.
    ///
    /// Without this a string fell through to the numeric path, where
    /// `get_raw_int_value` treats a POINTER as a boxed int and emits
    /// `mux_value_get_int` - so `<` compared addresses and answered `false` in
    /// both directions at once, silently (issue #390). `generate_numeric_compare`
    /// now rejects pointers outright, so this branch is what makes the operators
    /// work rather than merely error.
    fn try_generate_string_compare(
        &mut self,
        left_expr: &ExpressionNode,
        left: BasicValueEnum<'a>,
        right: BasicValueEnum<'a>,
        predicate: inkwell::IntPredicate,
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        if !matches!(
            self.resolve_expression_type_with_fallback(left_expr),
            Ok(Type::Primitive(PrimitiveType::Str))
        ) {
            return Ok(None);
        }
        // The runtime takes C strings, not `Value` pointers, and hands back an
        // owned copy that has to be freed - the same dance `call_string_comparison`
        // does for `==`. Passing the `Value` straight through reads the struct
        // header as text, which compares nothing and frees nothing.
        let left_ptr = self.ensure_pointer(left);
        let right_ptr = self.ensure_pointer(right);
        let left_cstr = self.extract_c_string_from_value(left_ptr)?;
        let right_cstr = self.extract_c_string_from_value(right_ptr)?;

        let func = self
            .runtime_function("mux_string_compare")
            .ok_or("mux_string_compare not found")?;
        let ordering = self
            .builder
            .build_call(func, &[left_cstr.into(), right_cstr.into()], "str_cmp")
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or("mux_string_compare should return a value")?
            .into_int_value();

        let free_fn = self
            .runtime_function("mux_free_string")
            .ok_or("mux_free_string not found")?;
        for cstr in [left_cstr, right_cstr] {
            self.builder
                .build_call(free_fn, &[cstr.into()], "free_cstr")
                .map_err(|e| e.to_string())?;
        }

        let zero = ordering.get_type().const_zero();
        self.builder
            .build_int_compare(predicate, ordering, zero, "str_ord")
            .map_err(|e| e.to_string())
            .map(|v| Some(v.into()))
    }

    /// Order two values of a class that declares `is Comparable`, by calling its
    /// `cmp` method and testing the result against zero.
    ///
    /// `cmp` returns negative, zero or positive like C's `strcmp`, so every
    /// ordering operator is the same call with a different predicate. Returns
    /// `None` when the operands are not such a class, leaving the numeric path
    /// to handle them.
    fn try_generate_comparable_class_compare(
        &mut self,
        left_expr: &ExpressionNode,
        left: BasicValueEnum<'a>,
        right: BasicValueEnum<'a>,
        predicate: inkwell::IntPredicate,
        label: &str,
    ) -> Result<Option<BasicValueEnum<'a>>, String> {
        let Ok(Type::Named(class_name, type_args)) =
            self.resolve_expression_type_with_fallback(left_expr)
        else {
            return Ok(None);
        };
        if !self
            .analyzer
            .type_implements_named_interface(&class_name, "Comparable")
        {
            return Ok(None);
        }
        // A generic class only ever emits monomorphized bodies, so the method
        // for `Ranked<string>` is `Ranked$string.cmp`; the unspecialized name
        // is a declaration that never gets one and fails at link time.
        let func_name = self.create_specialized_method_name(&class_name, &type_args, "cmp");
        let func = self
            .module
            .get_function(&func_name)
            .ok_or_else(|| format!("{func_name} not found for Comparable"))?;
        let ordering = self
            .builder
            .build_call(func, &[left.into(), right.into()], "cmp_call")
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| format!("{func_name} should return a value"))?;
        let ordering = self.get_raw_int_value(ordering)?;
        let zero = ordering.get_type().const_zero();
        self.builder
            .build_int_compare(predicate, ordering, zero, label)
            .map_err(|e| e.to_string())
            .map(|v| Some(v.into()))
    }

    /// Generate a numeric comparison (int or float) with the given predicates.
    ///
    /// A POINTER operand is rejected rather than unboxed. `get_raw_int_value`
    /// happily emits `mux_value_get_int` for one, which is right for a boxed
    /// int out of a collection and catastrophic for anything else - it is how
    /// `<` on strings came to compare addresses and answer `false` in both
    /// directions with no diagnostic (#390). Any type whose ordering needs more
    /// than a machine comparison has to be handled before reaching here, so
    /// failing loudly is what stops the next one shipping the same way.
    fn generate_numeric_compare(
        &mut self,
        left: BasicValueEnum<'a>,
        right: BasicValueEnum<'a>,
        int_pred: inkwell::IntPredicate,
        float_pred: inkwell::FloatPredicate,
        label: &str,
    ) -> Result<BasicValueEnum<'a>, String> {
        if left.is_pointer_value() || right.is_pointer_value() {
            return Err(format!(
                "Ordering comparison '{label}' reached the numeric path with a pointer operand. \
                 A type ordered by more than a machine comparison must be handled before \
                 this point"
            ));
        }
        if let (Ok(left_int), Ok(right_int)) =
            (self.get_raw_int_value(left), self.get_raw_int_value(right))
        {
            self.builder
                .build_int_compare(int_pred, left_int, right_int, label)
                .map_err(|e| e.to_string())
                .map(std::convert::Into::into)
        } else if let (Ok(left_float), Ok(right_float)) = (
            self.get_raw_float_value(left),
            self.get_raw_float_value(right),
        ) {
            let flabel = format!("f{label}");
            self.builder
                .build_float_compare(float_pred, left_float, right_float, &flabel)
                .map_err(|e| e.to_string())
                .map(std::convert::Into::into)
        } else {
            Err(format!("Unsupported {label} operands"))
        }
    }

    /// Pointer to an enum's raw struct, for the compare glue.
    ///
    /// Two operand shapes reach here and both need converting:
    ///
    /// - an inline struct value (a local, a constructor result), which is
    ///   spilled to a stack slot, and
    /// - a POINTER, which for a user enum means a BOXED value - a managed
    ///   `BoxedEnum` or Opaque out of a collection, as in `items[0] == Red`. It is
    ///   unboxed here for the same reason `generate_enum_match` unboxes its
    ///   subject (issue #309).
    ///
    /// `ensure_pointer` is wrong for both: it returns pointers untouched and
    /// falls back to `box_value` otherwise, so either shape can leave
    /// `mux_enum_cmp_<Enum>` reading a `Value` header as the discriminant. That
    /// misreads equal payloads as different while payload-less variants still
    /// compare correctly, which makes it easy to believe it works.
    ///
    /// The spill uses an entry-block alloca: codegen runs at
    /// `OptimizationLevel::None`, so an alloca emitted at the current insertion
    /// point is never hoisted, and a comparison inside a loop would grow the
    /// stack on every iteration.
    fn enum_struct_pointer(
        &mut self,
        val: BasicValueEnum<'a>,
        enum_name: &str,
    ) -> Result<PointerValue<'a>, String> {
        let val = if val.is_pointer_value() {
            self.unbox_enum_subject_value(enum_name, val.into_pointer_value())?
        } else {
            val
        };
        if !val.is_struct_value() {
            return Err(format!("enum operand is not a struct value: {val:?}"));
        }
        let struct_val = val.into_struct_value();
        let temp = self.create_entry_alloca(struct_val.get_type().into(), "enum_cmp_operand")?;
        self.builder
            .build_store(temp, struct_val)
            .map_err(|e| e.to_string())?;
        Ok(temp)
    }

    /// Compare two user-enum values through the enum's structural compare glue.
    ///
    /// `mux_enum_cmp_<Enum>` is the same three-way function sets and maps
    /// already use to order enum keys (issue #309); it compares discriminants
    /// first, then the matching variant's payload fields. Equality is that
    /// result against zero, so `==` and `!=` differ only by the predicate.
    fn call_enum_comparison(
        &mut self,
        left: PointerValue<'a>,
        right: PointerValue<'a>,
        enum_name: &str,
        predicate: inkwell::IntPredicate,
        label: &str,
    ) -> Result<BasicValueEnum<'a>, String> {
        let func = self.get_or_create_enum_cmp_fn(enum_name)?;
        let result = self
            .builder
            .build_call(func, &[left.into(), right.into()], label)
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or("enum comparison returned no value")?;
        let zero = self.context.i32_type().const_zero();
        self.builder
            .build_int_compare(predicate, result.into_int_value(), zero, label)
            .map_err(|e| e.to_string())
            .map(std::convert::Into::into)
    }

    /// Call a runtime comparison function on two pointer values and convert
    /// the i32 result to an i1 bool.
    fn call_comparison_runtime(
        &mut self,
        left: PointerValue<'a>,
        right: PointerValue<'a>,
        func_name: &str,
        label: &str,
    ) -> Result<BasicValueEnum<'a>, String> {
        let func = self
            .runtime_function(func_name)
            .ok_or_else(|| format!("{func_name} not found"))?;
        let result = self
            .builder
            .build_call(func, &[left.into(), right.into()], label)
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or("Call returned no value")?;

        let result_i32 = result.into_int_value();
        self.i32_to_bool(result_i32)
    }

    /// Extract both operands as owned C strings, run a string comparison
    /// runtime call, then free the two C strings. The getters return owned
    /// pointers, so without the frees every string comparison leaks two
    /// allocations.
    fn call_string_comparison(
        &mut self,
        left_ptr: PointerValue<'a>,
        right_ptr: PointerValue<'a>,
        func_name: &str,
        label: &str,
    ) -> Result<BasicValueEnum<'a>, String> {
        let left_cstr = self.extract_c_string_from_value(left_ptr)?;
        let right_cstr = self.extract_c_string_from_value(right_ptr)?;
        let result = self.call_comparison_runtime(left_cstr, right_cstr, func_name, label)?;
        let free_fn = self
            .runtime_function("mux_free_string")
            .ok_or("mux_free_string not found")?;
        for cstr in [left_cstr, right_cstr] {
            self.builder
                .build_call(free_fn, &[cstr.into()], "free_cstr")
                .map_err(|e| e.to_string())?;
        }
        Ok(result)
    }

    pub(super) fn i32_to_bool(&self, int_val: IntValue<'a>) -> Result<BasicValueEnum<'a>, String> {
        let zero = self.context.i32_type().const_zero();
        self.builder
            .build_int_compare(inkwell::IntPredicate::NE, int_val, zero, "to_bool")
            .map_err(|e| e.to_string())
            .map(std::convert::Into::into)
    }

    /// Hand the analyzer the locals of the function being emitted, so it types
    /// an expression against what is really in scope here.
    ///
    /// This is what the codegen scope is for. The analyzer's own scopes were
    /// popped when analysis finished, leaving a flat program-wide index where
    /// the last function to declare a name answers for every function that uses
    /// it - `h.get()` inside `func show(Holder<float> h)` was typed `int`
    /// because some other function also had an `h`. Codegen's variable table is
    /// the authority for the function it is emitting, and publishing it makes
    /// the analyzer right about composite expressions too: `hs[i].get()` needs
    /// `hs` typed to type the receiver, and only the analyzer walks inside the
    /// expression.
    ///
    /// Republished per query rather than kept in step with every mutation of
    /// the variable table: far more places add, drop or rebind a local than ask
    /// the analyzer a question, and a scope left stale would be a silently
    /// wrong answer of exactly the kind this removes.
    fn publish_locals_to_analyzer(&mut self) {
        let locals: Vec<(String, Symbol)> = self
            .variables
            .iter()
            .map(|(name, (_, _, ty))| {
                (
                    name.clone(),
                    SemanticAnalyzer::local_symbol(Span::new(0, 0), ty.clone()),
                )
            })
            .collect();
        self.analyzer.symbol_table_mut().set_codegen_locals(locals);
    }

    /// Query the semantic analyzer for an expression's type, then resolve away any
    /// remaining generic type variables using codegen's active generic context.
    ///
    /// The analyzer's view of local variable/parameter types was captured once during
    /// the initial whole-program analysis pass, before any generic class was
    /// specialized for a concrete type. So for a generic method, it can return a type
    /// that still names the class's own type parameter (e.g. `Variable("T")`) even
    /// when codegen is currently generating the `int`-specialized version of that
    /// method. Resolving through `self.resolve_type` substitutes any such leftover
    /// type variable using the generic context active for the specialization being
    /// generated right now.
    pub(super) fn get_resolved_expression_type(
        &mut self,
        expr: &ExpressionNode,
    ) -> Result<Type, String> {
        self.publish_locals_to_analyzer();
        let analyzer_result = self.analyzer.get_expression_type(expr);
        let ty = match analyzer_result {
            Ok(ty) => ty,
            Err(e) => {
                if let ExpressionKind::Identifier(name) = &expr.kind
                    && let Some((_, _, ty)) = self
                        .variables
                        .get(name)
                        .or_else(|| self.global_variables.get(name))
                {
                    ty.clone()
                } else {
                    return Err(e.to_string());
                }
            }
        };
        Ok(self.resolve_type(&ty).unwrap_or(ty))
    }

    /// Resolve an expression type during codegen.
    /// Falls back to codegen variable tables when semantic scopes are no longer available.
    pub(super) fn resolve_expression_type_with_fallback(
        &mut self,
        expr: &ExpressionNode,
    ) -> Result<Type, String> {
        match &expr.kind {
            ExpressionKind::Identifier(_) => self.resolve_identifier_type(expr),
            ExpressionKind::ListAccess { .. } => self.resolve_list_access_type(expr),
            ExpressionKind::Call { .. } => self.resolve_call_type(expr),
            ExpressionKind::Binary { .. } => self.resolve_binary_type(expr),
            ExpressionKind::FieldAccess { expr: inner, field } => {
                self.resolve_field_access_type_with_fallback(inner, field, expr)
            }
            // A dereference is the referenced type, resolved through codegen's
            // own tracking for the same reason as a field access above. Asking
            // the analyzer instead read a program-wide symbol index where the
            // last function to declare a parameter name wins, so `*r + 5` in a
            // function taking `&int` was generated as string concatenation
            // because another function had an `&string` parameter also named
            // `r`.
            ExpressionKind::Unary {
                op: UnaryOp::Deref,
                expr: inner,
                ..
            } => match self.resolve_expression_type_with_fallback(inner)? {
                Type::Reference(referenced) => Ok(*referenced),
                other => Ok(other),
            },
            _ => self.get_resolved_expression_type(expr),
        }
    }

    /// Resolve the type of `inner.field` using codegen's own variable/generic-context
    /// tracking rather than the semantic analyzer, which has no record of local
    /// variables or parameters by the time a generic method is lazily specialized
    /// during codegen (its scope for that function body was already popped after the
    /// initial whole-program analysis pass).
    fn resolve_field_access_type_with_fallback(
        &mut self,
        inner: &ExpressionNode,
        field: &str,
        full_expr: &ExpressionNode,
    ) -> Result<Type, String> {
        let inner_type = self.resolve_expression_type_with_fallback(inner)?;
        let resolved_inner_type = self.resolve_type(&inner_type).unwrap_or(inner_type);

        if let Type::Named(class_name, type_args) = &resolved_inner_type
            && let Some(class_symbol) = self.lookup_class_symbol(class_name)
            && let Some((field_type, _)) = class_symbol.fields.get(field)
        {
            let type_param_map = self.build_type_param_map(class_name, type_args)?;
            return Ok(self.substitute_type_with_map(field_type, &type_param_map));
        }

        self.get_resolved_expression_type(full_expr)
    }

    fn resolve_identifier_type(&mut self, expr: &ExpressionNode) -> Result<Type, String> {
        let ExpressionKind::Identifier(name) = &expr.kind else {
            return Err("Expected identifier expression".to_string());
        };
        if let Some((_, _, ty)) = self
            .variables
            .get(name)
            .or_else(|| self.global_variables.get(name))
        {
            return Ok(ty.clone());
        }

        if let Ok(ty) = self.get_resolved_expression_type(expr) {
            return Ok(ty);
        }

        if let Some(func_node) = self.function_nodes.get(name) {
            let mut param_types = Vec::with_capacity(func_node.params.len());
            for param in &func_node.params {
                let param_type = self
                    .analyzer
                    .resolve_type(&param.type_)
                    .map_err(|e| e.to_string())?;
                param_types.push(param_type);
            }
            let return_type = self
                .analyzer
                .resolve_type(&func_node.return_type)
                .map_err(|e| e.to_string())?;
            return Ok(Type::Function {
                params: param_types,
                returns: Box::new(return_type),
                default_count: 0,
            });
        }

        if let Some(symbol) = self.analyzer.symbol_table().lookup(name)
            && let Some(ty) = &symbol.type_
        {
            return Ok(ty.clone());
        }

        Err(format!("Undefined variable '{name}'"))
    }

    fn resolve_list_access_type(&mut self, expr: &ExpressionNode) -> Result<Type, String> {
        match &expr.kind {
            ExpressionKind::ListAccess {
                expr: container,
                index,
            } => {
                let container_type = self.resolve_expression_type_with_fallback(container)?;
                match container_type {
                    Type::List(inner) => Ok(*inner),
                    Type::Map(_, value) => Ok(*value),
                    // Indexing a string yields a character (#389).
                    Type::Primitive(PrimitiveType::Str) => Ok(Type::Primitive(PrimitiveType::Char)),
                    Type::Tuple(left, right) => match &index.kind {
                        ExpressionKind::Literal(LiteralNode::Integer(0)) => Ok(*left),
                        ExpressionKind::Literal(LiteralNode::Integer(1)) => Ok(*right),
                        _ => Err("Tuple index must be a literal 0 or 1".to_string()),
                    },
                    _ => Err(format!("Cannot index into type: {container_type:?}")),
                }
            }
            _ => Err("Expected list access expression".to_string()),
        }
    }

    fn resolve_call_type(&mut self, expr: &ExpressionNode) -> Result<Type, String> {
        let ExpressionKind::Call { func, .. } = &expr.kind else {
            return Err("Expected call expression".to_string());
        };

        if let Ok(ty) = self.get_resolved_expression_type(expr) {
            return Ok(ty);
        }

        let func_type = self.resolve_expression_type_with_fallback(func)?;
        match func_type {
            Type::Function { returns, .. } => Ok(*returns),
            other => Err(format!("Cannot call non-function type: {other:?}")),
        }
    }

    fn resolve_binary_type(&mut self, expr: &ExpressionNode) -> Result<Type, String> {
        match &expr.kind {
            ExpressionKind::Binary {
                left, op, right, ..
            } => {
                let left_type = self.resolve_expression_type_with_fallback(left)?;
                let right_type = self.resolve_expression_type_with_fallback(right)?;
                match op {
                    BinaryOp::Add
                    | BinaryOp::Subtract
                    | BinaryOp::Multiply
                    | BinaryOp::Divide
                    | BinaryOp::Modulo
                    | BinaryOp::Exponent => {
                        if left_type == Type::Primitive(PrimitiveType::Float)
                            || right_type == Type::Primitive(PrimitiveType::Float)
                        {
                            Ok(Type::Primitive(PrimitiveType::Float))
                        } else {
                            Ok(left_type)
                        }
                    }
                    BinaryOp::Less
                    | BinaryOp::Greater
                    | BinaryOp::LessEqual
                    | BinaryOp::GreaterEqual
                    | BinaryOp::Equal
                    | BinaryOp::NotEqual
                    | BinaryOp::LogicalAnd
                    | BinaryOp::LogicalOr
                    | BinaryOp::In => Ok(Type::Primitive(PrimitiveType::Bool)),
                    BinaryOp::Assign
                    | BinaryOp::AddAssign
                    | BinaryOp::SubtractAssign
                    | BinaryOp::MultiplyAssign
                    | BinaryOp::DivideAssign
                    | BinaryOp::ModuloAssign => Ok(left_type),
                }
            }
            _ => Err("Expected binary expression".to_string()),
        }
    }

    pub(super) fn generate_short_circuit_logical_op(
        &mut self,
        left_expr: &ExpressionNode,
        op: &BinaryOp,
        right_expr: &ExpressionNode,
    ) -> Result<BasicValueEnum<'a>, String> {
        // Get the current function from the current basic block
        let current_bb = self
            .builder
            .get_insert_block()
            .ok_or("No current basic block for short-circuit logical operation")?;
        let current_fn = current_bb
            .get_parent()
            .ok_or("No current function for short-circuit logical operation")?;

        let is_and = match op {
            BinaryOp::LogicalAnd => true,
            BinaryOp::LogicalOr => false,
            _ => {
                return Err(
                    "generate_short_circuit_logical_op called with non-logical operator"
                        .to_string(),
                );
            }
        };
        self.generate_short_circuit_branch(left_expr, right_expr, current_fn, is_and)
    }

    fn generate_short_circuit_branch(
        &mut self,
        left_expr: &ExpressionNode,
        right_expr: &ExpressionNode,
        current_fn: inkwell::values::FunctionValue<'a>,
        is_and: bool,
    ) -> Result<BasicValueEnum<'a>, String> {
        let operation = if is_and { "and" } else { "or" };
        let eval_right_bb = self
            .context
            .append_basic_block(current_fn, &format!("{operation}_eval_right"));
        let merge_bb = self
            .context
            .append_basic_block(current_fn, &format!("{operation}_merge"));

        let left_val = self.generate_expression(left_expr)?;
        let left_bool = self.get_raw_bool_value(left_val)?;
        let left_bb = self
            .builder
            .get_insert_block()
            .ok_or("No insert block after evaluating left operand")?;
        let (true_target, false_target) = if is_and {
            (eval_right_bb, merge_bb)
        } else {
            (merge_bb, eval_right_bb)
        };
        self.builder
            .build_conditional_branch(left_bool, true_target, false_target)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(eval_right_bb);
        let right_val = self.generate_expression(right_expr)?;
        let right_bool = self.get_raw_bool_value(right_val)?;
        let right_bb = self
            .builder
            .get_insert_block()
            .ok_or("No insert block after evaluating right operand")?;
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(merge_bb);
        let phi = self
            .builder
            .build_phi(self.context.bool_type(), &format!("{operation}_result"))
            .map_err(|e| e.to_string())?;
        let short_circuit_value = self
            .context
            .bool_type()
            .const_int(u64::from(!is_and), false);
        phi.add_incoming(&[(&short_circuit_value, left_bb), (&right_bool, right_bb)]);
        Ok(phi.as_basic_value())
    }

    fn generate_add_op(
        &mut self,
        left_expr: &ExpressionNode,
        left: BasicValueEnum<'a>,
        right: BasicValueEnum<'a>,
    ) -> Result<BasicValueEnum<'a>, String> {
        let left_type = self
            .resolve_expression_type_with_fallback(left_expr)
            .map_err(|e| format!("Failed to get left operand type for '+': {e}"))?;

        match &left_type {
            Type::Primitive(PrimitiveType::Str) => {
                let left_ptr = self.ensure_pointer(left);
                let right_ptr = self.ensure_pointer(right);
                // Both getters return owned C strings, and mux_string_concat
                // returns a third owned C string. box_string_value copies the
                // concatenated bytes into a new Value, so all three C strings are
                // ours to free once the Value exists (mirrors how list concat
                // frees its intermediate lists below).
                let left_cstr = self.extract_c_string_from_value(left_ptr)?;
                let right_cstr = self.extract_c_string_from_value(right_ptr)?;

                let concat_fn = self
                    .runtime_function("mux_string_concat")
                    .ok_or("mux_string_concat not found")?;
                let result = self
                    .builder
                    .build_call(
                        concat_fn,
                        &[left_cstr.into(), right_cstr.into()],
                        "string_concat",
                    )
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("Call returned no value")?
                    .into_pointer_value();

                let boxed = self.box_string_value(result)?;

                let free_string_fn = self
                    .runtime_function("mux_free_string")
                    .ok_or("mux_free_string not found")?;
                for cstr in [left_cstr, right_cstr, result] {
                    self.builder
                        .build_call(free_string_fn, &[cstr.into()], "free_cstr")
                        .map_err(|e| e.to_string())?;
                }

                Ok(boxed)
            }
            Type::List(_) => {
                let left_list = self.extract_list_from_value(left.into_pointer_value())?;
                let right_list = self.extract_list_from_value(right.into_pointer_value())?;

                let concat_fn = self
                    .runtime_function("mux_list_concat")
                    .ok_or("mux_list_concat not found")?;
                let result_list = self
                    .builder
                    .build_call(
                        concat_fn,
                        &[left_list.into(), right_list.into()],
                        "list_concat",
                    )
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("Call returned no value")?
                    .into_pointer_value();

                let free_list_fn = self
                    .runtime_function("mux_free_list")
                    .ok_or("mux_free_list not found")?;
                self.builder
                    .build_call(free_list_fn, &[left_list.into()], "free_list")
                    .map_err(|e| e.to_string())?;
                self.builder
                    .build_call(free_list_fn, &[right_list.into()], "free_list")
                    .map_err(|e| e.to_string())?;

                let list_value_fn = self
                    .runtime_function("mux_list_value")
                    .ok_or("mux_list_value not found")?;
                let result = self
                    .builder
                    .build_call(list_value_fn, &[result_list.into()], "list_value")
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("Call returned no value")?;

                Ok(result)
            }
            Type::Map(_, _) => {
                let left_map = self.extract_map_from_value(left.into_pointer_value())?;
                let right_map = self.extract_map_from_value(right.into_pointer_value())?;

                let merge_fn = self
                    .runtime_function("mux_map_merge")
                    .ok_or("mux_map_merge not found")?;
                let result_map = self
                    .builder
                    .build_call(merge_fn, &[left_map.into(), right_map.into()], "map_merge")
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("Call returned no value")?
                    .into_pointer_value();

                let free_map_fn = self
                    .runtime_function("mux_free_map")
                    .ok_or("mux_free_map not found")?;
                self.builder
                    .build_call(free_map_fn, &[left_map.into()], "free_map")
                    .map_err(|e| e.to_string())?;
                self.builder
                    .build_call(free_map_fn, &[right_map.into()], "free_map")
                    .map_err(|e| e.to_string())?;

                let map_value_fn = self
                    .runtime_function("mux_map_value")
                    .ok_or("mux_map_value not found")?;
                let result = self
                    .builder
                    .build_call(map_value_fn, &[result_map.into()], "map_value")
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("Call returned no value")?;

                Ok(result)
            }
            Type::Set(_) => {
                let left_set = self.extract_set_from_value(left.into_pointer_value())?;
                let right_set = self.extract_set_from_value(right.into_pointer_value())?;

                let union_fn = self
                    .runtime_function("mux_set_union")
                    .ok_or("mux_set_union not found")?;
                let result_set = self
                    .builder
                    .build_call(union_fn, &[left_set.into(), right_set.into()], "set_union")
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("Call returned no value")?
                    .into_pointer_value();

                let free_set_fn = self
                    .runtime_function("mux_free_set")
                    .ok_or("mux_free_set not found")?;
                self.builder
                    .build_call(free_set_fn, &[left_set.into()], "free_set")
                    .map_err(|e| e.to_string())?;
                self.builder
                    .build_call(free_set_fn, &[right_set.into()], "free_set")
                    .map_err(|e| e.to_string())?;

                let set_value_fn = self
                    .runtime_function("mux_set_value")
                    .ok_or("mux_set_value not found")?;
                let result = self
                    .builder
                    .build_call(set_value_fn, &[result_set.into()], "set_value")
                    .map_err(|e| e.to_string())?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("Call returned no value")?;

                Ok(result)
            }
            Type::Primitive(PrimitiveType::Int) => {
                let left_int = self.get_raw_int_value(left)?;
                let right_int = self.get_raw_int_value(right)?;
                self.checked_int_operation(
                    left_int,
                    right_int,
                    "llvm.sadd.with.overflow.i64",
                    "integer addition overflow",
                    Some(left_expr.span()),
                    "add",
                )
                .map(Into::into)
            }
            Type::Primitive(PrimitiveType::Float) => {
                let left_float = self.get_raw_float_value(left)?;
                let right_float = self.get_raw_float_value(right)?;
                self.builder
                    .build_float_add(left_float, right_float, "fadd")
                    .map_err(|e| e.to_string())
                    .map(std::convert::Into::into)
            }
            _ => Err(format!(
                "Add operation not supported for type: {left_type:?}"
            )),
        }
    }

    fn generate_checked_arithmetic_op(
        &mut self,
        left: BasicValueEnum<'a>,
        right: BasicValueEnum<'a>,
        span: Option<&Span>,
        kind: CheckedArithmeticKind,
    ) -> Result<BasicValueEnum<'a>, String> {
        let (intrinsic, message, block_prefix) = match kind {
            CheckedArithmeticKind::Subtract => (
                "llvm.ssub.with.overflow.i64",
                "integer subtraction overflow",
                "sub",
            ),
            CheckedArithmeticKind::Multiply => (
                "llvm.smul.with.overflow.i64",
                "integer multiplication overflow",
                "mul",
            ),
        };
        if let (Ok(left_int), Ok(right_int)) =
            (self.get_raw_int_value(left), self.get_raw_int_value(right))
        {
            self.checked_int_operation(left_int, right_int, intrinsic, message, span, block_prefix)
                .map(Into::into)
        } else if let (Ok(left_float), Ok(right_float)) = (
            self.get_raw_float_value(left),
            self.get_raw_float_value(right),
        ) {
            let result = match kind {
                CheckedArithmeticKind::Subtract => {
                    self.builder
                        .build_float_sub(left_float, right_float, "fsub")
                }
                CheckedArithmeticKind::Multiply => {
                    self.builder
                        .build_float_mul(left_float, right_float, "fmul")
                }
            };
            result
                .map_err(|e| e.to_string())
                .map(std::convert::Into::into)
        } else {
            let unsupported = match kind {
                CheckedArithmeticKind::Subtract => "Unsupported sub operands",
                CheckedArithmeticKind::Multiply => "Unsupported mul operands",
            };
            Err(unsupported.to_string())
        }
    }

    fn generate_division_op(
        &mut self,
        left: BasicValueEnum<'a>,
        right: BasicValueEnum<'a>,
        span: Option<&Span>,
        kind: DivisionKind,
    ) -> Result<BasicValueEnum<'a>, String> {
        let (block_prefix, zero_message, overflow_message, unsupported_message) = match kind {
            DivisionKind::Divide => (
                "div",
                "division by zero",
                "integer division overflow",
                "Unsupported div operands",
            ),
            DivisionKind::Modulo => (
                "mod",
                "modulo by zero",
                "integer modulo overflow",
                "Unsupported mod operands",
            ),
        };
        if let (Ok(left_int), Ok(right_int)) =
            (self.get_raw_int_value(left), self.get_raw_int_value(right))
        {
            self.emit_div_by_zero_check(right_int, span, block_prefix, zero_message)?;
            self.emit_div_overflow_check(
                left_int,
                right_int,
                span,
                block_prefix,
                overflow_message,
            )?;
            let result = match kind {
                DivisionKind::Divide => {
                    self.builder
                        .build_int_signed_div(left_int, right_int, block_prefix)
                }
                DivisionKind::Modulo => {
                    self.builder
                        .build_int_signed_rem(left_int, right_int, block_prefix)
                }
            };
            result
                .map_err(|e| e.to_string())
                .map(std::convert::Into::into)
        } else if matches!(kind, DivisionKind::Divide)
            && let (Ok(left_float), Ok(right_float)) = (
                self.get_raw_float_value(left),
                self.get_raw_float_value(right),
            )
        {
            // Float division follows IEEE 754 semantics (inf/nan), no check.
            self.builder
                .build_float_div(left_float, right_float, "fdiv")
                .map_err(|e| e.to_string())
                .map(std::convert::Into::into)
        } else {
            Err(unsupported_message.to_string())
        }
    }

    /// Panic with `message` if the integer `divisor` is zero, so division and
    /// modulo can report their own operation. Positions the builder at a new
    /// "continue" block for the operation.
    fn emit_div_by_zero_check(
        &mut self,
        divisor: IntValue<'a>,
        span: Option<&Span>,
        block_prefix: &str,
        message: &str,
    ) -> Result<(), String> {
        let zero = divisor.get_type().const_zero();
        let is_zero = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                divisor,
                zero,
                &format!("{block_prefix}_is_zero"),
            )
            .map_err(|e| e.to_string())?;

        let current_function = self
            .builder
            .get_insert_block()
            .ok_or("No current basic block")?
            .get_parent()
            .ok_or("No current function")?;

        let error_bb = self
            .context
            .append_basic_block(current_function, &format!("{block_prefix}_zero_error"));
        let continue_bb = self
            .context
            .append_basic_block(current_function, &format!("{block_prefix}_zero_continue"));

        self.builder
            .build_conditional_branch(is_zero, error_bb, continue_bb)
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(error_bb);
        let msg = self
            .builder
            .build_global_string_ptr(message, &format!("{block_prefix}_zero_msg"))
            .map_err(|e| e.to_string())?;
        let loc = self.panic_location_arg(span, &format!("{block_prefix}_zero_error"))?;
        self.generate_runtime_call(
            "mux_panic_cstr_code",
            &[
                self.context
                    .i32_type()
                    .const_int(
                        super::runtime::RuntimeErrorCode::DivisionByZero as u64,
                        false,
                    )
                    .into(),
                msg.as_pointer_value().into(),
                loc.into(),
            ],
        );
        self.builder
            .build_unreachable()
            .map_err(|e| e.to_string())?;

        self.builder.position_at_end(continue_bb);
        Ok(())
    }

    /// Signed division and remainder have one overflowing pair in addition to
    /// division by zero: `i64::MIN / -1` (and its remainder). LLVM does not
    /// provide a defined value for that pair, so reject it before emitting the
    /// operation just as the source-language checked arithmetic contract does.
    fn emit_div_overflow_check(
        &mut self,
        dividend: IntValue<'a>,
        divisor: IntValue<'a>,
        span: Option<&Span>,
        block_prefix: &str,
        message: &str,
    ) -> Result<(), String> {
        let int_type = dividend.get_type();
        let min = int_type.const_int(i64::MIN as u64, false);
        let minus_one = int_type.const_int((-1i64) as u64, false);
        let is_min = self
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, dividend, min, "is_min")
            .map_err(|e| e.to_string())?;
        let is_minus_one = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                divisor,
                minus_one,
                "is_minus_one",
            )
            .map_err(|e| e.to_string())?;
        let is_overflow = self
            .builder
            .build_and(is_min, is_minus_one, "div_overflow")
            .map_err(|e| e.to_string())?;
        self.emit_checked_failure(
            is_overflow,
            super::runtime::RuntimeErrorCode::IntegerOverflow,
            message,
            span,
            block_prefix,
        )?;
        Ok(())
    }

    fn generate_exponent_op(
        &mut self,
        left: BasicValueEnum<'a>,
        right: BasicValueEnum<'a>,
    ) -> Result<BasicValueEnum<'a>, String> {
        if let (Ok(left_int), Ok(right_int)) =
            (self.get_raw_int_value(left), self.get_raw_int_value(right))
        {
            let pow_fn = self
                .runtime_function("mux_int_pow")
                .ok_or("mux_int_pow not found")?;
            let result = self
                .builder
                .build_call(pow_fn, &[left_int.into(), right_int.into()], "pow")
                .map_err(|e| e.to_string())?
                .try_as_basic_value()
                .basic()
                .ok_or("Call returned no value")?;
            Ok(result)
        } else if let (Ok(left_float), Ok(right_float)) = (
            self.get_raw_float_value(left),
            self.get_raw_float_value(right),
        ) {
            let pow_fn = self
                .runtime_function("mux_math_pow")
                .ok_or("mux_math_pow not found")?;
            let result = self
                .builder
                .build_call(pow_fn, &[left_float.into(), right_float.into()], "pow")
                .map_err(|e| e.to_string())?
                .try_as_basic_value()
                .basic()
                .ok_or("Call returned no value")?;
            Ok(result)
        } else {
            Err("Unsupported pow operands".to_string())
        }
    }

    /// Emit `==` or `!=`.
    ///
    /// One dispatch for both, because they differ only in the predicates and
    /// runtime functions they use. Keeping them as two parallel matches is how
    /// `Type::Tuple` ended up supported by `==` and not by `!=`; sharing the
    /// arms makes that divergence unrepresentable (issue #360).
    ///
    /// The arms below must stay in step with
    /// `resolve_equality_binary_operator` in `semantics/mod.rs`, which rejects
    /// anything they cannot handle. Because of that, falling through to the
    /// error now means the two have drifted apart - a real compiler bug, which
    /// is what the internal-compiler-error wording claims.
    ///
    /// The one known way to reach it from a valid-looking program is a generic
    /// instantiated with an uncomparable type:
    ///
    /// ```mux
    /// func same<T>(T a, T b) returns bool { return a == b }
    /// same(Point.new(), Point.new())
    /// ```
    ///
    /// `T` is a placeholder that semantics has to accept, and monomorphisation
    /// substitutes the concrete type only here. Note this is NOT the same gap as
    /// issue #361 (declared trait bounds are never checked against concrete type
    /// arguments): the function above declares no bound at all, so checking
    /// declared bounds cannot reject it. Closing it needs the operator in the
    /// body to impose a bound on `T`, or a re-check at monomorphisation.
    fn generate_equality_op(
        &mut self,
        left_expr: &ExpressionNode,
        left: BasicValueEnum<'a>,
        right: BasicValueEnum<'a>,
        kind: EqualityKind,
    ) -> Result<BasicValueEnum<'a>, String> {
        let left_type = self
            .resolve_expression_type_with_fallback(left_expr)
            .map_err(|e| {
                format!(
                    "Failed to get left operand type for '{}': {}",
                    kind.operator(),
                    e
                )
            })?;

        match &left_type {
            Type::Primitive(PrimitiveType::Str) => {
                let left_ptr = self.ensure_pointer(left);
                let right_ptr = self.ensure_pointer(right);
                self.call_string_comparison(
                    left_ptr,
                    right_ptr,
                    kind.string_runtime(),
                    kind.string_label(),
                )
            }
            Type::Primitive(PrimitiveType::Int | PrimitiveType::Char) => {
                let left_int = self.get_raw_int_value(left)?;
                let right_int = self.get_raw_int_value(right)?;
                self.builder
                    .build_int_compare(kind.int_predicate(), left_int, right_int, kind.label())
                    .map_err(|e| e.to_string())
                    .map(std::convert::Into::into)
            }
            Type::Primitive(PrimitiveType::Bool) => {
                let left_bool = self.get_raw_bool_value(left)?;
                let right_bool = self.get_raw_bool_value(right)?;
                self.builder
                    .build_int_compare(kind.int_predicate(), left_bool, right_bool, kind.label())
                    .map_err(|e| e.to_string())
                    .map(std::convert::Into::into)
            }
            Type::Primitive(PrimitiveType::Float) => {
                let left_float = self.get_raw_float_value(left)?;
                let right_float = self.get_raw_float_value(right)?;
                self.builder
                    .build_float_compare(
                        kind.float_predicate(),
                        left_float,
                        right_float,
                        kind.float_label(),
                    )
                    .map_err(|e| e.to_string())
                    .map(std::convert::Into::into)
            }
            // optional/result are heap `*mut Value` like the collections, and
            // mux_value_equal already compares them structurally - a set
            // de-duplicates `some(1)` against `some(1)` today. Only the
            // operator dispatch was missing.
            Type::List(_)
            | Type::Map(_, _)
            | Type::Set(_)
            | Type::Tuple(_, _)
            | Type::Optional(_)
            | Type::Result(_, _)
            | Type::EmptyList
            | Type::EmptyMap
            | Type::EmptySet => {
                let left_ptr = self.ensure_pointer(left);
                let right_ptr = self.ensure_pointer(right);
                self.call_comparison_runtime(
                    left_ptr,
                    right_ptr,
                    kind.value_runtime(),
                    kind.value_label(),
                )
            }
            // A user enum compares structurally, the same way it already does as
            // a set member or map key. Classes also land in Type::Named, so the
            // guard keeps this to enums - and optional/result are seeded into
            // enum_variants but are heap `*mut Value` rather than inline
            // structs, so they are excluded by name as every other consumer
            // excludes them.
            Type::Named(name, type_args)
                if self.enum_variants.contains_key(name)
                    && !matches!(name.as_str(), "optional" | "result") =>
            {
                // A generic enum compares through its instantiation's glue.
                // Using the base enum's would read `Box<int>`'s inline i64
                // payload as the pointer the uninstantiated layout expects,
                // which faults at runtime rather than failing to build
                // (issue #359).
                let enum_name = self.mangled_enum_name(name, type_args);
                let left_ptr = self.enum_struct_pointer(left, &enum_name)?;
                let right_ptr = self.enum_struct_pointer(right, &enum_name)?;
                self.call_enum_comparison(
                    left_ptr,
                    right_ptr,
                    &enum_name,
                    kind.int_predicate(),
                    kind.enum_label(),
                )
            }
            // A class compares through the method its capability gives it: its
            // own `eq`, or `cmp` tested against zero when it only orders itself.
            Type::Named(class_name, type_args)
                if self.analyzer.class_supports_equality(class_name) =>
            {
                let equal = self.call_class_equality(class_name, type_args, left, right)?;
                // `eq` answers equality, so `!=` is its negation rather than a
                // second method the class has to write.
                let expected = equal
                    .get_type()
                    .const_int(u64::from(matches!(kind, EqualityKind::Equal)), false);
                self.builder
                    .build_int_compare(inkwell::IntPredicate::EQ, equal, expected, kind.label())
                    .map_err(|e| e.to_string())
                    .map(std::convert::Into::into)
            }
            _ => Err(format!(
                "{} comparison not supported for type: {:?}",
                kind.noun(),
                left_type
            )),
        }
    }

    /// Ask a class whether two instances are equal, as a single `i1`.
    ///
    /// A class that wrote `eq` answers directly. One that only declared
    /// `Comparable` answers through `cmp`, so declaring an order is enough to
    /// get `==` and the class does not write the same test twice.
    fn call_class_equality(
        &mut self,
        class_name: &str,
        type_args: &[Type],
        left: BasicValueEnum<'a>,
        right: BasicValueEnum<'a>,
    ) -> Result<IntValue<'a>, String> {
        let eq_name = self.create_specialized_method_name(class_name, type_args, "eq");
        // Only a capability that requires `eq` has had its signature checked;
        // a `Comparable` class may carry an unrelated method of that name.
        if self.analyzer.class_declares_equality_method(class_name)
            && let Some(func) = self.module.get_function(&eq_name)
        {
            let equal = self
                .builder
                .build_call(func, &[left.into(), right.into()], "eq_call")
                .map_err(|e| e.to_string())?
                .try_as_basic_value()
                .basic()
                .ok_or_else(|| format!("{eq_name} should return a value"))?;
            return self.get_raw_bool_value(equal);
        }
        let cmp_name = self.create_specialized_method_name(class_name, type_args, "cmp");
        let func = self
            .module
            .get_function(&cmp_name)
            .ok_or_else(|| format!("neither {eq_name} nor {cmp_name} found"))?;
        let ordering = self
            .builder
            .build_call(func, &[left.into(), right.into()], "cmp_call")
            .map_err(|e| e.to_string())?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| format!("{cmp_name} should return a value"))?;
        let ordering = self.get_raw_int_value(ordering)?;
        let zero = ordering.get_type().const_zero();
        self.builder
            .build_int_compare(inkwell::IntPredicate::EQ, ordering, zero, "cmp_equal")
            .map_err(|e| e.to_string())
    }

    fn generate_in_op(
        &mut self,
        left_expr: &ExpressionNode,
        right_expr: &ExpressionNode,
        left: BasicValueEnum<'a>,
        right: BasicValueEnum<'a>,
    ) -> Result<BasicValueEnum<'a>, String> {
        let right_type = self
            .resolve_expression_type_with_fallback(right_expr)
            .map_err(|e| format!("Failed to get right operand type for 'in': {e}"))?;
        let left_type = self
            .resolve_expression_type_with_fallback(left_expr)
            .map_err(|e| format!("Failed to get left operand type for 'in': {e}"))?;

        match right_type {
            Type::List(_) | Type::EmptyList => {
                let raw_list = self.extract_list_from_value(right.into_pointer_value())?;
                // Box an enum element as a managed value so its structural
                // comparison matches the list's stored elements (issue #309).
                let item_ptr = self.box_enum_or_value(left, &left_type)?;
                let result = self
                    .generate_runtime_call("mux_list_contains", &[raw_list.into(), item_ptr.into()])
                    .ok_or("mux_list_contains returned no value")?;
                let free_fn = self
                    .runtime_function("mux_free_list")
                    .ok_or("mux_free_list not found")?;
                self.builder
                    .build_call(free_fn, &[raw_list.into()], "free_list")
                    .map_err(|e| e.to_string())?;
                Ok(result)
            }
            Type::Set(_) | Type::EmptySet => {
                let raw_set = self.extract_set_from_value(right.into_pointer_value())?;
                let item_ptr = self.box_enum_or_value(left, &left_type)?;
                let result = self
                    .generate_runtime_call("mux_set_contains", &[raw_set.into(), item_ptr.into()])
                    .ok_or("mux_set_contains returned no value")?;
                let free_fn = self
                    .runtime_function("mux_free_set")
                    .ok_or("mux_free_set not found")?;
                self.builder
                    .build_call(free_fn, &[raw_set.into()], "free_set")
                    .map_err(|e| e.to_string())?;
                Ok(result)
            }
            // `key in map` tests key membership.
            Type::Map(_, _) | Type::EmptyMap => {
                let raw_map = self.extract_map_from_value(right.into_pointer_value())?;
                let key_ptr = self.box_enum_or_value(left, &left_type)?;
                let result = self
                    .generate_runtime_call("mux_map_contains", &[raw_map.into(), key_ptr.into()])
                    .ok_or("mux_map_contains returned no value")?;
                let free_fn = self
                    .runtime_function("mux_free_map")
                    .ok_or("mux_free_map not found")?;
                self.builder
                    .build_call(free_fn, &[raw_map.into()], "free_map")
                    .map_err(|e| e.to_string())?;
                Ok(result)
            }
            Type::Primitive(PrimitiveType::Str) => {
                let left_type = self
                    .resolve_expression_type_with_fallback(left_expr)
                    .map_err(|e| format!("Failed to get left operand type for 'in': {e}"))?;
                let string_ptr = right.into_pointer_value();

                match left_type {
                    Type::Primitive(PrimitiveType::Char) => {
                        let char_i64 = left.into_int_value();
                        let contains_fn = self
                            .runtime_function("mux_string_contains_char")
                            .ok_or("mux_string_contains_char not found")?;
                        let result = self
                            .builder
                            .build_call(
                                contains_fn,
                                &[string_ptr.into(), char_i64.into()],
                                "string_contains_char",
                            )
                            .map_err(|e| e.to_string())?
                            .try_as_basic_value()
                            .basic()
                            .expect("mux_string_contains_char should return a basic value");
                        Ok(result)
                    }
                    Type::Primitive(PrimitiveType::Str) => {
                        let substring_ptr = left.into_pointer_value();
                        let contains_fn = self
                            .runtime_function("mux_string_contains")
                            .ok_or("mux_string_contains not found")?;
                        let result = self
                            .builder
                            .build_call(
                                contains_fn,
                                &[string_ptr.into(), substring_ptr.into()],
                                "string_contains",
                            )
                            .map_err(|e| e.to_string())?
                            .try_as_basic_value()
                            .basic()
                            .expect("mux_string_contains should return a basic value");
                        Ok(result)
                    }
                    _ => Err(format!(
                        "Invalid left operand type for 'in' operator with string: {left_type:?}"
                    )),
                }
            }
            _ => Err(format!(
                "'in' operator not supported for type: {right_type:?}"
            )),
        }
    }

    pub(super) fn generate_binary_op(
        &mut self,
        left_expr: &ExpressionNode,
        left: BasicValueEnum<'a>,
        op: &BinaryOp,
        right_expr: &ExpressionNode,
        right: BasicValueEnum<'a>,
    ) -> Result<BasicValueEnum<'a>, String> {
        match op {
            BinaryOp::Add => self.generate_add_op(left_expr, left, right),
            BinaryOp::Subtract => self.generate_checked_arithmetic_op(
                left,
                right,
                Some(left_expr.span()),
                CheckedArithmeticKind::Subtract,
            ),
            BinaryOp::Multiply => self.generate_checked_arithmetic_op(
                left,
                right,
                Some(left_expr.span()),
                CheckedArithmeticKind::Multiply,
            ),
            BinaryOp::Divide => self.generate_division_op(
                left,
                right,
                Some(right_expr.span()),
                DivisionKind::Divide,
            ),
            BinaryOp::Exponent => self.generate_exponent_op(left, right),
            BinaryOp::Equal => {
                self.generate_equality_op(left_expr, left, right, EqualityKind::Equal)
            }
            BinaryOp::Less => self.generate_ordering_op(
                left_expr,
                left,
                right,
                inkwell::IntPredicate::SLT,
                inkwell::FloatPredicate::OLT,
                "lt",
            ),
            BinaryOp::Greater => self.generate_ordering_op(
                left_expr,
                left,
                right,
                inkwell::IntPredicate::SGT,
                inkwell::FloatPredicate::OGT,
                "gt",
            ),
            BinaryOp::LessEqual => self.generate_ordering_op(
                left_expr,
                left,
                right,
                inkwell::IntPredicate::SLE,
                inkwell::FloatPredicate::OLE,
                "le",
            ),
            BinaryOp::GreaterEqual => self.generate_ordering_op(
                left_expr,
                left,
                right,
                inkwell::IntPredicate::SGE,
                inkwell::FloatPredicate::OGE,
                "ge",
            ),
            BinaryOp::NotEqual => {
                self.generate_equality_op(left_expr, left, right, EqualityKind::NotEqual)
            }
            BinaryOp::LogicalAnd | BinaryOp::LogicalOr => {
                // These should be handled by generate_short_circuit_logical_op
                // and should not reach here
                Err("Logical AND/OR should use short-circuit evaluation".to_string())
            }
            BinaryOp::Modulo => self.generate_division_op(
                left,
                right,
                Some(right_expr.span()),
                DivisionKind::Modulo,
            ),
            BinaryOp::In => self.generate_in_op(left_expr, right_expr, left, right),
            _ => Err("Binary op not implemented".to_string()),
        }
    }
}
