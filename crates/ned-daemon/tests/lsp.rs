//! The workspace's language servers, against a fake server (spec §1.1).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ned_core::lang::Language;
use ned_core::lsp::{
    Diagnostic, FileEdits, Formatting, Locate, Located, Location, Position, Renamed, Severity,
    TextEdit,
};
use ned_daemon::protocol::{Document, ServerState, ServerStatus};
use ned_daemon::servers::Servers;
use serde_json::{Value, json};
use tempfile::TempDir;

const FAKE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake_lsp.py");

/// A workspace whose Rust server is the fake, logging to `log`.
struct Workspace {
    dir: TempDir,
    log: PathBuf,
}

impl Workspace {
    fn new() -> Workspace {
        Workspace::with_config("")
    }

    /// With `extra` added to the `[lsp]` table.
    fn with_config(extra: &str) -> Workspace {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("lsp.log");
        let config = format!("[lsp]\nrust = [{FAKE:?}, {log:?}]\n{extra}");
        fs::write(dir.path().join(".ned.toml"), config).unwrap();
        Workspace { dir, log }
    }

    /// With the fake given `flags`, and `extra` added to the `[lsp]` table.
    fn with_flags(flags: &[&str], extra: &str) -> Workspace {
        let ws = Workspace::with_config(extra);
        let mut argv = vec![FAKE.to_string(), ws.log.display().to_string()];
        argv.extend(flags.iter().map(|f| f.to_string()));
        let config = format!("[lsp]\nrust = {argv:?}\n{extra}");
        fs::write(ws.dir.path().join(".ned.toml"), config).unwrap();
        ws
    }

    fn root(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap()
    }

    fn servers(&self) -> Servers {
        Servers::new(self.root(), None)
    }

    fn doc(&self, name: &str, lang: Language, text: &str) -> Document {
        Document {
            path: self.root().join(name),
            lang,
            text: text.into(),
        }
    }

    fn rust(&self, name: &str, text: &str) -> Document {
        self.doc(name, Language::Rust, text)
    }

