//! End-to-end tests of sessions (command-language spec §1.2).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use insta::assert_snapshot;
use serde_json::Value;
use tempfile::TempDir;

/// A git workspace with private state, config and runtime dirs.
struct Workspace {
    dir: TempDir,
    state: TempDir,
    config: TempDir,
    runtime: TempDir,
}

impl Workspace {
    fn new(files: &[(&str, &str)]) -> Workspace {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        for (name, text) in files {
            fs::write(dir.path().join(name), text).unwrap();
        }
        let config = tempfile::tempdir().unwrap();
        fs::create_dir(config.path().join("ned")).unwrap();
        fs::write(
            config.path().join("ned/config.toml"),
            "[format]\nrust = false\n",
        )
        .unwrap();
        Workspace {
            dir,
            state: tempfile::tempdir().unwrap(),
            config,
            runtime: tempfile::tempdir().unwrap(),
        }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap()
    }

    /// Runs `ned ARGS` in the workspace, with `NED_SESSION` set to `session`.
    fn ned_with(&self, session: Option<&str>, args: &[&str]) -> Output {
        let mut cmd = cargo_bin_cmd!("ned");
        cmd.args(args)
            .current_dir(self.dir.path())
            .env("XDG_STATE_HOME", self.state.path())
            .env("XDG_CONFIG_HOME", self.config.path())
            .env("XDG_RUNTIME_DIR", self.runtime.path())
            .env_remove("NED_SESSION")
            .write_stdin("");
        if let Some(session) = session {
            cmd.env("NED_SESSION", session);
        }
        cmd.output().unwrap()
    }

    fn ned(&self, args: &[&str]) -> Output {
        self.ned_with(None, args)
    }

    /// `ned ARGS`'s exit code, stdout and stderr, with the workspace's path
    /// replaced by `{dir}`.
    fn report(&self, args: &[&str]) -> String {
        let output = self.ned(args);
        format!(
            "exit: {}\n--- stdout\n{}--- stderr\n{}",
            output.status.code().unwrap(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        )
        .replace(self.root().to_str().unwrap(), "{dir}")
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.dir.path().join(name)).unwrap()
    }

    fn sessions_dir(&self) -> PathBuf {
        self.state.path().join("ned/sessions")
    }

