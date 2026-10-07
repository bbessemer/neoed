//! Sessions: per-workspace logs of `ned` invocations (spec §1.2).

use std::collections::{BTreeMap, HashSet};
use std::env;

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use std::time::{SystemTime, UNIX_EPOCH};

use crate::diff;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The log format version, in the header's `ned_session` field.
pub const FORMAT: u64 = 1;

/// One recorded invocation: a script, an undo, or a REPL's write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Assigned by [`Log::append`], numbering entries from 1.
    pub id: u64,
    /// Seconds since the Unix epoch.
    pub time: u64,
    pub cwd: PathBuf,
    pub files: Vec<String>,
    pub workspace: Option<PathBuf>,
    /// `None` for an undo or a write.
    pub script: Option<String>,
    pub undoes: Option<u64>,
    /// A REPL's write of its buffers (spec §1.4).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub write: bool,
    pub dry_run: bool,
    pub exit: u8,
    pub error: Option<String>,
    pub changes: Vec<FileChange>,
    /// The commit that `--commit` made (spec §1.3).
    #[serde(default)]
    pub commit: Option<String>,
    /// What the call was for, from the MCP server's `comment` (spec §1.5).
    #[serde(default)]
    pub comment: Option<String>,
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
    let absolute = |var| {
        env::var_os(var)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    let state = absolute("XDG_STATE_HOME")
        .or_else(|| Some(absolute("HOME")?.join(".local/state")))
        .ok_or(SessionError::NoStateDir)?;
    Ok(state.join("ned"))
}

/// The names of the sessions with a log for the workspace at `root`, sorted.
pub fn sessions(state_dir: &Path, root: &Path) -> Result<Vec<String>, SessionError> {
    names_in(&workspace_dir(state_dir, root))
}

/// The names of the sessions with a log in `dir`, a workspace's directory,
/// sorted.
fn names_in(dir: &Path) -> Result<Vec<String>, SessionError> {
    let io_error = |source| SessionError::Io {
        path: dir.to_path_buf(),
        source,
    };
    let read = match fs::read_dir(dir) {
        Ok(read) => read,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(io_error(err)),
    };
    let mut names = Vec::new();
    for item in read {
        let name = item.map_err(io_error)?.file_name();
        if let Some(name) = name.to_str().and_then(|name| name.strip_suffix(".log")) {
            names.push(name.to_string());
        }
    }
    names.sort();
    Ok(names)
}

/// Every workspace with sessions, by path, with its sessions' names, sorted.
/// A workspace's path is read from its logs' headers, or is its directory if
/// none can be read.
pub fn workspaces(state_dir: &Path) -> Result<Vec<(PathBuf, Vec<String>)>, SessionError> {
    let sessions = state_dir.join("sessions");
    let io_error = |source| SessionError::Io {
        path: sessions.clone(),
        source,
    };
    let read = match fs::read_dir(&sessions) {
        Ok(read) => read,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(io_error(err)),
    };
    let mut workspaces = Vec::new();
    for item in read {
        let dir = item.map_err(io_error)?.path();
        let names = names_in(&dir)?;
        if names.is_empty() {
            continue;
        }
        let root = names
            .iter()
            .find_map(|name| log_workspace(&dir.join(format!("{name}.log"))))
            .unwrap_or(dir);
        workspaces.push((root, names));
    }
    workspaces.sort();
    Ok(workspaces)
}

/// The workspace named in the header of the log at `path`, if it can be read.
fn log_workspace(path: &Path) -> Option<PathBuf> {
    let mut header = String::new();
    BufReader::new(File::open(path).ok()?)
        .read_line(&mut header)
        .ok()?;
    serde_json::from_str::<Header>(&header)
        .ok()
        .map(|header| header.workspace)
}

/// The first of `PREFIX-1`, `PREFIX-2`, ... that the workspace at `root` has
/// no log for, a new session, whose log it creates so no other caller takes
/// the same name.
pub fn next_free(state_dir: &Path, root: &Path, prefix: &str) -> Result<Session, SessionError> {
    for n in 1.. {
        let session = Session::new(state_dir, root, &format!("{prefix}-{n}"))?;
        if session.lock()?.create()? {
            return Ok(session);
        }
    }
    unreachable!("there are always more names")
}

#[derive(Debug, PartialEq, Eq, Error)]
pub enum RepeatError {
    #[error("`!!` repeats the session's last script, but it has none; write the script out")]
    NoScript,
    #[error(
        "`!!` would apply entry {0}, a dry run; send the script again to apply it, or add -n to preview it again"
    )]
    DryRun(u64),
    /// `hint` is empty, or `; ` and a fix.
    #[error("`{old}` isn't in the last script{hint}; the script is:\n{script}")]
    NotFound {
        old: String,
        script: String,
        hint: String,
    },
    /// Text after a `:s` modifier that doesn't start another.
    #[error(
        "`{rest}` follows the `!!` modifiers; NEW ends at its first unescaped `{delimiter}`: write `\\{delimiter}` for a literal one, or use another delimiter"
    )]
    Trailing { rest: String, delimiter: char },
    #[error("malformed `!!` modifier `{0}`; usage: !![:s/OLD/NEW/][:gs/OLD/NEW/]...")]
    Malformed(String),
}

/// Whether `src` is a `!!` script, which [`repeat`] expands.
pub fn is_repeat(src: &str) -> bool {
    src.trim_start().starts_with("!!")
}

/// If `src` is a `!!` script, the last script entry of `entries` that edits or
/// failed (else the last script entry) and its script with the modifiers
/// applied (spec §1.2); `None` for another script.
/// Repeating a dry run is an error unless `dry_run`, as flags aren't repeated.
pub fn repeat<'a>(
    src: &str,
    entries: &'a [Entry],
    dry_run: bool,
) -> Option<Result<(&'a Entry, String), RepeatError>> {
    let mut rest = src.trim().strip_prefix("!!")?;
    let scripts = || entries.iter().rev().filter(|entry| entry.script.is_some());
    let Some(entry) = scripts()
        .find(|entry| edits_or_failed(entry))
        .or_else(|| scripts().next())
    else {
        return Some(Err(RepeatError::NoScript));
    };
    if entry.dry_run && !dry_run {
        return Some(Err(RepeatError::DryRun(entry.id)));
    }
    let mut script = entry.script.clone().unwrap_or_default();
    // The delimiter of the modifier before, if any.
    let mut previous = None;
    while !rest.is_empty() {
        let modifier = rest;
        let malformed = || RepeatError::Malformed(modifier.to_string());
        let Some(body) = modifier.strip_prefix(':') else {
            return Some(Err(match previous {
                Some(delimiter) => RepeatError::Trailing {
                    rest: modifier.to_string(),
                    delimiter,
                },
                None => malformed(),
            }));
        };
        let (global, body) = match body.strip_prefix("gs") {
            Some(body) => (true, body),
            None => match body.strip_prefix('s') {
                Some(body) => (false, body),
                None => return Some(Err(malformed())),
            },
        };
        let mut chars = body.chars();
        let Some(delimiter) = chars.next().filter(char::is_ascii_punctuation) else {
            return Some(Err(malformed()));
        };
        let (old, after, closed) = field(chars.as_str(), delimiter);
        if !closed || old.is_empty() {
            return Some(Err(malformed()));
        }
        let (new, after, _) = field(after, delimiter);
        if !script.contains(&old) {
            let hint = not_found_hint(&old, &script, delimiter);
            return Some(Err(RepeatError::NotFound { old, script, hint }));
        }
        script = match global {
            true => script.replace(&old, &new),
            false => script.replacen(&old, &new, 1),
        };
        rest = after;
        previous = Some(delimiter);
    }
    Some(Ok((entry, script)))
}

