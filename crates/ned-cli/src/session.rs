//! Sessions in the CLI: `-s`, `NED_SESSION` and `ned history` (spec §1.2).

use std::env;
use std::path::Path;
use std::process::ExitCode;

use ned_core::session::{self, Entry, Session, SessionError};
use ned_core::workspace;

/// The session `-s` (`flag`) or `NED_SESSION` names, if any.
pub fn name(flag: Option<String>) -> Option<String> {
    flag.or_else(|| env::var("NED_SESSION").ok())
        .filter(|name| !name.is_empty())
}

/// Session `name` of the workspace at `root`, or the error to print and the
/// exit code.
pub fn open(name: &str, root: &Path) -> Result<Session, (String, u8)> {
    let fail = |err: SessionError| (format!("error: {err}"), exit_code(&err));
    Session::new(&session::state_dir().map_err(fail)?, root, name).map_err(fail)
}

/// Appends `entry` to `session`; a failure is only a note, since the
/// invocation's files are already written.
pub fn record(session: &Session, entry: Entry) {
    if let Err(err) = session.lock().and_then(|mut log| log.append(entry)) {
        eprintln!("note: not recorded in session {}: {err}", session.name());
    }
}

/// `ned history`.
pub fn history(flag: Option<String>, all: bool) -> ExitCode {
    let cwd = env::current_dir().unwrap_or_else(|_| ".".into());
    let root = workspace::root(&cwd).unwrap_or(cwd);
    let opened = name(flag)
        .ok_or_else(|| {
            let listed = listing(&root);
            let error =
                format!("error: no session; give one with -s NAME or NED_SESSION ({listed})");
            (error, 2)
        })
        .and_then(|name| open(&name, &root))
        .and_then(|session| match session.exists() {
            true => Ok(session),
            false => Err((
                format!(
                    "error: no session `{}` in this workspace; {}",
                    session.name(),
                    listing(&root).replace(" in this workspace", " in it")
                ),
                2,
            )),
        })
        .and_then(|session| {
            let log = session.lock();
            log.and_then(|log| log.entries())
                .map_err(|err| (format!("error: {err}"), exit_code(&err)))
        });
    match opened {
        Ok(entries) => {
            out!("{}", session::history(&entries, all));
            ExitCode::SUCCESS
        }
        Err((error, code)) => {
            eprintln!("{error}");
            ExitCode::from(code)
        }
    }
}

/// The workspace's sessions, for an error's fix.
fn listing(root: &Path) -> String {
    let names = session::state_dir().and_then(|dir| session::sessions(&dir, root));
    match names.unwrap_or_default() {
        names if names.is_empty() => "none recorded in this workspace yet".to_string(),
        names => format!("sessions in this workspace: {}", names.join(", ")),
    }
}

fn exit_code(err: &SessionError) -> u8 {
    match err {
        SessionError::BadName(_) => 2,
        _ => 3,
    }
}
