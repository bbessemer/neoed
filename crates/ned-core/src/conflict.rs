//! Merge conflicts that git writes into a file (§3.3).

use std::borrow::Cow;
use std::ops::Range;

/// One conflict: the byte ranges of its marker lines, each with its line
/// break.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    /// The `<<<<<<<` line, before ours.
    pub start: Range<usize>,
    /// The `|||||||` line before the base, in diff3 style.
    pub base: Option<Range<usize>>,
    /// The `=======` line, before theirs.
    pub split: Range<usize>,
    /// The `>>>>>>>` line.
    pub end: Range<usize>,
}

/// A side of a conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Ours,
    Base,
    Theirs,
}

impl Side {
    const ALL: [Side; 3] = [Side::Ours, Side::Base, Side::Theirs];

    /// The side's name, as its part is written: `ours` for `.ours`.
    pub fn name(self) -> &'static str {
        match self {
            Side::Ours => "ours",
            Side::Base => "base",
            Side::Theirs => "theirs",
        }
    }
}

impl Conflict {
    fn markers(&self) -> impl Iterator<Item = &Range<usize>> {
        [
            Some(&self.start),
            self.base.as_ref(),
            Some(&self.split),
            Some(&self.end),
        ]
        .into_iter()
        .flatten()
    }

    /// The whole lines of the conflict, markers included.
    pub fn span(&self) -> Range<usize> {
        self.start.start..self.end.end
    }

    /// The lines of `side`, between its markers; `None` for the base of a
    /// conflict that isn't in diff3 style.
    pub fn side(&self, side: Side) -> Option<Range<usize>> {
        Some(match side {
            Side::Ours => self.start.end..self.base.as_ref().unwrap_or(&self.split).start,
            Side::Base => self.base.as_ref()?.end..self.split.start,
            Side::Theirs => self.split.end..self.end.start,
        })
    }

    /// The lines of each side, in file order.
    pub fn sides(&self) -> impl Iterator<Item = Range<usize>> {
        Side::ALL.into_iter().filter_map(|side| self.side(side))
    }

    /// The side whose lines `range` is.
    pub fn side_at(&self, range: &Range<usize>) -> Option<Side> {
        Side::ALL
            .into_iter()
            .find(|&side| self.side(side).as_ref() == Some(range))
    }
}

#[derive(Clone, Copy)]
enum Marker {
    Start,
    Base,
    Split,
    End,
}

/// The marker `line` is, if any: seven marker characters, then a space or the
/// end of the line, except that `=======` stands alone.
fn marker(line: &str) -> Option<Marker> {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let line = line.strip_suffix('\r').unwrap_or(line);
    if line == "=======" {
        return Some(Marker::Split);
    }
    let (marker, rest) = line.split_at_checked(7)?;
    if !(rest.is_empty() || rest.starts_with(' ')) {
        return None;
    }
    match marker {
        "<<<<<<<" => Some(Marker::Start),
        "|||||||" => Some(Marker::Base),
        ">>>>>>>" => Some(Marker::End),
        _ => None,
    }
}

/// The well-formed conflicts of `text`, in order.
pub fn conflicts(text: &str) -> Vec<Conflict> {
    let mut found = Vec::new();
    if !text.contains("<<<<<<<") {
        return found;
    }
    let (mut start, mut base, mut split) = (None, None, None);
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let range = at..at + line.len();
        at = range.end;
        let Some(marker) = marker(line) else {
            continue;
        };
        match (marker, start.is_some(), split.is_some()) {
            (Marker::Start, ..) => {
                start = Some(range);
                (base, split) = (None, None);
            }
            (Marker::Base, true, false) if base.is_none() => base = Some(range),
            (Marker::Split, true, false) => split = Some(range),
            (Marker::End, true, true) => found.push(Conflict {
                start: start.take().expect("matched as started"),
                base: base.take(),
                split: split.take().expect("matched as split"),
                end: range,
            }),
            // A marker out of order ends the conflict it was in.
            _ => (start, base, split) = (None, None, None),
        }
    }
    found
}

