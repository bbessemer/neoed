//! Committing an invocation's edits to git (command-language spec, §1.3),
//! through git's plumbing commands.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};

use thiserror::Error;

use crate::diff;
use crate::hint::{self, Fix, Hint};

use GitErrorKind as K;

/// A git repository's working tree, and the commit `HEAD` named when it was
/// found.
#[derive(Debug)]
pub struct Repo {
    pub top: PathBuf,
    head: Option<String>,
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

/// The empty blob's name, with SHA-1 and SHA-256, which an intent-to-add
/// entry holds.
const EMPTY_BLOBS: [&str; 2] = [
    "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391",
    "473a0f4c3be8a93681a267e3b1e9a7dcda1185436fe141f7749120a303721813",
];

/// An index entry to write: `blob` at `path` with `mode`, or with no blob,
/// the path removed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    mode: String,
    blob: Option<String>,
    path: String,
}

/// A commit that can't be made (spec §1.3).
pub type GitError = hint::Error<GitErrorKind>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum GitErrorKind {
    #[error("git isn't installed")]
    NoGit,
    #[error("{0} isn't in a git repository")]
    NotARepo(String),
    #[error("a merge is in progress")]
    Merging,
    /// `edit` is the index of the edit.
    #[error("{path} isn't in the repository at {top}")]
    Outside {
        path: String,
        top: String,
        edit: usize,
    },
    /// `edit` is the index of the file's first edit.
    #[error("{path} is ignored by git")]
    Ignored { path: String, edit: usize },
    #[error("the edit to {path} overlaps its uncommitted changes at line {line}")]
    Overlap { path: String, line: usize },
    #[error("{0} isn't UTF-8 text in HEAD or the index")]
    NotUtf8(String),
    #[error("nothing to commit: the edits leave every file as HEAD has it")]
    NothingToCommit,
    #[error("HEAD moved while ned ran")]
    HeadMoved,
    #[error("{0} exists, so another git process is using the index")]
    IndexLocked(String),
    #[error("git {command} failed: {message}")]
    Failed { command: String, message: String },
}

impl Hint for GitErrorKind {
    /// The exit code for a refused commit (spec §1.3).
    fn exit_code(&self) -> u8 {
        match self {
            K::NoGit | K::NotARepo(_) => 2,
            K::Failed { .. } | K::IndexLocked(_) => 3,
            _ => 1,
        }
    }

    fn fix(&self) -> Option<Fix> {
        let leave_out = "{cli:leave out --commit}{mcp:leave out `commit`}{repl:use `:write` in place of `:commit`}";
        let fix = match self {
            K::NoGit => format!("install it, or {leave_out}"),
            K::NotARepo(_) => format!("run git init, or {leave_out}"),
            K::Merging => format!("finish it with git commit, or {leave_out}"),
            K::Outside { .. } => format!("commit it in a ned call of its own, or {leave_out}"),
            K::Ignored { .. } => "edit it without {--commit}, or stop ignoring it".into(),
            K::Overlap { .. } => format!("commit or stash them first, or {leave_out}"),
            K::NotUtf8(_) => format!("commit it with git instead, or {leave_out}"),
            K::HeadMoved => "nothing was written, so rerun the script".into(),
            K::IndexLocked(lock) => {
                format!(
                    "rerun once it's done, or remove {} if none is running",
                    hint::verbatim(lock)
                )
            }
            K::NothingToCommit => {
                "edit a file{cli:, or leave out --commit}{mcp:, or leave out `commit`}{repl:, then `:commit MSG`}".into()
            }
            K::Failed { .. } => return None,
        };
        Some(fix.into())
    }
}

impl Repo {
    /// The repository whose working tree holds `path`, a file or directory that
    /// may not exist yet.
    pub fn discover(path: &Path) -> Result<Repo, GitError> {
        let dir = existing_dir(path);
        let top = toplevel(dir)?.ok_or_else(|| K::NotARepo(dir.display().to_string()))?;
        let mut repo = Repo { top, head: None };
        repo.head = repo.verify("HEAD^{commit}")?;
        Ok(repo)
    }

