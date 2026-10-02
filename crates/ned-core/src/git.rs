//! Committing an invocation's edits to git (command-language spec, §1.3),
//! through git's plumbing commands.

use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};

use thiserror::Error;

use crate::diff;

/// A git repository's working tree.
#[derive(Debug)]
pub struct Repo {
    top: PathBuf,
}

/// One file's edit: its text before (`None` if it was created) and after
/// (`None` if it was removed).
#[derive(Debug, Clone, Copy)]
pub struct FileEdit<'a> {
    pub path: &'a Path,
    pub before: Option<&'a str>,
    pub after: Option<&'a str>,
}

/// A commit that isn't on `HEAD` yet, and the index to stage once it is,
/// with the index locked until then.
#[derive(Debug)]
pub struct Prepared {
    pub commit: String,
    parent: Option<String>,
    index: IndexLock,
}

/// Git's lock on the index (`index.lock`), held while ned reads the index
/// and puts its next version in place. Dropping it removes the lock and the
/// files beside the index.
#[derive(Debug)]
struct IndexLock {
    index: PathBuf,
    lock: PathBuf,
    /// The index to put in place, if the edits change it.
    next: Option<PathBuf>,
    /// Once `next` is in place, the index it replaced, if there was one.
    replaced: Option<Option<PathBuf>>,
}

impl Drop for IndexLock {
    fn drop(&mut self) {
        let beside = [self.next.as_ref(), self.replaced.iter().flatten().next()];
        for path in beside.into_iter().flatten() {
            let _ = std::fs::remove_file(path);
        }
        let _ = std::fs::remove_file(&self.lock);
    }
}

/// An index entry to write: `blob` at `path` with `mode`, or with no blob,
/// the path removed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    mode: String,
    blob: Option<String>,
    path: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum GitError {
    #[error("--commit needs git, but it isn't installed; install it, or leave out --commit")]
    NoGit,
    #[error(
        "--commit needs a git repository, but {0} isn't in one; run git init, or leave out --commit"
    )]
    NotARepo(String),
    #[error("a merge is in progress; finish it with git commit, or leave out --commit")]
    Merging,
    /// `edit` is the index of the edit.
    #[error(
        "{path} isn't in the repository at {top}; commit it in a ned call of its own, or leave out --commit"
    )]
    Outside {
        path: String,
        top: String,
        edit: usize,
    },
    /// `edit` is the index of the file's first edit.
    #[error("{path} is ignored by git; edit it without --commit, or stop ignoring it")]
    Ignored { path: String, edit: usize },
    #[error(
        "the edit to {path} overlaps its uncommitted changes at line {line}; commit or stash them first, or leave out --commit"
    )]
    Overlap { path: String, line: usize },
    #[error(
        "{0} isn't UTF-8 text in HEAD or the index; commit it with git instead, or leave out --commit"
    )]
    NotUtf8(String),
    #[error("nothing to commit: the script leaves every file as HEAD has it")]
    NothingToCommit,
    #[error("HEAD moved while ned ran; nothing was written, so rerun the script")]
    HeadMoved,
    #[error(
        "{0} exists, so another git process is using the index; rerun once it's done, or remove {0} if none is running"
    )]
    IndexLocked(String),
    #[error("git {command} failed: {message}")]
    Failed { command: String, message: String },
}

impl Repo {
    /// The repository whose working tree holds `path`, a file or directory that
    /// may not exist yet.
    pub fn discover(path: &Path) -> Result<Repo, GitError> {
        let dir = existing_dir(path);
        let top = toplevel(dir)?.ok_or_else(|| GitError::NotARepo(dir.display().to_string()))?;
        Ok(Repo { top })
    }

