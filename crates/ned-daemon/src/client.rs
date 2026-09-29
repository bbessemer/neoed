//! A blocking client for the daemon, so `ned` never starts an async runtime.

use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::paths::Paths;
use crate::protocol::{Request, Response};

/// A connection point to a running daemon.
#[derive(Debug, Clone)]
pub struct Client {
    paths: Paths,
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("cannot reach the daemon: {0}; run `ned daemon stop`, then retry")]
    Io(#[from] io::Error),
    #[error("cannot start the daemon: {source}")]
    Spawn { source: io::Error },
    #[error("the daemon didn't start; see its log, {}", log.display())]
    NotStarted { log: PathBuf },
    #[error("the daemon sent an invalid response: {0}; run `ned daemon stop`, then retry")]
    Protocol(String),
}

impl Client {
    /// The running daemon at `paths`, if there is one.
    pub fn connect(paths: &Paths) -> Option<Client> {
        let _ = paths;
        todo!()
    }

    /// The running daemon at `paths`, spawning `exe daemon run ROOT` first if
    /// there is none.
    pub fn connect_or_spawn(paths: &Paths, exe: &Path, root: &Path) -> Result<Client, ClientError> {
        let _ = (paths, exe, root);
        todo!()
    }

    pub fn request(&self, request: &Request) -> Result<Response, ClientError> {
        let _ = (&self.paths, request);
        todo!()
    }
}