    /// Makes the commit of `edits` on `HEAD`, with `message`, without moving
    /// `HEAD`, if `HEAD` hasn't moved since the repository was found. Several
    /// edits to one file apply one after another.
    pub fn prepare(&self, edits: &[FileEdit], message: &str) -> Result<Prepared, GitError> {
        if edits.is_empty() {
            return Err(K::NothingToCommit.into());
        }
        if self.verify("MERGE_HEAD")?.is_some() {
            return Err(K::Merging.into());
        }
        let mut index = self.lock_index()?;
        let parent = self.verify("HEAD^{commit}")?;
        if parent != self.head {
            return Err(K::HeadMoved.into());
        }
        let mut committed = Vec::new();
        let mut staged = Vec::new();
        // Each file's edits, in order, by its path in the repository, with the
        // index of its first edit.
        let mut files: Vec<(String, usize, Vec<&FileEdit>)> = Vec::new();
        for (i, edit) in edits.iter().enumerate() {
            let path = self.relative(edit.path)?.ok_or_else(|| K::Outside {
                path: edit.path.display().to_string(),
                top: self.top.display().to_string(),
                edit: i,
            })?;
            match files.iter_mut().find(|(p, _, _)| *p == path) {
                Some((_, _, steps)) => steps.push(edit),
                None => files.push((path, i, vec![edit])),
            }
        }
        let mut heads = match parent {
            Some(ref parent) => listing(
                &self.git(&["ls-tree", "-r", "-z", parent], &[], None)?,
                true,
            ),
            None => HashMap::new(),
        };
        let mut indexed = listing(&self.git(&["ls-files", "-s", "-z"], &[], None)?, false);
        // An intent-to-add entry holds the empty blob; only diff-files tells it
        // apart, as a file added in the working tree.
        let intent = files.iter().any(|(path, _, _)| {
            indexed
                .get(path)
                .is_some_and(|e| EMPTY_BLOBS.contains(&e.blob.as_str()))
        });
        if intent {
            for path in self.intents()? {
                indexed.remove(&path);
            }
        }
        let entries = |path| [heads.get(path), indexed.get(path)];
        if files
            .iter()
            .any(|(path, _, _)| entries(path)[1].is_some_and(|e| e.stage.as_deref() != Some("0")))
        {
            return Err(K::Merging.into());
        }
        let wanted: Vec<(&Listed, &str)> = files
            .iter()
            .flat_map(|(path, _, _)| {
                entries(path)
                    .into_iter()
                    .flatten()
                    .map(|e| (e, path.as_str()))
            })
            .collect();
        let mut texts = self.texts(&wanted)?.into_iter();
        let new: Vec<&str> = files
            .iter()
            .filter(|(path, _, _)| entries(path).iter().all(Option::is_none))
            .map(|(path, _, _)| path.as_str())
            .collect();
        let ignored = match new.is_empty() {
            true => HashSet::new(),
            false => self.ignored(&new)?,
        };
        for (path, first, steps) in files {
            let head = heads.remove(&path);
            let index = indexed.remove(&path);
            // An untracked file whose edits undo each other stays untracked.
            let last = steps.last().expect("a file has edits");
            if head.is_none()
                && index.is_none()
                && steps[0].before.is_some()
                && steps[0].before == last.after
            {
                continue;
            }
            let mut text = |entry: &Option<Listed>| {
                entry
                    .as_ref()
                    .map(|_| {
                        let text = texts.next().expect("a text for each entry");
                        text.ok_or_else(|| K::NotUtf8(path.clone()))
                    })
                    .transpose()
            };
            let mut commit = text(&head)?;
            let mut index_text = text(&index)?;
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
                        |line| K::Overlap {
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
            if commit.is_some() && head.is_none() && index.is_none() && ignored.contains(&path) {
                return Err(K::Ignored { path, edit: first }.into());
            }
            let mode = head
                .as_ref()
                .or(index.as_ref())
                .map_or_else(|| new_mode(last.path), |e| e.mode.clone());
            let blob = commit
                .as_ref()
                .map(|text| self.blob(text, &path))
                .transpose()?;
            if commit.is_some() || head.is_some() {
                committed.push(Entry {
                    mode: mode.clone(),
                    blob: blob.clone(),
                    path: path.clone(),
                });
            }
            if let Some(stage) = stage {
                let blob = match stage {
                    Some(text) if Some(&text) == commit.as_ref() => blob,
                    stage => stage.map(|text| self.blob(&text, &path)).transpose()?,
                };
                staged.push(Entry {
                    mode: index.map_or(mode, |e| e.mode),
                    blob,
                    path,
                });
            }
        }
        if !staged.is_empty() {
            self.write_next(&mut index, &staged)?;
        }
        let tree = self.tree(parent.as_deref(), &committed)?;
        if parent.is_some() && self.verify("HEAD^{tree}")?.as_deref() == Some(tree.as_str()) {
            return Err(K::NothingToCommit.into());
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
            return Err(K::HeadMoved.into());
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
        String::from_utf8(out.stdout).map_err(|_| {
            GitError::new(K::Failed {
                command: args[0].into(),
                message: "its output isn't UTF-8".into(),
            })
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
        let path = crate::fs::canonical(path);
        let Ok(rel) = path.strip_prefix(&self.top) else {
            return Ok(None);
        };
        let nested = existing_dir(&path)
            .ancestors()
            .take_while(|dir| *dir != self.top)
            .any(|dir| dir.join(".git").exists());
        if nested {
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

    /// The paths that the index only marks an intent to add (`git add -N`).
    fn intents(&self) -> Result<HashSet<String>, GitError> {
        let args = ["diff-files", "--diff-filter=A", "--name-only", "-z"];
        Ok(self
            .git(&args, &[], None)?
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(String::from)
            .collect())
    }

    /// Which of `paths` git ignores.
    fn ignored(&self, paths: &[&str]) -> Result<HashSet<String>, GitError> {
        // check-ignore takes paths, not pathspecs, and refuses literal ones.
        let args = ["check-ignore", "--stdin", "-z"];
        let input: String = paths.iter().map(|p| format!("{p}\0")).collect();
        let env = [("GIT_LITERAL_PATHSPECS", Path::new("0"))];
        let out = run(&self.top, &args, &env, Some(&input))?;
        match out.status.code() {
            Some(0 | 1) => Ok(String::from_utf8_lossy(&out.stdout)
                .split('\0')
                .filter(|p| !p.is_empty())
                .map(String::from)
                .collect()),
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

    /// The text of `entry`'s blob, as the working tree would have it; `None` if
    /// it isn't UTF-8.
    fn text(&self, entry: &Listed, path: &str) -> Result<Option<String>, GitError> {
        let flag = format!("--path={path}");
        let args = ["cat-file", "--filters", &flag, &entry.blob];
        let out = run(&self.top, &args, &[], None)?;
        if !out.status.success() {
            return Err(failed(&args, &out));
        }
        Ok(String::from_utf8(out.stdout).ok())
    }

    /// Which of `paths` checking out may convert, so that their blobs' text
    /// differs from the working tree's.
    fn filtered(&self, paths: &[&str]) -> Result<HashSet<String>, GitError> {
        let args = ["config", "--get-regexp", r"^core\.(autocrlf|eol)$"];
        let out = run(&self.top, &args, &[], None)?;
        if !matches!(out.status.code(), Some(0 | 1)) {
            return Err(failed(&args, &out));
        }
        let crlf = String::from_utf8_lossy(&out.stdout).lines().any(|line| {
            let line = line.to_ascii_lowercase();
            line == "core.autocrlf true" || line == "core.eol crlf"
        });
        let args = [
            "check-attr",
            "--stdin",
            "-z",
            "text",
            "crlf",
            "eol",
            "filter",
            "ident",
            "working-tree-encoding",
        ];
        let input: String = paths.iter().map(|p| format!("{p}\0")).collect();
        let out = run(&self.top, &args, &[], Some(&input))?;
        if !out.status.success() {
            return Err(failed(&args, &out));
        }
        let out = String::from_utf8_lossy(&out.stdout);
        let fields: Vec<&str> = out.split('\0').collect();
        let mut filtered = HashSet::new();
        let mut text = HashSet::new();
        for &[path, attribute, value] in fields.as_chunks::<3>().0 {
            let set = !matches!(value, "unspecified" | "unset");
            let converts = match attribute {
                "text" | "crlf" => {
                    if value != "unset" {
                        text.insert(path);
                    }
                    false
                }
                "eol" => value == "crlf",
                _ => set,
            };
            if converts {
                filtered.insert(path.to_string());
            }
        }
        // core.autocrlf and core.eol convert a file unless it is marked -text.
        if crlf {
            filtered.extend(text.into_iter().map(String::from));
        }
        Ok(filtered)
    }

    /// The text of each of `entries`' blobs at its path, as the working tree
    /// would have it, read in one batch; `None` for one that isn't UTF-8.
    fn texts(&self, entries: &[(&Listed, &str)]) -> Result<Vec<Option<String>>, GitError> {
        // A batch line can't hold a path with a line break, and a converted blob's
        // header gives its size before conversion; those take a call each.
        let paths: Vec<&str> = entries.iter().map(|(_, path)| *path).collect();
        let filtered = match paths.is_empty() {
            true => HashSet::new(),
            false => self.filtered(&paths)?,
        };
        let batched = |path: &str| !path.contains('\n') && !filtered.contains(path);
        let input: String = entries
            .iter()
            .filter(|(_, path)| batched(path))
            .map(|(entry, _)| format!("{}\n", entry.blob))
            .collect();
        let args = ["cat-file", "--batch"];
        let mut rest: &[u8] = &[];
        let out;
        if !input.is_empty() {
            out = run(&self.top, &args, &[], Some(&input))?;
            if !out.status.success() {
                return Err(failed(&args, &out));
            }
            rest = &out.stdout;
        }
        let malformed = |message: String| {
            GitError::new(K::Failed {
                command: args[0].into(),
                message,
            })
        };
        entries
            .iter()
            .map(|(entry, path)| {
                if !batched(path) {
                    return self.text(entry, path);
                }
                // OBJECT TYPE SIZE, or OBJECT missing
                let end = rest.iter().position(|&b| b == b'\n');
                let header = String::from_utf8_lossy(&rest[..end.unwrap_or(rest.len())]);
                let size = header
                    .rsplit(' ')
                    .next()
                    .and_then(|s| s.parse::<usize>().ok());
                let (Some(end), Some(size)) = (end, size) else {
                    return Err(malformed(header.into_owned()));
                };
                let body = rest
                    .get(end + 1..end + 1 + size)
                    .ok_or_else(|| malformed("its output ended early".into()))?;
                rest = rest.get(end + 2 + size..).unwrap_or_default();
                Ok(String::from_utf8(body.to_vec()).ok())
            })
            .collect()
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
                Err(K::IndexLocked(lock.display().to_string()).into())
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
        let mut add = String::new();
        let mut remove = String::new();
        for e in entries {
            match &e.blob {
                Some(blob) => add.push_str(&format!("{} {blob}\t{}\0", e.mode, e.path)),
                None => remove.push_str(&format!("{}\0", e.path)),
            }
        }
        let add_command = ["update-index", "--add", "-z", "--index-info"];
        let remove_command = ["update-index", "--force-remove", "-z", "--stdin"];
        for (command, input) in [(add_command, add), (remove_command, remove)] {
            if !input.is_empty() {
                self.git(&command, env, Some(&input))?;
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

/// The entries that `ls-tree -r -z` (`tree`) or `ls-files -s -z` lists in
/// `out`, by path.
fn listing(out: &str, tree: bool) -> HashMap<String, Listed> {
    out.split('\0')
        .filter_map(|line| {
            let (fields, path) = line.split_once('\t')?;
            let fields: Vec<&str> = fields.split(' ').collect();
            let listed = match tree {
                // MODE TYPE BLOB
                true => Listed {
                    mode: fields.first()?.to_string(),
                    blob: fields.get(2)?.to_string(),
                    stage: None,
                },
                // MODE BLOB STAGE; an unmerged path's stages are all non-0.
                false => Listed {
                    mode: fields.first()?.to_string(),
                    blob: fields.get(1)?.to_string(),
                    stage: Some(fields.get(2)?.to_string()),
                },
            };
            Some((path.to_string(), listed))
        })
        .collect()
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

/// `path` with `suffix` added to its name.
fn beside(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn io_failed(command: &str, err: std::io::Error) -> GitError {
    GitError::new(K::Failed {
        command: command.into(),
        message: err.to_string(),
    })
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
        std::io::ErrorKind::NotFound => K::NoGit.into(),
        _ => io_failed(err),
    })?;
    // Write the input while git's output is read, so a batch can't fill both
    // pipes and deadlock.
    let stdin = child.stdin.take();
    std::thread::scope(|scope| {
        let writer = scope.spawn(move || match (input, stdin) {
            (Some(input), Some(mut stdin)) => stdin.write_all(input.as_bytes()),
            _ => Ok(()),
        });
        let out = child.wait_with_output().map_err(io_failed)?;
        match writer.join().expect("writing git's input doesn't panic") {
            // git failing before it read all its input is git's error to report.
            Err(err) if out.status.success() => Err(io_failed(err)),
            _ => Ok(out),
        }
    })
}

fn failed(args: &[&str], out: &Output) -> GitError {
    GitError::new(K::Failed {
        command: args[0].into(),
        message: String::from_utf8_lossy(&out.stderr).trim_end().to_string(),
    })
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
    fn head_moving_after_discovery_is_refused() {
        let (dir, repo) = repo();
        let tree = ["commit-tree", "HEAD^{tree}", "-p", "HEAD", "-m", "moved"];
        let moved = repo.git(&tree, &[], None).unwrap().trim_end().to_string();
        repo.git(&["update-ref", "HEAD", &moved], &[], None)
            .unwrap();
        let edit = FileEdit {
            path: &dir.path().join("f.txt"),
            before: Some(HEAD),
            after: Some("A\nb\nc\n"),
        };
        assert_eq!(
            repo.prepare(&[edit], "m").map(|_| ()),
            Err(K::HeadMoved.into())
        );
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
        assert_eq!(repo.advance(&prepared), Err(K::HeadMoved.into()));
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

    #[test]
    fn listing_reads_trees_and_the_index_by_path() {
        let tree = "100644 blob aaa\ta b/c\td.txt\x00160000 commit bbb\tsub\x00";
        let listed = listing(tree, true);
        assert_eq!(listed.len(), 2);
        let file = &listed["a b/c\td.txt"];
        assert_eq!((file.mode.as_str(), file.blob.as_str()), ("100644", "aaa"));
        assert_eq!(file.stage, None);
        assert_eq!(listed["sub"].mode, "160000");
        let index = "100755 ccc 0\tx.sh\x00100644 ddd 1\tm.txt\x00100644 eee 2\tm.txt\x00100644 fff 3\tm.txt\x00";
        let listed = listing(index, false);
        assert_eq!(listed.len(), 2);
        let script = &listed["x.sh"];
        assert_eq!(
            (script.mode.as_str(), script.blob.as_str()),
            ("100755", "ccc")
        );
        assert_eq!(script.stage.as_deref(), Some("0"));
        assert_ne!(listed["m.txt"].stage.as_deref(), Some("0"));
        assert!(listing("", false).is_empty());
    }

    #[test]
    fn texts_reads_every_blob_in_order() {
        let (dir, repo) = repo();
        let blob = |bytes: &[u8]| {
            let file = dir.path().join("blob");
            std::fs::write(&file, bytes).unwrap();
            let args = ["hash-object", "-w", file.to_str().unwrap()];
            let blob = repo.git(&args, &[], None).unwrap().trim_end().to_string();
            Listed {
                mode: "100644".into(),
                blob,
                stage: None,
            }
        };
        let lines = blob(b"x\ny\n");
        let empty = blob(b"");
        let binary = blob(b"\xff\xfe\n");
        let entries = [
            (&lines, "a b.txt"),
            (&empty, "e.txt"),
            (&binary, "bin"),
            (&lines, "new\nline.txt"),
            (&empty, "last.txt"),
        ];
        assert_eq!(
            repo.texts(&entries).unwrap(),
            [
                Some("x\ny\n".to_string()),
                Some(String::new()),
                None,
                Some("x\ny\n".to_string()),
                Some(String::new()),
            ]
        );
        assert_eq!(repo.texts(&[]).unwrap(), Vec::<Option<String>>::new());
    }

    #[test]
    fn texts_reads_converted_blobs_whole() {
        let (dir, repo) = repo();
        std::fs::write(dir.path().join(".gitattributes"), "*.crlf eol=crlf\n").unwrap();
        let args = ["hash-object", "-w", "f.txt"];
        let blob = repo.git(&args, &[], None).unwrap().trim_end().to_string();
        let lines = Listed {
            mode: "100644".into(),
            blob,
            stage: None,
        };
        let entries = [(&lines, "a.crlf"), (&lines, "b.txt"), (&lines, "c.crlf")];
        let crlf = HEAD.replace('\n', "\r\n");
        assert_eq!(
            repo.texts(&entries).unwrap(),
            [Some(crlf.clone()), Some(HEAD.to_string()), Some(crlf)]
        );
    }

    #[test]
    fn ignored_names_the_ignored_paths() {
        let (dir, repo) = repo();
        std::fs::write(dir.path().join(".gitignore"), "*.log\nbuild/\n").unwrap();
        let ignored = repo
            .ignored(&["a.log", "b.txt", "build/x", "c d.log"])
            .unwrap();
        let expected = ["a.log", "build/x", "c d.log"].map(String::from);
        assert_eq!(ignored, HashSet::from(expected));
        assert!(repo.ignored(&["b.txt"]).unwrap().is_empty());
    }

    #[test]
    fn intents_names_the_files_added_with_intent() {
        let (dir, repo) = repo();
        std::fs::write(dir.path().join("n w.txt"), "x\n").unwrap();
        std::fs::write(dir.path().join("staged.txt"), "x\n").unwrap();
        repo.git(&["add", "-N", "n w.txt"], &[], None).unwrap();
        repo.git(&["add", "staged.txt"], &[], None).unwrap();
        assert_eq!(
            repo.intents().unwrap(),
            HashSet::from(["n w.txt".to_string()])
        );
    }

    #[test]
    fn a_path_in_a_nested_repository_or_submodule_is_outside() {
        let (dir, repo) = repo();
        for (nested, git) in [("nested", true), ("sub", false)] {
            let inner = dir.path().join(nested).join("deep");
            std::fs::create_dir_all(&inner).unwrap();
            let marker = dir.path().join(nested).join(".git");
            match git {
                true => std::fs::create_dir(&marker).unwrap(),
                false => std::fs::write(&marker, "gitdir: ../.git/modules/sub\n").unwrap(),
            }
            assert_eq!(repo.relative(&inner.join("f.txt")).unwrap(), None);
        }
        let path = dir.path().join("plain dir/new.txt");
        assert_eq!(
            repo.relative(&path).unwrap().as_deref(),
            Some("plain dir/new.txt")
        );
    }

    #[test]
    fn prepare_commits_and_stages_every_files_edit() {
        let (dir, repo) = repo();
        let path = |p: &str| dir.path().join(p);
        std::fs::create_dir_all(path("d 1")).unwrap();
        std::fs::write(path("d 1/ü.txt"), HEAD).unwrap();
        std::fs::write(path("gone.txt"), "x\n").unwrap();
        repo.git(&["add", "."], &[], None).unwrap();
        repo.git(&["commit", "-qm", "more"], &[], None).unwrap();
        let repo = Repo::discover(dir.path()).unwrap();
        let staged = "a\nb\nC\n";
        std::fs::write(path("f.txt"), staged).unwrap();
        repo.git(&["add", "f.txt"], &[], None).unwrap();
        let (f, u, gone, new) = (
            path("f.txt"),
            path("d 1/ü.txt"),
            path("gone.txt"),
            path("new dir/n w.txt"),
        );
        let edits = [
            FileEdit {
                path: &u,
                before: Some(HEAD),
                after: Some("A\nb\nc\n"),
            },
            FileEdit {
                path: &gone,
                before: Some("x\n"),
                after: None,
            },
            FileEdit {
                path: &new,
                before: None,
                after: Some("n\n"),
            },
            FileEdit {
                path: &f,
                before: Some(staged),
                after: Some("A\nb\nC\n"),
            },
        ];
        let mut prepared = repo.prepare(&edits, "m").unwrap();
        repo.advance(&prepared).unwrap();
        repo.stage(&mut prepared).unwrap();
        let show = |rev: &str| repo.git(&["show", rev], &[], None).ok();
        assert_eq!(show("HEAD:d 1/ü.txt").as_deref(), Some("A\nb\nc\n"));
        assert_eq!(show(":d 1/ü.txt").as_deref(), Some("A\nb\nc\n"));
        assert_eq!(show("HEAD:new dir/n w.txt").as_deref(), Some("n\n"));
        assert_eq!(show(":new dir/n w.txt").as_deref(), Some("n\n"));
        assert_eq!(show("HEAD:f.txt").as_deref(), Some("A\nb\nc\n"));
        assert_eq!(show(":f.txt").as_deref(), Some("A\nb\nC\n"));
        assert_eq!(show("HEAD:gone.txt"), None);
        assert_eq!(show(":gone.txt"), None);
    }

    #[test]
    fn prepare_refuses_a_new_ignored_file() {
        let (dir, repo) = repo();
        std::fs::write(dir.path().join(".gitignore"), "*.log\n").unwrap();
        let (ok, log) = (dir.path().join("ok.txt"), dir.path().join("x.log"));
        let edits = [&ok, &log].map(|path| FileEdit {
            path,
            before: None,
            after: Some("x\n"),
        });
        assert_eq!(
            repo.prepare(&edits, "m").unwrap_err().kind,
            K::Ignored {
                path: "x.log".into(),
                edit: 1
            }
        );
    }

    #[test]
    fn a_refused_commit_says_how_to_go_without_one_in_each_frontends_terms() {
        use crate::hint::Frontend;

        let error = GitError::new(K::Merging);
        let rendered =
            [Frontend::Cli, Frontend::Mcp, Frontend::Repl].map(|f| error.render(f, None));
        assert_eq!(
            rendered,
            [
                "error: a merge is in progress; finish it with git commit, or leave out --commit",
                "error: a merge is in progress; finish it with git commit, or leave out `commit`",
                "error: a merge is in progress; finish it with git commit, or use `:write` in place of `:commit`",
            ]
        );
    }

    #[test]
    fn nothing_to_commit_says_what_to_do_in_each_frontends_terms() {
        use crate::hint::Frontend;

        let error = GitError::new(K::NothingToCommit);
        let rendered =
            [Frontend::Cli, Frontend::Mcp, Frontend::Repl].map(|f| error.render(f, None));
        assert_eq!(
            rendered,
            [
                "error: nothing to commit: the edits leave every file as HEAD has it; edit a file, or leave out --commit",
                "error: nothing to commit: the edits leave every file as HEAD has it; edit a file, or leave out `commit`",
                "error: nothing to commit: the edits leave every file as HEAD has it; edit a file, then `:commit MSG`",
            ]
        );
    }

    #[test]
    fn a_path_in_a_fix_prints_as_it_is() {
        let error = GitError::new(K::IndexLocked("/tmp/{-w}/index.lock".into()));
        assert_eq!(
            error.render(crate::hint::Frontend::Mcp, None),
            "error: /tmp/{-w}/index.lock exists, so another git process is using the index; rerun once it's done, or remove /tmp/{-w}/index.lock if none is running"
        );
    }
}
