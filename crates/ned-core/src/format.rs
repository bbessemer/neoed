//! External formatters and their configuration (command-language spec, §6.4).

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::{fs, io, thread};

use toml::Value;

use crate::buffer::Buffer;
use crate::config::{Config, ConfigError, Entry, program};
use crate::conflict::conflicts;
use crate::edit::{Edit, EditSet};
use crate::exec::Change;
use crate::lang::Language;
use crate::lsp::{Document, Formatting, Lsp, LspFailure, TextEdit};

const DEFAULT_EDITION: &str = "2015";

/// A file's formatter, ready to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Formatter {
    /// Commands with placeholders and programs resolved, tried in order while
    /// the program isn't found.
    pub commands: Vec<Vec<String>>,
    /// The directory to run in: the file's.
    pub dir: PathBuf,
}

/// What formatting did to a changed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// No formatter, or its output is the text it was given.
    Unchanged,
    /// The text from the formatter named `name`.
    Formatted { name: String, text: String },
    /// No formatter for the file is installed; the note says so.
    NotFound(String),
    /// The formatter wasn't run; the note says why.
    Skipped(String),
    /// The formatter failed on the text.
    Failed(Failure),
}

/// A formatter's failure on a file's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// The formatter, to run again.
    pub formatter: Formatter,
    /// Its name in output.
    pub name: String,
    /// The first line of its stderr, or how else it failed.
    pub why: String,
    /// The file's path.
    pub path: String,
}

impl Failure {
    /// The note for writing the file unformatted.
    pub fn note(&self) -> String {
        format!(
            "{} failed: {}; skipped formatting {}",
            self.name, self.why, self.path
        )
    }
}

