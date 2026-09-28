//! Running scripts against files, and the errors that can stop a run.

use std::fmt;
use std::fs;
use std::ops::Range;

use crate::edit::{Edit, EditError, EditSet};
use crate::script::Script;
use crate::script::ast::{Command, CommandKind, Pattern, Position, Target, Text, TextKind};
use crate::script::error::location;
use crate::select::{self, Match, SourceFile, line_numbers, same_path};
use crate::text;

/// The result of running a script: the output of the reads that ran, in
/// command order, and either every modified file or the error that rejected
/// the script.
#[derive(Debug)]
pub struct Run {
    pub output: String,
    pub result: Result<Vec<Change>, ExecError>,
}

/// A modified file, with the number of spans edited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    pub old: String,
    pub new: String,
    pub edits: usize,
}

/// Runs `script` (parsed from `src`) with `files` as the initial file set.
/// Nothing is written.
pub fn run(script: &Script, src: &str, files: &[String]) -> Run {
    let mut executor = Executor {
        src,
        files: Vec::new(),
        set: Vec::new(),
        output: String::new(),
    };
    let result = executor.run(script, files);
    Run {
        output: executor.output,
        result,
    }
}

struct Loaded {
    file: SourceFile,
    edits: EditSet,
}

struct Executor<'s> {
    src: &'s str,
    /// Every file loaded so far, in order of first appearance.
    files: Vec<Loaded>,
    /// The current file set, as indices into `files`.
    set: Vec<usize>,
    output: String,
}

