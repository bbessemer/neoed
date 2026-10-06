//! Language servers: which one serves each language, and what `ned` asks of
//! them (spec §1.1, §4.1).

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::buffer::Buffer;
use crate::config::{Config, ConfigError, Entry, program};
use crate::exec::Change;
use crate::lang::Language;
use crate::style::{Role, Style};

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

/// Diagnostics for some documents, with the workspace's `[check]` levels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnosis {
    pub show: Severity,
    /// The lowest severity of an introduced diagnostic that rejects an edit;
    /// `None` for none.
    pub block: Option<Severity>,
    /// One per document, in order; `None` where its language has no server.
    pub files: Vec<Option<Vec<Diagnostic>>>,
    /// Why some diagnostics may be missing, e.g. a save-time check that
    /// didn't finish.
    #[serde(default)]
    pub notes: Vec<String>,
}

/// What an edit's changed files introduced (spec §6.5).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Checked {
    /// Per changed file: its introduced diagnostics at the `show` level or
    /// above, by position.
    pub files: Vec<Vec<Diagnostic>>,
    /// The introduced diagnostics that reject the edit, by changed file.
    pub blocking: Vec<(usize, Diagnostic)>,
}

/// Why language servers couldn't answer; the message ends with a fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspFailure(pub String);

/// A replacement of the text between two positions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextEdit {
    pub start: Position,
    pub end: Position,
    pub text: String,
}

/// A server's edits to one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEdits {
    /// Absolute.
    pub path: PathBuf,
    pub edits: Vec<TextEdit>,
}

/// A server's answer to a rename.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Renamed {
    Edits(Vec<FileEdits>),
    /// Why the server can't rename there; the message ends with a fix.
    Refused(String),
    /// The document's language has no server.
    NoServer,
}

/// What `.refs` and `.def` ask a server for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Locate {
    References,
    Definition,
}

/// A range in a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    /// Absolute.
    pub path: PathBuf,
    pub start: Position,
    pub end: Position,
}

/// A server's answer to `Locate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Located {
    /// In path order.
    Locations(Vec<Location>),
    /// The document's language has no server.
    NoServer,
}

/// A server's answer to a formatting request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Formatting {
    /// `server` names the server that made `edits`.
    Edits {
        server: String,
        edits: Vec<TextEdit>,
    },
    /// The document's language has no server, or its server can't format.
    NoServer,
}

/// What `ned` asks of the workspace's language servers.
pub trait Lsp {
    /// Whether the servers are up already, so edits can be checked and
    /// formatted through them without starting any (spec §1.1).
    fn running(&mut self) -> bool {
        true
    }

    /// Diagnostics for each of `documents`, as their text stands. `saved`
    /// says the documents' files hold their texts, so servers may also run,
    /// and are waited for, the checks they run when a file is saved.
    fn diagnose(&mut self, documents: &[Document], saved: bool) -> Result<Diagnosis, LspFailure>;

    /// Brings the servers' copies of `documents` up to date.
    fn sync(&mut self, documents: &[Document]) -> Result<(), LspFailure>;

    /// The edits that rename the symbol at `position` in `document` to `name`.
    fn rename(
        &mut self,
        document: &Document,
        position: Position,
        name: &str,
    ) -> Result<Renamed, LspFailure>;

    /// The references to, or definition of, the symbol at `position` in
    /// `document`.
    fn locate(
        &mut self,
        kind: Locate,
        document: &Document,
        position: Position,
    ) -> Result<Located, LspFailure>;

    /// The edits that format `document`, as its text stands.
    fn format(&mut self, document: &Document) -> Result<Formatting, LspFailure>;
}