/// Formats the new text of each of `changes`, in parallel.
pub fn run(changes: &[Change], config: &mut Config) -> Result<Vec<Outcome>, ConfigError> {
    let written: HashMap<PathBuf, &str> = changes
        .iter()
        .filter_map(|c| Some((std::path::absolute(&c.path).ok()?, c.new.as_str())))
        .collect();
    config.overlay(&written);
    let found = changes
        .iter()
        .map(|c| match c.lang {
            Some(lang) => config.formatter(Path::new(&c.path), lang, &written),
            None => Ok(None),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(thread::scope(|scope| {
        let handles: Vec<_> = changes
            .iter()
            .zip(&found)
            .map(|(change, formatter)| {
                scope.spawn(move || match formatter {
                    Some(_) if !conflicts(&change.new).is_empty() => Outcome::Skipped(format!(
                        "skipped formatting {}: it has merge conflicts",
                        change.path
                    )),
                    Some(formatter) => formatter.format(&change.path, &change.new),
                    None => Outcome::Unchanged,
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("formatting doesn't panic"))
            .collect()
    }))
}

/// Formats with `lsp` each of `changes` whose outcome is `NotFound`, where the
/// language server can.
pub fn fallback(changes: &[Change], outcomes: &mut [Outcome], lsp: &mut dyn Lsp) {
    for (change, outcome) in changes.iter().zip(outcomes) {
        let (Outcome::NotFound(note), Some(lang)) = (&*outcome, change.lang) else {
            continue;
        };
        let Ok(path) = std::path::absolute(&change.path) else {
            continue;
        };
        let document = Document {
            path,
            lang,
            text: change.new.clone(),
        };
        *outcome = match lsp.format(&document) {
            Ok(Formatting::NoServer) => continue,
            Ok(Formatting::Edits { server, edits }) => match apply(&change.new, &edits) {
                Some(text) if text == change.new => Outcome::Unchanged,
                Some(text) => Outcome::Formatted { name: server, text },
                None => Outcome::NotFound(format!("{note}; {server} sent overlapping edits")),
            },
            Err(LspFailure(reason)) => Outcome::NotFound(format!("{note}; {reason}")),
        };
    }
}

/// `text` with a server's `edits` applied, or `None` if they overlap.
fn apply(text: &str, edits: &[TextEdit]) -> Option<String> {
    let buffer = Buffer::new(text);
    let mut set = EditSet::new(&buffer);
    for edit in edits {
        let start = buffer.lsp_offset(edit.start.line, edit.start.character);
        let end = buffer.lsp_offset(edit.end.line, edit.end.character);
        let text = edit.text.clone();
        set.push(Edit {
            range: start..end,
            text,
            command: 0,
        })
        .ok()?;
    }
    Some(set.apply())
}

impl Config {
    /// The formatter for `path` in `lang`, or `None` if it has none. Manifests
    /// are read from `written`, the new texts by absolute path, before the disk.
    pub fn formatter(
        &mut self,
        path: &Path,
        lang: Language,
        written: &HashMap<PathBuf, &str>,
    ) -> Result<Option<Formatter>, ConfigError> {
        let path = std::path::absolute(path).map_err(|err| crate::config::io_error(path, &err))?;
        let dir = path.parent().unwrap_or(&path).to_path_buf();
        let configured = self
            .layers(&dir)?
            .into_iter()
            .find_map(|layer| Some((layer.format.get(&lang)?, layer.dir.as_path())));
        let (commands, base) = match configured {
            Some((Entry::Off, _)) => return Ok(None),
            Some((Entry::Command(command), base)) => {
                (vec![command.clone()], Some(base.to_path_buf()))
            }
            None => (defaults(lang), None),
        };
        let mut edition = None;
        let mut fill = |arg: &str| {
            let arg = arg.replace("{path}", &path.to_string_lossy());
            if arg.contains("{edition}") {
                arg.replace(
                    "{edition}",
                    edition.get_or_insert_with(|| rust_edition(&dir, written)),
                )
            } else {
                arg
            }
        };
        let commands = commands
            .iter()
            .map(|command| {
                let mut command: Vec<String> = command.iter().map(|arg| fill(arg)).collect();
                command[0] = program(&command[0], base.as_deref(), &dir);
                command
            })
            .collect();
        // A file `create` makes in a new directory is formatted before the
        // directory exists.
        let dir = dir
            .ancestors()
            .find(|a| a.is_dir())
            .unwrap_or(&dir)
            .to_path_buf();
        Ok(Some(Formatter { commands, dir }))
    }
}

impl Formatter {
    /// Formats `text`, the new contents of the file at `path`.
    pub fn format(&self, path: &str, text: &str) -> Outcome {
        let failed = |name: &str, why: &str| {
            Outcome::Failed(Failure {
                formatter: self.clone(),
                name: name.into(),
                why: why.into(),
                path: path.into(),
            })
        };
        let mut missing = String::new();
        for command in &self.commands {
            let name = name(&command[0]);
            let spawned = Command::new(&command[0])
                .args(&command[1..])
                .current_dir(&self.dir)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn();
            let mut child = match spawned {
                Ok(child) => child,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    missing = name;
                    continue;
                }
                Err(err) => return failed(&name, &err.to_string()),
            };
            // Feed stdin from another thread so a formatter that writes before
            // reading everything can't fill its stdout pipe and deadlock.
            let mut stdin = child.stdin.take().expect("stdin is piped");
            let output = thread::scope(|scope| {
                // A formatter may exit without reading all its input; its
                // exit status says whether that's a failure.
                scope.spawn(move || stdin.write_all(text.as_bytes()).ok());
                child.wait_with_output()
            });
            let output = match output {
                Ok(output) => output,
                Err(err) => return failed(&name, &err.to_string()),
            };
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let first = stderr.lines().map(str::trim).find(|l| !l.is_empty());
                let why = first.map_or(output.status.to_string(), String::from);
                return failed(&name, &why);
            }
            return match String::from_utf8(output.stdout) {
                Ok(formatted) if formatted == text => Outcome::Unchanged,
                Ok(formatted) => Outcome::Formatted {
                    name,
                    text: formatted,
                },
                Err(_) => failed(&name, "output is not UTF-8"),
            };
        }
        Outcome::NotFound(format!("{missing} not found; skipped formatting {path}"))
    }
}

fn defaults(lang: Language) -> Vec<Vec<String>> {
    let commands: &[&[&str]] = match lang {
        Language::Rust => &[&["rustfmt", "--edition", "{edition}"]],
        Language::Go => &[&["gofmt"]],
        Language::Python => &[
            &["ruff", "format", "--stdin-filename", "{path}", "-"],
            &["black", "-q", "--stdin-filename", "{path}", "-"],
        ],
        Language::TypeScript | Language::Tsx | Language::JavaScript | Language::Markdown => {
            &[&["prettier", "--stdin-filepath", "{path}"]]
        }
    };
    commands
        .iter()
        .map(|c| c.iter().map(|w| w.to_string()).collect())
        .collect()
}

fn name(program: &str) -> String {
    Path::new(program)
        .file_name()
        .map_or(program.into(), |n| n.to_string_lossy().into_owned())
}

/// The Rust edition of the package holding `dir`, from its `Cargo.toml` or,
/// for `edition.workspace = true`, its workspace's.
fn rust_edition(dir: &Path, written: &HashMap<PathBuf, &str>) -> String {
    let edition = |table: &toml::Table| table.get("edition").cloned();
    let manifests = dir.ancestors().filter_map(|a| {
        let path = a.join("Cargo.toml");
        let text = match written.get(&path) {
            Some(text) => text.to_string(),
            None => fs::read_to_string(&path).ok()?,
        };
        text.parse::<toml::Table>().ok()
    });
    let mut inherit = false;
    for manifest in manifests {
        if !inherit {
            let Some(Value::Table(package)) = manifest.get("package") else {
                continue;
            };
            match edition(package) {
                Some(Value::String(e)) => return e,
                Some(Value::Table(t)) if t.get("workspace") == Some(&Value::Boolean(true)) => {
                    inherit = true
                }
                _ => break,
            }
        }
        if let Some(Value::Table(workspace)) = manifest.get("workspace") {
            if let Some(Value::Table(package)) = workspace.get("package")
                && let Some(Value::String(e)) = edition(package)
            {
                return e;
            }
            break;
        }
    }
    DEFAULT_EDITION.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::{Diagnosis, Locate, Located, Position, Renamed};
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
        Config::new(user.as_deref())?.formatter(&root.path().join(file), lang, &HashMap::new())
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
        let root = tree(&[("src/keep", "")]);
        let a = |name: &str| at(&root, name);
        let rust = lookup(&root, None, "src/a.rs", Language::Rust)
            .unwrap()
            .unwrap();
        assert_eq!(
            rust,
            Formatter {
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
            ("a.md", Language::Markdown),
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
        let found = Config::new(Some(&missing))
            .unwrap()
            .formatter(&root.path().join("a.go"), Language::Go, &HashMap::new())
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
        assert_eq!(err.kind.location, format!("{}:2:1", at(&root, ".ned.toml")));
        assert_eq!(
            err.kind.message,
            "unknown language `ruby`; expected one of rust, python, typescript, tsx, javascript, go, markdown"
        );
    }

    #[test]
    fn unknown_table() {
        let root = tree(&[(".ned.toml", "[lint]\nrust = [\"clippy\"]\n")]);
        let err = error(&root, None, "a.rs");
        assert!(
            err.kind
                .location
                .starts_with(&format!("{}:1:", at(&root, ".ned.toml"))),
            "{err:?}"
        );
        assert!(err.kind.message.contains("unknown field `lint`"), "{err:?}");
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
                err.kind.location,
                format!("{}:2:8", at(&root, ".ned.toml")),
                "{value}"
            );
            assert_eq!(err.kind.message, message, "{value}");
        }
    }

    #[test]
    fn invalid_toml() {
        let root = tree(&[(".ned.toml", "[format\n")]);
        let err = error(&root, None, "a.rs");
        assert!(
            err.kind
                .location
                .starts_with(&format!("{}:1:", at(&root, ".ned.toml"))),
            "{err:?}"
        );
        assert!(!err.kind.message.contains('\n'), "{err:?}");
    }

    #[test]
    fn errors_in_the_user_config() {
        let root = tree(&[]);
        let err = error(&root, Some("format = 1\n"), "a.rs");
        assert!(
            err.kind
                .location
                .starts_with(&format!("{}:1:", at(&root, "home/.config/ned/config.toml"))),
            "{err:?}"
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
        assert_eq!(found.commands, [argv(&[&at(&root, "tools/fmt.sh"), "-x"])]);
        assert_eq!(
            commands(&root, None, "sub/a.rs", Language::Rust),
            [argv(&["/bin/fmt"])]
        );
    }

    fn change(path: &Path, lang: Option<Language>, new: &str) -> Change {
        Change {
            path: path.to_str().unwrap().to_string(),
            old: String::new(),
            new: new.into(),
            edits: 1,
            lang,
            created: false,
        }
    }

    fn formatted(name: &str, text: &str) -> Outcome {
        Outcome::Formatted {
            name: name.into(),
            text: text.into(),
        }
    }

    fn format_with(commands: &[&[&str]], text: &str) -> Outcome {
        let formatter = Formatter {
            commands: commands.iter().map(|c| argv(c)).collect(),
            dir: std::env::temp_dir(),
        };
        formatter.format("src/a.rs", text)
    }

    /// The note for `outcome`, a failure.
    fn failure(outcome: Outcome) -> String {
        match outcome {
            Outcome::Failed(failure) => failure.note(),
            other => panic!("not a failure: {other:?}"),
        }
    }

    #[test]
    fn run_formats_each_change_in_order() {
        let root = tree(&[(
            ".ned.toml",
            "[format]\nrust = [\"sh\", \"-c\", \"tr a-z A-Z\"]\ngo = [\"sh\", \"-c\", \"cat\"]\npython = false\n",
        )]);
        let changes = [
            change(
                &root.path().join("a.rs"),
                Some(Language::Rust),
                "fn f() {}\n",
            ),
            change(&root.path().join("a.go"), Some(Language::Go), "package a\n"),
            change(&root.path().join("a.py"), Some(Language::Python), "x=1\n"),
            change(&root.path().join("a.txt"), None, "text\n"),
            change(
                &root.path().join("b.rs"),
                Some(Language::Rust),
                "struct S;\n",
            ),
        ];
        let outcomes = run(&changes, &mut Config::new(None).unwrap()).unwrap();
        assert_eq!(
            outcomes,
            [
                formatted("sh", "FN F() {}\n"),
                Outcome::Unchanged,
                Outcome::Unchanged,
                Outcome::Unchanged,
                formatted("sh", "STRUCT S;\n"),
            ]
        );
    }

    #[test]
    fn run_reads_the_edition_from_manifests_the_script_writes() {
        let root = tree(&[
            (
                ".ned.toml",
                "[format]\nrust = [\"sh\", \"-c\", \"echo {edition}\"]\n",
            ),
            ("old/Cargo.toml", "[package]\nedition = \"2018\"\n"),
            (
                "ws/crates/x/Cargo.toml",
                "[package]\nedition.workspace = true\n",
            ),
        ]);
        let manifest = |path: &str, text: &str| change(&root.path().join(path), None, text);
        let rust = |path: &str| change(&root.path().join(path), Some(Language::Rust), "");
        let changes = [
            manifest("new/Cargo.toml", "[package]\nedition = \"2024\"\n"),
            rust("new/src/a.rs"),
            manifest("old/Cargo.toml", "[package]\nedition = \"2021\"\n"),
            rust("old/src/a.rs"),
            rust("ws/crates/x/src/a.rs"),
            manifest(
                "ws/Cargo.toml",
                "[workspace]\n\n[workspace.package]\nedition = \"2024\"\n",
            ),
        ];
        let outcomes = run(&changes, &mut Config::new(None).unwrap()).unwrap();
        assert_eq!(
            outcomes,
            [
                Outcome::Unchanged,
                formatted("sh", "2024\n"),
                Outcome::Unchanged,
                formatted("sh", "2021\n"),
                formatted("sh", "2024\n"),
                Outcome::Unchanged,
            ]
        );
    }

    #[test]
    fn run_reads_config_files_the_script_writes() {
        let root = tree(&[
            (
                ".ned.toml",
                "[format]\nrust = [\"sh\", \"-c\", \"echo disk\"]\n",
            ),
            ("old/.ned.toml", "[format]\nrust = false\n"),
        ]);
        let config = |path: &str, text: &str| change(&root.path().join(path), None, text);
        let rust = |path: &str| change(&root.path().join(path), Some(Language::Rust), "");
        let changes = [
            rust("a.rs"),
            rust("new/a.rs"),
            config(
                "new/.ned.toml",
                "[format]\nrust = [\"sh\", \"-c\", \"echo created\"]\n",
            ),
            config(
                "old/.ned.toml",
                "[format]\nrust = [\"sh\", \"-c\", \"echo edited\"]\n",
            ),
            rust("old/a.rs"),
        ];
        let outcomes = run(&changes, &mut Config::new(None).unwrap()).unwrap();
        assert_eq!(
            outcomes,
            [
                formatted("sh", "disk\n"),
                formatted("sh", "created\n"),
                Outcome::Unchanged,
                Outcome::Unchanged,
                formatted("sh", "edited\n"),
            ]
        );
    }

    #[test]
    fn run_reports_config_errors() {
        let root = tree(&[(".ned.toml", "[format]\nrust = 1\n")]);
        let changes = [change(&root.path().join("a.rs"), Some(Language::Rust), "")];
        let err = run(&changes, &mut Config::new(None).unwrap()).unwrap_err();
        assert_eq!(
            err.kind.message,
            "`rust` must be a command (an array of strings) or false"
        );
    }

    #[test]
    fn files_with_conflicts_are_not_formatted() {
        let root = tree(&[(
            ".ned.toml",
            "[format]\nmarkdown = [\"sh\", \"-c\", \"tr a-z A-Z\"]\n",
        )]);
        let path = root.path().join("a.md");
        let changes = [
            change(
                &path,
                Some(Language::Markdown),
                "<<<<<<< HEAD\na\n=======\nb\n>>>>>>> t\n",
            ),
            change(
                &root.path().join("b.md"),
                Some(Language::Markdown),
                "=======\nb\n",
            ),
        ];
        let outcomes = run(&changes, &mut Config::new(None).unwrap()).unwrap();
        assert_eq!(
            outcomes,
            [
                Outcome::Skipped(format!(
                    "skipped formatting {}: it has merge conflicts",
                    path.display()
                )),
                formatted("sh", "=======\nB\n"),
            ]
        );
    }

    #[test]
    fn a_missing_program_is_skipped() {
        assert_eq!(
            format_with(&[&["ned-no-such-formatter", "-q"]], "x\n"),
            Outcome::NotFound(
                "ned-no-such-formatter not found; skipped formatting src/a.rs".into()
            )
        );
    }

    #[test]
    fn missing_programs_fall_back_to_the_next_command() {
        assert_eq!(
            format_with(&[&["ned-no-such-a"], &["sh", "-c", "tr a-z A-Z"]], "x\n"),
            formatted("sh", "X\n")
        );
        assert_eq!(
            format_with(&[&["ned-no-such-a"], &["/nowhere/ned-no-such-b"]], "x\n"),
            Outcome::NotFound("ned-no-such-b not found; skipped formatting src/a.rs".into())
        );
    }

    #[test]
    fn a_failing_program_is_skipped_with_its_first_stderr_line() {
        let outcome = format_with(
            &[&[
                "sh",
                "-c",
                "cat >/dev/null; echo 'bad input' >&2; echo more >&2; exit 3",
            ]],
            "x\n",
        );
        assert_eq!(
            failure(outcome),
            "sh failed: bad input; skipped formatting src/a.rs"
        );
        assert_eq!(
            failure(format_with(&[&["sh", "-c", "exit 1"]], "x\n")),
            "sh failed: exit status: 1; skipped formatting src/a.rs"
        );
    }

    #[test]
    fn non_utf8_output_is_a_failure() {
        assert_eq!(
            failure(format_with(&[&["sh", "-c", "printf '\\377'"]], "x\n")),
            "sh failed: output is not UTF-8; skipped formatting src/a.rs"
        );
    }

    #[test]
    fn large_texts_pass_through_the_pipes() {
        let text = "abc\n".repeat(1 << 18);
        assert_eq!(
            format_with(&[&["sh", "-c", "tr a-z A-Z"]], &text),
            formatted("sh", &text.to_uppercase())
        );
        assert_eq!(format_with(&[&["cat"]], &text), Outcome::Unchanged);
    }

    #[test]
    fn formatters_run_in_the_file_directory() {
        let root = tree(&[
            (".ned.toml", "[format]\ngo = [\"sh\", \"-c\", \"pwd -P\"]\n"),
            ("sub/keep", ""),
        ]);
        let changes = [change(
            &root.path().join("sub/a.go"),
            Some(Language::Go),
            "",
        )];
        let outcomes = run(&changes, &mut Config::new(None).unwrap()).unwrap();
        let dir = fs::canonicalize(root.path().join("sub")).unwrap();
        assert_eq!(outcomes, [formatted("sh", &format!("{}\n", dir.display()))]);
    }

    #[test]
    fn formatters_for_files_in_new_directories_run_in_an_existing_ancestor() {
        let root = tree(&[
            (".ned.toml", "[format]\ngo = [\"sh\", \"-c\", \"pwd -P\"]\n"),
            ("sub/keep", ""),
        ]);
        let changes = [change(
            &root.path().join("sub/new/deeper/a.go"),
            Some(Language::Go),
            "",
        )];
        let outcomes = run(&changes, &mut Config::new(None).unwrap()).unwrap();
        let dir = fs::canonicalize(root.path().join("sub")).unwrap();
        assert_eq!(outcomes, [formatted("sh", &format!("{}\n", dir.display()))]);
    }

    /// A server that answers every formatting request with `answer`.
    struct FormatLsp {
        answer: Result<Formatting, LspFailure>,
        asked: Vec<Document>,
    }

    impl Lsp for FormatLsp {
        fn diagnose(&mut self, _: &[Document], _: bool) -> Result<Diagnosis, LspFailure> {
            unreachable!("formatting doesn't diagnose")
        }

        fn sync(&mut self, _: &[Document]) -> Result<(), LspFailure> {
            unreachable!("formatting doesn't sync")
        }

        fn rename(&mut self, _: &Document, _: Position, _: &str) -> Result<Renamed, LspFailure> {
            unreachable!("formatting doesn't rename")
        }

        fn locate(&mut self, _: Locate, _: &Document, _: Position) -> Result<Located, LspFailure> {
            unreachable!("formatting doesn't locate")
        }

        fn format(&mut self, document: &Document) -> Result<Formatting, LspFailure> {
            self.asked.push(document.clone());
            self.answer.clone()
        }
    }

    fn serve(answer: Result<Formatting, LspFailure>) -> FormatLsp {
        FormatLsp {
            answer,
            asked: Vec::new(),
        }
    }

    fn edits(edits: &[(u32, u32, u32, u32, &str)]) -> Result<Formatting, LspFailure> {
        Ok(Formatting::Edits {
            server: "rust-analyzer".into(),
            edits: edits
                .iter()
                .map(|&(line, start, end_line, end, text)| TextEdit {
                    start: Position {
                        line,
                        character: start,
                    },
                    end: Position {
                        line: end_line,
                        character: end,
                    },
                    text: text.into(),
                })
                .collect(),
        })
    }

    const NOT_FOUND: &str = "rustfmt not found; skipped formatting src/a.rs";

    /// `fallback` on one change to src/a.rs whose formatter wasn't found.
    fn fall_back(new: &str, lsp: &mut FormatLsp) -> Outcome {
        let changes = [change(Path::new("src/a.rs"), Some(Language::Rust), new)];
        let mut outcomes = [Outcome::NotFound(NOT_FOUND.into())];
        fallback(&changes, &mut outcomes, lsp);
        outcomes[0].clone()
    }

    #[test]
    fn the_language_server_formats_changes_without_a_formatter() {
        let changes = [
            change(
                Path::new("src/a.rs"),
                Some(Language::Rust),
                "fn a( ) {}\nx\n",
            ),
            change(Path::new("src/b.rs"), Some(Language::Rust), "b\n"),
            change(Path::new("src/c.rs"), Some(Language::Rust), "c\n"),
            change(Path::new("src/d.rs"), Some(Language::Rust), "d\n"),
        ];
        let mut outcomes = [
            Outcome::NotFound(NOT_FOUND.into()),
            Outcome::Unchanged,
            Outcome::Skipped("skipped formatting src/c.rs: it has merge conflicts".into()),
            formatted("rustfmt", "D\n"),
        ];
        let expected_rest = outcomes[1..].to_vec();
        let mut lsp = serve(edits(&[(0, 5, 0, 6, ""), (1, 0, 1, 0, "// y\n")]));
        fallback(&changes, &mut outcomes, &mut lsp);
        assert_eq!(
            outcomes[0],
            formatted("rust-analyzer", "fn a() {}\n// y\nx\n")
        );
        assert_eq!(outcomes[1..], expected_rest);
        assert_eq!(lsp.asked.len(), 1);
        assert_eq!(lsp.asked[0].path, std::path::absolute("src/a.rs").unwrap());
        assert_eq!(lsp.asked[0].lang, Language::Rust);
        assert_eq!(lsp.asked[0].text, "fn a( ) {}\nx\n");
    }

    #[test]
    fn server_edits_count_utf16_units() {
        let mut lsp = serve(edits(&[(0, 3, 0, 4, "b")]));
        assert_eq!(
            fall_back("é😀a\n", &mut lsp),
            formatted("rust-analyzer", "é😀b\n")
        );
    }

    #[test]
    fn no_server_edits_leave_the_change_unformatted() {
        assert_eq!(fall_back("a\n", &mut serve(edits(&[]))), Outcome::Unchanged);
    }

    #[test]
    fn without_a_server_the_note_stays() {
        let mut lsp = serve(Ok(Formatting::NoServer));
        assert_eq!(
            fall_back("a\n", &mut lsp),
            Outcome::NotFound(NOT_FOUND.into())
        );
    }

    #[test]
    fn a_server_failure_is_added_to_the_note() {
        let mut lsp = serve(Err(LspFailure("rust-analyzer exited; retry".into())));
        assert_eq!(
            fall_back("a\n", &mut lsp),
            Outcome::NotFound(format!("{NOT_FOUND}; rust-analyzer exited; retry"))
        );
    }

    #[test]
    fn overlapping_server_edits_are_a_failure() {
        let mut lsp = serve(edits(&[(0, 0, 0, 2, "x"), (0, 1, 0, 2, "y")]));
        assert_eq!(
            fall_back("abc\n", &mut lsp),
            Outcome::NotFound(format!("{NOT_FOUND}; rust-analyzer sent overlapping edits"))
        );
    }
}
