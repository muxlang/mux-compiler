use std::io::{Error, ErrorKind};
use std::path::Path;
use std::sync::Arc;

fn line_starts(input: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(input.match_indices('\n').map(|(index, _)| index + 1));
    starts
}

fn floor_char_boundary(input: &str, mut byte: usize) -> usize {
    while !input.is_char_boundary(byte) {
        byte -= 1;
    }
    byte
}

pub struct Source {
    source: Arc<SourceText>,
    pub pos: usize,
}

/// Immutable source text and its byte-indexed line starts, shared by frontend
/// consumers for one parsed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceText {
    input: String,
    line_starts: Vec<usize>,
}

impl SourceText {
    pub(crate) fn new(input: String) -> Self {
        Self {
            line_starts: line_starts(&input),
            input,
        }
    }

    #[must_use]
    pub(crate) fn text(&self) -> &str {
        &self.input
    }

    /// Return the one-based line and display column for a UTF-8 byte offset.
    /// Offsets are clamped to the source and rounded down to a character boundary.
    #[must_use]
    pub(crate) fn line_col(&self, byte: usize) -> (usize, usize) {
        let byte = floor_char_boundary(&self.input, byte.min(self.input.len()));
        let line_index = self
            .line_starts
            .partition_point(|&start| start <= byte)
            .saturating_sub(1);
        let line_start = self.line_starts.get(line_index).copied().unwrap_or(0);
        let col = self.input[line_start..byte]
            .chars()
            .map(|ch| unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1))
            .sum::<usize>()
            + 1;
        (line_index + 1, col)
    }
}

impl Source {
    #[allow(dead_code)]
    pub fn new(file_path: &str) -> std::io::Result<Self> {
        if Path::new(file_path).exists() {
            let input = std::fs::read_to_string(file_path)?;
            return Ok(Self {
                source: Arc::new(SourceText::new(input)),
                pos: 0,
            });
        }
        Err(Error::new(ErrorKind::NotFound, "File does not exist"))
    }

    #[must_use]
    pub fn from_string(input: String) -> Self {
        Self {
            source: Arc::new(SourceText::new(input)),
            pos: 0,
        }
    }

    pub(crate) fn from_source_text(source: Arc<SourceText>) -> Self {
        Self { source, pos: 0 }
    }

    #[allow(dead_code)]
    #[must_use]
    pub fn from_test_str(string: &str) -> Source {
        Source::from_string(string.to_string())
    }

    /// Read the immutable source text.
    #[must_use]
    pub fn text(&self) -> &str {
        self.source.text()
    }

    pub fn next_char(&mut self) -> Option<char> {
        if self.pos >= self.source.input.len() {
            return None;
        }
        let ch = self.source.input[self.pos..].chars().next()?;
        let char_len = ch.len_utf8();
        self.pos += char_len;
        Some(ch)
    }

    #[must_use]
    pub fn peek(&self) -> Option<char> {
        if self.pos >= self.source.input.len() {
            return None;
        }
        self.source.input[self.pos..].chars().next()
    }

    #[must_use]
    pub fn peek_nth(&self, n: usize) -> Option<char> {
        if self.pos >= self.source.input.len() {
            return None;
        }
        self.source.input[self.pos..].chars().nth(n)
    }

    /// Return the one-based line and display column for a UTF-8 byte offset.
    /// Offsets are clamped to the source and rounded down to a character boundary.
    #[must_use]
    pub fn line_col(&self, byte: usize) -> (usize, usize) {
        self.source.line_col(byte)
    }

    #[must_use]
    pub fn slice(&self, range: crate::lexer::ByteRange) -> &str {
        &self.source.input[range.start..range.end]
    }

    /// Return the byte range for a one-based source line, excluding its line ending.
    #[must_use]
    pub fn line_range(&self, line: usize) -> Option<crate::lexer::ByteRange> {
        let index = line.checked_sub(1)?;
        let start = *self.source.line_starts.get(index)?;
        let mut end = self
            .source
            .line_starts
            .get(index + 1)
            .copied()
            .unwrap_or(self.source.input.len());
        if end > start && self.source.input.as_bytes().get(end - 1) == Some(&b'\n') {
            end -= 1;
            if end > start && self.source.input.as_bytes().get(end - 1) == Some(&b'\r') {
                end -= 1;
            }
        }
        Some(crate::lexer::ByteRange::new(start, end))
    }

    #[must_use]
    pub fn line_count(&self) -> usize {
        self.source.line_starts.len()
    }

