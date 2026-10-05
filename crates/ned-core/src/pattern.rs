//! Matching syntax patterns against syntax trees (command-language spec,
//! §3.10): the pattern and the file are both lowered to abstract syntax trees
//! (`crate::ast`), which match node by node.

use std::cmp::Reverse;
use std::ops::Range;

use tree_sitter::Tree;

use crate::ast::{Ast, Body};
use crate::fragment::{self, Context, FragmentError, Slot};
use crate::lang::Language;
use crate::template::{Count, Template};
use crate::text::strip_indent;

/// A pattern compiled for one context.
#[derive(Debug)]
pub struct Pattern {
    lang: Language,
    /// Its root node, or its run of sibling root nodes, with a hole for each
    /// placeholder. A run left out of the text, where no node parsed, is a
    /// hole without a kind.
    roots: Vec<Ast>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternMatch {
    pub range: Range<usize>,
    /// Each named placeholder's capture, in the pattern's order.
    pub captures: Vec<(String, Range<usize>)>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PatternError {
    #[error(
        "doesn't parse as {lang} where it's searched, at `{at}`; {}", no_parse_fix(.lang)
    )]
    NoParse { lang: Language, at: String },
    #[error("has `{placeholder}` where a whole node must be; write `@@` for a literal `@`")]
    Fused { placeholder: String },
    #[error("has no code; write the code to match between the backquotes")]
    Empty,
}

/// How to make a pattern parse in `lang`. A match arm, a case or a decorator
/// alone parses nowhere a step can search, so their fix is the code around them.
fn no_parse_fix(lang: &Language) -> &'static str {
    match lang {
        Language::Rust => {
            "select the code it goes in first (struct:S>`x: u8`), or write the code around it too (a match arm's whole `match`)"
        }
        Language::Python => {
            "select the code it goes in first (class:C>`x: int = 1`), or write the code around it too (a decorator's whole `def`)"
        }
        Language::Go => {
            "select the code it goes in first (struct:S>`X int`), or write the code around it too (a case's whole `switch`)"
        }
        Language::JavaScript | Language::TypeScript | Language::Tsx => {
            "select the code it goes in first (class:C>`x = 1`), or write the code around it too (a case's whole `switch`)"
        }
        Language::Markdown => "add the code around it",
    }
}

impl Pattern {
    /// Compiles the pattern `src` for `lang` in `context`, or alone, ignoring
    /// the common indentation of its lines.
    pub fn compile(
        lang: Language,
        src: &str,
        context: Option<&Context>,
    ) -> Result<Pattern, PatternError> {
        let code = strip_indent(src).join("\n");
        if code.trim().is_empty() {
            return Err(PatternError::Empty);
        }

        let template = Template::parse(&code);
        let fragment = fragment::parse(lang, &template, context).map_err(|e| match e {
            FragmentError::NoParse { lang, at } => PatternError::NoParse {
                lang,
                at: excerpt(&code, at),
            },
            FragmentError::Fused { span } => PatternError::Fused {
                placeholder: code[span].to_string(),
            },
        })?;
        let mut roots: Vec<Ast> = fragment
            .roots()
            .into_iter()
            .filter_map(|r| Ast::lower(lang, r, &fragment.text, r.byte_range()))
            .collect();
        for (h, slot) in template.holes().zip(&fragment.holes) {
            let hole = Body::Hole {
                name: h.name.clone(),
                count: h.count,
            };
            match slot {
                Slot::Node(r) => hole_at(&mut roots, r, hole),
                Slot::Gap(at) => gap_at(&mut roots, *at, hole),
                Slot::Literal => {}
            }
        }
        Ok(Pattern { lang, roots })
    }

    /// The matches in `tree`, the tree of `text`, that lie within `range`, in
    /// source order. A match inside an earlier one is skipped.
    pub fn find(&self, tree: &Tree, text: &str, range: Range<usize>) -> Vec<PatternMatch> {
        let mut all = Vec::new();
        if let Some(file) = Ast::lower(self.lang, tree.root_node(), text, range.clone()) {
            self.walk(&file, true, text, &range, &mut all);
        }
        all.sort_by_key(|m| (m.range.start, Reverse(m.range.end)));
        let mut out: Vec<PatternMatch> = Vec::new();
        for m in all {
            if out
                .last()
                .is_none_or(|last| m.range.start >= last.range.end)
            {
                out.push(m);
            }
        }
        out
    }

