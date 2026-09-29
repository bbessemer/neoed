//! Syntax items found by each language's selector query (command-language
//! spec, §3.3).
//!
//! A query in `queries/<lang>/selectors.scm` captures each item node as its
//! kind (`@fn`, `@struct`, ...) and the item's name node as `@name`, in one
//! pattern, with optional `@body` and `@params` nodes for those parts, or a
//! `@head` node whose following lines are the body (a Markdown heading).
//! Standalone `@doc` and `@attr` patterns capture the doc comments and
//! attributes that extend an item's default span when they directly precede
//! it.

use std::cmp::Reverse;
use std::ops::Range;

use crate::script::ast::Part;
use crate::text::full_lines;
use tree_sitter::{Node, Query, QueryCursor, StreamingIterator, Tree};

pub const KINDS: [&str; 18] = [
    "fn",
    "class",
    "struct",
    "enum",
    "variant",
    "trait",
    "interface",
    "impl",
    "type",
    "const",
    "var",
    "field",
    "mod",
    "import",
    "section",
    "item",
    "table",
    "code",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub kind: &'static str,
    pub name: String,
    /// For a trait impl, named `TRAIT for TYPE`: the self type, which the
    /// item's selector also matches.
    pub base_name: Option<String>,
    /// The default span: the item with its leading doc comments and
    /// attributes, and a `,` that directly follows it.
    pub range: Range<usize>,
    /// Whether `range` ends with a trailing `,`.
    pub trailing_comma: bool,
    /// The item node alone.
    pub node: Range<usize>,
    pub name_range: Range<usize>,
    /// The `@body` and `@params` nodes, delimiters included, unless
    /// `body_lines`.
    pub body: Option<Range<usize>>,
    pub params: Option<Range<usize>>,
    /// Whether `body` is the whole lines after a `@head` (a Markdown
    /// section's heading) rather than a delimited node.
    pub body_lines: bool,
    /// The leading doc comments.
    pub doc: Option<Range<usize>>,
}

/// Every item `query` finds in `tree`, ordered by start, outer items first.
pub fn items(query: &Query, tree: &Tree, text: &str) -> Vec<Item> {
    let names = query.capture_names();
    let mut found: Vec<Found> = Vec::new();
    // Every @doc and @attr node, and whether it's a doc.
    let mut leading: Vec<(Range<usize>, bool)> = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), text.as_bytes());
    while let Some(m) = matches.next() {
        let (mut item, mut name, mut body, mut params, mut trait_name, mut head) =
            (None, None, None, None, None, None);
        for capture in m.captures() {
            match names[capture.index as usize] {
                "name" => name = Some(capture.node),
                "body" => body = Some(capture.node.byte_range()),
                "params" => params = Some(capture.node.byte_range()),
                "trait_name" => trait_name = Some(capture.node),
                "head" => head = Some(capture.node.byte_range()),
                "doc" => leading.push((capture.node.byte_range(), true)),
                "attr" => leading.push((capture.node.byte_range(), false)),
                other => {
                    if let Some(kind) = KINDS.iter().find(|&&k| k == other) {
                        item = Some((*kind, capture.node));
                    }
                }
            }
        }
        if let Some((kind, node)) = item {
            found.push(Found {
                kind,
                node,
                name,
                body,
                params,
                trait_name,
                head,
            });
        }
    }
    leading.sort_by_key(|(r, _)| r.start);

    // A wrapper and the node it wraps can both match; keep the widest.
    found.sort_by_key(|f| {
        (
            f.kind,
            f.name.map_or(f.node.start_byte(), |n| n.start_byte()),
            f.node.start_byte(),
            Reverse(f.node.end_byte()),
        )
    });
    found.dedup_by(|b, a| a.kind == b.kind && a.name.is_some() && a.name == b.name);
    // One item per node, under its first name if it has one.
    found.sort_by_key(|f| {
        (
            f.kind,
            f.node.start_byte(),
            Reverse(f.node.end_byte()),
            f.name.is_none(),
            f.name.map(|n| n.start_byte()),
        )
    });
    found.dedup_by(|b, a| a.kind == b.kind && a.node == b.node);

    let name_text = |name: Option<Node>| {
        name.map_or(String::new(), |n| {
            let text = &text[n.byte_range()];
            text.lines().next().unwrap_or_default().trim().to_string()
        })
    };
    let mut items: Vec<Item> = found
        .into_iter()
        .map(
            |Found {
                 kind,
                 node,
                 name,
                 body,
                 params,
                 trait_name,
                 head,
             }| {
                let mut range = node.byte_range();
                // Some nodes (Markdown blocks) take the blank lines after them.
                range.end = range.start + text[range.clone()].trim_end().len();
                let comma = node
                    .next_sibling()
                    .filter(|n| n.kind() == "," && !n.is_named())
                    .filter(|n| {
                        text[range.end..n.start_byte()]
                            .trim_matches([' ', '\t'])
                            .is_empty()
                    });
                if let Some(comma) = comma {
                    range.end = comma.end_byte();
                }
                let mut doc: Option<Range<usize>> = None;
                // Only the nearest node can be directly before: any earlier one has
                // it in between.
                while let Some((l, is_doc)) = leading
                    [..leading.partition_point(|(l, _)| l.start < range.start)]
                    .iter()
                    .rev()
                    .find(|(l, _)| l.end <= range.start)
                    .filter(|(l, _)| directly_before(text, l, range.start))
                {
                    range.start = l.start;
                    if *is_doc {
                        doc = Some(l.start..doc.map_or(l.end, |d| d.end));
                    }
                }
                let body = match &head {
                    Some(head) => Some(lines_after(text, head.clone(), &range)),
                    None => body,
                };
                Item {
                    kind,
                    name: match trait_name {
                        Some(t) => format!("{} for {}", &text[t.byte_range()], name_text(name)),
                        None => name_text(name),
                    },
                    base_name: trait_name.map(|_| name_text(name)),
                    range,
                    trailing_comma: comma.is_some(),
                    node: node.byte_range(),
                    name_range: name
                        .map_or(node.start_byte()..node.start_byte(), |n| n.byte_range()),
                    body,
                    params,
                    body_lines: head.is_some(),
                    doc,
                }
            },
        )
        .collect();
    items.sort_by_key(|i| (i.range.start, Reverse(i.range.end)));
    items
}

