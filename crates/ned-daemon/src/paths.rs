//! Where a workspace's daemon listens, locks and logs.

use std::env;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use ned_core::fs::{PrivateDirError, private_dir, uid};
use ned_core::hint::{Fix, Hint};
use thiserror::Error;

/// The files of one workspace's daemon, for one build of `ned`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// The `ned` build the daemon serves: a daemon only talks to its own build.
    pub version: String,
    pub socket: PathBuf,
    /// Held by the running daemon, so only one serves a workspace.
    pub lock: PathBuf,
    pub log: PathBuf,
}

#[derive(Debug, Error)]
pub enum PathsError {
    #[error("{0}")]
    Dir(#[from] PrivateDirError),
}

impl Hint for PathsError {
    fn exit_code(&self) -> u8 {
        3
    }

    fn fix(&self) -> Option<Fix> {
        Some("remove it, or set XDG_RUNTIME_DIR to a private directory".into())
    }
}

impl Paths {
    /// The paths of the daemon for `root` (canonical) and `ned` build
    /// `version`, in `runtime_dir`.
    pub fn new(runtime_dir: &Path, root: &Path, version: &str) -> Paths {
        let name = name(root, version);
        let path = |ext: &str| runtime_dir.join(format!("{name}.{ext}"));
        Paths {
            version: version.to_string(),
            socket: path("sock"),
            lock: path("lock"),
            log: path("log"),
        }
    }
}

/// The directory for daemon files, `$XDG_RUNTIME_DIR/ned` or `$TMPDIR/ned-UID`:
/// created private if missing, and rejected if others own or can access it.
pub fn runtime_dir() -> Result<PathBuf, PathsError> {
    let dir = match env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty()) {
        Some(base) => PathBuf::from(base).join("ned"),
        None => env::temp_dir().join(format!("ned-{}", uid())),
    };
    private_dir(&dir)?;
    Ok(dir)
}

/// The file name stem for `root`'s daemon under `version` of `ned`.
fn name(root: &Path, version: &str) -> String {
    // FNV-1a: stable across Rust releases, unlike `DefaultHasher`.
    let bytes = root
        .as_os_str()
        .as_bytes()
        .iter()
        .chain(&[0])
        .chain(version.as_bytes());
    let hash = bytes.fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_stable_and_distinct_per_root_and_version() {
        let dir = Path::new("/run/ned");
        let a = Paths::new(dir, Path::new("/src/a"), "0.1.0 (abc)");
        assert_eq!(a, Paths::new(dir, Path::new("/src/a"), "0.1.0 (abc)"));
        assert_eq!(a.version, "0.1.0 (abc)");
        assert_ne!(
            a.socket,
            Paths::new(dir, Path::new("/src/b"), "0.1.0 (abc)").socket
        );
        assert_ne!(
            a.socket,
            Paths::new(dir, Path::new("/src/a"), "0.1.0 (def)").socket
        );
        for path in [&a.socket, &a.lock, &a.log] {
            assert_eq!(path.parent(), Some(dir));
        }
        assert_ne!(a.socket, a.lock);
        assert_ne!(a.socket, a.log);
        assert_ne!(a.lock, a.log);
    }
}