    fn messages(&self) -> Vec<Value> {
        let log = fs::read_to_string(&self.log).unwrap_or_default();
        log.lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// The log, once `done` holds for it.
    async fn wait(&self, done: impl Fn(&[Value]) -> bool) -> Vec<Value> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let messages = self.messages();
            if done(&messages) {
                return messages;
            }
            assert!(Instant::now() < deadline, "timed out; log: {messages:#?}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

fn with_method<'a>(messages: &'a [Value], method: &str) -> Vec<&'a Value> {
    messages.iter().filter(|m| m["method"] == method).collect()
}

fn has(method: &'static str, count: usize) -> impl Fn(&[Value]) -> bool {
    move |messages| with_method(messages, method).len() >= count
}

fn uri(path: &Path) -> String {
    url::Url::from_file_path(path).unwrap().to_string()
}

async fn wait_for_state(servers: &Servers, state: ServerState) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while servers.status().first().map(|s| s.state) != Some(state) {
        assert!(Instant::now() < deadline, "{:?}", servers.status());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn opening_starts_and_initializes_a_server() {
    let ws = Workspace::new();
    let mut servers = ws.servers();
    servers
        .open(&[ws.rust("a.rs", "fn a() {}\n")])
        .await
        .unwrap();
    let messages = ws.wait(has("textDocument/didOpen", 1)).await;
    let methods: Vec<_> = messages
        .iter()
        .filter_map(|m| m["method"].as_str())
        .collect();
    assert_eq!(
        methods,
        [
            "initialize",
            "initialized",
            "workspace/didChangeConfiguration",
            "textDocument/didOpen"
        ]
    );

    let init = &messages[0]["params"];
    assert_eq!(init["processId"], std::process::id());
    assert_eq!(init["rootUri"], uri(&ws.root()));
    assert_eq!(init["workspaceFolders"][0]["uri"], uri(&ws.root()));
    assert_eq!(init["capabilities"]["window"]["workDoneProgress"], true);
    assert_eq!(init["capabilities"]["workspace"]["configuration"], true);
    let text = &init["capabilities"]["textDocument"];
    assert_eq!(text["formatting"]["dynamicRegistration"], false);

    let open = &messages[3]["params"]["textDocument"];
    assert_eq!(open["uri"], uri(&ws.root().join("a.rs")));
    assert_eq!(open["languageId"], "rust");
    assert_eq!(open["version"], 1);
    assert_eq!(open["text"], "fn a() {}\n");

    let status = servers.status();
    assert_eq!(status.len(), 1);
    assert_eq!(status[0].name, "fake_lsp.py");
    assert_eq!(status[0].documents, 1);
    servers.shutdown().await;
}

#[tokio::test]
async fn only_changed_documents_are_sent_again() {
    let ws = Workspace::new();
    let mut servers = ws.servers();
    servers.open(&[ws.rust("a.rs", "one\n")]).await.unwrap();
    servers.open(&[ws.rust("a.rs", "one\n")]).await.unwrap();
    servers
        .open(&[ws.rust("a.rs", "two\n"), ws.rust("b.rs", "b\n")])
        .await
        .unwrap();
    let messages = ws.wait(has("textDocument/didOpen", 2)).await;
    let changes = with_method(&messages, "textDocument/didChange");
    assert_eq!(changes.len(), 1, "{messages:#?}");
    let change = &changes[0]["params"];
    assert_eq!(change["textDocument"]["version"], 2);
    assert_eq!(change["contentChanges"], json!([{"text": "two\n"}]));
    assert_eq!(servers.status()[0].documents, 2);
    servers.shutdown().await;
}

#[tokio::test]
async fn server_requests_are_answered() {
    let ws = Workspace::new();
    let mut servers = ws.servers();
    servers.open(&[ws.rust("a.rs", "")]).await.unwrap();
    let answered = |id: &'static str| move |m: &Value| m["id"] == id && m.get("result").is_some();
    let messages = ws
        .wait(|ms| ms.iter().any(answered("s1")) && ms.iter().any(answered("s2")))
        .await;
    let answer = |id| messages.iter().find(|m| answered(id)(m)).unwrap()["result"].clone();
    assert_eq!(answer("s1"), json!([null, null]));
    assert_eq!(answer("s2"), Value::Null);
    servers.shutdown().await;
}

#[tokio::test]
async fn a_server_indexes_then_is_ready() {
    let ws = Workspace::new();
    let mut servers = ws.servers();
    servers.open(&[ws.rust("a.rs", "")]).await.unwrap();
    wait_for_state(&servers, ServerState::Indexing).await;
    servers.open(&[ws.rust("a.rs", "done")]).await.unwrap();
    wait_for_state(&servers, ServerState::Ready).await;
    servers.shutdown().await;
}

#[tokio::test]
async fn an_exited_server_is_restarted() {
    let ws = Workspace::new();
    let mut servers = ws.servers();
    servers.open(&[ws.rust("a.rs", "crash")]).await.unwrap();
    wait_for_state(&servers, ServerState::Exited).await;
    servers.open(&[ws.rust("b.rs", "b")]).await.unwrap();
    ws.wait(has("initialize", 2)).await;
    let status = servers.status();
    assert_eq!(status.len(), 1);
    assert_ne!(status[0].state, ServerState::Exited);
    assert_eq!(status[0].documents, 1);
    servers.shutdown().await;
}

#[tokio::test]
async fn shutdown_stops_every_server() {
    let ws = Workspace::new();
    let mut servers = ws.servers();
    servers.open(&[ws.rust("a.rs", "")]).await.unwrap();
    servers.shutdown().await;
    let messages = ws.messages();
    let methods: Vec<_> = messages
        .iter()
        .filter_map(|m| m["method"].as_str())
        .collect();
    assert_eq!(methods[methods.len() - 2..], ["shutdown", "exit"]);
    assert_eq!(servers.status(), Vec::<ServerStatus>::new());
}

#[tokio::test]
async fn languages_without_a_server_are_skipped() {
    let ws = Workspace::with_config("python = false\n");
    let mut servers = ws.servers();
    servers
        .open(&[
            ws.doc("a.py", Language::Python, ""),
            ws.doc("a.md", Language::Markdown, ""),
        ])
        .await
        .unwrap();
    assert_eq!(servers.status(), Vec::<ServerStatus>::new());
    assert!(!ws.log.exists());
}

#[tokio::test]
async fn languages_with_the_same_server_share_it() {
    let ws = Workspace::new();
    let config = fs::read_to_string(ws.dir.path().join(".ned.toml")).unwrap();
    let go = config.lines().nth(1).unwrap().replace("rust =", "go =");
    fs::write(ws.dir.path().join(".ned.toml"), format!("{config}{go}\n")).unwrap();
    let mut servers = ws.servers();
    servers
        .open(&[ws.rust("a.rs", ""), ws.doc("a.go", Language::Go, "")])
        .await
        .unwrap();
    let messages = ws.wait(has("textDocument/didOpen", 2)).await;
    assert_eq!(with_method(&messages, "initialize").len(), 1);
    let languages: Vec<_> = with_method(&messages, "textDocument/didOpen")
        .iter()
        .map(|m| m["params"]["textDocument"]["languageId"].clone())
        .collect();
    assert_eq!(languages, ["rust", "go"]);
    assert_eq!(servers.status()[0].documents, 2);
    servers.shutdown().await;
}

#[tokio::test]
async fn a_missing_server_names_the_setting() {
    let ws = Workspace::with_config("go = [\"no-such-server-for-ned\"]\n");
    let mut servers = ws.servers();
    let err = servers
        .open(&[ws.doc("a.go", Language::Go, "")])
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("`no-such-server-for-ned` not found"), "{err}");
    assert!(err.contains("[lsp] go ="), "{err}");
}

#[tokio::test]
async fn a_server_that_fails_to_start_says_why() {
    let ws = Workspace::new();
    let config = format!("[lsp]\nrust = [{FAKE:?}, {:?}, \"fail\"]\n", ws.log);
    fs::write(ws.dir.path().join(".ned.toml"), config).unwrap();
    let err = ws.servers().open(&[ws.rust("a.rs", "")]).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "fake_lsp.py exited: fake: cannot start: broken on purpose; check that it runs, then rerun"
    );
}

