//! Line-oriented text: whole-line spans, indentation, and re-basing
//! (command-language spec, §5). Inserted text uses `\n`; the edit set converts
//! it to the file's line ending.

use std::ops::Range;

/// Whether only whitespace precedes `range` on its first line and only
/// whitespace follows it on its last line (§5.1).
pub fn is_whole_line(text: &str, range: &Range<usize>) -> bool {
    let before = &text[line_start(text, range.start)..range.start];
    let after = &text[range.end..line_end(text, range.end, range.start)];
    before.trim().is_empty() && after.trim().is_empty()
}

/// Widens `range` to the whole lines it touches, including the last line's
/// ending.
pub fn full_lines(text: &str, range: Range<usize>) -> Range<usize> {
    let start = line_start(text, range.start);
    let end = if range.end > range.start && text[..range.end].ends_with('\n') {
        range.end
    } else {
        next_line(text, range.end)
    };
    start..end
}

/// The indentation of the line containing `offset`.
pub fn indent_at(text: &str, offset: usize) -> &str {
    leading_whitespace(&text[line_start(text, offset)..])
}

/// The indentation of the first non-blank line that starts inside `range`.
pub fn first_indent(text: &str, range: Range<usize>) -> Option<&str> {
    let mut pos = range.start;
    if pos != line_start(text, pos) {
        pos = next_line(text, pos);
    }
    while pos < range.end {
        let end = next_line(text, pos);
        let line = &text[pos..end];
        if !line.trim().is_empty() {
            return Some(leading_whitespace(line));
        }
        pos = end;
    }
    None
}

/// The indentation of the last non-blank line of the whole lines `range`.
pub fn last_indent(text: &str, range: Range<usize>) -> Option<&str> {
    text[range]
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(leading_whitespace)
}

/// The indent unit of a file's `lines`: the smallest non-zero increase in
/// indentation between consecutive non-blank lines, in the style (tabs or
/// spaces) that indents most of them, or `default` if there is none.
pub fn indent_unit<'a>(lines: impl IntoIterator<Item = &'a str>, default: &str) -> String {
    let indents: Vec<&str> = lines
        .into_iter()
        .filter(|l| !l.trim().is_empty())
        .map(leading_whitespace)
        .collect();
    let count = |c| indents.iter().filter(|i| i.starts_with(c)).count();
    let (tabs, spaces) = (count('\t'), count(' '));
    let style = match tabs.cmp(&spaces) {
        std::cmp::Ordering::Greater => '\t',
        std::cmp::Ordering::Less => ' ',
        std::cmp::Ordering::Equal => default.chars().next().unwrap_or(' '),
    };
    indents
        .windows(2)
        .filter_map(|w| w[1].strip_prefix(w[0]))
        .filter(|increase| !increase.is_empty() && increase.chars().all(|c| c == style))
        .min_by_key(|increase| increase.len())
        .unwrap_or(default)
        .to_string()
}

