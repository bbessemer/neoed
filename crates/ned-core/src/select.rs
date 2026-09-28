//! Resolving selectors to spans of files (command-language spec, §3).

use std::cell::OnceCell;
use std::ops::Range;

use regex::Regex;
use tree_sitter::Tree;

use crate::buffer::{Buffer, LineEnding};
use crate::exec::{Candidates, ExecError, ExecErrorKind as E};
use crate::lang::Language;
use crate::script::ast::{LineNo, Part, Primary, Step, Target, TextKind};
use crate::text::{full_lines, strip_indent};

const MAX_CANDIDATES: usize = 10;

/// A file in the file set, with its path as the user wrote it.
#[derive(Debug, Clone)]
pub struct SourceFile {
    pub path: String,
    pub text: String,
    pub buffer: Buffer,
    pub lang: Option<Language>,
    tree: OnceCell<Tree>,
}

impl SourceFile {
    pub fn new(path: impl Into<String>, text: String, lang: Option<Language>) -> Self {
        let buffer = Buffer::new(&text);
        SourceFile {
            path: path.into(),
            text,
            buffer,
            lang,
            tree: OnceCell::new(),
        }
    }

    /// The syntax tree of the text, parsed on first use; `None` without a
    /// language.
    pub fn tree(&self) -> Option<&Tree> {
        let lang = self.lang?;
        Some(self.tree.get_or_init(|| lang.parse(&self.text)))
    }
}

/// A selected span of `files[file]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub file: usize,
    pub range: Range<usize>,
}

/// Resolves `target` against every file in `files`, enforcing the ambiguity
/// rules of §3.5. `src` is the script, for error messages.
pub fn resolve(target: &Target, files: &[&SourceFile], src: &str) -> Result<Vec<Match>, ExecError> {
    let span = &target.selector.span;
    let error = |kind| ExecError::new(kind, Some(span.clone()));
    let mut matches: Vec<Match> = files
        .iter()
        .enumerate()
        .map(|(file, f)| Match {
            file,
            range: 0..f.text.len(),
        })
        .collect();
    for step in &target.selector.steps {
        matches = resolve_step(step, files, &matches).map_err(error)?;
    }
    let selector = &src[span.clone()];
    match matches.len() {
        0 => Err(error(E::NoMatch {
            selector: selector.into(),
            files: files
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        })),
        1 => Ok(matches),
        _ if target.all => Ok(matches),
        _ => Err(error(E::Ambiguous {
            selector: selector.into(),
            candidates: candidates(&matches, files, selector),
        })),
    }
}

fn resolve_step(step: &Step, files: &[&SourceFile], parents: &[Match]) -> Result<Vec<Match>, E> {
    for part in &step.parts {
        if *part != Part::Lines {
            return Err(E::Unsupported(format!("part `.{}`", part_name(*part))));
        }
    }
    let matcher = Matcher::new(&step.primary, files, parents)?;
    let mut out: Vec<Match> = Vec::new();
    for parent in parents {
        let f = &files[parent.file];
        for mut range in matcher.find(f, parent.range.clone()) {
            if !step.parts.is_empty() {
                range = full_lines(&f.text, range);
            }
            let m = Match {
                file: parent.file,
                range,
            };
            if out.last() != Some(&m) {
                out.push(m);
            }
        }
    }
    Ok(out)
}

