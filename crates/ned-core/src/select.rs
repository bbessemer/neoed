//! Resolving selectors to spans of files (command-language spec, §3).

use std::ops::Range;

use crate::buffer::Buffer;
use crate::exec::ExecError;
use crate::script::ast::Target;

/// A file in the file set, with its path as the user wrote it.
#[derive(Debug, Clone)]
pub struct SourceFile {
    pub path: String,
    pub text: String,
    pub buffer: Buffer,
}

impl SourceFile {
    pub fn new(path: impl Into<String>, text: String) -> Self {
        let buffer = Buffer::new(&text);
        SourceFile {
            path: path.into(),
            text,
            buffer,
        }
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
pub fn resolve(target: &Target, files: &[SourceFile], src: &str) -> Result<Vec<Match>, ExecError> {
    let _ = (target, files, src);
    todo!()
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
            .map(|(path, text)| SourceFile::new(*path, text.to_string()))
            .collect()
    }

    /// Resolves the target of the script's first command, `delete TARGET`.
    fn resolve_in(script: &str, files: &[SourceFile]) -> Result<Vec<Match>, ExecError> {
        let parsed = parse(script).unwrap();
        let CommandKind::Delete(target) = &parsed.commands[0].kind else {
            panic!("expected delete: {script}");
        };
        resolve(target, files, script)
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
