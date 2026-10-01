//! Matching syntax patterns against syntax trees (command-language spec,
//! §3.10): node by node, skipping comments on both sides and tokens the
//! pattern leaves out.

use std::collections::HashMap;
use std::ops::Range;

use tree_sitter::Tree;

use crate::fragment::Fragment;
use crate::lang::Language;

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
}

impl Pattern {
    /// Compiles the pattern `src` for `lang`, ignoring the common indentation
    /// of its lines.
    pub fn compile(_lang: Language, _src: &str) -> Result<Pattern, PatternError> {
        unimplemented!()
    }

    /// The matches in `tree`, the tree of `text`, that lie within `range`, in
    /// source order. A match inside an earlier one is skipped.
    pub fn find(&self, _tree: &Tree, _text: &str, _range: Range<usize>) -> Vec<PatternMatch> {
        unimplemented!()
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
    fn compile_errors() {
        let Err(PatternError::NoParse { lang, at }) = Pattern::compile(Language::Rust, "fn (@a")
        else {
            panic!("compiled");
        };
        assert_eq!(lang, Language::Rust);
        assert!("fn (@a".ends_with(&at) && !at.is_empty(), "at {at:?}");
        assert_eq!(
            Pattern::compile(Language::Rust, "foo@bar(1)").unwrap_err(),
            PatternError::Fused {
                placeholder: "@bar".into()
            }
        );
    }
}
