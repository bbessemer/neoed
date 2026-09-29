//! Language servers: which one serves each language (spec §1.1).

use std::path::Path;

use crate::config::{Config, ConfigError, Entry, program};
use crate::lang::Language;

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
}
