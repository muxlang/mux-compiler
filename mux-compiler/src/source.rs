use std::io::{Error, ErrorKind};
use std::path::Path;

pub struct Source {
    pub input: String,
    pub pos: usize,
    pub line: usize,
    pub col: usize,
}

impl Source {
    #[allow(dead_code)]
    pub fn new(file_path: &str) -> std::io::Result<Self> {
        if Path::new(file_path).exists() {
            let input = std::fs::read_to_string(file_path)?;
            return Ok(Self {
                input,
                pos: 0,
                line: 1,
                col: 1,
            });
        }
        Err(Error::new(ErrorKind::NotFound, "File does not exist"))
    }

    #[must_use]
    pub fn from_string(input: String) -> Self {
        Self {
            input,
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    #[allow(dead_code)]
    #[must_use]
    pub fn from_test_str(string: &str) -> Source {
        Source::from_string(string.to_string())
    }

    pub fn next_char(&mut self) -> Option<char> {
        if self.pos >= self.input.len() {
            return None;
        }
        let ch = self.input[self.pos..].chars().next()?;
        let char_len = ch.len_utf8();
        self.pos += char_len;

        if ch == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            // for multi-byte characters, we need to count the number of columns
            // based on the character's width in the terminal
            self.col += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
        }

        Some(ch)
    }

    #[must_use]
    pub fn peek(&self) -> Option<char> {
        if self.pos >= self.input.len() {
            return None;
        }
        self.input[self.pos..].chars().next()
    }

    #[must_use]
    pub fn peek_nth(&self, n: usize) -> Option<char> {
        if self.pos >= self.input.len() {
            return None;
        }
        self.input[self.pos..].chars().nth(n)
    }

    // consumes characters until the specified stop character is found. the stop
    // character is not consumed and not included in the returned string. leading
    // and trailing whitespace is trimmed from the result.
    pub fn consume_until(&mut self, stop: char) -> String {
        let mut buf = String::new();
        while let Some(c) = self.peek() {
            if c == stop {
                break;
            }
            buf.push(c);
            self.next_char();
        }
        buf.trim().to_string()
    }

    // consumes characters until the end of a multiline comment ("*/") is found. the
    // comment closer is consumed but not included in the returned string. the opening
    // "/*" should already be consumed before calling this.
    // Returns (content, found_terminator)
    pub fn consume_multiline_comment(&mut self) -> (String, bool) {
        let mut buf = String::new();
        let mut found = false;

        while let Some(c) = self.next_char() {
            if c == '*' && self.peek() == Some('/') {
                self.next_char(); // consume the '/'
                found = true;
                break;
            }
            buf.push(c);
        }

        (buf.trim().to_string(), found)
    }
}

#[cfg(test)]
mod tests {
    use super::Source;

    #[test]
    fn test_next_char_basic() {
        let mut src = Source {
            input: "abc\n".to_string(),
            pos: 0,
            line: 1,
            col: 1,
        };

        assert_eq!(src.next_char(), Some('a'));
        assert_eq!(src.line, 1);
        assert_eq!(src.col, 2);

        assert_eq!(src.next_char(), Some('b'));
        assert_eq!(src.line, 1);
        assert_eq!(src.col, 3);

        assert_eq!(src.next_char(), Some('c'));
        assert_eq!(src.line, 1);
        assert_eq!(src.col, 4);

        assert_eq!(src.next_char(), Some('\n'));
        assert_eq!(src.line, 2);
        assert_eq!(src.col, 1);

        assert_eq!(src.next_char(), None);
    }

    #[test]
    fn test_peek() {
        let mut src = Source {
            input: "xy".to_string(),
            pos: 0,
            line: 1,
            col: 1,
        };

        assert_eq!(src.peek(), Some('x')); // peek doesn't advance pos
        assert_eq!(src.line, 1);
        assert_eq!(src.col, 1); // position unchanged after peek

        assert_eq!(src.next_char(), Some('x'));
        assert_eq!(src.line, 1);
        assert_eq!(src.col, 2);

        assert_eq!(src.peek(), Some('y'));
        assert_eq!(src.line, 1);
        assert_eq!(src.col, 2); // position unchanged after peek

        assert_eq!(src.next_char(), Some('y'));
        assert_eq!(src.line, 1);
        assert_eq!(src.col, 3);

        assert_eq!(src.peek(), None);
    }

