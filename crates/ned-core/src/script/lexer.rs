//! Tokenizer for scripts.
//!
//! The lexer is pulled by the parser, which calls [`Lexer::path`] instead of
//! [`Lexer::next_token`] where the grammar expects `file` paths. Heredoc
//! bodies are read as soon as their `<<TAG` is lexed; the lexer then skips
//! over them when it reaches the end of the command line.

use std::ops::Range;

use super::ast::{LineNo, Part, RegexFlags};
use super::error::ParseError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Range<usize>,
    /// Whether whitespace, a comment, or the start of a line precedes the
    /// token. Selectors may not contain whitespace.
    pub space_before: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    /// A verb or keyword.
    Word(String),
    /// `kind:name`; for `file:PATH`, `name` is the path.
    Syntax {
        kind: String,
        name: String,
    },
    Lines {
        start: LineNo,
        end: Option<LineNo>,
    },
    Regex {
        pattern: String,
        flags: RegexFlags,
    },
    Str(String),
    /// `<<TAG` (or `<<'TAG'` when `raw`); `body` excludes the terminator.
    Heredoc {
        tag: String,
        body: String,
        raw: bool,
    },
    Query(String),
    Part(Part),
    /// A `file` command argument, from [`Lexer::path`].
    Path(String),
    Gt,
    Semicolon,
    Newline,
    Eof,
}

#[derive(Debug)]
pub struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    /// Where the next heredoc body on the current line starts.
    heredoc_cursor: Option<usize>,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        todo!()
    }

    pub fn next_token(&mut self) -> Result<Token, ParseError> {
        todo!()
    }

    /// Lexes one `file` argument: a quoted string, or a run of characters up
    /// to whitespace or `;`. Returns `None`, consuming nothing, at the end of
    /// the command.
    pub fn path(&mut self) -> Result<Option<Token>, ParseError> {
        todo!()
    }
}
