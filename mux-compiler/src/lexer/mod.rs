//! Lexical analyzer for the Mux language.
//!
//! This module provides the lexer that converts source code into a stream of tokens.

mod error;
mod span;
mod token;

pub use error::LexerError;
pub use span::Span;
pub use token::{Token, TokenType};

use crate::source::Source;
use ordered_float::OrderedFloat;

/// The lexer for the Mux language.
pub struct Lexer<'a> {
    source: &'a mut Source,
    /// How many `(` or `[` are currently open.
    ///
    /// A newline inside one continues the expression instead of ending the
    /// statement, so a long call or list literal can be wrapped across lines.
    ///
    /// `{` is deliberately NOT counted. Braces open both blocks and map/set
    /// literals, and the lexer cannot tell which - counting them would swallow
    /// the newlines that separate statements inside every function body.
    bracket_depth: usize,
    /// `bracket_depth` as it was when each currently-open `{` was reached.
    ///
    /// A brace RESETS the depth rather than merely not incrementing it, because
    /// a block can appear inside an open bracket - a lambda passed as a call
    /// argument, `spawn(func() returns void { ... })`, is the everyday case.
    /// Leaving the count alone there suppressed the newlines *inside the block*
    /// too, so its statements silently ran together: `auto b = a` followed by
    /// `-a` parsed as `auto b = a - a`, with no diagnostic.
    ///
    /// Saving and restoring means continuation applies to the bracketed
    /// expression itself and stops at the boundary of any block written inside
    /// it, which is the rule the two constructs actually need.
    brace_depths: Vec<usize>,
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
            bracket_depth: 0,
            brace_depths: Vec::new(),
        }
    }

    pub fn lex_all(&mut self) -> Result<Vec<Token>, LexerError> {
        let mut tokens = Vec::new();
        loop {
            let tok = self.next_token()?;
            if matches!(tok.token_type, TokenType::Eof) {
                break;
            }
            tokens.push(tok);
        }
        Ok(tokens)
    }

    /// Consume the next character after a successful peek.
    /// Only call this when peek() has already confirmed a character exists.
    fn consume_char(&mut self) -> char {
        self.source
            .next_char()
            .expect("peek confirmed a character exists")
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
        span.complete(self.source.line, self.source.col);
    }

    fn skip_space(&mut self) -> Result<(), LexerError> {
        // skip spaces and tabs, but not newlines
        while let Some(ch) = self.source.peek() {
            match ch {
                ' ' | '\t' | '\r' => {
                    self.source.next_char();
                }
                _ => break,
            }
        }
        Ok(())
    }

    pub fn next_token(&mut self) -> Result<Token, LexerError> {
        self.skip_space()?;

        match self.source.peek() {
            None => {
                return Ok(Token::new(
                    TokenType::Eof,
                    Span::new(self.source.line, self.source.col),
                ));
            }
            Some('\n') => {
                let start_span = Span::new(self.source.line, self.source.col);
                self.source.next_char(); // consume '\n'
                if self.bracket_depth > 0 {
                    // Inside `(` or `[` a newline continues the expression, so a
                    // wrapped call or list literal parses as one. Recursing
                    // rather than looping keeps a run of blank lines collapsing
                    // to nothing; a comment on the next line is returned as
                    // usual and filtered downstream like any other.
                    return self.next_token();
                }
                return Ok(Token::new(TokenType::NewLine, start_span));
            }
            // handle comments and division operator starting with slash
            Some('/') => {
                let start_span = Span::new(self.source.line, self.source.col);
                self.source.next_char(); // consume '/'

                match self.source.peek() {
                    Some('/') => {
                        self.source.next_char(); // consume second '/'
                        let comment = self.source.consume_until('\n');
                        return Ok(Token::new(
                            TokenType::LineComment(comment.trim().to_string()),
                            start_span,
                        ));
                    }
                    Some('*') => {
                        self.source.next_char(); // consume '*'
                        let comment = self.read_multiline_comment(start_span)?;
                        return Ok(Token::new(
                            TokenType::MultilineComment(comment.trim().to_string()),
                            start_span,
                        ));
                    }
                    Some('=') => {
                        self.source.next_char(); // consume '='
                        return Ok(Token::new(TokenType::SlashEq, start_span));
                    }
                    _ => {
                        return Ok(Token::new(TokenType::Slash, start_span));
                    }
                }
            }
            _ => {}
        }

        let start_span = Span::new(self.source.line, self.source.col);
        let c = match self.source.next_char() {
            Some(c) => c,
            None => return Ok(Token::new(TokenType::Eof, start_span)),
        };

        match c {
            '(' => {
                self.bracket_depth += 1;
                Ok(Token::new(TokenType::OpenParen, start_span))
            }
            ')' => {
                // Saturating: an unbalanced ')' is the parser's error to report,
                // and underflowing here would panic before it ever got the chance.
                self.bracket_depth = self.bracket_depth.saturating_sub(1);
                Ok(Token::new(TokenType::CloseParen, start_span))
            }
            '{' => {
                // Start a fresh continuation context: statements inside a block
                // are newline-separated even when the block sits inside an open
                // bracket.
                self.brace_depths.push(self.bracket_depth);
                self.bracket_depth = 0;
                Ok(Token::new(TokenType::OpenBrace, start_span))
            }
            '}' => {
                // Back to whatever was open around the brace, so a wrapped call
                // keeps continuing after a lambda argument closes.
                self.bracket_depth = self.brace_depths.pop().unwrap_or(0);
                Ok(Token::new(TokenType::CloseBrace, start_span))
            }
            '[' => {
                self.bracket_depth += 1;
                Ok(Token::new(TokenType::OpenBracket, start_span))
            }
            ']' => {
                self.bracket_depth = self.bracket_depth.saturating_sub(1);
                Ok(Token::new(TokenType::CloseBracket, start_span))
            }
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

    fn read_multiline_comment(&mut self, start_span: Span) -> Result<String, LexerError> {
        let (comment, found) = self.source.consume_multiline_comment();
        if found {
            Ok(comment)
        } else {
            Err(LexerError::with_help(
                "Unterminated block comment",
                start_span,
                "Add a closing '*/' to end the block comment",
            ))
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
            _ => TokenType::Id(ident),
        }
    }

    fn read_identifier_or_keyword(&mut self, first_char: char, mut start_span: Span) -> Token {
        let mut ident = first_char.to_string();
        self.consume_identifier_tail(&mut ident);
        start_span.complete(self.source.line, self.source.col);
        Token::new(Self::keyword_or_identifier_token_type(ident), start_span)
    }

    fn read_nested_multiline_comment_body(
        &mut self,
        start_span: Span,
    ) -> Result<String, LexerError> {
        let mut comment = String::new();
        let mut depth = 1;
        while depth > 0 {
            match self.source.next_char() {
                Some('*') => {
                    if self.source.peek() == Some('/') {
                        self.source.next_char();
                        depth -= 1;
                        if depth > 0 {
                            comment.push('*');
                        }
                    } else {
                        comment.push('*');
                    }
                }
                Some('/') => {
                    if self.source.peek() == Some('*') {
                        self.source.next_char();
                        depth += 1;
                        comment.push_str("/*");
                    } else {
                        comment.push('/');
                    }
                }
                Some(ch) => comment.push(ch),
                None => {
                    return Err(LexerError::with_help(
                        "Unterminated block comment",
                        start_span,
                        "Add a closing '*/' to end the block comment",
                    ));
                }
            }
        }
        Ok(comment)
    }

    fn read_slash_token(&mut self, mut start_span: Span) -> Result<Token, LexerError> {
        if self.source.peek() == Some('/') {
            self.source.next_char();
            let mut comment = String::new();
            while let Some(ch) = self.source.next_char() {
                if ch == '\n' || ch == '\r' {
                    break;
                }
                comment.push(ch);
            }
            start_span.complete(self.source.line, self.source.col);
            return Ok(Token::new(
                TokenType::LineComment(comment.trim().to_string()),
                start_span,
            ));
        }

        if self.source.peek() == Some('*') {
            self.source.next_char();
            let comment = self.read_nested_multiline_comment_body(start_span)?;
            start_span.complete(self.source.line, self.source.col);
            return Ok(Token::new(
                TokenType::MultilineComment(comment.trim().to_string()),
                start_span,
            ));
        }

        if self.source.peek() == Some('=') {
            self.source.next_char();
            start_span.complete(self.source.line, self.source.col);
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
                "Mux does not have range literal syntax",
                Span::new(self.source.line, self.source.col),
                "Use range(a, b) to iterate over a numeric range, e.g. for int i in range(0, 10)",
            ));
        } else {
            return Err(LexerError::new(
                "Expected digit after decimal point",
                Span::new(self.source.line, self.source.col),
            ));
        }

        num.push(self.consume_char());
        if !self.consume_digits_and_underscores(num) {
            return Err(LexerError::new(
                "Expected digit after decimal point",
                Span::new(self.source.line, self.source.col),
            ));
        }

        Ok(())
    }

    fn consume_exponent_if_present(
        &mut self,
        num: &mut String,
        is_float: &mut bool,
    ) -> Result<(), LexerError> {
        if !matches!(self.source.peek(), Some('e') | Some('E')) {
            return Ok(());
        }

        *is_float = true;
        num.push(self.consume_char());

        if let Some('+') | Some('-') = self.source.peek() {
            num.push(self.consume_char());
        }

        if !self.consume_digits_and_underscores(num) {
            return Err(LexerError::with_help(
                "Missing exponent in scientific notation",
                Span::new(self.source.line, self.source.col),
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
                format!("Invalid float literal: {}", num),
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
                format!("Invalid float literal: {}", num),
                start_span,
                "Float literals cannot contain letters. Use a space or separate expression.",
            ));
        }

        let clean_num: String = num.chars().filter(|c| *c != '_').collect();
        clean_num
            .parse::<f64>()
            .map(OrderedFloat)
            .map(|f| Token::new(TokenType::Float(f), start_span))
            .map_err(|_| LexerError::new(format!("Invalid float literal: {}", num), start_span))
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
                format!("Invalid integer literal: {}", num),
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
                    format!("Invalid integer literal: {}", num),
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
        start_span.complete(self.source.line, self.source.col);

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
            'a'..='z' | 'A'..='Z' => Ok(self.read_identifier_or_keyword(first_char, start_span)),
            _ => Err(LexerError::with_help(
                format!("Unexpected character: '{}'", first_char),
                start_span,
                "This character is not recognized as valid Mux syntax. Check for accidental special characters or encoding issues.",
            )),
        }
    }

    fn handle_star_operator(&mut self, mut start_span: Span) -> Result<Token, LexerError> {
        if self.source.peek() == Some('*') {
            self.source.next_char();
            start_span.complete(self.source.line, self.source.col);
            Ok(Token::new(TokenType::StarStar, start_span))
        } else if self.source.peek() == Some('=') {
            self.source.next_char();
            start_span.complete(self.source.line, self.source.col);
            Ok(Token::new(TokenType::StarEq, start_span))
        } else {
            Ok(Token::new(TokenType::Star, start_span))
        }
    }

    fn handle_eq_operator(&mut self, mut start_span: Span) -> Result<Token, LexerError> {
        if self.source.peek() == Some('=') {
            self.source.next_char();
            start_span.complete(self.source.line, self.source.col);
            Ok(Token::new(TokenType::EqEq, start_span))
        } else {
            Ok(Token::new(TokenType::Eq, start_span))
        }
    }

    fn handle_bang_operator(&mut self, mut start_span: Span) -> Result<Token, LexerError> {
        if self.source.peek() == Some('=') {
            self.source.next_char();
            start_span.complete(self.source.line, self.source.col);
            Ok(Token::new(TokenType::NotEq, start_span))
        } else {
            Ok(Token::new(TokenType::Bang, start_span))
        }
    }

    fn handle_plus_operator(&mut self, mut start_span: Span) -> Result<Token, LexerError> {
        match self.source.peek() {
            Some('+') => {
                self.source.next_char();
                start_span.complete(self.source.line, self.source.col);
                Ok(Token::new(TokenType::Incr, start_span))
            }
            Some('=') => {
                self.source.next_char();
                start_span.complete(self.source.line, self.source.col);
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
                start_span.complete(self.source.line, self.source.col);
                Ok(Token::new(TokenType::Decr, start_span))
            }
            Some('=') => {
                self.source.next_char();
                start_span.complete(self.source.line, self.source.col);
                Ok(Token::new(TokenType::MinusEq, start_span))
            }
            Some(c) if c.is_ascii_digit() => self.read_number(first_char, start_span),
            _ => Ok(Token::new(TokenType::Minus, start_span)),
        }
    }

    fn handle_lt_operator(&mut self, mut start_span: Span) -> Result<Token, LexerError> {
        if self.source.peek() == Some('=') {
            self.source.next_char();
            start_span.complete(self.source.line, self.source.col);
            Ok(Token::new(TokenType::Le, start_span))
        } else {
            Ok(Token::new(TokenType::Lt, start_span))
        }
    }

    fn handle_gt_operator(&mut self, mut start_span: Span) -> Result<Token, LexerError> {
        if self.source.peek() == Some('=') {
            self.source.next_char();
            start_span.complete(self.source.line, self.source.col);
            Ok(Token::new(TokenType::Ge, start_span))
        } else {
            Ok(Token::new(TokenType::Gt, start_span))
        }
    }

    fn handle_or_operator(&mut self, start_span: Span) -> Result<Token, LexerError> {
        if self.source.peek() == Some('|') {
            self.source.next_char();
            let mut span = start_span;
            span.complete(self.source.line, self.source.col);
            Ok(Token::new(TokenType::Or, span))
        } else {
            Err(LexerError::with_help(
                "Unexpected character '|'",
                start_span,
                "Single '|' is not a valid operator. Use '||' for logical OR",
            ))
        }
    }

    fn handle_and_operator(&mut self, mut start_span: Span) -> Result<Token, LexerError> {
        if self.source.peek() == Some('&') {
            self.source.next_char();
            start_span.complete(self.source.line, self.source.col);
            Ok(Token::new(TokenType::And, start_span))
        } else {
            start_span.complete(self.source.line, self.source.col);
            Ok(Token::new(TokenType::Ref, start_span))
        }
    }

    fn handle_string_start(&mut self, start_span: Span) -> Result<Token, LexerError> {
        if self.source.peek() == Some('"') && self.source.peek_nth(1) == Some('"') {
            self.source.next_char();
            self.source.next_char();
            self.read_string(start_span)
        } else {
            self.read_string(start_span)
        }
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
        let start_col = start_span.col_start;

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

        self.handle_unterminated_string(&s, start_col, start_span)
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
                start_span.complete(self.source.line, self.source.col);
                Some(Ok(Token::new(TokenType::Str(s.to_string()), start_span)))
            } else {
                None
            }
        } else {
            start_span.complete(self.source.line, self.source.col);
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
                    format!("Unknown escape sequence: \\{}", c),
                    Span::new(self.source.line, self.source.col - 1),
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
        string_content: &str,
        start_col: usize,
        mut start_span: Span,
    ) -> Result<Token, LexerError> {
        let first_newline = string_content.find('\n').unwrap_or(string_content.len());
        let error_col = start_col + first_newline;
        start_span.complete(self.source.line, error_col);
        Err(LexerError::with_help(
            "Unterminated string",
            start_span,
            "Make sure to close the string with a matching quote",
        ))
    }

    fn read_char(&mut self, start_span: Span) -> Result<Token, LexerError> {
        let start_col = self.source.col - 1;
        let mut chars = Vec::new();
        let mut escaped = false;

        while let Some(c) = self.source.next_char() {
            if c == '\\' && !escaped {
                escaped = true;
                continue;
            }

            if c == '\'' && !escaped {
                return self.finish_char_literal(chars, start_span, start_col);
            }

            self.process_char_literal_char(c, &mut escaped, &mut chars, start_span, start_col)?;

            if chars.len() > 1 && !escaped {
                return Err(LexerError::with_help(
                    "Char literal must be exactly one character",
                    Span::new(start_span.row_start, start_col + 1),
                    "Example: 'a', '\\n', '\\''",
                ));
            }
        }

        Err(LexerError::with_help(
            "Unterminated character literal",
            start_span,
            "Make sure to close the character with a matching quote",
        ))
    }

    fn finish_char_literal(
        &mut self,
        chars: Vec<char>,
        mut start_span: Span,
        start_col: usize,
    ) -> Result<Token, LexerError> {
        if chars.len() != 1 {
            return Err(LexerError::with_help(
                "Char literal must be exactly one character",
                Span::new(start_span.row_start, start_col + 1),
                "Example: 'a', '\\n', '\\''",
            ));
        }
        start_span.complete(self.source.line, self.source.col);
        Ok(Token::new(TokenType::Char(chars[0]), start_span))
    }

    fn process_char_literal_char(
        &mut self,
        c: char,
        escaped: &mut bool,
        chars: &mut Vec<char>,
        _start_span: Span,
        _start_col: usize,
    ) -> Result<(), LexerError> {
        if *escaped {
            if let Some(decoded) = Self::decode_escape(c) {
                chars.push(decoded);
            } else {
                return Err(LexerError::with_help(
                    format!("Unknown escape sequence: \\{}", c),
                    Span::new(self.source.line, self.source.col - 1),
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
                    "Expected digit after decimal point",
                    Span::new(self.source.line, self.source.col),
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

        start_span.complete(self.source.line, self.source.col);

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
        result: Result<Vec<Token>, LexerError>,
        expected_msg: &str,
        expected_line: usize,
        expected_col: usize,
    ) {
        match result {
            Ok(tokens) => panic!(
                "Expected error '{}' but got tokens: {:?}",
                expected_msg, tokens
            ),
            Err(e) => {
                assert!(
                    e.message.contains(expected_msg),
                    "Expected error message to contain '{}' but got '{}'",
                    expected_msg,
                    e.message
                );
                assert_eq!(
                    e.span.row_start, expected_line,
                    "Expected error on line {} but got {}",
                    expected_line, e.span.row_start
                );
                assert_eq!(
                    e.span.col_start, expected_col,
                    "Expected error at column {} but got {}",
                    expected_col, e.span.col_start
                );
            }
        }
    }

    #[test]
    fn test_position_tracking_across_lines() {
        // Test a more complex example across multiple lines
        let input = "auto x = 42\nfunc test() {\n  return x\n}";
        let mut source = Source::from_test_str(input);
        let mut lexer = Lexer::new(&mut source);

        assert_eq!(lexer.next_token().unwrap().token_type, TokenType::Auto);
        assert_eq!(
            lexer.next_token().unwrap().token_type,
            TokenType::Id("x".to_string())
        );
        assert_eq!(lexer.next_token().unwrap().token_type, TokenType::Eq);
        match lexer.next_token().unwrap().token_type {
            TokenType::Int(42) => {}
            other => panic!("Expected Int(42), got {:?}", other),
        }
        assert_eq!(lexer.next_token().unwrap().token_type, TokenType::NewLine);

        assert_eq!(lexer.next_token().unwrap().token_type, TokenType::Func);
        assert_eq!(
            lexer.next_token().unwrap().token_type,
            TokenType::Id("test".to_string())
        );
        assert_eq!(lexer.next_token().unwrap().token_type, TokenType::OpenParen);
        assert_eq!(
            lexer.next_token().unwrap().token_type,
            TokenType::CloseParen
        );
        assert_eq!(lexer.next_token().unwrap().token_type, TokenType::OpenBrace);
        assert_eq!(lexer.next_token().unwrap().token_type, TokenType::NewLine);

        assert_eq!(lexer.next_token().unwrap().token_type, TokenType::Return);
        assert_eq!(
            lexer.next_token().unwrap().token_type,
            TokenType::Id("x".to_string())
        );
        assert_eq!(lexer.next_token().unwrap().token_type, TokenType::NewLine);

        assert_eq!(
            lexer.next_token().unwrap().token_type,
            TokenType::CloseBrace
        );
        assert_eq!(lexer.next_token().unwrap().token_type, TokenType::Eof);
    }

    #[test]
    fn test_char_errors() {
        // Test string literals instead of character literals since Mux might not support single-quoted chars

        // Empty string literal
        let input = "auto x = \"\"";
        let mut source = Source::from_test_str(input);
        let result = Lexer::new(&mut source).lex_all();
        assert!(result.is_ok(), "Empty string literals should be valid");

        // Unterminated string
        let input = "auto x = \"unterminated";
        let mut source = Source::from_test_str(input);
        let result = Lexer::new(&mut source).lex_all();
        assert_lexer_error(result, "Unterminated string", 1, 10);

        // Invalid escape sequence in string
        let input = "auto x = \"invalid \\z escape\"";
        let mut source = Source::from_test_str(input);
        let result = Lexer::new(&mut source).lex_all();
        assert_lexer_error(result, "Unknown escape sequence", 1, 20);
    }

    #[test]
    fn test_string_errors() {
        let input = r#"auto x = "unterminated
auto y = 42"#;
        let mut source = Source::from_test_str(input);
        let result = Lexer::new(&mut source).lex_all();
        assert_lexer_error(result, "Unterminated string", 1, 10);

        let input = r#"auto x = "invalid \z escape""#;
        let mut source = Source::from_test_str(input);
        let result = Lexer::new(&mut source).lex_all();

        match result {
            Ok(tokens) => {
                panic!(
                    "Expected error for invalid escape sequence but got tokens: {:?}",
                    tokens
                );
            }
            Err(e) => {
                assert!(
                    e.message.contains("Unknown escape sequence: \\z"),
                    "Expected 'Unknown escape sequence: \\z' error, got: {}",
                    e.message
                );
                assert_eq!(e.span.row_start, 1);
                assert_eq!(e.span.col_start, 20);
            }
        }
    }

    #[test]
    fn test_number_errors() {
        // The lexer should fail on multiple decimal points
        let input = "auto x = 1.2.3";
        let mut source = Source::from_test_str(input);
        let result = Lexer::new(&mut source).lex_all();
        assert!(
            result.is_err(),
            "Expected error for invalid float literal with multiple decimals"
        );

        // The lexer should fail on invalid scientific notation
        let input = "auto x = 1.23e";
        let mut source = Source::from_test_str(input);
        let result = Lexer::new(&mut source).lex_all();
        assert!(
            result.is_err(),
            "Expected error for invalid scientific notation"
        );

        // The lexer should fail on "1e+" as it's invalid scientific notation
        let input = "auto x = 1e+";
        let mut source = Source::from_test_str(input);
        let result = Lexer::new(&mut source).lex_all();
        assert!(
            result.is_err(),
            "Expected error for invalid scientific notation"
        );

        // The lexer should fail on trailing decimal points
        let input = "auto x = 1.";
        let mut source = Source::from_test_str(input);
        let result = Lexer::new(&mut source).lex_all();
        assert!(result.is_err(), "Expected error for trailing decimal point");

        // The lexer should fail on integer literals with identifier suffixes
        let input = "auto x = 123abc";
        let mut source = Source::from_test_str(input);
        let result = Lexer::new(&mut source).lex_all();
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
        let result = Lexer::new(&mut source).lex_all();
        assert_lexer_error(result, "Mux does not have range literal syntax", 1, 15);
    }

    #[test]
    fn test_multiple_errors() {
        let input = r#"
            auto x = "unterminated
            auto y = 1.2.3
            auto z = 123invalid
        "#;

        let mut source = Source::from_test_str(input);
        let result = Lexer::new(&mut source).lex_all();

        // The lexer will stop at the first error, so we should only get one error
        match result {
            Ok(tokens) => panic!("Expected error but got tokens: {:?}", tokens),
            Err(e) => {
                // The first error should be about the unterminated string
                assert!(
                    e.message.contains("Unterminated string"),
                    "Unexpected error: {}",
                    e.message
                );
                assert_eq!(e.span.row_start, 2);
                assert_eq!(e.span.col_start, 22);
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
        let result = Lexer::new(&mut source).lex_all();
        match result {
            Ok(tokens) => panic!("Expected error but got tokens: {:?}", tokens),
            Err(e) => {
                assert!(
                    e.message.contains("Unterminated string"),
                    "Expected 'Unterminated string' error, got: {}",
                    e.message
                );
                assert_eq!(e.span.row_start, 2);
                assert_eq!(e.span.col_start, 22);
            }
        }

        // Test with a valid variable declaration
        let fixed_input = r#"
            auto x = 42
            auto y = 1.2
        "#;
        let mut source = Source::from_test_str(fixed_input);
        let result = Lexer::new(&mut source).lex_all();
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
                panic!("Expected successful tokenization but got error: {}", e);
            }
        }
    }

    #[test]
    fn test_number_parsing() {
        // Method calls on literals should be unambiguous: `1.to_string()` is int + dot + ident.
        let mut source = Source::from_test_str("auto x = 1.to_string()");
        let tokens = Lexer::new(&mut source).lex_all().unwrap();
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
        let tokens = Lexer::new(&mut source).lex_all().unwrap();
        match tokens.last().map(|t| &t.token_type) {
            Some(TokenType::Float(f)) => assert!((f.into_inner() - 0.5).abs() < f64::EPSILON),
            other => panic!("Expected Float(0.5), got {:?}", other),
        }
    }

    #[test]
    fn test_numbers() {
        let input = "42 1_000 3.45 0.5 5.0";
        let mut source = Source::from_test_str(input);
        let mut lexer = Lexer::new(&mut source);
        let tokens = lexer.lex_all().unwrap();
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
            _ => panic!("Unexpected token types: {:?}", token_types),
        }
    }

    #[test]
    fn test_simple_strings() {
        let input = r#""hello" "world""#;
        let mut source = Source::from_test_str(input);
        let mut lexer = Lexer::new(&mut source);
        let tokens = lexer.lex_all().unwrap();

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
        let tokens = lexer.lex_all().unwrap();

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
        let input = r#"'a''\n''\''"#; // No spaces between characters
        let mut source = Source::from_test_str(input);
        let mut lexer = Lexer::new(&mut source);
        let tokens = lexer.lex_all().unwrap();

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
        let tokens = lexer.lex_all().unwrap();

        assert_eq!(tokens.len(), 1);
        match &tokens[0].token_type {
            TokenType::Str(s) => assert_eq!(s, "a\n\t\\\"'\r\0"),
            _ => panic!("Expected Str token, got {:?}", tokens[0]),
        }

        // test that uppercase legacy forms are treated as identifiers
        let input = "Some None Ok Err";
        let mut source = Source::from_test_str(input);
        let mut lexer = Lexer::new(&mut source);
        let tokens = lexer.lex_all().unwrap();

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
        let tokens = lexer.lex_all().unwrap();

        assert_eq!(
            tokens.into_iter().map(|t| t.token_type).collect::<Vec<_>>(),
            vec![TokenType::Bool(true), TokenType::Bool(false)]
        );

        // test that keywords are case-sensitive
        let input = "some none ok err";
        let mut source = Source::from_test_str(input);
        let mut lexer = Lexer::new(&mut source);
        let tokens = lexer.lex_all().unwrap();

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
        assert!(lexer.lex_all().is_err());

        // unknown escape sequences
        for input in [r#""\a""#, r#""\c""#] {
            let mut source = Source::from_test_str(input);
            let mut lexer = Lexer::new(&mut source);
            assert!(lexer.lex_all().is_err());
        }
    }

    fn lex_types(input: &str) -> Vec<TokenType> {
        let mut source = Source::from_test_str(input);
        let mut lexer = Lexer::new(&mut source);
        lexer
            .lex_all()
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
                other => panic!("'{}' should lex as one identifier, got {:?}", name, other),
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
        let tokens: Vec<_> = lexer.lex_all().unwrap().into_iter().collect();

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
            _ => panic!("Unexpected token sequence: {:?}", token_types),
        }
    }

    #[test]
    fn test_operators() {
        // Helper function to get non-whitespace tokens
        fn get_tokens(input: &str) -> Vec<TokenType> {
            let mut source = Source::from_test_str(input);
            let mut lexer = Lexer::new(&mut source);
            lexer
                .lex_all()
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
            _ => panic!("Unexpected token sequence: {:?}", token_types),
        }
    }

    #[test]
    fn test_comments() {
        let input = "// line comment\n/* multi\nline */";
        let mut source = Source::from_test_str(input);
        let mut lexer = Lexer::new(&mut source);
        let tokens = lexer.lex_all().unwrap();

        // We should have 3 tokens: line comment, newline, and multiline comment
        assert_eq!(tokens.len(), 3);

        // Check line comment - leading space is now trimmed
        match &tokens[0].token_type {
            TokenType::LineComment(s) => assert_eq!(s, "line comment"),
            _ => panic!("Expected LineComment, got {:?}", tokens[0]),
        }

        // Check newline
        assert_eq!(tokens[1].token_type, TokenType::NewLine);

        // Check multiline comment
        match &tokens[2].token_type {
            TokenType::MultilineComment(s) => assert_eq!(s, "multi\nline"),
            _ => panic!("Expected MultilineComment, got {:?}", tokens[2]),
        }
    }

    #[test]
    fn test_multiple_line_comments() {
        let input = "// first comment\n// second comment\n// third comment";
        let mut source = Source::from_test_str(input);
        let mut lexer = Lexer::new(&mut source);
        let tokens = lexer.lex_all().unwrap();

        // We should have 5 tokens: 3 comments and 2 newlines
        assert_eq!(tokens.len(), 5);

        // Check first comment
        match &tokens[0].token_type {
            TokenType::LineComment(s) => assert_eq!(s, "first comment"),
            _ => panic!("Expected first LineComment, got {:?}", tokens[0]),
        }

        // First newline
        assert_eq!(tokens[1].token_type, TokenType::NewLine);

        // Second comment
        match &tokens[2].token_type {
            TokenType::LineComment(s) => assert_eq!(s, "second comment"),
            _ => panic!("Expected second LineComment, got {:?}", tokens[2]),
        }

        // Second newline
        assert_eq!(tokens[3].token_type, TokenType::NewLine);

        // Third comment
        match &tokens[4].token_type {
            TokenType::LineComment(s) => assert_eq!(s, "third comment"),
            _ => panic!("Expected third LineComment, got {:?}", tokens[4]),
        }
    }

    #[test]
    fn test_dots_and_newlines() {
        let input = "1.2\n3.4\n5.6";
        let mut source = Source::from_test_str(input);
        let mut lexer = Lexer::new(&mut source);
        let tokens = lexer.lex_all().unwrap();

        // We expect 3 floats and 2 newlines
        assert_eq!(tokens.len(), 5);

        // First float
        match &tokens[0].token_type {
            TokenType::Float(f) => assert!((f.into_inner() - 1.2).abs() < f64::EPSILON),
            _ => panic!("Expected Float(1.2), got {:?}", tokens[0]),
        }
        assert_eq!(tokens[0].span.row_start, 1);
        assert_eq!(tokens[0].span.col_start, 1);

        // First newline
        assert_eq!(tokens[1].token_type, TokenType::NewLine);
        assert_eq!(tokens[1].span.row_start, 1);
        assert_eq!(tokens[1].span.col_start, 4);

        // Second float
        match &tokens[2].token_type {
            TokenType::Float(f) => assert!((f.into_inner() - 3.4).abs() < f64::EPSILON),
            _ => panic!("Expected Float(3.4), got {:?}", tokens[2]),
        }
        assert_eq!(tokens[2].span.row_start, 2);
        assert_eq!(tokens[2].span.col_start, 1);

        // Second newline
        assert_eq!(tokens[3].token_type, TokenType::NewLine);
        assert_eq!(tokens[3].span.row_start, 2);
        assert_eq!(tokens[3].span.col_start, 4);

        // Third float
        match &tokens[4].token_type {
            TokenType::Float(f) => assert!((f.into_inner() - 5.6).abs() < f64::EPSILON),
            _ => panic!("Expected Float(5.6), got {:?}", tokens[4]),
        }
        assert_eq!(tokens[4].span.row_start, 3);
        assert_eq!(tokens[4].span.col_start, 1);
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
        let tokens = lexer.lex_all().unwrap();

        // Find the 'auto' token
        let auto_token = tokens
            .iter()
            .find(|t| t.token_type == TokenType::Auto)
            .expect("'auto' token not found");
        assert_eq!(auto_token.span.row_start, 2);
        assert_eq!(auto_token.span.col_start, 9);

        // Find the string literal
        let string_token = tokens
            .iter()
            .find(|t| matches!(t.token_type, TokenType::Str(_)))
            .expect("String token not found");
        if let TokenType::Str(s) = &string_token.token_type {
            assert_eq!(s, "hello");
            assert_eq!(string_token.span.row_start, 3);
            assert_eq!(string_token.span.col_start, 18);
            // Check that the span includes the quotes
            assert_eq!(string_token.span.col_end, Some(25));
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
        assert_eq!(tokens[fn_token].span.row_start, 4);
        assert_eq!(tokens[i].span.row_start, 4);
    }

    #[test]
    fn test_multiline_span() {
        // Using raw string literal with triple quotes to avoid escape sequence issues
        // Note: The first line is empty due to the newline after the opening """
        let input = r###"
        let message = """This is a 
        multi-line 
        string"""
        "###;

        let mut source = Source::from_test_str(input);
        let mut lexer = Lexer::new(&mut source);
        let tokens = lexer.lex_all().unwrap();

        // Find the string token
        let string_token = tokens
            .iter()
            .find(|t| matches!(t.token_type, TokenType::Str(_)))
            .expect("String token not found");

        // The string should span multiple lines
        // The first line of the input is empty, so the string starts on line 2
        // The column should be the start of the triple quotes (after the indentation)
        assert_eq!(string_token.span.row_start, 2);
        assert_eq!(string_token.span.col_start, 23);
        // The string ends at column 16 on the last line (after "string" and before the closing triple quotes)
        assert_eq!(string_token.span.col_end, Some(16));
        // The string should span 3 lines (from row 2 to row 4)
        assert_eq!(string_token.span.row_end, Some(4));
    }
}
