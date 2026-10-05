//! End-to-end tests of the REPL (command-language spec §1.4), its input piped.

use std::fs;
use std::path::PathBuf;
use std::process::Output;

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
            .current_dir(self.dir.path())
            .env("XDG_STATE_HOME", self.state.path())
            .env("XDG_CONFIG_HOME", self.config.path())
            .env("XDG_RUNTIME_DIR", self.runtime.path())
            .env_remove("NED_SESSION")
            .write_stdin(input)
            .output()
            .unwrap()
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
            "error: the input ended with unwritten edits to a.rs, which were discarded\n"
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
    let input = "replace fn:a.name with \"c\"\n:d\n:frob\n:wri\n:q\n";
    let out = ws.repl(&["a.rs"], input);
    assert!(out.starts_with("exit: 0\n"), "{out}");
    assert!(
        out.contains("error: `:d` could be :diff or :detach\n"),
        "{out}"
    );
    assert!(
        out.contains("error: unknown command `:frob`; commands are :write :commit :undo :diff :reload :files :history :attach :detach :help :quit :wq\n"),
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
