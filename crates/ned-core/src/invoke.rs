//! An invocation of `ned` (spec §1): a script run on a file set, its edits
//! written and committed (§1.3) and the invocation recorded in its session
//! (§1.2), printed through an [`Output`] so every frontend shares it.

use std::env;
use std::path::{Path, PathBuf};

use crate::apply::{self, Committed, Finished, Render, Settings};
use crate::exec::{self, Change, Initial, Options};
use crate::git::Repo;
use crate::lang::Language;
use crate::lsp::Lsp;
use crate::session::{self, Entry, FileChange, Session, SessionError, UncommittedError, UndoError};
use crate::style::Style;
use crate::{fs, script};

/// Where an invocation prints.
pub trait Output {
    /// Text for stdout.
    fn out(&mut self, text: &str);

    /// A line for stderr, starting `error:` or `note:`.
    fn message(&mut self, message: &str);
}

/// An error to print, and the exit code.
pub type Failure = (String, u8);

/// An invocation's file set and flags.
pub struct Invocation {
    pub cwd: PathBuf,
    /// The FILE arguments.
    pub files: Vec<String>,
    /// Whether the file set is the workspace (`-w`) instead of `files`.
    pub workspace: bool,
    /// The workspace's root: `-w`'s, or the one detected.
    pub root: PathBuf,
    pub dry_run: bool,
    pub quiet: bool,
    pub force: bool,
    pub no_fmt: bool,
    pub no_check: bool,
    /// `--lang`: a language for every file, `None` within for text.
    pub lang: Option<Option<Language>>,
    pub context: usize,
    /// `--commit`'s message.
    pub commit: Option<String>,
    /// The style of what goes to stdout.
    pub style: Style,
    /// What the invocation is for, recorded with it.
    pub comment: Option<String>,
}

/// The outcome of running a script, as a session records it.
pub struct Ran {
    pub exit: u8,
    pub error: Option<String>,
    pub changes: Vec<FileChange>,
    /// The commit `--commit` made.
    pub commit: Option<String>,
}

impl Ran {
    /// Prints `error` and fails with `exit`.
    pub fn failed(exit: u8, error: String, out: &mut dyn Output) -> Ran {
        let error = error.trim_end().to_string();
        out.message(&error);
        Ran {
            exit,
            error: Some(error),
            changes: Vec::new(),
            commit: None,
        }
    }
}

/// A script's edits, made and finished (formatted and checked), but not
/// written.
pub struct Edited {
    pub changes: Vec<Change>,
    pub finished: Finished,
}

/// Runs `src` as `invocation`, recorded in `session` if there is one: expands
/// `!!`, then runs the script with the language servers `connect` gives for
/// the workspace's root. Returns the exit code.
pub fn invoke<L: Lsp>(
    mut invocation: Invocation,
    src: String,
    session: Option<&Session>,
    connect: impl FnOnce(PathBuf) -> L,
    out: &mut dyn Output,
) -> u8 {
    let Invocation {
        cwd,
        files,
        workspace,
        root,
        dry_run,
        ..
    } = &mut invocation;
    let src = match repeat(session, src, cwd, files, workspace, root, *dry_run, out) {
        Ok(src) => src,
        Err(failure) => return fail(failure, out),
    };
    let prior = match (&invocation.commit, session) {
        (Some(_), Some(session)) => match uncommitted(session) {
            Ok(prior) => prior,
            Err(failure) => return fail(failure, out),
        },
        _ => Vec::new(),
    };
    let mut lsp = connect(invocation.root.clone());
    let ran = run(&invocation, &src, &prior, &mut lsp, out);
    if let Some(session) = session {
        let entry = Entry {
            id: 0,
            time: session::now(),
            cwd: invocation.cwd,
            files: invocation.files,
            workspace: invocation.workspace.then_some(invocation.root),
            script: Some(src),
            undoes: None,
            write: false,
            dry_run: invocation.dry_run,
            exit: ran.exit,
            error: ran.error,
            changes: ran.changes,
            commit: ran.commit,
            comment: invocation.comment,
        };
        record(session, entry, out);
    }
    ran.exit
}

/// Prints `failure`'s error, returning its exit code.
pub fn fail((error, exit): Failure, out: &mut dyn Output) -> u8 {
    out.message(&error);
    exit
}

