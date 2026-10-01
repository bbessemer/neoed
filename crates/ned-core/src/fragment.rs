//! Parsing code fragments (command-language spec, §3.10). tree-sitter always
//! parses from a grammar's start rule, so a fragment that only parses inside
//! other code (a method, a match arm, a field) is also tried inside each of
//! its language's builders, `queries/<lang>/builders.scm`.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use ned_scheme::{Datum, Syntax};
use serde::Deserialize;
use tree_sitter::{Node, Tree};

use crate::lang::Language;
use crate::template::Template;

/// How to build a node of `kind` around a fragment: `(build KIND PART...)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Builder {
    pub kind: String,
    pub parts: Vec<Part>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    /// A string: literal text.
    Text(String),
    /// A symbol other than `_`: one of the kind's fields.
    Field(String),
    /// `_`, where the fragment goes.
    Hole,
}

/// A malformed builders file.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct BuilderError {
    pub message: String,
    pub span: Range<usize>,
}

/// What a grammar's `node-types.json` says each named kind can hold.
#[derive(Debug, Default)]
pub struct NodeTypes {
    fields: HashMap<String, HashSet<String>>,
    contains: HashMap<String, HashSet<String>>,
}

/// One reading of a fragment: the program it parsed in, and where the
/// fragment and its holes are in it.
#[derive(Debug)]
pub struct Fragment {
    pub text: String,
    pub tree: Tree,
    /// The byte range of the fragment's root node, or of its run of root
    /// nodes.
    pub roots: Range<usize>,
    /// Each hole's range in `text`, or `None` where it's literal text (in a
    /// string or comment).
    pub holes: Vec<Option<Range<usize>>>,
    /// The index of the builder it parsed in, or `None` if it parsed alone.
    pub builder: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FragmentError {
    #[error("the pattern doesn't parse as {lang}; add the code around it, or use query{{}}")]
    NoParse {
        lang: Language,
        /// Where parsing failed, in the template's text.
        at: usize,
    },
    #[error("a placeholder must stand for a whole node; write `@@` for a literal `@`")]
    Fused {
        /// The placeholder, in the template's text.
        span: Range<usize>,
    },
}

impl Builder {
    /// Reads a builders file, checking its kinds and fields against `types`.
    pub fn read_all(src: &str, types: &NodeTypes) -> Result<Vec<Builder>, BuilderError> {
        let data = ned_scheme::read_all(src).map_err(|e| BuilderError {
            message: e.to_string(),
            span: e.span,
        })?;
        data.iter().map(|s| Builder::read(s, types)).collect()
    }

    fn read(form: &Syntax, types: &NodeTypes) -> Result<Builder, BuilderError> {
        let error = |message: String, span: &Range<usize>| BuilderError {
            message,
            span: span.clone(),
        };
        let shape = || error("expected (build KIND PART...)".into(), &form.span);
        let Datum::List(items) = &form.datum else {
            return Err(shape());
        };
        let [head, kind, parts @ ..] = items.as_slice() else {
            return Err(shape());
        };
        let (Datum::Symbol(head), Datum::Symbol(kind_name)) = (&head.datum, &kind.datum) else {
            return Err(shape());
        };
        if head != "build" {
            return Err(shape());
        }
        if !types.has_kind(kind_name) {
            let message = format!(
                "unknown kind `{kind_name}`; use a kind from the grammar's node-types.json"
            );
            return Err(error(message, &kind.span));
        }
        let mut holes = 0;
        let parts = parts
            .iter()
            .map(|part| match &part.datum {
                Datum::Str(s) => Ok(Part::Text(s.clone())),
                Datum::Symbol(s) if s == "_" => {
                    holes += 1;
                    if holes > 1 {
                        return Err(error(
                            "a builder needs one `_`; delete this one".into(),
                            &part.span,
                        ));
                    }
                    Ok(Part::Hole)
                }
                Datum::Symbol(field) if types.has_field(kind_name, field) => {
                    Ok(Part::Field(field.clone()))
                }
                Datum::Symbol(field) => {
                    let message =
                        format!("{kind_name} has no field `{field}`; write its text as a string");
                    Err(error(message, &part.span))
                }
                _ => Err(error("a part is a string or a symbol".into(), &part.span)),
            })
            .collect::<Result<Vec<_>, _>>()?;
        if holes == 0 {
            return Err(error(
                "a builder needs one `_`, where the fragment goes".into(),
                &form.span,
            ));
        }
        Ok(Builder {
            kind: kind_name.clone(),
            parts,
        })
    }