/// Re-bases line-oriented `text` (§5.2): strips its common indentation,
/// converts its indent style to `unit`'s, and prefixes each non-blank line
/// with `indent`. Blank lines become empty. No final newline is added.
pub fn rebase(text: &str, indent: &str, unit: &str) -> String {
    let lines = strip_indent(text);
    let level = lines
        .iter()
        .map(|l| leading_whitespace(l))
        .filter(|w| !w.is_empty())
        .min_by_key(|w| w.len());
    let convert = level.filter(|level| !unit.starts_with(&level[..1]));
    lines
        .iter()
        .map(|line| match (line.is_empty(), convert) {
            (true, _) => String::new(),
            (false, Some(level)) => format!("{indent}{}", convert_indent(line, level, unit)),
            (false, None) => format!("{indent}{line}"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Verbatim text for a partial-line span (§5.1): the first line as written,
/// the rest re-based to `indent`, the indentation of the span's line.
pub fn rebase_tail(text: &str, indent: &str, unit: &str) -> String {
    match text.split_once('\n') {
        Some((first, rest)) => format!("{first}\n{}", rebase(rest, indent, unit)),
        None => text.to_string(),
    }
}

/// Whether a blank line lies directly above or below the whole lines `full`.
pub fn blank_separated(text: &str, full: Range<usize>) -> bool {
    let above = text[..full.start].strip_suffix('\n').is_some_and(|before| {
        before[before.rfind('\n').map_or(0, |i| i + 1)..]
            .trim()
            .is_empty()
    });
    let below = text[full.end..]
        .lines()
        .next()
        .is_some_and(|l| l.trim().is_empty());
    above || below
}

/// Widens a whole-line deletion by one adjacent blank line when deleting
/// `range` would leave two blank lines in a row, a blank line right after an
/// opening delimiter (or a line ending in `:`) or right before a closing one,
/// or a blank line at the start or end of the file (§4.2).
pub fn tidy_delete(text: &str, range: Range<usize>) -> Range<usize> {
    let blank = |line: &str| line.trim().is_empty();
    // The lines around the span, with their other ends; `None` at the start
    // or end of the file, which count as delimiters.
    let prev = (range.start > 0).then(|| {
        let start = line_start(text, range.start - 1);
        (start, &text[start..range.start])
    });
    let next = (range.end < text.len()).then(|| {
        let end = next_line(text, range.end);
        (end, &text[range.end..end])
    });
    let after_opening =
        prev.is_none_or(|(_, p)| blank(p) || p.trim_end().ends_with(['{', '(', '[', ':']));
    let before_closing = next.is_none_or(|(_, n)| n.trim_start().starts_with(['}', ')', ']']));
    match (prev, next) {
        (_, Some((end, next))) if blank(next) && after_opening => range.start..end,
        (Some((start, prev)), _) if blank(prev) && before_closing => start..range.end,
        _ => range,
    }
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

/// Replaces each whole `level` of `line`'s indentation with `unit`.
fn convert_indent(line: &str, level: &str, unit: &str) -> String {
    let mut rest = line;
    let mut levels = 0;
    while let Some(r) = rest.strip_prefix(level) {
        rest = r;
        levels += 1;
    }
    unit.repeat(levels) + rest
}

/// The start of the line after the one containing `offset`, or the end of
/// the text.
fn next_line(text: &str, offset: usize) -> usize {
    text[offset..]
        .find('\n')
        .map_or(text.len(), |i| offset + i + 1)
}

/// Where the line a span ending at `end` finishes: `end` itself if the span
/// (starting at `start`) already ends with a line ending, else the position
/// of the next line ending (or the end of the text).
fn line_end(text: &str, end: usize, start: usize) -> usize {
    if end > start && text[..end].ends_with('\n') {
        end
    } else {
        text[end..].find('\n').map_or(text.len(), |i| end + i)
    }
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
        let unit = |text: &str, default| indent_unit(text.lines(), default);
        assert_eq!(unit(TEXT, "    "), "    ");
        assert_eq!(unit("a:\n  b\n\n  c:\n    d\n", "    "), "  ");
        assert_eq!(unit("a {\n\tb {\n\t\tc\n\t}\n}\n", "    "), "\t");
        assert_eq!(unit("a\n    b\n      c\n", "    "), "  ");
        assert_eq!(unit("a\nb\n", "    "), "    ");
        assert_eq!(unit("", "    "), "    ");
    }

    #[test]
    fn indent_unit_takes_the_dominant_style() {
        let unit = |text: &str, default| indent_unit(text.lines(), default);
        let mixed = "a {\n\tb\n    c {\n        d\n    }\n}\n";
        assert_eq!(unit(mixed, "\t"), "    ");
        assert_eq!(unit("a\n\tb\nc\n  d\n", "    "), "  ");
        assert_eq!(unit("a\n\tb\nc\n  d\n", "\t"), "\t");
        assert_eq!(unit("a\n\tb\n\t  c\n\t\td\n", "    "), "\t");
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
        assert_eq!(delete("b\nc\n", "b\n"), "c\n");
        assert_eq!(delete("b\n", "b\n"), "");
    }

    #[test]
    fn tidy_removes_blank_at_start_of_file() {
        assert_eq!(delete("b\n\nc\n", "b\n"), "c\n");
        assert_eq!(delete("b\r\n\r\nc\r\n", "b\r\n"), "c\r\n");
        assert_eq!(delete("b\n\n\nc\n", "b\n"), "\nc\n");
    }

    #[test]
    fn tidy_removes_blank_at_end_of_file() {
        assert_eq!(delete("a\n\nb\n", "b\n"), "a\n");
        assert_eq!(delete("a\n\nb", "b"), "a\n");
        assert_eq!(delete("a\r\n\r\nb\r\n", "b\r\n"), "a\r\n");
    }
}
