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
