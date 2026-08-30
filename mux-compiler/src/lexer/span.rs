//! Source location tracking for tokens and errors.

/// Represents a source location span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Span {
    pub row_start: usize,
    pub row_end: Option<usize>,
    pub col_start: usize,
    pub col_end: Option<usize>,
}

impl Span {
    #[must_use]
    pub fn new(row_start: usize, col_start: usize) -> Self {
        Self {
            row_start,
            row_end: None,
            col_start,
            col_end: None,
        }
    }

    pub fn complete(&mut self, row_end: usize, col_end: usize) {
        self.row_end = Some(row_end);
        self.col_end = Some(col_end);
    }
}
