//! End-to-end tests of the `ned` binary against docs/command-language.md.

use std::fs;
use std::path::Path;
use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use insta::assert_snapshot;
use tempfile::TempDir;

/// `src/parser.rs` from the spec's worked examples (§8).
const PARSER: &str = r#"use std::fmt;

/// A recursive-descent parser.
pub struct Parser {
    src: String,
    pos: usize,
}

impl Parser {
    pub fn new(src: &str) -> Self {
        Parser { src: src.to_string(), pos: 0 }
    }

    pub fn parse(&mut self) -> Result<Ast, Error> {
        let tok = self.next().expect("unexpected end");
        self.parse_expr(tok)
    }

    fn debug_dump(&self) {
        eprintln!("{}", self.src);
    }
}
"#;

const APP: &str =
    "def handle(req):\n    if req.ok:\n        log(req)\n        return 200\n    return 500\n";

fn dir_with(files: &[(&str, &str)]) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (name, text) in files {
        fs::write(dir.path().join(name), text).unwrap();
    }
    dir
}

/// Runs `ned ARGS` in `dir`, with `stdin` as its input, and reports the exit
/// code, stdout, and stderr.
fn ned(dir: &Path, args: &[&str], stdin: &str) -> String {
    let output = cargo_bin_cmd!("ned")
        .current_dir(dir)
        .args(args)
        .write_stdin(stdin)
        .output()
        .unwrap();
    report(&output)
}

fn report(output: &Output) -> String {
    format!(
        "exit: {}\n--- stdout\n{}--- stderr\n{}",
        output.status.code().unwrap(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    )
}

fn read(dir: &TempDir, name: &str) -> String {
    fs::read_to_string(dir.path().join(name)).unwrap()
}

#[test]
fn edit_prints_summary_and_hunks() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let out = ned(
        dir.path(),
        &[
            "parser.rs",
            "-e",
            r#"replace "unexpected end" with "unexpected end of input""#,
        ],
        "",
    );
    assert_snapshot!(out, @r#"
    exit: 0
    --- stdout
    parser.rs: 1 edit, +1 -1
    @@ -14,3 +14,3 @@
         pub fn parse(&mut self) -> Result<Ast, Error> {
    -        let tok = self.next().expect("unexpected end");
    +        let tok = self.next().expect("unexpected end of input");
             self.parse_expr(tok)
    --- stderr
    "#);
    assert_eq!(
        read(&dir, "parser.rs"),
        PARSER.replace("unexpected end", "unexpected end of input")
    );
}

#[test]
fn script_from_stdin_with_heredoc() {
    let dir = dir_with(&[("app.py", APP)]);
    let script = "insert after \"log(req)\".lines <<END\nif req.slow:\n    warn(req)\nEND\n";
    let out = ned(dir.path(), &["app.py"], script);
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    app.py: 1 edit, +2 -0
    @@ -3,2 +3,4 @@
             log(req)
    +        if req.slow:
    +            warn(req)
             return 200
    --- stderr
    ");
    assert_eq!(
        read(&dir, "app.py"),
        APP.replace(
            "log(req)\n",
            "log(req)\n        if req.slow:\n            warn(req)\n"
        )
    );
}

#[test]
fn sub_across_files_reports_each_modified_file() {
    let dir = dir_with(&[
        ("a.rs", "let old_name = 1;\nprint(old_name);\n"),
        ("b.rs", "fn f() {}\n"),
        ("c.rs", "old_name();\n"),
    ]);
    let out = ned(
        dir.path(),
        &[
            "a.rs",
            "b.rs",
            "c.rs",
            "-e",
            r#"sub /\bold_name\b/ with "new_name""#,
        ],
        "",
    );
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    a.rs: 2 edits, +2 -2
    @@ -1,2 +1,2 @@
    -let old_name = 1;
    -print(old_name);
    +let new_name = 1;
    +print(new_name);
    c.rs: 1 edit, +1 -1
    @@ -1,1 +1,1 @@
    -old_name();
    +new_name();
    --- stderr
    ");
    assert_eq!(read(&dir, "b.rs"), "fn f() {}\n");
    assert_eq!(read(&dir, "c.rs"), "new_name();\n");
}

#[test]
fn show_prints_lines_and_writes_nothing() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let out = ned(dir.path(), &["parser.rs", "-e", "show 14-17"], "");
    assert_snapshot!(out, @r#"
    exit: 0
    --- stdout
    parser.rs:14-17
    14:    pub fn parse(&mut self) -> Result<Ast, Error> {
    15:        let tok = self.next().expect("unexpected end");
    16:        self.parse_expr(tok)
    17:    }
    --- stderr
    "#);
    assert_eq!(read(&dir, "parser.rs"), PARSER);
}

#[test]
fn repeated_scripts_are_joined() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let out = ned(
        dir.path(),
        &["parser.rs", "-e", "show 1", "-e", "show $"],
        "",
    );
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    parser.rs:1
    1:use std::fmt;
    parser.rs:22
    22:}
    --- stderr
    ");
}

