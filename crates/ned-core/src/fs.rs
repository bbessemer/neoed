//! Reads and atomic writes of edited files, and private state directories.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use thiserror::Error;

use crate::hint::{self, Hint};

/// The text of the file at `path`, `None` if it's missing.
pub fn read(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// `path`, absolute, with each part that exists resolved as the system
/// resolves it (symlinks, `.` and `..`), and the rest normalized as text: one
/// path for a file however it's reached, even one not made yet.
pub fn canonical(path: &Path) -> PathBuf {
    let Ok(absolute) = std::path::absolute(path) else {
        return path.to_path_buf();
    };
    let mut resolved = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            part => {
                resolved.push(part);
                if let Ok(real) = resolved.canonicalize() {
                    resolved = real;
                }
            }
        }
    }
    resolved
}

/// Replaces the contents of every file in `files`, preserving permissions and
/// writing through symlinks to their targets, and removes the files in
/// `remove`.
///
/// All contents are first staged in temporary files beside their targets,
/// and each file to remove is renamed to one; only once every one is staged
/// are they renamed into place. A failure while staging leaves every target
/// untouched and removes the staged files.
pub fn write_atomic(files: &[(PathBuf, String)], remove: &[PathBuf]) -> io::Result<()> {
    let mut staged: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(files.len());
    let mut removed: Vec<(PathBuf, &PathBuf)> = Vec::with_capacity(remove.len());
    let roll_back = |staged: &[(PathBuf, PathBuf)], removed: &[(PathBuf, &PathBuf)]| {
        for (temp, _) in staged {
            let _ = fs::remove_file(temp);
        }
        for (temp, path) in removed {
            let _ = fs::rename(temp, path);
        }
    };
    for (path, contents) in files {
        match stage(path, contents) {
            Ok(pair) => staged.push(pair),
            Err(err) => {
                roll_back(&staged, &removed);
                return Err(err);
            }
        }
    }
    for path in remove {
        let temp = temp_path(path);
        if let Err(err) = fs::rename(path, &temp) {
            roll_back(&staged, &removed);
            return Err(err);
        }
        removed.push((temp, path));
    }
    for (i, (temp, target)) in staged.iter().enumerate() {
        if let Err(err) = fs::rename(temp, target) {
            roll_back(&staged[i..], &removed);
            return Err(err);
        }
    }
    for (temp, _) in &removed {
        let _ = fs::remove_file(temp);
    }
    Ok(())
}

/// Writes `contents` to a new temporary file beside the file `path` resolves
/// to, returning the temporary and target paths.
fn stage(path: &Path, contents: &str) -> io::Result<(PathBuf, PathBuf)> {
    let target = match fs::canonicalize(path) {
        Ok(resolved) => resolved,
        Err(err) if err.kind() == io::ErrorKind::NotFound => path.to_path_buf(),
        Err(err) => return Err(err),
    };
    let temp = temp_path(&target);
    let result = (|| {
        if let Some(dir) = target.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir)?;
        }
        let mut file = File::create_new(&temp)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        if let Ok(meta) = fs::metadata(&target) {
            fs::set_permissions(&temp, meta.permissions())?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => Ok((temp, target)),
        Err(err) => {
            let _ = fs::remove_file(&temp);
            Err(err)
        }
    }
}

fn temp_path(target: &Path) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    target.with_file_name(format!(".{name}.ned-{}-{n}.tmp", std::process::id()))
}

/// Why [`private_dir`] refused or couldn't make a directory. The caller gives
/// the fix, which depends on what the directory is for.
pub type PrivateDirError = hint::Error<PrivateDirErrorKind>;

#[derive(Debug, Error)]
pub enum PrivateDirErrorKind {
    #[error("cannot make {}: {source}", dir.display())]
    Io { dir: PathBuf, source: io::Error },
    #[error("{} is {why}", dir.display())]
    Unsafe { dir: PathBuf, why: &'static str },
}

impl Hint for PrivateDirErrorKind {
    /// Not the user's input: the state it runs in.
    fn exit_code(&self) -> u8 {
        3
    }
}

/// Creates `dir` accessible only to the user if it's missing; otherwise
/// checks that it's a directory the user owns and others can't access.
#[cfg(unix)]
pub fn private_dir(dir: &Path) -> Result<(), PrivateDirError> {
    use std::fs::DirBuilder;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};

