//! Syntax highlighting from the grammars' highlight queries, for terminal
//! output (command-language spec, §6.6).

use std::borrow::Cow;
use std::cmp::Reverse;
use std::ops::Range;

use tree_sitter::{Node, QueryCursor, StreamingIterator, Tree};

use crate::lang::Language;
use crate::style::{Role, Style};

/// What a highlight capture marks, which decides its colour. Captures
/// without one, such as `@variable` and `@punctuation`, stay plain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Keyword,
    String,
    Comment,
    Function,
    Type,
    /// Constants, numbers, escapes and attributes.
    Constant,
    /// A JSX tag.
    Tag,
    /// A Markdown heading.
    Heading,
    /// A Markdown link destination or reference.
    Link,
}

/// The capture names, or dotted prefixes of them, that have a group.
const GROUPS: &[(&str, Group)] = &[
    ("keyword", Group::Keyword),
    ("string", Group::String),
    ("character", Group::String),
    ("text.literal", Group::String),
    ("comment", Group::Comment),
    ("function", Group::Function),
    ("constructor", Group::Type),
    ("type", Group::Type),
    ("constant", Group::Constant),
    ("number", Group::Constant),
    ("boolean", Group::Constant),
    ("escape", Group::Constant),
    ("string.escape", Group::Constant),
    ("attribute", Group::Constant),
    ("tag", Group::Tag),
    ("text.title", Group::Heading),
    ("text.uri", Group::Link),
    ("text.reference", Group::Link),
];

/// The group of the capture `name`, by its longest dotted prefix that has
/// one: `function.method` is a `Function`.
pub fn group(name: &str) -> Option<Group> {
    let mut prefix = name;
    loop {
        if let Some((_, group)) = GROUPS.iter().find(|(p, _)| *p == prefix) {
            return Some(*group);
        }
        prefix = &prefix[..prefix.rfind('.')?];
    }
}

