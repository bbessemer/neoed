//! Rope-backed text buffer with conversions between byte offsets, line
//! numbers, and points.
//!
//! Lines and columns are 0-based, and columns count bytes, matching
//! tree-sitter. Only `\n` and `\r\n` end a line. A trailing line ending does
//! not start a new line: `"a\nb\n"` has two lines, and its end offset is at
//! line 2, column 0, as tree-sitter reports it.

use std::ops::Range;

use ropey::Rope;

/// A position as a 0-based line and a byte column within that line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Point {
    pub line: usize,
    pub column: usize,
}

/// The line-ending convention of a file, detected from its first line ending.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineEnding {
    #[default]
    Lf,
    Crlf,
}

impl LineEnding {
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::Crlf => "\r\n",
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BufferError {
    #[error("byte offset {offset} is past the end of the buffer ({len} bytes)")]
    ByteOutOfBounds { offset: usize, len: usize },
    #[error("byte range {}..{} is reversed", .0.start, .0.end)]
    Reversed(Range<usize>),
    #[error("byte offset {0} is not on a character boundary")]
    NotCharBoundary(usize),
    #[error("line {line} is past the end of the buffer ({count} lines)")]
    LineOutOfBounds { line: usize, count: usize },
    #[error("column {column} is past the end of line {line}")]
    ColumnOutOfBounds { line: usize, column: usize },
}

#[derive(Debug, Clone)]
pub struct Buffer {
    rope: Rope,
    line_ending: LineEnding,
}

impl Buffer {
    pub fn new(text: &str) -> Self {
        let line_ending = match text.find('\n') {
            Some(i) if text[..i].ends_with('\r') => LineEnding::Crlf,
            _ => LineEnding::Lf,
        };
        Buffer {
            rope: Rope::from_str(text),
            line_ending,
        }
    }

    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    pub fn line_count(&self) -> usize {
        if self.len_bytes() == 0 {
            0
        } else if self.ends_with_newline() {
            self.rope.len_lines() - 1
        } else {
            self.rope.len_lines()
        }
    }

    /// The byte range of `line`, including its line ending.
    pub fn line_range(&self, line: usize) -> Result<Range<usize>, BufferError> {
        let count = self.line_count();
        if line >= count {
            return Err(BufferError::LineOutOfBounds { line, count });
        }
        Ok(self.rope.line_to_byte(line)..self.rope.line_to_byte(line + 1))
    }

    pub fn byte_to_line(&self, offset: usize) -> Result<usize, BufferError> {
        self.check_offset(offset)?;
        Ok(self.rope.byte_to_line(offset))
    }

    pub fn byte_to_point(&self, offset: usize) -> Result<Point, BufferError> {
        let line = self.byte_to_line(offset)?;
        Ok(Point {
            line,
            column: offset - self.rope.line_to_byte(line),
        })
    }

    pub fn point_to_byte(&self, point: Point) -> Result<usize, BufferError> {
        let Point { line, column } = point;
        let count = self.line_count();
        // The last valid column is the line's `\n` itself (or its end, for an
        // unterminated last line).
        let (start, last) = if line < count {
            let range = self.line_range(line)?;
            let has_newline = self.rope.byte(range.end - 1) == b'\n';
            (range.start, range.end - usize::from(has_newline))
        } else if line == count && (count == 0 || self.ends_with_newline()) {
            (self.len_bytes(), self.len_bytes())
        } else {
            return Err(BufferError::LineOutOfBounds { line, count });
        };
        if column > last - start {
            return Err(BufferError::ColumnOutOfBounds { line, column });
        }
        self.check_offset(start + column)?;
        Ok(start + column)
    }

