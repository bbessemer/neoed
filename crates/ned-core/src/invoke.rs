//! An invocation of `ned` (spec §1): a script run on a file set, its edits
//! written and committed (§1.3) and the invocation recorded in its session
//! (§1.2), printed through an [`Output`] so every frontend shares it.

use std::env;
use std::path::{Path, PathBuf};

use crate::apply::{self, Committed, Finished, Render, Settings};
use crate::exec::{self, Change, Initial, Options};
use crate::git::Repo;
use crate::hint::{Error, Errors, Frontend, Hint, Note, Report};
use crate::lang::Language;
use crate::lsp::Lsp;
use crate::session::{self, Entry, FileChange, RepeatErrorKind, Session, SessionError};
use crate::style::Style;
use crate::{fs, script};

/// Where an invocation prints.
pub trait Output {
    /// Text for stdout.
    fn out(&mut self, text: &str);

    /// A line for stderr, starting `error:` or `note:`.
    fn message(&mut self, message: &str);
}

/// Errors rendered for a frontend, and their exit code.
pub type Failure = (String, u8);

/// Files that couldn't be written, so none was.
#[derive(Debug, thiserror::Error)]
#[error("cannot write files: {0}; no file was changed")]
pub struct WriteError(pub String);

impl Hint for WriteError {
    fn exit_code(&self) -> u8 {
        3
    }
}

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
    /// Who gave it, which names its flags in notes.
    pub frontend: Frontend,
    /// `--commit`'s message.
    pub commit: Option<String>,
    /// The style of what goes to stdout.
    pub style: Style,
    /// What the invocation is for, recorded with it.
    pub comment: Option<String>,
}

/// What a script that ran changed, as a session records it.
pub struct Ran {
    pub changes: Vec<FileChange>,
    /// The commit `--commit` made.
    pub commit: Option<String>,
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
        quiet,
        force,
        no_fmt,
        no_check,
        lang,
        commit,
        frontend,
        ..
    } = &mut invocation;
    let frontend = *frontend;
    // The MCP server's language is its own, not a call's.
    let lang = lang.is_some() && frontend == Frontend::Cli;
    let flags = *dry_run || *quiet || *force || *no_fmt || *no_check || lang || commit.is_some();
    let without = (!flags).then_some(frontend);
    let committing = commit.is_some();
    let started = Report::collect(|notes| {
        let src = repeat(
            session, src, cwd, files, workspace, root, *dry_run, without, notes,
        )?;
        let prior = match (committing, session) {
            (true, Some(session)) => uncommitted(session)?,
            _ => Vec::new(),
        };
        Ok((src, prior))
    });
    let (src, prior) = match report(started, frontend, None, out) {
        Ok(started) => started,
        Err(failure) => return fail(failure, out),
    };
    let mut lsp = connect(invocation.root.clone());
    let ran = Report::collect(|notes| run(&invocation, &src, &prior, &mut lsp, notes, out));
    let (ran, exit, error) = match report(ran, frontend, Some(&src), out) {
        Ok(ran) => (ran, 0, None),
        Err((error, exit)) => {
            out.message(&error);
            let ran = Ran {
                changes: Vec::new(),
                commit: None,
            };
            (ran, exit, Some(error))
        }
    };
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
            exit,
            error,
            changes: ran.changes,
            commit: ran.commit,
            comment: invocation.comment,
        };
        let recorded = Report::collect(|notes| Ok(record(session, entry, notes)));
        let _ = report(recorded, frontend, None, out);
    }
    exit
}

/// Prints `failure`'s error, returning its exit code.
pub fn fail((error, exit): Failure, out: &mut dyn Output) -> u8 {
    out.message(&error);
    exit
}

/// Prints `report`'s notes in `frontend`'s terms, and gives its value, or its
/// errors in those terms, located in `src`, with their exit code, for the
/// caller to print.
pub fn report<T>(
    report: impl Into<Report<T>>,
    frontend: Frontend,
    src: Option<&str>,
    out: &mut dyn Output,
) -> Result<T, Failure> {
    let report = report.into();
    let notes = report.notes.len();
    match report.render(frontend, src) {
        Ok((value, lines)) => {
            for line in lines {
                out.message(&line);
            }
            Ok(value)
        }
        Err((lines, exit)) => {
            for line in &lines[..notes] {
                out.message(line);
            }
            Err((lines[notes..].join("\n").trim_end().to_string(), exit))
        }
    }
}

