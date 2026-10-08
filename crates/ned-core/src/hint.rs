//! Errors with fixes (command-language spec, §7): an error's kind says what
//! went wrong, and its fix how to put it right, in the terms of the frontend
//! that prints it. Notes say what a reader should know whether or not the
//! work succeeded; a [`Report`] carries them beside a result.
//!
//! A fix is a template: `{KEY}` is an option named in `OPTIONS`, and
//! `{cli:TEXT}`, `{mcp:TEXT}` and `{repl:TEXT}` are TEXT for that frontend
//! alone. Other braces are kept as written, so a fix can quote script text
//! that holds them, and `{{` is a `{`, so [`verbatim`] text is never a
//! placeholder; a fix's detail is never a template.

use std::fmt;
use std::ops::Range;

use crate::script::error::{excerpt, location};

/// Who reads the output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Frontend {
    Cli,
    Mcp,
    Repl,
}

/// Each option's placeholder key, then what the CLI, the MCP server and the
/// REPL call it.
pub(crate) const OPTIONS: &[(&str, &str, &str, &str)] = &[
    ("-w", "-w", "`workspace`", "-w"),
    ("--force", "--force", "`force`", "`ned repl --force`"),
    (
        "--no-check",
        "--no-check",
        "`no_check`",
        "`ned repl --no-check`",
    ),
    ("--no-fmt", "--no-fmt", "`no_fmt`", "`ned repl --no-fmt`"),
    (
        "the FILE arguments",
        "the FILE arguments",
        "`files`",
        "the FILE arguments",
    ),
    ("ned help", "ned help", "`help`", ":help"),
    ("--commit", "--commit", "`commit`", "`:commit`"),
    (
        "-s NAME",
        "-s NAME or NED_SESSION",
        "restart the server with -s NAME",
        "restart the REPL with -s NAME",
    ),
    ("ned history", "`ned history`", "`history`", "`:history`"),
    ("ned undo", "`ned undo`", "`undo`", "`ned undo`"),
    ("--lang", "--lang", "`ned mcp --lang`", "`ned repl --lang`"),
];

impl Frontend {
    /// `template` with its placeholders replaced.
    pub fn render(self, mut template: &str) -> String {
        let mine = match self {
            Frontend::Cli => "{cli:",
            Frontend::Mcp => "{mcp:",
            Frontend::Repl => "{repl:",
        };
        let others = ["{cli:", "{mcp:", "{repl:"];
        let mut out = String::with_capacity(template.len());
        while let Some(start) = template.find('{') {
            if template[start + 1..].starts_with('{') {
                out.push_str(&template[..=start]);
                template = &template[start + 2..];
                continue;
            }
            let Some(len) = template[start..].find('}') else {
                break;
            };
            if let Some(inner) = template[start + 1..start + len].find('{') {
                out.push_str(&template[..=start + inner]);
                template = &template[start + 1 + inner..];
                continue;
            }
            out.push_str(&template[..start]);
            let placeholder = &template[start..=start + len];
            let option = OPTIONS
                .iter()
                .find(|(key, ..)| *key == &placeholder[1..len]);
            if let Some((_, cli, mcp, repl)) = option {
                out.push_str(match self {
                    Frontend::Cli => cli,
                    Frontend::Mcp => mcp,
                    Frontend::Repl => repl,
                });
            } else if let Some(only) = placeholder[..len].strip_prefix(mine) {
                out.push_str(only);
            } else if !others.iter().any(|other| placeholder.starts_with(other)) {
                out.push_str(placeholder);
            }
            template = &template[start + len + 1..];
        }
        out + template
    }
}

/// `text` for a template, to render as it is: each `{` doubled.
pub fn verbatim(text: &str) -> String {
    text.replace('{', "{{")
}

/// What can set an error's fix: a fix, its text, or none.
pub trait IntoFix {
    fn into_fix(self) -> Option<Fix>;
}

impl IntoFix for Fix {
    fn into_fix(self) -> Option<Fix> {
        Some(self)
    }
}

impl IntoFix for &str {
    fn into_fix(self) -> Option<Fix> {
        Some(self.into())
    }
}

impl IntoFix for String {
    fn into_fix(self) -> Option<Fix> {
        Some(self.into())
    }
}

impl<F: IntoFix> IntoFix for Option<F> {
    fn into_fix(self) -> Option<Fix> {
        self.and_then(IntoFix::into_fix)
    }
}