#[tokio::test]
async fn config_errors_are_reported() {
    let ws = Workspace::new();
    fs::write(ws.dir.path().join(".ned.toml"), "[lsp]\nrust = 1\n").unwrap();
    let err = ws.servers().open(&[ws.rust("a.rs", "")]).await.unwrap_err();
    assert!(err.to_string().contains("invalid config"), "{err}");
}

fn fake(line: u32, character: u32, word: &str, severity: Severity) -> Diagnostic {
    Diagnostic {
        start: Position { line, character },
        end: Position {
            line,
            character: character + word.len() as u32,
        },
        severity,
        message: format!("{} here", word.to_lowercase()),
        source: Some("fake".into()),
        code: (word == "ERROR").then(|| "F1".into()),
    }
}

#[tokio::test]
async fn diagnose_reports_published_diagnostics() {
    let ws = Workspace::new();
    let mut servers = ws.servers();
    let diagnosis = servers
        .diagnose(&[ws.rust("a.rs", "done\nlet x; // ERROR\n  WARN\n")], false)
        .await
        .unwrap();
    assert_eq!(diagnosis.show, Severity::Warning);
    assert_eq!(
        diagnosis.files,
        [Some(vec![
            fake(1, 10, "ERROR", Severity::Error),
            fake(2, 2, "WARN", Severity::Warning),
        ])]
    );
    servers.shutdown().await;
}

#[tokio::test]
async fn diagnostics_follow_the_text() {
    let ws = Workspace::new();
    let mut servers = ws.servers();
    servers
        .diagnose(&[ws.rust("a.rs", "done ERROR")], false)
        .await
        .unwrap();
    let diagnosis = servers
        .diagnose(&[ws.rust("a.rs", "done HINT")], false)
        .await
        .unwrap();
    assert_eq!(
        diagnosis.files,
        [Some(vec![fake(0, 5, "HINT", Severity::Hint)])]
    );
    let again = servers
        .diagnose(&[ws.rust("a.rs", "done HINT")], false)
        .await
        .unwrap();
    assert_eq!(again, diagnosis);
    servers.shutdown().await;
}

#[tokio::test]
async fn diagnose_pulls_from_servers_that_offer_it() {
    let ws = Workspace::new();
    let config = format!(
        "[lsp]\nrust = [{FAKE:?}, {:?}, \"pull\", \"cancel-once\"]\n",
        ws.log
    );
    fs::write(ws.dir.path().join(".ned.toml"), config).unwrap();
    let mut servers = ws.servers();
    let diagnosis = servers
        .diagnose(&[ws.rust("a.rs", "done ERROR")], false)
        .await
        .unwrap();
    assert_eq!(
        diagnosis.files,
        [Some(vec![fake(0, 5, "ERROR", Severity::Error)])]
    );
    let pulls = with_method(&ws.messages(), "textDocument/diagnostic").len();
    assert_eq!(pulls, 2, "one cancelled, one answered");
    servers.shutdown().await;
}

#[tokio::test]
async fn diagnose_waits_for_indexing_up_to_the_timeout() {
    let ws = Workspace::with_config("timeout = 1\n");
    let mut servers = ws.servers();
    let started = Instant::now();
    let err = servers
        .diagnose(&[ws.rust("a.rs", "still indexing ERROR")], false)
        .await
        .unwrap_err()
        .to_string();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(err.contains("fake_lsp.py"), "{err}");
    assert!(err.contains("rerun"), "{err}");
    servers.shutdown().await;
}

