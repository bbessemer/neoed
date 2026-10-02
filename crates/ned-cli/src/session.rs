//! Sessions in the CLI: `-s`, `NED_SESSION` and `ned history` (spec §1.2).

use std::path::Path;
use std::process::ExitCode;

use ned_core::session::{Entry, Session, SessionError};

/// The session `-s` (given as `flag`) or `NED_SESSION` names, if any.
pub fn name(flag: &Option<Option<String>>) -> Option<String> {
    todo!()
}

/// Session `name` of the workspace at `root`, or the error to print and the
/// exit code.
pub fn open(name: &str, root: &Path) -> Result<Session, (String, u8)> {
    todo!()
}

/// Appends `entry` to `session`; a failure is only a note, since the
/// invocation's files are already written.
pub fn record(session: &Session, entry: Entry) {
    todo!()
}

/// `ned history`.
pub fn history(flag: &Option<Option<String>>, all: bool) -> ExitCode {
    todo!()
}

fn exit_code(err: &SessionError) -> u8 {
    todo!()
}
