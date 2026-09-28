use clap::Parser;

/// Token-economical, syntax-aware line editor for AI agents.
#[derive(Parser)]
#[command(name = "ned", version)]
struct Cli {}

fn main() {
    Cli::parse();
}
