//! Parsing code fragments (command-language spec, §3.10). tree-sitter always
//! parses from a grammar's start rule, so a fragment that only parses inside
//! other code (a method, a match arm, a field) is also tried inside each of
//! its language's builders, `queries/<lang>/builders.scm`.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

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
    /// Reads a builders file, checking its fields against `types`.
    pub fn read_all(_src: &str, _types: &NodeTypes) -> Result<Vec<Builder>, BuilderError> {
        unimplemented!()
    }
}

impl NodeTypes {
    pub fn read(_json: &str) -> NodeTypes {
        unimplemented!()
    }

    pub fn has_kind(&self, _kind: &str) -> bool {
        unimplemented!()
    }

    pub fn has_field(&self, _kind: &str, _field: &str) -> bool {
        unimplemented!()
    }

    /// Whether a `parent` node can have a named `child` node, as a field or
    /// child, through any supertype.
    pub fn can_contain(&self, _parent: &str, _child: &str) -> bool {
        unimplemented!()
    }
}

impl Fragment {
    /// The fragment's root node, or its run of sibling root nodes.
    pub fn roots(&self) -> Vec<Node<'_>> {
        unimplemented!()
    }

    /// Hole `i`'s node: the outermost node spanning exactly its placeholder,
    /// within the roots. `None` if the hole is literal.
    pub fn hole(&self, _i: usize) -> Option<Node<'_>> {
        unimplemented!()
    }
}

/// Every reading of `template` in `lang` that parses without errors: alone,
/// then inside each builder. Readings with the same root kinds count once.
pub fn parse(_lang: Language, _template: &Template) -> Result<Vec<Fragment>, FragmentError> {
    unimplemented!()
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
        has_reading(Language::Rust, "T: Clone", "constrained_type_parameter");
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
        (Language::Rust, "fn len(&self) -> usize;"),
        (Language::Rust, "Some(@x) => @y,"),
        (Language::Rust, "pub name: String,"),
        (Language::Rust, "Leaf(u32),"),
        (Language::Rust, "a: u32"),
        (Language::Rust, "T: Clone"),
        (Language::Rust, "Vec<@t>"),
        (Language::Python, "def start(self):\n    return @x"),
        (Language::Python, "x: int = 0, *args"),
        (Language::Python, "case [@x]:\n    pass"),
        (Language::Python, "\"a\": @v"),
        (Language::Go, "Name string"),
        (Language::Go, "Close() error"),
        (Language::Go, "case 1:\n\t@x"),
        (Language::Go, "ctx context.Context"),
        (Language::Go, "return @x"),
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
