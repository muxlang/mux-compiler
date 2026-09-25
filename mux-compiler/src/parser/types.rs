//! Type parsing for the Mux parser.

use super::{Parser, ParserError, ParserResult};
use crate::ast::{PrimitiveType, TypeKind, TypeNode};
use crate::diagnostic::DiagnosticCode;
use crate::lexer::{ByteRange, Span, Token, TokenType};
use crate::syntax::{SyntaxData, SyntaxKind};

/// Parser-owned type facts retain the source range without coupling recursive
/// parsing to the compatibility AST consumed by existing parser clients.
#[derive(Clone, Debug)]
pub(super) struct TypeFact {
    kind: TypeFactKind,
    span: Span,
}

#[derive(Clone, Debug)]
enum TypeFactKind {
    Primitive(PrimitiveType),
    Named(String, Vec<TypeFact>),
    TraitObject(Box<TypeFact>),
    Reference(Box<TypeFact>),
    List(Box<TypeFact>),
    Map(Box<TypeFact>, Box<TypeFact>),
    Set(Box<TypeFact>),
    Tuple(Box<TypeFact>, Box<TypeFact>),
    Function {
        params: Vec<TypeFact>,
        returns: Box<TypeFact>,
    },
}

impl TypeFact {
    pub(super) fn into_compat_type_node(self) -> TypeNode {
        let kind = match self.kind {
            TypeFactKind::Primitive(kind) => TypeKind::Primitive(kind),
            TypeFactKind::Named(name, args) => TypeKind::Named(
                name,
                args.into_iter()
                    .map(TypeFact::into_compat_type_node)
                    .collect(),
            ),
            TypeFactKind::TraitObject(inner) => {
                TypeKind::TraitObject(Box::new(inner.into_compat_type_node()))
            }
            TypeFactKind::Reference(inner) => {
                TypeKind::Reference(Box::new(inner.into_compat_type_node()))
            }
            TypeFactKind::List(inner) => TypeKind::List(Box::new(inner.into_compat_type_node())),
            TypeFactKind::Map(key, value) => TypeKind::Map(
                Box::new(key.into_compat_type_node()),
                Box::new(value.into_compat_type_node()),
            ),
            TypeFactKind::Set(inner) => TypeKind::Set(Box::new(inner.into_compat_type_node())),
            TypeFactKind::Tuple(left, right) => TypeKind::Tuple(
                Box::new(left.into_compat_type_node()),
                Box::new(right.into_compat_type_node()),
            ),
            TypeFactKind::Function { params, returns } => TypeKind::Function {
                params: params
                    .into_iter()
                    .map(TypeFact::into_compat_type_node)
                    .collect(),
                returns: Box::new(returns.into_compat_type_node()),
            },
        };
        TypeNode {
            kind,
            span: self.span,
        }
    }

    pub(super) fn source_range(&self) -> Option<ByteRange> {
        self.span.byte_range
    }
}

impl<'a> Parser<'a> {
    fn parse_named_type_with_builtin_support(
        &mut self,
        mut name: String,
        start_span: Span,
    ) -> ParserResult<TypeFact> {
        // A module-qualified type, `graph.Graph<string>`. This is the form
        // `import std.dsa.*` naturally leads you to write - the same path that
        // already works in expression position - and it was rejected outright
        // in a type position, so a function could not take or return one (#391).
        //
        // A type is never followed by a field access, so a dot here is
        // unambiguous and consuming it greedily costs nothing. The qualifier is
        // kept in the name and resolved by the analyzer, which is the only
        // place that knows which module namespaces are in scope.
        while self.check(TokenType::Dot) {
            self.advance();
            let segment = self.consume_identifier("Expected a type name after '.'")?;
            name.push('.');
            name.push_str(&segment);
        }

        if let Ok(prim_type) =
            PrimitiveType::parse(&Token::new(TokenType::Id(name.clone()), start_span))
        {
            return Ok(TypeFact {
                kind: TypeFactKind::Primitive(prim_type),
                span: start_span,
            });
        }

        if let Some(node) = self.parse_container_type(&name, start_span)? {
            return Ok(node);
        }

        let type_args = if self.matches(&[TokenType::Lt]) {
            let args = self.parse_type_argument_facts()?;
            self.consume_token(TokenType::Gt, "Expected '>' after type arguments")?;
            args
        } else {
            Vec::new()
        };

        if name == "dyn" && !type_args.is_empty() {
            return Ok(TypeFact {
                kind: TypeFactKind::TraitObject(Box::new(type_args[0].clone())),
                span: start_span,
            });
        }

        Ok(TypeFact {
            kind: TypeFactKind::Named(name, type_args),
            span: start_span,
        })
    }

