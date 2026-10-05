//! Code with `@` placeholders, as syntax patterns and `replace` TEXT use it
//! (command-language spec, §3.10).

use std::ops::Range;

/// Text split into literal text and placeholders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    pub src: String,
    pub pieces: Vec<Piece>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// Literal text: a run of the template's text, or the `@` an `@@` stands for.
    Text {
        text: String,
        span: Range<usize>,
    },
    Hole(Hole),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hole {
    /// `None` for `@_`.
    pub name: Option<String>,
    pub count: Count,
    /// Where the placeholder is in the template's text.
    pub span: Range<usize>,
}

/// How many sibling nodes a placeholder stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Count {
    /// `@name`
    One,
    /// `@name...`, a run
    OneOrMore,
    /// `@name...?`, a run that may be empty
    ZeroOrMore,
}

impl Count {
    /// What follows the name in the placeholder.
    pub fn suffix(self) -> &'static str {
        match self {
            Count::One => "",
            Count::OneOrMore => "...",
            Count::ZeroOrMore => "...?",
        }
    }
}

/// A template's text as a grammar parses it: each hole becomes its
/// placeholder identifier, or its own text where it's literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub text: String,
    /// Each hole's range in `text`, in order.
    pub holes: Vec<Range<usize>>,
    /// Each piece's range in `text`, and in the template's text.
    pieces: Vec<(Range<usize>, Range<usize>)>,
}

/// How [`Template::source`] writes a hole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoleText {
    /// Its placeholder identifier.
    Placeholder,
    /// Its own text, `@name`: inside a string or comment.
    Own,
    /// Nothing: a run where no identifier parses, as among an impl's items.
    Empty,
}

impl Template {
    pub fn parse(src: &str) -> Template {
        let mut pieces = Vec::new();
        let mut text_start = 0;
        let flush = |pieces: &mut Vec<Piece>, from: usize, to: usize| {
            if from < to {
                pieces.push(Piece::Text {
                    text: src[from..to].to_string(),
                    span: from..to,
                });
            }
        };
        let mut i = 0;
        while let Some(at) = src[i..].find('@').map(|n| i + n) {
            let rest = &src[at + 1..];
            if rest.starts_with('@') {
                flush(&mut pieces, text_start, at);
                pieces.push(Piece::Text {
                    text: "@".into(),
                    span: at..at + 2,
                });
                i = at + 2;
                text_start = i;
                continue;
            }
            let name_len = rest
                .char_indices()
                .find(|&(j, c)| {
                    !(c == '_' || c.is_ascii_alphabetic() || (j > 0 && c.is_ascii_digit()))
                })
                .map_or(rest.len(), |(j, _)| j);
            if name_len == 0 {
                i = at + 1;
                continue;
            }
            let name = &rest[..name_len];
            let count = [Count::ZeroOrMore, Count::OneOrMore]
                .into_iter()
                .find(|c| rest[name_len..].starts_with(c.suffix()))
                .unwrap_or(Count::One);
            let end = at + 1 + name_len + count.suffix().len();
            flush(&mut pieces, text_start, at);
            pieces.push(Piece::Hole(Hole {
                name: (name != "_").then(|| name.to_string()),
                count,
                span: at..end,
            }));
            i = end;
            text_start = end;
        }
        flush(&mut pieces, text_start, src.len());
        Template {
            src: src.to_string(),
            pieces,
        }
    }

    pub fn holes(&self) -> impl Iterator<Item = &Hole> {
        self.pieces.iter().filter_map(|p| match p {
            Piece::Hole(h) => Some(h),
            Piece::Text { .. } => None,
        })
    }

    /// The text to parse, with each hole `i` written as `text(i)` says.
    pub fn source(&self, text: impl Fn(usize) -> HoleText) -> Source {
        let mut out = String::new();
        let mut holes = Vec::new();
        let mut pieces = Vec::new();
        for piece in &self.pieces {
            let start = out.len();
            let span = match piece {
                Piece::Text { text: t, span } => {
                    // Separators don't count (§3.10), so a run left out
                    // takes the one after it along, unless it's the rest of
                    // the node before it, which the separator ends.
                    let alone = out
                        .trim_end()
                        .chars()
                        .last()
                        .is_none_or(|c| "([{,;".contains(c));
                    let gone = match holes.last() {
                        Some(h) if *h == (start..start) && alone => {
                            let ws = t.len() - t.trim_start().len();
                            match t[ws..].starts_with([',', ';']) {
                                true => ws + 1,
                                false => 0,
                            }
                        }
                        _ => 0,
                    };
                    out.push_str(&t[gone..]);
                    &(span.start + gone..span.end)
                }
                Piece::Hole(hole) => {
                    match text(holes.len()) {
                        HoleText::Placeholder => out.push_str(&hole.placeholder()),
                        HoleText::Own => out.push_str(&self.src[hole.span.clone()]),
                        HoleText::Empty => {}
                    }
                    holes.push(start..out.len());
                    &hole.span
                }
            };
            pieces.push((start..out.len(), span.clone()));
        }
        Source {
            text: out,
            holes,
            pieces,
        }
    }