enum Matcher<'a> {
    Lines { start: LineNo, end: LineNo },
    Regex(Regex),
    Str(&'a str),
    Heredoc { lines: Vec<String>, raw: bool },
    File(&'a str),
}

impl<'a> Matcher<'a> {
    fn new(primary: &'a Primary, files: &[&SourceFile], parents: &[Match]) -> Result<Self, E> {
        Ok(match primary {
            Primary::Lines { start, end } => {
                let end = end.unwrap_or(*start);
                check_lines(*start, end, files, parents)?;
                Matcher::Lines { start: *start, end }
            }
            Primary::Regex(pattern) => Matcher::Regex(
                pattern
                    .regex()
                    .expect("regexes are validated when the script is parsed"),
            ),
            Primary::Literal(text) => match text.kind {
                TextKind::Str => Matcher::Str(&text.value),
                TextKind::Heredoc => Matcher::Heredoc {
                    lines: strip_indent(&text.value),
                    raw: false,
                },
                TextKind::RawHeredoc => Matcher::Heredoc {
                    lines: text.value.split('\n').map(String::from).collect(),
                    raw: true,
                },
            },
            Primary::File(path) => {
                if !files.iter().any(|f| same_path(&f.path, path)) {
                    return Err(E::NotInFileSet {
                        path: path.clone(),
                        files: files
                            .iter()
                            .map(|f| f.path.as_str())
                            .collect::<Vec<_>>()
                            .join(", "),
                    });
                }
                Matcher::File(path)
            }
            Primary::Syntax { kind, name } => {
                return Err(E::Unsupported(format!("syntax selector `{kind}:{name}`")));
            }
            Primary::Query(_) => return Err(E::Unsupported("`query{}` selector".into())),
        })
    }

    fn find(&self, f: &SourceFile, parent: Range<usize>) -> Vec<Range<usize>> {
        let within = |r: &Range<usize>| parent.start <= r.start && r.end <= parent.end;
        match self {
            Matcher::Lines { start, end } => {
                let count = f.buffer.line_count();
                let (Some(first), Some(last)) =
                    (line_index(*start, count), line_index(*end, count))
                else {
                    return Vec::new();
                };
                let range = line_range(&f.buffer, first).start..line_range(&f.buffer, last).end;
                if within(&range) {
                    vec![range]
                } else {
                    Vec::new()
                }
            }
            Matcher::Regex(re) => re
                .find_iter(&f.text[parent.clone()])
                .map(|m| parent.start + m.start()..parent.start + m.end())
                .collect(),
            Matcher::Str(needle) => {
                let needle = match f.buffer.line_ending() {
                    LineEnding::Lf => needle.to_string(),
                    LineEnding::Crlf => needle.replace('\n', "\r\n"),
                };
                if needle.is_empty() {
                    return Vec::new();
                }
                f.text[parent.clone()]
                    .match_indices(&needle)
                    .map(|(i, _)| parent.start + i..parent.start + i + needle.len())
                    .collect()
            }
            Matcher::Heredoc { lines, raw } => {
                let whole: Vec<(Range<usize>, &str)> = (0..f.buffer.line_count())
                    .map(|i| line_range(&f.buffer, i))
                    .filter(|r| within(r))
                    .map(|r| {
                        let content = f.text[r.clone()].trim_end_matches('\n');
                        let content = content.strip_suffix('\r').unwrap_or(content);
                        (r, content)
                    })
                    .collect();
                let mut out = Vec::new();
                let mut i = 0;
                while i + lines.len() <= whole.len() {
                    let window = &whole[i..i + lines.len()];
                    if heredoc_matches(lines, *raw, window.iter().map(|(_, l)| *l)) {
                        out.push(window[0].0.start..window[window.len() - 1].0.end);
                        i += lines.len();
                    } else {
                        i += 1;
                    }
                }
                out
            }
            Matcher::File(path) => {
                if same_path(&f.path, path) {
                    vec![parent]
                } else {
                    Vec::new()
                }
            }
        }
    }
}

/// Fails if the line range is past the end of every file that still has a
/// span to search.
fn check_lines(
    start: LineNo,
    end: LineNo,
    files: &[&SourceFile],
    parents: &[Match],
) -> Result<(), E> {
    let mut searched: Vec<usize> = parents.iter().map(|m| m.file).collect();
    searched.dedup();
    let past_end = |count| {
        [start, end].into_iter().find_map(|n| match n {
            LineNo::Number(n) if n > count => Some(n.to_string()),
            LineNo::Last if count == 0 => Some("$".to_string()),
            _ => None,
        })
    };
    let mut line = None;
    for &file in &searched {
        match past_end(files[file].buffer.line_count()) {
            Some(n) => line = line.or(Some(n)),
            None => return Ok(()),
        }
    }
    let Some(line) = line else { return Ok(()) };
    let files = searched
        .iter()
        .map(|&i| {
            let count = files[i].buffer.line_count();
            let unit = if count == 1 { "line" } else { "lines" };
            format!("{} ({count} {unit})", files[i].path)
        })
        .collect::<Vec<_>>()
        .join(", ");
    Err(E::LineOutOfRange { line, files })
}

fn line_index(n: LineNo, count: usize) -> Option<usize> {
    match n {
        LineNo::Number(n) if (1..=count).contains(&n) => Some(n - 1),
        LineNo::Last if count > 0 => Some(count - 1),
        _ => None,
    }
}

fn line_range(buffer: &Buffer, line: usize) -> Range<usize> {
    buffer
        .line_range(line)
        .expect("line index within the buffer")
}

/// Whether `window` equals the body lines, each non-blank line behind one
/// shared whitespace prefix (or exactly, if `raw`).
fn heredoc_matches<'w>(body: &[String], raw: bool, window: impl Iterator<Item = &'w str>) -> bool {
    let mut prefix = None;
    for (b, line) in body.iter().zip(window) {
        if raw {
            if b != line {
                return false;
            }
        } else if b.is_empty() {
            if !line.trim().is_empty() {
                return false;
            }
        } else {
            let Some(p) = line.strip_suffix(b.as_str()) else {
                return false;
            };
            if !p.chars().all(|c| c == ' ' || c == '\t') || *prefix.get_or_insert(p) != p {
                return false;
            }
        }
    }
    true
}

