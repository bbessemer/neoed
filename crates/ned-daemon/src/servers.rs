//! The workspace's language servers, started as documents need them.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::lsp::{LspError, Server};
use crate::protocol::{Document, ServerStatus};

pub struct Servers {
    root: PathBuf,
    /// The user config file, reread with the workspace's for each request.
    user_config: Option<PathBuf>,
    /// By command, so languages with the same server share it.
    servers: HashMap<Vec<String>, Server>,
}

#[derive(Debug, thiserror::Error)]
pub enum ServersError {
    #[error("{0}")]
    Config(#[from] ned_core::config::ConfigError),
    #[error("{0}")]
    Lsp(#[from] LspError),
}

impl Servers {
    pub fn new(root: PathBuf, user_config: Option<PathBuf>) -> Servers {
        Servers {
            root,
            user_config,
            servers: HashMap::new(),
        }
    }

    /// Starts the servers `documents` need, restarting any that exited, and
    /// syncs the documents; skips documents whose language has no server.
    pub async fn open(&mut self, documents: &[Document]) -> Result<(), ServersError> {
        let _ = documents;
        todo!()
    }

    pub fn status(&self) -> Vec<ServerStatus> {
        todo!()
    }

    pub async fn shutdown(&mut self) {
        todo!()
    }
}
