//! Sessions in the CLI: `-s`, `NED_SESSION`, `ned history` and `ned undo`
//! (spec §1.2).

use std::env;
use std::io;
use std::path::{Path, PathBuf};

use ned_core::diff::{self, DiffStat};
use ned_core::fs;
use ned_core::session::{self, Entry, Session, SessionError, UndoError};
use ned_core::workspace;

/// The session `-s` (`flag`) or `NED_SESSION` names, if any.
pub fn name(flag: Option<String>) -> Option<String> {
    flag.or_else(|| env::var("NED_SESSION").ok())
        .filter(|name| !name.is_empty())
}

/// Session `name` of the workspace at `root`, or the error to print and the
/// exit code.
pub fn open(name: &str, root: &Path) -> Result<Session, Failure> {
    Session::new(&session::state_dir().map_err(failure)?, root, name).map_err(failure)
}

/// Appends `entry` to `session`; a failure is only a note, since the
/// invocation's files are already written.
pub fn record(session: &Session, entry: Entry) {
    if let Err(err) = session.lock().and_then(|mut log| log.append(entry)) {
        eprintln!("note: not recorded in session {}: {err}", session.name());
    }
}

/// An error to print, and the exit code.
pub type Failure = (String, u8);

/// `ned history`.
pub fn history(flag: Option<String>, all: bool) -> Result<(), Failure> {
    let session = existing(flag, &here().1)?;
    let entries = session.lock().and_then(|log| log.entries());
    out!("{}", session::history(&entries.map_err(failure)?, all));
    Ok(())
}

/// `ned undo`, holding the session's lock from reading the log to recording
/// the undo, so no other invocation in the session comes between.
pub fn undo(flag: Option<String>, force: bool) -> Result<(), Failure> {
    let (cwd, root) = here();
    let session = existing(flag, &root)?;
    let mut log = session.lock().map_err(failure)?;
    let entries = log.entries().map_err(failure)?;
    let read = |path: &Path| match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    };
    let undo = session::undo(&entries, read, force).map_err(|err| {
        let code = match err {
            UndoError::Io { .. } => 3,
            _ => 1,
        };
        (format!("error: {}", err.relative_to(&cwd)), code)
    })?;

    let mut writes = Vec::new();
    let mut removes = Vec::new();
    for change in &undo.changes {
        match &change.after {
            Some(text) => writes.push((change.path.clone(), text.clone())),
            None => removes.push(change.path.clone()),
        }
    }
    if let Err(err) = fs::write_atomic(&writes, &removes) {
        return Err((
            format!("error: cannot write files: {err}; no file was changed"),
            3,
        ));
    }

    let script = undo.script.as_deref().map(session::script_summary);
    outln!("undo {}: {}", undo.id, script.unwrap_or_default());
    for change in &undo.changes {
        let path = change.path.strip_prefix(&cwd).unwrap_or(&change.path);
        let path = path.to_string_lossy();
        let before = change.before.as_deref().unwrap_or_default();
        let after = change.after.as_deref().unwrap_or_default();
        let stat = DiffStat::between(before, after);
        outln!(
            "{}",
            match (&change.before, &change.after) {
                (None, _) => diff::created_summary(&path, stat, false),
                (_, None) => diff::removed_summary(&path, stat),
                _ => diff::summary(&path, diff::regions(before, after), stat, false),
            }
        );
        out!("{}", diff::hunks(before, after, 1));
    }

    let entry = Entry {
        id: 0,
        time: session::now(),
        cwd,
        files: Vec::new(),
        workspace: None,
        script: None,
        undoes: Some(undo.id),
        dry_run: false,
        exit: 0,
        error: None,
        changes: undo.changes,
    };
    if let Err(err) = log.append(entry) {
        eprintln!("note: not recorded in session {}: {err}", session.name());
    }
    Ok(())
}

/// Expands a `!!` script from `session`'s log, printing a note of what it
/// repeats; another script is returned as it is. Without FILE arguments or
/// -w, the repeat takes the last script's, and with its -w, its `root`.
pub fn repeat(
    session: Option<&Session>,
    src: String,
    cwd: &Path,
    files: &mut Vec<String>,
    workspace: &mut Option<Option<PathBuf>>,
    root: &mut PathBuf,
) -> Result<String, Failure> {
    if !session::is_repeat(&src) {
        return Ok(src);
    }
    let Some(session) = session else {
        let error = "error: `!!` repeats a session's last script; give the session with -s NAME or NED_SESSION";
        return Err((error.to_string(), 2));
    };
    let entries = session.lock().and_then(|log| log.entries());
    let entries = entries.map_err(failure)?;
    let (entry, script) = match session::repeat(&src, &entries) {
        None => return Ok(src),
        Some(result) => result.map_err(|err| (format!("error: {err}"), 2))?,
    };
    eprintln!(
        "note: repeating {}: {}",
        entry.id,
        session::script_summary(&script)
    );
    if files.is_empty() && workspace.is_none() {
        match &entry.workspace {
            Some(dir) => {
                *workspace = Some(Some(dir.clone()));
                *root = dir.clone();
            }
            None => {
                *files = entry
                    .files
                    .iter()
                    .map(|file| match entry.cwd == cwd {
                        true => file.clone(),
                        false => entry.cwd.join(file).to_string_lossy().into_owned(),
                    })
                    .collect();
            }
        }
    }
    Ok(script)
}

/// The working directory, canonical so recorded paths can be shown relative
/// to it, and its workspace's root.
fn here() -> (PathBuf, PathBuf) {
    let cwd = env::current_dir().unwrap_or_else(|_| ".".into());
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    let root = workspace::root(&cwd).unwrap_or(cwd.clone());
    (cwd, root)
}

/// The session `flag` or `NED_SESSION` names, which must have a log in the
/// workspace at `root`.
fn existing(flag: Option<String>, root: &Path) -> Result<Session, Failure> {
    let Some(name) = name(flag) else {
        let listed = listing(root);
        let error = format!("error: no session; give one with -s NAME or NED_SESSION ({listed})");
        return Err((error, 2));
    };
    let session = open(&name, root)?;
    if !session.exists() {
        let listed = listing(root).replace(" in this workspace", " in it");
        let error = format!("error: no session `{name}` in this workspace; {listed}");
        return Err((error, 2));
    }
    Ok(session)
}

fn failure(err: SessionError) -> Failure {
    (format!("error: {err}"), exit_code(&err))
}

/// The workspace's sessions, for an error's fix.
fn listing(root: &Path) -> String {
    let names = session::state_dir().and_then(|dir| session::sessions(&dir, root));
    match names.unwrap_or_default() {
        names if names.is_empty() => "none recorded in this workspace yet".to_string(),
        names => format!("sessions in this workspace: {}", names.join(", ")),
    }
}

fn exit_code(err: &SessionError) -> u8 {
    match err {
        SessionError::BadName(_) => 2,
        _ => 3,
    }
}
