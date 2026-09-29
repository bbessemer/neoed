//! Where a workspace's daemon listens, locks and logs.

use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// The files of one workspace's daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub socket: PathBuf,
    /// Held by the running daemon, so only one serves a workspace.
    pub lock: PathBuf,
    pub log: PathBuf,
}

#[derive(Debug, Error)]
pub enum PathsError {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error(
        "{} is {why}; remove it, or set XDG_RUNTIME_DIR to a private directory",
        dir.display()
    )]
    UnsafeDir { dir: PathBuf, why: &'static str },
}

impl Paths {
    /// The paths of the daemon for `root` (canonical) in `runtime_dir`. The
    /// names depend on the `ned` version too, so a reinstalled `ned` never
    /// talks to an older daemon.
    pub fn new(runtime_dir: &Path, root: &Path) -> Paths {
        let _ = (runtime_dir, root);
        todo!()
    }
}

/// The directory for daemon files, `$XDG_RUNTIME_DIR/ned` or `$TMPDIR/ned-UID`:
/// created private if missing, and rejected if others own or can access it.
pub fn runtime_dir() -> Result<PathBuf, PathsError> {
    todo!()
}

fn private_dir(dir: &Path) -> Result<(), PathsError> {
    let _ = dir;
    todo!()
}

/// The workspace containing `dir`: the nearest directory from `dir` up that
/// holds `.git`, `.hg` or `.jj`, or else `dir`; canonical.
pub fn workspace_root(dir: &Path) -> io::Result<PathBuf> {
    let _ = dir;
    todo!()
}
