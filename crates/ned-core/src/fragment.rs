//! Parsing code fragments (command-language spec, §3.10): alone, or in place
//! of code in a file, so a fragment that only parses inside other code (a
//! field, a method) parses where it's searched for.

use std::collections::HashSet;
use std::ops::Range;

use tree_sitter::{Node, Tree};

use crate::lang::Language;
use crate::template::{Count, HoleText, Template};

/// Where a fragment parses (§3.10): in place of some code in a file.
#[derive(Debug, Clone)]
pub struct Context<'a> {
    /// The code before and after the fragment, in the top-level node of the
    /// file that holds it: the rest can't change how the fragment parses.
    before: &'a str,
    after: &'a str,
    /// How many ancestors the node the fragment goes in has.
    depth: usize,
    /// How many errors the top-level node has already.
    errors: usize,
}

impl<'a> Context<'a> {
    /// In place of `splice` of `text`, the masked text of a file whose tree
    /// is `tree`, inside the node spanning `container`, or else the deepest
    /// node that strictly contains the code in `splice`.
    pub fn new(
        text: &'a str,
        tree: &'a Tree,
        splice: Range<usize>,
        container: Option<Range<usize>>,
    ) -> Self {
        let root = tree.root_node();
        let node = match container {
            Some(c) => {
                // A Python body's lines take in indentation its block doesn't.
                let c = trim(text, &c);
                // The outermost node spanning it: a Python block of one
                // statement spans what the statement does.
                let mut node = root
                    .descendant_for_byte_range(c.start, c.end)
                    .unwrap_or(root);
                while let Some(parent) = node.parent()
                    && parent.byte_range() == node.byte_range()
                {
                    node = parent;
                }
                node
            }
            None => {
                let code = trim(text, &splice);
                let mut node = root
                    .descendant_for_byte_range(code.start, code.end)
                    .unwrap_or(root);
                while !(node.is_named()
                    && node.child_count() > 0
                    && trim(text, &node.byte_range()) != code)
                    && let Some(parent) = node.parent()
                {
                    node = parent;
                }
                node
            }
        };
        let top = std::iter::successors(Some(node), Node::parent)
            .take_while(|n| n.parent().is_some())
            .last()
            .map_or(0..text.len(), |n| n.byte_range());
        // The indentation of the splice's first line stays before the
        // fragment, and its final newline after it.
        let Range { mut start, mut end } = splice;
        let spliced = &text[start..end];
        if start == 0 || text[..start].ends_with('\n') {
            start += spliced.len() - spliced.trim_start_matches([' ', '\t']).len();
        }
        if end > start && text[..end].ends_with('\n') {
            end -= 1;
        }
        Context {
            before: &text[top.start..start],
            after: &text[end..top.end],
            depth: ancestors(node),
            errors: errors(tree)
                .iter()
                .filter(|e| top.start <= e.start && e.end <= top.end)
                .count(),
        }
    }
}

/// The code around a fragment in one reading: `before` it, then
/// `terminator` and `after`.
struct Site<'s> {
    before: &'s str,
    terminator: &'s str,
    after: &'s str,
    /// How many ancestors the node the fragment goes in has.
    depth: usize,
    /// How many errors the code around it has already.
    errors: usize,
}

/// A parsed fragment: the program it parsed in, and where the fragment and
/// its holes are in it.
#[derive(Debug)]
pub struct Fragment {
    pub text: String,
    pub tree: Tree,
    /// The byte range of the fragment's root node, or of its run of root
    /// nodes.
    pub roots: Range<usize>,
    /// Where each hole is in `text`.
    pub holes: Vec<Slot>,
    /// How many ancestors the node it parsed in has.
    depth: usize,
}