/// An item pattern's captures.
struct Found<'t> {
    kind: &'static str,
    node: Node<'t>,
    name: Option<Node<'t>>,
    body: Option<Range<usize>>,
    params: Option<Range<usize>>,
    trait_name: Option<Node<'t>>,
    head: Option<Range<usize>>,
}

/// Whether only whitespace, and no blank line, separates `leading` from
/// `start`.
fn directly_before(text: &str, leading: &Range<usize>, start: usize) -> bool {
    let gap = &text[leading.end..start];
    gap.trim().is_empty()
        && gap.matches('\n').count() + usize::from(text[..leading.end].ends_with('\n')) <= 1
}

/// The whole lines after `head` to the end of `range`'s last line, from the
/// first non-blank one; empty at the end if there are none.
fn lines_after(text: &str, head: Range<usize>, range: &Range<usize>) -> Range<usize> {
    let mut start = full_lines(text, head).end;
    let end = full_lines(text, range.clone()).end.max(start);
    while start < end {
        let line = full_lines(text, start..start);
        if !text[line.clone()].trim().is_empty() {
            break;
        }
        start = line.end;
    }
    start..end
}

/// The span of `part` of `item` (§3.4); `None` if the item doesn't have it.
/// `.lines`, `.refs` and `.def` aren't item parts.
pub fn part(item: &Item, part: Part, text: &str) -> Option<Range<usize>> {
    match part {
        Part::Body => item
            .body
            .clone()
            .map(|r| if item.body_lines { r } else { inside(text, r) }),
        Part::Params => item.params.clone().map(|r| inside(text, r)),
        Part::Name => Some(item.name_range.clone()),
        Part::Sig => Some(match &item.body {
            Some(body) => {
                let start = item.node.start;
                start..start + text[start..body.start].trim_end().len()
            }
            None => item.node.clone(),
        }),
        Part::Doc => item.doc.clone().map(|r| full_lines(text, r)),
        Part::Lines | Part::Refs | Part::Def => None,
    }
}

