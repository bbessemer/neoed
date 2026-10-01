//! Languages with a linked-in tree-sitter grammar (command-language spec, §1).

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use tree_sitter::{Parser, Query, Tree};

use crate::fragment::{Builder, NodeTypes};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Rust,
    Python,
    TypeScript,
    Tsx,
    JavaScript,
    Go,
    Markdown,
}

impl Language {
    pub const ALL: [Language; 7] = [
        Language::Rust,
        Language::Python,
        Language::TypeScript,
        Language::Tsx,
        Language::JavaScript,
        Language::Go,
        Language::Markdown,
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
            Language::Markdown => "markdown",
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
            "md" | "markdown" => Some(Language::Markdown),
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
            Language::Markdown => tree_sitter_md::LANGUAGE.into(),
        }
    }

    /// The indent unit of a file with no indented lines (§5.2).
    pub fn default_indent(self) -> &'static str {
        match self {
            Language::Go => "\t",
            Language::Markdown => "  ",
            _ => "    ",
        }
    }

    /// The compiled selector query, `queries/<lang>/selectors.scm`.
    pub fn selectors(self) -> &'static Query {
        static QUERIES: [OnceLock<Query>; 7] = [const { OnceLock::new() }; 7];
        let source = match self {
            Language::Rust => include_str!("../../../queries/rust/selectors.scm"),
            Language::Markdown => include_str!("../../../queries/markdown/selectors.scm"),
            Language::Python => include_str!("../../../queries/python/selectors.scm"),
            Language::Go => include_str!("../../../queries/go/selectors.scm"),
            Language::JavaScript => concat!(
                include_str!("../../../queries/ecma/selectors.scm"),
                include_str!("../../../queries/javascript/selectors.scm")
            ),
            Language::TypeScript | Language::Tsx => concat!(
                include_str!("../../../queries/ecma/selectors.scm"),
                include_str!("../../../queries/typescript/selectors.scm")
            ),
        };
        QUERIES[self as usize].get_or_init(|| {
            Query::new(&self.grammar(), source).expect("selector queries are valid")
        })
    }

    /// The builders of fragments that only parse inside other code,
    /// `queries/<lang>/builders.scm` (§3.10).
    pub fn builders(self) -> &'static [Builder] {
        static BUILDERS: [OnceLock<Vec<Builder>>; 7] = [const { OnceLock::new() }; 7];
        let source = match self {
            Language::Rust => include_str!("../../../queries/rust/builders.scm"),
            Language::Python => include_str!("../../../queries/python/builders.scm"),
            Language::Go => include_str!("../../../queries/go/builders.scm"),
            Language::JavaScript => include_str!("../../../queries/ecma/builders.scm"),
            Language::TypeScript | Language::Tsx => concat!(
                include_str!("../../../queries/ecma/builders.scm"),
                include_str!("../../../queries/typescript/builders.scm")
            ),
            Language::Markdown => "",
        };
        BUILDERS[self as usize].get_or_init(|| {
            Builder::read_all(source, self.node_types())
                .unwrap_or_else(|e| panic!("{self} builders, bytes {:?}: {e}", e.span))
        })
    }

    /// Which node kinds each kind has as fields and can contain, from the
    /// grammar's `node-types.json`.
    pub fn node_types(self) -> &'static NodeTypes {
        static TYPES: [OnceLock<NodeTypes>; 7] = [const { OnceLock::new() }; 7];
        TYPES[self as usize].get_or_init(|| {
            NodeTypes::read(match self {
                Language::Rust => tree_sitter_rust::NODE_TYPES,
                Language::Python => tree_sitter_python::NODE_TYPES,
                Language::TypeScript => tree_sitter_typescript::TYPESCRIPT_NODE_TYPES,
                Language::Tsx => tree_sitter_typescript::TSX_NODE_TYPES,
                Language::JavaScript => tree_sitter_javascript::NODE_TYPES,
                Language::Go => tree_sitter_go::NODE_TYPES,
                Language::Markdown => tree_sitter_md::NODE_TYPES_BLOCK,
            })
        })
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
            ("README.md", Language::Markdown),
            ("notes.markdown", Language::Markdown),
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
            Err("unknown language `ruby`; expected one of rust, python, typescript, tsx, javascript, go, markdown".into())
        );
    }

    #[test]
    fn default_indents() {
        assert_eq!(Language::Go.default_indent(), "\t");
        assert_eq!(Language::Markdown.default_indent(), "  ");
        let others = Language::ALL
            .into_iter()
            .filter(|&l| l != Language::Go && l != Language::Markdown);
        for lang in others {
            assert_eq!(lang.default_indent(), "    ", "{lang}");
        }
    }

    #[test]
    fn every_grammar_parses() {
        for lang in Language::ALL {
            assert!(!lang.parse("").root_node().has_error(), "{lang}");
        }
    }

    #[test]
    fn builders_and_node_types_load() {
        for lang in Language::ALL {
            assert!(
                lang.node_types()
                    .has_kind(lang.parse("").root_node().kind()),
                "{lang}"
            );
            let builders = lang.builders();
            assert_eq!(builders.is_empty(), lang == Language::Markdown, "{lang}");
        }
    }
}