/// Parses and runs `src` on `initial`, printing its reads and notes, and
/// finishes its edits (spec §6.4, §6.5).
pub fn execute(
    src: &str,
    initial: Initial,
    options: &Options,
    settings: Settings,
    lsp: &mut dyn Lsp,
    out: &mut dyn Output,
) -> Result<Edited, Ran> {
    let parsed = match script::parse(src) {
        Ok(parsed) => parsed,
        Err(err) => return Err(Ran::failed(2, err.render(src), out)),
    };
    let run = exec::run(&parsed, src, initial, options, Some(&mut *lsp));
    out.out(&run.output);
    for note in &run.notes {
        out.message(&format!("note: {note}"));
    }
    let changes = match run.result {
        Ok(changes) => changes,
        Err(err) => return Err(Ran::failed(err.kind.exit_code(), err.render(src), out)),
    };
    let running: Option<&mut dyn Lsp> = lsp.running().then_some(lsp);
    let mut messages = Vec::new();
    let finished = apply::finish(&changes, run.allow, settings, running, &mut messages);
    for message in messages {
        out.message(&message);
    }
    match finished {
        Ok(finished) => Ok(Edited { changes, finished }),
        Err(rejected) => Err(Ran::failed(rejected.exit, rejected.message, out)),
    }
}

/// Runs the script `src`: prints its output and writes its edits, and with
/// `--commit` commits them after the session's `prior` changes.
fn run(
    invocation: &Invocation,
    src: &str,
    prior: &[(u64, FileChange)],
    lsp: &mut dyn Lsp,
    out: &mut dyn Output,
) -> Ran {
    let Invocation { cwd, root, .. } = invocation;
    // HEAD as the script starts, so a commit made while it runs is noticed.
    let before = invocation
        .commit
        .as_ref()
        .and_then(|_| Repo::discover(root).ok());
    let initial = match invocation.workspace {
        true => Initial::Workspace(root.clone()),
        false => Initial::Files(&invocation.files),
    };
    let options = Options {
        lang: invocation.lang,
        force: invocation.force,
        style: invocation.style,
        overlay: None,
    };
    let settings = Settings {
        format: !invocation.no_fmt,
        check: !invocation.no_check,
        force: invocation.force,
    };
    let Edited { changes, finished } = match execute(src, initial, &options, settings, lsp, out) {
        Ok(edited) => edited,
        Err(ran) => return ran,
    };
    let mut lsp: Option<&mut dyn Lsp> = lsp.running().then_some(lsp);
    let mut messages = Vec::new();
    let finals = finished.finals(&changes);

    let committed = match &invocation.commit {
        Some(message) => {
            // A script that changes nothing has nothing to commit, whatever
            // the session's earlier edits (spec §1.3).
            let prior = if changes.is_empty() { &[][..] } else { prior };
            let commit = apply::commit(
                root,
                before,
                cwd,
                prior,
                &changes,
                &finals,
                message,
                &mut messages,
            );
            match commit {
                Ok(committed) => Some(committed),
                Err(err) => {
                    if let Some(lsp) = lsp.as_deref_mut() {
                        apply::restore(lsp, &changes, &mut messages);
                    }
                    for message in messages {
                        out.message(&message);
                    }
                    return Ran::failed(err.exit_code(), apply::commit_error(&err, prior), out);
                }
            }
        }
        None => None,
    };

    let before_save = match (
        invocation.dry_run || invocation.no_check,
        lsp.as_deref_mut(),
    ) {
        (false, Some(lsp)) => apply::before_save(lsp, &changes, &mut messages),
        _ => None,
    };
    for message in messages.drain(..) {
        out.message(&message);
    }
    if !invocation.dry_run {
        let writes: Vec<(PathBuf, String)> = changes
            .iter()
            .zip(&finals)
            .map(|(change, text)| (PathBuf::from(&change.path), text.to_string()))
            .collect();
        if let Err(err) = fs::write_atomic(&writes, &[]) {
            if let Some(Committed { repo, prepared }) = &committed
                && let Err(git) = repo.retreat(prepared)
            {
                out.message(&format!("error: {git}"));
            }
            let error = format!("error: cannot write files: {err}; no file was changed");
            return Ran::failed(3, error, out);
        }
    }
    let recorded = match invocation.dry_run {
        true => Vec::new(),
        false => changes
            .iter()
            .zip(&finals)
            .map(|(change, text)| {
                let path = cwd.join(&change.path);
                FileChange {
                    path: std::fs::canonicalize(&path).unwrap_or(path),
                    before: (!change.created).then(|| change.old.clone()),
                    after: Some(text.to_string()),
                }
            })
            .collect(),
    };
    let how = Render {
        context: invocation.context,
        quiet: invocation.quiet,
        dry_run: invocation.dry_run,
        style: invocation.style,
    };
    out.out(&apply::render(&changes, &finished, how));
    if let (Some(before), Some(lsp)) = (before_save, lsp.as_deref_mut()) {
        let style = invocation.style;
        let found = apply::after_save(lsp, before, &changes, &finished, style, &mut messages);
        out.out(&found);
        for message in messages.drain(..) {
            out.message(&message);
        }
    }
    if let Some(committed) = &committed {
        let message = invocation.commit.as_deref().unwrap_or_default();
        out.out(&format!("{}\n", committed.line(message)));
    }
    if finished.checked.is_some()
        && invocation.dry_run
        && let Some(lsp) = lsp
    {
        apply::restore(lsp, &changes, &mut messages);
        for message in messages {
            out.message(&message);
        }
    }
    Ran {
        exit: 0,
        error: None,
        changes: recorded,
        commit: committed.map(|c| c.prepared.commit),
    }
}