    /// Pushes the matches in `node`'s subtree that lie within `range`. The
    /// `top` node is the whole file, not code to match.
    fn walk(
        &self,
        node: &Ast,
        top: bool,
        text: &str,
        range: &Range<usize>,
        out: &mut Vec<PatternMatch>,
    ) {
        let inside = |r: &Range<usize>| range.start <= r.start && r.end <= range.end;
        let children = match &node.body {
            Body::Node(children) => children.as_slice(),
            _ => &[],
        };
        if let [root] = self.roots.as_slice() {
            let candidate = !top
                && node.named
                && (matches!(root.body, Body::Hole { .. }) || root.kind == node.kind);
            if candidate && inside(&node.range) {
                let mut m = Matcher::new(text);
                if m.node(root, node) {
                    out.push(m.matched(node.range.clone()));
                }
            }
        } else {
            for (i, start) in children.iter().enumerate() {
                if !start.named || !inside(&start.range) {
                    continue;
                }
                let mut m = Matcher::new(text);
                // A run of roots may match no siblings, which is no match.
                if let Some(end) = m.seq(&self.roots, &children[i..], false).filter(|&n| n > 0) {
                    let r = start.range.start..children[i + end - 1].range.end;
                    if inside(&r) {
                        out.push(m.matched(r));
                    }
                }
            }
        }
        for child in children {
            self.walk(child, false, text, range, out);
        }
    }
}

/// The rest of the line of `code` at `at`, where parsing failed.
fn excerpt(code: &str, at: usize) -> String {
    let at = if at < code.trim_end().len() {
        at
    } else {
        code.trim_end().rfind('\n').map_or(0, |i| i + 1)
    };
    let line = code[at..].lines().next().unwrap_or_default().trim_end();
    line.chars().take(30).collect()
}

/// Makes the outermost node among `asts` whose source is `r`, a
/// placeholder's, a hole.
fn hole_at(asts: &mut [Ast], r: &Range<usize>, hole: Body) {
    for a in asts {
        if a.range == *r {
            a.body = hole;
            return;
        }
        if a.range.start <= r.start
            && r.end <= a.range.end
            && let Body::Node(children) = &mut a.body
        {
            return hole_at(children, r, hole);
        }
    }
}

/// Puts a hole for a run left out of the text at `at` among the children of
/// the deepest node in `asts` that holds it, or among `asts` themselves.
fn gap_at(asts: &mut Vec<Ast>, at: usize, hole: Body) {
    let holder = asts
        .iter_mut()
        .find(|a| a.range.start < at && at < a.range.end && matches!(a.body, Body::Node(_)));
    if let Some(Ast {
        body: Body::Node(children),
        ..
    }) = holder
    {
        return gap_at(children, at, hole);
    }
    let i = asts.iter().filter(|a| a.range.end <= at).count();
    asts.insert(
        i,
        Ast {
            kind: String::new(),
            named: false,
            field: None,
            body: hole,
            range: at..at,
            outer: at..at,
        },
    );
}

/// A match in progress: what its placeholders have bound.
struct Matcher<'t> {
    text: &'t str,
    binds: Vec<Bind<'t>>,
}

struct Bind<'t> {
    name: String,
    nodes: Vec<&'t Ast>,
    range: Range<usize>,
}

impl<'t> Matcher<'t> {
    fn new(text: &'t str) -> Self {
        Matcher {
            text,
            binds: Vec::new(),
        }
    }

    fn matched(self, range: Range<usize>) -> PatternMatch {
        PatternMatch {
            range,
            captures: self.binds.into_iter().map(|b| (b.name, b.range)).collect(),
        }
    }