    fn parse_container_type(
        &mut self,
        name: &str,
        start_span: Span,
    ) -> ParserResult<Option<TypeFact>> {
        if !matches!(name, "list" | "map" | "set" | "tuple") {
            return Ok(None);
        }

        if !self.matches(&[TokenType::Lt]) {
            return Ok(None);
        }

        let node = match name {
            "list" => {
                let element_type = self.parse_type_fact()?;
                self.consume_token(TokenType::Gt, "Expected '>' after list element type")?;
                TypeFact {
                    kind: TypeFactKind::List(Box::new(element_type)),
                    span: start_span,
                }
            }
            "map" => {
                let key_type = self.parse_type_fact()?;
                self.consume_token(
                    TokenType::Comma,
                    "Expected ',' between key and value types in map",
                )?;
                let value_type = self.parse_type_fact()?;
                self.consume_token(TokenType::Gt, "Expected '>' after map value type")?;
                TypeFact {
                    kind: TypeFactKind::Map(Box::new(key_type), Box::new(value_type)),
                    span: start_span,
                }
            }
            "set" => {
                let element_type = self.parse_type_fact()?;
                self.consume_token(TokenType::Gt, "Expected '>' after set element type")?;
                TypeFact {
                    kind: TypeFactKind::Set(Box::new(element_type)),
                    span: start_span,
                }
            }
            "tuple" => {
                let left_type = self.parse_type_fact()?;
                self.consume_token(TokenType::Comma, "Expected ',' in tuple type")?;
                let right_type = self.parse_type_fact()?;
                self.consume_token(TokenType::Gt, "Expected '>' after tuple type")?;
                TypeFact {
                    kind: TypeFactKind::Tuple(Box::new(left_type), Box::new(right_type)),
                    span: start_span,
                }
            }
            _ => unreachable!("guarded by matches!"),
        };

        Ok(Some(node))
    }

    pub(super) fn parse_type(&mut self) -> ParserResult<TypeNode> {
        self.parse_type_fact().map(TypeFact::into_compat_type_node)
    }

    /// Compatibility adapter for parser clients that still construct AST nodes.
    /// New syntax consumers should work from the fact and its recorded ranges.
    pub(super) fn parse_type_fact(&mut self) -> ParserResult<TypeFact> {
        let start = self.current;
        match self.parse_type_inner() {
            Ok(mut fact) => {
                if let Some(range) = self.source_range(start, self.current) {
                    if let (Some(first), Some(last)) = (
                        self.tokens.get(start),
                        self.tokens.get(self.current.saturating_sub(1)),
                    ) {
                        fact.span.row_start = first.span.row_start;
                        fact.span.col_start = first.span.col_start;
                        fact.span.row_end = last.span.row_end;
                        fact.span.col_end = last.span.col_end;
                        fact.span.byte_range = Some(range);
                    }
                }
                if let Some(data) = self.syntax_data_for_type(start, self.current, &fact.kind) {
                    self.record_typed_syntax_node(SyntaxKind::Type, start, self.current, data);
                } else {
                    self.record_syntax_node(SyntaxKind::Type, start, self.current);
                }
                Ok(fact)
            }
            Err(error) => {
                if self.current > start {
                    self.record_syntax_node(SyntaxKind::Error, start, self.current);
                }
                Err(error)
            }
        }
    }

