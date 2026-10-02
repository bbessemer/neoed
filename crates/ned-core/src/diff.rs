//! Diff summaries and hunks for edit output (command-language spec, §6.3).

use std::fmt::{self, Write};
use std::ops::Range;

use similar::{ChangeTag, TextDiff};

use crate::style::{Role, Style};

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
    let prefix = if dry_run { "(dry run) " } else { "" };
    format!("{prefix}{path}: created, +{}", stat.added)
}

/// The summary line of a file removed by an undo: `PATH: removed, -D`.
pub fn removed_summary(path: &str, stat: DiffStat) -> String {
    format!("{path}: removed, -{}", stat.removed)
}

/// The number of separate changed regions between two texts.
pub fn regions(old: &str, new: &str) -> usize {
    TextDiff::from_lines(old, new).grouped_ops(0).len()
}

/// A three-way merge of whole lines: `ours` and `theirs` are both edits of
/// `base`, and the result keeps the changes of each. Changes that overlap,
/// or insert at the same point, conflict unless identical: the error is the
/// first one's line in `ours` (from 1).
pub fn merge(base: &str, ours: &str, theirs: &str) -> Result<String, usize> {
    let lines = |text| -> Vec<&str> { str::split_inclusive(text, '\n').collect() };
    let (base, ours, theirs) = (lines(base), lines(ours), lines(theirs));
    let (mine, other) = (changes(&base, &ours), changes(&base, &theirs));
    let mut all: Vec<&Change> = Vec::new();
    for change in &mine {
        if other
            .iter()
            .any(|o| overlaps(&change.base, &o.base) && o != change)
        {
            return Err(change.at + 1);
        }
        all.push(change);
    }
    all.extend(other.iter().filter(|o| !mine.contains(o)));
    all.sort_by_key(|change| (change.base.start, change.base.end));
    let mut out = String::new();
    let mut pos = 0;
    for change in all {
        out.extend(base[pos..change.base.start].iter().copied());
        out.extend(change.lines.iter().copied());
        pos = change.base.end;
    }
    out.extend(base[pos..].iter().copied());
    Ok(out)
}

/// One side's change to a merge's base: base lines `base` become `lines`,
/// which start at line `at` (from 0) of that side.
#[derive(Debug)]
struct Change<'a> {
    base: Range<usize>,
    lines: Vec<&'a str>,
    at: usize,
}

impl PartialEq for Change<'_> {
    /// The same edit, wherever it lands in its side.
    fn eq(&self, other: &Self) -> bool {
        self.base == other.base && self.lines == other.lines
    }
}

fn changes<'a>(base: &[&str], side: &[&'a str]) -> Vec<Change<'a>> {
    let mut out: Vec<Change> = Vec::new();
    for op in similar::capture_diff_slices(similar::Algorithm::Myers, base, side) {
        if op.tag() == similar::DiffTag::Equal {
            continue;
        }
        let (old, new) = (op.old_range(), op.new_range());
        match out.last_mut() {
            Some(last) if last.base.end == old.start => {
                last.base.end = old.end;
                last.lines.extend(&side[new]);
            }
            _ => out.push(Change {
                base: old,
                lines: side[new.clone()].to_vec(),
                at: new.start,
            }),
        }
    }
    out
}

/// Whether two changes touch the same base lines. An insertion conflicts
/// with a change it borders, as it may continue or depend on it.
fn overlaps(a: &Range<usize>, b: &Range<usize>) -> bool {
    match a.is_empty() || b.is_empty() {
        true => a.start <= b.end && b.start <= a.end,
        false => a.start < b.end && b.start < a.end,
    }
}

