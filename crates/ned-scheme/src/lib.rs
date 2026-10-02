//! The Scheme dialect behind `ned`'s data files and, later, its plugins: a
//! Scheme that reads tree-sitter query syntax natively (`@capture`, `[...]`
//! alternation, `field:`, `#eq?`, `.` anchors). This crate only reads it.

mod read;

use std::fmt;
use std::ops::Range;

pub use read::read_all;

/// A datum and the byte range of the source it was read from.
#[derive(Debug, Clone, PartialEq)]
pub struct Syntax {
    pub datum: Datum,
    pub span: Range<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Datum {
    Bool(bool),
    Int(i64),
    Real(f64),
    Str(String),
    /// Any other token, including `_`, `*`, `?`, `!field` and `#eq?`.
    Symbol(String),
    /// `name:`, a query's field name (SRFI 88 keyword syntax).
    Keyword(String),
    /// `@name`, without the `@`.
    Capture(String),
    /// `(...)`
    List(Vec<Syntax>),
    /// `[...]`
    Alternation(Vec<Syntax>),
    /// A lone `.`, a query's anchor. There is no dotted-pair syntax.
    Anchor,
}

/// Writes the datum back in the syntax [`read_all`] reads.
impl fmt::Display for Datum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let items = |f: &mut fmt::Formatter<'_>, items: &[Syntax]| {
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    f.write_str(" ")?;
                }
                write!(f, "{}", item.datum)?;
            }
            Ok(())
        };
        match self {
            Datum::Bool(b) => f.write_str(if *b { "#t" } else { "#f" }),
            Datum::Int(n) => write!(f, "{n}"),
            // Keeps the decimal point, so the number reads back as a Real.
            Datum::Real(x) if x.fract() == 0.0 => write!(f, "{x:.1}"),
            Datum::Real(x) => write!(f, "{x}"),
            Datum::Str(s) => {
                f.write_str("\"")?;
                for c in s.chars() {
                    match c {
                        '"' => f.write_str("\\\"")?,
                        '\\' => f.write_str("\\\\")?,
                        '\n' => f.write_str("\\n")?,
                        '\r' => f.write_str("\\r")?,
                        '\t' => f.write_str("\\t")?,
                        '\0' => f.write_str("\\0")?,
                        c => write!(f, "{c}")?,
                    }
                }
                f.write_str("\"")
            }
            Datum::Symbol(s) => f.write_str(s),
            Datum::Keyword(name) => write!(f, "{name}:"),
            Datum::Capture(name) => write!(f, "@{name}"),
            Datum::List(v) => {
                f.write_str("(")?;
                items(f, v)?;
                f.write_str(")")
            }
            Datum::Alternation(v) => {
                f.write_str("[")?;
                items(f, v)?;
                f.write_str("]")
            }
            Datum::Anchor => f.write_str("."),
        }
    }
}

/// An error at `span`, a byte range of the source.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{kind}")]
pub struct ReadError {
    pub kind: ReadErrorKind,
    pub span: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReadErrorKind {
    #[error("unterminated string; end it with `\"`")]
    UnterminatedString,
    #[error("unterminated `{0}`; close it with `{close}`", close = closer(*.0))]
    Unterminated(char),
    #[error("unterminated block comment; close it with `|#`")]
    UnterminatedComment,
    #[error("unexpected `{0}`; delete it, or open a `{open}` before it", open = opener(*.0))]
    UnexpectedCloser(char),
    #[error("`{open}` closed by `{close}`; close it with `{expected}`", expected = closer(*open))]
    MismatchedCloser { open: char, close: char },
    #[error("`{0}` needs a datum after it; add one, such as `{0}x`")]
    MissingDatum(&'static str),
    #[error("`@` needs a name; name the capture, such as `@x`")]
    EmptyCapture,
    #[error("`{0}` isn't supported; write a string or a list instead")]
    Unsupported(String),
    #[error("integer `{0}` is out of range; integers must fit in 64 bits")]
    IntOutOfRange(String),
}

fn closer(open: char) -> char {
    if open == '[' { ']' } else { ')' }
}

fn opener(close: char) -> char {
    if close == ']' { '[' } else { '(' }
}