impl Executor<'_> {
    fn run(&mut self, script: &Script, initial: &[String]) -> Result<Vec<Change>, ExecError> {
        self.set = initial
            .iter()
            .map(|path| self.load(path, None))
            .collect::<Result<_, _>>()?;
        for (index, command) in script.commands.iter().enumerate() {
            self.command(index, command)?;
        }
        Ok(self
            .files
            .iter()
            .filter(|l| !l.edits.is_empty())
            .map(|l| Change {
                path: l.file.path.clone(),
                old: l.file.text.clone(),
                new: l.edits.apply(),
                edits: l.edits.len(),
            })
            .collect())
    }

    fn load(&mut self, path: &str, span: Option<Range<usize>>) -> Result<usize, ExecError> {
        if let Some(i) = self
            .files
            .iter()
            .position(|l| same_path(&l.file.path, path))
        {
            return Ok(i);
        }
        let io = |message: String| {
            ExecError::new(
                ExecErrorKind::Io {
                    path: path.into(),
                    message,
                },
                span.clone(),
            )
        };
        let bytes = fs::read(path).map_err(|e| io(e.to_string()))?;
        let text = String::from_utf8(bytes).map_err(|_| io("not valid UTF-8".into()))?;
        let file = SourceFile::new(path, text);
        let edits = EditSet::new(&file.buffer);
        self.files.push(Loaded { file, edits });
        Ok(self.files.len() - 1)
    }

    fn command(&mut self, index: usize, command: &Command) -> Result<(), ExecError> {
        let span = &command.span;
        let error = |kind| ExecError::new(kind, Some(span.clone()));
        match &command.kind {
            CommandKind::File(paths) => {
                self.set = paths
                    .iter()
                    .map(|path| self.load(path, Some(span.clone())))
                    .collect::<Result<_, _>>()?;
                return Ok(());
            }
            CommandKind::Outline(_) => {
                return Err(error(ExecErrorKind::Unsupported("`outline`".into())));
            }
            CommandKind::Move { .. } => {
                return Err(error(ExecErrorKind::Unsupported("`move`".into())));
            }
            _ if self.set.is_empty() => return Err(error(ExecErrorKind::NoFiles)),
            _ => {}
        }
        match &command.kind {
            CommandKind::Show(target) => self.show(target.as_ref())?,
            CommandKind::Replace { target, text } => {
                for m in self.resolve(target)? {
                    let (range, new) = replace(&self.files[m.file].file, m.range, text);
                    self.push(index, span, m.file, range, new)?;
                }
            }
            CommandKind::Insert {
                position,
                target,
                text,
            } => {
                for m in self.resolve(target)? {
                    let (at, new) = insert(&self.files[m.file].file, m.range, *position, text);
                    self.push(index, span, m.file, at..at, new)?;
                }
            }
            CommandKind::Delete(target) => {
                for m in self.resolve(target)? {
                    let range = delete(&self.files[m.file].file, m.range);
                    self.push(index, span, m.file, range, String::new())?;
                }
            }
            CommandKind::Sub {
                scope,
                pattern,
                text,
            } => self.sub(index, span, scope.as_ref(), pattern, text)?,
            CommandKind::File(_) | CommandKind::Outline(_) | CommandKind::Move { .. } => {
                unreachable!("handled above")
            }
        }
        Ok(())
    }

    /// Resolves `target` in the current file set, returning matches whose
    /// `file` indexes `self.files`.
    fn resolve(&self, target: &Target) -> Result<Vec<Match>, ExecError> {
        let set: Vec<&SourceFile> = self.set.iter().map(|&i| &self.files[i].file).collect();
        let matches = select::resolve(target, &set, self.src)?;
        Ok(matches
            .into_iter()
            .map(|m| Match {
                file: self.set[m.file],
                range: m.range,
            })
            .collect())
    }

    fn push(
        &mut self,
        index: usize,
        span: &Range<usize>,
        file: usize,
        range: Range<usize>,
        text: String,
    ) -> Result<(), ExecError> {
        let loaded = &mut self.files[file];
        let edit = Edit {
            range,
            text,
            command: index,
        };
        loaded.edits.push(edit).map_err(|err| match err {
            EditError::Overlap { first, range, .. } => ExecError::new(
                ExecErrorKind::Overlap {
                    command: first + 1,
                    location: format!(
                        "{}:{}",
                        loaded.file.path,
                        line_numbers(&loaded.file.buffer, &range)
                    ),
                },
                Some(span.clone()),
            ),
            EditError::Buffer(err) => unreachable!("edits come from resolved spans: {err}"),
        })
    }

    fn show(&mut self, target: Option<&Target>) -> Result<(), ExecError> {
        // (file, first line, last line), 0-based.
        let mut spans: Vec<(usize, usize, usize)> = Vec::new();
        match target {
            None => {
                for &i in &self.set {
                    let count = self.files[i].file.buffer.line_count();
                    if count > 0 {
                        spans.push((i, 0, count - 1));
                    }
                }
            }
            Some(target) => {
                for m in self.resolve(target)? {
                    let buffer = &self.files[m.file].file.buffer;
                    let max = buffer.line_count().saturating_sub(1);
                    let line = |offset| buffer.byte_to_line(offset).unwrap_or(max).min(max);
                    let first = line(m.range.start);
                    let last = if m.range.is_empty() {
                        first
                    } else {
                        line(m.range.end - 1)
                    };
                    spans.push((m.file, first, last));
                }
            }
        }
        let mut regions: Vec<(usize, usize, usize)> = Vec::new();
        for (file, first, last) in spans {
            match regions.last_mut() {
                Some((f, _, end)) if *f == file && first <= *end + 2 => *end = (*end).max(last),
                _ => regions.push((file, first, last)),
            }
        }
        for (file, first, last) in regions {
            let f = &self.files[file].file;
            let lines = if first == last {
                format!("{}", first + 1)
            } else {
                format!("{}-{}", first + 1, last + 1)
            };
            self.output.push_str(&format!("{}:{lines}\n", f.path));
            for line in first..=last {
                let range = f.buffer.line_range(line).expect("line within the file");
                let content = f.text[range].trim_end_matches('\n');
                let content = content.strip_suffix('\r').unwrap_or(content);
                self.output.push_str(&format!("{}:{content}\n", line + 1));
            }
        }
        Ok(())
    }

    fn sub(
        &mut self,
        index: usize,
        span: &Range<usize>,
        scope: Option<&Target>,
        pattern: &Pattern,
        text: &Text,
    ) -> Result<(), ExecError> {
        let regex = pattern
            .regex()
            .expect("regexes are validated when the script is parsed");
        let scopes = match scope {
            Some(target) => self.resolve(target)?,
            None => self
                .set
                .iter()
                .map(|&file| Match {
                    file,
                    range: 0..self.files[file].file.text.len(),
                })
                .collect(),
        };
        let mut total = 0;
        for scope in scopes {
            let haystack = &self.files[scope.file].file.text[scope.range.clone()];
            let edits: Vec<(Range<usize>, String)> = regex
                .captures_iter(haystack)
                .map(|caps| {
                    let whole = caps.get(0).expect("group 0 always matches");
                    let mut expanded = String::new();
                    caps.expand(&text.value, &mut expanded);
                    let start = scope.range.start;
                    (start + whole.start()..start + whole.end(), expanded)
                })
                .collect();
            for (range, expanded) in edits {
                self.push(index, span, scope.file, range, expanded)?;
                total += 1;
            }
        }
        if total == 0 {
            let flags = [
                (pattern.flags.case_insensitive, "i"),
                (pattern.flags.dot_all, "s"),
            ]
            .iter()
            .filter_map(|(on, flag)| on.then_some(*flag))
            .collect::<String>();
            return Err(ExecError::new(
                ExecErrorKind::NoMatch {
                    selector: format!("/{}/{flags}", pattern.source.replace('/', "\\/")),
                    files: self
                        .set
                        .iter()
                        .map(|&i| self.files[i].file.path.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                },
                Some(span.clone()),
            ));
        }
        Ok(())
    }
}

