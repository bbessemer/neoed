//! Abstract syntax trees (command-language spec, §3.10): a syntax tree without
//! what doesn't change what the code means. Comments and tokens of only
//! whitespace go everywhere; `queries/<lang>/ast.scm` marks each language's
//! separators with `@skip`, and those that stay anyway with `@keep`.

use std::collections::HashSet;
use std::fmt;
use std::ops::Range;

use tree_sitter::{Node, QueryCursor, StreamingIterator};

use crate::lang::Language;

/// A node of an abstract syntax tree.
#[derive(Debug, Clone)]
pub struct Ast {
    pub kind: String,
    /// Whether it's a named node, not a token such as `+`.
    pub named: bool,
    /// Its field in its parent.
    pub field: Option<String>,
    pub body: Body,
    /// Its source, without whitespace at its ends.
    pub range: Range<usize>,
    /// Its source with the comments next to it, as a capture keeps them.
    pub outer: Range<usize>,
}

#[derive(Debug, Clone)]
pub enum Body {
    /// A node without children: its text.
    Leaf(String),
    Node(Vec<Ast>),
    /// A pattern's placeholder (§3.10): `None` for `@_`; `many` for a run.
    Hole {
        name: Option<String>,
        many: bool,
    },
}

impl Ast {
    /// The tree of `node`, a node of `lang`'s tree of `text`, without its
    /// descendants that lie outside `range`. `None` if `node` itself is left
    /// out.
    pub fn lower(lang: Language, node: Node<'_>, text: &str, range: Range<usize>) -> Option<Ast> {
        let query = lang.ast();
        let (mut skip, mut keep) = (HashSet::new(), HashSet::new());
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(range.clone());
        let mut matches = cursor.matches(query, node, text.as_bytes());
        while let Some(m) = matches.next() {
            for c in m.captures() {
                match query.capture_names()[c.index as usize] {
                    "keep" => keep.insert(c.node.id()),
                    _ => skip.insert(c.node.id()),
                };
            }
        }
        skip.retain(|id| !keep.contains(id));
        let outer = trim(text, node.byte_range());
        Lowering { text, range, skip }.node(node, None, outer)
    }

    /// Whether the two trees are the same code, wherever they are.
    pub fn same_code(&self, other: &Ast) -> bool {
        let children = |a: &[Ast], b: &[Ast]| {
            a.len() == b.len()
                && a.iter()
                    .zip(b)
                    .all(|(x, y)| x.field == y.field && x.same_code(y))
        };
        self.kind == other.kind
            && self.named == other.named
            && match (&self.body, &other.body) {
                (Body::Leaf(a), Body::Leaf(b)) => a == b,
                (Body::Node(a), Body::Node(b)) => children(a, b),
                (Body::Hole { name: a, many: m }, Body::Hole { name: b, many: n }) => {
                    a == b && m == n
                }
                _ => false,
            }
    }
}

/// The tree as a tree-sitter query would write it:
/// `(call_expression function: (identifier "foo") arguments: (arguments "(" ")"))`.
impl fmt::Display for Ast {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(field) = &self.field {
            write!(f, "{field}: ")?;
        }
        match &self.body {
            Body::Hole { name, many } => {
                write!(f, "@{}", name.as_deref().unwrap_or("_"))?;
                if *many {
                    f.write_str("...")?;
                }
                Ok(())
            }
            Body::Leaf(text) if !self.named => quoted(f, text),
            Body::Leaf(text) => {
                write!(f, "({} ", self.kind)?;
                quoted(f, text)?;
                f.write_str(")")
            }
            Body::Node(children) => {
                f.write_str("(")?;
                f.write_str(&self.kind)?;
                for c in children {
                    write!(f, " {c}")?;
                }
                f.write_str(")")
            }
        }
    }
}

/// Lowers the nodes of a tree that lie within `range`, leaving out `skip`.
struct Lowering<'t> {
    text: &'t str,
    range: Range<usize>,
    skip: HashSet<usize>,
}