/// How to put an error right: a template, then the candidates to pick from,
/// if any, one per line, then the detail, printed as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fix {
    pub text: String,
    pub candidates: Option<Candidates>,
    pub detail: Option<String>,
}

impl Fix {
    pub fn new(text: impl Into<String>) -> Fix {
        Fix {
            text: text.into(),
            candidates: None,
            detail: None,
        }
    }

    /// `text`, then `candidates`.
    pub fn choose(text: impl Into<String>, candidates: Candidates) -> Fix {
        Fix {
            candidates: Some(candidates),
            ..Fix::new(text)
        }
    }

    /// The fix, then `detail` on the lines after it: text such as a listing
    /// or a file's lines, which may hold braces of its own.
    pub fn then(self, detail: impl Into<String>) -> Fix {
        Fix {
            detail: Some(detail.into()),
            ..self
        }
    }

    /// This fix, then `other`'s, after a `; `: the first's candidates, else the
    /// second's, and both details.
    pub fn and(self, other: impl Into<Fix>) -> Fix {
        let other = other.into();
        let detail = match (self.detail, other.detail) {
            (Some(first), Some(second)) => Some(format!("{first}\n{second}")),
            (first, second) => first.or(second),
        };
        Fix {
            text: format!("{}; {}", self.text, other.text),
            candidates: self.candidates.or(other.candidates),
            detail,
        }
    }

    /// Appends the text in `frontend`'s terms, the candidates, and the
    /// detail.
    fn render(&self, frontend: Frontend, out: &mut String) {
        out.push_str(&frontend.render(&self.text));
        if let Some(candidates) = &self.candidates {
            candidates.render(out);
        }
        if let Some(detail) = &self.detail {
            out.push('\n');
            out.push_str(detail);
        }
    }
}

impl From<&str> for Fix {
    fn from(text: &str) -> Fix {
        Fix::new(text)
    }
}

impl From<String> for Fix {
    fn from(text: String) -> Fix {
        Fix::new(text)
    }
}

/// Selectors that each pick one of an ambiguous selector's matches, with the
/// location of that match; `total` counts all matches, listed or not, and
/// `shared` those that no selector picks alone, since they share a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidates {
    pub listed: Vec<(String, String)>,
    pub total: usize,
    pub shared: usize,
}

impl Candidates {
    /// Appends a line for each selector, aligned, then the count of those
    /// left out.
    fn render(&self, out: &mut String) {
        let width = self.listed.iter().map(|(s, _)| s.len()).max();
        for (selector, loc) in &self.listed {
            let w = width.unwrap_or(0);
            out.push_str(&format!("\n  {selector:<w$}   {loc}"));
        }
        let more = self.total - self.shared - self.listed.len();
        if more > 0 {
            out.push_str(&format!("\n  … and {more} more"));
        }
        if self.shared > 0 {
            let shared = self.shared;
            out.push_str(&format!(
                "\n  {shared} more share a line with another match; select longer text to pick one"
            ));
        }
    }
}

/// What went wrong: its `Display` is the problem alone, without the fix.
pub trait Hint: fmt::Display + fmt::Debug + Send + Sync {
    /// The exit code of a script this stops (spec §7).
    fn exit_code(&self) -> u8;

    /// The fix that follows from the kind alone.
    fn fix(&self) -> Option<Fix> {
        None
    }

    /// Whether a located error shows the script line, with a caret under its
    /// start.
    fn excerpt(&self) -> bool {
        false
    }

    /// The error of this kind at `span` in the script.
    fn at(self, span: impl Into<Option<Range<usize>>>) -> Error<Self>
    where
        Self: Sized,
    {
        Error::new(self).at(span)
    }
}

/// An error of kind `K`, at `span` in the script if it has a place there,
/// with the fix that renders after it.
///
/// It prints in a frontend's terms through [`Error::render`]; its `Display`
/// is the CLI's, for wrappers and logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error<K: ?Sized> {
    pub span: Option<Range<usize>>,
    pub fix: Option<Fix>,
    pub kind: K,
}

/// An error of any kind.
pub type AnyError = Box<Error<dyn Hint>>;

impl<K: Hint> Error<K> {
    /// The error, with the fix its kind gives.
    pub fn new(kind: K) -> Self {
        let fix = kind.fix();
        Error {
            kind,
            span: None,
            fix,
        }
    }

    pub fn at(self, span: impl Into<Option<Range<usize>>>) -> Self {
        Error {
            span: span.into(),
            ..self
        }
    }