/// The span and text that replace `range` (§5.1).
fn replace(f: &SourceFile, range: Range<usize>, new: &Text) -> (Range<usize>, String) {
    let t = &f.text;
    let unit = text::indent_unit(t);
    if text::is_whole_line(t, &range) {
        let full = text::full_lines(t, range);
        let mut new = line_oriented(new, text::indent_at(t, full.start), &unit);
        if !t[..full.end].ends_with('\n') {
            new.pop();
        }
        (full, new)
    } else {
        let indent = text::indent_at(t, range.start);
        (range, verbatim(new, indent, &unit))
    }
}

/// The offset and text of an insertion at `position` of `range` (§4.2, §5).
fn insert(f: &SourceFile, range: Range<usize>, position: Position, new: &Text) -> (usize, String) {
    let t = &f.text;
    let unit = text::indent_unit(t);
    if !text::is_whole_line(t, &range) {
        let at = match position {
            Position::Before | Position::Start => range.start,
            Position::After | Position::End => range.end,
        };
        return (at, verbatim(new, text::indent_at(t, range.start), &unit));
    }
    let full = text::full_lines(t, range);
    let first = text::indent_at(t, full.start);
    let inner = text::first_indent(t, full.clone()).unwrap_or(first);
    let (at, indent) = match position {
        Position::Before => (full.start, first),
        Position::After => (full.end, first),
        Position::Start => (full.start, inner),
        Position::End => (full.end, inner),
    };
    let mut new = line_oriented(new, indent, &unit);
    if at == full.end && !full.is_empty() && !t[..at].ends_with('\n') {
        // The span's last line has no line ending; give it one instead.
        new.pop();
        new.insert(0, '\n');
    }
    (at, new)
}

/// The span removed by deleting `range`: whole lines, tidied, or the span
/// itself.
fn delete(f: &SourceFile, range: Range<usize>) -> Range<usize> {
    if text::is_whole_line(&f.text, &range) {
        text::tidy_delete(&f.text, text::full_lines(&f.text, range))
    } else {
        range
    }
}

/// `new` as line-oriented text: re-based (unless raw), with a final newline.
fn line_oriented(new: &Text, indent: &str, unit: &str) -> String {
    let mut out = match new.kind {
        TextKind::RawHeredoc => new.value.clone(),
        TextKind::Str | TextKind::Heredoc => text::rebase(&new.value, indent, unit),
    };
    out.push('\n');
    out
}

