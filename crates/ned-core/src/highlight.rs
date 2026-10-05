//! Syntax highlighting from the grammars' highlight queries, for terminal
//! output (command-language spec, §6.6).

use std::borrow::Cow;
use std::cmp::Reverse;
use std::ops::Range;

use tree_sitter::{Node, Query, QueryCursor, QueryError, StreamingIterator, Tree};

use crate::lang::Language;
use crate::style::{Role, Style};

/// A language's compiled highlight query, and how specific each of its
/// patterns is.
pub struct Highlights {
    pub query: Query,
    specificity: Vec<usize>,
}

impl Highlights {
    pub fn new(grammar: &tree_sitter::Language, source: &str) -> Result<Self, QueryError> {
        let query = Query::new(grammar, source)?;
        let specificity = (0..query.pattern_count())
            .map(|i| {
                specificity(&source[query.start_byte_for_pattern(i)..query.end_byte_for_pattern(i)])
            })
            .collect();
        Ok(Highlights { query, specificity })
    }
}

/// `name` and each of its dotted prefixes, longest first: `function.method`,
/// then `function`.
pub fn prefixes(name: &str) -> impl Iterator<Item = &str> {
    std::iter::successors(Some(name), |p| p.rfind('.').map(|i| &p[..i]))
}

/// The highlighted ranges of `text` within `range`, each with its capture
/// name, from `tree` and `lang`'s highlight query, in order and not
/// overlapping. Where captures nest, the innermost wins; where they mark the
/// same node, the latest pattern wins, as in tree-sitter's own highlighter.
/// Unlike it, captures `style` leaves plain are ignored, so Go's closing
/// `(identifier) @variable` doesn't hide functions.
pub fn spans(
    lang: Language,
    tree: &Tree,
    text: &str,
    range: Range<usize>,
    style: Style,
) -> Vec<(Range<usize>, &'static str)> {
    let Highlights { query, specificity } = lang.highlights();
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(range.clone());
    let mut captures = Vec::new();
    let mut matches = cursor.matches(query, tree.root_node(), text.as_bytes());
    while let Some(m) = matches.next() {
        for c in m.captures() {
            let capture = match names[c.index as usize] {
                // In the 16 colours, a builtin the grammar parses as its own node, such
                // as `self` or `this`, is a keyword; one parsed as an identifier, such
                // as JavaScript's `console`, may be shadowed by a local. A theme colours
                // both as `variable.builtin`.
                "variable.builtin" if style == Style::Color && c.node.kind() != "identifier" => {
                    "keyword"
                }
                name => name,
            };
            if style.colours(capture) && !c.node.byte_range().is_empty() {
                captures.push((c.node, m.pattern_index, capture));
            }
        }
    }
    use std::cmp::Ordering;
    // Outer nodes before inner ones, and a node's patterns in order, so that
    // painting in order leaves the innermost node's most specific, then latest,
    // pattern.
    captures.sort_by(|(a, a_pattern, _), (b, b_pattern, _)| {
        let key = |node: &Node| (node.start_byte(), Reverse(node.end_byte()));
        key(a)
            .cmp(&key(b))
            .then_with(|| match a == b {
                true => Ordering::Equal,
                false if encloses(*a, *b) => Ordering::Less,
                false => Ordering::Greater,
            })
            .then((specificity[*a_pattern], a_pattern).cmp(&(specificity[*b_pattern], b_pattern)))
    });
    let mut painted: Vec<Option<&'static str>> = vec![None; range.len()];
    for (node, _, capture) in captures {
        let (start, end) = (
            node.start_byte().max(range.start),
            node.end_byte().min(range.end),
        );
        if start < end {
            painted[start - range.start..end - range.start].fill(Some(capture));
        }
    }
    let mut spans: Vec<(Range<usize>, &'static str)> = Vec::new();
    for (i, capture) in painted.into_iter().enumerate() {
        let Some(capture) = capture else { continue };
        let at = range.start + i;
        match spans.last_mut() {
            Some((r, c)) if r.end == at && *c == capture => r.end += 1,
            _ => spans.push((at..at + 1, capture)),
        }
    }
    spans
}

/// Whether `outer` contains `inner`, a different node with the same,
/// non-empty range. Such nodes form a chain, so this walks down it rather than
/// up from `inner`, as `Node::parent` searches from the root.
fn encloses(outer: Node, inner: Node) -> bool {
    let mut node = outer;
    while let Some(child) = node.child_with_descendant(inner) {
        if child == inner {
            return true;
        }
        if child.byte_range() != inner.byte_range() {
            return false;
        }
        node = child;
    }
    false
}

/// How specific a highlight query pattern is: its nodes and predicates.
fn specificity(pattern: &str) -> usize {
    let mut count = 0;
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => _ = chars.next(),
                        '"' => break,
                        _ => {}
                    }
                }
            }
            ';' => while chars.next_if(|c| *c != '\n').is_some() {},
            // A `(` before another, or before space, only groups.
            '(' if chars
                .peek()
                .is_some_and(|c| *c != '(' && !c.is_whitespace()) =>
            {
                count += 1
            }
            _ => {}
        }
    }
    count
}