/// The inside of a node with one-byte delimiters (§3.4): the whole lines
/// between them if the opener ends its line and the closer starts its line,
/// otherwise the text between them with surrounding whitespace trimmed.
fn inside(text: &str, delimited: Range<usize>) -> Range<usize> {
    let (open, close) = (delimited.start + 1, delimited.end - 1);
    let inner = &text[open..close];
    if let Some(newline) = inner.find('\n')
        && inner[..newline].trim().is_empty()
    {
        let first = open + newline + 1;
        let last = text[..close].rfind('\n').map_or(0, |i| i + 1);
        if last >= first && text[last..close].trim().is_empty() {
            return first..last;
        }
    }
    let trimmed = inner.trim();
    if trimmed.is_empty() {
        return open..open;
    }
    let start = open + (inner.len() - inner.trim_start().len());
    start..start + trimmed.len()
}

/// The kinds `query` captures, in `KINDS` order.
pub fn kinds(query: &Query) -> Vec<&'static str> {
    let names = query.capture_names();
    KINDS.into_iter().filter(|k| names.contains(k)).collect()
}

/// Whether `name` matches `pattern`, where `*` matches any run of characters.
pub fn name_matches(pattern: &str, name: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    let Some((last, middle)) = parts.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(i) => rest = &rest[i + part.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

/// Whether `item`'s name, or its base name, matches `pattern`.
pub fn item_matches(pattern: &str, item: &Item) -> bool {
    name_matches(pattern, &item.name)
        || item
            .base_name
            .as_ref()
            .is_some_and(|base| name_matches(pattern, base))
}

/// `kind:name`, with the name quoted when it has characters a bare name can't.
pub fn selector(kind: &str, name: &str) -> String {
    let bare = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '*');
    if !name.is_empty() && name.chars().all(bare) {
        format!("{kind}:{name}")
    } else {
        let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
        format!("{kind}:\"{escaped}\"")
    }
}

/// The optimal string alignment distance between `a` and `b`: edits are
/// insertions, deletions, substitutions and adjacent transpositions.
pub fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    // d[i][j]: the distance between a[..i] and b[..j].
    let mut d = vec![vec![0; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    d[0] = (0..=b.len()).collect();
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[a.len()][b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::Language;

    fn items_in(text: &str) -> Vec<Item> {
        let lang = Language::Rust;
        items(lang.selectors().unwrap(), &lang.parse(text), text)
    }

    /// `(kind, name)` of every item in `text`.
    fn names(text: &str) -> Vec<(&'static str, String)> {
        items_in(text)
            .into_iter()
            .map(|i| (i.kind, i.name))
            .collect()
    }

    /// The default span of the only item of `kind` in `text`.
    fn span<'t>(kind: &str, text: &'t str) -> &'t str {
        let found: Vec<Item> = items_in(text)
            .into_iter()
            .filter(|i| i.kind == kind)
            .collect();
        assert_eq!(found.len(), 1, "{found:?}");
        &text[found[0].range.clone()]
    }

    const RUST: &str = r#"use std::fmt;
use std::{io, fs};

/// A parser.
#[derive(Debug)]
pub struct Parser {
    /// Source.
    src: String,
    pos: usize,
}

enum Token {
    Ident(String),
    Eof,
}

pub trait Parse {
    type Output;
    fn parse(&mut self) -> Self::Output;
}

impl<T> Parse for Wrapper<T> {
    type Output = T;
    fn parse(&mut self) -> T {
        todo!()
    }
}

impl Parser {
    pub fn new(src: &str) -> Self {
        let pos = 0;
        let mut len = src.len();
        Parser { src: src.to_string(), pos }
    }
}

impl fmt::Display for Token {}

type Result<T> = std::result::Result<T, Error>;
const MAX: usize = 10;
static NAME: &str = "ned";
mod tests {}
"#;

    #[test]
    fn rust_items_of_every_kind() {
        let expected = [
            ("import", "std::fmt"),
            ("import", "std::{io, fs}"),
            ("struct", "Parser"),
            ("field", "src"),
            ("field", "pos"),
            ("enum", "Token"),
            ("variant", "Ident"),
            ("variant", "Eof"),
            ("trait", "Parse"),
            ("type", "Output"),
            ("fn", "parse"),
            ("impl", "Parse for Wrapper"),
            ("type", "Output"),
            ("fn", "parse"),
            ("impl", "Parser"),
            ("fn", "new"),
            ("var", "pos"),
            ("var", "len"),
            ("impl", "Display for Token"),
            ("type", "Result"),
            ("const", "MAX"),
            ("const", "NAME"),
            ("mod", "tests"),
        ];
        let expected: Vec<(&str, String)> =
            expected.iter().map(|(k, n)| (*k, n.to_string())).collect();
        assert_eq!(names(RUST), expected);
    }

    #[test]
    fn trait_impls_are_named_trait_for_type() {
        let text = "impl<T> From<T> for a::B<T> {}\nimpl X for &Foo {}\n\
                    impl std::ops::Add for Foo {}\nimpl<T> Y for a::C<T> {}\nimpl Foo {}\n";
        let found: Vec<(String, Option<String>)> = items_in(text)
            .into_iter()
            .map(|i| (i.name, i.base_name))
            .collect();
        let base = |name: &str| Some(name.to_string());
        assert_eq!(
            found,
            [
                ("From for B".into(), base("B")),
                ("X for Foo".into(), base("Foo")),
                ("Add for Foo".into(), base("Foo")),
                ("Y for C".into(), base("C")),
                ("Foo".into(), None),
            ]
        );
        let display = items_in(RUST)
            .into_iter()
            .find(|i| i.name == "Display for Token")
            .unwrap();
        assert!(item_matches("Token", &display));
        assert!(item_matches("Display for *", &display));
        assert!(item_matches("* for Token", &display));
        assert!(!item_matches("Display", &display));
    }

    #[test]
    fn rust_kinds() {
        let lang = Language::Rust;
        assert_eq!(
            kinds(lang.selectors().unwrap()),
            [
                "fn", "struct", "enum", "variant", "trait", "impl", "type", "const", "var",
                "field", "mod", "import"
            ]
        );
    }

    #[test]
    fn default_span_takes_leading_docs_and_attributes() {
        let text = "// Not a doc.\n/// A.\n/** B. */\n#[derive(Debug)]\n/// C.\npub struct A;\n";
        assert_eq!(
            span("struct", text),
            "/// A.\n/** B. */\n#[derive(Debug)]\n/// C.\npub struct A;"
        );
    }

    #[test]
    fn a_blank_line_ends_the_leading_docs() {
        let text = "/// A.\n\n#[inline]\nfn a() {}\n";
        assert_eq!(span("fn", text), "#[inline]\nfn a() {}");
        let text = "/// A.\n\nfn a() {}\n";
        assert_eq!(span("fn", text), "fn a() {}");
    }

    #[test]
    fn leading_docs_belong_to_the_next_item_only() {
        let text = "/// S.\n#[derive(Debug)]\nstruct S;\n/// A.\nfn a() {}\nfn b() {}\n";
        let docs: Vec<Option<&str>> = items_in(text)
            .iter()
            .map(|i| i.doc.clone().map(|d| &text[d]))
            .collect();
        assert_eq!(docs, [Some("/// S.\n"), Some("/// A.\n"), None]);
    }

    #[test]
    fn default_span_takes_a_trailing_comma() {
        let text = "enum A {\n    B(u8),\n    C\n}\nstruct D {\n    e: u8, // e\n}\n";
        let found = items_in(text);
        let spans: Vec<(&str, bool)> = found
            .iter()
            .filter(|i| i.kind != "enum" && i.kind != "struct")
            .map(|i| (&text[i.range.clone()], i.trailing_comma))
            .collect();
        assert_eq!(spans, [("B(u8),", true), ("C", false), ("e: u8,", true)]);
    }

    #[test]
    fn names_match_with_wildcards() {
        assert!(name_matches("parse", "parse"));
        assert!(!name_matches("parse", "parser"));
        assert!(name_matches("test_*", "test_a"));
        assert!(name_matches("test_*", "test_"));
        assert!(!name_matches("test_*", "a_test_b"));
        assert!(name_matches("*_mut", "get_mut"));
        assert!(name_matches("*", ""));
        assert!(name_matches("a*b*c", "a1b2b3c"));
        assert!(!name_matches("a*b*c", "a1b2b3"));
        assert!(name_matches("std::*", "std::fmt"));
    }

    #[test]
    fn selectors_quote_names_when_needed() {
        assert_eq!(selector("fn", "parse_2"), "fn:parse_2");
        assert_eq!(selector("import", "std::fmt"), "import:std::fmt");
        assert_eq!(
            selector("import", "std::{io, fs}"),
            "import:\"std::{io, fs}\""
        );
        assert_eq!(selector("import", "a\"b\\"), "import:\"a\\\"b\\\\\"");
    }

    fn markdown_items(text: &str) -> Vec<Item> {
        let lang = Language::Markdown;
        items(lang.selectors().unwrap(), &lang.parse(text), text)
    }

    const MARKDOWN: &str = "# Title\n\nIntro.\n\n## 6.4 Formatting\n\n- [ ] item one\n      continued\n- item two\n\n  second para\n\n| Code | Meaning |\n| ---- | ------- |\n| 0    | Success |\n\n```rust\nfn x() {}\n```\n\n```\nplain\n```\n\n    indented\n\n### Deep\n\ntext\n";

    #[test]
    fn markdown_items_of_every_kind() {
        let found: Vec<(&str, String)> = markdown_items(MARKDOWN)
            .into_iter()
            .map(|i| (i.kind, i.name))
            .collect();
        let expected = [
            ("section", "Title"),
            ("section", "6.4 Formatting"),
            ("item", "item one"),
            ("item", "item two"),
            ("table", "Code"),
            ("code", "rust"),
            ("code", ""),
            ("code", ""),
            ("section", "Deep"),
        ];
        let expected: Vec<(&str, String)> =
            expected.iter().map(|(k, n)| (*k, n.to_string())).collect();
        assert_eq!(found, expected);
    }

    #[test]
    fn section_bodies_are_the_lines_after_the_heading() {
        let text = "# A\n\nintro\n\n## B\n\nb text\n\n## C\n\n# D";
        let items = markdown_items(text);
        let section = |name: &str| items.iter().find(|i| i.name == name).unwrap();
        let body = |name: &str| part(section(name), Part::Body, text).map(|r| &text[r]);
        assert_eq!(body("A"), Some("intro\n\n## B\n\nb text\n\n## C\n"));
        assert_eq!(body("B"), Some("b text\n"));
        assert_eq!(body("C"), Some(""));
        assert_eq!(part(section("C"), Part::Body, text), Some(31..31));
        assert_eq!(
            part(section("D"), Part::Body, text),
            Some(text.len()..text.len())
        );
        assert_eq!(
            part(section("A"), Part::Sig, text).map(|r| &text[r]),
            Some("# A")
        );
    }

    #[test]
    fn markdown_spans_end_at_their_last_text() {
        let items = markdown_items(MARKDOWN);
        let span = |kind: &str, name: &str| {
            let item = items
                .iter()
                .find(|i| i.kind == kind && i.name == name)
                .unwrap();
            &MARKDOWN[item.range.clone()]
        };
        assert_eq!(span("section", "Deep"), "### Deep\n\ntext");
        assert_eq!(span("item", "item one"), "- [ ] item one\n      continued");
        assert_eq!(span("item", "item two"), "- item two\n\n  second para");
        assert_eq!(
            span("table", "Code"),
            "| Code | Meaning |\n| ---- | ------- |\n| 0    | Success |"
        );
        let indented = items.iter().filter(|i| i.kind == "code").nth(2).unwrap();
        assert_eq!(&MARKDOWN[indented.range.clone()], "    indented");
    }

    #[test]
    fn empty_names_are_quoted() {
        assert_eq!(selector("code", ""), "code:\"\"");
    }

    #[test]
    fn distance_counts_edits_and_transpositions() {
        assert_eq!(distance("parse", "parse"), 0);
        assert_eq!(distance("prase", "parse"), 1);
        assert_eq!(distance("pars", "parse"), 1);
        assert_eq!(distance("parse", "parser"), 1);
        assert_eq!(distance("new", "nwe"), 1);
        assert_eq!(distance("abc", "xyz"), 3);
        assert_eq!(distance("", "ab"), 2);
    }

    /// The text of `part` of the only item of `kind` named `name` in `text`.
    fn part_of<'t>(kind: &str, name: &str, p: Part, text: &'t str) -> Option<&'t str> {
        let found: Vec<Item> = items_in(text)
            .into_iter()
            .filter(|i| i.kind == kind && i.name == name)
            .collect();
        assert_eq!(found.len(), 1, "{found:?}");
        part(&found[0], p, text).map(|r| &text[r])
    }

    const PARTS: &str = r#"/// A parser.