/// The session `-s` (`flag`) or `NED_SESSION` names, if any.
pub fn session_name(flag: Option<String>) -> Option<String> {
    flag.or_else(|| env::var("NED_SESSION").ok())
        .filter(|name| !name.is_empty())
}

/// Session `name` of the workspace at `root`.
pub fn open(name: &str, root: &Path) -> Result<Session, Failure> {
    Session::new(&session::state_dir().map_err(failure)?, root, name).map_err(failure)
}

/// Appends `entry` to `session`, returning its id; a failure is only a note,
/// since the invocation's files are already written.
pub fn record(session: &Session, entry: Entry, out: &mut dyn Output) -> Option<u64> {
    match session.lock().and_then(|mut log| log.append(entry)) {
        Ok(id) => Some(id),
        Err(err) => {
            let name = session.name();
            out.message(&format!("note: not recorded in session {name}: {err}"));
            None
        }
    }
}

/// The changes `--commit` commits in `session` besides the invocation's own,
/// each with its entry's id (spec §1.3).
pub fn uncommitted(session: &Session) -> Result<Vec<(u64, FileChange)>, Failure> {
    let entries = session.lock().and_then(|log| log.entries());
    session::uncommitted(&entries.map_err(failure)?, fs::read).map_err(|err| {
        let code = match err {
            UncommittedError::Io { .. } => 3,
            UncommittedError::Changed { .. } => 1,
        };
        (format!("error: {err}"), code)
    })
}

/// Expands a `!!` script from `session`'s log, printing a note of what it
/// repeats; another script is returned as it is. Without `files` or
/// `workspace`, the repeat takes the last script's, and with its -w, its
/// `root`.
#[allow(clippy::too_many_arguments)]
pub fn repeat(
    session: Option<&Session>,
    src: String,
    cwd: &Path,
    files: &mut Vec<String>,
    workspace: &mut bool,
    root: &mut PathBuf,
    dry_run: bool,
    out: &mut dyn Output,
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
    let (entry, script) = match session::repeat(&src, &entries, dry_run) {
        None => return Ok(src),
        Some(result) => result.map_err(|err| (format!("error: {err}"), 2))?,
    };
    let summary = session::script_summary(&script);
    out.message(&format!("note: repeating {}: {summary}", entry.id));
    if files.is_empty() && !*workspace {
        match &entry.workspace {
            Some(dir) => {
                *workspace = true;
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

/// `ned history` of `session`.
pub fn history(session: &Session, all: bool, out: &mut dyn Output) -> Result<(), Failure> {
    let entries = session.lock().and_then(|log| log.entries());
    out.out(&session::history(&entries.map_err(failure)?, all));
    Ok(())
}

/// `ned undo` in `session`, holding its lock from reading the log to
/// recording the undo, so no other invocation in the session comes between.
pub fn undo(
    session: &Session,
    cwd: PathBuf,
    force: bool,
    style: Style,
    out: &mut dyn Output,
) -> Result<(), Failure> {
    let mut log = session.lock().map_err(failure)?;
    let entries = log.entries().map_err(failure)?;
    let undo = session::undo(&entries, fs::read, force).map_err(|err| {
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

    // An entry undo can revert has no script only if it's a REPL write.
    let script = undo
        .script
        .as_deref()
        .map_or("write".to_string(), session::script_summary);
    out.out(&format!("undo {}: {script}\n", undo.id));
    out.out(&apply::file_changes(&undo.changes, &cwd, 1, style));

    let entry = Entry {
        id: 0,
        time: session::now(),
        cwd,
        files: Vec::new(),
        workspace: None,
        script: None,
        undoes: Some(undo.id),
        write: false,
        dry_run: false,
        exit: 0,
        error: None,
        changes: undo.changes,
        commit: None,
        comment: None,
    };
    if let Err(err) = log.append(entry) {
        let name = session.name();
        out.message(&format!("note: not recorded in session {name}: {err}"));
    }
    Ok(())
}

pub fn failure(err: SessionError) -> Failure {
    let code = match err {
        SessionError::BadName(_) => 2,
        _ => 3,
    };
    (format!("error: {err}"), code)
}
