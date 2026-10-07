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
    prefix(&relative(text, unit), indent)
}

/// Re-bases line-oriented `text` that replaces lines at `indent` (§5.2): as
/// `rebase`, but when its first non-blank line is indented more than another,
/// that line lands at `indent` and the rest shift with it, down to column 0.
pub fn rebase_replacing(text: &str, indent: &str, unit: &str) -> String {
    let lines = relative(text, unit);
    let first = lines
        .iter()
        .find(|l| !l.is_empty())
        .map_or("", |l| leading_whitespace(l));
    prefix(&lines, &indent[..indent.len().saturating_sub(first.len())])
}

/// `text`'s lines with its common indentation stripped, its indent level (the
/// smallest non-zero indentation of the lines not aligned) converted to
/// `unit`, keeping aligned lines at their offset and lines that start inside a
/// string as written, and blank lines emptied.
fn relative(text: &str, unit: &str) -> Vec<String> {
    let lines = strip_indent(text);
    let aligned = alignments(&lines);
    let quoted = in_string(&lines);
    let level = lines
        .iter()
        .zip(aligned.iter().zip(&quoted))
        .filter(|(_, (a, q))| a.is_none() && !**q)
        .map(|(l, _)| leading_whitespace(l))
        .filter(|w| !w.is_empty())
        .min_by_key(|w| w.len());
    let Some(level) = level.filter(|&level| level != unit) else {
        return lines;
    };
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    for ((line, aligned), quoted) in lines.iter().zip(&aligned).zip(quoted) {
        out.push(match aligned {
            _ if line.is_empty() => String::new(),
            _ if quoted => line.clone(),
            Some((anchor, offset)) => format!(
                "{}{}{}",
                leading_whitespace(&out[*anchor]),
                " ".repeat(*offset),
                line.trim_start()
            ),
            None => convert_indent(line, level, unit),
        });
    }
    out
}

/// For each of `lines`, whether it starts inside a `"` string (a Python `"""`
/// one too) that an earlier line opens.
fn in_string(lines: &[String]) -> Vec<bool> {
    let mut quoted = false;
    lines
        .iter()
        .map(|line| {
            let starts = quoted;
            let mut chars = line.chars().peekable();
            let mut prev = None;
            while let Some(c) = chars.next() {
                match c {
                    '\\' => {
                        chars.next();
                    }
                    // A character literal.
                    '"' if prev == Some('\'') && chars.peek() == Some(&'\'') => {}
                    '"' => quoted = !quoted,
                    '/' if !quoted && chars.peek() == Some(&'/') => break,
                    _ => {}
                }
                prev = Some(c);
            }
            starts
        })
        .collect()
}

