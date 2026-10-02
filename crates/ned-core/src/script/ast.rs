//! Syntax tree of a script. Spans are byte ranges of the script.

use std::ops::Range;

use regex::{Regex, RegexBuilder};

use crate::lsp::Severity;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    pub commands: Vec<Command>,
    /// The index of each command that starts a stage after a `|` (§2.3).
    pub stages: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub kind: CommandKind,
    pub span: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandKind {
    /// `show [SEL [+CONTEXT]]`
    Show {
        target: Option<Target>,
        context: usize,
    },
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
    /// Creates a file holding the text, as if it had existed from the start.
    Create {
        path: String,
        text: Text,
    },
    /// `check [SEL] [LEVEL]`; `None` for the configured level.
    Check {
        target: Option<Target>,
        level: Option<Severity>,
    },
    /// `allow errors` (`Error`) or `allow warnings` (`Warning`): the most
    /// severe level an introduced diagnostic may have without rejecting the
    /// edits.
    Allow(Severity),
    /// `rename SEL to NAME`, via the language server.
    Rename {
        selector: Selector,
        name: String,
    },
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
    /// The step's filters, each with its span in the script.
    pub filters: Vec<(Filter, Range<usize>)>,
    /// The step in the script, parts and filters included.
    pub span: Range<usize>,
}

/// A filter's condition (§3.9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    Or(Vec<Filter>),
    And(Vec<Filter>),
    Cond {
        property: Property,
        op: Op,
        value: Value,
    },
}

/// `.text` or `.len` of the span, or of its `part`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Property {
    pub part: Option<Part>,
    pub len: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    /// `~=`
    Match,
    Lt,
    Gt,
    Le,
    Ge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Number(usize),
    Str(String),
    Regex(Pattern),
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
    /// A syntax pattern's code (§3.10).
    Code(String),
    /// `from..to`: from the start of a match of `from` to the end of the next
    /// match of `to` (§3.8).
    Range {
        from: Box<Primary>,
        to: Box<Primary>,
    },
}

impl Primary {
    /// The syntax patterns in the primary: its own, or its range's ends.
    pub fn patterns(&self) -> Vec<&str> {
        match self {
            Primary::Code(code) => vec![code],
            Primary::Range { from, to } => [from.patterns(), to.patterns()].concat(),
            _ => Vec::new(),
        }
    }
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

/// A regex, compiled with multi-line mode on and its flags applied.
#[derive(Debug, Clone)]
pub struct Pattern {
    pub source: String,
    pub flags: RegexFlags,
    regex: Regex,
}

impl Pattern {
    pub fn new(source: String, flags: RegexFlags) -> Result<Pattern, regex::Error> {
        let regex = RegexBuilder::new(&source)
            .multi_line(true)
            .case_insensitive(flags.case_insensitive)
            .dot_matches_new_line(flags.dot_all)
            .build()?;
        Ok(Pattern {
            source,
            flags,
            regex,
        })
    }

    pub fn regex(&self) -> &Regex {
        &self.regex
    }
}

impl PartialEq for Pattern {
    fn eq(&self, other: &Pattern) -> bool {
        (&self.source, self.flags) == (&other.source, other.flags)
    }
}

impl Eq for Pattern {}

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
    Attrs,
    Ret,
    Type,
    Value,
    Whole,
    Lines,
    Refs,
    Def,
}
