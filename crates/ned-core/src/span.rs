//! Spans and the parts they have (command-language spec, §3.4 and §3.8).

use std::ops::Range;

use crate::exec::ExecErrorKind as E;
use crate::script::ast::Part;
use crate::syntax::Item;

/// A span of a file's text, and the item it is if a syntax step selected it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span<'a> {
    pub range: Range<usize>,
    pub item: Option<&'a Item>,
}

impl<'a> Span<'a> {
    /// The spans `part` selects in `text`, the span's file. A part's spans
    /// are plain spans, not items. `.refs` and `.def` are the executor's.
    pub fn part(&self, part: Part, text: &str) -> Result<Vec<Span<'a>>, E> {
        todo!()
    }

    /// The parts the span has, as `.body .sig ...`.
    pub fn parts(&self) -> String {
        todo!()
    }
}
