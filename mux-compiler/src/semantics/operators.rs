use super::{SemanticAnalyzer, SymbolKind, Type};
use crate::ast::{BinaryOp, PrimitiveType};

impl SemanticAnalyzer {
    pub(super) fn resolve_binary_operator(
        &self,
        left_type: &Type,
        right_type: &Type,
        op: &BinaryOp,
    ) -> Option<Type> {
        if matches!(op, BinaryOp::In) {
            return self.resolve_in_binary_operator(left_type, right_type);
        }

        if left_type != right_type {
            return None;
        }

        self.resolve_equal_type_binary_operator(left_type, right_type, op)
    }

    fn resolve_in_binary_operator(&self, left_type: &Type, right_type: &Type) -> Option<Type> {
        match right_type {
            Type::List(_) | Type::Set(_) => Some(Type::Primitive(PrimitiveType::Bool)),
            Type::Map(key_type, _) => {
                if left_type == key_type.as_ref() {
                    Some(Type::Primitive(PrimitiveType::Bool))
                } else {
                    None
                }
            }
            Type::Primitive(PrimitiveType::Str) => {
                if matches!(
                    left_type,
                    Type::Primitive(PrimitiveType::Char | PrimitiveType::Str)
                ) {
                    Some(Type::Primitive(PrimitiveType::Bool))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn resolve_equal_type_binary_operator(
        &self,
        left_type: &Type,
        right_type: &Type,
        op: &BinaryOp,
    ) -> Option<Type> {
        match op {
            BinaryOp::Add => self.resolve_add_binary_operator(left_type),
            BinaryOp::Subtract
            | BinaryOp::Multiply
            | BinaryOp::Divide
            | BinaryOp::Modulo
            | BinaryOp::Exponent => self.resolve_numeric_binary_operator(left_type),
            BinaryOp::Equal | BinaryOp::NotEqual => {
                self.resolve_equality_binary_operator(left_type)
            }
            BinaryOp::Less | BinaryOp::LessEqual | BinaryOp::Greater | BinaryOp::GreaterEqual => {
                self.resolve_comparison_binary_operator(left_type)
            }
            BinaryOp::LogicalAnd | BinaryOp::LogicalOr => {
                self.resolve_logical_binary_operator(left_type, right_type)
            }
            _ => None,
        }
    }

    fn resolve_add_binary_operator(&self, left_type: &Type) -> Option<Type> {
        if matches!(
            left_type,
            Type::Primitive(PrimitiveType::Str | PrimitiveType::Int | PrimitiveType::Float)
                | Type::List(_)
                | Type::Map(_, _)
                | Type::Set(_)
        ) {
            Some(left_type.clone())
        } else {
            None
        }
    }

    fn resolve_numeric_binary_operator(&self, left_type: &Type) -> Option<Type> {
        if matches!(
            left_type,
            Type::Primitive(PrimitiveType::Int | PrimitiveType::Float)
        ) {
            Some(left_type.clone())
        } else {
            None
        }
    }

    /// `==` and `!=` accept exactly the types codegen knows how to compare -
    /// see `generate_equality_op` in `codegen/operators.rs`, which this must be
    /// kept in step with.
    ///
    /// Returning `Some` for everything is how comparing a class, a reference, a
    /// function value or a `void` call used to reach codegen and be reported as
    /// an internal compiler error, telling users to file a compiler bug about
    /// their own program. A codegen error that describes user code is a bug in
    /// type checking (issue #360), so the rejection belongs here, where there is
    /// a span to point at.
    pub(super) fn resolve_equality_binary_operator(&self, left_type: &Type) -> Option<Type> {
        let comparable = match left_type {
            // Void and Auto are deliberately absent: neither is a value.
            Type::Primitive(
                PrimitiveType::Int
                | PrimitiveType::Float
                | PrimitiveType::Bool
                | PrimitiveType::Char
                | PrimitiveType::Str,
            ) => true,
            // Compared structurally by the runtime, which is the same
            // comparison that already makes them usable as map keys.
            Type::List(_)
            | Type::Map(_, _)
            | Type::Set(_)
            | Type::Tuple(_, _)
            | Type::Optional(_)
            | Type::Result(_, _)
            | Type::EmptyList
            | Type::EmptyMap
            | Type::EmptySet => true,
            // Classes and interfaces also land in Named. Only enums have the
            // generated structural compare, but a class may opt in by declaring
            // `is Equatable` and writing `eq`.
            Type::Named(name, _) => self.is_enum_name(name) || self.class_supports_equality(name),
            // A type parameter must declare `Equatable` to be compared. Using
            // the operator imposes the bound rather than inferring it, so the
            // error lands on the declaration the reader can fix instead of on
            // each instantiation. `<` already works this way via
            // `resolve_comparison_binary_operator`, and a method call already
            // imposes its own bound, so this makes `==` consistent with both
            // (issue #361).
            //
            // `Instantiated` is absent: semantics never builds one (only
            // codegen's substitution helpers do), and `generate_equality_op` has
            // no arm for it, so accepting it could only ever admit an internal
            // compiler error.
            Type::Generic(_) | Type::Variable(_) => {
                self.type_implements_interface(left_type, "Equatable")
            }
            _ => false,
        };

        comparable.then_some(Type::Primitive(PrimitiveType::Bool))
    }

    /// Whether `name` names a user enum - one with the generated structural
    /// compare - as opposed to a class or an interface.
    ///
    /// `optional` and `result` are excluded to mirror codegen, which excludes
    /// them by name because they are heap `*mut Value`s rather than inline
    /// structs despite being seeded into `enum_variants`. Declaring a type under
    /// either name is now rejected outright (issue #369), so this is defence in
    /// depth against the two sides drifting rather than a reachable case.
    fn is_enum_name(&self, name: &str) -> bool {
        if matches!(name, "optional" | "result") {
            return false;
        }
        self.symbol_table
            .lookup(name)
            .is_some_and(|symbol| symbol.kind == SymbolKind::Enum)
    }

    fn resolve_comparison_binary_operator(&self, left_type: &Type) -> Option<Type> {
        if matches!(
            left_type,
            Type::Primitive(PrimitiveType::Int | PrimitiveType::Float | PrimitiveType::Str)
        ) || self.type_implements_interface(left_type, "Comparable")
        {
            Some(Type::Primitive(PrimitiveType::Bool))
        } else {
            None
        }
    }

    fn resolve_logical_binary_operator(&self, left_type: &Type, right_type: &Type) -> Option<Type> {
        if matches!(left_type, Type::Primitive(PrimitiveType::Bool))
            && matches!(right_type, Type::Primitive(PrimitiveType::Bool))
        {
            Some(Type::Primitive(PrimitiveType::Bool))
        } else {
            None
        }
    }
}