impl Lowering<'_> {
    /// The tree of `node`, `field` in its parent, whose source with its
    /// comments is `outer`.
    fn node(&self, node: Node<'_>, field: Option<String>, outer: Range<usize>) -> Option<Ast> {
        let r = node.byte_range();
        let whitespace = !node.is_named() && self.text[r.clone()].trim().is_empty();
        if r.end <= self.range.start
            || r.start >= self.range.end
            || node.is_extra()
            || whitespace
            || self.skip.contains(&node.id())
        {
            return None;
        }
        let body = if node.child_count() == 0 {
            Body::Leaf(self.text[r.clone()].to_string())
        } else {
            Body::Node(self.children(node))
        };
        Some(Ast {
            kind: node.kind().to_string(),
            named: node.is_named(),
            field,
            body,
            range: trim(self.text, r),
            outer,
        })
    }

    /// `node`'s children, with the text between them that no node holds, as
    /// between a Python string's escape sequences, as tokens of its own.
    fn children(&self, node: Node<'_>) -> Vec<Ast> {
        let mut all = Vec::new();
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                all.push((cursor.node(), cursor.field_name().map(String::from)));
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        let comment = |i: &usize| all[*i].0.is_extra();
        let mut out = Vec::new();
        let mut at = node.start_byte();
        for i in 0..all.len() {
            let (child, field) = all[i].clone();
            out.extend(self.text_between(at..child.start_byte()));
            at = child.end_byte();
            let first = (0..i).rev().take_while(comment).last().unwrap_or(i);
            let last = (i + 1..all.len()).take_while(comment).last().unwrap_or(i);
            let start = trim(self.text, all[first].0.byte_range()).start;
            let end = trim(self.text, all[last].0.byte_range()).end;
            out.extend(self.node(child, field, start..end.max(start)));
        }
        out.extend(self.text_between(at..node.end_byte()));
        out
    }

    /// The text in `r`, which no node holds, as a token, unless it's only
    /// whitespace or lies outside the range.
    fn text_between(&self, r: Range<usize>) -> Option<Ast> {
        let text = &self.text[r.clone()];
        if text.trim().is_empty() || r.end <= self.range.start || r.start >= self.range.end {
            return None;
        }
        Some(Ast {
            kind: text.to_string(),
            named: false,
            field: None,
            body: Body::Leaf(text.to_string()),
            range: r.clone(),
            outer: r,
        })
    }
}

/// `range` of `text` without the whitespace at its ends: Go ends a statement
/// with its line break.
fn trim(text: &str, range: Range<usize>) -> Range<usize> {
    let s = &text[range.clone()];
    let start = range.start + (s.len() - s.trim_start().len());
    start..start + s.trim().len()
}

