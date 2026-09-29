//! The workspace's language servers, against a fake server (spec §1.1).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ned_core::lang::Language;
use ned_core::lsp::{Diagnostic, Position, Severity};
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
        ["initialize", "initialized", "textDocument/didOpen"]
    );

    let init = &messages[0]["params"];
    assert_eq!(init["processId"], std::process::id());
    assert_eq!(init["rootUri"], uri(&ws.root()));
    assert_eq!(init["workspaceFolders"][0]["uri"], uri(&ws.root()));
    assert_eq!(init["capabilities"]["window"]["workDoneProgress"], true);
    assert_eq!(init["capabilities"]["workspace"]["configuration"], true);

    let open = &messages[2]["params"]["textDocument"];
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
        .diagnose(&[ws.rust("a.rs", "done\nlet x; // ERROR\n  WARN\n")])
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
        .diagnose(&[ws.rust("a.rs", "done ERROR")])
        .await
        .unwrap();
    let diagnosis = servers
        .diagnose(&[ws.rust("a.rs", "done HINT")])
        .await
        .unwrap();
    assert_eq!(
        diagnosis.files,
        [Some(vec![fake(0, 5, "HINT", Severity::Hint)])]
    );
    let again = servers
        .diagnose(&[ws.rust("a.rs", "done HINT")])
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
        .diagnose(&[ws.rust("a.rs", "done ERROR")])
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
    let ws = Workspace::with_config("\n[check]\ntimeout = 1\n");
    let mut servers = ws.servers();
    let started = Instant::now();
    let err = servers
        .diagnose(&[ws.rust("a.rs", "still indexing ERROR")])
        .await
        .unwrap_err()
        .to_string();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(err.contains("fake_lsp.py"), "{err}");
    assert!(err.contains("rerun"), "{err}");
    servers.shutdown().await;
}

#[tokio::test]
async fn diagnose_returns_the_configured_level_and_skips_serverless_files() {
    let ws = Workspace::with_config("\n[check]\nshow = \"hint\"\n");
    let mut servers = ws.servers();
    let diagnosis = servers
        .diagnose(&[
            ws.doc("a.md", Language::Markdown, "# ERROR"),
            ws.rust("a.rs", "done"),
        ])
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
    let diagnosis = servers.diagnose(&documents).await.unwrap();
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