/// Unified-diff hunks with `@@ -a,b +c,d @@` headers and no file headers,
/// rendered with `\n` line endings.
pub fn hunks(old: &str, new: &str, context: usize, style: Style) -> String {
    let diff = TextDiff::from_lines(old, new);
    let mut out = String::new();
    for hunk in diff.unified_diff().context_radius(context).iter_hunks() {
        let ops = hunk.ops();
        let (first, last) = (&ops[0], &ops[ops.len() - 1]);
        let old_range = first.old_range().start..last.old_range().end;
        let new_range = first.new_range().start..last.new_range().end;
        // similar's own header drops a count of 1; the spec always shows both.
        let header = format!("@@ -{} +{} @@", HunkRange(old_range), HunkRange(new_range));
        let _ = writeln!(out, "{}", style.paint(Role::HunkHeader, &header));
        for change in hunk.iter_changes() {
            let (sign, role) = match change.tag() {
                ChangeTag::Insert => ('+', Some(Role::Added)),
                ChangeTag::Delete => ('-', Some(Role::Removed)),
                ChangeTag::Equal => (' ', None),
            };
            let line = change.value();
            let line = line.strip_suffix('\n').unwrap_or(line);
            let line = line.strip_suffix('\r').unwrap_or(line);
            let line = format!("{sign}{line}");
            let _ = match role {
                Some(role) => writeln!(out, "{}", style.paint(role, &line)),
                None => writeln!(out, "{line}"),
            };
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
    use crate::style::shown;

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
        assert_eq!(hunks(PARSER, &new, 1, Style::Plain), expected);
    }

    #[test]
    fn identical_texts_have_no_hunks() {
        assert_eq!(hunks(PARSER, PARSER, 1, Style::Plain), "");
    }

    #[test]
    fn zero_context() {
        let new = numbered(5).replace("line 3\n", "three\n");
        assert_eq!(
            hunks(&numbered(5), &new, 0, Style::Plain),
            "@@ -3,1 +3,1 @@\n-line 3\n+three\n"
        );
    }

    #[test]
    fn pure_insertion_and_deletion_headers_keep_both_counts() {
        let inserted = numbered(3).replace("line 1\n", "line 1\nnew\n");
        assert_eq!(
            hunks(&numbered(3), &inserted, 0, Style::Plain),
            "@@ -1,0 +2,1 @@\n+new\n"
        );
        let deleted = numbered(3).replace("line 2\n", "");
        assert_eq!(
            hunks(&numbered(3), &deleted, 0, Style::Plain),
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
        assert_eq!(hunks(&numbered(10), &new, 1, Style::Plain), expected);
    }

    #[test]
    fn nearby_changes_share_a_hunk() {
        let new = numbered(6)
            .replace("line 2\n", "two\n")
            .replace("line 4\n", "four\n");
        let expected =
            "@@ -1,5 +1,5 @@\n line 1\n-line 2\n+two\n line 3\n-line 4\n+four\n line 5\n";
        assert_eq!(hunks(&numbered(6), &new, 1, Style::Plain), expected);
    }

    #[test]
    fn crlf_is_rendered_with_lf() {
        let old = "a\r\nb\r\nc\r\n";
        let new = "a\r\nB\r\nc\r\n";
        assert_eq!(
            hunks(old, new, 1, Style::Plain),
            "@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n"
        );
    }

    #[test]
    fn colored_hunks_paint_headers_and_changed_lines() {
        let new = numbered(3).replace("line 2", "two");
        let expected = [
            r"\e[36m@@ -1,3 +1,3 @@\e[0m",
            " line 1",
            r"\e[31m-line 2\e[0m",
            r"\e[32m+two\e[0m",
            " line 3",
            "",
        ];
        assert_eq!(
            shown(&hunks(&numbered(3), &new, 1, Style::Color)),
            expected.join("\n")
        );
    }

    #[test]
    fn removed_summary_line() {
        let stat = DiffStat::between("a\nb\n", "");
        assert_eq!(removed_summary("src/a.rs", stat), "src/a.rs: removed, -2");
    }

    #[test]
    fn regions_count_separate_changes() {
        assert_eq!(regions("a\nb\nc\n", "a\nb\nc\n"), 0);
        assert_eq!(regions("a\nb\nc\n", "A\nB\nc\n"), 1);
        assert_eq!(regions("a\nb\nc\n", "A\nb\nC\n"), 2);
    }

    #[test]
    fn merge_keeps_each_sides_changes() {
        let base = "a\nb\nc\nd\n";
        assert_eq!(merge(base, base, base), Ok(base.to_string()));
        assert_eq!(merge(base, "a\nB\nc\nd\n", base), Ok("a\nB\nc\nd\n".into()));
        assert_eq!(merge(base, base, "a\nb\nC\nd\n"), Ok("a\nb\nC\nd\n".into()));
        assert_eq!(
            merge(base, "a\nb\nc\nD\n", "A\nb\nc\nd\n"),
            Ok("A\nb\nc\nD\n".into())
        );
        assert_eq!(
            merge(base, "a\nnew\nb\nc\nd\n", "a\nb\nc\n"),
            Ok("a\nnew\nb\nc\n".into())
        );
    }

    #[test]
    fn identical_changes_merge_once() {
        let base = "a\nb\nc\n";
        assert_eq!(
            merge(base, "a\nB\nc\n", "a\nB\nc\n"),
            Ok("a\nB\nc\n".into())
        );
    }

    #[test]
    fn overlapping_changes_conflict_at_their_line_in_ours() {
        let base = "a\nb\nc\nd\n";
        assert_eq!(merge(base, "a\nX\nc\nd\n", "a\nY\nc\nd\n"), Err(2));
        assert_eq!(merge(base, "a\nnew\nb\nc\nX\n", "a\nb\nc\nY\n"), Err(5));
        assert_eq!(merge(base, "a\nx\nb\nc\nd\n", "a\ny\nb\nc\nd\n"), Err(2));
    }

    #[test]
    fn a_change_beside_the_other_sides_change_conflicts() {
        assert_eq!(merge("x\n", "x\ny\n", ""), Err(2));
        assert_eq!(merge("a\nb\nc\n", "a\nb\nc\nd\n", ""), Err(4));
        assert_eq!(
            merge("a\nb\nc\n", "a\nb\nc\nd\n", "A\nb\nc\n"),
            Ok("A\nb\nc\nd\n".into())
        );
    }

    #[test]
    fn merge_keeps_line_endings_and_a_missing_final_newline() {
        assert_eq!(
            merge("a\r\nb\r\n", "a\r\nB\r\n", "A\r\nb\r\n"),
            Ok("A\r\nB\r\n".into())
        );
        assert_eq!(merge("a\nb", "a\nb\nc", "A\nb"), Ok("A\nb\nc".into()));
        assert_eq!(merge("a\nb", "a\nb\nc", "a\nB"), Err(2));
    }
}
