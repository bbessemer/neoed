use std::io::{self, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use ned_core::diff::{self, DiffStat};
use ned_core::exec::{self, ExecErrorKind, Options};
use ned_core::lang::Language;
use ned_core::{fs, script};

/// Token-economical, syntax-aware line editor for AI agents.
#[derive(Parser)]
#[command(name = "ned", version)]
struct Cli {
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
    /// Use this language for every file instead of detecting it.
    #[arg(long, value_name = "LANG")]
    lang: Option<Language>,
    /// Context lines around diff hunks.
    #[arg(long, value_name = "N", default_value_t = 1)]
    context: usize,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
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

    if !cli.dry_run {
        let writes: Vec<(PathBuf, String)> = changes
            .iter()
            .map(|c| (PathBuf::from(&c.path), c.new.clone()))
            .collect();
        if let Err(err) = fs::write_atomic(&writes) {
            eprintln!("error: cannot write files: {err}");
            return ExitCode::from(3);
        }
    }
    for change in &changes {
        let stat = DiffStat::between(&change.old, &change.new);
        println!(
            "{}",
            diff::summary(&change.path, change.edits, stat, cli.dry_run)
        );
        if !cli.quiet {
            print!("{}", diff::hunks(&change.old, &change.new, cli.context));
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
        | ExecErrorKind::SyntaxError { .. } => 1,
        ExecErrorKind::Unsupported(_) | ExecErrorKind::NoFiles => 2,
        ExecErrorKind::Io { .. } => 3,
    }
}
