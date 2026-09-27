//! Lexical analyzer for the Mux language.
//!
//! This module provides the lexer that converts source code into a stream of tokens.

mod error;
mod span;
mod token;

pub use error::LexerError;
pub use span::{ByteRange, Span};
pub use token::{Token, TokenType};

use crate::diagnostic::DiagnosticCode;
use crate::source::Source;
use ordered_float::OrderedFloat;

/// Output from lossless tokenization. The final token is always a zero-width EOF.
#[derive(Debug, Clone, PartialEq)]
pub struct LosslessLexResult {
    pub tokens: Vec<Token>,
    pub errors: Vec<LexerError>,
}

/// The lexer for the Mux language.
pub struct Lexer<'a> {
    source: &'a mut Source,
    pending_token_start: Option<usize>,
}

/// A character that may appear in an identifier after its first character.
///
/// The full identifier grammar is `[a-zA-Z][a-zA-Z0-9_]* | _[a-zA-Z0-9_]+`:
/// a leading `_` needs at least one of these after it, otherwise the `_` is
/// the wildcard token.
fn is_identifier_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

impl<'a> Lexer<'a> {
    pub fn new(source: &'a mut Source) -> Self {
        Lexer {
            source,
            pending_token_start: None,
        }
    }

    /// Tokenize every source byte, retaining horizontal whitespace, comments,
    /// newlines, and invalid text. Errors are collected up to the diagnostic
    /// limit while scanning continues through EOF.
    pub fn lex_all_lossless(&mut self) -> LosslessLexResult {
        self.scan_all()
    }

    #[cfg(test)]
    fn lex_tokens(&mut self) -> Result<Vec<Token>, LexerError> {
        let result = self.lex_all_lossless();
        if let Some(error) = result.errors.into_iter().next() {
            return Err(error);
        }
        Ok(result
            .tokens
            .into_iter()
            .filter(|token| !matches!(token.token_type, TokenType::Whitespace | TokenType::Eof))
            .collect())
    }

    fn scan_all(&mut self) -> LosslessLexResult {
        let mut tokens = Vec::new();
        let mut errors = Vec::new();
        loop {
            let before = self.source.pos;
            match self.next_token() {
                Ok(token) => {
                    let eof = matches!(token.token_type, TokenType::Eof);
                    tokens.push(token);
                    if eof {
                        break;
                    }
                }
                Err(error) => {
                    if errors.len() < crate::diagnostic::MAX_DIAGNOSTICS {
                        errors.push(error.clone());
                    }
                    if before == self.source.pos {
                        // Lexical errors that are diagnosed before consuming the
                        // offending byte must still make progress in lossless mode.
                        let start = self.source.pos;
                        let ch = self.source.next_char().expect("source is not at EOF");
                        let span = Span::new(start, self.source.pos);
                        tokens.push(Token::new(TokenType::Invalid(ch.to_string()), span));
                    } else {
                        let range = ByteRange::new(before, self.source.pos);
                        let text = self.source.slice(range).to_owned();
                        let span = Span::new(before, self.source.pos);
                        tokens.push(Token::new(TokenType::Invalid(text), span));
                    }
                }
            }
        }
        LosslessLexResult { tokens, errors }
    }

    /// Consume the next character after a successful peek.
    /// Only call this when `peek()` has already confirmed a character exists.
    fn consume_char(&mut self) -> char {
        self.source
            .next_char()
            .expect("peek confirmed a character exists")
    }

    fn span_at_byte(&self, start: usize, end: usize) -> Span {
        Span::new(start, end)
    }

    fn span_at_cursor(&self) -> Span {
        self.span_at_byte(self.source.pos, self.source.pos)
    }

