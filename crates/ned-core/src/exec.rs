//! Running scripts against files, and the errors that can stop a run.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use tree_sitter::{Node, QueryCursor, StreamingIterator, Tree};

use crate::edit::{Edit, EditError, EditSet};
use crate::lang::{self, Language};
use crate::lsp::{self, Document, Locate, Located, Lsp, LspFailure, Renamed, Severity, render};
use crate::outline;
use crate::script::Script;
use crate::script::ast::{
    Command, CommandKind, Part, Pattern, Position, Primary, Selector, Step, Target, Text, TextKind,
};
use crate::script::error::{excerpt, location};
use crate::select::{self, Match, SourceFile, line_numbers, same_path};
use crate::span::Span;
use crate::syntax::{self, Item};
use crate::template::Template;
use crate::text;
use crate::workspace;

/// Where the initial file set comes from (spec §2.4).
#[derive(Debug, Clone)]
pub enum Initial<'a> {
    /// `FILE` arguments; globs are expanded.
    Files(&'a [String]),
    /// Every file in the workspace at this root (`-w`).
    Workspace(PathBuf),
}

/// The result of running a script: the output of the reads that ran, in
/// command order, and either every modified file or the error that rejected
/// the script.
#[derive(Debug)]
pub struct Run {
    pub output: String,
    pub result: Result<Vec<Change>, ExecError>,
    /// The script's most permissive `allow` (§4.4).
    pub allow: Option<Severity>,
    /// Notes for stderr, e.g. from `check`.
    pub notes: Vec<String>,
}

/// A modified file, with the number of spans edited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    pub old: String,
    pub new: String,
    pub edits: usize,
    pub lang: Option<Language>,
    /// Made by `create`; `old` is empty.
    pub created: bool,
}

/// Settings from the command line that affect a run.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// The language of every file, instead of detecting it; `Some(None)` reads
    /// every file as text.
    pub lang: Option<Option<Language>>,
    /// Skip the parse-error guard (§4.3).
    pub force: bool,
}

/// Runs `script` (parsed from `src`) on the `initial` file set. Nothing is
/// written.
pub fn run<'s, 'l: 's>(
    script: &Script,
    src: &'s str,
    initial: Initial,
    options: &'s Options,
    lsp: Option<&'s mut (dyn Lsp + 'l)>,
) -> Run {
    let mut executor = Executor {
        src,
        options,
        lsp: lsp.map(|lsp| -> &'s mut dyn Lsp { lsp }),
        allow: None,
        named: Vec::new(),
        workspace: None,
        files: Vec::new(),
        set: Vec::new(),
        output: String::new(),
        notes: Vec::new(),
        unknown: BTreeSet::new(),
    };
    let result = executor.run(script, initial).map_err(|mut e| {
        if let (Some(None), ExecErrorKind::NoLanguage { selector, .. })
        | (Some(None), ExecErrorKind::NoCodeLanguage { selector, .. }) = (options.lang, &e.kind)
        {
            e.kind = ExecErrorKind::ParsingDisabled {
                selector: selector.clone(),
            };
        }
        e
    });
    if !executor.unknown.is_empty() {
        let extensions: Vec<_> = executor.unknown.into_iter().collect();
        executor.notes.push(format!(
            "read {} files as text; syntax selectors skip them",
            extensions.join(", ")
        ));
    }
    Run {
        output: executor.output,
        result,
        allow: executor.allow,
        notes: executor.notes,
    }
}

struct Loaded {
    file: SourceFile,
    edits: EditSet,
    /// The whole-line deletions among `edits`, for merging neighbours.
    deletions: Vec<Deletion>,
    /// The character after a `.sig` that `edits` replace with text ending in
    /// it, which the parse-error guard's error then explains (§4.3).
    sig_end: Option<char>,
    /// Made by `create`: its original text is what `create` gave it.
    created: bool,
    /// The text before the first `|` changed it (§2.3), and the edits
    /// applied at each `|` since.
    original: Option<String>,
    applied: usize,
}

/// A whole-line deletion: its `lines`, and the span it `removed` once tidied.
struct Deletion {
    lines: Range<usize>,
    removed: Range<usize>,
    command: usize,
}

/// A file in the set: loaded into `files`, or only named so far (§2.4).
#[derive(Debug, Clone)]
struct Member {
    path: String,
    file: Option<usize>,
    /// From `-w`: skipped, rather than an error, if it can't be read.
    workspace: bool,
}

struct Executor<'s> {
    src: &'s str,
    options: &'s Options,
    /// Every file loaded so far, in order of first appearance.
    files: Vec<Loaded>,
    /// The current file set, read as commands need it.
    set: Vec<Member>,
    output: String,
    notes: Vec<String>,
    /// The unknown extensions of files read as text, for one note.
    unknown: BTreeSet<String>,
    /// The workspace's language servers, for `check`.
    lsp: Option<&'s mut dyn Lsp>,
    /// The most permissive `allow` so far.
    allow: Option<Severity>,
    /// Every path the file set has held, in the order the script named them.
    named: Vec<String>,
    /// Every file `-w` listed: what `rename` may reach beyond the set.
    workspace: Option<Vec<String>>,
}

