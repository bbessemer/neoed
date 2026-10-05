//! What follows a script's execution, for every frontend: formatting, checking
//! the edits with language servers, committing them, and rendering the result
//! (command-language spec §6.3-§6.5, §1.3).

use std::path::{Path, PathBuf};

use crate::buffer::Buffer;
use crate::config::{self, Config, ConfigError};
use crate::diff::{self, DiffStat};
use crate::exec::Change;
use crate::format::{self, Outcome};
use crate::git::{FileEdit, GitError, Prepared, Repo};
use crate::lang::Language;
use crate::lsp::{self, Checked, Diagnostic, Lsp, LspFailure, Severity};
use crate::session::FileChange;
use crate::style::{Role, Style};

/// How a script's edits are finished.
#[derive(Debug, Clone, Copy, Default)]
pub struct Settings {
    /// Run formatters (not `--no-fmt`).
    pub format: bool,
    /// Check the edits with language servers (not `--no-check`).
    pub check: bool,
    /// `--force`: introduced diagnostics don't block.
    pub force: bool,
}

/// The finished edits: what formatting did to each changed file, and what
/// checking found, if it ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    pub outcomes: Vec<Outcome>,
    pub checked: Option<Checked>,
}

/// Why the edits can't be written: the message ends with a fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    pub exit: u8,
    pub message: String,
}

impl Finished {
    /// The text to write for each change: formatted, or as the script left it.
    pub fn finals<'a>(&'a self, changes: &'a [Change]) -> Vec<&'a str> {
        changes
            .iter()
            .zip(&self.outcomes)
            .map(|(change, outcome)| match outcome {
                Outcome::Formatted { text, .. } => text.as_str(),
                _ => change.new.as_str(),
            })
            .collect()
    }
}

