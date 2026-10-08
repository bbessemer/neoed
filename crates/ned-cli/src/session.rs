//! `ned history`, `ned undo` and `ned session` (spec §1.2).

use std::collections::HashSet;
use std::env;
use std::fmt;
use std::path::{Path, PathBuf};

use clap::Subcommand;
use ned_core::hint::{self, AnyError, Fix, Frontend, Hint, Report};
use ned_core::invoke::{self, Failure};
use ned_core::session::{self, Session, SessionErrorKind};
use ned_core::workspace;

use crate::{Terminal, error};

#[derive(Subcommand)]
pub enum Action {
    /// List the workspace's sessions.
    List {
        /// List every workspace's sessions, under its path.
        #[arg(long)]
        all: bool,
    },
    /// Delete sessions of the workspace.
    Delete {
        #[arg(required = true, value_name = "NAME")]
        names: Vec<String>,
        /// The workspace the sessions belong to, instead of the working
        /// directory's.
        #[arg(short, long, value_name = "DIR")]
        workspace: Option<PathBuf>,
    },
}

/// `ned session`.
pub fn run(action: Action) -> Result<(), Failure> {
    match action {
        Action::List { all } => list(all),
        Action::Delete { names, workspace } => delete(names, workspace),
    }
}

fn list(all: bool) -> Result<(), Failure> {
    let state_dir = invoke::report(session::state_dir(), Frontend::Cli, None, &mut Terminal)?;
    if all {
        let workspaces = session::workspaces(&state_dir);
        for (root, names) in invoke::report(workspaces, Frontend::Cli, None, &mut Terminal)? {
            outln!("{}", root.display());
            for name in names {
                outln!("  {name}");
            }
        }
    } else {
        let sessions = session::sessions(&state_dir, &here(None)?.1);
        for name in invoke::report(sessions, Frontend::Cli, None, &mut Terminal)? {
            outln!("{name}");
        }
    }
    Ok(())
}

fn delete(mut names: Vec<String>, dir: Option<PathBuf>) -> Result<(), Failure> {
    let mut seen = HashSet::new();
    names.retain(|name| seen.insert(name.clone()));
    let root = here(dir)?.1;
    let sessions = names
        .into_iter()
        .map(|name| existing(Some(name), &root))
        .collect::<Result<Vec<_>, _>>()?;
    for session in sessions {
        let name = session.name().to_string();
        invoke::report(session.delete(), Frontend::Cli, None, &mut Terminal)?;
        outln!("{name}: deleted");
    }
    Ok(())
}

/// `ned history`.
pub fn history(flag: Option<String>, dir: Option<PathBuf>, all: bool) -> Result<(), Failure> {
    let session = existing(flag, &here(dir)?.1)?;
    let history = invoke::history(&session, all, &mut Terminal);
    invoke::report(history, Frontend::Cli, None, &mut Terminal)
}

/// `ned undo`.
pub fn undo(flag: Option<String>, dir: Option<PathBuf>, force: bool) -> Result<(), Failure> {
    let (cwd, root) = here(dir)?;
    let session = existing(flag, &root)?;
    let style = crate::styles().0;
    let undone =
        Report::collect(|notes| invoke::undo(&session, cwd, force, style, notes, &mut Terminal));
    invoke::report(undone, Frontend::Cli, None, &mut Terminal)
}

/// The working directory, canonical so recorded paths can be shown relative
/// to it, and the workspace's root: `dir` if given, else the working
/// directory's.
fn here(dir: Option<PathBuf>) -> Result<(PathBuf, PathBuf), Failure> {
    let cwd = env::current_dir().unwrap_or_else(|_| ".".into());
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    let root = match dir {
        Some(dir) => invoke::report(error::canonical(&dir), Frontend::Cli, None, &mut Terminal)?,
        None => workspace::root(&cwd).unwrap_or(cwd.clone()),
    };
    Ok((cwd, root))
}

/// The session `flag` or `NED_SESSION` names, which must have a log in the
/// workspace at `root`.
fn existing(flag: Option<String>, root: &Path) -> Result<Session, Failure> {
    invoke::report(logged(flag, root), Frontend::Cli, None, &mut Terminal)
}

fn logged(flag: Option<String>, root: &Path) -> Result<Session, AnyError> {
    let Some(name) = invoke::session_name(flag) else {
        return Err(listed(MissingSession::Unnamed.into(), root).into());
    };
    let session = invoke::open(&name, root).map_err(|err| match err.kind {
        // An invalid name: the fix is also one of the workspace's sessions.
        SessionErrorKind::BadName(_) => listed(err, root),
        _ => err,
    })?;
    if !session.exists() {
        return Err(listed(MissingSession::Unrecorded(name).into(), root).into());
    }
    Ok(session)
}

/// A session `ned history`, `ned undo` or `ned session delete` can't act on.
#[derive(Debug)]
enum MissingSession {
    Unnamed,
    Unrecorded(String),
}

impl fmt::Display for MissingSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MissingSession::Unnamed => f.write_str("no session"),
            MissingSession::Unrecorded(name) => write!(f, "no session `{name}`"),
        }
    }
}

impl Hint for MissingSession {
    fn exit_code(&self) -> u8 {
        2
    }

    fn fix(&self) -> Option<Fix> {
        match self {
            MissingSession::Unnamed => Some("give one with {-s NAME}".into()),
            MissingSession::Unrecorded(_) => None,
        }
    }
}

/// `error`, its fix followed by the workspace's sessions.
fn listed<K: Hint>(mut error: hint::Error<K>, root: &Path) -> hint::Error<K> {
    let fix = match error.fix.take() {
        Some(fix) => fix.and(listing(root)),
        None => listing(root),
    };
    error.with_fix(fix)
}

/// The workspace's sessions, as a fix.
fn listing(root: &Path) -> Fix {
    let names = session::state_dir().and_then(|dir| session::sessions(&dir, root));
    Fix::new(match names.unwrap_or_default() {
        names if names.is_empty() => "none recorded in this workspace yet".to_string(),
        names => {
            let names = hint::verbatim(&names.join(", "));
            format!("sessions in this workspace: {names}")
        }
    })
}
