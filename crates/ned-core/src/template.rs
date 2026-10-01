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
