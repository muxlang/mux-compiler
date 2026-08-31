//! Token cursor operations shared by the parser productions.

use super::{Parser, ParserError, ParserResult};
use crate::diagnostic::DiagnosticCode;
use crate::lexer::{Span, Token, TokenType};

impl<'a> Parser<'a> {
    pub(super) fn advance(&mut self) -> &Token {
        if !self.is_at_end() {
            self.current += 1;
        }
        self.previous()
    }

    pub(super) fn previous(&self) -> &Token {
        self.tokens
            .get(self.current.saturating_sub(1))
            .copied()
            .unwrap_or_else(|| self.peek())
    }

    #[must_use]
    pub(super) fn is_at_end(&self) -> bool {
        self.current >= self.tokens.len()
    }

    pub(super) fn peek(&self) -> &Token {
        if self.is_at_end() {
            // Return the last token if available, otherwise use a default EOF token.
            // This prevents "line 0" errors.
            if let Some(last_token) = self.tokens.last() {
                last_token
            } else {
                static EOF_TOKEN: Token = Token {
                    token_type: TokenType::Eof,
                    span: Span {
                        row_start: 1,
                        row_end: None,
                        col_start: 1,
                        col_end: None,
                    },
                };
                &EOF_TOKEN
            }
        } else {
            self.tokens[self.current]
        }
    }

    pub(super) fn consume(&mut self) -> &Token {
        if let Some(token) = self.tokens.get(self.current).copied() {
            self.current += 1;
            token
        } else {
            self.peek()
        }
    }

    /// Look ahead `n` tokens without consuming.
    pub(super) fn peek_ahead(&self, n: usize) -> Option<&Token> {
        self.tokens.get(self.current + n).copied()
    }

    pub(super) fn matches(&mut self, types: &[TokenType]) -> bool {
        if self.is_at_end() {
            return false;
        }

        let token = self.peek();
        for ty in types {
            if token.token_type == *ty {
                if token.token_type == TokenType::Eof {
                    return false;
                }
                self.current += 1;
                return true;
            }
        }
        false
    }

    pub(super) fn check(&self, ty: TokenType) -> bool {
        if self.is_at_end() {
            return false;
        }
        self.peek().token_type == ty
    }

    pub(super) fn consume_token(
        &mut self,
        expected: TokenType,
        error_msg: &str,
    ) -> ParserResult<Span> {
        if self.is_at_end() {
            return Err(ParserError::new(
                DiagnosticCode::ParseExpectedToken,
                format!("{error_msg}, but reached end of file"),
                self.tokens.last().map_or_else(
                    || Span {
                        row_start: 1,
                        row_end: None,
                        col_start: 1,
                        col_end: None,
                    },
                    |t| t.span,
                ),
            ));
        }

        let token = &self.tokens[self.current];
        if token.token_type == expected {
            self.current += 1;
            Ok(token.span)
        } else {
            let found_desc = Self::describe_token(&token.token_type);
            Err(ParserError::new(
                DiagnosticCode::ParseExpectedToken,
                format!("{error_msg}, found {found_desc}"),
                token.span,
            ))
        }
    }

    pub(super) fn consume_identifier(&mut self, error_msg: &str) -> ParserResult<String> {
        if self.is_at_end() {
            return Err(ParserError::new(
                DiagnosticCode::ParseExpectedToken,
                format!("{error_msg}, but reached end of file"),
                self.peek().span,
            ));
        }

        match &self.peek().token_type {
            TokenType::Id(name) => {
                let name_clone = name.clone();
                self.current += 1;
                Ok(name_clone)
            }
            TokenType::Underscore => {
                let name_clone = "_".to_string();
                self.current += 1;
                Ok(name_clone)
            }
            _ => {
                let found_desc = Self::describe_token(&self.peek().token_type);
                Err(ParserError::new(
                    DiagnosticCode::ParseExpectedToken,
                    format!("{error_msg}, found {found_desc}"),
                    self.peek().span,
                ))
            }
        }
    }

