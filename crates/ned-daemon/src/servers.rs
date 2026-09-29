//! The workspace's language servers, started as documents need them.

use std::collections::HashMap;
use std::path::PathBuf;

use ned_core::config::Config;

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
        let mut config = Config::new(self.user_config.as_deref())?;
        for document in documents {
            let Some(command) = config.server(&self.root, document.lang)? else {
                continue;
            };
            if self.servers.get(&command).is_none_or(Server::exited) {
                if let Some(exited) = self.servers.remove(&command) {
                    exited.shutdown().await;
                }
                let server = Server::start(&command, document.lang, &self.root).await?;
                self.servers.insert(command.clone(), server);
            }
            let server = self.servers.get_mut(&command).expect("started above");
            server.sync(&document.path, document.lang, &document.text)?;
        }
        Ok(())
    }

    pub fn status(&self) -> Vec<ServerStatus> {
        let mut status: Vec<ServerStatus> = self.servers.values().map(Server::status).collect();
        status.sort_by(|a, b| a.name.cmp(&b.name));
        status
    }

    pub async fn shutdown(&mut self) {
        for (_, server) in self.servers.drain() {
            server.shutdown().await;
        }
    }
}
