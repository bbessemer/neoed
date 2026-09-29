//! The workspace: its root, and with `-w`, its files (spec §1.1, §2.4).

use std::io;
use std::path::{Path, PathBuf};

/// The workspace containing `dir`: the nearest directory from `dir` up that
/// holds `.git`, `.hg` or `.jj`, or else `dir`; canonical.
pub fn root(dir: &Path) -> io::Result<PathBuf> {
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

/// Every file in the workspace at `root`, as `-w` takes them: regular files
/// that ignore files and git's excludes leave in, outside hidden
/// directories, in path order. Paths are relative to `cwd` when under it.
pub fn files(root: &Path, cwd: &Path) -> Vec<String> {
    let _ = (root, cwd);
    todo!()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    #[test]
    fn the_workspace_is_the_nearest_vcs_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        assert_eq!(super::root(&root.join("a/b")).unwrap(), root);
        fs::create_dir(root.join("a/.jj")).unwrap();
        assert_eq!(super::root(&root.join("a/b")).unwrap(), root.join("a"));
    }

    #[test]
    fn without_a_vcs_root_the_workspace_is_the_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("a");
        fs::create_dir(&dir).unwrap();
        assert_eq!(super::root(&dir).unwrap(), dir.canonicalize().unwrap());
    }
}