    /// The text before and after the hole, with each field a dummy name.
    fn text(&self) -> (String, String) {
        let (mut before, mut after) = (String::new(), String::new());
        let mut out = &mut before;
        for part in &self.parts {
            match part {
                Part::Text(s) => out.push_str(s),
                Part::Field(field) => out.push_str(&format!("__ned_{field}")),
                Part::Hole => out = &mut after,
            }
        }
        (before, after)
    }
}

#[derive(Deserialize)]
struct Entry {
    #[serde(rename = "type")]
    kind: String,
    named: bool,
    #[serde(default)]
    fields: HashMap<String, Children>,
    children: Option<Children>,
    #[serde(default)]
    subtypes: Vec<TypeRef>,
}

#[derive(Deserialize)]
struct Children {
    types: Vec<TypeRef>,
}

#[derive(Deserialize)]
struct TypeRef {
    #[serde(rename = "type")]
    kind: String,
    named: bool,
}

impl NodeTypes {
    pub fn read(json: &str) -> NodeTypes {
        let entries: Vec<Entry> = serde_json::from_str(json).expect("node-types.json is valid");
        let named = |types: &[TypeRef]| -> Vec<String> {
            types
                .iter()
                .filter(|t| t.named)
                .map(|t| t.kind.clone())
                .collect()
        };
        let mut subtypes = HashMap::new();
        let mut direct = HashMap::new();
        let mut fields = HashMap::new();
        for e in entries.iter().filter(|e| e.named) {
            subtypes.insert(e.kind.clone(), named(&e.subtypes));
            let children = e
                .fields
                .values()
                .chain(&e.children)
                .flat_map(|c| named(&c.types));
            direct.insert(e.kind.clone(), children.collect::<Vec<_>>());
            fields.insert(e.kind.clone(), e.fields.keys().cloned().collect());
        }
        // Each supertype stands for its subtypes, and theirs.
        let expand = |kinds: &[String]| {
            let mut out = HashSet::new();
            let mut todo = kinds.to_vec();
            while let Some(kind) = todo.pop() {
                if let Some(subs) = subtypes.get(&kind) {
                    todo.extend(subs.iter().cloned());
                }
                out.insert(kind);
            }
            out
        };
        let contains = direct
            .iter()
            .map(|(k, children)| (k.clone(), expand(children)))
            .collect();
        NodeTypes { fields, contains }
    }

    pub fn has_kind(&self, kind: &str) -> bool {
        self.fields.contains_key(kind)
    }

    pub fn has_field(&self, kind: &str, field: &str) -> bool {
        self.fields.get(kind).is_some_and(|f| f.contains(field))
    }

    /// Whether a `parent` node can have a named `child` node, as a field or
    /// child, through any supertype.
    pub fn can_contain(&self, parent: &str, child: &str) -> bool {
        self.contains.get(parent).is_some_and(|c| c.contains(child))
    }
}

impl Fragment {
    /// The fragment's root node, or its run of sibling root nodes.
    pub fn roots(&self) -> Vec<Node<'_>> {
        let r = &self.roots;
        let node = self.spanning(r);
        if self.span(node) == *r && node.is_named() {
            return vec![node];
        }
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .filter(|c| !c.is_extra() && c.start_byte() >= r.start && self.span(*c).end <= r.end)
            .collect()
    }

    /// Hole `i`'s node: the outermost node spanning exactly its placeholder,
    /// within the roots. `None` if the hole is literal.
    pub fn hole(&self, i: usize) -> Option<Node<'_>> {
        let range = self.holes[i].as_ref()?;
        let container = self.container().map(|c| c.id());
        let mut node = self.spanning(range);
        while let Some(parent) = node.parent() {
            if self.span(parent) != *range || Some(parent.id()) == container {
                break;
            }
            node = parent;
        }
        Some(node)
    }

    /// The node the roots are children of.
    fn container(&self) -> Option<Node<'_>> {
        let node = self.spanning(&self.roots);
        if self.span(node) == self.roots && node.is_named() {
            node.parent()
        } else {
            Some(node)
        }
    }

    fn spanning(&self, r: &Range<usize>) -> Node<'_> {
        self.tree
            .root_node()
            .descendant_for_byte_range(r.start, r.end)
            .expect("ranges lie within the text")
    }

    /// The node's range without the whitespace at its ends: some grammars end
    /// a statement with its newline (Go).
    fn span(&self, node: Node<'_>) -> Range<usize> {
        let r = node.byte_range();
        let text = &self.text[r.clone()];
        let start = r.start + (text.len() - text.trim_start().len());
        start..(start + text.trim().len()).max(start)
    }
}

