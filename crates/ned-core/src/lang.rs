//! Languages with a linked-in tree-sitter grammar (command-language spec, §1).

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use std::sync::OnceLock;

use tree_sitter::{Parser, Query, Tree};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    Python,
    TypeScript,
    Tsx,
    JavaScript,
    Go,
}

impl Language {
    pub const ALL: [Language; 6] = [
        Language::Rust,
        Language::Python,
        Language::TypeScript,
        Language::Tsx,
        Language::JavaScript,
        Language::Go,
    ];

    /// The name `--lang` takes.
    pub fn name(self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::Python => "python",
            Language::TypeScript => "typescript",
            Language::Tsx => "tsx",
            Language::JavaScript => "javascript",
            Language::Go => "go",
        }
    }

    /// The language of a file, from its extension, then its shebang.
    pub fn detect(path: &str, text: &str) -> Option<Language> {
        let extension = Path::new(path).extension().and_then(|e| e.to_str());
        let by_extension = match extension.unwrap_or_default() {
            "rs" => Some(Language::Rust),
            "py" | "pyi" => Some(Language::Python),
            "ts" | "mts" | "cts" => Some(Language::TypeScript),
            "tsx" => Some(Language::Tsx),
            "js" | "mjs" | "cjs" | "jsx" => Some(Language::JavaScript),
            "go" => Some(Language::Go),
            _ => None,
        };
        by_extension.or_else(|| from_shebang(text))
    }

    pub fn grammar(self) -> tree_sitter::Language {
        match self {
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            Language::Python => tree_sitter_python::LANGUAGE.into(),
            Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Language::Go => tree_sitter_go::LANGUAGE.into(),
        }
    }

    /// The indent unit of a file with no indented lines (§5.2).
    pub fn default_indent(self) -> &'static str {
        match self {
            Language::Go => "\t",
            _ => "    ",
        }
    }

    /// The compiled selector query, `queries/<lang>/selectors.scm`; `None`
    /// until the language has one.
    pub fn selectors(self) -> Option<&'static Query> {
        static QUERIES: [OnceLock<Query>; 6] = [const { OnceLock::new() }; 6];
        let source = match self {
            Language::Rust => include_str!("../../../queries/rust/selectors.scm"),
            _ => return None,
        };
        Some(QUERIES[self as usize].get_or_init(|| {
            Query::new(&self.grammar(), source).expect("selector queries are valid")
        }))
    }

    pub fn parse(self, text: &str) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&self.grammar())
            .expect("linked-in grammars are compatible");
        parser
            .parse(text, None)
            .expect("parsing without a timeout or cancellation succeeds")
    }
}

/// The language of the interpreter named on a `#!` first line, looking past
/// `env` and its flags.
fn from_shebang(text: &str) -> Option<Language> {
    let line = text.lines().next()?.strip_prefix("#!")?;
    let basename = |word: &str| word.rsplit('/').next().unwrap_or(word).to_string();
    let mut words = line.split_whitespace();
    let mut program = basename(words.next()?);
    if program == "env" {
        program = basename(words.find(|w| !w.starts_with('-'))?);
    }
    if program.starts_with("python") {
        Some(Language::Python)
    } else if program == "node" {
        Some(Language::JavaScript)
    } else {
        None
    }
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Language {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Language::ALL
            .into_iter()
            .find(|l| l.name() == s)
            .ok_or_else(|| {
                let names: Vec<_> = Language::ALL.iter().map(|l| l.name()).collect();
                format!(
                    "unknown language `{s}`; expected one of {}",
                    names.join(", ")
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_by_extension() {
        let cases = [
            ("src/a.rs", Language::Rust),
            ("a.py", Language::Python),
            ("a.pyi", Language::Python),
            ("a.ts", Language::TypeScript),
            ("a.mts", Language::TypeScript),
            ("a.cts", Language::TypeScript),
            ("a.tsx", Language::Tsx),
            ("a.js", Language::JavaScript),
            ("a.mjs", Language::JavaScript),
            ("a.cjs", Language::JavaScript),
            ("a.jsx", Language::JavaScript),
            ("./dir.d/a.go", Language::Go),
        ];
        for (path, lang) in cases {
            assert_eq!(Language::detect(path, ""), Some(lang), "{path}");
        }
        assert_eq!(Language::detect("a.txt", ""), None);
        assert_eq!(Language::detect("Makefile", ""), None);
        assert_eq!(Language::detect("a.RS", ""), None);
    }

    #[test]
    fn detects_by_shebang() {
        let cases = [
            ("#!/usr/bin/env python3\nx = 1\n", Some(Language::Python)),
            ("#!/usr/bin/python\n", Some(Language::Python)),
            ("#!/usr/bin/python3.12 -u\n", Some(Language::Python)),
            ("#! /usr/local/bin/node\n", Some(Language::JavaScript)),
            (
                "#!/usr/bin/env -S node --no-warnings\n",
                Some(Language::JavaScript),
            ),
            ("#!/bin/sh\n", None),
            ("#![allow(dead_code)]\n", None),
            ("x = 1\n#!/usr/bin/python\n", None),
            ("", None),
        ];
        for (text, lang) in cases {
            assert_eq!(Language::detect("script", text), lang, "{text:?}");
        }
    }

    #[test]
    fn extension_wins_over_shebang() {
        assert_eq!(
            Language::detect("a.js", "#!/usr/bin/env python3\n"),
            Some(Language::JavaScript)
        );
    }

    #[test]
    fn parses_names() {
        for lang in Language::ALL {
            assert_eq!(lang.name().parse(), Ok(lang));
        }
        assert_eq!(
            "ruby".parse::<Language>(),
            Err("unknown language `ruby`; expected one of rust, python, typescript, tsx, javascript, go".into())
        );
    }

    #[test]
    fn go_defaults_to_tabs() {
        assert_eq!(Language::Go.default_indent(), "\t");
        for lang in Language::ALL.into_iter().filter(|&l| l != Language::Go) {
            assert_eq!(lang.default_indent(), "    ", "{lang}");
        }
    }

    #[test]
    fn every_grammar_parses() {
        for lang in Language::ALL {
            assert!(!lang.parse("").root_node().has_error(), "{lang}");
        }
    }
}