impl Executor<'_> {
    fn run(&mut self, script: &Script, initial: Initial) -> Result<Vec<Change>, ExecError> {
        self.set = match initial {
            Initial::Files(paths) => self.open(paths, None)?,
            Initial::Workspace(root) => {
                let cwd = std::env::current_dir().unwrap_or_default();
                let paths = workspace::files(&root, &cwd);
                self.workspace = Some(paths.clone());
                paths
                    .into_iter()
                    .map(|path| Member {
                        path,
                        file: None,
                        workspace: true,
                    })
                    .collect()
            }
        };
        self.named = self.set.iter().map(|m| m.path.clone()).collect();
        for (index, command) in script.commands.iter().enumerate() {
            if script.stages.contains(&index) {
                self.commit()?;
            }
            if let Err(e) = self.command(index, command) {
                return Err(self.pipe_hint(index, command, e));
            }
        }
        // Files are read as commands need them, so they're reported in the
        // order the script named them instead.
        let rank = |path: &str| self.named.iter().position(|n| same_path(n, path));
        let mut changed: Vec<&Loaded> = self
            .files
            .iter()
            .filter(|l| l.created || l.applied > 0 || !l.edits.is_empty())
            .collect();
        changed.sort_by_key(|l| rank(&l.file.path));
        let changes: Vec<Change> = changed
            .iter()
            .map(|l| Change {
                path: l.file.path.clone(),
                old: match (&l.original, l.created) {
                    (_, true) => String::new(),
                    (Some(original), false) => original.clone(),
                    (None, false) => l.file.text.clone(),
                },
                new: l.edits.apply(),
                edits: l.applied + l.edits.len(),
                lang: l.file.lang,
                created: l.created,
            })
            .collect();
        if !self.options.force {
            for (l, change) in changed.iter().zip(&changes) {
                guard(l, &change.new)?;
            }
        }
        Ok(changes)
    }

    /// Applies each file's edits at a `|`, after the parse-error guard, so the
    /// next stage sees them (§2.3).
    fn commit(&mut self) -> Result<(), ExecError> {
        for l in &mut self.files {
            if l.edits.is_empty() {
                continue;
            }
            let new = l.edits.apply();
            if !self.options.force {
                guard(l, &new)?;
            }
            l.applied += l.edits.len();
            l.original.get_or_insert_with(|| l.file.text.clone());
            l.file = SourceFile::new(&l.file.path, new, l.file.lang);
            l.edits = EditSet::new(&l.file.buffer);
            l.deletions.clear();
            l.sig_end = None;
        }
        Ok(())
    }

    /// Hints at a `|` when `error` is a selector that matches nothing in this
    /// stage's input but matches once the stage's earlier edits apply (§2.3).
    fn pipe_hint(&mut self, index: usize, command: &Command, mut error: ExecError) -> ExecError {
        let ExecErrorKind::NoMatch { hint, .. } = &mut error.kind else {
            return error;
        };
        if self.files.iter().all(|l| l.edits.is_empty()) {
            return error;
        }
        // The script has failed, so this run's state can be spent on the trial,
        // as long as it prints nothing and asks no language server.
        let (output, notes) = (self.output.len(), self.notes.len());
        self.lsp = None;
        if self.commit().is_err() {
            return error;
        }
        let retry = self.command(index, command);
        self.output.truncate(output);
        self.notes.truncate(notes);
        let unmatched =
            |e: &ExecError| e.span == error.span && matches!(e.kind, ExecErrorKind::NoMatch { .. });
        if !retry.as_ref().is_err_and(unmatched) {
            *hint = "; it matches only after the edits before it, which a stage's selectors \
                don't see: put a `|` before the command"
                .into();
        }
        error
    }

    /// The language of the file at `path`, holding `text`, from `--lang` or
    /// detected. Notes an unknown extension that makes it text.
    fn lang(&mut self, path: &str, text: &str) -> Option<Language> {
        if let Some(lang) = self.options.lang {
            return lang;
        }
        let lang = Language::detect(path, text);
        if let (None, Some(extension)) = (lang, lang::unknown_extension(path)) {
            self.unknown.insert(format!(".{extension}"));
        }
        lang
    }

    /// Adds a file made by `create` to the file set, holding `new` (§4.2).
    fn create(&mut self, path: &str, new: &Text) -> Result<(), ExecErrorKind> {
        let loaded = self.files.iter().any(|l| same_path(&l.file.path, path))
            || self.set.iter().any(|m| same_path(&m.path, path));
        if loaded || std::path::Path::new(path).exists() {
            return Err(ExecErrorKind::FileExists { path: path.into() });
        }
        let lang = self.lang(path, &new.value);
        let text = if new.value.is_empty() {
            String::new()
        } else {
            line_oriented(
                new,
                "",
                lang.map_or("    ", |l| l.default_indent()),
                text::rebase,
            )
        };
        let file = SourceFile::new(path, text, lang);
        let edits = EditSet::new(&file.buffer);
        self.files.push(Loaded {
            file,
            edits,
            deletions: Vec::new(),
            sig_end: None,
            original: None,
            applied: 0,
            created: true,
        });
        self.named.push(path.into());
        self.set.push(Member {
            path: path.into(),
            file: Some(self.files.len() - 1),
            workspace: false,
        });
        Ok(())
    }

    /// The files `paths` name, expanding globs (§2.4), without duplicates.
    /// They're read later, but a missing one is an error now.
    fn open(
        &mut self,
        paths: &[String],
        span: Option<&Range<usize>>,
    ) -> Result<Vec<Member>, ExecError> {
        let mut set: Vec<Member> = Vec::new();
        for path in paths {
            for path in expand(path, span)? {
                if set.iter().any(|m| same_path(&m.path, &path)) {
                    continue;
                }
                let file = self
                    .files
                    .iter()
                    .position(|l| same_path(&l.file.path, &path));
                if file.is_none()
                    && let Err(err) = fs::metadata(&path)
                {
                    let message = match err.kind() {
                        std::io::ErrorKind::NotFound => {
                            format!("no such file{}", relative_note(&path))
                        }
                        _ => err.to_string(),
                    };
                    return Err(ExecError::new(
                        ExecErrorKind::Io { path, message },
                        span.cloned(),
                    ));
                }
                set.push(Member {
                    path,
                    file,
                    workspace: false,
                });
            }
        }
        Ok(set)
    }

    /// Reads the set's files whose paths satisfy `wanted`, returning their
    /// indices into `files`. Workspace files that can't be read leave the set.
    fn read(&mut self, wanted: impl Fn(&str) -> bool) -> Result<Vec<usize>, ExecError> {
        let mut indices = Vec::new();
        let mut i = 0;
        while i < self.set.len() {
            let member = &self.set[i];
            if !wanted(&member.path) {
                i += 1;
                continue;
            }
            let index = match member.file {
                Some(index) => index,
                None => {
                    let (path, workspace) = (member.path.clone(), member.workspace);
                    match self.load(&path, None) {
                        Ok(index) => {
                            self.set[i].file = Some(index);
                            index
                        }
                        Err(_) if workspace => {
                            self.set.remove(i);
                            continue;
                        }
                        Err(err) => return Err(err),
                    }
                }
            };
            indices.push(index);
            i += 1;
        }
        Ok(indices)
    }

    fn load(&mut self, path: &str, span: Option<Range<usize>>) -> Result<usize, ExecError> {
        if let Some(i) = self
            .files
            .iter()
            .position(|l| same_path(&l.file.path, path))
        {
            return Ok(i);
        }
        let io = |message: String| {
            ExecError::new(
                ExecErrorKind::Io {
                    path: path.into(),
                    message,
                },
                span.clone(),
            )
        };
        let bytes = fs::read(path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => io(format!("no such file{}", relative_note(path))),
            _ => io(e.to_string()),
        })?;
        let text = String::from_utf8(bytes).map_err(|_| io("not valid UTF-8".into()))?;
        let lang = self.lang(path, &text);
        let file = SourceFile::new(path, text, lang);
        let edits = EditSet::new(&file.buffer);
        self.files.push(Loaded {
            file,
            edits,
            deletions: Vec::new(),
            sig_end: None,
            original: None,
            applied: 0,
            created: false,
        });
        Ok(self.files.len() - 1)
    }

    fn command(&mut self, index: usize, command: &Command) -> Result<(), ExecError> {
        let span = &command.span;
        let error = |kind| ExecError::new(kind, Some(span.clone()));
        match &command.kind {
            CommandKind::Create { path, text } => return self.create(path, text).map_err(error),
            CommandKind::Allow(level) => {
                self.allow = Some(self.allow.map_or(*level, |allow| allow.min(*level)));
                return Ok(());
            }
            CommandKind::File(paths) => {
                self.set = self.open(paths, Some(span))?;
                for member in &self.set {
                    if !self.named.iter().any(|n| same_path(n, &member.path)) {
                        self.named.push(member.path.clone());
                    }
                }
                return Ok(());
            }
            _ if self.set.is_empty() => return Err(error(ExecErrorKind::NoFiles)),
            _ => {}
        }
        match &command.kind {
            CommandKind::Show { target, context } => self.show(target.as_ref(), *context)?,
            CommandKind::Outline(target) => self.outline(span, target.as_ref())?,
            CommandKind::Replace { target, text } => {
                let last = target.selector.steps.last().and_then(|s| s.parts.last());
                let whole = last == Some(&Part::Whole);
                let patterns = target
                    .selector
                    .steps
                    .iter()
                    .any(|s| !s.primary.patterns().is_empty());
                for m in self.resolve(target)? {
                    let f = &self.files[m.file].file;
                    let filled;
                    let text = match patterns {
                        true => {
                            filled = substitute(f, &m.captures, text).map_err(|(kind, name)| {
                                let span = placeholder_span(self.src, &target.selector.span, &name);
                                ExecError::new(kind, Some(span))
                            })?;
                            &filled
                        }
                        false => text,
                    };
                    if text.value.is_empty()
                        && !m.range.is_empty()
                        && text::is_whole_line(&f.text, &m.range)
                    {
                        self.delete(index, span, m.file, m.range)?;
                        continue;
                    }
                    let selector = &self.src[target.selector.span.clone()];
                    if let Some(note) = off_by_one(f, &m.range, text, selector) {
                        self.notes.push(note);
                    }
                    let after = f.text[m.range.end..].trim_start_matches([' ', '\t']);
                    let sig_end = after.chars().next().filter(|&c| {
                        last == Some(&Part::Sig)
                            && c.is_ascii_punctuation()
                            && text.value.trim_end().ends_with(c)
                    });
                    let (range, new) = replace(f, m.range, text, whole);
                    if sig_end.is_some() {
                        self.files[m.file].sig_end = sig_end;
                    }
                    self.push(index, span, m.file, range, new)?;
                }
            }
            CommandKind::Insert {
                position,
                target,
                text,
            } => {
                for m in self.resolve(&implied_body(target, *position))? {
                    let f = &self.files[m.file].file;
                    let range = heredoc_lines(f, target, *position, text, m.range);
                    let text = separated(f, target, *position, &range, text);
                    let (range, new) = insert(f, range, *position, &text, &target.selector);
                    self.push(index, span, m.file, range, new)?;
                }
            }
            CommandKind::Delete(target) => {
                for m in self.resolve(target)? {
                    self.delete(index, span, m.file, m.range)?;
                }
            }
            CommandKind::Sub {
                scope,
                pattern,
                text,
            } => self.sub(index, span, scope.as_ref(), pattern, text)?,
            CommandKind::Move {
                target,
                position,
                dest,
            } => self.move_to(index, span, target, *position, dest)?,
            CommandKind::Check { target, level } => self.check(span, target.as_ref(), *level)?,
            CommandKind::Rename { selector, name } => self.rename(index, span, selector, name)?,
            CommandKind::File(_) | CommandKind::Create { .. } | CommandKind::Allow(_) => {
                unreachable!("handled above")
            }
        }
        Ok(())
    }

    /// Resolves `target` in the current file set, returning matches whose
    /// `file` indexes `self.files`. A leading `file:` step reads only its
    /// file.
    fn resolve(&mut self, target: &Target) -> Result<Vec<Match>, ExecError> {
        let located = |s: &Step| s.parts.iter().any(|p| matches!(p, Part::Refs | Part::Def));
        if target.selector.steps.iter().any(located) {
            return self.resolve_located(target);
        }
        let only = match target.selector.steps.first().map(|s| &s.primary) {
            Some(Primary::File(path)) => {
                if !self.set.iter().any(|m| same_path(&m.path, path)) {
                    let paths: Vec<&str> = self.set.iter().map(|m| m.path.as_str()).collect();
                    return Err(ExecError::new(
                        ExecErrorKind::NotInFileSet {
                            path: path.clone(),
                            files: select::file_list(&paths),
                            add: select::add_to_set(&paths, path),
                        },
                        Some(target.selector.span.clone()),
                    ));
                }
                Some(path.clone())
            }
            _ => None,
        };
        let indices = self.read(|p| only.as_ref().is_none_or(|only| same_path(p, only)))?;
        let set: Vec<&SourceFile> = indices.iter().map(|&i| &self.files[i].file).collect();
        let matches = select::resolve(target, &set, self.src)?;
        Ok(matches
            .into_iter()
            .map(|m| Match {
                file: indices[m.file],
                range: m.range,
                captures: m.captures,
            })
            .collect())
    }

    /// `resolve` for a selector with `.refs` or `.def`, which the language
    /// server resolves (§3.4).
    fn resolve_located(&mut self, target: &Target) -> Result<Vec<Match>, ExecError> {
        let span = target.selector.span.clone();
        let what = &self.src[span.clone()];
        let located = |p: &Part| matches!(p, Part::Refs | Part::Def);
        let mut rest = target.selector.steps.as_slice();
        let mut matches: Option<Vec<Match>> = None;
        while !rest.is_empty() {
            // The steps up to the next `.refs` or `.def`, resolved first.
            let split = rest
                .iter()
                .enumerate()
                .find_map(|(i, s)| s.parts.iter().position(located).map(|j| (i, j)));
            let (mut steps, parts) = match split {
                Some((i, j)) => {
                    let mut steps = rest[..i].to_vec();
                    steps.push(Step {
                        primary: rest[i].primary.clone(),
                        parts: rest[i].parts[..j].to_vec(),
                        filters: Vec::new(),
                        span: rest[i].span.clone(),
                    });
                    (steps, &rest[i].parts[j..])
                }
                None => (rest.to_vec(), &[][..]),
            };
            if !parts.is_empty() {
                at_name(&mut steps);
            }
            let segment = Target {
                all: true,
                selector: Selector {
                    steps,
                    span: span.clone(),
                },
            };
            let mut found = match matches {
                None => self.resolve(&segment)?,
                Some(start) => {
                    let files: Vec<&SourceFile> = self.files.iter().map(|l| &l.file).collect();
                    select::resolve_within(&segment, &files, start, self.src)?
                }
            };
            for part in parts {
                found = match part {
                    Part::Refs => self.locate(Locate::References, &found, what, &span)?,
                    Part::Def => self.locate(Locate::Definition, &found, what, &span)?,
                    part => found
                        .into_iter()
                        .map(|m| {
                            let plain = Span {
                                range: m.range,
                                item: None,
                            };
                            let spans = plain.part(*part, &self.files[m.file].file.text)?;
                            let (file, captures) = (m.file, m.captures);
                            Ok(spans.into_iter().map(move |s| Match {
                                file,
                                range: s.range,
                                captures: captures.clone(),
                            }))
                        })
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|kind| ExecError::new(kind, Some(span.clone())))?
                        .into_iter()
                        .flatten()
                        .collect(),
                };
            }
            let filters = match split {
                Some((i, _)) => rest[i].filters.as_slice(),
                None => &[],
            };
            let mut kept = Vec::new();
            for m in found {
                let plain = Span {
                    range: m.range.clone(),
                    item: None,
                };
                let text = &self.files[m.file].file.text;
                if plain
                    .passes(filters, text)
                    .map_err(|kind| ExecError::new(kind, Some(span.clone())))?
                {
                    kept.push(m);
                }
            }
            found = kept;
            matches = Some(found);
            rest = match split {
                Some((i, _)) => &rest[i + 1..],
                None => &[],
            };
        }
        let matches = matches.unwrap_or_default();
        let error = |kind| ExecError::new(kind, Some(span.clone()));
        match matches.len() {
            0 => {
                let paths: Vec<&str> = self.set.iter().map(|m| m.path.as_str()).collect();
                Err(error(ExecErrorKind::NoMatch {
                    selector: what.into(),
                    files: select::file_list(&paths),
                    hint: String::new(),
                    searched: Some(paths.len()),
                }))
            }
            1 => Ok(matches),
            _ if target.all => Ok(matches),
            total => {
                let locations: Vec<String> = matches
                    .iter()
                    .map(|m| {
                        let f = &self.files[m.file].file;
                        format!("{}:{}", f.path, line_numbers(&f.buffer, &m.range))
                    })
                    .collect();
                let locations: Vec<&str> = locations.iter().map(String::as_str).collect();
                Err(error(ExecErrorKind::AmbiguousLocated {
                    selector: what.into(),
                    total,
                    locations: select::file_list(&locations),
                }))
            }
        }
    }

    /// The references to, or definitions of, the symbols at `matches` (§3.4),
    /// in order; `what` is the selector, for errors.
    fn locate(
        &mut self,
        kind: Locate,
        matches: &[Match],
        what: &str,
        span: &Range<usize>,
    ) -> Result<Vec<Match>, ExecError> {
        let error = |kind| ExecError::new(kind, Some(span.clone()));
        let mut locations = Vec::new();
        for m in matches {
            let (document, position) = self.symbol(m, what, span)?;
            let Some(lsp) = self.lsp.as_deref_mut() else {
                let message = "`.refs` and `.def` need the language-server daemon, which is Unix-only for now; select with a /regex/ or kind:NAME instead";
                return Err(error(ExecErrorKind::Lsp(message.into())));
            };
            match lsp.locate(kind, &document, position) {
                Ok(Located::Locations(found)) => locations.extend(found),
                Ok(Located::NoServer) => {
                    return Err(error(ExecErrorKind::NoServer {
                        langs: document.lang.name().into(),
                    }));
                }
                Err(LspFailure(message)) => return Err(error(ExecErrorKind::Lsp(message))),
            }
        }
        let paths: Vec<&Path> = locations.iter().map(|l| l.path.as_path()).collect();
        let files = self.reach(&paths, what, span)?;
        let mut found: Vec<Match> = locations
            .iter()
            .zip(files)
            .map(|(l, file)| {
                let f = &self.files[file].file;
                let start = f.buffer.lsp_offset(l.start.line, l.start.character);
                let end = f.buffer.lsp_offset(l.end.line, l.end.character);
                let range = match kind {
                    Locate::References => start..end,
                    Locate::Definition => defining_item(f, start).unwrap_or(start..end),
                };
                Match {
                    file,
                    range,
                    captures: Vec::new(),
                }
            })
            .collect();
        let rank = |m: &Match| {
            let path = &self.files[m.file].file.path;
            (
                self.named.iter().position(|n| same_path(n, path)),
                m.range.start,
            )
        };
        found.sort_by_key(rank);
        found.dedup();
        Ok(found)
    }

    /// The document holding `m` and the LSP position of its start, for asking
    /// about the symbol there; `what` names the feature, for errors.
    fn symbol(
        &self,
        m: &Match,
        what: &str,
        span: &Range<usize>,
    ) -> Result<(Document, lsp::Position), ExecError> {
        let error = |kind| ExecError::new(kind, Some(span.clone()));
        let file = &self.files[m.file].file;
        let Some(lang) = file.lang else {
            return Err(error(ExecErrorKind::NoLanguage {
                selector: what.into(),
                files: file.path.clone(),
            }));
        };
        let path = std::path::absolute(&file.path).map_err(|err| {
            error(ExecErrorKind::Io {
                path: file.path.clone(),
                message: err.to_string(),
            })
        })?;
        let (line, character) = file.buffer.lsp_position(m.range.start);
        let document = Document {
            path,
            lang,
            text: file.text.clone(),
        };
        Ok((document, lsp::Position { line, character }))
    }

    /// Loads the files a server named, each by the path the script knows it
    /// by, which must be in the file set or, under `-w`, the workspace (§3.4);
    /// `what` names the feature, for errors.
    fn reach(
        &mut self,
        paths: &[&Path],
        what: &str,
        span: &Range<usize>,
    ) -> Result<Vec<usize>, ExecError> {
        let mut reached = Vec::new();
        let mut outside = Vec::new();
        for path in paths {
            match self.reachable(path) {
                Ok(path) => reached.push(path),
                Err(shown) => outside.push(shown),
            }
        }
        if !outside.is_empty() {
            outside.dedup();
            let shown: Vec<&str> = outside.iter().map(String::as_str).collect();
            return Err(ExecError::new(
                ExecErrorKind::Outside {
                    what: what.into(),
                    files: select::file_list(&shown),
                    workspace: self.workspace.is_some(),
                },
                Some(span.clone()),
            ));
        }
        let mut files = Vec::new();
        for path in reached {
            files.push(self.load(&path, Some(span.clone()))?);
            if !self.named.iter().any(|n| same_path(n, &path)) {
                self.named.push(path);
            }
        }
        Ok(files)
    }

    fn push(
        &mut self,
        index: usize,
        span: &Range<usize>,
        file: usize,
        range: Range<usize>,
        text: String,
    ) -> Result<(), ExecError> {
        let loaded = &mut self.files[file];
        let edit = Edit {
            range,
            text,
            command: index,
        };
        loaded.edits.push(edit).map_err(|err| match err {
            EditError::Overlap { first, range, .. } => ExecError::new(
                ExecErrorKind::Overlap {
                    command: first + 1,
                    location: format!(
                        "{}:{}",
                        loaded.file.path,
                        line_numbers(&loaded.file.buffer, &range)
                    ),
                },
                Some(span.clone()),
            ),
            EditError::Buffer(err) => unreachable!("edits come from resolved spans: {err}"),
        })
    }

    /// Deletes `range`. Whole lines are tidied (§4.2) after merging them with
    /// earlier whole-line deletions that only blank lines separate from them
    /// (§2.3), so neighbours don't both claim the blank line between them.
    fn delete(
        &mut self,
        index: usize,
        span: &Range<usize>,
        file: usize,
        range: Range<usize>,
    ) -> Result<(), ExecError> {
        let Loaded {
            file: f,
            edits,
            deletions,
            ..
        } = &mut self.files[file];
        if !text::is_whole_line(&f.text, &range) {
            return self.push(index, span, file, range, String::new());
        }
        let (mut lines, mut command) = (text::full_lines(&f.text, range), index);
        let beside = |lines: &Range<usize>, d: &Deletion| {
            let gap = if d.lines.end <= lines.start {
                d.lines.end..lines.start
            } else if lines.end <= d.lines.start {
                lines.end..d.lines.start
            } else {
                return false;
            };
            f.text[gap].trim().is_empty()
        };
        while let Some(i) = deletions.iter().position(|d| beside(&lines, d)) {
            let d = deletions.remove(i);
            edits.remove(&d.removed, d.command);
            lines = lines.start.min(d.lines.start)..lines.end.max(d.lines.end);
            command = command.min(d.command);
        }
        let removed = text::tidy_delete(&f.text, lines.clone());
        deletions.push(Deletion {
            lines,
            removed: removed.clone(),
            command,
        });
        self.push(command, span, file, removed, String::new())
    }

    /// Moves each span of `target` to `position` of `dest` (§4.2).
    fn move_to(
        &mut self,
        index: usize,
        span: &Range<usize>,
        target: &Target,
        position: Position,
        dest: &Selector,
    ) -> Result<(), ExecError> {
        let dest = Target {
            all: false,
            selector: dest.clone(),
        };
        let to = self.resolve(&implied_body(&dest, position))?.remove(0);
        for from in self.resolve(target)? {
            let source = &self.files[from.file].file;
            let removal = removal(source, from.range.clone());
            let (mut moved, separated) = moved_text(source, &from.range);
            let target = &self.files[to.file].file;
            let at = heredoc_lines(target, &dest, position, &moved, to.range.clone());
            // Doc comments and attributes attach to the item they move before.
            let attaches =
                matches!(position, Position::Before) && only_leading(target, &moved.value);
            if separated && !attaches && text::is_whole_line(&target.text, &at) {
                match position {
                    Position::Before => moved.value.push('\n'),
                    Position::After => moved.value.insert(0, '\n'),
                    Position::Start | Position::End => {}
                }
            }
            let moved = with_trailing_comma(target, &at, &moved);
            let (range, new) = insert(target, at, position, &moved, &dest.selector);
            if from.file == to.file && removal.start < range.start && range.end < removal.end {
                let location = format!(
                    "{}:{}",
                    source.path,
                    line_numbers(&source.buffer, &from.range)
                );
                return Err(ExecError::new(
                    ExecErrorKind::MoveIntoSource { location },
                    Some(span.clone()),
                ));
            }
            self.delete(index, span, from.file, from.range.clone())?;
            self.push(index, span, to.file, range, new)?;
        }
        Ok(())
    }

    fn show(&mut self, target: Option<&Target>, context: usize) -> Result<(), ExecError> {
        // (file, first line, last line), 0-based.
        let mut spans: Vec<(usize, usize, usize)> = Vec::new();
        match target {
            None => {
                for i in self.read(|_| true)? {
                    let count = self.files[i].file.buffer.line_count();
                    if count > 0 {
                        spans.push((i, 0, count - 1));
                    }
                }
            }
            Some(target) => {
                let found = match self.resolve(target) {
                    // `show all` is a search, and finding nothing is an answer.
                    Err(err) if target.all => match &err.kind {
                        ExecErrorKind::NoMatch {
                            selector,
                            searched: Some(n),
                            ..
                        } => {
                            let files = if *n == 1 { "file" } else { "files" };
                            let line = format!("no matches for {selector} in {n} {files}\n");
                            self.output.push_str(&line);
                            Vec::new()
                        }
                        _ => return Err(err),
                    },
                    found => found?,
                };
                for m in found {
                    let buffer = &self.files[m.file].file.buffer;
                    let max = buffer.line_count().saturating_sub(1);
                    let line = |offset| buffer.byte_to_line(offset).unwrap_or(max).min(max);
                    let first = line(m.range.start);
                    let last = if m.range.is_empty() {
                        first
                    } else {
                        line(m.range.end - 1)
                    };
                    spans.push((
                        m.file,
                        first.saturating_sub(context),
                        (last + context).min(max),
                    ));
                }
            }
        }
        let mut regions: Vec<(usize, usize, usize)> = Vec::new();
        for (file, first, last) in spans {
            match regions.last_mut() {
                Some((f, _, end)) if *f == file && first <= *end + 2 => *end = (*end).max(last),
                _ => regions.push((file, first, last)),
            }
        }
        for (file, first, last) in regions {
            let f = &self.files[file].file;
            let lines = if first == last {
                format!("{}", first + 1)
            } else {
                format!("{}-{}", first + 1, last + 1)
            };
            self.output.push_str(&format!("{}:{lines}\n", f.path));
            for line in first..=last {
                let range = f.buffer.line_range(line).expect("line within the file");
                let content = f.text[range].trim_end_matches('\n');
                let content = content.strip_suffix('\r').unwrap_or(content);
                self.output.push_str(&format!("{}:{content}\n", line + 1));
            }
        }
        Ok(())
    }

    fn outline(&mut self, span: &Range<usize>, target: Option<&Target>) -> Result<(), ExecError> {
        let error = |kind| ExecError::new(kind, Some(span.clone()));
        let spans: Vec<(usize, Option<Range<usize>>)> = match target {
            None => self
                .read(|_| true)?
                .into_iter()
                .map(|i| (i, None))
                .collect(),
            Some(target) => self
                .resolve(target)?
                .into_iter()
                .map(|m| (m.file, Some(m.range)))
                .collect(),
        };
        let mut files: Vec<usize> = spans.iter().map(|(i, _)| *i).collect();
        files.dedup();
        let any = files.iter().any(|&i| self.files[i].file.lang.is_some());
        if !any {
            return Err(error(ExecErrorKind::NoLanguage {
                selector: "outline".into(),
                files: select::file_list(
                    &files
                        .iter()
                        .map(|&i| self.files[i].file.path.as_str())
                        .collect::<Vec<_>>(),
                ),
            }));
        }
        let mut last = None;
        for (i, range) in spans {
            let f = &self.files[i].file;
            if f.lang.is_none() {
                continue;
            }
            if last != Some(i) {
                self.output.push_str(&format!("{}\n", f.path));
                last = Some(i);
            }
            self.output.push_str(&outline::render(f, range.as_ref()));
        }
        Ok(())
    }

    fn check(
        &mut self,
        span: &Range<usize>,
        target: Option<&Target>,
        level: Option<Severity>,
    ) -> Result<(), ExecError> {
        let error = |kind| ExecError::new(kind, Some(span.clone()));
        let spans: Vec<(usize, Option<Range<usize>>)> = match target {
            None => self
                .read(|_| true)?
                .into_iter()
                .map(|i| (i, None))
                .collect(),
            Some(target) => self
                .resolve(target)?
                .into_iter()
                .map(|m| (m.file, Some(m.range)))
                .collect(),
        };
        let mut files: Vec<usize> = spans.iter().map(|(i, _)| *i).collect();
        files.dedup();
        let typed: Vec<usize> = files
            .iter()
            .copied()
            .filter(|&i| self.files[i].file.lang.is_some())
            .collect();
        if typed.is_empty() {
            let names: Vec<&str> = files
                .iter()
                .map(|&i| self.files[i].file.path.as_str())
                .collect();
            return Err(error(ExecErrorKind::NoLanguage {
                selector: "check".into(),
                files: select::file_list(&names),
            }));
        }
        let documents = typed
            .iter()
            .map(|&i| {
                let f = &self.files[i].file;
                let path = std::path::absolute(&f.path).map_err(|err| {
                    error(ExecErrorKind::Io {
                        path: f.path.clone(),
                        message: err.to_string(),
                    })
                })?;
                Ok(Document {
                    path,
                    lang: f.lang.expect("typed"),
                    text: f.text.clone(),
                })
            })
            .collect::<Result<Vec<_>, ExecError>>()?;
        let Some(lsp) = self.lsp.as_deref_mut() else {
            let message = "`check` needs the language-server daemon, which is Unix-only for now; run the project's build or linter";
            return Err(error(ExecErrorKind::Lsp(message.into())));
        };
        let mut diagnosis = lsp
            .diagnose(&documents, true)
            .map_err(|LspFailure(message)| error(ExecErrorKind::Lsp(message)))?;

        self.notes.append(&mut diagnosis.notes);
        if diagnosis.files.iter().all(Option::is_none) {
            let mut langs: Vec<&str> = documents.iter().map(|d| d.lang.name()).collect();
            langs.dedup();
            return Err(error(ExecErrorKind::NoServer {
                langs: langs.join(", "),
            }));
        }
        let level = level.unwrap_or(diagnosis.show);
        let mut out = String::new();
        for (&i, diagnostics) in typed.iter().zip(diagnosis.files) {
            let Some(mut diagnostics) = diagnostics else {
                continue;
            };
            diagnostics.sort_by_key(|d| d.start);
            let f = &self.files[i].file;
            let ranges: Vec<&Range<usize>> = spans
                .iter()
                .filter(|(file, _)| *file == i)
                .filter_map(|(_, range)| range.as_ref())
                .collect();
            for d in diagnostics.iter().filter(|d| d.severity <= level) {
                let start = f.buffer.lsp_offset(d.start.line, d.start.character);
                let end = f
                    .buffer
                    .lsp_offset(d.end.line, d.end.character)
                    .max(start + 1);
                if !ranges.is_empty() && !ranges.iter().any(|r| start < r.end && end > r.start) {
                    continue;
                }
                out.push_str(&render(&f.path, &f.buffer, d));
            }
        }
        if out.is_empty() {
            out = format!("no diagnostics at {level} or above\n");
        }
        self.output.push_str(&out);
        Ok(())
    }

    /// Renames the symbol at `selector` to `name`, via the language server (§4.2).
    fn rename(
        &mut self,
        index: usize,
        span: &Range<usize>,
        selector: &Selector,
        name: &str,
    ) -> Result<(), ExecError> {
        let error = |kind| ExecError::new(kind, Some(span.clone()));
        let mut steps = selector.steps.clone();
        at_name(&mut steps);
        let target = Target {
            all: false,
            selector: Selector {
                steps,
                span: selector.span.clone(),
            },
        };
        let m = self.resolve(&target)?.remove(0);
        let (document, position) = self.symbol(&m, "rename", span)?;
        let file = &self.files[m.file].file;
        let line_start = file
            .buffer
            .line_range(position.line as usize)
            .map_or(0, |r| r.start);
        let location = format!(
            "{}:{}:{}",
            file.path,
            position.line + 1,
            file.text[line_start..m.range.start].chars().count() + 1
        );
        let Some(lsp) = self.lsp.as_deref_mut() else {
            let message = r#"`rename` needs the language-server daemon, which is Unix-only for now; use sub /\bOLD\b/ with "NEW" over the files"#;
            return Err(error(ExecErrorKind::Lsp(message.into())));
        };
        let files = match lsp.rename(&document, position, name) {
            Ok(Renamed::Edits(files)) => files,
            Ok(Renamed::Refused(message)) => {
                return Err(error(ExecErrorKind::RenameRefused { location, message }));
            }
            Ok(Renamed::NoServer) => {
                return Err(error(ExecErrorKind::NoServer {
                    langs: document.lang.name().into(),
                }));
            }
            Err(LspFailure(message)) => return Err(error(ExecErrorKind::Lsp(message))),
        };
        let paths: Vec<&Path> = files.iter().map(|f| f.path.as_path()).collect();
        let reached = self.reach(&paths, "rename", span)?;
        for (file, f) in reached.into_iter().zip(files) {
            for edit in f.edits {
                let buffer = &self.files[file].file.buffer;
                let start = buffer.lsp_offset(edit.start.line, edit.start.character);
                let end = buffer.lsp_offset(edit.end.line, edit.end.character);
                self.push(index, span, file, start..end, edit.text)?;
            }
        }
        Ok(())
    }

    /// The path by which the script knows a file a server named: a set
    /// member's or, under `-w`, a workspace file's. Otherwise, the path to show
    /// in the error.
    fn reachable(&self, path: &Path) -> Result<String, String> {
        let cwd = std::env::current_dir().unwrap_or_default();
        let shown = |path: &Path| {
            path.strip_prefix(&cwd)
                .unwrap_or(path)
                .display()
                .to_string()
        };
        let Ok(canonical) = fs::canonicalize(path) else {
            return Err(shown(path));
        };
        let key = shown(&canonical);
        let workspace = self.workspace.iter().flatten();
        if let Some(found) = self
            .set
            .iter()
            .map(|m| &m.path)
            .chain(workspace)
            .find(|p| **p == key)
        {
            return Ok(found.clone());
        }
        // FILE arguments may name it by another path.
        self.set
            .iter()
            .filter(|m| !m.workspace)
            .find(|m| fs::canonicalize(&m.path).is_ok_and(|c| c == canonical))
            .map(|m| m.path.clone())
            .ok_or(key)
    }

    fn sub(
        &mut self,
        index: usize,
        span: &Range<usize>,
        scope: Option<&Target>,
        pattern: &Pattern,
        text: &Text,
    ) -> Result<(), ExecError> {
        let regex = pattern.regex();
        let scopes: Vec<Match> = match scope {
            // A scope's whole lines, as a nested step searches them (§3.4).
            Some(target) => self
                .resolve(target)?
                .into_iter()
                .map(|m| Match {
                    range: select::scope(&self.files[m.file].file.text, &m.range),
                    ..m
                })
                .collect(),
            None => self
                .read(|_| true)?
                .into_iter()
                .map(|file| Match {
                    file,
                    range: 0..self.files[file].file.text.len(),
                    captures: Vec::new(),
                })
                .collect(),
        };
        let mut total = 0;
        for scope in &scopes {
            let haystack = &self.files[scope.file].file.text[scope.range.clone()];
            let edits: Vec<(Range<usize>, String)> = regex
                .captures_iter(haystack)
                .map(|caps| {
                    let whole = caps.get(0).expect("group 0 always matches");
                    let mut expanded = String::new();
                    caps.expand(&text.value, &mut expanded);
                    let start = scope.range.start;
                    (start + whole.start()..start + whole.end(), expanded)
                })
                .collect();
            for (range, expanded) in edits {
                self.push(index, span, scope.file, range, expanded)?;
                total += 1;
            }
        }
        if total == 0 {
            let flags = [
                (pattern.flags.case_insensitive, "i"),
                (pattern.flags.dot_all, "s"),
            ]
            .iter()
            .filter_map(|(on, flag)| on.then_some(*flag))
            .collect::<String>();
            let selector = format!("/{}/{flags}", pattern.source.replace('/', "\\/"));
            let mut searched: Vec<usize> = scopes.iter().map(|m| m.file).collect();
            searched.dedup();
            let set: Vec<&SourceFile> = searched.iter().map(|&i| &self.files[i].file).collect();
            let parents: Vec<Match> = scopes
                .iter()
                .map(|m| Match {
                    file: searched
                        .iter()
                        .position(|&i| i == m.file)
                        .expect("scopes are searched"),
                    range: m.range.clone(),
                    captures: Vec::new(),
                })
                .collect();
            let step = Step {
                primary: Primary::Regex(pattern.clone()),
                parts: Vec::new(),
                filters: Vec::new(),
                span: span.clone(),
            };
            let hint = select::hint(&step, &set, &parents, &selector, 0);
            return Err(ExecError::new(
                ExecErrorKind::NoMatch {
                    selector,
                    files: select::file_list(
                        &set.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
                    ),
                    hint,
                    searched: None,
                },
                Some(span.clone()),
            ));
        }
        Ok(())
    }
}

/// Makes a syntax step with no parts, at the end of `steps`, select its
/// item's name, where language servers look for the symbol.
fn at_name(steps: &mut [Step]) {
    if let Some(step) = steps.last_mut()
        && matches!(step.primary, Primary::Syntax { .. })
        && step.parts.is_empty()
    {
        step.parts.push(Part::Name);
    }
}

/// The span of the innermost item in `f` whose name holds `offset`.
fn defining_item(f: &SourceFile, offset: usize) -> Option<Range<usize>> {
    f.items()?
        .iter()
        .filter(|i| i.name_range.contains(&offset))
        .map(|i| i.range.clone())
        .min_by_key(|r| r.len())
}

/// For a relative `path`, ` (paths are relative to DIR)`, naming the working
/// directory; otherwise empty.
fn relative_note(path: &str) -> String {
    match std::env::current_dir() {
        Ok(cwd) if std::path::Path::new(path).is_relative() => {
            format!(" (paths are relative to {})", cwd.display())
        }
        _ => String::new(),
    }
}

fn last_line_hint(line: &str) -> &'static str {
    if line == "$" {
        ""
    } else {
        "; use `$` for the last line"
    }
}

/// The files `path` names: itself, or a glob's sorted matches.
fn expand(path: &str, span: Option<&Range<usize>>) -> Result<Vec<String>, ExecError> {
    let options = glob::MatchOptions {
        require_literal_leading_dot: true,
        ..glob::MatchOptions::new()
    };
    let paths = match glob::glob_with(path, options) {
        Ok(paths) if path.contains(['*', '?', '[']) => paths,
        // Not a glob, or not a valid one, such as `a[.rs`: a plain path.
        _ => return Ok(vec![path.to_string()]),
    };
    let mut files: Vec<String> = paths
        .filter_map(Result::ok)
        .filter(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    if files.is_empty() {
        let kind = ExecErrorKind::NoGlobMatch {
            glob: path.into(),
            note: relative_note(path),
        };
        return Err(ExecError::new(kind, span.cloned()));
    }
    files.sort();
    Ok(files)
}

/// `target`, with `.body` added when `insert start|end` targets a syntax
/// step with no parts (§4.2).
fn implied_body(target: &Target, position: Position) -> Cow<'_, Target> {
    let last = target.selector.steps.last();
    let syntax =
        last.is_some_and(|s| matches!(s.primary, Primary::Syntax { .. }) && s.parts.is_empty());
    if !syntax || matches!(position, Position::Before | Position::After) {
        return Cow::Borrowed(target);
    }
    let mut target = target.clone();
    if let Some(step) = target.selector.steps.last_mut() {
        step.parts.push(Part::Body);
    }
    Cow::Owned(target)
}

/// The item whose `.body` is the empty span `range`, if any.
fn empty_body<'f>(f: &'f SourceFile, range: &Range<usize>) -> Option<&'f Item> {
    if !range.is_empty() {
        return None;
    }
    body_of(f, range)
}

/// The item whose `.body` is `range`, if any.
fn body_of<'f>(f: &'f SourceFile, range: &Range<usize>) -> Option<&'f Item> {
    f.items()?
        .iter()
        .find(|i| syntax::part(i, Part::Body, &f.text).as_ref() == Some(range))
}

/// The span and text that fill `item`'s empty body with `new` (§4.2):
/// line-oriented, one indent unit inside the item, and on lines of its own;
/// for an undelimited body, on the lines after its heading or docstring.
fn fill_body(
    f: &SourceFile,
    item: &Item,
    range: Range<usize>,
    new: &Text,
) -> (Range<usize>, String) {
    let t = &f.text;
    let unit = f.indent_unit();
    if item.undelimited {
        // The lines right after the heading or docstring, which may end the
        // file, at that line's indentation.
        let indent = text::indent_at(t, range.start.saturating_sub(1));
        let lines = line_oriented(new, indent, unit, text::rebase);
        let lead = if t[..range.start].ends_with('\n') {
            ""
        } else {
            "\n"
        };
        return (range, format!("{lead}{lines}"));
    }
    let indent = format!("{}{unit}", text::indent_at(t, item.node.start));
    let lines = line_oriented(new, &indent, unit, text::rebase);
    let body = item.body.clone().expect("an empty body is a body");
    let inner = body.start + 1..body.end - 1;
    if t[inner.clone()].contains('\n') {
        (range.start..range.start, lines)
    } else {
        let closer = text::indent_at(t, body.end - 1);
        (inner, format!("\n{lines}{closer}"))
    }
}