/// Every reading of `template` in `lang` that parses without errors: alone,
/// then inside each builder. Readings with the same root kinds count once.
pub fn parse(lang: Language, template: &Template) -> Result<Vec<Fragment>, FragmentError> {
    let wraps = std::iter::once(None).chain(lang.builders().iter().enumerate().map(Some));
    let mut readings: Vec<Fragment> = Vec::new();
    let mut fused = None;
    for wrap in wraps {
        match reading(lang, template, wrap) {
            Ok(Some(f)) => {
                let kinds =
                    |f: &Fragment| f.roots().iter().map(|n| n.kind_id()).collect::<Vec<_>>();
                if !readings.iter().any(|r| kinds(r) == kinds(&f)) {
                    readings.push(f);
                }
            }
            Ok(None) => {}
            Err(e) => fused = fused.or(Some(e)),
        }
    }
    if !readings.is_empty() {
        return Ok(readings);
    }
    if let Some(e) = fused {
        return Err(e);
    }
    let bare = template.source(|_| false);
    let tree = lang.parse(&bare.text);
    let at = first_error(tree.root_node()).map_or(0, |n| n.start_byte());
    Err(FragmentError::NoParse {
        lang,
        at: bare.to_template(at),
    })
}

/// The reading of `template` alone or inside the builder `wrap`, if it parses
/// without errors and its roots are whole nodes the context can contain.
fn reading(
    lang: Language,
    template: &Template,
    wrap: Option<(usize, &Builder)>,
) -> Result<Option<Fragment>, FragmentError> {
    let Some(f) = build(lang, template, wrap, &HashSet::new()) else {
        return Ok(None);
    };
    let partial: HashSet<usize> = (0..f.holes.len())
        .filter(|&i| {
            f.holes[i]
                .as_ref()
                .is_some_and(|r| f.span(f.spanning(r)) != *r)
        })
        .collect();
    if partial.is_empty() {
        return Ok(Some(f));
    }
    // Holes inside a string or comment keep their text, which leaves the
    // tree's shape unchanged; any other hole that isn't a whole node is
    // fused into a neighbouring token.
    match build(lang, template, wrap, &partial) {
        Some(literal) if shape(&literal.tree) == shape(&f.tree) => Ok(Some(literal)),
        _ => {
            let i = *partial.iter().min().expect("not empty");
            let span = template
                .holes()
                .nth(i)
                .expect("a hole per range")
                .span
                .clone();
            Err(FragmentError::Fused { span })
        }
    }
}

/// Parses `template` inside `wrap`, with the holes in `literal` keeping their
/// text. `None` if it has errors, or its roots aren't whole nodes that the
/// context can contain.
fn build(
    lang: Language,
    template: &Template,
    wrap: Option<(usize, &Builder)>,
    literal: &HashSet<usize>,
) -> Option<Fragment> {
    let source = template.source(|i| literal.contains(&i));
    // Alone, the fragment ends with a newline: Go ends a statement with one.
    let (before, after) = wrap.map_or_else(|| (String::new(), "\n".into()), |(_, b)| b.text());
    // Later lines of the fragment take the indentation the prefix ends at.
    let last_line = &before[before.rfind('\n').map_or(0, |i| i + 1)..];
    let indent = &last_line[..last_line.len() - last_line.trim_start().len()];
    let at = |offset: usize| {
        before.len() + offset + indent.len() * source.text[..offset].matches('\n').count()
    };
    let text = format!(
        "{before}{}{after}",
        source.text.replace('\n', &format!("\n{indent}"))
    );
    let tree = lang.parse(&text);
    if tree.root_node().has_error() {
        return None;
    }
    let start = source.text.len() - source.text.trim_start().len();
    let end = source.text.trim_end().len();
    if start >= end {
        return None;
    }
    let holes = source
        .holes
        .iter()
        .enumerate()
        .map(|(i, r)| (!literal.contains(&i)).then(|| at(r.start)..at(r.end)))
        .collect();
    let f = Fragment {
        text,
        tree,
        roots: at(start)..at(end),
        holes,
        builder: wrap.map(|(i, _)| i),
    };
    let node = f.spanning(&f.roots);
    if f.span(node) != f.roots && !covers(&f, node) {
        return None;
    }
    let roots = f.roots();
    if roots.is_empty() {
        return None;
    }
    let types = lang.node_types();
    let container = f.container()?.kind();
    if !roots.iter().all(|r| types.can_contain(container, r.kind())) {
        return None;
    }
    Some(f)
}

