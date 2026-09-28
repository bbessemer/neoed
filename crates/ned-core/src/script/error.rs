//! Script errors and their rendering (command-language spec, §7).

use std::ops::Range;

use crate::buffer::Buffer;

/// An error at `span`, a byte range of the script.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{kind}")]
pub struct ParseError {
    pub kind: ParseErrorKind,
    pub span: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseErrorKind {
    #[error("unexpected character `{0}`")]
    UnexpectedChar(char),
    #[error(
        "unterminated string; close it with `\"` on the same line (use \\n or a heredoc for multi-line text)"
    )]
    UnterminatedString,
    #[error("invalid escape `\\{0}`; strings support \\n \\t \\\" \\\\")]
    InvalidEscape(char),
    #[error("unterminated regex; close it with `/` (write `\\/` for a literal slash)")]
    UnterminatedRegex,
    #[error("unknown regex flag `{0}`; flags are i and s")]
    UnknownRegexFlag(char),
    #[error("unterminated query; close it with `}}` on the same line")]
    UnterminatedQuery,
    #[error("unknown part `.{0}`; parts are .body .sig .params .name .doc .lines")]
    UnknownPart(String),
    #[error("line numbers start at 1")]
    ZeroLine,
    #[error("expected a line number or `$` after `-`, e.g. 12-20 or 12-$")]
    MissingRangeEnd,
    #[error("line range {start}-{end} is reversed; write {end}-{start}")]
    ReversedLines { start: usize, end: usize },
    #[error("line number is too large")]
    LineOverflow,
    #[error("expected a tag after `<<`, e.g. <<END")]
    MissingHeredocTag,
    #[error("unterminated heredoc <<{0} (started here)")]
    UnterminatedHeredoc(String),
    #[error("expected a name after `{0}:`, e.g. {0}:foo or {0}:*")]
    MissingName(String),
}

impl ParseError {
    /// Renders the error as `error: script:LINE:COL: message`, followed by the
    /// offending script line and a caret under the error's start. `COL` is
    /// 1-based and counts characters.
    pub fn render(&self, src: &str) -> String {
        let buf = Buffer::new(src);
        let Ok(point) = buf.byte_to_point(self.span.start) else {
            return format!("error: script: {}", self.kind);
        };
        let line_no = point.line + 1;
        let Ok(range) = buf.line_range(point.line) else {
            return format!("error: script:{line_no}:1: {}", self.kind);
        };
        let line = &src[range];
        let line = line.strip_suffix('\n').unwrap_or(line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        let before = &line[..point.column];
        let column = before.chars().count() + 1;
        let label = format!("{line_no}:");
        let pad: String = " ".repeat(label.len())
            + &before
                .chars()
                .map(|c| if c == '\t' { '\t' } else { ' ' })
                .collect::<String>();
        format!(
            "error: script:{line_no}:{column}: {}\n{label}{line}\n{pad}^",
            self.kind
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(kind: ParseErrorKind, span: Range<usize>) -> ParseError {
        ParseError { kind, span }
    }

    #[test]
    fn renders_spec_example() {
        let src = "show\nreplace fn:parse.body <<END\nfoo\n";
        let e = err(ParseErrorKind::UnterminatedHeredoc("END".into()), 27..32);
        assert_eq!(
            e.render(src),
            "error: script:2:23: unterminated heredoc <<END (started here)\n\
             2:replace fn:parse.body <<END\n\
             \x20                       ^"
        );
    }

    #[test]
    fn caret_keeps_tabs_aligned() {
        let e = err(ParseErrorKind::UnterminatedString, 6..10);
        assert_eq!(
            e.render("\tshow \"abc"),
            format!(
                "error: script:1:7: {}\n1:\tshow \"abc\n  \t     ^",
                ParseErrorKind::UnterminatedString
            )
        );
    }

    #[test]
    fn column_counts_characters() {
        let e = err(ParseErrorKind::UnexpectedChar('@'), 5..6);
        assert_eq!(
            e.render("\"é\" @"),
            "error: script:1:5: unexpected character `@`\n1:\"é\" @\n      ^"
        );
    }

    #[test]
    fn excerpt_strips_line_ending() {
        let e = err(ParseErrorKind::ZeroLine, 7..8);
        assert_eq!(
            e.render("show\r\nx 0\r\n"),
            "error: script:2:2: line numbers start at 1\n2:x 0\n   ^"
        );
    }

    #[test]
    fn error_past_last_line_has_no_excerpt() {
        let e = err(ParseErrorKind::ZeroLine, 5..5);
        assert_eq!(
            e.render("show\n"),
            "error: script:2:1: line numbers start at 1"
        );
    }
}
