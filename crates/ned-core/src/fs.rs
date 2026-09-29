//! Atomic writes of edited files.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Replaces the contents of every file in `files`, preserving permissions and
/// writing through symlinks to their targets.
///
/// All contents are first staged in temporary files beside their targets;
/// only once every one is staged are they renamed into place. A failure while
/// staging leaves every target untouched and removes the staged files.
pub fn write_atomic(files: &[(PathBuf, String)]) -> io::Result<()> {
    let mut staged: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(files.len());
    for (path, contents) in files {
        match stage(path, contents) {
            Ok(pair) => staged.push(pair),
            Err(err) => {
                for (temp, _) in &staged {
                    let _ = fs::remove_file(temp);
                }
                return Err(err);
            }
        }
    }
    for (i, (temp, target)) in staged.iter().enumerate() {
        if let Err(err) = fs::rename(temp, target) {
            for (temp, _) in &staged[i..] {
                let _ = fs::remove_file(temp);
            }
            return Err(err);
        }
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
        write_atomic(&[(a.clone(), "new a\r\n".into()), (b.clone(), "new b".into())]).unwrap();
        assert_eq!(fs::read_to_string(&a).unwrap(), "new a\r\n");
        assert_eq!(fs::read_to_string(&b).unwrap(), "new b");
        assert_eq!(entries(dir.path()), ["a.rs", "b.rs"]);
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
        write_atomic(&[
            (script.clone(), "new".into()),
            (private.clone(), "new".into()),
        ])
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
        write_atomic(&[(link.clone(), "new".into())]).unwrap();
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
        let result = write_atomic(&[(a.clone(), "new a".into()), (under_a_file, "new b".into())]);
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&a).unwrap(), "old a");
        assert_eq!(entries(dir.path()), ["a.rs"]);
    }
}
