//! The `outline` symbol tree (command-language spec, §6.2).

use std::ops::Range;

use crate::select::SourceFile;

/// The outline entries of the items in `f`, one per line, or of the items
/// strictly inside `within`. Empty if `f` has no items.
pub fn render(f: &SourceFile, within: Option<&Range<usize>>) -> String {
    let _ = (f, within);
    String::new()
}