/// The fix for a `!!` modifier whose `old`, read with `delimiter`, isn't in
/// `script`: when the script has it with `\` before each delimiter, which
/// the modifier read as escapes, another delimiter.
fn not_found_hint(old: &str, script: &str, delimiter: char) -> String {
    let escaped = old.replace(delimiter, &format!("\\{delimiter}"));
    if escaped == old || !script.contains(&escaped) {
        return "; OLD must match its text exactly, spacing and escapes included".into();
    }
    let other = ['|', '#', ',', '@', '%']
        .into_iter()
        .find(|&c| c != delimiter && !escaped.contains(c))
        .unwrap_or('#');
    format!(
        "; it has `{escaped}`, and `\\{delimiter}` in a modifier stands for `{delimiter}`: \
         use another delimiter, as in !!:s{other}{escaped}{other}NEW{other}"
    )
}

/// Whether `entry`'s script failed or has a command that isn't a read, so `!!`
/// doesn't pass over it; a script that no longer parses counts as an edit.
fn edits_or_failed(entry: &Entry) -> bool {
    use crate::script::ast::CommandKind;
    let reads = |script: crate::script::Script| {
        script.commands.iter().all(|command| {
            matches!(
                command.kind,
                CommandKind::Show { .. }
                    | CommandKind::Outline(_)
                    | CommandKind::Check { .. }
                    | CommandKind::File(_)
                    | CommandKind::Allow(_)
            )
        })
    };
    let script = entry.script.as_deref().unwrap_or_default();
    entry.exit != 0 || entry.error.is_some() || !crate::script::parse(script).is_ok_and(reads)
}

/// Reads a `!!` modifier's field up to `delimiter`, which `\` makes literal:
/// the field, the text after it, and whether the delimiter ended it.
fn field(text: &str, delimiter: char) -> (String, &str, bool) {
    let mut out = String::new();
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '\\' && chars.peek().is_some_and(|&(_, next)| next == delimiter) {
            out.push(delimiter);
            chars.next();
        } else if c == delimiter {
            return (out, &text[i + c.len_utf8()..], true);
        } else {
            out.push(c);
        }
    }
    (out, "", false)
}

/// Seconds since the Unix epoch, for an entry's `time`.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_secs())
}

/// A script's first line, followed by `(+N lines)` if it has more.
pub fn script_summary(script: &str) -> String {
    let mut lines = script.lines();
    let first = lines.next().unwrap_or_default();
    match lines.count() {
        0 => first.to_string(),
        1 => format!("{first} (+1 line)"),
        more => format!("{first} (+{more} lines)"),
    }
}

/// `ned history`'s output: the last 10 entries, or every one with `all`, a
/// line each (spec §1.2).
pub fn history(entries: &[Entry], all: bool) -> String {
    let undone: HashSet<u64> = entries.iter().filter_map(|entry| entry.undoes).collect();
    let shown = match all {
        true => entries,
        false => &entries[entries.len().saturating_sub(10)..],
    };
    let mut out = String::new();
    for entry in shown {
        let mut parts = vec![match (entry.undoes, entry.exit, entry.dry_run) {
            _ if entry.write => "write".to_string(),
            (Some(id), _, _) => format!("undo {id}"),
            (None, 0, true) => "dry run".to_string(),
            (None, 0, false) => "ok".to_string(),
            (None, exit, _) => format!("exit {exit}"),
        }];
        match entry.changes.len() {
            0 => {}
            1 => parts.push("1 file".to_string()),
            n => parts.push(format!("{n} files")),
        }
        if let Some(commit) = &entry.commit {
            parts.push(format!("commit {}", &commit[..commit.len().min(7)]));
        }
        if undone.contains(&entry.id) {
            parts.push("undone".to_string());
        }
        out.push_str(&format!("{} {}", entry.id, parts.join(", ")));
        if let Some(script) = &entry.script {
            out.push_str(&format!(": {}", script_summary(script)));
        }
        out.push('\n');
    }
    out
}

/// The changes of the entries after the last one that made a commit, in
/// order, each with its entry's id: what a session's `--commit` commits
/// (spec §1.3). `read` gives a file's current text, `None` if it's missing;
/// a file that no longer holds what the session last wrote to it is an
/// error.
pub fn uncommitted(
    entries: &[Entry],
    mut read: impl FnMut(&Path) -> io::Result<Option<String>>,
) -> Result<Vec<(u64, FileChange)>, UncommittedError> {
    let start = entries
        .iter()
        .rposition(|entry| entry.commit.is_some())
        .map_or(0, |i| i + 1);
    let changes: Vec<(u64, FileChange)> = entries[start..]
        .iter()
        .flat_map(|entry| entry.changes.iter().map(|c| (entry.id, c.clone())))
        .collect();
    let last: BTreeMap<&Path, (u64, &Option<String>)> = changes
        .iter()
        .map(|(id, change)| (change.path.as_path(), (*id, &change.after)))
        .collect();
    for (path, (id, after)) in last {
        let current = read(path).map_err(|source| UncommittedError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if current != *after {
            let path = path.to_path_buf();
            return Err(UncommittedError::Changed { path, id });
        }
    }
    Ok(changes)
}

#[derive(Debug, Error)]
pub enum UncommittedError {
    #[error("{}: {source}; check its permissions, then rerun", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error(
        "{} changed since session entry {id} wrote it, so the session's edits can't be committed; put back what entry {id} wrote, or start a new session (-s NAME) to commit only the edits from then on",
        path.display()
    )]
    Changed { path: PathBuf, id: u64 },
}

/// What `ned undo` writes: the files of entry `id`, restored. Each change's
/// `before` is the file's current text, and `after` what the undo leaves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Undo {
    pub id: u64,
    pub script: Option<String>,
    pub changes: Vec<FileChange>,
}

#[derive(Debug, Error)]
pub enum UndoError {
    #[error("nothing to undo; `ned history` lists the session's entries")]
    Nothing,
    #[error("{}: {source}; check its permissions, then run `ned undo` again", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error(
        "{} changed since entry {id} wrote it; use --force to merge the undo into its current text",
        path.display()
    )]
    Changed { path: PathBuf, id: u64 },
    #[error(
        "{} was removed since entry {id} wrote it, so it can't be undone; restore it by hand (`ned history` lists the entries)",
        path.display()
    )]
    Removed { path: PathBuf, id: u64 },
    #[error(
        "{}:{line}: undoing entry {id} conflicts with a later change; edit the file by hand",
        path.display()
    )]
    Conflict { path: PathBuf, line: usize, id: u64 },
}

impl UndoError {
    /// The error with its path relative to `dir`, if it's inside it.
    pub fn relative_to(self, dir: &Path) -> UndoError {
        let relative = |path: PathBuf| {
            path.strip_prefix(dir)
                .map(Path::to_path_buf)
                .unwrap_or(path)
        };
        match self {
            UndoError::Nothing => UndoError::Nothing,
            UndoError::Io { path, source } => UndoError::Io {
                path: relative(path),
                source,
            },
            UndoError::Changed { path, id } => UndoError::Changed {
                path: relative(path),
                id,
            },
            UndoError::Removed { path, id } => UndoError::Removed {
                path: relative(path),
                id,
            },
            UndoError::Conflict { path, line, id } => UndoError::Conflict {
                path: relative(path),
                line,
                id,
            },
        }
    }
}

