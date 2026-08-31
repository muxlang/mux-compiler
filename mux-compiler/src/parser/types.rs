//! Type parsing for the Mux parser.

use super::{Parser, ParserError, ParserResult};
use crate::ast::{PrimitiveType, TypeKind, TypeNode};
use crate::diagnostic::DiagnosticCode;
use crate::lexer::{Span, Token, TokenType};

impl<'a> Parser<'a> {
    fn parse_named_type_with_builtin_support(
        &mut self,
        mut name: String,
        start_span: Span,
    ) -> ParserResult<TypeNode> {
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
            self.current += 1;
            let segment = self.consume_identifier("Expected a type name after '.'")?;
            name.push('.');
            name.push_str(&segment);
        }

        if let Ok(prim_type) =
            PrimitiveType::parse(&Token::new(TokenType::Id(name.clone()), start_span))
        {
            return Ok(TypeNode {
                kind: TypeKind::Primitive(prim_type),
                span: start_span,
            });
        }

        if let Some(node) = self.parse_container_type(&name, start_span)? {
            return Ok(node);
        }

        let type_args = if self.matches(&[TokenType::Lt]) {
            let args = self.parse_type_arguments()?;
            self.consume_token(TokenType::Gt, "Expected '>' after type arguments")?;
            args
        } else {
            Vec::new()
        };

        if name == "dyn" && !type_args.is_empty() {
            return Ok(TypeNode {
                kind: TypeKind::TraitObject(Box::new(type_args[0].clone())),
                span: start_span,
            });
        }

        Ok(TypeNode {
            kind: TypeKind::Named(name, type_args),
            span: start_span,
        })
    }

    fn parse_container_type(
        &mut self,
        name: &str,
        start_span: Span,
    ) -> ParserResult<Option<TypeNode>> {
        if !matches!(name, "list" | "map" | "set" | "tuple") {
            return Ok(None);
        }

        if !self.matches(&[TokenType::Lt]) {
            return Ok(None);
        }

        let node = match name {
            "list" => {
                let element_type = self.parse_type()?;
                self.consume_token(TokenType::Gt, "Expected '>' after list element type")?;
                TypeNode {
                    kind: TypeKind::List(Box::new(element_type)),
                    span: start_span,
                }
            }
            "map" => {
                let key_type = self.parse_type()?;
                self.consume_token(
                    TokenType::Comma,
                    "Expected ',' between key and value types in map",
                )?;
                let value_type = self.parse_type()?;
                self.consume_token(TokenType::Gt, "Expected '>' after map value type")?;
                TypeNode {
                    kind: TypeKind::Map(Box::new(key_type), Box::new(value_type)),
                    span: start_span,
                }
            }
            "set" => {
                let element_type = self.parse_type()?;
                self.consume_token(TokenType::Gt, "Expected '>' after set element type")?;
                TypeNode {
                    kind: TypeKind::Set(Box::new(element_type)),
                    span: start_span,
                }
            }
            "tuple" => {
                let left_type = self.parse_type()?;
                self.consume_token(TokenType::Comma, "Expected ',' in tuple type")?;
                let right_type = self.parse_type()?;
                self.consume_token(TokenType::Gt, "Expected '>' after tuple type")?;
                TypeNode {
                    kind: TypeKind::Tuple(Box::new(left_type), Box::new(right_type)),
                    span: start_span,
                }
            }
            _ => unreachable!("guarded by matches!"),
        };

        Ok(Some(node))
    }

    pub(super) fn parse_type(&mut self) -> ParserResult<TypeNode> {
        if self.matches(&[TokenType::Ref]) {
            let start_span = self.previous().span;
            let referenced_type = self.parse_type()?;
            return Ok(TypeNode {
                kind: TypeKind::Reference(Box::new(referenced_type)),
                span: Span {
                    row_start: start_span.row_start,
                    col_start: start_span.col_start,
                    row_end: self.previous().span.row_end,
                    col_end: self.previous().span.col_end,
                },
            });
        }

        // We are essentially doing a consume here, but without borrowing the
        // parser again so we do not have to clone it.
        let token = &self.tokens[self.current];
        let start_span = token.span;
        self.current += 1;

        match token.token_type {
            TokenType::Id(ref name) => {
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
                        param_types.push(self.parse_type()?);

                        if !self.matches(&[TokenType::Comma]) {
                            break;
                        }
                        self.skip_newlines();
                    }
                }

                self.consume_token(TokenType::CloseParen, "Expected ')' after parameter types")?;
                self.consume_token(TokenType::Returns, "Expected 'returns' in function type")?;

                let return_type = Box::new(self.parse_type()?);

                Ok(TypeNode {
                    kind: TypeKind::Function {
                        params: param_types,
                        returns: return_type,
                    },
                    span: start_span,
                })
            }
            _ => Err(ParserError::from_token(
                DiagnosticCode::ParseExpectedType,
                "Expected type",
                token,
            )),
        }
    }

    pub(super) fn parse_type_arguments(&mut self) -> ParserResult<Vec<TypeNode>> {
        let mut args = Vec::new();
        while !self.check(TokenType::Gt) && !self.is_at_end() {
            let arg = self.parse_type()?;
            args.push(arg);

            if !self.matches(&[TokenType::Comma]) {
                break;
            }
            self.skip_newlines();
        }
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
}
