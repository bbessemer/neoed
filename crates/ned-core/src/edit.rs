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

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(range: Range<usize>, text: &str, command: usize) -> Edit {
        Edit {
            range,
            text: text.to_string(),
            command,
        }
    }

    fn apply(src: &str, edits: &[Edit]) -> Result<String, EditError> {
        let buf = Buffer::new(src);
        let mut set = EditSet::new(&buf);
        for e in edits {
            set.push(e.clone())?;
        }
        Ok(set.apply())
    }

    #[test]
    fn empty_set_returns_original() {
        let buf = Buffer::new("abc\n");
        let set = EditSet::new(&buf);
        assert!(set.is_empty());
        assert_eq!(set.len(), 0);
        assert_eq!(set.apply(), "abc\n");
    }

    #[test]
    fn replace_delete_and_insert() {
        assert_eq!(
            apply("hello world", &[edit(6..11, "ned", 0)]).unwrap(),
            "hello ned"
        );
        assert_eq!(
            apply("hello world", &[edit(5..11, "", 0)]).unwrap(),
            "hello"
        );
        assert_eq!(
            apply("hello world", &[edit(5..5, ",", 0)]).unwrap(),
            "hello, world"
        );
    }

    #[test]
    fn edits_use_original_coordinates_in_any_order() {
        let out = apply(
            "one two three",
            &[edit(8..13, "3", 0), edit(0..3, "uno", 1), edit(4..7, "", 2)],
        )
        .unwrap();
        assert_eq!(out, "uno  3");
    }

    #[test]
    fn overlapping_replacements_are_rejected() {
        assert_eq!(
            apply("abcdefgh", &[edit(2..5, "x", 0), edit(4..6, "y", 1)]),
            Err(EditError::Overlap {
                first: 0,
                second: 1,
                range: 2..5
            })
        );
        assert_eq!(
            apply("abcdefgh", &[edit(4..6, "y", 3), edit(2..5, "x", 7)]),
            Err(EditError::Overlap {
                first: 3,
                second: 7,
                range: 4..6
            })
        );
    }

    #[test]
    fn identical_and_nested_ranges_overlap() {
        assert!(matches!(
            apply("abcdef", &[edit(1..3, "x", 0), edit(1..3, "y", 1)]),
            Err(EditError::Overlap { .. })
        ));
        assert!(matches!(
            apply("abcdef", &[edit(0..6, "x", 0), edit(2..3, "y", 1)]),
            Err(EditError::Overlap { .. })
        ));
    }

    #[test]
    fn insertion_inside_a_replaced_span_overlaps() {
        assert_eq!(
            apply("abcdef", &[edit(1..4, "x", 0), edit(2..2, "y", 1)]),
            Err(EditError::Overlap {
                first: 0,
                second: 1,
                range: 1..4
            })
        );
        assert_eq!(
            apply("abcdef", &[edit(2..2, "y", 0), edit(1..4, "x", 1)]),
            Err(EditError::Overlap {
                first: 0,
                second: 1,
                range: 2..2
            })
        );
    }

    #[test]
    fn adjacent_replacements_are_allowed() {
        let out = apply("abcdef", &[edit(0..2, "X", 0), edit(2..4, "Y", 1)]).unwrap();
        assert_eq!(out, "XYef");
    }

    #[test]
    fn insertions_at_span_boundaries_are_allowed() {
        let out = apply(
            "abcdef",
            &[edit(2..4, "X", 0), edit(2..2, "<", 1), edit(4..4, ">", 2)],
        )
        .unwrap();
        assert_eq!(out, "ab<X>ef");
    }

    #[test]
    fn insertions_at_one_point_apply_in_command_order() {
        let out = apply(
            "ab",
            &[edit(1..1, "3", 3), edit(1..1, "1", 1), edit(1..1, "2", 2)],
        )
        .unwrap();
        assert_eq!(out, "a123b");
    }

    #[test]
    fn insertions_from_one_command_keep_push_order() {
        let out = apply("ab", &[edit(1..1, "x", 0), edit(1..1, "y", 0)]).unwrap();
        assert_eq!(out, "axyb");
    }

    #[test]
    fn rejects_out_of_bounds_split_and_reversed_ranges() {
        assert_eq!(
            apply("abc", &[edit(2..4, "", 0)]),
            Err(EditError::Buffer(BufferError::ByteOutOfBounds {
                offset: 4,
                len: 3
            }))
        );
        assert_eq!(
            apply("é", &[edit(1..1, "x", 0)]),
            Err(EditError::Buffer(BufferError::NotCharBoundary(1)))
        );
        assert_eq!(
            apply("abc", &[edit(Range { start: 2, end: 1 }, "", 0)]),
            Err(EditError::Buffer(BufferError::Reversed(Range {
                start: 2,
                end: 1
            })))
        );
    }

    #[test]
    fn rejected_edit_is_not_added() {
        let buf = Buffer::new("abcdef");
        let mut set = EditSet::new(&buf);
        set.push(edit(0..2, "X", 0)).unwrap();
        assert!(set.push(edit(1..3, "Y", 1)).is_err());
        assert!(set.push(edit(9..9, "Z", 2)).is_err());
        assert_eq!(set.len(), 1);
        assert!(!set.is_empty());
        assert_eq!(set.apply(), "Xcdef");
    }

    #[test]
    fn inserted_text_follows_crlf_buffers() {
        let out = apply("a\r\nb\r\n", &[edit(3..3, "x\ny\n", 0)]).unwrap();
        assert_eq!(out, "a\r\nx\r\ny\r\nb\r\n");
        let already = apply("a\r\nb\r\n", &[edit(3..3, "x\r\n", 0)]).unwrap();
        assert_eq!(already, "a\r\nx\r\nb\r\n");
    }

    #[test]
    fn inserted_text_is_untouched_in_lf_buffers() {
        let out = apply("a\nb\n", &[edit(2..2, "x\r\ny\n", 0)]).unwrap();
        assert_eq!(out, "a\nx\r\ny\nb\n");
    }
}
