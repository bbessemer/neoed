//! Sessions: per-workspace logs of `ned` invocations (spec §1.2).

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The log format version, in the header's `ned_session` field.
pub const FORMAT: u64 = 1;

/// One recorded invocation: a script, or an undo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Assigned by [`Log::append`], numbering entries from 1.
    pub id: u64,
    /// Seconds since the Unix epoch.
    pub time: u64,
    pub cwd: PathBuf,
    pub files: Vec<String>,
    pub workspace: Option<PathBuf>,
    /// `None` for an undo.
    pub script: Option<String>,
    pub undoes: Option<u64>,
    pub dry_run: bool,
    pub exit: u8,
    pub error: Option<String>,
    pub changes: Vec<FileChange>,
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
    todo!()
}

/// The names of the sessions with a log for the workspace at `root`, sorted.
pub fn sessions(state_dir: &Path, root: &Path) -> Result<Vec<String>, SessionError> {
    todo!()
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
        todo!()
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
        todo!()
    }
}

/// A locked session's log.
#[derive(Debug)]
pub struct Log<'a> {
    session: &'a Session,
    _lock: File,
}

impl Log<'_> {
    /// Every entry, oldest first; none if nothing has been recorded.
    pub fn entries(&self) -> Result<Vec<Entry>, SessionError> {
        todo!()
    }

    /// Appends `entry` with the next id, which it returns.
    pub fn append(&mut self, entry: Entry) -> Result<u64, SessionError> {
        todo!()
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
            dry_run: false,
            exit: 0,
            error: None,
            changes: vec![FileChange {
                path: PathBuf::from("/src/proj/a.rs"),
                before: Some("old\n".into()),
                after: Some("new\n".into()),
            }],
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
}
