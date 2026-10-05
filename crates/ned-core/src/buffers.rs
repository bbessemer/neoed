//! The REPL's buffers: edits kept in memory until written, and undone script
//! by script (command-language spec §1.4).

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::diff;
use crate::exec::{Change, Overlay};
use crate::session::FileChange;

/// Files with unwritten edits, by absolute path, and the edits' history.
#[derive(Debug, Default)]
pub struct Buffers {
    /// Each buffer's text.
    texts: Overlay,
    /// Each buffer's base: the file's text when its first unwritten edit was
    /// made, `None` for a file made by `create` and not yet written.
    bases: BTreeMap<PathBuf, Option<String>>,
    /// Per script that edited files, oldest first, the files it edited.
    history: Vec<Vec<Edited>>,
}

/// A file a script edited: its text before (`None` if the script made it)
/// and after.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Edited {
    path: PathBuf,
    before: Option<String>,
    after: String,
}

#[derive(Debug, Error)]
pub enum BuffersError {
    #[error("nothing to undo; `:history` lists the session's entries, which `ned undo` reverts")]
    Nothing,
    #[error("{} has no unwritten edits; `:files` lists the buffers that have", path.display())]
    NotBuffered { path: PathBuf },
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error(
        "{}:{line}: the edit overlaps a change made to the file since; `:diff` shows the edits, and `:write!` writes them over it",
        path.display()
    )]
    Overlap { path: PathBuf, line: usize },
    #[error(
        "{}:{line}: undoing the edit conflicts with a later change; edit the text by hand",
        path.display()
    )]
    UndoOverlap { path: PathBuf, line: usize },
    #[error("{} exists now; `:write!` writes over it", path.display())]
    Exists { path: PathBuf },
    #[error("{} was removed since its buffer was read; `:write!` writes it again", path.display())]
    Removed { path: PathBuf },
    #[error(
        "{} was written since the script created it, so `:undo` can't remove it; remove it by hand, or with `ned undo`",
        path.display()
    )]
    Written { path: PathBuf },
    #[error(
        "{} and {} are the same file; `:reload` one of them to drop its edits, then `:write` the other",
        path.display(),
        other.display()
    )]
    SameFile { path: PathBuf, other: PathBuf },
}

impl Buffers {
    /// The buffers' texts, for scripts to read in place of the files.
    pub fn overlay(&self) -> &Overlay {
        &self.texts
    }

    /// The files with unwritten edits, in path order.
    pub fn unwritten(&self) -> impl Iterator<Item = &Path> {
        self.texts.keys().map(PathBuf::as_path)
    }

    /// The base of the buffer for `path`: `Some(None)` for a file not yet
    /// written; `None` without a buffer.
    pub fn base(&self, path: &Path) -> Option<Option<&str>> {
        self.bases.get(path).map(Option::as_deref)
    }

    /// Applies a script's `changes`, whose paths are relative to `cwd`, with
    /// `finals`, the texts they leave (formatted), as one step to undo.
    pub fn apply(&mut self, cwd: &Path, changes: &[Change], finals: &[&str]) {
        let mut step = Vec::new();
        for (change, after) in changes.iter().zip(finals) {
            let path = crate::fs::canonical(&cwd.join(&change.path));
            let before = (!change.created).then(|| change.old.clone());
            self.bases
                .entry(path.clone())
                .or_insert_with(|| before.clone());
            self.set(&path, after.to_string());
            step.push(Edited {
                path,
                before,
                after: after.to_string(),
            });
        }
        if !step.is_empty() {
            self.history.push(step);
        }
    }

