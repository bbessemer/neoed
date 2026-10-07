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
    #[error("unexpected character `{0}`; {hint}", hint = quote_hint(*.0))]
    UnexpectedChar(char),
    #[error(
        "unterminated string; close it with `\"` on the same line (use \\n or a heredoc for multi-line text)"
    )]
    UnterminatedString,
    #[error(
        "invalid escape `\\{0}`; write `\\\\{0}` for a backslash and {0} (strings support \\n \\t \\\" \\\\)"
    )]
    InvalidEscape(char),
    #[error("unterminated regex; close it with `/` (write `\\/` for a literal slash)")]
    UnterminatedRegex,
    #[error("unknown regex flag `{0}`; flags are i and s")]
    UnknownRegexFlag(char),
    #[error("unterminated query; close it with `}}` on the same line")]
    UnterminatedQuery,
    #[error("unterminated pattern; close it with as many backquotes as opened it")]
    UnterminatedPattern,
    #[error("unterminated filter; close it with `]` on the same line")]
    UnterminatedFilter,
    #[error("expected {expected} in the filter, found {found}")]
    InFilter {
        expected: &'static str,
        found: String,
    },
    #[error(
        "`.{0}` can't be a filter property; use .text, .len or a part, such as .name or .body.len"
    )]
    NotAProperty(String),
    #[error(
        "{property} is a number; compare it with == != < > <= >= and a number, e.g. {property} > 80"
    )]
    CompareNumber { property: String },
    #[error(
        "{property} is text; compare it with == or != and a \"string\", or with ~= and a /regex/"
    )]
    CompareText { property: String },
    #[error("a part can't follow a filter; put it first: fn.body[...]")]
    PartAfterFilter,
    #[error(
        "unknown part `.{0}`; parts are .body .sig .params .name .doc .attrs .ret .type .value .whole .lines .refs .def"
    )]
    UnknownPart(String),
    /// `selector` is the whole dotted name, quoted.
    #[error("unknown part `.{part}`; quote a name that has dots: {selector}")]
    DottedName { selector: String, part: String },
    /// An unquoted name followed by `-`, as in `import:react-router`; the
    /// selector is the whole name, quoted.
    #[error("unexpected character `-`; quote the name: {0}")]
    DashedName(String),
    /// `selector` nests the dotted name's segments under a placeholder `KIND`;
    /// `method` is the quoted Go method name, for `fn:Recv.Name`.
    #[error(
        "unknown part `.{part}`; to name a member, nest it: {selector}{}",
        method.as_ref().map(|m| format!(" (a Go method: {m})")).unwrap_or_default()
    )]
    NestedName {
        selector: String,
        part: String,
        method: Option<String>,
    },
    /// `selector` is the whole name up to the matching `}`, quoted.
    #[error("unexpected character `{{`; quote a name that has braces: {selector}")]
    BracedName { selector: String },
    #[error("line numbers start at 1; use 1 for the first line")]
    ZeroLine,
    #[error("expected a line number or `$` after `-`, e.g. 12-20 or 12-$")]
    MissingRangeEnd,
    #[error("expected a line count after `+`, e.g. show fn:parse +3")]
    MissingContext,
    /// `show SEL +N..+M`; the fix is `show SEL +M`.
    #[error("`+N` is one count of lines around each span, not a range; write {0}")]
    ContextRange(String),
    /// `show SEL -N`; the fix is `show SEL +N`.
    #[error("context is written `+N`, not `-N`; write {0}")]
    MinusContext(String),
    #[error("line range {start}-{end} is reversed; write {end}-{start}")]
    ReversedLines { start: usize, end: usize },
    /// `N,M`, sed's line range; the fix is the `N-M` range it means.
    #[error("line ranges are written N-M, not sed's N,M; write {0}")]
    SedRange(String),
    #[error("line number is too large; use `$` for the last line")]
    LineOverflow,
    #[error("expected a tag after `<<`, e.g. <<END")]
    MissingHeredocTag,
    #[error("unterminated heredoc <<{0} (started here); end it with a line holding only {0}")]
    UnterminatedHeredoc(String),
    /// `error`, after a heredoc that ended at a line of its text holding only
    /// its tag, so the script went on in what was meant as text.
    #[error(
        "{error}; the heredoc <<{tag} at line {opener} ended at line {end}, which holds only {tag}: pick a tag its text doesn't hold"
    )]
    EarlyHeredoc {
        error: Box<ParseErrorKind>,
        tag: String,
        opener: usize,
        end: usize,
    },
    /// A bare word where a selector is expected, followed by `rest` of the
    /// selector; `kind` is that of an item with that name, if known.
    #[error("expected a selector, found `{word}`; {}", bare_name_hint(word, rest, *kind))]
    BareName {
        word: String,
        rest: String,
        kind: Option<&'static str>,
    },
    #[error("expected a name after `{0}:`, e.g. {0}:foo or {0}:*")]
    MissingName(String),
    #[error(
        "`*` alone selects nothing; *:NAME is the item NAME of any kind, e.g. *:parse, and *:* is every item"
    )]
    BareStar,
    #[error("unknown command `{0}`; commands are {list}", list = COMMANDS)]
    UnknownCommand(String),
    #[error("`{0}:` is a part, not a kind; select the symbol and add .{0}, e.g. fn:NAME.{0}")]
    PartAsKind(String),
    #[error(
        "`conflict:{0}` isn't a conflict's number; conflicts count from 1 in file order, as in conflict:1, and `conflict` is each of them"
    )]
    ConflictNumber(String),
    #[error(
        "`.lines:{0}` isn't a line's number; lines count from 1 within the span, as in .lines:1, and .lines:$ is the last"
    )]
    LineIndex(String),
    /// `.PART:N` for a part other than `.lines`.
    #[error("`.{part}` takes no number; pick a line of it with .{part}.lines:{number}")]
    PartNumber { part: String, number: String },
    /// `hint` is empty, or `; ` and a fix.
    #[error("expected {expected}, found {found}{hint}")]
    Expected {
        expected: &'static str,
        found: String,
        hint: String,
    },
    /// `insert start` or `insert end` with text but no selector.
    #[error(
    "`insert {0}` needs a selector before the text, e.g. insert {0} fn:NAME TEXT; {fix}",
    fix = whole_file_insert(.0)
)]
    InsertNeedsSelector(&'static str),
    #[error("selectors can't contain spaces; write e.g. `impl:Parser>fn:new`")]
    SpaceInSelector,
    #[error("`all` can't be used here; a `move` destination must be a single span")]
    AllNotAllowed,
    #[error("`sub` needs a regex before `with`, e.g. sub fn:parse /old/ with \"new\"")]
    MissingSubPattern,
    /// `sub all /re/ with`, holding the regex as written.
    #[error("`sub` already replaces every match; drop `all`: sub {0} with ...")]
    SubAll(String),
    /// `sub SEL /re/text/`, sed's form; the fix is the `sub` it means.
    #[error("`sub` takes /re/ with TEXT, not sed's /re/text/; write {0}")]
    SedSub(String),
    /// `sub [SEL] "lit" with TEXT`; the fix is the `replace all` it means.
    #[error("`sub` takes a regex, not a literal; write {0}")]
    LiteralSub(String),
    /// `all` after a target's selector; the fix puts it before.
    #[error("`all` goes before the selector; write {0}")]
    AllAfterSelector(String),
    /// A regex, string or pattern glued to a selector; the fix nests it with `>`.
    #[error("a search in a step goes after `>`; write {0}")]
    GluedStep(String),
    /// A `$` reference in `sub` TEXT to a group the regex doesn't have; `fix`
    /// splits off the group it starts with, or lists the groups.
    #[error("`{reference}` names group `{name}`, which the regex doesn't have; {fix}")]
    UnknownGroup {
        reference: String,
        name: String,
        fix: String,
    },
    /// `${}` in `sub` TEXT, which the regex crate expands to nothing.
    #[error("`${{}}` names no group; write `$$` for a literal `$`")]
    EmptyGroup,
    #[error(
        "invalid regex: {0}; escape literal characters such as ( [ . * with \\, or select a \"string\""
    )]
    InvalidRegex(String),
    /// `fix` names the levels, or the one meant.
    #[error("unknown level `{word}`; {fix}")]
    UnknownLevel { word: String, fix: String },
    #[error("`|` needs a command on each side")]
    EmptyStage,
    /// A command or part that reads the files on disk, in a stage after the
    /// first (§2.3).
    #[error(
        "{0} reads the files on disk, which don't hold the edits before a `|`; run it before the first `|`, or in a separate ned call"
    )]
    ReadsDisk(&'static str),
}

