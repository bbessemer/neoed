//! What follows a script's execution, for every frontend: formatting, checking
//! the edits with language servers, committing them, and rendering the result
//! (command-language spec §6.3-§6.5, §1.3).

use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::buffer::Buffer;
use crate::config::{self, Config};
use crate::diff::{self, DiffStat};
use crate::exec::Change;
use crate::format::{self, Failure, Outcome};
use crate::git::{FileEdit, GitError, GitErrorKind, Prepared, Repo};
use crate::hint::{self, Errors, Fix, Hint, Note};
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
    /// `--force`: introduced diagnostics and formatter failures don't block.
    pub force: bool,
}

/// The finished edits: what formatting did to each changed file, and what
/// checking found, if it ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    pub outcomes: Vec<Outcome>,
    pub checked: Option<Checked>,
}

/// Why edits are rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
enum Rejection {
    #[error("{path}: edit makes {formatter} fail: {why}")]
    BreaksFormatter {
        path: String,
        formatter: String,
        why: String,
    },
    /// Counts of the blocking diagnostics, such as `2 errors and 1 warning`.
    #[error("edit introduces {0}")]
    Introduces(String),
}

impl Hint for Rejection {
    fn exit_code(&self) -> u8 {
        1
    }

    fn fix(&self) -> Option<Fix> {
        match self {
            Rejection::BreaksFormatter { .. } => {
                Some("fix it, or use {--force} to apply anyway".into())
            }
            Rejection::Introduces(_) => None,
        }
    }
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
/// daemon's) is given, and checks them with it, adding to `notes` as it goes.
/// Edits that make a formatter fail, or blocked by the diagnostics they
/// introduce, are rejected, after the servers are sent the original texts
/// back.
pub fn finish(
    changes: &[Change],
    allow: Option<Severity>,
    settings: Settings,
    mut lsp: Option<&mut (dyn Lsp + '_)>,
    notes: &mut Vec<Note>,
) -> Result<Finished, Errors> {
    let mut outcomes = match settings.format {
        false => vec![Outcome::Unchanged; changes.len()],
        true => Config::new(config::user_config().as_deref())
            .and_then(|mut config| format::run(changes, &mut config))?,
    };
    if let Some(lsp) = lsp.as_deref_mut() {
        format::fallback(changes, &mut outcomes, lsp);
    }
    let mut introduced: Option<Errors> = None;
    for (change, outcome) in changes.iter().zip(&outcomes) {
        match outcome {
            Outcome::NotFound(note) => notes.push(note.clone()),
            Outcome::Skipped(note) => notes.push(note.clone().into()),
            Outcome::Failed(failure) if !settings.force && introduces(change, failure) => {
                let rejection = hint::Error::new(Rejection::BreaksFormatter {
                    path: change.path.clone(),
                    formatter: failure.name.clone(),
                    why: failure.why.clone(),
                });
                match &mut introduced {
                    Some(errors) => errors.push(rejection),
                    None => introduced = Some(rejection.into()),
                }
            }
            Outcome::Failed(failure) => notes.push(failure.note().into()),
            Outcome::Formatted { name, text } if !change.created => {
                let outside = diff::outside(&change.old, &change.new, text);
                if !outside.is_empty() {
                    notes.push(
                        format!(
                            "{name} also changed {} outside the edit, at {}",
                            change.path,
                            line_list(&outside)
                        )
                        .into(),
                    );
                }
            }
            Outcome::Unchanged | Outcome::Formatted { .. } => {}
        }
    }
    if let Some(errors) = introduced {
        if let Some(lsp) = lsp {
            restore(lsp, changes, notes);
        }
        return Err(errors);
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
        Err(failure) => {
            let paths: Vec<&str> = changes.iter().map(|c| c.path.as_str()).collect();
            notes.push(failure.note(&format!("skipped checking {}", paths.join(", "))));
            return Ok(finished);
        }
    };
    if !checked.blocking.is_empty() {
        let error = blocked(changes, &finals, &checked);
        restore(lsp, changes, notes);
        return Err(error.into());
    }
    finished.checked = Some(checked);
    Ok(finished)
}

/// `line 3`, or `lines 3, 7-9`, for ranges of line numbers; at most five.
fn line_list(ranges: &[Range<usize>]) -> String {
    let mut list: Vec<String> = ranges
        .iter()
        .take(5)
        .map(|r| match r.len() {
            0 | 1 => r.start.to_string(),
            _ => format!("{}-{}", r.start, r.end - 1),
        })
        .collect();
    if ranges.len() > 5 {
        list.push("...".into());
    }
    match ranges {
        [r] if r.len() <= 1 => format!("line {}", list[0]),
        _ => format!("lines {}", list.join(", ")),
    }
}

/// Whether `change` introduced `failure`: its formatter passes the text the
/// change replaced.
fn introduces(change: &Change, failure: &Failure) -> bool {
    !change.created
        && !matches!(
            failure.formatter.format(&change.path, &change.old),
            Outcome::Failed(_)
        )
}

/// Sends the servers the original texts of `changes` back, after an edit that
/// wasn't written, adding a note to `notes` if that fails.
pub fn restore(lsp: &mut dyn Lsp, changes: &[Change], notes: &mut Vec<Note>) {
    if let Err(failure) = lsp::restore(lsp, changes) {
        notes.push(failure.into());
    }
}

/// Starts checking a write with the checks servers run on save (spec §6.5),
/// before the files are written; `None`, with a note in `notes`, if that
/// failed.
pub fn before_save(
    lsp: &mut dyn Lsp,
    changes: &[Change],
    notes: &mut Vec<Note>,
) -> Option<lsp::BeforeSave> {
    match lsp::before_save(lsp, changes) {
        Ok(before) => Some(before),
        Err(failure) => {
            notes.push(skipped_on_save(failure, changes));
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
    notes: &mut Vec<Note>,
) -> String {
    let finals = finished.finals(changes);
    let saved = match lsp::check_saved(lsp, before, changes, &finals, finished.checked.as_ref()) {
        Ok(saved) => saved,
        Err(failure) => {
            notes.push(skipped_on_save(failure, changes));
            return String::new();
        }
    };
    notes.extend(saved.notes.into_iter().map(Note::from));
    let mut out = String::new();
    for ((change, text), found) in changes.iter().zip(&finals).zip(&saved.files) {
        out.push_str(&diagnostics(&change.path, text, found, style));
    }
    out
}

/// The note for checks on save that failed.
fn skipped_on_save(failure: LspFailure, changes: &[Change]) -> Note {
    let paths: Vec<&str> = changes.iter().map(|c| c.path.as_str()).collect();
    failure.note(&format!(
        "skipped the checks run on save of {}",
        paths.join(", ")
    ))
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
fn blocked(changes: &[Change], finals: &[&str], checked: &Checked) -> hint::Error<Rejection> {
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
    let listing: String = checked
        .blocking
        .iter()
        .map(|(i, d)| lsp::render(&changes[*i].path, &Buffer::new(finals[*i]), d, Style::Plain))
        .collect();
    let fix = format!("fix {them}, or add `allow {allow}` to the script to apply it anyway");
    hint::Error::new(Rejection::Introduces(counts.join(" and ")))
        .with_fix(Fix::new(fix).then(listing))
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
/// is moved back, and a failure to do that follows the error.
#[allow(clippy::too_many_arguments)]
pub fn commit(
    top: &Path,
    before: Option<Repo>,
    cwd: &Path,
    prior: &[(u64, FileChange)],
    changes: &[Change],
    finals: &[&str],
    message: &str,
) -> Result<Committed, Errors> {
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
    if edits.is_empty() {
        return Err(GitError::from(GitErrorKind::NothingToCommit).into());
    }
    let mut prepared = repo
        .prepare(&edits, message)
        .map_err(|err| in_session(err, prior))?;
    repo.advance(&prepared)?;
    if let Err(err) = repo.stage(&mut prepared) {
        let mut errors = Errors::from(err);
        if let Err(git) = repo.retreat(&prepared) {
            errors.push(git);
        }
        return Err(errors);
    }
    Ok(Committed { repo, prepared })
}

/// `err`, with the fix for an edit git can't commit that `prior` (§1.3), a
/// session's earlier changes, holds.
fn in_session(err: GitError, prior: &[(u64, FileChange)]) -> GitError {
    let (GitErrorKind::Ignored { edit, .. } | GitErrorKind::Outside { edit, .. }) = err.kind else {
        return err;
    };
    match prior.get(edit) {
        Some((id, _)) => err.with_fix(format!(
            "session entry {id} edited it, so start a new session ({{-s NAME}}) to commit only the edits from then on"
        )),
        None => err,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::hint::Frontend;

    /// A formatter that fails on text holding `bad`, and passes the rest.
    const FAILS_ON_BAD: &str = "[format]\nrust = [\"sh\", \"-c\", \"input=$(cat); case $input in *bad*) echo 'bad input' >&2; exit 1;; esac; printf '%s\\\\n' \\\"$input\\\"\"]\n";

    /// Finishes a change of `a.rs` from `old` to `new` (`None`: `create`
    /// makes it), formatted by `FAILS_ON_BAD`.
    fn finish_change(
        old: Option<&str>,
        new: &str,
        force: bool,
    ) -> (Result<Finished, (u8, String)>, Vec<String>) {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".ned.toml"), FAILS_ON_BAD).unwrap();
        let changes = [Change {
            path: dir.path().join("a.rs").to_str().unwrap().into(),
            old: old.unwrap_or_default().into(),
            new: new.into(),
            edits: 1,
            lang: Some(Language::Rust),
            created: old.is_none(),
        }];
        let settings = Settings {
            format: true,
            check: false,
            force,
        };
        let mut notes = Vec::new();
        let finished = finish(&changes, None, settings, None, &mut notes);
        let dir = dir.path().to_str().unwrap();
        let unrooted = |s: &str| s.replace(dir, "DIR");
        let notes = notes
            .iter()
            .map(|n| unrooted(&n.render(Frontend::Cli)))
            .collect();
        let finished = finished.map_err(|errors| {
            let rendered = errors.render(Frontend::Cli, None);
            (errors.exit_code(), unrooted(&rendered))
        });
        (finished, notes)
    }

    const NOTE: &str = "note: sh failed: bad input; skipped formatting DIR/a.rs";

    #[test]
    fn a_formatter_failure_the_edit_introduced_is_rejected() {
        let (finished, messages) = finish_change(Some("fn f() {}\n"), "fn bad() {}\n", false);
        assert_eq!(
            finished,
            Err((
                1,
                "error: DIR/a.rs: edit makes sh fail: bad input; fix it, or use --force to apply anyway".into(),
            ))
        );
        assert!(messages.is_empty(), "{messages:?}");
    }

    #[test]
    fn a_formatter_failure_on_the_old_text_too_is_a_note() {
        let (finished, messages) = finish_change(Some("fn bad() {}\n"), "fn bad() { x }\n", false);
        let finished = finished.unwrap();
        assert!(
            matches!(finished.outcomes[..], [Outcome::Failed(_)]),
            "{finished:?}"
        );
        assert_eq!(messages, [NOTE]);
    }

    #[test]
    fn force_keeps_an_introduced_formatter_failure_a_note() {
        let (finished, messages) = finish_change(Some("fn f() {}\n"), "fn bad() {}\n", true);
        assert!(finished.is_ok(), "{finished:?}");
        assert_eq!(messages, [NOTE]);
    }

    #[test]
    fn a_formatter_failure_on_a_created_file_is_a_note() {
        let (finished, messages) = finish_change(None, "fn bad() {}\n", false);
        assert!(finished.is_ok(), "{finished:?}");
        assert_eq!(messages, [NOTE]);
    }

    #[test]
    fn a_formatter_that_passes_formats() {
        let (finished, messages) = finish_change(Some("fn f() {}\n"), "fn g() {}", false);
        let finished = finished.unwrap();
        assert_eq!(
            finished.outcomes,
            [Outcome::Formatted {
                name: "sh".into(),
                text: "fn g() {}\n".into()
            }]
        );
        assert!(messages.is_empty(), "{messages:?}");
    }

    #[test]
    fn blocking_diagnostics_are_listed_as_the_server_wrote_them() {
        let change = Change {
            path: "a.rs".into(),
            old: String::new(),
            new: "fn a() {}\n".into(),
            edits: 1,
            lang: Some(Language::Rust),
            created: false,
        };
        let diagnostic = Diagnostic {
            start: lsp::Position {
                line: 0,
                character: 3,
            },
            end: lsp::Position {
                line: 0,
                character: 4,
            },
            severity: Severity::Error,
            message: "expected `{-w}`".into(),
            source: None,
            code: None,
        };
        let checked = Checked {
            files: vec![vec![diagnostic.clone()]],
            blocking: vec![(0, diagnostic)],
        };
        let message = blocked(&[change], &["fn a() {}\n"], &checked).render(Frontend::Cli, None);
        assert!(
            message.starts_with(
                "error: edit introduces 1 error; fix it, or add `allow errors` to the script to apply it anyway\n"
            ),
            "{message}"
        );
        assert!(message.contains("expected `{-w}`"), "{message}");
    }
}