    /// Whether pattern node `p` matches target node `t`.
    fn node(&mut self, p: &Ast, t: &'t Ast) -> bool {
        match (&p.body, &t.body) {
            (Body::Hole { name, .. }, _) => t.named && self.bind(name, vec![t], t.outer.clone()),
            _ if p.kind != t.kind || p.named != t.named => false,
            (Body::Leaf(a), Body::Leaf(b)) => a == b,
            (Body::Node(pc), Body::Node(tc)) => self.seq(pc, tc, true).is_some(),
            _ => false,
        }
    }

    /// Matches the pattern nodes `ps` against a prefix of the target nodes
    /// `ts`, or all of them if `all`, when they're a node's children, each in
    /// the same field. The number of target nodes matched, if they match.
    fn seq(&mut self, ps: &[Ast], ts: &'t [Ast], all: bool) -> Option<usize> {
        let Some((p, rest)) = ps.split_first() else {
            return (!all || ts.is_empty()).then_some(0);
        };
        let saved = self.binds.len();
        if let Body::Hole { name, count } = &p.body
            && *count != Count::One
        {
            // A run takes as few siblings as it can.
            for end in usize::from(*count == Count::OneOrMore)..=ts.len() {
                let run = &ts[..end];
                let range = match (run.first(), run.last()) {
                    (Some(first), Some(last)) => first.outer.start..last.outer.end,
                    _ => {
                        let at = ts.get(end).map_or(0, |t| t.range.start);
                        at..at
                    }
                };
                if self.bind(name, run.iter().collect(), range)
                    && let Some(n) = self.seq(rest, &ts[end..], all)
                {
                    return Some(end + n);
                }
                self.binds.truncate(saved);
            }
            return None;
        }
        let t = ts.first()?;
        if all && p.field != t.field {
            return None;
        }
        if self.node(p, t)
            && let Some(n) = self.seq(rest, &ts[1..], all)
        {
            return Some(1 + n);
        }
        self.binds.truncate(saved);
        self.rest_of(p, t, rest, &ts[1..], all).map(|n| 1 + n)
    }

    /// Matches pattern node `p`, then the run left out of the text `rest[0]`,
    /// against target `t`, a node that starts with a match of `p` (the run
    /// takes the rest of `t`, as the tail of a method chain), then `rest[1..]`
    /// against `ts`. The number of `ts` matched, if they match.
    fn rest_of(
        &mut self,
        p: &Ast,
        t: &'t Ast,
        rest: &[Ast],
        ts: &'t [Ast],
        all: bool,
    ) -> Option<usize> {
        let (run, rest) = rest.split_first()?;
        let Body::Hole { name, count } = &run.body else {
            return None;
        };
        if *count == Count::One || !run.kind.is_empty() || !p.named || !t.named {
            return None;
        }
        let saved = self.binds.len();
        let mut start = t;
        while let Body::Node(children) = &start.body
            && let Some(first) = children.first().filter(|c| c.named)
        {
            start = first;
            let tail = &self.text[start.range.end..t.range.end];
            let from = t.range.end - tail.trim_start().len();
            if self.node(p, start)
                && self.bind(name, Vec::new(), from..t.range.end)
                && let Some(n) = self.seq(rest, ts, all)
            {
                return Some(n);
            }
            self.binds.truncate(saved);
        }
        None
    }