#[tokio::test]
async fn rust_analyzer_is_busy_until_it_says_it_is_quiescent() {
    let ws = Workspace::new();
    let config = format!(
        "[lsp]\nrust = [{FAKE:?}, {:?}, \"ra\", \"pull\"]\ntimeout = 1\n",
        ws.log
    );
    fs::write(ws.dir.path().join(".ned.toml"), config).unwrap();
    let mut servers = ws.servers();
    let err = servers
        .diagnose(&[ws.rust("a.rs", "ERROR")], false)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("rerun"), "{err}");
    let diagnosis = servers
        .diagnose(&[ws.rust("a.rs", "done ERROR")], false)
        .await
        .unwrap();
    assert_eq!(
        diagnosis.files,
        [Some(vec![fake(0, 5, "ERROR", Severity::Error)])]
    );
    servers.shutdown().await;
}

#[tokio::test]
async fn diagnose_returns_the_configured_level_and_skips_serverless_files() {
    let ws = Workspace::with_config("\n[check]\nshow = \"hint\"\n");
    let mut servers = ws.servers();
    let diagnosis = servers
        .diagnose(
            &[
                ws.doc("a.md", Language::Markdown, "# ERROR"),
                ws.rust("a.rs", "done"),
            ],
            false,
        )
        .await
        .unwrap();
    assert_eq!(diagnosis.show, Severity::Hint);
    assert_eq!(diagnosis.files, [None, Some(vec![])]);
    servers.shutdown().await;
}

/// The default servers report errors: `cargo test -- --ignored`.
#[tokio::test]
#[ignore]
async fn real_servers_report_errors() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let files = [
        (
            "Cargo.toml",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        ),
        ("src/lib.rs", "pub fn a( {\n"),
        ("go.mod", "module a\n\ngo 1.21\n"),
        ("a.go", "package a\n\nfunc A() int { return \"x\" }\n"),
        ("a.py", "def a():\n    return undefined_name\n"),
        ("a.ts", "export const a: number = \"x\";\n"),
    ];
    let mut documents = Vec::new();
    for (name, text) in files {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        if let Some(lang) = Language::detect(name, text) {
            documents.push(Document {
                path,
                lang,
                text: text.into(),
            });
        }
    }
    let mut servers = Servers::new(root, None);
    let diagnosis = servers.diagnose(&documents, false).await.unwrap();
    for (document, diagnostics) in documents.iter().zip(&diagnosis.files) {
        let diagnostics = diagnostics.as_ref().expect("a server");
        assert!(
            diagnostics.iter().any(|d| d.severity == Severity::Error),
            "{}: {diagnostics:?}",
            document.path.display()
        );
    }
    servers.shutdown().await;
}

#[tokio::test]
async fn diagnose_returns_the_configured_block_level() {
    let ws = Workspace::with_config("\n[check]\nblock = false\n");
    let mut servers = ws.servers();
    let diagnosis = servers
        .diagnose(&[ws.rust("a.rs", "done")], false)
        .await
        .unwrap();
    assert_eq!(diagnosis.block, None);
    servers.shutdown().await;
    let ws = Workspace::new();
    let mut servers = ws.servers();
    let diagnosis = servers
        .diagnose(&[ws.rust("a.rs", "done")], false)
        .await
        .unwrap();
    assert_eq!(diagnosis.block, Some(Severity::Error));
    servers.shutdown().await;
}

/// The fake's error for a CARGO line.
fn cargo(line: u32, end: u32) -> Diagnostic {
    Diagnostic {
        start: Position { line, character: 0 },
        end: Position {
            line,
            character: end,
        },
        severity: Severity::Error,
        message: "cargo here".into(),
        source: Some("cargo".into()),
        code: None,
    }
}

/// `ws`'s a.rs, written to disk too.
fn saved(ws: &Workspace, text: &str) -> Document {
    fs::write(ws.root().join("a.rs"), text).unwrap();
    ws.rust("a.rs", text)
}

#[tokio::test]
async fn saved_diagnoses_wait_for_save_time_checks() {
    let ws = Workspace::with_flags(&["flycheck"], "");
    let mut servers = ws.servers();
    let a = saved(&ws, "// done\nfn a() {} // CARGO\n// WARN\n");
    let mut diagnosis = servers.diagnose(&[a], true).await.unwrap();
    diagnosis.files[0]
        .as_mut()
        .unwrap()
        .sort_by_key(|d| d.start);
    assert_eq!(
        diagnosis.files,
        [Some(vec![
            cargo(1, 18),
            fake(2, 3, "WARN", Severity::Warning)
        ])]
    );
    assert!(diagnosis.notes.is_empty(), "{:?}", diagnosis.notes);
    servers.shutdown().await;
    let messages = ws.messages();
    let saves = with_method(&messages, "textDocument/didSave");
    assert_eq!(saves.len(), 1);
    let uri = uri(&ws.root().join("a.rs"));
    assert_eq!(saves[0]["params"], json!({"textDocument": {"uri": uri}}));
}