/// Where a hole is in a reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Slot {
    /// Literal text, in a string or comment.
    Literal,
    /// The range of its placeholder.
    Node(Range<usize>),
    /// A run left out of the text, as among an impl's items where no
    /// identifier parses: the offset between the siblings it stands for.
    Gap(usize),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FragmentError {
    #[error("the pattern doesn't parse as {lang} where it's searched")]
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

impl Fragment {
    /// The fragment's root node, or its run of sibling root nodes.
    pub fn roots(&self) -> Vec<Node<'_>> {
        let r = &self.roots;
        let node = self.spanning(r);
        if self.is_root(node) {
            return vec![node];
        }
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .filter(|c| !c.is_extra() && c.start_byte() >= r.start && self.span(*c).end <= r.end)
            .collect()
    }

    /// Whether `node`, the node spanning the roots, is the one root. The node
    /// the fragment parsed in isn't: when it spans exactly the fragment (Go's
    /// `source_file` for statements, a Python block), its children are the
    /// roots.
    fn is_root(&self, node: Node<'_>) -> bool {
        self.span(node) == self.roots && node.is_named() && ancestors(node) > self.depth
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

/// The reading of `template` in `lang` in `context`, or alone: as written,
/// then with a statement terminator after it, then with its runs left out.
pub fn parse(
    lang: Language,
    template: &Template,
    context: Option<&Context>,
) -> Result<Fragment, FragmentError> {
    let runs: HashSet<usize> = template
        .holes()
        .enumerate()
        .filter_map(|(i, h)| (h.count != Count::One).then_some(i))
        .collect();
    // Alone, the fragment ends with a newline: Go ends a statement with one.
    let (before, after) = context.map_or(("", "\n"), |c| (c.before, c.after));
    let site = |terminator| Site {
        before,
        terminator,
        after,
        depth: context.map_or(0, |c| c.depth),
        errors: context.map_or(0, |c| c.errors),
    };
    let terminators = match lang.terminator() {
        "" => vec![""],
        t => vec!["", t],
    };
    let mut fused = None;
    // Runs are left out of the text only if nothing parses with them in.
    let passes = match runs.is_empty() {
        true => vec![HashSet::new()],
        false => vec![HashSet::new(), runs],
    };
    for gaps in passes {
        if fused.is_some() {
            break;
        }
        for &terminator in &terminators {
            match reading(lang, template, &site(terminator), &gaps) {
                Ok(Some(f)) => return Ok(f),
                Ok(None) => {}
                Err(e) => fused = fused.or(Some(e)),
            }
        }
    }
    if let Some(e) = fused {
        return Err(e);
    }
    let bare = template.source(|_| HoleText::Placeholder);
    let tree = lang.parse(&bare.text);
    let at = first_error(tree.root_node()).map_or(0, |n| n.start_byte());
    Err(FragmentError::NoParse {
        lang,
        at: bare.to_template(at),
    })
}

/// The reading of `template` at `site`, if it parses without new errors and
/// its roots are whole nodes.
fn reading(
    lang: Language,
    template: &Template,
    site: &Site,
    gaps: &HashSet<usize>,
) -> Result<Option<Fragment>, FragmentError> {
    let Some(f) = build(lang, template, site, &HashSet::new(), gaps) else {
        return Ok(None);
    };
    // A hole inside a string or comment keeps its text, which leaves the
    // tree's shape unchanged, even where its placeholder is a whole node (all
    // of a string's content). Any other hole that isn't a whole node is fused
    // into a neighbouring token.
    let shape = shape(&f.tree);
    let mut literal = HashSet::new();
    for (i, slot) in f.holes.iter().enumerate() {
        let Slot::Node(r) = slot else { continue };
        let mut trial = literal.clone();
        trial.insert(i);
        let keeps_shape = build(lang, template, site, &trial, gaps)
            .is_some_and(|t| self::shape(&t.tree) == shape);
        if keeps_shape {
            literal = trial;
        } else if f.span(f.spanning(r)) != *r {
            let span = template
                .holes()
                .nth(i)
                .expect("a hole per slot")
                .span
                .clone();
            return Err(FragmentError::Fused { span });
        }
    }
    if literal.is_empty() {
        return Ok(Some(f));
    }
    Ok(build(lang, template, site, &literal, gaps))
}

/// Parses `template` at `site`, with the holes in `literal` keeping their
/// text. `None` if it adds errors, or its roots aren't whole nodes inside
/// the node it goes in.
fn build(
    lang: Language,
    template: &Template,
    site: &Site,
    literal: &HashSet<usize>,
    gaps: &HashSet<usize>,
) -> Option<Fragment> {
    let source = template.source(|i| match (literal.contains(&i), gaps.contains(&i)) {
        (true, _) => HoleText::Own,
        (_, true) => HoleText::Empty,
        _ => HoleText::Placeholder,
    });
    let Site {
        before,
        terminator,
        after,
        ..
    } = site;
    // Later lines of the fragment take the indentation the prefix ends at.
    let last_line = &before[before.rfind('\n').map_or(0, |i| i + 1)..];
    let indent = &last_line[..last_line.len() - last_line.trim_start().len()];
    // The terminator goes after the code, before the comments that end it.
    let cut = match terminator.is_empty() {
        true => source.text.trim_end().len(),
        false => code_end(lang, &source.text),
    };
    let at = |offset: usize| {
        let terminated = if offset > cut { terminator.len() } else { 0 };
        before.len()
            + offset
            + terminated
            + indent.len() * source.text[..offset].matches('\n').count()
    };
    let indented = |s: &str| s.replace('\n', &format!("\n{indent}"));
    let code = format!(
        "{}{terminator}{}",
        indented(&source.text[..cut]),
        indented(&source.text[cut..])
    );
    let text = format!("{before}{code}{after}");
    let tree = lang.parse(&text);
    let inserted = before.len()..before.len() + code.len();
    let errors = errors(&tree);
    if errors.len() > site.errors
        || errors
            .iter()
            .any(|e| e.start <= inserted.end && inserted.start <= e.end)
    {
        return None;
    }
    // A terminator ends a statement only as a token of its own, not at the end
    // of a comment.
    let ends = at(cut)..at(cut) + terminator.len();
    if !terminator.is_empty()
        && tree
            .root_node()
            .descendant_for_byte_range(ends.start, ends.end)
            .is_none_or(|n| n.kind() != *terminator)
    {
        return None;
    }
    let start = source.text.len() - source.text.trim_start().len();
    let end = cut;
    if start >= end {
        return None;
    }
    let holes = source
        .holes
        .iter()
        .enumerate()
        .map(|(i, r)| match (literal.contains(&i), gaps.contains(&i)) {
            (true, _) => Slot::Literal,
            (_, true) => Slot::Gap(at(r.start)),
            _ => Slot::Node(at(r.start)..at(r.end)),
        })
        .collect();
    let f = Fragment {
        text,
        tree,
        roots: at(start)..at(end),
        holes,
        depth: site.depth,
    };
    let node = f.spanning(&f.roots);
    if ancestors(node) < f.depth {
        return None;
    }
    if f.span(node) != f.roots && !covers(&f, node) {
        return None;
    }
    // A comment alone is no code to match.
    let roots = f.roots();
    if roots.is_empty() || roots.iter().any(|r| r.is_extra()) {
        return None;
    }
    Some(f)
}

/// Where `code` ends without the comments after it.
fn code_end(lang: Language, code: &str) -> usize {
    let tree = lang.parse(code);
    let mut end = code.trim_end().len();
    while end > 0
        && let Some(comment) = tree
            .root_node()
            .descendant_for_byte_range(end - 1, end)
            .and_then(|n| {
                std::iter::successors(Some(n), Node::parent).find(|a| a.is_extra() && !a.is_error())
            })
    {
        end = code[..comment.start_byte()].trim_end().len();
    }
    end
}

/// Whether the fragment's roots start at one of `node`'s children and end at
/// one, a run of whole children. Its ends may be tokens, such as a trailing
/// `,`.
fn covers(f: &Fragment, node: Node<'_>) -> bool {
    let range = &f.roots;
    let children: Vec<_> = children_of(node).into_iter().map(|c| f.span(c)).collect();
    let straddles = |at: usize| children.iter().any(|c| c.start < at && at < c.end);
    children.iter().any(|c| c.start == range.start)
        && children.iter().any(|c| c.end == range.end)
        && !straddles(range.start)
        && !straddles(range.end)
}

/// How many ancestors `node` has.
fn ancestors(node: Node<'_>) -> usize {
    std::iter::successors(node.parent(), Node::parent).count()
}

/// `range` of `text` without the whitespace at its ends.
fn trim(text: &str, range: &Range<usize>) -> Range<usize> {
    let s = &text[range.clone()];
    let start = range.start + (s.len() - s.trim_start().len());
    start..(start + s.trim().len())
}

/// The ranges of `tree`'s error and missing nodes.
fn errors(tree: &Tree) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.is_error() || node.is_missing() {
            out.push(node.byte_range());
        }
        if node.has_error() {
            stack.extend(node.children(&mut node.walk()));
        }
    }
    out
}