    /// The error, with the fix `fix` gives if it has none.
    pub fn or_fix<F: Into<Fix>>(self, fix: impl FnOnce(&K) -> Option<F>) -> Self {
        if self.fix.is_some() {
            return self;
        }
        let fix = fix(&self.kind).map(Into::into);
        Error { fix, ..self }
    }

    /// The error, with `fix` in place of its own, if there is one.
    pub fn with_fix(self, fix: impl IntoFix) -> Self {
        match fix.into_fix() {
            Some(fix) => Error {
                fix: Some(fix),
                ..self
            },
            None => self,
        }
    }

    /// The error, with `fix` after its own fix, if it has one.
    pub fn and_fix(self, fix: impl Into<Fix>) -> Self {
        let fix = match self.fix {
            Some(own) => own.and(fix),
            None => fix.into(),
        };
        Error {
            fix: Some(fix),
            ..self
        }
    }

    /// The error as one of kind `L`, made from its own, with its place and
    /// fix, or `L`'s fix if it has none.
    pub fn map_kind<L: Hint>(self, kind: impl FnOnce(K) -> L) -> Error<L> {
        let kind = kind(self.kind);
        let fix = self.fix.or_else(|| kind.fix());
        Error {
            span: self.span,
            fix,
            kind,
        }
    }
}

impl<K: Hint + ?Sized> Error<K> {
    pub fn exit_code(&self) -> u8 {
        self.kind.exit_code()
    }

    /// `script:LINE:COL: PROBLEM; FIX`, without the location when there's no
    /// `src` or span and without `; FIX` when there's no fix; then the
    /// candidates, each on its own line, the fix's detail, and the excerpt if
    /// the kind shows one.
    pub fn message(&self, frontend: Frontend, src: Option<&str>) -> String {
        let at = self.span.as_ref().zip(src);
        let mut out = match at {
            Some((span, src)) => {
                let (line, column) = location(src, span.start);
                format!("script:{line}:{column}: {}", &self.kind)
            }
            None => self.kind.to_string(),
        };
        if let Some(fix) = &self.fix {
            out.push_str("; ");
            fix.render(frontend, &mut out);
        }
        let shown = at.filter(|_| self.kind.excerpt());
        if let Some(excerpt) = shown.and_then(|(span, src)| excerpt(src, span.start)) {
            out.push('\n');
            out.push_str(&excerpt);
        }
        out
    }

    /// `error: ` and the message.
    pub fn render(&self, frontend: Frontend, src: Option<&str>) -> String {
        format!("error: {}", self.message(frontend, src))
    }

    /// The error as a note: its problem and fix, without its place.
    fn note(&self) -> Note {
        Note {
            text: self.kind.to_string(),
            fix: self.fix.clone(),
        }
    }
}

impl<K: Hint> From<K> for Error<K> {
    fn from(kind: K) -> Self {
        Error::new(kind)
    }
}

impl<K: Hint + 'static> From<Error<K>> for AnyError {
    fn from(error: Error<K>) -> Self {
        Box::new(error)
    }
}

impl<K: Hint + 'static> From<K> for AnyError {
    fn from(kind: K) -> Self {
        Box::new(Error::new(kind))
    }
}

impl<K: Hint + ?Sized> fmt::Display for Error<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message(Frontend::Cli, None))
    }
}

impl<K: Hint + ?Sized> std::error::Error for Error<K> {}

/// Something to tell the reader that doesn't stop the work: plain text, and a
/// fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub text: String,
    pub fix: Option<Fix>,
}

impl Note {
    /// `note: TEXT; FIX`, without `; FIX` when there's no fix.
    pub fn render(&self, frontend: Frontend) -> String {
        let mut out = format!("note: {}", self.text);
        if let Some(fix) = &self.fix {
            out.push_str("; ");
            fix.render(frontend, &mut out);
        }
        out
    }

    /// The note, its text after `context` and a `: `.
    pub fn context(self, context: impl fmt::Display) -> Note {
        Note {
            text: format!("{context}: {}", self.text),
            ..self
        }
    }
}

impl From<&str> for Note {
    fn from(text: &str) -> Note {
        text.to_string().into()
    }
}

impl From<String> for Note {
    fn from(text: String) -> Note {
        Note { text, fix: None }
    }
}

/// An error worked around: its problem and fix, without its place.
impl<K: Hint> From<Error<K>> for Note {
    fn from(error: Error<K>) -> Note {
        error.note()
    }
}

impl From<AnyError> for Note {
    fn from(error: AnyError) -> Note {
        error.note()
    }
}

