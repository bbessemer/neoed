//! A client for one language server process.

mod rpc;

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ned_core::lang::Language;
use ned_core::lsp::language_id;
use serde_json::{Value, json};
use thiserror::Error;
use tokio::io::AsyncBufReadExt;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time;

use crate::protocol::{ServerState, ServerStatus};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// A running language server, initialized.
pub struct Server {
    name: String,
    child: Child,
    outgoing: mpsc::UnboundedSender<Vec<u8>>,
    shared: Arc<Mutex<Shared>>,
    next_id: i64,
    /// Open documents: their version and text.
    documents: HashMap<PathBuf, (i32, String)>,
    /// Forwards the server's stderr to the daemon's.
    stderr: Option<JoinHandle<()>>,
}

/// What the server's reader task updates.
#[derive(Default)]
struct Shared {
    pending: HashMap<i64, oneshot::Sender<Result<Value, String>>>,
    state: State,
    exited: bool,
    /// The server's last non-blank line on stderr.
    last_error: Option<String>,
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
        tokio::spawn(read_messages(
            BufReader::new(stdout),
            shared.clone(),
            outgoing.clone(),
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
                "workspace": {"configuration": true, "workspaceFolders": true},
                "textDocument": {
                    "synchronization": {"didSave": true},
                    "publishDiagnostics": {"versionSupport": true},
                },
                "experimental": {"serverStatusNotification": true},
            },
        });
        server.request("initialize", params).await?;
        server.notify("initialized", json!({}));
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
        match self.documents.get_mut(path) {
            None => {
                let document = json!({
                    "uri": uri(path),
                    "languageId": language_id(lang),
                    "version": 1,
                    "text": text,
                });
                self.notify("textDocument/didOpen", json!({"textDocument": document}));
                self.documents.insert(path.to_path_buf(), (1, text.into()));
            }
            Some((_, old)) if old == text => {}
            Some((version, old)) => {
                *version += 1;
                *old = text.into();
                let params = json!({
                    "textDocument": {"uri": uri(path), "version": *version},
                    "contentChanges": [{"text": text}],
                });
                self.notify("textDocument/didChange", params);
            }
        }
        Ok(())
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
            Ok(Ok(Err(message))) => Err(LspError::Failed {
                name: self.name.clone(),
                method: method.into(),
                message,
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
            (Some(method), None) => shared
                .lock()
                .unwrap()
                .state
                .notify(method, &message["params"]),
            (None, Some(id)) => {
                let sender = id
                    .as_i64()
                    .and_then(|id| shared.lock().unwrap().pending.remove(&id));
                let result = match message.get("error") {
                    Some(error) => Err(error["message"].as_str().unwrap_or("unknown error").into()),
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
