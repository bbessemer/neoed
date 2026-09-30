// `print!` and `println!` that ignore a closed stdout: the script still
// completes when the reader stops early (`ned ... | head`).
macro_rules! out {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = write!(std::io::stdout(), $($arg)*);
    }};
}

macro_rules! outln {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}

mod daemon;
mod help;

use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use ned_core::buffer::Buffer;
use ned_core::config::{self, Config};
use ned_core::diff::{self, DiffStat};
use ned_core::exec::{self, ExecErrorKind, Initial, Options};
use ned_core::format::{self, Outcome};
use ned_core::lang::Language;
use ned_core::lsp;
#[cfg(unix)]
use ned_core::lsp::Lsp;
use ned_core::{fs, script, workspace};

/// Token-economical, syntax-aware line editor for AI agents.
#[derive(Parser)]
#[command(
    name = "ned",
    version = env!("NED_VERSION"),
    args_conflicts_with_subcommands = true,
    disable_help_subcommand = true,
    // clap leaves a user-defined `help` subcommand out of the usage.
    override_usage = "ned [OPTIONS] [FILES... | -w [DIR]] [-e SCRIPT]...\n       ned help [TOPIC]    (the command language)\n       ned daemon start|status|stop [DIR]"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Files to edit: the initial file set.
    files: Vec<String>,
    /// Start with every file in the workspace, DIR or the one containing the
    /// working directory, instead of FILES.
    #[arg(short, long, value_name = "DIR", num_args = 0..=1, conflicts_with = "files")]
    workspace: Option<Option<PathBuf>>,
    /// A script to run; repeat to join several with newlines. Without -e, the
    /// script is read from stdin.
    #[arg(short = 'e', value_name = "SCRIPT")]
    scripts: Vec<String>,
    /// Resolve and apply edits in memory and print the output, but write
    /// nothing.
    #[arg(short = 'n', long)]
    dry_run: bool,
    /// Print only the per-file summary lines on success.
    #[arg(short, long)]
    quiet: bool,
    /// Skip the parse-error guard: apply edits even if they introduce syntax
    /// errors.
    #[arg(long)]
    force: bool,
    /// Don't run formatters.
    #[arg(long)]
    no_fmt: bool,
    /// Don't check edits with language servers.
    #[arg(long)]
    no_check: bool,
    /// Use this language for every file instead of detecting it.
    #[arg(long, value_name = "LANG")]
    lang: Option<Language>,
    /// Context lines around diff hunks.
    #[arg(long, value_name = "N", default_value_t = 1)]
    context: usize,
}