/// The errors that stopped the work: one or more.
#[derive(Debug)]
pub struct Errors(Vec<AnyError>);

impl Errors {
    pub fn push(&mut self, error: impl Into<AnyError>) {
        self.0.push(error.into());
    }

    /// The highest of the errors' exit codes.
    pub fn exit_code(&self) -> u8 {
        self.iter().map(Error::exit_code).max().unwrap_or_default()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Error<dyn Hint>> {
        self.0.iter().map(|error| &**error)
    }

    /// Each error, rendered in `frontend`'s terms and located in `src`, one after
    /// another.
    pub fn render(&self, frontend: Frontend, src: Option<&str>) -> String {
        let rendered: Vec<String> = self.iter().map(|e| e.render(frontend, src)).collect();
        rendered.join("\n")
    }
}

impl<K: Hint + 'static> From<Error<K>> for Errors {
    fn from(error: Error<K>) -> Errors {
        Errors(vec![error.into()])
    }
}

impl<K: Hint + 'static> From<K> for Errors {
    fn from(kind: K) -> Errors {
        Error::new(kind).into()
    }
}

impl From<AnyError> for Errors {
    fn from(error: AnyError) -> Errors {
        Errors(vec![error])
    }
}

/// A result, and the notes gathered on the way to it, whether or not it
/// succeeded.
#[derive(Debug)]
pub struct Report<T> {
    pub result: std::result::Result<T, Errors>,
    pub notes: Vec<Note>,
}

impl<T> Report<T> {
    /// The report of `work`, which pushes its notes as it goes.
    pub fn collect(
        work: impl FnOnce(&mut Vec<Note>) -> std::result::Result<T, Errors>,
    ) -> Report<T> {
        let mut notes = Vec::new();
        let result = work(&mut notes);
        Report { result, notes }
    }

    /// The value and the notes, or the notes then the errors and the
    /// highest exit code; each note and error rendered in `frontend`'s terms,
    /// errors located in `src`.
    pub fn render(
        self,
        frontend: Frontend,
        src: Option<&str>,
    ) -> std::result::Result<(T, Vec<String>), (Vec<String>, u8)> {
        let mut lines: Vec<String> = self
            .notes
            .iter()
            .map(|note| note.render(frontend))
            .collect();
        match self.result {
            Ok(value) => Ok((value, lines)),
            Err(errors) => {
                lines.extend(errors.iter().map(|error| error.render(frontend, src)));
                Err((lines, errors.exit_code()))
            }
        }
    }
}

/// A result with no notes.
impl<T, E: Into<Errors>> From<std::result::Result<T, E>> for Report<T> {
    fn from(result: std::result::Result<T, E>) -> Report<T> {
        Report {
            result: result.map_err(Into::into),
            notes: Vec::new(),
        }
    }
}

/// A result that fails with an error of kind `K`.
pub type Result<T, K> = std::result::Result<T, Error<K>>;

/// [`Error`]'s fixes, applied through a [`Result`].
pub trait ResultExt<K> {
    /// The result, with the fix `fix` gives on an error that has none.
    fn ok_or_fix<F: Into<Fix>>(self, fix: impl FnOnce(&K) -> Option<F>) -> Self;
}

impl<T, K: Hint> ResultExt<K> for Result<T, K> {
    fn ok_or_fix<F: Into<Fix>>(self, fix: impl FnOnce(&K) -> Option<F>) -> Self {
        self.map_err(|error| error.or_fix(fix))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Kind {
        Plain,
        Fixed,
        Parse,
    }

    impl fmt::Display for Kind {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(match self {
                Kind::Plain => "plain problem",
                Kind::Fixed => "fixed problem",
                Kind::Parse => "parse problem",
            })
        }
    }

    impl Hint for Kind {
        fn exit_code(&self) -> u8 {
            match self {
                Kind::Plain => 1,
                Kind::Fixed => 2,
                Kind::Parse => 3,
            }
        }

        fn fix(&self) -> Option<Fix> {
            (*self != Kind::Plain).then(|| Fix::new("give {--force}"))
        }

        fn excerpt(&self) -> bool {
            *self == Kind::Parse
        }
    }

    const FRONTENDS: [Frontend; 3] = [Frontend::Cli, Frontend::Mcp, Frontend::Repl];

    fn candidates() -> Candidates {
        Candidates {
            listed: vec![
                ("fn:a".to_string(), "3".to_string()),
                ("impl:B>fn:a".to_string(), "10-12".to_string()),
            ],
            total: 4,
            shared: 1,
        }
    }

