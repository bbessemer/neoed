//! Where a workspace's daemon listens, locks and logs.

use std::io;
use std::path::{Path, PathBuf};

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
        let _ = (runtime_dir, root, version);
        todo!()
    }
}

/// The directory for daemon files, `$XDG_RUNTIME_DIR/ned` or `$TMPDIR/ned-UID`:
/// created private if missing, and rejected if others own or can access it.
pub fn runtime_dir() -> Result<PathBuf, PathsError> {
    todo!()
}

/// The file name stem for `root`'s daemon under `version` of `ned`.
fn name(root: &Path, version: &str) -> String {
    let _ = (root, version);
    todo!()
}

fn private_dir(dir: &Path) -> Result<(), PathsError> {
    let _ = dir;
    todo!()
}

/// The workspace containing `dir`: the nearest directory from `dir` up that
/// holds `.git`, `.hg` or `.jj`, or else `dir`; canonical.
pub fn workspace_root(dir: &Path) -> io::Result<PathBuf> {
    let _ = dir;
    todo!()
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