/// `after`'s diagnostics that `before` has no identical one left to match:
/// the same severity, source, code and message, wherever they are.
pub fn introduced(before: &[Diagnostic], after: &[Diagnostic]) -> Vec<Diagnostic> {
    let same = |a: &Diagnostic, b: &Diagnostic| {
        (a.severity, &a.source, &a.code, &a.message) == (b.severity, &b.source, &b.code, &b.message)
    };
    let mut unmatched: Vec<&Diagnostic> = before.iter().collect();
    after
        .iter()
        .filter(|a| match unmatched.iter().position(|b| same(a, b)) {
            Some(i) => {
                unmatched.swap_remove(i);
                false
            }
            None => true,
        })
        .cloned()
        .collect()
}

/// Diagnoses the original and `finals` text of each of `changes`, and finds
/// what the edit introduced. `allow` raises the block level (`allow
/// errors`: `Error`); `force` blocks nothing.
pub fn check_changes(
    lsp: &mut dyn Lsp,
    changes: &[Change],
    finals: &[&str],
    allow: Option<Severity>,
    force: bool,
) -> Result<Checked, LspFailure> {
    let mut checked = Checked {
        files: vec![Vec::new(); changes.len()],
        blocking: Vec::new(),
    };
    let typed: Vec<usize> = (0..changes.len())
        .filter(|&i| changes[i].lang.is_some())
        .collect();
    if typed.is_empty() {
        return Ok(checked);
    }
    let before = lsp.diagnose(
        &documents(changes, &typed, |i| changes[i].old.clone())?,
        false,
    )?;
    let after = lsp.diagnose(
        &documents(changes, &typed, |i| finals[i].to_string())?,
        false,
    )?;
    let blocks = |severity: Severity| {
        !force
            && after.block.is_some_and(|block| severity <= block)
            && allow.is_none_or(|allow| severity < allow)
    };
    for (k, &i) in typed.iter().enumerate() {
        let Some(diagnostics) = &after.files[k] else {
            continue;
        };
        let original = before.files[k].as_deref().unwrap_or_default();
        let mut new = introduced(original, diagnostics);
        new.sort_by_key(|d| d.start);
        for d in &new {
            if blocks(d.severity) {
                checked.blocking.push((i, d.clone()));
            }
        }
        checked.files[i] = new
            .into_iter()
            .filter(|d| d.severity <= after.show)
            .collect();
    }
    Ok(checked)
}

/// Sends the servers the original text of each of `changes`, after an edit
/// that wasn't written, so their view matches the files again.
pub fn restore(lsp: &mut dyn Lsp, changes: &[Change]) -> Result<(), LspFailure> {
    let typed: Vec<usize> = (0..changes.len())
        .filter(|&i| changes[i].lang.is_some())
        .collect();
    if typed.is_empty() {
        return Ok(());
    }
    lsp.sync(&documents(changes, &typed, |i| changes[i].old.clone())?)
}

/// The diagnostics of files' texts on disk, as saved, before an edit is
/// written over them (spec §6.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeforeSave {
    /// The changes whose original text was diagnosed.
    typed: Vec<usize>,
    diagnosis: Option<Diagnosis>,
}

/// What the checks servers run on save found that a write introduced: per
/// change, at the `show` level or above, by position; and why some may be
/// missing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Saved {
    pub files: Vec<Vec<Diagnostic>>,
    pub notes: Vec<String>,
}

/// Diagnoses the original text of each of `changes` that a file holds, as
/// saved, before the edit is written.
pub fn before_save(lsp: &mut dyn Lsp, changes: &[Change]) -> Result<BeforeSave, LspFailure> {
    let typed: Vec<usize> = (0..changes.len())
        .filter(|&i| changes[i].lang.is_some() && !changes[i].created)
        .collect();
    let diagnosis = match typed.is_empty() {
        true => None,
        false => Some(lsp.diagnose(
            &documents(changes, &typed, |i| changes[i].old.clone())?,
            true,
        )?),
    };
    Ok(BeforeSave { typed, diagnosis })
}

