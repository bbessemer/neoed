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

/// One file's edit: its text before the script (`None` if the script
/// created it) and the text written.
#[derive(Debug, Clone, Copy)]
pub struct FileEdit<'a> {
    pub path: &'a Path,
    pub before: Option<&'a str>,
    pub after: &'a str,
}

/// A commit that isn't on `HEAD` yet, and the index entries to stage once it
/// is.
#[derive(Debug)]
pub struct Prepared {
    pub commit: String,
    parent: Option<String>,
    staged: Vec<Entry>,
}

/// An index entry: `MODE,BLOB,PATH` for `update-index --cacheinfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    mode: String,
    blob: String,
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
    #[error("{path} is outside the repository at {top}; edit it without --commit")]
    Outside { path: String, top: String },
    #[error("{0} is ignored by git; edit it without --commit, or stop ignoring it")]
    Ignored(String),
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
    #[error("git {command} failed: {message}")]
    Failed { command: String, message: String },
}

impl Repo {
    /// The repository whose working tree holds `dir`.
    pub fn discover(dir: &Path) -> Result<Repo, GitError> {
        let out = run(dir, &["rev-parse", "--show-toplevel"], &[], None)?;
        if !out.status.success() {
            return Err(GitError::NotARepo(dir.display().to_string()));
        }
        let top = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n'));
        Ok(Repo {
            top: top.canonicalize().unwrap_or(top),
        })
    }

    /// Makes the commit of `edits` on `HEAD`, with `message`, without moving
    /// `HEAD`.
    pub fn prepare(&self, edits: &[FileEdit], message: &str) -> Result<Prepared, GitError> {
        if edits.is_empty() {
            return Err(GitError::NothingToCommit);
        }
        if self.verify("MERGE_HEAD")?.is_some() {
            return Err(GitError::Merging);
        }
        let parent = self.verify("HEAD^{commit}")?;
        let mut committed = Vec::new();
        let mut staged = Vec::new();
        for edit in edits {
            let path = self.relative(edit.path)?;
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
            if head.is_none() && index.is_none() && self.ignored(&path)? {
                return Err(GitError::Ignored(path));
            }
            let head_text = head.as_ref().map(|e| self.text(e, &path)).transpose()?;
            let index_text = index.as_ref().map(|e| self.text(e, &path)).transpose()?;
            let (commit, stage) = apply_edit(
                edit.before.unwrap_or_default(),
                edit.after,
                head_text.as_deref(),
                index_text.as_deref(),
            )
            .map_err(|line| GitError::Overlap {
                path: path.clone(),
                line,
            })?;
            let mode = head
                .or(index)
                .map_or_else(|| new_mode(edit.path), |e| e.mode);
            committed.push(Entry {
                mode: mode.clone(),
                blob: self.blob(&commit, &path)?,
                path: path.clone(),
            });
            if let Some(stage) = stage {
                staged.push(Entry {
                    mode,
                    blob: self.blob(&stage, &path)?,
                    path,
                });
            }
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
            staged,
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

    /// Moves `HEAD` back to the prepared commit's parent.
    pub fn retreat(&self, prepared: &Prepared) -> Result<(), GitError> {
        let commit = prepared.commit.as_str();
        let mut args = vec!["update-ref", "-m", "ned --commit (rolled back)"];
        match &prepared.parent {
            Some(parent) => args.extend(["HEAD", parent, commit]),
            None => args.extend(["-d", "HEAD", commit]),
        }
        self.git(&args, &[], None).map(drop)
    }

    /// Stages each edit in the index, as the commit has it relative to the
    /// staged text.
    pub fn stage(&self, prepared: &Prepared) -> Result<(), GitError> {
        if prepared.staged.is_empty() {
            return Ok(());
        }
        self.git(&cacheinfo(&prepared.staged), &[], None).map(drop)
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

    /// `path` relative to the working tree, with `/` separators.
    fn relative(&self, path: &Path) -> Result<String, GitError> {
        let outside = || GitError::Outside {
            path: path.display().to_string(),
            top: self.top.display().to_string(),
        };
        let path = canonical(path);
        let rel = path.strip_prefix(&self.top).map_err(|_| outside())?;
        let parts: Option<Vec<&str>> = rel
            .components()
            .map(|c| match c {
                Component::Normal(part) => part.to_str(),
                _ => None,
            })
            .collect();
        parts.map(|p| p.join("/")).ok_or_else(outside)
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
            self.git(&cacheinfo(entries), &env, None)?;
            self.git(&["write-tree"], &env, None)
        })();
        let _ = std::fs::remove_file(&index);
        Ok(tree?.trim_end().to_string())
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

/// `update-index` arguments adding `entries`.
fn cacheinfo(entries: &[Entry]) -> Vec<&str> {
    let mut args = vec!["update-index", "--add"];
    for e in entries {
        args.extend(["--cacheinfo", &e.mode, &e.blob, &e.path]);
    }
    args
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
    let io_failed = |err: std::io::Error| GitError::Failed {
        command: args[0].into(),
        message: err.to_string(),
    };
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
}
