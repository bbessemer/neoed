//! End-to-end tests of `check` (command-language spec §4.1) against the fake
//! language server.

use std::fs;
use std::process::Output;
use std::thread;
use std::time::{Duration, Instant};

use assert_cmd::cargo::cargo_bin_cmd;
use tempfile::TempDir;

const FAKE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../ned-daemon/tests/fake_lsp.py"
);

/// A workspace whose Rust server is the fake, with private runtime and
/// config dirs; stops its daemon when dropped.
struct Workspace {
    dir: TempDir,
    runtime: TempDir,
    config: TempDir,
}

impl Workspace {
    fn new(lsp: &str) -> Workspace {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        let log = dir.path().join("lsp.log");
        let config = format!(
            "[format]\nrust = false\ngo = false\n\n[lsp]\nrust = [{FAKE:?}, {log:?}]\n{lsp}"
        );
        fs::write(dir.path().join(".ned.toml"), config).unwrap();
        Workspace {
            dir,
            runtime: tempfile::tempdir().unwrap(),
            config: tempfile::tempdir().unwrap(),
        }
    }

    /// With the fake given `flags`, and `lsp` added to the `[lsp]` table.
    fn with_flags(flags: &[&str], lsp: &str) -> Workspace {
        let ws = Workspace::new("");
        let log = ws.dir.path().join("lsp.log");
        let config = format!(
            "[format]\nrust = false\n\n[lsp]\nrust = [{FAKE:?}, {log:?}, {}]\n{lsp}",
            flags
                .iter()
                .map(|f| format!("{f:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        ws.write(".ned.toml", &config);
        ws
    }

    fn write(&self, name: &str, text: &str) {
        fs::write(self.dir.path().join(name), text).unwrap();
    }

    fn ned(&self, args: &[&str]) -> Output {
        cargo_bin_cmd!("ned")
            .args(args)
            .current_dir(self.dir.path())
            .env("XDG_RUNTIME_DIR", self.runtime.path())
            .env("XDG_CONFIG_HOME", self.config.path())
            .env_remove("NED_SESSION")
            .write_stdin("")
            .output()
            .unwrap()
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.dir.path().join(name)).unwrap()
    }

    /// Starts the workspace's daemon, so edits are checked.
    fn start(&self) {
        let out = self.ned(&["daemon", "start"]);
        assert!(out.status.success(), "{out:?}");
    }

    /// The texts the fake server was last sent for each document, once the
    /// last is `text` or five seconds have passed: the daemon answers once it
    /// has sent a change, before the server has logged it.
    fn texts_ending(&self, text: &str) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let texts = self.last_texts();
            if texts.last().is_some_and(|last| last == text) || Instant::now() > deadline {
                return texts;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn last_texts(&self) -> Vec<String> {
        let log = fs::read_to_string(self.dir.path().join("lsp.log")).unwrap_or_default();
        log.lines()
            .filter_map(|line| {
                // The server may be writing the last line.
                let message: serde_json::Value = serde_json::from_str(line).ok()?;
                match message["method"].as_str()? {
                    "textDocument/didOpen" => message["params"]["textDocument"]["text"]
                        .as_str()
                        .map(String::from),
                    "textDocument/didChange" => message["params"]["contentChanges"][0]["text"]
                        .as_str()
                        .map(String::from),
                    _ => None,
                }
            })
            .collect()
    }

    fn root(&self) -> std::path::PathBuf {
        self.dir.path().canonicalize().unwrap()
    }

    /// Runs `ned ARGS` in `cwd`, with the workspace's runtime and config dirs.
    fn ned_in(&self, cwd: &std::path::Path, args: &[&str]) -> Output {
        cargo_bin_cmd!("ned")
            .args(args)
            .current_dir(cwd)
            .env("XDG_RUNTIME_DIR", self.runtime.path())
            .env("XDG_CONFIG_HOME", self.config.path())
            .env_remove("NED_SESSION")
            .write_stdin("")
            .output()
            .unwrap()
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        self.ned(&["daemon", "stop"]);
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[test]
fn check_starts_the_daemon_and_prints_diagnostics() {
    let ws = Workspace::new("");
    ws.write("a.rs", "// done\nfn a() {} // ERROR\n// HINT\n");
    let out = ws.ned(&["a.rs", "-e", "check"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        text(&out.stdout),
        "a.rs:2:14: error: error here [fake F1]\n"
    );
    let status = text(&ws.ned(&["daemon", "status"]).stdout);
    assert!(
        status.contains("\n  fake_lsp.py: ready, 1 file\n"),
        "{status}"
    );
}

#[test]
fn check_takes_a_selector_and_a_level() {
    let ws = Workspace::new("");
    ws.write("a.rs", "// done ERROR\nfn a() {\n    // HINT\n}\n");
    let out = ws.ned(&["a.rs", "-e", "check fn:a hint"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(text(&out.stdout), "a.rs:3:8: hint: hint here [fake]\n");
}

#[test]
fn check_includes_save_time_checks() {
    let ws = Workspace::with_flags(&["flycheck"], "");
    ws.write("a.rs", "// done\nfn a() {} // CARGO\n");
    let out = ws.ned(&["a.rs", "-e", "check"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(text(&out.stdout), "a.rs:2:1: error: cargo here [cargo]\n");
    assert_eq!(text(&out.stderr), "");
}

#[test]
fn an_unfinished_save_time_check_is_a_note() {
    let ws = Workspace::with_flags(&["flycheck", "flycheck-slow"], "timeout = 1\n");
    ws.write("a.rs", "// done\nfn a() {} // CARGO\n");
    let out = ws.ned(&["a.rs", "-e", "check"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(text(&out.stdout), "a.rs:2:1: error: cargo here [cargo]\n");
    assert_eq!(
        text(&out.stderr),
        "note: fake_lsp.py's check on save didn't finish within 1s; raise [lsp] timeout\n"
    );
}

#[test]
fn edits_are_checked_without_saving() {
    let ws = Workspace::with_flags(&["flycheck"], "");
    ws.write("a.rs", CLEAN);
    ws.start();
    let out = ws.ned(&["a.rs", "-q", "-e", "insert after 2 \"// CARGO\""]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(text(&out.stdout), "a.rs: 1 edit, +1 -0\n");
    let log = fs::read_to_string(ws.dir.path().join("lsp.log")).unwrap();
    assert!(!log.contains("textDocument/didSave"), "{log}");
}

#[test]
fn check_without_a_server_exits_2() {
    let ws = Workspace::new("");
    ws.write("a.md", "# A\n");
    let out = ws.ned(&["a.md", "-e", "check"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(
        text(&out.stderr).contains("no language server for markdown"),
        "{out:?}"
    );
}

#[test]
fn a_missing_server_exits_3() {
    let ws = Workspace::new("go = [\"no-such-server-for-ned\"]\n");
    ws.write("a.go", "package a\n");
    let out = ws.ned(&["a.go", "-e", "check"]);
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("`no-such-server-for-ned` not found"),
        "{stderr}"
    );
}

const CLEAN: &str = "// done\nfn a() {}\n";

#[test]
fn an_edit_that_introduces_an_error_is_rejected() {
    let ws = Workspace::new("");
    ws.write("a.rs", CLEAN);
    ws.start();
    let out = ws.ned(&["a.rs", "-e", "insert after 2 \"// ERROR\""]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(
        text(&out.stderr),
        "error: edit introduces 1 error; fix it, or add `allow errors` to the script to apply it anyway\n\
         a.rs:3:4: error: error here [fake F1]\n"
    );
    assert_eq!(text(&out.stdout), "");
    assert_eq!(ws.read("a.rs"), CLEAN);
    let texts = ws.texts_ending(CLEAN);
    assert_eq!(
        texts.last().unwrap(),
        CLEAN,
        "the original is sent back: {texts:?}"
    );
}

#[test]
fn allow_errors_applies_the_edit_and_shows_the_error() {
    let ws = Workspace::new("");
    ws.write("a.rs", CLEAN);
    ws.start();
    let out = ws.ned(&["a.rs", "-e", "allow errors\ninsert after 2 \"// ERROR\""]);
    assert!(out.status.success(), "{out:?}");
    let stdout = text(&out.stdout);
    assert!(stdout.starts_with("a.rs: 1 edit, +1 -0\n"), "{stdout}");
    assert!(
        stdout.ends_with("+// ERROR\na.rs:3:4: error: error here [fake F1]\n"),
        "{stdout}"
    );
    assert_eq!(ws.read("a.rs"), format!("{CLEAN}// ERROR\n"));
}

#[test]
fn introduced_warnings_are_shown_even_quietly() {
    let ws = Workspace::new("");
    ws.write("a.rs", CLEAN);
    ws.start();
    let out = ws.ned(&["a.rs", "-q", "-e", "insert after 2 \"// WARN\""]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        text(&out.stdout),
        "a.rs: 1 edit, +1 -0\na.rs:3:4: warning: warn here [fake]\n"
    );
}

#[test]
fn existing_errors_are_not_reported() {
    let ws = Workspace::new("");
    ws.write("a.rs", "// done\n// ERROR\n");
    ws.start();
    let out = ws.ned(&["a.rs", "-q", "-e", "insert before 2 \"// fine\""]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(text(&out.stdout), "a.rs: 1 edit, +1 -0\n");
}

#[test]
fn without_a_daemon_edits_are_not_checked() {
    let ws = Workspace::new("");
    ws.write("a.rs", CLEAN);
    let out = ws.ned(&["a.rs", "-q", "-e", "insert after 2 \"// ERROR\""]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(text(&out.stdout), "a.rs: 1 edit, +1 -0\n");
    assert_eq!(
        text(&ws.ned(&["daemon", "status"]).stdout)
            .lines()
            .next()
            .unwrap()
            .split(' ')
            .next(),
        Some("no")
    );
}

#[test]
fn a_running_daemon_formats_when_no_formatter_is_installed() {
    let ws = Workspace::new("");
    let log = ws.dir.path().join("lsp.log");
    ws.write(
        ".ned.toml",
        &format!(
            "[format]\nrust = [\"ned-no-such-fmt\"]\n\n[lsp]\nrust = [{FAKE:?}, {log:?}, \"format\"]\n"
        ),
    );
    ws.write("a.rs", CLEAN);
    let edit = ["a.rs", "-q", "-e", "insert after 2 \"fn b() {}  \""];
    let out = ws.ned(&edit);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        text(&out.stderr),
        "note: ned-no-such-fmt not found; skipped formatting a.rs\n"
    );
    assert_eq!(ws.read("a.rs"), format!("{CLEAN}fn b() {{}}  \n"));

    ws.write("a.rs", CLEAN);
    ws.start();
    let out = ws.ned(&edit);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        text(&out.stdout),
        "a.rs: 1 edit, +1 -0\nfmt fake_lsp.py: +1 -1\n"
    );
    assert_eq!(text(&out.stderr), "");
    assert_eq!(ws.read("a.rs"), format!("{CLEAN}fn b() {{}}\n"));
}

#[test]
fn no_check_and_force_skip_blocking() {
    let ws = Workspace::new("");
    ws.write("a.rs", CLEAN);
    ws.start();
    let out = ws.ned(&[
        "a.rs",
        "-q",
        "--no-check",
        "-e",
        "insert after 2 \"// ERROR\"",
    ]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(text(&out.stdout), "a.rs: 1 edit, +1 -0\n");
    ws.write("a.rs", CLEAN);
    let out = ws.ned(&["a.rs", "-q", "--force", "-e", "insert after 2 \"// ERROR\""]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        text(&out.stdout),
        "a.rs: 1 edit, +1 -0\na.rs:3:4: error: error here [fake F1]\n"
    );
}

#[test]
fn a_blocked_dry_run_exits_1() {
    let ws = Workspace::new("");
    ws.write("a.rs", CLEAN);
    ws.start();
    let out = ws.ned(&["a.rs", "-n", "-e", "insert after 2 \"// ERROR\""]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(ws.texts_ending(CLEAN).last().unwrap(), CLEAN);
}

/// gopls blocks an edit that breaks the types: `cargo test -- --ignored`.
#[test]
#[ignore]
fn a_real_server_blocks_a_breaking_edit() {
    let ws = Workspace::new("");
    ws.write("go.mod", "module a\n\ngo 1.21\n");
    ws.write("a.go", "package a\n\nfunc A() int { return 1 }\n");
    ws.start();
    let out = ws.ned(&[
        "a.go",
        "-e",
        "replace \"return 1\" with \"return \\\"x\\\"\"",
    ]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(text(&out.stderr).contains("a.go:3:"), "{out:?}");
    let out = ws.ned(&["a.go", "-q", "-e", "replace \"return 1\" with \"return 2\""]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(text(&out.stdout), "a.go: 1 edit, +1 -1\n");
}

#[test]
fn workspace_mode_is_checked_by_the_workspace_daemon() {
    let ws = Workspace::new("");
    ws.write("a.rs", CLEAN);
    ws.start();
    let elsewhere = tempfile::tempdir().unwrap();
    let root = ws.root().display().to_string();
    let out = ws.ned_in(
        elsewhere.path(),
        &["-w", &root, "-e", "insert after fn:a \"// ERROR\""],
    );
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(
        text(&out.stderr).contains("error here [fake F1]"),
        "{out:?}"
    );
    assert_eq!(ws.read("a.rs"), CLEAN);
}

#[test]
fn rename_spawns_the_daemon_and_stays_inside_the_file_set_or_workspace() {
    let ws = Workspace::new("");
    ws.write("a.rs", "// done\nfn foo() {}\n");
    ws.write("b.rs", "fn g() { foo(); }\n");
    ws.write("c.rs", "fn h() { foo(); }\n");
    let out = ws.ned(&["a.rs", "b.rs", "-e", "rename fn:foo to bar"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(
        text(&out.stderr),
        "error: script:1:1: rename reaches files outside the file set: c.rs; add them to the file set, or use -w\n"
    );
    let out = ws.ned(&["-w", "-e", "rename fn:foo to bar"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = text(&out.stdout);
    for summary in ["a.rs: 1 edit", "b.rs: 1 edit", "c.rs: 1 edit"] {
        assert!(stdout.contains(summary), "{stdout}");
    }
    assert_eq!(ws.read("a.rs"), "// done\nfn bar() {}\n");
    assert_eq!(ws.read("c.rs"), "fn h() { bar(); }\n");
}

#[test]
fn refs_and_def_read_through_the_daemon() {
    let ws = Workspace::new("");
    ws.write("a.rs", "// done\nfn foo() {}\n");
    ws.write("b.rs", "fn g() {\n    foo();\n}\n");
    let out = ws.ned(&["-w", "-e", "show all fn:foo.refs"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(text(&out.stdout), "b.rs:2\n2:    foo();\n");
    let out = ws.ned(&["b.rs", "a.rs", "-e", r#"show "foo(".def"#]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(text(&out.stdout), "a.rs:2\n2:fn foo() {}\n");
}