    pub fn slice(&self, range: Range<usize>) -> Result<String, BufferError> {
        self.check_range(&range)?;
        Ok(self.rope.byte_slice(range).to_string())
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    /// Checks that `range` is ordered, in bounds, and on character boundaries.
    pub fn check_range(&self, range: &Range<usize>) -> Result<(), BufferError> {
        if range.start > range.end {
            return Err(BufferError::Reversed(range.clone()));
        }
        self.check_offset(range.start)?;
        self.check_offset(range.end)
    }

    fn check_offset(&self, offset: usize) -> Result<(), BufferError> {
        let len = self.len_bytes();
        if offset > len {
            return Err(BufferError::ByteOutOfBounds { offset, len });
        }
        if self.rope.char_to_byte(self.rope.byte_to_char(offset)) != offset {
            return Err(BufferError::NotCharBoundary(offset));
        }
        Ok(())
    }

    fn ends_with_newline(&self) -> bool {
        self.len_bytes() > 0 && self.rope.byte(self.len_bytes() - 1) == b'\n'
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(line: usize, column: usize) -> Point {
        Point { line, column }
    }

    #[test]
    fn detects_line_ending_from_first_line_break() {
        assert_eq!(Buffer::new("a\nb\r\n").line_ending(), LineEnding::Lf);
        assert_eq!(Buffer::new("a\r\nb\n").line_ending(), LineEnding::Crlf);
        assert_eq!(Buffer::new("no breaks").line_ending(), LineEnding::Lf);
        assert_eq!(Buffer::new("").line_ending(), LineEnding::Lf);
    }

    #[test]
    fn line_ending_strings() {
        assert_eq!(LineEnding::Lf.as_str(), "\n");
        assert_eq!(LineEnding::Crlf.as_str(), "\r\n");
    }

    #[test]
    fn trailing_line_ending_does_not_start_a_line() {
        assert_eq!(Buffer::new("").line_count(), 0);
        assert_eq!(Buffer::new("a").line_count(), 1);
        assert_eq!(Buffer::new("a\n").line_count(), 1);
        assert_eq!(Buffer::new("\n").line_count(), 1);
        assert_eq!(Buffer::new("a\nb").line_count(), 2);
        assert_eq!(Buffer::new("a\nb\n").line_count(), 2);
        assert_eq!(Buffer::new("a\r\nb\r\n").line_count(), 2);
        assert_eq!(Buffer::new("a\n\n").line_count(), 2);
    }

    #[test]
    fn only_lf_and_crlf_break_lines() {
        let buf = Buffer::new("a\rb\u{2028}c\u{85}d\n");
        assert_eq!(buf.line_count(), 1);
        assert_eq!(buf.byte_to_point(buf.len_bytes() - 1), Ok(pt(0, 10)));
    }

    #[test]
    fn line_range_includes_line_ending() {
        let buf = Buffer::new("ab\r\ncd\nef");
        assert_eq!(buf.line_range(0), Ok(0..4));
        assert_eq!(buf.line_range(1), Ok(4..7));
        assert_eq!(buf.line_range(2), Ok(7..9));
        assert_eq!(
            buf.line_range(3),
            Err(BufferError::LineOutOfBounds { line: 3, count: 3 })
        );
    }

    #[test]
    fn byte_to_line_and_point() {
        let buf = Buffer::new("ab\ncd\n");
        assert_eq!(buf.byte_to_line(0), Ok(0));
        assert_eq!(buf.byte_to_line(2), Ok(0));
        assert_eq!(buf.byte_to_line(3), Ok(1));
        assert_eq!(buf.byte_to_point(4), Ok(pt(1, 1)));
        assert_eq!(buf.byte_to_point(2), Ok(pt(0, 2)));
    }

    #[test]
    fn end_offset_matches_tree_sitter() {
        let trailing = Buffer::new("ab\ncd\n");
        assert_eq!(trailing.byte_to_point(6), Ok(pt(2, 0)));
        assert_eq!(trailing.byte_to_line(6), Ok(2));
        let unterminated = Buffer::new("ab\ncd");
        assert_eq!(unterminated.byte_to_point(5), Ok(pt(1, 2)));
        assert_eq!(Buffer::new("").byte_to_point(0), Ok(pt(0, 0)));
    }

    #[test]
    fn byte_conversions_reject_bad_offsets() {
        let buf = Buffer::new("aé\n");
        assert_eq!(
            buf.byte_to_point(5),
            Err(BufferError::ByteOutOfBounds { offset: 5, len: 4 })
        );
        assert_eq!(
            buf.byte_to_line(5),
            Err(BufferError::ByteOutOfBounds { offset: 5, len: 4 })
        );
        assert_eq!(buf.byte_to_point(2), Err(BufferError::NotCharBoundary(2)));
        assert_eq!(buf.byte_to_line(2), Err(BufferError::NotCharBoundary(2)));
        assert_eq!(buf.byte_to_point(3), Ok(pt(0, 3)));
    }

    #[test]
    fn point_to_byte_inverts_byte_to_point() {
        let buf = Buffer::new("aé\r\nxyz\n\nq");
        for offset in 0..=buf.len_bytes() {
            if let Ok(point) = buf.byte_to_point(offset) {
                assert_eq!(buf.point_to_byte(point), Ok(offset), "{point:?}");
            }
        }
    }

    #[test]
    fn point_to_byte_rejects_bad_points() {
        let buf = Buffer::new("aé\nxy");
        assert_eq!(
            buf.point_to_byte(pt(0, 2)),
            Err(BufferError::NotCharBoundary(2))
        );
        assert_eq!(
            buf.point_to_byte(pt(0, 5)),
            Err(BufferError::ColumnOutOfBounds { line: 0, column: 5 })
        );
        assert_eq!(
            buf.point_to_byte(pt(1, 3)),
            Err(BufferError::ColumnOutOfBounds { line: 1, column: 3 })
        );
        assert_eq!(
            buf.point_to_byte(pt(2, 0)),
            Err(BufferError::LineOutOfBounds { line: 2, count: 2 })
        );
    }

    #[test]
    fn point_after_trailing_line_ending_is_end_of_buffer() {
        let buf = Buffer::new("ab\n");
        assert_eq!(buf.point_to_byte(pt(1, 0)), Ok(3));
        assert_eq!(
            buf.point_to_byte(pt(1, 1)),
            Err(BufferError::ColumnOutOfBounds { line: 1, column: 1 })
        );
        assert_eq!(Buffer::new("").point_to_byte(pt(0, 0)), Ok(0));
    }

    #[test]
    fn slice_returns_exact_bytes() {
        let buf = Buffer::new("aé\r\nxy");
        assert_eq!(buf.slice(1..5).as_deref(), Ok("é\r\n"));
        assert_eq!(buf.slice(3..3).as_deref(), Ok(""));
        assert_eq!(buf.slice(0..buf.len_bytes()).as_deref(), Ok("aé\r\nxy"));
    }

    #[test]
    fn slice_rejects_bad_ranges() {
        let buf = Buffer::new("aé\nxy");
        assert_eq!(
            buf.slice(4..7),
            Err(BufferError::ByteOutOfBounds { offset: 7, len: 6 })
        );
        assert_eq!(buf.slice(0..2), Err(BufferError::NotCharBoundary(2)));
        assert_eq!(
            buf.slice(Range { start: 3, end: 1 }),
            Err(BufferError::Reversed(Range { start: 3, end: 1 }))
        );
    }

    #[test]
    fn text_round_trips_bytes() {
        let src = "fn a() {}\r\n\r\n\tb\u{2028}\n";
        let buf = Buffer::new(src);
        assert_eq!(buf.text(), src);
        assert_eq!(buf.len_bytes(), src.len());
    }
}