    /// Makes the commit of `edits` on `HEAD`, with `message`, without moving
    /// `HEAD`. Several edits to one file apply one after another.
    pub fn prepare(&self, edits: &[FileEdit], message: &str) -> Result<Prepared, GitError> {
        if edits.is_empty() {
            return Err(GitError::NothingToCommit);
        }
        if self.verify("MERGE_HEAD")?.is_some() {
            return Err(GitError::Merging);
        }
        let mut index = self.lock_index()?;
        let parent = self.verify("HEAD^{commit}")?;
        let mut committed = Vec::new();
        let mut staged = Vec::new();
        // Each file's edits, in order, by its path in the repository, with the
        // index of its first edit.
        let mut files: Vec<(String, usize, Vec<&FileEdit>)> = Vec::new();
        for (i, edit) in edits.iter().enumerate() {
            let path = self.relative(edit.path)?.ok_or_else(|| GitError::Outside {
                path: edit.path.display().to_string(),
                top: self.top.display().to_string(),
                edit: i,
            })?;
            match files.iter_mut().find(|(p, _, _)| *p == path) {
                Some((_, _, steps)) => steps.push(edit),
                None => files.push((path, i, vec![edit])),
            }
        }
        for (path, first, steps) in files {
            let head = match parent {
                Some(ref parent) => self.entry(&["ls-tree", "-z", parent, "--", &path])?,
                None => None,
            };
            let index = self.index_entry(&path)?;
            if index
                .as_ref()
                .is_some_and(|e| e.stage.as_deref() != Some("0"))
            {
                return Err(GitError::Merging);
            }
            let mut commit = head.as_ref().map(|e| self.text(e, &path)).transpose()?;
            let mut index_text = index.as_ref().map(|e| self.text(e, &path)).transpose()?;
            // What to stage: `None` leaves the index as it is, `Some(None)`
            // removes the file.
            let mut stage: Option<Option<String>> = None;
            for edit in &steps {
                let Some(after) = edit.after else {
                    commit = None;
                    if index_text.take().is_some() {
                        stage = Some(None);
                    }
                    continue;
                };
                let before = edit.before.unwrap_or_default();
                let (next_commit, next_stage) =
                    apply_edit(before, after, commit.as_deref(), index_text.as_deref()).map_err(
                        |line| GitError::Overlap {
                            path: path.clone(),
                            line,
                        },
                    )?;
                commit = Some(next_commit);
                if let Some(next_stage) = next_stage {
                    index_text = Some(next_stage.clone());
                    stage = Some(Some(next_stage));
                }
            }
            if commit.is_some() && head.is_none() && index.is_none() && self.ignored(&path)? {
                return Err(GitError::Ignored { path, edit: first });
            }
            let last = steps.last().expect("a file has edits").path;
            let mode = head
                .as_ref()
                .or(index.as_ref())
                .map_or_else(|| new_mode(last), |e| e.mode.clone());
            if commit.is_some() || head.is_some() {
                committed.push(Entry {
                    mode: mode.clone(),
                    blob: commit.map(|text| self.blob(&text, &path)).transpose()?,
                    path: path.clone(),
                });
            }
            if let Some(stage) = stage {
                staged.push(Entry {
                    mode: index.map_or(mode, |e| e.mode),
                    blob: stage.map(|text| self.blob(&text, &path)).transpose()?,
                    path,
                });
            }
        }
        if !staged.is_empty() {
            self.write_next(&mut index, &staged)?;
        }
        let tree = self.tree(parent.as_deref(), &committed)?;
        if parent.is_some() && self.verify("HEAD^{tree}")?.as_deref() == Some(tree.as_str()) {
            return Err(GitError::NothingToCommit);
        }
        let mut args = vec!["commit-tree", tree.as_str(), "-m", message];
        if let Some(parent) = &parent {
            args.extend(["-p", parent]);
        }
        if self.signs()? {
            args.push("-S");
        }
        let commit = self.git(&args, &[], None)?.trim_end().to_string();
        Ok(Prepared {
            commit,
            parent,
            index,
        })
    }

    /// Moves `HEAD` to the prepared commit, if it is still the commit's
    /// parent.
    pub fn advance(&self, prepared: &Prepared) -> Result<(), GitError> {
        let old = prepared.parent.as_deref().unwrap_or_default();
        let args = [
            "update-ref",
            "-m",
            "ned --commit",
            "HEAD",
            &prepared.commit,
            old,
        ];
        let out = run(&self.top, &args, &[], None)?;
        if out.status.success() {
            return Ok(());
        }
        if self.verify("HEAD")? != prepared.parent {
            return Err(GitError::HeadMoved);
        }
        Err(failed(&args, &out))
    }

