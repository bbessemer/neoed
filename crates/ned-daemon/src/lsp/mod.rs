//! A client for one language server process.

mod rpc;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lsp_types::{DiagnosticSeverity, NumberOrString};

use ned_core::lang::Language;
use ned_core::lsp::{
    Diagnostic, FileEdits, Locate, Location, Position, Renamed, Severity, TextEdit, language_id,
};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::io::AsyncBufReadExt;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdout, Command};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{self, Instant};

use crate::protocol::{ServerState, ServerStatus};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
/// How long pushed diagnostics must stay unchanged to count as final.
const SETTLE: Duration = Duration::from_millis(100);
/// Errors that ask for a request to be sent again: ServerCancelled and
/// ContentModified.
const RETRY: [i64; 2] = [-32802, -32801];

/// A running language server, initialized.
pub struct Server {
    name: String,
    child: Child,
    outgoing: mpsc::UnboundedSender<Vec<u8>>,
    shared: Arc<Mutex<Shared>>,
    next_id: i64,
    documents: HashMap<PathBuf, OpenDocument>,
    /// Whether the server answers `textDocument/diagnostic`; otherwise it
    /// publishes diagnostics.
    pulls: bool,
    /// Signalled whenever the server sends a notification or exits.
    changed: Arc<Notify>,
    /// Forwards the server's stderr to the daemon's.
    stderr: Option<JoinHandle<()>>,
}

struct OpenDocument {
    version: i32,
    text: String,
    /// `Shared::generation` when the text was last sent.
    synced: u64,
}

/// A response: its result, or an error's code and message.
type Reply = Result<Value, (i64, String)>;

/// What the server's reader task updates.
#[derive(Default)]
struct Shared {
    /// Error responses are a code and a message.
    pending: HashMap<i64, oneshot::Sender<Reply>>,
    /// Diagnostics published for each URI.
    published: HashMap<String, Published>,
    /// Counts publications.
    generation: u64,
    state: State,
    exited: bool,
    /// The server's last non-blank line on stderr.
    last_error: Option<String>,
}

struct Published {
    version: Option<i64>,
    generation: u64,
    diagnostics: Value,
}

/// What a server has told us about its work.
#[derive(Debug, Default)]
struct State {
    /// Tokens of work-done progress that has begun and not ended.
    progress: HashSet<String>,
    /// rust-analyzer's `experimental/serverStatus`, once sent.
    quiescent: Option<bool>,
}

#[derive(Debug, Error)]
pub enum LspError {
    #[error("`{program}` not found; install it, or set `[lsp] {lang} = [...]` in .ned.toml")]
    NotFound { program: String, lang: Language },
    #[error("cannot start `{program}`: {source}")]
    Spawn {
        program: String,
        source: std::io::Error,
    },
    #[error("{name} exited{detail}; check that it runs, then rerun")]
    Exited { name: String, detail: String },
    #[error(
        "{name} didn't answer {method} within {secs}s; it may still be indexing, so rerun in a few seconds"
    )]
    Timeout {
        name: String,
        method: String,
        secs: u64,
    },
    #[error("{name} failed {method}: {message}")]
    Failed {
        name: String,
        method: String,
        message: String,
        code: i64,
    },
    #[error("{name} sent an invalid {method} reply ({message}); check that it's up to date")]
    Invalid {
        name: String,
        method: String,
        message: String,
    },
}

impl Server {
    /// Starts `command` for the workspace at `root`, serving `lang`, and
    /// initializes it.
    pub async fn start(
        command: &[String],
        lang: Language,
        root: &Path,
    ) -> Result<Server, LspError> {
        let program = &command[0];
        let mut child = Command::new(program)
            .args(&command[1..])
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|source| match source.kind() {
                io::ErrorKind::NotFound => LspError::NotFound {
                    program: program.clone(),
                    lang,
                },
                _ => LspError::Spawn {
                    program: program.clone(),
                    source,
                },
            })?;
        let mut stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        let (outgoing, mut queue) = mpsc::unbounded_channel::<Vec<u8>>();
        tokio::spawn(async move {
            while let Some(bytes) = queue.recv().await {
                if stdin.write_all(&bytes).await.is_err() {
                    break;
                }
            }
        });
        let shared = Arc::new(Mutex::new(Shared::default()));
        let changed = Arc::new(Notify::new());

