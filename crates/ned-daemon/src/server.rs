//! The daemon's event loop.

use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use std::{io, process};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::net::unix::OwnedWriteHalf;
use tokio::signal::unix::{SignalKind, signal};
use tokio::time;

use crate::paths::Paths;
use crate::protocol::{Request, Response, Status};
use crate::servers::Servers;

/// How long a client has to send its request once connected.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Serves `root`'s daemon at `paths` until a `stop` request, `idle` without
/// a request, or SIGTERM or SIGINT, reading server settings from
/// `user_config` and the workspace's config files. Returns at once if another
/// daemon holds the lock.
pub fn serve(
    paths: &Paths,
    root: &Path,
    user_config: Option<PathBuf>,
    idle: Duration,
) -> io::Result<()> {
    let lock = File::options()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&paths.lock)?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(()),
        Err(TryLockError::Error(err)) => return Err(err),
    }
    // Only the lock holder touches the socket, so this one is stale.
    match fs::remove_file(&paths.socket) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
        _ => {}
    }
    let mut daemon = Daemon {
        paths,
        root,
        started: Instant::now(),
        servers: Servers::new(root.to_path_buf(), user_config),
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(daemon.listen(lock, idle))
}

struct Daemon<'a> {
    paths: &'a Paths,
    root: &'a Path,
    started: Instant,
    servers: Servers,
}

impl Daemon<'_> {
    async fn listen(&mut self, lock: File, idle: Duration) -> io::Result<()> {
        let listener = UnixListener::bind(&self.paths.socket)?;
        let mut terminate = signal(SignalKind::terminate())?;
        let mut interrupt = signal(SignalKind::interrupt())?;
        loop {
            let stream = tokio::select! {
                accepted = listener.accept() => accepted?.0,
                () = time::sleep(idle) => break,
                _ = terminate.recv() => break,
                _ = interrupt.recv() => break,
            };
            let (read, mut write) = stream.into_split();
            let mut line = String::new();
            let mut read = BufReader::new(read);
            match time::timeout(REQUEST_TIMEOUT, read.read_line(&mut line)).await {
                // A client that only checked the daemon is there.
                Ok(Ok(0)) => continue,
                Ok(Ok(_)) => {}
                Ok(Err(err)) => {
                    eprintln!("ned daemon: cannot read a request: {err}");
                    continue;
                }
                Err(_) => {
                    eprintln!("ned daemon: a client sent no request");
                    continue;
                }
            }
            let response = match serde_json::from_str(&line) {
                Ok(Request::Status) => Response::Status(self.status()),
                Ok(Request::Stop) => {
                    // Released before replying, so a `start` right after
                    // the reply can take over.
                    self.release(listener, lock);
                    reply(&mut write, &Response::Stopped).await;
                    self.servers.shutdown().await;
                    return Ok(());
                }
                Ok(Request::Open { documents }) => match self.servers.open(&documents).await {
                    Ok(()) => Response::Opened,
                    Err(err) => Response::Error(err.to_string()),
                },
                Ok(Request::Diagnose { documents }) => {
                    match self.servers.diagnose(&documents).await {
                        Ok(diagnosis) => Response::Diagnosis(diagnosis),
                        Err(err) => Response::Error(err.to_string()),
                    }
                }
                Ok(Request::Rename {
                    document,
                    position,
                    name,
                }) => match self.servers.rename(&document, position, &name).await {
                    Ok(renamed) => Response::Renamed(renamed),
                    Err(err) => Response::Error(err.to_string()),
                },
                Err(err) => Response::Error(format!("invalid request: {err}")),
            };
            reply(&mut write, &response).await;
        }
        self.release(listener, lock);
        self.servers.shutdown().await;
        Ok(())
    }

    fn status(&self) -> Status {
        Status {
            version: self.paths.version.clone(),
            root: self.root.to_path_buf(),
            pid: process::id(),
            uptime_secs: self.started.elapsed().as_secs(),
            servers: self.servers.status(),
        }
    }

    /// Removes the socket, then gives up the lock.
    fn release(&self, listener: UnixListener, lock: File) {
        drop(listener);
        if let Err(err) = fs::remove_file(&self.paths.socket) {
            eprintln!(
                "ned daemon: cannot remove {}: {err}",
                self.paths.socket.display()
            );
        }
        drop(lock);
    }
}

async fn reply(write: &mut OwnedWriteHalf, response: &Response) {
    let mut line = serde_json::to_string(response).expect("responses serialize");
    line.push('\n');
    if let Err(err) = write.write_all(line.as_bytes()).await {
        eprintln!("ned daemon: cannot reply: {err}");
    }
}
