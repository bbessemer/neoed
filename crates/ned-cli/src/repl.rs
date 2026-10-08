//! `ned repl`: a human edits interactively, in memory until written
//! (command-language spec §1.4).

use std::collections::HashSet;
use std::fmt;
use std::io::{self, BufRead, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use clap::Args;
use ned_core::apply::{self, Committed, Finished, Render, Settings};
use ned_core::buffers::{Buffers, BuffersError, BuffersErrorKind};
use ned_core::diff::{self, DiffStat};
use ned_core::exec::{Change, Initial, Options};
use ned_core::format::Outcome;
use ned_core::git::Repo;
use ned_core::hint::{self, Errors, Fix, Frontend, Hint, Note, Report};
use ned_core::invoke::{self, Edited};
use ned_core::lang::{self, Language};
use ned_core::lsp::{Document, Lsp};
use ned_core::session::{self, Entry, FileChange, Follower, Session};
use ned_core::style::Role;
use ned_core::{fs, script, workspace};
use rustyline::error::ReadlineError;
use rustyline::{DefaultEditor, ExternalPrinter};

use crate::error::{self, UsageError};
use crate::{Cli, LangFlag, Terminal, daemon, styles};

/// The REPL's arguments: the file set and the flags it shares with scripts.
#[derive(Args, Debug, Default)]
pub struct ReplArgs {
    /// Files to edit: the initial file set.
    files: Vec<String>,
    /// Start with every file in the workspace, DIR or the one containing the
    /// working directory, instead of FILES.
    #[arg(short, long, value_name = "DIR", num_args = 0..=1, conflicts_with = "files")]
    workspace: Option<Option<PathBuf>>,
    /// Skip the guards: apply edits even if they introduce syntax errors,
    /// language-server errors or a formatter failure.
    #[arg(long)]
    force: bool,
    /// Don't run formatters.
    #[arg(long)]
    no_fmt: bool,
    /// Don't check edits with language servers.
    #[arg(long)]
    no_check: bool,
    /// Use this language for every file instead of detecting it, or `text` to
    /// parse none of them.
    #[arg(long, value_name = "LANG", value_parser = lang::parse_lang)]
    lang: Option<LangFlag>,
    /// Context lines around diff hunks.
    #[arg(long, value_name = "N", default_value_t = 1)]
    context: usize,
    /// Record into session NAME; overrides NED_SESSION.
    #[arg(short, long, value_name = "NAME")]
    session: Option<String>,
    /// Follow session NAME, an agent's, say, and record into it.
    #[arg(long, value_name = "NAME")]
    attach: Option<String>,
}

impl ReplArgs {
    /// The REPL's arguments for bare `ned` on a terminal, or a usage error for
    /// a flag only scripts take.
    pub fn from_cli(cli: Cli) -> Result<ReplArgs, hint::Error<UsageError>> {
        let script_only = [
            (cli.dry_run, "-n"),
            (cli.quiet, "-q"),
            (cli.commit.is_some(), "--commit"),
        ];
        if let Some((_, flag)) = script_only.iter().find(|(given, _)| *given) {
            return Err(UsageError::ScriptOnly(flag).into());
        }
        Ok(ReplArgs {
            files: cli.files,
            workspace: cli.workspace,
            force: cli.force,
            no_fmt: cli.no_fmt,
            no_check: cli.no_check,
            lang: cli.lang,
            context: cli.context,
            session: cli.session,
            attach: None,
        })
    }
}

/// Runs the REPL until it quits or its input ends.
pub fn run(args: ReplArgs) -> ExitCode {
    let mut repl = match Repl::new(args) {
        Ok(repl) => repl,
        Err((error, code)) => {
            errln!("{error}");
            return ExitCode::from(code);
        }
    };
    errln!(
        "{}",
        error::recording(repl.session.name()).render(Frontend::Repl)
    );
    if let Some(name) = repl.args.attach.clone()
        && let Err(errors) = repl.attach(&[name.as_str()])
    {
        errln!("{}", errors.render(Frontend::Repl, None));
        return ExitCode::from(2);
    }
    let mut input = Input::new();
    loop {
        // The printer starts on the first attach (see `Input::follow`).
        if repl.attached.is_some() {
            input.follow(&repl.follow);
        }
        let read = input.read();
        // On a terminal, a thread prints what the followed session records as
        // it does; otherwise it's printed before each input runs.
        if !input.terminal() {
            repl.poll();
        }
        let quit = match read {
            Read::Script(src) => repl.eval(&src),
            Read::Interrupted => None,
            Read::End if input.terminal() => repl.command("quit"),
            Read::End => Some(repl.end()),
        };
        if let Some(code) = quit {
            return ExitCode::from(code);
        }
    }
}

/// The commands, in the order an unknown one lists them. `:wq` is written
/// only in full, so it's not among them.
const COMMANDS: [&str; 11] = [
    "write", "commit", "undo", "diff", "reload", "files", "history", "attach", "detach", "help",
    "quit",
];

/// The command `word` names: in full, or by a prefix of only one.
fn command_name(word: &str) -> Result<&'static str, ReplError> {
    if word == "wq" {
        return Ok("wq");
    }
    let named: Vec<&str> = COMMANDS
        .into_iter()
        .filter(|c| !word.is_empty() && c.starts_with(word))
        .collect();
    let word = word.to_string();
    match named[..] {
        [name] => Ok(name),
        [] => Err(ReplError::new(ReplErrorKind::UnknownCommand(word))),
        [ref rest @ .., last] => {
            let rest: Vec<String> = rest.iter().map(|c| format!(":{c}")).collect();
            let fix = format!("write {} or :{last}", rest.join(", "));
            Err(ReplError::new(ReplErrorKind::Ambiguous(word)).with_fix(fix))
        }
    }
}