        tokio::spawn(read_messages(
            BufReader::new(stdout),
            shared.clone(),
            outgoing.clone(),
            changed.clone(),
        ));
        let name = Path::new(program)
            .file_name()
            .map_or(program.clone(), |n| n.to_string_lossy().into_owned());
        let stderr = tokio::spawn(forward_stderr(
            name.clone(),
            BufReader::new(stderr),
            shared.clone(),
        ));
        let mut server = Server {
            name,
            child,
            stderr: Some(stderr),
            outgoing,
            shared,
            next_id: 0,
            documents: HashMap::new(),
            pulls: false,
            changed,
        };
        let root_uri = uri(root);
        let root_name = root.file_name().map(|n| n.to_string_lossy());
        let params = json!({
            "processId": std::process::id(),
            "clientInfo": {"name": "ned", "version": env!("CARGO_PKG_VERSION")},
            "rootPath": root,
            "rootUri": root_uri,
            "workspaceFolders": [{"uri": root_uri, "name": root_name}],
            "capabilities": {
                "window": {"workDoneProgress": true},
                "workspace": {
                    "configuration": true,
                    "workspaceFolders": true,
                    // So servers say when a rename moves files, which ned refuses.
                    "workspaceEdit": {
                        "documentChanges": true,
                        "resourceOperations": ["create", "rename", "delete"],
                    },
                },
                "textDocument": {
                    "synchronization": {"didSave": true},
                    "publishDiagnostics": {"versionSupport": true},
                    "diagnostic": {"dynamicRegistration": false},
                    "definition": {"linkSupport": true},
                    "references": {},

                },
                "experimental": {"serverStatusNotification": true},
            },
        });
        let result = server.request("initialize", params).await?;
        let provider = &result["capabilities"]["diagnosticProvider"];
        server.pulls = !(provider.is_null() || *provider == Value::Bool(false));
        // rust-analyzer's only sign of loading the project is
        // `experimental/serverStatus`; until it's quiescent, it has nothing
        // to report.
        if result["serverInfo"]["name"] == "rust-analyzer" {
            server.shared.lock().unwrap().state.quiescent = Some(false);
        }
        server.notify("initialized", json!({}));
        // pyright analyzes nothing until it has been sent settings.
        server.notify("workspace/didChangeConfiguration", json!({"settings": {}}));
        Ok(server)
    }

    pub fn exited(&self) -> bool {
        self.shared.lock().unwrap().exited
    }

    fn exited_error(&self) -> LspError {
        let last_error = self.shared.lock().unwrap().last_error.clone();
        LspError::Exited {
            name: self.name.clone(),
            detail: last_error.map_or_else(String::new, |line| format!(": {line}")),
        }
    }

    /// Brings the server's copy of `path` up to date with `text`.
    pub fn sync(&mut self, path: &Path, lang: Language, text: &str) -> Result<(), LspError> {
        if self.exited() {
            return Err(self.exited_error());
        }
        let generation = self.shared.lock().unwrap().generation;
        match self.documents.get(path) {
            None => {
                let document = json!({
                    "uri": uri(path),
                    "languageId": language_id(lang),
                    "version": 1,
                    "text": text,
                });
                self.notify("textDocument/didOpen", json!({"textDocument": document}));
                let open = OpenDocument {
                    version: 1,
                    text: text.into(),
                    synced: generation,
                };
                self.documents.insert(path.to_path_buf(), open);
            }
            Some(open) if open.text == text => {}
            Some(_) => self.change(path, text.into()),
        }
        Ok(())
    }

    /// Sends `text` as the new text of the open document `path`.
    fn change(&mut self, path: &Path, text: String) {
        let generation = self.shared.lock().unwrap().generation;
        let open = self.documents.get_mut(path).expect("open");
        open.version += 1;
        open.synced = generation;
        let params = json!({
            "textDocument": {"uri": uri(path), "version": open.version},
            "contentChanges": [{"text": text}],
        });
        open.text = text;
        self.notify("textDocument/didChange", params);
    }

    /// Brings the open documents other than `except` up to date with their
    /// files, which may have changed since they were sent.
    pub fn refresh(&mut self, except: &Path) {
        let stale: Vec<(PathBuf, String)> = self
            .documents
            .iter()
            .filter(|(path, _)| path.as_path() != except)
            .filter_map(|(path, open)| {
                let text = std::fs::read_to_string(path).ok()?;
                (text != open.text).then(|| (path.clone(), text))
            })
            .collect();
        for (path, text) in stale {
            self.change(&path, text);
        }
    }

    /// The server's diagnostics for `path`, once synced, waiting up to
    /// `timeout` for it to finish indexing and report them.
    pub async fn diagnostics(
        &mut self,
        path: &Path,
        timeout: Duration,
    ) -> Result<Vec<Diagnostic>, LspError> {
        let deadline = Instant::now() + timeout;
        let name = self.name.clone();
        let timed_out = move || LspError::Timeout {
            name: name.clone(),
            method: "diagnostics".into(),
            secs: timeout.as_secs(),
        };
        self.wait_until(deadline, &timed_out, |shared| !shared.state.busy())
            .await?;
        let uri = uri(path);
        if self.pulls {
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                let params = json!({"textDocument": {"uri": uri}});
                match self
                    .request_within(remaining, "textDocument/diagnostic", params)
                    .await
                {
                    Ok(report) => return Ok(convert(&report["items"])),
                    Err(LspError::Failed { code, .. }) if RETRY.contains(&code) => {
                        if Instant::now() >= deadline {
                            return Err(timed_out());
                        }
                        time::sleep(SETTLE).await;
                    }
                    Err(LspError::Timeout { .. }) => return Err(timed_out()),
                    Err(err) => return Err(err),
                }
            }
        }
        let (version, synced) = self
            .documents
            .get(path)
            .map_or((0, 0), |open| (open.version, open.synced));
        // Settled: published for the synced text, idle, and unchanged for
        // `SETTLE`.
        let settled = |shared: &Shared| {
            !shared.state.busy()
                && shared.published.get(&uri).is_some_and(|p| match p.version {
                    Some(published) => published >= i64::from(version),
                    None => p.generation > synced,
                })
        };
        loop {
            self.wait_until(deadline, &timed_out, settled).await?;
            let seen = self.shared.lock().unwrap().generation;
            time::sleep(SETTLE).await;
            let shared = self.shared.lock().unwrap();
            if shared.generation == seen && settled(&shared) {
                break;
            }
        }
        let shared = self.shared.lock().unwrap();
        Ok(convert(&shared.published[&uri].diagnostics))
    }

    /// The edits that rename the symbol at `position` in `path` to `name`,
    /// waiting up to `timeout` for the server to finish indexing.
    pub async fn rename(
        &mut self,
        path: &Path,
        position: Position,
        name: &str,
        timeout: Duration,
    ) -> Result<Renamed, LspError> {
        let deadline = Instant::now() + timeout;
        let server = self.name.clone();
        let timed_out = move || LspError::Timeout {
            name: server.clone(),
            method: "rename".into(),
            secs: timeout.as_secs(),
        };
        self.wait_until(deadline, &timed_out, |shared| !shared.state.busy())
            .await?;
        let params = json!({
            "textDocument": {"uri": uri(path)},
            "position": {"line": position.line, "character": position.character},
            "newName": name,
        });
        let remaining = deadline.saturating_duration_since(Instant::now());
        match self
            .request_within(remaining, "textDocument/rename", params)
            .await
        {
            Ok(edit) => renamed(&self.name, &edit).map_err(|message| LspError::Invalid {
                name: self.name.clone(),
                method: "textDocument/rename".into(),
                message,
            }),
            Err(LspError::Failed { message, .. }) => Ok(Renamed::Refused(format!(
                "{} can't rename there: {message}; select the name itself",
                self.name
            ))),
            Err(LspError::Timeout { .. }) => Err(timed_out()),
            Err(err) => Err(err),
        }
    }

    /// The references to (without the declaration), or the definition of, the
    /// symbol at `position` in `path`, within `timeout`; the server must be
    /// ready.
    pub async fn locate(
        &mut self,
        kind: Locate,
        path: &Path,
        position: Position,
        timeout: Duration,
    ) -> Result<Vec<Location>, LspError> {
        let (method, mut params) = match kind {
            Locate::References => (
                "textDocument/references",
                json!({"context": {"includeDeclaration": false}}),
            ),
            Locate::Definition => ("textDocument/definition", json!({})),
        };
        params["textDocument"] = json!({"uri": uri(path)});
        params["position"] = json!({"line": position.line, "character": position.character});
        let reply = self.request_within(timeout, method, params).await?;
        locations(&reply).map_err(|message| LspError::Invalid {
            name: self.name.clone(),
            method: method.into(),
            message,
        })
    }

    /// Waits until `done` holds, the server exits, or `deadline` passes.
    async fn wait_until(
        &self,
        deadline: Instant,
        timed_out: &dyn Fn() -> LspError,
        done: impl Fn(&Shared) -> bool,
    ) -> Result<(), LspError> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            // Registered before checking, so no signal is missed.
            changed.as_mut().enable();
            let exited = {
                let shared = self.shared.lock().unwrap();
                if done(&shared) {
                    return Ok(());
                }
                shared.exited
            };
            if exited {
                return Err(self.exited_error());
            }
            if time::timeout_at(deadline, changed).await.is_err() {
                return Err(timed_out());
            }
        }
    }

    pub async fn request(&mut self, method: &str, params: Value) -> Result<Value, LspError> {
        self.request_within(REQUEST_TIMEOUT, method, params).await
    }

    async fn request_within(
        &mut self,
        limit: Duration,
        method: &str,
        params: Value,
    ) -> Result<Value, LspError> {
        self.next_id += 1;
        let id = self.next_id;
        let (sender, receiver) = oneshot::channel();
        if self.exited() {
            return Err(self.exited_error());
        }
        self.shared.lock().unwrap().pending.insert(id, sender);
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        match time::timeout(limit, receiver).await {
            Ok(Ok(Ok(result))) => Ok(result),
            Ok(Ok(Err((code, message)))) => Err(LspError::Failed {
                name: self.name.clone(),
                method: method.into(),
                message,
                code,
            }),
            Ok(Err(_)) => {
                // Its last words explain the exit, so wait for them.
                if let Some(stderr) = self.stderr.take() {
                    let _ = time::timeout(SHUTDOWN_TIMEOUT, stderr).await;
                }
                Err(self.exited_error())
            }
            Err(_) => {
                self.shared.lock().unwrap().pending.remove(&id);
                Err(LspError::Timeout {
                    name: self.name.clone(),
                    method: method.into(),
                    secs: limit.as_secs(),
                })
            }
        }
    }

    fn notify(&self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    fn send(&self, message: Value) {
        // A server that has gone is noticed by its reader.
        let _ = self.outgoing.send(rpc::frame(&message));
    }

    pub fn status(&self) -> ServerStatus {
        let shared = self.shared.lock().unwrap();
        let (state, documents) = if shared.exited {
            (ServerState::Exited, 0)
        } else if shared.state.busy() {
            (ServerState::Indexing, self.documents.len())
        } else {
            (ServerState::Ready, self.documents.len())
        };
        ServerStatus {
            name: self.name.clone(),
            state,
            documents,
        }
    }

    /// Asks the server to shut down and exit, and kills it if it doesn't.
    pub async fn shutdown(mut self) {
        if !self.exited() {
            let shutdown = self.request_within(SHUTDOWN_TIMEOUT, "shutdown", Value::Null);
            if shutdown.await.is_ok() {
                self.notify("exit", Value::Null);
            }
        }
        if time::timeout(SHUTDOWN_TIMEOUT, self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
        }
    }
}

