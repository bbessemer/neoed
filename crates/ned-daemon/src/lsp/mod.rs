//! A client for one language server process.

mod rpc;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ned_core::lang::Language;
use serde_json::Value;
use thiserror::Error;
use tokio::process::Child;
use tokio::sync::{mpsc, oneshot};

use crate::protocol::ServerStatus;

/// A running language server, initialized.
pub struct Server {
    name: String,
    child: Child,
    outgoing: mpsc::UnboundedSender<Vec<u8>>,
    shared: Arc<Mutex<Shared>>,
    next_id: i64,
    /// Open documents: their version and text.
    documents: HashMap<PathBuf, (i32, String)>,
}

/// What the server's reader task updates.
#[derive(Default)]
struct Shared {
    pending: HashMap<i64, oneshot::Sender<Result<Value, String>>>,
    state: State,
    exited: bool,
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
    #[error("{name} exited; rerun to restart it")]
    Exited { name: String },
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
        let _ = (command, lang, root);
        todo!()
    }

    /// Brings the server's copy of `path` up to date with `text`.
    pub fn sync(&mut self, path: &Path, lang: Language, text: &str) -> Result<(), LspError> {
        let _ = (path, lang, text);
        todo!()
    }

    pub async fn request(&mut self, method: &str, params: Value) -> Result<Value, LspError> {
        let _ = (method, params);
        todo!()
    }

    pub fn status(&self) -> ServerStatus {
        todo!()
    }

    /// Asks the server to shut down and exit, and kills it if it doesn't.
    pub async fn shutdown(mut self) {
        todo!()
    }
}

impl State {
    fn notify(&mut self, method: &str, params: &Value) {
        let _ = (method, params);
        todo!()
    }

    fn busy(&self) -> bool {
        todo!()
    }
}
