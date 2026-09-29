//! `ned daemon` (command-language spec §1.1).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Subcommand;
use ned_core::buffer::Buffer;
use ned_core::exec::Change;
#[cfg(unix)]
use ned_core::lsp::LspFailure;
use ned_core::lsp::{self, Checked, Severity};

/// This build of `ned`, which a daemon must match.
#[cfg_attr(not(unix), allow(dead_code))]
const VERSION: &str = env!("NED_VERSION");

#[cfg_attr(not(unix), allow(dead_code))]
const DEFAULT_IDLE_TIMEOUT: u64 = 600;

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
        /// Overrides `[daemon] idle_timeout`.
        #[arg(long, value_name = "SECS")]
        idle_timeout: Option<u64>,
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
    use ned_core::config::{self, Config};
    use ned_daemon::protocol::{Request, Response};
    use ned_daemon::{Client, Paths, paths, server};

    let dir = match action {
        Action::Run { root, idle_timeout } => {
            let user_config = config::user_config();
            let idle_timeout = match idle_timeout {
                Some(secs) => secs,
                None => Config::new(user_config.as_deref())?
                    .idle_timeout(&root)?
                    .unwrap_or(DEFAULT_IDLE_TIMEOUT),
            };
            let paths = Paths::new(&paths::runtime_dir()?, &root, VERSION);
            server::serve(
                &paths,
                &root,
                user_config,
                Duration::from_secs(idle_timeout),
            )?;
            return Ok(());
        }
        Action::Start { ref dir } | Action::Status { ref dir } | Action::Stop { ref dir } => {
            dir.clone().unwrap_or_else(|| PathBuf::from("."))
        }
    };
    let root = ned_core::workspace::root(&dir)
        .with_context(|| format!("cannot read {}", dir.display()))?;
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
        Response::Status(status) => print!("{}", status_text(&status)),
        Response::Opened
        | Response::Diagnosis(_)
        | Response::Renamed(_)
        | Response::Located(_)
        | Response::Formatted(_)
        | Response::Stopped => {
            println!("stopped the daemon for {shown}")
        }
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

/// The daemon for the workspace at `root`, for `check` and checking edits.
#[cfg(unix)]
pub fn workspace(root: PathBuf) -> ned_daemon::client::Workspace {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ned"));
    ned_daemon::client::Workspace::new(exe, root, VERSION)
}

/// Checks the edit, if a daemon is running for the workspace (spec §6.5).
/// `None` if none is, or if checking failed, which gets a note.
#[cfg(unix)]
pub fn check(
    workspace: &mut ned_daemon::client::Workspace,
    changes: &[Change],
    finals: &[&str],
    allow: Option<Severity>,
    force: bool,
) -> Option<Checked> {
    if changes.is_empty() || !workspace.running() {
        return None;
    }
    match lsp::check_changes(workspace, changes, finals, allow, force) {
        Ok(checked) => Some(checked),
        Err(LspFailure(message)) => {
            let paths: Vec<&str> = changes.iter().map(|c| c.path.as_str()).collect();
            eprintln!("note: {message}; skipped checking {}", paths.join(", "));
            None
        }
    }
}

/// Sends the servers the original texts back, after an edit that wasn't
/// written.
#[cfg(unix)]
pub fn restore(workspace: &mut ned_daemon::client::Workspace, changes: &[Change]) {
    if let Err(LspFailure(message)) = lsp::restore(workspace, changes) {
        eprintln!("note: {message}");
    }
}

/// The error for an edit rejected by the diagnostics it introduces.
pub fn blocked(changes: &[Change], finals: &[&str], checked: &Checked) -> String {
    let count = |severity| {
        checked
            .blocking
            .iter()
            .filter(|(_, d)| d.severity == severity)
            .count()
    };
    let counts: Vec<String> = Severity::ALL
        .into_iter()
        .filter(|&s| count(s) > 0)
        .map(|s| match count(s) {
            1 => format!("1 {s}"),
            n => format!("{n} {s}s"),
        })
        .collect();
    let them = if checked.blocking.len() == 1 {
        "it"
    } else {
        "them"
    };
    let allow = if count(Severity::Error) > 0 {
        "errors"
    } else {
        "warnings"
    };
    let mut text = format!(
        "error: edit introduces {}; fix {them}, or add `allow {allow}` to the script to apply it anyway\n",
        counts.join(" and ")
    );
    for (i, d) in &checked.blocking {
        text.push_str(&lsp::render(&changes[*i].path, &Buffer::new(finals[*i]), d));
    }
    text
}

/// A status line for the daemon, then one per server.
#[cfg(unix)]
fn status_text(status: &ned_daemon::protocol::Status) -> String {
    use ned_daemon::protocol::ServerState;
    use std::fmt::Write;

    let mut text = format!(
        "daemon for {}: pid {}, up {}\n",
        status.root.display(),
        status.pid,
        uptime(status.uptime_secs)
    );
    for server in &status.servers {
        let state = match server.state {
            ServerState::Indexing => "indexing",
            ServerState::Ready => "ready",
            ServerState::Exited => "exited",
        };
        let files = if server.documents == 1 {
            "file"
        } else {
            "files"
        };
        let _ = writeln!(
            text,
            "  {}: {state}, {} {files}",
            server.name, server.documents
        );
    }
    text
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

    #[cfg(unix)]
    #[test]
    fn status_lists_servers() {
        use ned_daemon::protocol::{ServerState, ServerStatus, Status};
        let server = |name: &str, state, documents| ServerStatus {
            name: name.into(),
            state,
            documents,
        };
        let status = Status {
            version: "0.1.0+abc".into(),
            root: PathBuf::from("/w"),
            pid: 7,
            uptime_secs: 180,
            servers: vec![
                server("rust-analyzer", ServerState::Ready, 12),
                server("gopls", ServerState::Indexing, 1),
                server("pyright-langserver", ServerState::Exited, 0),
            ],
        };
        assert_eq!(
            status_text(&status),
            "daemon for /w: pid 7, up 3m\n  rust-analyzer: ready, 12 files\n  gopls: indexing, 1 file\n  pyright-langserver: exited, 0 files\n"
        );
    }
}
