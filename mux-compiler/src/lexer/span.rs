//! Source location tracking for tokens and errors.

/// A half-open byte range into a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}

impl ByteRange {
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    #[must_use]
    pub const fn empty(at: usize) -> Self {
        Self { start: at, end: at }
    }
}

/// Represents a source location span. Byte ranges are authoritative when present;
/// row and column fields remain for diagnostic compatibility.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Span {
    pub row_start: usize,
    pub row_end: Option<usize>,
    pub col_start: usize,
    pub col_end: Option<usize>,
    pub byte_range: Option<ByteRange>,
}

impl std::fmt::Debug for Span {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Span")
            .field("row_start", &self.row_start)
            .field("row_end", &self.row_end)
            .field("col_start", &self.col_start)
            .field("col_end", &self.col_end)
            .finish()
    }
}

impl Span {
    #[must_use]
    pub fn new(row_start: usize, col_start: usize) -> Self {
        Self {
            row_start,
            row_end: None,
            col_start,
            col_end: None,
            byte_range: None,
        }
    }

    #[must_use]
    pub fn with_byte_range(mut self, start: usize, end: usize) -> Self {
        self.byte_range = Some(ByteRange::new(start, end));
        self
    }

    pub fn complete(&mut self, row_end: usize, col_end: usize) {
        self.row_end = Some(row_end);
        self.col_end = Some(col_end);
    }
}