/// `text` with the marker lines of `found`, its conflicts, blanked to spaces,
/// so a parse sees the sides one after the other at their own offsets.
pub fn mask<'t>(text: &'t str, found: &[Conflict]) -> Cow<'t, str> {
    if found.is_empty() {
        return Cow::Borrowed(text);
    }
    let mut bytes = text.as_bytes().to_vec();
    for range in found.iter().flat_map(Conflict::markers) {
        for b in &mut bytes[range.clone()] {
            if !matches!(*b, b'\r' | b'\n') {
                *b = b' ';
            }
        }
    }
    // Every byte of a multi-byte character becomes a space, so the text stays
    // UTF-8.
    Cow::Owned(String::from_utf8(bytes).expect("masking keeps UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The text of each marker line of the only conflict in `text`.
    fn markers(text: &str) -> Vec<&str> {
        let found = conflicts(text);
        assert_eq!(found.len(), 1, "{found:?}");
        found[0].markers().map(|r| &text[r.clone()]).collect()
    }

    #[test]
    fn finds_a_conflict() {
        let text = "a\n<<<<<<< HEAD\nb\n=======\nc\n>>>>>>> topic\nd\n";
        assert_eq!(
            conflicts(text),
            vec![Conflict {
                start: 2..15,
                base: None,
                split: 17..25,
                end: 27..41,
            }]
        );
    }

    #[test]
    fn finds_a_diff3_base() {
        let text = "<<<<<<< ours\nb\n||||||| base\na\n=======\nc\n>>>>>>> theirs\n";
        assert_eq!(
            markers(text),
            [
                "<<<<<<< ours\n",
                "||||||| base\n",
                "=======\n",
                ">>>>>>> theirs\n"
            ]
        );
    }

    #[test]
    fn sides_may_be_empty() {
        let text = "<<<<<<< HEAD\n=======\n>>>>>>> topic\n";
        assert_eq!(
            markers(text),
            ["<<<<<<< HEAD\n", "=======\n", ">>>>>>> topic\n"]
        );
    }

    #[test]
    fn finds_every_conflict_in_order() {
        let one = "<<<<<<< HEAD\nb\n=======\nc\n>>>>>>> topic\n";
        let text = format!("{one}x\n{one}");
        let found = conflicts(&text);
        assert_eq!(found.len(), 2);
        assert_eq!(found[1].start.start, one.len() + 2);
    }

    #[test]
    fn markers_end_with_a_space_or_the_line() {
        let text = "<<<<<<<\nb\n=======\nc\n>>>>>>>\n";
        assert_eq!(markers(text).len(), 3);
        for text in [
            "<<<<<<<< HEAD\nb\n=======\nc\n>>>>>>> topic\n",
            "<<<<<<<HEAD\nb\n=======\nc\n>>>>>>> topic\n",
            "<<<<<<< HEAD\nb\n======= x\nc\n>>>>>>> topic\n",
            "<<<<<<< HEAD\nb\n========\nc\n>>>>>>> topic\n",
            " <<<<<<< HEAD\nb\n=======\nc\n>>>>>>> topic\n",
        ] {
            assert_eq!(conflicts(text), vec![], "{text:?}");
        }
    }

    #[test]
    fn stray_markers_are_not_conflicts() {
        for text in [
            "Title\n=======\n",
            "<<<<<<< HEAD\nb\n",
            "<<<<<<< HEAD\nb\n=======\nc\n",
            ">>>>>>> topic\n",
            "<<<<<<< HEAD\nb\n>>>>>>> topic\n",
            "=======\n<<<<<<< HEAD\n>>>>>>> topic\n",
            "<<<<<<< HEAD\n=======\n||||||| base\n>>>>>>> topic\n",
        ] {
            assert_eq!(conflicts(text), vec![], "{text:?}");
        }
    }

    #[test]
    fn a_later_start_restarts_the_conflict() {
        let text = "<<<<<<< a\nx\n<<<<<<< b\ny\n=======\nz\n>>>>>>> c\n";
        assert_eq!(markers(text)[0], "<<<<<<< b\n");
    }

    #[test]
    fn reads_crlf_lines_and_a_last_line_without_a_break() {
        let text = "<<<<<<< HEAD\r\nb\r\n=======\r\nc\r\n>>>>>>> topic";
        assert_eq!(
            markers(text),
            ["<<<<<<< HEAD\r\n", "=======\r\n", ">>>>>>> topic"]
        );
    }

    #[test]
    fn masks_marker_lines_with_spaces() {
        let text = "a\n<<<<<<< HEAD\nb\n||||||| base\n=======\r\nc\n>>>>>>> topic";
        let masked = mask(text, &conflicts(text));
        assert_eq!(
            masked,
            "a\n            \nb\n            \n       \r\nc\n             "
        );
        assert_eq!(masked.len(), text.len());
    }

    #[test]
    fn masking_text_without_conflicts_borrows_it() {
        let text = "a\n=======\n";
        assert!(matches!(mask(text, &conflicts(text)), Cow::Borrowed(t) if t == text));
    }
}