/// Two lines.
#[derive(Debug)]
pub struct Parser {
    src: String,
}

impl Parser {
    /// Makes one.
    pub fn new(src: &str) -> Self {
        Parser { src }
    }

    fn inline(a: u8) { a }

    fn tall(
        a: u8,
        b: u8,
    ) -> u8 {
        a + b
    }

    fn empty() {}

    fn spaced() {  }

    fn open() {
    }
}

trait T {
    fn f(&self);
}

enum E {
    A { x: u8 },
    B,
}

const C: u8 = 1;
"#;

    #[test]
    fn whole_line_body_and_params() {
        assert_eq!(
            part_of("fn", "new", Part::Body, PARTS),
            Some("        Parser { src }\n")
        );
        assert_eq!(
            part_of("fn", "tall", Part::Params, PARTS),
            Some("        a: u8,\n        b: u8,\n")
        );
        assert_eq!(
            part_of("fn", "tall", Part::Body, PARTS),
            Some("        a + b\n")
        );
        assert_eq!(
            part_of("struct", "Parser", Part::Body, PARTS),
            Some("    src: String,\n")
        );
        assert!(
            part_of("impl", "Parser", Part::Body, PARTS)
                .unwrap()
                .starts_with("    /// Makes one.\n")
        );
        assert!(
            part_of("impl", "Parser", Part::Body, PARTS)
                .unwrap()
                .ends_with("    fn open() {\n    }\n")
        );
        assert_eq!(
            part_of("enum", "E", Part::Body, PARTS),
            Some("    A { x: u8 },\n    B,\n")
        );
        assert_eq!(
            part_of("trait", "T", Part::Body, PARTS),
            Some("    fn f(&self);\n")
        );
    }

    #[test]
    fn inline_body_and_params_are_trimmed() {
        assert_eq!(part_of("fn", "inline", Part::Body, PARTS), Some("a"));
        assert_eq!(part_of("fn", "inline", Part::Params, PARTS), Some("a: u8"));
        assert_eq!(part_of("fn", "new", Part::Params, PARTS), Some("src: &str"));
        assert_eq!(part_of("variant", "A", Part::Body, PARTS), Some("x: u8"));
    }

    #[test]
    fn empty_bodies_are_empty_spans() {
        let empty = |name| {
            let item = items_in(PARTS)
                .into_iter()
                .find(|i| i.name == name)
                .unwrap();
            let range = part(&item, Part::Body, PARTS).unwrap();
            assert!(range.is_empty(), "{name}: {range:?}");
            range.start
        };
        let inline = PARTS.find("fn empty() {}").unwrap() + "fn empty() {".len();
        assert_eq!(empty("empty"), inline);
        let spaced = PARTS.find("fn spaced() {  }").unwrap() + "fn spaced() {".len();
        assert!((spaced..=spaced + 2).contains(&empty("spaced")));
        let open = PARTS.find("fn open() {\n").unwrap() + "fn open() {\n".len();
        assert_eq!(empty("open"), open);
        assert_eq!(part_of("fn", "empty", Part::Params, PARTS), Some(""));
    }

    #[test]
    fn name_sig_and_doc() {
        assert_eq!(part_of("fn", "new", Part::Name, PARTS), Some("new"));
        assert_eq!(part_of("impl", "Parser", Part::Name, PARTS), Some("Parser"));
        assert_eq!(
            part_of("fn", "new", Part::Sig, PARTS),
            Some("pub fn new(src: &str) -> Self")
        );
        assert_eq!(
            part_of("fn", "tall", Part::Sig, PARTS),
            Some("fn tall(\n        a: u8,\n        b: u8,\n    ) -> u8")
        );
        assert_eq!(
            part_of("struct", "Parser", Part::Sig, PARTS),
            Some("pub struct Parser")
        );
        assert_eq!(part_of("fn", "f", Part::Sig, PARTS), Some("fn f(&self);"));
        assert_eq!(
            part_of("fn", "new", Part::Doc, PARTS),
            Some("    /// Makes one.\n")
        );
        assert_eq!(
            part_of("struct", "Parser", Part::Doc, PARTS),
            Some("/// A parser.\n/// Two lines.\n")
        );
    }

    #[test]
    fn missing_parts() {
        assert_eq!(part_of("const", "C", Part::Body, PARTS), None);
        assert_eq!(part_of("const", "C", Part::Doc, PARTS), None);
        assert_eq!(part_of("struct", "Parser", Part::Params, PARTS), None);
        assert_eq!(part_of("fn", "f", Part::Body, PARTS), None);
        assert_eq!(part_of("variant", "B", Part::Body, PARTS), None);
    }
}