/// Plans undoing the last entry of `entries` that wrote files and isn't
/// undone. `read` gives a file's current text, `None` if it's missing. A file
/// that no longer holds what the entry wrote is an error, unless `force`
/// merges the undo into it.
pub fn undo(
    entries: &[Entry],
    mut read: impl FnMut(&Path) -> io::Result<Option<String>>,
    force: bool,
) -> Result<Undo, UndoError> {
    let undone: HashSet<u64> = entries.iter().filter_map(|entry| entry.undoes).collect();
    let target = entries
        .iter()
        .rev()
        .find(|e| e.undoes.is_none() && !e.changes.is_empty() && !undone.contains(&e.id))
        .ok_or(UndoError::Nothing)?;
    let id = target.id;
    let mut changes = Vec::new();
    for change in &target.changes {
        let path = change.path.clone();
        let current = read(&path).map_err(|source| UndoError::Io {
            path: path.clone(),
            source,
        })?;
        let restored = if current == change.after {
            change.before.clone()
        } else if current.is_none() {
            return Err(UndoError::Removed { path, id });
        } else if !force {
            return Err(UndoError::Changed { path, id });
        } else {
            let merged = diff::merge(
                change.after.as_deref().unwrap_or_default(),
                current.as_deref().unwrap_or_default(),
                change.before.as_deref().unwrap_or_default(),
            )
            .map_err(|line| UndoError::Conflict {
                path: path.clone(),
                line,
                id,
            })?;
            (change.before.is_some() || !merged.is_empty()).then_some(merged)
        };
        if restored != current {
            changes.push(FileChange {
                path,
                before: current,
                after: restored,
            });
        }
    }
    Ok(Undo {
        id,
        script: target.script.clone(),
        changes,
    })
}

/// The directory of `root`'s sessions: its last component and a hash of its
/// path, so it's recognizable and unique.
fn workspace_dir(state_dir: &Path, root: &Path) -> PathBuf {
    // FNV-1a: stable across Rust releases, unlike `DefaultHasher`.
    let hash = root
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
        });
    let base = root
        .file_name()
        .map_or("root".into(), |name| name.to_string_lossy());
    state_dir
        .join("sessions")
        .join(format!("{base}-{hash:016x}"))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn private_dir(dir: &Path) -> Result<(), SessionError> {
    #[cfg(unix)]
    crate::fs::private_dir(dir)?;
    #[cfg(not(unix))]
    fs::create_dir_all(dir).map_err(|source| SessionError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    Ok(())
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
        if !valid_name(name) {
            return Err(SessionError::BadName(name.to_string()));
        }
        if let Some(parent) = state_dir.parent() {
            fs::create_dir_all(parent).map_err(|source| SessionError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let dir = workspace_dir(state_dir, root);
        for dir in [state_dir, &state_dir.join("sessions"), &dir] {
            private_dir(dir)?;
        }
        Ok(Session {
            name: name.to_string(),
            root: root.to_path_buf(),
            log: dir.join(format!("{name}.log")),
            lock: dir.join(format!("{name}.lock")),
        })
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
        let io_error = |source| SessionError::Io {
            path: self.lock.clone(),
            source,
        };
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.lock)
            .map_err(io_error)?;
        file.lock().map_err(io_error)?;
        Ok(Log {
            session: self,
            _lock: file,
        })
    }

    /// Deletes the session's log and lock once it holds the lock, and the
    /// workspace's directory if no other session is left in it.
    pub fn delete(self) -> Result<(), SessionError> {
        let held = self.lock()?;
        for path in [&self.log, &self.lock] {
            fs::remove_file(path).map_err(|source| SessionError::Io {
                path: path.clone(),
                source,
            })?;
        }
        drop(held);
        if let Some(dir) = self.log.parent() {
            // Fails, as it should, while another session is left in it.
            let _ = fs::remove_dir(dir);
        }
        Ok(())
    }
}

/// A locked session's log.
#[derive(Debug)]
pub struct Log<'a> {
    session: &'a Session,
    _lock: File,
}

/// The offset of the last `\n` in `file` before offset `before`.
fn newline_before(file: &mut File, before: u64) -> io::Result<Option<u64>> {
    let mut end = before;
    let mut chunk = vec![0; 8192];
    while end > 0 {
        let start = end.saturating_sub(chunk.len() as u64);
        let chunk = &mut chunk[..(end - start) as usize];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(chunk)?;
        if let Some(i) = chunk.iter().rposition(|&byte| byte == b'\n') {
            return Ok(Some(start + i as u64));
        }
        end = start;
    }
    Ok(None)
}

impl Log<'_> {
    /// Creates the log with just its header, unless it exists: whether it did.
    fn create(&self) -> Result<bool, SessionError> {
        let path = &self.session.log;
        let io_error = |source| SessionError::Io {
            path: path.clone(),
            source,
        };
        let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
            Err(err) => return Err(io_error(err)),
        };
        file.write_all(self.header().as_bytes()).map_err(io_error)?;
        Ok(true)
    }

    /// The log's first line.
    fn header(&self) -> String {
        let header = Header {
            ned_session: FORMAT,
            workspace: self.session.root.clone(),
        };
        serde_json::to_string(&header).expect("a header serializes") + "\n"
    }

    /// Every entry, oldest first; none if nothing has been recorded.
    pub fn entries(&self) -> Result<Vec<Entry>, SessionError> {
        let Some(text) = self.read()? else {
            return Ok(Vec::new());
        };
        parse(&self.session.log, complete(&text), 0)
    }

    /// Appends `entry` with the id after the last entry's, which it returns.
    /// It reads only the log's header and the start of its last line, as
    /// entries hold whole files.
    pub fn append(&mut self, entry: Entry) -> Result<u64, SessionError> {
        let path = &self.session.log;
        let io_error = |source| SessionError::Io {
            path: path.clone(),
            source,
        };
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(io_error)?;
        let len = file.metadata().map_err(io_error)?.len();
        let end = newline_before(&mut file, len)
            .map_err(io_error)?
            .map_or(0, |i| i + 1);
        file.set_len(end).map_err(io_error)?;

        let mut record = String::new();
        let last = if end == 0 {
            record.push_str(&self.header());
            0
        } else {
            let mut header = String::new();
            file.seek(SeekFrom::Start(0)).map_err(io_error)?;
            BufReader::new(&file)
                .read_line(&mut header)
                .map_err(io_error)?;
            check_header(&self.session.log, header.trim_end())?;
            match self.last_id(&mut file, end).map_err(io_error)? {
                Some(id) => id,
                // Not an entry: `entries` says where the log is malformed.
                None => self.entries()?.last().map_or(0, |entry| entry.id),
            }
        };
        let id = last + 1;
        let entry = Entry { id, ..entry };
        record.push_str(&serde_json::to_string(&entry).expect("an entry serializes"));
        record.push('\n');
        file.seek(SeekFrom::End(0))
            .and_then(|_| file.write_all(record.as_bytes()))
            .map_err(io_error)?;
        Ok(id)
    }

    /// The id of the last entry in the first `end` bytes of the log: 0 if
    /// there's only the header, `None` if the line doesn't start like an entry.
    fn last_id(&self, file: &mut File, end: u64) -> io::Result<Option<u64>> {
        let start = newline_before(file, end - 1)?.map_or(0, |i| i + 1);
        if start == 0 {
            return Ok(Some(0));
        }
        let mut prefix = Vec::new();
        file.seek(SeekFrom::Start(start))?;
        file.take(32).read_to_end(&mut prefix)?;
        // `id` is the first field an entry serializes.
        let digits = String::from_utf8_lossy(&prefix)
            .strip_prefix("{\"id\":")
            .map(|rest| {
                rest.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
            });
        Ok(digits.and_then(|digits| digits.parse().ok()))
    }

    /// The log's text, or `None` if nothing has been recorded.
    fn read(&self) -> Result<Option<String>, SessionError> {
        let path = &self.session.log;
        match fs::read_to_string(path) {
            Ok(text) => Ok(Some(text)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(SessionError::Io {
                path: path.clone(),
                source,
            }),
        }
    }

    /// Every entry, as [`Log::entries`] reads them, and a [`Follower`] that reads
    /// the entries appended after them.
    pub fn follow(&self) -> Result<(Vec<Entry>, Follower), SessionError> {
        let text = self.read()?.unwrap_or_default();
        let text = complete(&text);
        let entries = parse(&self.session.log, text, 0)?;
        let follower = Follower {
            log: self.session.log.clone(),
            offset: text.len() as u64,
            line: text.lines().count(),
            file: fs::metadata(&self.session.log)
                .ok()
                .as_ref()
                .and_then(identity),
        };
        Ok((entries, follower))
    }
}