    /// The text with each hole replaced by its capture. `capture(name)` is the
    /// captured text and the indentation of the line it starts on; a capture's
    /// later lines move under the indentation of the hole's line. Err with the
    /// name of a hole that nothing captured (`_` for `@_`).
    pub fn fill<'c>(
        &self,
        capture: impl Fn(&str) -> Option<(&'c str, &'c str)>,
    ) -> Result<String, String> {
        let mut out = String::new();
        for piece in &self.pieces {
            let hole = match piece {
                Piece::Text { text, .. } => {
                    out.push_str(text);
                    continue;
                }
                Piece::Hole(hole) => hole,
            };
            let name = hole.name.as_deref().unwrap_or("_");
            let (text, indent) = capture(name).ok_or_else(|| name.to_string())?;
            let line = &out[out.rfind('\n').map_or(0, |i| i + 1)..];
            let under = line[..line.len() - line.trim_start().len()].to_string();
            for (i, l) in text.split('\n').enumerate() {
                if i > 0 {
                    out.push('\n');
                    match l.strip_prefix(indent) {
                        _ if l.trim().is_empty() => {}
                        Some(rest) => {
                            out.push_str(&under);
                            out.push_str(rest);
                        }
                        None => out.push_str(l),
                    }
                } else {
                    out.push_str(l);
                }
            }
        }
        Ok(out)
    }
}

impl Hole {
    /// The identifier that stands for the hole in parsed code: `__ned_NAME`,
    /// or `__ned__` for `@_`.
    pub fn placeholder(&self) -> String {
        format!("__ned_{}", self.name.as_deref().unwrap_or("_"))
    }
}

