//! End-to-end tests of `-w`, `--workspace` (command-language spec §2.4).

use std::fs;
use std::path::Path;
use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use tempfile::TempDir;

/// A git-less workspace with nested and ignored files.
fn workspace() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir(root.join(".git")).unwrap();
    for (path, text) in [
        (".gitignore", "target/\n"),
        ("src/a.rs", "// needle a\n"),
        ("src/deep/b.rs", "// needle b\n"),
        ("target/c.rs", "// needle c\n"),
        ("README.md", "no match\n"),
    ] {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    dir
}

fn ned(cwd: &Path, args: &[&str]) -> Output {
    let runtime = tempfile::tempdir().unwrap();
    cargo_bin_cmd!("ned")
        .args(args)
        .current_dir(cwd)
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env_remove("NED_SESSION")
        .write_stdin("")
        .output()
        .unwrap()
}

fn stdout(out: &Output) -> String {
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout.clone()).unwrap()
}

#[test]
fn workspace_mode_searches_every_file_from_a_subdirectory() {
    let dir = workspace();
    let out = ned(
        &dir.path().join("src/deep"),
        &["-w", "-e", "show all /needle/"],
    );
    let root = dir.path().canonicalize().unwrap();
    assert_eq!(
        stdout(&out),
        format!(
            "{}:1\n1:// needle a\nb.rs:1\n1:// needle b\n",
            root.join("src/a.rs").display()
        )
    );
}

#[test]
fn an_explicit_workspace_dir() {
    let dir = workspace();
    let root = dir.path().canonicalize().unwrap();
    let out = ned(&root, &["-w", "src/deep", "-e", "show all /needle/"]);
    assert_eq!(stdout(&out), "src/deep/b.rs:1\n1:// needle b\n");
}

#[test]
fn workspace_mode_takes_no_files() {
    let dir = workspace();
    let out = ned(dir.path(), &["-w", ".", "src/a.rs", "-e", "show 1"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
}

#[test]
fn a_missing_workspace_dir_exits_3() {
    let dir = workspace();
    let out = ned(dir.path(), &["-w", "nowhere", "-e", "show 1"]);
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.starts_with("error: cannot read nowhere: "),
        "{stderr}"
    );
    assert!(
        stderr.ends_with("; check that it exists and you can read it\n"),
        "{stderr}"
    );
}

#[test]
fn workspace_edits_write_only_their_files() {
    let dir = workspace();
    let out = ned(
        dir.path(),
        &["-w", "-q", "-e", "sub file:src/a.rs /needle/ with \"pin\""],
    );
    assert_eq!(stdout(&out), "src/a.rs: 1 edit, +1 -1\n");
    assert_eq!(
        fs::read_to_string(dir.path().join("src/a.rs")).unwrap(),
        "// pin a\n"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("target/c.rs")).unwrap(),
        "// needle c\n"
    );
}

#[cfg(unix)]
#[test]
fn a_file_glob_reads_only_the_files_it_matches() {
    let dir = workspace();
    let locked = dir.path().join("src/locked.md");
    fs::write(&locked, "needle\n").unwrap();
    fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o000)).unwrap();
    let out = ned(
        dir.path(),
        &["-w", "-e", "show all file:src/**/*.rs>/needle/"],
    );
    assert_eq!(
        stdout(&out),
        "src/a.rs:1\n1:// needle a\nsrc/deep/b.rs:1\n1:// needle b\n"
    );
}
