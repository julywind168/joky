//! Source locations shared by the parser, AST, and structured diagnostics.
use std::ops::Range;

/// A half-open UTF-8 byte range in the original source text.
///
/// `start` is inclusive and `end` is exclusive. Spans do not carry a source
/// file identifier; the caller supplies the source text when slicing them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    start: usize,
    end: usize,
}

impl Span {
    /// Construct a span from UTF-8 byte offsets.
    pub const fn new(start: usize, end: usize) -> Self {
        assert!(start <= end, "span start must not exceed span end");
        Self { start, end }
    }

    /// Construct an empty span at one byte offset.
    pub const fn empty(at: usize) -> Self {
        Self::new(at, at)
    }

    pub const fn start(self) -> usize {
        self.start
    }

    pub const fn end(self) -> usize {
        self.end
    }

    pub const fn len(self) -> usize {
        self.end - self.start
    }

    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    pub const fn contains(self, offset: usize) -> bool {
        self.start <= offset && offset < self.end
    }

    /// Return the equivalent standard range for APIs such as Ariadne.
    pub const fn range(self) -> Range<usize> {
        self.start..self.end
    }

    /// Borrow the source slice covered by this span when it is valid UTF-8.
    pub fn slice(self, source: &str) -> Option<&str> {
        source.get(self.range())
    }
}

impl From<Range<usize>> for Span {
    fn from(range: Range<usize>) -> Self {
        Self::new(range.start, range.end)
    }
}

impl From<Span> for Range<usize> {
    fn from(span: Span) -> Self {
        span.range()
    }
}

#[cfg(test)]
mod tests {
    use super::Span;

    #[test]
    fn exposes_half_open_utf8_byte_ranges() {
        let source = "héllo";
        let span = Span::new(1, 3);
        assert_eq!(span.start(), 1);
        assert_eq!(span.end(), 3);
        assert_eq!(span.len(), 2);
        assert_eq!(span.slice(source), Some("é"));
        assert!(span.contains(1));
        assert!(!span.contains(3));
        assert_eq!(span.range(), 1..3);
    }
}