#[derive(Subcommand)]
enum Command {
    /// Print a summary of the command language, or details of one topic.
    Help { topic: Option<help::Topic> },
    /// Manage the language-server daemon for the workspace containing DIR.
    Daemon {
        #[command(subcommand)]
        action: daemon::Action,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Help { topic }) => {
            out!("{}", help::text(topic));
            return ExitCode::SUCCESS;
        }
        Some(Command::Daemon { action }) => return daemon::run(action),
        None => {}
    }
    if let Some(err) = usage_error(&cli) {
        eprintln!("error: {err}");
        return ExitCode::from(2);
    }
    let src = if cli.scripts.is_empty() {
        let mut src = String::new();
        if let Err(err) = io::stdin().read_to_string(&mut src) {
            eprintln!("error: cannot read the script from stdin: {err}");
            return ExitCode::from(2);
        }
        src
    } else {
        cli.scripts.join("\n")
    };
    let parsed = match script::parse(&src) {
        Ok(parsed) => parsed,
        Err(err) => {
            eprintln!("{}", err.render(&src));
            return ExitCode::from(2);
        }
    };

    let options = Options {
        lang: cli.lang,
        force: cli.force,
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = match &cli.workspace {
        Some(Some(dir)) => match dir.canonicalize() {
            Ok(dir) => dir,
            Err(err) => {
                eprintln!("error: cannot read {}: {err}", dir.display());
                return ExitCode::from(3);
            }
        },
        _ => workspace::root(&cwd).unwrap_or(cwd),
    };
    let initial = match cli.workspace {
        Some(_) => Initial::Workspace(root.clone()),
        None => Initial::Files(&cli.files),
    };
    #[cfg(unix)]
    let mut workspace = daemon::workspace(root);
    #[cfg(unix)]
    let lsp: Option<&mut dyn Lsp> = Some(&mut workspace);
    #[cfg(not(unix))]
    let lsp = None;
    let run = exec::run(&parsed, &src, initial, &options, lsp);
    out!("{}", run.output);
    for note in &run.notes {
        eprintln!("note: {note}");
    }
    let changes = match run.result {
        Ok(changes) => changes,
        Err(err) => {
            eprintln!("{}", err.render(&src));
            return ExitCode::from(exit_code(&err.kind));
        }
    };

    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut outcomes = if cli.no_fmt {
        vec![Outcome::Unchanged; changes.len()]
    } else {
        let formatted = Config::new(config::user_config().as_deref())
            .and_then(|mut config| format::run(&changes, &mut config));
        match formatted {
            Ok(outcomes) => outcomes,
            Err(err) => {
                eprintln!("error: {err}");
                return ExitCode::from(2);
            }
        }
    };

    #[cfg(unix)]
    if workspace.running() {
        format::fallback(&changes, &mut outcomes, &mut workspace);
    }

    let finals: Vec<&str> = changes
        .iter()
        .zip(&outcomes)
        .map(|(change, outcome)| match outcome {
            Outcome::Formatted { text, .. } => text.as_str(),
            _ => change.new.as_str(),
        })
        .collect();
    #[cfg(unix)]
    let checked = match cli.no_check {
        true => None,
        false => daemon::check(&mut workspace, &changes, &finals, run.allow, cli.force),
    };
    #[cfg(not(unix))]
    let checked: Option<ned_core::lsp::Checked> = None;
    if let Some(checked) = &checked
        && !checked.blocking.is_empty()
    {
        eprint!("{}", daemon::blocked(&changes, &finals, checked));
        #[cfg(unix)]
        daemon::restore(&mut workspace, &changes);
        return ExitCode::from(1);
    }

    if !cli.dry_run {
        let writes: Vec<(PathBuf, String)> = changes
            .iter()
            .zip(&finals)
            .map(|(change, text)| (PathBuf::from(&change.path), text.to_string()))
            .collect();
        if let Err(err) = fs::write_atomic(&writes) {
            eprintln!("error: cannot write files: {err}; no file was changed");
            return ExitCode::from(3);
        }
    }
    for (i, (change, outcome)) in changes.iter().zip(&outcomes).enumerate() {
        let stat = DiffStat::between(&change.old, &change.new);
        outln!(
            "{}",
            if change.created {
                diff::created_summary(&change.path, stat, cli.dry_run)
            } else {
                diff::summary(&change.path, change.edits, stat, cli.dry_run)
            }
        );
        if !cli.quiet {
            out!("{}", diff::hunks(&change.old, &change.new, cli.context));
        }
        match outcome {
            Outcome::Formatted { name, text } => {
                outln!("fmt {name}: {}", DiffStat::between(&change.new, text));
                if !cli.quiet {
                    out!("{}", diff::hunks(&change.new, text, cli.context));
                }
            }
            Outcome::NotFound(note) | Outcome::Failed(note) => eprintln!("note: {note}"),
            Outcome::Unchanged => {}
        }
        if let Some(checked) = &checked {
            let buffer = Buffer::new(finals[i]);
            for d in &checked.files[i] {
                out!("{}", lsp::render(&change.path, &buffer, d));
            }
        }
    }
    #[cfg(unix)]
    if checked.is_some() && cli.dry_run {
        daemon::restore(&mut workspace, &changes);
    }
    ExitCode::SUCCESS
}

/// A usage error found before the script is read (spec §1): a command's name
/// given as a FILE, or no script with stdin a terminal.
fn usage_error(cli: &Cli) -> Option<String> {
    let exists = |arg: &str| std::path::Path::new(arg).exists();
    let is_command = |arg: &str| script::error::COMMANDS.split(' ').any(|c| c == arg);
    if cli.workspace.is_none()
        && let Some(verb) = cli.files.iter().position(|a| !exists(a) && is_command(a))
    {
        // The command's words are the arguments from it on that aren't files.
        let (mut files, mut words) = (Vec::new(), Vec::new());
        for (i, arg) in cli.files.iter().enumerate() {
            match i < verb || exists(arg) {
                true => files.push(arg.as_str()),
                false => words.push(arg.as_str()),
            }
        }
        return Some(format!(
            "`{}` is a command, not a file; give the script with -e: ned {} -e '{}'",
            cli.files[verb],
            files.join(" "),
            words.join(" ")
        ));
    }
    (cli.scripts.is_empty() && io::stdin().is_terminal())
        .then(|| "no script: give one with -e SCRIPT or on stdin, e.g. ned FILE -e outline".into())
}

/// The exit code for an error that rejected a script (spec §7).
fn exit_code(kind: &ExecErrorKind) -> u8 {
    match kind {
        ExecErrorKind::NoMatch { .. }
        | ExecErrorKind::Ambiguous { .. }
        | ExecErrorKind::LineOutOfRange { .. }
        | ExecErrorKind::NotInFileSet { .. }
        | ExecErrorKind::Overlap { .. }
        | ExecErrorKind::SyntaxError { .. }
        | ExecErrorKind::NoLanguage { .. }
        | ExecErrorKind::UnknownKind { .. }
        | ExecErrorKind::MissingPart { .. }
        | ExecErrorKind::PartNeedsItem { .. }
        | ExecErrorKind::MoveIntoSource { .. }
        | ExecErrorKind::FileExists { .. }
        | ExecErrorKind::RenameRefused { .. }
        | ExecErrorKind::Outside { .. }
        | ExecErrorKind::AmbiguousLocated { .. } => 1,
        ExecErrorKind::Unsupported { .. }
        | ExecErrorKind::NoFiles
        | ExecErrorKind::InvalidQuery { .. }
        | ExecErrorKind::NoServer { .. } => 2,
        ExecErrorKind::Io { .. } | ExecErrorKind::NoGlobMatch { .. } | ExecErrorKind::Lsp(_) => 3,
    }
}