pub(crate) fn same_path(a: &str, b: &str) -> bool {
    a.strip_prefix("./").unwrap_or(a) == b.strip_prefix("./").unwrap_or(b)
}

fn part_name(part: Part) -> &'static str {
    match part {
        Part::Body => "body",
        Part::Sig => "sig",
        Part::Params => "params",
        Part::Name => "name",
        Part::Doc => "doc",
        Part::Lines => "lines",
    }
}

fn candidates(matches: &[Match], files: &[&SourceFile], selector: &str) -> Candidates {
    let listed = matches
        .iter()
        .take(MAX_CANDIDATES)
        .map(|m| {
            let f = &files[m.file];
            let lines = line_numbers(&f.buffer, &m.range);
            let scope = if files.len() > 1 {
                format!("file:{}>", f.path)
            } else {
                String::new()
            };
            (
                format!("{scope}{lines}>{selector}"),
                format!("{}:{lines}", f.path),
            )
        })
        .collect();
    Candidates {
        listed,
        total: matches.len(),
    }
}

/// The 1-based line or line range `range` touches, as a line selector.
pub(crate) fn line_numbers(buffer: &Buffer, range: &Range<usize>) -> String {
    let line = |offset| buffer.byte_to_line(offset).expect("match within the file") + 1;
    let first = line(range.start);
    let last = if range.is_empty() {
        first
    } else {
        line(range.end - 1)
    };
    if first == last {
        first.to_string()
    } else {
        format!("{first}-{last}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::ExecErrorKind;
    use crate::script::ast::CommandKind;
    use crate::script::parse;

    const TEXT: &str =
        "fn a() {\n    let x = 1;\n    let y = 2;\n}\n\nfn b() {\n    let x = 3;\n}\n";

    fn files(texts: &[(&str, &str)]) -> Vec<SourceFile> {
        texts
            .iter()
            .map(|(path, text)| SourceFile::new(*path, text.to_string(), None))
            .collect()
    }

    /// Resolves the target of the script's first command, `delete TARGET`.
    fn resolve_in(script: &str, files: &[SourceFile]) -> Result<Vec<Match>, ExecError> {
        let parsed = parse(script).unwrap();
        let CommandKind::Delete(target) = &parsed.commands[0].kind else {
            panic!("expected delete: {script}");
        };
        resolve(target, &files.iter().collect::<Vec<_>>(), script)
    }

    /// The text of each span `script` selects in a single file `a.rs`.
    fn select(script: &str, text: &str) -> Vec<String> {
        resolve_in(script, &files(&[("a.rs", text)]))
            .unwrap()
            .into_iter()
            .map(|m| {
                assert_eq!(m.file, 0);
                text[m.range].to_string()
            })
            .collect()
    }

    fn error(script: &str, texts: &[(&str, &str)]) -> String {
        resolve_in(script, &files(texts))
            .unwrap_err()
            .render(script)
    }

    #[test]
    fn lines_cover_whole_lines_with_endings() {
        assert_eq!(select("delete 2", TEXT), ["    let x = 1;\n"]);
        assert_eq!(
            select("delete 2-3", TEXT),
            ["    let x = 1;\n    let y = 2;\n"]
        );
        assert_eq!(select("delete $", TEXT), ["}\n"]);
        assert_eq!(select("delete 7-$", TEXT), ["    let x = 3;\n}\n"]);
        assert_eq!(select("delete $", "a\nb"), ["b"]);
    }

    #[test]
    fn line_past_end_is_an_error() {
        assert_eq!(
            error("delete 9", &[("a.rs", TEXT)]),
            "error: script:1:8: line 9 is past the end of a.rs (8 lines)"
        );
        assert_eq!(
            error("delete 7-12", &[("a.rs", TEXT)]),
            "error: script:1:8: line 12 is past the end of a.rs (8 lines)"
        );
        assert_eq!(
            error("delete $", &[("a.rs", "")]),
            "error: script:1:8: line $ is past the end of a.rs (0 lines)"
        );
    }

    #[test]
    fn line_past_end_of_only_some_files_selects_in_the_others() {
        let set = files(&[("a.rs", TEXT), ("b.rs", "x\n")]);
        let matches = resolve_in("delete 5", &set).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].file, 0);
        assert_eq!(
            error("delete 9", &[("a.rs", TEXT), ("b.rs", "x\n")]),
            "error: script:1:8: line 9 is past the end of a.rs (8 lines), b.rs (1 line)"
        );
    }

    #[test]
    fn regex_selects_every_match() {
        assert_eq!(
            select("delete all /let \\w/", TEXT),
            ["let x", "let y", "let x"]
        );
        assert_eq!(select("delete all /LET X/i", TEXT), ["let x", "let x"]);
        assert_eq!(select("delete all /^}$/", TEXT), ["}", "}"]);
    }

    #[test]
    fn string_selects_exact_occurrences() {
        assert_eq!(select("delete all \"let x\"", TEXT), ["let x", "let x"]);
        assert_eq!(select("delete \"1;\\n    let y\"", TEXT), ["1;\n    let y"]);
        assert_eq!(select("delete all \"aa\"", "aaaaa"), ["aa", "aa"]);
    }

    #[test]
    fn nested_steps_resolve_within_each_span() {
        let set = files(&[("a.rs", TEXT)]);
        let m = resolve_in("delete 6-8>/let x/", &set).unwrap();
        assert_eq!(
            m,
            [Match {
                file: 0,
                range: TEXT.rfind("let x").unwrap()..TEXT.rfind(" = 3").unwrap()
            }]
        );
        let m = resolve_in("delete /fn b[^}]*/>\"x\"", &set).unwrap();
        let x = TEXT.rfind('x').unwrap();
        assert_eq!(
            m,
            [Match {
                file: 0,
                range: x..x + 1
            }]
        );
        assert_eq!(
            select("delete all /let . = \\d/>/\\d/", TEXT),
            ["1", "2", "3"]
        );
    }

    #[test]
    fn nested_lines_must_lie_inside_the_parent() {
        assert_eq!(
            error("delete 1-4>7", &[("a.rs", TEXT)]),
            "error: script:1:8: 1-4>7 matches nothing in a.rs"
        );
    }

    #[test]
    fn lines_part_widens_to_whole_lines() {
        assert_eq!(select("delete \"y = 2\".lines", TEXT), ["    let y = 2;\n"]);
        assert_eq!(
            select("delete \"2;\\n}\".lines", TEXT),
            ["    let y = 2;\n}\n"]
        );
        assert_eq!(select("delete all /x/.lines", "x x\ny\n"), ["x x\n"]);
    }

    #[test]
    fn heredoc_matches_whole_lines_at_any_indentation() {
        let body = "    let x = 1;\n    let y = 2;\n";
        assert_eq!(
            select("delete <<END\nlet x = 1;\nlet y = 2;\nEND\n", TEXT),
            [body]
        );
        assert_eq!(
            select(
                "delete <<END\n        let x = 1;\n        let y = 2;\nEND\n",
                TEXT
            ),
            [body]
        );
        assert_eq!(
            select("delete <<END\nfn b() {\n    let x = 3;\n}\nEND\n", TEXT),
            ["fn b() {\n    let x = 3;\n}\n"]
        );
    }

    #[test]
    fn heredoc_keeps_relative_indentation() {
        assert_eq!(
            error(
                "delete <<END\nfn b() {\nlet x = 3;\n}\nEND\n",
                &[("a.rs", TEXT)]
            ),
            "error: script:1:8: <<END matches nothing in a.rs"
        );
        let text = "  let x = 1;\n    let y = 2;\n";
        assert!(
            resolve_in(
                "delete <<END\nlet x = 1;\nlet y = 2;\nEND\n",
                &files(&[("a.rs", text)])
            )
            .is_err()
        );
    }

    #[test]
    fn heredoc_blank_lines_match_blank_or_whitespace_lines() {
        assert_eq!(
            select("delete <<END\n}\n\nfn b() {\nEND\n", TEXT),
            ["}\n\nfn b() {\n"]
        );
        let text = "  a\n  \n  b\n";
        assert_eq!(select("delete <<END\na\n\nb\nEND\n", text), [text]);
    }

    #[test]
    fn heredoc_must_match_whole_lines() {
        assert_eq!(
            error("delete <<END\nlet x\nEND\n", &[("a.rs", TEXT)]),
            "error: script:1:8: <<END matches nothing in a.rs"
        );
    }

    #[test]
    fn raw_heredoc_matches_exactly() {
        assert_eq!(
            select("delete <<'END'\n    let y = 2;\nEND\n", TEXT),
            ["    let y = 2;\n"]
        );
        assert!(
            resolve_in(
                "delete <<'END'\nlet y = 2;\nEND\n",
                &files(&[("a.rs", TEXT)])
            )
            .is_err()
        );
    }

    #[test]
    fn literal_line_breaks_match_crlf() {
        let text = "a\r\nb\r\n";
        assert_eq!(select("delete \"a\\nb\"", text), ["a\r\nb"]);
        assert_eq!(select("delete <<END\na\nb\nEND\n", text), [text]);
        assert!(resolve_in("delete /a\\nb/", &files(&[("a.rs", text)])).is_err());
    }

    #[test]
    fn file_step_narrows_to_one_file() {
        let set = files(&[("a.rs", "x\n"), ("b.rs", "y\nx\n")]);
        let m = resolve_in("delete all file:b.rs>/x/", &set).unwrap();
        assert_eq!(
            m,
            [Match {
                file: 1,
                range: 2..3
            }]
        );
        assert_eq!(
            error("delete file:c.rs>/x/", &[("a.rs", "x\n"), ("b.rs", "x\n")]),
            "error: script:1:8: file:c.rs is not in the file set: a.rs, b.rs"
        );
    }

    #[test]
    fn all_selects_across_files_in_order() {
        let set = files(&[("a.rs", "x x\n"), ("b.rs", "x\n")]);
        let m = resolve_in("delete all /x/", &set).unwrap();
        assert_eq!(
            m,
            [
                Match {
                    file: 0,
                    range: 0..1
                },
                Match {
                    file: 0,
                    range: 2..3
                },
                Match {
                    file: 1,
                    range: 0..1
                },
            ]
        );
    }

    #[test]
    fn zero_matches_is_an_error_even_with_all() {
        assert_eq!(
            error("delete all /z/", &[("a.rs", "x\n"), ("b.rs", "y\n")]),
            "error: script:1:12: /z/ matches nothing in a.rs, b.rs"
        );
    }

    #[test]
    fn ambiguity_lists_line_scoped_candidates() {
        assert_eq!(
            error("delete /let x/", &[("a.rs", TEXT)]),
            "error: script:1:8: /let x/ matches 2 items; add `all` or use one of:\n  \
             2>/let x/   a.rs:2\n  \
             7>/let x/   a.rs:7"
        );
        let text = "a\nb\na\nb\n";
        assert_eq!(
            error("delete <<END\na\nb\nEND\n", &[("a.rs", text)]),
            "error: script:1:8: <<END matches 2 items; add `all` or use one of:\n  \
             1-2><<END   a.rs:1-2\n  \
             3-4><<END   a.rs:3-4"
        );
    }

    #[test]
    fn ambiguity_candidates_are_aligned() {
        let text = "\n".repeat(8) + "x\nx\n";
        assert_eq!(
            error("delete /x/", &[("a.rs", &text)]),
            "error: script:1:8: /x/ matches 2 items; add `all` or use one of:\n  \
             9>/x/    a.rs:9\n  \
             10>/x/   a.rs:10"
        );
    }

    #[test]
    fn ambiguity_across_files_scopes_candidates_by_file() {
        assert_eq!(
            error("delete /x/", &[("a.rs", "x\n"), ("b.rs", "y\nx\n")]),
            "error: script:1:8: /x/ matches 2 items; add `all` or use one of:\n  \
             file:a.rs>1>/x/   a.rs:1\n  \
             file:b.rs>2>/x/   b.rs:2"
        );
    }

    #[test]
    fn ambiguity_lists_at_most_ten_candidates() {
        let err = error("delete /x/", &[("a.rs", &"x\n".repeat(12))]);
        assert!(
            err.starts_with("error: script:1:8: /x/ matches 12 items;"),
            "{err}"
        );
        assert!(
            err.contains("\n  10>/x/   a.rs:10\n  … and 2 more"),
            "{err}"
        );
        assert!(!err.contains("11>"), "{err}");
    }

    #[test]
    fn syntax_query_and_parts_are_not_yet_supported() {
        assert_eq!(
            error("delete fn:parse", &[("a.rs", TEXT)]),
            "error: script:1:8: syntax selector `fn:parse` is not yet supported"
        );
        for script in ["delete query{(identifier) @sel}", "delete /x/.body"] {
            let err = resolve_in(script, &files(&[("a.rs", TEXT)])).unwrap_err();
            assert!(matches!(err.kind, ExecErrorKind::Unsupported(_)), "{err:?}");
        }
    }
}
