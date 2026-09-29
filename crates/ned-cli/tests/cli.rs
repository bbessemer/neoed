//! End-to-end tests of the `ned` binary against docs/command-language.md.

use std::fs;
use std::os::unix::fs::PermissionsExt;
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

/// A user config turning every formatter off, so only tests that configure
/// one in a `.ned.toml` format anything.
const NO_FORMATTERS: &str = "[format]
rust = false
python = false
typescript = false
tsx = false
javascript = false
go = false
";

/// Runs `ned ARGS` in `dir`, with `stdin` as its input, and reports the exit
/// code, stdout, and stderr.
fn ned(dir: &Path, args: &[&str], stdin: &str) -> String {
    let config = tempfile::tempdir().unwrap();
    fs::create_dir(config.path().join("ned")).unwrap();
    fs::write(config.path().join("ned/config.toml"), NO_FORMATTERS).unwrap();
    let output = cargo_bin_cmd!("ned")
        .current_dir(dir)
        .env("XDG_CONFIG_HOME", config.path())
        .args(args)
        .write_stdin(stdin)
        .output()
        .unwrap();
    // Messages name the working directory; keep snapshots independent of it.
    let dir = fs::canonicalize(dir).unwrap();
    report(&output).replace(dir.to_str().unwrap(), "{dir}")
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
      fn:new>/pub fn/     parser.rs:10
      fn:parse>/pub fn/   parser.rs:14
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
    error: script:3:8: "nope" matches nothing in parser.rs; `show` prints the text to match against
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
    error: script:2:1: edit overlaps command 1 at parser.rs:15; merge the two edits, or make one in a separate ned run
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
    let out = ned(dir.path(), &["parser.rs", "-e", "check"], "");
    assert_snapshot!(out, @r"
    exit: 2
    --- stdout
    --- stderr
    error: script:1:1: `check` is not yet supported; run the project's build or linter
    1:check
      ^
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
    error: cannot read nope.rs: no such file (paths are relative to {dir})
    ");
}

#[test]
fn quoted_glob_arguments_are_expanded() {
    let dir = dir_with(&[
        ("b.rs", "let x = 1;\n"),
        ("a.rs", "let x = 2;\n"),
        ("c.py", "x = 3\n"),
    ]);
    let out = ned(dir.path(), &["-q", "*.rs", "-e", "sub /x/ with \"y\""], "");
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    a.rs: 1 edit, +1 -1
    b.rs: 1 edit, +1 -1
    --- stderr
    ");
    assert_eq!(read(&dir, "c.py"), "x = 3\n");
}

#[test]
fn file_command_expands_globs() {
    let dir = dir_with(&[("a.rs", "let x = 1;\n"), ("b.rs", "let x = 2;\n")]);
    let out = ned(
        dir.path(),
        &["-q", "-e", "file *.rs; sub /x/ with \"y\""],
        "",
    );
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    a.rs: 1 edit, +1 -1
    b.rs: 1 edit, +1 -1
    --- stderr
    ");
    assert_eq!(read(&dir, "b.rs"), "let y = 2;\n");
}

#[test]
fn glob_matching_nothing_exits_3() {
    let dir = dir_with(&[("a.rs", "let x = 1;\n")]);
    let out = ned(
        dir.path(),
        &["a.rs", "*.rx", "-e", "sub /x/ with \"y\""],
        "",
    );
    assert_snapshot!(out, @r"
    exit: 3
    --- stdout
    --- stderr
    error: glob `*.rx` matched nothing (paths are relative to {dir})
    ");
    assert_eq!(read(&dir, "a.rs"), "let x = 1;\n");
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
    error: cannot write files: Permission denied (os error 13); no file was changed
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
    error: parser.rs:15:31: edit introduces a syntax error (use --force to apply anyway)
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

#[test]
fn syntax_selector_scopes_a_literal() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let script = r#"replace fn:parse>"unexpected end" with "unexpected end of input""#;
    let out = ned(dir.path(), &["parser.rs", "-e", script], "");
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
}

#[test]
fn delete_function_tidies_blank_line() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let out = ned(dir.path(), &["parser.rs", "-e", "delete fn:debug_dump"], "");
    assert_snapshot!(out, @r#"
    exit: 0
    --- stdout
    parser.rs: 1 edit, +0 -4
    @@ -17,6 +17,2 @@
         }
    -
    -    fn debug_dump(&self) {
    -        eprintln!("{}", self.src);
    -    }
     }
    --- stderr
    "#);
}

