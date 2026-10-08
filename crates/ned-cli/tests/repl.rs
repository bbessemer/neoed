//! End-to-end tests of the REPL (command-language spec §1.4), its input piped.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use assert_cmd::cargo::cargo_bin_cmd;
use insta::assert_snapshot;
use tempfile::TempDir;

/// A git workspace with private state, config and runtime dirs.
struct Workspace {
    dir: TempDir,
    state: TempDir,
    config: TempDir,
    runtime: TempDir,
}

impl Workspace {
    fn new(files: &[(&str, &str)]) -> Workspace {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        for (name, text) in files {
            fs::write(dir.path().join(name), text).unwrap();
        }
        let config = tempfile::tempdir().unwrap();
        fs::create_dir(config.path().join("ned")).unwrap();
        fs::write(
            config.path().join("ned/config.toml"),
            "[format]\nrust = false\n",
        )
        .unwrap();
        Workspace {
            dir,
            state: tempfile::tempdir().unwrap(),
            config,
            runtime: tempfile::tempdir().unwrap(),
        }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap()
    }

    /// Runs `ned ARGS` in the workspace with `input` on stdin.
    fn ned(&self, args: &[&str], input: &str) -> Output {
        cargo_bin_cmd!("ned")
            .args(args)
            .envs(self.env())
            .current_dir(self.dir.path())
            .env_remove("NED_SESSION")
            .write_stdin(input)
            .output()
            .unwrap()
    }

    /// `ned`'s environment: private dirs, and git without user config.
    fn env(&self) -> Vec<(&'static str, PathBuf)> {
        let home = self.state.path();
        vec![
            ("XDG_STATE_HOME", home.to_path_buf()),
            ("XDG_CONFIG_HOME", self.config.path().to_path_buf()),
            ("XDG_RUNTIME_DIR", self.runtime.path().to_path_buf()),
            ("GIT_CONFIG_GLOBAL", home.join("gitconfig")),
            ("GIT_CONFIG_NOSYSTEM", "1".into()),
            ("GIT_AUTHOR_NAME", "A U Thor".into()),
            ("GIT_AUTHOR_EMAIL", "author@example.com".into()),
            ("GIT_COMMITTER_NAME", "C O Mitter".into()),
            ("GIT_COMMITTER_EMAIL", "committer@example.com".into()),
        ]
    }

    /// Runs `git ARGS` in the workspace and returns its stdout.
    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .envs(self.env())
            .current_dir(self.dir.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {}", report(&output));
        String::from_utf8(output.stdout).unwrap()
    }

    /// A git repository, its `.git` a real one, with one commit of its files.
    fn repo(files: &[(&str, &str)]) -> Workspace {
        let ws = Workspace::new(files);
        fs::remove_dir(ws.dir.path().join(".git")).unwrap();
        ws.git(&["init", "-q", "-b", "main"]);
        ws.git(&["add", "."]);
        ws.git(&["commit", "-q", "-m", "init"]);
        ws
    }

    /// `ned repl ARGS` given `input`: its exit code, stdout and stderr, with
    /// the workspace's path replaced by `{dir}`.
    fn repl(&self, args: &[&str], input: &str) -> String {
        let args: Vec<&str> = ["repl"].iter().chain(args).copied().collect();
        report(&self.ned(&args, input)).replace(self.root().to_str().unwrap(), "{dir}")
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.dir.path().join(name)).unwrap()
    }
}

