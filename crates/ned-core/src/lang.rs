//! Languages with a linked-in tree-sitter grammar (command-language spec, §1).

use std::fmt;
use std::str::FromStr;

use tree_sitter::{Parser, Tree};

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
        let _ = (path, text);
        None
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
        "    "
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