/// Diagnoses the written `finals` of `changes` as saved, and finds what the
/// write introduced since `before`, leaving out what `already`, the edit's
/// check, reported.
pub fn check_saved(
    lsp: &mut dyn Lsp,
    before: BeforeSave,
    changes: &[Change],
    finals: &[&str],
    already: Option<&Checked>,
) -> Result<Saved, LspFailure> {
    let mut saved = Saved {
        files: vec![Vec::new(); changes.len()],
        notes: Vec::new(),
    };
    let typed: Vec<usize> = (0..changes.len())
        .filter(|&i| changes[i].lang.is_some())
        .collect();
    if typed.is_empty() {
        return Ok(saved);
    }
    let after = lsp.diagnose(
        &documents(changes, &typed, |i| finals[i].to_string())?,
        true,
    )?;
    let notes = before
        .diagnosis
        .iter()
        .chain([&after])
        .flat_map(|d| &d.notes);
    for note in notes {
        if !saved.notes.contains(note) {
            saved.notes.push(note.clone());
        }
    }
    for (k, &i) in typed.iter().enumerate() {
        let Some(diagnostics) = &after.files[k] else {
            continue;
        };
        let mut original: Vec<Diagnostic> = before
            .typed
            .iter()
            .position(|&j| j == i)
            .and_then(|b| before.diagnosis.as_ref()?.files[b].clone())
            .unwrap_or_default();
        if let Some(already) = already {
            original.extend(already.files[i].iter().cloned());
        }
        let mut new = introduced(&original, diagnostics);
        new.retain(|d| d.severity <= after.show);
        new.sort_by_key(|d| d.start);
        saved.files[i] = new;
    }
    Ok(saved)
}

/// The `typed` changes as documents holding `text(i)`.
fn documents(
    changes: &[Change],
    typed: &[usize],
    text: impl Fn(usize) -> String,
) -> Result<Vec<Document>, LspFailure> {
    typed
        .iter()
        .map(|&i| {
            let path = &changes[i].path;
            Ok(Document {
                path: std::path::absolute(path)
                    .map_err(|err| LspFailure(format!("cannot read {path}: {err}")))?,
                lang: changes[i].lang.expect("typed"),
                text: text(i),
            })
        })
        .collect()
}

/// `d` as a line of `check` output (spec §4.1), for the file at `path`
/// holding `buffer`.
pub fn render(path: &str, buffer: &Buffer, d: &Diagnostic, style: Style) -> String {
    let start = buffer.lsp_offset(d.start.line, d.start.character);
    let line = buffer.byte_to_line(start).unwrap_or(d.start.line as usize);
    let line_start = buffer.line_range(line).map_or(start, |r| r.start);
    let column = buffer
        .slice(line_start..start)
        .map_or(0, |s| s.chars().count())
        + 1;
    let tag = match (&d.source, &d.code) {
        (Some(source), Some(code)) => format!(" [{source} {code}]"),
        (Some(tag), None) | (None, Some(tag)) => format!(" [{tag}]"),
        (None, None) => String::new(),
    };
    let mut message = d.message.lines();
    let first = message.next().unwrap_or_default();
    let role = match d.severity {
        Severity::Error => Role::Error,
        Severity::Warning => Role::Warning,
        Severity::Info | Severity::Hint => Role::Info,
    };
    let severity = style.paint(role, d.severity.name());
    let mut out = format!("{path}:{}:{column}: {severity}: {first}{tag}\n", line + 1);
    for rest in message {
        out.push_str(&format!("  {rest}\n"));
    }
    out
}

impl Severity {
    pub const ALL: [Severity; 4] = [
        Severity::Error,
        Severity::Warning,
        Severity::Info,
        Severity::Hint,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
            Severity::Hint => "hint",
        }
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
        Severity::ALL.into_iter().find(|l| l.name() == s).ok_or(())
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
    use crate::style::shown;

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

    fn d(line: u32, severity: Severity, message: &str) -> Diagnostic {
        Diagnostic {
            start: Position { line, character: 0 },
            end: Position { line, character: 1 },
            severity,
            message: message.into(),
            source: Some("fake".into()),
            code: None,
        }
    }