/// Formats `changes`, falling back to language servers when `lsp` (a running
/// daemon's) is given, and checks them with it. `messages` gets the lines for
/// stderr, each starting `note:` or `error:`. Edits blocked by the diagnostics
/// they introduce are rejected, after the servers are sent the original texts
/// back.
pub fn finish(
    changes: &[Change],
    allow: Option<Severity>,
    settings: Settings,
    mut lsp: Option<&mut (dyn Lsp + '_)>,
    messages: &mut Vec<String>,
) -> Result<Finished, Rejected> {
    let mut outcomes = match settings.format {
        false => vec![Outcome::Unchanged; changes.len()],
        true => Config::new(config::user_config().as_deref())
            .and_then(|mut config| format::run(changes, &mut config))
            .map_err(|err: ConfigError| Rejected {
                exit: 2,
                message: format!("error: {err}"),
            })?,
    };
    if let Some(lsp) = lsp.as_deref_mut() {
        format::fallback(changes, &mut outcomes, lsp);
    }
    for outcome in &outcomes {
        if let Outcome::NotFound(note) | Outcome::Failed(note) = outcome {
            messages.push(format!("note: {note}"));
        }
    }
    let mut finished = Finished {
        outcomes,
        checked: None,
    };
    let Some(lsp) = lsp.filter(|_| settings.check && !changes.is_empty()) else {
        return Ok(finished);
    };
    let finals = finished.finals(changes);
    let checked = match lsp::check_changes(lsp, changes, &finals, allow, settings.force) {
        Ok(checked) => checked,
        Err(LspFailure(message)) => {
            let paths: Vec<&str> = changes.iter().map(|c| c.path.as_str()).collect();
            messages.push(format!(
                "note: {message}; skipped checking {}",
                paths.join(", ")
            ));
            return Ok(finished);
        }
    };
    if !checked.blocking.is_empty() {
        let message = blocked(changes, &finals, &checked);
        restore(lsp, changes, messages);
        return Err(Rejected { exit: 1, message });
    }
    finished.checked = Some(checked);
    Ok(finished)
}

/// Sends the servers the original texts of `changes` back, after an edit that
/// wasn't written, adding a note to `messages` if that fails.
pub fn restore(lsp: &mut dyn Lsp, changes: &[Change], messages: &mut Vec<String>) {
    if let Err(LspFailure(message)) = lsp::restore(lsp, changes) {
        messages.push(format!("note: {message}"));
    }
}

/// Starts checking a write with the checks servers run on save (spec §6.5),
/// before the files are written; `None`, with a note in `messages`, if that
/// failed.
pub fn before_save(
    lsp: &mut dyn Lsp,
    changes: &[Change],
    messages: &mut Vec<String>,
) -> Option<lsp::BeforeSave> {
    match lsp::before_save(lsp, changes) {
        Ok(before) => Some(before),
        Err(LspFailure(message)) => {
            messages.push(skipped_on_save(&message, changes));
            None
        }
    }
}

/// Finishes checking the write, once the files are written: the diagnostics it
/// introduced that `finished`'s check didn't print, rendered.
pub fn after_save(
    lsp: &mut dyn Lsp,
    before: lsp::BeforeSave,
    changes: &[Change],
    finished: &Finished,
    style: Style,
    messages: &mut Vec<String>,
) -> String {
    let finals = finished.finals(changes);
    let saved = match lsp::check_saved(lsp, before, changes, &finals, finished.checked.as_ref()) {
        Ok(saved) => saved,
        Err(LspFailure(message)) => {
            messages.push(skipped_on_save(&message, changes));
            return String::new();
        }
    };
    messages.extend(saved.notes.iter().map(|note| format!("note: {note}")));
    let mut out = String::new();
    for ((change, text), found) in changes.iter().zip(&finals).zip(&saved.files) {
        out.push_str(&diagnostics(&change.path, text, found, style));
    }
    out
}

/// The note for checks on save that failed.
fn skipped_on_save(message: &str, changes: &[Change]) -> String {
    let paths: Vec<&str> = changes.iter().map(|c| c.path.as_str()).collect();
    format!(
        "note: {message}; skipped the checks run on save of {}",
        paths.join(", ")
    )
}

/// `found`, diagnostics of the file at `path` holding `text`, in `check`'s
/// format.
fn diagnostics(path: &str, text: &str, found: &[Diagnostic], style: Style) -> String {
    let buffer = Buffer::new(text);
    found
        .iter()
        .map(|d| lsp::render(path, &buffer, d, style))
        .collect()
}

/// The error for an edit rejected by the diagnostics it introduces.
fn blocked(changes: &[Change], finals: &[&str], checked: &Checked) -> String {
    let count = |severity| {
        checked
            .blocking
            .iter()
            .filter(|(_, d)| d.severity == severity)
            .count()
    };
    let counts: Vec<String> = Severity::ALL
        .into_iter()
        .filter(|&s| count(s) > 0)
        .map(|s| match count(s) {
            1 => format!("1 {s}"),
            n => format!("{n} {s}s"),
        })
        .collect();
    let them = if checked.blocking.len() == 1 {
        "it"
    } else {
        "them"
    };
    let allow = if count(Severity::Error) > 0 {
        "errors"
    } else {
        "warnings"
    };
    let mut text = format!(
        "error: edit introduces {}; fix {them}, or add `allow {allow}` to the script to apply it anyway\n",
        counts.join(" and ")
    );
    for (i, d) in &checked.blocking {
        text.push_str(&lsp::render(
            &changes[*i].path,
            &Buffer::new(finals[*i]),
            d,
            Style::Plain,
        ));
    }
    text
}

/// How finished edits are printed.
#[derive(Debug, Clone, Copy)]
pub struct Render {
    pub context: usize,
    /// `--quiet`: summary lines only.
    pub quiet: bool,
    pub dry_run: bool,
    pub style: Style,
}

/// Each change's summary line and hunks, then its formatter's changes and the
/// diagnostics it introduced (spec §6.3-§6.5).
pub fn render(changes: &[Change], finished: &Finished, how: Render) -> String {
    let style = how.style;
    let finals = finished.finals(changes);
    let mut out = String::new();
    for (i, (change, outcome)) in changes.iter().zip(&finished.outcomes).enumerate() {
        let stat = DiffStat::between(&change.old, &change.new);
        let summary = if change.created {
            diff::created_summary(&change.path, stat, how.dry_run)
        } else {
            diff::summary(&change.path, change.edits, stat, how.dry_run)
        };
        out.push_str(&style.paint(Role::Header, &summary));
        out.push('\n');
        let new = diff::Side::new(&change.new, change.lang);
        if !how.quiet {
            let old = diff::Side::new(&change.old, change.lang);
            out.push_str(&diff::hunks(&old, &new, how.context, style));
        }
        if let Outcome::Formatted { name, text } = outcome {
            let header = format!("fmt {name}: {}", DiffStat::between(&change.new, text));
            out.push_str(&style.paint(Role::Header, &header));
            out.push('\n');
            if !how.quiet {
                let text = diff::Side::new(text, change.lang);
                out.push_str(&diff::hunks(&new, &text, how.context, style));
            }
        }
        if let Some(checked) = &finished.checked {
            out.push_str(&diagnostics(
                &change.path,
                finals[i],
                &checked.files[i],
                style,
            ));
        }
    }
    out
}

/// Each of `changes` as a summary line and hunks, as `ned undo` prints them
/// (spec §1.2), with paths relative to `cwd` where they're inside it.
pub fn file_changes(changes: &[FileChange], cwd: &Path, context: usize, style: Style) -> String {
    let mut out = String::new();
    for change in changes {
        let path = change.path.strip_prefix(cwd).unwrap_or(&change.path);
        let path = path.to_string_lossy();
        let before = change.before.as_deref().unwrap_or_default();
        let after = change.after.as_deref().unwrap_or_default();
        let stat = DiffStat::between(before, after);
        let summary = match (&change.before, &change.after) {
            (None, _) => diff::created_summary(&path, stat, false),
            (_, None) => diff::removed_summary(&path, stat),
            _ => diff::summary(&path, diff::regions(before, after), stat, false),
        };
        out.push_str(&style.paint(Role::Header, &summary));
        out.push('\n');
        let lang = Language::detect(&path, after);
        let (before, after) = (diff::Side::new(before, lang), diff::Side::new(after, lang));
        out.push_str(&diff::hunks(&before, &after, context, style));
    }
    out
}

/// A made commit, and the repository it's in.
pub struct Committed {
    pub repo: Repo,
    pub prepared: Prepared,
}

impl Committed {
    /// `commit SHA: SUBJECT`, the line printed after the summaries.
    pub fn line(&self, message: &str) -> String {
        let subject = message.lines().next().unwrap_or_default();
        let name = self
            .repo
            .short(&self.prepared.commit)
            .unwrap_or_else(|_| self.prepared.commit.clone());
        format!("commit {name}: {subject}")
    }
}

/// Makes the commit of `prior`, a session's earlier changes, and then
/// `changes` (with their `finals`), and moves `HEAD` to it (spec §1.3), if
/// `HEAD` hasn't moved since `before`, the repository found as the run
/// started. Paths in `changes` are relative to `cwd`; the repository is
/// found from the first, or from `top` without one. If staging fails, `HEAD`
/// is moved back, and a failure to do that goes to `messages`.
#[allow(clippy::too_many_arguments)]
pub fn commit(
    top: &Path,
    before: Option<Repo>,
    cwd: &Path,
    prior: &[(u64, FileChange)],
    changes: &[Change],
    finals: &[&str],
    message: &str,
    messages: &mut Vec<String>,
) -> Result<Committed, GitError> {
    let paths: Vec<PathBuf> = changes.iter().map(|c| cwd.join(&c.path)).collect();
    let earlier = prior.iter().map(|(_, change)| FileEdit {
        path: &change.path,
        before: change.before.as_deref(),
        after: change.after.as_deref(),
    });
    let edits: Vec<FileEdit> = earlier
        .chain(
            changes
                .iter()
                .zip(finals)
                .zip(&paths)
                .map(|((change, after), path)| FileEdit {
                    path,
                    before: (!change.created).then_some(change.old.as_str()),
                    after: Some(after),
                }),
        )
        .collect();
    let found = Repo::discover(paths.first().map_or(top, PathBuf::as_path))?;
    let repo = before.filter(|b| b.top == found.top).unwrap_or(found);
    if changes.is_empty() {
        return Err(GitError::NothingToCommit);
    }
    let mut prepared = repo.prepare(&edits, message)?;
    repo.advance(&prepared)?;
    if let Err(err) = repo.stage(&mut prepared) {
        if let Err(git) = repo.retreat(&prepared) {
            messages.push(format!("error: {git}"));
        }
        return Err(err);
    }
    Ok(Committed { repo, prepared })
}

/// The message for a failed commit, naming the session entry whose edit it
/// concerns, if `prior` (§1.3) holds that edit.
pub fn commit_error(err: &GitError, prior: &[(u64, FileChange)]) -> String {
    let (problem, edit) = match err {
        GitError::Ignored { path, edit } => (format!("{path} is ignored by git"), edit),
        GitError::Outside { path, top, edit } => {
            (format!("{path} isn't in the repository at {top}"), edit)
        }
        _ => return format!("error: {err}"),
    };
    match prior.get(*edit) {
        Some((id, _)) => format!(
            "error: {problem}, and session entry {id} edited it, so the session's edits can't be committed; start a new session (-s NAME) to commit only the edits from then on"
        ),
        None => format!("error: {err}"),
    }
}