    fn source_range(&self, start: usize, end: usize) -> Option<ByteRange> {
        let first = self.tokens.get(start)?.span.byte_range?;
        let last = self.tokens.get(end.checked_sub(1)?)?.span.byte_range?;
        Some(ByteRange::new(first.start, last.end))
    }

    fn type_name_range(&self, start: usize, end: usize) -> Option<ByteRange> {
        let mut cursor = start;
        if !matches!(self.tokens.get(cursor)?.token_type, TokenType::Id(_)) {
            return None;
        }
        cursor += 1;
        while cursor + 1 < end
            && self.tokens.get(cursor)?.token_type == TokenType::Dot
            && matches!(self.tokens.get(cursor + 1)?.token_type, TokenType::Id(_))
        {
            cursor += 2;
        }
        self.source_range(start, cursor)
    }

    fn syntax_data_for_type(
        &self,
        start: usize,
        end: usize,
        kind: &TypeFactKind,
    ) -> Option<SyntaxData> {
        let name = || self.type_name_range(start, end);
        let ranges = |types: &[TypeFact]| {
            types
                .iter()
                .map(|node| node.span.byte_range)
                .collect::<Option<Vec<_>>>()
        };
        Some(match kind {
            TypeFactKind::Primitive(_) | TypeFactKind::Named(_, _) => SyntaxData::TypeName {
                name: name()?,
                arguments: match kind {
                    TypeFactKind::Named(_, args) => ranges(args)?,
                    _ => Vec::new(),
                },
            },
            TypeFactKind::TraitObject(inner) => SyntaxData::TypeName {
                name: name()?,
                arguments: vec![inner.span.byte_range?],
            },
            TypeFactKind::Reference(reference) => SyntaxData::TypeReference {
                reference: reference.span.byte_range?,
            },
            TypeFactKind::List(element) | TypeFactKind::Set(element) => SyntaxData::TypeContainer {
                name: name()?,
                arguments: vec![element.span.byte_range?],
            },
            TypeFactKind::Map(key, value) | TypeFactKind::Tuple(key, value) => {
                SyntaxData::TypeContainer {
                    name: name()?,
                    arguments: vec![key.span.byte_range?, value.span.byte_range?],
                }
            }
            TypeFactKind::Function { params, returns } => SyntaxData::FunctionType {
                parameters: ranges(params)?,
                returns: returns.span.byte_range?,
            },
        })
    }

    fn parse_type_inner(&mut self) -> ParserResult<TypeFact> {
        if self.is_at_end() {
            return Err(ParserError::new(
                DiagnosticCode::ParseExpectedToken,
                "Expected a type, but reached end of input",
                self.peek().span,
            ));
        }
        if self.matches(&[TokenType::Ref]) {
            let start_span = self.previous().span;
            let referenced_type = self.parse_type_fact()?;
            return Ok(TypeFact {
                kind: TypeFactKind::Reference(Box::new(referenced_type)),
                span: Span {
                    row_start: start_span.row_start,
                    col_start: start_span.col_start,
                    row_end: self.previous().span.row_end,
                    col_end: self.previous().span.col_end,
                    byte_range: match (start_span.byte_range, self.previous().span.byte_range) {
                        (Some(start), Some(end)) => {
                            Some(crate::lexer::ByteRange::new(start.start, end.end))
                        }
                        _ => None,
                    },
                },
            });
        }

        // We are essentially doing a consume here, but without borrowing the
        // parser again so we do not have to clone it.
        let token_type = self.tokens[self.current].token_type.clone();
        let start_span = self.tokens[self.current].span;
        self.advance();

        match &token_type {
            TokenType::Id(name) => {
                self.parse_named_type_with_builtin_support(name.clone(), start_span)
            }

            TokenType::Func => {
                self.consume_token(
                    TokenType::OpenParen,
                    "Expected '(' after 'func' in function type",
                )?;
                let mut param_types = Vec::new();

                if !self.check(TokenType::CloseParen) {
                    loop {
                        // Parse parameter types only (no parameter names for function types)
                        param_types.push(self.parse_type_fact()?);

                        if !self.matches(&[TokenType::Comma]) {
                            break;
                        }
                        self.skip_newlines();
                    }
                }

                self.consume_token(TokenType::CloseParen, "Expected ')' after parameter types")?;
                self.consume_token(TokenType::Returns, "Expected 'returns' in function type")?;

                let return_type = Box::new(self.parse_type_fact()?);

                Ok(TypeFact {
                    kind: TypeFactKind::Function {
                        params: param_types,
                        returns: return_type,
                    },
                    span: start_span,
                })
            }
            _ => Err(ParserError::from_token(
                DiagnosticCode::ParseExpectedType,
                "Expected type",
                &Token::new(token_type.clone(), start_span),
            )),
        }
    }

