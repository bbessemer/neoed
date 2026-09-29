//! The workspace's language servers, started as documents need them.

use std::collections::HashMap;
use std::path::PathBuf;

use ned_core::config::Config;
use ned_core::text::indent_unit;
use std::time::Duration;

use ned_core::lsp::{Diagnosis, Formatting, Locate, Located, Position, Renamed, Severity};

use crate::lsp::{LspError, Server};
use crate::protocol::{Document, ServerStatus};

/// `[lsp] timeout`, in seconds, unless configured.
const DEFAULT_TIMEOUT: u64 = 30;

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

    /// `open`, then each document's diagnostics, with the workspace's
    /// `[check] show`.
    pub async fn diagnose(&mut self, documents: &[Document]) -> Result<Diagnosis, ServersError> {
        self.open(documents).await?;
        let mut config = Config::new(self.user_config.as_deref())?;
        let show = config.check_show(&self.root)?.unwrap_or(Severity::Warning);
        let timeout = config.lsp_timeout(&self.root)?.unwrap_or(DEFAULT_TIMEOUT);
        let mut files = Vec::new();
        for document in documents {
            let Some(command) = config.server(&self.root, document.lang)? else {
                files.push(None);
                continue;
            };
            let server = self.servers.get_mut(&command).expect("opened above");
            let diagnostics = server
                .diagnostics(&document.path, Duration::from_secs(timeout))
                .await?;
            files.push(Some(diagnostics));
        }
        Ok(Diagnosis {
            show,
            block: config
                .check_block(&self.root)?
                .unwrap_or(Some(Severity::Error)),
            files,
        })
    }

    /// Renames the symbol at `position` in the document to `name`.
    pub async fn rename(
        &mut self,
        document: &Document,
        position: Position,
        name: &str,
    ) -> Result<Renamed, ServersError> {
        let Some((server, timeout)) = self.prepare(document).await? else {
            return Ok(Renamed::NoServer);
        };
        Ok(server
            .rename(&document.path, position, name, timeout)
            .await?)
    }

    /// `open`s the document and readies its server to answer about it: brings
    /// the server's other documents up to date with the disk, and waits until
    /// it has diagnosed this one. `None` if the language has no server.
    async fn prepare(
        &mut self,
        document: &Document,
    ) -> Result<Option<(&mut Server, Duration)>, ServersError> {
        self.open(std::slice::from_ref(document)).await?;
        let mut config = Config::new(self.user_config.as_deref())?;
        let Some(command) = config.server(&self.root, document.lang)? else {
            return Ok(None);
        };
        let timeout = config.lsp_timeout(&self.root)?.unwrap_or(DEFAULT_TIMEOUT);
        let timeout = Duration::from_secs(timeout);
        let server = self.servers.get_mut(&command).expect("opened above");
        server.refresh(&document.path);
        // Servers such as pyright give no sign of loading the workspace, but
        // diagnose a document only once they have.
        server.diagnostics(&document.path, timeout).await?;
        Ok(Some((server, timeout)))
    }

    /// The references to, or definition of, the symbol at `position` in the
    /// document.
    pub async fn locate(
        &mut self,
        kind: Locate,
        document: &Document,
        position: Position,
    ) -> Result<Located, ServersError> {
        let Some((server, timeout)) = self.prepare(document).await? else {
            return Ok(Located::NoServer);
        };
        let locations = server
            .locate(kind, &document.path, position, timeout)
            .await?;
        Ok(Located::Locations(locations))
    }

    /// The edits that format the document.
    pub async fn format(&mut self, document: &Document) -> Result<Formatting, ServersError> {
        let Some((server, timeout)) = self.prepare(document).await? else {
            return Ok(Formatting::NoServer);
        };
        if !server.formats() {
            return Ok(Formatting::NoServer);
        }
        let indent = indent_unit(&document.text, document.lang.default_indent());
        let edits = server.format(&document.path, &indent, timeout).await?;
        Ok(Formatting::Edits {
            server: server.name().into(),
            edits,
        })
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