    /// Moves `HEAD` back to the prepared commit's parent, and puts back the
    /// index that `stage` replaced.
    pub fn retreat(&self, prepared: &Prepared) -> Result<(), GitError> {
        let commit = prepared.commit.as_str();
        let mut args = vec!["update-ref", "-m", "ned --commit (rolled back)"];
        match &prepared.parent {
            Some(parent) => args.extend(["HEAD", parent, commit]),
            None => args.extend(["-d", "HEAD", commit]),
        }
        let moved = self.git(&args, &[], None).map(drop);
        let index = &prepared.index;
        let restored = match &index.replaced {
            Some(Some(replaced)) => std::fs::rename(replaced, &index.index),
            Some(None) => std::fs::remove_file(&index.index),
            None => Ok(()),
        };
        moved.and(restored.map_err(|err| io_failed("update-index", err)))
    }

    /// Puts the prepared index in place: each edit staged, as the commit has it
    /// relative to the staged text.
    pub fn stage(&self, prepared: &mut Prepared) -> Result<(), GitError> {
        let index = &mut prepared.index;
        let Some(next) = &index.next else {
            return Ok(());
        };
        let staged = (|| {
            let replaced = match index.index.exists() {
                true => {
                    let replaced = beside(&index.index, ".ned-replaced");
                    std::fs::hard_link(&index.index, &replaced)?;
                    Some(replaced)
                }
                false => None,
            };
            std::fs::rename(next, &index.index)?;
            Ok(replaced)
        })();
        index.replaced = Some(staged.map_err(|err| io_failed("update-index", err))?);
        Ok(())
    }

    /// The abbreviated name of `commit`.
    pub fn short(&self, commit: &str) -> Result<String, GitError> {
        Ok(self
            .git(&["rev-parse", "--short", commit], &[], None)?
            .trim_end()
            .to_string())
    }

    /// Runs `git ARGS` in the working tree; its stdout.
    fn git(
        &self,
        args: &[&str],
        env: &[(&str, &Path)],
        input: Option<&str>,
    ) -> Result<String, GitError> {
        let out = run(&self.top, args, env, input)?;
        if !out.status.success() {
            return Err(failed(args, &out));
        }
        String::from_utf8(out.stdout).map_err(|_| GitError::Failed {
            command: args[0].into(),
            message: "its output isn't UTF-8".into(),
        })
    }

