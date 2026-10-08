//! `ned daemon` (command-language spec §1.1).

use std::fmt;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Subcommand;
use ned_core::hint::{AnyError, Fix, Hint};
#[cfg(not(unix))]
use ned_core::lsp::{
    Diagnosis, Document, Formatting, Locate, Located, LspFailure, Position, Renamed,
};

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
        Err(err) => crate::fail(err),
    }
}

#[cfg(unix)]
fn daemon(action: Action) -> Result<(), AnyError> {
    use std::env;
    use std::time::Duration;

    use ned_core::config::{self, Config};
    use ned_daemon::protocol::{Request, Response};
    use ned_daemon::{Client, Paths, paths, server};

    let runtime_dir = paths::runtime_dir;
    let dir = match action {
        Action::Run { root, idle_timeout } => {
            let user_config = config::user_config();
            let idle_timeout = match idle_timeout {
                Some(secs) => secs,
                None => Config::new(user_config.as_deref())?
                    .idle_timeout(&root)?
                    .unwrap_or(DEFAULT_IDLE_TIMEOUT),
            };
            let paths = Paths::new(&runtime_dir()?, &root, VERSION);
            server::serve(
                &paths,
                &root,
                user_config,
                Duration::from_secs(idle_timeout),
            )
            .map_err(DaemonError::Io)?;
            return Ok(());
        }
        Action::Start { ref dir } | Action::Status { ref dir } | Action::Stop { ref dir } => {
            dir.clone().unwrap_or_else(|| PathBuf::from("."))
        }
    };
    let root =
        ned_core::workspace::root(&crate::error::canonical(&dir)?).map_err(DaemonError::Io)?;
    let paths = Paths::new(&runtime_dir()?, &root, VERSION);
    let client = match action {
        Action::Start { .. } => {
            let exe = env::current_exe().map_err(DaemonError::Io)?;
            Some(Client::connect_or_spawn(&paths, &exe, &root)?)
        }
        _ => Client::connect(&paths),
    };
    let shown = root.display();
    let Some(client) = client else {
        outln!("no daemon for {shown}");
        return Ok(());
    };
    let request = match action {
        Action::Stop { .. } => Request::Stop,
        _ => Request::Status,
    };
    match client.request(&request)? {
        Response::Status(status) => out!("{}", status_text(&status)),
        Response::Opened
        | Response::Diagnosis(_)
        | Response::Renamed(_)
        | Response::Located(_)
        | Response::Formatted(_)
        | Response::Stopped => {
            outln!("stopped the daemon for {shown}")
        }
        Response::Error(err) => return Err(DaemonError::Refused(err).into()),
    }
    Ok(())
}

#[cfg(not(unix))]
fn daemon(_: Action) -> Result<(), AnyError> {
    Err(DaemonError::UnixOnly.into())
}

/// What stopped `ned daemon`, besides ned-daemon's own errors.
#[derive(Debug)]
enum DaemonError {
    #[cfg(unix)]
    Io(std::io::Error),
    /// The daemon answered with an error.
    #[cfg(unix)]
    Refused(ned_core::lsp::LspFailure),
    #[cfg(not(unix))]
    UnixOnly,
}

impl fmt::Display for DaemonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(unix)]
            DaemonError::Io(err) => write!(f, "{err}"),
            #[cfg(unix)]
            DaemonError::Refused(failure) => write!(f, "the daemon refused the request: {failure}"),
            #[cfg(not(unix))]
            DaemonError::UnixOnly => f.write_str("the daemon is Unix-only for now"),
        }
    }
}

impl Hint for DaemonError {
    fn exit_code(&self) -> u8 {
        3
    }

    fn fix(&self) -> Option<Fix> {
        match self {
            #[cfg(unix)]
            DaemonError::Refused(failure) => failure
                .fix()
                .or_else(|| Some("run `ned daemon stop`, then retry".into())),
            _ => None,
        }
    }
}

/// The workspace's language servers, through its daemon.
#[cfg(unix)]
pub use ned_daemon::client::Workspace;

/// The daemon for the workspace at `root`, for `check` and checking edits.
#[cfg(unix)]
pub fn workspace(root: PathBuf) -> Workspace {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ned"));
    Workspace::new(exe, root, VERSION)
}

/// Without Unix there's no daemon, so every language-server feature is an
/// error (spec §1.1).
#[cfg(not(unix))]
pub struct Workspace;

#[cfg(not(unix))]
pub fn workspace(_root: PathBuf) -> Workspace {
    Workspace
}

#[cfg(not(unix))]
impl ned_core::lsp::Lsp for Workspace {
    fn running(&mut self) -> bool {
        false
    }

    fn diagnose(&mut self, _: &[Document], _: bool) -> Result<Diagnosis, LspFailure> {
        Err(unix_only())
    }

    fn sync(&mut self, _: &[Document]) -> Result<(), LspFailure> {
        Err(unix_only())
    }

    fn rename(&mut self, _: &Document, _: Position, _: &str) -> Result<Renamed, LspFailure> {
        Err(unix_only())
    }

    fn locate(&mut self, _: Locate, _: &Document, _: Position) -> Result<Located, LspFailure> {
        Err(unix_only())
    }

    fn format(&mut self, _: &Document) -> Result<Formatting, LspFailure> {
        Err(unix_only())
    }
}

#[cfg(not(unix))]
fn unix_only() -> LspFailure {
    LspFailure::from("language servers run through the daemon, which needs Unix")
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