impl Source {
    /// The offset in the template's text that `offset` in `text` comes from.
    pub fn to_template(&self, offset: usize) -> usize {
        match self.pieces.iter().find(|(text, _)| offset < text.end) {
            Some((text, template)) => {
                let into = offset - text.start;
                template.start + into.min(template.len() - 1)
            }
            None => self.pieces.last().map_or(0, |(_, template)| template.end),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hole(name: Option<&str>, count: Count, span: Range<usize>) -> Piece {
        Piece::Hole(Hole {
            name: name.map(String::from),
            count,
            span,
        })
    }

    fn text(text: &str, span: Range<usize>) -> Piece {
        Piece::Text {
            text: text.into(),
            span,
        }
    }

    #[test]
    fn placeholders() {
        let t = Template::parse("f(@a, @rest...) @_ @_...");
        assert_eq!(
            t.pieces,
            [
                text("f(", 0..2),
                hole(Some("a"), Count::One, 2..4),
                text(", ", 4..6),
                hole(Some("rest"), Count::OneOrMore, 6..14),
                text(") ", 14..16),
                hole(None, Count::One, 16..18),
                text(" ", 18..19),
                hole(None, Count::OneOrMore, 19..24),
            ]
        );
    }

    #[test]
    fn runs_that_may_be_empty() {
        let t = Template::parse("f(@a...?) @_...? @b? @c..?");
        assert_eq!(
            t.pieces,
            [
                text("f(", 0..2),
                hole(Some("a"), Count::ZeroOrMore, 2..8),
                text(") ", 8..10),
                hole(None, Count::ZeroOrMore, 10..16),
                text(" ", 16..17),
                hole(Some("b"), Count::One, 17..19),
                text("? ", 19..21),
                hole(Some("c"), Count::One, 21..23),
                text("..?", 23..26),
            ]
        );
        let s = t.source(|_| HoleText::Placeholder);
        assert_eq!(s.text, "f(__ned_a) __ned__ __ned_b? __ned_c..?");
    }

    #[test]
    fn literal_ats() {
        let t = Template::parse("@@x a @ b @1 x@");
        assert_eq!(t.pieces, [text("@", 0..2), text("x a @ b @1 x@", 2..15),]);
        assert_eq!(t.holes().count(), 0);
    }

    #[test]
    fn names_take_letters_digits_and_underscores() {
        let t = Template::parse("@a_1b.c @_x");
        let names: Vec<_> = t.holes().map(|h| h.name.clone()).collect();
        assert_eq!(names, [Some("a_1b".into()), Some("_x".into())]);
    }

    #[test]
    fn placeholder_identifiers() {
        let t = Template::parse("@a @rest... @_");
        let ids: Vec<_> = t.holes().map(Hole::placeholder).collect();
        assert_eq!(ids, ["__ned_a", "__ned_rest", "__ned__"]);
    }

    #[test]
    fn source_replaces_holes() {
        let t = Template::parse("f(@a, @@b) @x...");
        let s = t.source(|_| HoleText::Placeholder);
        assert_eq!(s.text, "f(__ned_a, @b) __ned_x");
        assert_eq!(s.holes, [2..9, 15..22]);
    }

    #[test]
    fn literal_holes_keep_their_text() {
        let t = Template::parse("log(\"@who\", @x)");
        let s = t.source(|i| {
            if i == 0 {
                HoleText::Own
            } else {
                HoleText::Placeholder
            }
        });
        assert_eq!(s.text, "log(\"@who\", __ned_x)");
        assert_eq!(s.holes, [5..9, 12..19]);
    }

    #[test]
    fn empty_holes_leave_nothing() {
        let t = Template::parse("impl @t { @_... }");
        let s = t.source(|i| {
            if i == 1 {
                HoleText::Empty
            } else {
                HoleText::Placeholder
            }
        });
        assert_eq!(s.text, "impl __ned_t {  }");
        assert_eq!(s.holes, [5..12, 15..15]);
    }

    #[test]
    fn empty_holes_take_their_separator_with_them() {
        let t = Template::parse("match @e { @_..., B => 1 }");
        let s = t.source(|i| {
            if i == 1 {
                HoleText::Empty
            } else {
                HoleText::Placeholder
            }
        });
        assert_eq!(s.text, "match __ned_e {  B => 1 }");
        assert_eq!(s.holes, [6..13, 16..16]);
        assert_eq!(s.to_template(17), 18);
    }

    #[test]
    fn source_offsets_map_back() {
        let t = Template::parse("f(@a, @@b) z");
        let s = t.source(|_| HoleText::Placeholder);
        // `f(__ned_a, @b) z`
        assert_eq!(s.to_template(0), 0);
        assert_eq!(s.to_template(2), 2);
        assert_eq!(s.to_template(9), 4);
        assert_eq!(s.to_template(11), 6);
        assert_eq!(s.to_template(12), 8);
        assert_eq!(s.to_template(15), 11);
        assert_eq!(s.to_template(16), 12);
    }

    /// Fills `template` from `(name, text, indent)` captures.
    fn filled(template: &str, captures: &[(&str, &str, &str)]) -> Result<String, String> {
        Template::parse(template).fill(|name| {
            captures
                .iter()
                .find(|(n, _, _)| *n == name)
                .map(|(_, text, indent)| (*text, *indent))
        })
    }

    #[test]
    fn fill_substitutes_captures() {
        let caps = [("a", "x", ""), ("b", "y + 1", "")];
        assert_eq!(filled("foo(@b, @a)", &caps), Ok("foo(y + 1, x)".into()));
        assert_eq!(filled("foo(@a...)", &caps), Ok("foo(x)".into()));
        assert_eq!(filled("foo(@a...?)", &caps), Ok("foo(x)".into()));
        assert_eq!(filled("@@a and a @ b", &caps), Ok("@a and a @ b".into()));
    }

    #[test]
    fn fill_needs_every_name_captured() {
        assert_eq!(filled("foo(@c)", &[("a", "x", "")]), Err("c".into()));
        assert_eq!(filled("foo(@_)", &[("a", "x", "")]), Err("_".into()));
    }

    #[test]
    fn fill_reindents_multi_line_captures() {
        // A capture whose first line is indented 8, now under a hole at 4.
        let body = [(
            "body",
            "a();\n        if x {\n            b();\n        }",
            "        ",
        )];
        assert_eq!(
            filled("fn f() {\n    @body\n}", &body),
            Ok("fn f() {\n    a();\n    if x {\n        b();\n    }\n}".into())
        );
        assert_eq!(
            filled("@body", &body),
            Ok("a();\nif x {\n    b();\n}".into())
        );
        // Lines less indented than the capture's first keep their indentation,
        // and blank lines stay empty.
        let odd = [("s", "\"a\n  b\n\nc\"", "    ")];
        assert_eq!(filled("  x = @s", &odd), Ok("  x = \"a\n  b\n\nc\"".into()));
    }
}
