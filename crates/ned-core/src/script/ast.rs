//! Syntax tree of a script. Spans are byte ranges of the script.

use std::ops::Range;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    pub commands: Vec<Command>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub kind: CommandKind,
    pub span: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandKind {
    Show(Option<Target>),
    Outline(Option<Target>),
    Replace {
        target: Target,
        text: Text,
    },
    Insert {
        position: Position,
        target: Target,
        text: Text,
    },
    Delete(Target),
    /// Replaces every match of `pattern` inside `scope` (default: each whole
    /// file in the set).
    Sub {
        scope: Option<Target>,
        pattern: Pattern,
        text: Text,
    },
    Move {
        target: Target,
        position: Position,
        dest: Selector,
    },
    /// Replaces the file set; paths may be globs.
    File(Vec<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    Before,
    After,
    Start,
    End,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub all: bool,
    pub selector: Selector,
}

/// `step { ">" step }`: each step resolves within the spans of the previous.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selector {
    pub steps: Vec<Step>,
    pub span: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub primary: Primary,
    pub parts: Vec<Part>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Primary {
    Lines {
        start: LineNo,
        end: Option<LineNo>,
    },
    Regex(Pattern),
    Literal(Text),
    Syntax {
        kind: String,
        name: String,
    },
    /// `file:PATH`
    File(String),
    Query(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Text {
    pub value: String,
    pub kind: TextKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKind {
    Str,
    /// `<<TAG`: re-based when inserted, indentation-insensitive as a selector.
    Heredoc,
    /// `<<'TAG'`: verbatim.
    RawHeredoc,
}

/// A regex, validated at parse time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    pub source: String,
    pub flags: RegexFlags,
}

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