    /// The object `rev` names, if it exists.
    fn verify(&self, rev: &str) -> Result<Option<String>, GitError> {
        let out = run(&self.top, &["rev-parse", "-q", "--verify", rev], &[], None)?;
        Ok(out
            .status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim_end().to_string()))
    }

    /// `path` relative to the working tree, with `/` separators, or `None` if
    /// the repository doesn't hold it (or one nested in its tree, or a
    /// submodule, does).
    fn relative(&self, path: &Path) -> Result<Option<String>, GitError> {
        let path = canonical(path);
        let Ok(rel) = path.strip_prefix(&self.top) else {
            return Ok(None);
        };
        if toplevel(existing_dir(&path))?.as_ref() != Some(&self.top) {
            return Ok(None);
        }
        let parts: Option<Vec<&str>> = rel
            .components()
            .map(|c| match c {
                Component::Normal(part) => part.to_str(),
                _ => None,
            })
            .collect();
        Ok(parts.map(|p| p.join("/")))
    }

    /// The one entry that `ls-tree -z` or `ls-files -s -z` (`args`) lists.
    fn entry(&self, args: &[&str]) -> Result<Option<Listed>, GitError> {
        let out = self.git(args, &[], None)?;
        Ok(out.split('\0').find(|l| !l.is_empty()).and_then(|line| {
            let (fields, _) = line.split_once('\t')?;
            let fields: Vec<&str> = fields.split(' ').collect();
            Some(match args[0] {
                // MODE TYPE BLOB
                "ls-tree" => Listed {
                    mode: fields.first()?.to_string(),
                    blob: fields.get(2)?.to_string(),
                    stage: None,
                },
                // MODE BLOB STAGE
                _ => Listed {
                    mode: fields.first()?.to_string(),
                    blob: fields.get(1)?.to_string(),
                    stage: Some(fields.get(2)?.to_string()),
                },
            })
        }))
    }

    /// The index's entry for `path`, unless it only marks an intent to add it
    /// (`git add -N`).
    fn index_entry(&self, path: &str) -> Result<Option<Listed>, GitError> {
        let Some(entry) = self.entry(&["ls-files", "-s", "-z", "--", path])? else {
            return Ok(None);
        };
        // An intent-to-add entry holds the empty blob; only diff-files tells it
        // apart, as a file added in the working tree.
        let args = [
            "diff-files",
            "--diff-filter=A",
            "--name-only",
            "-z",
            "--",
            path,
        ];
        Ok(self.git(&args, &[], None)?.is_empty().then_some(entry))
    }

    fn ignored(&self, path: &str) -> Result<bool, GitError> {
        // check-ignore takes paths, not pathspecs, and refuses literal ones.
        let args = ["check-ignore", "-q", "--", path];
        let out = run(
            &self.top,
            &args,
            &[("GIT_LITERAL_PATHSPECS", Path::new("0"))],
            None,
        )?;
        match out.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(failed(&args, &out)),
        }
    }

    /// Whether `commit.gpgSign` asks for commits to be signed.
    fn signs(&self) -> Result<bool, GitError> {
        let args = ["config", "--type=bool", "commit.gpgSign"];
        let out = run(&self.top, &args, &[], None)?;
        match out.status.code() {
            Some(0) => Ok(out.stdout.starts_with(b"true")),
            Some(1) => Ok(false),
            _ => Err(failed(&args, &out)),
        }
    }

    /// The text of `entry`'s blob, as the working tree would have it.
    fn text(&self, entry: &Listed, path: &str) -> Result<String, GitError> {
        let flag = format!("--path={path}");
        let args = ["cat-file", "--filters", &flag, &entry.blob];
        let out = run(&self.top, &args, &[], None)?;
        if !out.status.success() {
            return Err(failed(&args, &out));
        }
        String::from_utf8(out.stdout).map_err(|_| GitError::NotUtf8(path.into()))
    }

    /// Writes `text` as the blob of `path`, as `git add` would; its name.
    fn blob(&self, text: &str, path: &str) -> Result<String, GitError> {
        let path = format!("--path={path}");
        let blob = self.git(&["hash-object", "-w", "--stdin", &path], &[], Some(text))?;
        Ok(blob.trim_end().to_string())
    }

    /// Writes the tree of `HEAD` (or an empty one) with `entries`, in an index
    /// of its own; its name.
    fn tree(&self, parent: Option<&str>, entries: &[Entry]) -> Result<String, GitError> {
        let git_dir = self.git(&["rev-parse", "--absolute-git-dir"], &[], None)?;
        let index =
            PathBuf::from(git_dir.trim_end()).join(format!("ned-index-{}", std::process::id()));
        let env = [("GIT_INDEX_FILE", index.as_path())];
        let tree = (|| {
            let read = match parent {
                Some(parent) => vec!["read-tree", parent],
                None => vec!["read-tree", "--empty"],
            };
            self.git(&read, &env, None)?;
            self.update_index(entries, &env)?;
            self.git(&["write-tree"], &env, None)
        })();
        let _ = std::fs::remove_file(&index);
        Ok(tree?.trim_end().to_string())
    }

    /// Takes git's lock on the index, as `git add` would.
    fn lock_index(&self) -> Result<IndexLock, GitError> {
        let index = self.git(&["rev-parse", "--git-path", "index"], &[], None)?;
        let index = self.top.join(index.trim_end());
        let lock = beside(&index, ".lock");
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
        {
            Ok(_) => Ok(IndexLock {
                index,
                lock,
                next: None,
                replaced: None,
            }),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(GitError::IndexLocked(lock.display().to_string()))
            }
            Err(err) => Err(io_failed("update-index", err)),
        }
    }

    /// Writes the index with `entries` beside the locked one, to put in place
    /// later.
    fn write_next(&self, lock: &mut IndexLock, entries: &[Entry]) -> Result<(), GitError> {
        let next = beside(&lock.index, &format!(".ned-{}", std::process::id()));
        if lock.index.exists() {
            std::fs::copy(&lock.index, &next).map_err(|err| io_failed("update-index", err))?;
        }
        lock.next = Some(next.clone());
        self.update_index(entries, &[("GIT_INDEX_FILE", &next)])
    }

    /// Writes `entries` to the index, or the one `env` names.
    fn update_index(&self, entries: &[Entry], env: &[(&str, &Path)]) -> Result<(), GitError> {
        let mut add = Vec::new();
        let mut remove = Vec::new();
        for e in entries {
            match &e.blob {
                Some(blob) => add.extend(["--cacheinfo", &e.mode, blob, &e.path]),
                None => remove.push(e.path.as_str()),
            }
        }
        let add_command = ["update-index", "--add"].as_slice();
        let remove_command = ["update-index", "--force-remove", "--"].as_slice();
        for (command, args) in [(add_command, add), (remove_command, remove)] {
            if !args.is_empty() {
                self.git(&[command, &args].concat(), env, None)?;
            }
        }
        Ok(())
    }
}

