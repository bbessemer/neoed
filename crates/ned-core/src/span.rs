//! Spans and the parts they have (command-language spec, §3.4 and §3.8).

use std::ops::Range;

use crate::conflict::{Conflict, Side};
use crate::exec::{ExecError, ExecErrorKind as E};
use crate::script::ast::{Filter, LineNo, Op, Part, Value};
use crate::select::part_name;
use crate::syntax::{self, Item};
use crate::text::full_lines;

/// A span of a file's text, and what it is, which decides its parts (§3.8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span<'a> {
    pub range: Range<usize>,
    pub of: Of<'a>,
}

/// What a span is beyond its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Of<'a> {
    Plain,
    Item(&'a Item),
    /// The file's `N`th conflict (§3.11).
    Conflict(usize, &'a Conflict),
    /// A side of a conflict; an empty one has no lines.
    Side,
}

impl<'a> Span<'a> {
    /// The spans `part` selects in `text`, the span's file. A part's spans
    /// are plain spans, not items. `.refs` and `.def` are the executor's.
    pub fn part(&self, part: Part, text: &str) -> Result<Vec<Span<'a>>, ExecError> {
        let plain = |range| Span {
            range,
            of: Of::Plain,
        };
        match (part, self.of) {
            (Part::Lines, Of::Side) if self.range.is_empty() => Ok(Vec::new()),
            (Part::Lines, _) => Ok(lines(text, self.range.clone()).map(plain).collect()),
            (Part::Line(n), _) => {
                let mut lines = self.part(Part::Lines, text)?;
                let index = match n {
                    LineNo::Number(n) => n - 1,
                    LineNo::Last => lines.len().wrapping_sub(1),
                };
                // A span too short for the line is skipped (§3.4).
                Ok(match index < lines.len() {
                    true => vec![lines.swap_remove(index)],
                    false => Vec::new(),
                })
            }
            (Part::Refs | Part::Def, _) => unreachable!("resolved by the executor"),
            (Part::Ours | Part::Theirs | Part::Base, Of::Conflict(n, conflict)) => {
                let side = match part {
                    Part::Ours => Side::Ours,
                    Part::Base => Side::Base,
                    _ => Side::Theirs,
                };
                conflict
                    .side(side)
                    .map(|range| {
                        vec![Span {
                            range,
                            of: Of::Side,
                        }]
                    })
                    .ok_or_else(|| missing_base(n))
            }
            (Part::Ours | Part::Theirs | Part::Base, _) => {
                let part = part_name(part).into();
                Err(E::PartNeedsConflict { part }.into())
            }
            (part, Of::Plain | Of::Side | Of::Conflict(..)) => {
                let part = part_name(part).into();
                Err(E::PartNeedsItem { part }.into())
            }
            (Part::Whole, Of::Item(item)) => Ok(vec![plain(item.range.clone())]),
            (part, Of::Item(item)) => syntax::part(item, part, text)
                .map(|range| vec![plain(range)])
                .ok_or_else(|| {
                    let kind = E::MissingPart {
                        item: syntax::selector(item.kind, &item.name),
                        part: part_name(part).into(),
                    };
                    ExecError::new(kind).with_fix(format!("it has {}", self.parts()))
                }),
        }
    }

    /// The parts the span has, as `.body .sig ...`.
    pub fn parts(&self) -> String {
        let item = match self.of {
            Of::Plain | Of::Side => return ".lines".into(),
            Of::Conflict(_, conflict) => {
                return match conflict.base {
                    Some(_) => ".ours .base .theirs .lines",
                    None => ".ours .theirs .lines",
                }
                .into();
            }
            Of::Item(item) => item,
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
    pub fn passes(
        &self,
        filters: &[(Filter, Range<usize>)],
        text: &str,
    ) -> Result<bool, ExecError> {
        self.all_hold(filters.iter().map(|(filter, _)| filter), text)
    }

    /// Whether `filter` holds for the span (§3.9).
    pub fn holds(&self, filter: &Filter, text: &str) -> Result<bool, ExecError> {
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
                Err(e) if matches!(e.kind, E::MissingPart { .. }) => None,
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
            (false, Op::Match, Value::Regex(pattern)) => pattern.regex().is_match(shown),
            _ => unreachable!("checked when parsed"),
        })
    }

    fn all_hold<'f>(
        &self,
        filters: impl IntoIterator<Item = &'f Filter>,
        text: &str,
    ) -> Result<bool, ExecError> {
        for filter in filters {
            if !self.holds(filter, text)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// The error for the `.base` of conflict `n`, which git didn't write in
/// diff3 style (§3.11).
pub fn missing_base(n: usize) -> ExecError {
    let kind = E::MissingPart {
        item: format!("conflict:{n}"),
        part: "base".into(),
    };
    ExecError::new(kind).with_fix(
        "it has .ours .theirs .lines; `git checkout --conflict=diff3 -- FILE` (or zdiff3) \
         rewrites the file's conflicts with a base, undoing its edits since the merge",
    )
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
        Span {
            range,
            of: Of::Plain,
        }
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

    fn line(range: Range<usize>, n: LineNo) -> Result<Vec<&'static str>, ExecError> {
        Ok(Span {
            range,
            of: Of::Plain,
        }
        .part(Part::Line(n), TEXT)?
        .into_iter()
        .map(|s| &TEXT[s.range])
        .collect())
    }

    #[test]
    fn line_picks_one_line() {
        assert_eq!(line(0..20, LineNo::Number(1)), Ok(vec!["fn a() {\n"]));
        assert_eq!(line(0..20, LineNo::Number(2)), Ok(vec!["    x();\n"]));
        assert_eq!(line(0..20, LineNo::Last), Ok(vec!["}\n"]));
        assert_eq!(line(13..15, LineNo::Last), Ok(vec!["    x();\n"]));
        assert_eq!(line(0..20, LineNo::Number(4)), Ok(vec![]));
    }

    #[test]
    fn an_empty_side_has_no_line() {
        let side = Span {
            range: 9..9,
            of: Of::Side,
        };
        assert_eq!(side.part(Part::Line(LineNo::Last), TEXT), Ok(vec![]));
    }

    #[test]
    fn plain_spans_have_only_lines() {
        let span = Span {
            range: 0..2,
            of: Of::Plain,
        };
        assert_eq!(span.parts(), ".lines");
        assert!(matches!(
            span.part(Part::Body, TEXT),
            Err(ExecError { kind: E::PartNeedsItem { part }, .. }) if part == "body"
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
        Span {
            range,
            of: Of::Plain,
        }
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
            of: Of::Plain,
        };
        assert!(matches!(
            span.holds(&filter(r#"[.name == ""]"#), TEXT),
            Err(ExecError { kind: E::PartNeedsItem { part }, .. }) if part == "name"
        ));
    }
}
