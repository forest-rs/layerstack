//! Source locations and spans.
//!
//! All tokens and CST/AST nodes carry [`Span`] information linking them
//! back to byte offsets in the original source. This enables diagnostics,
//! editor integration, and lossless round-tripping.

/// A half-open byte range `[start, end)` into the source text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    /// Byte offset of the first character (inclusive).
    pub start: u32,
    /// Byte offset past the last character (exclusive).
    pub end: u32,
}

impl Span {
    /// Creates a new span from byte offsets.
    #[inline]
    pub fn new(start: u32, end: u32) -> Self {
        debug_assert!(start <= end, "span start must not exceed end");
        Self { start, end }
    }

    /// Returns the byte length of this span.
    #[inline]
    pub fn len(&self) -> u32 {
        self.end - self.start
    }

    /// Returns `true` if the span is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Merges two spans into one covering both.
    #[inline]
    pub fn cover(self, other: Self) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    /// Extracts the spanned slice from the source text.
    #[inline]
    pub fn text<'a>(&self, source: &'a str) -> &'a str {
        &source[self.start as usize..self.end as usize]
    }
}

/// A line/column position in source text (1-indexed).
///
/// Computed on demand from byte offsets — not stored on every token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextPosition {
    /// 1-indexed line number.
    pub line: u32,
    /// 1-indexed column (in bytes, not characters).
    pub column: u32,
}

impl TextPosition {
    /// Computes the line and column for a byte offset in `source`.
    pub fn from_offset(source: &str, offset: u32) -> Self {
        let offset = offset as usize;
        let mut line = 1_u32;
        let mut col_start = 0_usize;

        for (i, byte) in source.as_bytes().iter().enumerate() {
            if i == offset {
                break;
            }
            if *byte == b'\n' {
                line += 1;
                col_start = i + 1;
            }
        }

        Self {
            line,
            column: (offset - col_start) as u32 + 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_basics() {
        let s = Span::new(2, 5);
        assert_eq!(s.len(), 3);
        assert!(!s.is_empty());
        assert_eq!(s.text("hello world"), "llo");
    }

    #[test]
    fn span_empty() {
        let s = Span::new(3, 3);
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn span_cover() {
        let a = Span::new(2, 5);
        let b = Span::new(7, 10);
        assert_eq!(a.cover(b), Span::new(2, 10));
    }

    #[test]
    fn text_position_first_line() {
        let pos = TextPosition::from_offset("hello", 3);
        assert_eq!(pos, TextPosition { line: 1, column: 4 });
    }

    #[test]
    fn text_position_multiline() {
        let src = "abc\ndef\nghi";
        // 'd' is at byte 4, line 2 col 1
        assert_eq!(
            TextPosition::from_offset(src, 4),
            TextPosition { line: 2, column: 1 }
        );
        // 'h' is at byte 8, line 3 col 1
        assert_eq!(
            TextPosition::from_offset(src, 8),
            TextPosition { line: 3, column: 1 }
        );
        // 'i' is at byte 10, line 3 col 3
        assert_eq!(
            TextPosition::from_offset(src, 10),
            TextPosition { line: 3, column: 3 }
        );
    }
}