/// An entry that `ls-tree` or `ls-files -s` listed.
#[derive(Debug)]
struct Listed {
    mode: String,
    blob: String,
    /// `None` for a tree's entry.
    stage: Option<String>,
}

/// The mode of a file that isn't in `HEAD` or the index.
#[cfg_attr(not(unix), allow(unused_variables))]
fn new_mode(path: &Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0) {
            return "100755".into();
        }
    }
    "100644".into()
}

/// The top of the working tree holding `dir`, if one does.
fn toplevel(dir: &Path) -> Result<Option<PathBuf>, GitError> {
    let out = run(dir, &["rev-parse", "--show-toplevel"], &[], None)?;
    Ok(out.status.success().then(|| {
        let top = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n'));
        top.canonicalize().unwrap_or(top)
    }))
}

/// The nearest of `path` and its ancestors that is a directory.
fn existing_dir(path: &Path) -> &Path {
    path.ancestors().find(|dir| dir.is_dir()).unwrap_or(path)
}

/// `path` with its longest existing ancestor canonicalized, for a file the
/// script creates.
fn canonical(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut at = path;
    loop {
        if let Ok(found) = at.canonicalize() {
            return rest.iter().rev().fold(found, |p, part| p.join(part));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                at = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// `path` with `suffix` added to its name.
fn beside(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn io_failed(command: &str, err: std::io::Error) -> GitError {
    GitError::Failed {
        command: command.into(),
        message: err.to_string(),
    }
}

/// Runs `git ARGS` in `dir`, with literal pathspecs, `env` and `input`.
fn run(
    dir: &Path,
    args: &[&str],
    env: &[(&str, &Path)],
    input: Option<&str>,
) -> Result<Output, GitError> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_LITERAL_PATHSPECS", "1")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        cmd.env(key, value);
    }
    let io_failed = |err| io_failed(args[0], err);
    let mut child = cmd.spawn().map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => GitError::NoGit,
        _ => io_failed(err),
    })?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        stdin.write_all(input.as_bytes()).map_err(io_failed)?;
    }
    child.wait_with_output().map_err(io_failed)
}

fn failed(args: &[&str], out: &Output) -> GitError {
    GitError::Failed {
        command: args[0].into(),
        message: String::from_utf8_lossy(&out.stderr).trim_end().to_string(),
    }
}