    #[test]
    fn a_new_error_has_its_kinds_fix_and_no_span() {
        let fixed = Error::new(Kind::Fixed);
        assert_eq!(fixed.fix, Some(Fix::new("give {--force}")));
        assert_eq!(fixed.span, None);
        assert_eq!(Error::new(Kind::Plain).fix, None);
        assert_eq!(Error::new(Kind::Plain).at(2..5).span, Some(2..5));
    }

    #[test]
    fn a_fix_is_text_without_candidates_unless_it_has_a_choice() {
        let fix = Fix::new("say");
        assert_eq!((fix.text.as_str(), fix.candidates), ("say", None));
        let fix = Fix::choose("pick", candidates());
        assert_eq!(fix.text, "pick");
        assert_eq!(fix.candidates, Some(candidates()));
    }

    #[test]
    fn or_fix_fills_in_a_missing_fix_and_keeps_one_already_there() {
        let filled = Error::new(Kind::Plain).or_fix(|kind| {
            assert_eq!(*kind, Kind::Plain);
            Some(Fix::new("fallback"))
        });
        assert_eq!(filled.fix, Some(Fix::new("fallback")));
        let kept = Error::new(Kind::Fixed).or_fix(|_| Some(Fix::new("fallback")));
        assert_eq!(kept.fix, Some(Fix::new("give {--force}")));
        let none = Error::new(Kind::Plain).or_fix(|_| None::<Fix>);
        assert_eq!(none.fix, None);
    }

    #[test]
    fn with_fix_replaces_the_fix_or_adds_one() {
        let replaced = Error::new(Kind::Fixed).with_fix("better");
        assert_eq!(replaced.fix, Some(Fix::new("better")));
        let added = Error::new(Kind::Plain).with_fix(format!("{}er", "bett"));
        assert_eq!(added.fix, Some(Fix::new("better")));
    }

    #[test]
    fn and_fix_follows_the_fix_or_adds_one() {
        let joined = Error::new(Kind::Fixed).and_fix("then this");
        assert_eq!(joined.fix, Some(Fix::new("give {--force}; then this")));
        let added = Error::new(Kind::Plain).and_fix("this");
        assert_eq!(added.fix, Some(Fix::new("this")));
    }

    #[test]
    fn at_locates_a_kinds_error() {
        let error = Kind::Fixed.at(2..5);
        assert_eq!(error.span, Some(2..5));
        assert_eq!(error.fix, Some(Fix::new("give {--force}")));
    }

    #[test]
    fn ok_or_fix_fixes_only_an_error_without_a_fix() {
        let ok: Result<u8, Kind> = Ok(1);
        assert_eq!(
            ok.ok_or_fix(|_| -> Option<Fix> { panic!("an ok result needs no fix") }),
            Ok(1)
        );
        let failed: Result<u8, Kind> = Err(Error::new(Kind::Plain));
        let fixed = failed.ok_or_fix(|kind| {
            assert_eq!(*kind, Kind::Plain);
            Some(Fix::new("fallback"))
        });
        assert_eq!(fixed.unwrap_err().fix, Some(Fix::new("fallback")));
        let failed: Result<u8, Kind> = Err(Error::new(Kind::Fixed));
        let kept = failed.ok_or_fix(|_| Some(Fix::new("fallback")));
        assert_eq!(kept.unwrap_err().fix, Some(Fix::new("give {--force}")));
    }

    #[test]
    fn an_error_without_a_fix_has_no_separator() {
        for frontend in FRONTENDS {
            let error = Error::new(Kind::Plain);
            assert_eq!(error.render(frontend, None), "error: plain problem");
        }
    }

    #[test]
    fn a_located_error_names_its_script_line_and_column() {
        let src = "show 1\ndelete fn:x";
        let error = Error::new(Kind::Plain).at(14..18);
        assert_eq!(
            error.render(Frontend::Cli, Some(src)),
            "error: script:2:8: plain problem"
        );
        assert_eq!(
            error.render(Frontend::Cli, None),
            "error: plain problem",
            "without the script, there's no location"
        );
        assert_eq!(
            Error::new(Kind::Plain).render(Frontend::Cli, Some(src)),
            "error: plain problem",
            "without a span, there's no location"
        );
    }

    #[test]
    fn the_fix_follows_the_problem_in_the_frontends_terms() {
        let error = Error::new(Kind::Fixed);
        let rendered = FRONTENDS.map(|frontend| error.render(frontend, None));
        assert_eq!(
            rendered,
            [
                "error: fixed problem; give --force",
                "error: fixed problem; give `force`",
                "error: fixed problem; give `ned repl --force`",
            ]
        );
    }