/// Handles what the server sends until its output ends.
async fn read_messages(
    mut stdout: BufReader<ChildStdout>,
    shared: Arc<Mutex<Shared>>,
    outgoing: mpsc::UnboundedSender<Vec<u8>>,
    changed: Arc<Notify>,
) {
    loop {
        let message = match rpc::read(&mut stdout).await {
            Ok(Some(message)) => message,
            Ok(None) => break,
            Err(err) => {
                eprintln!("ned daemon: a server sent a bad message: {err}");
                break;
            }
        };
        let method = message.get("method").and_then(Value::as_str);
        match (method, message.get("id")) {
            (Some(method), Some(id)) => {
                let result = answer(method, &message["params"]);
                let reply = json!({"jsonrpc": "2.0", "id": id, "result": result});
                let _ = outgoing.send(rpc::frame(&reply));
            }
            (Some(method), None) => {
                let params = &message["params"];
                let mut shared = shared.lock().unwrap();
                if method == "textDocument/publishDiagnostics"
                    && let Some(uri) = params["uri"].as_str()
                {
                    shared.generation += 1;
                    let published = Published {
                        version: params["version"].as_i64(),
                        generation: shared.generation,
                        diagnostics: params["diagnostics"].clone(),
                    };
                    shared.published.insert(uri.into(), published);
                }
                shared.state.notify(method, params);
                changed.notify_waiters();
            }
            (None, Some(id)) => {
                let sender = id
                    .as_i64()
                    .and_then(|id| shared.lock().unwrap().pending.remove(&id));
                let result = match message.get("error") {
                    Some(error) => {
                        let code = error["code"].as_i64().unwrap_or_default();
                        let message = error["message"].as_str().unwrap_or("unknown error");
                        Err((code, message.to_string()))
                    }
                    None => Ok(message.get("result").cloned().unwrap_or_default()),
                };
                if let Some(sender) = sender {
                    let _ = sender.send(result);
                }
            }
            (None, None) => {}
        }
    }
    let mut shared = shared.lock().unwrap();
    shared.exited = true;
    shared.pending.clear();
    changed.notify_waiters();
}

