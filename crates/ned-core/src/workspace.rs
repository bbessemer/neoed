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
    let mut paths: Vec<PathBuf> = ignore::WalkBuilder::new(root)
        .require_git(false)
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|t| t.is_file()))
        .map(ignore::DirEntry::into_path)
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|path| path.strip_prefix(cwd).unwrap_or(path).display().to_string())
        .collect()
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

    fn tree(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for (path, text) in files {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        (dir, root)
    }

    #[test]
    fn files_respect_ignore_files_and_skip_hidden_ones() {
        let (_dir, root) = tree(&[
            (".gitignore", "target/\n*.log\n"),
            ("src/b/c.rs", ""),
            ("src/a.rs", ""),
            ("target/x.rs", ""),
            ("a.log", ""),
            (".hidden/d.rs", ""),
            (".env", ""),
            ("README.md", ""),
            ("src/.ignore", "gen.rs\n"),
            ("src/gen.rs", ""),
        ]);
        assert_eq!(files(&root, &root), ["README.md", "src/a.rs", "src/b/c.rs"]);
    }

    #[test]
    fn files_outside_the_working_directory_are_absolute() {
        let (_dir, root) = tree(&[("src/a.rs", ""), ("README.md", "")]);
        let readme = root.join("README.md").display().to_string();
        assert_eq!(files(&root, &root.join("src")), [readme.as_str(), "a.rs"]);
    }

    #[test]
    fn files_skip_symlinked_directories() {
        let (_dir, root) = tree(&[("src/a.rs", "")]);
        std::os::unix::fs::symlink(root.join("src"), root.join("link")).unwrap();
        assert_eq!(files(&root, &root), ["src/a.rs"]);
    }
}
