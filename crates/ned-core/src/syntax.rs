//! Syntax items found by each language's selector query (command-language
//! spec, §3.3).
//!
//! A query in `queries/<lang>/selectors.scm` captures each item node as its
//! kind (`@fn`, `@struct`, ...) and the item's name node as `@name`, in one
//! pattern. Standalone `@doc` and `@attr` patterns capture the doc comments
//! and attributes that extend an item's default span when they directly
//! precede it.

use std::ops::Range;

use tree_sitter::{Query, Tree};

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
    let _ = (query, tree, text);
    Vec::new()
}

/// The kinds `query` captures, in `KINDS` order.
pub fn kinds(query: &Query) -> Vec<&'static str> {
    let _ = query;
    Vec::new()
}

/// Whether `name` matches `pattern`, where `*` matches any run of characters.
pub fn name_matches(pattern: &str, name: &str) -> bool {
    pattern == name
}

/// `kind:name`, with the name quoted when it has characters a bare name can't.
pub fn selector(kind: &str, name: &str) -> String {
    format!("{kind}:{name}")
}

/// The optimal string alignment distance between `a` and `b`: edits are
/// insertions, deletions, substitutions and adjacent transpositions.
pub fn distance(a: &str, b: &str) -> usize {
    let _ = (a, b);
    0
}
