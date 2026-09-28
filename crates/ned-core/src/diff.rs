//! Diff summaries and hunks for edit output (command-language spec, §6.3).

use std::fmt;

/// Lines added and removed between two texts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiffStat {
    pub added: usize,
    pub removed: usize,
}

impl DiffStat {
    pub fn between(old: &str, new: &str) -> Self {
        todo!()
    }
}

/// Formats as `+A -D`.
impl fmt::Display for DiffStat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        todo!()
    }
}

/// The per-file summary line: `PATH: N edits, +A -D`, prefixed with
/// `(dry run) ` when `dry_run` is set.
pub fn summary(path: &str, edits: usize, stat: DiffStat, dry_run: bool) -> String {
    todo!()
}

/// Unified-diff hunks with `@@ -a,b +c,d @@` headers and no file headers,
/// rendered with `\n` line endings.
pub fn hunks(old: &str, new: &str, context: usize) -> String {
    todo!()
}