#[tokio::test]
async fn only_saved_diagnoses_of_files_holding_their_text_save() {
    let ws = Workspace::with_flags(&["flycheck"], "");
    let mut servers = ws.servers();
    saved(&ws, "// done\n");
    let a = ws.rust("a.rs", "// done\n// CARGO\n");
    let unsaved = servers
        .diagnose(std::slice::from_ref(&a), false)
        .await
        .unwrap();
    assert_eq!(unsaved.files, [Some(vec![])]);
    let changed = servers.diagnose(&[a], true).await.unwrap();
    assert_eq!(changed.files, [Some(vec![])]);
    servers.shutdown().await;
    assert!(with_method(&ws.messages(), "textDocument/didSave").is_empty());
}

#[tokio::test]
async fn servers_that_ask_for_no_saves_get_none() {
    let ws = Workspace::new();
    let mut servers = ws.servers();
    let a = saved(&ws, "// done\n// WARN\n");
    let diagnosis = servers.diagnose(&[a], true).await.unwrap();
    assert_eq!(
        diagnosis.files,
        [Some(vec![fake(1, 3, "WARN", Severity::Warning)])]
    );
    servers.shutdown().await;
    assert!(with_method(&ws.messages(), "textDocument/didSave").is_empty());
}

#[tokio::test]
async fn an_unfinished_save_time_check_is_a_note() {
    let ws = Workspace::with_flags(&["flycheck", "flycheck-slow"], "timeout = 1\n");
    let mut servers = ws.servers();
    let a = saved(&ws, "// done\n// CARGO\n");
    let started = Instant::now();
    let diagnosis = servers.diagnose(&[a], true).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(diagnosis.files, [Some(vec![cargo(1, 8)])]);
    assert_eq!(
        diagnosis.notes,
        ["fake_lsp.py's check on save didn't finish within 1s; raise [lsp] timeout"]
    );
    servers.shutdown().await;
}

#[tokio::test]
async fn servers_pulled_from_add_what_they_publish_on_save() {
    for flags in [
        &["pull", "flycheck"][..],
        &["pull", "flycheck", "flycheck-slow"],
    ] {
        let ws = Workspace::with_flags(flags, "timeout = 1\n");
        let mut servers = ws.servers();
        let a = saved(&ws, "// done\n// CARGO\n// WARN\n");
        let mut diagnosis = servers.diagnose(&[a], true).await.unwrap();
        diagnosis.files[0]
            .as_mut()
            .unwrap()
            .sort_by_key(|d| d.start);
        assert_eq!(
            diagnosis.files,
            [Some(vec![
                cargo(1, 8),
                fake(2, 3, "WARN", Severity::Warning)
            ])],
            "{flags:?}"
        );
        assert_eq!(diagnosis.notes.len(), flags.len() - 2, "{flags:?}");
        servers.shutdown().await;
    }
}

/// rust-analyzer reports `cargo check`'s errors for saved files: `cargo test
/// -- --ignored`.
#[tokio::test]
#[ignore]
async fn real_rust_analyzer_reports_cargo_check_errors() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::create_dir(root.join("src")).unwrap();
    let text = "pub fn a() -> String {\n    let s = String::new();\n    drop(s);\n    s\n}\n";
    fs::write(root.join("src/lib.rs"), text).unwrap();
    let document = Document {
        path: root.join("src/lib.rs"),
        lang: Language::Rust,
        text: text.into(),
    };
    let mut servers = Servers::new(root.clone(), None);
    let diagnosis = servers.diagnose(&[document], true).await.unwrap();
    let found = diagnosis.files[0].clone().unwrap();
    assert!(
        found
            .iter()
            .any(|d| d.source.as_deref() == Some("rustc") && d.code.as_deref() == Some("E0382")),
        "{found:#?}"
    );
    servers.shutdown().await;
}

/// The default servers, which must be installed: `cargo test -- --ignored`.
#[tokio::test]
#[ignore]
async fn real_servers_start_index_and_shut_down() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let files = [
        (
            "Cargo.toml",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        ),
        ("src/lib.rs", "pub fn a() -> u8 {\n    1\n}\n"),
        ("go.mod", "module a\n\ngo 1.21\n"),
        ("a.go", "package a\n\nfunc A() int { return 1 }\n"),
        ("a.py", "def a():\n    return 1\n"),
        ("a.ts", "export const a = (): number => 1;\n"),
    ];
    let mut documents = Vec::new();
    for (name, text) in files {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        if let Some(lang) = Language::detect(name, text) {
            documents.push(Document {
                path,
                lang,
                text: text.into(),
            });
        }
    }
    let mut servers = Servers::new(root, None);
    servers.open(&documents).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    while servers
        .status()
        .iter()
        .any(|s| s.state != ServerState::Ready)
    {
        assert!(Instant::now() < deadline, "{:?}", servers.status());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let names: Vec<_> = servers.status().into_iter().map(|s| s.name).collect();
    assert_eq!(
        names,
        [
            "gopls",
            "pyright-langserver",
            "rust-analyzer",
            "typescript-language-server"
        ]
    );
    servers.shutdown().await;
}