/// Copies the server's stderr lines to the daemon's log, prefixed with
/// `name`, keeping the last one for errors.
async fn forward_stderr(name: String, stderr: BufReader<ChildStderr>, shared: Arc<Mutex<Shared>>) {
    let mut lines = stderr.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        eprintln!("{name}: {line}");
        if !line.trim().is_empty() {
            shared.lock().unwrap().last_error = Some(line);
        }
    }
}

/// LSP diagnostics as `ned`'s; a missing severity counts as an error.
fn convert(diagnostics: &Value) -> Vec<Diagnostic> {
    let diagnostics: Vec<lsp_types::Diagnostic> =
        serde_json::from_value(diagnostics.clone()).unwrap_or_default();
    let position = |p: lsp_types::Position| Position {
        line: p.line,
        character: p.character,
    };
    diagnostics
        .into_iter()
        .map(|d| Diagnostic {
            start: position(d.range.start),
            end: position(d.range.end),
            severity: match d.severity {
                Some(DiagnosticSeverity::WARNING) => Severity::Warning,
                Some(DiagnosticSeverity::INFORMATION) => Severity::Info,
                Some(DiagnosticSeverity::HINT) => Severity::Hint,
                _ => Severity::Error,
            },
            message: d.message,
            source: d.source,
            code: d.code.map(|code| match code {
                NumberOrString::Number(n) => n.to_string(),
                NumberOrString::String(s) => s,
            }),
        })
        .collect()
}

