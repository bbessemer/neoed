//! Edits collected against a buffer's original contents and applied in one
//! pass (the snapshot semantics of the command-language spec, §2.3).

use std::ops::Range;

use crate::buffer::{Buffer, BufferError};

/// Replaces `range` of the original buffer with `text`. An empty range is an
/// insertion. `command` is the index of the script command that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub range: Range<usize>,
    pub text: String,
    pub command: usize,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EditError {
    #[error(transparent)]
    Buffer(#[from] BufferError),
    #[error("edit range {}..{} is reversed", .0.start, .0.end)]
    Reversed(Range<usize>),
    /// `range` is the span of the earlier edit, from command `first`.
    #[error("edit from command {second} overlaps command {first}")]
    Overlap {
        first: usize,
        second: usize,
        range: Range<usize>,
    },
}

#[derive(Debug)]
pub struct EditSet<'a> {
    buffer: &'a Buffer,
    edits: Vec<Edit>,
}

impl<'a> EditSet<'a> {
    pub fn new(buffer: &'a Buffer) -> Self {
        todo!()
    }

    /// Adds an edit, rejecting it if it is out of bounds, splits a
    /// character, or overlaps an edit already in the set.
    pub fn push(&mut self, edit: Edit) -> Result<(), EditError> {
        todo!()
    }

    pub fn is_empty(&self) -> bool {
        todo!()
    }

    pub fn len(&self) -> usize {
        todo!()
    }

    /// The buffer's text with every edit applied. Inserted `\n` line endings
    /// are converted to the buffer's line ending.
    pub fn apply(&self) -> String {
        todo!()
    }
}
