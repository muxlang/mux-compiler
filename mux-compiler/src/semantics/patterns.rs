use super::{SemanticAnalyzer, SemanticError, Symbol, SymbolKind, Type, format_type};
use crate::ast::{LiteralNode, PatternNode, PrimitiveType};
use crate::diagnostic::DiagnosticCode;
use crate::lexer::Span;
use std::collections::HashMap;

impl SemanticAnalyzer {
    pub(super) fn set_pattern_types(
        &mut self,
        pattern: &PatternNode,
        expected_type: &Type,
        span: Span,
    ) -> Result<(), SemanticError> {
        match pattern {
            PatternNode::Identifier(name) => {
                self.handle_identifier_pattern(name, expected_type, span)?;
            }
            PatternNode::EnumVariant { name, args } => {
                self.handle_enum_variant_pattern(name, args, expected_type, span)?;
            }
            PatternNode::Literal(lit) => {
                self.handle_literal_pattern(lit, expected_type, span)?;
            }
            PatternNode::List { elements, rest } => {
                self.handle_list_pattern(elements, rest, expected_type, span)?;
            }
            PatternNode::Wildcard => {}
        }
        Ok(())
    }

    fn handle_identifier_pattern(
        &mut self,
        name: &str,
        expected_type: &Type,
        span: Span,
    ) -> Result<(), SemanticError> {
        let is_constant = self
            .symbol_table
            .lookup(name)
            .is_some_and(|s| s.kind == SymbolKind::Constant);

        if is_constant {
            let const_type = self
                .symbol_table
                .lookup(name)
                .and_then(|s| s.type_.clone())
                .ok_or_else(|| {
                    SemanticError::new(
                        DiagnosticCode::InvalidOperation,
                        format!("Constant '{name}' has no type information"),
                        span,
                    )
                })?;
            self.check_type_compatibility(&const_type, expected_type, span)?;
        } else {
            self.symbol_table.add_symbol(
                name,
                Symbol {
                    kind: SymbolKind::Variable,
                    span,
                    type_: Some(expected_type.clone()),
                    interfaces: HashMap::new(),
                    methods: HashMap::new(),
                    fields: HashMap::new(),
                    type_params: Vec::new(),
                    original_name: None,
                    llvm_name: None,
                    default_param_count: 0,
                    variants: None,
                },
            )?;
        }
        Ok(())
    }

    fn handle_enum_variant_pattern(
        &mut self,
        name: &str,
        args: &[PatternNode],
        expected_type: &Type,
        span: Span,
    ) -> Result<(), SemanticError> {
        match expected_type {
            Type::Optional(inner) => self.match_optional_variant(name, args, inner, span),
            Type::Result(ok_type, err_type) => {
                self.match_result_variant(name, args, ok_type, err_type, span)
            }
            Type::Named(enum_name, type_args) => {
                let symbol = self
                    .symbol_table
                    .lookup(enum_name)
                    .ok_or_else(|| self.undefined_symbol_error("type", enum_name, span))?;
                self.match_enum_variant(name, args, enum_name, &symbol, type_args, span)
            }
            _ => Err(SemanticError::with_help(
                DiagnosticCode::InvalidPattern,
                format!(
                    "Enum variant patterns are not supported for type {}",
                    format_type(expected_type)
                ),
                span,
                "Variant patterns can only be used with enum, Optional, or Result types",
            )),
        }
    }

    fn match_optional_variant(
        &mut self,
        name: &str,
        args: &[PatternNode],
        inner: &Type,
        span: Span,
    ) -> Result<(), SemanticError> {
        if name == "some" && args.len() == 1 {
            self.set_pattern_types(&args[0], inner, span)?;
            Ok(())
        } else if name == "none" && args.is_empty() {
            Ok(())
        } else {
            Err(SemanticError::with_help(
                DiagnosticCode::InvalidPattern,
                format!(
                    "Pattern '{}' does not match type {}",
                    name,
                    format_type(&Type::Optional(Box::new(inner.clone())))
                ),
                span,
                "Optional values can only be matched with Some(value) or None",
            ))
        }
    }

    fn match_result_variant(
        &mut self,
        name: &str,
        args: &[PatternNode],
        ok_type: &Type,
        err_type: &Type,
        span: Span,
    ) -> Result<(), SemanticError> {
        if name == "ok" && args.len() == 1 {
            self.set_pattern_types(&args[0], ok_type, span)?;
            Ok(())
        } else if name == "err" && args.len() == 1 {
            self.set_pattern_types(&args[0], err_type, span)?;
            Ok(())
        } else {
            Err(SemanticError::with_help(
                DiagnosticCode::InvalidPattern,
                format!(
                    "Pattern '{}' does not match type {}",
                    name,
                    format_type(&Type::Result(
                        Box::new(ok_type.clone()),
                        Box::new(err_type.clone())
                    ))
                ),
                span,
                "Result values can only be matched with Ok(value) or Err(value)",
            ))
        }
    }

