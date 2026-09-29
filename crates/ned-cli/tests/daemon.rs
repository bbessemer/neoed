//! End-to-end tests of `ned daemon` (command-language spec §1.1).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use tempfile::TempDir;

/// A workspace and a private runtime dir; stops its daemon when dropped.
struct Workspace {
    runtime: TempDir,
    dir: TempDir,
}

impl Workspace {
    fn new() -> Workspace {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::create_dir_all(dir.path().join("src/deep")).unwrap();
        Workspace {
            runtime: tempfile::tempdir().unwrap(),
            dir,
        }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap()
    }

    /// Runs `ned daemon ARGS` in `cwd` (relative to the workspace).
    fn ned_in(&self, cwd: &str, args: &[&str]) -> Output {
        cargo_bin_cmd!("ned")
            .arg("daemon")
            .args(args)
            .current_dir(self.dir.path().join(cwd))
            .env("XDG_RUNTIME_DIR", self.runtime.path())
            .output()
            .unwrap()
    }

    fn stdout(&self, args: &[&str]) -> String {
        let output = self.ned_in(".", args);
        assert!(output.status.success(), "ned daemon {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        self.ned_in(".", &["stop"]);
    }
}

/// The pid in a status line.
fn pid(status: &str) -> u32 {
    let (_, rest) = status.split_once(": pid ").expect(status);
    rest.split(',').next().unwrap().parse().unwrap()
}

#[test]
fn the_version_names_the_build() {
    let output = cargo_bin_cmd!("ned").arg("--version").output().unwrap();
    let version = String::from_utf8(output.stdout).unwrap();
    let build = version
        .strip_prefix(&format!("ned {}+", env!("CARGO_PKG_VERSION")))
        .and_then(|rest| rest.strip_suffix('\n'))
        .expect(&version);
    let mut identifiers = build.split('.');
    let commit = identifiers.next().unwrap();
    assert!(
        commit.len() >= 7 && commit.chars().all(|c| c.is_ascii_hexdigit()),
        "{version}"
    );
    if let Some(dirty) = identifiers.next() {
        assert_eq!(dirty, "dirty", "{version}");
        assert!(
            identifiers.next().unwrap().parse::<u64>().is_ok(),
            "{version}"
        );
    }
    assert_eq!(identifiers.next(), None, "{version}");
}

#[test]
fn status_without_a_daemon() {
    let ws = Workspace::new();
    assert_eq!(
        ws.stdout(&["status"]),
        format!("no daemon for {}\n", ws.root().display())
    );
}

#[test]
fn start_status_and_stop() {
    let ws = Workspace::new();
    let root = ws.root().display().to_string();
    let started = ws.stdout(&["start"]);
    assert!(
        started.starts_with(&format!("daemon for {root}: pid ")),
        "{started}"
    );
    assert!(started.ends_with(", up 0s\n"), "{started}");
    let status = ws.stdout(&["status"]);
    assert_eq!(pid(&status), pid(&started));
    assert_eq!(
        ws.stdout(&["stop"]),
        format!("stopped the daemon for {root}\n")
    );
    assert_eq!(ws.stdout(&["status"]), format!("no daemon for {root}\n"));
}

#[test]
fn starting_twice_keeps_one_daemon() {
    let ws = Workspace::new();
    let first = pid(&ws.stdout(&["start"]));
    assert_eq!(pid(&ws.stdout(&["start"])), first);
}

#[test]
fn stop_without_a_daemon() {
    let ws = Workspace::new();
    assert_eq!(
        ws.stdout(&["stop"]),
        format!("no daemon for {}\n", ws.root().display())
    );
}

#[test]
fn the_workspace_is_found_from_a_subdirectory_or_dir() {
    let ws = Workspace::new();
    let started = ws.ned_in("src/deep", &["start"]);
    assert!(started.status.success(), "{started:?}");
    let status = ws.stdout(&["status", "src"]);
    assert_eq!(
        pid(&status),
        pid(&String::from_utf8(started.stdout).unwrap())
    );
}

#[test]
fn an_unsafe_runtime_dir_is_refused() {
    let ws = Workspace::new();
    let dir: &Path = &ws.runtime.path().join("ned");
    fs::create_dir(dir).unwrap();
    fs::set_permissions(dir, fs::Permissions::from_mode(0o777)).unwrap();
    let output = ws.ned_in(".", &["start"]);
    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.starts_with("error: "), "{stderr}");
    assert!(stderr.contains("XDG_RUNTIME_DIR"), "{stderr}");
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
}