fn edit(line: u32, start: u32, end: u32, text: &str) -> TextEdit {
    TextEdit {
        start: Position {
            line,
            character: start,
        },
        end: Position {
            line,
            character: end,
        },
        text: text.into(),
    }
}

/// The fake renaming `foo` in `a.rs`, open, and `b.rs`, on disk.
async fn rename_foo(ws: &Workspace) -> Renamed {
    fs::write(ws.root().join("b.rs"), "fn g() { foo(); }\n").unwrap();
    fs::write(ws.root().join("c.py"), "foo\n").unwrap();
    let mut servers = ws.servers();
    let a = ws.rust("a.rs", "// done\nfn foo() {}\n");
    let renamed = servers
        .rename(
            &a,
            Position {
                line: 1,
                character: 4,
            },
            "bar",
        )
        .await
        .unwrap();
    servers.shutdown().await;
    renamed
}

fn foo_renamed(ws: &Workspace) -> Renamed {
    Renamed::Edits(vec![
        FileEdits {
            path: ws.root().join("a.rs"),
            edits: vec![edit(1, 3, 6, "bar")],
        },
        FileEdits {
            path: ws.root().join("b.rs"),
            edits: vec![edit(0, 9, 12, "bar")],
        },
    ])
}

#[tokio::test]
async fn rename_edits_open_documents_and_files_on_disk() {
    let ws = Workspace::new();
    assert_eq!(rename_foo(&ws).await, foo_renamed(&ws));
    let ws = Workspace::new();
    let config = format!(
        "[lsp]\nrust = [{FAKE:?}, {:?}, \"document-changes\"]\n",
        ws.log
    );
    fs::write(ws.dir.path().join(".ned.toml"), config).unwrap();
    assert_eq!(rename_foo(&ws).await, foo_renamed(&ws));
}

#[tokio::test]
async fn rename_refusals_say_why() {
    for (flag, why) in [
        ("rename-file", "would create, rename or delete files"),
        ("rename-error", "cannot rename a keyword"),
    ] {
        let ws = Workspace::new();
        let config = format!("[lsp]\nrust = [{FAKE:?}, {:?}, {flag:?}]\n", ws.log);
        fs::write(ws.dir.path().join(".ned.toml"), config).unwrap();
        match rename_foo(&ws).await {
            Renamed::Refused { why: refused, .. } => {
                assert!(refused.contains(why), "{refused}")
            }
            other => panic!("{flag}: {other:?}"),
        }
    }
    let ws = Workspace::new();
    let mut servers = ws.servers();
    let a = ws.rust("a.rs", "// done\n\n");
    let renamed = servers
        .rename(
            &a,
            Position {
                line: 1,
                character: 0,
            },
            "bar",
        )
        .await
        .unwrap();
    match renamed {
        Renamed::Refused { why, .. } => assert!(why.contains("nothing to rename"), "{why}"),
        other => panic!("{other:?}"),
    }
    let md = ws.doc("a.md", Language::Markdown, "# A\n");
    let renamed = servers
        .rename(
            &md,
            Position {
                line: 0,
                character: 2,
            },
            "B",
        )
        .await;
    assert_eq!(renamed.unwrap(), Renamed::NoServer);
    servers.shutdown().await;
}

#[tokio::test]
async fn rename_first_brings_open_documents_up_to_date_with_the_disk() {
    let ws = Workspace::new();
    let mut servers = ws.servers();
    let b = ws.rust("b.rs", "fn g() { foo(); }\n");
    servers.open(std::slice::from_ref(&b)).await.unwrap();
    fs::write(&b.path, "fn g() {}\nfn h() { foo(); }\n").unwrap();
    let a = ws.rust("a.rs", "// done\nfn foo() {}\n");
    let renamed = servers
        .rename(
            &a,
            Position {
                line: 1,
                character: 4,
            },
            "bar",
        )
        .await
        .unwrap();
    let Renamed::Edits(files) = renamed else {
        panic!("{renamed:?}");
    };
    assert_eq!(files[1].path, b.path);
    assert_eq!(files[1].edits, [edit(1, 9, 12, "bar")]);
    servers.shutdown().await;
}

fn location(path: PathBuf, line: u32, start: u32, end: u32) -> Location {
    let e = edit(line, start, end, "");
    Location {
        path,
        start: e.start,
        end: e.end,
    }
}