    #[test]
    fn test_multiline_positioning() {
        let mut src = Source::from_test_str("hello\nworld\n");

        // test that positions track correctly across multiple lines
        for expected_char in "hello".chars() {
            assert_eq!(src.next_char(), Some(expected_char));
        }
        assert_eq!(src.line, 1);
        assert_eq!(src.col, 6);

        assert_eq!(src.next_char(), Some('\n')); // newline
        assert_eq!(src.line, 2);
        assert_eq!(src.col, 1);

        for expected_char in "world".chars() {
            assert_eq!(src.next_char(), Some(expected_char));
        }
        assert_eq!(src.line, 2);
        assert_eq!(src.col, 6);
    }

    #[test]
    fn test_consume_until() {
        let mut src = Source::from_test_str("hello world");

        let result = src.consume_until(' ');
        assert_eq!(result, "hello");
        assert_eq!(src.line, 1);
        assert_eq!(src.col, 6); // position should be at the space
        assert_eq!(src.next_char(), Some(' ')); // consume the space
        assert_eq!(src.next_char(), Some('w')); // next character should be 'w'
    }

    #[test]
    fn test_consume_multiline_comment() {
        let mut src = Source::from_test_str(" hello\nworld*/");

        let (result, found) = src.consume_multiline_comment();
        assert_eq!(result, "hello\nworld");
        assert!(found);
        assert_eq!(src.line, 2); // should be on line 2 after the newline
        assert_eq!(src.col, 8); // after "world */"
    }

    #[test]
    fn test_unicode_handling() {
        let mut src = Source::from_test_str("こんにちは");

        // First character 'こ' is a full-width character (2 columns in terminal)
        let ch = src.next_char();
        println!("First char: {:?}, col: {}", ch, src.col);
        assert_eq!(ch, Some('こ'));
        assert_eq!(src.col, 3); // 1 (start) + 2 (full-width char) = 3

        // Second character 'ん' is also a full-width character
        let ch = src.next_char();
        println!("Second char: {:?}, col: {}", ch, src.col);
        assert_eq!(ch, Some('ん'));
        assert_eq!(src.col, 5); // 3 + 2 = 5

        // Third character 'に' is also a full-width character
        let ch = src.next_char();
        println!("Third char: {:?}, col: {}", ch, src.col);
        assert_eq!(ch, Some('に'));
        assert_eq!(src.col, 7); // 5 + 2 = 7

        // Fourth character 'ち' is also a full-width character
        let ch = src.next_char();
        println!("Fourth char: {:?}, col: {}", ch, src.col);
        assert_eq!(ch, Some('ち'));
        assert_eq!(src.col, 9); // 7 + 2 = 9

        // Fifth character 'は' is also a full-width character
        let ch = src.next_char();
        println!("Fifth char: {:?}, col: {}", ch, src.col);
        assert_eq!(ch, Some('は'));
        assert_eq!(src.col, 11); // 9 + 2 = 11

        // Test with emoji (typically 4 bytes, but displayed as one column)
        let mut src = Source::from_test_str("hello 🌟");
        // Skip "hello " (6 characters including space)
        for _ in 0..6 {
            src.next_char();
        }

        // The star emoji is a double-width character
        // We've consumed "hello " (6 characters, columns 1-6)
        assert_eq!(src.col, 7); // After 6 characters, we're at column 7
        assert_eq!(src.next_char(), Some('🌟'));
        assert_eq!(src.col, 9); // Should increment by 2 for the emoji
    }

    #[test]
    fn test_from_test_str_type_consistency() {
        let src = Source::from_test_str("test");
        assert_eq!(src.input, "test".to_string());
    }
}