#[test]
fn file_command_sets_files_without_arguments() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let out = ned(dir.path(), &["-e", "file parser.rs", "-e", "show 1"], "");
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    parser.rs:1
    1:use std::fmt;
    --- stderr
    ");
}

#[test]
fn dry_run_prints_but_does_not_write() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let out = ned(
        dir.path(),
        &["-n", "parser.rs", "-e", "replace 15 with \"todo!()\""],
        "",
    );
    assert_snapshot!(out, @r#"
    exit: 0
    --- stdout
    (dry run) parser.rs: 1 edit, +1 -1
    @@ -14,3 +14,3 @@
         pub fn parse(&mut self) -> Result<Ast, Error> {
    -        let tok = self.next().expect("unexpected end");
    +        todo!()
             self.parse_expr(tok)
    --- stderr
    "#);
    assert_eq!(read(&dir, "parser.rs"), PARSER);
}

#[test]
fn quiet_prints_only_summaries() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let out = ned(dir.path(), &["-q", "parser.rs", "-e", "delete 20"], "");
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    parser.rs: 1 edit, +0 -1
    --- stderr
    ");
    assert!(!read(&dir, "parser.rs").contains("eprintln"));
}

#[test]
fn context_sets_hunk_context() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let out = ned(
        dir.path(),
        &[
            "--context",
            "0",
            "parser.rs",
            "-e",
            "replace 15 with \"todo!()\"",
        ],
        "",
    );
    assert_snapshot!(out, @r#"
    exit: 0
    --- stdout
    parser.rs: 1 edit, +1 -1
    @@ -15,1 +15,1 @@
    -        let tok = self.next().expect("unexpected end");
    +        todo!()
    --- stderr
    "#);
}

#[test]
fn crlf_files_keep_their_line_endings() {
    let dir = dir_with(&[("a.txt", "a\r\nb\r\n")]);
    let out = ned(dir.path(), &["a.txt", "-e", "insert after 1 \"x\""], "");
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    a.txt: 1 edit, +1 -0
    @@ -1,2 +1,3 @@
     a
    +x
     b
    --- stderr
    ");
    assert_eq!(read(&dir, "a.txt"), "a\r\nx\r\nb\r\n");
}

#[test]
fn ambiguous_selector_exits_1() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let out = ned(dir.path(), &["parser.rs", "-e", "delete /pub fn/"], "");
    assert_snapshot!(out, @r"
    exit: 1
    --- stdout
    --- stderr
    error: script:1:8: /pub fn/ matches 2 items; add `all` or use one of:
      10>/pub fn/   parser.rs:10
      14>/pub fn/   parser.rs:14
    ");
    assert_eq!(read(&dir, "parser.rs"), PARSER);
}

#[test]
fn failed_script_keeps_reads_and_writes_nothing() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let out = ned(
        dir.path(),
        &[
            "parser.rs",
            "-e",
            "show 1",
            "-e",
            "replace 15 with \"x\"",
            "-e",
            "delete \"nope\"",
        ],
        "",
    );
    assert_snapshot!(out, @r#"
    exit: 1
    --- stdout
    parser.rs:1
    1:use std::fmt;
    --- stderr
    error: script:3:8: "nope" matches nothing in parser.rs
    "#);
    assert_eq!(read(&dir, "parser.rs"), PARSER);
}

#[test]
fn overlapping_edits_exit_1() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let out = ned(
        dir.path(),
        &[
            "parser.rs",
            "-e",
            "delete 15",
            "-e",
            "replace 15 with \"x\"",
        ],
        "",
    );
    assert_snapshot!(out, @r"
    exit: 1
    --- stdout
    --- stderr
    error: script:2:1: edit overlaps command 1 at parser.rs:15
    ");
    assert_eq!(read(&dir, "parser.rs"), PARSER);
}