/// The text `l`'s current stage started from, as the parse-error guard
/// compares it: empty for a file `create` made in this stage.
fn stage_input(l: &Loaded) -> Cow<'_, SourceFile> {
    if l.created && l.original.is_none() {
        Cow::Owned(SourceFile::new(&l.file.path, String::new(), l.file.lang))
    } else {
        Cow::Borrowed(&l.file)
    }
}

/// Rejects `new`, the edited text of `l`, if it has more syntax errors than
/// its stage input (§4.3).
fn guard(l: &Loaded, new: &str) -> Result<(), ExecError> {
    let f = stage_input(l);
    let (Some(lang), Some(old)) = (f.lang, f.tree()) else {
        return Ok(());
    };
    let before = error_nodes(lang, old, &f.text).len();
    let tree = lang.parse(new);
    let errors = error_nodes(lang, &tree, new);
    if errors.len() <= before {
        return Ok(());
    }
    // The edits all lie between the texts' common prefix and suffix.
    let prefix = common_len(f.text.bytes(), new.bytes());
    let suffix = common_len(f.text.bytes().rev(), new.bytes().rev())
        .min(f.text.len().min(new.len()) - prefix);
    let changed = prefix..new.len() - suffix;
    let node = errors
        .iter()
        .find(|n| n.start_byte() <= changed.end && changed.start <= n.end_byte())
        .unwrap_or(&errors[0]);
    // An `ERROR` node can start well before the edits, even span the file.
    let start = node.start_byte().max(changed.start.min(node.end_byte()));
    let (line, column) = location(new, start);
    Err(ExecError::new(
        ExecErrorKind::SyntaxError {
            location: format!("{}:{line}:{column}", f.path),
            hint: match l.sig_end {
                Some(c) => {
                    format!("; `.sig` stops before the `{c}`, so leave it out of TEXT")
                }
                None => String::new(),
            } + &escape_hint(&new[changed]),
            excerpt: excerpt(new, start)
                .map(|e| format!("\n{e}"))
                .unwrap_or_default(),
        },
        None,
    ))
}

/// The guard's hint for `text`, edited text that holds an escape such as
/// `\x27`, which a heredoc takes as written; empty if it holds none.
fn escape_hint(text: &str) -> String {
    text.match_indices("\\x")
        .find_map(|(i, _)| {
            let digits = text.get(i + 2..i + 4)?;
            digits.bytes().all(|b| b.is_ascii_hexdigit()).then_some(digits)
        })
        .map(|digits| {
            format!(
                "; heredocs read no escapes, so `\\x{digits}` went in as written: pass a script that holds a ' on stdin (ned FILE <<'EOF') instead of escaping it into -e"
            )
        })
        .unwrap_or_default()
}

/// The `ERROR` and `MISSING` nodes of `tree`, a parse of `text`, and the
/// matches of `lang`'s error query, in source order.
fn error_nodes<'t>(lang: Language, tree: &'t Tree, text: &str) -> Vec<Node<'t>> {
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.is_error() || node.is_missing() {
            out.push(node);
        }
        if node.has_error() {
            stack.extend(node.children(&mut node.walk()));
        }
    }
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(lang.errors(), tree.root_node(), text.as_bytes());
    while let Some(m) = matches.next() {
        out.extend(m.captures().iter().map(|c| c.node));
    }
    out.sort_by_key(|n| (n.start_byte(), n.end_byte()));
    out
}

fn common_len(a: impl Iterator<Item = u8>, b: impl Iterator<Item = u8>) -> usize {
    a.zip(b).take_while(|(x, y)| x == y).count()
}

/// `text` with the pattern captures of a match in `f` substituted (§3.10).
/// Err with the name of a placeholder it can't fill.
fn substitute(
    f: &SourceFile,
    captures: &select::Captures,
    text: &Text,
) -> Result<Text, (ExecErrorKind, String)> {
    let capture = |name: &str| {
        let (_, range) = captures.iter().find(|(n, _)| n == name)?;
        Some((
            &f.text[range.clone()],
            text::indent_at(&f.text, range.start),
        ))
    };
    let value = Template::parse(&text.value).fill(capture).map_err(|name| {
        if name == "_" {
            return (ExecErrorKind::WildcardInText, name);
        }
        let names: Vec<String> = captures.iter().map(|(n, _)| format!("@{n}")).collect();
        let literal = format!("write `@@{name}` for a literal `@`");
        let fix = match names.is_empty() {
            true => literal,
            false => format!("use {}, or {literal}", names.join(", ")),
        };
        (
            ExecErrorKind::UnknownCapture {
                name: name.clone(),
                fix,
            },
            name,
        )
    })?;
    Ok(Text {
        value,
        kind: text.kind,
    })
}

/// Where `@name` is in the script's TEXT after `selector`, or the selector
/// if it can't be found there.
fn placeholder_span(src: &str, selector: &Range<usize>, name: &str) -> Range<usize> {
    let placeholder = format!("@{name}");
    let rest = &src[selector.end..];
    let word = |c: char| c == '_' || c.is_ascii_alphanumeric();
    rest.match_indices(&placeholder)
        .map(|(i, _)| i)
        .find(|&i| !rest[..i].ends_with('@') && !rest[i + placeholder.len()..].starts_with(word))
        .map_or(selector.clone(), |i| {
            selector.end + i..selector.end + i + placeholder.len()
        })
}

/// The span and text that replace `range` (§5.1). Unless `whole` (`.whole`),
/// a syntax item keeps its leading docs and attributes.
fn replace(f: &SourceFile, range: Range<usize>, new: &Text, whole: bool) -> (Range<usize>, String) {
    if let Some(item) = empty_body(f, &range) {
        return fill_body(f, item, range, new);
    }
    let t = &f.text;
    let new = &*with_trailing_comma(f, &range, new);
    let range = match whole {
        true => range,
        false => without_leading(f, range, new),
    };
    let unit = f.indent_unit();
    if text::is_whole_line(t, &range) {
        let full = text::full_lines(t, range);
        let indent = match list_anchor(f, full.start, new) {
            Some(item) => text::indent_at(t, item.range.start),
            None => text::indent_at(t, full.start),
        };
        let mut new = line_oriented(new, indent, unit, text::rebase_replacing);
        if !t[..full.end].ends_with('\n') {
            new.pop();
        }
        (full, new)
    } else {
        let indent = text::indent_at(t, range.start);
        (range, verbatim(new, indent, unit))
    }
}

/// `range` without the leading doc comments and attributes of the syntax item
/// it is, when `new` doesn't start with its own (§4.2).
fn without_leading(f: &SourceFile, range: Range<usize>, new: &Text) -> Range<usize> {
    let t = &f.text;
    let Some(item) = f
        .items()
        .and_then(|items| items.iter().find(|i| i.range == range))
    else {
        return range;
    };
    let line = t[..item.node.start].rfind('\n').map_or(0, |i| i + 1);
    // An attribute on the item's own line goes with it.
    if item.node.start == range.start || !t[line..item.node.start].trim().is_empty() {
        return range;
    }
    if leading_len(f, &new.value) > 0 {
        return range;
    }
    line..range.end
}

/// The length of the doc comments and attributes that `text` starts with, in
/// `f`'s language.
fn leading_len(f: &SourceFile, text: &str) -> usize {
    let Some(lang) = f.lang else {
        return 0;
    };
    syntax::leading_len(lang.selectors(), &lang.parse(text), text)
}

/// Whether `text` is only doc comments and attributes, in `f`'s language.
fn only_leading(f: &SourceFile, text: &str) -> bool {
    let len = text.trim_end().len();
    len > 0 && leading_len(f, text) == len
}

/// A note when replacing `range` with `new` looks off by one (§4.2): `new`
/// repeats the line just outside a whole-line span, or the rest of a partial
/// span's line, which `selector.lines` would have replaced.
fn off_by_one(f: &SourceFile, range: &Range<usize>, new: &Text, selector: &str) -> Option<String> {
    let t = &f.text;
    let counts = |s: &str| s.chars().any(char::is_alphanumeric);
    // `TEXT` may re-wrap what it repeats, so whitespace doesn't count either.
    let squash = |s: &str| s.split_whitespace().collect::<String>();
    let line_of = |offset: usize| f.buffer.byte_to_line(offset).map_or(0, |l| l + 1);
    let at = format!("{}:{}", f.path, line_of(range.start));
    let value = new.value.trim_matches(['\n', '\r']);
    if text::is_whole_line(t, range) {
        let full = text::full_lines(t, range.clone());
        let lines: Vec<&str> = t[full.clone()].lines().map(str::trim).collect();
        let first = value.lines().map(str::trim).find(|l| !l.is_empty())?;
        let last = value.lines().map(str::trim).rfind(|l| !l.is_empty())?;
        let above = t[..full.start].strip_suffix('\n').map(|before| {
            let start = before.rfind('\n').map_or(0, |i| i + 1);
            (line_of(start), before[start..].trim())
        });
        let below = t[full.end..]
            .lines()
            .next()
            .map(|l| (line_of(full.end), l.trim()));
        if let Some((n, line)) = above
            && first == line
            && counts(line)
            && lines.first() != Some(&line)
        {
            return Some(format!(
                "{at}: the new text starts with a copy of line {n} (`{line}`), just above \
                 the replaced lines; the range may be off by one"
            ));
        }
        if let Some((n, line)) = below
            && full.end < t.len()
            && last == line
            && counts(line)
            && lines.last() != Some(&line)
        {
            return Some(format!(
                "{at}: the new text ends with a copy of line {n} (`{line}`), just below \
                 the replaced lines; the range may be off by one"
            ));
        }
        return None;
    }
    let line_start = t[..range.start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = t[range.end..].find('\n').map_or(t.len(), |i| range.end + i);
    let (before, after) = (
        t[line_start..range.start].trim(),
        t[range.end..line_end].trim(),
    );
    // Across lines, `.lines` would select each line.
    let fix = match t[range.clone()].trim_end_matches('\n').contains('\n') {
        true => String::new(),
        false => format!("; to replace whole lines, select {selector}.lines"),
    };
    if counts(after) && squash(value).ends_with(&squash(after)) {
        return Some(format!(
            "{at}: the new text ends with `{after}`, which already follows the replaced \
             text on its line{fix}"
        ));
    }
    if counts(before) && squash(value).starts_with(&squash(before)) {
        return Some(format!(
            "{at}: the new text starts with `{before}`, which already precedes the \
             replaced text on its line{fix}"
        ));
    }
    None
}

/// `new`, with a `,` appended if the item at `range` ends with one and `new`
/// doesn't (§3.3).
fn with_trailing_comma<'t>(f: &SourceFile, range: &Range<usize>, new: &'t Text) -> Cow<'t, Text> {
    let trailing_comma = f
        .items()
        .unwrap_or_default()
        .iter()
        .any(|i| i.range == *range && i.trailing_comma);
    if !trailing_comma || new.value.trim_end().ends_with(',') {
        return Cow::Borrowed(new);
    }
    let mut value = new.value.clone();
    value.insert(value.trim_end().len(), ',');
    Cow::Owned(Text {
        value,
        kind: new.kind,
    })
}

/// `new`, with a blank line separating it from the syntax item at `range`
/// when inserting before or after an item that has one (§4.2). Inserting an
/// item before the line or match that starts an item's docs is inserting
/// before the item.
fn separated<'t>(
    f: &SourceFile,
    target: &Target,
    position: Position,
    range: &Range<usize>,
    new: &'t Text,
) -> Cow<'t, Text> {
    let t = &f.text;
    if !text::is_whole_line(t, range) {
        return Cow::Borrowed(new);
    }
    let item = match target.selector.steps.last() {
        Some(Step {
            primary: Primary::Syntax { kind, .. },
            parts,
            ..
        }) => (parts.is_empty() && !syntax::find_kind(kind).is_some_and(|k| k.stacked))
            .then(|| text::full_lines(t, range.clone())),
        Some(Step { parts, .. })
            if parts.is_empty()
                && matches!(position, Position::Before)
                && ends_with_item(f, &new.value) =>
        {
            item_lines_at(f, range.start)
        }
        _ => None,
    };
    if !item.is_some_and(|full| text::blank_separated(t, full)) {
        return Cow::Borrowed(new);
    }
    let blank = |line: Option<&str>| line.is_some_and(|l| l.trim().is_empty());
    // The target is whole-line, so a string is lines as a heredoc's are, and
    // its final newline only ends its last line (§5.1).
    let (lines, kind) = match new.kind {
        TextKind::Str => (lines_of(new), TextKind::Heredoc),
        kind => (new.value.as_str(), kind),
    };
    let mut value = lines.to_string();
    match position {
        Position::After if !blank(lines.split('\n').next()) => value.insert(0, '\n'),
        // Doc comments and attributes attach to the item.
        Position::Before if only_leading(f, lines) => {
            return Cow::Borrowed(new);
        }
        Position::Before if !blank(lines.split('\n').next_back()) => value.push('\n'),
        _ => return Cow::Borrowed(new),
    }
    Cow::Owned(Text { value, kind })
}

/// Whether `text` ends with a syntax item that isn't stacked, in `f`'s
/// language.
fn ends_with_item(f: &SourceFile, text: &str) -> bool {
    let Some(lang) = f.lang else {
        return false;
    };
    let end = text.trim_end().len();
    syntax::items(lang.selectors(), &lang.parse(text), text)
        .iter()
        .any(|i| i.range.end >= end && !syntax::find_kind(i.kind).is_some_and(|k| k.stacked))
}

/// The full lines of the widest unstacked item in `f` that has leading doc
/// comments or attributes and whose first line starts at `start`.
fn item_lines_at(f: &SourceFile, start: usize) -> Option<Range<usize>> {
    let t = &f.text;
    f.items()?
        .iter()
        .filter(|i| {
            i.range.start < i.node.start && !syntax::find_kind(i.kind).is_some_and(|k| k.stacked)
        })
        .map(|i| text::full_lines(t, i.range.clone()))
        .filter(|full| full.start == start)
        .max_by_key(|full| full.end)
}

/// The text `move` carries from `range`: its full lines, to be re-based, if
/// it's whole-line, else the span verbatim. The flag says whether a blank line
/// was directly above or below those full lines.
fn moved_text(f: &SourceFile, range: &Range<usize>) -> (Text, bool) {
    let t = &f.text;
    if !text::is_whole_line(t, range) {
        let value = t[range.clone()].replace("\r\n", "\n");
        return (
            Text {
                value,
                kind: TextKind::Str,
            },
            false,
        );
    }
    let full = text::full_lines(t, range.clone());
    let mut value = t[full.clone()].replace("\r\n", "\n");
    if value.ends_with('\n') {
        value.pop();
    }

    let text = Text {
        value,
        kind: TextKind::Heredoc,
    };
    (text, text::blank_separated(t, full))
}

/// `range`, widened to its whole lines if it's partial and heredoc `new` is
/// inserted before or after it, unless `target` ends in an item part (§5.1).
fn heredoc_lines(
    f: &SourceFile,
    target: &Target,
    position: Position,
    new: &Text,
    range: Range<usize>,
) -> Range<usize> {
    let item_part = target.selector.steps.last().is_some_and(|s| {
        s.parts
            .iter()
            .any(|p| !matches!(p, Part::Lines | Part::Refs | Part::Def))
    });
    let beside = matches!(position, Position::Before | Position::After);
    if beside && new.kind != TextKind::Str && !item_part && !text::is_whole_line(&f.text, &range) {
        text::full_lines(&f.text, range)
    } else {
        range
    }
}

/// The span and text of an insertion at `position` of `range` (§4.2, §5):
/// an empty span, unless it opens an empty body. Text after a match of
/// `selector` takes the indentation of its last line (§5.2).
fn insert(
    f: &SourceFile,
    range: Range<usize>,
    position: Position,
    new: &Text,
    selector: &Selector,
) -> (Range<usize>, String) {
    if let Some(item) = empty_body(f, &range) {
        return fill_body(f, item, range, new);
    }
    let t = &f.text;
    let unit = f.indent_unit();
    if !text::is_whole_line(t, &range) {
        let at = match position {
            Position::Before | Position::Start => range.start,
            Position::After | Position::End => range.end,
        };
        return (at..at, verbatim(new, text::indent_at(t, range.start), unit));
    }
    let full = text::full_lines(t, range.clone());
    // List-item text next to a list item's line goes beside the whole item.
    let anchor = match position {
        Position::Before => list_anchor(f, full.start, new),
        Position::After => list_anchor(f, full.end.saturating_sub(1).max(full.start), new),
        Position::Start | Position::End => None,
    };
    let full = anchor.map_or(full, |item| text::full_lines(t, item.range.clone()));
    let first = text::indent_at(t, full.start);
    let inner = match text::first_indent(t, full.clone()) {
        Some(indent) => indent.to_string(),
        // A blank delimited body is filled like an empty one.
        None => body_of(f, &range)
            .filter(|i| !i.undelimited)
            .map_or(first.to_string(), |i| {
                format!("{}{unit}", text::indent_at(t, i.node.start))
            }),
    };
    let match_end = selector.steps.last().is_some_and(|s| {
        s.parts.is_empty() && matches!(s.primary, Primary::Regex(_) | Primary::Literal(_))
    });
    let (at, indent) = match position {
        Position::Before => (full.start, first),
        Position::After => {
            let indent = anchor.map_or_else(
                || {
                    let line = after_line(t, full.clone(), match_end);
                    construct_indent(f, line).unwrap_or(text::indent_at(t, line))
                },
                |_| first,
            );
            (full.end, indent)
        }
        Position::Start => (full.start, inner.as_str()),
        Position::End => (full.end, inner.as_str()),
    };
    let mut new = line_oriented(new, indent, unit, text::rebase);
    if at == full.end && !full.is_empty() && !t[..at].ends_with('\n') {
        // The span's last line has no line ending; give it one instead.
        new.pop();
        new.insert(0, '\n');
    }
    (at..at, new)
}

/// The span removed by deleting `range`: whole lines, tidied, or the span
/// itself.
fn removal(f: &SourceFile, range: Range<usize>) -> Range<usize> {
    if text::is_whole_line(&f.text, &range) {
        text::tidy_delete(&f.text, text::full_lines(&f.text, range))
    } else {
        range
    }
}

/// `new` as line-oriented text: re-based with `rebase` (unless raw), with a
/// final newline.
fn line_oriented(
    new: &Text,
    indent: &str,
    unit: &str,
    rebase: fn(&str, &str, &str) -> String,
) -> String {
    let mut out = match new.kind {
        TextKind::RawHeredoc => new.value.clone(),
        TextKind::Str | TextKind::Heredoc => rebase(lines_of(new), indent, unit),
    };
    out.push('\n');
    out
}

/// The lines of line-oriented `new`: a string's final newline only ends its
/// last line (§5.1).
fn lines_of(new: &Text) -> &str {
    match new.kind {
        TextKind::Str => new.value.strip_suffix('\n').unwrap_or(&new.value),
        _ => &new.value,
    }
}

/// The start of the line `insert after` takes its indentation from (§5.2):
/// the first line of the whole lines `full`, or its last non-blank line after
/// a match.
fn after_line(t: &str, full: Range<usize>, match_end: bool) -> usize {
    if !match_end {
        return full.start;
    }
    let end = full.start + t[full.clone()].trim_end().len();
    text::line_start(t, end).max(full.start)
}

/// The indentation of the construct that the line starting at `line` ends,
/// when it begins on an earlier line (§5.2).
fn construct_indent(f: &SourceFile, line: usize) -> Option<&str> {
    let t = &f.text;
    let content = &t[line..t[line..].find('\n').map_or(t.len(), |i| line + i)];
    let first = line + content.len() - content.trim_start().len();
    let end = line + content.trim_end().len();
    if first >= end {
        return None;
    }
    let mut node = f
        .tree()?
        .root_node()
        .descendant_for_byte_range(first, first)?;
    while let Some(parent) = node.parent() {
        if parent.end_byte() > end || lays_out_lines(t, parent) {
            break;
        }
        node = parent;
    }
    let start = node.start_byte();
    (start < line && starts_line(t, start)).then(|| text::indent_at(t, start))
}

/// Whether `node` puts a named child after its first at the start of a line,
/// as a block does its statements.
fn lays_out_lines(t: &str, node: Node) -> bool {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .skip(1)
        .any(|c| starts_line(t, c.start_byte()))
}

/// Whether only indentation comes before `offset` on its line.
fn starts_line(t: &str, offset: usize) -> bool {
    t[text::line_start(t, offset)..offset].trim().is_empty()
}

/// The Markdown list item that list-item `new` placed on the line holding
/// `offset` anchors to (§5.2): the innermost item on that line, if it's a list
/// item that isn't in a block quote.
fn list_anchor<'f>(f: &'f SourceFile, offset: usize, new: &Text) -> Option<&'f Item> {
    let first = new.value.lines().map(str::trim).find(|l| !l.is_empty())?;
    let digits = first.len() - first.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let marker = match digits {
        0 => first.strip_prefix(['-', '*', '+']),
        1..=9 => first[digits..].strip_prefix(['.', ')']),
        _ => None,
    };
    if !marker.is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace)) {
        return None;
    }
    let t = &f.text;
    let item = f
        .items()?
        .iter()
        .filter(|i| text::full_lines(t, i.range.clone()).contains(&offset))
        .min_by_key(|i| i.range.len())?;
    let line = t[..item.range.start].rfind('\n').map_or(0, |i| i + 1);
    (item.kind == "item" && t[line..item.range.start].trim().is_empty()).then_some(item)
}

/// `new` inserted into a partial line: later lines re-based (unless raw).
fn verbatim(new: &Text, indent: &str, unit: &str) -> String {
    match new.kind {
        TextKind::RawHeredoc => new.value.clone(),
        TextKind::Str | TextKind::Heredoc => text::rebase_tail(&new.value, indent, unit),
    }
}

