//! Spans and the parts they have (command-language spec, §3.4 and §3.8).

use std::ops::Range;

use crate::exec::ExecErrorKind as E;
use crate::script::ast::{Filter, Part};
use crate::select::part_name;
use crate::syntax::{self, Item};
use crate::text::full_lines;

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
        let plain = |range| Span { range, item: None };
        match (part, self.item) {
            (Part::Lines, _) => Ok(lines(text, self.range.clone()).map(plain).collect()),
            (Part::Refs | Part::Def, _) => unreachable!("resolved by the executor"),
            (part, None) => Err(E::PartNeedsItem {
                part: part_name(part).into(),
            }),
            (Part::Whole, Some(item)) => Ok(vec![plain(item.range.clone())]),
            (part, Some(item)) => syntax::part(item, part, text)
                .map(|range| vec![plain(range)])
                .ok_or_else(|| E::MissingPart {
                    item: syntax::selector(item.kind, &item.name),
                    part: part_name(part).into(),
                    has: self.parts(),
                }),
        }
    }

    /// The parts the span has, as `.body .sig ...`.
    pub fn parts(&self) -> String {
        let Some(item) = self.item else {
            return ".lines".into();
        };
        [
            item.body.is_some().then_some(".body"),
            Some(".sig"),
            item.params.is_some().then_some(".params"),
            Some(".name"),
            item.doc.is_some().then_some(".doc"),
            item.attrs.is_some().then_some(".attrs"),
            item.ret.is_some().then_some(".ret"),
            item.ty.is_some().then_some(".type"),
            item.value.is_some().then_some(".value"),
            Some(".whole"),
            Some(".lines"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ")
    }

    /// Whether `filter` holds for the span (§3.9).
    pub fn holds(&self, filter: &Filter, text: &str) -> Result<bool, E> {
        todo!()
    }
}

/// Each whole line `range` touches, with its line ending.
fn lines(text: &str, range: Range<usize>) -> impl Iterator<Item = Range<usize>> {
    let full = full_lines(text, range);
    let mut start = full.start;
    let mut pieces: Vec<Range<usize>> = text[full.clone()]
        .split_inclusive('\n')
        .map(|line| {
            start += line.len();
            start - line.len()..start
        })
        .collect();
    if pieces.is_empty() {
        pieces.push(full);
    }
    pieces.into_iter()
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