/// `lines` joined, each non-blank one prefixed with `indent`.
fn prefix(lines: &[String], indent: &str) -> String {
    lines
        .iter()
        .map(|line| match line.is_empty() {
            true => String::new(),
            false => format!("{indent}{line}"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Verbatim text for a partial-line span (§5.1): the first line as written,
/// the rest re-based to `indent`, the indentation of the span's line.
pub fn rebase_tail(text: &str, indent: &str, unit: &str) -> String {
    rebase_hanging(text, indent, unit, str::to_string)
}

/// `text` with its first line placed by `first` and the rest re-based to
/// `hang`, apart from it (§5.2: prose in a Markdown list item).
pub fn rebase_hanging(
    text: &str,
    hang: &str,
    unit: &str,
    first: impl Fn(&str) -> String,
) -> String {
    match text.split_once('\n') {
        Some((head, rest)) => format!("{}\n{}", first(head), rebase(rest, hang, unit)),
        None => first(text),
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

/// How many blank lines separate the whole lines `full` from their neighbour
/// above and below: the nearest non-blank line, if it is indented at least as
/// deeply as `full`'s first line and isn't an opening (above) or closing
/// (below) delimiter line; `None` where there is no neighbour (§4.2).
pub fn neighbour_gaps(text: &str, full: Range<usize>) -> (Option<usize>, Option<usize>) {
    let indent = leading_whitespace(&text[full.clone()]).len();
    let neighbour = |run: &BlankRun, delimiter: fn(&str) -> bool| {
        run.beyond
            .filter(|l| leading_whitespace(l).len() >= indent && !delimiter(l))
            .map(|_| run.edges.len())
    };
    (
        neighbour(&blank_above(text, full.start), opens),
        neighbour(&blank_below(text, full.end), closes),
    )
}

/// Widens a whole-line deletion over the blank lines beside it, so the gap
/// left behind is the larger of the two around `range`, or no wider than the
/// gap already beside an opening or closing delimiter, and empty at the start
/// or end of the file (§4.2).
pub fn tidy_delete(text: &str, range: Range<usize>) -> Range<usize> {
    let (above, below) = (blank_above(text, range.start), blank_below(text, range.end));
    let (gap_above, gap_below) = (above.edges.len(), below.edges.len());
    let keep = match (above.beyond, below.beyond) {
        (None, _) | (_, None) => 0,
        (Some(prev), Some(next)) => match (opens(prev), closes(next)) {
            (true, true) => gap_above.min(gap_below),
            (true, false) => gap_above,
            (false, true) => gap_below,
            (false, false) => gap_above.max(gap_below),
        },
    };
    // Blank lines below the span go first, so the gap above it survives.
    let removed = gap_above + gap_below - keep;
    let from_below = removed.min(gap_below);
    let start = match removed - from_below {
        0 => range.start,
        n => above.edges[n - 1],
    };
    let end = match from_below {
        0 => range.end,
        n => below.edges[n - 1],
    };
    start..end
}

/// A run of blank lines beside a span: the far edge of each of its lines,
/// nearest first, and the non-blank line beyond it, or `None` at the start or
/// end of the file.
struct BlankRun<'t> {
    edges: Vec<usize>,
    beyond: Option<&'t str>,
}

/// The blank lines directly above `start`, a line start.
fn blank_above(text: &str, start: usize) -> BlankRun<'_> {
    let (mut at, mut edges) = (start, Vec::new());
    while at > 0 {
        let line = line_start(text, at - 1);
        if !text[line..at].trim().is_empty() {
            return BlankRun {
                edges,
                beyond: Some(&text[line..at]),
            };
        }
        edges.push(line);
        at = line;
    }
    BlankRun {
        edges,
        beyond: None,
    }
}

/// The blank lines directly below `end`, a line end.
fn blank_below(text: &str, end: usize) -> BlankRun<'_> {
    let (mut at, mut edges) = (end, Vec::new());
    while at < text.len() {
        let line = next_line(text, at);
        if !text[at..line].trim().is_empty() {
            return BlankRun {
                edges,
                beyond: Some(&text[at..line]),
            };
        }
        edges.push(line);
        at = line;
    }
    BlankRun {
        edges,
        beyond: None,
    }
}

/// Whether `line` opens a block: ends with an opening delimiter or, as in
/// Python, a `:`.
fn opens(line: &str) -> bool {
    line.trim_end().ends_with(['{', '(', '[', ':'])
}

/// Whether `line` starts with a closing delimiter.
fn closes(line: &str) -> bool {
    line.trim_start().starts_with(['}', ')', ']'])
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

/// The length of the list marker (`-`, `*`, `+`, `1.` or `1)`) that `line`
/// starts with.
pub fn list_marker(line: &str) -> Option<usize> {
    let digits = line.len() - line.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let marker = match digits {
        0 => line.strip_prefix(['-', '*', '+']),
        1..=9 => line[digits..].strip_prefix(['.', ')']),
        _ => None,
    }?;
    (marker.is_empty() || marker.starts_with(char::is_whitespace))
        .then(|| line.len() - marker.len())
}

/// For each of `lines` (stripped of their common indentation) that is aligned
/// rather than indented by levels, the line it aligns to and its offset from
/// that line's indentation: a continuation aligned to the text after an
/// unclosed bracket, a block comment's ` * ` line, a list item's continuation
/// under its text, or a line of a fenced code block.
fn alignments(lines: &[String]) -> Vec<Option<(usize, usize)>> {
    let indent = |i: usize| leading_whitespace(&lines[i]).len();
    let mut out = vec![None; lines.len()];
    // Each unclosed bracket's line and the column of the text after it.
    let mut open: Vec<(usize, Option<usize>)> = Vec::new();
    let mut fence = None;
    let mut prev: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        let text = line.trim_start();
        if text.is_empty() {
            continue;
        }
        let at = indent(i);
        let fenced = text.starts_with("```") || text.starts_with("~~~");
        if let Some(f) = fence {
            out[i] = Some((f, at.saturating_sub(indent(f))));
            fence = (!fenced).then_some(f);
            prev = Some(i);
            continue;
        }
        let aligned = prev.and_then(|p| {
            let above = lines[p].trim_start();
            let offset = at.checked_sub(indent(p))?;
            let item =
                list_marker(above).map(|m| m + above[m..].len() - above[m..].trim_start().len());
            let bracket = open
                .last()
                .and_then(|&(l, c)| (c == Some(at)).then(|| (l, at - indent(l))));
            // A comment's ` * ` lines align to its `/*` line.
            let comment = match text.starts_with('*') {
                true if above.starts_with("/*") && offset == 1 => Some((p, 1)),
                true if above.starts_with('*') && offset == 0 => out[p],
                _ => None,
            };
            bracket
                .or(comment)
                .or_else(|| (item == Some(offset)).then_some((p, offset)))
                // A line level with an item's continuation continues it.
                .or_else(|| {
                    out[p].filter(|&(a, _)| {
                        offset == 0 && list_marker(lines[a].trim_start()).is_some()
                    })
                })
        });
        out[i] = aligned;
        if fenced {
            fence = Some(i);
            prev = Some(i);
            continue;
        }
        let mut quoted = false;
        for (col, c) in line.char_indices() {
            match c {
                '"' => quoted = !quoted,
                '(' | '[' | '{' if !quoted => {
                    let after = &line[col + 1..];
                    let text = after.trim_start();
                    let column =
                        (!text.is_empty()).then(|| line[..line.len() - text.len()].chars().count());
                    open.push((i, column));
                }
                ')' | ']' | '}' if !quoted => {
                    open.pop();
                }
                _ => {}
            }
        }
        prev = Some(i);
    }
    out
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

/// The start of the line containing `offset`.
pub fn line_start(text: &str, offset: usize) -> usize {
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
        assert_eq!(rebase("    a\n      b", "", "    "), "a\n    b");
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
    fn rebase_converts_indent_widths() {
        assert_eq!(
            rebase("a {\n  b {\n    c\n  }\n}", "", "    "),
            "a {\n    b {\n        c\n    }\n}"
        );
        assert_eq!(rebase("a\n    b", "", "  "), "a\n  b");
        // A bracket ending its line opens a hanging indent, which is a level.
        assert_eq!(rebase("f(\n  a,\n)", "", "    "), "f(\n    a,\n)");
    }

    #[test]
    fn rebase_keeps_lines_aligned_to_a_bracket() {
        assert_eq!(
            rebase("f(a,\n  b) {\n  c\n}", "", "    "),
            "f(a,\n  b) {\n    c\n}"
        );
        assert_eq!(
            rebase("x = {\n  y: g(1,\n       2),\n}", "", "    "),
            "x = {\n    y: g(1,\n         2),\n}"
        );
        assert_eq!(
            rebase("if x {\n  f(a,\n    b)\n}", "", "\t"),
            "if x {\n\tf(a,\n\t  b)\n}"
        );
        assert_eq!(
            rebase("def f(a,\n      b):\n  return a", "", "    "),
            "def f(a,\n      b):\n    return a"
        );
        // Brackets in strings don't count.
        let text = "f(a,\n  \")\",\n  b)";
        assert_eq!(rebase(text, "", "    "), text);
        assert_eq!(
            rebase("if y {\n  let ü = g(a,\n            b);\n}", "", "    "),
            "if y {\n    let ü = g(a,\n              b);\n}"
        );
    }

    #[test]
    fn rebase_keeps_comment_and_list_continuations_aligned() {
        assert_eq!(
            rebase("/**\n * doc\n */\nfn f() {\n  x\n}", "", "    "),
            "/**\n * doc\n */\nfn f() {\n    x\n}"
        );
        let list = "1. one\n   more\n2. two\n   - x\n     y";
        assert_eq!(rebase(list, "", "    "), list);
        // A fence in an item is aligned like its paragraphs.
        let list = "- c\n\n  para\n\n  ```\n  code\n  ```";
        assert_eq!(rebase(list, "", "    "), list);
    }

    #[test]
    fn rebase_keeps_fenced_code_as_written() {
        let text = "Text:\n\n```\nif x:\n    y\n```\n\n- a\n  b";
        assert_eq!(rebase(text, "", "  "), text);
    }

    #[test]
    fn rebase_keeps_lines_in_strings_as_written() {
        assert_eq!(
            rebase("fn t() {\n    let s = \"\n  hi\";\n    x();\n}", "", "  "),
            "fn t() {\n  let s = \"\n  hi\";\n  x();\n}"
        );
        assert_eq!(
            rebase(
                "def f():\n    s = \"\"\"\n  hi\n\"\"\"\n    return s",
                "",
                "  "
            ),
            "def f():\n  s = \"\"\"\n  hi\n\"\"\"\n  return s"
        );
        // Quotes in character literals and comments open no string.
        assert_eq!(
            rebase("a {\n  b('\"');\n  c // 5\" wide\n  d\n}", "", "    "),
            "a {\n    b('\"');\n    c // 5\" wide\n    d\n}"
        );
    }

    #[test]
    fn rebase_replacing_puts_a_deeper_first_line_at_the_target() {
        assert_eq!(
            rebase_replacing("    x;\n}\n\nfn g() {\n    y;", "    ", "    "),
            "    x;\n}\n\nfn g() {\n    y;"
        );
        assert_eq!(
            rebase_replacing("    x;\n}\nz;", "        ", "    "),
            "        x;\n    }\n    z;"
        );
        assert_eq!(
            rebase_replacing("\tx;\n}", "        ", "    "),
            "        x;\n    }"
        );
    }

    #[test]
    fn rebase_replacing_stops_at_column_zero() {
        assert_eq!(
            rebase_replacing("        x;\n    }\n}\nfn g() {}", "    ", "    "),
            "        x;\n    }\n}\nfn g() {}"
        );
    }

    #[test]
    fn rebase_replacing_text_not_dedenting_its_first_line_rebases() {
        for text in ["if a {\n    b();\n}", "a\n\n  b\n", "use std::io;"] {
            assert_eq!(
                rebase_replacing(text, "    ", "    "),
                rebase(text, "    ", "    ")
            );
        }
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
        assert_eq!(delete("b\n\n\nc\n", "b\n"), "c\n");
        assert_eq!(delete("\nb\nc\n", "b\n"), "c\n");
    }

    #[test]
    fn tidy_removes_blank_at_end_of_file() {
        assert_eq!(delete("a\n\nb\n", "b\n"), "a\n");
        assert_eq!(delete("a\n\nb", "b"), "a\n");
        assert_eq!(delete("a\r\n\r\nb\r\n", "b\r\n"), "a\r\n");
        assert_eq!(delete("a\n\n\nb\n\n", "b\n"), "a\n");
    }

    #[test]
    fn tidy_keeps_the_larger_gap() {
        assert_eq!(delete("a\n\n\nb\n\n\nc\n", "b\n"), "a\n\n\nc\n");
        assert_eq!(delete("a\n\nb\n\n\nc\n", "b\n"), "a\n\n\nc\n");
        assert_eq!(delete("a\n\n\nb\nc\n", "b\n"), "a\n\n\nc\n");
    }

    #[test]
    fn tidy_keeps_blank_lines_already_beside_a_delimiter() {
        assert_eq!(delete("{\n\n\nx\n\ny\n}\n", "x\n"), "{\n\n\ny\n}\n");
        assert_eq!(delete("{\nx\n\n\ny\n\n}\n", "y\n"), "{\nx\n\n}\n");
        assert_eq!(
            delete("class A:\n\n    x\n\n\n    y\n", "    x\n"),
            "class A:\n\n    y\n"
        );
    }

    #[test]
    fn neighbour_gaps_count_blank_lines_to_neighbours() {
        let gaps = |text: &str, needle: &str| neighbour_gaps(text, span(text, needle));
        let py = "def f():\n    pass\n\n\ndef g():\n    pass\n";
        assert_eq!(gaps(py, "def f():\n    pass\n"), (None, Some(2)));
        assert_eq!(gaps(py, "def g():\n    pass\n"), (Some(2), None));
        let class = "class A:\n    def f(self):\n        pass\n\n    def g(self):\n        pass\n\n\nX = 1\n";
        assert_eq!(
            gaps(class, "    def f(self):\n        pass\n"),
            (None, Some(1))
        );
        assert_eq!(
            gaps(class, "    def g(self):\n        pass\n"),
            (Some(1), None)
        );
        assert_eq!(gaps("{\n    a\n\n}\n", "    a\n"), (None, None));
        assert_eq!(gaps("a\nb\nc\n", "b\n"), (Some(0), Some(0)));
    }
}