/// What a REPL command can't do.
#[derive(Debug)]
enum ReplErrorKind {
    UnknownCommand(String),
    Ambiguous(String),
    NoMessage,
    NoName,
    NoSession(String),
    NotAttached,
    FilesFlag,
    Unwritten(Vec<String>),
    /// The input ended with unwritten edits to these files.
    Ended(Vec<String>),
    Terminal(String),
}

type ReplError = hint::Error<ReplErrorKind>;

impl fmt::Display for ReplErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReplErrorKind::UnknownCommand(word) => write!(f, "unknown command `:{word}`"),
            ReplErrorKind::Ambiguous(word) => write!(f, "ambiguous command `:{word}`"),
            ReplErrorKind::NoMessage => f.write_str("`:commit` needs a message"),
            ReplErrorKind::NoName => f.write_str("`:attach` takes a session's name"),
            ReplErrorKind::NoSession(name) => write!(f, "the workspace has no session {name}"),
            ReplErrorKind::NotAttached => f.write_str("not attached to a session"),
            ReplErrorKind::FilesFlag => {
                f.write_str("`:files` takes FILE... or -w, the REPL's workspace")
            }
            ReplErrorKind::Unwritten(names) => {
                write!(f, "unwritten edits to {}", names.join(", "))
            }
            ReplErrorKind::Ended(names) => write!(
                f,
                "the input ended with unwritten edits to {}, which were discarded",
                names.join(", ")
            ),
            ReplErrorKind::Terminal(err) => write!(f, "cannot read the terminal: {err}"),
        }
    }
}

impl Hint for ReplErrorKind {
    fn exit_code(&self) -> u8 {
        match self {
            ReplErrorKind::Unwritten(_) | ReplErrorKind::Ended(_) => 1,
            ReplErrorKind::Terminal(_) => 3,
            _ => 2,
        }
    }

    fn fix(&self) -> Option<Fix> {
        Some(Fix::from(match self {
            ReplErrorKind::UnknownCommand(_) => {
                let all: Vec<String> = COMMANDS.iter().map(|c| format!(":{c}")).collect();
                return Some(format!("commands are {} :wq", all.join(" ")).into());
            }
            ReplErrorKind::NoMessage => "give one: `:commit MSG`",
            ReplErrorKind::NoName => "give one: `:attach NAME`",
            ReplErrorKind::NotAttached => "`:attach NAME` follows one",
            ReplErrorKind::Unwritten(_) => "`:write` them, or `:quit!` to discard them",
            ReplErrorKind::Ended(_) => {
                "end it with `:write` to write them, or `:quit!` to discard them"
            }
            ReplErrorKind::Terminal(_) => "pipe the REPL's input instead",
            ReplErrorKind::Ambiguous(_)
            | ReplErrorKind::NoSession(_)
            | ReplErrorKind::FilesFlag => return None,
        }))
    }
}

/// Following another session's log (spec §1.4).
struct Follow {
    name: String,
    follower: Follower,
    /// The entries this REPL recorded, which aren't printed.
    own: HashSet<u64>,
    /// The REPL's buffers, into which it merges what the session writes.
    buffers: Arc<Mutex<Buffers>>,
    cwd: PathBuf,
    context: usize,
    /// Whether reading the log failed, which is noted once.
    failed: bool,
}

