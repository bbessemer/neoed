//! `ned history`, `ned undo` and `ned session` (spec §1.2).

use std::collections::HashSet;
use std::env;
use std::path::{Path, PathBuf};

use clap::Subcommand;
use ned_core::invoke::{self, Failure};
use ned_core::{session, workspace};

use crate::Terminal;

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
    let state_dir = session::state_dir().map_err(invoke::failure)?;
    if all {
        for (root, names) in session::workspaces(&state_dir).map_err(invoke::failure)? {
            outln!("{}", root.display());
            for name in names {
                outln!("  {name}");
            }
        }
    } else {
        for name in session::sessions(&state_dir, &here(None)?.1).map_err(invoke::failure)? {
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
        session.delete().map_err(invoke::failure)?;
        outln!("{name}: deleted");
    }
    Ok(())
}

/// `ned history`.
pub fn history(flag: Option<String>, dir: Option<PathBuf>, all: bool) -> Result<(), Failure> {
    let session = existing(flag, &here(dir)?.1)?;
    invoke::history(&session, all, &mut Terminal)
}

/// `ned undo`.
pub fn undo(flag: Option<String>, dir: Option<PathBuf>, force: bool) -> Result<(), Failure> {
    let (cwd, root) = here(dir)?;
    let session = existing(flag, &root)?;
    invoke::undo(&session, cwd, force, crate::styles().0, &mut Terminal)
}

/// The working directory, canonical so recorded paths can be shown relative
/// to it, and the workspace's root: `dir` if given, else the working
/// directory's.
fn here(dir: Option<PathBuf>) -> Result<(PathBuf, PathBuf), Failure> {
    let cwd = env::current_dir().unwrap_or_else(|_| ".".into());
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    let root = match dir {
        Some(dir) => dir.canonicalize().map_err(|err| {
            let error = format!("error: cannot read {}: {err}", dir.display());
            (error, 3)
        })?,
        None => workspace::root(&cwd).unwrap_or(cwd.clone()),
    };
    Ok((cwd, root))
}

/// The session `flag` or `NED_SESSION` names, which must have a log in the
/// workspace at `root`.
fn existing(flag: Option<String>, root: &Path) -> Result<session::Session, Failure> {
    let Some(name) = invoke::session_name(flag) else {
        let listed = listing(root);
        let error = format!("error: no session; give one with -s NAME or NED_SESSION ({listed})");
        return Err((error, 2));
    };
    let session = invoke::open(&name, root).map_err(|(error, code)| match code {
        // An invalid name: the fix is one of the workspace's sessions.
        2 => (format!("{error}; {}", listing(root)), code),
        _ => (error, code),
    })?;
    if !session.exists() {
        let listed = listing(root).replace(" in this workspace", " in it");
        let error = format!("error: no session `{name}` in this workspace; {listed}");
        return Err((error, 2));
    }
    Ok(session)
}

/// The workspace's sessions, for an error's fix.
fn listing(root: &Path) -> String {
    let names = session::state_dir().and_then(|dir| session::sessions(&dir, root));
    match names.unwrap_or_default() {
        names if names.is_empty() => "none recorded in this workspace yet".to_string(),
        names => format!("sessions in this workspace: {}", names.join(", ")),
    }
}
