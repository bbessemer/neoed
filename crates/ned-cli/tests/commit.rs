//! End-to-end tests of `--commit` (command-language spec §1.3), against real
//! git repositories.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

use assert_cmd::cargo::cargo_bin_cmd;
use tempfile::TempDir;

/// A git repository with one commit of `files`, and private config and
/// state dirs for `ned`.
struct Repo {
    dir: TempDir,
    home: TempDir,
}

impl Repo {
    fn new(files: &[(&str, &str)]) -> Repo {
        let repo = Repo::empty();
        repo.write(files);
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", "init"]);
        repo
    }

    /// A repository without commits.
    fn empty() -> Repo {
        let repo = Repo {
            dir: tempfile::tempdir().unwrap(),
            home: tempfile::tempdir().unwrap(),
        };
        repo.git(&["init", "-q", "-b", "main"]);
        repo
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn write(&self, files: &[(&str, &str)]) {
        for (name, text) in files {
            fs::write(self.path().join(name), text).unwrap();
        }
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.path().join(name)).unwrap()
    }

    /// Runs `git ARGS` and returns its stdout.
    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .envs(isolated(self.home.path()))
            .args(args)
            .current_dir(self.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    /// Runs `ned ARGS` in the repository.
    fn ned(&self, args: &[&str]) -> Output {
        let mut cmd = cargo_bin_cmd!("ned");
        cmd.envs(isolated(self.home.path()))
            .args(args)
            .current_dir(self.path())
            .env("XDG_CONFIG_HOME", self.home.path())
            .env("XDG_STATE_HOME", self.home.path())
            .env("XDG_RUNTIME_DIR", self.home.path())
            .env_remove("NED_SESSION")
            .write_stdin("")
            .output()
            .unwrap()
    }

    fn head(&self) -> String {
        self.git(&["rev-parse", "HEAD"])
    }
}

/// The environment of a git with a fixed identity and no user or system
/// config.
fn isolated(home: &Path) -> Vec<(&'static str, String)> {
    vec![
        (
            "GIT_CONFIG_GLOBAL",
            home.join("gitconfig").display().to_string(),
        ),
        ("GIT_CONFIG_NOSYSTEM", "1".into()),
        ("GIT_AUTHOR_NAME", "A U Thor".into()),
        ("GIT_AUTHOR_EMAIL", "author@example.com".into()),
        ("GIT_COMMITTER_NAME", "C O Mitter".into()),
        ("GIT_COMMITTER_EMAIL", "committer@example.com".into()),
    ]
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

const NOTES: &str = "one\ntwo\nthree\nfour\nfive\n";

#[test]
fn commits_only_the_edits_written() {
    let repo = Repo::new(&[
        ("notes.txt", NOTES),
        ("other.txt", "x\n"),
        ("staged.txt", "s\n"),
    ]);
    repo.write(&[
        ("notes.txt", "one\ntwo\nthree\nfour\nFIVE\n"),
        ("other.txt", "y\n"),
        ("staged.txt", "S\n"),
    ]);
    repo.git(&["add", "staged.txt"]);
    let out = repo.ned(&[
        "notes.txt",
        "--commit",
        "Shout one\n\nThe body.",
        "-e",
        "replace \"one\" with \"ONE\"",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let last = stdout(&out).lines().last().unwrap().to_string();
    assert!(
        last.starts_with("commit ") && last.ends_with(": Shout one"),
        "{last}"
    );
    assert_eq!(
        repo.git(&["log", "-1", "--format=%B"]),
        "Shout one\n\nThe body.\n\n"
    );
    assert_eq!(
        repo.git(&["show", "--format=", "--name-only", "HEAD"]),
        "notes.txt\n"
    );
    assert_eq!(
        repo.git(&["show", "HEAD:notes.txt"]),
        "ONE\ntwo\nthree\nfour\nfive\n"
    );
    assert_eq!(repo.read("notes.txt"), "ONE\ntwo\nthree\nfour\nFIVE\n");
    assert_eq!(repo.git(&["diff", "--name-only"]), "notes.txt\nother.txt\n");
    assert_eq!(
        repo.git(&["diff", "--cached", "--name-only"]),
        "staged.txt\n"
    );
    assert_eq!(
        repo.git(&["status", "--porcelain"]),
        " M notes.txt\n M other.txt\nM  staged.txt\n"
    );
}

#[test]
fn staged_changes_to_an_edited_file_stay_staged() {
    let repo = Repo::new(&[("notes.txt", NOTES)]);
    repo.write(&[("notes.txt", "one\ntwo\nthree\nfour\nFIVE\n")]);
    repo.git(&["add", "notes.txt"]);
    let out = repo.ned(&[
        "notes.txt",
        "--commit",
        "Shout one",
        "-e",
        "replace \"one\" with \"ONE\"",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        repo.git(&["show", "HEAD:notes.txt"]),
        "ONE\ntwo\nthree\nfour\nfive\n"
    );
    assert_eq!(
        repo.git(&["show", ":notes.txt"]),
        "ONE\ntwo\nthree\nfour\nFIVE\n"
    );
    assert_eq!(repo.git(&["diff", "--name-only"]), "");
}

#[test]
fn created_files_are_added() {
    let repo = Repo::new(&[("notes.txt", NOTES)]);
    let out = repo.ned(&[
        "--commit",
        "Add todo",
        "-e",
        "create docs/todo.txt \"buy milk\"",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(repo.git(&["show", "HEAD:docs/todo.txt"]), "buy milk\n");
    assert_eq!(repo.git(&["status", "--porcelain"]), "");
}

#[test]
fn the_first_commit_has_no_parent() {
    let repo = Repo::empty();
    let out = repo.ned(&["--commit", "Start", "-e", "create notes.txt \"one\""]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(repo.git(&["rev-list", "--count", "HEAD"]), "1\n");
    assert_eq!(repo.git(&["status", "--porcelain"]), "");
}

#[test]
fn an_edit_overlapping_uncommitted_changes_writes_nothing() {
    let repo = Repo::new(&[("notes.txt", NOTES)]);
    let dirty = "one\nTWO\nthree\nfour\nfive\n";
    repo.write(&[("notes.txt", dirty)]);
    let head = repo.head();
    let out = repo.ned(&[
        "notes.txt",
        "--commit",
        "Edit",
        "-e",
        "replace \"TWO\" with \"2\"",
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("the edit to notes.txt overlaps its uncommitted changes at line 2"),
        "{}",
        stderr(&out)
    );
    assert_eq!(repo.read("notes.txt"), dirty);
    assert_eq!(repo.head(), head);
}

#[test]
fn refusals_write_nothing() {
    let repo = Repo::new(&[("notes.txt", NOTES), (".gitignore", "*.log\n")]);
    repo.write(&[("build.log", "x\n")]);
    let head = repo.head();
    for (args, code, message) in [
        (
            vec![
                "-n",
                "notes.txt",
                "--commit",
                "m",
                "-e",
                "replace \"one\" with \"1\"",
            ],
            2,
            "cannot be used with",
        ),
        (
            vec![
                "build.log",
                "--commit",
                "m",
                "-e",
                "replace \"x\" with \"y\"",
            ],
            1,
            "build.log is ignored by git",
        ),
        (
            vec!["notes.txt", "--commit", "m", "-e", "show 1"],
            1,
            "nothing to commit",
        ),
        (
            vec![
                "notes.txt",
                "--commit",
                "",
                "-e",
                "replace \"one\" with \"1\"",
            ],
            2,
            "--commit",
        ),
    ] {
        let out = repo.ned(&args);
        assert_eq!(out.status.code(), Some(code), "{args:?}: {}", stderr(&out));
        assert!(stderr(&out).contains(message), "{args:?}: {}", stderr(&out));
    }
    assert_eq!(repo.read("notes.txt"), NOTES);
    assert_eq!(repo.read("build.log"), "x\n");
    assert_eq!(repo.head(), head);
}

#[test]
fn a_merge_in_progress_is_refused() {
    let repo = Repo::new(&[("notes.txt", NOTES)]);
    fs::write(repo.path().join(".git/MERGE_HEAD"), repo.head()).unwrap();
    let out = repo.ned(&[
        "notes.txt",
        "--commit",
        "m",
        "-e",
        "replace \"one\" with \"1\"",
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("a merge is in progress"),
        "{}",
        stderr(&out)
    );
    assert_eq!(repo.read("notes.txt"), NOTES);
}

#[test]
fn a_staged_new_files_unstaged_changes_stay_out() {
    let repo = Repo::new(&[("notes.txt", NOTES)]);
    repo.write(&[("new.txt", "a\nb\nc\n")]);
    repo.git(&["add", "new.txt"]);
    repo.write(&[("new.txt", "a\nb\nc\nSECRET\n")]);
    let out = repo.ned(&["new.txt", "--commit", "m", "-e", "replace 1 with \"A\""]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(repo.git(&["show", "HEAD:new.txt"]), "A\nb\nc\n");
    assert_eq!(repo.git(&["show", ":new.txt"]), "A\nb\nc\n");
    assert_eq!(repo.git(&["status", "--porcelain"]), " M new.txt\n");
}

#[test]
fn an_intent_to_add_file_is_added_whole() {
    let repo = Repo::new(&[("notes.txt", NOTES)]);
    repo.write(&[("new.txt", "a\nb\n")]);
    repo.git(&["add", "-N", "new.txt"]);
    let out = repo.ned(&["new.txt", "--commit", "m", "-e", "replace 1 with \"A\""]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(repo.git(&["show", "HEAD:new.txt"]), "A\nb\n");
    assert_eq!(repo.git(&["status", "--porcelain"]), "");
}

#[test]
fn a_committed_version_that_isnt_utf8_is_refused() {
    let repo = Repo::new(&[("notes.txt", NOTES)]);
    fs::write(repo.path().join("notes.txt"), b"caf\xe9\none\n").unwrap();
    repo.git(&["commit", "-qam", "latin-1"]);
    repo.write(&[("notes.txt", "café\none\n")]);
    let head = repo.head();
    let out = repo.ned(&[
        "notes.txt",
        "--commit",
        "m",
        "-e",
        "replace \"one\" with \"1\"",
    ]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("notes.txt isn't UTF-8 text in HEAD or the index"),
        "{}",
        stderr(&out)
    );
    assert_eq!(repo.read("notes.txt"), "café\none\n");
    assert_eq!(repo.head(), head);
}

#[test]
fn commit_gpgsign_signs_and_a_failed_signature_writes_nothing() {
    let repo = Repo::new(&[("notes.txt", NOTES)]);
    let gpg = repo.home.path().join("gpg.sh");
    fs::write(&gpg, "#!/bin/sh\ntouch \"$0.called\"\nexit 1\n").unwrap();
    fs::set_permissions(&gpg, fs::Permissions::from_mode(0o755)).unwrap();
    repo.git(&["config", "commit.gpgSign", "true"]);
    repo.git(&["config", "gpg.program", &gpg.display().to_string()]);
    let head = repo.head();
    let out = repo.ned(&[
        "notes.txt",
        "--commit",
        "m",
        "-e",
        "replace \"one\" with \"1\"",
    ]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("git commit-tree failed"),
        "{}",
        stderr(&out)
    );
    assert!(repo.home.path().join("gpg.sh.called").exists());
    assert_eq!(repo.read("notes.txt"), NOTES);
    assert_eq!(repo.head(), head);
}

#[test]
fn a_failed_write_moves_head_back() {
    let repo = Repo::new(&[("notes.txt", NOTES)]);
    fs::create_dir(repo.path().join("sub")).unwrap();
    repo.write(&[("sub/notes.txt", NOTES)]);
    repo.git(&["add", "."]);
    repo.git(&["commit", "-qm", "sub"]);
    let head = repo.head();
    let sub = repo.path().join("sub");
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o555)).unwrap();
    let out = repo.ned(&[
        "sub/notes.txt",
        "--commit",
        "m",
        "-e",
        "replace \"one\" with \"1\"",
    ]);
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("cannot write files"),
        "{}",
        stderr(&out)
    );
    assert_eq!(repo.head(), head);
    assert_eq!(repo.read("sub/notes.txt"), NOTES);
    assert_eq!(repo.git(&["status", "--porcelain"]), "");
}

#[test]
fn a_detached_head_moves_alone() {
    let repo = Repo::new(&[("notes.txt", NOTES)]);
    let main = repo.head();
    repo.git(&["checkout", "-q", "--detach"]);
    let out = repo.ned(&[
        "notes.txt",
        "--commit",
        "m",
        "-e",
        "replace \"one\" with \"1\"",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(repo.git(&["rev-parse", "main"]), main);
    assert_eq!(repo.git(&["rev-parse", "HEAD~1"]), main);
    assert_eq!(repo.git(&["rev-parse", "--abbrev-ref", "HEAD"]), "HEAD\n");
}

#[test]
fn the_formatted_text_is_committed() {
    let repo = Repo::new(&[
        ("a.rs", "fn f() {\n    a();\n}\n"),
        (".ned.toml", "[format]\nrust = [\"./fmt.sh\"]\n"),
        ("fmt.sh", "#!/bin/sh\nsed 's/;;/;/'\n"),
    ]);
    fs::set_permissions(
        repo.path().join("fmt.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let out = repo.ned(&[
        "a.rs",
        "--commit",
        "m",
        "-e",
        "replace \"a();\" with \"b();;\"",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(repo.git(&["show", "HEAD:a.rs"]), "fn f() {\n    b();\n}\n");
    assert_eq!(repo.git(&["status", "--porcelain", "a.rs"]), "");
}

#[test]
fn outside_a_repository_is_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("notes.txt"), NOTES).unwrap();
    let mut cmd = cargo_bin_cmd!("ned");
    let out = cmd
        .envs(isolated(home.path()))
        .env("GIT_CEILING_DIRECTORIES", dir.path().parent().unwrap())
        .args([
            "notes.txt",
            "--commit",
            "m",
            "-e",
            "replace \"one\" with \"1\"",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("--commit needs a git repository"),
        "{}",
        stderr(&out)
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
        NOTES
    );
}

impl Repo {
    /// Runs `ned ARGS` in session `s`.
    fn ned_in_session(&self, args: &[&str]) -> Output {
        let args: Vec<&str> = ["-s", "s"].iter().chain(args).copied().collect();
        let out = self.ned(&args);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", stderr(&out));
        out
    }

    fn committed(&self, rev: &str) -> String {
        self.git(&["show", "--format=%s", "--name-only", rev])
    }
}

#[test]
fn a_session_commits_its_edits_since_its_last_commit() {
    let repo = Repo::new(&[("notes.txt", NOTES), ("other.txt", "x\n")]);
    repo.ned_in_session(&["notes.txt", "-e", "replace \"one\" with \"1\""]);
    repo.ned_in_session(&["other.txt", "-e", "replace \"x\" with \"y\""]);
    repo.ned_in_session(&[
        "notes.txt",
        "--commit",
        "First",
        "-e",
        "replace \"two\" with \"2\"",
    ]);
    assert_eq!(repo.committed("HEAD"), "First\n\nnotes.txt\nother.txt\n");
    assert_eq!(
        repo.git(&["show", "HEAD:notes.txt"]),
        "1\n2\nthree\nfour\nfive\n"
    );
    repo.ned_in_session(&[
        "notes.txt",
        "--commit",
        "Second",
        "-e",
        "replace \"three\" with \"3\"",
    ]);
    assert_eq!(
        repo.git(&["diff", "--stat", "HEAD~1", "HEAD"])
            .lines()
            .count(),
        2
    );
    assert_eq!(repo.git(&["status", "--porcelain"]), "");
    let history = stdout(&repo.ned(&["history", "-s", "s"]));
    let committed: Vec<&str> = history
        .lines()
        .filter(|l| l.contains(", commit "))
        .collect();
    assert_eq!(committed.len(), 2, "{history}");
}

#[test]
fn a_session_leaves_undone_edits_out() {
    let repo = Repo::new(&[("notes.txt", NOTES)]);
    repo.ned_in_session(&["notes.txt", "-e", "replace \"one\" with \"1\""]);
    repo.ned_in_session(&["-e", "create new.txt \"x\""]);
    assert_eq!(repo.ned(&["undo", "-s", "s"]).status.code(), Some(0));
    assert_eq!(repo.ned(&["undo", "-s", "s"]).status.code(), Some(0));
    repo.ned_in_session(&[
        "notes.txt",
        "--commit",
        "Only two",
        "-e",
        "replace \"two\" with \"2\"",
    ]);
    assert_eq!(repo.committed("HEAD"), "Only two\n\nnotes.txt\n");
    assert_eq!(
        repo.git(&["show", "HEAD:notes.txt"]),
        "one\n2\nthree\nfour\nfive\n"
    );
}

#[test]
fn a_staged_mode_change_stays_staged() {
    let repo = Repo::new(&[("notes.txt", NOTES)]);
    let notes = repo.path().join("notes.txt");
    fs::set_permissions(&notes, fs::Permissions::from_mode(0o755)).unwrap();
    repo.git(&["add", "notes.txt"]);
    let out = repo.ned(&[
        "notes.txt",
        "--commit",
        "m",
        "-e",
        "replace \"one\" with \"1\"",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let mode = |listed: String| listed.split(' ').next().unwrap().to_string();
    assert_eq!(mode(repo.git(&["ls-tree", "HEAD", "notes.txt"])), "100644");
    assert_eq!(mode(repo.git(&["ls-files", "-s", "notes.txt"])), "100755");
    assert_eq!(repo.git(&["status", "--porcelain"]), "M  notes.txt\n");
}

#[test]
fn a_locked_index_writes_nothing() {
    let repo = Repo::new(&[("notes.txt", NOTES)]);
    let lock = repo.path().join(".git/index.lock");
    fs::write(&lock, "").unwrap();
    let head = repo.head();
    let out = repo.ned(&[
        "notes.txt",
        "--commit",
        "m",
        "-e",
        "replace \"one\" with \"1\"",
    ]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("index.lock exists"),
        "{}",
        stderr(&out)
    );
    assert!(lock.exists());
    fs::remove_file(&lock).unwrap();
    assert_eq!(repo.head(), head);
    assert_eq!(repo.read("notes.txt"), NOTES);
    assert_eq!(repo.git(&["status", "--porcelain"]), "");
}

#[test]
fn the_repository_is_the_first_files() {
    let repo = Repo::new(&[("notes.txt", NOTES)]);
    let elsewhere = tempfile::tempdir().unwrap();
    let notes = repo.path().join("notes.txt").display().to_string();
    let out = cargo_bin_cmd!("ned")
        .envs(isolated(repo.home.path()))
        .env(
            "GIT_CEILING_DIRECTORIES",
            elsewhere.path().parent().unwrap(),
        )
        .env("XDG_STATE_HOME", repo.home.path())
        .args([&notes, "--commit", "m", "-e", "replace \"one\" with \"1\""])
        .current_dir(elsewhere.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(repo.git(&["log", "-1", "--format=%s"]), "m\n");
}

#[test]
fn a_file_in_a_nested_repository_is_refused() {
    let repo = Repo::new(&[("notes.txt", NOTES), (".gitignore", "inner/\n")]);
    let inner = repo.path().join("inner");
    fs::create_dir(&inner).unwrap();
    repo.git(&["-C", "inner", "init", "-q"]);
    repo.write(&[("inner/f.txt", "x\n")]);
    repo.git(&["-C", "inner", "add", "f.txt"]);
    repo.git(&["-C", "inner", "commit", "-qm", "inner"]);
    let head = repo.head();
    let out = repo.ned(&[
        "notes.txt",
        "inner/f.txt",
        "--commit",
        "m",
        "-e",
        "replace all /^(one|x)$/ with \"1\"",
    ]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("f.txt isn't in the repository at"),
        "{}",
        stderr(&out)
    );
    assert_eq!(repo.head(), head);
    assert_eq!(repo.read("inner/f.txt"), "x\n");
    let out = repo.ned(&["inner/f.txt", "--commit", "m", "-e", "replace 1 with \"y\""]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(repo.git(&["-C", "inner", "show", "HEAD:f.txt"]), "y\n");
    assert_eq!(repo.head(), head);
}

#[test]
fn a_file_in_a_submodule_is_refused() {
    let repo = Repo::new(&[("notes.txt", NOTES)]);
    let sub = Repo::new(&[("f.txt", "x\n")]);
    let url = sub.path().display().to_string();
    repo.git(&[
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "add",
        "-q",
        &url,
        "sub",
    ]);
    repo.git(&["commit", "-qm", "sub"]);
    let head = repo.head();
    let out = repo.ned(&[
        "notes.txt",
        "sub/f.txt",
        "--commit",
        "m",
        "-e",
        "replace all /^(one|x)$/ with \"1\"",
    ]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("f.txt isn't in the repository at"),
        "{}",
        stderr(&out)
    );
    assert_eq!(repo.head(), head);
    assert_eq!(repo.read("sub/f.txt"), "x\n");
}

#[test]
fn a_session_script_that_changes_nothing_commits_nothing() {
    let repo = Repo::new(&[("notes.txt", NOTES)]);
    repo.ned_in_session(&["notes.txt", "-e", "replace \"one\" with \"1\""]);
    let head = repo.head();
    let out = repo.ned(&["-s", "s", "notes.txt", "--commit", "m", "-e", "show 1"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("nothing to commit"),
        "{}",
        stderr(&out)
    );
    assert_eq!(repo.head(), head);
}

#[test]
fn a_session_edit_no_longer_on_disk_is_refused() {
    let repo = Repo::new(&[("notes.txt", NOTES), ("other.txt", "x\n")]);
    repo.ned_in_session(&["notes.txt", "-e", "replace \"one\" with \"1\""]);
    repo.git(&["checkout", "--", "notes.txt"]);
    let head = repo.head();
    let out = repo.ned(&[
        "-s",
        "s",
        "other.txt",
        "--commit",
        "m",
        "-e",
        "replace \"x\" with \"y\"",
    ]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("notes.txt changed since session entry 1 wrote it"),
        "{}",
        stderr(&out)
    );
    assert_eq!(repo.head(), head);
    assert_eq!(repo.read("other.txt"), "x\n");
    assert_eq!(repo.git(&["status", "--porcelain"]), "");
}

#[test]
fn a_session_edit_git_cant_commit_names_its_entry() {
    let repo = Repo::new(&[("notes.txt", NOTES), (".gitignore", "*.log\n")]);
    let elsewhere = tempfile::tempdir().unwrap();
    let outside = elsewhere.path().join("outside.txt");
    fs::write(&outside, "x\n").unwrap();
    let outside = outside.display().to_string();
    repo.write(&[("build.log", "x\n")]);
    for (session, file, problem) in [
        ("ignored", "build.log", "build.log is ignored by git"),
        (
            "outside",
            outside.as_str(),
            "outside.txt isn't in the repository at",
        ),
    ] {
        let edit = |args: &[&str]| {
            let args: Vec<&str> = ["-s", session].iter().chain(args).copied().collect();
            repo.ned(&args)
        };
        let out = edit(&[file, "-e", "replace \"x\" with \"y\""]);
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
        let head = repo.head();
        let commit = [
            "notes.txt",
            "--commit",
            "m",
            "-e",
            "replace \"one\" with \"1\"",
        ];
        let out = edit(&commit);
        assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
        for expected in [problem, ", and session entry 1 edited it"] {
            assert!(stderr(&out).contains(expected), "{}", stderr(&out));
        }
        assert!(
            stderr(&out).contains("start a new session"),
            "{}",
            stderr(&out)
        );
        assert_eq!(repo.head(), head);
        assert_eq!(repo.read("notes.txt"), NOTES);
    }
    let out = repo.ned(&[
        "-s",
        "fresh",
        "notes.txt",
        "--commit",
        "m",
        "-e",
        "replace \"one\" with \"1\"",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
}