/// Every command, as error messages list them.
pub const COMMANDS: &str =
    "show outline check replace insert delete sub move rename resolve file create allow";

fn bare_name_hint(word: &str, rest: &str, kind: Option<&str>) -> String {
    match kind {
        Some(kind) => format!("select the {kind} by name: {kind}:{word}{rest}"),
        // A lone word is more likely unquoted text than an item.
        None if rest.is_empty() => format!("quote literal text: \"{word}\""),
        None => {
            format!("quote literal text: \"{word}\", or select an item by name: *:{word}{rest}")
        }
    }
}

fn quote_hint(c: char) -> &'static str {
    match c {
        '\'' => "strings use double quotes: \"...\"",
        '-' => "ranges between selectors are written SEL..SEL, e.g. /a/../b/",
        _ => "quote literal text: \"...\"",
    }
}

fn whole_file_insert(position: &str) -> &'static str {
    match position {
        "start" => "insert before 1 TEXT adds to the top of the file",
        _ => "insert after $ TEXT appends to the file",
    }
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

    /// Whether the script ends inside a heredoc or a pattern, so more lines
    /// could complete it (the REPL's continuation, §1.4).
    pub fn incomplete(&self) -> bool {
        matches!(
            self.kind,
            ParseErrorKind::UnterminatedHeredoc(_) | ParseErrorKind::UnterminatedPattern
        )
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
                "error: script:2:28: unterminated heredoc <<END (started here); \
                 end it with a line holding only END\n\
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
            format!("error: script:1:5: {}\n1:\"é\" @\n      ^", e.kind)
        );
    }

    #[test]
    fn excerpt_strips_line_ending() {
        let e = err(ParseErrorKind::ZeroLine, 7..8);
        assert_eq!(
            e.render("show\r\nx 0\r\n"),
            format!("error: script:2:2: {}\n2:x 0\n   ^", e.kind)
        );
    }

    #[test]
    fn error_past_last_line_has_no_excerpt() {
        let e = err(ParseErrorKind::ZeroLine, 5..5);
        assert_eq!(e.render("show\n"), format!("error: script:2:1: {}", e.kind));
    }

    #[test]
    fn a_script_ending_in_a_heredoc_or_pattern_is_incomplete() {
        let incomplete = |src: &str| crate::script::parse(src).unwrap_err().incomplete();
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