/// What tells one file from another at the same path: its device and inode.
#[cfg(unix)]
fn identity(metadata: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn identity(_: &fs::Metadata) -> Option<(u64, u64)> {
    None
}

/// Reads the entries appended to a session's log, without its lock: appends
/// write whole lines, and a line without its newline yet is left for the next
/// [`Follower::poll`].
#[derive(Debug)]
pub struct Follower {
    log: PathBuf,
    /// Where the next line starts.
    offset: u64,
    /// The index of the line at `offset`, for errors.
    line: usize,
    /// The log's device and inode when last read, to tell a log made anew.
    file: Option<(u64, u64)>,
}

impl Follower {
    /// The entries appended since the last poll, oldest first.
    pub fn poll(&mut self) -> Result<Vec<Entry>, SessionError> {
        let io_error = |source| SessionError::Io {
            path: self.log.clone(),
            source,
        };
        let mut file = match File::open(&self.log) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(io_error(err)),
        };
        let metadata = file.metadata().map_err(io_error)?;
        // A log deleted and recorded in again is read from its start.
        if metadata.len() < self.offset || identity(&metadata) != self.file {
            self.offset = 0;
            self.line = 0;
        }
        self.file = identity(&metadata);
        let mut bytes = Vec::new();
        file.seek(SeekFrom::Start(self.offset))
            .and_then(|_| file.read_to_end(&mut bytes))
            .map_err(io_error)?;
        let text = String::from_utf8_lossy(&bytes);
        let text = complete(&text);
        let entries = parse(&self.log, text, self.line)?;
        self.offset += text.len() as u64;
        self.line += text.lines().count();
        Ok(entries)
    }
}

/// `text` up to the end of its last line: a last line without its newline is
/// an append cut short, which the next append replaces, or one in progress.
fn complete(text: &str) -> &str {
    &text[..text.rfind('\n').map_or(0, |i| i + 1)]
}

/// The entries in `text`, whole lines of the log at `log` from line index
/// `first` (the header's is 0, which is checked instead).
fn parse(log: &Path, text: &str, first: usize) -> Result<Vec<Entry>, SessionError> {
    let mut entries = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let i = first + i;
        match i {
            0 => check_header(log, line)?,
            _ => entries.push(serde_json::from_str(line).map_err(|err| malformed(log, i, &err))?),
        }
    }
    Ok(entries)
}

fn check_header(log: &Path, line: &str) -> Result<(), SessionError> {
    let header: Header = serde_json::from_str(line).map_err(|err| malformed(log, 0, &err))?;
    if header.ned_session != FORMAT {
        return Err(SessionError::UnknownFormat {
            path: log.to_path_buf(),
            version: header.ned_session,
        });
    }
    Ok(())
}

