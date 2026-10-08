//! A blocking client for the daemon, so `ned` never starts an async runtime.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use ned_core::hint::{self, Fix, Hint};
use ned_core::lsp::{
    Diagnosis, Document, Formatting, Locate, Located, Lsp, LspFailure, Position, Renamed,
};
use thiserror::Error;

use crate::paths::{Paths, runtime_dir};
use crate::protocol::{Request, Response};

const START_TIMEOUT: Duration = Duration::from_secs(5);
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);
/// For requests that wait on language servers, which time out themselves.
const SERVER_REPLY_TIMEOUT: Duration = Duration::from_secs(600);

/// The longest socket path the platform can bind: `sun_path` also holds a NUL.
const MAX_SOCKET_LEN: usize =
    size_of::<libc::sockaddr_un>() - std::mem::offset_of!(libc::sockaddr_un, sun_path) - 1;

/// A connection point to a running daemon.
#[derive(Debug, Clone)]
pub struct Client {
    paths: Paths,
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("cannot reach the daemon: {0}")]
    Io(#[from] io::Error),
    #[error("cannot start the daemon: {source}")]
    Spawn { source: io::Error },
    #[error("the daemon didn't start")]
    NotStarted { log: PathBuf },
    #[error(
        "daemon socket path {} is {len} bytes, over the platform's limit of {MAX_SOCKET_LEN}",
        socket.display()
    )]
    SocketTooLong { socket: PathBuf, len: usize },
    #[error("the daemon sent an invalid response: {0}")]
    Protocol(String),
}

impl Hint for ClientError {
    fn exit_code(&self) -> u8 {
        3
    }

    fn fix(&self) -> Option<Fix> {
        Some(Fix::from(match self {
            ClientError::Io(_) | ClientError::Protocol(_) => "run `ned daemon stop`, then retry",
            ClientError::NotStarted { log } => {
                let log = hint::verbatim(&log.display().to_string());
                return Some(format!("see its log, {log}").into());
            }
            ClientError::SocketTooLong { .. } => "set XDG_RUNTIME_DIR to a shorter directory",
            ClientError::Spawn { .. } => return None,
        }))
    }
}

impl Client {
    /// The running daemon at `paths`, if there is one.
    pub fn connect(paths: &Paths) -> Option<Client> {
        UnixStream::connect(&paths.socket).ok().map(|_| Client {
            paths: paths.clone(),
        })
    }

    /// The running daemon at `paths`, spawning `exe daemon run ROOT` first if
    /// there is none.
    pub fn connect_or_spawn(paths: &Paths, exe: &Path, root: &Path) -> Result<Client, ClientError> {
        if let Some(client) = Client::connect(paths) {
            return Ok(client);
        }
        check_socket_len(&paths.socket)?;
        let spawn = |source| ClientError::Spawn { source };
        let log = File::create(&paths.log).map_err(spawn)?;
        let mut child = Command::new(exe)
            .arg("daemon")
            .arg("run")
            .arg(root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            // Out of the terminal's process group, so ^C in it leaves the daemon.
            .process_group(0)
            .spawn()
            .map_err(spawn)?;
        let deadline = Instant::now() + START_TIMEOUT;
        let mut delay = Duration::from_millis(5);
        loop {
            if let Some(client) = Client::connect(paths) {
                return Ok(client);
            }
            // A daemon that exits 0 lost the race to another, which is starting.
            let failed = matches!(child.try_wait(), Ok(Some(status)) if !status.success());
            if failed || Instant::now() >= deadline {
                return Err(ClientError::NotStarted {
                    log: paths.log.clone(),
                });
            }
            thread::sleep(delay);
            delay = (delay * 2).min(Duration::from_millis(200));
        }
    }

    pub fn request(&self, request: &Request) -> Result<Response, ClientError> {
        let mut stream = UnixStream::connect(&self.paths.socket)?;
        let timeout = match request {
            Request::Status | Request::Stop => REPLY_TIMEOUT,
            Request::Open { .. }
            | Request::Diagnose { .. }
            | Request::Rename { .. }
            | Request::Locate { .. }
            | Request::Format { .. } => SERVER_REPLY_TIMEOUT,
        };
        stream.set_read_timeout(Some(timeout))?;
        let mut line = serde_json::to_string(request).expect("requests serialize");
        line.push('\n');
        stream.write_all(line.as_bytes())?;
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply)?;
        serde_json::from_str(&reply).map_err(|err| ClientError::Protocol(err.to_string()))
    }
}

