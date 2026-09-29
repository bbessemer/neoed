//! Language servers: which one serves each language, and what `ned` asks of
//! them (spec §1.1, §4.1).

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::config::{Config, ConfigError, Entry, program};
use crate::lang::Language;

/// A file's text, as `ned` sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    /// Absolute.
    pub path: PathBuf,
    pub lang: Language,
    pub text: String,
}

/// A position in LSP terms: a 0-based line, and UTF-16 code units into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

/// Diagnostic severities, most severe first; the `check` LEVEL words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub start: Position,
    pub end: Position,
    pub severity: Severity,
    pub message: String,
    /// The tool that reported it, e.g. `rustc`.
    pub source: Option<String>,
    pub code: Option<String>,
}

/// Diagnostics for some documents, with the workspace's `[check] show`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnosis {
    pub show: Severity,
    /// One per document, in order; `None` where its language has no server.
    pub files: Vec<Option<Vec<Diagnostic>>>,
}

/// Why language servers couldn't answer; the message ends with a fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspFailure(pub String);

/// What `ned` asks of the workspace's language servers.
pub trait Lsp {
    /// Diagnostics for each of `documents`, as their text stands.
    fn diagnose(&mut self, documents: &[Document]) -> Result<Diagnosis, LspFailure>;
}

impl Severity {
    pub const ALL: [Severity; 4] = [
        Severity::Error,
        Severity::Warning,
        Severity::Info,
        Severity::Hint,
    ];

    pub fn name(self) -> &'static str {
        todo!()
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Severity {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        let _ = s;
        todo!()
    }
}

impl Config {
    /// The server command for `lang` in the workspace at `root`, with its
    /// program resolved, or `None` if the language has no server.
    pub fn server(
        &mut self,
        root: &Path,
        lang: Language,
    ) -> Result<Option<Vec<String>>, ConfigError> {
        let configured = self.layers(root)?.into_iter().find_map(|layer| {
            let command = match layer.lsp.get(&lang)? {
                Entry::Command(command) => Some(command.clone()),
                Entry::Off => None,
            };
            Some((command, layer.dir.clone()))
        });
        let (command, base) = match configured {
            Some((command, base)) => (command, Some(base)),
            None => (
                default_server(lang).map(|c| c.iter().map(|w| w.to_string()).collect()),
                None,
            ),
        };
        Ok(command.map(|mut command: Vec<String>| {
            command[0] = program(&command[0], base.as_deref(), root);
            command
        }))
    }
}

fn default_server(lang: Language) -> Option<&'static [&'static str]> {
    match lang {
        Language::Rust => Some(&["rust-analyzer"]),
        Language::Go => Some(&["gopls"]),
        Language::Python => Some(&["pyright-langserver", "--stdio"]),
        Language::TypeScript | Language::Tsx | Language::JavaScript => {
            Some(&["typescript-language-server", "--stdio"])
        }
        Language::Markdown => None,
    }
}

/// The LSP `languageId` of `lang`.
pub fn language_id(lang: Language) -> &'static str {
    match lang {
        Language::Tsx => "typescriptreact",
        lang => lang.name(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::config::tests::tree;

    fn server(root: &Path, lang: Language) -> Option<Vec<String>> {
        Config::new(None).unwrap().server(root, lang).unwrap()
    }

    fn argv(words: &[&str]) -> Option<Vec<String>> {
        Some(words.iter().map(|w| w.to_string()).collect())
    }

    #[test]
    fn default_servers() {
        let root = tree(&[]);
        let root = root.path();
        assert_eq!(server(root, Language::Rust), argv(&["rust-analyzer"]));
        assert_eq!(server(root, Language::Go), argv(&["gopls"]));
        assert_eq!(
            server(root, Language::Python),
            argv(&["pyright-langserver", "--stdio"])
        );
        for lang in [Language::TypeScript, Language::Tsx, Language::JavaScript] {
            assert_eq!(
                server(root, lang),
                argv(&["typescript-language-server", "--stdio"])
            );
        }
        assert_eq!(server(root, Language::Markdown), None);
    }

    #[test]
    fn config_overrides_and_turns_off_servers() {
        let root = tree(&[(
            ".ned.toml",
            "[lsp]\npython = [\"basedpyright-langserver\", \"--stdio\"]\nrust = false\nmarkdown = [\"marksman\"]\n",
        )]);
        let root = root.path();
        assert_eq!(
            server(root, Language::Python),
            argv(&["basedpyright-langserver", "--stdio"])
        );
        assert_eq!(server(root, Language::Rust), None);
        assert_eq!(server(root, Language::Markdown), argv(&["marksman"]));
    }

    #[test]
    fn the_nearest_config_above_the_root_wins_over_the_user_config() {
        let dir = tree(&[
            (
                "config.toml",
                "[lsp]\ngo = [\"user-gopls\"]\nrust = [\"user-ra\"]\n",
            ),
            (".ned.toml", "[lsp]\ngo = [\"outer-gopls\"]\n"),
            ("ws/.ned.toml", "[lsp]\ngo = [\"ws-gopls\"]\n"),
            ("ws/sub/.ned.toml", "[lsp]\ngo = [\"sub-gopls\"]\n"),
        ]);
        let mut config = Config::new(Some(&dir.path().join("config.toml"))).unwrap();
        let root = dir.path().join("ws");
        assert_eq!(
            config.server(&root, Language::Go).unwrap(),
            argv(&["ws-gopls"])
        );
        assert_eq!(
            config.server(&root, Language::Rust).unwrap(),
            argv(&["user-ra"])
        );
    }

    #[test]
    fn programs_are_found_like_formatters() {
        let dir = tree(&[
            ("ws/.ned.toml", "[lsp]\ngo = [\"bin/gopls\"]\n"),
            ("node_modules/.bin/typescript-language-server", ""),
        ]);
        let bin = dir
            .path()
            .join("node_modules/.bin/typescript-language-server");
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        let root = dir.path().join("ws");
        let gopls = dir.path().join("ws/bin/gopls");
        assert_eq!(
            server(&root, Language::Go),
            argv(&[gopls.to_str().unwrap()])
        );
        assert_eq!(
            server(&root, Language::TypeScript),
            argv(&[bin.to_str().unwrap(), "--stdio"])
        );
    }

    #[test]
    fn language_ids() {
        let ids: Vec<_> = Language::ALL.iter().map(|&l| language_id(l)).collect();
        assert_eq!(
            ids,
            [
                "rust",
                "python",
                "typescript",
                "typescriptreact",
                "javascript",
                "go",
                "markdown"
            ]
        );
    }

    #[test]
    fn severities_order_and_parse() {
        assert!(Severity::Error < Severity::Warning && Severity::Info < Severity::Hint);
        for severity in Severity::ALL {
            assert_eq!(severity.name().parse(), Ok(severity));
            assert_eq!(severity.to_string(), severity.name());
        }
        let names: Vec<_> = Severity::ALL.iter().map(|s| s.name()).collect();
        assert_eq!(names, ["error", "warning", "info", "hint"]);
        assert_eq!("warnings".parse::<Severity>(), Err(()));
    }
}
