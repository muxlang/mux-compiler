//! Type parsing for the Mux parser.

use super::{Parser, ParserError, ParserResult};
use crate::diagnostic::DiagnosticCode;
use crate::lexer::{ByteRange, Span, Token, TokenType};
use crate::syntax::{SyntaxData, SyntaxKind};

/// Parser-owned type facts retain source ranges for later syntax lowering.
#[derive(Clone, Debug)]
pub(super) struct TypeFact {
    kind: TypeFactKind,
    span: Span,
}

#[derive(Clone, Debug)]
enum TypeFactKind {
    Primitive,
    Named {
        name_range: ByteRange,
        args: Vec<TypeFact>,
    },
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

#[derive(Clone, Copy)]
enum TypeNameClass {
    Primitive,
    List,
    Map,
    Set,
    Tuple,
    Dyn,
    Named,
}

impl TypeNameClass {
    fn classify(name: &str) -> Self {
        match name {
            "int" | "float" | "bool" | "char" | "byte" | "bytes" | "string" | "void" | "auto" => {
                Self::Primitive
            }
            "list" => Self::List,
            "map" => Self::Map,
            "set" => Self::Set,
            "tuple" => Self::Tuple,
            "dyn" => Self::Dyn,
            _ => Self::Named,
        }
    }
}

impl TypeFact {
    pub(super) fn void(span: Span) -> Self {
        Self {
            kind: TypeFactKind::Primitive,
            span,
        }
    }

    pub(super) fn source_range(&self) -> Option<ByteRange> {
        self.span.byte_range
    }

    pub(super) fn generic_parameter_name_range(&self) -> Option<ByteRange> {
        match &self.kind {
            TypeFactKind::Named {
                name_range, args, ..
            } if args.is_empty() => Some(*name_range),
            _ => None,
        }
    }
}

impl<'a> Parser<'a> {
    fn parse_named_type_with_builtin_support(
        &mut self,
        name_class: TypeNameClass,
        name_token: usize,
        start_span: Span,
    ) -> ParserResult<TypeFact> {
        let mut name_range = self.tokens[name_token]
            .span
            .byte_range
            .expect("type name token has a source range");
        let mut qualified = false;
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
            self.consume_identifier_fact("Expected a type name after '.'")?;
            let segment_range = self
                .previous()
                .span
                .byte_range
                .expect("type name segment has a source range");
            name_range = ByteRange::new(name_range.start, segment_range.end);
            qualified = true;
        }

        if !qualified && matches!(name_class, TypeNameClass::Primitive) {
            return Ok(TypeFact {
                kind: TypeFactKind::Primitive,
                span: start_span,
            });
        }

        if !qualified && let Some(node) = self.parse_container_type(name_class, start_span)? {
            return Ok(node);
        }

        let mut type_args = if self.matches(&[TokenType::Lt]) {
            let args = self.parse_type_argument_facts()?;
            self.consume_token(TokenType::Gt, "Expected '>' after type arguments")?;
            args
        } else {
            Vec::new()
        };

        if !qualified && matches!(name_class, TypeNameClass::Dyn) && !type_args.is_empty() {
            let trait_object = type_args.remove(0);
            return Ok(TypeFact {
                kind: TypeFactKind::TraitObject(Box::new(trait_object)),
                span: start_span,
            });
        }

        Ok(TypeFact {
            kind: TypeFactKind::Named {
                name_range,
                args: type_args,
            },
            span: start_span,
        })
    }

    fn parse_container_type(
        &mut self,
        name: TypeNameClass,
        start_span: Span,
    ) -> ParserResult<Option<TypeFact>> {
        if !matches!(
            name,
            TypeNameClass::List | TypeNameClass::Map | TypeNameClass::Set | TypeNameClass::Tuple
        ) {
            return Ok(None);
        }

        if !self.matches(&[TokenType::Lt]) {
            return Ok(None);
        }

        let node = match name {
            TypeNameClass::List => {
                let element_type = self.parse_type_fact()?;
                self.consume_token(TokenType::Gt, "Expected '>' after list element type")?;
                TypeFact {
                    kind: TypeFactKind::List(Box::new(element_type)),
                    span: start_span,
                }
            }
            TypeNameClass::Map => {
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
            TypeNameClass::Set => {
                let element_type = self.parse_type_fact()?;
                self.consume_token(TokenType::Gt, "Expected '>' after set element type")?;
                TypeFact {
                    kind: TypeFactKind::Set(Box::new(element_type)),
                    span: start_span,
                }
            }
            TypeNameClass::Tuple => {
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

    /// Parse a type into its source range and shape facts.
    pub(super) fn parse_type_fact(&mut self) -> ParserResult<TypeFact> {
        let start = self.current;
        match self.parse_type_inner() {
            Ok(mut fact) => {
                if let Some(range) = self.source_range(start, self.current)
                    && let (Some(first), Some(last)) = (
                        self.tokens.get(start),
                        self.tokens.get(self.current.saturating_sub(1)),
                    )
                {
                    fact.span.row_start = first.span.row_start;
                    fact.span.col_start = first.span.col_start;
                    fact.span.row_end = last.span.row_end;
                    fact.span.col_end = last.span.col_end;
                    fact.span.byte_range = Some(range);
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
            TypeFactKind::Primitive | TypeFactKind::Named { .. } => SyntaxData::TypeName {
                name: name()?,
                arguments: match kind {
                    TypeFactKind::Named { args, .. } => ranges(args)?,
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
        let token_index = self.current;
        let token_span = self.tokens[token_index].span;
        let is_function_type = self.tokens[token_index].token_type == TokenType::Func;
        let identifier_class = match &self.tokens[token_index].token_type {
            TokenType::Id(name) => Some(TypeNameClass::classify(name)),
            _ => None,
        };
        self.advance();

        if let Some(class) = identifier_class {
            return self.parse_named_type_with_builtin_support(class, token_index, token_span);
        }

        if is_function_type {
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
                span: token_span,
            })
        } else {
            let token_type = self.tokens[token_index].token_type.clone();
            Err(ParserError::from_token(
                DiagnosticCode::ParseExpectedType,
                "Expected type",
                &Token::new(token_type, token_span),
            ))
        }
    }

    pub(super) fn parse_type_argument_facts(&mut self) -> ParserResult<Vec<TypeFact>> {
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
    use crate::syntax::parse_source;

    fn parses_type(source: &str) -> bool {
        let parsed = parse_source(&format!("func parser_type() returns {source} {{}}"));
        parsed.errors.is_empty()
    }

    #[test]
    fn parses_nested_container_types() {
        assert!(parses_type("map<string, list<int>>"));
    }

    #[test]
    fn parses_function_reference_types() {
        assert!(parses_type("&func(int, string) returns bool"));
    }

    #[test]
    fn parses_dynamic_interface_types() {
        assert!(parses_type("dyn<Greeter>"));
    }
}
