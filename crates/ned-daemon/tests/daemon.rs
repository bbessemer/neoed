//! The daemon and its client, in process (command-language spec §1.1).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use std::{io, process};

use ned_core::lang::Language;
use ned_daemon::client::ClientError;
use ned_daemon::protocol::{Document, Request, Response};
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
        self.thread = Some(thread::spawn(move || {
            server::serve(&paths, &root, None, idle)
        }));
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
    server::serve(&daemon.paths, &daemon.root, None, IDLE).unwrap();
    assert!(matches!(
        daemon.client().request(&Request::Status),
        Ok(Response::Status(_))
    ));
}

#[test]
fn a_stale_socket_is_replaced() {
    let daemon = Daemon::new();
    // Bound by a child process: a listener bound here could leak into a
    // process another test spawns at that moment (macOS sets close-on-exec
    // after creating the socket), and keep listening after being dropped.
    let bound = process::Command::new("python3")
        .args([
            "-c",
            "import socket, sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])",
        ])
        .arg(&daemon.paths.socket)
        .status()
        .unwrap();
    assert!(bound.success());
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

#[test]
fn open_starts_servers_that_status_lists_and_stop_shuts_down() {
    let fake = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake_lsp.py");
    let daemon = Daemon::new();
    let log = daemon.root.join("lsp.log");
    let config = format!("[lsp]\nrust = [{fake:?}, {log:?}]\n");
    std::fs::write(daemon.root.join(".ned.toml"), config).unwrap();
    let mut daemon = daemon.start(IDLE);
    let open = Request::Open {
        documents: vec![Document {
            path: daemon.root.join("a.rs"),
            lang: Language::Rust,
            text: "fn a() {}\n".into(),
        }],
    };
    assert_eq!(daemon.client().request(&open).unwrap(), Response::Opened);
    let Response::Status(status) = daemon.client().request(&Request::Status).unwrap() else {
        panic!("not a status");
    };
    assert_eq!(status.servers.len(), 1);
    assert_eq!(status.servers[0].name, "fake_lsp.py");
    assert_eq!(status.servers[0].documents, 1);

    assert_eq!(
        daemon.client().request(&Request::Stop).unwrap(),
        Response::Stopped
    );
    daemon.join();
    let log = std::fs::read_to_string(&log).unwrap();
    assert!(log.lines().last().unwrap().contains("\"exit\""), "{log}");
}
