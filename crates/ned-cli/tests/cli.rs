//! End-to-end tests of the `ned` binary against docs/command-language.md.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};

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
markdown = false
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
        .env_remove("NED_SESSION")
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

/// `ned ARGS` with `--color always`, its escapes shown as `\e[...m`.
fn colored(dir: &Path, args: &[&str]) -> String {
    let args = [&["--color", "always"], args].concat();
    ned(dir, &args, "").replace('\x1b', r"\e")
}

#[test]
fn color_always_paints_reads_edits_and_messages() {
    let dir = dir_with(&[("parser.rs", PARSER), ("a.toml", "x = 1\n")]);
    let script = r#"replace "unexpected end" with "unexpected end of input""#;
    assert_snapshot!(colored(dir.path(), &["parser.rs", "a.toml", "-e", script]), @r#"
    exit: 0
    --- stdout
    \e[1mparser.rs: 1 edit, +1 -1\e[0m
    \e[36m@@ -14,3 +14,3 @@\e[0m
         \e[35mpub\e[0m \e[35mfn\e[0m \e[34mparse\e[0m(&\e[35mmut\e[0m \e[35mself\e[0m) -> \e[33mResult\e[0m<\e[33mAst\e[0m, \e[33mError\e[0m> {
    \e[31m-\e[0m\e[31m        \e[0m\e[35mlet\e[0m\e[31m tok = \e[0m\e[35mself\e[0m\e[31m.\e[0m\e[34mnext\e[0m\e[31m().\e[0m\e[34mexpect\e[0m\e[31m(\e[0m\e[32m"unexpected end"\e[0m\e[31m);\e[0m
    \e[32m+\e[0m\e[32m        \e[0m\e[35mlet\e[0m\e[32m tok = \e[0m\e[35mself\e[0m\e[32m.\e[0m\e[34mnext\e[0m\e[32m().\e[0m\e[34mexpect\e[0m\e[32m(\e[0m\e[32m"unexpected end of input"\e[0m\e[32m);\e[0m
             \e[35mself\e[0m.\e[34mparse_expr\e[0m(tok)
    --- stderr
    \e[1;36mnote:\e[0m read .toml files as text; syntax selectors skip them
    "#);
    assert_snapshot!(colored(dir.path(), &["a.toml", "-e", "show 1"]), @r"
    exit: 0
    --- stdout
    \e[1ma.toml:1\e[0m
    \e[2m1\e[0m x = 1
    --- stderr
    \e[1;36mnote:\e[0m read .toml files as text; syntax selectors skip them
    ");
    assert_snapshot!(colored(dir.path(), &["parser.rs", "-e", "outline"]), @r"
    exit: 0
    --- stdout
    \e[1mparser.rs\e[0m
    \e[2m1\e[0m \e[36mimport\e[0m (1)
    \e[2m3-7\e[0m \e[36mstruct\e[0m:Parser
    \e[2m9-22\e[0m \e[36mimpl\e[0m:Parser
      \e[2m10-12\e[0m \e[36mfn\e[0m:new
      \e[2m14-17\e[0m \e[36mfn\e[0m:parse
      \e[2m19-21\e[0m \e[36mfn\e[0m:debug_dump
    --- stderr
    ");
    assert_snapshot!(colored(dir.path(), &["parser.rs", "-e", "show fn:nope"]), @r"
    exit: 1
    --- stdout
    --- stderr
    \e[1;31merror:\e[0m script:1:6: fn:nope matches nothing in parser.rs; `outline` lists the items
    ");
}

#[test]
fn color_always_highlights_shown_code() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    assert_snapshot!(colored(dir.path(), &["parser.rs", "-e", "show fn:new"]), @r"
    exit: 0
    --- stdout
    \e[1mparser.rs:10-12\e[0m
    \e[2m10\e[0m     \e[35mpub\e[0m \e[35mfn\e[0m \e[34mnew\e[0m(src: &\e[33mstr\e[0m) -> \e[33mSelf\e[0m {
    \e[2m11\e[0m         \e[33mParser\e[0m { src: src.\e[34mto_string\e[0m(), pos: \e[36m0\e[0m }
    \e[2m12\e[0m     }
    --- stderr
    ");
}

#[test]
fn without_a_terminal_or_with_color_never_output_is_plain() {
    let dir = dir_with(&[("parser.rs", PARSER), ("a.toml", "x = 1\n")]);
    let script = "show fn:new; outline; show fn:nope";
    let auto = ned(dir.path(), &["parser.rs", "a.toml", "-e", script], "");
    let never = ned(
        dir.path(),
        &["--color", "never", "parser.rs", "a.toml", "-e", script],
        "",
    );
    assert_eq!(auto, never);
    assert!(!auto.contains('\x1b'), "{auto}");
}

#[test]
fn an_unknown_color_word_is_a_usage_error() {
    let dir = dir_with(&[]);
    assert_snapshot!(ned(dir.path(), &["--color", "sometimes", "-e", "show 1"], ""), @"
    exit: 2
    --- stdout
    --- stderr
    error: invalid value 'sometimes' for '--color <WHEN>': expected auto, always or never

    For more information, try '--help'.
    ");
}

#[test]
fn color_always_paints_usage_errors() {
    let dir = dir_with(&[]);
    assert_snapshot!(colored(dir.path(), &["--bogus"]), @r"
    exit: 2
    --- stdout
    --- stderr
    \e[1m\e[31merror:\e[0m unexpected argument '\e[33m--bogus\e[0m' found

      \e[32mtip:\e[0m to pass '\e[33m--bogus\e[0m' as a value, use '\e[32m-- --bogus\e[0m'

    \e[1m\e[4mUsage:\e[0m ned [OPTIONS] [FILES... | -w [DIR]] [-e SCRIPT]...
           ned help [TOPIC]    (the command language)
           ned daemon start|status|stop [DIR]
           ned history|undo [-s NAME] [-w DIR]

    For more information, try '\e[1m--help\e[0m'.
    ");
    let joined = ned(dir.path(), &["--color=always", "--bogus"], "");
    assert_eq!(
        joined.replace('\x1b', r"\e"),
        colored(dir.path(), &["--bogus"])
    );
    let plain = ned(dir.path(), &["--bogus"], "");
    assert!(!plain.contains('\x1b'), "{plain}");
}

#[test]
fn color_always_overrides_no_color_set_or_empty() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let painted = |no_color: &str| {
        let output = cargo_bin_cmd!("ned")
            .current_dir(dir.path())
            .env("NO_COLOR", no_color)
            .args(["--color", "always", "parser.rs", "-e", "show 1"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).replace('\x1b', r"\e")
    };
    let expected = colored(dir.path(), &["parser.rs", "-e", "show 1"]);
    for no_color in ["1", ""] {
        let out = painted(no_color);
        assert!(out.contains(r"\e["), "NO_COLOR={no_color:?}: {out}");
        assert!(expected.contains(&out), "NO_COLOR={no_color:?}: {out}");
    }
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
    error: script:2:1: edit overlaps command 1 at parser.rs:15; merge the two edits, or put a `|` between them
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
    let out = ned(dir.path(), &["parser.rs", "-e", "show refs:parse"], "");
    assert_snapshot!(out, @r#"
    exit: 2
    --- stdout
    --- stderr
    error: script:1:6: `refs:` is a part, not a kind; select the symbol and add .refs, e.g. fn:NAME.refs
    1:show refs:parse
           ^
    "#);
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
            "unknown language `ruby`; expected one of rust, python, typescript, tsx, javascript, go, markdown"
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
fn lines_of_an_item_are_listed_by_line_number() {
    let dir = dir_with(&[("a.rs", "fn a() {\n    x();\n}\n")]);
    let out = ned(dir.path(), &["a.rs", "-e", "show fn:a.lines"], "");
    assert_snapshot!(out, @r"
    exit: 1
    --- stdout
    --- stderr
    error: script:1:6: fn:a.lines matches 3 items; add `all` or use one of:
      fn:a>1   a.rs:1
      fn:a>2   a.rs:2
      fn:a>3   a.rs:3
    ");
}

#[test]
fn ambiguous_filter_lists_filtered_candidates() {
    let dir = dir_with(&[(
        "a.rs",
        "fn a() {}\n\nfn b() {\n    x();\n}\n\nfn c() {\n}\n",
    )]);
    let out = ned(dir.path(), &["a.rs", "-e", r#"show fn[.name != "a"]"#], "");
    assert_snapshot!(out, @r#"
    exit: 1
    --- stdout
    --- stderr
    error: script:1:6: fn[.name != "a"] matches 2 items; add `all` or use one of:
      fn:b[.name != "a"]   a.rs:3-5
      fn:c[.name != "a"]   a.rs:7-8
    "#);
}

#[test]
fn filter_type_error_exits_2_with_caret() {
    let dir = dir_with(&[("a.rs", "fn a() {}\n")]);
    let out = ned(dir.path(), &["a.rs", "-e", "show fn[.len ~= /x/]"], "");
    assert_snapshot!(out, @r"
    exit: 2
    --- stdout
    --- stderr
    error: script:1:14: .len is a number; compare it with == != < > <= >= and a number, e.g. .len > 80
    1:show fn[.len ~= /x/]
                   ^
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
fn replace_function_return_type() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let script = r#"replace fn:new.ret with "Parser""#;
    let out = ned(dir.path(), &["parser.rs", "-e", script], "");
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    parser.rs: 1 edit, +1 -1
    @@ -9,3 +9,3 @@
     impl Parser {
    -    pub fn new(src: &str) -> Self {
    +    pub fn new(src: &str) -> Parser {
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
fn a_use_over_several_lines_is_named_on_one_line() {
    let uses = "use a::b;\nuse c::{\n    d,\n    e,\n};\n";
    let dir = dir_with(&[("uses.rs", uses)]);
    let out = ned(
        dir.path(),
        &["uses.rs", "-e", r#"show import:"c::{d, e}""#],
        "",
    );
    assert_snapshot!(out, @r#"
    exit: 0
    --- stdout
    uses.rs:2-5
    2:use c::{
    3:    d,
    4:    e,
    5:};
    --- stderr
    "#);
    let out = ned(
        dir.path(),
        &["uses.rs", "-e", r#"show import:"c::{d, f}""#],
        "",
    );
    assert_snapshot!(out, @r#"
    exit: 1
    --- stdout
    --- stderr
    error: script:1:6: import:"c::{d, f}" matches nothing in uses.rs; did you mean import:"c::{d, e}" (2-5)?
    "#);
}

#[test]
fn missing_part_exits_1() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let out = ned(dir.path(), &["parser.rs", "-e", "show fn:new.doc"], "");
    assert_snapshot!(out, @r"
    exit: 1
    --- stdout
    --- stderr
    error: script:1:6: fn:new has no .doc; it has .body .sig .params .name .ret .whole .lines
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
fn outline_can_come_before_a_pipe() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let out = ned(dir.path(), &["parser.rs", "-e", "outline | show 1"], "");
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
    parser.rs:1
    1:use std::fmt;
    --- stderr
    ");
}

#[test]
fn outline_without_a_language_exits_1() {
    let dir = dir_with(&[("a.json", "{}\n")]);
    let out = ned(dir.path(), &["a.json", "-e", "outline"], "");
    assert_snapshot!(out, @r"
    exit: 1
    --- stdout
    --- stderr
    note: read .json files as text; syntax selectors skip them
    error: script:1:1: outline needs a language, but a.json has none; use --lang
    ");
}

#[test]
fn lang_text_edits_code_as_text() {
    let dir = dir_with(&[("a.rs", "fn a() {}\n")]);
    let out = ned(
        dir.path(),
        &["--lang", "text", "a.rs", "-e", "sub /\\{\\}/ with \"{\""],
        "",
    );
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    a.rs: 1 edit, +1 -1
    @@ -1,1 +1,1 @@
    -fn a() {}
    +fn a() {
    --- stderr
    ");
    assert_eq!(read(&dir, "a.rs"), "fn a() {\n");
    let out = ned(
        dir.path(),
        &["--lang", "text", "a.rs", "-e", "show fn:a"],
        "",
    );
    assert_snapshot!(out, @r"
    exit: 1
    --- stdout
    --- stderr
    error: script:1:6: fn:a needs a language, but parsing was disabled with --lang text; drop it, or use a regex or literal
    ");
}

#[test]
fn unknown_extensions_are_read_as_text_with_one_note() {
    let dir = dir_with(&[
        ("a.toml", "x = 1\n"),
        ("b.json", "{\"x\": 1}\n"),
        ("c.toml", "x = 2\n"),
        ("d.txt", "x\n"),
        ("Makefile", "x:\n"),
    ]);
    let out = ned(
        dir.path(),
        &[
            "a.toml",
            "b.json",
            "c.toml",
            "d.txt",
            "Makefile",
            "-e",
            "show all /x/",
        ],
        "",
    );
    assert_snapshot!(out, @r#"
    exit: 0
    --- stdout
    a.toml:1
    1:x = 1
    b.json:1
    1:{"x": 1}
    c.toml:1
    1:x = 2
    d.txt:1
    1:x
    Makefile:1
    1:x:
    --- stderr
    note: read .json, .toml files as text; syntax selectors skip them
    "#);
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

#[test]
fn pattern_selector_edits_matches() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let out = ned(
        dir.path(),
        &["parser.rs", "-e", "delete `eprintln!(@_...);`"],
        "",
    );
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
fn invalid_pattern_exits_2() {
    let dir = dir_with(&[("parser.rs", PARSER)]);
    let out = ned(dir.path(), &["parser.rs", "-e", "show `fn (@a`"], "");
    assert_snapshot!(out, @r"
    exit: 2
    --- stdout
    --- stderr
    error: script:1:6: `fn (@a` doesn't parse as rust at `@a`; add the code around it, or use query{}
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
    error: .ned.toml:2:1: invalid config: unknown language `ruby`; expected one of rust, python, typescript, tsx, javascript, go, markdown
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

#[test]
fn an_off_by_one_replace_notes_it_on_stderr() {
    let dir = dir_with(&[("a.txt", "- [ ] Foo bar\n")]);
    let out = ned(
        dir.path(),
        &["a.txt", "-q"],
        "replace /^- \\[ \\] Foo/ with \"- [x] Foo bar\"\n",
    );
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    a.txt: 1 edit, +1 -1
    --- stderr
    note: a.txt:1: the new text ends with `bar`, which already follows the replaced text on its line; to replace whole lines, select /^- \[ \] Foo/.lines
    ");
}

#[test]
fn a_command_named_as_a_file_suggests_the_script_form() {
    let dir = dir_with(&[("a.rs", "fn a() {}\n")]);
    assert_snapshot!(ned(dir.path(), &["outline", "a.rs"], ""), @r"
    exit: 2
    --- stdout
    --- stderr
    error: `outline` is a command, not a file; give the script with -e: ned a.rs -e 'outline'
    ");
    assert_snapshot!(ned(dir.path(), &["show", "fn:a", "a.rs"], ""), @r"
    exit: 2
    --- stdout
    --- stderr
    error: `show` is a command, not a file; give the script with -e: ned a.rs -e 'show fn:a'
    ");
    assert_snapshot!(ned(dir.path(), &["delete", "a.rs", "-e", "outline"], ""), @r"
    exit: 2
    --- stdout
    --- stderr
    error: `delete` is a command, not a file; give the script with -e: ned a.rs -e 'delete'
    ");
}

#[test]
fn a_subcommand_after_a_flag_suggests_it_first() {
    let dir = dir_with(&[("a.rs", "fn a() {}\n")]);
    assert_snapshot!(ned(dir.path(), &["-s", "x", "undo"], ""), @r"
    exit: 2
    --- stdout
    --- stderr
    error: `undo` is a subcommand, not a file; give it first: ned undo -s x
    ");
    assert_snapshot!(ned(dir.path(), &["-w", ".", "history"], ""), @r"
    exit: 2
    --- stdout
    --- stderr
    error: `history` is a subcommand, not a file; give it first: ned history -w .
    ");
    assert_snapshot!(ned(dir.path(), &["-w", "history", "--all"], ""), @r"
    exit: 2
    --- stdout
    --- stderr
    error: `history` is a subcommand, not a file; give it first: ned history --all
    ");
    assert_snapshot!(ned(dir.path(), &["--force", "-ns", "x", "undo"], ""), @r"
    exit: 2
    --- stdout
    --- stderr
    error: `undo` is a subcommand, not a file; give it first: ned undo --force -s x
    ");
    assert_snapshot!(ned(dir.path(), &["-e", "show 1", "help", "outline"], ""), @r"
    exit: 2
    --- stdout
    --- stderr
    error: `help` is a subcommand, not a file; give it first: ned help outline
    ");
    for (args, fix) in [
        (&["--session=x", "undo"][..], "ned undo --session=x"),
        (&["-sx", "undo"], "ned undo -s x"),
        (&["-n", "a.rs", "history"], "ned history"),
        (&["-X", "undo"], "ned undo"),
    ] {
        let out = ned(dir.path(), args, "");
        assert!(
            out.ends_with(&format!("give it first: {fix}\n")),
            "{args:?}: {out}"
        );
    }
}

#[test]
fn a_subcommand_after_no_flag_is_a_file() {
    let dir = dir_with(&[("a.rs", "fn a() {}\n")]);
    assert_snapshot!(ned(dir.path(), &["a.rs", "undo"], ""), @r"
    exit: 3
    --- stdout
    --- stderr
    error: cannot read undo: no such file (paths are relative to {dir})
    ");
    assert_snapshot!(ned(dir.path(), &["--", "undo"], ""), @r"
    exit: 3
    --- stdout
    --- stderr
    error: cannot read undo: no such file (paths are relative to {dir})
    ");
    assert_snapshot!(ned(dir.path(), &["-s", "undo", "a.rs", "-e", "outline"], ""), @r"
    exit: 0
    --- stdout
    a.rs
    1 fn:a
    --- stderr
    ");
}

#[test]
fn a_file_named_as_a_subcommand_is_still_a_file() {
    let dir = dir_with(&[("undo", "a\n")]);
    let script = ["-e", "replace 1 with \"b\""];
    assert_snapshot!(ned(dir.path(), &[&["-n", "undo"][..], &script].concat(), ""), @r"
    exit: 0
    --- stdout
    (dry run) undo: 1 edit, +1 -1
    @@ -1,1 +1,1 @@
    -a
    +b
    --- stderr
    ");
    assert_snapshot!(ned(dir.path(), &[&["-n", "./undo"][..], &script].concat(), ""), @r"
    exit: 0
    --- stdout
    (dry run) ./undo: 1 edit, +1 -1
    @@ -1,1 +1,1 @@
    -a
    +b
    --- stderr
    ");
}

const NOTES: &str = "# Notes\n\n## Todo\n\n- [ ] one\n      more\n- [ ] two\n\n## Done\n\ntext\n";

#[test]
fn markdown_outline_and_edits() {
    let dir = dir_with(&[("notes.md", NOTES)]);
    let out = ned(dir.path(), &["notes.md", "-e", "outline"], "");
    assert_snapshot!(out, @r#"
    exit: 0
    --- stdout
    notes.md
    1-11 section:Notes
      3-7 section:Todo
      9-11 section:Done
    --- stderr
    "#);
    let script = "insert after item:one \"- [ ] one and a half\"\nreplace section:Done with <<END\n## Done\n\n- [x] zero\nEND\n";
    let out = ned(dir.path(), &["-q", "notes.md"], script);
    assert!(out.starts_with("exit: 0\n"), "{out}");
    assert_eq!(
        read(&dir, "notes.md"),
        "# Notes\n\n## Todo\n\n- [ ] one\n      more\n- [ ] one and a half\n- [ ] two\n\n## Done\n\n- [x] zero\n"
    );
}

#[test]
fn python_outline_and_edits() {
    let dir = dir_with(&[("app.py", APP)]);
    let out = ned(dir.path(), &["app.py", "-e", "outline"], "");
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    app.py
    1-5 fn:handle
    --- stderr
    ");
    let script =
        "replace fn:handle.params with \"req, log\"\ninsert start fn:handle \"trace(req)\"\n";
    let out = ned(dir.path(), &["-q", "app.py"], script);
    assert!(out.starts_with("exit: 0\n"), "{out}");
    assert_eq!(
        read(&dir, "app.py"),
        "def handle(req, log):\n    trace(req)\n    if req.ok:\n        log(req)\n        return 200\n    return 500\n"
    );
}

const CONFLICTED: &str = "<<<<<<< HEAD\ndef ours():\n    return 1\n=======\ndef theirs():\n    return 2\n>>>>>>> topic\n";

#[test]
fn files_with_merge_conflicts_parse_and_edit() {
    let dir = dir_with(&[("app.py", CONFLICTED)]);
    let out = ned(dir.path(), &["app.py", "-e", "outline"], "");
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    app.py
    2-3 fn:ours
    5-6 fn:theirs
    1-7 conflict:1
    --- stderr
    ");
    let out = ned(
        dir.path(),
        &["-q", "app.py", "-e", "replace fn:theirs>\"2\" with \"3\""],
        "",
    );
    assert!(out.starts_with("exit: 0\n"), "{out}");
    assert_eq!(read(&dir, "app.py"), CONFLICTED.replace("2", "3"));
}

#[test]
fn conflict_sides_in_a_text_file() {
    let notes = "a\n<<<<<<< HEAD\nb\n=======\nc\n>>>>>>> topic\n";
    let dir = dir_with(&[("notes.txt", notes)]);
    let out = ned(dir.path(), &["notes.txt", "-e", "show conflict.theirs"], "");
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    notes.txt:5
    5:c
    --- stderr
    ");
}

#[test]
fn resolve_keeps_one_side_of_a_conflict() {
    let dir = dir_with(&[("app.py", CONFLICTED)]);
    let out = ned(dir.path(), &["app.py", "-e", "resolve conflict theirs"], "");
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    app.py: 1 edit, +0 -5
    @@ -1,7 +1,2 @@
    -<<<<<<< HEAD
    -def ours():
    -    return 1
    -=======
     def theirs():
         return 2
    ->>>>>>> topic
    --- stderr
    ");
    assert_eq!(read(&dir, "app.py"), "def theirs():\n    return 2\n");
}

#[test]
fn resolve_and_replace_keep_crlf_line_endings() {
    let text = "a\r\n<<<<<<< HEAD\r\none\r\n=======\r\ntwo\r\n>>>>>>> topic\r\nb\r\n";
    let dir = dir_with(&[("a.txt", text), ("b.txt", text)]);
    let out = ned(
        dir.path(),
        &["a.txt", "-q", "-e", "resolve conflict theirs"],
        "",
    );
    assert!(out.starts_with("exit: 0\n"), "{out}");
    assert_eq!(read(&dir, "a.txt"), "a\r\ntwo\r\nb\r\n");
    let out = ned(
        dir.path(),
        &["b.txt", "-q", "-e", "replace conflict with \"x\\ny\""],
        "",
    );
    assert!(out.starts_with("exit: 0\n"), "{out}");
    assert_eq!(read(&dir, "b.txt"), "a\r\nx\r\ny\r\nb\r\n");
}

#[test]
fn go_outline_and_edits() {
    let text = "package main\n\ntype T struct{}\n\nfunc (t T) Run() {\n\tstart()\n}\n";
    let dir = dir_with(&[("main.go", text)]);
    let out = ned(dir.path(), &["main.go", "-e", "outline"], "");
    assert_snapshot!(out, @r#"
    exit: 0
    --- stdout
    main.go
    3 struct:T
    5-7 fn:"T.Run"
    --- stderr
    "#);
    let out = ned(
        dir.path(),
        &["-q", "main.go"],
        "insert end fn:Run \"stop()\"\n",
    );
    assert!(out.starts_with("exit: 0\n"), "{out}");
    assert_eq!(
        read(&dir, "main.go"),
        "package main\n\ntype T struct{}\n\nfunc (t T) Run() {\n\tstart()\n\tstop()\n}\n"
    );
}

#[test]
fn typescript_outline_and_edits() {
    let text = "import { a } from \"./a\";\n\nexport interface Shape {\n  area(): number;\n}\n\nexport function main(): void {\n  a();\n}\n";
    let dir = dir_with(&[("main.ts", text)]);
    let out = ned(dir.path(), &["main.ts", "-e", "outline"], "");
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    main.ts
    1 import (1)
    3-5 interface:Shape
      4 fn:area
    7-9 fn:main
    --- stderr
    ");
    let script = "insert end interface:Shape \"name: string;\"\ninsert start fn:main \"init();\"\n";
    let out = ned(dir.path(), &["-q", "main.ts"], script);
    assert!(out.starts_with("exit: 0\n"), "{out}");
    assert_eq!(
        read(&dir, "main.ts"),
        "import { a } from \"./a\";\n\nexport interface Shape {\n  area(): number;\n  name: string;\n}\n\nexport function main(): void {\n  init();\n  a();\n}\n"
    );
}

#[test]
fn a_closed_stdout_does_not_stop_the_script() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("a.txt");
    fs::write(&path, "x\n".repeat(50_000)).unwrap();
    // More output than a pipe holds, so ned writes after the reader is gone.
    let mut child = Command::new(env!("CARGO_BIN_EXE_ned"))
        .arg(&path)
        .args(["-e", "show all /x/; replace 1 with \"y\""])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(fs::read_to_string(&path).unwrap().starts_with("y\nx\n"));
}