/// The text to commit for an edit from `before` to `after`, given `HEAD`'s
/// text (or the index's, for a file `HEAD` lacks), and the text to stage
/// given the index's (§1.3): the edit applied to each. `None` to stage leaves
/// the index as it is. The error is the line of an uncommitted change that
/// the edit overlaps.
fn apply_edit(
    before: &str,
    after: &str,
    head: Option<&str>,
    index: Option<&str>,
) -> Result<(String, Option<String>), usize> {
    let commit = match head.or(index) {
        Some(base) => diff::merge(before, base, after)?,
        None => after.to_string(),
    };
    let stage = match (index, head) {
        (Some(index), _) => Some(diff::merge(before, index, after)?),
        (None, None) => Some(commit.clone()),
        (None, Some(_)) => None,
    };
    Ok((commit, stage))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "a\nb\nc\n";

    #[test]
    fn a_clean_files_edit_is_committed_and_staged_as_written() {
        let after = "A\nb\nc\n";
        assert_eq!(
            apply_edit(HEAD, after, Some(HEAD), Some(HEAD)),
            Ok((after.into(), Some(after.into())))
        );
    }

    #[test]
    fn unstaged_changes_stay_out_of_the_commit_and_the_index() {
        let before = "a\nb\nC\n";
        let after = "A\nb\nC\n";
        assert_eq!(
            apply_edit(before, after, Some(HEAD), Some(HEAD)),
            Ok(("A\nb\nc\n".into(), Some("A\nb\nc\n".into())))
        );
    }

    #[test]
    fn staged_changes_stay_staged_but_uncommitted() {
        let staged = "a\nb\nC\n";
        assert_eq!(
            apply_edit(staged, "A\nb\nC\n", Some(HEAD), Some(staged)),
            Ok(("A\nb\nc\n".into(), Some("A\nb\nC\n".into())))
        );
    }

    #[test]
    fn an_edit_overlapping_uncommitted_changes_is_refused() {
        assert_eq!(
            apply_edit("a\nX\nc\n", "a\nY\nc\n", Some(HEAD), Some(HEAD)),
            Err(2)
        );
        assert_eq!(
            apply_edit(HEAD, "a\nY\nc\n", Some(HEAD), Some("a\nX\nc\n")),
            Err(2)
        );
    }

    #[test]
    fn a_new_or_untracked_file_is_committed_whole() {
        assert_eq!(
            apply_edit("", "x\n", None, None),
            Ok(("x\n".into(), Some("x\n".into())))
        );
        assert_eq!(
            apply_edit("x\n", "y\n", None, None),
            Ok(("y\n".into(), Some("y\n".into())))
        );
    }

    #[test]
    fn a_staged_removal_stays_staged() {
        assert_eq!(
            apply_edit(HEAD, "A\nb\nc\n", Some(HEAD), None),
            Ok(("A\nb\nc\n".into(), None))
        );
    }

    #[test]
    fn a_staged_new_files_unstaged_changes_stay_uncommitted() {
        let staged = "a\nb\nc\n";
        assert_eq!(
            apply_edit("a\nb\nc\nSECRET\n", "A\nb\nc\nSECRET\n", None, Some(staged)),
            Ok(("A\nb\nc\n".into(), Some("A\nb\nc\n".into())))
        );
    }

    /// A repository with one commit of `f.txt` holding `HEAD`.
    fn repo() -> (tempfile::TempDir, Repo) {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let out = run(dir.path(), args, &[], None).unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        git(&["init", "-q"]);
        for (key, value) in [
            ("user.name", "A U Thor"),
            ("user.email", "author@example.com"),
            ("commit.gpgSign", "false"),
        ] {
            git(&["config", key, value]);
        }
        std::fs::write(dir.path().join("f.txt"), HEAD).unwrap();
        git(&["add", "f.txt"]);
        git(&["commit", "-qm", "init"]);
        let repo = Repo::discover(dir.path()).unwrap();
        (dir, repo)
    }

    #[test]
    fn head_moving_after_prepare_is_refused() {
        let (dir, repo) = repo();
        let edit = FileEdit {
            path: &dir.path().join("f.txt"),
            before: Some(HEAD),
            after: Some("A\nb\nc\n"),
        };
        let prepared = repo.prepare(&[edit], "m").unwrap();
        let tree = ["commit-tree", "HEAD^{tree}", "-p", "HEAD", "-m", "moved"];
        let moved = repo.git(&tree, &[], None).unwrap().trim_end().to_string();
        repo.git(&["update-ref", "HEAD", &moved], &[], None)
            .unwrap();
        assert_eq!(repo.advance(&prepared), Err(GitError::HeadMoved));
        assert_eq!(repo.verify("HEAD").unwrap(), Some(moved));
    }

    #[test]
    fn retreat_puts_head_and_the_index_back() {
        let (dir, repo) = repo();
        let path = dir.path().join("f.txt");
        let edit = FileEdit {
            path: &path,
            before: Some(HEAD),
            after: Some("A\nb\nc\n"),
        };
        let head = repo.verify("HEAD").unwrap();
        let index = std::fs::read(dir.path().join(".git/index")).unwrap();
        let mut prepared = repo.prepare(&[edit], "m").unwrap();
        repo.advance(&prepared).unwrap();
        repo.stage(&mut prepared).unwrap();
        assert_ne!(std::fs::read(dir.path().join(".git/index")).unwrap(), index);
        repo.retreat(&prepared).unwrap();
        drop(prepared);
        assert_eq!(repo.verify("HEAD").unwrap(), head);
        assert_eq!(std::fs::read(dir.path().join(".git/index")).unwrap(), index);
        let left: Vec<_> = std::fs::read_dir(dir.path().join(".git"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.starts_with("index") || name.starts_with("ned-"))
            .collect();
        assert_eq!(left, ["index"]);
    }
}