    /// Undoes the last step's edits (spec §1.4), merging into a text changed
    /// since. `read` gives a file's text on disk, `None` if it's missing.
    pub fn undo(
        &mut self,
        mut read: impl FnMut(&Path) -> io::Result<Option<String>>,
    ) -> Result<Vec<FileChange>, BuffersError> {
        let step = self.history.last().ok_or(BuffersError::Nothing)?;
        // Every file is planned before any changes, so an error changes nothing.
        let mut plans = Vec::new();
        for edit in step {
            let path = edit.path.clone();
            let disk = read(&path).map_err(|source| BuffersError::Io {
                path: path.clone(),
                source,
            })?;
            let current = self.texts.get(&path).cloned().or_else(|| disk.clone());
            let restored = if current.as_deref() == Some(edit.after.as_str()) {
                edit.before.clone()
            } else {
                let merged = diff::merge(
                    &edit.after,
                    current.as_deref().unwrap_or_default(),
                    edit.before.as_deref().unwrap_or_default(),
                )
                .map_err(|line| BuffersError::UndoOverlap {
                    path: path.clone(),
                    line,
                })?;
                (edit.before.is_some() || !merged.is_empty()).then_some(merged)
            };
            if restored.is_none() && disk.is_some() {
                return Err(BuffersError::Written { path });
            }
            plans.push((path, disk, current, restored));
        }
        self.history.pop();
        let mut changes = Vec::new();
        for (path, disk, current, restored) in plans {
            match &restored {
                Some(text) => {
                    self.bases.entry(path.clone()).or_insert(disk);
                    self.set(&path, text.clone());
                }
                None => {
                    self.texts.remove(&path);
                    self.bases.remove(&path);
                }
            }
            if restored != current {
                changes.push(FileChange {
                    path,
                    before: current,
                    after: restored,
                });
            }
        }
        Ok(changes)
    }

    /// Drops the unwritten edits of `paths`, or of every buffer.
    pub fn reload(&mut self, paths: Option<&[PathBuf]>) -> Result<(), BuffersError> {
        let Some(paths) = paths else {
            self.texts.clear();
            self.bases.clear();
            return Ok(());
        };
        self.check_buffered(paths)?;
        for path in paths {
            self.texts.remove(path);
            self.bases.remove(path);
        }
        Ok(())
    }

    /// Plans writing the buffers for `paths`, or every buffer: each file's
    /// text on disk and the text to write, merged into a file changed since its
    /// base unless `force`. Nothing changes until [`Buffers::written`].
    pub fn plan_write(
        &self,
        paths: Option<&[PathBuf]>,
        mut read: impl FnMut(&Path) -> io::Result<Option<String>>,
        force: bool,
    ) -> Result<Vec<FileChange>, BuffersError> {
        let paths: Vec<&PathBuf> = match paths {
            Some(paths) => {
                self.check_buffered(paths)?;
                paths.iter().collect()
            }
            None => self.texts.keys().collect(),
        };
        let mut canonical = BTreeMap::new();
        for path in &paths {
            if let Some(other) = canonical.insert(crate::fs::canonical(path), *path) {
                return Err(BuffersError::SameFile {
                    path: other.clone(),
                    other: (*path).clone(),
                });
            }
        }
        let mut writes = Vec::new();
        for path in paths {
            let text = &self.texts[path];
            let disk = read(path).map_err(|source| BuffersError::Io {
                path: path.clone(),
                source,
            })?;
            let after = match (&self.bases[path], &disk) {
                _ if force || disk == self.bases[path] => text.clone(),
                (None, _) => return Err(BuffersError::Exists { path: path.clone() }),
                (Some(_), None) => return Err(BuffersError::Removed { path: path.clone() }),
                (Some(base), Some(disk)) => {
                    diff::merge(base, text, disk).map_err(|line| BuffersError::Overlap {
                        path: path.clone(),
                        line,
                    })?
                }
            };
            writes.push(FileChange {
                path: path.clone(),
                before: disk,
                after: Some(after),
            });
        }
        Ok(writes)
    }

    /// Drops the buffers that `writes`, from [`Buffers::plan_write`], wrote.
    pub fn written(&mut self, writes: &[FileChange]) {
        for write in writes {
            self.texts.remove(&write.path);
            self.bases.remove(&write.path);
        }
    }