impl Follow {
    /// What the session recorded since the last poll, for stdout, and notes
    /// for stderr.
    fn poll(&mut self) -> (String, String) {
        let entries = match self.follower.poll() {
            Ok(entries) => entries,
            Err(_) if self.failed => return Default::default(),
            Err(err) => {
                self.failed = true;
                let note =
                    Note::from(err).context(format!("stopped following session {}", self.name));
                return (String::new(), painted(&[note]));
            }
        };
        let (mut printed, mut notes) = (String::new(), Vec::new());
        let style = styles().0;
        for entry in entries.iter().filter(|e| !self.own.contains(&e.id)) {
            let line = session::history(std::slice::from_ref(entry), true);
            printed.push_str(&style.paint(Role::Header, &format!("{} {line}", self.name)));
            for line in entry.comment.iter().flat_map(|comment| comment.lines()) {
                printed.push_str(&format!("# {line}\n"));
            }
            printed.push_str(&apply::file_changes(
                &entry.changes,
                &self.cwd,
                self.context,
                style,
            ));
            for change in &entry.changes {
                let Some(after) = &change.after else {
                    continue;
                };
                let path = change.path.strip_prefix(&self.cwd).unwrap_or(&change.path);
                let mut buffers = self.buffers.lock().unwrap();
                let same = |key: &&Path| {
                    *key == change.path || key.canonicalize().is_ok_and(|k| k == change.path)
                };
                let Some(key) = buffers.unwritten().find(same).map(Path::to_path_buf) else {
                    continue;
                };
                notes.push(match buffers.rebase(&key, after) {
                    Ok(_) => Note::from(format!(
                        "merged the change into your unwritten edits to {}",
                        path.display()
                    )),
                    Err(err) => Note::from(err.relative_to(&self.cwd)),
                });
            }
        }
        (printed, painted(&notes))
    }
}

/// `notes` in the REPL's terms, each on a line, painted for stderr.
fn painted(notes: &[Note]) -> String {
    notes
        .iter()
        .map(|note| {
            styles()
                .1
                .message(&format!("{}\n", note.render(Frontend::Repl)))
                .into_owned()
        })
        .collect()
}

struct Repl {
    args: ReplArgs,
    cwd: PathBuf,
    root: PathBuf,
    /// The file set each script starts with: FILE arguments, or `-w`.
    files: Vec<String>,
    workspace: bool,
    /// Shared with the thread that merges a followed session's edits into
    /// them.
    buffers: Arc<Mutex<Buffers>>,
    /// The REPL's own session.
    session: Session,
    /// The session it follows and records into instead, if attached.
    attached: Option<Session>,
    /// Following `attached`, shared with the thread that prints what it
    /// records, on a terminal.
    follow: Arc<Mutex<Option<Follow>>>,
    daemon: daemon::Workspace,
}

