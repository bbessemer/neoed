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