/// Parses and runs `src` on `initial`, printing its reads, and finishes its
/// edits (spec §6.4, §6.5), adding to `notes` as it goes.
pub fn execute(
    src: &str,
    initial: Initial,
    options: &Options,
    settings: Settings,
    lsp: &mut dyn Lsp,
    notes: &mut Vec<Note>,
    out: &mut dyn Output,
) -> Result<Edited, Errors> {
    let parsed = match script::parse(src) {
        Ok(parsed) => parsed,
        Err(mut err) => {
            if let (script::ParseErrorKind::BareName { word, rest }, Initial::Files(paths)) =
                (&err.kind, &initial)
                // After a heredoc that ended early, the fix names the heredoc,
                // the cause, not the name.
                && err.fix == Some(script::bare_name_fix(word, rest, None))
            {
                let kind = exec::kinds_named(paths, options, word).first().copied();
                err.fix = Some(script::bare_name_fix(word, rest, kind));
            }
            return Err(err.into());
        }
    };
    let run = exec::run(&parsed, src, initial, options, Some(&mut *lsp));
    out.out(&run.output);
    notes.extend(run.notes);
    let changes = run.result?;
    let running: Option<&mut dyn Lsp> = lsp.running().then_some(lsp);
    let finished = apply::finish(&changes, run.allow, settings, running, notes)?;
    Ok(Edited { changes, finished })
}

