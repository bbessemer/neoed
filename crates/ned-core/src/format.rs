//! External formatters and their configuration (command-language spec, §6.4).

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::lang::Language;

/// A file's formatter, ready to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Formatter {
    /// The basename of its program, as shown in the output.
    pub name: String,
    /// Commands with placeholders and programs resolved, tried in order while
    /// the program isn't found.
    pub commands: Vec<Vec<String>>,
    /// The directory to run in: the file's.
    pub dir: PathBuf,
}

/// The formatter settings from every config file read so far.
#[derive(Debug)]
pub struct Formatters {
    user: Option<Layer>,
    /// Parsed `.ned.toml` files by directory; `None` where there is none.
    layers: HashMap<PathBuf, Option<Layer>>,
}

/// The settings from one config file.
#[derive(Debug)]
struct Layer {
    /// The directory holding the config file.
    dir: PathBuf,
    format: HashMap<Language, Entry>,
}

#[derive(Debug)]
enum Entry {
    Command(Vec<String>),
    Off,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    /// The config file, with the line and column when known.
    pub location: String,
    pub message: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: invalid config: {}", self.location, self.message)
    }
}

impl std::error::Error for ConfigError {}

/// The user config file: `$XDG_CONFIG_HOME/ned/config.toml`, else
/// `~/.config/ned/config.toml`.
pub fn user_config() -> Option<PathBuf> {
    None
}

impl Formatters {
    /// Reads the user config at `user`, if it exists.
    pub fn new(user: Option<&Path>) -> Result<Self, ConfigError> {
        let _ = user;
        Ok(Formatters {
            user: None,
            layers: HashMap::new(),
        })
    }

