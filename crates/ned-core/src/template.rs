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
    /// `@name...`: a run of siblings.
    pub many: bool,
    /// Where the placeholder is in the template's text.
    pub span: Range<usize>,
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

impl Template {
    pub fn parse(_src: &str) -> Template {
        unimplemented!()
    }

    pub fn holes(&self) -> impl Iterator<Item = &Hole> {
        self.pieces.iter().filter_map(|p| match p {
            Piece::Hole(h) => Some(h),
            Piece::Text { .. } => None,
        })
    }

    /// The text to parse; the holes for which `literal(i)` holds keep their
    /// own text.
    pub fn source(&self, _literal: impl Fn(usize) -> bool) -> Source {
        unimplemented!()
    }
}

impl Hole {
    /// The identifier that stands for the hole in parsed code: `__ned_NAME`,
    /// or `__ned__` for `@_`.
    pub fn placeholder(&self) -> String {
        unimplemented!()
    }
}

impl Source {
    /// The offset in the template's text that `offset` in `text` comes from.
    pub fn to_template(&self, _offset: usize) -> usize {
        unimplemented!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hole(name: Option<&str>, many: bool, span: Range<usize>) -> Piece {
        Piece::Hole(Hole {
            name: name.map(String::from),
            many,
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
                hole(Some("a"), false, 2..4),
                text(", ", 4..6),
                hole(Some("rest"), true, 6..14),
                text(") ", 14..16),
                hole(None, false, 16..18),
                text(" ", 18..19),
                hole(None, true, 19..24),
            ]
        );
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
        let s = t.source(|_| false);
        assert_eq!(s.text, "f(__ned_a, @b) __ned_x");
        assert_eq!(s.holes, [2..9, 15..22]);
    }

    #[test]
    fn literal_holes_keep_their_text() {
        let t = Template::parse("log(\"@who\", @x)");
        let s = t.source(|i| i == 0);
        assert_eq!(s.text, "log(\"@who\", __ned_x)");
        assert_eq!(s.holes, [5..9, 12..19]);
    }

    #[test]
    fn source_offsets_map_back() {
        let t = Template::parse("f(@a, @@b) z");
        let s = t.source(|_| false);
        // `f(__ned_a, @b) z`
        assert_eq!(s.to_template(0), 0);
        assert_eq!(s.to_template(2), 2);
        assert_eq!(s.to_template(9), 4);
        assert_eq!(s.to_template(11), 6);
        assert_eq!(s.to_template(12), 8);
        assert_eq!(s.to_template(15), 11);
        assert_eq!(s.to_template(16), 12);
    }
}