    #[must_use]
    pub fn line_text(&self, line: usize) -> Option<&str> {
        let range = self.line_range(line)?;
        Some(self.slice(range))
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
    use super::{Source, SourceText};
    use std::sync::Arc;

    fn assert_cursor_location(source: &Source, expected: (usize, usize)) {
        assert_eq!(source.line_col(source.pos), expected);
    }

    #[test]
    fn test_next_char_basic() {
        let mut src = Source {
            source: Arc::new(SourceText::new("abc\n".to_owned())),
            pos: 0,
        };

        assert_eq!(src.next_char(), Some('a'));
        assert_cursor_location(&src, (1, 2));

        assert_eq!(src.next_char(), Some('b'));
        assert_cursor_location(&src, (1, 3));

        assert_eq!(src.next_char(), Some('c'));
        assert_cursor_location(&src, (1, 4));

        assert_eq!(src.next_char(), Some('\n'));
        assert_cursor_location(&src, (2, 1));

        assert_eq!(src.next_char(), None);
    }

    #[test]
    fn test_peek() {
        let mut src = Source {
            source: Arc::new(SourceText::new("xy".to_owned())),
            pos: 0,
        };

        assert_eq!(src.peek(), Some('x')); // peek doesn't advance pos
        assert_cursor_location(&src, (1, 1));

        assert_eq!(src.next_char(), Some('x'));
        assert_cursor_location(&src, (1, 2));

        assert_eq!(src.peek(), Some('y'));
        assert_cursor_location(&src, (1, 2));

        assert_eq!(src.next_char(), Some('y'));
        assert_cursor_location(&src, (1, 3));

        assert_eq!(src.peek(), None);
    }

    #[test]
    fn test_multiline_positioning() {
        let mut src = Source::from_test_str("hello\nworld\n");

        for expected_char in "hello".chars() {
            assert_eq!(src.next_char(), Some(expected_char));
        }
        assert_cursor_location(&src, (1, 6));

        assert_eq!(src.next_char(), Some('\n')); // newline
        assert_cursor_location(&src, (2, 1));

        for expected_char in "world".chars() {
            assert_eq!(src.next_char(), Some(expected_char));
        }
        assert_cursor_location(&src, (2, 6));
    }

    #[test]
    fn test_consume_until() {
        let mut src = Source::from_test_str("hello world");

        let result = src.consume_until(' ');
        assert_eq!(result, "hello");
        assert_cursor_location(&src, (1, 6));
        assert_eq!(src.next_char(), Some(' ')); // consume the space
        assert_eq!(src.next_char(), Some('w')); // next character should be 'w'
    }

    #[test]
    fn test_consume_multiline_comment() {
        let mut src = Source::from_test_str(" hello\nworld*/");

        let (result, found) = src.consume_multiline_comment();
        assert_eq!(result, "hello\nworld");
        assert!(found);
        assert_cursor_location(&src, (2, 8));
    }

    #[test]
    fn test_unicode_handling() {
        let mut src = Source::from_test_str("こんにちは");

        // First character 'こ' is a full-width character (2 columns in terminal)
        let ch = src.next_char();
        assert_eq!(ch, Some('こ'));
        assert_cursor_location(&src, (1, 3));

        // Second character 'ん' is also a full-width character
        let ch = src.next_char();
        assert_eq!(ch, Some('ん'));
        assert_cursor_location(&src, (1, 5));

        // Third character 'に' is also a full-width character
        let ch = src.next_char();
        assert_eq!(ch, Some('に'));
        assert_cursor_location(&src, (1, 7));

        // Fourth character 'ち' is also a full-width character
        let ch = src.next_char();
        assert_eq!(ch, Some('ち'));
        assert_cursor_location(&src, (1, 9));

        // Fifth character 'は' is also a full-width character
        let ch = src.next_char();
        assert_eq!(ch, Some('は'));
        assert_cursor_location(&src, (1, 11));

        // Test with emoji (typically 4 bytes, but displayed as one column)
        let mut src = Source::from_test_str("hello 🌟");
        // Skip "hello " (6 characters including space)
        for _ in 0..6 {
            src.next_char();
        }

        assert_cursor_location(&src, (1, 7));
        assert_eq!(src.next_char(), Some('🌟'));
        assert_cursor_location(&src, (1, 9));
    }

    #[test]
    fn test_from_test_str_type_consistency() {
        let src = Source::from_test_str("test");
        assert_eq!(src.text(), "test");
    }

    #[test]
    fn line_index_resolves_utf8_and_crlf_offsets() {
        let source = Source::from_test_str("λ\r\nwide界\n");
        assert_eq!(source.line_col(0), (1, 1));
        assert_eq!(source.line_col("λ\r\n".len()), (2, 1));
        assert_eq!(source.line_col("λ\r\nwide".len()), (2, 5));
        assert_eq!(
            source.slice(crate::lexer::ByteRange::new(0, "λ".len())),
            "λ"
        );
        assert_eq!(source.line_count(), 3);
        assert_eq!(source.line_text(1), Some("λ"));
        assert_eq!(source.line_text(2), Some("wide界"));
        assert_eq!(source.line_text(3), Some(""));
    }

    #[test]
    fn source_cursor_shares_immutable_text_and_line_index() {
        let text = Arc::new(SourceText::new("λ\r\nwide界\n".to_owned()));
        let source = Source::from_source_text(text.clone());
        assert!(Arc::ptr_eq(&source.source, &text));
        assert_eq!(source.text(), "λ\r\nwide界\n");
        assert_eq!(source.line_col("λ\r\nwide".len()), (2, 5));
    }
}