    /// Merges a change another program wrote to the file at `path`, whose text
    /// is now `disk`, into the buffer's unwritten edits, as a write would merge
    /// it, and makes `disk` the buffer's base (spec §1.4). `false` without a
    /// buffer; an overlap leaves the buffer as it was.
    pub fn rebase(&mut self, path: &Path, disk: &str) -> Result<bool, BuffersError> {
        let Some(text) = self.texts.get(path) else {
            return Ok(false);
        };
        let merged = match &self.bases[path] {
            None => {
                return Err(BuffersError::Exists {
                    path: path.to_path_buf(),
                });
            }
            Some(base) => diff::merge(base, text, disk).map_err(|line| BuffersError::Overlap {
                path: path.to_path_buf(),
                line,
            })?,
        };
        self.bases
            .insert(path.to_path_buf(), Some(disk.to_string()));
        self.set(path, merged);
        Ok(true)
    }

    /// Sets the buffer for `path`, which has a base, to `text`, or drops it if
    /// that's its base: nothing is left to write.
    fn set(&mut self, path: &Path, text: String) {
        if self.bases[path].as_deref() == Some(text.as_str()) {
            self.texts.remove(path);
            self.bases.remove(path);
        } else {
            self.texts.insert(path.to_path_buf(), text);
        }
    }

    /// An error unless every one of `paths` has a buffer.
    fn check_buffered(&self, paths: &[PathBuf]) -> Result<(), BuffersError> {
        match paths.iter().find(|p| !self.texts.contains_key(*p)) {
            Some(path) => Err(BuffersError::NotBuffered { path: path.clone() }),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    const CWD: &str = "/w";

    fn path(name: &str) -> PathBuf {
        Path::new(CWD).join(name)
    }

    fn change(name: &str, old: &str, new: &str) -> Change {
        Change {
            path: name.into(),
            old: old.into(),
            new: new.into(),
            edits: 1,
            lang: None,
            created: false,
        }
    }

    fn created(name: &str, new: &str) -> Change {
        Change {
            created: true,
            ..change(name, "", new)
        }
    }

    /// Applies `changes` as one script, writing what each leaves.
    fn script(buffers: &mut Buffers, changes: &[Change]) {
        let finals: Vec<&str> = changes.iter().map(|c| c.new.as_str()).collect();
        buffers.apply(Path::new(CWD), changes, &finals);
    }

    /// Reads files from `disk`, by name.
    fn disk<'a>(disk: &'a [(&str, &str)]) -> impl FnMut(&Path) -> io::Result<Option<String>> + 'a {
        let files: HashMap<PathBuf, String> =
            disk.iter().map(|(n, t)| (path(n), t.to_string())).collect();
        move |p| Ok(files.get(p).cloned())
    }