/// `text[range]` painted with `spans`, which `spans` gave for a range
/// containing it, and the text between them painted as `base`.
pub fn paint(
    style: Style,
    text: &str,
    range: Range<usize>,
    spans: &[(Range<usize>, &'static str)],
    base: Option<Role>,
) -> String {
    let gap = |text| match base {
        Some(role) => style.paint(role, text),
        None => Cow::Borrowed(text),
    };
    let mut out = String::new();
    let mut at = range.start;
    let first = spans.partition_point(|(r, _)| r.end <= range.start);
    for (span, capture) in &spans[first..] {
        if span.start >= range.end {
            break;
        }
        let (start, end) = (span.start.max(range.start), span.end.min(range.end));
        out.push_str(&gap(&text[at..start]));
        out.push_str(&style.code(capture, base, &text[start..end]));
        at = end;
    }
    out.push_str(&gap(&text[at..range.end]));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::{Group, group, shown};

    /// Each highlighted piece of `text` in `lang`, with its group.
    fn groups(lang: Language, text: &str) -> Vec<(&str, Group)> {
        let tree = lang.parse(text);
        spans(lang, &tree, text, 0..text.len(), Style::Color)
            .into_iter()
            .map(|(range, capture)| (&text[range], group(capture).unwrap()))
            .collect()
    }

    /// Asserts that `text` in `lang` highlights each of `expected`.
    fn assert_highlights(lang: Language, text: &str, expected: &[(&str, Group)]) {
        let found = groups(lang, text);
        for want in expected {
            assert!(found.contains(want), "{lang}: {want:?} not in {found:?}");
        }
    }

    #[test]
    fn every_language_highlights_its_keywords_strings_and_comments() {
        use Group::*;
        assert_highlights(
            Language::Rust,
            "fn main() { let s = \"hi\"; } // c\n",
            &[
                ("fn", Keyword),
                ("main", Function),
                ("let", Keyword),
                ("\"hi\"", String),
                ("// c", Comment),
            ],
        );
        assert_highlights(
            Language::Python,
            "def f():\n    return \"x\"  # c\n",
            &[
                ("def", Keyword),
                ("f", Function),
                ("return", Keyword),
                ("\"x\"", String),
                ("# c", Comment),
            ],
        );
        assert_highlights(
            Language::Go,
            "package main\n\nfunc f() string { return \"x\" } // c\n",
            &[
                ("func", Keyword),
                ("f", Function),
                ("string", Type),
                ("\"x\"", String),
                ("// c", Comment),
            ],
        );
        assert_highlights(
            Language::JavaScript,
            "function f() { return \"x\"; } // c\nconst a = <div>x</div>;\n",
            &[
                ("function", Keyword),
                ("f", Function),
                ("\"x\"", String),
                ("// c", Comment),
                ("div", Tag),
            ],
        );
        assert_highlights(
            Language::TypeScript,
            "function f(): number { return 1; } // c\n",
            &[
                ("function", Keyword),
                ("f", Function),
                ("number", Type),
                ("1", Constant),
                ("// c", Comment),
            ],
        );
        assert_highlights(
            Language::Tsx,
            "const a: string = <div>x</div>;\n",
            &[("const", Keyword), ("string", Type), ("div", Tag)],
        );
        let markdown = groups(Language::Markdown, "# Title\n\n```\ncode\n```\n");
        assert!(markdown.contains(&("Title", Heading)), "{markdown:?}");
        let code = markdown
            .iter()
            .any(|(text, g)| *g == String && text.contains("code"));
        assert!(code, "{markdown:?}");
    }

    #[test]
    fn the_innermost_capture_wins_and_splits_the_outer_one() {
        use Group::*;
        let text = "let s = \"a\\nb\";\n";
        let found = groups(Language::Rust, text);
        let string: Vec<_> = found.into_iter().filter(|(_, g)| *g != Keyword).collect();
        assert_eq!(
            string,
            [("\"a", String), ("\\n", Constant), ("b\"", String)]
        );
    }

    #[test]
    fn of_two_patterns_for_one_node_the_later_wins() {
        // The Python query marks every identifier `@variable`, then capitalised
        // ones `@constructor`, then all-caps ones `@constant`.
        let found = groups(Language::Python, "a = Foo\nb = FOO\nc = foo\n");
        assert!(found.contains(&("Foo", Group::Type)), "{found:?}");
        assert!(found.contains(&("FOO", Group::Constant)), "{found:?}");
        assert!(!found.iter().any(|(text, _)| *text == "foo"), "{found:?}");
    }

    #[test]
    fn of_two_nodes_with_one_range_the_inner_wins() {
        // TypeScript marks `void` a keyword, and the `predefined_type` around it,
        // in a later pattern, a type.
        let found = groups(Language::TypeScript, "function f(): void {}\n");
        assert!(found.contains(&("void", Group::Keyword)), "{found:?}");
    }

    #[test]
    fn builtin_variables_are_plain_but_self_and_this_are_keywords() {
        let rust = groups(Language::Rust, "fn f(&self) { self.a; }\n");
        assert_eq!(
            rust.iter()
                .filter(|g| **g == ("self", Group::Keyword))
                .count(),
            2
        );
        let js = groups(Language::JavaScript, "this.a; console.log(1);\n");
        assert!(js.contains(&("this", Group::Keyword)), "{js:?}");
        assert!(!js.iter().any(|(text, _)| *text == "console"), "{js:?}");
    }

    #[test]
    fn spans_are_ordered_and_disjoint() {
        let text = "/// Doc.\n#[test]\nfn f() -> u8 { CONST.max(b'x') }\n";
        let tree = Language::Rust.parse(text);
        let found = spans(Language::Rust, &tree, text, 0..text.len(), Style::Color);
        for pair in found.windows(2) {
            assert!(pair[0].0.end <= pair[1].0.start, "{found:?}");
        }
    }

    #[test]
    fn spans_cover_only_the_range_asked_for() {
        let text = "fn a() {}\nfn b() {}\n";
        let tree = Language::Rust.parse(text);
        let found = spans(Language::Rust, &tree, text, 10..text.len(), Style::Color);
        assert!(found.iter().all(|(r, _)| r.start >= 10), "{found:?}");
        assert!(!found.is_empty());
    }

    #[test]
    fn paint_colours_the_spans_inside_the_range() {
        let text = "let s = \"a\nb\";\n";
        let tree = Language::Rust.parse(text);
        let found = spans(Language::Rust, &tree, text, 0..text.len(), Style::Color);
        let first = shown(&paint(Style::Color, text, 0..10, &found, None));
        assert_eq!(first, r#"\e[35mlet\e[0m s = \e[32m"a\e[0m"#);
        let second = shown(&paint(Style::Color, text, 11..14, &found, None));
        assert_eq!(second, r#"\e[32mb"\e[0m;"#);
    }

    #[test]
    fn paint_gives_the_text_between_spans_the_base_role() {
        let text = "let s = 1;\n";
        let tree = Language::Rust.parse(text);
        let found = spans(Language::Rust, &tree, text, 0..text.len(), Style::Color);
        let painted = shown(&paint(
            Style::Color,
            text,
            0..10,
            &found,
            Some(Role::Removed),
        ));
        let expected = r"\e[35mlet\e[0m\e[31m s = \e[0m\e[36m1\e[0m\e[31m;\e[0m";
        assert_eq!(painted, expected);
    }

    #[test]
    fn plain_paint_is_the_text() {
        let text = "let s = 1;\n";
        let tree = Language::Rust.parse(text);
        let found = spans(Language::Rust, &tree, text, 0..text.len(), Style::Color);
        assert_eq!(paint(Style::Plain, text, 0..10, &found, None), "let s = 1;");
        assert_eq!(paint(Style::Color, text, 4..5, &[], None), "s");
    }

    const THEME: &str = r##"
[theme]
dark = true
added = "#00ff00"
removed = "#ff0000"
[theme.syntax]
keyword = "#c678dd"
function = "#61afef"
variable = "#abb2bf"
"variable.builtin" = "#e06c75"
constructor = "#e5c07b"
constant = "#d19a66"
"##;

    fn captures(lang: Language, text: &str) -> Vec<(&str, &'static str)> {
        let style = Style::Theme(
            crate::theme::tests::leaked(THEME),
            crate::style::Depth::Truecolor,
        );
        let tree = lang.parse(text);
        spans(lang, &tree, text, 0..text.len(), style)
            .into_iter()
            .map(|(range, capture)| (&text[range], capture))
            .collect()
    }

    #[test]
    fn a_theme_highlights_by_capture_name() {
        let found = captures(Language::Rust, "fn main() { let x = 1; }\n");
        assert!(found.contains(&("fn", "keyword")), "{found:?}");
        assert!(found.contains(&("main", "function")), "{found:?}");
    }

    #[test]
    fn a_theme_colours_self_and_this_as_builtin_variables() {
        let found = captures(Language::Rust, "fn f(&self) { self.a; }\n");
        assert!(found.contains(&("self", "variable.builtin")), "{found:?}");
        assert!(!found.contains(&("self", "keyword")), "{found:?}");
        let found = captures(Language::JavaScript, "this.a;\n");
        assert!(found.contains(&("this", "variable.builtin")), "{found:?}");
    }

    #[test]
    fn a_more_specific_pattern_beats_a_later_catch_all() {
        // Go's query ends with `(identifier) @variable`, after the patterns that
        // mark functions.
        let found = captures(Language::Go, "package p\nfunc main() { foo(x) }\n");
        assert!(found.contains(&("main", "function")), "{found:?}");
        assert!(found.contains(&("foo", "function")), "{found:?}");
        assert!(found.contains(&("x", "variable")), "{found:?}");
    }

    #[test]
    fn a_later_pattern_with_a_predicate_beats_an_earlier_catch_all() {
        let found = captures(Language::Python, "a = Foo\nb = FOO\n");
        assert!(found.contains(&("a", "variable")), "{found:?}");
        assert!(found.contains(&("Foo", "constructor")), "{found:?}");
        assert!(found.contains(&("FOO", "constant")), "{found:?}");
    }

    #[test]
    fn specificity_counts_nodes_and_predicates() {
        assert_eq!(specificity("(identifier) @variable"), 1);
        assert_eq!(specificity("((identifier) @c (#match? @c \"^[A-Z]\"))"), 2);
        assert_eq!(
            specificity("(call_expression function: (identifier) @function)"),
            2
        );
        assert_eq!(
            specificity("((identifier) @x (#match? @x \"^(a)(b)$\"))"),
            2
        );
        assert_eq!(specificity("((identifier) @x (#eq? @x \"\\\"(\"))"), 2);
        assert_eq!(specificity("; (comment)\n(identifier) @variable"), 1);
        assert_eq!(specificity("\"fn\" @keyword"), 0);
        assert_eq!(specificity("[(a) (b)] @x"), 2);
    }

    #[test]
    fn a_theme_tints_highlighted_code_on_changed_lines() {
        let style = Style::Theme(
            crate::theme::tests::leaked(THEME),
            crate::style::Depth::Truecolor,
        );
        let text = "let s = 1;\n";
        let tree = Language::Rust.parse(text);
        let found = spans(Language::Rust, &tree, text, 0..text.len(), style);
        let painted = paint(style, text, 0..10, &found, Some(Role::Removed));
        let expected = [
            style.code("keyword", Some(Role::Removed), "let"),
            style.paint(Role::Removed, " s = "),
            style.code("constant.builtin", Some(Role::Removed), "1"),
            style.paint(Role::Removed, ";"),
        ]
        .concat();
        assert_eq!(shown(&painted), shown(&expected));
    }
}