/// An error that rejects a script, at `span` of the script when it has one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{kind}")]
pub struct ExecError {
    pub kind: ExecErrorKind,
    pub span: Option<Range<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExecErrorKind {
    /// `hint` is empty, or `; did you mean SEL (LINES)?`. `searched` is the
    /// number of files searched when only a search's last step (a regex,
    /// literal, heredoc or `.refs`) matched nothing.
    #[error("{selector} matches nothing in {files}{hint}")]
    NoMatch {
        selector: String,
        files: String,
        hint: String,
        searched: Option<usize>,
    },
    #[error("{selector} needs a language, but {files} has none; use --lang")]
    NoLanguage { selector: String, files: String },
    /// `has` lists the parts the item has, e.g. `.sig .name .lines`.
    #[error("{item} has no .{part}; it has {has}")]
    MissingPart {
        item: String,
        part: String,
        has: String,
    },
    #[error(".{part} needs a syntax item, e.g. fn:NAME.{part}")]
    PartNeedsItem { part: String },
    #[error("invalid {lang} query: {message}")]
    InvalidQuery { lang: String, message: String },
    #[error("{selector} {message}")]
    InvalidPattern { selector: String, message: String },
    #[error("`@{name}` is captured by two pattern steps; rename one of them")]
    DuplicateCapture { name: String },
    #[error("`@{name}` is nothing the target captured; {fix}")]
    UnknownCapture { name: String, fix: String },
    #[error("`@_` captures nothing, so TEXT can't use it; write `@@_` for a literal `@`")]
    WildcardInText,
    #[error(
        "{selector} needs a programming language, but {files} has none; use a regex or literal, or --lang"
    )]
    NoCodeLanguage { selector: String, files: String },
    #[error(
        "{selector} needs a language, but parsing was disabled with --lang text; drop it, or use a regex or literal"
    )]
    ParsingDisabled { selector: String },
    #[error("{lang} has no `{kind}` items; use one of: {kinds}")]
    UnknownKind {
        kind: String,
        lang: String,
        kinds: String,
    },
    #[error(
        "{selector} matches {} items; {}",
        .candidates.total,
        .candidates
    )]
    Ambiguous {
        selector: String,
        candidates: Candidates,
    },
    #[error("line {line} is past the end of {files}{hint}", hint = last_line_hint(line))]
    LineOutOfRange { line: String, files: String },
    /// `add` is the fix: the `file` command that adds it, when short.
    #[error("file:{path} is not in the file set: {files}; {add}")]
    NotInFileSet {
        path: String,
        files: String,
        add: String,
    },
    #[error(
        "edit overlaps command {command} at {location}; merge the two edits, or put a `|` between them"
    )]
    Overlap { command: usize, location: String },
    #[error("no files to edit; pass FILE arguments or use `file PATH`")]
    NoFiles,
    #[error("cannot read {path}: {message}")]
    Io { path: String, message: String },
    #[error(
        "move destination is inside the moved span at {location}; choose a destination outside it"
    )]
    MoveIntoSource { location: String },
    #[error("{path} already exists; edit it with `file {path}`")]
    FileExists { path: String },
    /// `note` is empty, or where relative paths start.
    #[error("glob `{glob}` matched nothing{note}")]
    NoGlobMatch { glob: String, note: String },
    /// `location` is `PATH:LINE:COL`; `hint` is empty or a `; ` and a fix;
    /// `excerpt` is empty, or a newline and the offending line with a caret.
    #[error(
        "{location}: edit introduces a syntax error (use --force to apply anyway){hint}{excerpt}"
    )]
    SyntaxError {
        location: String,
        hint: String,
        excerpt: String,
    },
    /// The message ends with its fix.
    #[error("{0}")]
    Lsp(String),
    #[error("no language server for {langs}; set one with `[lsp] {first} = [\"PROGRAM\", ...]` in .ned.toml", first = langs.split(", ").next().unwrap_or_default())]
    NoServer { langs: String },
    /// `message` ends with a fix.
    #[error("cannot rename at {location}: {message}")]
    RenameRefused { location: String, message: String },
    /// `what` is `rename` or a selector; `workspace`: under `-w`, where the
    /// boundary is the workspace.
    #[error(
        "{what} reaches files outside the {}: {files}; {}",
        if *workspace { "workspace" } else { "file set" },
        if *workspace { "these are ignored or outside the root; use a regex there instead" } else { "add them to the file set, or use -w" }
    )]
    Outside {
        what: String,
        files: String,
        workspace: bool,
    },
    /// `locations` lists where the matches are.
    #[error("{selector} matches {total} spans, at {locations}; add `all` to take every one")]
    AmbiguousLocated {
        selector: String,
        total: usize,
        locations: String,
    },
}

/// Selectors that each pick one of an ambiguous selector's matches, with the
/// location of that match; `total` counts all matches, listed or not, and
/// `shared` those that no selector picks alone, since they share a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidates {
    pub listed: Vec<(String, String)>,
    pub total: usize,
    pub shared: usize,
}

impl fmt::Display for Candidates {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.listed.is_empty() {
            return f.write_str(
                "add `all`, or select longer text; matches on the same line can't be picked by scope",
            );
        }
        f.write_str("add `all` or use one of:")?;
        let width = self.listed.iter().map(|(s, _)| s.len()).max();
        for (selector, loc) in &self.listed {
            write!(f, "\n  {selector:<w$}   {loc}", w = width.unwrap_or(0))?;
        }
        let more = self.total - self.shared - self.listed.len();
        if more > 0 {
            write!(f, "\n  … and {more} more")?;
        }
        if self.shared > 0 {
            let shared = self.shared;
            write!(
                f,
                "\n  {shared} more share a line with another match; select longer text to pick one"
            )?;
        }
        Ok(())
    }
}

impl ExecError {
    pub fn new(kind: ExecErrorKind, span: Option<Range<usize>>) -> Self {
        ExecError { kind, span }
    }

