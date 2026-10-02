//! Sessions: per-workspace logs of `ned` invocations (spec §1.2).

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
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
    let dir = workspace_dir(state_dir, root);
    let io_error = |source| SessionError::Io {
        path: dir.clone(),
        source,
    };
    let read = match fs::read_dir(&dir) {
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
    /// Every entry, oldest first; none if nothing has been recorded.
    pub fn entries(&self) -> Result<Vec<Entry>, SessionError> {
        let Some(text) = self.read()? else {
            return Ok(Vec::new());
        };
        // A last line without its newline is an append cut short, which the next
        // append replaces.
        let text = &text[..text.rfind('\n').map_or(0, |i| i + 1)];
        let mut lines = text.lines().enumerate();
        if let Some((_, header)) = lines.next() {
            self.check_header(header)?;
        }
        lines
            .map(|(i, line)| serde_json::from_str(line).map_err(|err| self.malformed(i, &err)))
            .collect()
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
            let header = Header {
                ned_session: FORMAT,
                workspace: self.session.root.clone(),
            };
            record.push_str(&serde_json::to_string(&header).expect("a header serializes"));
            record.push('\n');
            0
        } else {
            let mut header = String::new();
            file.seek(SeekFrom::Start(0)).map_err(io_error)?;
            BufReader::new(&file)
                .read_line(&mut header)
                .map_err(io_error)?;
            self.check_header(header.trim_end())?;
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

    fn check_header(&self, line: &str) -> Result<(), SessionError> {
        let header: Header = serde_json::from_str(line).map_err(|err| self.malformed(0, &err))?;
        if header.ned_session != FORMAT {
            return Err(SessionError::UnknownFormat {
                path: self.session.log.clone(),
                version: header.ned_session,
            });
        }
        Ok(())
    }

    /// The error for the log's line `index` (from 0).
    fn malformed(&self, index: usize, err: &serde_json::Error) -> SessionError {
        SessionError::Malformed {
            path: self.session.log.clone(),
            line: index + 1,
            message: err.to_string(),
        }
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
