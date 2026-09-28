//! Syntax items found by each language's selector query (command-language
//! spec, §3.3).
//!
//! A query in `queries/<lang>/selectors.scm` captures each item node as its
//! kind (`@fn`, `@struct`, ...) and the item's name node as `@name`, in one
//! pattern. Standalone `@doc` and `@attr` patterns capture the doc comments
//! and attributes that extend an item's default span when they directly
//! precede it.

use std::cmp::Reverse;
use std::ops::Range;

use tree_sitter::{Node, Query, QueryCursor, StreamingIterator, Tree};

/// The core kinds a query may capture.
pub const KINDS: [&str; 14] = [
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
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub kind: &'static str,
    pub name: String,
    /// The default span: the item with its leading doc comments and
    /// attributes, and a `,` that directly follows it.
    pub range: Range<usize>,
    /// Whether `range` ends with a trailing `,`.
    pub trailing_comma: bool,
}

/// Every item `query` finds in `tree`, ordered by start, outer items first.
pub fn items(query: &Query, tree: &Tree, text: &str) -> Vec<Item> {
    let names = query.capture_names();
    // (kind, item node, name node) for each match, and every @doc/@attr node.
    let mut found: Vec<(&'static str, Node, Node)> = Vec::new();
    let mut leading: Vec<Range<usize>> = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), text.as_bytes());
    while let Some(m) = matches.next() {
        let (mut item, mut name) = (None, None);
        for capture in m.captures() {
            match names[capture.index as usize] {
                "name" => name = Some(capture.node),
                "doc" | "attr" => leading.push(capture.node.byte_range()),
                other => {
                    if let Some(kind) = KINDS.iter().find(|&&k| k == other) {
                        item = Some((*kind, capture.node));
                    }
                }
            }
        }
        if let (Some((kind, node)), Some(name)) = (item, name) {
            found.push((kind, node, name));
        }
    }
    leading.sort_by_key(|r| r.start);

    // A wrapper and the node it wraps can both match; keep the widest.
    found.sort_by_key(|(kind, node, name)| {
        (
            *kind,
            name.start_byte(),
            node.start_byte(),
            Reverse(node.end_byte()),
        )
    });
    found.dedup_by(|b, a| a.0 == b.0 && a.2 == b.2);

    let mut items: Vec<Item> = found
        .into_iter()
        .map(|(kind, node, name)| {
            let mut range = node.byte_range();
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
            while let Some(doc) = leading
                .iter()
                .rev()
                .find(|l| l.end <= range.start && directly_before(text, l, range.start))
            {
                range.start = doc.start;
            }
            Item {
                kind,
                name: text[name.byte_range()].to_string(),
                range,
                trailing_comma: comma.is_some(),
            }
        })
        .collect();
    items.sort_by_key(|i| (i.range.start, Reverse(i.range.end)));
    items
}

/// Whether only whitespace, and no blank line, separates `leading` from
/// `start`.
fn directly_before(text: &str, leading: &Range<usize>, start: usize) -> bool {
    let gap = &text[leading.end..start];
    let newlines = gap.matches('\n').count() + usize::from(text[..leading.end].ends_with('\n'));
    gap.trim().is_empty() && newlines <= 1
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
            ("impl", "Wrapper"),
            ("type", "Output"),
            ("fn", "parse"),
            ("impl", "Parser"),
            ("fn", "new"),
            ("var", "pos"),
            ("var", "len"),
            ("impl", "Token"),
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
}