impl Repl {
    fn new(args: ReplArgs) -> Result<Repl, invoke::Failure> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let root = match &args.workspace {
            Some(Some(dir)) => {
                invoke::report(error::canonical(dir), Frontend::Repl, None, &mut Terminal)?
            }
            _ => workspace::root(&cwd).unwrap_or(cwd.clone()),
        };
        let session = match invoke::session_name(args.session.clone()) {
            Some(name) => invoke::report(
                invoke::open(&name, &root),
                Frontend::Repl,
                None,
                &mut Terminal,
            )?,
            None => {
                let free =
                    session::state_dir().and_then(|dir| session::next_free(&dir, &root, "repl"));
                invoke::report(free, Frontend::Repl, None, &mut Terminal)?
            }
        };
        Ok(Repl {
            session,
            attached: None,
            follow: Arc::default(),
            daemon: daemon::workspace(root.clone()),
            files: args.files.clone(),
            workspace: args.workspace.is_some(),
            buffers: Arc::default(),
            args,
            cwd,
            root,
        })
    }

    /// Runs a line of input; `Some` exit code to quit.
    fn eval(&mut self, src: &str) -> Option<u8> {
        match src.trim().strip_prefix(':') {
            Some(command) => self.command(command),
            None if src.trim().is_empty() => None,
            None => {
                self.script(src);
                None
            }
        }
    }

    fn command(&mut self, line: &str) -> Option<u8> {
        let (word, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let (word, force) = match word.strip_suffix('!') {
            Some(word) => (word, true),
            None => (word, false),
        };
        let args: Vec<&str> = rest.split_whitespace().collect();
        let done = Report::collect(|notes| match command_name(word)? {
            "write" => self.write(&args, force, None, notes).map(|()| None),
            "commit" => self.commit(rest, notes).map(|()| None),
            "attach" => self.attach(&args).map(|()| None),
            "detach" => self.detach().map(|()| None),
            "wq" => self
                .write(&args, force, None, notes)
                .and_then(|()| self.quit(false))
                .map(Some),
            "quit" => self.quit(force).map(Some),
            "undo" => self.undo().map(|()| None),
            "diff" => self.diff(&args, notes).map(|()| None),
            "reload" => self.reload(&args).map(|()| None),
            "files" => self.set_files(&args).map(|()| None),
            "history" => self.history(args.contains(&"--all")).map(|()| None),
            "help" => help(&args).map(|()| None).map_err(Into::into),
            _ => unreachable!("every command is handled"),
        });
        match invoke::report(done, Frontend::Repl, None, &mut Terminal) {
            Ok(quit) => quit,
            Err(failure) => {
                invoke::fail(failure, &mut Terminal);
                None
            }
        }
    }

    /// Runs a script on the buffers and records it, as `ned` would run it.
    fn script(&mut self, src: &str) {
        // A repeat runs on its script's file set (spec §1.2).
        let (mut files, mut workspace) = match session::is_repeat(src) {
            true => (Vec::new(), false),
            false => (self.files.clone(), self.workspace),
        };
        let mut root = self.root.clone();
        let repeated = Report::collect(|notes| {
            invoke::repeat(
                Some(self.recording()),
                src.to_string(),
                &self.cwd,
                &mut files,
                &mut workspace,
                &mut root,
                true,
                None,
                notes,
            )
        });
        let src = match invoke::report(repeated, Frontend::Repl, None, &mut Terminal) {
            Ok(src) => src,
            Err(failure) => {
                invoke::fail(failure, &mut Terminal);
                return;
            }
        };
        let initial = match workspace {
            true => Initial::Workspace(root.clone()),
            false => Initial::Files(&files),
        };
        let ran = Report::collect(|notes| self.run_script(&src, initial, notes));
        let (exit, error) = match invoke::report(ran, Frontend::Repl, Some(&src), &mut Terminal) {
            Ok(()) => (0, None),
            Err((error, exit)) => {
                errln!("{error}");
                (exit, Some(error))
            }
        };
        let entry = Entry {
            id: 0,
            time: session::now(),
            cwd: self.cwd.clone(),
            files,
            workspace: workspace.then_some(root),
            script: Some(src),
            undoes: None,
            write: false,
            dry_run: false,
            exit,
            error,
            changes: Vec::new(),
            commit: None,
            comment: None,
        };
        self.record(entry);
    }

    fn run_script(
        &mut self,
        src: &str,
        initial: Initial,
        notes: &mut Vec<Note>,
    ) -> Result<(), Errors> {
        // Held while the script runs, so a followed edit merged meanwhile
        // can't slip between what the script read and what it changed.
        let mut buffers = self.buffers.lock().unwrap();
        let options = Options {
            lang: self.args.lang,
            force: self.args.force,
            style: styles().0,
            overlay: Some(buffers.overlay()),
        };
        let settings = Settings {
            format: !self.args.no_fmt,
            check: !self.args.no_check,
            force: self.args.force,
        };
        let Edited { changes, finished } = invoke::execute(
            src,
            initial,
            &options,
            settings,
            &mut self.daemon,
            notes,
            &mut Terminal,
        )?;
        buffers.apply(&self.cwd, &changes, &finished.finals(&changes));
        drop(buffers);
        let how = Render {
            context: self.args.context,
            quiet: false,
            dry_run: false,
            style: styles().0,
        };
        out!("{}", apply::render(&changes, &finished, how));
        Ok(())
    }

    /// `:write`, and `:commit` with a `message`: writes the buffers, committing
    /// them first with the session's earlier edits (spec §1.3), then reports
    /// what the checks run on save find the write introduced (spec §6.5), and
    /// records it.
    fn write(
        &mut self,
        args: &[&str],
        force: bool,
        message: Option<&str>,
        notes: &mut Vec<Note>,
    ) -> Result<(), Errors> {
        let paths = self.paths(args);
        let writes = self
            .buffers()
            .plan_write(paths.as_deref(), fs::read, force)
            .map_err(|err| err.relative_to(&self.cwd))?;
        if writes.is_empty() && message.is_none() {
            outln!("no unwritten edits");
            return Ok(());
        }
        let changes: Vec<Change> = writes.iter().map(|w| self.change(w)).collect();
        let committed = match message {
            None => None,
            Some(message) => Some(self.commit_writes(&changes, message)?),
        };
        let before = match self.args.no_check || !self.daemon.running() {
            true => None,
            false => apply::before_save(&mut self.daemon, &changes, notes),
        };
        let files: Vec<(PathBuf, String)> = writes
            .iter()
            .map(|w| (w.path.clone(), w.after.clone().unwrap_or_default()))
            .collect();
        if let Err(err) = fs::write_atomic(&files, &[]) {
            let mut errors = Errors::from(invoke::WriteError(err.to_string()));
            if let Some(Committed { repo, prepared }) = &committed
                && let Err(git) = repo.retreat(prepared)
            {
                errors.push(git);
            }
            return Err(errors);
        }
        self.buffers().written(&writes);
        let style = styles().0;
        for change in &changes {
            let stat = DiffStat::between(&change.old, &change.new);
            let summary = match change.created {
                true => diff::created_summary(&change.path, stat, false),
                false => format!("{}: written, {stat}", change.path),
            };
            outln!("{}", style.paint(Role::Header, &summary));
        }
        if let Some(before) = before {
            let finished = Finished {
                outcomes: vec![Outcome::Unchanged; changes.len()],
                checked: None,
            };
            let found =
                apply::after_save(&mut self.daemon, before, &changes, &finished, style, notes);
            out!("{found}");
        }
        if let Some(committed) = &committed {
            outln!("{}", committed.line(message.unwrap_or_default()));
        }
        let written = writes
            .into_iter()
            .map(|w| FileChange {
                path: w.path.canonicalize().unwrap_or(w.path),
                ..w
            })
            .collect();
        let entry = Entry {
            id: 0,
            time: session::now(),
            cwd: self.cwd.clone(),
            files: Vec::new(),
            workspace: None,
            script: None,
            undoes: None,
            write: true,
            dry_run: false,
            exit: 0,
            error: None,
            changes: written,
            commit: committed.map(|c| c.prepared.commit),
            comment: None,
        };
        self.record(entry);
        Ok(())
    }

    /// `:commit MSG` (spec §1.4).
    fn commit(&mut self, message: &str, notes: &mut Vec<Note>) -> Result<(), Errors> {
        match message.trim() {
            "" => Err(ReplErrorKind::NoMessage.into()),
            message => self.write(&[], false, Some(message), notes),
        }
    }

    /// Commits `changes`, a write about to be made, after the session's edits
    /// since its last commit, as `--commit` does.
    fn commit_writes(&mut self, changes: &[Change], message: &str) -> Result<Committed, Errors> {
        let entries = self.recording().lock().and_then(|log| log.entries())?;
        let prior =
            session::uncommitted(&entries, fs::read).map_err(|err| err.relative_to(&self.cwd))?;
        let head = Repo::discover(&self.root).ok();
        let finals: Vec<&str> = changes.iter().map(|c| c.new.as_str()).collect();
        apply::commit(
            &self.root, head, &self.cwd, &prior, changes, &finals, message,
        )
    }

    /// `:attach NAME`: prints the session's history, then follows it and
    /// records into it.
    fn attach(&mut self, args: &[&str]) -> Result<(), Errors> {
        let [name] = args else {
            return Err(ReplErrorKind::NoName.into());
        };
        let names = session::state_dir().and_then(|dir| session::sessions(&dir, &self.root))?;
        if !names.iter().any(|n| n == name) {
            let known = match names.is_empty() {
                true => "it has none".to_string(),
                false => format!("its sessions are {}", hint::verbatim(&names.join(", "))),
            };
            let kind = ReplErrorKind::NoSession(name.to_string());
            return Err(ReplError::new(kind).with_fix(known).into());
        }
        let attached = invoke::open(name, &self.root)?;
        let (entries, follower) = attached.lock().and_then(|log| log.follow())?;
        out!("{}", session::history(&entries, false));
        *self.follow.lock().unwrap() = Some(Follow {
            name: name.to_string(),
            follower,
            own: HashSet::new(),
            buffers: Arc::clone(&self.buffers),
            cwd: self.cwd.clone(),
            context: self.args.context,
            failed: false,
        });
        self.attached = Some(attached);
        Ok(())
    }

    fn detach(&mut self) -> Result<(), Errors> {
        if self.attached.take().is_none() {
            return Err(ReplErrorKind::NotAttached.into());
        }
        *self.follow.lock().unwrap() = None;
        Ok(())
    }

    fn buffers(&self) -> MutexGuard<'_, Buffers> {
        self.buffers.lock().unwrap()
    }

    /// The session the REPL records into: the attached one, or its own.
    fn recording(&self) -> &Session {
        self.attached.as_ref().unwrap_or(&self.session)
    }

    /// Records `entry`; one recorded into an attached session isn't printed
    /// as followed. The follower's lock is held throughout, so it can't read
    /// the entry before it's known as the REPL's.
    fn record(&mut self, entry: Entry) {
        let mut follow = self.follow.lock().unwrap();
        let recorded = Report::collect(|notes| Ok(invoke::record(self.recording(), entry, notes)));
        let id = invoke::report(recorded, Frontend::Repl, None, &mut Terminal)
            .ok()
            .flatten();
        if let (Some(follow), Some(id)) = (follow.as_mut(), id) {
            follow.own.insert(id);
        }
    }

    /// Prints what the followed session recorded since the last poll.
    fn poll(&mut self) {
        if let Some(follow) = self.follow.lock().unwrap().as_mut() {
            let (printed, notes) = follow.poll();
            out!("{printed}");
            eprint!("{notes}");
        }
    }

    /// The script-like change a write makes to a file, for the checks run on
    /// save.
    fn change(&self, write: &FileChange) -> Change {
        let path = self.shown(&write.path);
        let new = write.after.clone().unwrap_or_default();
        Change {
            lang: self
                .args
                .lang
                .unwrap_or_else(|| Language::detect(&path, &new)),
            path,
            old: write.before.clone().unwrap_or_default(),
            new,
            edits: 0,
            created: write.before.is_none(),
        }
    }

    fn undo(&mut self) -> Result<(), Errors> {
        let changes = self
            .buffers()
            .undo(fs::read)
            .map_err(|err| err.relative_to(&self.cwd))?;
        let style = styles().0;
        out!(
            "{}",
            apply::file_changes(&changes, &self.cwd, self.args.context, style)
        );
        let paths: Vec<PathBuf> = changes.into_iter().map(|c| c.path).collect();
        self.sync(&paths);
        Ok(())
    }

    /// `:diff`: what `:write` would write, against the files on disk (spec
    /// §1.4).
    fn diff(&self, args: &[&str], notes: &mut Vec<Note>) -> Result<(), Errors> {
        let paths = self.paths(args).unwrap_or_else(|| self.unwritten());
        if paths.is_empty() {
            outln!("no unwritten edits");
            return Ok(());
        }
        let buffers = self.buffers();
        let mut changes = Vec::new();
        for path in paths {
            let one = std::slice::from_ref(&path);
            let writes = match buffers.plan_write(Some(one), fs::read, false) {
                Err(
                    err @ BuffersError {
                        kind: BuffersErrorKind::Overlap { .. },
                        ..
                    },
                ) => {
                    let err = err.with_fix("this is what `:write!` writes");
                    notes.push(err.relative_to(&self.cwd).into());
                    buffers.plan_write(Some(one), fs::read, true)
                }
                Err(
                    err @ BuffersError {
                        kind: BuffersErrorKind::Exists { .. } | BuffersErrorKind::Removed { .. },
                        ..
                    },
                ) => {
                    notes.push(err.relative_to(&self.cwd).into());
                    buffers.plan_write(Some(one), fs::read, true)
                }
                writes => writes,
            };
            changes.extend(writes.map_err(|err| err.relative_to(&self.cwd))?);
        }
        let style = styles().0;
        out!(
            "{}",
            apply::file_changes(&changes, &self.cwd, self.args.context, style)
        );
        Ok(())
    }

    fn reload(&mut self, args: &[&str]) -> Result<(), Errors> {
        let paths = self.paths(args);
        let reloaded = paths.clone().unwrap_or_else(|| self.unwritten());
        self.buffers()
            .reload(paths.as_deref())
            .map_err(|err| err.relative_to(&self.cwd))?;
        for path in &reloaded {
            outln!("{}: reloaded", self.shown(path));
        }
        self.sync(&reloaded);
        Ok(())
    }

    /// `:files`: prints the file set and the buffers with unwritten edits, or
    /// replaces the set.
    fn set_files(&mut self, args: &[&str]) -> Result<(), Errors> {
        match args {
            [] => {}
            ["-w" | "--workspace"] => {
                self.files.clear();
                self.workspace = true;
                return Ok(());
            }
            [flag, ..] if flag.starts_with('-') => {
                let root = hint::verbatim(&self.root.display().to_string());
                let fix = format!("start another REPL for a workspace other than {root}");
                return Err(ReplError::new(ReplErrorKind::FilesFlag)
                    .with_fix(fix)
                    .into());
            }
            files => {
                self.files = files.iter().map(|f| f.to_string()).collect();
                self.workspace = false;
                return Ok(());
            }
        }
        match self.workspace {
            true => outln!("workspace: {}", self.root.display()),
            false => outln!("files: {}", self.files.join(" ")),
        }
        for path in self.unwritten() {
            let buffers = self.buffers();
            let base = buffers.base(&path).flatten().unwrap_or_default();
            let stat = DiffStat::between(base, &buffers.overlay()[&path]);
            outln!("{}: unwritten, {stat}", self.shown(&path));
        }
        Ok(())
    }

    fn history(&self, all: bool) -> Result<(), Errors> {
        let entries = self.recording().lock().and_then(|log| log.entries())?;
        out!("{}", session::history(&entries, all));
        Ok(())
    }

    fn quit(&mut self, force: bool) -> Result<u8, Errors> {
        let unwritten = self.unwritten();
        if !unwritten.is_empty() && !force {
            let names: Vec<String> = unwritten.iter().map(|p| self.shown(p)).collect();
            return Err(ReplErrorKind::Unwritten(names).into());
        }
        self.discard();
        Ok(0)
    }

    /// At the end of input that isn't a terminal: discards unwritten edits,
    /// failing if there were any.
    fn end(&mut self) -> u8 {
        let unwritten = self.unwritten();
        if unwritten.is_empty() {
            return 0;
        }
        let names: Vec<String> = unwritten.iter().map(|p| self.shown(p)).collect();
        let error = ReplError::new(ReplErrorKind::Ended(names));
        errln!("{}", error.render(Frontend::Repl, None));
        self.discard();
        error.exit_code()
    }

    /// Drops every unwritten edit, and tells the servers.
    fn discard(&mut self) {
        let unwritten = self.unwritten();
        let _ = self.buffers().reload(None);
        self.sync(&unwritten);
    }

    /// Sends the servers the text of each of `paths` (its buffer's, or the
    /// file's), so they see what scripts will read.
    fn sync(&mut self, paths: &[PathBuf]) {
        if paths.is_empty() || !self.daemon.running() {
            return;
        }
        let documents: Vec<Document> = paths
            .iter()
            .filter_map(|path| {
                let text = match self.buffers().overlay().get(path) {
                    Some(text) => text.clone(),
                    None => fs::read(path).ok()??,
                };
                let lang = Language::detect(&path.to_string_lossy(), &text)?;
                Some(Document {
                    path: path.clone(),
                    lang,
                    text,
                })
            })
            .collect();
        if let Err(err) = self.daemon.sync(&documents) {
            errln!("note: {}", err.0);
        }
    }

    /// The buffers `args` name, by absolute path; `None` for every one.
    fn paths(&self, args: &[&str]) -> Option<Vec<PathBuf>> {
        let absolute = |arg: &&str| ned_core::fs::canonical(&self.cwd.join(arg));
        (!args.is_empty()).then(|| args.iter().map(absolute).collect())
    }

    fn unwritten(&self) -> Vec<PathBuf> {
        self.buffers().unwritten().map(Path::to_path_buf).collect()
    }

    /// `path` as shown: relative to the working directory, if it's inside.
    fn shown(&self, path: &Path) -> String {
        path.strip_prefix(&self.cwd)
            .unwrap_or(path)
            .display()
            .to_string()
    }
}