#[test]
fn ambiguous_syntax_selector_lists_candidates() {
    let dir = dir_with(&[
        ("parser.rs", PARSER),
        ("lexer.rs", "impl Lexer {\n    fn new() {}\n}\n"),
    ]);
    let out = ned(
        dir.path(),
        &["parser.rs", "lexer.rs", "-e", "delete fn:new"],
        "",
    );
    assert_snapshot!(out, @r"
    exit: 1
    --- stdout
    --- stderr
    error: script:1:8: fn:new matches 2 items; add `all` or use one of:
      impl:Parser>fn:new   parser.rs:10-12
      impl:Lexer>fn:new    lexer.rs:2
    ");
}

#[test]
fn insert_method_at_end_of_impl() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let script = "insert end impl:Parser <<END\n\nfn peek(&self) -> Option<char> {\n    self.src[self.pos..].chars().next()\n}\nEND\n";
    let out = ned(dir.path(), &["parser.rs"], script);
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    parser.rs: 1 edit, +4 -0
    @@ -21,2 +21,6 @@
         }
    +
    +    fn peek(&self) -> Option<char> {
    +        self.src[self.pos..].chars().next()
    +    }
     }
    --- stderr
    ");
}

#[test]
fn replace_function_params() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let script = r#"replace fn:new.params with "src: impl Into<String>""#;
    let out = ned(dir.path(), &["parser.rs", "-e", script], "");
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    parser.rs: 1 edit, +1 -1
    @@ -9,3 +9,3 @@
     impl Parser {
    -    pub fn new(src: &str) -> Self {
    +    pub fn new(src: impl Into<String>) -> Self {
             Parser { src: src.to_string(), pos: 0 }
    --- stderr
    ");
}

#[test]
fn replace_function_body() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let script = "replace fn:parse.body with <<END\nlet tok = self.next().ok_or(Error::Eof)?;\nself.parse_expr(tok)\nEND\n";
    let out = ned(dir.path(), &["parser.rs"], script);
    assert_snapshot!(out, @r#"
    exit: 0
    --- stdout
    parser.rs: 1 edit, +1 -1
    @@ -14,3 +14,3 @@
         pub fn parse(&mut self) -> Result<Ast, Error> {
    -        let tok = self.next().expect("unexpected end");
    +        let tok = self.next().ok_or(Error::Eof)?;
             self.parse_expr(tok)
    --- stderr
    "#);
}

#[test]
fn insert_import_after_import() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let script = r#"insert after import:std::fmt "use std::io;""#;
    let out = ned(dir.path(), &["parser.rs", "-q", "-e", script], "");
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    parser.rs: 1 edit, +1 -0
    --- stderr
    ");
    assert_eq!(
        read(&dir, "parser.rs"),
        PARSER.replacen("use std::fmt;\n", "use std::fmt;\nuse std::io;\n", 1)
    );
}

#[test]
fn missing_part_exits_1() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let out = ned(dir.path(), &["parser.rs", "-e", "show fn:new.doc"], "");
    assert_snapshot!(out, @r"
    exit: 1
    --- stdout
    --- stderr
    error: script:1:6: fn:new has no .doc; it has .body .sig .params .name .lines
    ");
}

#[test]
fn outline_prints_the_symbol_tree() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let out = ned(dir.path(), &["parser.rs", "-e", "outline"], "");
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    parser.rs
    1 import (1)
    3-7 struct:Parser
    9-22 impl:Parser
      10-12 fn:new
      14-17 fn:parse
      19-21 fn:debug_dump
    --- stderr
    ");
}

#[test]
fn outline_of_a_selector_lists_the_items_inside() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let out = ned(
        dir.path(),
        &[
            "parser.rs",
            "-e",
            "outline struct:Parser; outline impl:Parser",
        ],
        "",
    );
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    parser.rs
    5 field:src
    6 field:pos
    parser.rs
    10-12 fn:new
    14-17 fn:parse
    19-21 fn:debug_dump
    --- stderr
    ");
}