/// The highlighted ranges of `text` within `range`, from `tree` and `lang`'s
/// highlight query, in order and not overlapping. Where captures nest, the
/// innermost wins; where they mark the same node, the latest pattern wins, as
/// in tree-sitter's own highlighter. Unlike it, captures without a group are
/// ignored, so Go's closing `(identifier) @variable` doesn't hide functions.
pub fn spans(
    lang: Language,
    tree: &Tree,
    text: &str,
    range: Range<usize>,
) -> Vec<(Range<usize>, Group)> {
    let query = lang.highlights();
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(range.clone());
    let mut captures = Vec::new();
    let mut matches = cursor.matches(query, tree.root_node(), text.as_bytes());
    while let Some(m) = matches.next() {
        for c in m.captures() {
            let group = match names[c.index as usize] {
                // A builtin the grammar parses as its own node, such as `self` or
                // `this`, is a keyword; one parsed as an identifier, such as
                // JavaScript's `console`, may be shadowed by a local.
                "variable.builtin" if c.node.kind() != "identifier" => Some(Group::Keyword),
                name => group(name),
            };
            if let Some(group) = group
                && !c.node.byte_range().is_empty()
            {
                captures.push((c.node, m.pattern_index, group));
            }
        }
    }
    use std::cmp::Ordering;
    // Outer nodes before inner ones, and a node's patterns in order, so that
    // painting in order leaves the innermost node's latest pattern.
    captures.sort_by(|(a, a_pattern, _), (b, b_pattern, _)| {
        let key = |node: &Node| (node.start_byte(), Reverse(node.end_byte()));
        key(a)
            .cmp(&key(b))
            .then_with(|| match a == b {
                true => Ordering::Equal,
                false if encloses(*a, *b) => Ordering::Less,
                false => Ordering::Greater,
            })
            .then(a_pattern.cmp(b_pattern))
    });
    let mut painted: Vec<Option<Group>> = vec![None; range.len()];
    for (node, _, group) in captures {
        let (start, end) = (
            node.start_byte().max(range.start),
            node.end_byte().min(range.end),
        );
        if start < end {
            painted[start - range.start..end - range.start].fill(Some(group));
        }
    }
    let mut spans: Vec<(Range<usize>, Group)> = Vec::new();
    for (i, group) in painted.into_iter().enumerate() {
        let Some(group) = group else { continue };
        let at = range.start + i;
        match spans.last_mut() {
            Some((r, g)) if r.end == at && *g == group => r.end += 1,
            _ => spans.push((at..at + 1, group)),
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

/// `text[range]` painted with `spans`, which `spans` gave for a range
/// containing it, and the text between them painted as `base`.
pub fn paint(
    style: Style,
    text: &str,
    range: Range<usize>,
    spans: &[(Range<usize>, Group)],
    base: Option<Role>,
) -> String {
    let gap = |text| match base {
        Some(role) => style.paint(role, text),
        None => Cow::Borrowed(text),
    };
    let mut out = String::new();
    let mut at = range.start;
    let first = spans.partition_point(|(r, _)| r.end <= range.start);
    for (span, group) in &spans[first..] {
        if span.start >= range.end {
            break;
        }
        let (start, end) = (span.start.max(range.start), span.end.min(range.end));
        out.push_str(&gap(&text[at..start]));
        out.push_str(&style.paint(Role::Code(*group), &text[start..end]));
        at = end;
    }
    out.push_str(&gap(&text[at..range.end]));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::shown;

    /// Each highlighted piece of `text` in `lang`, with its group.
    fn groups(lang: Language, text: &str) -> Vec<(&str, Group)> {
        let tree = lang.parse(text);
        spans(lang, &tree, text, 0..text.len())
            .into_iter()
            .map(|(range, group)| (&text[range], group))
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
    fn captures_group_by_their_longest_dotted_prefix() {
        let cases = [
            ("keyword", Some(Group::Keyword)),
            ("function.method", Some(Group::Function)),
            ("function.macro", Some(Group::Function)),
            ("string.special", Some(Group::String)),
            ("string.escape", Some(Group::Constant)),
            ("escape", Some(Group::Constant)),
            ("comment.documentation", Some(Group::Comment)),
            ("constant.builtin", Some(Group::Constant)),
            ("number", Some(Group::Constant)),
            ("attribute", Some(Group::Constant)),
            ("constructor", Some(Group::Type)),
            ("type.builtin", Some(Group::Type)),
            ("variable.builtin", None),
            ("tag", Some(Group::Tag)),
            ("text.title", Some(Group::Heading)),
            ("text.uri", Some(Group::Link)),
            ("text.reference", Some(Group::Link)),
            ("text.literal", Some(Group::String)),
            ("variable", None),
            ("variable.parameter", None),
            ("property", None),
            ("punctuation.bracket", None),
            ("operator", None),
            ("none", None),
            ("keywords", None),
        ];
        for (name, expected) in cases {
            assert_eq!(group(name), expected, "{name}");
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
        let found = spans(Language::Rust, &tree, text, 0..text.len());
        for pair in found.windows(2) {
            assert!(pair[0].0.end <= pair[1].0.start, "{found:?}");
        }
    }

    #[test]
    fn spans_cover_only_the_range_asked_for() {
        let text = "fn a() {}\nfn b() {}\n";
        let tree = Language::Rust.parse(text);
        let found = spans(Language::Rust, &tree, text, 10..text.len());
        assert!(found.iter().all(|(r, _)| r.start >= 10), "{found:?}");
        assert!(!found.is_empty());
    }

    #[test]
    fn paint_colours_the_spans_inside_the_range() {
        let text = "let s = \"a\nb\";\n";
        let tree = Language::Rust.parse(text);
        let found = spans(Language::Rust, &tree, text, 0..text.len());
        let first = shown(&paint(Style::Color, text, 0..10, &found, None));
        assert_eq!(first, r#"\e[35mlet\e[0m s = \e[32m"a\e[0m"#);
        let second = shown(&paint(Style::Color, text, 11..14, &found, None));
        assert_eq!(second, r#"\e[32mb"\e[0m;"#);
    }

    #[test]
    fn paint_gives_the_text_between_spans_the_base_role() {
        let text = "let s = 1;\n";
        let tree = Language::Rust.parse(text);
        let found = spans(Language::Rust, &tree, text, 0..text.len());
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
        let found = spans(Language::Rust, &tree, text, 0..text.len());
        assert_eq!(paint(Style::Plain, text, 0..10, &found, None), "let s = 1;");
        assert_eq!(paint(Style::Color, text, 4..5, &[], None), "s");
    }
}
