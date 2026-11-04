//! Source locations shared by the parser, AST, and structured diagnostics.
use super::source::SourceId;
use std::ops::Range;

/// CRLF counts once, at the CR. LF and standalone CR also begin a new line.
pub(crate) fn is_line_break(bytes: &[u8], at: usize) -> bool {
    bytes[at] == b'\r' || (bytes[at] == b'\n' && (at == 0 || bytes[at - 1] != b'\r'))
}

/// A half-open UTF-8 byte range with file identity.
/// Source zero denotes caller-owned text; named files belong to a source snapshot.
#[derive(joky_image_derive::Image, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    source: SourceId,
    start: usize,
    end: usize,
}

impl Span {
    /// Construct a span from UTF-8 byte offsets.
    pub const fn new(start: usize, end: usize) -> Self {
        assert!(start <= end, "span start must not exceed span end");
        Self {
            source: SourceId::ANONYMOUS,
            start,
            end,
        }
    }

    /// Construct an empty span at one byte offset.
    pub const fn empty(at: usize) -> Self {
        Self::new(at, at)
    }

    pub const fn with_source(mut self, source: SourceId) -> Self {
        self.source = source;
        self
    }

    pub const fn source_id(self) -> SourceId {
        self.source
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

    /// One-based line and Unicode scalar column of the start, excluding a leading BOM.
    pub fn line_column(self, source: &str) -> (usize, usize) {
        let end = source.floor_char_boundary(self.start.min(source.len()));
        let prefix = &source[..end];
        let line = (0..end)
            .filter(|&i| is_line_break(source.as_bytes(), i))
            .count()
            + 1;
        let prefix = prefix.strip_prefix('\u{feff}').unwrap_or(prefix);
        let tail = prefix.rsplit(['\r', '\n']).next().unwrap_or("");
        (line, tail.chars().count() + 1)
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