/// `new` inserted into a partial line: later lines re-based (unless raw).
fn verbatim(new: &Text, indent: &str, unit: &str) -> String {
    match new.kind {
        TextKind::RawHeredoc => new.value.clone(),
        TextKind::Str | TextKind::Heredoc => text::rebase_tail(&new.value, indent, unit),
    }
}

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
    #[error("edit overlaps command {command} at {location}")]
    Overlap { command: usize, location: String },
    #[error("no files to edit; pass FILE arguments or use `file PATH`")]
    NoFiles,
    #[error("cannot read {path}: {message}")]
    Io { path: String, message: String },
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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::script::parse;

    const TEXT: &str =
        "fn a() {\n    let x = 1;\n    let y = 2;\n}\n\nfn b() {\n    let x = 3;\n}\n";

    /// The outcome of running a script, with the temporary directory removed
    /// from every path.
    struct Outcome {
        output: String,
        result: Result<Vec<Change>, String>,
    }

    impl Outcome {
        /// The new contents of the only modified file.
        fn new_text(&self) -> &str {
            let changes = self.result.as_ref().unwrap();
            assert_eq!(changes.len(), 1, "{changes:?}");
            &changes[0].new
        }

        fn error(&self) -> &str {
            self.result.as_ref().unwrap_err()
        }
    }

    /// Writes `files` to a temporary directory and runs `script` with the
    /// first `initial` of them as the file set. `{dir}` in the script is
    /// replaced by the directory.
    fn exec_with(files: &[(&str, &str)], initial: usize, script: &str) -> Outcome {
        let dir = tempfile::tempdir().unwrap();
        let root = format!("{}/", dir.path().display());
        for (name, text) in files {
            fs::write(dir.path().join(name), text).unwrap();
        }
        let paths: Vec<String> = files[..initial]
            .iter()
            .map(|(name, _)| format!("{root}{name}"))
            .collect();
        let src = script.replace("{dir}/", &root);
        let parsed = parse(&src).unwrap();
        let run = run(&parsed, &src, &paths);
        let strip = |s: &str| s.replace(&root, "");
        Outcome {
            output: strip(&run.output),
            result: match run.result {
                Ok(changes) => Ok(changes
                    .into_iter()
                    .map(|c| Change {
                        path: strip(&c.path),
                        ..c
                    })
                    .collect()),
                Err(err) => Err(strip(&err.render(&src))),
            },
        }
    }

    fn exec(text: &str, script: &str) -> Outcome {
        exec_with(&[("a.rs", text)], 1, script)
    }

    fn edited(text: &str, script: &str) -> String {
        exec(text, script).new_text().to_string()
    }

    #[test]
    fn show_prints_numbered_lines() {
        let out = exec(TEXT, "show 2-3");
        assert_eq!(out.output, "a.rs:2-3\n2:    let x = 1;\n3:    let y = 2;\n");
        assert_eq!(out.result, Ok(vec![]));
        assert_eq!(exec("a\r\nb\r\n", "show $").output, "a.rs:2\n2:b\n");
    }

    #[test]
    fn show_without_selector_prints_every_file() {
        let out = exec_with(&[("a.rs", "x\ny\n"), ("b.rs", "z")], 2, "show");
        assert_eq!(out.output, "a.rs:1-2\n1:x\n2:y\nb.rs:1\n1:z\n");
    }

    #[test]
    fn show_merges_regions_within_one_line() {
        assert_eq!(
            exec(TEXT, "show all /let/").output,
            "a.rs:2-3\n2:    let x = 1;\n3:    let y = 2;\na.rs:7\n7:    let x = 3;\n"
        );
        assert_eq!(
            exec("a\nb\na\nc\nd\na\n", "show all /a/").output,
            "a.rs:1-3\n1:a\n2:b\n3:a\na.rs:6\n6:a\n"
        );
    }

    #[test]
    fn show_prints_the_whole_lines_of_partial_spans() {
        assert_eq!(
            exec(TEXT, "show \"2;\\n}\"").output,
            "a.rs:3-4\n3:    let y = 2;\n4:}\n"
        );
    }

    #[test]
    fn replace_partial_span_verbatim() {
        let out = exec(TEXT, "replace \"= 1\" with \"= 10\"");
        let changes = out.result.unwrap();
        assert_eq!(changes[0].path, "a.rs");
        assert_eq!(changes[0].old, TEXT);
        assert_eq!(changes[0].new, TEXT.replace("= 1;", "= 10;"));
        assert_eq!(changes[0].edits, 1);
    }

    #[test]
    fn replace_whole_lines_rebases_text() {
        assert_eq!(
            edited(TEXT, "replace 2 with \"let z = 0;\""),
            TEXT.replace("let x = 1;", "let z = 0;")
        );
        assert_eq!(
            edited(TEXT, "replace 2-3 with <<END\nif a {\n    b();\n}\nEND\n"),
            "fn a() {\n    if a {\n        b();\n    }\n}\n\nfn b() {\n    let x = 3;\n}\n"
        );
    }

    #[test]
    fn replace_with_raw_heredoc_is_verbatim() {
        assert_eq!(
            edited(TEXT, "replace 2-3 with <<'END'\nz();\nEND\n"),
            "fn a() {\nz();\n}\n\nfn b() {\n    let x = 3;\n}\n"
        );
    }

    #[test]
    fn replace_last_line_keeps_missing_final_newline() {
        assert_eq!(edited("a\nb", "replace $ with \"c\""), "a\nc");
    }

    #[test]
    fn replace_multi_line_text_in_partial_span() {
        assert_eq!(
            edited(TEXT, "replace \"2\" with \"vec![\\n1,\\n]\""),
            TEXT.replace("2;", "vec![\n    1,\n    ];")
        );
    }

    #[test]
    fn replace_never_expands_dollars() {
        assert_eq!(
            edited(TEXT, "replace \"= 1\" with \"= $0\""),
            TEXT.replace("= 1", "= $0")
        );
    }

    #[test]
    fn insert_around_whole_lines() {
        assert_eq!(
            edited(TEXT, "insert after 2 \"let w = 0;\""),
            TEXT.replace("1;\n", "1;\n    let w = 0;\n")
        );
        assert_eq!(
            edited(TEXT, "insert before 1 \"// top\""),
            format!("// top\n{TEXT}")
        );
        assert_eq!(
            edited(TEXT, "insert start 2-3 \"let w = 0;\""),
            TEXT.replace("    let x = 1;", "    let w = 0;\n    let x = 1;")
        );
        assert_eq!(
            edited(TEXT, "insert end 2-3 \"let w = 0;\""),
            TEXT.replace("2;\n", "2;\n    let w = 0;\n")
        );
    }

    #[test]
    fn insert_after_last_line_without_final_newline() {
        assert_eq!(edited("a\nb", "insert after $ \"c\""), "a\nb\nc");
    }

    #[test]
    fn insert_at_partial_span_is_verbatim() {
        assert_eq!(
            edited(TEXT, "insert after \"let y\" \": u8\""),
            TEXT.replace("let y", "let y: u8")
        );
        assert_eq!(
            edited(TEXT, "insert start \"y = 2\" \"mut \""),
            TEXT.replace("let y", "let mut y")
        );
    }

    #[test]
    fn insert_into_python_block() {
        let app = "def handle(req):\n    if req.ok:\n        log(req)\n        return 200\n    return 500\n";
        assert_eq!(
            edited(
                app,
                "insert after \"log(req)\".lines <<END\nif req.slow:\n    warn(req)\nEND\n"
            ),
            "def handle(req):\n    if req.ok:\n        log(req)\n        if req.slow:\n            warn(req)\n        return 200\n    return 500\n"
        );
    }

    #[test]
    fn inserted_text_takes_the_files_indent_style() {
        assert_eq!(
            edited(
                "fn a() {\n\tx();\n}\n",
                "insert after 2 <<END\nif y {\n    z();\n}\nEND\n"
            ),
            "fn a() {\n\tx();\n\tif y {\n\t\tz();\n\t}\n}\n"
        );
    }

    #[test]
    fn edits_keep_crlf_line_endings() {
        let text = "a {\r\n    b\r\n}\r\n";
        assert_eq!(
            edited(text, "insert after 2 <<END\nc\nd\nEND\n"),
            "a {\r\n    b\r\n    c\r\n    d\r\n}\r\n"
        );
        assert_eq!(
            edited(text, "replace 2 with \"e\""),
            "a {\r\n    e\r\n}\r\n"
        );
    }

    #[test]
    fn delete_whole_lines_and_tidy_blank_lines() {
        assert_eq!(edited("a\n\nb\n\nc\n", "delete 3"), "a\n\nc\n");
        assert_eq!(
            edited(TEXT, "delete 6-8"),
            "fn a() {\n    let x = 1;\n    let y = 2;\n}\n\n"
        );
        assert_eq!(
            edited(TEXT, "delete \"let y = 2;\""),
            TEXT.replace("    let y = 2;\n", "")
        );
    }

    #[test]
    fn delete_partial_span() {
        assert_eq!(edited(TEXT, "delete \" = 2\""), TEXT.replace(" = 2", ""));
    }

    #[test]
    fn edit_count_is_the_number_of_spans() {
        let out = exec(TEXT, "delete all \"let x\".lines");
        let changes = out.result.unwrap();
        assert_eq!(changes[0].edits, 2);
        assert_eq!(
            changes[0].new,
            "fn a() {\n    let y = 2;\n}\n\nfn b() {\n}\n"
        );
    }

    #[test]
    fn sub_replaces_every_match_in_every_file() {
        let out = exec_with(
            &[
                ("a.rs", "old x old\n"),
                ("b.rs", "none\n"),
                ("c.rs", "old\n"),
            ],
            3,
            "sub /\\bold\\b/ with \"new\"",
        );
        let changes = out.result.unwrap();
        let summary: Vec<_> = changes
            .iter()
            .map(|c| (c.path.as_str(), c.new.as_str(), c.edits))
            .collect();
        assert_eq!(summary, [("a.rs", "new x new\n", 2), ("c.rs", "new\n", 1)]);
    }

    #[test]
    fn sub_expands_captures() {
        assert_eq!(
            edited(TEXT, "sub /let (?<v>\\w)/ with \"let mut ${v}$$\""),
            TEXT.replace("let x", "let mut x$")
                .replace("let y", "let mut y$")
        );
        assert_eq!(
            edited("a=1\n", "sub /(\\w)=(\\d)/ with \"$2=$1 ($0)\""),
            "1=a (a=1)\n"
        );
    }

    #[test]
    fn sub_within_scope() {
        assert_eq!(
            edited(TEXT, "sub 6-8 /x/ with \"q\""),
            TEXT.replace("x = 3", "q = 3")
        );
        assert_eq!(
            edited(TEXT, "sub all /let \\w/ /let/ with \"var\""),
            TEXT.replace("let", "var")
        );
    }

    #[test]
    fn sub_inserts_text_verbatim() {
        assert_eq!(
            edited(TEXT, "sub /1;\\n/ with <<END\n1;\n  // one\nEND\n"),
            TEXT.replace("1;\n", "1;\n  // one")
        );
    }

    #[test]
    fn sub_without_matches_is_an_error() {
        assert_eq!(
            exec(TEXT, "sub /nope/ with \"x\"").error(),
            "error: script:1:1: /nope/ matches nothing in a.rs"
        );
    }

    #[test]
    fn selectors_resolve_against_the_original_text() {
        assert_eq!(
            edited(TEXT, "delete 1\nreplace 3 with \"z();\""),
            "    let x = 1;\n    z();\n}\n\nfn b() {\n    let x = 3;\n}\n"
        );
        let out = exec(TEXT, "delete 2\nshow 2");
        assert_eq!(out.output, "a.rs:2\n2:    let x = 1;\n");
    }

    #[test]
    fn overlapping_edits_name_the_earlier_command() {
        assert_eq!(
            exec(TEXT, "replace 2 with \"a\"\ndelete 2-3").error(),
            "error: script:2:1: edit overlaps command 1 at a.rs:2"
        );
        assert_eq!(
            exec(TEXT, "show 1; delete 2-3; replace \"y\" with \"z\"").error(),
            "error: script:1:21: edit overlaps command 2 at a.rs:2-3"
        );
    }

    #[test]
    fn insertions_at_one_point_apply_in_command_order() {
        assert_eq!(
            edited(TEXT, "insert after 2 \"a;\"; insert before 3 \"b;\""),
            TEXT.replace("1;\n", "1;\n    a;\n    b;\n")
        );
        assert_eq!(
            edited(TEXT, "replace 2 with \"q;\"; insert after 2 \"r;\""),
            TEXT.replace("let x = 1;\n", "q;\n    r;\n")
        );
    }

    #[test]
    fn reads_before_an_error_are_kept() {
        let out = exec(TEXT, "show 1\ndelete /nope/\nshow 2");
        assert_eq!(out.output, "a.rs:1\n1:fn a() {\n");
        assert_eq!(
            out.error(),
            "error: script:2:8: /nope/ matches nothing in a.rs"
        );
    }

    #[test]
    fn file_command_replaces_the_file_set() {
        let out = exec_with(
            &[("a.rs", "x\n"), ("b.rs", "x\n")],
            1,
            "show\nfile {dir}/b.rs\nsub /x/ with \"y\"\nshow",
        );
        assert_eq!(out.output, "a.rs:1\n1:x\nb.rs:1\n1:x\n");
        let changes = out.result.unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(
            (changes[0].path.as_str(), changes[0].new.as_str()),
            ("b.rs", "y\n")
        );
    }

    #[test]
    fn changes_are_listed_in_order_of_first_appearance() {
        let out = exec_with(
            &[("a.rs", "x\n"), ("b.rs", "x\n")],
            0,
            "file {dir}/b.rs\ndelete 1\nfile {dir}/a.rs {dir}/b.rs\ninsert before file:{dir}/a.rs>1 \"z\"",
        );
        let paths: Vec<_> = out.result.unwrap().into_iter().map(|c| c.path).collect();
        assert_eq!(paths, ["b.rs", "a.rs"]);
    }

    #[test]
    fn missing_files_are_io_errors() {
        let out = exec_with(&[("a.rs", "x\n")], 1, "file {dir}/nope.rs");
        assert!(
            out.error()
                .starts_with("error: script:1:1: cannot read nope.rs: "),
            "{}",
            out.error()
        );
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("gone.rs").display().to_string();
        let parsed = parse("show").unwrap();
        let err = run(&parsed, "show", std::slice::from_ref(&missing))
            .result
            .unwrap_err();
        assert!(
            err.render("show")
                .starts_with(&format!("error: cannot read {missing}: "))
        );
    }

    #[test]
    fn non_utf8_files_are_io_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bin.rs");
        fs::write(&path, [0xff, 0xfe, b'\n']).unwrap();
        let path = path.display().to_string();
        let parsed = parse("show").unwrap();
        let err = run(&parsed, "show", std::slice::from_ref(&path))
            .result
            .unwrap_err();
        assert_eq!(
            err.render("show"),
            format!("error: cannot read {path}: not valid UTF-8")
        );
    }

    #[test]
    fn commands_need_files() {
        let parsed = parse("show 1").unwrap();
        let err = run(&parsed, "show 1", &[]).result.unwrap_err();
        assert_eq!(
            err.render("show 1"),
            "error: script:1:1: no files to edit; pass FILE arguments or use `file PATH`"
        );
    }

    #[test]
    fn outline_and_move_are_not_yet_supported() {
        assert_eq!(
            exec(TEXT, "show 1\noutline").error(),
            "error: script:2:1: `outline` is not yet supported"
        );
        assert_eq!(
            exec(TEXT, "move 2 after 3").error(),
            "error: script:1:1: `move` is not yet supported"
        );
    }
}