/// The error for line `index` (from 0) of the log at `log`.
fn malformed(log: &Path, index: usize, err: &serde_json::Error) -> SessionError {
    SessionError::Malformed {
        path: log.to_path_buf(),
        line: index + 1,
        message: err.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use super::*;

    fn entry(script: &str) -> Entry {
        Entry {
            id: 0,
            time: 1_700_000_000,
            cwd: PathBuf::from("/src/proj"),
            files: vec!["a.rs".into()],
            workspace: None,
            script: Some(script.into()),
            undoes: None,
            write: false,
            dry_run: false,
            exit: 0,
            error: None,
            changes: vec![FileChange {
                path: PathBuf::from("/src/proj/a.rs"),
                before: Some("old\n".into()),
                after: Some("new\n".into()),
            }],
            commit: None,
            comment: None,
        }
    }

    fn session(state: &Path, root: &str, name: &str) -> Session {
        Session::new(state, Path::new(root), name).unwrap()
    }

    fn log_path(state: &Path) -> PathBuf {
        let dirs: Vec<_> = fs::read_dir(state.join("sessions")).unwrap().collect();
        assert_eq!(dirs.len(), 1);
        dirs[0].as_ref().unwrap().path().join("default.log")
    }

    #[test]
    fn appended_entries_are_numbered_from_one_and_read_back() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("ned");
        let session = session(&state, "/src/proj", "default");
        let mut log = session.lock().unwrap();
        assert_eq!(log.append(entry("show 1")).unwrap(), 1);
        assert_eq!(log.append(entry("show 2")).unwrap(), 2);
        drop(log);

        let log = session.lock().unwrap();
        let entries = log.entries().unwrap();
        let mut first = entry("show 1");
        first.id = 1;
        let mut second = entry("show 2");
        second.id = 2;
        assert_eq!(entries, vec![first, second]);
    }

    #[test]
    fn a_log_starts_with_a_versioned_header() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("ned");
        let session = session(&state, "/src/proj", "default");
        assert!(!session.exists());
        session.lock().unwrap().append(entry("show 1")).unwrap();
        assert!(session.exists());

        let text = fs::read_to_string(log_path(&state)).unwrap();
        let mut lines = text.lines();
        assert_eq!(
            lines.next(),
            Some(r#"{"ned_session":1,"workspace":"/src/proj"}"#)
        );
        let line: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(line["id"], 1);
        assert_eq!(line["script"], "show 1");
        assert_eq!(line["changes"][0]["before"], "old\n");
        assert_eq!(lines.next(), None);
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn an_unrecorded_session_has_no_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let session = session(&tmp.path().join("ned"), "/src/proj", "default");
        assert_eq!(session.lock().unwrap().entries().unwrap(), vec![]);
        assert!(!session.exists());
    }

    #[test]
    fn a_log_of_an_unknown_format_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("ned");
        let session = session(&state, "/src/proj", "default");
        session.lock().unwrap().append(entry("show 1")).unwrap();
        let path = log_path(&state);
        let text = fs::read_to_string(&path).unwrap();
        fs::write(
            &path,
            text.replace(r#""ned_session":1"#, r#""ned_session":2"#),
        )
        .unwrap();

        let log = session.lock().unwrap();
        let err = log.entries().unwrap_err();
        assert!(
            matches!(err, SessionError::UnknownFormat { version: 2, .. }),
            "{err}"
        );
        assert!(err.to_string().contains("another session name"), "{err}");
        drop(log);
        assert!(matches!(
            session.lock().unwrap().append(entry("show 2")),
            Err(SessionError::UnknownFormat { .. })
        ));
    }

    #[test]
    fn a_malformed_entry_is_an_error_naming_its_line() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("ned");
        let session = session(&state, "/src/proj", "default");
        session.lock().unwrap().append(entry("show 1")).unwrap();
        let path = log_path(&state);
        let mut text = fs::read_to_string(&path).unwrap();
        text.push_str("{\"id\":2,\n");
        fs::write(&path, text).unwrap();

        let err = session.lock().unwrap().entries().unwrap_err();
        assert!(
            matches!(err, SessionError::Malformed { line: 3, .. }),
            "{err}"
        );
    }

    #[test]
    fn ids_follow_the_last_entry_even_with_lines_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("ned");
        let session = session(&state, "/src/proj", "default");
        let mut log = session.lock().unwrap();
        for script in ["show 1", "show 2", "show 3"] {
            log.append(entry(script)).unwrap();
        }
        drop(log);
        let path = log_path(&state);
        let text = fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = text
            .lines()
            .filter(|line| !line.contains("show 2"))
            .collect();
        fs::write(&path, lines.join("\n") + "\n").unwrap();

        assert_eq!(session.lock().unwrap().append(entry("show 4")).unwrap(), 4);
        let ids: Vec<_> = session
            .lock()
            .unwrap()
            .entries()
            .unwrap()
            .iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(ids, [1, 3, 4]);
    }

    #[test]
    fn a_cut_short_last_line_is_ignored_and_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("ned");
        let session = session(&state, "/src/proj", "default");
        session.lock().unwrap().append(entry("show 1")).unwrap();
        let path = log_path(&state);
        let mut text = fs::read_to_string(&path).unwrap();
        text.push_str("{\"id\":2,\"time\":17");
        fs::write(&path, &text).unwrap();
        assert_eq!(session.lock().unwrap().entries().unwrap().len(), 1);

        assert_eq!(session.lock().unwrap().append(entry("show 2")).unwrap(), 2);
        let scripts: Vec<_> = session
            .lock()
            .unwrap()
            .entries()
            .unwrap()
            .into_iter()
            .map(|e| (e.id, e.script.unwrap()))
            .collect();
        assert_eq!(scripts, [(1, "show 1".into()), (2, "show 2".into())]);
    }

    #[test]
    fn appending_to_a_log_with_a_malformed_last_line_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("ned");
        let session = session(&state, "/src/proj", "default");
        session.lock().unwrap().append(entry("show 1")).unwrap();
        let path = log_path(&state);
        let mut text = fs::read_to_string(&path).unwrap();
        text.push_str("garbage\n");
        fs::write(&path, text).unwrap();
        let err = session.lock().unwrap().append(entry("show 2")).unwrap_err();
        assert!(
            matches!(err, SessionError::Malformed { line: 3, .. }),
            "{err}"
        );
    }

    #[test]
    fn session_names_are_checked() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("ned");
        for name in ["default", "agent-1", "a.b_C"] {
            assert_eq!(session(&state, "/src/proj", name).name(), name);
        }
        for name in ["", ".hidden", "..", "a/b", "a b", "ä"] {
            let err = Session::new(&state, Path::new("/src/proj"), name).unwrap_err();
            assert!(matches!(err, SessionError::BadName(_)), "{name}: {err}");
        }
    }

    #[test]
    fn sessions_are_kept_per_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("ned");
        session(&state, "/src/proj", "b")
            .lock()
            .unwrap()
            .append(entry("show 1"))
            .unwrap();
        session(&state, "/src/proj", "a")
            .lock()
            .unwrap()
            .append(entry("show 1"))
            .unwrap();
        session(&state, "/other/proj", "c")
            .lock()
            .unwrap()
            .append(entry("show 1"))
            .unwrap();
        session(&state, "/src/proj", "unrecorded");

        assert_eq!(
            sessions(&state, Path::new("/src/proj")).unwrap(),
            ["a", "b"]
        );
        assert_eq!(sessions(&state, Path::new("/other/proj")).unwrap(), ["c"]);
        assert!(sessions(&state, Path::new("/new")).unwrap().is_empty());

        let mut dirs: Vec<_> = fs::read_dir(state.join("sessions"))
            .unwrap()
            .map(|dir| dir.unwrap().file_name().into_string().unwrap())
            .collect();
        dirs.sort();
        assert_eq!(dirs.len(), 2);
        assert!(dirs.iter().all(|dir| dir.starts_with("proj-")), "{dirs:?}");
        assert_ne!(dirs[0], dirs[1]);
    }

    fn record(state: &Path, root: &str, name: &str) {
        session(state, root, name)
            .lock()
            .unwrap()
            .append(entry("show 1"))
            .unwrap();
    }

    #[test]
    fn workspaces_are_listed_by_path_with_their_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("ned");
        assert!(workspaces(&state).unwrap().is_empty());
        record(&state, "/src/proj", "b");
        record(&state, "/src/proj", "a");
        record(&state, "/other/proj", "c");
        session(&state, "/new", "unrecorded");

        assert_eq!(
            workspaces(&state).unwrap(),
            [
                (PathBuf::from("/other/proj"), vec!["c".to_string()]),
                (
                    PathBuf::from("/src/proj"),
                    vec!["a".to_string(), "b".to_string()]
                ),
            ]
        );
    }

    #[test]
    fn a_workspace_is_named_by_any_readable_log_else_its_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("ned");
        record(&state, "/src/proj", "a");
        record(&state, "/src/proj", "b");
        record(&state, "/other/proj", "c");
        let dir_of = |name: &str| {
            fs::read_dir(state.join("sessions"))
                .unwrap()
                .map(|dir| dir.unwrap().path())
                .find(|dir| dir.join(format!("{name}.log")).exists())
                .unwrap()
        };
        fs::write(dir_of("a").join("a.log"), "not json\n").unwrap();
        let other = dir_of("c");
        fs::write(other.join("c.log"), "").unwrap();

        assert_eq!(
            workspaces(&state).unwrap(),
            [
                (
                    PathBuf::from("/src/proj"),
                    vec!["a".to_string(), "b".to_string()]
                ),
                (other, vec!["c".to_string()]),
            ]
        );
    }

    #[test]
    fn delete_removes_only_its_session_and_then_the_empty_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("ned");
        record(&state, "/src/proj", "a");
        record(&state, "/src/proj", "b");
        record(&state, "/other/proj", "c");

        session(&state, "/src/proj", "a").delete().unwrap();
        assert!(!session(&state, "/src/proj", "a").exists());
        assert_eq!(sessions(&state, Path::new("/src/proj")).unwrap(), ["b"]);
        let dir = workspace_dir(&state, Path::new("/src/proj"));
        assert!(!dir.join("a.lock").exists());

        session(&state, "/src/proj", "b").delete().unwrap();
        assert!(!dir.exists());
        assert_eq!(
            workspaces(&state).unwrap(),
            [(PathBuf::from("/other/proj"), vec!["c".to_string()])]
        );
    }

    #[test]
    fn delete_waits_for_the_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("ned");
        record(&state, "/src/proj", "a");
        let held = session(&state, "/src/proj", "a");
        let log = held.lock().unwrap();
        let deleting = std::thread::spawn({
            let state = state.clone();
            move || session(&state, "/src/proj", "a").delete().unwrap()
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(held.exists());
        drop(log);
        deleting.join().unwrap();
        assert!(!held.exists());
    }

    #[cfg(unix)]
    #[test]
    fn session_dirs_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("ned");
        session(&state, "/src/proj", "default");
        let workspace = log_path(&state).parent().unwrap().to_path_buf();
        for dir in [&state, &state.join("sessions"), &workspace] {
            let mode = fs::metadata(dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{}", dir.display());
        }

        fs::set_permissions(&workspace, fs::Permissions::from_mode(0o755)).unwrap();
        let err = Session::new(&state, Path::new("/src/proj"), "default").unwrap_err();
        assert!(
            matches!(
                err,
                SessionError::Dir(crate::fs::PrivateDirError::Unsafe { .. })
            ),
            "{err}"
        );
        assert!(err.to_string().contains("XDG_STATE_HOME"), "{err}");
    }

    #[test]
    fn the_lock_excludes_other_holders() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("ned");
        let session = session(&state, "/src/proj", "default");
        let mut held = session.lock().unwrap();

        let (sent, received) = mpsc::channel();
        let waiter = {
            let state = state.clone();
            thread::spawn(move || {
                let other = Session::new(&state, Path::new("/src/proj"), "default").unwrap();
                let mut log = other.lock().unwrap();
                log.append(entry("show 2")).unwrap();
                sent.send(()).unwrap();
            })
        };
        assert!(received.recv_timeout(Duration::from_millis(200)).is_err());
        held.append(entry("show 1")).unwrap();
        drop(held);
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        waiter.join().unwrap();

        let entries = session.lock().unwrap().entries().unwrap();
        let scripts: Vec<_> = entries
            .iter()
            .map(|e| (e.id, e.script.clone().unwrap()))
            .collect();
        assert_eq!(scripts, [(1, "show 1".into()), (2, "show 2".into())]);
    }

    fn recorded(id: u64, script: Option<&str>, exit: u8, files: usize) -> Entry {
        let mut entry = entry(script.unwrap_or_default());
        entry.id = id;
        entry.script = script.map(String::from);
        entry.exit = exit;
        entry.changes.truncate(files);
        entry
    }

    #[test]
    fn history_lines_give_each_entry_outcome_files_and_script() {
        let mut dry = recorded(3, Some("delete fn:a"), 0, 0);
        dry.dry_run = true;
        let mut undo = recorded(6, None, 0, 1);
        undo.undoes = Some(5);
        let mut two = recorded(4, Some("sub /a/ with \"b\""), 0, 1);
        two.changes.push(two.changes[0].clone());
        let entries = [
            recorded(1, Some("show fn:parse"), 0, 0),
            recorded(2, Some("replace fn:prase>\"x\" with \"y\""), 1, 0),
            dry,
            two,
            recorded(5, Some("replace fn:parse>\"x\" with \"y\"\n"), 0, 1),
            undo,
            recorded(7, Some("file a.rs\ndelete fn:a\ndelete fn:b\n"), 0, 1),
            recorded(8, Some("show 1\nshow 2"), 2, 0),
        ];
        assert_eq!(
            history(&entries, false),
            "\
1 ok: show fn:parse
2 exit 1: replace fn:prase>\"x\" with \"y\"
3 dry run: delete fn:a
4 ok, 2 files: sub /a/ with \"b\"
5 ok, 1 file, undone: replace fn:parse>\"x\" with \"y\"
6 undo 5, 1 file
7 ok, 1 file: file a.rs (+2 lines)
8 exit 2: show 1 (+1 line)
"
        );
    }

    #[test]
    fn a_write_is_recorded_without_a_script_and_shown_as_a_write() {
        let mut write = recorded(2, None, 0, 1);
        write.write = true;
        let entries = [recorded(1, Some("show 1"), 0, 0), write.clone()];
        assert_eq!(history(&entries, false), "1 ok: show 1\n2 write, 1 file\n");

        let json = serde_json::to_string(&write).unwrap();
        assert!(json.contains("\"write\":true"), "{json}");
        assert_eq!(serde_json::from_str::<Entry>(&json).unwrap(), write);
        let script = serde_json::to_string(&entries[0]).unwrap();
        assert!(!script.contains("write"), "{script}");
        assert!(!serde_json::from_str::<Entry>(&script).unwrap().write);

        let (repeated, script) = repeat("!!", &entries, false).unwrap().unwrap();
        assert_eq!((repeated.id, script.as_str()), (1, "show 1"));
    }

    #[test]
    fn a_new_session_takes_the_first_free_name() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("ned");
        let root = Path::new("/src/proj");
        assert_eq!(next_free(&state, root, "repl").unwrap().name(), "repl-1");
        for name in ["repl-1", "repl-3", "agent"] {
            session(&state, "/src/proj", name)
                .lock()
                .unwrap()
                .append(entry("show 1"))
                .unwrap();
        }
        assert_eq!(next_free(&state, root, "repl").unwrap().name(), "repl-2");
        assert_eq!(
            next_free(&state, Path::new("/src/other"), "repl")
                .unwrap()
                .name(),
            "repl-1"
        );
    }

    #[test]
    fn a_new_session_reserves_its_name_before_anything_is_recorded() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("ned");
        let root = Path::new("/src/proj");
        let first = next_free(&state, root, "mcp").unwrap();
        let second = next_free(&state, root, "mcp").unwrap();
        assert_eq!([first.name(), second.name()], ["mcp-1", "mcp-2"]);
        assert_eq!(sessions(&state, root).unwrap(), ["mcp-1", "mcp-2"]);
        assert!(first.exists());
        assert!(first.lock().unwrap().entries().unwrap().is_empty());
        assert_eq!(first.lock().unwrap().append(entry("show 1")).unwrap(), 1);
        assert_eq!(
            history(&first.lock().unwrap().entries().unwrap(), false)
                .lines()
                .count(),
            1
        );
        second.delete().unwrap();
        assert_eq!(sessions(&state, root).unwrap(), ["mcp-1"]);
    }

    #[test]
    fn a_follower_reads_each_entry_appended_once_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("ned");
        let session = session(&state, "/src/proj", "default");
        let (entries, mut follower) = session.lock().unwrap().follow().unwrap();
        assert!(entries.is_empty());
        assert!(follower.poll().unwrap().is_empty());

        session.lock().unwrap().append(entry("show 1")).unwrap();
        session.lock().unwrap().append(entry("show 2")).unwrap();
        let ids = |entries: Vec<Entry>| entries.iter().map(|e| e.id).collect::<Vec<_>>();
        assert_eq!(ids(follower.poll().unwrap()), [1, 2]);
        assert!(follower.poll().unwrap().is_empty());

        let (entries, mut later) = session.lock().unwrap().follow().unwrap();
        assert_eq!(ids(entries), [1, 2]);
        assert!(later.poll().unwrap().is_empty());

        // An append in progress: its line has no newline yet.
        let path = log_path(&state);
        let text = fs::read_to_string(&path).unwrap();
        let mut third = entry("show 3");
        third.id = 3;
        let line = serde_json::to_string(&third).unwrap();
        fs::write(&path, format!("{text}{}", &line[..10])).unwrap();
        assert!(follower.poll().unwrap().is_empty());
        fs::write(&path, format!("{text}{line}\n")).unwrap();
        assert_eq!(follower.poll().unwrap(), [third.clone()]);
        assert_eq!(later.poll().unwrap(), [third]);
    }

    #[test]
    fn a_follower_starts_over_on_a_log_made_anew() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("ned");
        let ids = |entries: Vec<Entry>| entries.iter().map(|e| e.id).collect::<Vec<_>>();
        let (_, mut follower) = session(&state, "/src/proj", "default")
            .lock()
            .unwrap()
            .follow()
            .unwrap();
        let append = |script| {
            session(&state, "/src/proj", "default")
                .lock()
                .unwrap()
                .append(entry(script))
                .unwrap()
        };
        append("show 1");
        append("show 2");
        assert_eq!(ids(follower.poll().unwrap()), [1, 2]);

        // Deleted and recorded in again: shorter than what was read.
        session(&state, "/src/proj", "default").delete().unwrap();
        append("show 3");
        assert_eq!(
            follower.poll().unwrap()[0].script.as_deref(),
            Some("show 3")
        );

        // Replaced by a longer log: another file.
        let path = log_path(&state);
        let other = tmp.path().join("other.log");
        let mut text = fs::read_to_string(&path).unwrap();
        for id in 2..=4 {
            let mut entry = entry("show 4");
            entry.id = id;
            text += &(serde_json::to_string(&entry).unwrap() + "\n");
        }
        fs::write(&other, text).unwrap();
        fs::rename(&other, &path).unwrap();
        assert_eq!(ids(follower.poll().unwrap()), [1, 2, 3, 4]);
    }

    #[test]
    fn a_follower_names_a_malformed_line() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("ned");
        let session = session(&state, "/src/proj", "default");
        session.lock().unwrap().append(entry("show 1")).unwrap();
        let (_, mut follower) = session.lock().unwrap().follow().unwrap();
        let path = log_path(&state);
        let text = fs::read_to_string(&path).unwrap();
        fs::write(&path, format!("{text}not json\n")).unwrap();
        match follower.poll() {
            Err(SessionError::Malformed { line: 3, .. }) => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn history_shows_the_last_ten_entries_unless_all() {
        let entries: Vec<_> = (1..=12)
            .map(|id| recorded(id, Some(&format!("show {id}")), 0, 0))
            .collect();
        let last: Vec<_> = (3..=12).map(|id| format!("{id} ok: show {id}\n")).collect();
        assert_eq!(history(&entries, false), last.concat());
        let every: Vec<_> = (1..=12).map(|id| format!("{id} ok: show {id}\n")).collect();
        assert_eq!(history(&entries, true), every.concat());
        assert_eq!(history(&[], true), "");
    }

    #[test]
    fn history_names_the_commit_an_entry_made() {
        let mut entry = recorded(1, Some("edit"), 0, 1);
        entry.commit = Some("0123456789abcdef0123456789abcdef01234567".into());
        assert_eq!(
            history(&[entry], false),
            "1 ok, 1 file, commit 0123456: edit\n"
        );
    }

    #[test]
    fn a_log_without_commits_reads_as_null() {
        let mut line = serde_json::to_value(entry("show 1")).unwrap();
        line.as_object_mut().unwrap().remove("commit");
        let read: Entry = serde_json::from_value(line).unwrap();
        assert_eq!(read.commit, None);
    }

    #[test]
    fn uncommitted_changes_follow_the_last_commit() {
        let mut committed = edit(2, &[("/b", Some("1"), Some("2"))]);
        committed.commit = Some("abc".into());
        let entries = [
            edit(1, &[("/a", Some("1"), Some("2"))]),
            committed,
            edit(3, &[("/c", None, Some("1")), ("/a", Some("2"), Some("3"))]),
            recorded(4, Some("show 1"), 1, 0),
            undo_of(5, 3, &[("/c", Some("1"), None)]),
        ];
        let on_disk = |path: &Path| Ok((path == Path::new("/a")).then(|| "3".to_string()));
        let paths: Vec<_> = uncommitted(&entries, on_disk)
            .unwrap()
            .into_iter()
            .map(|(id, c)| (id, c.path, c.before, c.after))
            .collect();
        let s = |s: &str| Some(s.to_string());
        assert_eq!(
            paths,
            [
                (3, PathBuf::from("/c"), None, s("1")),
                (3, PathBuf::from("/a"), s("2"), s("3")),
                (5, PathBuf::from("/c"), s("1"), None),
            ]
        );
        assert_eq!(uncommitted(&entries[..2], on_disk).unwrap(), vec![]);
    }

    #[test]
    fn a_file_changed_since_the_session_wrote_it_is_an_error() {
        let entries = [
            edit(1, &[("/a", Some("1"), Some("2"))]),
            edit(
                2,
                &[("/b", Some("1"), Some("2")), ("/a", Some("2"), Some("3"))],
            ),
        ];
        let on_disk = |path: &Path| Ok((path != Path::new("/a")).then(|| "2".to_string()));
        match uncommitted(&entries, on_disk) {
            Err(UncommittedError::Changed { path, id }) => {
                assert_eq!((path, id), (PathBuf::from("/a"), 2));
            }
            other => panic!("{other:?}"),
        }
        let reverted = |_: &Path| Ok(Some("1".to_string()));
        match uncommitted(&entries, reverted) {
            Err(UncommittedError::Changed { path, id }) => {
                assert_eq!((path, id), (PathBuf::from("/a"), 2));
            }
            other => panic!("{other:?}"),
        }
    }

    fn edit(id: u64, files: &[(&str, Option<&str>, Option<&str>)]) -> Entry {
        let mut entry = recorded(id, Some("edit"), 0, 0);
        entry.changes = files
            .iter()
            .map(|(path, before, after)| FileChange {
                path: PathBuf::from(path),
                before: before.map(String::from),
                after: after.map(String::from),
            })
            .collect();
        entry
    }

    fn undo_of(id: u64, undoes: u64, files: &[(&str, Option<&str>, Option<&str>)]) -> Entry {
        let mut entry = edit(id, files);
        entry.script = None;
        entry.undoes = Some(undoes);
        entry
    }

    /// Plans an undo of `entries` over files holding `disk`.
    fn plan(entries: &[Entry], disk: &[(&str, &str)], force: bool) -> Result<Undo, UndoError> {
        let disk: std::collections::HashMap<_, _> = disk
            .iter()
            .map(|(path, text)| (PathBuf::from(path), text.to_string()))
            .collect();
        undo(entries, |path| Ok(disk.get(path).cloned()), force)
    }

    fn change(path: &str, before: Option<&str>, after: Option<&str>) -> FileChange {
        FileChange {
            path: PathBuf::from(path),
            before: before.map(String::from),
            after: after.map(String::from),
        }
    }

    #[test]
    fn undo_restores_the_last_entry_that_wrote_files() {
        let entries = [
            edit(1, &[("/p/a.rs", Some("a0\n"), Some("a1\n"))]),
            edit(
                2,
                &[
                    ("/p/a.rs", Some("a1\n"), Some("a2\n")),
                    ("/p/b.rs", None, Some("b\n")),
                ],
            ),
            recorded(3, Some("show 1"), 0, 0),
            recorded(4, Some("delete fn:x"), 1, 0),
        ];
        let undo = plan(&entries, &[("/p/a.rs", "a2\n"), ("/p/b.rs", "b\n")], false).unwrap();
        assert_eq!(
            undo,
            Undo {
                id: 2,
                script: Some("edit".into()),
                changes: vec![
                    change("/p/a.rs", Some("a2\n"), Some("a1\n")),
                    change("/p/b.rs", Some("b\n"), None),
                ],
            }
        );
    }

    #[test]
    fn undo_walks_back_past_undone_entries() {
        let entries = [
            edit(1, &[("/p/a.rs", Some("a0\n"), Some("a1\n"))]),
            edit(2, &[("/p/a.rs", Some("a1\n"), Some("a2\n"))]),
            undo_of(3, 2, &[("/p/a.rs", Some("a2\n"), Some("a1\n"))]),
        ];
        let undo = plan(&entries, &[("/p/a.rs", "a1\n")], false).unwrap();
        assert_eq!(undo.id, 1);
        assert_eq!(
            undo.changes,
            [change("/p/a.rs", Some("a1\n"), Some("a0\n"))]
        );

        let mut entries = entries.to_vec();
        entries.push(undo_of(4, 1, &[("/p/a.rs", Some("a1\n"), Some("a0\n"))]));
        assert!(matches!(
            plan(&entries, &[("/p/a.rs", "a0\n")], false),
            Err(UndoError::Nothing)
        ));
    }

    #[test]
    fn nothing_to_undo_without_an_entry_that_wrote_files() {
        assert!(matches!(plan(&[], &[], false), Err(UndoError::Nothing)));
        let mut dry = recorded(2, Some("delete fn:a"), 0, 0);
        dry.dry_run = true;
        let entries = [recorded(1, Some("show 1"), 0, 0), dry];
        assert!(matches!(plan(&entries, &[], true), Err(UndoError::Nothing)));
    }

    #[test]
    fn a_file_changed_since_is_refused_unless_forced() {
        let entries = [edit(
            1,
            &[("/p/a.rs", Some("a\nb\nc\n"), Some("A\nb\nc\n"))],
        )];
        let disk = [("/p/a.rs", "A\nb\nC\n")];
        let err = plan(&entries, &disk, false).unwrap_err();
        assert!(matches!(err, UndoError::Changed { id: 1, .. }), "{err}");
        assert!(err.to_string().contains("--force"), "{err}");

        let undo = plan(&entries, &disk, true).unwrap();
        assert_eq!(
            undo.changes,
            [change("/p/a.rs", Some("A\nb\nC\n"), Some("a\nb\nC\n"))]
        );
    }

    #[test]
    fn a_forced_undo_that_overlaps_a_later_change_conflicts() {
        let entries = [edit(1, &[("/p/a.rs", Some("a\nb\n"), Some("A\nb\n"))])];
        let err = plan(&entries, &[("/p/a.rs", "x\nA2\nb\n")], true).unwrap_err();
        assert!(
            matches!(err, UndoError::Conflict { line: 1, id: 1, .. }),
            "{err}"
        );
    }

    #[test]
    fn a_file_removed_since_cannot_be_undone() {
        let entries = [edit(1, &[("/p/a.rs", Some("a\n"), Some("b\n"))])];
        for force in [false, true] {
            let err = plan(&entries, &[], force).unwrap_err();
            assert!(matches!(err, UndoError::Removed { id: 1, .. }), "{err}");
        }
    }

    #[test]
    fn a_forced_undo_removes_a_created_file_it_empties() {
        let entries = [edit(1, &[("/p/c.rs", None, Some("c\n"))])];
        let undo = plan(&entries, &[("/p/c.rs", "c\n")], true).unwrap();
        assert_eq!(undo.changes, [change("/p/c.rs", Some("c\n"), None)]);
        let err = plan(&entries, &[("/p/c.rs", "c\nlater\n")], true).unwrap_err();
        assert!(matches!(err, UndoError::Conflict { line: 2, .. }), "{err}");
    }

    #[test]
    fn undo_errors_name_paths_relative_to_a_dir() {
        let err = UndoError::Changed {
            path: PathBuf::from("/p/src/a.rs"),
            id: 3,
        };
        let err = err.relative_to(Path::new("/p"));
        assert!(err.to_string().starts_with("src/a.rs changed"), "{err}");
        let err = UndoError::Removed {
            path: PathBuf::from("/q/a.rs"),
            id: 3,
        };
        assert!(
            err.relative_to(Path::new("/p"))
                .to_string()
                .starts_with("/q/a.rs ")
        );
    }

    /// The script `src` repeats from entries with `scripts`, or its error.
    fn expand(src: &str, scripts: &[&str]) -> Option<Result<String, RepeatError>> {
        let entries: Vec<_> = scripts
            .iter()
            .enumerate()
            .map(|(i, script)| recorded(i as u64 + 1, Some(script), 0, 0))
            .collect();
        repeat(src, &entries, false).map(|result| result.map(|(_, script)| script))
    }

    fn expanded(src: &str, script: &str) -> String {
        expand(src, &["show 1", script]).unwrap().unwrap()
    }

    #[test]
    fn repeat_gives_the_last_script() {
        let entries = [
            recorded(1, Some("show 1"), 0, 0),
            recorded(2, Some("delete fn:prase"), 1, 0),
            undo_of(3, 1, &[]),
        ];
        let (entry, script) = repeat("!!", &entries, false).unwrap().unwrap();
        assert_eq!((entry.id, script.as_str()), (2, "delete fn:prase"));
        assert_eq!(expanded("  !!\n", "show 2"), "show 2");
    }

    #[test]
    fn repeat_passes_over_reads_that_succeeded() {
        let last = |entries: &[Entry]| repeat("!!", entries, false).unwrap().unwrap().0.id;
        let failed_edit = || recorded(1, Some("delete fn:prase"), 1, 0);
        let reads = |id| {
            recorded(
                id,
                Some("file b.rs; outline; show 1..2; check; allow errors"),
                0,
                0,
            )
        };
        assert_eq!(last(&[failed_edit(), reads(2), reads(3)]), 1);
        assert_eq!(
            last(&[recorded(1, Some("delete fn:parse"), 0, 1), reads(2)]),
            1
        );
        assert_eq!(last(&[reads(1), reads(2)]), 2);

        let mut errored = reads(2);
        errored.error = Some("bad".into());
        assert_eq!(last(&[failed_edit(), errored, reads(3)]), 2);
        let failed_read = recorded(2, Some("show fn:prase"), 1, 0);
        assert_eq!(last(&[failed_edit(), failed_read]), 2);
        let unparsed = recorded(2, Some("show \"open"), 0, 0);
        assert_eq!(last(&[failed_edit(), unparsed]), 2);
        let mixed = recorded(2, Some("show 1; sub /a/ with \"b\""), 0, 0);
        assert_eq!(last(&[reads(1), mixed, reads(3)]), 2);
    }

    #[test]
    fn other_scripts_are_no_repeat() {
        assert_eq!(expand("show 1", &["show 2"]), None);
        assert_eq!(expand("replace \"!!\" with \"!\"", &["show 2"]), None);
    }

    #[test]
    fn substitutions_correct_the_repeat_in_order() {
        let script = "replace fn:prase>\"a\" with \"a\"";
        assert_eq!(
            expanded("!!:s/prase/parse/", script),
            "replace fn:parse>\"a\" with \"a\""
        );
        assert_eq!(expanded("!!:s/a/b/", "a a a"), "b a a");
        assert_eq!(expanded("!!:gs/a/b/", "a a a"), "b b b");
        assert_eq!(expanded("!!:s/a/b/:gs/a/c/", "a a a"), "b c c");
        assert_eq!(expanded("!!:s/a/b", "a a"), "b a");
        assert_eq!(expanded("!!:s|a/b|c|", "x a/b"), "x c");
        assert_eq!(expanded(r"!!:s/a\/b/c\//", "a/b"), "c/");
        assert_eq!(expanded(r"!!:s/\n/x/", r"a\nb"), "axb");
        assert_eq!(expanded("!!:s/ a//", "show a"), "show");
    }

    #[test]
    fn repeat_without_an_earlier_script_is_an_error() {
        assert_eq!(expand("!!", &[]), Some(Err(RepeatError::NoScript)));
        let entries = [undo_of(1, 1, &[])];
        assert!(matches!(
            repeat("!!", &entries, false),
            Some(Err(RepeatError::NoScript))
        ));
    }

    #[test]
    fn a_dry_run_is_repeated_only_as_one() {
        let mut dry = recorded(2, Some("delete fn:a"), 0, 0);
        dry.dry_run = true;
        let entries = [recorded(1, Some("show 1"), 0, 0), dry];
        assert_eq!(
            repeat("!!:s/a/b/", &entries, false),
            Some(Err(RepeatError::DryRun(2)))
        );
        let (entry, script) = repeat("!!:s/a/b/", &entries, true).unwrap().unwrap();
        assert_eq!((entry.id, script.as_str()), (2, "delete fn:b"));
    }

    #[test]
    fn a_substitution_of_missing_text_shows_the_script() {
        let err = expand("!!:s/x/y/", &["show 1\nshow 2"])
            .unwrap()
            .unwrap_err();
        assert_eq!(
            err,
            RepeatError::NotFound {
                old: "x".into(),
                script: "show 1\nshow 2".into(),
                hint: "; OLD must match its text exactly, spacing and escapes included".into(),
            }
        );
    }

    #[test]
    fn malformed_modifiers_are_errors() {
        for src in [
            "!!x",
            "!!:",
            "!!:s",
            "!!:s/a",
            "!!:s//b/",
            "!!:q/a/b/",
            "!!:sxaxbx",
            "!!:s a b ",
        ] {
            let err = expand(src, &["a"]).unwrap().unwrap_err();
            assert!(matches!(err, RepeatError::Malformed(_)), "{src}: {err}");
            assert!(err.to_string().contains("usage: !!"), "{err}");
        }
    }

    #[test]
    fn modifier_errors_say_how_to_fix_them() {
        let err = expand(r"!!:s/a\/b/c/", &[r"show /a\/b/"])
            .unwrap()
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "`a/b` isn't in the last script; it has `a\\/b`, and `\\/` in a modifier stands for `/`: \
             use another delimiter, as in !!:s|a\\/b|NEW|; the script is:\nshow /a\\/b/"
        );
        let err = expand("!!:s/zz/b/", &["show 1"]).unwrap().unwrap_err();
        assert!(
            err.to_string()
                .starts_with("`zz` isn't in the last script; OLD must match its text exactly"),
            "{err}"
        );
        for (src, rest) in [("!!:s|a|| a|", " a|"), ("!!:s/a/b/x", "x")] {
            let err = expand(src, &["show a"]).unwrap().unwrap_err();
            assert_eq!(
                err,
                RepeatError::Trailing {
                    rest: rest.into(),
                    delimiter: src.as_bytes()[4] as char
                },
                "{src}"
            );
        }
    }
}