    pub(super) fn parse_type_arguments(&mut self) -> ParserResult<Vec<TypeNode>> {
        self.parse_type_argument_facts().map(|args| {
            args.into_iter()
                .map(TypeFact::into_compat_type_node)
                .collect()
        })
    }

    fn parse_type_argument_facts(&mut self) -> ParserResult<Vec<TypeFact>> {
        let start = self.current;
        let mut args = Vec::new();
        while !self.check(TokenType::Gt) && !self.is_at_end() {
            let arg = self.parse_type_fact()?;
            args.push(arg);

            if !self.matches(&[TokenType::Comma]) {
                break;
            }
            self.skip_newlines();
        }
        self.record_syntax_node(SyntaxKind::TypeArguments, start, self.current);
        Ok(args)
    }
}

#[cfg(test)]
mod tests {
    use super::Parser;
    use crate::ast::{PrimitiveType, TypeKind};
    use crate::lexer::Lexer;
    use crate::source::Source;

    fn parse_type(source: &str) -> Result<TypeKind, String> {
        let mut source = Source::from_test_str(source);
        let tokens = Lexer::new(&mut source)
            .lex_all()
            .map_err(|error| format!("type fixture should lex: {error:?}"))?;
        let mut parser = Parser::new(&tokens);
        parser
            .parse_type()
            .map(|node| node.kind)
            .map_err(|error| format!("type fixture should parse: {error:?}"))
    }

    #[test]
    fn parses_nested_container_types() -> Result<(), String> {
        let kind = parse_type("map<string, list<int>>")?;
        let TypeKind::Map(key, value) = kind else {
            return Err("expected map type".to_owned());
        };
        assert!(matches!(key.kind, TypeKind::Primitive(PrimitiveType::Str)));
        let TypeKind::List(element) = value.kind else {
            return Err("expected list value type".to_owned());
        };
        assert!(matches!(
            element.kind,
            TypeKind::Primitive(PrimitiveType::Int)
        ));
        Ok(())
    }

    #[test]
    fn parses_function_reference_types() -> Result<(), String> {
        let kind = parse_type("&func(int, string) returns bool")?;
        let TypeKind::Reference(function) = kind else {
            return Err("expected reference type".to_owned());
        };
        let TypeKind::Function { params, returns } = function.kind else {
            return Err("expected function type".to_owned());
        };
        let first_param = params.first().ok_or("missing first parameter")?;
        let second_param = params.get(1).ok_or("missing second parameter")?;
        assert!(matches!(
            first_param.kind,
            TypeKind::Primitive(PrimitiveType::Int)
        ));
        assert!(matches!(
            second_param.kind,
            TypeKind::Primitive(PrimitiveType::Str)
        ));
        assert!(matches!(
            returns.kind,
            TypeKind::Primitive(PrimitiveType::Bool)
        ));
        Ok(())
    }

    #[test]
    fn parses_dynamic_interface_types() -> Result<(), String> {
        let kind = parse_type("dyn<Greeter>")?;
        let TypeKind::TraitObject(inner) = kind else {
            return Err("expected dynamic interface type".to_owned());
        };
        assert!(
            matches!(inner.kind, TypeKind::Named(ref name, args) if name == "Greeter" && args.is_empty())
        );
        Ok(())
    }
}
