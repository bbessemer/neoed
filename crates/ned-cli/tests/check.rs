//! End-to-end tests of `check` (command-language spec §4.1) against the fake
//! language server.

use std::fs;
use std::process::Output;

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
        let config = format!("[lsp]\nrust = [{FAKE:?}, {log:?}]\n{lsp}");
        fs::write(dir.path().join(".ned.toml"), config).unwrap();
        Workspace {
            dir,
            runtime: tempfile::tempdir().unwrap(),
            config: tempfile::tempdir().unwrap(),
        }
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
