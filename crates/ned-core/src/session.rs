//! Sessions: per-workspace logs of `ned` invocations (spec §1.2).

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The log format version, in the header's `ned_session` field.
pub const FORMAT: u64 = 1;

/// One recorded invocation: a script, or an undo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Assigned by [`Log::append`], numbering entries from 1.
    pub id: u64,
    /// Seconds since the Unix epoch.
    pub time: u64,
    pub cwd: PathBuf,
    pub files: Vec<String>,
    pub workspace: Option<PathBuf>,
    /// `None` for an undo.
    pub script: Option<String>,
    pub undoes: Option<u64>,
    pub dry_run: bool,
    pub exit: u8,
    pub error: Option<String>,
    pub changes: Vec<FileChange>,
}

/// A file an entry wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    /// Absolute.
    pub path: PathBuf,
    /// `None` if the entry created the file.
    pub before: Option<String>,
    /// `None` if the entry removed the file.
    pub after: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Header {
    ned_session: u64,
    workspace: PathBuf,
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error(
        "{}: {source}; check its permissions, or set XDG_STATE_HOME to another directory",
        path.display()
    )]
    Io { path: PathBuf, source: io::Error },
    #[error("{0}; remove it, or set XDG_STATE_HOME to a private directory")]
    Dir(#[from] crate::fs::PrivateDirError),
    #[error("no directory for sessions; set XDG_STATE_HOME or HOME")]
    NoStateDir,
    #[error(
        "invalid session name `{0}`; use letters, digits, `.`, `_` and `-`, not starting with `.`"
    )]
    BadName(String),
    #[error(
        "{}: session log format {version} is unknown to this ned; use another session name",
        path.display()
    )]
    UnknownFormat { path: PathBuf, version: u64 },
    #[error(
        "{}:{line}: malformed session log ({message}); use another session name",
        path.display()
    )]
    Malformed {
        path: PathBuf,
        line: usize,
        message: String,
    },
}

/// The directory sessions live in: `$XDG_STATE_HOME/ned`, else
/// `~/.local/state/ned`. Not created until a session is opened.
pub fn state_dir() -> Result<PathBuf, SessionError> {
    todo!()
}

/// The names of the sessions with a log for the workspace at `root`, sorted.
pub fn sessions(state_dir: &Path, root: &Path) -> Result<Vec<String>, SessionError> {
    todo!()
}

/// One session of one workspace.
#[derive(Debug)]
pub struct Session {
    name: String,
    root: PathBuf,
    log: PathBuf,
    lock: PathBuf,
}

impl Session {
    /// Session `name` of the workspace at `root` (canonical), creating its
    /// directories under `state_dir` private to the user.
    pub fn new(state_dir: &Path, root: &Path, name: &str) -> Result<Session, SessionError> {
        todo!()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether anything has been recorded in it.
    pub fn exists(&self) -> bool {
        self.log.exists()
    }

    /// Waits for the session's lock, which the [`Log`] holds until dropped.
    pub fn lock(&self) -> Result<Log<'_>, SessionError> {
        todo!()
    }
}

/// A locked session's log.
#[derive(Debug)]
pub struct Log<'a> {
    session: &'a Session,
    _lock: File,
}

impl Log<'_> {
    /// Every entry, oldest first; none if nothing has been recorded.
    pub fn entries(&self) -> Result<Vec<Entry>, SessionError> {
        todo!()
    }

    /// Appends `entry` with the next id, which it returns.
    pub fn append(&mut self, entry: Entry) -> Result<u64, SessionError> {
        todo!()
    }
}