    /// The entries of session `name`'s log.
    fn entries(&self, name: &str) -> Vec<Value> {
        let dirs: Vec<_> = fs::read_dir(self.sessions_dir())
            .unwrap()
            .map(|dir| dir.unwrap().path())
            .collect();
        assert_eq!(dirs.len(), 1, "{dirs:?}");
        let text = fs::read_to_string(dirs[0].join(format!("{name}.log"))).unwrap();
        let mut lines = text.lines();
        let header: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(header["ned_session"], 1);
        assert_eq!(header["workspace"], self.root().to_str().unwrap());
        lines
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

fn path(value: &Value) -> &Path {
    Path::new(value.as_str().unwrap())
}

#[test]
fn an_edit_is_recorded_with_each_files_text() {
    let ws = Workspace::new(&[("a.rs", "fn a() {}\n"), ("b.rs", "fn b() {}\n")]);
    let script = "replace fn:a with \"fn a2() {}\"\ncreate c.rs \"fn c() {}\"";
    let output = ws.ned(&["-s", "agent", "a.rs", "b.rs", "-e", script]);
    assert!(output.status.success(), "{output:?}");

    let entries = ws.entries("agent");
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry["id"], 1);
    assert!(entry["time"].as_u64().unwrap() > 1_700_000_000);
    assert_eq!(path(&entry["cwd"]), ws.root());
    assert_eq!(entry["files"], serde_json::json!(["a.rs", "b.rs"]));
    assert_eq!(entry["workspace"], Value::Null);
    assert_eq!(entry["script"], script);
    assert_eq!(entry["undoes"], Value::Null);
    assert_eq!(entry["dry_run"], false);
    assert_eq!(entry["exit"], 0);
    assert_eq!(entry["error"], Value::Null);
    let changes = entry["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 2);
    assert_eq!(path(&changes[0]["path"]), ws.root().join("a.rs"));
    assert_eq!(changes[0]["before"], "fn a() {}\n");
    assert_eq!(changes[0]["after"], "fn a2() {}\n");
    assert_eq!(path(&changes[1]["path"]), ws.root().join("c.rs"));
    assert_eq!(changes[1]["before"], Value::Null);
    assert_eq!(changes[1]["after"], ws.read("c.rs"));
}

#[test]
fn reads_failures_and_dry_runs_are_recorded_without_changes() {
    let ws = Workspace::new(&[("a.rs", "fn a() {}\n")]);
    let runs: [&[&str]; 4] = [
        &["a.rs", "-e", "show fn:a"],
        &["a.rs", "-e", "delete fn:b"],
        &["-n", "a.rs", "-e", "delete fn:a"],
        &["a.rs", "-e", "replace fn:a with"],
    ];
    for args in runs {
        ws.ned_with(Some("agent"), args);
    }
    assert_eq!(ws.read("a.rs"), "fn a() {}\n");

    let entries = ws.entries("agent");
    let fields: Vec<_> = entries
        .iter()
        .map(|e| (e["exit"].as_u64().unwrap(), e["dry_run"].as_bool().unwrap()))
        .collect();
    assert_eq!(fields, [(0, false), (1, false), (0, true), (2, false)]);
    assert!(
        entries
            .iter()
            .all(|e| e["changes"] == serde_json::json!([]))
    );
    assert_eq!(entries[0]["error"], Value::Null);
    let error = entries[1]["error"].as_str().unwrap();
    assert!(error.contains("fn:b matches nothing"), "{error}");
    let error = entries[3]["error"].as_str().unwrap();
    assert!(error.contains("usage: replace"), "{error}");
}

#[test]
fn history_lists_the_sessions_entries() {
    let ws = Workspace::new(&[("a.rs", "fn a() {}\n")]);
    ws.ned(&["-s", "agent", "a.rs", "-e", "show fn:a"]);
    ws.ned(&["-s", "agent", "a.rs", "-e", "delete fn:b"]);
    ws.ned(&["-s", "agent", "-n", "a.rs", "-e", "delete fn:a"]);
    ws.ned(&["-s", "agent", "a.rs", "-e", "outline", "-e", "show 1"]);
    ws.ned(&[
        "-s",
        "agent",
        "a.rs",
        "-e",
        "replace fn:a with \"fn b() {}\"",
    ]);
    ws.ned(&["-s", "other", "a.rs", "-e", "show 1"]);
    assert_snapshot!(ws.report(&["history", "-s", "agent"]), @r#"
    exit: 0
    --- stdout
    1 ok: show fn:a
    2 exit 1: delete fn:b
    3 dry run: delete fn:a
    4 ok: outline (+1 line)
    5 ok, 1 file: replace fn:a with "fn b() {}"
    --- stderr
    "#);
}

#[test]
fn ned_session_names_the_session_and_s_overrides_it() {
    let ws = Workspace::new(&[("a.rs", "fn a() {}\n")]);
    ws.ned_with(Some("agent"), &["a.rs", "-e", "show 1"]);
    ws.ned_with(Some("agent"), &["-s", "other", "a.rs", "-e", "show fn:a"]);
    ws.ned_with(Some(""), &["a.rs", "-e", "outline"]);
    assert_eq!(ws.entries("agent").len(), 1);
    assert_eq!(ws.entries("other")[0]["script"], "show fn:a");

    let output = ws.ned_with(Some("agent"), &["history"]);
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "1 ok: show 1\n");
}

#[test]
fn nothing_is_recorded_without_a_session() {
    let ws = Workspace::new(&[("a.rs", "fn a() {}\n")]);
    let output = ws.ned(&["a.rs", "-e", "delete fn:a"]);
    assert!(output.status.success(), "{output:?}");
    assert!(!ws.state.path().join("ned").exists());
}

#[test]
fn a_workspace_run_records_its_root() {
    let ws = Workspace::new(&[("a.rs", "fn a() {}\n")]);
    ws.ned(&["-s", "agent", "-w", "-e", "show all fn"]);
    let entry = &ws.entries("agent")[0];
    assert_eq!(path(&entry["workspace"]), ws.root());
    assert_eq!(entry["files"], serde_json::json!([]));
}

#[test]
fn the_recorded_text_is_the_formatted_text() {
    let ws = Workspace::new(&[
        ("a.rs", "fn a() {}\n"),
        (
            ".ned.toml",
            "[format]\nrust = [\"sh\", \"-c\", \"sed 's/;;/;/'\"]\n",
        ),
    ]);
    ws.ned(&[
        "-s",
        "agent",
        "a.rs",
        "-e",
        r#"replace fn:a with "fn a() { x;; }""#,
    ]);
    assert_eq!(ws.read("a.rs"), "fn a() { x; }\n");
    assert_eq!(
        ws.entries("agent")[0]["changes"][0]["after"],
        "fn a() { x; }\n"
    );
}

#[test]
fn history_without_a_session_lists_the_workspaces_sessions() {
    let ws = Workspace::new(&[("a.rs", "fn a() {}\n")]);
    assert_snapshot!(ws.report(&["history"]), @r"
    exit: 2
    --- stdout
    --- stderr
    error: no session; give one with -s NAME or NED_SESSION (none recorded in this workspace yet)
    ");
    ws.ned(&["-s", "beta", "a.rs", "-e", "show 1"]);
    ws.ned(&["-s", "alpha", "a.rs", "-e", "show 1"]);
    assert_snapshot!(ws.report(&["history"]), @r"
    exit: 2
    --- stdout
    --- stderr
    error: no session; give one with -s NAME or NED_SESSION (sessions in this workspace: alpha, beta)
    ");
    assert_snapshot!(ws.report(&["history", "-s", "gamma"]), @r"
    exit: 2
    --- stdout
    --- stderr
    error: no session `gamma` in this workspace; sessions in it: alpha, beta
    ");
}

#[test]
fn history_shows_the_last_ten_entries_unless_all() {
    let ws = Workspace::new(&[("a.rs", "fn a() {}\n")]);
    for i in 1..=11 {
        ws.ned(&["-s", "agent", "a.rs", "-e", &format!("show {}", i % 2 + 1)]);
    }
    let stdout = |args: &[&str]| String::from_utf8(ws.ned(args).stdout).unwrap();
    let last = stdout(&["history", "-s", "agent"]);
    assert_eq!(last.lines().count(), 10);
    assert!(last.starts_with("2 ok: show 1\n"), "{last}");
    assert_eq!(
        stdout(&["history", "-s", "agent", "--all"]).lines().count(),
        11
    );
}

#[test]
fn a_bad_session_name_is_a_usage_error() {
    let ws = Workspace::new(&[("a.rs", "fn a() {}\n")]);
    assert_snapshot!(ws.report(&["-s", "a/b", "a.rs", "-e", "delete fn:a"]), @r"
    exit: 2
    --- stdout
    --- stderr
    error: invalid session name `a/b`; use letters, digits, `.`, `_` and `-`, not starting with `.`
    ");
    assert_eq!(ws.read("a.rs"), "fn a() {}\n");
}

#[test]
fn an_unsafe_session_dir_is_refused_before_editing() {
    use std::os::unix::fs::PermissionsExt;
    let ws = Workspace::new(&[("a.rs", "fn a() {}\n")]);
    let dir = ws.state.path().join("ned");
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    let output = ws.ned(&["-s", "agent", "a.rs", "-e", "delete fn:a"]);
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("XDG_STATE_HOME"), "{stderr}");
    assert_eq!(ws.read("a.rs"), "fn a() {}\n");
}
