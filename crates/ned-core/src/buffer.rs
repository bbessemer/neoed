//! Rope-backed text buffer with conversions between byte offsets, line
//! numbers, and points.
//!
//! Lines and columns are 0-based, and columns count bytes, matching
//! tree-sitter. Only `\n` and `\r\n` end a line. A trailing line ending does
//! not start a new line: `"a\nb\n"` has two lines.

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
        todo!()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BufferError {
    #[error("byte offset {offset} is past the end of the buffer ({len} bytes)")]
    ByteOutOfBounds { offset: usize, len: usize },
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
        todo!()
    }

    pub fn line_ending(&self) -> LineEnding {
        todo!()
    }

    pub fn len_bytes(&self) -> usize {
        todo!()
    }

    pub fn line_count(&self) -> usize {
        todo!()
    }

    /// The byte range of `line`, including its line ending.
    pub fn line_range(&self, line: usize) -> Result<Range<usize>, BufferError> {
        todo!()
    }

    /// The line containing `offset`. The end-of-buffer offset belongs to the
    /// last line.
    pub fn byte_to_line(&self, offset: usize) -> Result<usize, BufferError> {
        todo!()
    }

    pub fn byte_to_point(&self, offset: usize) -> Result<Point, BufferError> {
        todo!()
    }

    pub fn point_to_byte(&self, point: Point) -> Result<usize, BufferError> {
        todo!()
    }

    pub fn slice(&self, range: Range<usize>) -> Result<String, BufferError> {
        todo!()
    }

    pub fn text(&self) -> String {
        todo!()
    }
}