#[test]
fn outline_in_a_deferred_language_exits_2() {
    let dir = dir_with(&[("app.py", APP)]);
    let out = ned(dir.path(), &["app.py", "-e", "outline"], "");
    assert_snapshot!(out, @r"
    exit: 2
    --- stdout
    --- stderr
    error: script:1:1: `outline` in python files is not yet supported; use `show`
    ");
}

#[test]
fn query_selector_edits_matches() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let script = r#"delete query{(expression_statement (macro_invocation macro: (identifier) @m (#eq? @m "eprintln"))) @sel}"#;
    let out = ned(dir.path(), &["parser.rs", "-e", script], "");
    assert_snapshot!(out, @r#"
    exit: 0
    --- stdout
    parser.rs: 1 edit, +0 -1
    @@ -19,3 +19,2 @@
         fn debug_dump(&self) {
    -        eprintln!("{}", self.src);
         }
    --- stderr
    "#);
}

#[test]
fn invalid_query_exits_2() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let out = ned(
        dir.path(),
        &["parser.rs", "-e", "show query{(no_such_node) @sel}"],
        "",
    );
    assert_snapshot!(out, @r"
    exit: 2
    --- stdout
    --- stderr
    error: script:1:6: invalid rust query: unknown node type `no_such_node` at column 2
    ");
}

const FN_A: &str = "fn f() {\n    a();\n}\n";

/// A directory with `a.rs` (`FN_A`) and a fake Rust formatter, `fmt.sh`, that
/// collapses `;;` to `;`.
fn dir_with_formatter() -> TempDir {
    let dir = dir_with(&[
        ("a.rs", FN_A),
        (".ned.toml", "[format]\nrust = [\"./fmt.sh\"]\n"),
        ("fmt.sh", "#!/bin/sh\nsed 's/;;/;/'\n"),
    ]);
    let script = dir.path().join("fmt.sh");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

const DOUBLE_SEMI: &str = r#"replace "a();" with "b();;""#;

#[test]
fn formatter_changes_follow_the_edit_hunks() {
    let dir = dir_with_formatter();
    let out = ned(dir.path(), &["a.rs", "-e", DOUBLE_SEMI], "");
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    a.rs: 1 edit, +1 -1
    @@ -1,3 +1,3 @@
     fn f() {
    -    a();
    +    b();;
     }
    fmt fmt.sh: +1 -1
    @@ -1,3 +1,3 @@
     fn f() {
    -    b();;
    +    b();
     }
    --- stderr
    ");
    assert_eq!(read(&dir, "a.rs"), "fn f() {\n    b();\n}\n");
}

#[test]
fn no_fmt_skips_formatting() {
    let dir = dir_with_formatter();
    let out = ned(
        dir.path(),
        &["--no-fmt", "-q", "a.rs", "-e", DOUBLE_SEMI],
        "",
    );
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    a.rs: 1 edit, +1 -1
    --- stderr
    ");
    assert_eq!(read(&dir, "a.rs"), "fn f() {\n    b();;\n}\n");
}

#[test]
fn quiet_keeps_the_fmt_header() {
    let dir = dir_with_formatter();
    let out = ned(dir.path(), &["-q", "a.rs", "-e", DOUBLE_SEMI], "");
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    a.rs: 1 edit, +1 -1
    fmt fmt.sh: +1 -1
    --- stderr
    ");
}

#[test]
fn dry_run_formats_without_writing() {
    let dir = dir_with_formatter();
    let out = ned(dir.path(), &["-n", "-q", "a.rs", "-e", DOUBLE_SEMI], "");
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    (dry run) a.rs: 1 edit, +1 -1
    fmt fmt.sh: +1 -1
    --- stderr
    ");
    assert_eq!(read(&dir, "a.rs"), FN_A);
}

#[test]
fn missing_formatter_is_a_note() {
    let dir = dir_with(&[
        ("a.rs", FN_A),
        (
            ".ned.toml",
            "[format]\nrust = [\"ned-no-such-formatter\"]\n",
        ),
    ]);
    let out = ned(dir.path(), &["-q", "a.rs", "-e", DOUBLE_SEMI], "");
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    a.rs: 1 edit, +1 -1
    --- stderr
    note: ned-no-such-formatter not found; skipped formatting a.rs
    ");
    assert_eq!(read(&dir, "a.rs"), "fn f() {\n    b();;\n}\n");
}

#[test]
fn invalid_config_exits_2_and_writes_nothing() {
    let dir = dir_with(&[
        ("a.rs", FN_A),
        (".ned.toml", "[format]\nruby = [\"rubocop\"]\n"),
    ]);
    let out = ned(dir.path(), &["a.rs", "-e", DOUBLE_SEMI], "");
    assert_snapshot!(out, @r"
    exit: 2
    --- stdout
    --- stderr
    error: .ned.toml:2:1: invalid config: unknown language `ruby`; expected one of rust, python, typescript, tsx, javascript, go
    ");
    assert_eq!(read(&dir, "a.rs"), FN_A);
    let out = ned(
        dir.path(),
        &["--no-fmt", "-q", "a.rs", "-e", DOUBLE_SEMI],
        "",
    );
    assert!(out.starts_with("exit: 0\n"), "{out}");
}

#[test]
fn rustfmt_formats_rust() {
    let dir = dir_with(&[
        ("a.rs", "struct S {\n    a: u8,\n}\n"),
        (
            ".ned.toml",
            "[format]\nrust = [\"rustfmt\", \"--edition\", \"{edition}\"]\n",
        ),
    ]);
    let out = ned(
        dir.path(),
        &["a.rs", "-e", r#"insert after /a: u8,/ "b:   u8,""#],
        "",
    );
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    a.rs: 1 edit, +1 -0
    @@ -2,2 +2,3 @@
         a: u8,
    +    b:   u8,
     }
    fmt rustfmt: +1 -1
    @@ -2,3 +2,3 @@
         a: u8,
    -    b:   u8,
    +    b: u8,
     }
    --- stderr
    ");
    assert_eq!(
        read(&dir, "a.rs"),
        "struct S {\n    a: u8,\n    b: u8,\n}\n"
    );
}

#[test]
fn move_across_files() {
    let dir = dir_with(&[
        ("parser.rs", PARSER),
        ("debug.rs", "impl Parser {\n    fn trace(&self) {}\n}\n"),
    ]);
    let out = ned(
        dir.path(),
        &[
            "parser.rs",
            "debug.rs",
            "-e",
            "move fn:debug_dump end file:debug.rs>impl:Parser",
        ],
        "",
    );
    assert_snapshot!(out, @r#"
    exit: 0
    --- stdout
    parser.rs: 1 edit, +0 -4
    @@ -17,6 +17,2 @@
         }
    -
    -    fn debug_dump(&self) {
    -        eprintln!("{}", self.src);
    -    }
     }
    debug.rs: 1 edit, +3 -0
    @@ -2,2 +2,5 @@
         fn trace(&self) {}
    +    fn debug_dump(&self) {
    +        eprintln!("{}", self.src);
    +    }
     }
    --- stderr
    "#);
}

#[test]
fn move_into_its_own_source_exits_1() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let out = ned(
        dir.path(),
        &["parser.rs", "-e", "move impl:Parser after fn:new"],
        "",
    );
    assert_snapshot!(out, @r"
    exit: 1
    --- stdout
    --- stderr
    error: script:1:1: move destination is inside the moved span at parser.rs:9-22; choose a destination outside it
    ");
    assert_eq!(read(&dir, "parser.rs"), PARSER);
}

#[test]
fn create_writes_a_new_file_and_its_directories() {
    let dir = dir_with(&[]);
    let script = "create src/new.rs <<END\nfn a() {}\nEND\n";
    let out = ned(dir.path(), &["-n"], script);
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    (dry run) src/new.rs: created, +1
    @@ -0,0 +1,1 @@
    +fn a() {}
    --- stderr
    ");
    assert!(!dir.path().join("src").exists());
    let out = ned(dir.path(), &["-q"], script);
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    src/new.rs: created, +1
    --- stderr
    ");
    assert_eq!(read(&dir, "src/new.rs"), "fn a() {}\n");
    let out = ned(dir.path(), &[], script);
    assert!(out.starts_with("exit: 1\n"), "{out}");
    assert!(out.contains("src/new.rs already exists"), "{out}");
}