    fn text<'a>(buffers: &'a Buffers, name: &str) -> Option<&'a str> {
        buffers.overlay().get(&path(name)).map(String::as_str)
    }

    fn file_change(name: &str, before: Option<&str>, after: Option<&str>) -> FileChange {
        FileChange {
            path: path(name),
            before: before.map(String::from),
            after: after.map(String::from),
        }
    }

    #[test]
    fn an_edit_makes_a_buffer_based_on_the_file() {
        let mut buffers = Buffers::default();
        script(&mut buffers, &[change("a.rs", "a\n", "b\n")]);
        assert_eq!(text(&buffers, "a.rs"), Some("b\n"));
        assert_eq!(buffers.unwritten().collect::<Vec<_>>(), [path("a.rs")]);
        assert_eq!(buffers.base(&path("a.rs")), Some(Some("a\n")));
        assert_eq!(buffers.base(&path("b.rs")), None);
    }

    #[test]
    fn a_buffer_holds_the_formatted_text() {
        let mut buffers = Buffers::default();
        let changes = [change("a.rs", "a\n", "b  \n")];
        buffers.apply(Path::new(CWD), &changes, &["b\n"]);
        assert_eq!(text(&buffers, "a.rs"), Some("b\n"));
    }

    #[test]
    fn later_edits_keep_the_first_base() {
        let mut buffers = Buffers::default();
        script(&mut buffers, &[change("a.rs", "a\n", "b\n")]);
        script(&mut buffers, &[change("a.rs", "b\n", "c\n")]);
        assert_eq!(text(&buffers, "a.rs"), Some("c\n"));
        assert_eq!(buffers.base(&path("a.rs")), Some(Some("a\n")));
    }

    #[test]
    fn an_edit_back_to_the_base_leaves_nothing_unwritten() {
        let mut buffers = Buffers::default();
        script(&mut buffers, &[change("a.rs", "a\n", "b\n")]);
        script(&mut buffers, &[change("a.rs", "b\n", "a\n")]);
        assert_eq!(buffers.unwritten().count(), 0);
    }

    #[test]
    fn a_created_file_has_no_base() {
        let mut buffers = Buffers::default();
        script(&mut buffers, &[created("n.rs", "n\n")]);
        assert_eq!(text(&buffers, "n.rs"), Some("n\n"));
        assert_eq!(buffers.base(&path("n.rs")), Some(None));
    }

    #[test]
    fn undo_walks_back_script_by_script() {
        let mut buffers = Buffers::default();
        let files = [("a.rs", "a\n"), ("b.rs", "x\n")];
        script(&mut buffers, &[change("a.rs", "a\n", "b\n")]);
        script(
            &mut buffers,
            &[change("a.rs", "b\n", "c\n"), change("b.rs", "x\n", "y\n")],
        );
        assert_eq!(
            buffers.undo(disk(&files)).unwrap(),
            [
                file_change("a.rs", Some("c\n"), Some("b\n")),
                file_change("b.rs", Some("y\n"), Some("x\n")),
            ]
        );
        assert_eq!(text(&buffers, "a.rs"), Some("b\n"));
        assert_eq!(text(&buffers, "b.rs"), None);
        assert_eq!(
            buffers.undo(disk(&files)).unwrap(),
            [file_change("a.rs", Some("b\n"), Some("a\n"))]
        );
        assert_eq!(buffers.unwritten().count(), 0);
        assert!(matches!(
            buffers.undo(disk(&files)),
            Err(BuffersError::Nothing)
        ));
    }

    #[test]
    fn undo_of_an_unwritten_create_drops_its_buffer() {
        let mut buffers = Buffers::default();
        script(&mut buffers, &[created("n.rs", "n\n")]);
        assert_eq!(
            buffers.undo(disk(&[])).unwrap(),
            [file_change("n.rs", Some("n\n"), None)]
        );
        assert_eq!(buffers.unwritten().count(), 0);
    }

    #[test]
    fn undo_after_a_write_leaves_the_file_unwritten() {
        let mut buffers = Buffers::default();
        script(&mut buffers, &[change("a.rs", "a\n", "b\n")]);
        let writes = buffers
            .plan_write(None, disk(&[("a.rs", "a\n")]), false)
            .unwrap();
        buffers.written(&writes);
        assert_eq!(buffers.unwritten().count(), 0);
        buffers.undo(disk(&[("a.rs", "b\n")])).unwrap();
        assert_eq!(text(&buffers, "a.rs"), Some("a\n"));
        assert_eq!(buffers.base(&path("a.rs")), Some(Some("b\n")));
    }

    #[test]
    fn undo_merges_into_a_file_changed_since() {
        let mut buffers = Buffers::default();
        script(&mut buffers, &[change("a.rs", "1\n2\n3\n", "x\n2\n3\n")]);
        let writes = buffers
            .plan_write(None, disk(&[("a.rs", "1\n2\n3\n")]), false)
            .unwrap();
        buffers.written(&writes);
        buffers.undo(disk(&[("a.rs", "x\n2\ny\n")])).unwrap();
        assert_eq!(text(&buffers, "a.rs"), Some("1\n2\ny\n"));

        let mut buffers = Buffers::default();
        script(&mut buffers, &[change("a.rs", "1\n2\n3\n", "x\n2\n3\n")]);
        let writes = buffers
            .plan_write(None, disk(&[("a.rs", "1\n2\n3\n")]), false)
            .unwrap();
        buffers.written(&writes);
        match buffers.undo(disk(&[("a.rs", "z\n2\n3\n")])) {
            Err(BuffersError::UndoOverlap { line: 1, .. }) => {}
            other => panic!("{other:?}"),
        }
        assert_eq!(buffers.unwritten().count(), 0);
    }

    #[test]
    fn undo_of_a_written_create_is_refused() {
        let mut buffers = Buffers::default();
        script(&mut buffers, &[created("n.rs", "n\n")]);
        let writes = buffers.plan_write(None, disk(&[]), false).unwrap();
        buffers.written(&writes);
        assert!(matches!(
            buffers.undo(disk(&[("n.rs", "n\n")])),
            Err(BuffersError::Written { .. })
        ));
    }

    #[test]
    fn a_write_holds_each_buffer_over_its_file() {
        let mut buffers = Buffers::default();
        script(
            &mut buffers,
            &[change("a.rs", "a\n", "b\n"), created("n.rs", "n\n")],
        );
        let writes = buffers
            .plan_write(None, disk(&[("a.rs", "a\n")]), false)
            .unwrap();
        assert_eq!(
            writes,
            [
                file_change("a.rs", Some("a\n"), Some("b\n")),
                file_change("n.rs", None, Some("n\n")),
            ]
        );
        assert_eq!(buffers.unwritten().count(), 2);
        buffers.written(&writes);
        assert_eq!(buffers.unwritten().count(), 0);
    }

    #[test]
    fn a_write_merges_into_a_file_changed_since() {
        let mut buffers = Buffers::default();
        script(&mut buffers, &[change("a.rs", "1\n2\n3\n", "x\n2\n3\n")]);
        let writes = buffers
            .plan_write(None, disk(&[("a.rs", "1\n2\ny\n")]), false)
            .unwrap();
        assert_eq!(
            writes,
            [file_change("a.rs", Some("1\n2\ny\n"), Some("x\n2\ny\n"))]
        );
    }

    #[test]
    fn an_overlapping_write_is_refused_unless_forced() {
        let mut buffers = Buffers::default();
        script(&mut buffers, &[change("a.rs", "1\n2\n3\n", "x\n2\n3\n")]);
        let files = [("a.rs", "z\n2\n3\n")];
        match buffers.plan_write(None, disk(&files), false) {
            Err(BuffersError::Overlap { line: 1, .. }) => {}
            other => panic!("{other:?}"),
        }
        assert_eq!(
            buffers.plan_write(None, disk(&files), true).unwrap(),
            [file_change("a.rs", Some("z\n2\n3\n"), Some("x\n2\n3\n"))]
        );
    }

    #[test]
    fn a_created_file_that_now_exists_is_refused_unless_forced() {
        let mut buffers = Buffers::default();
        script(&mut buffers, &[created("n.rs", "n\n")]);
        let files = [("n.rs", "m\n")];
        assert!(matches!(
            buffers.plan_write(None, disk(&files), false),
            Err(BuffersError::Exists { .. })
        ));
        assert_eq!(
            buffers.plan_write(None, disk(&files), true).unwrap(),
            [file_change("n.rs", Some("m\n"), Some("n\n"))]
        );
    }

    #[test]
    fn a_removed_file_is_refused_unless_forced() {
        let mut buffers = Buffers::default();
        script(&mut buffers, &[change("a.rs", "a\n", "b\n")]);
        assert!(matches!(
            buffers.plan_write(None, disk(&[]), false),
            Err(BuffersError::Removed { .. })
        ));
        assert_eq!(
            buffers.plan_write(None, disk(&[]), true).unwrap(),
            [file_change("a.rs", None, Some("b\n"))]
        );
    }

    #[test]
    fn a_file_reached_by_two_paths_has_one_buffer() {
        let mut buffers = Buffers::default();
        script(&mut buffers, &[change("a.rs", "a\n", "b\n")]);
        script(&mut buffers, &[change("sub/../a.rs", "b\n", "c\n")]);
        assert_eq!(buffers.unwritten().collect::<Vec<_>>(), [path("a.rs")]);
        assert_eq!(buffers.base(&path("a.rs")), Some(Some("a\n")));
    }

    #[cfg(unix)]
    #[test]
    fn two_buffers_that_became_one_file_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("a.rs"), "a\n").unwrap();
        let mut buffers = Buffers::default();
        let changes = [change("a.rs", "a\n", "b\n"), created("link/a.rs", "c\n")];
        buffers.apply(&root, &changes, &["b\n", "c\n"]);
        std::os::unix::fs::symlink(&root, root.join("link")).unwrap();
        let read = |p: &Path| Ok(std::fs::read_to_string(p).ok());
        assert!(matches!(
            buffers.plan_write(None, read, true),
            Err(BuffersError::SameFile { .. })
        ));
    }

    #[test]
    fn named_buffers_are_written_or_reloaded_alone() {
        let mut buffers = Buffers::default();
        script(
            &mut buffers,
            &[change("a.rs", "a\n", "b\n"), change("b.rs", "x\n", "y\n")],
        );
        let files = [("a.rs", "a\n"), ("b.rs", "x\n")];
        let writes = buffers
            .plan_write(Some(&[path("b.rs")]), disk(&files), false)
            .unwrap();
        assert_eq!(writes, [file_change("b.rs", Some("x\n"), Some("y\n"))]);
        assert!(matches!(
            buffers.plan_write(Some(&[path("c.rs")]), disk(&files), false),
            Err(BuffersError::NotBuffered { .. })
        ));
        buffers.reload(Some(&[path("a.rs")])).unwrap();
        assert_eq!(buffers.unwritten().collect::<Vec<_>>(), [path("b.rs")]);
        assert!(matches!(
            buffers.reload(Some(&[path("a.rs")])),
            Err(BuffersError::NotBuffered { .. })
        ));
        buffers.reload(None).unwrap();
        assert_eq!(buffers.unwritten().count(), 0);
    }

    #[test]
    fn rebasing_merges_a_change_into_the_unwritten_edits() {
        let mut buffers = Buffers::default();
        script(&mut buffers, &[change("a.rs", "1\n2\n3\n", "x\n2\n3\n")]);
        assert!(buffers.rebase(&path("a.rs"), "1\n2\ny\n").unwrap());
        assert_eq!(text(&buffers, "a.rs"), Some("x\n2\ny\n"));
        assert_eq!(buffers.base(&path("a.rs")), Some(Some("1\n2\ny\n")));
        let writes = buffers
            .plan_write(None, disk(&[("a.rs", "1\n2\ny\n")]), false)
            .unwrap();
        assert_eq!(
            writes,
            [file_change("a.rs", Some("1\n2\ny\n"), Some("x\n2\ny\n"))]
        );
        assert!(!buffers.rebase(&path("b.rs"), "b\n").unwrap());
    }

    #[test]
    fn a_change_that_makes_the_same_edit_leaves_nothing_unwritten() {
        let mut buffers = Buffers::default();
        script(&mut buffers, &[change("a.rs", "a\n", "b\n")]);
        assert!(buffers.rebase(&path("a.rs"), "b\n").unwrap());
        assert_eq!(buffers.unwritten().count(), 0);
    }

    #[test]
    fn an_overlapping_change_leaves_the_buffer_as_it_was() {
        let mut buffers = Buffers::default();
        script(&mut buffers, &[change("a.rs", "1\n2\n3\n", "x\n2\n3\n")]);
        match buffers.rebase(&path("a.rs"), "z\n2\n3\n") {
            Err(BuffersError::Overlap { line: 1, .. }) => {}
            other => panic!("{other:?}"),
        }
        assert_eq!(text(&buffers, "a.rs"), Some("x\n2\n3\n"));
        assert_eq!(buffers.base(&path("a.rs")), Some(Some("1\n2\n3\n")));
    }

    #[test]
    fn a_created_file_that_another_program_writes_is_an_error() {
        let mut buffers = Buffers::default();
        script(&mut buffers, &[created("n.rs", "n\n")]);
        assert!(matches!(
            buffers.rebase(&path("n.rs"), "m\n"),
            Err(BuffersError::Exists { .. })
        ));
        assert_eq!(text(&buffers, "n.rs"), Some("n\n"));
    }
}
