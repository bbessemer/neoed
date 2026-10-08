//! Script errors (command-language spec, §7).

use crate::hint::{Error, Fix, Hint, verbatim};

use ParseErrorKind as E;

/// An error in the script, located at a byte range of it.
pub type ParseError = Error<ParseErrorKind>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseErrorKind {
    #[error("unexpected character `{0}`")]
    UnexpectedChar(char),
    #[error("unterminated string")]
    UnterminatedString,
    #[error("invalid escape `\\{0}`")]
    InvalidEscape(char),
    #[error("unterminated regex")]
    UnterminatedRegex,
    #[error("unknown regex flag `{0}`")]
    UnknownRegexFlag(char),
    #[error("unterminated query")]
    UnterminatedQuery,
    #[error("unterminated pattern")]
    UnterminatedPattern,
    #[error("unterminated filter")]
    UnterminatedFilter,
    #[error("expected {expected} in the filter, found {found}")]
    InFilter {
        expected: &'static str,
        found: String,
    },
    #[error("`.{0}` can't be a filter property")]
    NotAProperty(String),
    #[error("{property} is a number")]
    CompareNumber { property: String },
    #[error("{property} is text")]
    CompareText { property: String },
    #[error("a part can't follow a filter")]
    PartAfterFilter,
    #[error("unknown part `.{0}`")]
    UnknownPart(String),
    #[error("line numbers start at 1")]
    ZeroLine,
    #[error("expected a line number or `$` after `-`")]
    MissingRangeEnd,
    #[error("expected a line count after `+`")]
    MissingContext,
    #[error("line range {start}-{end} is reversed")]
    ReversedLines { start: usize, end: usize },
    #[error("line number is too large")]
    LineOverflow,
    #[error("expected a tag after `<<`")]
    MissingHeredocTag,
    #[error("unterminated heredoc <<{0} (started here)")]
    UnterminatedHeredoc(String),
    /// A bare word where a selector is expected, followed by `rest` of the
    /// selector.
    #[error("expected a selector, found `{word}`")]
    BareName { word: String, rest: String },
    #[error("expected a name after `{0}:`")]
    MissingName(String),
    #[error("`*` alone selects nothing")]
    BareStar,
    #[error("unknown command `{0}`")]
    UnknownCommand(String),
    #[error("`{0}:` is a part, not a kind")]
    PartAsKind(String),
    #[error("`conflict:{0}` isn't a conflict's number")]
    ConflictNumber(String),
    #[error("`.lines:{0}` isn't a line's number")]
    LineIndex(String),
    /// `.PART:N` for a part other than `.lines`.
    #[error("`.{0}` takes no number")]
    PartNumber(String),
    /// Its fix, if any, depends on where it is.
    #[error("expected {expected}, found {found}")]
    Expected {
        expected: &'static str,
        found: String,
    },
    /// `insert start` or `insert end` with text but no selector.
    #[error("`insert {0}` needs a selector before the text, e.g. insert {0} fn:NAME TEXT")]
    InsertNeedsSelector(&'static str),
    #[error("selectors can't contain spaces")]
    SpaceInSelector,
    #[error("`all` can't be used here")]
    AllNotAllowed,
    #[error("`sub` needs a regex before `with`")]
    MissingSubPattern,
    /// A `$` reference in `sub` TEXT to a group the regex doesn't have.
    #[error("`{reference}` names group `{name}`, which the regex doesn't have")]
    UnknownGroup { reference: String, name: String },
    /// `${}` in `sub` TEXT, which the regex crate expands to nothing.
    #[error("`${{}}` names no group")]
    EmptyGroup,
    #[error("invalid regex: {0}")]
    InvalidRegex(String),
    #[error("unknown level `{0}`")]
    UnknownLevel(String),
    #[error("`|` needs a command on each side")]
    EmptyStage,
    /// A command or part that reads the files on disk, in a stage after the
    /// first (§2.3).
    #[error("{0} reads the files on disk, which don't hold the edits before a `|`")]
    ReadsDisk(&'static str),
}

impl ParseErrorKind {
    /// Whether the script ends inside a heredoc or a pattern, so more lines
    /// could complete it (the REPL's continuation, §1.4).
    pub fn incomplete(&self) -> bool {
        matches!(self, E::UnterminatedHeredoc(_) | E::UnterminatedPattern)
    }
}

impl Hint for ParseErrorKind {
    fn exit_code(&self) -> u8 {
        2
    }

    fn fix(&self) -> Option<Fix> {
        let fix = match self {
            E::UnexpectedChar(c) => quote_hint(*c).to_string(),
            E::UnterminatedString => "close it with `\"` on the same line (use \\n or a heredoc for multi-line text)".into(),
            E::InvalidEscape(c) => format!("write `\\\\{c}` for a backslash and {c} (strings support \\n \\t \\\" \\\\)"),
            E::UnterminatedRegex => "close it with `/` (write `\\/` for a literal slash)".into(),
            E::UnknownRegexFlag(_) => "flags are i and s".into(),
            E::UnterminatedQuery => "close it with `}` on the same line".into(),
            E::UnterminatedPattern => "close it with as many backquotes as opened it".into(),
            E::UnterminatedFilter => "close it with `]` on the same line".into(),
            E::NotAProperty(_) => "use .text, .len or a part, such as .name or .body.len".into(),
            E::CompareNumber { property } => format!("compare it with == != < > <= >= and a number, e.g. {property} > 80"),
            E::CompareText { .. } => "compare it with == or != and a \"string\", or with ~= and a /regex/".into(),
            E::PartAfterFilter => "put it first: fn.body[...]".into(),
            E::UnknownPart(_) => "parts are .body .sig .params .name .doc .attrs .ret .type .value .whole .lines .refs .def".into(),
            E::ZeroLine => "use 1 for the first line".into(),
            E::MissingRangeEnd => "write one, e.g. 12-20 or 12-$".into(),
            E::MissingContext => "write one, e.g. show fn:parse +3".into(),
            E::ReversedLines { start, end } => format!("write {end}-{start}"),
            E::LineOverflow => "use `$` for the last line".into(),
            E::MissingHeredocTag => "write one, e.g. <<END".into(),
            E::UnterminatedHeredoc(tag) => format!("end it with a line holding only {tag}"),
            E::BareName { word, rest } => return Some(bare_name_fix(word, rest, None)),
            E::MissingName(kind) => format!("write one, e.g. {kind}:foo or {kind}:*"),
            E::BareStar => "*:NAME is the item NAME of any kind, e.g. *:parse, and *:* is every item".into(),
            E::UnknownCommand(_) => format!("commands are {COMMANDS}"),
            E::PartAsKind(part) => format!("select the symbol and add .{part}, e.g. fn:NAME.{part}"),
            E::ConflictNumber(_) => "conflicts count from 1 in file order, as in conflict:1, and `conflict` is each of them".into(),
            E::LineIndex(_) => "lines count from 1 within the span, as in .lines:1, and .lines:$ is the last".into(),
            E::InsertNeedsSelector(position) => whole_file_insert(position).into(),
            E::SpaceInSelector => "write e.g. `impl:Parser>fn:new`".into(),
            E::MissingSubPattern => "write one, e.g. sub fn:parse /old/ with \"new\"".into(),
            E::EmptyGroup => "write `$$` for a literal `$`".into(),
            E::InvalidRegex(_) => "escape literal characters such as ( [ . * with \\, or select a \"string\"".into(),
            E::UnknownLevel(_) => "levels are error warning info hint".into(),
            E::ReadsDisk(_) => "run it before the first `|`, or in a separate ned call".into(),
            E::InFilter { .. }
            | E::Expected { .. }
            | E::PartNumber(_)
            | E::AllNotAllowed
            | E::UnknownGroup { .. }
            | E::EmptyStage => return None,
        };
        Some(Fix::new(verbatim(&fix)))
    }

    fn excerpt(&self) -> bool {
        true
    }
}

/// Every command, as error messages list them.
pub const COMMANDS: &str =
    "show outline check replace insert delete sub move rename resolve file create allow";

/// The fix for a bare word where a selector is expected, followed by `rest`
/// of the selector; `kind` is that of an item with that name, if known.
pub(crate) fn bare_name_fix(word: &str, rest: &str, kind: Option<&str>) -> Fix {
    let fix = match kind {
        Some(kind) => format!("select the {kind} by name: {kind}:{word}{rest}"),
        // A lone word is more likely unquoted text than an item.
        None if rest.is_empty() => format!("quote literal text: \"{word}\""),
        None => {
            format!("quote literal text: \"{word}\", or select an item by name: *:{word}{rest}")
        }
    };
    Fix::new(verbatim(&fix))
}

fn quote_hint(c: char) -> &'static str {
    match c {
        '\'' => "strings use double quotes: \"...\"",
        '-' => "ranges between selectors are written SEL..SEL, e.g. /a/../b/",
        _ => "quote literal text: \"...\"",
    }
}

/// The fix for a comma at byte `at` of `src`: the commands its line means, as
/// `delete 3; delete 7`, when the command takes only selectors or levels.
pub(super) fn comma_fix(src: &str, at: usize) -> String {
    let hint = "a command takes no comma lists; separate commands with `;` or a new line";
    match comma_split(src, at) {
        Some(split) => verbatim(&format!("{hint}: {split}")),
        None => hint.to_string(),
    }
}

fn comma_split(src: &str, at: usize) -> Option<String> {
    let start = src[..at].rfind(['\n', ';']).map_or(0, |i| i + 1);
    let before = src[start..at].trim();
    let verb = before.split_whitespace().next()?;
    let lists = ["show", "outline", "check", "delete", "allow"];
    if !lists.contains(&verb) || before == verb {
        return None;
    }
    let mut commands = vec![before.to_string()];
    for arg in src[at + 1..].split(['\n', ';']).next()?.split(',') {
        let arg = arg.trim();
        if arg.is_empty() {
            return None;
        }
        commands.push(format!("{verb} {arg}"));
    }
    Some(commands.join("; "))
}

fn whole_file_insert(position: &str) -> &'static str {
    match position {
        "start" => "insert before 1 TEXT adds to the top of the file",
        _ => "insert after $ TEXT appends to the file",
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
    use std::ops::Range;

    use super::*;
    use crate::hint::Frontend;

    fn render(kind: ParseErrorKind, span: Range<usize>, src: &str) -> String {
        kind.at(span).render(Frontend::Cli, Some(src))
    }

    #[test]
    fn renders_spec_example() {
        let src = "show\nreplace fn:parse.body with <<END\nfoo\n";
        assert_eq!(
            render(E::UnterminatedHeredoc("END".into()), 32..37, src),
            format!(
                "error: script:2:28: unterminated heredoc <<END (started here); \
                 end it with a line holding only END\n\
                 2:replace fn:parse.body with <<END\n{}^",
                " ".repeat(29)
            )
        );
    }

    #[test]
    fn caret_keeps_tabs_aligned() {
        assert_eq!(
            render(E::ZeroLine, 6..10, "\tshow \"abc"),
            "error: script:1:7: line numbers start at 1; use 1 for the first line\n1:\tshow \"abc\n  \t     ^"
        );
    }

    #[test]
    fn column_counts_characters() {
        assert_eq!(
            render(E::ZeroLine, 5..6, "\"é\" @"),
            "error: script:1:5: line numbers start at 1; use 1 for the first line\n1:\"é\" @\n      ^"
        );
    }

    #[test]
    fn excerpt_strips_line_ending() {
        assert_eq!(
            render(E::ZeroLine, 7..8, "show\r\nx 0\r\n"),
            "error: script:2:2: line numbers start at 1; use 1 for the first line\n2:x 0\n   ^"
        );
    }

    #[test]
    fn error_past_last_line_has_no_excerpt() {
        assert_eq!(
            render(E::ZeroLine, 5..5, "show\n"),
            "error: script:2:1: line numbers start at 1; use 1 for the first line"
        );
    }

    #[test]
    fn an_error_without_a_fix_ends_with_its_problem() {
        let found = E::Expected {
            expected: "a selector",
            found: "`;`".into(),
        };
        assert_eq!(
            render(found, 5..6, "show ;"),
            "error: script:1:6: expected a selector, found `;`\n1:show ;\n       ^"
        );
    }

    #[test]
    fn a_fix_quoting_braces_keeps_them() {
        let braced = E::UnexpectedChar('{')
            .at(0..1)
            .with_fix("quote a name that has braces: import:\"a::{b}\"");
        let unknown = E::UnknownGroup {
            reference: "$x".into(),
            name: "x".into(),
        }
        .at(0..1)
        .with_fix("its groups are `${1}` `${y}`");
        for frontend in [Frontend::Cli, Frontend::Mcp, Frontend::Repl] {
            assert_eq!(
                braced.render(frontend, None),
                "error: unexpected character `{`; quote a name that has braces: import:\"a::{b}\""
            );
            assert_eq!(
                unknown.render(frontend, None),
                "error: `$x` names group `x`, which the regex doesn't have; its groups are `${1}` `${y}`"
            );
        }
    }

    #[test]
    fn a_bare_name_of_a_known_kind_selects_it() {
        assert_eq!(
            bare_name_fix("GitError", ".body", Some("enum")).text,
            "select the enum by name: enum:GitError.body"
        );
        assert_eq!(
            bare_name_fix("x", "", None).text,
            "quote literal text: \"x\""
        );
        assert_eq!(
            bare_name_fix("x", ">fn:y", None).text,
            "quote literal text: \"x\", or select an item by name: *:x>fn:y"
        );
    }

    #[test]
    fn a_script_ending_in_a_heredoc_or_pattern_is_incomplete() {
        let incomplete = |src: &str| crate::script::parse(src).unwrap_err().kind.incomplete();
        assert!(incomplete("replace 1 with <<END\nfn a() {}"));
        assert!(incomplete("insert after 1 <<'END'"));
        assert!(incomplete("show `foo(@x"));
        assert!(incomplete("show ``foo(`x`"));
        assert!(!incomplete("show \"foo"));
        assert!(!incomplete("show /foo"));
        assert!(!incomplete("frobnicate 1"));
        assert!(!incomplete("show 1 +"));
    }
}