    /// The formatter for `path` in `lang`, or `None` if it has none.
    pub fn get(&mut self, path: &Path, lang: Language) -> Result<Option<Formatter>, ConfigError> {
        let _ = (path, lang, &self.user, &mut self.layers);
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn tree(files: &[(&str, &str)]) -> TempDir {
        let root = TempDir::new().unwrap();
        for (path, text) in files {
            let path = root.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        root
    }

    fn lookup(
        root: &TempDir,
        user: Option<&str>,
        file: &str,
        lang: Language,
    ) -> Result<Option<Formatter>, ConfigError> {
        let user = user.map(|text| {
            let path = root.path().join("home/.config/ned/config.toml");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, text).unwrap();
            path
        });
        Formatters::new(user.as_deref())?.get(&root.path().join(file), lang)
    }

    fn commands(
        root: &TempDir,
        user: Option<&str>,
        file: &str,
        lang: Language,
    ) -> Vec<Vec<String>> {
        lookup(root, user, file, lang)
            .unwrap()
            .map(|f| f.commands)
            .unwrap_or_default()
    }

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    fn at(root: &TempDir, path: &str) -> String {
        root.path().join(path).to_str().unwrap().to_string()
    }

    fn error(root: &TempDir, user: Option<&str>, file: &str) -> ConfigError {
        lookup(root, user, file, Language::Rust).unwrap_err()
    }

    #[test]
    fn defaults() {
        let root = tree(&[]);
        let a = |name: &str| at(&root, name);
        let rust = lookup(&root, None, "src/a.rs", Language::Rust)
            .unwrap()
            .unwrap();
        assert_eq!(
            rust,
            Formatter {
                name: "rustfmt".into(),
                commands: vec![argv(&["rustfmt", "--edition", "2015"])],
                dir: root.path().join("src"),
            }
        );
        assert_eq!(
            commands(&root, None, "a.go", Language::Go),
            [argv(&["gofmt"])]
        );
        assert_eq!(
            commands(&root, None, "a.py", Language::Python),
            [
                argv(&["ruff", "format", "--stdin-filename", &a("a.py"), "-"]),
                argv(&["black", "-q", "--stdin-filename", &a("a.py"), "-"]),
            ]
        );
        for (file, lang) in [
            ("a.ts", Language::TypeScript),
            ("a.tsx", Language::Tsx),
            ("a.js", Language::JavaScript),
        ] {
            assert_eq!(
                commands(&root, None, file, lang),
                [argv(&["prettier", "--stdin-filepath", &a(file)])],
                "{lang}"
            );
        }
    }

    #[test]
    fn a_missing_user_config_is_no_config() {
        let root = tree(&[]);
        let missing = root.path().join("nope/config.toml");
        let found = Formatters::new(Some(&missing))
            .unwrap()
            .get(&root.path().join("a.go"), Language::Go)
            .unwrap();
        assert_eq!(found.unwrap().commands, [argv(&["gofmt"])]);
    }

    #[test]
    fn user_config_overrides_defaults() {
        let root = tree(&[]);
        let user = "[format]\nrust = [\"rustfmt\", \"--edition\", \"2021\"]\ngo = false\n";
        assert_eq!(
            commands(&root, Some(user), "a.rs", Language::Rust),
            [argv(&["rustfmt", "--edition", "2021"])]
        );
        assert_eq!(lookup(&root, Some(user), "a.go", Language::Go), Ok(None));
        assert_eq!(
            commands(&root, Some(user), "a.py", Language::Python).len(),
            2
        );
    }

    #[test]
    fn nearer_configs_win() {
        let root = tree(&[
            (
                ".ned.toml",
                "[format]\ngo = [\"far\"]\npython = [\"far\"]\njavascript = false\n",
            ),
            (
                "sub/.ned.toml",
                "[format]\ngo = [\"near\"]\npython = false\njavascript = [\"near\"]\n",
            ),
            ("sub/deeper/keep", ""),
        ]);
        let user = Some("[format]\ngo = [\"user\"]\nrust = [\"user\"]\n");
        let file = "sub/deeper/a";
        assert_eq!(commands(&root, user, file, Language::Go), [argv(&["near"])]);
        assert_eq!(lookup(&root, user, file, Language::Python), Ok(None));
        assert_eq!(
            commands(&root, user, file, Language::JavaScript),
            [argv(&["near"])]
        );
        assert_eq!(
            commands(&root, user, file, Language::Rust),
            [argv(&["user"])]
        );
        assert_eq!(
            commands(&root, user, "a", Language::Python),
            [argv(&["far"])]
        );
        assert_eq!(lookup(&root, user, "a", Language::JavaScript), Ok(None));
    }

    #[test]
    fn a_config_may_leave_out_format() {
        let root = tree(&[(".ned.toml", "")]);
        assert_eq!(
            commands(&root, None, "a.go", Language::Go),
            [argv(&["gofmt"])]
        );
    }

    #[test]
    fn unknown_language() {
        let root = tree(&[(".ned.toml", "[format]\nruby = [\"rubocop\"]\n")]);
        let err = error(&root, None, "a.rs");
        assert_eq!(err.location, format!("{}:2:1", at(&root, ".ned.toml")));
        assert_eq!(
            err.message,
            "unknown language `ruby`; expected one of rust, python, typescript, tsx, javascript, go"
        );
    }

    #[test]
    fn unknown_table() {
        let root = tree(&[(".ned.toml", "[lsp]\nrust = [\"rust-analyzer\"]\n")]);
        let err = error(&root, None, "a.rs");
        assert!(
            err.location
                .starts_with(&format!("{}:1:", at(&root, ".ned.toml"))),
            "{err}"
        );
        assert!(err.message.contains("unknown field `lsp`"), "{err}");
    }

    #[test]
    fn wrong_types() {
        for (value, message) in [
            (
                "\"rustfmt\"",
                "`rust` must be a command (an array of strings) or false",
            ),
            (
                "true",
                "`rust` must be a command (an array of strings) or false",
            ),
            (
                "[\"rustfmt\", 2]",
                "`rust` must be a command (an array of strings) or false",
            ),
            ("[]", "`rust` has an empty command"),
        ] {
            let root = tree(&[(".ned.toml", &format!("[format]\nrust = {value}\n"))]);
            let err = error(&root, None, "a.rs");
            assert_eq!(
                err.location,
                format!("{}:2:8", at(&root, ".ned.toml")),
                "{value}"
            );
            assert_eq!(err.message, message, "{value}");
        }
    }

    #[test]
    fn invalid_toml() {
        let root = tree(&[(".ned.toml", "[format\n")]);
        let err = error(&root, None, "a.rs");
        assert!(
            err.location
                .starts_with(&format!("{}:1:", at(&root, ".ned.toml"))),
            "{err}"
        );
        assert!(!err.message.contains('\n'), "{err}");
    }

    #[test]
    fn errors_in_the_user_config() {
        let root = tree(&[]);
        let err = error(&root, Some("format = 1\n"), "a.rs");
        assert!(
            err.location
                .starts_with(&format!("{}:1:", at(&root, "home/.config/ned/config.toml"))),
            "{err}"
        );
    }

    #[test]
    fn edition_from_the_package() {
        let root = tree(&[(
            "Cargo.toml",
            "[package]\nname = \"x\"\nedition = \"2021\"\n",
        )]);
        assert_eq!(
            commands(&root, None, "src/bin/a.rs", Language::Rust),
            [argv(&["rustfmt", "--edition", "2021"])]
        );
    }

    #[test]
    fn edition_from_the_workspace() {
        let root = tree(&[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.package]\nedition = \"2024\"\n",
            ),
            (
                "crates/x/Cargo.toml",
                "[package]\nname = \"x\"\nedition.workspace = true\n",
            ),
        ]);
        assert_eq!(
            commands(&root, None, "crates/x/src/a.rs", Language::Rust),
            [argv(&["rustfmt", "--edition", "2024"])]
        );
    }

    #[test]
    fn placeholders_inside_arguments() {
        let root = tree(&[("Cargo.toml", "[package]\nedition = \"2018\"\n")]);
        let user = "[format]\nrust = [\"rustfmt\", \"--edition={edition}\", \"--file={path}\"]\n";
        assert_eq!(
            commands(&root, Some(user), "a.rs", Language::Rust),
            [argv(&[
                "rustfmt",
                "--edition=2018",
                &format!("--file={}", at(&root, "a.rs"))
            ])]
        );
    }

    #[test]
    fn programs_in_node_modules() {
        let root = tree(&[
            ("node_modules/.bin/prettier", ""),
            ("web/app/node_modules/.bin/prettier", ""),
        ]);
        let found = lookup(&root, None, "web/src/a.ts", Language::TypeScript)
            .unwrap()
            .unwrap();
        assert_eq!(found.name, "prettier");
        assert_eq!(
            found.commands[0][0],
            at(&root, "node_modules/.bin/prettier")
        );
        assert_eq!(
            commands(&root, None, "web/app/a.ts", Language::TypeScript)[0][0],
            at(&root, "web/app/node_modules/.bin/prettier")
        );
    }

    #[test]
    fn program_paths_are_relative_to_their_config() {
        let root = tree(&[(
            ".ned.toml",
            "[format]\ngo = [\"tools/fmt.sh\", \"-x\"]\nrust = [\"/bin/fmt\"]\n",
        )]);
        let found = lookup(&root, None, "sub/a.go", Language::Go)
            .unwrap()
            .unwrap();
        assert_eq!(found.name, "fmt.sh");
        assert_eq!(found.commands, [argv(&[&at(&root, "tools/fmt.sh"), "-x"])]);
        assert_eq!(
            commands(&root, None, "sub/a.rs", Language::Rust),
            [argv(&["/bin/fmt"])]
        );
    }
}