/// The fake locating `foo` in `a.rs`, open, and `b.rs`, on disk.
async fn locate_foo(ws: &Workspace, kind: Locate) -> Located {
    fs::write(ws.root().join("b.rs"), "fn g() { foo(); }\n").unwrap();
    let mut servers = ws.servers();
    let a = ws.rust("a.rs", "// done\nfn foo() {}\n");
    let position = Position {
        line: 1,
        character: 4,
    };
    let located = servers.locate(kind, &a, position).await.unwrap();
    servers.shutdown().await;
    located
}

#[tokio::test]
async fn locate_finds_references_and_definitions() {
    let ws = Workspace::new();
    assert_eq!(
        locate_foo(&ws, Locate::References).await,
        Located::Locations(vec![location(ws.root().join("b.rs"), 0, 9, 12)])
    );
    let ws = Workspace::new();
    let def = Located::Locations(vec![location(ws.root().join("a.rs"), 1, 3, 6)]);
    assert_eq!(locate_foo(&ws, Locate::Definition).await, def);
    let links = Workspace::new();
    let config = format!("[lsp]\nrust = [{FAKE:?}, {:?}, \"links\"]\n", links.log);
    fs::write(links.dir.path().join(".ned.toml"), config).unwrap();
    let def = Located::Locations(vec![location(links.root().join("a.rs"), 1, 3, 6)]);
    assert_eq!(locate_foo(&links, Locate::Definition).await, def);
}

#[tokio::test]
async fn locate_without_a_server_says_so() {
    let ws = Workspace::new();
    let mut servers = ws.servers();
    let md = ws.doc("a.md", Language::Markdown, "# A\n");
    let position = Position {
        line: 0,
        character: 2,
    };
    let located = servers.locate(Locate::References, &md, position).await;
    assert_eq!(located.unwrap(), Located::NoServer);
}

#[tokio::test]
async fn format_returns_the_server_edits_for_the_text() {
    let ws = Workspace::new();
    let config = format!(
        "[lsp]\nrust = [{FAKE:?}, {:?}, \"format\"]\ngo = [{FAKE:?}, {:?}, \"format\"]\n",
        ws.log, ws.log
    );
    fs::write(ws.dir.path().join(".ned.toml"), config).unwrap();
    let mut servers = ws.servers();
    let a = ws.rust("a.rs", "// done\nfn a() {}  \nx \n");
    let formatting = servers.format(&a).await.unwrap();
    assert_eq!(
        formatting,
        Formatting::Edits {
            server: "fake_lsp.py".into(),
            edits: vec![edit(1, 9, 11, ""), edit(2, 1, 2, "")],
        }
    );
    let go = ws.doc("a.go", Language::Go, "// done\nfunc a() {\n\tx()\n}\n");
    servers.format(&go).await.unwrap();
    servers.shutdown().await;

    let messages = ws.messages();
    let requests = with_method(&messages, "textDocument/formatting");
    assert_eq!(requests.len(), 2);
    let rust = &requests[0]["params"];
    assert_eq!(rust["textDocument"]["uri"], uri(&ws.root().join("a.rs")));
    assert_eq!(rust["options"], json!({"tabSize": 4, "insertSpaces": true}));
    let go = &requests[1]["params"];
    assert_eq!(go["options"], json!({"tabSize": 4, "insertSpaces": false}));
}

#[tokio::test]
async fn format_without_a_formatting_server_says_so() {
    let ws = Workspace::new();
    let mut servers = ws.servers();
    let a = ws.rust("a.rs", "// done\nfn a() {}  \n");
    assert_eq!(servers.format(&a).await.unwrap(), Formatting::NoServer);
    let md = ws.doc("a.md", Language::Markdown, "# A  \n");
    assert_eq!(servers.format(&md).await.unwrap(), Formatting::NoServer);
    servers.shutdown().await;
    assert!(with_method(&ws.messages(), "textDocument/formatting").is_empty());
}

/// rust-analyzer and gopls format unsaved text: `cargo test -- --ignored`.
#[tokio::test]
#[ignore]
async fn real_servers_format() {
    let cases: [FormatCase; 2] = [
        (
            &[(
                "Cargo.toml",
                "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
            )],
            "src/lib.rs",
            "pub fn a( )->u8{1}\n",
            "pub fn a() -> u8 {\n    1\n}\n",
        ),
        (
            &[("go.mod", "module a\n\ngo 1.21\n")],
            "a.go",
            "package a\nfunc A( ) int {return 1}\n",
            "package a\n\nfunc A() int { return 1 }\n",
        ),
    ];
    for (files, name, text, formatted) in cases {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for (file, text) in files {
            fs::write(root.join(file), text).unwrap();
        }
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        let document = Document {
            path,
            lang: Language::detect(name, text).unwrap(),
            text: text.into(),
        };
        let mut servers = Servers::new(root.clone(), None);
        let formatting = servers.format(&document).await.unwrap();
        let Formatting::Edits { edits, .. } = formatting else {
            panic!("{name}: {formatting:?}");
        };
        let buffer = ned_core::buffer::Buffer::new(text);
        let mut result = text.to_string();
        for edit in edits.iter().rev() {
            let start = buffer.lsp_offset(edit.start.line, edit.start.character);
            let end = buffer.lsp_offset(edit.end.line, edit.end.character);
            result.replace_range(start..end, &edit.text);
        }
        assert_eq!(result, formatted, "{name}: {edits:?}");
        servers.shutdown().await;
    }
}