/// `:help [TOPIC]`.
fn help(args: &[&str]) -> Result<(), hint::Error<ned_core::help::UnknownTopic>> {
    out!("{}", Frontend::Repl.text(args.first().copied())?);
    Ok(())
}

/// What reading a line of input gave.
enum Read {
    Script(String),
    /// Ctrl-C: the line is dropped.
    Interrupted,
    /// Ctrl-D, or the end of input that isn't a terminal.
    End,
}

/// Lines from a terminal, with editing keys and history, or from stdin.
struct Input {
    /// The editor and its history file, on a terminal.
    editor: Option<(DefaultEditor, Option<PathBuf>)>,
    /// The prompts for a script's first line and the lines that continue it.
    prompts: (&'static str, &'static str),
    /// Whether a thread prints what a followed session records.
    following: bool,
}

impl Input {
    fn new() -> Input {
        let mut input = Input {
            editor: None,
            following: false,
            prompts: match unicode() {
                true => ("ned› ", "   … "),
                false => ("ned> ", "...> "),
            },
        };
        if !io::stdin().is_terminal() {
            return input;
        }
        let Ok(mut editor) = DefaultEditor::new() else {
            return input;
        };
        let history = session::state_dir()
            .ok()
            .map(|dir| dir.join("repl_history"));
        if let Some(history) = &history {
            let _ = editor.load_history(history);
        }
        input.editor = Some((editor, history));
        input
    }

