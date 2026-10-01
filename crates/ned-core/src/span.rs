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

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "fn a() {\n    x();\n}\nlast";

    fn lines(range: Range<usize>) -> Vec<&'static str> {
        Span { range, item: None }
            .part(Part::Lines, TEXT)
            .unwrap()
            .into_iter()
            .map(|s| &TEXT[s.range])
            .collect()
    }

    #[test]
    fn lines_split_a_multi_line_span() {
        assert_eq!(lines(4..12), ["fn a() {\n", "    x();\n"]);
        assert_eq!(lines(0..20), ["fn a() {\n", "    x();\n", "}\n"]);
    }

    #[test]
    fn lines_widen_a_single_line_span() {
        assert_eq!(lines(13..15), ["    x();\n"]);
        assert_eq!(lines(0..9), ["fn a() {\n"]);
        assert_eq!(lines(21..23), ["last"]);
    }

    #[test]
    fn plain_spans_have_only_lines() {
        let span = Span {
            range: 0..2,
            item: None,
        };
        assert_eq!(span.parts(), ".lines");
        assert!(matches!(
            span.part(Part::Body, TEXT),
            Err(E::PartNeedsItem { part }) if part == "body"
        ));
    }
}
