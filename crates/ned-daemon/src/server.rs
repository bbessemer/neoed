//! The daemon's event loop.

use std::io;
use std::path::Path;
use std::time::Duration;

use crate::paths::Paths;

/// Serves `root`'s daemon at `paths` until a `stop` request, `idle` without
/// a request, or SIGTERM or SIGINT. Returns at once if another daemon holds
/// the lock.
pub fn serve(paths: &Paths, root: &Path, idle: Duration) -> io::Result<()> {
    let _ = (paths, root, idle);
    todo!()
}