    #[test]
    fn a_frontends_text_is_left_out_of_the_others() {
        let template = "run it{cli: with -e}{mcp: as `script`}{repl: at the prompt}";
        let rendered = FRONTENDS.map(|frontend| frontend.render(template));
        assert_eq!(
            rendered,
            [
                "run it with -e",
                "run it as `script`",
                "run it at the prompt"
            ]
        );
    }

    #[test]
    fn braces_that_arent_placeholders_are_kept() {
        assert_eq!(
            Frontend::Mcp.render("{path} {-w} ${} ${name} {"),
            "{path} `workspace` ${} ${name} {"
        );
        assert_eq!(Frontend::Repl.render("{path} {-w} {"), "{path} -w {");
        assert_eq!(Frontend::Mcp.render("{a {-w}"), "{a `workspace`");
    }

    #[test]
    fn candidates_follow_the_fix_aligned_one_per_line() {
        let error = Error::new(Kind::Plain)
            .or_fix(|_| Some(Fix::choose("add `all` or use one of:", candidates())));
        assert_eq!(
            error.render(Frontend::Cli, None),
            "error: plain problem; add `all` or use one of:\n  fn:a          3\n  impl:B>fn:a   10-12\n  … and 1 more\n  1 more share a line with another match; select longer text to pick one"
        );
    }

    #[test]
    fn every_candidate_listed_leaves_no_count() {
        let listed = Candidates {
            total: 2,
            shared: 0,
            ..candidates()
        };
        let error = Error::new(Kind::Plain).or_fix(|_| Some(Fix::choose("use one of:", listed)));
        assert_eq!(
            error.render(Frontend::Cli, None),
            "error: plain problem; use one of:\n  fn:a          3\n  impl:B>fn:a   10-12"
        );
        let empty = Candidates {
            listed: Vec::new(),
            total: 0,
            shared: 0,
        };
        let error = Error::new(Kind::Plain).or_fix(|_| Some(Fix::choose("add `all`", empty)));
        assert_eq!(
            error.render(Frontend::Cli, None),
            "error: plain problem; add `all`"
        );
    }

    #[test]
    fn a_kind_with_an_excerpt_shows_the_script_line_last() {
        let src = "show (";
        let error = Error::new(Kind::Parse).at(5..6).with_fix(Fix::choose(
            "use one of:",
            Candidates {
                total: 2,
                shared: 0,
                ..candidates()
            },
        ));
        assert_eq!(
            error.render(Frontend::Mcp, Some(src)),
            "error: script:1:6: parse problem; use one of:\n  fn:a          3\n  impl:B>fn:a   10-12\n1:show (\n       ^"
        );
        assert_eq!(
            error.render(Frontend::Mcp, None),
            "error: parse problem; use one of:\n  fn:a          3\n  impl:B>fn:a   10-12",
            "without the script, there's no excerpt"
        );
        let other = Error::new(Kind::Fixed).at(5..6);
        assert_eq!(
            other.render(Frontend::Cli, Some(src)),
            "error: script:1:6: fixed problem; give --force"
        );
    }

    fn failing(kind: Kind) -> Result<u8, Kind> {
        Err(kind)?
    }

    #[test]
    fn a_kind_converts_into_its_error_with_its_fix() {
        let error: Error<Kind> = Kind::Fixed.into();
        assert_eq!(error, Error::new(Kind::Fixed));
        assert_eq!(failing(Kind::Plain), Err(Error::new(Kind::Plain)));
    }

    #[test]
    fn an_error_exits_with_its_kinds_code_erased_or_not() {
        assert_eq!(Error::new(Kind::Fixed).exit_code(), 2);
        let any: AnyError = Error::new(Kind::Parse).at(5..6).into();
        assert_eq!(any.exit_code(), 3);
        assert_eq!(
            any.render(Frontend::Mcp, Some("show (")),
            "error: script:1:6: parse problem; give `force`\n1:show (\n       ^"
        );
    }

