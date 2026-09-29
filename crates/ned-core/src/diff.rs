//! Diff summaries and hunks for edit output (command-language spec, §6.3).

use std::fmt::{self, Write};
use std::ops::Range;

use similar::{ChangeTag, TextDiff};

/// Lines added and removed between two texts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiffStat {
    pub added: usize,
    pub removed: usize,
}

impl DiffStat {
    pub fn between(old: &str, new: &str) -> Self {
        let mut stat = DiffStat::default();
        for change in TextDiff::from_lines(old, new).iter_all_changes() {
            match change.tag() {
                ChangeTag::Insert => stat.added += 1,
                ChangeTag::Delete => stat.removed += 1,
                ChangeTag::Equal => {}
            }
        }
        stat
    }
}

/// Formats as `+A -D`.
impl fmt::Display for DiffStat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "+{} -{}", self.added, self.removed)
    }
}

/// The per-file summary line: `PATH: N edits, +A -D`, prefixed with
/// `(dry run) ` when `dry_run` is set.
pub fn summary(path: &str, edits: usize, stat: DiffStat, dry_run: bool) -> String {
    let prefix = if dry_run { "(dry run) " } else { "" };
    let plural = if edits == 1 { "" } else { "s" };
    format!("{prefix}{path}: {edits} edit{plural}, {stat}")
}

/// The summary line of a file made by `create`: `PATH: created, +A`.
pub fn created_summary(path: &str, stat: DiffStat, dry_run: bool) -> String {
    let _ = (path, stat, dry_run);
    String::new()
}

/// Unified-diff hunks with `@@ -a,b +c,d @@` headers and no file headers,
/// rendered with `\n` line endings.
pub fn hunks(old: &str, new: &str, context: usize) -> String {
    let diff = TextDiff::from_lines(old, new);
    let mut out = String::new();
    for hunk in diff.unified_diff().context_radius(context).iter_hunks() {
        let ops = hunk.ops();
        let (first, last) = (&ops[0], &ops[ops.len() - 1]);
        let old_range = first.old_range().start..last.old_range().end;
        let new_range = first.new_range().start..last.new_range().end;
        // similar's own header drops a count of 1; the spec always shows both.
        let _ = writeln!(
            out,
            "@@ -{} +{} @@",
            HunkRange(old_range),
            HunkRange(new_range)
        );
        for change in hunk.iter_changes() {
            let sign = match change.tag() {
                ChangeTag::Insert => '+',
                ChangeTag::Delete => '-',
                ChangeTag::Equal => ' ',
            };
            let line = change.value();
            let line = line.strip_suffix('\n').unwrap_or(line);
            let line = line.strip_suffix('\r').unwrap_or(line);
            let _ = writeln!(out, "{sign}{line}");
        }
    }
    out
}

/// A 0-based line range, formatted as unified diff's 1-based `start,len`.
/// An empty range starts at the line before it.
struct HunkRange(Range<usize>);

impl fmt::Display for HunkRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let len = self.0.len();
        let start = if len == 0 {
            self.0.start
        } else {
            self.0.start + 1
        };
        write!(f, "{start},{len}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PARSER: &str = "\
impl Parser {
    pub fn parse(&mut self) -> Result<Ast, Error> {
        let tok = self.next().expect(\"unexpected end\");
        self.parse_expr(tok)
    }
}
";

    fn numbered(n: usize) -> String {
        (1..=n).map(|i| format!("line {i}\n")).collect()
    }

    #[test]
    fn stat_counts_changed_lines() {
        assert_eq!(DiffStat::between("a\nb\n", "a\nb\n"), DiffStat::default());
        assert_eq!(
            DiffStat::between("a\nb\nc\n", "a\nB\nc\nd\ne\n"),
            DiffStat {
                added: 3,
                removed: 1
            }
        );
        assert_eq!(
            DiffStat::between("a\nb\nc\n", "c\n"),
            DiffStat {
                added: 0,
                removed: 2
            }
        );
    }

    #[test]
    fn stat_display() {
        assert_eq!(
            DiffStat {
                added: 5,
                removed: 3
            }
            .to_string(),
            "+5 -3"
        );
        assert_eq!(DiffStat::default().to_string(), "+0 -0");
    }

    #[test]
    fn summary_line() {
        let stat = DiffStat {
            added: 1,
            removed: 1,
        };
        assert_eq!(
            summary("src/parser.rs", 1, stat, false),
            "src/parser.rs: 1 edit, +1 -1"
        );
        assert_eq!(summary("a.py", 3, stat, false), "a.py: 3 edits, +1 -1");
        assert_eq!(
            summary("a.py", 2, stat, true),
            "(dry run) a.py: 2 edits, +1 -1"
        );
    }

    #[test]
    fn created_summary_line() {
        let stat = DiffStat {
            added: 3,
            removed: 0,
        };
        assert_eq!(created_summary("a.rs", stat, false), "a.rs: created, +3");
        assert_eq!(
            created_summary("a.rs", stat, true),
            "(dry run) a.rs: created, +3"
        );
    }

    #[test]
    fn spec_example_hunk() {
        let new = PARSER.replace("unexpected end", "unexpected end of input");
        let expected = concat!(
            "@@ -2,3 +2,3 @@\n",
            "     pub fn parse(&mut self) -> Result<Ast, Error> {\n",
            "-        let tok = self.next().expect(\"unexpected end\");\n",
            "+        let tok = self.next().expect(\"unexpected end of input\");\n",
            "         self.parse_expr(tok)\n",
        );
        assert_eq!(hunks(PARSER, &new, 1), expected);
    }

    #[test]
    fn identical_texts_have_no_hunks() {
        assert_eq!(hunks(PARSER, PARSER, 1), "");
    }

    #[test]
    fn zero_context() {
        let new = numbered(5).replace("line 3\n", "three\n");
        assert_eq!(
            hunks(&numbered(5), &new, 0),
            "@@ -3,1 +3,1 @@\n-line 3\n+three\n"
        );
    }

    #[test]
    fn pure_insertion_and_deletion_headers_keep_both_counts() {
        let inserted = numbered(3).replace("line 1\n", "line 1\nnew\n");
        assert_eq!(hunks(&numbered(3), &inserted, 0), "@@ -1,0 +2,1 @@\n+new\n");
        let deleted = numbered(3).replace("line 2\n", "");
        assert_eq!(
            hunks(&numbered(3), &deleted, 0),
            "@@ -2,1 +1,0 @@\n-line 2\n"
        );
    }

    #[test]
    fn distant_changes_get_separate_hunks() {
        let new = numbered(10)
            .replace("line 2\n", "two\n")
            .replace("line 9\n", "nine\n");
        let expected = concat!(
            "@@ -1,3 +1,3 @@\n line 1\n-line 2\n+two\n line 3\n",
            "@@ -8,3 +8,3 @@\n line 8\n-line 9\n+nine\n line 10\n",
        );
        assert_eq!(hunks(&numbered(10), &new, 1), expected);
    }

    #[test]
    fn nearby_changes_share_a_hunk() {
        let new = numbered(6)
            .replace("line 2\n", "two\n")
            .replace("line 4\n", "four\n");
        let expected =
            "@@ -1,5 +1,5 @@\n line 1\n-line 2\n+two\n line 3\n-line 4\n+four\n line 5\n";
        assert_eq!(hunks(&numbered(6), &new, 1), expected);
    }

    #[test]
    fn crlf_is_rendered_with_lf() {
        let old = "a\r\nb\r\nc\r\n";
        let new = "a\r\nB\r\nc\r\n";
        assert_eq!(hunks(old, new, 1), "@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n");
    }
}