    /// Binds `name` to `nodes`, or checks them against its earlier binding.
    fn bind(&mut self, name: &Option<String>, nodes: Vec<&'t Ast>, range: Range<usize>) -> bool {
        let Some(name) = name else {
            return true;
        };
        if let Some(earlier) = self.binds.iter().find(|b| b.name == *name) {
            if earlier.nodes.is_empty() || nodes.is_empty() {
                // The rest of a longer node has no nodes of its own.
                let code =
                    |r: &Range<usize>| self.text[r.clone()].split_whitespace().collect::<String>();
                return code(&earlier.range) == code(&range);
            }
            return earlier.nodes.len() == nodes.len()
                && earlier
                    .nodes
                    .iter()
                    .zip(&nodes)
                    .all(|(a, b)| a.same_code(b));
        }
        self.binds.push(Bind {
            name: name.clone(),
            nodes,
            range,
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(lang: Language, pattern: &str, text: &str) -> Vec<PatternMatch> {
        let p = Pattern::compile(lang, pattern, None).unwrap_or_else(|e| panic!("{pattern}: {e}"));
        p.find(&lang.parse(text), text, 0..text.len())
    }

    /// The text of each match of `pattern` in Rust `text`.
    fn found(pattern: &str, text: &str) -> Vec<String> {
        found_in(Language::Rust, pattern, text)
    }

    fn found_in(lang: Language, pattern: &str, text: &str) -> Vec<String> {
        matches(lang, pattern, text)
            .iter()
            .map(|m| text[m.range.clone()].to_string())
            .collect()
    }

    /// The text of each match of `pattern`, parsed in place of the first
    /// `site` in Rust `text`.
    fn found_within(pattern: &str, text: &str, site: &str) -> Vec<String> {
        let tree = Language::Rust.parse(text);
        let start = text.find(site).expect("the site is in the text");
        let context = Context::new(text, &tree, start..start + site.len(), None);
        let p = Pattern::compile(Language::Rust, pattern, Some(&context))
            .unwrap_or_else(|e| panic!("{pattern}: {e}"));
        p.find(&tree, text, 0..text.len())
            .iter()
            .map(|m| text[m.range.clone()].to_string())
            .collect()
    }

    /// Each capture's name and text, for each match.
    fn captured(pattern: &str, text: &str) -> Vec<Vec<(String, String)>> {
        matches(Language::Rust, pattern, text)
            .iter()
            .map(|m| {
                m.captures
                    .iter()
                    .map(|(n, r)| (n.clone(), text[r.clone()].to_string()))
                    .collect()
            })
            .collect()
    }

    fn pairs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(n, t)| (n.to_string(), t.to_string()))
            .collect()
    }

    #[test]
    fn matches_in_statements_and_expressions() {
        assert_eq!(
            found(
                "foo(@a)",
                "fn main() { foo(1); let x = foo(2); bar(foo(3)); foo(4, 5); }"
            ),
            ["foo(1)", "foo(2)", "foo(3)"]
        );
    }

    #[test]
    fn whitespace_and_comments_do_not_matter() {
        assert_eq!(
            found(
                "foo(1, 2)",
                "fn main() { foo(\n    1, /* one */\n    2 // two\n); }"
            ),
            ["foo(\n    1, /* one */\n    2 // two\n)"]
        );
    }

    #[test]
    fn tokens_the_pattern_leaves_out_are_skipped() {
        assert_eq!(
            found("foo(@a, @b)", "fn main() { foo(x, y,); }"),
            ["foo(x, y,)"]
        );
    }

    #[test]
    fn tokens_the_pattern_has_must_match() {
        assert_eq!(found("@a + @b", "fn main() { x - y; x + y; }"), ["x + y"]);
    }

    #[test]
    fn separators_are_skipped_on_both_sides() {
        let text = "fn main() { foo(x, y,); foo(x, y); }";
        assert_eq!(found("foo(@a, @b)", text), ["foo(x, y,)", "foo(x, y)"]);
        assert_eq!(found("foo(@a, @b,)", text), ["foo(x, y,)", "foo(x, y)"]);
        assert_eq!(
            found_in(Language::Python, "foo(@a,)", "foo(x)\nfoo(x,)\n"),
            ["foo(x)", "foo(x,)"]
        );
    }

    #[test]
    fn children_match_in_the_same_role() {
        assert_eq!(
            found("[@v; @n]", "fn main() { let a = [0, 4]; let b = [0; 4]; }"),
            ["[0; 4]"]
        );
    }

    #[test]
    fn text_between_nodes_must_match() {
        assert_eq!(
            found_in(Language::Python, "'a\\nb'", "x = 'a\\nb'\ny = 'c\\nd'\n"),
            ["'a\\nb'"]
        );
    }

    #[test]
    fn tokens_other_than_separators_must_match() {
        assert!(found("fn f(self) {}", "fn f(&self) {}\n").is_empty());
        assert!(found("|x| x", "fn main() { let f = move |x| x; }").is_empty());
        assert!(
            found_in(
                Language::JavaScript,
                "function f() {}",
                "async function f() {}\n"
            )
            .is_empty()
        );
        assert!(
            found_in(
                Language::Python,
                "def f():\n    pass",
                "async def f():\n    pass\n"
            )
            .is_empty()
        );
        assert_eq!(
            found_within(
                "Some(@x) => @y",
                "fn main() { match a { Some(b) => c, } }",
                "Some(b) => c,"
            ),
            ["Some(b) => c,"]
        );
    }

    #[test]
    fn named_nodes_must_all_match() {
        assert_eq!(
            found("fn @name() {}", "pub fn f() {}\nfn g() {}\n"),
            ["fn g() {}"]
        );
    }

    #[test]
    fn leaves_match_by_text() {
        assert_eq!(found("foo(1)", "fn main() { foo(2); foo(1); }"), ["foo(1)"]);
    }

    #[test]
    fn placeholders_capture() {
        assert_eq!(
            captured("foo(@a, @b)", "fn main() { foo(x + 1, y); }"),
            [pairs(&[("a", "x + 1"), ("b", "y")])]
        );
        assert_eq!(captured("foo(@_)", "fn main() { foo(x); }"), [vec![]]);
    }

    #[test]
    fn captures_keep_the_comments_at_their_ends() {
        assert_eq!(
            captured("foo(@a)", "fn main() { foo(/* c */ 1); }"),
            [pairs(&[("a", "/* c */ 1")])]
        );
        assert_eq!(
            captured(
                "if @c { @body... }",
                "fn main() { if x {\n    // first\n    a();\n    b(); // done\n} }"
            ),
            [pairs(&[
                ("c", "x"),
                ("body", "// first\n    a();\n    b(); // done")
            ])]
        );
    }

    #[test]
    fn runs_take_as_few_siblings_as_they_can() {
        assert_eq!(
            captured("foo(@first, @rest...)", "fn main() { foo(a, b, c); }"),
            [pairs(&[("first", "a"), ("rest", "b, c")])]
        );
        assert_eq!(
            captured("foo(@args...?)", "fn main() { foo(); foo(a, b); }"),
            [pairs(&[("args", "")]), pairs(&[("args", "a, b")])]
        );
        assert_eq!(
            captured("if @c { @body... }", "fn main() { if x { a(); b(); } }"),
            [pairs(&[("c", "x"), ("body", "a(); b();")])]
        );
    }

    #[test]
    fn a_run_is_one_or_more_siblings_unless_it_may_be_empty() {
        let text = "fn main() { foo(a, b, c); foo(d); foo(); }";
        assert_eq!(found("foo(@first, @rest...)", text), ["foo(a, b, c)"]);
        assert_eq!(
            captured("foo(@first, @rest...?)", text),
            [
                pairs(&[("first", "a"), ("rest", "b, c")]),
                pairs(&[("first", "d"), ("rest", "")])
            ]
        );
        assert_eq!(found("foo(@_...)", text), ["foo(a, b, c)", "foo(d)"]);
        assert_eq!(
            found("foo(@_...?)", text),
            ["foo(a, b, c)", "foo(d)", "foo()"]
        );
    }

    #[test]
    fn a_run_left_out_of_the_text_may_be_empty_too() {
        let text = "struct A;\nimpl Display for A {}\nimpl Debug for A { fn fmt() {} }\n";
        assert_eq!(
            found("impl @_ for A { @_... }", text),
            ["impl Debug for A { fn fmt() {} }"]
        );
        assert_eq!(found("impl @_ for A { @_...? }", text).len(), 2);
    }

    #[test]
    fn a_run_after_a_node_matches_the_rest_of_a_longer_node() {
        assert_eq!(
            found(
                "let n = items[i] @rest...;",
                "fn main() { let n = items[i].iter().count(); let n = items[i]; }"
            ),
            ["let n = items[i].iter().count();"]
        );
        assert_eq!(
            captured(
                "let n = items[i] @rest...?;",
                "fn main() { let n = items[i].iter().count(); let n = items[i]; let n = other[i].len(); }"
            ),
            [
                pairs(&[("rest", ".iter().count()")]),
                pairs(&[("rest", "")])
            ]
        );
        assert_eq!(
            found_within(
                "Kind::A(_) => x @_...,",
                "fn main() { match k { Kind::A(_) => x.foo().bar(), Kind::A(_) => y.foo(), } }",
                "Kind::A(_) => x.foo().bar(), Kind::A(_) => y.foo(),"
            ),
            ["Kind::A(_) => x.foo().bar(),"]
        );
        assert_eq!(
            found(
                "let n = x.foo() @_...;",
                "fn main() { let n = x.foo().bar(); let n = x.baz().bar(); }"
            ),
            ["let n = x.foo().bar();"]
        );
    }

    #[test]
    fn a_repeated_name_matches_equal_rests() {
        assert_eq!(
            found(
                "foo(a @t...?, b @t...?)",
                "fn main() { foo(a.x(), b.x()); foo(a.x(), b.y()); foo(a, b); }"
            ),
            ["foo(a.x(), b.x())", "foo(a, b)"]
        );
    }

    #[test]
    fn a_repeated_name_matches_equal_code() {
        assert_eq!(
            found("@x == @x", "fn main() { a == a; a == b; f(1) == f( 1 ); }"),
            ["a == a", "f(1) == f( 1 )"]
        );
    }

    #[test]
    fn matches_do_not_overlap() {
        assert_eq!(
            found("foo(@a)", "fn main() { foo(foo(1)); }"),
            ["foo(foo(1))"]
        );
    }

    #[test]
    fn matches_lie_within_the_range() {
        let text = "fn a() { foo(1); }\nfn b() { foo(2); }\n";
        let p = Pattern::compile(Language::Rust, "foo(@x)", None).unwrap();
        let start = text.find("fn b").unwrap();
        let found = p.find(&Language::Rust.parse(text), text, start..text.len());
        assert_eq!(found.len(), 1);
        assert_eq!(&text[found[0].range.clone()], "foo(2)");
    }

    #[test]
    fn several_statements_match_a_run_of_siblings() {
        assert_eq!(
            found(
                "let a = 1;\nlet b = @v;",
                "fn f() { let a = 1; let b = 2; let c = 3; }"
            ),
            ["let a = 1; let b = 2;"]
        );
    }

    #[test]
    fn several_statements_match_in_every_language() {
        assert_eq!(
            found_in(
                Language::Go,
                "x := 1\ny := @v",
                "package main\n\nfunc main() {\n\tx := 1\n\ty := 2\n\tz := 3\n}\n"
            ),
            ["x := 1\n\ty := 2"]
        );
        assert_eq!(
            found_in(
                Language::Python,
                "x = 1\ny = @v",
                "def f():\n    x = 1\n    y = 2\n"
            ),
            ["x = 1\n    y = 2"]
        );
        assert_eq!(
            found_in(
                Language::JavaScript,
                "f(1);\ng(@v);",
                "function h() { f(1); g(2); }\n"
            ),
            ["f(1); g(2);"]
        );
    }

    #[test]
    fn a_pattern_of_runs_matching_nothing_is_no_match() {
        assert!(found_in(Language::Python, "@a...?\n@b...?", "x = 1\ny = 2\n").is_empty());
    }

    #[test]
    fn a_pattern_matches_at_any_depth() {
        assert_eq!(
            found_in(
                Language::Python,
                "x = 1",
                "x = 1\ndef f():\n    if ok:\n        x = 1\n"
            ),
            ["x = 1", "x = 1"]
        );
    }

    #[test]
    fn a_pattern_parses_in_its_context() {
        let text = "struct S {\n    b: u8,\n}\nfn f(c: u32) {}\n";
        let tree = Language::Rust.parse(text);
        let body = text.find("b: u8,").unwrap()..text.find("\n}").unwrap();
        let context = Context::new(text, &tree, body, None);
        let p = Pattern::compile(Language::Rust, "@a: u32", Some(&context)).unwrap();
        assert!(p.find(&tree, text, 0..text.len()).is_empty());
        let p = Pattern::compile(Language::Rust, "@a: u8", Some(&context)).unwrap();
        let found: Vec<_> = p
            .find(&tree, text, 0..text.len())
            .iter()
            .map(|m| &text[m.range.clone()])
            .collect();
        assert_eq!(found, ["b: u8"]);
    }

    #[test]
    fn placeholders_in_strings_are_literal() {
        assert_eq!(
            found(
                "log(\"user@host\")",
                "fn main() { log(\"user@host\"); log(\"x\"); }"
            ),
            ["log(\"user@host\")"]
        );
    }

    #[test]
    fn a_placeholder_that_is_a_whole_string_is_literal() {
        assert_eq!(
            found(
                "log(\"@who\", @x)",
                "fn main() { log(\"anyone\", 1); log(\"@who\", 2); }"
            ),
            ["log(\"@who\", 2)"]
        );
    }

    #[test]
    fn python_patterns() {
        let text = "class App:\n    def start(self):\n        if ok:\n            run()\n\n    def stop(self, now):\n        pass\n";
        assert_eq!(
            found_in(Language::Python, "def @f(self):\n    @_...", text),
            ["def start(self):\n        if ok:\n            run()"]
        );
        assert_eq!(
            found_in(Language::Python, "\n    if @c:\n        @body...\n", text),
            ["if ok:\n            run()"]
        );
    }

    #[test]
    fn other_languages() {
        assert_eq!(
            found_in(
                Language::Go,
                "fmt.Println(@x)",
                "package main\n\nfunc main() {\n\tfmt.Println(1)\n}\n"
            ),
            ["fmt.Println(1)"]
        );
        assert_eq!(
            found_in(
                Language::TypeScript,
                "console.log(@x)",
                "function f() { console.log(1); }\n"
            ),
            ["console.log(1)"]
        );
    }

    #[test]
    fn go_captures_leave_out_the_statement_newline() {
        let text = "package main\n\nfunc main() {\n\tif c {\n\t\ta()\n\t\tb()\n\t}\n}\n";
        let m = &matches(Language::Go, "if @c { @body... }", text)[0];
        let body = m.captures.iter().find(|(n, _)| n == "body").unwrap();
        assert_eq!(&text[body.1.clone()], "a()\n\t\tb()");
        assert_eq!(&text[m.range.clone()], "if c {\n\t\ta()\n\t\tb()\n\t}");
    }

    #[test]
    fn compile_errors() {
        let Err(PatternError::NoParse { lang, at }) =
            Pattern::compile(Language::Rust, "fn (@a", None)
        else {
            panic!("compiled");
        };
        assert_eq!(lang, Language::Rust);
        assert_eq!(at, "@a");
        let Err(PatternError::NoParse { at, .. }) =
            Pattern::compile(Language::Go, "x := := 1", None)
        else {
            panic!("compiled");
        };
        assert_eq!(at, ":= 1");
        let Err(PatternError::NoParse { at, .. }) =
            Pattern::compile(Language::Rust, "x: i64", None)
        else {
            panic!("compiled");
        };
        assert_eq!(at, "i64");
        assert_eq!(
            Pattern::compile(Language::Rust, "foo@bar(1)", None).unwrap_err(),
            PatternError::Fused {
                placeholder: "@bar".into()
            }
        );
    }

    #[test]
    fn a_lone_placeholder_matches_each_top_level_node() {
        assert_eq!(
            found("@x", "fn a() {}\nfn b() {}\n"),
            ["fn a() {}", "fn b() {}"]
        );
    }

    #[test]
    fn a_pattern_needs_code() {
        assert_eq!(
            Pattern::compile(Language::Rust, "  ", None).unwrap_err(),
            PatternError::Empty
        );
        let Err(PatternError::NoParse { at, .. }) =
            Pattern::compile(Language::Rust, "// only a comment", None)
        else {
            panic!("compiled");
        };
        assert_eq!(at, "// only a comment");
    }
}
