//! Syntax tree of a script.

/// A 1-based line number, or `$` for the last line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineNo {
    Number(usize),
    Last,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RegexFlags {
    /// `i`
    pub case_insensitive: bool,
    /// `s`
    pub dot_all: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Body,
    Sig,
    Params,
    Name,
    Doc,
    Lines,
}