    /// Format a human-readable description of a token type for use in error messages.
    pub(super) fn describe_token(token_type: &TokenType) -> String {
        match token_type {
            TokenType::Auto => "'auto' keyword".to_string(),
            TokenType::Func => "'func' keyword".to_string(),
            TokenType::Returns => "'returns' keyword".to_string(),
            TokenType::Return => "'return' keyword".to_string(),
            TokenType::Class => "'class' keyword".to_string(),
            TokenType::Interface => "'interface' keyword".to_string(),
            TokenType::Enum => "'enum' keyword".to_string(),
            TokenType::If => "'if' keyword".to_string(),
            TokenType::Else => "'else' keyword".to_string(),
            TokenType::For => "'for' keyword".to_string(),
            TokenType::While => "'while' keyword".to_string(),
            TokenType::Match => "'match' keyword".to_string(),
            TokenType::Const => "'const' keyword".to_string(),
            TokenType::Import => "'import' keyword".to_string(),
            TokenType::Break => "'break' keyword".to_string(),
            TokenType::Continue => "'continue' keyword".to_string(),
            TokenType::In => "'in' keyword".to_string(),
            TokenType::Is => "'is' keyword".to_string(),
            TokenType::As => "'as' keyword".to_string(),
            TokenType::Common => "'common' keyword".to_string(),
            TokenType::Where => "'where' keyword".to_string(),
            TokenType::None => "'none' keyword".to_string(),
            TokenType::OpenBrace => "'{'".to_string(),
            TokenType::CloseBrace => "'}'".to_string(),
            TokenType::OpenParen => "'('".to_string(),
            TokenType::CloseParen => "')'".to_string(),
            TokenType::OpenBracket => "'['".to_string(),
            TokenType::CloseBracket => "']'".to_string(),
            TokenType::Dot => "'.'".to_string(),
            TokenType::DotDot => "'..'".to_string(),
            TokenType::Comma => "','".to_string(),
            TokenType::Colon => "':'".to_string(),
            TokenType::Eq => "'='".to_string(),
            TokenType::Plus => "'+'".to_string(),
            TokenType::Minus => "'-'".to_string(),
            TokenType::Star => "'*'".to_string(),
            TokenType::Slash => "'/'".to_string(),
            TokenType::Percent => "'%'".to_string(),
            TokenType::Lt => "'<'".to_string(),
            TokenType::Gt => "'>'".to_string(),
            TokenType::EqEq => "'=='".to_string(),
            TokenType::NotEq => "'!='".to_string(),
            TokenType::Bang => "'!'".to_string(),
            TokenType::And => "'&&'".to_string(),
            TokenType::Or => "'||'".to_string(),
            TokenType::Id(name) => format!("identifier '{name}'"),
            TokenType::Int(n) => format!("integer literal '{n}'"),
            TokenType::Float(n) => format!("float literal '{n}'"),
            TokenType::Str(s) => format!("string literal \"{s}\""),
            TokenType::Bool(b) => format!("boolean literal '{b}'"),
            TokenType::Char(c) => format!("character literal '{c}'"),
            TokenType::Eof => "end of file".to_string(),
            TokenType::NewLine => "newline".to_string(),
            TokenType::StarStar => "'**'".to_string(),
            TokenType::Le => "'<='".to_string(),
            TokenType::Ge => "'>='".to_string(),
            TokenType::Incr => "'++'".to_string(),
            TokenType::Decr => "'--'".to_string(),
            TokenType::PlusEq => "'+='".to_string(),
            TokenType::MinusEq => "'-='".to_string(),
            TokenType::StarEq => "'*='".to_string(),
            TokenType::SlashEq => "'/='".to_string(),
            TokenType::PercentEq => "'%='".to_string(),
            TokenType::Ref => "'&'".to_string(),
            TokenType::Underscore => "'_'".to_string(),
            _ => format!("{token_type:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_consumes_identifiers_and_preserves_lookahead() {
        let tokens = [
            Token::new(TokenType::Id("value".to_string()), Span::new(1, 1)),
            Token::new(TokenType::NewLine, Span::new(1, 6)),
        ];
        let mut parser = Parser::new(&tokens);

        assert!(!parser.is_at_end());
        assert_eq!(
            parser.peek_ahead(1).map(|token| &token.token_type),
            Some(&TokenType::NewLine)
        );
        assert_eq!(
            parser.consume_identifier("expected name"),
            Ok("value".to_string())
        );
        assert!(parser.matches(&[TokenType::NewLine]));
        assert!(parser.is_at_end());
    }

    #[test]
    fn cursor_bounds_are_safe_at_start_and_end() {
        let tokens = [Token::new(TokenType::Eof, Span::new(1, 1))];
        let mut parser = Parser::new(&tokens);

        assert_eq!(parser.previous().token_type, TokenType::Eof);
        assert_eq!(parser.advance().token_type, TokenType::Eof);
        assert_eq!(parser.consume().token_type, TokenType::Eof);

        let mut empty = Parser::new(&[]);
        assert_eq!(empty.previous().token_type, TokenType::Eof);
        assert_eq!(empty.advance().token_type, TokenType::Eof);
        assert_eq!(empty.consume().token_type, TokenType::Eof);
    }
}
