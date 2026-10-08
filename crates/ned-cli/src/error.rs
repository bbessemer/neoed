//! The errors and notes of the CLI's own startup and subcommands (spec §7).

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use ned_core::hint::{self, Fix, Hint, Note};

/// Arguments that can't run, found before the script does.
#[derive(Debug)]
pub enum UsageError {
    /// A subcommand given after a flag.
    Subcommand(String),
    /// A command's name given as a FILE.
    Command(String),
    /// `-e -` given more than once.
    StdinTwice,
    /// A flag only scripts take, given to the REPL.
    ScriptOnly(&'static str),
    Stdin(io::Error),
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UsageError::Subcommand(name) => write!(f, "`{name}` is a subcommand, not a file"),
            UsageError::Command(name) => write!(f, "`{name}` is a command, not a file"),
            UsageError::StdinTwice => f.write_str("stdin holds one script"),
            UsageError::ScriptOnly(flag) => write!(f, "{flag} is for scripts, not the REPL"),
            UsageError::Stdin(err) => write!(f, "cannot read the script from stdin: {err}"),
        }
    }
}

impl Hint for UsageError {
    fn exit_code(&self) -> u8 {
        2
    }

    fn fix(&self) -> Option<Fix> {
        Some(Fix::from(match self {
            UsageError::StdinTwice => "give `-e -` once",
            UsageError::ScriptOnly(_) => "give a script with -e SCRIPT",
            UsageError::Stdin(_) => "give it with -e SCRIPT",
            UsageError::Subcommand(_) | UsageError::Command(_) => return None,
        }))
    }
}

/// A directory given as the workspace that can't be read.
#[derive(Debug)]
pub struct ReadDirError {
    dir: PathBuf,
    source: io::Error,
}

impl fmt::Display for ReadDirError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cannot read {}: {}", self.dir.display(), self.source)
    }
}

impl Hint for ReadDirError {
    fn exit_code(&self) -> u8 {
        3
    }

    fn fix(&self) -> Option<Fix> {
        Some("check that it exists and you can read it".into())
    }
}

/// `dir`, canonical.
pub fn canonical(dir: &Path) -> hint::Result<PathBuf, ReadDirError> {
    dir.canonicalize().map_err(|source| {
        let dir = dir.to_path_buf();
        hint::Error::new(ReadDirError { dir, source })
    })
}

/// That the REPL or MCP server records into session `name`.
pub fn recording(name: &str) -> Note {
    format!("recording in session {name}").into()
}
