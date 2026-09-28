use std::process::ExitCode;

use clap::Parser;

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
    /// Context lines around diff hunks.
    #[arg(long, value_name = "N", default_value_t = 1)]
    context: usize,
}

fn main() -> ExitCode {
    let _ = Cli::parse();
    todo!()
}