    #[test]
    fn render_paints_the_severity() {
        let buffer = Buffer::new("let x = 1;\n");
        let line = |severity| shown(&render("a.rs", &buffer, &d(0, severity, "m"), Style::Color));
        assert_eq!(
            line(Severity::Error),
            "a.rs:1:1: \\e[1;31merror\\e[0m: m [fake]\n"
        );
        assert_eq!(
            line(Severity::Warning),
            "a.rs:1:1: \\e[1;33mwarning\\e[0m: m [fake]\n"
        );
        assert_eq!(
            line(Severity::Info),
            "a.rs:1:1: \\e[1;34minfo\\e[0m: m [fake]\n"
        );
        assert_eq!(
            line(Severity::Hint),
            "a.rs:1:1: \\e[1;34mhint\\e[0m: m [fake]\n"
        );
        let plain = render("a.rs", &buffer, &d(0, Severity::Error, "m"), Style::Plain);
        assert_eq!(plain, "a.rs:1:1: error: m [fake]\n");
    }

    #[test]
    fn introduced_ignores_positions_and_counts_duplicates() {
        let before = [d(0, Severity::Error, "a"), d(1, Severity::Warning, "b")];
        let after = [
            d(5, Severity::Error, "a"),
            d(6, Severity::Error, "a"),
            d(7, Severity::Error, "b"),
            d(8, Severity::Warning, "b"),
        ];
        assert_eq!(
            introduced(&before, &after),
            [d(6, Severity::Error, "a"), d(7, Severity::Error, "b")]
        );
        assert_eq!(introduced(&[], &before), before);
        let mut coded = d(0, Severity::Error, "a");
        coded.code = Some("E1".into());
        assert_eq!(introduced(&before, std::slice::from_ref(&coded)), [coded]);
    }

    /// Servers that report a diagnostic for each line containing ERROR, WARN or
    /// HINT, for Rust files only, and, as saved, an error for each containing
    /// CARGO, as rust-analyzer's `cargo check` does.
    #[derive(Default)]
    struct TextLsp {
        show: Option<Severity>,
        block: Option<Option<Severity>>,
        failure: Option<&'static str>,
        asked: Vec<Vec<String>>,
        /// Whether each request was as saved.
        saved: Vec<bool>,
        /// Notes on every diagnosis.
        notes: Vec<String>,
    }

    impl Lsp for TextLsp {
        fn diagnose(
            &mut self,
            documents: &[Document],
            saved: bool,
        ) -> Result<Diagnosis, LspFailure> {
            self.saved.push(saved);
            self.asked
                .push(documents.iter().map(|d| d.text.clone()).collect());
            if let Some(failure) = self.failure {
                return Err(LspFailure(failure.into()));
            }
            let files = documents
                .iter()
                .map(|doc| {
                    (doc.lang == Language::Rust).then(|| {
                        let mut found = Vec::new();
                        for (line, text) in doc.text.lines().enumerate() {
                            for (word, severity) in [
                                ("ERROR", Severity::Error),
                                ("WARN", Severity::Warning),
                                ("HINT", Severity::Hint),
                                (if saved { "CARGO" } else { "\0" }, Severity::Error),
                            ] {
                                if text.contains(word) {
                                    found.push(d(line as u32, severity, word));
                                }
                            }
                        }
                        found
                    })
                })
                .collect();
            Ok(Diagnosis {
                show: self.show.unwrap_or(Severity::Warning),
                block: self.block.unwrap_or(Some(Severity::Error)),
                files,
                notes: self.notes.clone(),
            })
        }

        fn sync(&mut self, _: &[Document]) -> Result<(), LspFailure> {
            unreachable!("check_changes doesn't sync")
        }

        fn rename(&mut self, _: &Document, _: Position, _: &str) -> Result<Renamed, LspFailure> {
            unreachable!("not renamed in these tests")
        }

        fn locate(&mut self, _: Locate, _: &Document, _: Position) -> Result<Located, LspFailure> {
            unreachable!("not located in these tests")
        }

