//! Line-oriented text: whole-line spans, indentation, and re-basing
//! (command-language spec, §5). Inserted text uses `\n`; the edit set converts
//! it to the file's line ending.

use std::ops::Range;

/// Whether only whitespace precedes `range` on its first line and only
/// whitespace follows it on its last line (§5.1).
pub fn is_whole_line(text: &str, range: &Range<usize>) -> bool {
    let _ = (text, range);
    todo!()
}

/// Widens `range` to the whole lines it touches, including the last line's
/// ending.
pub fn full_lines(text: &str, range: Range<usize>) -> Range<usize> {
    let start = line_start(text, range.start);
    let end = if range.end > range.start && text[..range.end].ends_with('\n') {
        range.end
    } else {
        text[range.end..]
            .find('\n')
            .map_or(text.len(), |i| range.end + i + 1)
    };
    start..end
}

/// The indentation of the line containing `offset`.
pub fn indent_at(text: &str, offset: usize) -> &str {
    let _ = (text, offset);
    todo!()
}

/// The indentation of the first non-blank line that starts inside `range`.
pub fn first_indent(text: &str, range: Range<usize>) -> Option<&str> {
    let _ = (text, range);
    todo!()
}

/// The file's indent unit: the smallest non-zero increase in indentation
/// between consecutive non-blank lines, or four spaces if there is none.
pub fn indent_unit(text: &str) -> String {
    let _ = text;
    todo!()
}

/// Re-bases line-oriented `text` (§5.2): strips its common indentation,
/// converts its indent style to `unit`'s, and prefixes each non-blank line
/// with `indent`. Blank lines become empty. No final newline is added.
pub fn rebase(text: &str, indent: &str, unit: &str) -> String {
    let _ = (text, indent, unit);
    todo!()
}

/// Verbatim text for a partial-line span (§5.1): the first line as written,
/// the rest re-based to `indent`, the indentation of the span's line.
pub fn rebase_tail(text: &str, indent: &str, unit: &str) -> String {
    let _ = (text, indent, unit);
    todo!()
}

/// Widens a whole-line deletion by one adjacent blank line when deleting
/// `range` would leave two blank lines in a row, or a blank line right after
/// an opening delimiter or right before a closing one (§4.2).
pub fn tidy_delete(text: &str, range: Range<usize>) -> Range<usize> {
    let _ = (text, range);
    todo!()
}

/// Splits `text` into lines, strips their common indentation, and empties
/// blank lines.
pub fn strip_indent(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.split('\n').collect();
    let indent = common_indent(&lines);
    lines
        .iter()
        .map(|l| {
            if l.trim().is_empty() {
                String::new()
            } else {
                l[indent.len()..].to_string()
            }
        })
        .collect()
}

fn common_indent<'a>(lines: &[&'a str]) -> &'a str {
    lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| leading_whitespace(l))
        .reduce(|a, b| {
            let n = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
            &a[..n]
        })
        .unwrap_or("")
}

fn leading_whitespace(line: &str) -> &str {
    &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
}

