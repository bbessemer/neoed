mod help;

use std::io::{self, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use ned_core::diff::{self, DiffStat};
use ned_core::exec::{self, ExecErrorKind, Options};
use ned_core::format::{self, Formatters, Outcome};
use ned_core::lang::Language;
use ned_core::{fs, script};

/// Token-economical, syntax-aware line editor for AI agents.
#[derive(Parser)]
#[command(
    name = "ned",
    version,
    args_conflicts_with_subcommands = true,
    disable_help_subcommand = true,
    // clap leaves a user-defined `help` subcommand out of the usage.
    override_usage = "ned [OPTIONS] [FILES]... [-e SCRIPT]...\n       ned help [TOPIC]    (the command language)"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Files to edit: the initial file set.
    files: Vec<String>,
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
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Some(Command::Help { topic }) = cli.command {
        print!("{}", help::text(topic));
        return ExitCode::SUCCESS;
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
    let run = exec::run(&parsed, &src, &cli.files, &options);
    print!("{}", run.output);
    let changes = match run.result {
        Ok(changes) => changes,
        Err(err) => {
            eprintln!("{}", err.render(&src));
            return ExitCode::from(exit_code(&err.kind));
        }
    };

    let outcomes = if cli.no_fmt {
        vec![Outcome::Unchanged; changes.len()]
    } else {
        let formatted = Formatters::new(format::user_config().as_deref())
            .and_then(|mut formatters| format::run(&changes, &mut formatters));
        match formatted {
            Ok(outcomes) => outcomes,
            Err(err) => {
                eprintln!("error: {err}");
                return ExitCode::from(2);
            }
        }
    };

    if !cli.dry_run {
        let writes: Vec<(PathBuf, String)> = changes
            .iter()
            .zip(&outcomes)
            .map(|(change, outcome)| {
                let text = match outcome {
                    Outcome::Formatted { text, .. } => text,
                    _ => &change.new,
                };
                (PathBuf::from(&change.path), text.clone())
            })
            .collect();
        if let Err(err) = fs::write_atomic(&writes) {
            eprintln!("error: cannot write files: {err}");
            return ExitCode::from(3);
        }
    }
    for (change, outcome) in changes.iter().zip(&outcomes) {
        let stat = DiffStat::between(&change.old, &change.new);
        println!(
            "{}",
            diff::summary(&change.path, change.edits, stat, cli.dry_run)
        );
        if !cli.quiet {
            print!("{}", diff::hunks(&change.old, &change.new, cli.context));
        }
        match outcome {
            Outcome::Formatted { name, text } => {
                println!("fmt {name}: {}", DiffStat::between(&change.new, text));
                if !cli.quiet {
                    print!("{}", diff::hunks(&change.new, text, cli.context));
                }
            }
            Outcome::Skipped(note) => eprintln!("note: {note}"),
            Outcome::Unchanged => {}
        }
    }
    ExitCode::SUCCESS
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
        | ExecErrorKind::MoveIntoSource { .. } => 1,
        ExecErrorKind::Unsupported { .. }
        | ExecErrorKind::NoFiles
        | ExecErrorKind::InvalidQuery { .. } => 2,
        ExecErrorKind::Io { .. } | ExecErrorKind::NoGlobMatch(_) => 3,
    }
}
