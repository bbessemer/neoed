//! Where a workspace's daemon listens, locks and logs.

use std::fs::{self, DirBuilder};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::{env, io};

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
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error(
        "{} is {why}; remove it, or set XDG_RUNTIME_DIR to a private directory",
        dir.display()
    )]
    UnsafeDir { dir: PathBuf, why: &'static str },
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

fn private_dir(dir: &Path) -> Result<(), PathsError> {
    let io_error = |source| PathsError::Io {
        path: dir.to_path_buf(),
        source,
    };
    match DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => return Ok(()),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(io_error(err)),
    }
    let meta = fs::symlink_metadata(dir).map_err(io_error)?;
    let why = if !meta.is_dir() {
        "not a directory"
    } else if meta.uid() != uid() {
        "owned by another user"
    } else if meta.mode() & 0o077 != 0 {
        "accessible to other users"
    } else {
        return Ok(());
    };
    Err(PathsError::UnsafeDir {
        dir: dir.to_path_buf(),
        why,
    })
}

fn uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// The workspace containing `dir`: the nearest directory from `dir` up that
/// holds `.git`, `.hg` or `.jj`, or else `dir`; canonical.
pub fn workspace_root(dir: &Path) -> io::Result<PathBuf> {
    let dir = dir.canonicalize()?;
    let root = dir
        .ancestors()
        .find(|d| {
            [".git", ".hg", ".jj"]
                .iter()
                .any(|vcs| d.join(vcs).exists())
        })
        .unwrap_or(&dir);
    Ok(root.to_path_buf())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

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

    #[test]
    fn a_missing_runtime_dir_is_created_private() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ned");
        private_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
        private_dir(&dir).unwrap();
    }

    #[test]
    fn a_runtime_dir_others_can_access_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ned");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o770)).unwrap();
        let err = private_dir(&dir).unwrap_err();
        assert!(matches!(err, PathsError::UnsafeDir { .. }), "{err}");
        assert!(err.to_string().contains("XDG_RUNTIME_DIR"), "{err}");
    }

    #[test]
    fn a_runtime_path_that_is_a_file_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ned");
        fs::write(&dir, "").unwrap();
        assert!(matches!(
            private_dir(&dir),
            Err(PathsError::UnsafeDir { .. })
        ));
    }

    #[test]
    fn the_workspace_is_the_nearest_vcs_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        assert_eq!(workspace_root(&root.join("a/b")).unwrap(), root);
        fs::create_dir(root.join("a/.jj")).unwrap();
        assert_eq!(workspace_root(&root.join("a/b")).unwrap(), root.join("a"));
    }

    #[test]
    fn without_a_vcs_root_the_workspace_is_the_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("a");
        fs::create_dir(&dir).unwrap();
        assert_eq!(workspace_root(&dir).unwrap(), dir.canonicalize().unwrap());
    }
}