/// Fails unless the platform can bind `socket`.
fn check_socket_len(socket: &Path) -> Result<(), ClientError> {
    let len = socket.as_os_str().len();
    if len > MAX_SOCKET_LEN {
        return Err(ClientError::SocketTooLong {
            socket: socket.to_path_buf(),
            len,
        });
    }
    Ok(())
}

/// The daemon for the workspace containing a directory, reached (and
/// spawned if need be) on first use.
pub struct Workspace {
    exe: PathBuf,
    root: PathBuf,
    version: String,
    client: Option<Client>,
}

impl Workspace {
    /// The daemon for the workspace at `root`, spawned as `exe daemon run`
    /// for `ned` build `version`.
    pub fn new(exe: PathBuf, root: PathBuf, version: &str) -> Workspace {
        Workspace {
            exe,
            root,
            version: version.into(),
            client: None,
        }
    }

    /// The daemon, spawned if it isn't running.
    fn client(&mut self) -> Result<Client, LspFailure> {
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }

        let runtime = runtime_dir().map_err(hint::Error::new)?;
        let paths = Paths::new(&runtime, &self.root, &self.version);
        let client =
            Client::connect_or_spawn(&paths, &self.exe, &self.root).map_err(hint::Error::new)?;
        Ok(self.client.insert(client).clone())
    }

    fn request(&mut self, request: &Request) -> Result<Response, LspFailure> {
        match self.client()?.request(request) {
            Ok(Response::Error(failure)) => Err(failure),
            Ok(response) => Ok(response),
            Err(err) => Err(hint::Error::new(err).into()),
        }
    }
}

impl Lsp for Workspace {
    /// Whether a daemon is running for the workspace; never spawns one.
    fn running(&mut self) -> bool {
        if self.client.is_none()
            && let Ok(runtime) = runtime_dir()
        {
            self.client = Client::connect(&Paths::new(&runtime, &self.root, &self.version));
        }
        self.client.is_some()
    }

    fn diagnose(&mut self, documents: &[Document], saved: bool) -> Result<Diagnosis, LspFailure> {
        let documents = documents.to_vec();
        match self.request(&Request::Diagnose { documents, saved })? {
            Response::Diagnosis(diagnosis) => Ok(diagnosis),
            other => Err(hint::Error::new(ClientError::Protocol(format!("{other:?}"))).into()),
        }
    }

    fn sync(&mut self, documents: &[Document]) -> Result<(), LspFailure> {
        let documents = documents.to_vec();
        match self.request(&Request::Open { documents })? {
            Response::Opened => Ok(()),
            other => Err(hint::Error::new(ClientError::Protocol(format!("{other:?}"))).into()),
        }
    }

    fn rename(
        &mut self,
        document: &Document,
        position: Position,
        name: &str,
    ) -> Result<Renamed, LspFailure> {
        let request = Request::Rename {
            document: document.clone(),
            position,
            name: name.into(),
        };
        match self.request(&request)? {
            Response::Renamed(renamed) => Ok(renamed),
            other => Err(hint::Error::new(ClientError::Protocol(format!("{other:?}"))).into()),
        }
    }

    fn locate(
        &mut self,
        kind: Locate,
        document: &Document,
        position: Position,
    ) -> Result<Located, LspFailure> {
        let request = Request::Locate {
            kind,
            document: document.clone(),
            position,
        };
        match self.request(&request)? {
            Response::Located(located) => Ok(located),
            other => Err(hint::Error::new(ClientError::Protocol(format!("{other:?}"))).into()),
        }
    }

    fn format(&mut self, document: &Document) -> Result<Formatting, LspFailure> {
        let request = Request::Format {
            document: document.clone(),
        };
        match self.request(&request)? {
            Response::Formatted(formatting) => Ok(formatting),
            other => Err(hint::Error::new(ClientError::Protocol(format!("{other:?}"))).into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_socket_path_over_the_platform_limit_is_refused_with_a_fix() {
        let fits = PathBuf::from(format!("/{}", "a".repeat(MAX_SOCKET_LEN - 1)));
        assert!(check_socket_len(&fits).is_ok());
        let long = PathBuf::from(format!("/{}", "a".repeat(MAX_SOCKET_LEN)));
        let err = check_socket_len(&long).unwrap_err();
        assert!(
            matches!(&err, ClientError::SocketTooLong { socket, len } if *socket == long && *len == MAX_SOCKET_LEN + 1),
            "{err}"
        );
        let message = hint::Error::new(err).render(hint::Frontend::Cli, None);
        assert!(
            message.contains(&format!("is {} bytes", MAX_SOCKET_LEN + 1)),
            "{message}"
        );
        assert!(
            message.ends_with("set XDG_RUNTIME_DIR to a shorter directory"),
            "{message}"
        );
    }
}