    let io_error = |source| PrivateDirErrorKind::Io {
        dir: dir.to_path_buf(),
        source,
    };
    match DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => return Ok(()),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(io_error(err).into()),
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
    Err(PrivateDirErrorKind::Unsafe {
        dir: dir.to_path_buf(),
        why,
    }
    .into())
}

/// The user's id.
#[cfg(unix)]
pub fn uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn replaces_contents_of_every_file() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.rs");
        let b = dir.path().join("b.rs");
        fs::write(&a, "old a").unwrap();
        fs::write(&b, "old b").unwrap();
        write_atomic(
            &[(a.clone(), "new a\r\n".into()), (b.clone(), "new b".into())],
            &[],
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&a).unwrap(), "new a\r\n");
        assert_eq!(fs::read_to_string(&b).unwrap(), "new b");
        assert_eq!(entries(dir.path()), ["a.rs", "b.rs"]);
    }

    #[test]
    fn removes_files_once_the_others_are_written() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.rs");
        let b = dir.path().join("b.rs");
        fs::write(&a, "old a").unwrap();
        fs::write(&b, "old b").unwrap();
        write_atomic(&[(a.clone(), "new a".into())], std::slice::from_ref(&b)).unwrap();
        assert_eq!(fs::read_to_string(&a).unwrap(), "new a");
        assert_eq!(entries(dir.path()), ["a.rs"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_removal_leaves_every_file_untouched() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.rs");
        let locked = dir.path().join("locked");
        let b = locked.join("b.rs");
        fs::write(&a, "old a").unwrap();
        fs::create_dir(&locked).unwrap();
        fs::write(&b, "old b").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
        let result = write_atomic(&[(a.clone(), "new a".into())], std::slice::from_ref(&b));
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&a).unwrap(), "old a");
        assert_eq!(fs::read_to_string(&b).unwrap(), "old b");
        assert_eq!(entries(dir.path()), ["a.rs", "locked"]);
        assert_eq!(entries(&locked), ["b.rs"]);
    }

    #[cfg(unix)]
    #[test]
    fn preserves_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("run.sh");
        let private = dir.path().join("secret.txt");
        fs::write(&script, "old").unwrap();
        fs::write(&private, "old").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&private, fs::Permissions::from_mode(0o600)).unwrap();
        write_atomic(
            &[
                (script.clone(), "new".into()),
                (private.clone(), "new".into()),
            ],
            &[],
        )
        .unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&script), 0o755);
        assert_eq!(mode(&private), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn writes_through_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.rs");
        let link = dir.path().join("link.rs");
        fs::write(&target, "old").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        write_atomic(&[(link.clone(), "new".into())], &[]).unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        assert_eq!(entries(dir.path()), ["link.rs", "real.rs"]);
    }

    #[test]
    fn staging_failure_leaves_every_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.rs");
        fs::write(&a, "old a").unwrap();
        let under_a_file = a.join("b.rs");
        let result = write_atomic(
            &[(a.clone(), "new a".into()), (under_a_file, "new b".into())],
            &[],
        );
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&a).unwrap(), "old a");
        assert_eq!(entries(dir.path()), ["a.rs"]);
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_private_dir_is_created_private() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ned");
        private_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
        private_dir(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_dir_others_can_access_is_not_private() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ned");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o770)).unwrap();
        assert!(matches!(
            private_dir(&dir).map_err(|err| err.kind),
            Err(PrivateDirErrorKind::Unsafe {
                why: "accessible to other users",
                ..
            })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_file_is_not_a_private_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ned");
        fs::write(&dir, "").unwrap();
        assert!(matches!(
            private_dir(&dir).map_err(|err| err.kind),
            Err(PrivateDirErrorKind::Unsafe {
                why: "not a directory",
                ..
            })
        ));
    }
}
