//! Source ranges carried through lexing, parsing, and diagnostics.

/// A half-open byte range into one source file.
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

/// A source location represented only by its half-open byte range.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Span {
    pub byte_range: Option<ByteRange>,
}

impl std::fmt::Debug for Span {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Span").finish_non_exhaustive()
    }
}

impl Span {
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        Self {
            byte_range: Some(ByteRange::new(start, end)),
        }
    }

    #[must_use]
    pub const fn empty(at: usize) -> Self {
        Self {
            byte_range: Some(ByteRange::empty(at)),
        }
    }

    pub fn extend_to(&mut self, end: usize) {
        let start = self.byte_range.map_or(end, |range| range.start);
        self.byte_range = Some(ByteRange::new(start, end));
    }
}
