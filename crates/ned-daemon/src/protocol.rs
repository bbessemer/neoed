//! Messages between `ned` and its daemon: one JSON request per connection,
//! answered by one JSON response, each on a line of its own.

use std::path::PathBuf;

use ned_core::lsp::Diagnosis;
pub use ned_core::lsp::Document;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Request {
    Status,
    Stop,
    /// Start the documents' servers and bring them up to date with the texts.
    Open {
        documents: Vec<Document>,
    },
    /// `Open`, then the documents' diagnostics.
    Diagnose {
        documents: Vec<Document>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Response {
    Status(Status),
    Stopped,
    Opened,
    Diagnosis(Diagnosis),
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub version: String,
    pub root: PathBuf,
    pub pid: u32,
    pub uptime_secs: u64,
    pub servers: Vec<ServerStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerStatus {
    /// The basename of the server's program.
    pub name: String,
    pub state: ServerState,
    /// The number of documents open in the server.
    pub documents: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerState {
    /// Reporting work in progress, such as indexing.
    Indexing,
    Ready,
    Exited,
}