    /// On a terminal, prints what the session `follow` follows records, as it
    /// records it, above the line being edited. The printer and its thread
    /// start on the first attach, so a REPL that never attaches has neither.
    fn follow(&mut self, follow: &Arc<Mutex<Option<Follow>>>) {
        let Some((editor, _)) = self.editor.as_mut().filter(|_| !self.following) else {
            return;
        };
        let Ok(mut printer) = editor.create_external_printer() else {
            return;
        };
        self.following = true;
        let follow = Arc::clone(follow);
        thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_millis(250));
                let polled = follow.lock().unwrap().as_mut().map(Follow::poll);
                if let Some((printed, notes)) = polled
                    && !(printed.is_empty() && notes.is_empty())
                {
                    let _ = printer.print(format!("{printed}{notes}"));
                }
            }
        });
    }

    fn terminal(&self) -> bool {
        self.editor.is_some()
    }

    /// A script, read on as many lines as it takes: one that ends inside a
    /// heredoc or a pattern continues on the next (spec §1.4).
    fn read(&mut self) -> Read {
        let mut src = match self.line(self.prompts.0) {
            Read::Script(src) => src,
            other => return other,
        };
        while !src.trim_start().starts_with(':')
            && script::parse(&src).is_err_and(|err| err.kind.incomplete())
        {
            match self.line(self.prompts.1) {
                Read::Script(more) => {
                    src.push('\n');
                    src.push_str(&more);
                }
                Read::Interrupted => return Read::Interrupted,
                Read::End => break,
            }
        }
        if let Some((editor, history)) = &mut self.editor
            && !src.trim().is_empty()
        {
            let _ = editor.add_history_entry(src.as_str());
            if let Some(history) = history {
                let _ = editor.append_history(history);
            }
        }
        Read::Script(src)
    }

    fn line(&mut self, prompt: &str) -> Read {
        match &mut self.editor {
            Some((editor, _)) => match editor.readline(prompt) {
                Ok(line) => Read::Script(line),
                Err(ReadlineError::Interrupted) => Read::Interrupted,
                Err(ReadlineError::Eof) => Read::End,
                Err(err) => {
                    let error = ReplError::new(ReplErrorKind::Terminal(err.to_string()));
                    errln!("{}", error.render(Frontend::Repl, None));
                    Read::End
                }
            },
            None => {
                let mut line = String::new();
                match io::stdin().lock().read_line(&mut line) {
                    Ok(0) | Err(_) => Read::End,
                    Ok(_) => {
                        let end = line.trim_end_matches(['\n', '\r']).len();
                        line.truncate(end);
                        Read::Script(line)
                    }
                }
            }
        }
    }
}

/// Whether the locale is UTF-8, so the terminal shows Unicode prompts.
fn unicode() -> bool {
    utf8(["LC_ALL", "LC_CTYPE", "LANG"].map(|var| std::env::var(var).ok()))
}

/// Whether the first of a locale's `LC_ALL`, `LC_CTYPE` and `LANG` that's set
/// names UTF-8.
fn utf8(values: [Option<String>; 3]) -> bool {
    let locale = values
        .into_iter()
        .flatten()
        .find(|value| !value.is_empty())
        .unwrap_or_default()
        .to_ascii_lowercase();
    locale.contains("utf-8") || locale.contains("utf8")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_locale_variable_set_says_whether_it_is_utf8() {
        let locale = |values: [&str; 3]| utf8(values.map(|v| Some(v.to_string())));
        assert!(locale(["", "", "en_US.UTF-8"]));
        assert!(locale(["", "C.utf8", ""]));
        assert!(!locale(["C", "en_US.UTF-8", "en_US.UTF-8"]));
        assert!(!locale(["", "", ""]));
        assert!(!utf8([None, None, None]));
    }
}