/// A node's children, without comments.
fn children_of(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(|c| !c.is_extra())
        .collect()
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

/// Where parsing failed under `node`: the first missing node, or the token
/// the first error node gave up at.
fn first_error(node: Node<'_>) -> Option<Node<'_>> {
    if node.is_missing() {
        return Some(node);
    }
    // Not `children_of`: an error node can be an extra.
    let mut cursor = node.walk();
    let children: Vec<_> = node.children(&mut cursor).collect();
    if node.is_error() {
        // An error of only tokens is where parsing failed; one holding nodes
        // failed at its last child, after the nodes it could build.
        let last = children
            .last()
            .filter(|_| children.iter().any(|c| c.is_named()));
        return Some(last.copied().unwrap_or(node));
    }
    children
        .into_iter()
        .filter(|c| c.has_error())
        .find_map(first_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alone(lang: Language, src: &str) -> Fragment {
        parse(lang, &Template::parse(src), None).unwrap_or_else(|e| panic!("{src}: {e}"))
    }

    /// `src` parsed in place of the first `site` in `text`.
    fn within(
        lang: Language,
        text: &str,
        site: &str,
        src: &str,
    ) -> Result<Fragment, FragmentError> {
        let tree = lang.parse(text);
        let start = text.find(site).expect("the site is in the text");
        let context = Context::new(text, &tree, start..start + site.len(), None);
        parse(lang, &Template::parse(src), Some(&context))
    }

    fn root_kinds(f: &Fragment) -> Vec<&str> {
        f.roots().iter().map(|n| n.kind()).collect()
    }

    /// Asserts that `src` alone has one root, of `kind`.
    fn alone_is(lang: Language, src: &str, kind: &str) {
        assert_eq!(root_kinds(&alone(lang, src)), [kind], "{src}");
    }

    /// Asserts that `src`, in place of `site` in `text`, has one root, of
    /// `kind`.
    fn within_is(lang: Language, text: &str, site: &str, src: &str, kind: &str) {
        let f = within(lang, text, site, src).unwrap_or_else(|e| panic!("{src}: {e}"));
        assert_eq!(root_kinds(&f), [kind], "{src}");
    }

    #[test]
    fn rust_fragments() {
        alone_is(
            Language::Rust,
            "fn new(&self) -> Self { @body... }",
            "function_item",
        );
        alone_is(Language::Rust, "let @x = @e;", "let_declaration");
        alone_is(
            Language::Rust,
            "if let Some(@x) = @e { @body... }",
            "if_expression",
        );
        alone_is(Language::Rust, "dbg!(@x...)", "macro_invocation");
        let text = "fn f<T>(a: u8) -> X {\n    match a {\n        A => 1,\n    }\n}\n\
                    enum E {\n    A,\n}\ntrait T {\n    fn g();\n}\n";
        within_is(
            Language::Rust,
            text,
            "A => 1,",
            "Some(@x) => @y,",
            "match_arm",
        );
        within_is(Language::Rust, text, "A,", "Leaf(u32)", "enum_variant");
        within_is(Language::Rust, text, "X", "Vec<@t>", "generic_type");
        within_is(Language::Rust, text, "T", "T: Clone", "type_parameter");
        within_is(Language::Rust, text, "a: u8", "@a: u32", "parameter");
        within_is(
            Language::Rust,
            text,
            "fn g();",
            "fn len(&self) -> usize;",
            "function_signature_item",
        );
    }

    #[test]
    fn an_expression_parses_alone_with_a_terminator() {
        let f = alone(Language::Rust, "foo(@a, 1)");
        assert_eq!(root_kinds(&f), ["call_expression"]);
        assert_eq!(&f.text[f.roots.clone()], "foo(__ned_a, 1)");
        alone_is(Language::Rust, "@a + @b", "binary_expression");
    }

    #[test]
    fn the_terminator_goes_before_trailing_comments() {
        let f = alone(Language::Rust, "foo(@a, 1) // c");
        assert_eq!(root_kinds(&f), ["call_expression"]);
        assert_eq!(&f.text[f.roots.clone()], "foo(__ned_a, 1)");
        let f = alone(Language::Rust, "foo(@a) /* x */ // @b");
        assert_eq!(&f.text[f.roots.clone()], "foo(__ned_a)");
        assert_eq!(f.holes[1], Slot::Literal);
    }

    #[test]
    fn a_fragment_parses_only_in_its_context() {
        let text = "struct S {\n    b: u8,\n}\nfn f() {\n    g();\n}\n";
        within_is(
            Language::Rust,
            text,
            "b: u8,",
            "x: i64",
            "field_declaration",
        );
        assert!(matches!(
            parse(Language::Rust, &Template::parse("x: i64"), None),
            Err(FragmentError::NoParse { .. })
        ));
        assert!(matches!(
            within(Language::Rust, text, "g();", "x: i64"),
            Err(FragmentError::NoParse { .. })
        ));
    }

    #[test]
    fn errors_elsewhere_in_the_file_do_not_block() {
        let text = "struct S {\n    b: u8,\n    c: ,\n}\n";
        within_is(
            Language::Rust,
            text,
            "b: u8,",
            "x: i64",
            "field_declaration",
        );
    }

    #[test]
    fn python_fragments() {
        alone_is(
            Language::Python,
            "def start(self):\n    pass",
            "function_definition",
        );
        alone_is(Language::Python, "@x = 1", "assignment");
        let text = "match v:\n    case 1:\n        pass\n\nd = {\"k\": 1}\n\n\
                    @app.get(\"/\")\ndef f(a):\n    pass\n";
        within_is(
            Language::Python,
            text,
            "case 1:\n        pass",
            "case [@x]:\n    pass",
            "case_clause",
        );
        within_is(Language::Python, text, "\"k\": 1", "\"a\": @v", "pair");
        within_is(Language::Python, text, "(a)", "(@x: int)", "parameters");
        within_is(
            Language::Python,
            text,
            "@app.get(\"/\")",
            "@@app.route(@path)",
            "decorator",
        );
    }

    #[test]
    fn later_lines_take_the_indentation_of_the_splice() {
        let text = "class A:\n    def f(self):\n        pass\n";
        let f = within(
            Language::Python,
            text,
            "    def f(self):\n        pass\n",
            "def g(self):\n    if x:\n        return 1",
        )
        .unwrap();
        assert_eq!(root_kinds(&f), ["function_definition"]);
        assert_eq!(
            &f.text[f.roots.clone()],
            "def g(self):\n        if x:\n            return 1"
        );
    }

    #[test]
    fn a_whole_body_of_statements_is_a_run_of_roots() {
        let text = "def f():\n    a = 1\n";
        let tree = Language::Python.parse(text);
        let body = tree
            .root_node()
            .child(0)
            .and_then(|f| f.child_by_field_name("body"))
            .unwrap()
            .byte_range();
        let context = Context::new(text, &tree, body.clone(), Some(body));
        let f = parse(
            Language::Python,
            &Template::parse("x = 1\ny = 2"),
            Some(&context),
        )
        .unwrap();
        assert_eq!(
            root_kinds(&f),
            ["expression_statement", "expression_statement"]
        );
        let f = parse(Language::Python, &Template::parse("x = 1"), Some(&context)).unwrap();
        assert_eq!(root_kinds(&f), ["assignment"]);
    }

    #[test]
    fn go_fragments() {
        alone_is(Language::Go, "x := @v", "short_var_declaration");
        alone_is(Language::Go, "foo(@a)", "call_expression");
        let text = "package p\n\ntype T struct {\n\tA int\n}\n\ntype I interface {\n\tF()\n}\n\n\
                    func f(a int) {\n\tswitch a {\n\tcase 1:\n\t\tg()\n\t}\n}\n";
        within_is(
            Language::Go,
            text,
            "A int",
            "Name string",
            "field_declaration",
        );
        within_is(Language::Go, text, "F()", "Close() error", "method_elem");
        within_is(
            Language::Go,
            text,
            "case 1:\n\t\tg()",
            "case 1:\n\t@x",
            "expression_case",
        );
        within_is(
            Language::Go,
            text,
            "a int",
            "ctx context.Context",
            "parameter_declaration",
        );
    }

    #[test]
    fn javascript_fragments() {
        alone_is(Language::JavaScript, "foo(@a)", "call_expression");
        alone_is(Language::JavaScript, "a: @v", "labeled_statement");
        let text = "class A {\n  f() {}\n}\nconst o = { k: 1 };\nswitch (v) {\n  case 0: g();\n}\n";
        within_is(
            Language::JavaScript,
            text,
            "f() {}",
            "render() { return @x; }",
            "method_definition",
        );
        within_is(Language::JavaScript, text, "k: 1", "a: @v", "pair");
        within_is(
            Language::JavaScript,
            text,
            "case 0: g();",
            "case 1: @x;",
            "switch_case",
        );
    }

    #[test]
    fn typescript_fragments() {
        let text = "interface I {\n  a: number;\n}\ntype T = A;\n";
        within_is(
            Language::TypeScript,
            text,
            "a: number;",
            "name: string;",
            "property_signature",
        );
        within_is(Language::TypeScript, text, "A", "@a | @b", "union_type");
        alone_is(Language::TypeScript, "@a | @b", "binary_expression");
    }

    #[test]
    fn several_roots() {
        let f = alone(Language::Rust, "let a = 1;\nlet b = 2;");
        assert_eq!(root_kinds(&f), ["let_declaration", "let_declaration"]);
        assert_eq!(&f.text[f.roots.clone()], "let a = 1;\nlet b = 2;");
    }

    #[test]
    fn holes_are_whole_nodes() {
        let f = &alone(Language::Rust, "foo(@a, @rest...)");
        let text = |i: usize| match &f.holes[i] {
            Slot::Node(r) => &f.text[r.clone()],
            slot => panic!("{slot:?}"),
        };
        assert_eq!(text(0), "__ned_a");
        assert_eq!(text(1), "__ned_rest");
    }

    #[test]
    fn holes_in_strings_and_comments_are_literal() {
        let f = &alone(Language::Rust, "log(\"user@host\", /* @x */ @y)");
        assert_eq!(f.holes[0], Slot::Literal);
        assert_eq!(f.holes[1], Slot::Literal);
        assert!(matches!(f.holes[2], Slot::Node(_)));
        assert!(
            f.text.contains("\"user@host\", /* @x */ __ned_y"),
            "{}",
            f.text
        );
    }

    #[test]
    fn a_placeholder_that_is_a_whole_string_is_literal() {
        for (lang, src) in [
            (Language::Rust, "log(\"@who\", @x)"),
            (Language::Python, "log(\"@who\", @x)"),
            (Language::JavaScript, "log(\"@who\", @x)"),
            (Language::Go, "log(\"@who\", @x)"),
        ] {
            let f = &alone(lang, src);
            assert_eq!(f.holes[0], Slot::Literal, "{lang}");
            assert!(matches!(f.holes[1], Slot::Node(_)), "{lang}");
        }
    }

    #[test]
    fn a_run_where_no_name_parses_is_a_gap() {
        let f = &alone(Language::Rust, "impl Display for @t { @_... }");
        assert_eq!(f.roots()[0].kind(), "impl_item");
        assert!(matches!(f.holes[0], Slot::Node(_)));
        let Slot::Gap(at) = f.holes[1] else {
            panic!("{:?}", f.holes[1]);
        };
        assert_eq!(&f.text[at - 2..at + 2], "{  }");
    }

    #[test]
    fn a_hole_fused_into_a_name_is_an_error() {
        let template = Template::parse("foo@bar(1)");
        assert_eq!(
            parse(Language::Rust, &template, None).unwrap_err(),
            FragmentError::Fused { span: 3..7 }
        );
    }

    #[test]
    fn a_fragment_that_never_parses_is_an_error() {
        let template = Template::parse("fn (@a");
        let Err(FragmentError::NoParse { lang, at }) = parse(Language::Rust, &template, None)
        else {
            panic!("parsed");
        };
        assert_eq!(lang, Language::Rust);
        assert!(at <= 6, "at {at}");
    }
}
