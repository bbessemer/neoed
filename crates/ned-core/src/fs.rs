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
        let missing = dir.path().join("no-such-dir").join("b.rs");
        let result = write_atomic(&[(a.clone(), "new a".into()), (missing, "new b".into())]);
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&a).unwrap(), "old a");
        assert_eq!(entries(dir.path()), ["a.rs"]);
    }
}