/// A rename's `WorkspaceEdit` as edits by file, in path order; `Err` says
/// what's wrong with it.
fn renamed(server: &str, edit: &Value) -> Result<Renamed, String> {
    if edit.is_null() {
        return Ok(Renamed::Refused(format!(
            "{server} found nothing to rename there; select the name itself"
        )));
    }
    let mut files: BTreeMap<PathBuf, Vec<TextEdit>> = BTreeMap::new();
    let mut add = |uri: &str, edits: &Value| -> Result<(), String> {
        let path = file_path(uri)?;

        let edits: Vec<lsp_types::TextEdit> =
            serde_json::from_value(edits.clone()).map_err(|err| err.to_string())?;

        files
            .entry(path)
            .or_default()
            .extend(edits.into_iter().map(|e| TextEdit {
                start: position(e.range.start),
                end: position(e.range.end),
                text: e.new_text,
            }));
        Ok(())
    };
    if let Some(changes) = edit["documentChanges"].as_array() {
        for change in changes {
            if change.get("kind").is_some() {
                return Ok(Renamed::Refused(format!(
                    "{server} would create, rename or delete files, which ned can't do; rename or move the file yourself, then rerun"
                )));
            }
            let uri = change["textDocument"]["uri"].as_str().unwrap_or_default();
            add(uri, &change["edits"])?;
        }
    } else if let Some(changes) = edit["changes"].as_object() {
        for (uri, edits) in changes {
            add(uri, edits)?;
        }
    }
    let files = files
        .into_iter()
        .map(|(path, edits)| FileEdits { path, edits })
        .collect();
    Ok(Renamed::Edits(files))
}