        fn format(&mut self, _: &Document) -> Result<Formatting, LspFailure> {
            unreachable!("not formatted in these tests")
        }
    }

    fn change(path: &str, old: &str, new: &str) -> Change {
        Change {
            path: format!("/w/{path}"),
            old: old.into(),
            new: new.into(),
            edits: 1,
            lang: Language::detect(path, new),
            created: old.is_empty(),
        }
    }

    fn checked(
        lsp: &mut TextLsp,
        changes: &[Change],
        allow: Option<Severity>,
        force: bool,
    ) -> Checked {
        let finals: Vec<&str> = changes.iter().map(|c| c.new.as_str()).collect();
        check_changes(lsp, changes, &finals, allow, force).unwrap()
    }

    #[test]
    fn check_changes_diagnoses_originals_then_finals() {
        let mut lsp = TextLsp::default();
        let changes = [change("a.rs", "a\n", "a\nERROR\n")];
        let out =
            check_changes(&mut lsp, &changes, &["formatted ERROR WARN\n"], None, false).unwrap();
        assert_eq!(lsp.asked, [vec!["a\n"], vec!["formatted ERROR WARN\n"]]);
        assert_eq!(
            lsp.saved,
            [false, false],
            "edits aren't written when they're checked"
        );
        let error = d(0, Severity::Error, "ERROR");
        assert_eq!(
            out.files,
            [vec![error.clone(), d(0, Severity::Warning, "WARN")]]
        );
        assert_eq!(out.blocking, [(0, error)]);
    }

    #[test]
    fn check_changes_reports_only_what_the_edit_introduced() {
        let mut lsp = TextLsp::default();
        let changes = [
            change("a.rs", "ERROR\n", "x\nERROR\n"),
            change("b.rs", "ERROR\n", "ERROR\nERROR\n"),
            change("c.rs", "", "WARN\nHINT\n"),
        ];
        let out = checked(&mut lsp, &changes, None, false);
        assert_eq!(
            out.files,
            [
                vec![],
                vec![d(1, Severity::Error, "ERROR")],
                vec![d(0, Severity::Warning, "WARN")],
            ]
        );
        assert_eq!(out.blocking, [(1, d(1, Severity::Error, "ERROR"))]);
    }

    #[test]
    fn allow_force_and_block_levels_decide_what_blocks() {
        let changes = [change("a.rs", "a\n", "ERROR\nWARN\n")];
        let error = (0, d(0, Severity::Error, "ERROR"));
        let warning = (0, d(1, Severity::Warning, "WARN"));
        let mut lsp = TextLsp::default();
        assert_eq!(
            checked(&mut lsp, &changes, None, false).blocking,
            std::slice::from_ref(&error)
        );
        assert!(
            checked(&mut lsp, &changes, Some(Severity::Error), false)
                .blocking
                .is_empty()
        );
        assert!(checked(&mut lsp, &changes, None, true).blocking.is_empty());
        assert_eq!(checked(&mut lsp, &changes, None, true).files[0].len(), 2);

        let mut lsp = TextLsp {
            block: Some(Some(Severity::Warning)),
            ..TextLsp::default()
        };
        assert_eq!(
            checked(&mut lsp, &changes, None, false).blocking,
            [error.clone(), warning]
        );
        assert_eq!(
            checked(&mut lsp, &changes, Some(Severity::Warning), false).blocking,
            [error]
        );

        let mut lsp = TextLsp {
            block: Some(None),
            ..TextLsp::default()
        };
        assert!(checked(&mut lsp, &changes, None, false).blocking.is_empty());
    }