/// Project files, the file to format, its text, and its formatted text.
type FormatCase<'a> = (&'a [(&'a str, &'a str)], &'a str, &'a str, &'a str);

/// The default servers find references and definitions across files: `cargo
/// test -- --ignored`.
#[tokio::test]
#[ignore]
async fn real_servers_locate_across_files() {
    let cases: [RenameCase; 3] = [
        (
            &[
                (
                    "Cargo.toml",
                    "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
                ),
                ("src/lib.rs", "pub mod b;\npub fn helper() {}\n"),
                ("src/b.rs", "pub fn g() {\n    crate::helper();\n}\n"),
            ],
            "src/b.rs",
            Position {
                line: 1,
                character: 11,
            },
        ),
        (
            &[
                ("go.mod", "module a\n\ngo 1.21\n"),
                ("a.go", "package a\n\nfunc Helper() {}\n"),
                ("b.go", "package a\n\nfunc G() { Helper() }\n"),
            ],
            "b.go",
            Position {
                line: 2,
                character: 11,
            },
        ),
        (
            &[
                ("a.py", "def helper():\n    pass\n"),
                ("b.py", "from a import helper\n\nhelper()\n"),
            ],
            "b.py",
            Position {
                line: 2,
                character: 0,
            },
        ),
    ];
    for (files, name, position) in cases {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for (file, text) in files {
            let path = root.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, text).unwrap();
        }
        let text = fs::read_to_string(root.join(name)).unwrap();
        let document = Document {
            path: root.join(name),
            lang: Language::detect(name, &text).unwrap(),
            text,
        };
        let mut servers = Servers::new(root.clone(), None);
        let refs = servers
            .locate(Locate::References, &document, position)
            .await;
        let def = servers
            .locate(Locate::Definition, &document, position)
            .await;
        match (refs.unwrap(), def.unwrap()) {
            (Located::Locations(refs), Located::Locations(def)) => {
                assert!(!refs.is_empty(), "{name}: no references");
                assert_eq!(def.len(), 1, "{name}: {def:?}");
                assert_ne!(def[0].path, root.join(name), "{name}: {def:?}");
            }
            other => panic!("{name}: {other:?}"),
        }
        servers.shutdown().await;
    }
}

/// Files, the one to rename in, and the position of the name.
type RenameCase<'a> = (&'a [(&'a str, &'a str)], &'a str, Position);

/// The default servers rename across files: `cargo test -- --ignored`.
#[tokio::test]
#[ignore]
async fn real_servers_rename_across_files() {
    let cases: [RenameCase; 3] = [
        (
            &[
                (
                    "Cargo.toml",
                    "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
                ),
                ("src/lib.rs", "pub mod b;\npub fn helper() {}\n"),
                ("src/b.rs", "pub fn g() {\n    crate::helper();\n}\n"),
            ],
            "src/lib.rs",
            Position {
                line: 1,
                character: 7,
            },
        ),
        (
            &[
                ("go.mod", "module a\n\ngo 1.21\n"),
                ("a.go", "package a\n\nfunc Helper() {}\n"),
                ("b.go", "package a\n\nfunc G() { Helper() }\n"),
            ],
            "a.go",
            Position {
                line: 2,
                character: 5,
            },
        ),
        (
            &[
                ("a.py", "def helper():\n    pass\n"),
                ("b.py", "from a import helper\n\nhelper()\n"),
            ],
            "a.py",
            Position {
                line: 0,
                character: 4,
            },
        ),
    ];
    for (files, name, position) in cases {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for (file, text) in files {
            let path = root.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, text).unwrap();
        }
        let text = fs::read_to_string(root.join(name)).unwrap();
        let document = Document {
            path: root.join(name),
            lang: Language::detect(name, &text).unwrap(),
            text,
        };
        let mut servers = Servers::new(root, None);
        let renamed = servers.rename(&document, position, "assist").await.unwrap();
        match renamed {
            Renamed::Edits(files) => assert_eq!(files.len(), 2, "{name}: {files:?}"),
            other => panic!("{name}: {other:?}"),
        }
        servers.shutdown().await;
    }
}
