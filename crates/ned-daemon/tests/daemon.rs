//! The daemon and its client, in process (command-language spec §1.1).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use std::{io, process};

use ned_daemon::client::ClientError;
use ned_daemon::protocol::{Request, Response};
use ned_daemon::{Client, Paths, server};
use tempfile::TempDir;

const IDLE: Duration = Duration::from_secs(30);

struct Daemon {
    // Held so the runtime dir outlives the daemon.
    _dir: TempDir,
    root: PathBuf,
    paths: Paths,
    thread: Option<JoinHandle<io::Result<()>>>,
}

impl Daemon {
    fn new() -> Daemon {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let paths = Paths::new(&root, &root, "0.0.0 (test)");
        Daemon {
            _dir: dir,
            root,
            paths,
            thread: None,
        }
    }

    fn start(mut self, idle: Duration) -> Daemon {
        let (paths, root) = (self.paths.clone(), self.root.clone());
        self.thread = Some(thread::spawn(move || server::serve(&paths, &root, idle)));
        let deadline = Instant::now() + Duration::from_secs(5);
        while Client::connect(&self.paths).is_none() {
            assert!(Instant::now() < deadline, "the daemon didn't start");
            thread::sleep(Duration::from_millis(10));
        }
        self
    }

    fn client(&self) -> Client {
        Client::connect(&self.paths).expect("a running daemon")
    }

    fn join(&mut self) {
        self.thread.take().unwrap().join().unwrap().unwrap();
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if self.thread.is_some() && !thread::panicking() {
            let _ = self.client().request(&Request::Stop);
            self.join();
        }
    }
}

#[test]
fn status_reports_the_daemon() {
    let daemon = Daemon::new().start(IDLE);
    let Response::Status(status) = daemon.client().request(&Request::Status).unwrap() else {
        panic!("not a status");
    };
    assert_eq!(status.root, daemon.root);
    assert_eq!(status.pid, process::id());
    assert_eq!(status.version, "0.0.0 (test)");
    assert!(status.uptime_secs < 5);
}

#[test]
fn stop_ends_the_daemon_and_removes_its_socket() {
    let mut daemon = Daemon::new().start(IDLE);
    let response = daemon.client().request(&Request::Stop).unwrap();
    assert_eq!(response, Response::Stopped);
    daemon.join();
    assert!(!daemon.paths.socket.exists());
    assert!(Client::connect(&daemon.paths).is_none());
}

#[test]
fn an_idle_daemon_exits() {
    let mut daemon = Daemon::new().start(Duration::from_millis(200));
    let started = Instant::now();
    daemon.join();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(!daemon.paths.socket.exists());
}

#[test]
fn requests_keep_a_daemon_from_idling() {
    let daemon = Daemon::new().start(Duration::from_millis(500));
    for _ in 0..4 {
        thread::sleep(Duration::from_millis(250));
        daemon.client().request(&Request::Status).unwrap();
    }
}

#[test]
fn a_second_daemon_for_a_workspace_returns_at_once() {
    let daemon = Daemon::new().start(IDLE);
    server::serve(&daemon.paths, &daemon.root, IDLE).unwrap();
    assert!(matches!(
        daemon.client().request(&Request::Status),
        Ok(Response::Status(_))
    ));
}

#[test]
fn a_stale_socket_is_replaced() {
    let daemon = Daemon::new();
    drop(UnixListener::bind(&daemon.paths.socket).unwrap());
    assert!(daemon.paths.socket.exists());
    assert!(Client::connect(&daemon.paths).is_none());
    let daemon = daemon.start(IDLE);
    assert!(daemon.client().request(&Request::Status).is_ok());
}

#[test]
fn a_bad_request_gets_an_error() {
    let daemon = Daemon::new().start(IDLE);
    let mut stream = UnixStream::connect(&daemon.paths.socket).unwrap();
    stream.write_all(b"{\"method\":\"frobnicate\"}\n").unwrap();
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line).unwrap();
    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(matches!(response, Response::Error(_)), "{response:?}");
}

#[test]
fn connecting_without_a_daemon_finds_none() {
    assert!(Client::connect(&Daemon::new().paths).is_none());
}

#[test]
fn a_daemon_that_exits_at_once_did_not_start() {
    let daemon = Daemon::new();
    let err =
        Client::connect_or_spawn(&daemon.paths, Path::new("false"), &daemon.root).unwrap_err();
    assert!(
        matches!(&err, ClientError::NotStarted { log } if *log == daemon.paths.log),
        "{err}"
    );
}

#[test]
fn a_missing_program_cannot_start() {
    let daemon = Daemon::new();
    let exe = daemon.root.join("no-such-ned");
    let err = Client::connect_or_spawn(&daemon.paths, &exe, &daemon.root).unwrap_err();
    assert!(matches!(err, ClientError::Spawn { .. }), "{err}");
}
