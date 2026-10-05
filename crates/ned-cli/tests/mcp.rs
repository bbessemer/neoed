//! End-to-end tests of `ned mcp` (command-language spec §1.5), its requests
//! piped.

use std::fs;
use std::path::PathBuf;
use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use serde_json::{Value, json};
use tempfile::TempDir;

/// A workspace with private state, config and runtime dirs.
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

    /// Runs `ned ARGS` in the workspace with `input` on stdin.
    fn ned(&self, args: &[&str], input: &str) -> Output {
        cargo_bin_cmd!("ned")
            .args(args)
            .envs(self.env())
            .current_dir(self.dir.path())
            .env_remove("NED_SESSION")
            .write_stdin(input)
            .output()
            .unwrap()
    }

    fn env(&self) -> Vec<(&'static str, PathBuf)> {
        vec![
            ("XDG_STATE_HOME", self.state.path().to_path_buf()),
            ("XDG_CONFIG_HOME", self.config.path().to_path_buf()),
            ("XDG_RUNTIME_DIR", self.runtime.path().to_path_buf()),
        ]
    }

    /// `ned mcp ARGS` given `requests`, one per line: its responses, and its
    /// stderr.
    fn mcp(&self, args: &[&str], requests: &[Value]) -> (Vec<Value>, String) {
        let args: Vec<&str> = ["mcp"].iter().chain(args).copied().collect();
        let input: String = requests.iter().map(|r| format!("{r}\n")).collect();
        let output = self.ned(&args, &input);
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        let responses = stdout
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        (responses, stderr)
    }
}

fn request(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn initialize() -> Value {
    let params = json!({
        "protocolVersion": "2025-06-18",
        "capabilities": {},
        "clientInfo": {"name": "test", "version": "0"},
    });
    request(1, "initialize", params)
}

#[test]
fn serves_until_stdin_ends_and_names_its_session() {
    let ws = Workspace::new(&[]);
    let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    let (responses, stderr) = ws.mcp(
        &[],
        &[initialize(), initialized, request(2, "ping", json!({}))],
    );
    assert_eq!(responses.len(), 2);
    let version = String::from_utf8(ws.ned(&["-V"], "").stdout).unwrap();
    let server = &responses[0]["result"]["serverInfo"];
    assert_eq!(
        format!("ned {}\n", server["version"].as_str().unwrap()),
        version
    );
    assert_eq!(stderr, "note: recording in session mcp-1\n");
}

#[test]
fn the_session_is_s_or_ned_session_or_the_next_free_mcp_n() {
    let ws = Workspace::new(&[]);
    let (_, stderr) = ws.mcp(&["-s", "agent"], &[]);
    assert_eq!(stderr, "note: recording in session agent\n");
    let output = cargo_bin_cmd!("ned")
        .args(["mcp"])
        .envs(ws.env())
        .env("NED_SESSION", "env-agent")
        .current_dir(ws.dir.path())
        .write_stdin("")
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stderr, "note: recording in session env-agent\n");
}

#[test]
fn file_arguments_and_unknown_flags_are_usage_errors() {
    let ws = Workspace::new(&[("a.rs", "fn a() {}\n")]);
    assert_eq!(ws.ned(&["mcp", "a.rs"], "").status.code(), Some(2));
    assert_eq!(ws.ned(&["mcp", "-n"], "").status.code(), Some(2));
}