    #[test]
    fn check_changes_skips_files_without_a_server_or_language() {
        let mut lsp = TextLsp {
            show: Some(Severity::Hint),
            ..TextLsp::default()
        };
        let changes = [
            change("a.md", "a\n", "ERROR\n"),
            change("a.txt", "a\n", "ERROR\n"),
            change("a.rs", "a\n", "HINT\n"),
        ];
        let out = checked(&mut lsp, &changes, None, false);
        assert_eq!(
            out.files,
            [vec![], vec![], vec![d(0, Severity::Hint, "HINT")]]
        );
        assert!(out.blocking.is_empty());
        assert_eq!(lsp.asked, [vec!["a\n", "a\n"], vec!["ERROR\n", "HINT\n"]]);
    }

    #[test]
    fn check_changes_passes_failures_on() {
        let mut lsp = TextLsp {
            failure: Some("fake didn't answer"),
            ..TextLsp::default()
        };
        let changes = [change("a.rs", "a\n", "ERROR\n")];
        let err = check_changes(&mut lsp, &changes, &["ERROR\n"], None, false).unwrap_err();
        assert_eq!(err, LspFailure("fake didn't answer".into()));
    }

    #[test]
    fn check_changes_without_typed_files_asks_nothing() {
        let mut lsp = TextLsp::default();
        let changes = [change("a.txt", "a\n", "ERROR\n")];
        assert_eq!(
            checked(&mut lsp, &changes, None, false),
            Checked {
                files: vec![vec![]],
                blocking: vec![]
            }
        );
        assert!(lsp.asked.is_empty());
    }

    fn saved(lsp: &mut TextLsp, changes: &[Change], already: Option<&Checked>) -> Saved {
        let finals: Vec<&str> = changes.iter().map(|c| c.new.as_str()).collect();
        let before = before_save(lsp, changes).unwrap();
        check_saved(lsp, before, changes, &finals, already).unwrap()
    }

    #[test]
    fn a_write_is_diagnosed_as_saved_before_and_after() {
        let mut lsp = TextLsp::default();
        let changes = [change("a.rs", "a\n", "a\nCARGO\n")];
        let out = saved(&mut lsp, &changes, None);
        assert_eq!(lsp.asked, [vec!["a\n"], vec!["a\nCARGO\n"]]);
        assert_eq!(lsp.saved, [true, true]);
        assert_eq!(out.files, [vec![d(1, Severity::Error, "CARGO")]]);
    }

    #[test]
    fn a_write_reports_only_what_it_introduced_and_the_edit_check_did_not() {
        let mut lsp = TextLsp::default();
        let changes = [change("a.rs", "CARGO\n", "CARGO\nCARGO\nERROR\nHINT\n")];
        let already = Checked {
            files: vec![vec![d(2, Severity::Error, "ERROR")]],
            blocking: Vec::new(),
        };
        let out = saved(&mut lsp, &changes, Some(&already));
        assert_eq!(out.files, [vec![d(1, Severity::Error, "CARGO")]]);
    }

    #[test]
    fn a_created_file_has_nothing_before_its_write() {
        let mut lsp = TextLsp::default();
        let changes = [change("a.rs", "a\n", "b\n"), change("n.rs", "", "CARGO\n")];
        let out = saved(&mut lsp, &changes, None);
        assert_eq!(lsp.asked, [vec!["a\n"], vec!["b\n", "CARGO\n"]]);
        assert_eq!(out.files, [vec![], vec![d(0, Severity::Error, "CARGO")]]);
    }

    #[test]
    fn a_write_without_typed_files_asks_nothing() {
        let mut lsp = TextLsp::default();
        let out = saved(&mut lsp, &[change("a.txt", "a\n", "CARGO\n")], None);
        assert!(lsp.asked.is_empty());
        assert_eq!(
            out,
            Saved {
                files: vec![vec![]],
                notes: vec![]
            }
        );
    }

    #[test]
    fn notes_on_a_saved_diagnosis_are_passed_on() {
        let mut lsp = TextLsp {
            notes: vec!["cargo check didn't finish".into()],
            ..TextLsp::default()
        };
        let out = saved(&mut lsp, &[change("a.rs", "a\n", "b\n")], None);
        assert_eq!(out.notes, ["cargo check didn't finish"]);
    }
}