    /// Renders the error as `error: script:LINE:COL: message`, or
    /// `error: message` when it has no script position.
    pub fn render(&self, src: &str) -> String {
        match &self.span {
            Some(span) => {
                let (line, column) = location(src, span.start);
                format!("error: script:{line}:{column}: {}", self.kind)
            }
            None => format!("error: {}", self.kind),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;

    use super::*;
    use crate::lsp::{
        Diagnosis, Diagnostic, Document, FileEdits, Formatting, Locate, Located, Location,
        LspFailure, Position, Renamed, TextEdit,
    };
    use crate::script::parse;

    const TEXT: &str =
        "fn a() {\n    let x = 1;\n    let y = 2;\n}\n\nfn b() {\n    let x = 3;\n}\n";

    /// The outcome of running a script, with the temporary directory removed
    /// from every path.
    struct Outcome {
        output: String,
        result: Result<Vec<Change>, String>,
        notes: Vec<String>,
    }

    impl Outcome {
        /// The new contents of the only modified file.
        fn new_text(&self) -> &str {
            let changes = self.result.as_ref().unwrap();
            assert_eq!(changes.len(), 1, "{changes:?}");
            &changes[0].new
        }

        fn error(&self) -> &str {
            self.result.as_ref().unwrap_err()
        }
    }

    /// Runs `script` with the parse-error guard off: most tests edit
    /// Rust-like text without keeping it valid.
    fn exec_with(files: &[(&str, &str)], initial: usize, script: &str) -> Outcome {
        let force = Options {
            force: true,
            ..Options::default()
        };
        exec_with_options(files, initial, script, &force)
    }

    fn guarded(path: &str, text: &str, script: &str) -> Outcome {
        exec_with_options(&[(path, text)], 1, script, &Options::default())
    }

    /// Writes `files` to a temporary directory and runs `script` with the
    /// first `initial` of them as the file set. `{dir}` in the script is
    /// replaced by the directory.
    fn exec_with_options(
        files: &[(&str, &str)],
        initial: usize,
        script: &str,
        options: &Options,
    ) -> Outcome {
        exec_with_lsp(files, initial, script, options, None)
    }

    /// `exec_with_options`, with `lsp` for the language servers.
    fn exec_with_lsp(
        files: &[(&str, &str)],
        initial: usize,
        script: &str,
        options: &Options,
        lsp: Option<&mut dyn Lsp>,
    ) -> Outcome {
        let dir = tempfile::tempdir().unwrap();
        let root = format!("{}/", dir.path().display());
        for (name, text) in files {
            let path = dir.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let paths: Vec<String> = files[..initial]
            .iter()
            .map(|(name, _)| format!("{root}{name}"))
            .collect();
        let src = script.replace("{dir}/", &root);
        let parsed = parse(&src).unwrap();
        let run = run(&parsed, &src, Initial::Files(&paths), options, lsp);
        let strip = |s: &str| s.replace(&root, "");
        Outcome {
            output: strip(&run.output),
            result: match run.result {
                Ok(changes) => Ok(changes
                    .into_iter()
                    .map(|c| Change {
                        path: strip(&c.path),
                        ..c
                    })
                    .collect()),
                Err(err) => Err(strip(&err.render(&src))),
            },
            notes: run.notes.iter().map(|n| strip(n)).collect(),
        }
    }

    fn exec(text: &str, script: &str) -> Outcome {
        exec_with(&[("a.rs", text)], 1, script)
    }

    fn edited(text: &str, script: &str) -> String {
        edited_in("a.rs", text, script)
    }

    fn edited_in(path: &str, text: &str, script: &str) -> String {
        exec_with(&[(path, text)], 1, script).new_text().to_string()
    }

    #[test]
    fn show_prints_numbered_lines() {
        let out = exec(TEXT, "show 2-3");
        assert_eq!(out.output, "a.rs:2-3\n2:    let x = 1;\n3:    let y = 2;\n");
        assert_eq!(out.result, Ok(vec![]));
        assert_eq!(exec("a\r\nb\r\n", "show $").output, "a.rs:2\n2:b\n");
    }

    #[test]
    fn show_all_searching_in_vain_says_so_and_goes_on() {
        let out = exec_with(&[("a.rs", TEXT), ("b.rs", TEXT)], 2, "show all /zzz/");
        assert_eq!(out.output, "no matches for /zzz/ in 2 files\n");
        assert_eq!(out.result, Ok(vec![]));
        assert_eq!(
            exec(TEXT, "show all fn:a>\"zzz\"").output,
            "no matches for fn:a>\"zzz\" in 1 file\n"
        );
        let out = exec(TEXT, "show all /zzz/; replace 2 with \"x\"");
        assert_eq!(out.output, "no matches for /zzz/ in 1 file\n");
        assert_eq!(out.result.map(|changes| changes.len()), Ok(1));
    }

    #[test]
    fn show_all_fails_when_an_earlier_step_matches_nothing() {
        assert_eq!(
            exec(TEXT, "show all fn:zzz>\"x\"").error(),
            "error: script:1:10: fn:zzz matches nothing in a.rs; `outline` lists the items"
        );
    }

    #[test]
    fn show_all_still_fails_without_a_search() {
        for script in ["show /zzz/", "show all fn:nope", "show all fn:nope>/x/"] {
            assert!(exec(TEXT, script).result.is_err(), "{script}");
        }
    }

    #[test]
    fn a_stage_sees_the_edits_of_the_stages_before_it() {
        assert_eq!(
            edited(
                TEXT,
                "insert after fn:a <<END | replace fn:c with \"fn c() { x(); }\"\n\nfn c() {}\nEND\n"
            ),
            TEXT.replace("}\n\nfn b", "}\n\nfn c() { x(); }\n\nfn b")
        );
        assert_eq!(
            edited(
                TEXT,
                "move fn:b before fn:a | replace fn:b>\"3\" with \"4\""
            ),
            "fn b() {\n    let x = 4;\n}\n\nfn a() {\n    let x = 1;\n    let y = 2;\n}\n"
        );
        assert_eq!(
            edited(
                "a\r\nb\r\n",
                "replace 1 with \"x\" | replace 1 with \"y\"; replace 2 with \"z\""
            ),
            "y\r\nz\r\n"
        );
    }

    #[test]
    fn a_selector_matching_only_the_stages_edits_suggests_a_pipe() {
        let hint = "; it matches only after the edits before it, which a stage's selectors \
            don't see: put a `|` before the command";
        assert_eq!(
            guarded(
                "a.rs",
                TEXT,
                "insert after fn:a <<END\n\nfn p() { a(); }\nEND\nsub fn:p /a/ with \"b\""
            )
            .error(),
            format!("error: script:5:5: fn:p matches nothing in a.rs{hint}")
        );
        assert_eq!(
            exec(
                TEXT,
                "insert after fn:b \"zzz\"; replace \"zzz\" with \"y\""
            )
            .error(),
            format!("error: script:1:34: \"zzz\" matches nothing in a.rs{hint}")
        );
    }

    #[test]
    fn a_selector_the_stages_edits_dont_match_keeps_its_hint() {
        assert_eq!(
            exec(TEXT, "insert after fn:b \"fn p() {}\"; delete fn:zzzzzz").error(),
            "error: script:1:39: fn:zzzzzz matches nothing in a.rs; `outline` lists the items"
        );
        assert_eq!(
            exec(TEXT, "insert after fn:b \"fn p() {}\" | delete fn:zzzzzz").error(),
            "error: script:1:40: fn:zzzzzz matches nothing in a.rs; `outline` lists the items"
        );
    }

    #[test]
    fn trying_the_stages_edits_prints_nothing() {
        let out = exec(
            TEXT,
            "show fn:b; insert after fn:b \"fn p() {}\"; show fn:p",
        );
        assert!(
            out.error().ends_with("put a `|` before the command"),
            "{}",
            out.error()
        );
        assert_eq!(out.output, exec(TEXT, "show fn:b").output);
        assert!(out.notes.is_empty(), "{:?}", out.notes);
    }

    #[test]
    fn reads_show_their_stages_text() {
        let out = exec(TEXT, "show 2 | insert before 1 \"// c\" | show 3");
        assert_eq!(
            out.output,
            "a.rs:2\n2:    let x = 1;\na.rs:3\n3:    let x = 1;\n"
        );
    }

    #[test]
    fn stages_sum_their_edits_against_the_original() {
        let out = exec(
            TEXT,
            "replace 2 with \"let x = 5;\" | replace 3 with \"let y = 6;\"",
        );
        let changes = out.result.unwrap();
        assert_eq!(changes[0].old, TEXT);
        assert_eq!(changes[0].edits, 2);
        let two = exec_with(
            &[("a.rs", "a\n"), ("b.rs", "b\n")],
            1,
            "replace 1 with \"x\" | file {dir}/b.rs; replace 1 with \"y\"",
        );
        let news: Vec<&str> = two
            .result
            .as_ref()
            .unwrap()
            .iter()
            .map(|c| c.new.as_str())
            .collect();
        assert_eq!(news, ["x\n", "y\n"]);
    }

    #[test]
    fn every_stage_passes_the_parse_error_guard() {
        let out = exec_with_options(
            &[("a.rs", "fn a() {}\n")],
            1,
            "replace \"() {}\" with \"() {\" | replace \"fn a() {\" with \"fn a() {}\"",
            &Options::default(),
        );
        assert!(out.error().contains("syntax error"), "{}", out.error());
    }

    #[test]
    fn show_adds_context_lines() {
        assert_eq!(
            exec(TEXT, "show 3 +1").output,
            "a.rs:2-4\n2:    let x = 1;\n3:    let y = 2;\n4:}\n"
        );
        assert_eq!(
            exec(TEXT, "show 1 +2").output,
            "a.rs:1-3\n1:fn a() {\n2:    let x = 1;\n3:    let y = 2;\n"
        );
        assert_eq!(
            exec(TEXT, "show $ +1").output,
            "a.rs:7-8\n7:    let x = 3;\n8:}\n"
        );
    }

    #[test]
    fn show_context_regions_merge() {
        let two = exec(TEXT, "show all /let x/ +1").output;
        assert!(two.starts_with("a.rs:1-3\n"), "{two}");
        assert!(two.contains("\na.rs:6-8\n"), "{two}");
        let one = exec(TEXT, "show all /let x/ +2").output;
        assert!(one.starts_with("a.rs:1-8\n"), "{one}");
        assert_eq!(one.lines().count(), 9);
    }

    #[test]
    fn show_without_selector_prints_every_file() {
        let out = exec_with(&[("a.rs", "x\ny\n"), ("b.rs", "z")], 2, "show");
        assert_eq!(out.output, "a.rs:1-2\n1:x\n2:y\nb.rs:1\n1:z\n");
    }

    #[test]
    fn show_merges_regions_within_one_line() {
        assert_eq!(
            exec(TEXT, "show all /let/").output,
            "a.rs:2-3\n2:    let x = 1;\n3:    let y = 2;\na.rs:7\n7:    let x = 3;\n"
        );
        assert_eq!(
            exec("a\nb\na\nc\nd\na\n", "show all /a/").output,
            "a.rs:1-3\n1:a\n2:b\n3:a\na.rs:6\n6:a\n"
        );
    }

    #[test]
    fn show_prints_the_whole_lines_of_partial_spans() {
        assert_eq!(
            exec(TEXT, "show \"2;\\n}\"").output,
            "a.rs:3-4\n3:    let y = 2;\n4:}\n"
        );
    }

    #[test]
    fn replace_partial_span_verbatim() {
        let out = exec(TEXT, "replace \"= 1\" with \"= 10\"");
        let changes = out.result.unwrap();
        assert_eq!(changes[0].path, "a.rs");
        assert_eq!(changes[0].old, TEXT);
        assert_eq!(changes[0].new, TEXT.replace("= 1;", "= 10;"));
        assert_eq!(changes[0].edits, 1);
    }

    #[test]
    fn replace_whole_lines_rebases_text() {
        assert_eq!(
            edited(TEXT, "replace 2 with \"let z = 0;\""),
            TEXT.replace("let x = 1;", "let z = 0;")
        );
        assert_eq!(
            edited(TEXT, "replace 2-3 with <<END\nif a {\n    b();\n}\nEND\n"),
            "fn a() {\n    if a {\n        b();\n    }\n}\n\nfn b() {\n    let x = 3;\n}\n"
        );
    }

    #[test]
    fn replace_text_dedenting_its_first_line_steps_out_of_the_target() {
        assert_eq!(
            edited(
                TEXT,
                "replace 3 with <<END\n    z;\n}\n\nfn c() {\n    w;\nEND\n"
            ),
            TEXT.replace("    let y = 2;\n", "    z;\n}\n\nfn c() {\n    w;\n")
        );
        let text = "class A:\n    def f(self):\n        return 1\n";
        assert_eq!(
            edited_in(
                "a.py",
                text,
                "replace 3 with <<END\n        return 2\n\n    def g(self):\n        pass\nEND\n"
            ),
            text.replace("return 1\n", "return 2\n\n    def g(self):\n        pass\n")
        );
    }

    #[test]
    fn inserted_text_dedenting_its_first_line_keeps_the_target_indentation() {
        let text = "def f():\n    if x:\n        a()\n    else:\n        d()\n";
        assert_eq!(
            edited_in(
                "a.py",
                text,
                "insert after 2 <<END\n    b()\nelif y:\n    c()\nEND\n"
            ),
            text.replace(
                "    if x:\n",
                "    if x:\n        b()\n    elif y:\n        c()\n"
            )
        );
    }

    #[test]
    fn replace_with_raw_heredoc_is_verbatim() {
        assert_eq!(
            edited(TEXT, "replace 2-3 with <<'END'\nz();\nEND\n"),
            "fn a() {\nz();\n}\n\nfn b() {\n    let x = 3;\n}\n"
        );
    }

    #[test]
    fn replace_last_line_keeps_missing_final_newline() {
        assert_eq!(edited("a\nb", "replace $ with \"c\""), "a\nc");
    }

    #[test]
    fn replace_whole_lines_with_empty_text_deletes_them() {
        assert_eq!(edited("a\nb\nc\n", "replace 2 with \"\""), "a\nc\n");
        assert_eq!(edited("a\nb\nc\n", "replace \"b\\n\" with \"\""), "a\nc\n");
        assert_eq!(edited("a\nb\nc\n", "replace 2 with <<END\nEND\n"), "a\nc\n");
        assert_eq!(edited("a\n\nb\n\nc\n", "replace 3 with \"\""), "a\n\nc\n");
        assert_eq!(
            edited(TEXT, "replace fn:b with \"\""),
            edited(TEXT, "delete fn:b")
        );
    }

    #[test]
    fn replace_with_a_newline_leaves_an_empty_line() {
        assert_eq!(edited("a\nb\nc\n", "replace 2 with \"\\n\""), "a\n\nc\n");
    }

    #[test]
    fn replace_partial_span_with_empty_text_removes_only_the_span() {
        assert_eq!(
            edited(TEXT, "replace \" = 2\" with \"\""),
            TEXT.replace(" = 2", "")
        );
    }

    #[test]
    fn replace_multi_line_text_in_partial_span() {
        assert_eq!(
            edited(TEXT, "replace \"2\" with \"vec![\\n1,\\n]\""),
            TEXT.replace("2;", "vec![\n    1,\n    ];")
        );
    }

    #[test]
    fn replace_never_expands_dollars() {
        assert_eq!(
            edited(TEXT, "replace \"= 1\" with \"= $0\""),
            TEXT.replace("= 1", "= $0")
        );
    }

    #[test]
    fn replace_substitutes_pattern_captures() {
        let text = "fn main() {\n    assert_eq!(x, true);\n    assert_eq!(f(y), true);\n}\n";
        assert_eq!(
            edited(
                text,
                "replace all `assert_eq!(@a..., true)` with \"assert!(@a)\""
            ),
            "fn main() {\n    assert!(x);\n    assert!(f(y));\n}\n"
        );
    }

    #[test]
    fn replace_substitutes_captures_from_both_ends_of_a_range() {
        let text = "fn f() {\n    begin(1);\n    work();\n    end(2);\n}\n";
        assert_eq!(
            edited(text, "replace `begin(@a)`..`end(@b)` with \"run(@a, @b);\""),
            "fn f() {\n    run(1, 2);\n}\n"
        );
    }

    #[test]
    fn replace_reindents_multi_line_captures() {
        let text = "fn load() {\n    if let Some(x) = get() {\n        use_it(x);\n        if x > 1 {\n            more();\n        }\n    }\n}\n";
        let script = "replace fn:load>`if let Some(@x) = @e { @body... }` with <<END\nlet Some(@x) = @e else {\n    return;\n};\n@body\nEND\n";
        assert_eq!(
            edited(text, script),
            "fn load() {\n    let Some(x) = get() else {\n        return;\n    };\n    use_it(x);\n    if x > 1 {\n        more();\n    }\n}\n"
        );
    }

    #[test]
    fn replace_substitutes_in_crlf_files() {
        let text = "fn main() {\r\n    if x {\r\n        a();\r\n        b();\r\n    }\r\n}\r\n";
        let script = "replace `if @c { @body... }` with <<END\nwhile @c {\n    @body\n}\nEND\n";
        assert_eq!(
            edited(text, script),
            "fn main() {\r\n    while x {\r\n        a();\r\n        b();\r\n    }\r\n}\r\n"
        );
    }

    #[test]
    fn replace_converts_captures_to_the_files_indent() {
        let text = "package main\n\nfunc f() {\n\tif x {\n\t\ta()\n\t\tb()\n\t}\n}\n";
        let script = "replace `if @c { @body... }` with <<END\nfor @c {\n    @body\n}\nEND\n";
        assert_eq!(
            edited_in("a.go", text, script),
            "package main\n\nfunc f() {\n\tfor x {\n\t\ta()\n\t\tb()\n\t}\n}\n"
        );
    }

    #[test]
    fn replace_without_patterns_keeps_ats() {
        assert_eq!(
            edited(TEXT, "replace \"= 1\" with \"= @a\""),
            TEXT.replace("= 1", "= @a")
        );
    }

    #[test]
    fn an_uncaptured_name_in_replace_is_an_error() {
        let text = "fn main() {\n    foo(1);\n}\n";
        let out = exec_with(&[("a.rs", text)], 1, "replace `foo(@a)` with \"bar(@b)\"");
        assert_eq!(
            out.error(),
            "error: script:1:29: `@b` is nothing the target captured; use @a, or write `@@b` for a literal `@`"
        );
        let none = exec_with(&[("a.rs", text)], 1, "replace `foo(@_)` with \"@app\"");
        assert_eq!(
            none.error(),
            "error: script:1:25: `@app` is nothing the target captured; write `@@app` for a literal `@`"
        );
        let wildcard = exec_with(&[("a.rs", text)], 1, "replace `foo(@a)` with \"x(@_)\"");
        assert_eq!(
            wildcard.error(),
            "error: script:1:27: `@_` captures nothing, so TEXT can't use it; write `@@_` for a literal `@`"
        );
    }

    #[test]
    fn insert_around_whole_lines() {
        assert_eq!(
            edited(TEXT, "insert after 2 \"let w = 0;\""),
            TEXT.replace("1;\n", "1;\n    let w = 0;\n")
        );
        assert_eq!(
            edited(TEXT, "insert before 1 \"// top\""),
            format!("// top\n{TEXT}")
        );
        assert_eq!(
            edited(TEXT, "insert start 2-3 \"let w = 0;\""),
            TEXT.replace("    let x = 1;", "    let w = 0;\n    let x = 1;")
        );
        assert_eq!(
            edited(TEXT, "insert end 2-3 \"let w = 0;\""),
            TEXT.replace("2;\n", "2;\n    let w = 0;\n")
        );
    }

    #[test]
    fn insert_after_last_line_without_final_newline() {
        assert_eq!(edited("a\nb", "insert after $ \"c\""), "a\nb\nc");
    }

    #[test]
    fn insert_at_partial_span_is_verbatim() {
        assert_eq!(
            edited(TEXT, "insert after \"let y\" \": u8\""),
            TEXT.replace("let y", "let y: u8")
        );
        assert_eq!(
            edited(TEXT, "insert start \"y = 2\" \"mut \""),
            TEXT.replace("let y", "let mut y")
        );
    }

    #[test]
    fn heredoc_insert_beside_a_partial_span_takes_whole_lines() {
        assert_eq!(
            edited(TEXT, "insert after /let x = 1/ <<END\nlet z = 0;\nEND\n"),
            TEXT.replace("    let x = 1;\n", "    let x = 1;\n    let z = 0;\n")
        );
        assert_eq!(
            edited(TEXT, "insert before \"y = 2\" <<END\nw();\nEND\n"),
            TEXT.replace("    let y", "    w();\n    let y")
        );
        assert_eq!(
            edited(TEXT, "insert before /let y/ <<'END'\nraw\nEND\n"),
            TEXT.replace("    let y", "raw\n    let y")
        );
        // An item part stays where it is.
        assert_eq!(
            edited("fn f() { x }\n", "insert after fn:f.body <<END\ny\nEND\n"),
            "fn f() { xy }\n"
        );
    }

    #[test]
    fn move_beside_a_partial_span_takes_whole_lines() {
        assert_eq!(
            edited(TEXT, "move 7 after /let x = 1/"),
            "fn a() {\n    let x = 1;\n    let x = 3;\n    let y = 2;\n}\n\nfn b() {\n}\n"
        );
    }

    #[test]
    fn insert_into_python_block() {
        let app = "def handle(req):\n    if req.ok:\n        log(req)\n        return 200\n    return 500\n";
        assert_eq!(
            edited_in(
                "a.py",
                app,
                "insert after \"log(req)\".lines <<END\nif req.slow:\n    warn(req)\nEND\n"
            ),
            "def handle(req):\n    if req.ok:\n        log(req)\n        if req.slow:\n            warn(req)\n        return 200\n    return 500\n"
        );
    }

    const APP_PY: &str = "class App:\n    \"\"\"An app.\"\"\"\n\n    def start(self):\n        \"\"\"Start.\"\"\"\n        if self.ok:\n            run()\n\n    def stop(self):\n        pass\n\n\n@cache\ndef load(path):\n    return path\n";

    #[test]
    fn python_insert_start_and_end_go_inside_blocks() {
        assert_eq!(
            edited_in("a.py", APP_PY, "insert start fn:start \"self.n = 0\""),
            APP_PY.replace(
                "\"\"\"Start.\"\"\"\n",
                "\"\"\"Start.\"\"\"\n        self.n = 0\n"
            )
        );
        assert_eq!(
            edited_in("a.py", APP_PY, "insert end fn:start \"done()\""),
            APP_PY.replace("run()\n", "run()\n        done()\n")
        );
        assert_eq!(
            edited_in(
                "a.py",
                APP_PY,
                "insert end class:App <<END\n\ndef pause(self):\n    pass\nEND"
            ),
            APP_PY.replace(
                "        pass\n",
                "        pass\n\n    def pause(self):\n        pass\n"
            )
        );
        let doc_only = "class A:\n    \"\"\"Doc.\"\"\"\n";
        assert_eq!(
            edited_in("a.py", doc_only, "insert end class:A \"x = 1\""),
            "class A:\n    \"\"\"Doc.\"\"\"\n    x = 1\n"
        );
    }

    #[test]
    fn insert_after_a_match_takes_its_last_lines_indent() {
        let text = "fn a() {\n    if x {\n        y();\n    }\n}\n";
        assert_eq!(
            edited(
                text,
                "insert after \"if x {\\n        y();\" <<END\nv();\nEND"
            ),
            text.replace("y();\n", "y();\n        v();\n")
        );
        let expected = APP_PY.replace("run()\n", "run()\n            z()\n");
        for script in [
            "insert after /if self.ok:\\n.*run\\(\\)/ \"z()\"",
            "insert after fn:start>\"if self.ok:\\n            run()\" \"z()\"",
        ] {
            assert_eq!(edited_in("a.py", APP_PY, script), expected, "{script}");
        }
    }

    #[test]
    fn insert_after_lines_and_items_takes_the_first_lines_indent() {
        let text = "def f():\n    if x:\n        a()\n    b()\n";
        assert_eq!(
            edited_in("a.py", text, "insert after 2-3 \"c()\""),
            text.replace("a()\n", "a()\n    c()\n")
        );
        let text = "def f():\n    if x:\n        a()\n";
        assert_eq!(
            edited_in("a.py", text, "insert after fn:f.body \"c()\""),
            format!("{text}    c()\n")
        );
        assert_eq!(
            edited_in("a.py", APP_PY, "insert after fn:start \"x = 1\""),
            APP_PY.replace("run()\n", "run()\n\n    x = 1\n")
        );
        assert_eq!(
            edited_in("a.py", APP_PY, "insert after class:App \"x = 1\""),
            APP_PY.replace("pass\n", "pass\n\nx = 1\n")
        );
    }

    #[test]
    fn insert_after_a_continuation_line_takes_its_statements_indent() {
        let text = "fn f() {\n    let v: Vec<_> = xs\n        .iter()\n        .collect();\n}\n";
        let expected = text.replace("collect();\n", "collect();\n    let y = 2;\n");
        for script in [
            "insert after 4 \"let y = 2;\"",
            "insert after \".collect();\" <<END\nlet y = 2;\nEND",
            "insert after 2-4 \"let y = 2;\"",
        ] {
            assert_eq!(edited(text, script), expected, "{script}");
        }
        let text = "def f():\n    total = (a\n             + b)\n    return total\n";
        assert_eq!(
            edited_in("a.py", text, "insert after 3 \"c()\""),
            text.replace("+ b)\n", "+ b)\n    c()\n")
        );
    }

    #[test]
    fn insert_after_a_line_that_ends_no_construct_keeps_its_indent() {
        let text = "fn f() {\n    let v: Vec<_> = xs\n        .iter()\n        .collect();\n}\n";
        assert_eq!(
            edited(text, "insert after 3 \".map(g)\""),
            text.replace("iter()\n", "iter()\n        .map(g)\n")
        );
        let text = "fn f() {\n    let s = S {\n        b: T {\n            x: 1,\n            y: 2,\n        },\n    };\n}\n";
        assert_eq!(
            edited(text, "insert after 6 \"c: 3,\""),
            text.replace("},\n", "},\n        c: 3,\n")
        );
        let text = "def f():\n    if x:\n        a()\n";
        assert_eq!(
            edited_in("a.py", text, "insert after 3 \"b()\""),
            format!("{text}        b()\n")
        );
        let text = "let v = xs\n    .collect();\n";
        assert_eq!(
            edited_in("a.txt", text, "insert after 2 \"y\""),
            format!("{text}    y\n")
        );
    }

    #[test]
    fn python_decorators_stay_with_their_item() {
        assert_eq!(
            edited_in(
                "a.py",
                APP_PY,
                "replace fn:load with <<END\ndef load(path, mode):\n    return path\nEND"
            ),
            APP_PY.replace("def load(path):", "def load(path, mode):")
        );
        assert_eq!(
            edited_in("a.py", APP_PY, "insert before fn:load \"@trace\""),
            APP_PY.replace("@cache\n", "@trace\n@cache\n")
        );
        assert_eq!(
            edited_in("a.py", APP_PY, "replace fn:start.body with \"pass\""),
            APP_PY.replace("        if self.ok:\n            run()\n", "        pass\n")
        );
    }

    #[test]
    fn python_delete_tidies_the_blank_line_after_a_colon() {
        let text = "class A:\n\n    def a(self):\n        pass\n\n    def b(self):\n        pass\n";
        assert_eq!(
            edited_in("a.py", text, "delete fn:a"),
            "class A:\n\n    def b(self):\n        pass\n"
        );
        let text = "class A:\n    def a(self):\n        pass\n\n    def b(self):\n        pass\n";
        assert_eq!(
            edited_in("a.py", text, "delete fn:a"),
            "class A:\n    def b(self):\n        pass\n"
        );
    }

    #[test]
    fn python_outline() {
        let files = [("a.py", APP_PY)];
        assert_eq!(
            exec_with(&files, 1, "outline").output,
            "a.py\n1-10 class:App\n  4-7 fn:start\n  9-10 fn:stop\n13-15 fn:load\n"
        );
        let text = "X = 1\n\nclass A:\n    y = 2\n\n    def f(self):\n        def g():\n            pass\n";
        assert_eq!(
            exec_with(&[("a.py", text)], 1, "outline class:A").output,
            "a.py\n4 field:y\n6-8 fn:f\n"
        );
    }

    const MAIN_GO: &str = "package main\n\nconst (\n\tA = 1\n\tB = 2\n)\n\ntype Server struct {\n\taddr string\n}\n\nfunc (s *Server) Run() error {\n\treturn nil\n}\n";

    #[test]
    fn go_edits() {
        assert_eq!(
            edited_in("a.go", MAIN_GO, "insert end fn:\"Server.Run\" \"log()\""),
            MAIN_GO.replace("\treturn nil\n", "\treturn nil\n\tlog()\n")
        );
        assert_eq!(
            edited_in("a.go", MAIN_GO, "insert end struct:Server \"port int\""),
            MAIN_GO.replace("\taddr string\n", "\taddr string\n\tport int\n")
        );
        assert_eq!(
            edited_in("a.go", MAIN_GO, "delete const:A"),
            MAIN_GO.replace("\tA = 1\n", "")
        );
        assert_eq!(
            edited_in("a.go", MAIN_GO, "insert before fn:Run \"// Run serves.\""),
            MAIN_GO.replace("func (s", "// Run serves.\nfunc (s")
        );
    }

    #[test]
    fn go_outline() {
        assert_eq!(
            exec_with(&[("a.go", MAIN_GO)], 1, "outline").output,
            "a.go\n4 const:A\n5 const:B\n8-10 struct:Server\n12-14 fn:\"Server.Run\"\n"
        );
    }

    const STORE_JS: &str = "/** A store. */\nexport class Store {\n  size = 0;\n\n  @logged\n  add(item) {\n    this.size++;\n  }\n}\n\nexport const empty = () => new Store();\n";

    #[test]
    fn javascript_edits() {
        assert_eq!(
            edited_in(
                "a.js",
                STORE_JS,
                "insert end class:Store <<END\n\nclear() {\n  this.size = 0;\n}\nEND"
            ),
            STORE_JS.replace(
                "    this.size++;\n  }\n",
                "    this.size++;\n  }\n\n  clear() {\n    this.size = 0;\n  }\n"
            )
        );
        assert_eq!(
            edited_in(
                "a.js",
                STORE_JS,
                "replace fn:add with <<END\nadd(item, n) {\n  this.size += n;\n}\nEND"
            ),
            STORE_JS.replace(
                "add(item) {\n    this.size++;",
                "add(item, n) {\n    this.size += n;"
            )
        );
        assert_eq!(
            edited_in("a.js", STORE_JS, "insert before fn:add \"@traced\""),
            STORE_JS.replace("  @logged\n", "  @traced\n  @logged\n")
        );
        assert_eq!(
            edited_in("a.js", STORE_JS, "delete fn:empty"),
            STORE_JS.replace("\nexport const empty = () => new Store();\n", "")
        );
        assert_eq!(
            edited_in(
                "a.js",
                STORE_JS,
                "replace class:Store.sig with \"export class Shop\""
            ),
            STORE_JS.replace("export class Store", "export class Shop")
        );
    }

    #[test]
    fn typescript_outline_lists_a_function_variable_once() {
        let text = "export const f = (x: number) => x;\nconst n = 1;\n";
        assert_eq!(
            exec_with(&[("a.ts", text)], 1, "outline").output,
            "a.ts\n1 fn:f\n2 const:n\n"
        );
        assert_eq!(
            edited_in("a.ts", text, "replace const:f.name with \"g\""),
            text.replace("const f", "const g")
        );
    }

    #[test]
    fn inserted_text_takes_the_files_indent_style() {
        assert_eq!(
            edited(
                "fn a() {\n\tx();\n}\n",
                "insert after 2 <<END\nif y {\n    z();\n}\nEND\n"
            ),
            "fn a() {\n\tx();\n\tif y {\n\t\tz();\n\t}\n}\n"
        );
    }

    #[test]
    fn the_indent_unit_skips_strings_and_comments() {
        let code = "fn a() {\n    x();\n}\n";
        let script = "insert after 2 <<END\nif y {\n\tz();\n}\nEND\n";
        let expected = "fn a() {\n    x();\n    if y {\n        z();\n    }\n}\n";
        for rest in [
            "\nconst S: &str = \"\n\tb\n\";\n",
            "\n/*\n\tb\n*/\n",
            "\nconst S: &str = r#\"\nb\n\tc\n\t\"#;\n",
        ] {
            assert_eq!(
                edited(&format!("{code}{rest}"), script),
                format!("{expected}{rest}")
            );
        }
    }

    #[test]
    fn edits_keep_crlf_line_endings() {
        let text = "a {\r\n    b\r\n}\r\n";
        assert_eq!(
            edited(text, "insert after 2 <<END\nc\nd\nEND\n"),
            "a {\r\n    b\r\n    c\r\n    d\r\n}\r\n"
        );
        assert_eq!(
            edited(text, "replace 2 with \"e\""),
            "a {\r\n    e\r\n}\r\n"
        );
    }

    #[test]
    fn delete_whole_lines_and_tidy_blank_lines() {
        assert_eq!(edited("a\n\nb\n\nc\n", "delete 3"), "a\n\nc\n");
        assert_eq!(
            edited(TEXT, "delete 6-8"),
            "fn a() {\n    let x = 1;\n    let y = 2;\n}\n"
        );
        assert_eq!(
            edited(TEXT, "delete \"let y = 2;\""),
            TEXT.replace("    let y = 2;\n", "")
        );
    }

    const BLOCK: &str = "mod t {\n    fn x() {}\n\n    fn a() {}\n\n    fn b() {}\n}\n";

    #[test]
    fn deleting_neighbours_merges_their_blank_lines() {
        assert_eq!(
            edited(BLOCK, "delete fn:a; delete fn:b"),
            "mod t {\n    fn x() {}\n}\n"
        );
        assert_eq!(
            edited(BLOCK, "delete fn:x; delete fn:a"),
            "mod t {\n    fn b() {}\n}\n"
        );
        assert_eq!(
            edited(BLOCK, "delete fn:b; delete fn:x"),
            "mod t {\n    fn a() {}\n}\n"
        );
        assert_eq!(edited(BLOCK, "delete all mod:t>fn:*"), "mod t {\n}\n");
        let four = BLOCK.replace("fn b() {}\n", "fn b() {}\n\n    fn c() {}\n");
        assert_eq!(
            edited(&four, "delete fn:a; delete fn:b"),
            "mod t {\n    fn x() {}\n\n    fn c() {}\n}\n"
        );
    }

    #[test]
    fn moving_neighbours_out_merges_their_blank_lines() {
        let text = "mod t {\n    fn a() {}\n\n    fn b() {}\n}\n\nfn z() {}\n";
        assert_eq!(
            edited(text, "move fn:a after fn:z; move fn:b after fn:z"),
            "mod t {\n}\n\nfn z() {}\n\nfn a() {}\n\nfn b() {}\n"
        );
    }

    #[test]
    fn deleting_the_same_lines_twice_still_overlaps() {
        assert!(
            exec(BLOCK, "delete fn:a; delete fn:a")
                .error()
                .contains("edit overlaps command 1"),
        );
    }

    #[test]
    fn delete_partial_span() {
        assert_eq!(edited(TEXT, "delete \" = 2\""), TEXT.replace(" = 2", ""));
    }

    #[test]
    fn edit_count_is_the_number_of_spans() {
        let out = exec(TEXT, "delete all \"let x\".lines");
        let changes = out.result.unwrap();
        assert_eq!(changes[0].edits, 2);
        assert_eq!(
            changes[0].new,
            "fn a() {\n    let y = 2;\n}\n\nfn b() {\n}\n"
        );
    }

    #[test]
    fn sub_replaces_every_match_in_every_file() {
        let out = exec_with(
            &[
                ("a.rs", "old x old\n"),
                ("b.rs", "none\n"),
                ("c.rs", "old\n"),
            ],
            3,
            "sub /\\bold\\b/ with \"new\"",
        );
        let changes = out.result.unwrap();
        let summary: Vec<_> = changes
            .iter()
            .map(|c| (c.path.as_str(), c.new.as_str(), c.edits))
            .collect();
        assert_eq!(summary, [("a.rs", "new x new\n", 2), ("c.rs", "new\n", 1)]);
    }

    #[test]
    fn sub_expands_captures() {
        assert_eq!(
            edited(TEXT, "sub /let (?<v>\\w)/ with \"let mut ${v}$$\""),
            TEXT.replace("let x", "let mut x$")
                .replace("let y", "let mut y$")
        );
        assert_eq!(
            edited("a=1\n", "sub /(\\w)=(\\d)/ with \"$2=$1 ($0)\""),
            "1=a (a=1)\n"
        );
    }

    #[test]
    fn sub_within_scope() {
        assert_eq!(
            edited(TEXT, "sub 6-8 /x/ with \"q\""),
            TEXT.replace("x = 3", "q = 3")
        );
        assert_eq!(
            edited(TEXT, "sub all /let \\w/ /let/ with \"var\""),
            TEXT.replace("let", "var")
        );
    }

    #[test]
    fn sub_in_an_item_reaches_its_first_line_indentation() {
        let text = "impl S {\n    fn new() {\n        1\n    }\n}\n";
        assert_eq!(
            edited(text, "sub fn:new /^    / with \"\""),
            "impl S {\nfn new() {\n    1\n}\n}\n"
        );
    }

    #[test]
    fn markdown_section_bodies() {
        let md = |script: &str| md("# A\n\nintro\n\n## B\n\nb text\n\n## C\n\n# D", script);
        assert_eq!(
            md("insert end section:B \"more\""),
            "# A\n\nintro\n\n## B\n\nb text\nmore\n\n## C\n\n# D"
        );
        assert_eq!(
            md("replace section:B.body with \"new\""),
            "# A\n\nintro\n\n## B\n\nnew\n\n## C\n\n# D"
        );
        assert_eq!(
            md("insert start section:C \"c\""),
            "# A\n\nintro\n\n## B\n\nb text\n\n## C\nc\n\n# D"
        );
        assert_eq!(
            md("insert end section:D \"d\""),
            "# A\n\nintro\n\n## B\n\nb text\n\n## C\n\n# D\nd\n"
        );
        assert_eq!(
            md("insert end section:A <<END\n\n## E\nEND\n"),
            "# A\n\nintro\n\n## B\n\nb text\n\n## C\n\n## E\n\n# D"
        );
    }

    fn md(text: &str, script: &str) -> String {
        exec_with(&[("a.md", text)], 1, script)
            .new_text()
            .to_string()
    }

    const WRAPPED: &str = "- [ ] One item\n      wrapped here\n- [ ] Two\n";

    #[test]
    fn list_item_text_anchors_to_the_list_item() {
        let one_new_two = "- [ ] One item\n      wrapped here\n- [ ] New\n- [ ] Two\n";
        assert_eq!(
            md(WRAPPED, "insert after 2 <<END\n- [ ] New\nEND\n"),
            one_new_two
        );
        assert_eq!(
            md(WRAPPED, "insert after 1 <<END\n- [ ] New\nEND\n"),
            one_new_two
        );
        assert_eq!(
            md(WRAPPED, "insert before 2 <<END\n- [ ] New\nEND\n"),
            "- [ ] New\n- [ ] One item\n      wrapped here\n- [ ] Two\n"
        );
        assert_eq!(
            md(WRAPPED, "replace 2 with <<END\n- [ ] Replaced\nEND\n"),
            "- [ ] One item\n- [ ] Replaced\n- [ ] Two\n"
        );
        assert_eq!(
            md(WRAPPED, "insert after 2 <<'END'\n  - raw\nEND\n"),
            "- [ ] One item\n      wrapped here\n  - raw\n- [ ] Two\n"
        );
        // Prose continues the paragraph.
        assert_eq!(
            md(WRAPPED, "insert after 2 <<END\nmore words\nEND\n"),
            "- [ ] One item\n      wrapped here\n      more words\n- [ ] Two\n"
        );
    }

    #[test]
    fn list_item_text_anchors_to_the_innermost_list_item() {
        let nested = "- a\n  - b\n    wrapped\n- c\n";
        assert_eq!(
            md(nested, "insert after 3 <<END\n- new\nEND\n"),
            "- a\n  - b\n    wrapped\n  - new\n- c\n"
        );
        assert_eq!(
            md(nested, "insert after 1 <<END\n- new\nEND\n"),
            "- a\n  - b\n    wrapped\n- new\n- c\n"
        );
        assert_eq!(
            md(
                "1. one\n   more\n2. two\n",
                "insert after 2 <<END\n1. x\nEND\n"
            ),
            "1. one\n   more\n1. x\n2. two\n"
        );
        let code = "- a\n\n  ```\n  - y\n  ```\n";
        assert_eq!(
            md(code, "insert after 4 <<END\n- z\nEND\n"),
            "- a\n\n  ```\n  - y\n  - z\n  ```\n"
        );
    }

    #[test]
    fn sub_inserts_text_verbatim() {
        assert_eq!(
            edited(TEXT, "sub /1;\\n/ with <<END\n1;\n  // one\nEND\n"),
            TEXT.replace("1;\n", "1;\n  // one")
        );
    }

    #[test]
    fn sub_without_matches_is_an_error() {
        assert_eq!(
            exec(TEXT, "sub /nope/ with \"x\"").error(),
            "error: script:1:1: /nope/ matches nothing in a.rs; `show` prints the text to match against"
        );
    }

    #[test]
    fn selectors_resolve_against_the_original_text() {
        assert_eq!(
            edited(TEXT, "delete 1\nreplace 3 with \"z();\""),
            "    let x = 1;\n    z();\n}\n\nfn b() {\n    let x = 3;\n}\n"
        );
        let out = exec(TEXT, "delete 2\nshow 2");
        assert_eq!(out.output, "a.rs:2\n2:    let x = 1;\n");
    }

    #[test]
    fn overlapping_edits_name_the_earlier_command() {
        assert_eq!(
            exec(TEXT, "replace 2 with \"a\"\ndelete 2-3").error(),
            "error: script:2:1: edit overlaps command 1 at a.rs:2; merge the two edits, or put a `|` between them"
        );
        assert_eq!(
            exec(TEXT, "show 1; delete 2-3; replace \"y\" with \"z\"").error(),
            "error: script:1:21: edit overlaps command 2 at a.rs:2-3; merge the two edits, or put a `|` between them"
        );
    }

    #[test]
    fn insertions_at_one_point_apply_in_command_order() {
        assert_eq!(
            edited(TEXT, "insert after 2 \"a;\"; insert before 3 \"b;\""),
            TEXT.replace("1;\n", "1;\n    a;\n    b;\n")
        );
        assert_eq!(
            edited(TEXT, "replace 2 with \"q;\"; insert after 2 \"r;\""),
            TEXT.replace("let x = 1;\n", "q;\n    r;\n")
        );
    }

    #[test]
    fn reads_before_an_error_are_kept() {
        let out = exec(TEXT, "show 1\ndelete /nope/\nshow 2");
        assert_eq!(out.output, "a.rs:1\n1:fn a() {\n");
        assert_eq!(
            out.error(),
            "error: script:2:8: /nope/ matches nothing in a.rs; `show` prints the text to match against"
        );
    }

    #[test]
    fn file_command_replaces_the_file_set() {
        let out = exec_with(
            &[("a.rs", "x\n"), ("b.rs", "x\n")],
            1,
            "show\nfile {dir}/b.rs\nsub /x/ with \"y\"\nshow",
        );
        assert_eq!(out.output, "a.rs:1\n1:x\nb.rs:1\n1:x\n");
        let changes = out.result.unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(
            (changes[0].path.as_str(), changes[0].new.as_str()),
            ("b.rs", "y\n")
        );
    }

    #[test]
    fn changes_are_listed_in_order_of_first_appearance() {
        let out = exec_with(
            &[("a.rs", "x\n"), ("b.rs", "x\n")],
            0,
            "file {dir}/b.rs\ndelete 1\nfile {dir}/a.rs {dir}/b.rs\ninsert before file:{dir}/a.rs>1 \"z\"",
        );
        let paths: Vec<_> = out.result.unwrap().into_iter().map(|c| c.path).collect();
        assert_eq!(paths, ["b.rs", "a.rs"]);
    }

    /// The files `script`'s last `file` command selects, by running `show`.
    fn globbed(files: &[&str], script: &str) -> Vec<String> {
        let files: Vec<(&str, &str)> = files.iter().map(|f| (*f, "x\n")).collect();
        let out = exec_with(&files, 0, &format!("{script}; show"));
        assert!(out.result.is_ok(), "{:?}", out.result);
        out.output
            .lines()
            .filter_map(|l| l.strip_suffix(":1"))
            .map(String::from)
            .collect()
    }

    #[test]
    fn file_expands_globs_in_sorted_order() {
        let files = ["b.rs", "a.rs", "c.txt", ".hidden.rs", "sub/d.rs"];
        assert_eq!(globbed(&files, "file {dir}/*.rs"), ["a.rs", "b.rs"]);
        assert_eq!(globbed(&files, "file {dir}/[ab].r?"), ["a.rs", "b.rs"]);
    }

    #[test]
    fn double_star_recurses() {
        let files = ["sub/deep/c.rs", "a.rs", "sub/b.rs", "sub/b.txt"];
        assert_eq!(
            globbed(&files, "file {dir}/**/*.rs"),
            ["a.rs", "sub/b.rs", "sub/deep/c.rs"]
        );
    }

    #[test]
    fn globs_match_only_files() {
        assert_eq!(globbed(&["a.rs", "d.rs/x"], "file {dir}/*.rs"), ["a.rs"]);
    }

    #[test]
    fn files_named_twice_are_in_the_set_once() {
        let files = ["a.rs", "b.rs"];
        assert_eq!(
            globbed(&files, "file {dir}/b.rs {dir}/*.rs {dir}/b.rs"),
            ["b.rs", "a.rs"]
        );
    }

    #[test]
    fn a_glob_matching_nothing_is_an_error() {
        assert_eq!(
            exec_with(&[("a.rs", "x\n")], 0, "file {dir}/*.rx").error(),
            "error: script:1:1: glob `*.rx` matched nothing"
        );
    }

    #[test]
    fn missing_files_are_io_errors() {
        let out = exec_with(&[("a.rs", "x\n")], 1, "file {dir}/nope.rs");
        assert!(
            out.error()
                .starts_with("error: script:1:1: cannot read nope.rs: "),
            "{}",
            out.error()
        );
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("gone.rs").display().to_string();
        let parsed = parse("show").unwrap();
        let err = run(
            &parsed,
            "show",
            Initial::Files(std::slice::from_ref(&missing)),
            &Options::default(),
            None,
        )
        .result
        .unwrap_err();
        assert!(
            err.render("show")
                .starts_with(&format!("error: cannot read {missing}: "))
        );
    }

    #[test]
    fn non_utf8_files_are_io_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bin.rs");
        fs::write(&path, [0xff, 0xfe, b'\n']).unwrap();
        let path = path.display().to_string();
        let parsed = parse("show").unwrap();
        let err = run(
            &parsed,
            "show",
            Initial::Files(std::slice::from_ref(&path)),
            &Options::default(),
            None,
        )
        .result
        .unwrap_err();
        assert_eq!(
            err.render("show"),
            format!("error: cannot read {path}: not valid UTF-8")
        );
    }

    #[test]
    fn commands_need_files() {
        let parsed = parse("show 1").unwrap();
        let err = run(
            &parsed,
            "show 1",
            Initial::Files(&[]),
            &Options::default(),
            None,
        )
        .result
        .unwrap_err();
        assert_eq!(
            err.render("show 1"),
            "error: script:1:1: no files to edit; pass FILE arguments or use `file PATH`"
        );
    }

    #[test]
    fn insert_next_to_a_separated_item_adds_a_blank_line() {
        assert_eq!(
            edited(MOVE, r#"insert after fn:helper_y "fn z() {}""#),
            MOVE.replace("fn helper_y() {}\n", "fn helper_y() {}\n\nfn z() {}\n")
        );
        assert_eq!(
            edited(MOVE, r#"insert before fn:main "fn z() {}""#),
            MOVE.replace("fn main", "fn z() {}\n\nfn main")
        );
        assert_eq!(
            edited(MOVE, r#"insert after impl:A>fn:b "fn c() {}""#),
            MOVE.replace("    fn b() {}\n", "    fn b() {}\n\n    fn c() {}\n")
        );
    }

    #[test]
    fn insert_keeps_a_blank_line_the_text_already_has() {
        assert_eq!(
            edited(MOVE, "insert after fn:helper_y <<END\n\nfn z() {}\nEND\n"),
            MOVE.replace("fn helper_y() {}\n", "fn helper_y() {}\n\nfn z() {}\n")
        );
        assert_eq!(
            edited(MOVE, "insert before fn:main <<END\nfn z() {}\n\nEND\n"),
            MOVE.replace("fn main", "fn z() {}\n\nfn main")
        );
    }

    #[test]
    fn a_string_ending_in_a_newline_inserts_no_blank_line() {
        assert_eq!(
            edited(MOVE, r#"insert after fn:helper_y "fn z() {}\n""#),
            edited(MOVE, r#"insert after fn:helper_y "fn z() {}""#)
        );
        assert_eq!(
            edited(MOVE, r#"insert after fn:helper_y "fn z() {}\n\n""#),
            MOVE.replace("fn helper_y() {}\n", "fn helper_y() {}\n\nfn z() {}\n\n")
        );
    }

    #[test]
    fn insert_adds_no_blank_line_next_to_unseparated_items_or_imports() {
        assert_eq!(
            edited(
                "struct S {\n    a: u8,\n    b: u8,\n}\n",
                r#"insert after field:a "c: u8,""#
            ),
            "struct S {\n    a: u8,\n    c: u8,\n    b: u8,\n}\n"
        );
        assert_eq!(
            edited("use a;\n\nfn f() {}\n", r#"insert after import:a "use b;""#),
            "use a;\nuse b;\n\nfn f() {}\n"
        );
        assert_eq!(
            edited(MOVE, r#"insert after 13 "fn z() {}""#),
            MOVE.replace("fn helper_y() {}\n", "fn helper_y() {}\nfn z() {}\n")
        );
    }

    #[test]
    fn inserting_an_item_before_an_items_docs_separates_it() {
        let text = "fn z() {}\n\n/// Doc.\n#[inline]\nfn a() {}\n";
        let separated = "fn z() {}\n\nfn b() {}\n\n/// Doc.\n#[inline]\nfn a() {}\n";
        assert_eq!(edited(text, "insert before 3 \"fn b() {}\""), separated);
        assert_eq!(
            edited(text, "insert before \"/// Doc.\" <<END\nfn b() {}\nEND\n"),
            separated
        );
        assert_eq!(
            edited(
                "fn z() {}\n#[test]\nfn a() {}\n\n",
                "insert before 2 \"fn b() {}\""
            ),
            "fn z() {}\nfn b() {}\n\n#[test]\nfn a() {}\n\n"
        );
    }

    #[test]
    fn inserting_before_other_lines_adds_no_blank_line() {
        let text = "fn z() {}\n\n/// Doc.\n#[inline]\nfn a() {}\n";
        assert_eq!(
            edited(text, "insert before 4 \"/// More.\""),
            "fn z() {}\n\n/// Doc.\n/// More.\n#[inline]\nfn a() {}\n"
        );
        assert_eq!(
            edited(text, "insert before 3 \"// c\""),
            "fn z() {}\n\n// c\n/// Doc.\n#[inline]\nfn a() {}\n"
        );
        assert_eq!(
            edited(text, "insert before 3 \"x();\""),
            "fn z() {}\n\nx();\n/// Doc.\n#[inline]\nfn a() {}\n"
        );
        assert_eq!(
            edited(text, "insert before 3 <<END\nfn b() {}\n\nEND\n"),
            "fn z() {}\n\nfn b() {}\n\n/// Doc.\n#[inline]\nfn a() {}\n"
        );
        assert_eq!(
            edited(
                "fn z() {}\n/// Doc.\nfn a() {}\n",
                "insert before 2 \"fn b() {}\""
            ),
            "fn z() {}\nfn b() {}\n/// Doc.\nfn a() {}\n"
        );
        assert_eq!(
            edited("fn z() {}\n\nfn a() {}\n", "insert before 3 \"fn b() {}\""),
            "fn z() {}\n\nfn b() {}\nfn a() {}\n"
        );
    }

    const ATTRS: &str = "/// Doc.\n#[test]\nfn a() {\n    old();\n}\n";

    #[test]
    fn replacing_an_item_keeps_its_attributes_and_docs() {
        assert_eq!(
            edited(
                ATTRS,
                "replace fn:a with <<END\nfn a() {\n    new();\n}\nEND\n"
            ),
            "/// Doc.\n#[test]\nfn a() {\n    new();\n}\n"
        );
        assert_eq!(
            edited(
                ATTRS,
                "replace fn:a with <<END\n#[tokio::test]\nasync fn a() {}\nEND\n"
            ),
            "#[tokio::test]\nasync fn a() {}\n"
        );
        assert_eq!(
            edited(ATTRS, "replace fn:a with \"/// New.\\nfn a() {}\""),
            "/// New.\nfn a() {}\n"
        );
        assert_eq!(
            edited(ATTRS, "replace fn:a.whole with \"fn a() {}\""),
            "fn a() {}\n"
        );
        assert_eq!(
            edited("#[inline] fn a() {}\n", "replace fn:a with \"fn b() {}\""),
            "fn b() {}\n"
        );
    }

    #[test]
    fn attributes_inserted_before_an_item_attach_to_it() {
        let text = "fn z() {}\n\n/// Doc.\nfn a() {}\n";
        assert_eq!(
            edited(text, "insert before fn:a \"#[inline]\""),
            "fn z() {}\n\n#[inline]\n/// Doc.\nfn a() {}\n"
        );
        assert_eq!(
            edited(
                text,
                "insert before fn:a <<END\n/// More.\n#[must_use]\nEND\n"
            ),
            "fn z() {}\n\n/// More.\n#[must_use]\n/// Doc.\nfn a() {}\n"
        );
        assert_eq!(
            edited(text, "insert before fn:a \"// c\""),
            "fn z() {}\n\n// c\n\n/// Doc.\nfn a() {}\n"
        );
    }

    #[test]
    fn moved_docs_attach_to_the_item_they_move_before() {
        let text = "/// Doc.\n\nfn a() {}\n\nfn b() {}\n";
        assert_eq!(
            edited(text, "move 1 before fn:b"),
            "fn a() {}\n\n/// Doc.\nfn b() {}\n"
        );
    }

    #[test]
    fn replacing_off_by_one_lines_leaves_a_note() {
        let notes = |text: &str, script: &str| exec(text, script).notes;
        assert_eq!(
            notes(TEXT, "replace 3 with <<END\nlet x = 1;\nlet y = 5;\nEND\n"),
            [
                "a.rs:3: the new text starts with a copy of line 2 (`let x = 1;`), \
              just above the replaced lines; the range may be off by one"
            ]
        );
        assert_eq!(
            notes(TEXT, "replace 2 with <<END\nlet x = 0;\nlet y = 2;\nEND\n"),
            [
                "a.rs:2: the new text ends with a copy of line 3 (`let y = 2;`), \
              just below the replaced lines; the range may be off by one"
            ]
        );
        // `}` alone doesn't count, nor does repeating the span's own line.
        assert!(notes(TEXT, "replace 7 with <<END\nlet x = 4;\n}\nEND\n").is_empty());
        assert!(notes(TEXT, "replace 3 with <<END\nlet y = 2;\nlet z = 2;\nEND\n").is_empty());
    }

    #[test]
    fn replacing_part_of_a_line_with_its_rest_leaves_a_note() {
        let notes = |script: &str| exec("let a = f(b);\n", script).notes;
        assert_eq!(
            notes("replace /let a/ with \"let c = f(b);\""),
            [
                "a.rs:1: the new text ends with `= f(b);`, which already follows the \
              replaced text on its line; to replace whole lines, select /let a/.lines"
            ]
        );
        assert_eq!(
            notes("replace \"f(b)\" with \"let a = g(b)\""),
            [
                "a.rs:1: the new text starts with `let a =`, which already precedes the \
              replaced text on its line; to replace whole lines, select \"f(b)\".lines"
            ]
        );
        assert!(notes("replace \"f(b)\" with \"g(b)\"").is_empty());
        // Across lines, `.lines` would select each line, so no fix is suggested.
        assert_eq!(
            exec(
                "let a = f(\n    b) + c;\n",
                r#"replace /f\(\n    b\)/ with "g(\n    b) + c;""#
            )
            .notes,
            [
                "a.rs:1: the new text ends with `+ c;`, which already follows the \
              replaced text on its line"
            ]
        );
    }

    #[test]
    fn rewrapping_the_rest_of_a_line_still_leaves_a_note() {
        let notes = |script: &str| exec("let a = f(b) + c(d);\n", script).notes;
        assert_eq!(
            notes("replace \"let a\" with \"let x = f(b) +\\n    c( d );\""),
            [
                "a.rs:1: the new text ends with `= f(b) + c(d);`, which already follows \
              the replaced text on its line; to replace whole lines, select \"let a\".lines"
            ]
        );
        assert_eq!(
            notes("replace \"c(d)\" with \"let a = f(b)\\n    + e(d)\""),
            [
                "a.rs:1: the new text starts with `let a = f(b) +`, which already precedes \
              the replaced text on its line; to replace whole lines, select \"c(d)\".lines"
            ]
        );
        // A changed rest is no copy, and an empty rest counts for nothing.
        assert!(notes("replace \"let a\" with \"let x =\\n    g(b) + c(d);\"").is_empty());
        assert!(notes("replace \"c(d);\" with \"e(d);\\n\"").is_empty());
    }

    #[test]
    fn create_makes_a_file_later_commands_can_edit() {
        let out = exec_with(
            &[("a.rs", "x\n")],
            1,
            "create {dir}/b.rs <<END\n    fn b() {}\nEND\ninsert end fn:b \"c();\"\nshow fn:b",
        );
        assert_eq!(out.output, "b.rs:1\n1:fn b() {}\n");
        let changes = out.result.unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "b.rs");
        assert!(changes[0].created);
        assert_eq!(changes[0].old, "");
        assert_eq!(changes[0].new, "fn b() {\n    c();\n}\n");
    }

    #[test]
    fn created_files_join_the_file_set() {
        let out = exec_with(
            &[("a.rs", "x\n")],
            1,
            "create {dir}/b.rs \"y\"\nsub /x|y/ with \"z\"",
        );
        let news: Vec<_> = out.result.unwrap().into_iter().map(|c| c.new).collect();
        assert_eq!(news, ["z\n", "z\n"]);
    }

    #[test]
    fn create_needs_a_new_path() {
        assert_eq!(
            exec_with(&[("a.rs", "x\n")], 0, "create {dir}/a.rs \"y\"").error(),
            "error: script:1:1: a.rs already exists; edit it with `file a.rs`"
        );
        let twice = exec_with(&[], 0, "create {dir}/b.rs \"x\"\ncreate {dir}/b.rs \"y\"");
        assert!(
            twice
                .error()
                .starts_with("error: script:2:1: b.rs already exists"),
            "{}",
            twice.error()
        );
    }

    #[test]
    fn created_files_pass_the_guard_and_may_be_empty() {
        let out = exec_with_options(&[], 0, "create {dir}/b.rs \"fn (\"", &Options::default());
        assert!(
            out.error().contains("edit introduces a syntax error"),
            "{}",
            out.error()
        );
        let empty = exec_with(&[], 0, "create {dir}/e.txt \"\"");
        assert_eq!(empty.result.unwrap()[0].new, "");
    }

    #[test]
    fn create_ends_a_string_with_one_newline() {
        let new = |script| exec_with(&[], 0, script).result.unwrap()[0].new.clone();
        assert_eq!(new("create {dir}/b.rs \"fn a() {}\\n\""), "fn a() {}\n");
        assert_eq!(
            new("create {dir}/b.rs \"fn a() {}\\n\\n\""),
            "fn a() {}\n\n"
        );
    }

    #[test]
    fn insert_next_to_a_created_item_separates_it_as_on_disk() {
        let insert = |file: &str| {
            let script = format!("create {{dir}}/b.rs {file:?}\ninsert after fn:a \"fn b() {{}}\"");
            let created = exec_with(&[], 0, &script).result.unwrap()[0].new.clone();
            assert_eq!(
                created,
                edited(file, r#"insert after fn:a "fn b() {}""#),
                "{file:?}"
            );
            created
        };
        assert_eq!(insert("fn a() {}\n"), "fn a() {}\nfn b() {}\n");
        assert_eq!(
            insert("fn a() {}\n\nfn c() {}\n"),
            "fn a() {}\n\nfn b() {}\n\nfn c() {}\n"
        );
    }

    const MOVE: &str = "\
impl A {
    fn a() {
        one();
    }

    fn b() {}
}

fn helper_x() {
    x();
}

fn helper_y() {}

fn main() {}
";

    #[test]
    fn move_after_keeps_blank_separation() {
        assert_eq!(
            edited(MOVE, "move fn:helper_x after fn:main"),
            MOVE.replace("fn helper_x() {\n    x();\n}\n\n", "")
                + "\nfn helper_x() {\n    x();\n}\n"
        );
    }

    #[test]
    fn move_before_keeps_blank_separation() {
        assert_eq!(
            edited(MOVE, "move fn:helper_y before fn:helper_x"),
            MOVE.replace("fn helper_y() {}\n\n", "")
                .replace("fn helper_x", "fn helper_y() {}\n\nfn helper_x")
        );
    }

    #[test]
    fn move_all_keeps_source_order() {
        assert_eq!(edited(MOVE, "move all fn:helper_* before fn:main"), MOVE);
    }

    #[test]
    fn move_to_the_end_of_a_body_is_rebased() {
        assert_eq!(
            edited(MOVE, "move fn:helper_y end impl:A"),
            MOVE.replace("fn helper_y() {}\n\n", "")
                .replace("    fn b() {}\n", "    fn b() {}\n    fn helper_y() {}\n")
        );
    }

    #[test]
    fn move_to_the_start_of_a_body() {
        assert_eq!(
            edited(MOVE, "move fn:b start fn:helper_x"),
            MOVE.replace("\n    fn b() {}\n", "")
                .replace("{\n    x();", "{\n    fn b() {}\n    x();")
        );
    }

    #[test]
    fn move_into_an_empty_body_opens_it() {
        assert_eq!(
            edited(MOVE, "move fn:helper_x end fn:main"),
            MOVE.replace("fn helper_x() {\n    x();\n}\n\n", "")
                .replace(
                    "fn main() {}\n",
                    "fn main() {\n    fn helper_x() {\n        x();\n    }\n}\n"
                )
        );
    }

    #[test]
    fn move_across_files() {
        let b = "impl B {\n    fn c() {}\n}\n";
        let out = exec_with(
            &[("a.rs", MOVE), ("b.rs", b)],
            2,
            "move fn:helper_y end file:{dir}/b.rs>impl:B",
        );
        let changes = out.result.unwrap();
        assert_eq!(changes[0].path, "a.rs");
        assert_eq!(changes[0].new, MOVE.replace("fn helper_y() {}\n\n", ""));
        assert_eq!(changes[1].path, "b.rs");
        assert_eq!(
            changes[1].new,
            "impl B {\n    fn c() {}\n    fn helper_y() {}\n}\n"
        );
    }

    #[test]
    fn move_of_a_partial_span_is_verbatim() {
        assert_eq!(edited("(a)(b)\n", r#"move "a" before "b""#), "()(ab)\n");
    }

    #[test]
    fn move_before_a_field_adds_its_comma() {
        assert_eq!(
            edited(
                "struct S {\n    a: u8,\n    b: u8\n}\n",
                "move field:b before field:a"
            ),
            "struct S {\n    b: u8,\n    a: u8,\n}\n"
        );
    }

    #[test]
    fn move_into_its_own_source_is_an_error() {
        assert_eq!(
            exec(MOVE, "move impl:A before fn:b").error(),
            "error: script:1:1: move destination is inside the moved span at a.rs:1-7; \
             choose a destination outside it"
        );
    }

    #[test]
    fn move_destination_must_be_one_span() {
        let err = exec(MOVE, "move fn:main after fn:helper_*")
            .error()
            .to_string();
        assert!(err.starts_with("error: script:1:"), "{err}");
        assert!(err.contains("fn:helper_* matches 2 items"), "{err}");
    }

    #[test]
    fn move_from_crlf_to_lf() {
        let out = exec_with(
            &[
                ("a.rs", "use x;\r\n\r\nfn a() {}\r\n\r\nfn b() {}\r\n"),
                ("b.rs", "fn c() {}\n"),
            ],
            2,
            "move fn:a after file:{dir}/b.rs>fn:c",
        );
        let changes = out.result.unwrap();
        assert_eq!(changes[0].new, "use x;\r\n\r\nfn b() {}\r\n");
        assert_eq!(changes[1].new, "fn c() {}\n\nfn a() {}\n");
    }

    const GUARD_ERROR: &str = "edit introduces a syntax error (use --force to apply anyway)";

    #[test]
    fn guard_rejects_new_syntax_errors() {
        let out = guarded("a.rs", TEXT, "replace \"let y = 2;\" with \"let y = (2;\"");
        let err = out.error();
        assert!(err.starts_with("error: a.rs:3:"), "{err}");
        assert!(err.contains(GUARD_ERROR), "{err}");
        let excerpt: Vec<&str> = err.lines().skip(1).collect();
        assert_eq!(excerpt.len(), 2, "{err}");
        assert_eq!(excerpt[0], "3:    let y = (2;");
        assert!(excerpt[1].trim_start() == "^", "{err}");
    }

    #[test]
    fn guard_points_at_the_edited_text() {
        let text = "fn a() {\n    let x = (1;\n}\n\nfn b() {\n    let y = 2;\n}\n";
        let out = guarded("a.rs", text, "replace \"let y = 2;\" with \"let y = [2;\"");
        assert!(out.error().starts_with("error: a.rs:6:"), "{}", out.error());
    }

    #[test]
    fn guard_allows_errors_that_were_already_there() {
        let text = "fn a() {\n    let x = (1;\n}\n\nfn b() {\n    let y = 2;\n}\n";
        let out = guarded("a.rs", text, "replace \"let y = 2;\" with \"let y = 3;\"");
        assert_eq!(
            out.new_text(),
            "fn a() {\n    let x = (1;\n}\n\nfn b() {\n    let y = 3;\n}\n"
        );
    }

    #[test]
    fn guard_rejects_an_emptied_python_body() {
        let text = "class A:\n    def f(self):\n        pass\n";
        let out = guarded("a.py", text, "delete fn:f");
        assert!(
            out.error().starts_with("error: a.py:1:9:"),
            "{}",
            out.error()
        );
        let text = "def f():\n    return 1\n\n\nx = 1\n";
        let out = guarded("a.py", text, "delete fn:f.body");
        assert!(out.error().contains(GUARD_ERROR), "{}", out.error());
    }

    #[test]
    fn guard_allows_python_bodies_that_were_already_empty() {
        let text = "class A:\n\n\ndef f():\n    return 1\n";
        let out = guarded("a.py", text, "replace \"return 1\" with \"return 2\"");
        assert!(out.result.is_ok(), "{}", out.error());
        let out = guarded(
            "a.py",
            "class A:\n    x = 1\n",
            "replace \"x = 1\" with \"pass\"",
        );
        assert!(out.result.is_ok(), "{}", out.error());
    }

    const SIG_HINT: &str = "`.sig` stops before the `:`";

    #[test]
    fn guard_says_a_python_sig_stops_before_the_colon() {
        let text = "def f():\n    pass\n";
        let out = guarded("a.py", text, "replace fn:f.sig with \"def f() -> None:\"");
        assert!(out.error().contains(GUARD_ERROR), "{}", out.error());
        assert!(out.error().contains(SIG_HINT), "{}", out.error());
        let out = guarded("a.py", text, "replace fn:f.sig with \"def f(:\"");
        assert!(out.error().contains(SIG_HINT), "{}", out.error());
    }

    #[test]
    fn guard_hints_at_the_colon_only_for_a_python_sig() {
        let text = "def f():\n    pass\n";
        let out = guarded("a.py", text, "replace fn:f.sig with \"def f(\"");
        assert!(!out.error().contains(SIG_HINT), "{}", out.error());
        let out = guarded("a.py", text, "replace \"def f()\" with \"def f():\"");
        assert!(!out.error().contains(SIG_HINT), "{}", out.error());
    }

    const RUST_FN: &str = "fn f(a: u8) {\n    let x = a;\n}\n";

    #[test]
    fn guard_says_a_sig_stops_before_the_brace() {
        let out = guarded("a.rs", RUST_FN, "replace fn:f.sig with \"fn f(b: u8) {\"");
        assert!(out.error().contains(GUARD_ERROR), "{}", out.error());
        assert!(
            out.error().contains("`.sig` stops before the `{`"),
            "{}",
            out.error()
        );
        let out = guarded(
            "a.go",
            "package a\n\nfunc f() {\n}\n",
            "replace fn:f.sig with \"func f(x int) {\"",
        );
        assert!(
            out.error().contains("`.sig` stops before the `{`"),
            "{}",
            out.error()
        );
    }

    #[test]
    fn guard_hints_at_the_brace_only_when_text_ends_with_it() {
        let out = guarded("a.rs", RUST_FN, "replace fn:f.sig with \"fn f(b: u8\"");
        assert!(out.error().contains(GUARD_ERROR), "{}", out.error());
        assert!(!out.error().contains("stops before"), "{}", out.error());
    }

    #[test]
    fn guard_points_at_the_edit_inside_a_file_wide_error_node() {
        let out = guarded("a.rs", RUST_FN, "replace fn:f.sig with \"fn f(b: u8) {\"");
        assert!(
            out.error().starts_with("error: a.rs:1:6:"),
            "{}",
            out.error()
        );
        let text = format!("{RUST_FN}\n{}", RUST_FN.replace("f(", "g("));
        let out = guarded("a.rs", &text, "replace fn:g.sig with \"fn g(b: u8) {\"");
        assert!(
            out.error().starts_with("error: a.rs:5:6:"),
            "{}",
            out.error()
        );
    }

    const ESCAPE_HINT: &str = "heredocs read no escapes";

    #[test]
    fn guard_says_a_heredoc_reads_no_escapes() {
        let script = "insert end fn:a <<END\nlet c = \\x27x\\x27;\nEND";
        let out = guarded("a.rs", TEXT, script);
        let err = out.error();
        assert!(err.contains(GUARD_ERROR), "{err}");
        assert!(err.contains(ESCAPE_HINT), "{err}");
        assert!(err.contains("`\\x27`"), "{err}");
        assert!(err.contains("on stdin"), "{err}");
    }

    #[test]
    fn guard_hints_at_escapes_only_in_the_edited_text() {
        let text = "fn a() {\n    let s = \"\\x27\";\n    let y = 2;\n}\n";
        let out = guarded("a.rs", text, "replace \"let y = 2;\" with \"let y = (2;\"");
        assert!(out.error().contains(GUARD_ERROR), "{}", out.error());
        assert!(!out.error().contains(ESCAPE_HINT), "{}", out.error());
        let out = guarded(
            "a.rs",
            TEXT,
            "replace \"let y = 2;\" with \"let y = (2; // \\\\x\"",
        );
        assert!(!out.error().contains(ESCAPE_HINT), "{}", out.error());
    }

    #[test]
    fn guard_skips_files_without_a_language() {
        let out = guarded("a.txt", TEXT, "replace \"let y = 2;\" with \"(\"");
        assert!(out.result.is_ok(), "{}", out.error());
    }

    #[test]
    fn force_skips_the_guard() {
        let out = exec(TEXT, "replace \"let y = 2;\" with \"(\"");
        assert!(out.new_text().contains("    (\n"), "{:?}", out.result);
    }

    #[test]
    fn lang_option_overrides_detection() {
        let script = "replace \"let y = 2;\" with \"let y = (2;\"";
        let out = guarded("a.txt", TEXT, script);
        assert!(out.result.is_ok());
        let rust = Options {
            lang: Some(Some(Language::Rust)),
            ..Options::default()
        };
        let out = exec_with_options(&[("a.txt", TEXT)], 1, script, &rust);
        assert!(
            out.error().starts_with("error: a.txt:3:"),
            "{}",
            out.error()
        );
        let python = Options {
            lang: Some(Some(Language::Python)),
            ..Options::default()
        };
        let out = exec_with_options(
            &[("a.rs", "x = 1\n")],
            1,
            "replace 1 with \"y = 2\"",
            &python,
        );
        assert_eq!(out.new_text(), "y = 2\n");
    }

    #[test]
    fn lang_text_reads_every_file_as_text() {
        let text = Options {
            lang: Some(None),
            ..Options::default()
        };
        let script = "replace \"let y = 2;\" with \"let y = (2;\"";
        let out = exec_with_options(&[("a.rs", TEXT)], 1, script, &text);
        assert!(out.result.is_ok(), "{:?}", out.result);
        assert!(out.notes.is_empty(), "{:?}", out.notes);
        let out = exec_with_options(&[("a.rs", TEXT)], 1, "show fn:main", &text);
        assert_eq!(
            out.error(),
            "error: script:1:6: fn:main needs a language, but parsing was disabled with --lang text; drop it, or use a regex or literal"
        );
        let out = exec_with_options(&[("a.rs", TEXT)], 1, "show `let y = @x;`", &text);
        assert!(
            out.error()
                .contains("`let y = @x;` needs a language, but parsing was disabled"),
            "{}",
            out.error()
        );
    }

    #[test]
    fn unknown_extensions_get_one_note() {
        let files = [
            ("b.toml", "x\n"),
            ("a.json", "x\n"),
            ("c.toml", "x\n"),
            ("d.rs", "x\n"),
            ("e.txt", "x\n"),
            ("Makefile", "x\n"),
        ];
        let out = exec_with_options(&files, files.len(), "show all \"x\"", &Options::default());
        assert!(out.result.is_ok(), "{:?}", out.result);
        assert_eq!(
            out.notes,
            ["read .json, .toml files as text; syntax selectors skip them"]
        );
        let out = exec_with_options(&files[3..], 3, "show all \"x\"", &Options::default());
        assert!(out.notes.is_empty(), "{:?}", out.notes);
        let rust = Options {
            lang: Some(Some(Language::Rust)),
            ..Options::default()
        };
        let out = exec_with_options(&files, files.len(), "show all \"x\"", &rust);
        assert!(out.notes.is_empty(), "{:?}", out.notes);
    }

    #[test]
    fn created_files_with_unknown_extensions_are_noted() {
        let out = exec_with_options(&[], 0, "create {dir}/a.yaml \"k: v\"", &Options::default());
        assert!(out.result.is_ok(), "{:?}", out.result);
        assert_eq!(
            out.notes,
            ["read .yaml files as text; syntax selectors skip them"]
        );
    }

    #[test]
    fn go_files_default_to_tab_indents() {
        let text = "package a\n\nfunc f() {\n}\n";
        let out = exec_with(
            &[("a.go", text)],
            1,
            "insert after 3 \"if x {\\n    y()\\n}\"",
        );
        assert_eq!(
            out.new_text(),
            "package a\n\nfunc f() {\nif x {\n\ty()\n}\n}\n"
        );
        let out = exec_with(
            &[("a.txt", text)],
            1,
            "insert after 3 \"if x {\\n\\ty()\\n}\"",
        );
        assert_eq!(
            out.new_text(),
            "package a\n\nfunc f() {\nif x {\n    y()\n}\n}\n"
        );
    }

    #[test]
    fn replacing_an_item_keeps_its_trailing_comma() {
        let text = "enum A {\n    B(u8),\n    C,\n}\n";
        assert_eq!(
            edited(text, "replace variant:B with \"D\""),
            "enum A {\n    D,\n    C,\n}\n"
        );
        assert_eq!(
            edited(text, "replace variant:B with \"D,\""),
            "enum A {\n    D,\n    C,\n}\n"
        );
        assert_eq!(edited(text, "delete variant:B"), "enum A {\n    C,\n}\n");
        assert_eq!(
            edited(text, "replace 2 with \"D\""),
            "enum A {\n    D\n    C,\n}\n"
        );
    }

    const ITEMS: &str = "impl A {\n    fn f() {}\n\n    fn g() {\n    }\n\n    fn h() { x() }\n}\n\nfn main() {\n    let x = 1;\n}\n";

    #[test]
    fn insert_start_and_end_imply_body() {
        assert_eq!(
            edited(ITEMS, "insert start fn:main \"a();\""),
            ITEMS.replace("fn main() {\n", "fn main() {\n    a();\n")
        );
        assert_eq!(
            edited(ITEMS, "insert end fn:main \"a();\""),
            ITEMS.replace("let x = 1;\n", "let x = 1;\n    a();\n")
        );
        assert_eq!(
            edited(ITEMS, "insert end impl:A \"fn i() {}\""),
            ITEMS.replace("x() }\n}\n", "x() }\n    fn i() {}\n}\n")
        );
        assert_eq!(
            edited(ITEMS, "insert after fn:main \"fn b() {}\""),
            format!("{ITEMS}\nfn b() {{}}\n")
        );
    }

    #[test]
    fn blank_bodies_take_the_items_indent_plus_a_unit() {
        let text = "mod tests {\n\n}\n";
        assert_eq!(
            edited(text, "insert end mod:tests \"fn a() {}\""),
            "mod tests {\n\n    fn a() {}\n}\n"
        );
        assert_eq!(
            edited(text, "insert start mod:tests \"fn a() {}\""),
            "mod tests {\n    fn a() {}\n\n}\n"
        );
        let text = "impl A {\n    fn f() {\n  \n    }\n}\n";
        assert_eq!(
            edited(text, "insert end fn:f \"x();\""),
            "impl A {\n    fn f() {\n  \n        x();\n    }\n}\n"
        );
        let text = "mod tests {\n\n    fn b() {}\n}\n";
        assert_eq!(
            edited(text, "insert end mod:tests \"fn a() {}\""),
            "mod tests {\n\n    fn b() {}\n    fn a() {}\n}\n"
        );
        assert_eq!(
            edited(text, "insert start mod:tests \"fn a() {}\""),
            "mod tests {\n    fn a() {}\n\n    fn b() {}\n}\n"
        );
    }

    #[test]
    fn empty_bodies_open_onto_separate_lines() {
        let f = |script| edited(ITEMS, script);
        assert_eq!(
            f("insert end fn:f \"a();\\nb();\""),
            ITEMS.replace("fn f() {}", "fn f() {\n        a();\n        b();\n    }")
        );
        assert_eq!(
            f("replace fn:f.body with \"a();\""),
            ITEMS.replace("fn f() {}", "fn f() {\n        a();\n    }")
        );
        assert_eq!(
            f("insert start fn:g \"a();\""),
            ITEMS.replace("fn g() {\n    }", "fn g() {\n        a();\n    }")
        );
        assert_eq!(
            edited("fn f() {  }\n", "insert end fn:f <<'END'\n  raw\nEND"),
            "fn f() {\n  raw\n}\n"
        );
    }

    #[test]
    fn inline_parts_take_verbatim_text() {
        assert_eq!(
            edited(ITEMS, "replace fn:h.body with \"y()\""),
            ITEMS.replace("{ x() }", "{ y() }")
        );
        assert_eq!(
            edited(ITEMS, "replace fn:h.params with \"a: u8\""),
            ITEMS.replace("fn h()", "fn h(a: u8)")
        );
        assert_eq!(
            edited(ITEMS, "replace fn:h.name with \"k\""),
            ITEMS.replace("fn h()", "fn k()")
        );
    }

    #[test]
    fn replacing_a_whole_line_body_rebases() {
        assert_eq!(
            edited(
                ITEMS,
                "replace fn:main.body with <<END\nif y {\n    z();\n}\nEND"
            ),
            ITEMS.replace("    let x = 1;\n", "    if y {\n        z();\n    }\n")
        );
    }

    #[test]
    fn outline_prints_one_header_per_file() {
        let out = exec_with(
            &[
                ("a.rs", "fn a() {}\nfn b() {}\n"),
                ("b.rs", "struct S;\n"),
                ("c.txt", "x\n"),
            ],
            3,
            "outline",
        );
        assert_eq!(out.output, "a.rs\n1 fn:a\n2 fn:b\nb.rs\n1 struct:S\n");
        let out = exec("fn a() {}\nfn b() {}\n", "outline all fn:*");
        assert_eq!(out.output, "a.rs\n");
        let out = exec(ITEMS, "outline fn:main");
        assert_eq!(out.output, "a.rs\n11 var:x\n");
    }

    #[test]
    fn outline_needs_a_language() {
        let out = exec_with(&[("a.txt", "x\n")], 1, "outline");
        assert_eq!(
            out.error(),
            "error: script:1:1: outline needs a language, but a.txt has none; use --lang"
        );
    }

    /// Language servers with canned diagnostics, by file name; files not
    /// listed have a server and no diagnostics.
    #[derive(Default)]
    struct FakeLsp {
        show: Option<Severity>,
        files: HashMap<&'static str, Option<Vec<Diagnostic>>>,
        failure: Option<&'static str>,
        asked: Vec<Document>,
        /// `saved`, for each `diagnose`.
        saved: Vec<bool>,
        notes: Vec<&'static str>,
    }

    impl Lsp for FakeLsp {
        fn diagnose(
            &mut self,
            documents: &[Document],
            saved: bool,
        ) -> Result<Diagnosis, LspFailure> {
            self.asked.extend_from_slice(documents);
            self.saved.push(saved);
            if let Some(failure) = self.failure {
                return Err(LspFailure(failure.into()));
            }
            let files = documents
                .iter()
                .map(|d| {
                    let name = d.path.file_name().unwrap().to_str().unwrap();
                    self.files.get(name).cloned().unwrap_or(Some(Vec::new()))
                })
                .collect();
            Ok(Diagnosis {
                show: self.show.unwrap_or(Severity::Warning),
                block: Some(Severity::Error),
                files,
                notes: self.notes.iter().map(|n| n.to_string()).collect(),
            })
        }

        fn sync(&mut self, documents: &[Document]) -> Result<(), LspFailure> {
            self.asked.extend_from_slice(documents);
            Ok(())
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

    fn diag(line: u32, character: u32, severity: Severity, message: &str) -> Diagnostic {
        Diagnostic {
            start: Position { line, character },
            end: Position {
                line,
                character: character + 1,
            },
            severity,
            message: message.into(),
            source: Some("rust-analyzer".into()),
            code: None,
        }
    }

    const CHECKED: &str = "fn a() {\n    let x = 1;\n}\n\nfn b() {\n    oops();\n}\n";

    fn checked(files: &[(&str, &str)], script: &str, lsp: &mut FakeLsp) -> Outcome {
        exec_with_lsp(files, files.len(), script, &Options::default(), Some(lsp))
    }

    #[test]
    fn check_prints_diagnostics_by_file_then_position() {
        let mut oops = diag(5, 4, Severity::Error, "cannot find function `oops`");
        oops.source = Some("rustc".into());
        oops.code = Some("E0425".into());
        let mut lsp = FakeLsp::default();
        lsp.files.insert(
            "a.rs",
            Some(vec![
                oops,
                diag(1, 8, Severity::Warning, "unused variable: `x`"),
            ]),
        );
        lsp.files.insert(
            "b.rs",
            Some(vec![diag(0, 0, Severity::Error, "expected item")]),
        );
        let out = checked(&[("a.rs", CHECKED), ("b.rs", "}\n")], "check", &mut lsp);
        assert_eq!(
            out.output,
            "a.rs:2:9: warning: unused variable: `x` [rust-analyzer]\n\
         a.rs:6:5: error: cannot find function `oops` [rustc E0425]\n\
         b.rs:1:1: error: expected item [rust-analyzer]\n"
        );
        assert_eq!(out.result, Ok(vec![]));
        let asked: Vec<_> = lsp
            .asked
            .iter()
            .map(|d| (d.lang, d.text.as_str()))
            .collect();
        assert_eq!(asked, [(Language::Rust, CHECKED), (Language::Rust, "}\n")]);
        assert!(lsp.asked.iter().all(|d| d.path.is_absolute()));
    }

    #[test]
    fn check_filters_by_level() {
        let mut lsp = FakeLsp::default();
        lsp.files.insert(
            "a.rs",
            Some(vec![
                diag(1, 8, Severity::Warning, "unused"),
                diag(5, 4, Severity::Hint, "consider"),
                diag(0, 3, Severity::Info, "fyi"),
            ]),
        );
        let files = [("a.rs", CHECKED)];
        assert_eq!(
            checked(&files, "check", &mut lsp).output,
            "a.rs:2:9: warning: unused [rust-analyzer]\n"
        );
        assert_eq!(
            checked(&files, "check error", &mut lsp).output,
            "no diagnostics at error or above\n"
        );
        assert_eq!(
            checked(&files, "check hint", &mut lsp)
                .output
                .lines()
                .count(),
            3
        );
        lsp.show = Some(Severity::Info);
        assert_eq!(checked(&files, "check", &mut lsp).output.lines().count(), 2);
    }

    #[test]
    fn check_with_a_selector_shows_overlapping_diagnostics() {
        let mut lsp = FakeLsp::default();
        lsp.files.insert(
            "a.rs",
            Some(vec![
                diag(1, 8, Severity::Warning, "unused"),
                diag(5, 4, Severity::Error, "oops"),
            ]),
        );
        let out = checked(&[("a.rs", CHECKED)], "check fn:b", &mut lsp);
        assert_eq!(out.output, "a.rs:6:5: error: oops [rust-analyzer]\n");
    }

    #[test]
    fn check_columns_count_characters() {
        let mut lsp = FakeLsp::default();
        // `bad` starts at character 13, UTF-16 unit 14.
        lsp.files
            .insert("a.rs", Some(vec![diag(0, 14, Severity::Error, "bad")]));
        let out = checked(&[("a.rs", "let s = \"😀\"; bad\n")], "check", &mut lsp);
        assert_eq!(out.output, "a.rs:1:14: error: bad [rust-analyzer]\n");
    }

    #[test]
    fn check_indents_further_lines_of_a_message() {
        let mut lsp = FakeLsp::default();
        let mut d = diag(
            0,
            0,
            Severity::Error,
            "mismatched types\nexpected u8\nfound &str",
        );
        d.source = None;
        lsp.files.insert("a.rs", Some(vec![d]));
        let out = checked(&[("a.rs", CHECKED)], "check", &mut lsp);
        assert_eq!(
            out.output,
            "a.rs:1:1: error: mismatched types\n  expected u8\n  found &str\n"
        );
    }

    #[test]
    fn check_without_diagnostics_says_so() {
        let out = checked(&[("a.rs", CHECKED)], "check", &mut FakeLsp::default());
        assert_eq!(out.output, "no diagnostics at warning or above\n");
    }

    #[test]
    fn check_skips_files_without_a_server() {
        let mut lsp = FakeLsp::default();
        lsp.files.insert("a.md", None);
        lsp.files
            .insert("a.rs", Some(vec![diag(1, 8, Severity::Warning, "unused")]));
        let files = [("a.md", "# A\n"), ("a.rs", CHECKED), ("a.txt", "text\n")];
        let out = checked(&files, "check", &mut lsp);
        assert_eq!(out.output, "a.rs:2:9: warning: unused [rust-analyzer]\n");
        let asked: Vec<_> = lsp.asked.iter().map(|d| d.lang).collect();
        assert_eq!(asked, [Language::Markdown, Language::Rust]);
    }

    #[test]
    fn check_needs_a_server() {
        let mut lsp = FakeLsp::default();
        lsp.files.insert("a.md", None);
        let out = checked(&[("a.md", "# A\n")], "check", &mut lsp);
        assert_eq!(
            out.error(),
            "error: script:1:1: no language server for markdown; set one with `[lsp] markdown = [\"PROGRAM\", ...]` in .ned.toml"
        );
        let out = checked(&[("a.txt", "text\n")], "check", &mut FakeLsp::default());
        assert!(
            out.error().contains("check needs a language"),
            "{}",
            out.error()
        );
    }

    #[test]
    fn check_reports_server_failures() {
        let mut lsp = FakeLsp {
            failure: Some("rust-analyzer didn't answer; rerun in a few seconds"),
            ..FakeLsp::default()
        };
        let out = checked(&[("a.rs", CHECKED)], "check", &mut lsp);
        assert_eq!(
            out.error(),
            "error: script:1:1: rust-analyzer didn't answer; rerun in a few seconds"
        );
    }

    #[test]
    fn check_asks_for_save_time_checks_and_passes_on_notes() {
        let mut lsp = FakeLsp {
            notes: vec![
                "rust-analyzer's check on save didn't finish within 30s; raise [lsp] timeout",
            ],
            ..FakeLsp::default()
        };
        let out = checked(&[("a.rs", CHECKED)], "check", &mut lsp);
        assert_eq!(out.result, Ok(vec![]));
        assert_eq!(lsp.saved, [true]);
        assert_eq!(
            out.notes,
            ["rust-analyzer's check on save didn't finish within 30s; raise [lsp] timeout"]
        );
    }

    #[test]
    fn check_without_language_servers_is_an_error() {
        let out = exec(CHECKED, "check");
        assert!(out.error().contains("Unix-only"), "{}", out.error());
    }

    #[test]
    fn allow_records_the_most_permissive_level() {
        let src = "allow warnings\nallow errors\nallow warnings";
        let out = run(
            &parse(src).unwrap(),
            src,
            Initial::Files(&[]),
            &Options::default(),
            None,
        );
        assert_eq!(out.result.unwrap(), vec![]);
        assert_eq!(out.allow, Some(Severity::Error));
        let out = run(
            &parse("show 1").unwrap(),
            "show 1",
            Initial::Files(&[]),
            &Options::default(),
            None,
        );
        assert_eq!(out.allow, None);
    }

    /// Runs `script` on the workspace at `root`, with `root/` removed from
    /// every path.
    fn in_workspace(root: &std::path::Path, script: &str) -> Outcome {
        let prefix = format!("{}/", root.display());
        let src = script.replace("{dir}/", &prefix);
        let parsed = parse(&src).unwrap();
        let run = run(
            &parsed,
            &src,
            Initial::Workspace(root.to_path_buf()),
            &Options::default(),
            None,
        );
        let strip = |s: &str| s.replace(&prefix, "");
        Outcome {
            output: strip(&run.output),
            result: match run.result {
                Ok(changes) => Ok(changes
                    .into_iter()
                    .map(|c| Change {
                        path: strip(&c.path),
                        ..c
                    })
                    .collect()),
                Err(err) => Err(strip(&err.render(&src))),
            },
            notes: run.notes,
        }
    }

    fn workspace_tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("a.rs"), "fn a() {}\n").unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("sub/b.rs"), "fn b() {}\n").unwrap();
        fs::write(root.join("bin.dat"), [0xff, 0xfe, b'\n']).unwrap();
        fs::write(root.join(".gitignore"), "ignored.rs\n").unwrap();
        fs::write(root.join("ignored.rs"), "fn a() {}\n").unwrap();
        dir
    }

    #[test]
    fn a_workspace_set_holds_every_workspace_file() {
        let dir = workspace_tree();
        let root = dir.path().canonicalize().unwrap();
        let out = in_workspace(&root, "show fn:a\nshow all /fn /");
        assert_eq!(
            out.output,
            "a.rs:1\n1:fn a() {}\na.rs:1\n1:fn a() {}\nsub/b.rs:1\n1:fn b() {}\n"
        );
        assert_eq!(out.result, Ok(vec![]));
    }

    #[test]
    fn file_narrows_a_workspace_set() {
        let dir = workspace_tree();
        let root = dir.path().canonicalize().unwrap();
        let out = in_workspace(&root, "file {dir}/sub/b.rs\nshow all /fn /");
        assert_eq!(out.output, "sub/b.rs:1\n1:fn b() {}\n");
    }

    #[test]
    fn edits_in_a_workspace_change_only_their_files() {
        let dir = workspace_tree();
        let root = dir.path().canonicalize().unwrap();
        let out = in_workspace(&root, "replace fn:b.name with \"c\"");
        let changes = out.result.unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(
            (changes[0].path.as_str(), changes[0].new.as_str()),
            ("sub/b.rs", "fn c() {}\n")
        );
    }

    #[test]
    fn an_empty_workspace_has_no_files() {
        let dir = tempfile::tempdir().unwrap();
        let out = in_workspace(&dir.path().canonicalize().unwrap(), "show 1");
        assert!(out.error().contains("no files to edit"), "{}", out.error());
    }

    #[test]
    fn files_are_read_only_when_a_command_needs_them() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.rs");
        let bad = dir.path().join("bad.rs");
        fs::write(&good, "fn a() {}\n").unwrap();
        fs::write(&bad, [0xff, 0xfe, b'\n']).unwrap();
        let paths = [good.display().to_string(), bad.display().to_string()];
        let src = format!("show file:{}>1", paths[0]);
        let out = run(
            &parse(&src).unwrap(),
            &src,
            Initial::Files(&paths),
            &Options::default(),
            None,
        );
        assert!(out.result.is_ok(), "{:?}", out.result);
        assert!(out.output.ends_with("1:fn a() {}\n"), "{}", out.output);
        let out = run(
            &parse("show 1").unwrap(),
            "show 1",
            Initial::Files(&paths),
            &Options::default(),
            None,
        );
        let err = out.result.unwrap_err().render("show 1");
        assert!(err.contains("not valid UTF-8"), "{err}");
    }

    #[test]
    fn errors_list_at_most_five_files() {
        let names = ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs", "f.rs", "g.rs"];
        let files: Vec<(&str, &str)> = names.iter().map(|n| (*n, "x\n")).collect();
        let out = exec_with(&files, files.len(), "show fn:missing");
        assert!(
            out.error()
                .contains("matches nothing in a.rs, b.rs, c.rs, d.rs, e.rs and 2 more"),
            "{}",
            out.error()
        );
        let out = exec_with(&files, files.len(), "show file:z.rs>1");
        assert!(
        out.error().contains("is not in the file set: a.rs, b.rs, c.rs, d.rs, e.rs and 2 more; add it with `file`"),
        "{}",
        out.error()
    );
    }

    #[test]
    fn syntax_steps_skip_files_whose_language_lacks_the_kind() {
        let files = [
            ("a.rs", "fn a() {}\n"),
            ("b.md", "# B\n\ntext\n"),
            ("c.json", "{}\n"),
        ];
        let out = exec_with(&files, 3, "show fn:a\nshow section:B");
        assert_eq!(
            out.output,
            "a.rs:1\n1:fn a() {}\nb.md:1-3\n1:# B\n2:\n3:text\n"
        );
        let out = exec_with(&files, 3, "outline");
        assert!(out.output.starts_with("a.rs\n"), "{}", out.output);
        assert!(out.output.contains("b.md\n"), "{}", out.output);
        assert!(!out.output.contains("c.json"), "{}", out.output);
        let out = exec_with(&files[1..], 2, "show fn:a");
        assert!(
            out.error().contains("markdown has no `fn` items"),
            "{}",
            out.error()
        );
    }

    /// A language server that answers renames with `answer`, and `.refs` and
    /// `.def` with `refs` and `def`, whose relative paths are under `root`; it
    /// records what it was asked.
    struct ServerLsp {
        answer: Renamed,
        root: PathBuf,
        failure: Option<&'static str>,
        asked: Vec<(Document, Position, String)>,
        refs: Vec<(&'static str, TextEdit)>,
        def: Vec<(&'static str, TextEdit)>,
        no_server: bool,
        located: Vec<(Locate, Position)>,
    }

    impl ServerLsp {
        fn new(edits: Vec<(&str, Vec<TextEdit>)>) -> ServerLsp {
            let edits = edits
                .into_iter()
                .map(|(path, edits)| FileEdits {
                    path: path.into(),
                    edits,
                })
                .collect();
            ServerLsp {
                answer: Renamed::Edits(edits),
                root: PathBuf::new(),
                failure: None,
                asked: Vec::new(),
                refs: Vec::new(),
                def: Vec::new(),
                no_server: false,
                located: Vec::new(),
            }
        }
    }

    impl Lsp for ServerLsp {
        fn diagnose(&mut self, _: &[Document], _: bool) -> Result<Diagnosis, LspFailure> {
            unreachable!("renames aren't checked here")
        }

        fn sync(&mut self, _: &[Document]) -> Result<(), LspFailure> {
            unreachable!("renames aren't checked here")
        }

        fn rename(
            &mut self,
            document: &Document,
            position: Position,
            name: &str,
        ) -> Result<Renamed, LspFailure> {
            self.asked.push((document.clone(), position, name.into()));
            if let Some(failure) = self.failure {
                return Err(LspFailure(failure.into()));
            }
            Ok(match &self.answer {
                Renamed::Edits(files) => Renamed::Edits(
                    files
                        .iter()
                        .map(|f| FileEdits {
                            path: self.root.join(&f.path),
                            edits: f.edits.clone(),
                        })
                        .collect(),
                ),
                other => other.clone(),
            })
        }

        fn locate(
            &mut self,
            kind: Locate,
            _: &Document,
            position: Position,
        ) -> Result<Located, LspFailure> {
            self.located.push((kind, position));
            if let Some(failure) = self.failure {
                return Err(LspFailure(failure.into()));
            }
            if self.no_server {
                return Ok(Located::NoServer);
            }
            let found = match kind {
                Locate::References => &self.refs,
                Locate::Definition => &self.def,
            };
            let locations = found
                .iter()
                .map(|(path, e)| Location {
                    path: self.root.join(path),
                    start: e.start,
                    end: e.end,
                })
                .collect();
            Ok(Located::Locations(locations))
        }

        fn format(&mut self, _: &Document) -> Result<Formatting, LspFailure> {
            unreachable!("not formatted in these tests")
        }
    }

    /// Runs `script` on `files` with `lsp`: the first `set` of them as FILE
    /// arguments, or the whole directory as a workspace (`-w`) for `None`. ned
    /// sees the directory's canonical path and the server its given one, as a
    /// symlinked temporary directory can make them differ.
    fn served(
        files: &[(&str, &str)],
        set: Option<usize>,
        script: &str,
        lsp: &mut ServerLsp,
    ) -> Outcome {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in files {
            fs::write(dir.path().join(name), text).unwrap();
        }
        let root = dir.path().canonicalize().unwrap();
        lsp.root = dir.path().to_path_buf();
        let prefix = format!("{}/", root.display());
        let paths: Vec<String> = files[..set.unwrap_or(0)]
            .iter()
            .map(|(name, _)| format!("{prefix}{name}"))
            .collect();
        let initial = match set {
            Some(_) => Initial::Files(&paths),
            None => Initial::Workspace(root.clone()),
        };
        let src = script.replace("{dir}/", &prefix);
        let parsed = parse(&src).unwrap();
        let run = run(&parsed, &src, initial, &Options::default(), Some(lsp));
        let given = format!("{}/", dir.path().display());
        let strip = |s: &str| s.replace(&prefix, "").replace(&given, "");
        Outcome {
            output: strip(&run.output),
            result: match run.result {
                Ok(changes) => Ok(changes
                    .into_iter()
                    .map(|c| Change {
                        path: strip(&c.path),
                        ..c
                    })
                    .collect()),
                Err(err) => Err(strip(&err.render(&src))),
            },
            notes: run.notes,
        }
    }

    fn edit(line: u32, start: u32, end: u32, text: &str) -> TextEdit {
        TextEdit {
            start: Position {
                line,
                character: start,
            },
            end: Position {
                line,
                character: end,
            },
            text: text.into(),
        }
    }

    const FOO_A: &str = "fn foo() {}\nfn main() {\n    foo();\n}\n";
    const FOO_B: &str = "fn g() {\n    crate::foo();\n}\n";

    fn foo_edits() -> Vec<(&'static str, Vec<TextEdit>)> {
        vec![
            ("a.rs", vec![edit(0, 3, 6, "bar"), edit(2, 4, 7, "bar")]),
            ("b.rs", vec![edit(1, 11, 14, "bar")]),
        ]
    }

    fn paths(out: &Outcome) -> Vec<String> {
        let changes = out.result.as_ref().unwrap();
        changes.iter().map(|c| c.path.clone()).collect()
    }

    #[test]
    fn rename_applies_the_servers_edits_across_files() {
        let mut lsp = ServerLsp::new(foo_edits());
        let files = [("a.rs", FOO_A), ("b.rs", FOO_B)];
        let out = served(&files, Some(2), "rename fn:foo to bar", &mut lsp);
        let changes = out.result.unwrap();
        let summary: Vec<(&str, &str, usize)> = changes
            .iter()
            .map(|c| (c.path.as_str(), c.new.as_str(), c.edits))
            .collect();
        assert_eq!(
            summary,
            [
                ("a.rs", "fn bar() {}\nfn main() {\n    bar();\n}\n", 2),
                ("b.rs", "fn g() {\n    crate::bar();\n}\n", 1),
            ]
        );
        let (document, position, name) = &lsp.asked[0];
        assert_eq!(document.text, FOO_A);
        assert!(document.path.is_absolute() && document.path.ends_with("a.rs"));
        assert_eq!(
            *position,
            Position {
                line: 0,
                character: 3
            }
        );
        assert_eq!(name, "bar");
    }

    #[test]
    fn other_selectors_rename_at_their_start_in_utf16_units() {
        let mut lsp = ServerLsp::new(vec![("c.rs", vec![edit(0, 8, 11, "bar")])]);
        let files = [("c.rs", "/* é */ foo();\n")];
        let out = served(&files, Some(1), r#"rename "foo" to bar"#, &mut lsp);
        assert_eq!(out.new_text(), "/* é */ bar();\n");
        assert_eq!(
            lsp.asked[0].1,
            Position {
                line: 0,
                character: 8
            }
        );
    }

    #[test]
    fn rename_combines_with_the_scripts_other_edits() {
        let files = [("a.rs", FOO_A), ("b.rs", FOO_B)];
        let mut lsp = ServerLsp::new(foo_edits());
        let script = "rename fn:foo to bar\ninsert before fn:main \"// entry\"";
        let out = served(&files, Some(2), script, &mut lsp);
        let changes = out.result.unwrap();
        assert!(
            changes[0].new.contains("// entry\nfn main() {\n    bar();"),
            "{changes:?}"
        );
        let mut lsp = ServerLsp::new(foo_edits());
        let script = "replace fn:main with \"fn main() {}\"\nrename fn:foo to bar";
        let out = served(&files, Some(2), script, &mut lsp);
        assert!(
            out.error().contains("edit overlaps command 1 at a.rs:"),
            "{}",
            out.error()
        );
    }

    #[test]
    fn rename_needs_one_span() {
        let mut lsp = ServerLsp::new(foo_edits());
        let out = served(&[("a.rs", FOO_A)], Some(1), "rename /foo/ to bar", &mut lsp);
        assert!(out.error().contains("/foo/ matches 2"), "{}", out.error());
        assert!(lsp.asked.is_empty());
    }

    #[test]
    fn rename_outside_the_file_set_is_an_error() {
        let files = [("a.rs", FOO_A), ("b.rs", FOO_B)];
        let mut lsp = ServerLsp::new(foo_edits());
        let out = served(&files, Some(1), "rename fn:foo to bar", &mut lsp);
        assert_eq!(
            out.error(),
            "error: script:1:1: rename reaches files outside the file set: b.rs; add them to the file set, or use -w"
        );
        let mut edits = vec![("a.rs", vec![edit(0, 3, 6, "bar")])];
        edits.extend(
            ["b.rs", "c.rs", "d.rs", "e.rs", "f.rs", "g.rs"].map(|p| (p, vec![edit(0, 0, 1, "x")])),
        );
        let out = served(
            &files,
            Some(1),
            "rename fn:foo to bar",
            &mut ServerLsp::new(edits),
        );
        assert!(
            out.error()
                .contains("file set: b.rs, c.rs, d.rs, e.rs, f.rs and 1 more; "),
            "{}",
            out.error()
        );
    }

    #[test]
    fn with_w_rename_reaches_every_workspace_file() {
        let files = [("a.rs", FOO_A), ("b.rs", FOO_B)];
        let mut lsp = ServerLsp::new(foo_edits());
        let out = served(
            &files,
            None,
            "file {dir}/a.rs\nrename fn:foo to bar",
            &mut lsp,
        );
        assert_eq!(paths(&out), ["a.rs", "b.rs"]);
    }

    #[test]
    fn with_w_rename_stops_at_ignored_files_and_the_root() {
        let files = [
            ("a.rs", FOO_A),
            (".gitignore", "gen.rs\n"),
            ("gen.rs", "fn foo() {}\n"),
        ];
        let mut lsp = ServerLsp::new(vec![
            ("a.rs", vec![edit(0, 3, 6, "bar")]),
            ("gen.rs", vec![edit(0, 3, 6, "bar")]),
            ("/elsewhere/x.rs", vec![edit(0, 0, 1, "y")]),
        ]);
        let out = served(&files, None, "rename fn:foo to bar", &mut lsp);
        assert_eq!(
            out.error(),
            "error: script:1:1: rename reaches files outside the workspace: gen.rs, /elsewhere/x.rs; these are ignored or outside the root; use a regex there instead"
        );
    }

    #[test]
    fn a_refused_rename_says_where_and_why() {
        let mut lsp = ServerLsp::new(vec![]);
        lsp.answer = Renamed::Refused("fake can't rename a keyword; select the name itself".into());
        let out = served(
            &[("a.rs", FOO_A)],
            Some(1),
            r#"rename 3>"foo" to bar"#,
            &mut lsp,
        );
        assert_eq!(
            out.error(),
            "error: script:1:1: cannot rename at a.rs:3:5: fake can't rename a keyword; select the name itself"
        );
    }

    #[test]
    fn rename_needs_a_daemon_a_server_and_a_language() {
        let mut lsp = ServerLsp::new(vec![]);
        lsp.failure = Some("fake exited; check that it runs, then rerun");
        let out = served(
            &[("a.rs", FOO_A)],
            Some(1),
            "rename fn:foo to bar",
            &mut lsp,
        );
        assert_eq!(
            out.error(),
            "error: script:1:1: fake exited; check that it runs, then rerun"
        );

        let mut lsp = ServerLsp::new(vec![]);
        lsp.answer = Renamed::NoServer;
        let out = served(
            &[("a.md", "# A\n")],
            Some(1),
            "rename section:A to B",
            &mut lsp,
        );
        assert_eq!(
            out.error(),
            "error: script:1:1: no language server for markdown; set one with `[lsp] markdown = [\"PROGRAM\", ...]` in .ned.toml"
        );

        let mut lsp = ServerLsp::new(vec![]);
        let out = served(
            &[("a.txt", "foo\n")],
            Some(1),
            r#"rename "foo" to bar"#,
            &mut lsp,
        );
        assert!(
            out.error().contains("rename needs a language"),
            "{}",
            out.error()
        );
        assert!(lsp.asked.is_empty());

        let out = exec_with(&[("a.rs", FOO_A)], 1, "rename fn:foo to bar");
        assert_eq!(
            out.error(),
            r#"error: script:1:1: `rename` needs the language-server daemon, which is Unix-only for now; use sub /\bOLD\b/ with "NEW" over the files"#
        );
    }

    /// `foo`'s uses and definition in `FOO_A` and `FOO_B`.
    fn foo_server() -> ServerLsp {
        let mut lsp = ServerLsp::new(vec![]);
        lsp.refs = vec![("a.rs", edit(2, 4, 7, "")), ("b.rs", edit(1, 11, 14, ""))];
        lsp.def = vec![("a.rs", edit(0, 3, 6, ""))];
        lsp
    }

    const FOO_FILES: [(&str, &str); 2] = [("a.rs", FOO_A), ("b.rs", FOO_B)];

    #[test]
    fn refs_select_each_use_across_files() {
        let mut lsp = foo_server();
        let out = served(&FOO_FILES, Some(2), "show all fn:foo.refs", &mut lsp);
        assert_eq!(
            out.output,
            "a.rs:3\n3:    foo();\nb.rs:2\n2:    crate::foo();\n"
        );
        assert_eq!(
            lsp.located,
            [(
                Locate::References,
                Position {
                    line: 0,
                    character: 3
                }
            )]
        );
        let out = served(
            &FOO_FILES,
            Some(2),
            "replace all fn:foo.refs with \"bar\"",
            &mut foo_server(),
        );
        let changes = out.result.unwrap();
        assert_eq!(changes[0].new, "fn foo() {}\nfn main() {\n    bar();\n}\n");
        assert_eq!(changes[1].new, "fn g() {\n    crate::bar();\n}\n");
    }

    #[test]
    fn several_refs_need_all_and_one_does_not() {
        let out = served(&FOO_FILES, Some(2), "show fn:foo.refs", &mut foo_server());
        assert_eq!(
            out.error(),
            "error: script:1:6: fn:foo.refs matches 2 spans, at a.rs:3, b.rs:2; add `all` to take every one"
        );
        let mut lsp = foo_server();
        lsp.refs.pop();
        let out = served(&FOO_FILES, Some(2), "show fn:foo.refs", &mut lsp);
        assert_eq!(out.output, "a.rs:3\n3:    foo();\n");
        let mut lsp = foo_server();
        lsp.refs.clear();
        let out = served(&FOO_FILES, Some(2), "show fn:foo.refs", &mut lsp);
        assert!(
            out.error().contains("fn:foo.refs matches nothing"),
            "{}",
            out.error()
        );
    }

    #[test]
    fn def_selects_the_defining_item_or_else_the_identifier() {
        let mut lsp = foo_server();
        let out = served(&FOO_FILES, Some(2), r#"show 3>"foo".def"#, &mut lsp);
        assert_eq!(out.output, "a.rs:1\n1:fn foo() {}\n");
        assert_eq!(
            lsp.located,
            [(
                Locate::Definition,
                Position {
                    line: 2,
                    character: 4
                }
            )]
        );
        let mut lsp = ServerLsp::new(vec![]);
        lsp.def = vec![("c.rs", edit(0, 5, 6, ""))];
        let files = [("c.rs", "fn m(x: u8) -> u8 {\n    x\n}\n")];
        let out = served(&files, Some(1), r#"replace 2>"x".def with "y""#, &mut lsp);
        assert_eq!(out.new_text(), "fn m(y: u8) -> u8 {\n    x\n}\n");
    }

    #[test]
    fn lines_after_def_select_each_line_of_the_item() {
        let mut lsp = ServerLsp::new(vec![]);
        lsp.def = vec![("c.rs", edit(0, 3, 4, ""))];
        let files = [("c.rs", "fn m() {\n    m();\n}\n")];
        let out = served(&files, Some(1), r#"delete all 2>"m(".def.lines"#, &mut lsp);
        assert_eq!(out.new_text(), "");
        let mut lsp = ServerLsp::new(vec![]);
        lsp.def = vec![("c.rs", edit(0, 3, 4, ""))];
        let out = served(&files, Some(1), r#"delete 2>"m(".def.lines"#, &mut lsp);
        assert!(out.error().contains("matches 3"), "{}", out.error());
    }

    #[test]
    fn parts_and_steps_after_refs_apply_to_each_use() {
        let script = "delete all fn:foo.refs.lines";
        let out = served(&FOO_FILES, Some(2), script, &mut foo_server());
        let changes = out.result.unwrap();
        assert_eq!(changes[0].new, "fn foo() {}\nfn main() {\n}\n");
        assert_eq!(changes[1].new, "fn g() {\n}\n");
        let script = "replace all fn:foo.refs>/o+/ with \"0\"";
        let out = served(&FOO_FILES, Some(2), script, &mut foo_server());
        let changes = out.result.unwrap();
        assert!(changes[1].new.contains("crate::f0();"), "{changes:?}");
    }

    #[test]
    fn filters_after_refs_test_each_use() {
        let script = "delete all fn:foo.refs.lines[.text ~= /crate/]";
        let out = served(&FOO_FILES, Some(2), script, &mut foo_server());
        let changes = out.result.unwrap();
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert_eq!(changes[0].new, "fn g() {\n}\n");
    }

    #[test]
    fn deleting_filtered_items() {
        let text = "fn test_a() {}\n\nfn main() {}\n\nfn test_b() {}\n";
        assert_eq!(
            edited(text, "delete all fn[.name ~= /^test_/]"),
            "fn main() {}\n"
        );
    }

    #[test]
    fn refs_reach_only_the_file_set_or_workspace() {
        let out = served(
            &FOO_FILES,
            Some(1),
            "show all fn:foo.refs",
            &mut foo_server(),
        );
        assert_eq!(
            out.error(),
            "error: script:1:10: fn:foo.refs reaches files outside the file set: b.rs; add them to the file set, or use -w"
        );
        let script = "file {dir}/a.rs\nshow all fn:foo.refs";
        let out = served(&FOO_FILES, None, script, &mut foo_server());
        assert_eq!(
            out.output,
            "a.rs:3\n3:    foo();\nb.rs:2\n2:    crate::foo();\n"
        );
    }

    #[test]
    fn refs_need_a_daemon_a_server_and_a_language() {
        let out = exec_with(&FOO_FILES, 2, "show all fn:foo.refs");
        assert_eq!(
            out.error(),
            "error: script:1:10: `.refs` and `.def` need the language-server daemon, which is Unix-only for now; select with a /regex/ or kind:NAME instead"
        );
        let mut lsp = foo_server();
        lsp.failure = Some("fake exited; check that it runs, then rerun");
        let out = served(&FOO_FILES, Some(2), "show all fn:foo.refs", &mut lsp);
        assert_eq!(
            out.error(),
            "error: script:1:10: fake exited; check that it runs, then rerun"
        );
        let mut lsp = foo_server();
        lsp.no_server = true;
        let out = served(
            &[("a.md", "# A\n")],
            Some(1),
            "show section:A.def",
            &mut lsp,
        );
        assert_eq!(
            out.error(),
            "error: script:1:6: no language server for markdown; set one with `[lsp] markdown = [\"PROGRAM\", ...]` in .ned.toml"
        );
        let out = served(
            &[("a.txt", "foo\n")],
            Some(1),
            r#"show "foo".refs"#,
            &mut foo_server(),
        );
        assert_eq!(
            out.error(),
            "error: script:1:6: \"foo\".refs needs a language, but a.txt has none; use --lang"
        );
    }
}
