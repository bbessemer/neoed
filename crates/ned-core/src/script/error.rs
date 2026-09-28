//! Script errors and their rendering (command-language spec, §7).

use std::ops::Range;

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
    #[error("unknown command `{0}`; commands are show outline replace insert delete sub move file")]
    UnknownCommand(String),
    #[error("`{0}` is not yet supported")]
    Reserved(String),
    #[error("expected {expected}, found {found}")]
    Expected {
        expected: &'static str,
        found: String,
    },
    #[error("selectors can't contain spaces; write e.g. `impl:Parser>fn:new`")]
    SpaceInSelector,
    #[error("`all` can't be used here; a `move` destination must be a single span")]
    AllNotAllowed,
    #[error("`sub` needs a regex before `with`, e.g. sub fn:parse /old/ with \"new\"")]
    MissingSubPattern,
    #[error("invalid regex: {0}")]
    InvalidRegex(String),
}

impl ParseError {
    pub fn new(kind: ParseErrorKind, span: Range<usize>) -> Self {
        ParseError { kind, span }
    }

    /// Renders the error as `error: script:LINE:COL: message`, followed by the
    /// offending script line and a caret under the error's start.
    pub fn render(&self, src: &str) -> String {
        let (line, column) = location(src, self.span.start);
        let header = format!("error: script:{line}:{column}: {}", self.kind);
        match excerpt(src, self.span.start) {
            Some(excerpt) => format!("{header}\n{excerpt}"),
            None => header,
        }
    }
}

/// The line of `src` holding byte `offset`, labelled `LINE:`, and a caret
/// under `offset`; `None` if `offset` is past the last line.
pub fn excerpt(src: &str, offset: usize) -> Option<String> {
    let offset = offset.min(src.len());
    let start = src[..offset].rfind('\n').map_or(0, |i| i + 1);
    if start == src.len() {
        return None;
    }
    let line = src[start..].split('\n').next().unwrap_or_default();
    let line = line.strip_suffix('\r').unwrap_or(line);
    let label = format!("{}:", location(src, offset).0);
    let pad: String = " ".repeat(label.len())
        + &src[start..offset]
            .chars()
            .map(|c| if c == '\t' { '\t' } else { ' ' })
            .collect::<String>();
    Some(format!("{label}{line}\n{pad}^"))
}

/// The 1-based line and column of byte `offset` in `src`, where the column
/// counts characters.
pub fn location(src: &str, offset: usize) -> (usize, usize) {
    let before = &src[..offset.min(src.len())];
    let start = before.rfind('\n').map_or(0, |i| i + 1);
    let line = before.matches('\n').count() + 1;
    (line, before[start..].chars().count() + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(kind: ParseErrorKind, span: Range<usize>) -> ParseError {
        ParseError { kind, span }
    }

    #[test]
    fn renders_spec_example() {
        let src = "show\nreplace fn:parse.body with <<END\nfoo\n";
        let e = err(ParseErrorKind::UnterminatedHeredoc("END".into()), 32..37);
        assert_eq!(
            e.render(src),
            format!(
                "error: script:2:28: unterminated heredoc <<END (started here)\n\
                 2:replace fn:parse.body with <<END\n{}^",
                " ".repeat(29)
            )
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