    /// Consume remaining alphanumeric/underscore/dot/digit characters into `buf`
    /// for error reporting on invalid number literals.
    fn consume_remaining_invalid(&mut self, buf: &mut String, span: &mut Span) {
        while let Some(c) = self.source.peek() {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
                buf.push(self.consume_char());
            } else {
                break;
            }
        }
        span.extend_to(self.source.pos);
    }

    pub fn next_token(&mut self) -> Result<Token, LexerError> {
        self.pending_token_start = None;
        let result = self.next_token_inner();
        match result {
            Ok(mut token) => {
                if token
                    .span
                    .byte_range
                    .is_none_or(|range| range.start == range.end)
                    && !matches!(token.token_type, TokenType::Eof)
                {
                    let start = self.pending_token_start.unwrap_or(self.source.pos);
                    token.span = Span::new(start, self.source.pos);
                } else if token.span.byte_range.is_none() {
                    token.span = Span::empty(self.source.pos);
                }
                Ok(token)
            }
            Err(mut error) => {
                let token_start = self.pending_token_start.unwrap_or(self.source.pos);
                let needs_token_range = error
                    .span
                    .byte_range
                    .is_none_or(|range| range.start == token_start);
                if needs_token_range {
                    let start = token_start;
                    error.span = Span::new(start, self.source.pos.max(start));
                }
                Err(error)
            }
        }
    }

    fn next_token_inner(&mut self) -> Result<Token, LexerError> {
        if self
            .source
            .peek()
            .is_some_and(|ch| matches!(ch, ' ' | '\t' | '\r'))
        {
            let start = self.source.pos;
            while self
                .source
                .peek()
                .is_some_and(|ch| matches!(ch, ' ' | '\t' | '\r'))
            {
                self.source.next_char();
            }
            let span = Span::new(start, self.source.pos);
            return Ok(Token::new(TokenType::Whitespace, span));
        }

        self.pending_token_start = Some(self.source.pos);

        match self.source.peek() {
            None => {
                return Ok(Token::new(TokenType::Eof, Span::empty(self.source.pos)));
            }
            Some('\n') => {
                let start_span = Span::empty(self.source.pos);
                self.source.next_char(); // consume '\n'
                return Ok(Token::new(TokenType::NewLine, start_span));
            }
            _ => {}
        }

        let start_span = Span::empty(self.source.pos);
        let Some(c) = self.source.next_char() else {
            return Ok(Token::new(TokenType::Eof, start_span));
        };

        match c {
            '(' => Ok(Token::new(TokenType::OpenParen, start_span)),
            ')' => Ok(Token::new(TokenType::CloseParen, start_span)),
            '{' => Ok(Token::new(TokenType::OpenBrace, start_span)),
            '}' => Ok(Token::new(TokenType::CloseBrace, start_span)),
            '[' => Ok(Token::new(TokenType::OpenBracket, start_span)),
            ']' => Ok(Token::new(TokenType::CloseBracket, start_span)),
            ',' => Ok(Token::new(TokenType::Comma, start_span)),
            ':' => Ok(Token::new(TokenType::Colon, start_span)),
            '%' => {
                if self.source.peek() == Some('=') {
                    self.source.next_char();
                    Ok(Token::new(TokenType::PercentEq, start_span))
                } else {
                    Ok(Token::new(TokenType::Percent, start_span))
                }
            }
            // A bare '_' keeps its two jobs - the match wildcard and the
            // unused-parameter marker - so it stays its own token. Followed by
            // any identifier character it starts an ordinary identifier
            // ('_x', '_123', '__'), which is how every other language spells
            // "deliberately unused".
            '_' => match self.source.peek() {
                Some(ch) if is_identifier_char(ch) => {
                    Ok(self.read_identifier_or_keyword(c, start_span))
                }
                _ => Ok(Token::new(TokenType::Underscore, start_span)),
            },
            '.' => match self.source.peek() {
                Some(c) if c.is_ascii_digit() => self.read_number('.', start_span),
                Some('.') => {
                    self.source.next_char();
                    Ok(Token::new(TokenType::DotDot, start_span))
                }
                _ => Ok(Token::new(TokenType::Dot, start_span)),
            },
            _ => self.get_multichar_token(c, start_span),
        }
    }

    fn consume_identifier_tail(&mut self, ident: &mut String) {
        while let Some(ch) = self.source.peek() {
            if is_identifier_char(ch) {
                ident.push(ch);
                self.source.next_char();
            } else {
                break;
            }
        }
    }

    fn keyword_or_identifier_token_type(ident: String) -> TokenType {
        match ident.as_str() {
            "auto" => TokenType::Auto,
            "func" => TokenType::Func,
            "return" => TokenType::Return,
            "returns" => TokenType::Returns,
            "if" => TokenType::If,
            "else" => TokenType::Else,
            "for" => TokenType::For,
            "while" => TokenType::While,
            "match" => TokenType::Match,
            "use" => TokenType::Use,
            "const" => TokenType::Const,
            "class" => TokenType::Class,
            "interface" => TokenType::Interface,
            "enum" => TokenType::Enum,
            "import" => TokenType::Import,
            "is" => TokenType::Is,
            "as" => TokenType::As,
            "in" => TokenType::In,
            "break" => TokenType::Break,
            "continue" => TokenType::Continue,
            "none" => TokenType::None,
            "true" => TokenType::Bool(true),
            "false" => TokenType::Bool(false),
            "common" => TokenType::Common,
            "where" => TokenType::Where,
            "test" => TokenType::Test,
            _ => TokenType::Id(ident),
        }
    }

    fn read_identifier_or_keyword(&mut self, first_char: char, mut start_span: Span) -> Token {
        let mut ident = first_char.to_string();
        self.consume_identifier_tail(&mut ident);
        start_span.extend_to(self.source.pos);
        Token::new(Self::keyword_or_identifier_token_type(ident), start_span)
    }

    fn read_slash_token(&mut self, mut start_span: Span) -> Result<Token, LexerError> {
        if self.source.peek() == Some('/') {
            self.source.next_char();
            let mut comment = String::new();
            while let Some(ch) = self.source.peek() {
                if ch == '\n' || ch == '\r' {
                    break;
                }
                comment.push(self.consume_char());
            }
            start_span.extend_to(self.source.pos);
            return Ok(Token::new(TokenType::LineComment(comment), start_span));
        }

        if self.source.peek() == Some('*') {
            self.source.next_char();
            let mut comment = String::new();
            let mut found_terminator = false;
            let mut depth = 1_usize;
            while let Some(ch) = self.source.next_char() {
                if ch == '/' && self.source.peek() == Some('*') {
                    self.source.next_char();
                    comment.push_str("/*");
                    depth += 1;
                } else if ch == '*' && self.source.peek() == Some('/') {
                    self.source.next_char();
                    depth -= 1;
                    if depth == 0 {
                        found_terminator = true;
                        break;
                    }
                    comment.push_str("*/");
                } else {
                    comment.push(ch);
                }
            }
            if !found_terminator {
                return Err(LexerError::with_help(
                    DiagnosticCode::LexUnterminatedComment,
                    "Unterminated block comment",
                    start_span,
                    "Add a closing '*/' to end the block comment",
                ));
            }
            start_span.extend_to(self.source.pos);
            return Ok(Token::new(TokenType::MultilineComment(comment), start_span));
        }

        if self.source.peek() == Some('=') {
            self.source.next_char();
            start_span.extend_to(self.source.pos);
            return Ok(Token::new(TokenType::SlashEq, start_span));
        }

        Ok(Token::new(TokenType::Slash, start_span))
    }

    fn decode_escape(c: char) -> Option<char> {
        match c {
            'n' => Some('\n'),
            't' => Some('\t'),
            'r' => Some('\r'),
            '0' => Some('\0'),
            '\\' => Some('\\'),
            '\'' => Some('\''),
            '"' => Some('"'),
            _ => None,
        }
    }

    fn consume_digits_and_underscores(&mut self, out: &mut String) -> bool {
        let mut has_digit = false;
        while let Some(c) = self.source.peek() {
            if c.is_ascii_digit() {
                out.push(self.consume_char());
                has_digit = true;
            } else if c == '_' {
                self.source.next_char();
            } else {
                break;
            }
        }
        has_digit
    }

    fn consume_fraction_if_present(
        &mut self,
        num: &mut String,
        is_float: &mut bool,
    ) -> Result<(), LexerError> {
        if self.source.peek() != Some('.') {
            return Ok(());
        }

        let after_dot = self.source.peek_nth(1);
        if after_dot.is_some_and(|c| c.is_ascii_digit()) {
            *is_float = true;
        } else if after_dot.is_some_and(|c| c.is_ascii_alphabetic() || c == '_') {
            return Ok(());
        } else if after_dot == Some('.') {
            return Err(LexerError::with_help(
                DiagnosticCode::LexRangeLiteral,
                "Mux does not have range literal syntax",
                self.span_at_cursor(),
                "Use range(a, b) to iterate over a numeric range, e.g. for int i in range(0, 10)",
            ));
        } else {
            return Err(LexerError::new(
                DiagnosticCode::LexInvalidNumber,
                "Expected digit after decimal point",
                self.span_at_cursor(),
            ));
        }

        num.push(self.consume_char());
        if !self.consume_digits_and_underscores(num) {
            return Err(LexerError::new(
                DiagnosticCode::LexInvalidNumber,
                "Expected digit after decimal point",
                self.span_at_cursor(),
            ));
        }

        Ok(())
    }

    fn consume_exponent_if_present(
        &mut self,
        num: &mut String,
        is_float: &mut bool,
    ) -> Result<(), LexerError> {
        if !matches!(self.source.peek(), Some('e' | 'E')) {
            return Ok(());
        }

        *is_float = true;
        num.push(self.consume_char());

        if let Some('+' | '-') = self.source.peek() {
            num.push(self.consume_char());
        }

        if !self.consume_digits_and_underscores(num) {
            return Err(LexerError::with_help(
                DiagnosticCode::LexInvalidNumber,
                "Missing exponent in scientific notation",
                self.span_at_cursor(),
                "The 'e' notation requires digits after it, e.g. 1e10, 2.5e-3",
            ));
        }

        Ok(())
    }

    fn parse_float_token(
        &mut self,
        mut num: String,
        mut start_span: Span,
    ) -> Result<Token, LexerError> {
        if self.source.peek() == Some('.')
            && self.source.peek_nth(1).is_some_and(|c| c.is_ascii_digit())
        {
            self.consume_remaining_invalid(&mut num, &mut start_span);
            return Err(LexerError::with_help(
                DiagnosticCode::LexInvalidNumber,
                format!("Invalid float literal: {num}"),
                start_span,
                "A number can only have one decimal point. Use separate expressions for chained field access.",
            ));
        }

        if self
            .source
            .peek()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        {
            self.consume_remaining_invalid(&mut num, &mut start_span);
            return Err(LexerError::with_help(
                DiagnosticCode::LexInvalidNumber,
                format!("Invalid float literal: {num}"),
                start_span,
                "Float literals cannot contain letters. Use a space or separate expression.",
            ));
        }

        let clean_num: String = num.chars().filter(|c| *c != '_').collect();
        clean_num
            .parse::<f64>()
            .map(OrderedFloat)
            .map(|f| Token::new(TokenType::Float(f), start_span))
            .map_err(|_| {
                LexerError::new(
                    DiagnosticCode::LexInvalidNumber,
                    format!("Invalid float literal: {num}"),
                    start_span,
                )
            })
    }

    fn parse_int_token(
        &mut self,
        mut num: String,
        mut start_span: Span,
    ) -> Result<Token, LexerError> {
        if self
            .source
            .peek()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        {
            self.consume_remaining_invalid(&mut num, &mut start_span);
            return Err(LexerError::with_help(
                DiagnosticCode::LexInvalidNumber,
                format!("Invalid integer literal: {num}"),
                start_span,
                "Integer literals cannot contain letters. Variable names must not start with a digit.",
            ));
        }

        let clean_num: String = num.chars().filter(|c| *c != '_').collect();
        clean_num
            .parse::<i64>()
            .map(|i| Token::new(TokenType::Int(i), start_span))
            .map_err(|_| {
                LexerError::with_help(
                DiagnosticCode::LexInvalidNumber,
                    format!("Invalid integer literal: {num}"),
                    start_span,
                    "The integer value is out of range. Valid integers are between -9223372036854775808 and 9223372036854775807.",
                )
            })
    }

    fn get_multichar_token(
        &mut self,
        first_char: char,
        mut start_span: Span,
    ) -> Result<Token, LexerError> {
        start_span.extend_to(self.source.pos);

        match first_char {
            '*' => self.handle_star_operator(start_span),
            '=' => self.handle_eq_operator(start_span),
            '!' => self.handle_bang_operator(start_span),
            '+' => self.handle_plus_operator(start_span),
            '-' => self.handle_minus_operator(first_char, start_span),
            '<' => self.handle_lt_operator(start_span),
            '>' => self.handle_gt_operator(start_span),
            '|' => self.handle_or_operator(start_span),
            '&' => self.handle_and_operator(start_span),
            '/' => self.read_slash_token(start_span),
            '0'..='9' => self.read_number(first_char, start_span),
            '\'' => self.read_char(start_span),
            '"' => self.handle_string_start(start_span),
            // '_' never arrives here: `next_token` matches it first so it can
            // decide between the wildcard token and an identifier.
            'b' if self.source.peek() == Some('"') => self.read_bytes(start_span),
            'a'..='z' | 'A'..='Z' => Ok(self.read_identifier_or_keyword(first_char, start_span)),
            _ => Err(LexerError::with_help(
                DiagnosticCode::LexUnexpectedCharacter,
                format!("Unexpected character: '{first_char}'"),
                start_span,
                "This character is not recognized as valid Mux syntax. Check for accidental special characters or encoding issues.",
            )),
        }
    }

    fn handle_star_operator(&mut self, mut start_span: Span) -> Result<Token, LexerError> {
        if self.source.peek() == Some('*') {
            self.source.next_char();
            start_span.extend_to(self.source.pos);
            Ok(Token::new(TokenType::StarStar, start_span))
        } else if self.source.peek() == Some('=') {
            self.source.next_char();
            start_span.extend_to(self.source.pos);
            Ok(Token::new(TokenType::StarEq, start_span))
        } else {
            Ok(Token::new(TokenType::Star, start_span))
        }
    }

    fn handle_eq_operator(&mut self, mut start_span: Span) -> Result<Token, LexerError> {
        if self.source.peek() == Some('=') {
            self.source.next_char();
            start_span.extend_to(self.source.pos);
            Ok(Token::new(TokenType::EqEq, start_span))
        } else {
            Ok(Token::new(TokenType::Eq, start_span))
        }
    }

    fn handle_bang_operator(&mut self, mut start_span: Span) -> Result<Token, LexerError> {
        if self.source.peek() == Some('=') {
            self.source.next_char();
            start_span.extend_to(self.source.pos);
            Ok(Token::new(TokenType::NotEq, start_span))
        } else {
            Ok(Token::new(TokenType::Bang, start_span))
        }
    }

    fn handle_plus_operator(&mut self, mut start_span: Span) -> Result<Token, LexerError> {
        match self.source.peek() {
            Some('+') => {
                self.source.next_char();
                start_span.extend_to(self.source.pos);
                Ok(Token::new(TokenType::Incr, start_span))
            }
            Some('=') => {
                self.source.next_char();
                start_span.extend_to(self.source.pos);
                Ok(Token::new(TokenType::PlusEq, start_span))
            }
            _ => Ok(Token::new(TokenType::Plus, start_span)),
        }
    }

    fn handle_minus_operator(
        &mut self,
        first_char: char,
        mut start_span: Span,
    ) -> Result<Token, LexerError> {
        match self.source.peek() {
            Some('-') => {
                self.source.next_char();
                start_span.extend_to(self.source.pos);
                Ok(Token::new(TokenType::Decr, start_span))
            }
            Some('=') => {
                self.source.next_char();
                start_span.extend_to(self.source.pos);
                Ok(Token::new(TokenType::MinusEq, start_span))
            }
            Some(c) if c.is_ascii_digit() => self.read_number(first_char, start_span),
            _ => Ok(Token::new(TokenType::Minus, start_span)),
        }
    }

    fn handle_lt_operator(&mut self, mut start_span: Span) -> Result<Token, LexerError> {
        if self.source.peek() == Some('=') {
            self.source.next_char();
            start_span.extend_to(self.source.pos);
            Ok(Token::new(TokenType::Le, start_span))
        } else {
            Ok(Token::new(TokenType::Lt, start_span))
        }
    }

    fn handle_gt_operator(&mut self, mut start_span: Span) -> Result<Token, LexerError> {
        if self.source.peek() == Some('=') {
            self.source.next_char();
            start_span.extend_to(self.source.pos);
            Ok(Token::new(TokenType::Ge, start_span))
        } else {
            Ok(Token::new(TokenType::Gt, start_span))
        }
    }

    fn handle_or_operator(&mut self, start_span: Span) -> Result<Token, LexerError> {
        if self.source.peek() == Some('|') {
            self.source.next_char();
            let mut span = start_span;
            span.extend_to(self.source.pos);
            Ok(Token::new(TokenType::Or, span))
        } else {
            Err(LexerError::with_help(
                DiagnosticCode::LexUnexpectedCharacter,
                "Unexpected character '|'",
                start_span,
                "Single '|' is not a valid operator. Use '||' for logical OR",
            ))
        }
    }

    fn handle_and_operator(&mut self, mut start_span: Span) -> Result<Token, LexerError> {
        if self.source.peek() == Some('&') {
            self.source.next_char();
            start_span.extend_to(self.source.pos);
            Ok(Token::new(TokenType::And, start_span))
        } else {
            start_span.extend_to(self.source.pos);
            Ok(Token::new(TokenType::Ref, start_span))
        }
    }

    fn handle_string_start(&mut self, start_span: Span) -> Result<Token, LexerError> {
        self.read_string(start_span)
    }

    // Helper function to check for triple quotes
    fn is_triple_quote(&self) -> bool {
        let next1 = self.source.peek();
        let next2 = self.source.peek_nth(1);
        next1 == Some('"') && next2 == Some('"')
    }

    fn read_string(&mut self, start_span: Span) -> Result<Token, LexerError> {
        let mut s = String::new();
        let mut escaped = false;
        let is_triple = self.is_triple_quote();

        if is_triple {
            self.source.next_char(); // consume second quote
            self.source.next_char(); // consume third quote
        }

        while let Some(c) = self.source.next_char() {
            if c == '\\' && !escaped {
                escaped = true;
                continue;
            }

            if let Some(result) = self.try_end_string(c, escaped, is_triple, &s, start_span) {
                return result;
            }

            self.process_string_char(c, &mut escaped, &mut s)?;
        }

        self.handle_unterminated_string(&s, start_span)
    }

    fn read_bytes(&mut self, mut start_span: Span) -> Result<Token, LexerError> {
        if self.source.next_char() != Some('"') {
            return Err(LexerError::with_help(
                DiagnosticCode::LexUnexpectedCharacter,
                "Byte literal must start with a double quote",
                start_span,
                "Use b\"...\" for a byte literal",
            ));
        }
        let mut bytes = Vec::new();
        while let Some(c) = self.source.next_char() {
            if c == '"' {
                start_span.extend_to(self.source.pos);
                return Ok(Token::new(TokenType::Bytes(bytes), start_span));
            }
            if c == '\\' {
                let Some(escape) = self.source.next_char() else {
                    break;
                };
                if escape == 'x' {
                    let Some(high) = self.source.next_char() else {
                        break;
                    };
                    let Some(low) = self.source.next_char() else {
                        break;
                    };
                    let Some(high) = high.to_digit(16) else {
                        return Err(LexerError::with_help(
                            DiagnosticCode::LexUnknownEscape,
                            "Invalid hexadecimal byte escape",
                            start_span,
                            "Use two hexadecimal digits, for example \\xFF",
                        ));
                    };
                    let Some(low) = low.to_digit(16) else {
                        return Err(LexerError::with_help(
                            DiagnosticCode::LexUnknownEscape,
                            "Invalid hexadecimal byte escape",
                            start_span,
                            "Use two hexadecimal digits, for example \\xFF",
                        ));
                    };
                    bytes.push(((high << 4) | low) as u8);
                } else {
                    let Some(decoded) = Self::decode_escape(escape) else {
                        return Err(LexerError::with_help(
                            DiagnosticCode::LexUnknownEscape,
                            format!("Unknown byte escape: \\{escape}"),
                            start_span,
                            r#"Valid escapes include \\n, \\t, \\r, \\0, \\\\, \\" and \\xNN"#,
                        ));
                    };
                    if !decoded.is_ascii() {
                        return Err(LexerError::with_help(
                            DiagnosticCode::LexInvalidCharacterLiteral,
                            "Byte literals only contain ASCII characters",
                            start_span,
                            "Use \\xNN for an octet",
                        ));
                    }
                    bytes.push(decoded as u8);
                }
            } else if c.is_ascii() {
                bytes.push(c as u8);
            } else {
                return Err(LexerError::with_help(
                    DiagnosticCode::LexInvalidCharacterLiteral,
                    "Byte literals only contain ASCII characters",
                    start_span,
                    "Use \\xNN for an octet",
                ));
            }
        }
        Err(LexerError::with_help(
            DiagnosticCode::LexUnterminatedString,
            "Unterminated byte literal",
            start_span,
            "Make sure to close the byte literal with a matching quote",
        ))
    }

    fn try_end_string(
        &mut self,
        c: char,
        escaped: bool,
        is_triple: bool,
        s: &str,
        mut start_span: Span,
    ) -> Option<Result<Token, LexerError>> {
        if c != '"' || escaped {
            return None;
        }

        if is_triple {
            if self.source.peek() == Some('"') && self.source.peek_nth(1) == Some('"') {
                self.source.next_char();
                self.source.next_char();
                start_span.extend_to(self.source.pos);
                Some(Ok(Token::new(TokenType::Str(s.to_string()), start_span)))
            } else {
                None
            }
        } else {
            start_span.extend_to(self.source.pos);
            Some(Ok(Token::new(TokenType::Str(s.to_string()), start_span)))
        }
    }

    fn process_string_char(
        &mut self,
        c: char,
        escaped: &mut bool,
        s: &mut String,
    ) -> Result<(), LexerError> {
        if *escaped {
            if let Some(decoded) = Self::decode_escape(c) {
                s.push(decoded);
            } else {
                return Err(LexerError::with_help(
                    DiagnosticCode::LexUnknownEscape,
                    format!("Unknown escape sequence: \\{c}"),
                    self.span_at_byte(self.source.pos - c.len_utf8(), self.source.pos),
                    "Valid escape sequences: \\n, \\t, \\r, \\0, \\\\, \\', \\\"",
                ));
            }
            *escaped = false;
        } else {
            s.push(c);
        }
        Ok(())
    }

    fn handle_unterminated_string(
        &self,
        _string_content: &str,
        start_span: Span,
    ) -> Result<Token, LexerError> {
        Err(LexerError::with_help(
            DiagnosticCode::LexUnterminatedString,
            "Unterminated string",
            start_span,
            "Make sure to close the string with a matching quote",
        ))
    }

    fn read_char(&mut self, start_span: Span) -> Result<Token, LexerError> {
        let mut chars = Vec::new();
        let mut escaped = false;

        while let Some(c) = self.source.next_char() {
            if c == '\\' && !escaped {
                escaped = true;
                continue;
            }

            if c == '\'' && !escaped {
                return self.finish_char_literal(chars, start_span);
            }

            self.process_char_literal_char(c, &mut escaped, &mut chars)?;

            if chars.len() > 1 && !escaped {
                return Err(LexerError::with_help(
                    DiagnosticCode::LexInvalidCharacterLiteral,
                    "Char literal must be exactly one character",
                    self.char_literal_error_span(start_span),
                    "Example: 'a', '\\n', '\\''",
                ));
            }
        }

        Err(LexerError::with_help(
            DiagnosticCode::LexInvalidCharacterLiteral,
            "Unterminated character literal",
            start_span,
            "Make sure to close the character with a matching quote",
        ))
    }

    fn finish_char_literal(
        &mut self,
        chars: Vec<char>,
        mut start_span: Span,
    ) -> Result<Token, LexerError> {
        if chars.len() != 1 {
            return Err(LexerError::with_help(
                DiagnosticCode::LexInvalidCharacterLiteral,
                "Char literal must be exactly one character",
                self.char_literal_error_span(start_span),
                "Example: 'a', '\\n', '\\''",
            ));
        }
        start_span.extend_to(self.source.pos);
        Ok(Token::new(TokenType::Char(chars[0]), start_span))
    }

    fn char_literal_error_span(&self, start_span: Span) -> Span {
        let start = start_span
            .byte_range
            .expect("character literal has a source range")
            .start;
        self.span_at_byte(start, self.source.pos)
    }

    fn process_char_literal_char(
        &mut self,
        c: char,
        escaped: &mut bool,
        chars: &mut Vec<char>,
    ) -> Result<(), LexerError> {
        if *escaped {
            if let Some(decoded) = Self::decode_escape(c) {
                chars.push(decoded);
            } else {
                return Err(LexerError::with_help(
                    DiagnosticCode::LexUnknownEscape,
                    format!("Unknown escape sequence: \\{c}"),
                    self.span_at_byte(self.source.pos - c.len_utf8(), self.source.pos),
                    "Valid escape sequences: \\n, \\t, \\r, \\0, \\\\, \\'",
                ));
            }
            *escaped = false;
        } else {
            chars.push(c);
        }
        Ok(())
    }

    fn read_number(&mut self, first_char: char, mut start_span: Span) -> Result<Token, LexerError> {
        let mut num = String::new();
        let mut is_float = false;

        // Handle leading minus sign for negative numbers
        // Note: if first_char is '-', it was already added to num in the caller
        if first_char == '.' {
            is_float = true;
            num.push('0');
            num.push('.');
            if !self.consume_digits_and_underscores(&mut num) {
                return Err(LexerError::with_help(
                    DiagnosticCode::LexInvalidNumber,
                    "Expected digit after decimal point",
                    self.span_at_cursor(),
                    "A decimal point must be followed by at least one digit, e.g. 1.0",
                ));
            }
        } else {
            if first_char == '-' {
                num.push('-');
            } else {
                num.push(first_char);
            }

            self.consume_digits_and_underscores(&mut num);
            self.consume_fraction_if_present(&mut num, &mut is_float)?;
        }

        self.consume_exponent_if_present(&mut num, &mut is_float)?;

        start_span.extend_to(self.source.pos);

        if is_float {
            self.parse_float_token(num, start_span)
        } else {
            self.parse_int_token(num, start_span)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Source;

    fn assert_lexer_error(
        input: &str,
        result: Result<Vec<Token>, LexerError>,
        expected_msg: &str,
        expected_line: usize,
        expected_col: usize,
    ) {
        match result {
            Ok(tokens) => panic!("Expected error '{expected_msg}' but got tokens: {tokens:?}"),
            Err(e) => {
                assert!(
                    e.message.contains(expected_msg),
                    "Expected error message to contain '{}' but got '{}'",
                    expected_msg,
                    e.message
                );
                let range = e.span.byte_range.expect("lexer errors have byte ranges");
                let source = Source::from_string(input.to_owned());
                assert_eq!(source.line_col(range.start), (expected_line, expected_col));
            }
        }
    }

    fn assert_span_start(source: &Source, span: Span, expected: (usize, usize)) {
        let range = span.byte_range.expect("lexer tokens have byte ranges");
        assert_eq!(source.line_col(range.start), expected);
    }

    #[test]
    fn test_position_tracking_across_lines() {
        // Test a more complex example across multiple lines
        let input = "auto x = 42\nfunc check() {\n  return x\n}";
        let mut source = Source::from_test_str(input);
        let mut lexer = Lexer::new(&mut source);
        let tokens = lexer.lex_tokens().unwrap();
        let mut tokens = tokens.iter().map(|token| &token.token_type);

        assert_eq!(tokens.next(), Some(&TokenType::Auto));
        assert_eq!(tokens.next(), Some(&TokenType::Id("x".to_string())));
        assert_eq!(tokens.next(), Some(&TokenType::Eq));
        match tokens.next() {
            Some(TokenType::Int(42)) => {}
            other => panic!("Expected Int(42), got {other:?}"),
        }
        assert_eq!(tokens.next(), Some(&TokenType::NewLine));

        assert_eq!(tokens.next(), Some(&TokenType::Func));
        assert_eq!(tokens.next(), Some(&TokenType::Id("check".to_string())));
        assert_eq!(tokens.next(), Some(&TokenType::OpenParen));
        assert_eq!(tokens.next(), Some(&TokenType::CloseParen));
        assert_eq!(tokens.next(), Some(&TokenType::OpenBrace));
        assert_eq!(tokens.next(), Some(&TokenType::NewLine));

        assert_eq!(tokens.next(), Some(&TokenType::Return));
        assert_eq!(tokens.next(), Some(&TokenType::Id("x".to_string())));
        assert_eq!(tokens.next(), Some(&TokenType::NewLine));

        assert_eq!(tokens.next(), Some(&TokenType::CloseBrace));
        assert_eq!(tokens.next(), None);
    }

    #[test]
    fn continuation_newlines_are_consumed_iteratively() {
        let input = format!("(\n{}1)", "\n".repeat(100_000));
        let mut source = Source::from_test_str(&input);
        let result = Lexer::new(&mut source).lex_all_lossless();
        assert!(result.errors.is_empty());
        assert_eq!(result.tokens[0].token_type, TokenType::OpenParen);
        assert_eq!(result.tokens[1].token_type, TokenType::NewLine);
        assert_eq!(
            result
                .tokens
                .iter()
                .filter(|token| token.token_type == TokenType::NewLine)
                .count(),
            100_001
        );
        assert_eq!(
            result
                .tokens
                .iter()
                .filter(|token| token.token_type == TokenType::Int(1))
                .count(),
            1
        );
        assert_eq!(result.tokens.last().unwrap().token_type, TokenType::Eof);
    }

    #[test]
    fn lossless_tokens_cover_unicode_crlf_comments_and_whitespace() {
        let input = "auto  x = \"café\"\r\n// keep  this\r\n/* keep  this */";
        let mut source = Source::from_test_str(input);
        let result = Lexer::new(&mut source).lex_all_lossless();

        assert!(result.errors.is_empty());
        let eof = result.tokens.last().expect("EOF is always emitted");
        assert_eq!(eof.token_type, TokenType::Eof);
        assert_eq!(eof.span.byte_range, Some(ByteRange::empty(input.len())));

        let mut reconstructed = String::new();
        let mut previous_end = 0;
        for token in &result.tokens[..result.tokens.len() - 1] {
            let range = token.span.byte_range.expect("lexer token has byte range");
            assert_eq!(range.start, previous_end);
            assert!(input.is_char_boundary(range.start));
            assert!(input.is_char_boundary(range.end));
            reconstructed.push_str(&input[range.start..range.end]);
            previous_end = range.end;
        }
        assert_eq!(previous_end, input.len());
        assert_eq!(reconstructed, input);
        assert!(result.tokens.iter().any(|token| {
            matches!(&token.token_type, TokenType::LineComment(_))
                && source.slice(token.span.byte_range.unwrap()) == "// keep  this"
        }));
        assert!(result.tokens.iter().any(|token| {
            matches!(&token.token_type, TokenType::LineComment(comment) if comment == " keep  this")
        }));
        assert!(result.tokens.iter().any(|token| {
            token.token_type == TokenType::Whitespace
                && source.slice(token.span.byte_range.unwrap()) == "\r"
        }));
        assert!(result.tokens.iter().any(|token| {
            matches!(&token.token_type, TokenType::MultilineComment(comment) if comment == " keep  this ")
        }));
    }

    #[test]
    fn lossless_lexing_retains_invalid_text_and_continues_to_eof() {
        let input = "auto x = @\n\"unterminated";
        let mut source = Source::from_test_str(input);
        let result = Lexer::new(&mut source).lex_all_lossless();

        assert_eq!(result.errors.len(), 2);
        assert!(
            result.tokens.iter().any(|token| {
                matches!(&token.token_type, TokenType::Invalid(raw) if raw == "@")
            })
        );
        assert_eq!(result.tokens.last().unwrap().token_type, TokenType::Eof);

        let reconstructed = result.tokens[..result.tokens.len() - 1]
            .iter()
            .map(|token| source.slice(token.span.byte_range.unwrap()))
            .collect::<String>();
        assert_eq!(reconstructed, input);
    }

    #[test]
    fn lossless_empty_file_has_one_zero_width_eof() {
        let mut source = Source::from_test_str("");
        let result = Lexer::new(&mut source).lex_all_lossless();

        assert!(result.errors.is_empty());
        assert_eq!(result.tokens.len(), 1);
        assert_eq!(result.tokens[0].token_type, TokenType::Eof);
        assert_eq!(result.tokens[0].span.byte_range, Some(ByteRange::empty(0)));
    }

    #[test]
    fn lexical_error_ranges_survive_combining_characters() {
        let input = "\"a\u{301}\\z\"";
        let mut source = Source::from_test_str(input);
        let result = Lexer::new(&mut source).lex_all_lossless();

        let error_range = result.errors[0]
            .span
            .byte_range
            .expect("lexer errors have byte ranges");
        assert_eq!(source.slice(error_range), "z");
        assert_eq!(error_range.start, input.find('z').unwrap());
    }

    #[test]
    fn lossless_lexing_caps_errors_without_stopping_token_coverage() {
        let input = "@".repeat(crate::diagnostic::MAX_DIAGNOSTICS + 7);
        let mut source = Source::from_test_str(&input);
        let result = Lexer::new(&mut source).lex_all_lossless();

        assert_eq!(result.errors.len(), crate::diagnostic::MAX_DIAGNOSTICS);
        assert_eq!(result.tokens.len(), input.len() + 1);
        let reconstructed = result.tokens[..result.tokens.len() - 1]
            .iter()
            .map(|token| source.slice(token.span.byte_range.unwrap()))
            .collect::<String>();
        assert_eq!(reconstructed, input);
    }

    #[test]
    fn test_char_errors() {
        // Test string literals instead of character literals since Mux might not support single-quoted chars

        // Empty string literal
        let input = "auto x = \"\"";
        let mut source = Source::from_test_str(input);
        let result = Lexer::new(&mut source).lex_tokens();
        assert!(result.is_ok(), "Empty string literals should be valid");

        // Unterminated string
        let input = "auto x = \"unterminated";
        let mut source = Source::from_test_str(input);
        let result = Lexer::new(&mut source).lex_tokens();
        assert_lexer_error(input, result, "Unterminated string", 1, 10);

        // Invalid escape sequence in string
        let input = "auto x = \"invalid \\z escape\"";
        let mut source = Source::from_test_str(input);
        let result = Lexer::new(&mut source).lex_tokens();
        assert_lexer_error(input, result, "Unknown escape sequence", 1, 20);
    }

    #[test]
    fn test_string_errors() {
        let input = r#"auto x = "unterminated
auto y = 42"#;
        let mut source = Source::from_test_str(input);
        let result = Lexer::new(&mut source).lex_tokens();
        assert_lexer_error(input, result, "Unterminated string", 1, 10);

        let input = r#"auto x = "invalid \z escape""#;
        let mut source = Source::from_test_str(input);
        let result = Lexer::new(&mut source).lex_tokens();

        match result {
            Ok(tokens) => {
                panic!("Expected error for invalid escape sequence but got tokens: {tokens:?}");
            }
            Err(e) => {
                assert!(
                    e.message.contains("Unknown escape sequence: \\z"),
                    "Expected 'Unknown escape sequence: \\z' error, got: {}",
                    e.message
                );
                let range = e.span.byte_range.expect("lexer errors have byte ranges");
                assert_eq!(source.line_col(range.start), (1, 20));
            }
        }
    }

    #[test]
    fn test_number_errors() {
        // The lexer should fail on multiple decimal points
        let input = "auto x = 1.2.3";
        let mut source = Source::from_test_str(input);
        let result = Lexer::new(&mut source).lex_tokens();
        assert!(
            result.is_err(),
            "Expected error for invalid float literal with multiple decimals"
        );

        // The lexer should fail on invalid scientific notation
        let input = "auto x = 1.23e";
        let mut source = Source::from_test_str(input);
        let result = Lexer::new(&mut source).lex_tokens();
        assert!(
            result.is_err(),
            "Expected error for invalid scientific notation"
        );

        // The lexer should fail on "1e+" as it's invalid scientific notation
        let input = "auto x = 1e+";
        let mut source = Source::from_test_str(input);
        let result = Lexer::new(&mut source).lex_tokens();
        assert!(
            result.is_err(),
            "Expected error for invalid scientific notation"
        );

        // The lexer should fail on trailing decimal points
        let input = "auto x = 1.";
        let mut source = Source::from_test_str(input);
        let result = Lexer::new(&mut source).lex_tokens();
        assert!(result.is_err(), "Expected error for trailing decimal point");

        // The lexer should fail on integer literals with identifier suffixes
        let input = "auto x = 123abc";
        let mut source = Source::from_test_str(input);
        let result = Lexer::new(&mut source).lex_tokens();
        assert!(
            result.is_err(),
            "Expected error for invalid integer literal"
        );
    }

    #[test]
    fn test_range_literal_diagnostic() {
        // Mux has no range literal syntax; `0..10` should produce a targeted
        // diagnostic pointing users at range(a, b) instead of the generic
        // "Expected digit after decimal point" message.
        let input = "for int i in 0..10 {}";
        let mut source = Source::from_test_str(input);
        let result = Lexer::new(&mut source).lex_tokens();
        assert_lexer_error(
            input,
            result,
            "Mux does not have range literal syntax",
            1,
            15,
        );
    }

    #[test]
    fn test_multiple_errors() {
        let input = r#"
            auto x = "unterminated
            auto y = 1.2.3
            auto z = 123invalid
        "#;

        let mut source = Source::from_test_str(input);
        let result = Lexer::new(&mut source).lex_tokens();

        // The lexer will stop at the first error, so we should only get one error
        match result {
            Ok(tokens) => panic!("Expected error but got tokens: {tokens:?}"),
            Err(e) => {
                // The first error should be about the unterminated string
                assert!(
                    e.message.contains("Unterminated string"),
                    "Unexpected error: {}",
                    e.message
                );
                let range = e.span.byte_range.expect("lexer errors have byte ranges");
                assert_eq!(source.line_col(range.start), (2, 22));
            }
        }
    }

    #[test]
    fn test_error_spans() {
        // Test that error spans are reported correctly
        let input = r#"
            auto x = "unterminated
            auto y = 1.2.3
            auto z = 123invalid
        "#;

        // First error (unterminated string)
        let mut source = Source::from_test_str(input);
        let result = Lexer::new(&mut source).lex_tokens();
        match result {
            Ok(tokens) => panic!("Expected error but got tokens: {tokens:?}"),
            Err(e) => {
                assert!(
                    e.message.contains("Unterminated string"),
                    "Expected 'Unterminated string' error, got: {}",
                    e.message
                );
                let range = e.span.byte_range.expect("lexer errors have byte ranges");
                assert_eq!(source.line_col(range.start), (2, 22));
            }
        }

        // Test with a valid variable declaration
        let fixed_input = r"
            auto x = 42
            auto y = 1.2
        ";
        let mut source = Source::from_test_str(fixed_input);
        let result = Lexer::new(&mut source).lex_tokens();
        match result {
            Ok(tokens) => {
                // The lexer should successfully tokenize this input
                assert!(!tokens.is_empty());
                // Check that we have the expected number of tokens
                // auto, x, =, 42, newline, auto, y, =, 1.2
                assert!(
                    tokens.len() >= 8,
                    "Expected at least 8 tokens, got {}",
                    tokens.len()
                );
            }
            Err(e) => {
                panic!("Expected successful tokenization but got error: {e}");
            }
        }
    }

    #[test]
    fn test_number_parsing() {
        // Method calls on literals should be unambiguous: `1.to_string()` is int + dot + ident.
        let mut source = Source::from_test_str("auto x = 1.to_string()");
        let tokens = Lexer::new(&mut source).lex_tokens().unwrap();
        let token_types: Vec<_> = tokens.into_iter().map(|t| t.token_type).collect();
        assert_eq!(
            token_types,
            vec![
                TokenType::Auto,
                TokenType::Id("x".to_string()),
                TokenType::Eq,
                TokenType::Int(1),
                TokenType::Dot,
                TokenType::Id("to_string".to_string()),
                TokenType::OpenParen,
                TokenType::CloseParen,
            ]
        );

        // Leading-dot floats should be accepted (and normalized to 0.x)
        let mut source = Source::from_test_str("auto y = .5");
        let tokens = Lexer::new(&mut source).lex_tokens().unwrap();
        match tokens.last().map(|t| &t.token_type) {
            Some(TokenType::Float(f)) => assert!((f.into_inner() - 0.5).abs() < f64::EPSILON),
            other => panic!("Expected Float(0.5), got {other:?}"),
        }
    }

    #[test]
    fn test_numbers() {
        let input = "42 1_000 3.45 0.5 5.0";
        let mut source = Source::from_test_str(input);
        let mut lexer = Lexer::new(&mut source);
        let tokens = lexer.lex_tokens().unwrap();
        let token_types: Vec<_> = tokens.into_iter().map(|t| t.token_type).collect();

        match &token_types[..] {
            [
                TokenType::Int(42),
                TokenType::Int(1000),
                TokenType::Float(OrderedFloat(f1)),
                TokenType::Float(OrderedFloat(f2)),
                TokenType::Float(OrderedFloat(f3)),
            ] if (*f1 - 3.45).abs() < f64::EPSILON
                && (*f2 - 0.5).abs() < f64::EPSILON
                && (*f3 - 5.0).abs() < f64::EPSILON => {}
            _ => panic!("Unexpected token types: {token_types:?}"),
        }
    }

    #[test]
    fn test_simple_strings() {
        let input = r#""hello" "world""#;
        let mut source = Source::from_test_str(input);
        let mut lexer = Lexer::new(&mut source);
        let tokens = lexer.lex_tokens().unwrap();

        assert_eq!(tokens.len(), 2);
        match &tokens[0].token_type {
            TokenType::Str(s) => assert_eq!(s, "hello"),
            _ => panic!("Expected Str token, got {:?}", tokens[0]),
        }
        match &tokens[1].token_type {
            TokenType::Str(s) => assert_eq!(s, "world"),
            _ => panic!("Expected Str token, got {:?}", tokens[1]),
        }
    }

    #[test]
    fn test_multiline_string() {
        let input = r#"
"hello
world"
"#;
        let mut source = Source::from_test_str(input);
        let mut lexer = Lexer::new(&mut source);
        let tokens = lexer.lex_tokens().unwrap();

        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[0].token_type, TokenType::NewLine);
        match &tokens[1].token_type {
            TokenType::Str(s) => assert_eq!(s, "hello\nworld"),
            _ => panic!("Expected Str token, got {:?}", tokens[1]),
        }
        assert_eq!(tokens[2].token_type, TokenType::NewLine);
    }

    #[test]
    fn test_char_literals() {
        let input = r"'a''\n''\''"; // No spaces between characters
        let mut source = Source::from_test_str(input);
        let mut lexer = Lexer::new(&mut source);
        let tokens = lexer.lex_tokens().unwrap();

        // We expect 3 character literals with no whitespace tokens
        assert_eq!(tokens.len(), 3);

        // First character literal
        match tokens[0].token_type {
            TokenType::Char('a') => {}
            _ => panic!("Expected Char('a'), got {:?}", tokens[0]),
        }

        // Second character literal
        match tokens[1].token_type {
            TokenType::Char('\n') => {}
            _ => panic!("Expected Char('\\n'), got {:?}", tokens[1]),
        }

        // Third character literal
        match tokens[2].token_type {
            TokenType::Char('\'') => {}
            _ => panic!("Expected Char('\\''), got {:?}", tokens[2]),
        }
    }

    #[test]
    fn test_strings_with_escapes() {
        // basic escapes
        let input = r#""a\n\t\\\"\'\r\0""#;
        let mut source = Source::from_test_str(input);
        let mut lexer = Lexer::new(&mut source);
        let tokens = lexer.lex_tokens().unwrap();

        assert_eq!(tokens.len(), 1);
        match &tokens[0].token_type {
            TokenType::Str(s) => assert_eq!(s, "a\n\t\\\"'\r\0"),
            _ => panic!("Expected Str token, got {:?}", tokens[0]),
        }

        // test that uppercase forms are treated as identifiers
        let input = "Some None Ok Err";
        let mut source = Source::from_test_str(input);
        let mut lexer = Lexer::new(&mut source);
        let tokens = lexer.lex_tokens().unwrap();

        assert_eq!(
            tokens.into_iter().map(|t| t.token_type).collect::<Vec<_>>(),
            vec![
                TokenType::Id("Some".to_string()),
                TokenType::Id("None".to_string()),
                TokenType::Id("Ok".to_string()),
                TokenType::Id("Err".to_string()),
            ]
        );

        // test that true/false are treated as keywords
        let input = "true false";
        let mut source = Source::from_test_str(input);
        let mut lexer = Lexer::new(&mut source);
        let tokens = lexer.lex_tokens().unwrap();

        assert_eq!(
            tokens.into_iter().map(|t| t.token_type).collect::<Vec<_>>(),
            vec![TokenType::Bool(true), TokenType::Bool(false)]
        );

        // test that keywords are case-sensitive
        let input = "some none ok err";
        let mut source = Source::from_test_str(input);
        let mut lexer = Lexer::new(&mut source);
        let tokens = lexer.lex_tokens().unwrap();

        assert_eq!(
            tokens.into_iter().map(|t| t.token_type).collect::<Vec<_>>(),
            vec![
                TokenType::Id("some".to_string()),
                TokenType::None,
                TokenType::Id("ok".to_string()),
                TokenType::Id("err".to_string()),
            ]
        );

        // unterminated string
        let input = "\"unterminated";
        let mut source = Source::from_test_str(input);
        let mut lexer = Lexer::new(&mut source);
        assert!(lexer.lex_tokens().is_err());

        // unknown escape sequences
        for input in [r#""\a""#, r#""\c""#] {
            let mut source = Source::from_test_str(input);
            let mut lexer = Lexer::new(&mut source);
            assert!(lexer.lex_tokens().is_err());
        }
    }

    fn lex_types(input: &str) -> Vec<TokenType> {
        let mut source = Source::from_test_str(input);
        let mut lexer = Lexer::new(&mut source);
        lexer
            .lex_tokens()
            .unwrap()
            .into_iter()
            .map(|t| t.token_type)
            .collect()
    }

    #[test]
    fn test_leading_underscore_starts_an_identifier() {
        for name in ["_x", "_123", "__", "_x_1", "_X"] {
            match &lex_types(name)[..] {
                [TokenType::Id(id)] => assert_eq!(id, name),
                other => panic!("'{name}' should lex as one identifier, got {other:?}"),
            }
        }
    }

    #[test]
    fn test_bare_underscore_stays_the_wildcard() {
        // Nothing may follow it that could extend an identifier, so each of
        // these keeps '_' as its own token.
        assert_eq!(lex_types("_"), vec![TokenType::Underscore]);
        assert_eq!(
            lex_types("_ _"),
            vec![TokenType::Underscore, TokenType::Underscore]
        );
        assert_eq!(
            lex_types("(_)"),
            vec![
                TokenType::OpenParen,
                TokenType::Underscore,
                TokenType::CloseParen
            ]
        );
        assert_eq!(
            lex_types("_,"),
            vec![TokenType::Underscore, TokenType::Comma]
        );
    }

    #[test]
    fn test_keywords_and_identifiers() {
        let input = "auto x = 42 if else for while match const class interface enum is as in range list map optional result some none ok err true false common";
        let mut source = Source::from_test_str(input);
        let mut lexer = Lexer::new(&mut source);
        let tokens: Vec<_> = lexer.lex_tokens().unwrap().into_iter().collect();

        let token_types: Vec<_> = tokens.into_iter().map(|t| t.token_type).collect();

        match &token_types[..] {
            [
                TokenType::Auto,
                TokenType::Id(x),
                TokenType::Eq,
                TokenType::Int(42),
                TokenType::If,
                TokenType::Else,
                TokenType::For,
                TokenType::While,
                TokenType::Match,
                TokenType::Const,
                TokenType::Class,
                TokenType::Interface,
                TokenType::Enum,
                TokenType::Is,
                TokenType::As,
                TokenType::In,
                TokenType::Id(range),
                TokenType::Id(list),
                TokenType::Id(map),
                TokenType::Id(opt),
                TokenType::Id(res),
                TokenType::Id(some),
                TokenType::None,
                TokenType::Id(ok),
                TokenType::Id(err),
                TokenType::Bool(true),
                TokenType::Bool(false),
                TokenType::Common,
            ] => {
                assert_eq!(x, "x");
                assert_eq!(range, "range");
                assert_eq!(list, "list");
                assert_eq!(map, "map");
                assert_eq!(opt, "optional");
                assert_eq!(res, "result");
                assert_eq!(some, "some");
                // none is TokenType::None, not Id
                assert_eq!(ok, "ok");
                assert_eq!(err, "err");
            }
            _ => panic!("Unexpected token sequence: {token_types:?}"),
        }
    }

    #[test]
    fn test_operators() {
        // Helper function to get non-whitespace tokens
        fn get_tokens(input: &str) -> Vec<TokenType> {
            let mut source = Source::from_test_str(input);
            let mut lexer = Lexer::new(&mut source);
            lexer
                .lex_tokens()
                .unwrap()
                .into_iter()
                .map(|t| t.token_type)
                .collect()
        }

        // Comparison operators
        let token_types = get_tokens("= == ! != < <= > >=");
        assert_eq!(
            token_types,
            vec![
                TokenType::Eq,
                TokenType::EqEq,
                TokenType::Bang,
                TokenType::NotEq,
                TokenType::Lt,
                TokenType::Le,
                TokenType::Gt,
                TokenType::Ge,
            ]
        );

        // Arithmetic operators
        let token_types = get_tokens("+ - * / %");
        assert_eq!(
            token_types,
            vec![
                TokenType::Plus,
                TokenType::Minus,
                TokenType::Star,
                TokenType::Slash,
                TokenType::Percent,
            ]
        );

        // Logical operators
        let token_types = get_tokens("&& ||");
        assert_eq!(token_types, vec![TokenType::And, TokenType::Or,]);

        // Assignment operators
        let token_types = get_tokens("= += -= *= /=");
        assert_eq!(
            token_types,
            vec![
                TokenType::Eq,
                TokenType::PlusEq,
                TokenType::MinusEq,
                TokenType::StarEq,
                TokenType::SlashEq,
            ]
        );

        // Test combined operators with identifiers and numbers
        let token_types = get_tokens("a += 1 b -= 2 c *= 3 d /= 4");
        match &token_types[..] {
            [
                TokenType::Id(a),
                TokenType::PlusEq,
                TokenType::Int(1),
                TokenType::Id(b),
                TokenType::MinusEq,
                TokenType::Int(2),
                TokenType::Id(c),
                TokenType::StarEq,
                TokenType::Int(3),
                TokenType::Id(d),
                TokenType::SlashEq,
                TokenType::Int(4),
            ] => {
                assert_eq!(a, "a");
                assert_eq!(b, "b");
                assert_eq!(c, "c");
                assert_eq!(d, "d");
            }
            _ => panic!("Unexpected token sequence: {token_types:?}"),
        }
    }

    #[test]
    fn test_comments() {
        let input = "// line comment\n/* multi\nline */";
        let mut source = Source::from_test_str(input);
        let mut lexer = Lexer::new(&mut source);
        let tokens = lexer.lex_tokens().unwrap();

        // We should have 3 tokens: line comment, newline, and multiline comment
        assert_eq!(tokens.len(), 3);

        // The comment payload retains exactly the text after the marker.
        match &tokens[0].token_type {
            TokenType::LineComment(s) => assert_eq!(s, " line comment"),
            _ => panic!("Expected LineComment, got {:?}", tokens[0]),
        }

        // Check newline
        assert_eq!(tokens[1].token_type, TokenType::NewLine);

        // Check multiline comment
        match &tokens[2].token_type {
            TokenType::MultilineComment(s) => assert_eq!(s, " multi\nline "),
            _ => panic!("Expected MultilineComment, got {:?}", tokens[2]),
        }
    }

    #[test]
    fn nested_block_comments_end_at_the_outer_delimiter() {
        let input = "/* outer /* inner */ trailing */ auto value = 1";
        let mut source = Source::from_test_str(input);
        let tokens = Lexer::new(&mut source).lex_tokens().unwrap();

        assert!(matches!(
            &tokens[0].token_type,
            TokenType::MultilineComment(comment) if comment == " outer /* inner */ trailing "
        ));
        assert_eq!(tokens[1].token_type, TokenType::Auto);
    }

    #[test]
    fn test_multiple_line_comments() {
        let input = "// first comment\n// second comment\n// third comment";
        let mut source = Source::from_test_str(input);
        let mut lexer = Lexer::new(&mut source);
        let tokens = lexer.lex_tokens().unwrap();

        // We should have 5 tokens: 3 comments and 2 newlines
        assert_eq!(tokens.len(), 5);

        // Check first comment
        match &tokens[0].token_type {
            TokenType::LineComment(s) => assert_eq!(s, " first comment"),
            _ => panic!("Expected first LineComment, got {:?}", tokens[0]),
        }

        // First newline
        assert_eq!(tokens[1].token_type, TokenType::NewLine);

        // Second comment
        match &tokens[2].token_type {
            TokenType::LineComment(s) => assert_eq!(s, " second comment"),
            _ => panic!("Expected second LineComment, got {:?}", tokens[2]),
        }

        // Second newline
        assert_eq!(tokens[3].token_type, TokenType::NewLine);

        // Third comment
        match &tokens[4].token_type {
            TokenType::LineComment(s) => assert_eq!(s, " third comment"),
            _ => panic!("Expected third LineComment, got {:?}", tokens[4]),
        }
    }

    #[test]
    fn test_dots_and_newlines() {
        let input = "1.2\n3.4\n5.6";
        let mut source = Source::from_test_str(input);
        let mut lexer = Lexer::new(&mut source);
        let tokens = lexer.lex_tokens().unwrap();

        // We expect 3 floats and 2 newlines
        assert_eq!(tokens.len(), 5);

        // First float
        match &tokens[0].token_type {
            TokenType::Float(f) => assert!((f.into_inner() - 1.2).abs() < f64::EPSILON),
            _ => panic!("Expected Float(1.2), got {:?}", tokens[0]),
        }
        assert_span_start(&source, tokens[0].span, (1, 1));

        // First newline
        assert_eq!(tokens[1].token_type, TokenType::NewLine);
        assert_span_start(&source, tokens[1].span, (1, 4));

        // Second float
        match &tokens[2].token_type {
            TokenType::Float(f) => assert!((f.into_inner() - 3.4).abs() < f64::EPSILON),
            _ => panic!("Expected Float(3.4), got {:?}", tokens[2]),
        }
        assert_span_start(&source, tokens[2].span, (2, 1));

        // Second newline
        assert_eq!(tokens[3].token_type, TokenType::NewLine);
        assert_span_start(&source, tokens[3].span, (2, 4));

        // Third float
        match &tokens[4].token_type {
            TokenType::Float(f) => assert!((f.into_inner() - 5.6).abs() < f64::EPSILON),
            _ => panic!("Expected Float(5.6), got {:?}", tokens[4]),
        }
        assert_span_start(&source, tokens[4].span, (3, 1));
    }

    #[test]
    fn eof_token_preserves_empty_source_position() {
        let mut source = Source::from_test_str("");
        let mut lexer = Lexer::new(&mut source);
        let eof = lexer.next_token().expect("empty input must lex to EOF");

        assert_eq!(eof.token_type, TokenType::Eof);
        assert_span_start(&source, eof.span, (1, 1));
        assert_eq!(eof.span.byte_range, Some(ByteRange::empty(0)));
    }

    #[test]
    fn test_span_calculation() {
        // Test spans for various token types
        let input = r#"
        auto x = 42
        auto y = "hello"
        func add(a: int, b: int) -> int { a + b }
        "#;

        let mut source = Source::from_test_str(input);
        let mut lexer = Lexer::new(&mut source);
        let tokens = lexer.lex_tokens().unwrap();

        // Find the 'auto' token
        let auto_token = tokens
            .iter()
            .find(|t| t.token_type == TokenType::Auto)
            .expect("'auto' token not found");
        assert_span_start(&source, auto_token.span, (2, 9));

        // Find the string literal
        let string_token = tokens
            .iter()
            .find(|t| matches!(t.token_type, TokenType::Str(_)))
            .expect("String token not found");
        if let TokenType::Str(s) = &string_token.token_type {
            assert_eq!(s, "hello");
            assert_span_start(&source, string_token.span, (3, 18));
            let range = string_token.span.byte_range.expect("string byte range");
            assert_eq!(source.line_col(range.end), (3, 25));
        } else {
            panic!("Expected Str token");
        }

        // Find the function definition
        let fn_token = tokens
            .iter()
            .position(|t| t.token_type == TokenType::Func)
            .expect("'func' token not found");

        // The function should span multiple tokens until the closing brace
        let mut i = fn_token;
        while i < tokens.len() && tokens[i].token_type != TokenType::CloseBrace {
            i += 1;
        }
        assert!(i > fn_token, "Function should have multiple tokens");
        assert_span_start(&source, tokens[fn_token].span, (4, 9));
        let closing_brace = tokens[i].span.byte_range.expect("brace byte range");
        assert_eq!(source.line_col(closing_brace.start).0, 4);
    }

    #[test]
    fn test_multiline_span() {
        // Using raw string literal with triple quotes to avoid escape sequence issues
        // Note: The first line is empty due to the newline after the opening """
        let input = r#"
        let message = """This is a 
        multi-line 
        string"""
        "#;

        let mut source = Source::from_test_str(input);
        let mut lexer = Lexer::new(&mut source);
        let tokens = lexer.lex_tokens().unwrap();

        // Find the string token
        let string_token = tokens
            .iter()
            .find(|t| matches!(t.token_type, TokenType::Str(_)))
            .expect("String token not found");

        // The string should span multiple lines
        // The first line of the input is empty, so the string starts on line 2
        // The column should be the start of the triple quotes (after the indentation)
        assert_span_start(&source, string_token.span, (2, 23));
        let range = string_token.span.byte_range.expect("string byte range");
        assert_eq!(source.line_col(range.end), (4, 18));
        assert!(matches!(
            &string_token.token_type,
            TokenType::Str(value) if value.contains("multi-line") && value.contains("string")
        ));
        let range = string_token.span.byte_range.expect("token byte range");
        assert_eq!(
            &input[range.start..range.end],
            "\"\"\"This is a \n        multi-line \n        string\"\"\""
        );
    }

    #[test]
    fn test_byte_literal_hex_escapes() {
        let mut source = Source::from_test_str(r#"b"Mux\x00\xff""#);
        let mut lexer = Lexer::new(&mut source);
        let tokens = lexer.lex_tokens().unwrap();
        assert!(matches!(
            &tokens[0].token_type,
            TokenType::Bytes(bytes) if bytes == &vec![b'M', b'u', b'x', 0, 255]
        ));
    }
}
