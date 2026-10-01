//! Spans and the parts they have (command-language spec, §3.4 and §3.8).

use std::ops::Range;

use crate::exec::ExecErrorKind as E;
use crate::script::ast::{Filter, Op, Part, Value};
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

    /// Whether every one of a step's `filters` holds for the span.
    pub fn passes(&self, filters: &[(Filter, Range<usize>)], text: &str) -> Result<bool, E> {
        self.all_hold(filters.iter().map(|(filter, _)| filter), text)
    }

    /// Whether `filter` holds for the span (§3.9).
    pub fn holds(&self, filter: &Filter, text: &str) -> Result<bool, E> {
        let (property, op, value) = match filter {
            Filter::Or(any) => {
                for filter in any {
                    if self.holds(filter, text)? {
                        return Ok(true);
                    }
                }
                return Ok(false);
            }
            Filter::And(all) => return self.all_hold(all, text),
            Filter::Cond {
                property,
                op,
                value,
            } => (property, op, value),
        };
        // A part the item doesn't have is empty.
        let range = match property.part {
            None => Some(self.range.clone()),
            Some(part) => match self.part(part, text) {
                Ok(spans) => spans.into_iter().next().map(|s| s.range),
                Err(E::MissingPart { .. }) => None,
                Err(e) => return Err(e),
            },
        };
        let shown = range.map_or("", |r| without_break(&text[r]));
        Ok(match (property.len, op, value) {
            (true, op, Value::Number(n)) => {
                let len = match shown.contains('\n') {
                    true => shown.lines().count(),
                    false => shown.chars().count(),
                };
                match op {
                    Op::Eq => len == *n,
                    Op::Ne => len != *n,
                    Op::Lt => len < *n,
                    Op::Gt => len > *n,
                    Op::Le => len <= *n,
                    Op::Ge => len >= *n,
                    Op::Match => unreachable!("checked when parsed"),
                }
            }
            (false, Op::Eq, Value::Str(s)) => shown == s,
            (false, Op::Ne, Value::Str(s)) => shown != s,
            (false, Op::Match, Value::Regex(pattern)) => pattern
                .regex()
                .expect("validated when parsed")
                .is_match(shown),
            _ => unreachable!("checked when parsed"),
        })
    }

    fn all_hold<'f>(
        &self,
        filters: impl IntoIterator<Item = &'f Filter>,
        text: &str,
    ) -> Result<bool, E> {
        for filter in filters {
            if !self.holds(filter, text)? {
                return Ok(false);
            }
        }
        Ok(true)
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

/// `text` without a final line break.
fn without_break(text: &str) -> &str {
    let text = text.strip_suffix('\n').unwrap_or(text);
    text.strip_suffix('\r').unwrap_or(text)
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

    /// The first filter of `show /x/FILTERS`.
    fn filter(filters: &str) -> Filter {
        let script = format!("show /x/{filters}");
        let parsed = crate::script::parse(&script).unwrap();
        let crate::script::ast::CommandKind::Show {
            target: Some(target),
            ..
        } = &parsed.commands[0].kind
        else {
            panic!("{script}");
        };
        target.selector.steps[0].filters[0].0.clone()
    }

    fn holds(range: Range<usize>, filters: &str) -> bool {
        Span { range, item: None }
            .holds(&filter(filters), TEXT)
            .unwrap()
    }

    #[test]
    fn text_leaves_out_the_final_line_break() {
        assert!(holds(9..18, r#"[.text == "    x();"]"#));
        assert!(holds(9..18, r#"[.text ~= /x\(\);$/]"#));
        assert!(holds(9..18, r#"[.text != "x"]"#));
        assert!(!holds(9..18, r#"[.text ~= /^x/]"#));
    }

    #[test]
    fn len_counts_characters_on_one_line_and_lines_on_several() {
        assert!(holds(9..18, "[.len == 8]"));
        assert!(holds(0..9, "[.len == 8]"));
        assert!(holds(0..20, "[.len == 3]"));
        assert!(holds(4..12, "[.len == 2]"));
        assert!(holds(20..24, "[.len < 5]"));
        assert!(holds(20..24, "[.len >= 4 && .len <= 4]"));
        assert!(!holds(20..24, "[.len > 4]"));
    }

    #[test]
    fn and_and_or_combine_conditions() {
        assert!(holds(20..24, r#"[.len > 9 || .text == "last"]"#));
        assert!(!holds(20..24, r#"[.len > 9 && .text == "last"]"#));
        assert!(holds(20..24, r#"[(.len > 9 || .len < 5) && .text ~= /l/]"#));
    }

    #[test]
    fn parts_in_a_filter_need_an_item() {
        let span = Span {
            range: 0..2,
            item: None,
        };
        assert!(matches!(
            span.holds(&filter(r#"[.name == ""]"#), TEXT),
            Err(E::PartNeedsItem { part }) if part == "name"
        ));
    }
}