    #[test]
    fn a_detail_follows_the_candidates_as_written() {
        let fix = Fix::choose("use {-w} or one of:", candidates()).then("{-w} {cli:x}\n  line 2");
        let error = Error::new(Kind::Parse).at(5..6).with_fix(fix);
        assert_eq!(
            error.render(Frontend::Mcp, Some("show (")),
            "error: script:1:6: parse problem; use `workspace` or one of:\n  fn:a          3\n  impl:B>fn:a   10-12\n  … and 1 more\n  1 more share a line with another match; select longer text to pick one\n{-w} {cli:x}\n  line 2\n1:show (\n       ^"
        );
        let plain = Error::new(Kind::Plain).with_fix(Fix::new("see below").then("{--force}"));
        assert_eq!(
            plain.render(Frontend::Cli, None),
            "error: plain problem; see below\n{--force}"
        );
    }

    #[test]
    fn display_is_the_clis_message_without_the_prefix() {
        let error = Error::new(Kind::Fixed).at(0..1);
        assert_eq!(error.to_string(), "fixed problem; give --force");
        assert_eq!(error.message(Frontend::Cli, None), error.to_string());
        let src = Some("show 1");
        for frontend in FRONTENDS {
            assert_eq!(
                error.render(frontend, src),
                format!("error: {}", error.message(frontend, src))
            );
        }
        let boxed: Box<dyn std::error::Error> = Box::new(Error::new(Kind::Plain));
        assert_eq!(boxed.to_string(), "plain problem");
    }

    #[test]
    fn a_note_is_text_or_an_error_worked_around_without_its_place() {
        assert_eq!(Note::from("plain").render(Frontend::Mcp), "note: plain");
        assert_eq!(
            Note::from(format!("{} text", "owned")),
            Note {
                text: "owned text".into(),
                fix: None
            }
        );
        let demoted = Note::from(Error::new(Kind::Fixed).at(2..5));
        let rendered = FRONTENDS.map(|frontend| demoted.render(frontend));
        assert_eq!(
            rendered,
            [
                "note: fixed problem; give --force",
                "note: fixed problem; give `force`",
                "note: fixed problem; give `ned repl --force`",
            ]
        );
        let any: AnyError = Error::new(Kind::Plain)
            .with_fix(Fix::new("then").then("{-w}"))
            .into();
        assert_eq!(
            Note::from(any).render(Frontend::Mcp),
            "note: plain problem; then\n{-w}"
        );
    }

    #[test]
    fn errors_exit_with_the_highest_code() {
        let mut errors = Errors::from(Error::new(Kind::Fixed));
        assert_eq!(errors.exit_code(), 2);
        errors.push(Error::new(Kind::Parse));
        errors.push(Error::new(Kind::Plain));
        assert_eq!(errors.exit_code(), 3);
        assert_eq!(errors.iter().count(), 3);
        let any: AnyError = Error::new(Kind::Plain).into();
        assert_eq!(Errors::from(any).exit_code(), 1);
    }

    #[test]
    fn a_report_renders_its_notes_with_its_value() {
        let report = Report::collect(|notes| {
            notes.push("first".into());
            notes.push(Error::new(Kind::Fixed).into());
            Ok(7)
        });
        assert_eq!(
            report.render(Frontend::Mcp, None).unwrap(),
            (
                7,
                vec![
                    "note: first".to_string(),
                    "note: fixed problem; give `force`".to_string()
                ]
            )
        );
        let quiet = Report::collect(|_| Ok("done"));
        assert_eq!(
            quiet.render(Frontend::Cli, None).unwrap(),
            ("done", Vec::new())
        );
    }

    #[test]
    fn a_failed_report_renders_its_notes_then_each_error() {
        let report: Report<u8> = Report::collect(|notes| {
            notes.push("skipped".into());
            let mut errors = Errors::from(Error::new(Kind::Plain));
            errors.push(Error::new(Kind::Parse).at(5..6));
            Err(errors)
        });
        assert_eq!(
            report.render(Frontend::Cli, Some("show (")).unwrap_err(),
            (
                vec![
                    "note: skipped".to_string(),
                    "error: plain problem".to_string(),
                    "error: script:1:6: parse problem; give --force\n1:show (\n       ^"
                        .to_string(),
                ],
                3
            )
        );
        let failed: Report<u8> = Report::collect(|_| {
            let value = failing(Kind::Fixed)?;
            Ok(value)
        });
        assert_eq!(
            failed.render(Frontend::Mcp, None).unwrap_err(),
            (vec!["error: fixed problem; give `force`".to_string()], 2)
        );
    }