/// Whether the fragment's roots start at one of `node`'s children and end at
/// one, a run of whole children. Its ends may be tokens, such as a trailing
/// `,`.
fn covers(f: &Fragment, node: Node<'_>) -> bool {
    let range = &f.roots;
    let mut cursor = node.walk();
    let children: Vec<_> = node
        .children(&mut cursor)
        .filter(|c| !c.is_extra())
        .map(|c| f.span(c))
        .collect();
    let straddles = |at: usize| children.iter().any(|c| c.start < at && at < c.end);
    children.iter().any(|c| c.start == range.start)
        && children.iter().any(|c| c.end == range.end)
        && !straddles(range.start)
        && !straddles(range.end)
}

/// The tree's node kinds, in order.
fn shape(tree: &Tree) -> Vec<u16> {
    let mut out = Vec::new();
    let mut cursor = tree.walk();
    'walk: loop {
        out.push(cursor.node().kind_id());
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                break 'walk;
            }
        }
    }
    out
}

fn first_error(node: Node<'_>) -> Option<Node<'_>> {
    if node.is_error() || node.is_missing() {
        return Some(node);
    }
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(|c| c.has_error())
        .find_map(first_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn readings(lang: Language, src: &str) -> Vec<Fragment> {
        parse(lang, &Template::parse(src)).unwrap_or_else(|e| panic!("{src}: {e}"))
    }

    /// The kinds of each reading's roots.
    fn root_kinds(lang: Language, src: &str) -> Vec<Vec<String>> {
        readings(lang, src)
            .iter()
            .map(|f| f.roots().iter().map(|n| n.kind().to_string()).collect())
            .collect()
    }

    fn has_reading(lang: Language, src: &str, kind: &str) {
        let kinds = root_kinds(lang, src);
        assert!(
            kinds.iter().any(|k| k == &[kind]),
            "{src}: no {kind} reading in {kinds:?}"
        );
    }

    fn only_reading(lang: Language, src: &str, kind: &str) {
        assert_eq!(root_kinds(lang, src), [[kind]], "{src}");
    }

    fn hole_kind(f: &Fragment, i: usize) -> Option<&str> {
        f.hole(i).map(|n| n.kind())
    }

    #[test]
    fn rust_fragments() {
        has_reading(Language::Rust, "foo(@a)", "call_expression");
        only_reading(
            Language::Rust,
            "fn new(&self) -> Self { @body... }",
            "function_item",
        );
        has_reading(Language::Rust, "let @x = @e;", "let_declaration");
        has_reading(Language::Rust, "Some(@x) => @y,", "match_arm");
        has_reading(Language::Rust, "Leaf(u32)", "enum_variant");
        has_reading(Language::Rust, "Vec<@t>", "generic_type");
        has_reading(Language::Rust, "dyn Fn(@a) -> @r", "dynamic_type");
        has_reading(Language::Rust, "T: Clone", "type_parameter");
        has_reading(
            Language::Rust,
            "fn len(&self) -> usize;",
            "function_signature_item",
        );
    }

    #[test]
    fn rust_fragments_with_several_readings() {
        has_reading(Language::Rust, "@a: u32", "field_declaration");
        has_reading(Language::Rust, "@a: u32", "parameter");
    }

    #[test]
    fn python_fragments() {
        only_reading(
            Language::Python,
            "def start(self):\n    pass",
            "function_definition",
        );
        has_reading(Language::Python, "case [@x]:\n    pass", "case_clause");
        has_reading(Language::Python, "\"a\": @v", "pair");
        has_reading(Language::Python, "@x: int", "typed_parameter");
        has_reading(Language::Python, "@x = 1", "assignment");
    }

    #[test]
    fn go_fragments() {
        has_reading(Language::Go, "x := @v", "short_var_declaration");
        has_reading(Language::Go, "Name string", "field_declaration");
        has_reading(Language::Go, "Close() error", "method_elem");
        has_reading(Language::Go, "case 1:\n\t@x", "expression_case");
        has_reading(Language::Go, "ctx context.Context", "parameter_declaration");
    }

    #[test]
    fn javascript_fragments() {
        has_reading(
            Language::JavaScript,
            "render() { return @x; }",
            "method_definition",
        );
        has_reading(Language::JavaScript, "a: @v", "pair");
        has_reading(Language::JavaScript, "a: @v", "labeled_statement");
        has_reading(Language::JavaScript, "case 1: @x;", "switch_case");
        has_reading(Language::JavaScript, "foo(@a)", "call_expression");
    }

    #[test]
    fn typescript_fragments() {
        has_reading(Language::TypeScript, "name: string;", "property_signature");
        has_reading(Language::TypeScript, "@a | @b", "union_type");
        has_reading(
            Language::Tsx,
            "render() { return @x; }",
            "method_definition",
        );
    }

    #[test]
    fn several_roots() {
        let f = &readings(Language::Rust, "let a = 1;\nlet b = 2;")[0];
        let kinds: Vec<_> = f.roots().iter().map(|n| n.kind()).collect();
        assert_eq!(kinds, ["let_declaration", "let_declaration"]);
        assert_eq!(&f.text[f.roots.clone()], "let a = 1;\nlet b = 2;");
    }

    #[test]
    fn holes_are_whole_nodes() {
        let f = &readings(Language::Rust, "foo(@a, @rest...)")[0];
        assert_eq!(hole_kind(f, 0), Some("identifier"));
        assert_eq!(&f.text[f.holes[0].clone().unwrap()], "__ned_a");
        assert_eq!(&f.text[f.holes[1].clone().unwrap()], "__ned_rest");
    }

    #[test]
    fn a_hole_is_its_outermost_node_within_the_roots() {
        let f = &readings(Language::Python, "if @c:\n    @body")[0];
        assert_eq!(f.roots()[0].kind(), "if_statement");
        assert_eq!(hole_kind(f, 0), Some("identifier"));
        assert_eq!(hole_kind(f, 1), Some("block"));
        let only = &readings(Language::Python, "@x")[0];
        assert_eq!(hole_kind(only, 0), Some("identifier"));
    }

    #[test]
    fn holes_in_strings_and_comments_are_literal() {
        let f = &readings(Language::Rust, "log(\"user@host\", /* @x */ @y)")[0];
        assert_eq!(f.holes[0], None);
        assert_eq!(f.holes[1], None);
        assert_eq!(hole_kind(f, 2), Some("identifier"));
        assert!(
            f.text.contains("\"user@host\", /* @x */ __ned_y"),
            "{}",
            f.text
        );
    }

    #[test]
    fn a_hole_fused_into_a_name_is_an_error() {
        let template = Template::parse("foo@bar(1)");
        assert_eq!(
            parse(Language::Rust, &template).unwrap_err(),
            FragmentError::Fused { span: 3..7 }
        );
    }

    #[test]
    fn a_fragment_that_never_parses_is_an_error() {
        let template = Template::parse("fn (@a");
        let Err(FragmentError::NoParse { lang, at }) = parse(Language::Rust, &template) else {
            panic!("parsed");
        };
        assert_eq!(lang, Language::Rust);
        assert!(at <= 6, "at {at}");
    }

    #[test]
    fn readings_with_the_same_root_kinds_count_once() {
        // Alone, and inside an impl: both are a function_item.
        assert_eq!(root_kinds(Language::Rust, "fn f(&self) {}").len(), 1);
    }

    /// One fragment per builder that parses only inside it.
    const BUILDER_SAMPLES: &[(Language, &str)] = &[
        (Language::Rust, "return @x"),
        (Language::Rust, "Some(@x) => @y,"),
        (Language::Rust, "pub name: String,"),
        (Language::Rust, "Leaf(u32),"),
        (Language::Rust, "a: u32"),
        (Language::Rust, "T: Clone"),
        (Language::Python, "def start(self):\n    return @x"),
        (Language::Python, "x: int = 0, *args"),
        (Language::Python, "case [@x]:\n    pass"),
        (Language::Python, "\"a\": @v"),
        (Language::Go, "Name string"),
        (Language::Go, "Close() error"),
        (Language::Go, "case 1:\n\t@x"),
        (Language::Go, "ctx context.Context"),
        (Language::JavaScript, "render() { return @x; }"),
        (Language::JavaScript, "a: @v, b"),
        (Language::JavaScript, "a, ...rest"),
        (Language::JavaScript, "case 1: @x;"),
        (Language::TypeScript, "render(): void {}"),
        (Language::TypeScript, "a: @v, b"),
        (Language::TypeScript, "a: string, ...rest"),
        (Language::TypeScript, "case 1: @x;"),
        (Language::TypeScript, "name: string;"),
        (Language::TypeScript, "@a | @b"),
    ];

    #[test]
    fn every_builder_builds_its_kind() {
        let mut used = HashSet::new();
        for &(lang, src) in BUILDER_SAMPLES {
            for f in readings(lang, src) {
                let Some(i) = f.builder else { continue };
                used.insert((lang, i));
                let builder = &lang.builders()[i];
                let root = f.roots()[0];
                // The built kind encloses the fragment, with each field's
                // dummy name in that field.
                let built = std::iter::successors(Some(root), |n| n.parent())
                    .find(|n| n.kind() == builder.kind)
                    .unwrap_or_else(|| panic!("{src}: no {} around it", builder.kind));
                for part in &builder.parts {
                    if let Part::Field(field) = part {
                        let node = built
                            .child_by_field_name(field)
                            .unwrap_or_else(|| panic!("{src}: {} has no {field}", builder.kind));
                        let text = &f.text[node.byte_range()];
                        assert!(
                            text.contains(&format!("__ned_{field}")),
                            "{src}: {field} is {text}"
                        );
                    }
                }
            }
        }
        for lang in [
            Language::Rust,
            Language::Python,
            Language::Go,
            Language::JavaScript,
            Language::TypeScript,
        ] {
            for (i, builder) in lang.builders().iter().enumerate() {
                assert!(
                    used.contains(&(lang, i)),
                    "{lang} builder {i} ({}) has no sample",
                    builder.kind
                );
            }
        }
    }

    #[test]
    fn builders_are_read() {
        let types = Language::Rust.node_types();
        assert_eq!(
            Builder::read_all(
                "; impls\n(build impl_item \"impl \" type \" {\" _ \"}\")",
                types
            ),
            Ok(vec![Builder {
                kind: "impl_item".into(),
                parts: vec![
                    Part::Text("impl ".into()),
                    Part::Field("type".into()),
                    Part::Text(" {".into()),
                    Part::Hole,
                    Part::Text("}".into()),
                ],
            }])
        );
    }

    #[test]
    fn malformed_builders_are_errors() {
        let types = Language::Rust.node_types();
        for (src, message, span) in [
            ("x", "expected (build KIND PART...)", 0..1),
            ("(make impl_item _)", "expected (build KIND PART...)", 0..18),
            ("(build)", "expected (build KIND PART...)", 0..7),
            (
                "(build no_such_kind _)",
                "unknown kind `no_such_kind`",
                7..19,
            ),
            ("(build impl_item \"x\")", "a builder needs one `_`", 0..21),
            ("(build impl_item _ _)", "a builder needs one `_`", 19..20),
            (
                "(build impl_item nope _)",
                "impl_item has no field `nope`",
                17..21,
            ),
            (
                "(build impl_item 1 _)",
                "a part is a string or a symbol",
                17..18,
            ),
            (
                "(build impl_item",
                "unterminated `(`; close it with `)`",
                0..1,
            ),
        ] {
            let err = Builder::read_all(src, types).unwrap_err();
            assert!(err.message.starts_with(message), "{src}: {}", err.message);
            assert_eq!(err.span, span, "{src}");
        }
    }

    #[test]
    fn node_types() {
        let types = Language::Rust.node_types();
        assert!(types.has_kind("function_item"));
        assert!(!types.has_kind("no_such_kind"));
        assert!(types.has_field("function_item", "name"));
        assert!(!types.has_field("function_item", "nope"));
        assert!(types.can_contain("declaration_list", "function_item"));
        // Through the `_expression` supertype.
        assert!(types.can_contain("block", "call_expression"));
        assert!(types.can_contain("let_declaration", "call_expression"));
        assert!(!types.can_contain("declaration_list", "match_arm"));
    }
}