/// The locations in a `definition` or `references` reply, in path order;
/// `Err` says what's wrong with it.
fn locations(reply: &Value) -> Result<Vec<Location>, String> {
    let items: Vec<&Value> = match reply {
        Value::Null => Vec::new(),
        Value::Array(items) => items.iter().collect(),
        item => vec![item],
    };
    let mut locations = items
        .into_iter()
        .map(|item| {
            // A `LocationLink`'s selection range is the name.
            let (uri, range) = match item.get("targetUri") {
                Some(uri) => (uri, &item["targetSelectionRange"]),
                None => (&item["uri"], &item["range"]),
            };
            let range: lsp_types::Range =
                serde_json::from_value(range.clone()).map_err(|err| err.to_string())?;
            Ok(Location {
                path: file_path(uri.as_str().unwrap_or_default())?,
                start: position(range.start),
                end: position(range.end),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    locations.sort_by(|a, b| (&a.path, a.start).cmp(&(&b.path, b.start)));
    Ok(locations)
}

fn file_path(uri: &str) -> Result<PathBuf, String> {
    url::Url::parse(uri)
        .ok()
        .and_then(|url| url.to_file_path().ok())
        .ok_or_else(|| format!("{uri} is not a file"))
}

fn position(p: lsp_types::Position) -> Position {
    Position {
        line: p.line,
        character: p.character,
    }
}

/// The result for a request from the server: no settings for
/// `workspace/configuration`, and null, as acknowledgement, for the rest.
fn answer(method: &str, params: &Value) -> Value {
    match method {
        "workspace/configuration" => {
            let items = params["items"].as_array().map_or(0, Vec::len);
            Value::Array(vec![Value::Null; items])
        }
        _ => Value::Null,
    }
}

fn uri(path: &Path) -> String {
    url::Url::from_file_path(path).map_or_else(|()| path.display().to_string(), String::from)
}

impl State {
    fn notify(&mut self, method: &str, params: &Value) {
        match method {
            "$/progress" => {
                let token = params["token"].to_string();
                match params["value"]["kind"].as_str() {
                    Some("begin") => {
                        self.progress.insert(token);
                    }
                    Some("end") => {
                        self.progress.remove(&token);
                    }
                    _ => {}
                }
            }
            "experimental/serverStatus" => self.quiescent = params["quiescent"].as_bool(),
            _ => {}
        }
    }

    fn busy(&self) -> bool {
        !self.progress.is_empty() || self.quiescent == Some(false)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn progress(token: Value, kind: &str) -> Value {
        json!({"token": token, "value": {"kind": kind, "title": "Indexing"}})
    }

    #[test]
    fn work_done_progress_makes_a_server_busy() {
        let mut state = State::default();
        assert!(!state.busy());
        state.notify("$/progress", &progress(json!("a"), "begin"));
        assert!(state.busy());
        state.notify("$/progress", &progress(json!(7), "begin"));
        state.notify("$/progress", &progress(json!("a"), "report"));
        state.notify("$/progress", &progress(json!("a"), "end"));
        assert!(state.busy());
        state.notify("$/progress", &progress(json!(7), "end"));
        assert!(!state.busy());
    }

    #[test]
    fn rust_analyzer_is_busy_until_quiescent() {
        let mut state = State::default();
        state.notify(
            "experimental/serverStatus",
            &json!({"health": "ok", "quiescent": false}),
        );
        assert!(state.busy());
        state.notify(
            "experimental/serverStatus",
            &json!({"health": "ok", "quiescent": true}),
        );
        assert!(!state.busy());
    }

    #[test]
    fn other_notifications_are_ignored() {
        let mut state = State::default();
        state.notify("window/logMessage", &json!({"type": 3, "message": "hi"}));
        state.notify("$/progress", &json!({"token": "a"}));
        assert!(!state.busy());
    }
}