fn line_start(text: &str, offset: usize) -> usize {
    text[..offset].rfind('\n').map_or(0, |i| i + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "fn a() {\n    let x = 1;\n    let y = 2;\n}\n";

    fn span(text: &str, needle: &str) -> Range<usize> {
        let start = text.find(needle).unwrap();
        start..start + needle.len()
    }

    /// The text left after deleting `tidy_delete(text, span(text, needle))`.
    fn delete(text: &str, needle: &str) -> String {
        let range = tidy_delete(text, span(text, needle));
        format!("{}{}", &text[..range.start], &text[range.end..])
    }

    #[test]
    fn whole_line_spans() {
        for needle in [
            "    let x = 1;\n",
            "let x = 1;",
            "let x = 1;\n    let y = 2;",
            "fn a() {\n    let x = 1;\n    let y = 2;\n}\n",
        ] {
            assert!(is_whole_line(TEXT, &span(TEXT, needle)), "{needle:?}");
        }
        assert!(is_whole_line("a\nb", &(2..3)));
        assert!(is_whole_line("foo  \r\n", &(0..3)));
    }

    #[test]
    fn partial_line_spans() {
        for needle in ["let x", "x = 1;", "1;\n    let y", "{\n"] {
            assert!(!is_whole_line(TEXT, &span(TEXT, needle)), "{needle:?}");
        }
    }

    #[test]
    fn full_lines_widens_to_line_boundaries() {
        assert_eq!(
            full_lines(TEXT, span(TEXT, "x")),
            span(TEXT, "    let x = 1;\n")
        );
        assert_eq!(
            full_lines(TEXT, span(TEXT, "    let x = 1;\n")),
            span(TEXT, "    let x = 1;\n")
        );
        assert_eq!(full_lines("a\r\nbc", 4..5), 3..5);
    }

    #[test]
    fn indentation_lookups() {
        assert_eq!(indent_at(TEXT, TEXT.find('x').unwrap()), "    ");
        assert_eq!(indent_at("\t\tx\n", 2), "\t\t");
        assert_eq!(indent_at(TEXT, 0), "");
        assert_eq!(
            first_indent(TEXT, span(TEXT, "    let x = 1;\n    let y")),
            Some("    ")
        );
        assert_eq!(first_indent("a\n\n  b\n", 2..7), Some("  "));
        assert_eq!(first_indent("a\n\n  \nb\n", 2..6), None);
    }

    #[test]
    fn indent_unit_is_the_smallest_increase() {
        assert_eq!(indent_unit(TEXT), "    ");
        assert_eq!(indent_unit("a:\n  b\n\n  c:\n    d\n"), "  ");
        assert_eq!(indent_unit("a {\n\tb {\n\t\tc\n\t}\n}\n"), "\t");
        assert_eq!(indent_unit("a\n    b\n      c\n"), "  ");
        assert_eq!(indent_unit("a\nb\n"), "    ");
        assert_eq!(indent_unit(""), "    ");
    }

    #[test]
    fn rebase_prefixes_target_indentation() {
        assert_eq!(
            rebase("if req.slow:\n    warn(req)", "        ", "    "),
            "        if req.slow:\n            warn(req)"
        );
        assert_eq!(rebase("use std::io;", "", "    "), "use std::io;");
    }

    #[test]
    fn rebase_strips_common_indentation() {
        assert_eq!(rebase("    a\n      b", "", "    "), "a\n  b");
        assert_eq!(rebase("\t\ta\n\t\t\tb", "\t", "\t"), "\ta\n\t\tb");
    }

    #[test]
    fn rebase_keeps_blank_lines_empty() {
        assert_eq!(
            rebase(
                "\nfn peek(&self) -> Option<char> {\n    self.src[self.pos..].chars().next()\n}",
                "    ",
                "    "
            ),
            "\n    fn peek(&self) -> Option<char> {\n        self.src[self.pos..].chars().next()\n    }"
        );
        assert_eq!(rebase("a\n   \nb\n", "  ", "  "), "  a\n\n  b\n");
    }

    #[test]
    fn rebase_converts_tabs_to_spaces() {
        assert_eq!(
            rebase("a\n\tb\n\t\tc", "  ", "    "),
            "  a\n      b\n          c"
        );
    }

    #[test]
    fn rebase_converts_spaces_to_tabs() {
        assert_eq!(
            rebase("a\n  b\n    c\n     d", "\t", "\t"),
            "\ta\n\t\tb\n\t\t\tc\n\t\t\t d"
        );
    }

    #[test]
    fn rebase_tail_keeps_the_first_line() {
        assert_eq!(
            rebase_tail("foo(\n    a,\n)", "    ", "    "),
            "foo(\n        a,\n    )"
        );
        assert_eq!(rebase_tail("  x", "    ", "    "), "  x");
    }

    #[test]
    fn tidy_removes_one_of_two_blank_lines() {
        assert_eq!(delete("a\n\nb\n\nc\n", "b\n"), "a\n\nc\n");
        assert_eq!(delete("a\n  \nb\n\t\nc\n", "b\n"), "a\n  \nc\n");
        assert_eq!(delete("a\r\n\r\nb\r\n\r\nc\r\n", "b\r\n"), "a\r\n\r\nc\r\n");
    }

    #[test]
    fn tidy_removes_blank_after_opening_delimiter() {
        assert_eq!(
            delete("fn f() {\n    x;\n\n    y;\n}\n", "    x;\n"),
            "fn f() {\n    y;\n}\n"
        );
    }

    #[test]
    fn tidy_removes_blank_before_closing_delimiter() {
        let text =
            "    }\n\n    fn debug_dump(&self) {\n        eprintln!(\"{}\", self.src);\n    }\n}\n";
        assert_eq!(
            delete(
                text,
                "    fn debug_dump(&self) {\n        eprintln!(\"{}\", self.src);\n    }\n"
            ),
            "    }\n}\n"
        );
    }

    #[test]
    fn tidy_removes_at_most_one_line() {
        assert_eq!(delete("{\n\nx\n\n}\n", "x\n"), "{\n\n}\n");
    }

    #[test]
    fn tidy_leaves_single_blank_lines() {
        assert_eq!(delete("a\nb\nc\n", "b\n"), "a\nc\n");
        assert_eq!(delete("a\n\nb\nc\n", "b\n"), "a\n\nc\n");
        assert_eq!(delete("b\n\nc\n", "b\n"), "\nc\n");
    }
}