fn report(output: &Output) -> String {
    format!(
        "exit: {}\n--- stdout\n{}--- stderr\n{}",
        output.status.code().unwrap(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    )
}

const AB: &str = "fn a() {}\nfn b() {}\n";

#[test]
fn edits_stay_in_buffers_until_written() {
    let ws = Workspace::new(&[("a.rs", AB)]);
    let input = "replace fn:a.name with \"c\"\nshow 1\n:write\n:quit\n";
    assert_snapshot!(ws.repl(&["a.rs"], input), @r"
    exit: 0
    --- stdout
    a.rs: 1 edit, +1 -1
    @@ -1,2 +1,2 @@
    -fn a() {}
    +fn c() {}
     fn b() {}
    a.rs:1
    1:fn c() {}
    a.rs: written, +1 -1
    --- stderr
    note: recording in session repl-1
    ");
    assert_eq!(ws.read("a.rs"), "fn c() {}\nfn b() {}\n");
}

#[test]
fn a_file_reached_by_two_paths_has_one_buffer() {
    let ws = Workspace::new(&[("t.txt", "l1\nl2\nl3\n")]);
    fs::create_dir(ws.dir.path().join("sub")).unwrap();
    let input = "replace 1 with \"X1\"\nfile sub/../t.txt; replace 3 with \"X3\"\n:files\n:write\n";
    let out = ws.repl(&["t.txt", "--no-fmt"], input);
    assert!(out.starts_with("exit: 0\n"), "{out}");
    assert_eq!(ws.read("t.txt"), "X1\nl2\nX3\n");
}

#[test]
fn quit_is_refused_with_unwritten_edits_and_quit_bang_discards_them() {
    let ws = Workspace::new(&[("a.rs", AB)]);
    let input = "replace fn:a.name with \"c\"\n:quit\n:diff\n:quit!\n";
    assert_snapshot!(ws.repl(&["a.rs", "--context", "0"], input), @r"
    exit: 0
    --- stdout
    a.rs: 1 edit, +1 -1
    @@ -1,1 +1,1 @@
    -fn a() {}
    +fn c() {}
    a.rs: 1 edit, +1 -1
    @@ -1,1 +1,1 @@
    -fn a() {}
    +fn c() {}
    --- stderr
    note: recording in session repl-1
    error: unwritten edits to a.rs; `:write` them, or `:quit!` to discard them
    ");
    assert_eq!(ws.read("a.rs"), AB);
}

#[test]
fn input_that_ends_with_unwritten_edits_exits_1() {
    let ws = Workspace::new(&[("a.rs", AB)]);
    let out = ws.repl(&["a.rs", "-q"], "replace fn:a.name with \"c\"\n");
    assert!(out.starts_with("exit: 2\n"), "{out}");
    let out = ws.repl(&["a.rs"], "replace fn:a.name with \"c\"\n");
    assert!(out.starts_with("exit: 1\n"), "{out}");
    assert!(
        out.ends_with(
            "error: the input ended with unwritten edits to a.rs, which were discarded; end it with `:write` to write them, or `:quit!` to discard them\n"
        ),
        "{out}"
    );
    assert_eq!(ws.read("a.rs"), AB);
    assert!(ws.repl(&["a.rs"], "show 1\n").starts_with("exit: 0\n"));
}

#[test]
fn undo_walks_back_script_by_script_and_past_a_write() {
    let ws = Workspace::new(&[("a.rs", AB)]);
    let input = "\
replace fn:a.name with \"c\"
replace fn:b.name with \"d\"
:write
:undo
:undo
:undo
:write
";
    assert_snapshot!(ws.repl(&["a.rs", "--context", "0", "-s", "s"], input), @r"
    exit: 0
    --- stdout
    a.rs: 1 edit, +1 -1
    @@ -1,1 +1,1 @@
    -fn a() {}
    +fn c() {}
    a.rs: 1 edit, +1 -1
    @@ -2,1 +2,1 @@
    -fn b() {}
    +fn d() {}
    a.rs: written, +2 -2
    a.rs: 1 edit, +1 -1
    @@ -2,1 +2,1 @@
    -fn d() {}
    +fn b() {}
    a.rs: 1 edit, +1 -1
    @@ -1,1 +1,1 @@
    -fn c() {}
    +fn a() {}
    a.rs: written, +2 -2
    --- stderr
    note: recording in session s
    error: nothing to undo; `:history` lists the session's entries, which `ned undo` reverts
    ");
    assert_eq!(ws.read("a.rs"), AB);
}

#[test]
fn a_heredoc_continues_on_the_next_lines() {
    let ws = Workspace::new(&[("a.rs", AB)]);
    let input = "insert after fn:b <<END\nfn e() {\n}\nEND\n:wq\n";
    let out = ws.repl(&["a.rs"], input);
    assert!(out.starts_with("exit: 0\n"), "{out}");
    assert_eq!(ws.read("a.rs"), format!("{AB}fn e() {{\n}}\n"));
}

#[test]
fn commands_take_any_unambiguous_prefix() {
    let ws = Workspace::new(&[("a.rs", AB)]);
    let input = "replace fn:a.name with \"c\"\n:d\n:frob\n:files -x\n:attach\n:wri\n:q\n";
    let out = ws.repl(&["a.rs"], input);
    assert!(out.starts_with("exit: 0\n"), "{out}");
    assert!(
        out.contains("error: ambiguous command `:d`; write :diff or :detach\n"),
        "{out}"
    );
    assert!(
        out.contains("error: unknown command `:frob`; commands are :write :commit :undo :diff :reload :files :history :attach :detach :help :quit :wq\n"),
        "{out}"
    );
    assert!(
        out.contains("error: `:files` takes FILE... or -w, the REPL's workspace; start another REPL for a workspace other than "),
        "{out}"
    );
    assert!(
        out.contains("error: `:attach` takes a session's name; give one: `:attach NAME`\n"),
        "{out}"
    );
    assert_eq!(ws.read("a.rs"), "fn c() {}\nfn b() {}\n");
}

#[test]
fn reload_drops_unwritten_edits_and_files_lists_them() {
    let ws = Workspace::new(&[("a.rs", AB), ("b.rs", AB)]);
    let input = "\
replace all fn:a.name with \"c\"
:files
:reload b.rs
:files
:reload b.rs
:quit!
";
    let out = ws.repl(&["a.rs", "b.rs"], input);
    assert_snapshot!(out, @r"
    exit: 0
    --- stdout
    a.rs: 1 edit, +1 -1
    @@ -1,2 +1,2 @@
    -fn a() {}
    +fn c() {}
     fn b() {}
    b.rs: 1 edit, +1 -1
    @@ -1,2 +1,2 @@
    -fn a() {}
    +fn c() {}
     fn b() {}
    files: a.rs b.rs
    a.rs: unwritten, +1 -1
    b.rs: unwritten, +1 -1
    b.rs: reloaded
    files: a.rs b.rs
    a.rs: unwritten, +1 -1
    --- stderr
    note: recording in session repl-1
    error: b.rs has no unwritten edits; `:files` lists the buffers that have
    ");
}

#[test]
fn the_repl_records_scripts_and_writes_in_its_session() {
    let ws = Workspace::new(&[("a.rs", AB)]);
    let input = "replace fn:z.name with \"c\"\n!!:s/z/a/\n:history\n:write\n";
    let out = ws.repl(&["a.rs"], input);
    let history = "1 exit 1: replace fn:z.name with \"c\"\n2 ok: replace fn:a.name with \"c\"\n";
    assert!(out.contains(history), "{out}");
    assert!(
        out.contains("note: repeating 1: replace fn:a.name with \"c\"\n"),
        "{out}"
    );
    let logged = ws.ned(&["history", "-s", "repl-1"], "");
    assert_eq!(
        String::from_utf8_lossy(&logged.stdout),
        format!("{history}3 write, 1 file\n")
    );
    assert_eq!(ws.read("a.rs"), "fn c() {}\nfn b() {}\n");

    let undo = ws.ned(&["undo", "-s", "repl-1"], "");
    assert!(undo.status.success(), "{}", report(&undo));
    assert!(
        String::from_utf8_lossy(&undo.stdout).starts_with("undo 3: write\n"),
        "{}",
        report(&undo)
    );
    assert_eq!(ws.read("a.rs"), AB);
    assert!(
        ws.repl(&["a.rs"], "")
            .contains("note: recording in session repl-2\n")
    );
}

#[test]
fn help_prints_a_topic() {
    let ws = Workspace::new(&[("a.rs", AB)]);
    let out = ws.repl(&["a.rs"], ":help repl\n:help nope\n");
    assert!(out.contains("--- stdout\nrepl\n\n  ned repl"), "{out}");
    assert!(
        out.contains("error: unknown topic `nope`; topics are show"),
        "{out}"
    );
}

#[test]
fn attach_prints_the_history_and_records_into_the_session() {
    let ws = Workspace::new(&[("a.rs", AB)]);
    ws.ned(&["-s", "agent", "a.rs", "-e", "show 1"], "");
    let input = ":attach agent\nreplace fn:a.name with \"c\"\n:write\n:detach\nshow 1\n";
    let out = ws.repl(&["a.rs"], input);
    assert!(
        out.starts_with("exit: 0\n--- stdout\n1 ok: show 1\n"),
        "{out}"
    );
    let history = ws.ned(&["history", "-s", "agent"], "");
    assert_eq!(
        String::from_utf8_lossy(&history.stdout),
        "1 ok: show 1\n2 ok: replace fn:a.name with \"c\"\n3 write, 1 file\n"
    );
    let own = ws.ned(&["history", "-s", "repl-1"], "");
    assert_eq!(String::from_utf8_lossy(&own.stdout), "1 ok: show 1\n");

    let out = ws.repl(&["--attach", "agent", "a.rs"], "");
    assert!(out.contains("--- stdout\n1 ok: show 1\n2 ok"), "{out}");
}

#[test]
fn attach_needs_a_session_the_workspace_has() {
    let ws = Workspace::new(&[("a.rs", AB)]);
    ws.ned(&["-s", "agent", "a.rs", "-e", "show 1"], "");
    let out = ws.repl(&["a.rs"], ":attach nope\n:detach\n");
    assert!(
        out.contains("error: the workspace has no session nope; its sessions are agent, repl-1\n"),
        "{out}"
    );
    assert!(
        out.contains("error: not attached to a session; `:attach NAME` follows one\n"),
        "{out}"
    );
}

/// A running REPL, killed when dropped, whose stdout is read on a thread so a
/// missing line fails the test instead of hanging it.
struct Running {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
}

impl Running {
    fn new(ws: &Workspace, args: &[&str]) -> Running {
        let mut child = Command::new(env!("CARGO_BIN_EXE_ned"))
            .arg("repl")
            .args(args)
            .envs(ws.env())
            .current_dir(ws.dir.path())
            .env_remove("NED_SESSION")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        Running {
            stdin: child.stdin.take(),
            child,
            lines,
        }
    }

    fn input(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{line}").unwrap();
    }

    /// The lines of stdout up to and including `last`, waiting at most 10s.
    fn until(&self, last: &str) -> Vec<String> {
        let mut lines = Vec::new();
        while lines.last().is_none_or(|line| line != last) {
            match self.lines.recv_timeout(Duration::from_secs(10)) {
                Ok(line) => lines.push(line),
                Err(_) => panic!("no line `{last}` within 10s; got {lines:?}"),
            }
        }
        lines
    }

    /// Ends the input and returns the rest of stdout, and stderr.
    fn finish(mut self) -> (Vec<String>, String) {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "the REPL didn't exit within 10s");
            thread::sleep(Duration::from_millis(20));
        }
        let mut stderr = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        (self.lines.iter().collect(), stderr)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[test]
fn an_attached_repl_prints_each_entry_the_session_records() {
    let ws = Workspace::new(&[("a.rs", AB)]);
    ws.ned(&["-s", "agent", "a.rs", "-e", "show 1"], "");
    let mut repl = Running::new(&ws, &["a.rs", "--context", "0"]);
    repl.input(":attach agent");
    repl.until("1 ok: show 1");
    // The agent edits a file while the REPL's buffer of it has unwritten
    // edits, which the REPL recorded in the agent's session as entry 2.
    repl.input("replace fn:b.name with \"x\"");
    repl.until("+fn x() {}");
    let agent = ws.ned(
        &["-s", "agent", "a.rs", "-e", "replace fn:a.name with \"c\""],
        "",
    );
    assert!(agent.status.success(), "{}", report(&agent));
    // The REPL merges the agent's edit into its buffer: the next script sees
    // both.
    repl.input("show 1");
    repl.input(":write");
    let (rest, stderr) = repl.finish();
    assert_eq!(
        rest,
        [
            "agent 3 ok, 1 file: replace fn:a.name with \"c\"",
            "a.rs: 1 edit, +1 -1",
            "@@ -1,1 +1,1 @@",
            "-fn a() {}",
            "+fn c() {}",
            "a.rs:1",
            "1:fn c() {}",
            "a.rs: written, +1 -1",
        ]
    );
    assert!(
        stderr.contains("note: merged the change into your unwritten edits to a.rs\n"),
        "{stderr}"
    );
    assert_eq!(ws.read("a.rs"), "fn c() {}\nfn x() {}\n");
}

#[test]
fn an_attached_repl_prints_an_entrys_comment() {
    let ws = Workspace::new(&[("a.rs", AB)]);
    ws.ned(&["-s", "agent", "a.rs", "-e", "show 1"], "");
    let mut repl = Running::new(&ws, &["a.rs", "--context", "0"]);
    repl.input(":attach agent");
    repl.until("1 ok: show 1");
    let arguments = serde_json::json!({
        "script": "replace fn:a.name with \"c\"",
        "files": ["a.rs"],
        "comment": "Rename a\nfor clarity",
    });
    let call = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "ned", "arguments": arguments },
    });
    let agent = ws.ned(&["mcp", "-s", "agent"], &format!("{call}\n"));
    assert!(agent.status.success(), "{}", report(&agent));
    repl.input("show 1");
    let (rest, _) = repl.finish();
    assert_eq!(
        rest,
        [
            "agent 2 ok, 1 file: replace fn:a.name with \"c\"",
            "# Rename a",
            "# for clarity",
            "a.rs: 1 edit, +1 -1",
            "@@ -1,1 +1,1 @@",
            "-fn a() {}",
            "+fn c() {}",
            "a.rs:1",
            "1:fn c() {}",
        ]
    );
}

#[test]
fn a_followed_change_that_overlaps_unwritten_edits_is_left_out() {
    let ws = Workspace::new(&[("a.rs", AB)]);
    ws.ned(&["-s", "agent", "a.rs", "-e", "show 1"], "");
    let mut repl = Running::new(&ws, &["a.rs", "--context", "0"]);
    repl.input(":attach agent");
    repl.until("1 ok: show 1");
    repl.input("replace fn:a.name with \"x\"");
    repl.until("+fn x() {}");
    let agent = ws.ned(
        &["-s", "agent", "a.rs", "-e", "replace fn:a.name with \"c\""],
        "",
    );
    assert!(agent.status.success(), "{}", report(&agent));
    repl.input("show 1");
    repl.input(":quit!");
    let (rest, stderr) = repl.finish();
    assert!(
        rest.ends_with(&["a.rs:1".to_string(), "1:fn x() {}".to_string()]),
        "{rest:?}"
    );
    assert!(
        stderr.contains("note: a.rs:1: the edit overlaps a change made to the file since"),
        "{stderr}"
    );
}

#[test]
fn diff_shows_what_write_would_write() {
    let ws = Workspace::new(&[("t.txt", "l1\nl2\nl3\nl4\nl5\n")]);
    let mut repl = Running::new(&ws, &["t.txt", "--no-fmt", "--context", "0"]);
    repl.input("replace 1 with \"X1\"");
    repl.until("+X1");
    fs::write(ws.dir.path().join("t.txt"), "l1\nl2\nl3\nl4\nY5\n").unwrap();
    repl.input(":diff");
    repl.input("show 1");
    let merged = repl.until("1:X1");
    assert!(merged.contains(&"+X1".to_string()), "{merged:?}");
    assert!(!merged.iter().any(|l| l.contains("Y5")), "{merged:?}");
    fs::write(ws.dir.path().join("t.txt"), "Z1\nl2\nl3\nl4\nY5\n").unwrap();
    repl.input(":diff");
    repl.input(":quit!");
    let (forced, stderr) = repl.finish();
    assert!(forced.contains(&"-Y5".to_string()), "{forced:?}");
    assert!(
        stderr.contains("note: t.txt:1: the edit overlaps a change made to the file since"),
        "{stderr}"
    );
    assert!(
        stderr.contains("since; this is what `:write!` writes"),
        "{stderr}"
    );
}

#[test]
fn commit_writes_then_commits_the_sessions_edits() {
    let ws = Workspace::repo(&[("a.rs", AB), ("b.rs", AB)]);
    let input = "\
replace file:a.rs>fn:a.name with \"c\"
:write a.rs
replace file:b.rs>fn:b.name with \"d\"
:commit Rename a and b
:commit Again
";
    let out = ws.repl(&["a.rs", "b.rs"], input);
    assert!(out.starts_with("exit: 0\n"), "{out}");
    let sha = ws.git(&["rev-parse", "--short=7", "HEAD"]);
    assert!(
        out.contains(&format!(
            "b.rs: written, +1 -1\ncommit {}: Rename a and b\n",
            sha.trim()
        )),
        "{out}"
    );
    assert!(
        out.contains("error: nothing to commit: the edits leave every file as HEAD has it; edit a file, then `:commit MSG`\n"),
        "{out}"
    );
    assert_eq!(ws.git(&["log", "-1", "--format=%s"]), "Rename a and b\n");
    assert_eq!(
        ws.git(&["show", "--stat", "--format=", "HEAD"])
            .lines()
            .count(),
        3,
        "both files and a summary"
    );
    let history = ws.ned(&["history", "-s", "repl-1"], "");
    let history = String::from_utf8_lossy(&history.stdout);
    assert!(
        history.ends_with(&format!("4 write, 1 file, commit {}\n", sha.trim())),
        "{history}"
    );
}

#[test]
fn a_refused_commit_writes_nothing() {
    let ws = Workspace::new(&[("a.rs", AB)]);
    let input = "replace fn:a.name with \"c\"\n:commit\n:commit Rename\n:quit\n:quit!\n";
    let out = ws.repl(&["a.rs"], input);
    assert!(
        out.contains("error: `:commit` needs a message; give one: `:commit MSG`\n"),
        "{out}"
    );
    assert!(
        out.contains(
            "isn't in a git repository; run git init, or use `:write` in place of `:commit`"
        ),
        "{out}"
    );
    assert!(out.contains("error: unwritten edits to a.rs"), "{out}");
    assert_eq!(ws.read("a.rs"), AB);
}

#[test]
fn a_commit_names_a_changed_file_relative_to_the_repl() {
    let ws = Workspace::repo(&[("a.rs", AB)]);
    let out = ws.ned(
        &["-s", "s1", "a.rs", "-e", "replace fn:a.name with \"c\""],
        "",
    );
    assert!(out.status.success(), "{}", report(&out));
    fs::write(ws.dir.path().join("a.rs"), AB).unwrap();
    let out = ws.repl(&["-s", "s1"], ":commit Rename\n");
    assert!(
        out.contains("error: a.rs changed since session entry 1 wrote it"),
        "{out}"
    );
}