    fn handle_literal_pattern(
        &mut self,
        lit: &LiteralNode,
        expected_type: &Type,
        span: Span,
    ) -> Result<(), SemanticError> {
        let literal_type = match lit {
            LiteralNode::Integer(_) => Type::Primitive(PrimitiveType::Int),
            LiteralNode::Float(_) => Type::Primitive(PrimitiveType::Float),
            LiteralNode::String(_) => Type::Primitive(PrimitiveType::Str),
            LiteralNode::Boolean(_) => Type::Primitive(PrimitiveType::Bool),
            LiteralNode::Char(_) => Type::Primitive(PrimitiveType::Char),
        };
        self.check_type_compatibility(&literal_type, expected_type, span)
    }

    fn handle_list_pattern(
        &mut self,
        elements: &[PatternNode],
        rest: &Option<Box<PatternNode>>,
        expected_type: &Type,
        span: Span,
    ) -> Result<(), SemanticError> {
        let inner_type = match expected_type {
            Type::List(inner) => (**inner).clone(),
            Type::EmptyList => Type::Void,
            _ => {
                return Err(SemanticError::with_help(
                    DiagnosticCode::InvalidPattern,
                    format!(
                        "List pattern cannot match type {}",
                        format_type(expected_type)
                    ),
                    span,
                    "List patterns (e.g. [head, ...rest]) can only match list types",
                ));
            }
        };
        for elem in elements {
            self.set_pattern_types(elem, &inner_type, span)?;
        }
        if let Some(rest_pat) = rest {
            let rest_type = Type::List(Box::new(inner_type));
            self.set_pattern_types(rest_pat, &rest_type, span)?;
        }
        Ok(())
    }

    fn match_enum_variant(
        &mut self,
        name: &str,
        args: &[PatternNode],
        enum_name: &str,
        symbol: &Symbol,
        type_args: &[Type],
        span: Span,
    ) -> Result<(), SemanticError> {
        let sig = symbol.methods.get(name).ok_or_else(|| {
            let available_variants: Vec<&String> = symbol.methods.keys().collect();
            if available_variants.is_empty() {
                SemanticError::new(
                    DiagnosticCode::InvalidOperation,
                    format!("Unknown variant '{name}' for enum '{enum_name}'"),
                    span,
                )
            } else {
                SemanticError::with_help(
                    DiagnosticCode::InvalidOperation,
                    format!("Unknown variant '{name}' for enum '{enum_name}'"),
                    span,
                    format!(
                        "Available variants: {}",
                        available_variants
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )
            }
        })?;
        if args.len() != sig.params.len() {
            return Err(SemanticError::with_help(
                DiagnosticCode::WrongArgumentCount,
                format!(
                    "Variant '{}' expects {} argument{}, but pattern provides {}",
                    name,
                    sig.params.len(),
                    if sig.params.len() == 1 { "" } else { "s" },
                    args.len()
                ),
                span,
                format!(
                    "Match the variant with exactly {} argument{}",
                    sig.params.len(),
                    if sig.params.len() == 1 { "" } else { "s" }
                ),
            ));
        }
        // Substitute the subject's type arguments into the variant's declared
        // payload types, so matching a `Box<int>` binds an int rather than the
        // parameter T (issue #359).
        let sig = if type_args.is_empty() {
            sig.clone()
        } else {
            self.substitute_method_sig(sig, &symbol.type_params, type_args)
        };
        for (arg, param_type) in args.iter().zip(&sig.params) {
            self.set_pattern_types(arg, param_type, span)?;
        }
        Ok(())
    }

    #[allow(clippy::only_used_in_recursion)]
    pub(super) fn analyze_pattern(&mut self, pattern: &PatternNode) -> Result<(), SemanticError> {
        match pattern {
            PatternNode::Identifier(_) | PatternNode::Literal(_) | PatternNode::Wildcard => {}
            PatternNode::EnumVariant { args, .. } => {
                for arg in args {
                    self.analyze_pattern(arg)?;
                }
            }
            PatternNode::List { elements, rest } => {
                for elem in elements {
                    self.analyze_pattern(elem)?;
                }
                if let Some(rest_pat) = rest {
                    self.analyze_pattern(rest_pat)?;
                }
            }
        }
        Ok(())
    }
}