/// `text` as a string literal.
fn quoted(f: &mut fmt::Formatter<'_>, text: &str) -> fmt::Result {
    f.write_str("\"")?;
    for c in text.chars() {
        match c {
            '"' => f.write_str("\\\"")?,
            '\\' => f.write_str("\\\\")?,
            '\n' => f.write_str("\\n")?,
            '\t' => f.write_str("\\t")?,
            '\r' => f.write_str("\\r")?,
            c => write!(f, "{c}")?,
        }
    }
    f.write_str("\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lower_in(lang: Language, code: &str, range: Range<usize>) -> Ast {
        let tree = lang.parse(code);
        Ast::lower(lang, tree.root_node(), code, range).expect("the file is kept")
    }

    fn lower(lang: Language, code: &str) -> Ast {
        lower_in(lang, code, 0..code.len())
    }

    fn same(lang: Language, a: &str, b: &str) -> bool {
        lower(lang, a).same_code(&lower(lang, b))
    }

    /// The first node, depth first, whose source is `code`.
    fn find<'a>(ast: &'a Ast, text: &str, code: &str) -> Option<&'a Ast> {
        if text[ast.range.clone()] == *code {
            return Some(ast);
        }
        match &ast.body {
            Body::Node(children) => children.iter().find_map(|c| find(c, text, code)),
            _ => None,
        }
    }

    fn each(ast: &Ast, f: &mut impl FnMut(&Ast)) {
        f(ast);
        if let Body::Node(children) = &ast.body {
            for c in children {
                each(c, f);
            }
        }
    }

    #[test]
    fn prints_as_a_query_would() {
        assert_eq!(
            lower(Language::Python, "x = 1\n").to_string(),
            r#"(module (expression_statement (assignment left: (identifier "x") "=" right: (integer "1"))))"#
        );
    }

    #[test]
    fn prints_placeholders_as_written() {
        let hole = |name: Option<&str>, many| Ast {
            kind: "identifier".into(),
            named: true,
            field: Some("left".into()),
            body: Body::Hole {
                name: name.map(String::from),
                many,
            },
            range: 0..0,
            outer: 0..0,
        };
        assert_eq!(hole(Some("x"), false).to_string(), "left: @x");
        assert_eq!(hole(Some("rest"), true).to_string(), "left: @rest...");
        assert_eq!(hole(None, false).to_string(), "left: @_");
        assert_eq!(hole(None, true).to_string(), "left: @_...");
    }

    #[test]
    fn leaves_escape_their_text() {
        let ast = lower(Language::Python, "x = 'a\"b\\\\c'\n");
        assert!(
            ast.to_string()
                .contains(r#"(string_content "a\"b" (escape_sequence "\\\\") "c")"#),
            "{ast}"
        );
    }

    #[test]
    fn text_no_node_holds_is_kept() {
        assert!(!same(Language::Python, "'a\\nb'\n", "'x\\ny'\n"));
        assert!(same(Language::Python, "'a\\nb'\n", "'a\\nb'\n"));
    }

    #[test]
    fn comments_are_left_out() {
        assert!(same(
            Language::Rust,
            "foo(/* a */ x, // b\n y);",
            "foo(x, y);"
        ));
        assert!(same(
            Language::Python,
            "foo(x,  # a\n    y)\n",
            "foo(x, y)\n"
        ));
        assert!(
            !lower(Language::Rust, "// a\nfn f() {}")
                .to_string()
                .contains("comment")
        );
    }

    #[test]
    fn whitespace_tokens_are_left_out() {
        // Go ends a statement with a line break, or a `;`.
        assert!(same(
            Language::Go,
            "package p\nfunc f() {\n\tx()\n\ty()\n}\n",
            "package p\nfunc f() { x(); y() }\n"
        ));
    }

    #[test]
    fn separators_are_left_out_in_every_language() {
        let cases = [
            (Language::Rust, "foo(a, b,);", "foo(a, b);"),
            (Language::Python, "foo(a, b,)\n", "foo(a, b)\n"),
            (Language::Python, "a(); b()\n", "a()\nb()\n"),
            (
                Language::Go,
                "package p\nvar _ = f(a, b,)\n",
                "package p\nvar _ = f(a, b)\n",
            ),
            (Language::JavaScript, "foo(a, b,);\n", "foo(a, b)\n"),
            (Language::TypeScript, "foo(a, b,);\n", "foo(a, b)\n"),
            (Language::Tsx, "foo(a, b,);\n", "foo(a, b)\n"),
        ];
        for (lang, a, b) in cases {
            assert!(same(lang, a, b), "{lang}: {a:?} vs {b:?}");
        }
    }

    #[test]
    fn fields_keep_what_separators_meant() {
        assert!(!same(Language::Rust, "x = [0; 4];", "x = [0, 4];"));
        assert!(!same(
            Language::JavaScript,
            "for (a;;) {}\n",
            "for (; a;) {}\n"
        ));
        let ast = lower(Language::Rust, "x = [0; 4];");
        assert!(
            ast.to_string().contains("length: (integer_literal \"4\")"),
            "{ast}"
        );
    }

    #[test]
    fn separators_stay_where_they_are_the_code() {
        assert!(!same(Language::Rust, "vec![0; 4];", "vec![0, 4];"));
        assert!(same(Language::Rust, "vec![0, 4,];", "vec![0, 4];"));
        assert!(!same(
            Language::Rust,
            "let a: (u8,) = v;",
            "let a: (u8) = v;"
        ));
        assert!(!same(Language::Rust, "let (a,) = v;", "let (a) = v;"));
        assert!(same(
            Language::Rust,
            "let a: (u8, u8,) = v;",
            "let a: (u8, u8) = v;"
        ));
        assert!(same(Language::Rust, "let (a, b,) = v;", "let (a, b) = v;"));
    }

    #[test]
    fn tokens_that_change_meaning_stay() {
        assert!(!same(Language::Rust, "a + b;", "a - b;"));
        assert!(!same(Language::Rust, "fn f(&self) {}", "fn f(self) {}"));
        assert!(!same(
            Language::Python,
            "async def f(): pass\n",
            "def f(): pass\n"
        ));
    }

    #[test]
    fn markdown_lowers_without_rules() {
        let ast = lower(Language::Markdown, "# Title\n\nText.\n");
        assert_eq!(ast.kind, "document");
    }

    #[test]
    fn ranges_leave_out_whitespace() {
        let code = "package p\nfunc f() {\n\tx()\n\ty()\n}\n";
        let ast = lower(Language::Go, code);
        each(&ast, &mut |a| {
            let s = &code[a.range.clone()];
            assert_eq!(s, s.trim(), "{}", a.kind);
        });
    }

    #[test]
    fn outer_takes_in_the_comments_next_to_a_node() {
        let code = "foo(/* a */ x /* b */, y);";
        let ast = lower(Language::Rust, code);
        let x = find(&ast, code, "x").expect("x");
        assert_eq!(&code[x.outer.clone()], "/* a */ x /* b */");
        let y = find(&ast, code, "y").expect("y");
        assert_eq!(&code[y.outer.clone()], "y");
    }

    #[test]
    fn the_range_leaves_out_what_lies_outside() {
        let code = "fn a() {}\nfn b() {}\n";
        let b = code.find("fn b").unwrap();
        let ast = lower_in(Language::Rust, code, b..code.len());
        assert_eq!(
            ast.to_string(),
            lower(Language::Rust, "fn b() {}\n").to_string()
        );
        let tree = Language::Rust.parse(code);
        let a = tree.root_node().child(0).unwrap();
        assert!(Ast::lower(Language::Rust, a, code, b..code.len()).is_none());
    }

    #[test]
    fn same_code_ignores_layout_comments_and_place() {
        let code = "x == x /* c */;\nf(a /* c */, b);\nf(a, b);\nf(a, c);\n";
        let ast = lower(Language::Rust, code);
        let Body::Node(statements) = &ast.body else {
            panic!("{ast}")
        };
        let call = |i: usize| match &statements[i].body {
            Body::Node(c) => &c[0],
            _ => panic!(),
        };
        assert!(call(1).same_code(call(2)));
        assert!(!call(2).same_code(call(3)));
        let Body::Node(eq) = &call(0).body else {
            panic!()
        };
        assert_ne!(eq[0].field, eq[2].field);
        assert!(eq[0].same_code(&eq[2]));
    }

    #[test]
    fn every_rule_is_one_capture() {
        for lang in Language::ALL {
            let query = lang.ast();
            for i in 0..query.pattern_count() {
                let captures = query
                    .capture_quantifiers(i)
                    .iter()
                    .filter(|q| **q != tree_sitter::CaptureQuantifier::Zero)
                    .count();
                assert_eq!(captures, 1, "{lang}: pattern {i}");
            }
        }
    }
}