    #[test]
    fn verbatim_text_in_a_template_renders_as_written() {
        assert_eq!(verbatim("a{b}{"), "a{{b}{{");
        let template = format!("it has `{}`; use {{-w}}", verbatim("{-w} {cli:x} {"));
        let rendered = FRONTENDS.map(|frontend| frontend.render(&template));
        assert_eq!(
            rendered,
            [
                "it has `{-w} {cli:x} {`; use -w",
                "it has `{-w} {cli:x} {`; use `workspace`",
                "it has `{-w} {cli:x} {`; use -w",
            ]
        );
        assert_eq!(Frontend::Cli.render("{{-w} {path}"), "{-w} {path}");
    }

    #[test]
    fn map_kind_keeps_the_place_and_fix_or_takes_the_new_kinds() {
        let kept = Error::new(Kind::Plain)
            .at(1..2)
            .with_fix("own")
            .map_kind(|kind| {
                assert_eq!(kind, Kind::Plain);
                Kind::Fixed
            });
        assert_eq!(
            (kept.kind, kept.span, kept.fix),
            (Kind::Fixed, Some(1..2), Some(Fix::new("own")))
        );
        let taken = Error::new(Kind::Plain).map_kind(|_| Kind::Fixed);
        assert_eq!(taken.fix, Some(Fix::new("give {--force}")));
    }

    #[test]
    fn a_span_or_fix_may_be_absent() {
        assert_eq!(Kind::Plain.at(None).span, None);
        assert_eq!(Kind::Plain.at(Some(1..2)).span, Some(1..2));
        assert_eq!(Error::new(Kind::Plain).at(2..3).at(None).span, None);
        let kept = Error::new(Kind::Fixed).with_fix(None::<Fix>);
        assert_eq!(kept.fix, Some(Fix::new("give {--force}")));
        let replaced = Error::new(Kind::Fixed).with_fix(Some("other"));
        assert_eq!(replaced.fix, Some(Fix::new("other")));
    }

    #[test]
    fn joined_fixes_keep_the_first_choice_and_both_details() {
        let joined = Fix::new("a")
            .then("one")
            .and(Fix::choose("b", candidates()).then("two"));
        assert_eq!(
            joined,
            Fix {
                text: "a; b".into(),
                candidates: Some(candidates()),
                detail: Some("one\ntwo".into()),
            }
        );
        let first = Fix::choose("a", candidates()).and(Fix::choose(
            "b",
            Candidates {
                total: 2,
                ..candidates()
            },
        ));
        assert_eq!(first.candidates, Some(candidates()));
        assert_eq!(Fix::new("a").and("b"), Fix::new("a; b"));
    }

    #[test]
    fn a_notes_context_comes_first() {
        let note = Note::from(Error::new(Kind::Fixed)).context("not recorded in session s");
        assert_eq!(
            note.render(Frontend::Mcp),
            "note: not recorded in session s: fixed problem; give `force`"
        );
    }

    #[test]
    fn a_result_is_a_report_without_notes() {
        let report = Report::from(failing(Kind::Fixed));
        assert!(report.notes.is_empty());
        assert_eq!(report.result.unwrap_err().exit_code(), 2);
        let ok: Report<u8> = Ok::<u8, Error<Kind>>(1).into();
        assert_eq!(ok.render(Frontend::Cli, None).unwrap(), (1, Vec::new()));
    }

    #[test]
    fn session_commit_and_language_options_in_each_frontends_terms() {
        let template = "{--commit} {-s NAME} {ned history} {ned undo} {--lang}";
        let rendered = FRONTENDS.map(|frontend| frontend.render(template));
        assert_eq!(
            rendered,
            [
                "--commit -s NAME or NED_SESSION `ned history` `ned undo` --lang",
                "`commit` restart the server with -s NAME `history` `undo` `ned mcp --lang`",
                "`:commit` restart the REPL with -s NAME `:history` `ned undo` `ned repl --lang`",
            ]
        );
    }

    #[test]
    fn a_kind_converts_straight_into_errors() {
        fn stops() -> std::result::Result<(), Errors> {
            Err(Kind::Fixed)?
        }
        assert_eq!(stops().unwrap_err().exit_code(), 2);
        let any: AnyError = Kind::Parse.into();
        assert_eq!(any.exit_code(), 3);
        let mut errors = Errors::from(Kind::Plain);
        errors.push(Kind::Fixed);
        assert_eq!(errors.iter().count(), 2);
    }

    #[test]
    fn errors_render_one_after_another() {
        let mut errors = Errors::from(Kind::Plain);
        errors.push(Error::new(Kind::Parse).at(5..6));
        assert_eq!(
            errors.render(Frontend::Mcp, Some("show (")),
            "error: plain problem\nerror: script:1:6: parse problem; give `force`\n1:show (\n       ^"
        );
    }
}
