//! A blocking client for the daemon, so `ned` never starts an async runtime.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use ned_core::lsp::{Diagnosis, Document, Lsp, LspFailure};
use thiserror::Error;

use crate::paths::Paths;
use crate::protocol::{Request, Response};

const START_TIMEOUT: Duration = Duration::from_secs(5);
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// A connection point to a running daemon.
#[derive(Debug, Clone)]
pub struct Client {
    paths: Paths,
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("cannot reach the daemon: {0}; run `ned daemon stop`, then retry")]
    Io(#[from] io::Error),
    #[error("cannot start the daemon: {source}")]
    Spawn { source: io::Error },
    #[error("the daemon didn't start; see its log, {}", log.display())]
    NotStarted { log: PathBuf },
    #[error("the daemon sent an invalid response: {0}; run `ned daemon stop`, then retry")]
    Protocol(String),
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
        stream.set_read_timeout(Some(REPLY_TIMEOUT))?;
        let mut line = serde_json::to_string(request).expect("requests serialize");
        line.push('\n');
        stream.write_all(line.as_bytes())?;
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply)?;
        serde_json::from_str(&reply).map_err(|err| ClientError::Protocol(err.to_string()))
    }
}

/// The daemon for the workspace containing a directory, reached (and
/// spawned if need be) on first use.
pub struct Workspace {
    exe: PathBuf,
    dir: PathBuf,
    version: String,
    client: Option<Client>,
}

impl Workspace {
    /// Spawns the daemon as `exe daemon run`, for `ned` build `version`.
    pub fn new(exe: PathBuf, dir: PathBuf, version: &str) -> Workspace {
        Workspace {
            exe,
            dir,
            version: version.into(),
            client: None,
        }
    }
}

impl Lsp for Workspace {
    fn diagnose(&mut self, documents: &[Document]) -> Result<Diagnosis, LspFailure> {
        let _ = (
            &self.exe,
            &self.dir,
            &self.version,
            &mut self.client,
            documents,
        );
        todo!()
    }
}
