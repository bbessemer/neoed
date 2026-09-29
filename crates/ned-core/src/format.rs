//! External formatters and their configuration (command-language spec, §6.4).

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::{env, fmt, fs, io, thread};

use serde::Deserialize;
use toml::{Spanned, Value};

use crate::exec::Change;
use crate::lang::Language;

const CONFIG_FILE: &str = ".ned.toml";
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    format: BTreeMap<Spanned<String>, Spanned<Value>>,
}

/// What formatting did to a changed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// No formatter, or its output is the text it was given.
    Unchanged,
    /// The text from the formatter named `name`.
    Formatted { name: String, text: String },
    /// Formatting was skipped; the note says why.
    Skipped(String),
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
    let absolute = |var| {
        env::var_os(var)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    let config = absolute("XDG_CONFIG_HOME").or_else(|| Some(absolute("HOME")?.join(".config")))?;
    Some(config.join("ned/config.toml"))
}

/// Formats the new text of each of `changes`, in parallel.
pub fn run(changes: &[Change], formatters: &mut Formatters) -> Result<Vec<Outcome>, ConfigError> {
    let found = changes
        .iter()
        .map(|c| match c.lang {
            Some(lang) => formatters.get(Path::new(&c.path), lang),
            None => Ok(None),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(thread::scope(|scope| {
        let handles: Vec<_> = changes
            .iter()
            .zip(&found)
            .map(|(change, formatter)| {
                scope.spawn(move || match formatter {
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

impl Formatters {
    /// Reads the user config at `user`, if it exists.
    pub fn new(user: Option<&Path>) -> Result<Self, ConfigError> {
        Ok(Formatters {
            user: user.map(load).transpose()?.flatten(),
            layers: HashMap::new(),
        })
    }

    /// The formatter for `path` in `lang`, or `None` if it has none.
    pub fn get(&mut self, path: &Path, lang: Language) -> Result<Option<Formatter>, ConfigError> {
        let path = std::path::absolute(path).map_err(|err| io_error(path, &err))?;
        let dir = path.parent().unwrap_or(&path).to_path_buf();
        for ancestor in dir.ancestors() {
            if !self.layers.contains_key(ancestor) {
                let layer = load(&ancestor.join(CONFIG_FILE))?;
                self.layers.insert(ancestor.to_path_buf(), layer);
            }
        }
        let configured = dir
            .ancestors()
            .filter_map(|a| self.layers[a].as_ref())
            .chain(&self.user)
            .find_map(|layer| Some((layer.format.get(&lang)?, layer.dir.as_path())));
        let (commands, base) = match configured {
            Some((Entry::Off, _)) => return Ok(None),
            Some((Entry::Command(command), base)) => (vec![command.clone()], Some(base)),
            None => (defaults(lang), None),
        };
        let mut edition = None;
        let mut fill = |arg: &str| {
            let arg = arg.replace("{path}", &path.to_string_lossy());
            if arg.contains("{edition}") {
                arg.replace(
                    "{edition}",
                    edition.get_or_insert_with(|| rust_edition(&dir)),
                )
            } else {
                arg
            }
        };
        let commands = commands
            .iter()
            .map(|command| {
                let mut command: Vec<String> = command.iter().map(|arg| fill(arg)).collect();
                command[0] = program(&command[0], base, &dir);
                command
            })
            .collect();
        Ok(Some(Formatter { commands, dir }))
    }
}

impl Formatter {
    /// Formats `text`, the new contents of the file at `path`.
    fn format(&self, path: &str, text: &str) -> Outcome {
        let skipped = |name: &str, why: &str| {
            Outcome::Skipped(format!("{name} {why}; skipped formatting {path}"))
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
                Err(err) => return skipped(&name, &format!("failed: {err}")),
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
                Err(err) => return skipped(&name, &format!("failed: {err}")),
            };
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let first = stderr.lines().map(str::trim).find(|l| !l.is_empty());
                let why = first.map_or(output.status.to_string(), String::from);
                return skipped(&name, &format!("failed: {why}"));
            }
            return match String::from_utf8(output.stdout) {
                Ok(formatted) if formatted == text => Outcome::Unchanged,
                Ok(formatted) => Outcome::Formatted {
                    name,
                    text: formatted,
                },
                Err(_) => skipped(&name, "failed: output is not UTF-8"),
            };
        }
        skipped(&missing, "not found")
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

/// Where to run `program`: relative to `base`, the directory of the config
/// that named it, if it's a path; else from the nearest `node_modules/.bin`
/// above `dir`, or as is, for a `PATH` lookup.
fn program(program: &str, base: Option<&Path>, dir: &Path) -> String {
    let found = if program.contains('/') {
        base.map(|b| b.join(program))
    } else {
        dir.ancestors()
            .map(|a| a.join("node_modules/.bin").join(program))
            .find(|p| p.is_file())
    };
    found.map_or(program.into(), |p| p.to_string_lossy().into_owned())
}

/// The Rust edition of the package holding `dir`, from its `Cargo.toml` or,
/// for `edition.workspace = true`, its workspace's.
fn rust_edition(dir: &Path) -> String {
    let edition = |table: &toml::Table| table.get("edition").cloned();
    let manifests = dir.ancestors().filter_map(|a| {
        let text = fs::read_to_string(a.join("Cargo.toml")).ok()?;
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

/// The config file at `path`, or `None` if there is none.
fn load(path: &Path) -> Result<Option<Layer>, ConfigError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(io_error(path, &err)),
    };
    let error = |span: Option<std::ops::Range<usize>>, message: String| {
        let mut location = display(path);
        if let Some(span) = span {
            let (line, col) = crate::script::error::location(&text, span.start);
            location = format!("{location}:{line}:{col}");
        }
        ConfigError { location, message }
    };
    let raw: RawConfig =
        toml::from_str(&text).map_err(|err| error(err.span(), err.message().trim().into()))?;
    let mut format = HashMap::new();
    for (key, value) in raw.format {
        let lang: Language = key
            .get_ref()
            .parse()
            .map_err(|message| error(Some(key.span()), message))?;
        let entry = match value.get_ref() {
            Value::Boolean(false) => Entry::Off,
            Value::Array(words) if words.is_empty() => {
                return Err(error(
                    Some(value.span()),
                    format!("`{lang}` has an empty command"),
                ));
            }
            Value::Array(words) => {
                match words.iter().map(|w| w.as_str().map(String::from)).collect() {
                    Some(command) => Entry::Command(command),
                    None => return Err(error(Some(value.span()), not_a_command(lang))),
                }
            }
            _ => return Err(error(Some(value.span()), not_a_command(lang))),
        };
        format.insert(lang, entry);
    }
    Ok(Some(Layer {
        dir: path.parent().unwrap_or(path).to_path_buf(),
        format,
    }))
}

fn not_a_command(lang: Language) -> String {
    format!("`{lang}` must be a command (an array of strings) or false")
}

fn io_error(path: &Path, err: &io::Error) -> ConfigError {
    ConfigError {
        location: display(path),
        message: err.to_string(),
    }
}

/// `path`, relative to the working directory if it's inside it.
fn display(path: &Path) -> String {
    let cwd = env::current_dir().unwrap_or_default();
    path.strip_prefix(&cwd)
        .unwrap_or(path)
        .display()
        .to_string()
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
            dir: env::temp_dir(),
        };
        formatter.format("src/a.rs", text)
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
        let outcomes = run(&changes, &mut Formatters::new(None).unwrap()).unwrap();
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
    fn run_reports_config_errors() {
        let root = tree(&[(".ned.toml", "[format]\nrust = 1\n")]);
        let changes = [change(&root.path().join("a.rs"), Some(Language::Rust), "")];
        let err = run(&changes, &mut Formatters::new(None).unwrap()).unwrap_err();
        assert_eq!(
            err.message,
            "`rust` must be a command (an array of strings) or false"
        );
    }

    #[test]
    fn a_missing_program_is_skipped() {
        assert_eq!(
            format_with(&[&["ned-no-such-formatter", "-q"]], "x\n"),
            Outcome::Skipped("ned-no-such-formatter not found; skipped formatting src/a.rs".into())
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
            Outcome::Skipped("ned-no-such-b not found; skipped formatting src/a.rs".into())
        );
    }

    #[test]
    fn a_failing_program_is_skipped_with_its_first_stderr_line() {
        assert_eq!(
            format_with(
                &[&[
                    "sh",
                    "-c",
                    "cat >/dev/null; echo 'bad input' >&2; echo more >&2; exit 3"
                ]],
                "x\n"
            ),
            Outcome::Skipped("sh failed: bad input; skipped formatting src/a.rs".into())
        );
        assert_eq!(
            format_with(&[&["sh", "-c", "exit 1"]], "x\n"),
            Outcome::Skipped("sh failed: exit status: 1; skipped formatting src/a.rs".into())
        );
    }

    #[test]
    fn non_utf8_output_is_a_failure() {
        assert_eq!(
            format_with(&[&["sh", "-c", "printf '\\377'"]], "x\n"),
            Outcome::Skipped("sh failed: output is not UTF-8; skipped formatting src/a.rs".into())
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
        let outcomes = run(&changes, &mut Formatters::new(None).unwrap()).unwrap();
        let dir = fs::canonicalize(root.path().join("sub")).unwrap();
        assert_eq!(outcomes, [formatted("sh", &format!("{}\n", dir.display()))]);
    }
}