#[test]
fn syntax_error_exits_2_with_caret() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let out = ned(dir.path(), &["parser.rs", "-e", "show \"abc"], "");
    assert_snapshot!(out, @r#"
    exit: 2
    --- stdout
    --- stderr
    error: script:1:6: unterminated string; close it with `"` on the same line (use \n or a heredoc for multi-line text)
    1:show "abc
           ^
    "#);
}

#[test]
fn unsupported_feature_exits_2() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let out = ned(dir.path(), &["parser.rs", "-e", "outline"], "");
    assert_snapshot!(out, @r"
    exit: 2
    --- stdout
    --- stderr
    error: script:1:1: `outline` is not yet supported
    ");
}

#[test]
fn no_files_exits_2() {
    let dir = dir_with(&[]);
    let out = ned(dir.path(), &["-e", "show"], "");
    assert_snapshot!(out, @r"
    exit: 2
    --- stdout
    --- stderr
    error: script:1:1: no files to edit; pass FILE arguments or use `file PATH`
    ");
}

#[test]
fn bad_flag_exits_2() {
    let dir = dir_with(&[]);
    assert!(ned(dir.path(), &["--bogus"], "").starts_with("exit: 2\n"));
}

#[test]
fn missing_file_exits_3() {
    let dir = dir_with(&[]);
    let out = ned(dir.path(), &["nope.rs", "-e", "show"], "");
    assert_snapshot!(out, @r"
    exit: 3
    --- stdout
    --- stderr
    error: cannot read nope.rs: No such file or directory (os error 2)
    ");
}

#[cfg(unix)]
#[test]
fn write_failure_exits_3_and_leaves_files_untouched() {
    use std::os::unix::fs::PermissionsExt;
    let dir = dir_with(&[("a.txt", "a\n")]);
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();
    let out = ned(dir.path(), &["a.txt", "-e", "replace 1 with \"b\""], "");
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert_snapshot!(out, @r"
    exit: 3
    --- stdout
    --- stderr
    error: cannot write files: Permission denied (os error 13)
    ");
    assert_eq!(read(&dir, "a.txt"), "a\n");
}

#[test]
fn edit_introducing_syntax_error_exits_1() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let script = r#"replace "self.next().expect(\"unexpected end\")" with "(self.next()""#;
    let out = ned(dir.path(), &["parser.rs", "-e", script], "");
    assert_snapshot!(out, @r"
    exit: 1
    --- stdout
    --- stderr
    error: parser.rs:15:19: edit introduces a syntax error (use --force to apply anyway)
    15:        let tok = (self.next();
                         ^
    ");
    assert_eq!(read(&dir, "parser.rs"), PARSER);
}

#[test]
fn force_applies_edits_with_syntax_errors() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let script = r#"replace "self.next().expect(\"unexpected end\")" with "(self.next()""#;
    let out = ned(
        dir.path(),
        &["parser.rs", "--force", "-q", "-e", script],
        "",
    );
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    parser.rs: 1 edit, +1 -1
    --- stderr
    ");
    assert!(read(&dir, "parser.rs").contains("let tok = (self.next();"));
}

#[test]
fn lang_sets_the_language_of_every_file() {
    let dir = dir_with(&[("parser.txt", PARSER)]);
    let script = r#"replace "self.next().expect(\"unexpected end\")" with "(self.next()""#;
    let out = ned(dir.path(), &["parser.txt", "-n", "-q", "-e", script], "");
    assert!(out.starts_with("exit: 0\n"), "{out}");
    let out = ned(
        dir.path(),
        &["parser.txt", "--lang", "rust", "-e", script],
        "",
    );
    assert!(out.starts_with("exit: 1\n"), "{out}");
}

#[test]
fn unknown_lang_exits_2() {
    let dir = dir_with(&[("a.txt", "a\n")]);
    let out = ned(dir.path(), &["a.txt", "--lang", "ruby", "-e", "show"], "");
    assert!(out.starts_with("exit: 2\n"), "{out}");
    assert!(
        out.contains(
            "unknown language `ruby`; expected one of rust, python, typescript, tsx, javascript, go"
        ),
        "{out}"
    );
}
