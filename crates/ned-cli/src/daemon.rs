//! `ned daemon` (command-language spec §1.1).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Subcommand;

#[derive(Subcommand)]
pub enum Action {
    /// Start the daemon if it isn't running.
    Start { dir: Option<PathBuf> },
    /// Show whether the daemon is running.
    Status { dir: Option<PathBuf> },
    /// Stop the daemon.
    Stop { dir: Option<PathBuf> },
    /// Serve as the daemon for ROOT; `ned` spawns this itself.
    #[command(hide = true)]
    Run {
        root: PathBuf,
        #[arg(long, value_name = "SECS", default_value_t = 600)]
        idle_timeout: u64,
    },
}

pub fn run(action: Action) -> ExitCode {
    let _ = action;
    todo!()
}

/// `uptime_secs` as `42s`, `3m` or `2h5m`.
fn uptime(secs: u64) -> String {
    let _ = secs;
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptimes_are_short() {
        assert_eq!(uptime(0), "0s");
        assert_eq!(uptime(59), "59s");
        assert_eq!(uptime(60), "1m");
        assert_eq!(uptime(3599), "59m");
        assert_eq!(uptime(3600), "1h0m");
        assert_eq!(uptime(7500), "2h5m");
    }
}
