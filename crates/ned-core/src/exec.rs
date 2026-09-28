//! Running scripts against files, and the errors that can stop a run.

use std::fmt;
use std::ops::Range;

use crate::script::error::location;

/// An error that rejects a script, at `span` of the script when it has one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{kind}")]
pub struct ExecError {
    pub kind: ExecErrorKind,
    pub span: Option<Range<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExecErrorKind {
    #[error("{selector} matches nothing in {files}")]
    NoMatch { selector: String, files: String },
    #[error(
        "{selector} matches {} items; add `all` or use one of:{}",
        .candidates.total,
        .candidates
    )]
    Ambiguous {
        selector: String,
        candidates: Candidates,
    },
    #[error("line {line} is past the end of {files}")]
    LineOutOfRange { line: String, files: String },
    #[error("file:{path} is not in the file set: {files}")]
    NotInFileSet { path: String, files: String },
    #[error("{0} is not yet supported")]
    Unsupported(String),
}

/// Selectors that each pick one of an ambiguous selector's matches, with the
/// location of that match; `total` counts all matches, listed or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidates {
    pub listed: Vec<(String, String)>,
    pub total: usize,
}

impl fmt::Display for Candidates {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let width = self.listed.iter().map(|(s, _)| s.len()).max();
        for (selector, loc) in &self.listed {
            write!(f, "\n  {selector:<w$}   {loc}", w = width.unwrap_or(0))?;
        }
        let more = self.total - self.listed.len();
        if more > 0 {
            write!(f, "\n  … and {more} more")?;
        }
        Ok(())
    }
}

impl ExecError {
    pub fn new(kind: ExecErrorKind, span: Option<Range<usize>>) -> Self {
        ExecError { kind, span }
    }

    /// Renders the error as `error: script:LINE:COL: message`, or
    /// `error: message` when it has no script position.
    pub fn render(&self, src: &str) -> String {
        match &self.span {
            Some(span) => {
                let (line, column) = location(src, span.start);
                format!("error: script:{line}:{column}: {}", self.kind)
            }
            None => format!("error: {}", self.kind),
        }
    }
}
