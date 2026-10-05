//! End-to-end tests of `ned mcp` (command-language spec §1.5), its requests
//! piped.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

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
        let home = self.state.path();
        vec![
            ("XDG_STATE_HOME", home.to_path_buf()),
            ("XDG_CONFIG_HOME", self.config.path().to_path_buf()),
            ("XDG_RUNTIME_DIR", self.runtime.path().to_path_buf()),
            ("GIT_CONFIG_GLOBAL", home.join("gitconfig")),
            ("GIT_CONFIG_NOSYSTEM", "1".into()),
            ("GIT_AUTHOR_NAME", "A U Thor".into()),
            ("GIT_AUTHOR_EMAIL", "author@example.com".into()),
            ("GIT_COMMITTER_NAME", "C O Mitter".into()),
            ("GIT_COMMITTER_EMAIL", "committer@example.com".into()),
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

    /// The results of calling each tool, after `initialize`.
    fn calls(&self, args: &[&str], calls: &[(&str, Value)]) -> Vec<Value> {
        let mut requests = vec![initialize()];
        for (id, (name, arguments)) in (2..).zip(calls) {
            let params = json!({ "name": name, "arguments": arguments });
            requests.push(request(id, "tools/call", params));
        }
        let (responses, _) = self.mcp(args, &requests);
        let results = responses[1..].iter().map(|r| r["result"].clone());
        results.collect()
    }

    /// What `ned ARGS` prints: stdout, then stderr.
    fn printed(&self, args: &[&str]) -> String {
        let output = self.ned(args, "");
        String::from_utf8(output.stdout).unwrap() + &String::from_utf8(output.stderr).unwrap()
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.dir.path().join(name)).unwrap()
    }

    /// Runs `git ARGS` in the workspace and returns its stdout.
    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .envs(self.env())
            .current_dir(self.dir.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    /// A git repository with one commit of its files.
    fn repo(files: &[(&str, &str)]) -> Workspace {
        let ws = Workspace::new(files);
        fs::remove_dir(ws.dir.path().join(".git")).unwrap();
        ws.git(&["init", "-q", "-b", "main"]);
        ws.git(&["add", "."]);
        ws.git(&["commit", "-q", "-m", "init"]);
        ws
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

const AB: &str = "fn a() {}\nfn b() {}\n";

fn text(result: &Value) -> &str {
    result["content"][0]["text"].as_str().unwrap()
}

fn failed(result: &Value) -> bool {
    result["isError"].as_bool().unwrap()
}

#[test]
fn the_ned_tool_prints_what_ned_prints_and_writes_the_edits() {
    let (ws, twin) = (
        Workspace::new(&[("a.rs", AB)]),
        Workspace::new(&[("a.rs", AB)]),
    );
    let script = r#"replace fn:a.name with "c""#;
    let results = ws.calls(
        &[],
        &[("ned", json!({ "script": script, "files": ["a.rs"] }))],
    );
    assert!(!failed(&results[0]));
    assert_eq!(text(&results[0]), twin.printed(&["a.rs", "-e", script]));
    assert_eq!(ws.read("a.rs"), "fn c() {}\nfn b() {}\n");
}

#[test]
fn a_dry_run_writes_nothing() {
    let (ws, twin) = (
        Workspace::new(&[("a.rs", AB)]),
        Workspace::new(&[("a.rs", AB)]),
    );
    let script = r#"replace fn:a.name with "c""#;
    let call = json!({ "script": script, "files": ["a.rs"], "dry_run": true });
    let results = ws.calls(&[], &[("ned", call)]);
    assert_eq!(
        text(&results[0]),
        twin.printed(&["-n", "a.rs", "-e", script])
    );
    assert_eq!(ws.read("a.rs"), AB);
}

#[test]
fn a_failing_call_is_an_error_ending_with_its_exit_code() {
    let (ws, twin) = (
        Workspace::new(&[("a.rs", AB)]),
        Workspace::new(&[("a.rs", AB)]),
    );
    let (missing, unparsed) = (r#"replace fn:nope with "x""#, "replace");
    let results = ws.calls(
        &[],
        &[
            ("ned", json!({ "script": missing, "files": ["a.rs"] })),
            ("ned", json!({ "script": unparsed, "files": ["a.rs"] })),
        ],
    );
    assert!(failed(&results[0]) && failed(&results[1]));
    let expected = twin.printed(&["a.rs", "-e", missing]) + "exit 1\n";
    assert_eq!(text(&results[0]), expected);
    let expected = twin.printed(&["a.rs", "-e", unparsed]) + "exit 2\n";
    assert_eq!(text(&results[1]), expected);
}

#[test]
fn every_script_is_recorded_as_the_invocation_it_stands_for() {
    let (ws, twin) = (
        Workspace::new(&[("a.rs", AB)]),
        Workspace::new(&[("a.rs", AB)]),
    );
    let script = r#"replace fn:a.name with "c""#;
    ws.calls(
        &[],
        &[
            ("ned", json!({ "script": script, "files": ["a.rs"] })),
            ("outline", json!({ "files": ["a.rs"] })),
            ("show", json!({ "selector": "fn:b", "workspace": true })),
            ("help", json!({})),
        ],
    );
    for args in [
        &["a.rs", "-e", script][..],
        &["a.rs", "-e", "outline"],
        &["-w", "-e", "show fn:b"],
    ] {
        let args: Vec<&str> = ["-s", "mcp-1"].iter().chain(args).copied().collect();
        twin.ned(&args, "");
    }
    let history = |ws: &Workspace| ws.printed(&["history", "-s", "mcp-1"]);
    assert_eq!(history(&ws), history(&twin));
}

#[test]
fn bang_bang_repeats_the_last_script_on_its_files() {
    let ws = Workspace::new(&[("a.rs", AB)]);
    let results = ws.calls(
        &[],
        &[
            (
                "ned",
                json!({ "script": r#"replace fn:prase.name with "c""#, "files": ["a.rs"] }),
            ),
            ("ned", json!({ "script": "!!:s/prase/a/" })),
        ],
    );
    assert!(failed(&results[0]) && !failed(&results[1]));
    assert!(
        text(&results[1]).contains("note: repeating 1: "),
        "{}",
        text(&results[1])
    );
    assert_eq!(ws.read("a.rs"), "fn c() {}\nfn b() {}\n");
}

#[test]
fn outline_and_show_run_their_verbs() {
    let ws = Workspace::new(&[("a.rs", AB)]);
    let results = ws.calls(
        &[],
        &[
            ("outline", json!({ "files": ["a.rs"] })),
            ("show", json!({ "selector": "fn:b", "files": ["a.rs"] })),
            (
                "show",
                json!({ "selector": "all /fn/ +1", "workspace": true }),
            ),
        ],
    );
    assert_eq!(text(&results[0]), ws.printed(&["a.rs", "-e", "outline"]));
    assert_eq!(text(&results[1]), ws.printed(&["a.rs", "-e", "show fn:b"]));
    assert_eq!(
        text(&results[2]),
        ws.printed(&["-w", "-e", "show all /fn/ +1"])
    );
}

#[test]
fn a_show_selector_holding_more_commands_is_a_usage_error() {
    let ws = Workspace::new(&[("a.rs", AB)]);
    let results = ws.calls(
        &[],
        &[
            (
                "show",
                json!({ "selector": "fn:a; delete fn:b", "files": ["a.rs"] }),
            ),
            (
                "show",
                json!({ "selector": "fn:a | delete fn:b", "files": ["a.rs"] }),
            ),
            (
                "show",
                json!({ "selector": "fn:a\ndelete fn:b", "files": ["a.rs"] }),
            ),
        ],
    );
    for result in &results {
        assert!(failed(result), "{result}");
        assert!(text(result).starts_with("error: "), "{result}");
        assert!(text(result).contains("`ned` tool"), "{result}");
        assert!(text(result).ends_with("\nexit 2\n"), "{result}");
    }
    assert_eq!(ws.read("a.rs"), AB);
}

#[test]
fn help_prints_the_summary_or_a_topic() {
    let ws = Workspace::new(&[]);
    let results = ws.calls(
        &[],
        &[
            ("help", json!({})),
            ("help", json!({ "topic": "show" })),
            ("help", json!({ "topic": "nope" })),
        ],
    );
    assert_eq!(text(&results[0]), ws.printed(&["help"]));
    assert_eq!(text(&results[1]), ws.printed(&["help", "show"]));
    assert!(failed(&results[2]));
    assert!(text(&results[2]).starts_with("error: unknown topic `nope`; topics are show "));
    assert!(text(&results[2]).ends_with("\nexit 2\n"));
}

#[test]
fn conflicting_arguments_are_usage_errors() {
    let ws = Workspace::new(&[("a.rs", AB)]);
    let script = r#"replace fn:a.name with "c""#;
    let results = ws.calls(
        &[],
        &[
            (
                "ned",
                json!({ "script": "outline", "files": ["a.rs"], "workspace": true }),
            ),
            ("outline", json!({ "files": ["a.rs"], "workspace": true })),
            (
                "ned",
                json!({ "script": script, "files": ["a.rs"], "commit": "m", "dry_run": true }),
            ),
            (
                "ned",
                json!({ "script": script, "files": ["a.rs"], "commit": "" }),
            ),
        ],
    );
    for result in &results {
        assert!(failed(result), "{result}");
        assert!(text(result).starts_with("error: "), "{result}");
        assert!(text(result).ends_with("\nexit 2\n"), "{result}");
    }
    assert_eq!(ws.read("a.rs"), AB);
}

#[test]
fn commit_commits_the_calls_edits() {
    let ws = Workspace::repo(&[("a.rs", AB)]);
    let script = r#"replace fn:a.name with "c""#;
    let call = json!({ "script": script, "files": ["a.rs"], "commit": "Rename a" });
    let results = ws.calls(&[], &[("ned", call)]);
    assert!(!failed(&results[0]), "{}", text(&results[0]));
    let last = text(&results[0]).lines().last().unwrap();
    assert!(
        last.starts_with("commit ") && last.ends_with(": Rename a"),
        "{last}"
    );
    assert_eq!(ws.git(&["log", "-1", "--format=%s"]), "Rename a\n");
    assert_eq!(ws.git(&["status", "--porcelain"]), "");
}

#[test]
fn unknown_tools_and_bad_arguments_are_invalid_params() {
    let ws = Workspace::new(&[("a.rs", AB)]);
    let call = |id, name: &str, arguments: Value| {
        request(
            id,
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
    };
    let (responses, _) = ws.mcp(
        &[],
        &[
            call(1, "frobnicate", json!({})),
            call(2, "ned", json!({ "files": ["a.rs"] })),
            call(3, "ned", json!({ "script": "outline", "files": "a.rs" })),
            call(4, "ned", json!({ "script": "outline", "dry_run": "yes" })),
            call(5, "show", json!({ "files": ["a.rs"] })),
            call(6, "help", json!({ "topic": 1 })),
        ],
    );
    assert_eq!(responses.len(), 6);
    for response in &responses {
        assert_eq!(response["error"]["code"], -32602, "{response}");
    }
}

#[test]
fn history_lists_the_sessions_calls() {
    let ws = Workspace::new(&[("a.rs", AB)]);
    let script = r#"replace fn:a.name with "c""#;
    let results = ws.calls(
        &[],
        &[
            ("ned", json!({ "script": script, "files": ["a.rs"] })),
            ("outline", json!({ "files": ["a.rs"] })),
            ("history", json!({})),
            ("history", json!({ "all": true })),
        ],
    );
    assert_eq!(text(&results[2]), ws.printed(&["history", "-s", "mcp-1"]));
    let all = ws.printed(&["history", "-s", "mcp-1", "--all"]);
    assert_eq!(text(&results[3]), all);
    assert!(all.starts_with("1 ok, 1 file: replace"), "{all}");
}

#[test]
fn undo_reverts_the_last_edit_as_ned_undo_does() {
    let (ws, twin) = (
        Workspace::new(&[("a.rs", AB)]),
        Workspace::new(&[("a.rs", AB)]),
    );
    let script = r#"replace fn:a.name with "c""#;
    let results = ws.calls(
        &[],
        &[
            ("ned", json!({ "script": script, "files": ["a.rs"] })),
            ("undo", json!({})),
            ("undo", json!({})),
        ],
    );
    twin.ned(&["-s", "mcp-1", "a.rs", "-e", script], "");
    assert_eq!(text(&results[1]), twin.printed(&["undo", "-s", "mcp-1"]));
    assert_eq!(ws.read("a.rs"), AB);
    assert!(failed(&results[2]));
    assert_eq!(
        text(&results[2]),
        twin.printed(&["undo", "-s", "mcp-1"]) + "exit 1\n"
    );
    let history = ws.printed(&["history", "-s", "mcp-1"]);
    assert!(history.ends_with("2 undo 1, 1 file\n"), "{history}");
}

#[test]
fn undo_after_a_later_change_is_refused_unless_forced() {
    let ws = Workspace::new(&[("a.rs", AB)]);
    let script = r#"replace fn:a.name with "c""#;
    ws.calls(
        &["-s", "agent"],
        &[("ned", json!({ "script": script, "files": ["a.rs"] }))],
    );
    fs::write(ws.dir.path().join("a.rs"), "fn c() {}\nfn d() {}\n").unwrap();
    let results = ws.calls(
        &["-s", "agent"],
        &[("undo", json!({})), ("undo", json!({ "force": true }))],
    );
    assert!(failed(&results[0]), "{}", text(&results[0]));
    assert!(text(&results[0]).contains("a.rs"), "{}", text(&results[0]));
    assert!(text(&results[0]).ends_with("\nexit 1\n"));
    assert!(!failed(&results[1]), "{}", text(&results[1]));
    assert_eq!(ws.read("a.rs"), "fn a() {}\nfn d() {}\n");
}
