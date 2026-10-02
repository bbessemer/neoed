//! Matching syntax patterns against syntax trees (command-language spec,
//! §3.10): node by node, skipping comments on both sides and separators the
//! pattern leaves out.

use std::collections::HashMap;
use std::ops::Range;

use std::cmp::Reverse;

use tree_sitter::{Node, Tree};

use crate::fragment::{self, Fragment, FragmentError, children_of};
use crate::lang::Language;
use crate::template::Template;
use crate::text::strip_indent;

/// A pattern compiled for one language: each of its readings.
#[derive(Debug)]
pub struct Pattern {
    readings: Vec<Reading>,
}

#[derive(Debug)]
struct Reading {
    fragment: Fragment,
    /// The holes' nodes, by node id: the hole's name (`None` for `@_`), and
    /// whether it's a run.
    holes: HashMap<usize, (Option<String>, bool)>,
    /// The runs left out of the text, by the id of the node whose children
    /// they lie among: how many children come before each, and its name.
    gaps: HashMap<usize, Vec<(usize, Option<String>)>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternMatch {
    pub range: Range<usize>,
    /// Each named placeholder's capture, in the pattern's order.
    pub captures: Vec<(String, Range<usize>)>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PatternError {
    #[error("doesn't parse as {lang} at `{at}`; add the code around it, or use query{{}}")]
    NoParse { lang: Language, at: String },
    #[error("has `{placeholder}` where a whole node must be; write `@@` for a literal `@`")]
    Fused { placeholder: String },
    #[error("has no code; write the code to match between the backquotes")]
    Empty,
}

impl Pattern {
    /// Compiles the pattern `src` for `lang`, ignoring the common indentation
    /// of its lines.
    pub fn compile(lang: Language, src: &str) -> Result<Pattern, PatternError> {
        let code = strip_indent(src).join("\n");
        if code.trim().is_empty() {
            return Err(PatternError::Empty);
        }

        let template = Template::parse(&code);
        let fragments = fragment::parse(lang, &template).map_err(|e| match e {
            FragmentError::NoParse { lang, at } => PatternError::NoParse {
                lang,
                at: excerpt(&code, at),
            },
            FragmentError::Fused { span } => PatternError::Fused {
                placeholder: code[span].to_string(),
            },
        })?;
        let readings = fragments
            .into_iter()
            .map(|fragment| {
                let holes = template
                    .holes()
                    .enumerate()
                    .filter_map(|(i, h)| Some((fragment.hole(i)?.id(), (h.name.clone(), h.many))))
                    .collect();
                let mut gaps: HashMap<usize, Vec<(usize, Option<String>)>> = HashMap::new();
                for (i, h) in template.holes().enumerate() {
                    if let Some((node, before)) = fragment.gap(i) {
                        gaps.entry(node.id())
                            .or_default()
                            .push((before, h.name.clone()));
                    }
                }
                Reading {
                    fragment,
                    holes,
                    gaps,
                }
            })
            .collect();
        Ok(Pattern { readings })
    }

    /// The matches in `tree`, the tree of `text`, that lie within `range`, in
    /// source order. A match inside an earlier one is skipped.
    pub fn find(&self, tree: &Tree, text: &str, range: Range<usize>) -> Vec<PatternMatch> {
        let mut all = Vec::new();
        for reading in &self.readings {
            reading.find(tree.root_node(), text, &range, &mut all);
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

impl Reading {
    /// Pushes the matches in `root`'s subtree that lie within `range`.
    fn find<'t>(
        &self,
        root: Node<'t>,
        text: &'t str,
        range: &Range<usize>,
        out: &mut Vec<PatternMatch>,
    ) {
        let roots: Vec<Elem> = self.fragment.roots().into_iter().map(Elem::Node).collect();
        self.walk(&roots, root, text, range, out);
    }

    fn walk<'p, 't>(
        &'p self,
        roots: &[Elem<'p>],
        node: Node<'t>,
        text: &'t str,
        range: &Range<usize>,
        out: &mut Vec<PatternMatch>,
    ) {
        if node.end_byte() <= range.start || node.start_byte() >= range.end {
            return;
        }
        let inside = |r: &Range<usize>| range.start <= r.start && r.end <= range.end;
        let children = children_of(node);
        if let [Elem::Node(root)] = roots {
            // The grammar's top node is the whole file, not code to match.
            let candidate = node.is_named()
                && !node.is_extra()
                && node.parent().is_some()
                && (self.holes.contains_key(&root.id()) || root.kind_id() == node.kind_id());
            if candidate && inside(&node.byte_range()) {
                let mut m = Matcher::new(self, text);
                if m.node(*root, node) {
                    out.push(m.matched(node.byte_range()));
                }
            }
        } else {
            for (i, start) in children.iter().enumerate() {
                if !start.is_named() || !inside(&start.byte_range()) {
                    continue;
                }
                let mut m = Matcher::new(self, text);
                // A run of roots may match no siblings, which is no match.
                if let Some(end) = m.seq(roots, &children[i..], false).filter(|&n| n > 0) {
                    let r = start.start_byte()..children[i + end - 1].end_byte();
                    if inside(&r) {
                        out.push(m.matched(r));
                    }
                }
            }
        }
        for child in children {
            self.walk(roots, child, text, range, out);
        }
    }
}

/// An element of a pattern node's children: a node, or a run left out of
/// the text (a gap).
#[derive(Clone, Copy)]
enum Elem<'p> {
    Node(Node<'p>),
    Run(&'p Option<String>),
}

/// `range` without the whitespace at its ends: Go ends a statement node with
/// its newline.
fn trim(text: &str, range: Range<usize>) -> Range<usize> {
    let s = &text[range.clone()];
    let start = range.start + (s.len() - s.trim_start().len());
    start..(start + s.trim().len())
}

/// The range from `first` to `last`, siblings, with the comments between
/// them and their neighbours, which a capture keeps.
fn with_comments(first: Node<'_>, last: Node<'_>) -> Range<usize> {
    let mut start = first;
    while let Some(p) = start.prev_sibling().filter(|p| p.is_extra()) {
        start = p;
    }
    let mut end = last;
    while let Some(n) = end.next_sibling().filter(|n| n.is_extra()) {
        end = n;
    }
    start.start_byte()..end.end_byte()
}

/// A match of one reading in progress: what its placeholders have bound.
struct Matcher<'p, 't> {
    reading: &'p Reading,
    text: &'t str,
    binds: Vec<Bind<'t>>,
}

struct Bind<'t> {
    name: String,
    nodes: Vec<Node<'t>>,
    range: Range<usize>,
}

impl<'p, 't> Matcher<'p, 't> {
    fn new(reading: &'p Reading, text: &'t str) -> Self {
        Matcher {
            reading,
            text,
            binds: Vec::new(),
        }
    }

    fn matched(self, range: Range<usize>) -> PatternMatch {
        let text = self.text;
        PatternMatch {
            range: trim(text, range),
            captures: self
                .binds
                .into_iter()
                .map(|b| (b.name, trim(text, b.range)))
                .collect(),
        }
    }

    fn hole(&self, p: Node<'_>) -> Option<&'p (Option<String>, bool)> {
        self.reading.holes.get(&p.id())
    }

    fn pattern_text(&self, p: Node<'_>) -> &'p str {
        &self.reading.fragment.text[p.byte_range()]
    }

    /// Whether target node `t` is a separator: `,`, `;`, or a line break.
    fn separator(&self, t: Node<'t>) -> bool {
        let text = &self.text[t.byte_range()];
        !t.is_named() && (text == "," || text == ";" || text.trim().is_empty())
    }

    /// The elements of pattern node `p`'s children: its children, with its gaps
    /// among them.
    fn elems(&self, p: Node<'p>) -> Vec<Elem<'p>> {
        let mut elems: Vec<Elem<'p>> = children_of(p).into_iter().map(Elem::Node).collect();
        for (k, (before, name)) in self
            .reading
            .gaps
            .get(&p.id())
            .into_iter()
            .flatten()
            .enumerate()
        {
            elems.insert(before + k, Elem::Run(name));
        }
        elems
    }

    /// Whether pattern node `p` matches target node `t`.
    fn node(&mut self, p: Node<'p>, t: Node<'t>) -> bool {
        if let Some((name, _)) = self.hole(p) {
            return self.bind(name, vec![t], with_comments(t, t));
        }
        if p.kind_id() != t.kind_id() {
            return false;
        }
        let (pc, tc) = (self.elems(p), children_of(t));
        if pc.is_empty() && tc.is_empty() {
            return !p.is_named() || self.pattern_text(p) == &self.text[t.byte_range()];
        }
        self.seq(&pc, &tc, true).is_some()
    }

    /// Matches the pattern nodes `ps` against a prefix of the target nodes
    /// `ts`, or all of them if `all`, skipping target separators the pattern
    /// leaves out. The number of target nodes matched, if they match.
    fn seq(&mut self, ps: &[Elem<'p>], ts: &[Node<'t>], all: bool) -> Option<usize> {
        let Some((&p, rest)) = ps.split_first() else {
            return (!all || ts.iter().all(|t| self.separator(*t))).then_some(0);
        };
        let saved = self.binds.len();
        let run = match p {
            Elem::Run(name) => Some(name),
            Elem::Node(n) => match self.hole(n) {
                Some((name, true)) => Some(name),
                _ => None,
            },
        };
        if let Some(name) = run {
            // A run takes as few siblings as it can.
            let mut end = 0;
            loop {
                let run: Vec<Node<'t>> =
                    ts[..end].iter().copied().filter(|n| n.is_named()).collect();
                let range = match (run.first(), run.last()) {
                    (Some(first), Some(last)) => with_comments(*first, *last),
                    _ => {
                        let at = ts.get(end).map_or(0, |t| t.start_byte());
                        at..at
                    }
                };
                if self.bind(name, run, range)
                    && let Some(n) = self.seq(rest, &ts[end..], all)
                {
                    return Some(end + n);
                }
                self.binds.truncate(saved);
                end += ts[end..].iter().position(|n| n.is_named())? + 1;
            }
        }
        let Elem::Node(p) = p else {
            unreachable!("a gap is a run");
        };
        for (i, &t) in ts.iter().enumerate() {
            let matched = if p.is_named() || self.hole(p).is_some() {
                t.is_named() && self.node(p, t)
            } else {
                !t.is_named() && p.kind_id() == t.kind_id()
            };
            if matched {
                let n = self.seq(rest, &ts[i + 1..], all);
                if n.is_none() {
                    self.binds.truncate(saved);
                }
                return n.map(|n| i + 1 + n);
            }
            self.binds.truncate(saved);
            // Only a separator the pattern leaves out can be skipped: any other
            // token, such as `async` or `&`, changes what the code means.
            if !self.separator(t) {
                return None;
            }
        }
        None
    }

    /// Binds `name` to `nodes`, or checks them against its earlier binding.
    fn bind(&mut self, name: &Option<String>, nodes: Vec<Node<'t>>, range: Range<usize>) -> bool {
        let Some(name) = name else {
            return true;
        };
        if let Some(earlier) = self.binds.iter().find(|b| b.name == *name) {
            return earlier.nodes.len() == nodes.len()
                && earlier
                    .nodes
                    .iter()
                    .zip(&nodes)
                    .all(|(a, b)| self.equal(*a, *b));
        }
        self.binds.push(Bind {
            name: name.clone(),
            nodes,
            range,
        });
        true
    }

    /// Whether two target nodes are the same code, ignoring whitespace and
    /// comments.
    fn equal(&self, a: Node<'t>, b: Node<'t>) -> bool {
        let (ac, bc) = (children_of(a), children_of(b));
        a.kind_id() == b.kind_id()
            && ac.len() == bc.len()
            && if ac.is_empty() {
                self.text[a.byte_range()] == self.text[b.byte_range()]
            } else {
                ac.iter().zip(&bc).all(|(x, y)| self.equal(*x, *y))
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(lang: Language, pattern: &str, text: &str) -> Vec<PatternMatch> {
        let p = Pattern::compile(lang, pattern).unwrap_or_else(|e| panic!("{pattern}: {e}"));
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
    fn only_separators_the_pattern_leaves_out_are_skipped() {
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
            found("Some(@x) => @y", "fn main() { match a { Some(b) => c, } }"),
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
            captured(
                "foo(@first, @rest...)",
                "fn main() { foo(a, b, c); foo(d); }"
            ),
            [pairs(&[("first", "a"), ("rest", "b, c")])]
        );
        assert_eq!(
            captured("foo(@args...)", "fn main() { foo(); foo(a, b); }"),
            [pairs(&[("args", "")]), pairs(&[("args", "a, b")])]
        );
        assert_eq!(
            captured("if @c { @body... }", "fn main() { if x { a(); b(); } }"),
            [pairs(&[("c", "x"), ("body", "a(); b();")])]
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
        let p = Pattern::compile(Language::Rust, "foo(@x)").unwrap();
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
        assert!(found_in(Language::Python, "@a...\n@b...", "x = 1\ny = 2\n").is_empty());
    }

    #[test]
    fn a_pattern_matches_any_of_its_readings() {
        assert_eq!(
            found("@a: u32", "struct S { a: u32 }\nfn f(b: u32) {}\n"),
            ["a: u32", "b: u32"]
        );
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
        let Err(PatternError::NoParse { lang, at }) = Pattern::compile(Language::Rust, "fn (@a")
        else {
            panic!("compiled");
        };
        assert_eq!(lang, Language::Rust);
        assert_eq!(at, "@a");
        let Err(PatternError::NoParse { at, .. }) = Pattern::compile(Language::Go, "x := := 1")
        else {
            panic!("compiled");
        };
        assert_eq!(at, ":= 1");
        assert_eq!(
            Pattern::compile(Language::Rust, "foo@bar(1)").unwrap_err(),
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
            Pattern::compile(Language::Rust, "  ").unwrap_err(),
            PatternError::Empty
        );
        let Err(PatternError::NoParse { at, .. }) =
            Pattern::compile(Language::Rust, "// only a comment")
        else {
            panic!("compiled");
        };
        assert_eq!(at, "// only a comment");
    }
}
