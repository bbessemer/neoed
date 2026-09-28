//! Atomic writes of edited files.

use std::io;
use std::path::PathBuf;

/// Replaces the contents of every file in `files`, preserving permissions and
/// writing through symlinks to their targets.
///
/// All contents are first staged in temporary files beside their targets;
/// only once every one is staged are they renamed into place. A failure while
/// staging leaves every target untouched and removes the staged files.
pub fn write_atomic(files: &[(PathBuf, String)]) -> io::Result<()> {
    todo!()
}