/// Runs the script `src`: prints its output and writes its edits, and with
/// `--commit` commits them after the session's `prior` changes, adding to
/// `notes` as it goes.
fn run(
    invocation: &Invocation,
    src: &str,
    prior: &[(u64, FileChange)],
    lsp: &mut dyn Lsp,
    notes: &mut Vec<Note>,
    out: &mut dyn Output,
) -> Result<Ran, Errors> {
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
    let Edited { changes, finished } = execute(src, initial, &options, settings, lsp, notes, out)?;
    let mut lsp: Option<&mut dyn Lsp> = lsp.running().then_some(lsp);
    let finals = finished.finals(&changes);

    let committed = match &invocation.commit {
        Some(message) => {
            // A script that changes nothing has nothing to commit, whatever
            // the session's earlier edits (spec §1.3).
            let prior = if changes.is_empty() { &[][..] } else { prior };
            let commit = apply::commit(root, before, cwd, prior, &changes, &finals, message);
            match commit {
                Ok(committed) => Some(committed),
                Err(errors) => {
                    if let Some(lsp) = lsp.as_deref_mut() {
                        apply::restore(lsp, &changes, notes);
                    }
                    return Err(errors);
                }
            }
        }
        None => None,
    };

    let before_save = match (
        invocation.dry_run || invocation.no_check,
        lsp.as_deref_mut(),
    ) {
        (false, Some(lsp)) => apply::before_save(lsp, &changes, notes),
        _ => None,
    };
    if !invocation.dry_run {
        let writes: Vec<(PathBuf, String)> = changes
            .iter()
            .zip(&finals)
            .map(|(change, text)| (PathBuf::from(&change.path), text.to_string()))
            .collect();
        if let Err(err) = fs::write_atomic(&writes, &[]) {
            let mut errors = Errors::from(Error::new(WriteError(err.to_string())));
            if let Some(Committed { repo, prepared }) = &committed
                && let Err(git) = repo.retreat(prepared)
            {
                errors.push(git);
            }
            return Err(errors);
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
        out.out(&apply::after_save(
            lsp, before, &changes, &finished, style, notes,
        ));
    }
    if let Some(committed) = &committed {
        let message = invocation.commit.as_deref().unwrap_or_default();
        out.out(&format!("{}\n", committed.line(message)));
    }
    if finished.checked.is_some()
        && invocation.dry_run
        && let Some(lsp) = lsp
    {
        apply::restore(lsp, &changes, notes);
    }
    Ok(Ran {
        changes: recorded,
        commit: committed.map(|c| c.prepared.commit),
    })
}

/// The session `-s` (`flag`) or `NED_SESSION` names, if any.
pub fn session_name(flag: Option<String>) -> Option<String> {
    flag.or_else(|| env::var("NED_SESSION").ok())
        .filter(|name| !name.is_empty())
}

/// Session `name` of the workspace at `root`.
pub fn open(name: &str, root: &Path) -> Result<Session, SessionError> {
    Session::new(&session::state_dir()?, root, name)
}

/// Appends `entry` to `session`, returning its id; a failure is only a note in
/// `notes`, since the invocation's files are already written.
pub fn record(session: &Session, entry: Entry, notes: &mut Vec<Note>) -> Option<u64> {
    match session.lock().and_then(|mut log| log.append(entry)) {
        Ok(id) => Some(id),
        Err(err) => {
            notes.push(unrecorded(session, err));
            None
        }
    }
}

/// The note for an entry `session` couldn't record.
fn unrecorded(session: &Session, err: SessionError) -> Note {
    Note::from(err).context(format!("not recorded in session {}", session.name()))
}

/// The changes `--commit` commits in `session` besides the invocation's own,
/// each with its entry's id (spec §1.3).
fn uncommitted(session: &Session) -> Result<Vec<(u64, FileChange)>, Errors> {
    let entries = session.lock().and_then(|log| log.entries())?;
    Ok(session::uncommitted(&entries, fs::read)?)
}

/// Expands a `!!` script from `session`'s log, with a note in `notes` of what
/// it repeats, and, when `without` names the frontend of an invocation that
/// gave no flags, that it's without them; another script is returned as it
/// is. Without `files` or `workspace`, the repeat takes the last script's, and
/// with its -w, its `root`.
#[allow(clippy::too_many_arguments)]
pub fn repeat(
    session: Option<&Session>,
    src: String,
    cwd: &Path,
    files: &mut Vec<String>,
    workspace: &mut bool,
    root: &mut PathBuf,
    dry_run: bool,
    without: Option<Frontend>,
    notes: &mut Vec<Note>,
) -> Result<String, Errors> {
    if !session::is_repeat(&src) {
        return Ok(src);
    }
    let Some(session) = session else {
        return Err(Error::new(RepeatErrorKind::NoSession).into());
    };
    let entries = session.lock().and_then(|log| log.entries())?;
    let (entry, script) = match session::repeat(&src, &entries, dry_run) {
        None => return Ok(src),
        Some(result) => result?,
    };
    let summary = session::script_summary(&script);
    let without = match without {
        None | Some(Frontend::Repl) => "",
        Some(Frontend::Cli) => " without flags",
        Some(Frontend::Mcp) => " without arguments",
    };
    notes.push(format!("repeating {}{without}: {summary}", entry.id).into());
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
pub fn history(session: &Session, all: bool, out: &mut dyn Output) -> Result<(), SessionError> {
    let entries = session.lock().and_then(|log| log.entries())?;
    out.out(&session::history(&entries, all));
    Ok(())
}

/// `ned undo` in `session`, holding its lock from reading the log to
/// recording the undo, so no other invocation in the session comes between.
pub fn undo(
    session: &Session,
    cwd: PathBuf,
    force: bool,
    style: Style,
    notes: &mut Vec<Note>,
    out: &mut dyn Output,
) -> Result<(), Errors> {
    let mut log = session.lock()?;
    let entries = log.entries()?;
    let undo = session::undo(&entries, fs::read, force).map_err(|err| err.relative_to(&cwd))?;

    let mut writes = Vec::new();
    let mut removes = Vec::new();
    for change in &undo.changes {
        match &change.after {
            Some(text) => writes.push((change.path.clone(), text.clone())),
            None => removes.push(change.path.clone()),
        }
    }
    if let Err(err) = fs::write_atomic(&writes, &removes) {
        return Err(Error::new(WriteError(err.to_string())).into());
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
        notes.push(unrecorded(session, err));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What an invocation printed to stderr.
    #[derive(Default)]
    struct Messages(Vec<String>);

    impl Output for Messages {
        fn out(&mut self, _: &str) {}

        fn message(&mut self, message: &str) {
            self.0.push(message.to_string());
        }
    }

    fn noted<T>(result: Result<T, Errors>) -> Report<T> {
        Report::collect(|notes| {
            notes.push(Error::new(RepeatErrorKind::DryRun(4)).into());
            result
        })
    }

    #[test]
    fn report_prints_the_notes_and_gives_the_errors_to_print() {
        let mut out = Messages::default();
        assert_eq!(report(noted(Ok(7)), Frontend::Mcp, None, &mut out), Ok(7));
        assert_eq!(
            out.0,
            [
                "note: `!!` would apply entry 4, a dry run; send the script again to apply it, or give `dry_run` to preview it again"
            ]
        );

        let mut out = Messages::default();
        let mut errors = Errors::from(Error::new(WriteError("disk full".into())));
        errors.push(Error::new(RepeatErrorKind::NoSession));
        let failure = report(noted::<()>(Err(errors)), Frontend::Repl, None, &mut out);
        assert_eq!(out.0.len(), 1, "only the note is printed: {:?}", out.0);
        assert_eq!(
            failure,
            Err((
                "error: cannot write files: disk full; no file was changed\nerror: `!!` repeats a session's last script; name a session (restart the REPL with -s NAME)".to_string(),
                3
            ))
        );
    }
}
