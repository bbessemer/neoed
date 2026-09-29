//! `ned daemon` (command-language spec §1.1).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Subcommand;

/// This build of `ned`, which a daemon must match.
#[cfg_attr(not(unix), allow(dead_code))]
const VERSION: &str = env!("NED_VERSION");

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
    match daemon(action) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(3)
        }
    }
}

#[cfg(unix)]
fn daemon(action: Action) -> anyhow::Result<()> {
    use std::env;
    use std::time::Duration;

    use anyhow::{Context, bail};
    use ned_daemon::protocol::{Request, Response};
    use ned_daemon::{Client, Paths, paths, server};

    let dir = match action {
        Action::Run { root, idle_timeout } => {
            let paths = Paths::new(&paths::runtime_dir()?, &root, VERSION);
            server::serve(&paths, &root, Duration::from_secs(idle_timeout))?;
            return Ok(());
        }
        Action::Start { ref dir } | Action::Status { ref dir } | Action::Stop { ref dir } => {
            dir.clone().unwrap_or_else(|| PathBuf::from("."))
        }
    };
    let root =
        paths::workspace_root(&dir).with_context(|| format!("cannot read {}", dir.display()))?;
    let paths = Paths::new(&paths::runtime_dir()?, &root, VERSION);
    let client = match action {
        Action::Start { .. } => Some(Client::connect_or_spawn(
            &paths,
            &env::current_exe()?,
            &root,
        )?),
        _ => Client::connect(&paths),
    };
    let shown = root.display();
    let Some(client) = client else {
        println!("no daemon for {shown}");
        return Ok(());
    };
    let request = match action {
        Action::Stop { .. } => Request::Stop,
        _ => Request::Status,
    };
    match client.request(&request)? {
        Response::Status(status) => println!(
            "daemon for {shown}: pid {}, up {}",
            status.pid,
            uptime(status.uptime_secs)
        ),
        Response::Stopped => println!("stopped the daemon for {shown}"),
        Response::Error(err) => {
            bail!("the daemon refused the request: {err}; run `ned daemon stop`, then retry")
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn daemon(_: Action) -> anyhow::Result<()> {
    anyhow::bail!("the daemon is Unix-only for now")
}

/// `uptime_secs` as `42s`, `3m` or `2h5m`.
fn uptime(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        _ => format!("{}h{}m", secs / 3600, secs % 3600 / 60),
    }
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
