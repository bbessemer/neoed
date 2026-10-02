//! Resolving selectors to spans of files (command-language spec, §3).

use std::borrow::Cow;
use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

use regex::Regex;
use std::cmp::Reverse;

use tree_sitter::{Node, Query, QueryCursor, QueryError, QueryErrorKind, StreamingIterator, Tree};

use crate::buffer::{Buffer, LineEnding};
use crate::conflict::{self, Conflict};
use crate::exec::{Candidates, ExecError, ExecErrorKind as E};
use crate::lang::Language;
use crate::pattern;
use crate::script::ast::{
    LineNo, Part, Pattern, Primary, RegexFlags, Selector, Step, Target, TextKind,
};
use crate::span::{Of, Span};
use crate::syntax::{self, Item};
use crate::template::Template;
use crate::text::{self, full_lines, is_whole_line, strip_indent};

const MAX_CANDIDATES: usize = 10;

/// The most files an error lists by name.
pub(crate) const MAX_LISTED_FILES: usize = 5;

/// `paths` for an error message: at most `MAX_LISTED_FILES`, then a count.
pub(crate) fn file_list(paths: &[&str]) -> String {
    match paths.len().checked_sub(MAX_LISTED_FILES) {
        Some(more) if more > 0 => {
            format!("{} and {more} more", paths[..MAX_LISTED_FILES].join(", "))
        }
        _ => paths.join(", "),
    }
}

/// The fix for `path` not being in the set `paths`.
pub(crate) fn add_to_set(paths: &[&str], path: &str) -> String {
    if paths.len() <= MAX_LISTED_FILES {
        format!("add it with `file {} {path}`", paths.join(" "))
    } else {
        "add it with `file`, which replaces the set".into()
    }
}

/// A file in the file set, with its path as the user wrote it.
#[derive(Debug, Clone)]
pub struct SourceFile {
    pub path: String,
    pub text: String,
    pub buffer: Buffer,
    pub lang: Option<Language>,
    tree: OnceCell<Tree>,
    items: OnceCell<Vec<Item>>,
    conflicts: OnceCell<Vec<Conflict>>,
    /// The text with its conflicts' markers hidden, if it has any.
    masked: OnceCell<Option<String>>,
    indent_unit: OnceCell<String>,
}

impl SourceFile {
    pub fn new(path: impl Into<String>, text: String, lang: Option<Language>) -> Self {
        let buffer = Buffer::new(&text);
        SourceFile {
            path: path.into(),
            text,
            buffer,
            lang,
            tree: OnceCell::new(),
            items: OnceCell::new(),
            conflicts: OnceCell::new(),
            masked: OnceCell::new(),
            indent_unit: OnceCell::new(),
        }
    }

    /// The syntax items of the text; `None` without a language.
    pub fn items(&self) -> Option<&[Item]> {
        let query = self.lang?.selectors();
        let tree = self.tree()?;
        Some(
            self.items
                .get_or_init(|| syntax::items(query, tree, self.masked())),
        )
    }

    /// The text's merge conflicts (§3.11).
    pub fn conflicts(&self) -> &[Conflict] {
        self.conflicts
            .get_or_init(|| conflict::conflicts(&self.text))
    }

    /// The file's conflict, numbered from 1, that `range` is or is an empty side
    /// of.
    pub fn conflict_at(&self, range: &Range<usize>) -> Option<(usize, &Conflict)> {
        self.conflicts()
            .iter()
            .enumerate()
            .find(|(_, c)| c.span() == *range || (range.is_empty() && c.side_at(range).is_some()))
            .map(|(i, c)| (i + 1, c))
    }

    /// The text with its conflicts' marker lines hidden from the grammar (§3.3).
    fn masked(&self) -> &str {
        self.masked
            .get_or_init(|| match conflict::mask(&self.text, self.conflicts()) {
                Cow::Owned(masked) => Some(masked),
                Cow::Borrowed(_) => None,
            })
            .as_deref()
            .unwrap_or(&self.text)
    }

    /// The syntax tree of the text, parsed on first use; `None` without a
    /// language.
    pub fn tree(&self) -> Option<&Tree> {
        let lang = self.lang?;
        Some(self.tree.get_or_init(|| lang.parse_masked(self.masked())))
    }

    /// The indent unit of the text (§5.2), measured on lines that don't start
    /// inside a string or comment, or its language's default.
    pub fn indent_unit(&self) -> &str {
        self.indent_unit.get_or_init(|| {
            let default = self.lang.map_or("    ", Language::default_indent);
            let skipped = self
                .tree()
                .map(|t| text_rows(t.root_node()))
                .unwrap_or_default();
            let lines = self.text.lines().enumerate();
            let lines = lines.filter(|(row, _)| !skipped.get(*row).is_some_and(|&s| s));
            text::indent_unit(lines.map(|(_, line)| line), default)
        })
    }
}

/// Whether each row under `root` starts inside a string or comment, so its
/// indentation is text, not code.
fn text_rows(root: Node<'_>) -> Vec<bool> {
    let mut skipped = vec![false; root.end_position().row + 1];
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let (start, end) = (node.start_position().row, node.end_position().row);
        if start == end {
            continue;
        }
        let kind = node.kind();
        if kind.contains("string") || kind.contains("comment") {
            skipped[start + 1..=end].fill(true);
        } else {
            stack.extend(node.children(&mut node.walk()));
        }
    }
    skipped
}

/// A selected span of `files[file]`, with what its selector's pattern steps
/// captured (§3.10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub file: usize,
    pub range: Range<usize>,
    pub captures: Captures,
}

/// Each named placeholder and the range it matched.
pub type Captures = Vec<(String, Range<usize>)>;

/// Resolves `target` against every file in `files`, enforcing the ambiguity
/// rules of §3.5. `src` is the script, for error messages.
pub fn resolve(target: &Target, files: &[&SourceFile], src: &str) -> Result<Vec<Match>, ExecError> {
    let whole = files
        .iter()
        .enumerate()
        .map(|(file, f)| Match {
            file,
            range: 0..f.text.len(),
            captures: Vec::new(),
        })
        .collect();
    resolve_within(target, files, whole, src)
}

/// `resolve`, with the first step searching `start` instead of whole files.
/// `target` has no `.refs` or `.def`: the executor asks servers for those.
pub fn resolve_within(
    target: &Target,
    files: &[&SourceFile],
    start: Vec<Match>,
    src: &str,
) -> Result<Vec<Match>, ExecError> {
    let span = &target.selector.span;
    let error = |kind| ExecError::new(kind, Some(span.clone()));
    // A name may repeat within a pattern, but not across patterns: steps, or
    // a range's ends (§3.10).
    let mut named = HashSet::new();
    let patterns = target
        .selector
        .steps
        .iter()
        .flat_map(|s| s.primary.patterns());
    for template in patterns.map(Template::parse) {
        let names: HashSet<&str> = template.holes().filter_map(|h| h.name.as_deref()).collect();
        if let Some(name) = names.iter().find(|n| named.contains(**n)) {
            return Err(error(E::DuplicateCapture {
                name: name.to_string(),
            }));
        }
        named.extend(names.into_iter().map(String::from));
    }
    let mut matches = start;
    let mut parents = Vec::new();
    let mut found = Vec::new();
    for (i, step) in target.selector.steps.iter().enumerate() {
        found = resolve_step(step, files, &matches).map_err(error)?;
        let next = found.iter().map(|f| f.m.clone()).collect();
        parents = std::mem::replace(&mut matches, next);
        if matches.is_empty() {
            return Err(error(no_match(target, i, files, &parents, src)));
        }
    }
    let selector = &src[span.clone()];
    match matches.len() {
        1 => Ok(matches),
        _ if target.all => Ok(matches),
        _ => Err(error(E::Ambiguous {
            selector: selector.into(),
            candidates: candidates(&found, &parents, files, &target.selector, src),
        })),
    }
}

/// The error for `target`, whose step `failed` matched nothing within the
/// `parents` the steps before it found: it names the selector up to that step,
/// with the fix for it (§7).
fn no_match(
    target: &Target,
    failed: usize,
    files: &[&SourceFile],
    parents: &[Match],
    src: &str,
) -> E {
    let selector = &target.selector;
    let step = &selector.steps[failed];
    let whole = &src[selector.span.clone()];
    let (named, hint) = match range_precedence(selector, src) {
        Some(hint) => (whole, hint),
        None => (
            &src[selector.span.start..step.span.end],
            hint(
                step,
                files,
                parents,
                whole,
                step.span.start - selector.span.start,
            ),
        ),
    };
    let last = failed + 1 == selector.steps.len();
    E::NoMatch {
        selector: named.into(),
        files: file_list(&files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>()),
        hint,
        searched: last.then(|| searched(step, parents)).flatten(),
    }
}

/// For a search `step` (a regex, literal or heredoc) that matched nothing
/// within the `parents` the earlier steps found, the number of files searched.
fn searched(step: &Step, parents: &[Match]) -> Option<usize> {
    let search = matches!(step.primary, Primary::Regex(_) | Primary::Literal(_));
    if !search || parents.is_empty() {
        return None;
    }
    let mut files: Vec<usize> = parents.iter().map(|m| m.file).collect();
    files.dedup();
    Some(files.len())
}

/// A match of a step, with what candidates need to scope it.
struct Found {
    m: Match,
    /// The span before the step's parts: the item itself, for a syntax step.
    core: Range<usize>,
    /// The index of the parent match it lies in.
    parent: usize,
    /// Whether it's one of the lines `.lines` split a multi-line span into.
    line: bool,
}

fn resolve_step(step: &Step, files: &[&SourceFile], parents: &[Match]) -> Result<Vec<Found>, E> {
    // Only a syntax item has parts other than `.lines`, and a conflict its
    // sides; a part's span is neither.
    let mut primary = Some(&step.primary);
    for &part in &step.parts {
        let side = matches!(part, Part::Ours | Part::Theirs | Part::Base);
        match primary {
            _ if part == Part::Lines => {}
            Some(Primary::Conflict(_)) if side => {}
            Some(Primary::Syntax { .. }) if !side => {}
            _ if side => {
                return Err(E::PartNeedsConflict {
                    part: part_name(part).into(),
                });
            }
            _ => {
                return Err(E::PartNeedsItem {
                    part: part_name(part).into(),
                });
            }
        }
        primary = None;
    }
    let matcher = Matcher::new(&step.primary, files, parents)?;
    let mut out: Vec<Found> = Vec::new();
    for (p, parent) in parents.iter().enumerate() {
        let f = &files[parent.file];
        for (range, captured) in matcher.find(f, parent.range.clone()) {
            let mut captures = parent.captures.clone();
            captures.extend(captured);
            let core = range.clone();
            let of = match &step.primary {
                Primary::Syntax { kind, .. } => f
                    .items()
                    .unwrap_or_default()
                    .iter()
                    .find(|i| i.kind == kind && i.range == range)
                    .map_or(Of::Plain, Of::Item),
                Primary::Conflict(_) => f
                    .conflict_at(&range)
                    .map_or(Of::Plain, |(n, c)| Of::Conflict(n, c)),
                _ => Of::Plain,
            };
            let mut spans = vec![Span { range, of }];
            for part in &step.parts {
                spans = spans
                    .iter()
                    .map(|s| s.part(*part, &f.text))
                    .collect::<Result<Vec<_>, _>>()?
                    .concat();
            }
            let line = spans.len() > 1;
            for span in spans {
                if !span.passes(&step.filters, &f.text)? {
                    continue;
                }
                let m = Match {
                    file: parent.file,
                    range: span.range,
                    captures: captures.clone(),
                };
                if out.last().map(|f| &f.m) != Some(&m) {
                    out.push(Found {
                        m,
                        core: core.clone(),
                        parent: p,
                        line,
                    });
                }
            }
        }
    }
    Ok(out)
}

enum Matcher<'a> {
    Lines {
        start: LineNo,
        end: LineNo,
    },
    Regex(&'a Regex),
    Str(&'a str),
    Heredoc {
        lines: Vec<String>,
        raw: bool,
    },
    File(&'a str),
    /// `conflict:N`, or every conflict.
    Conflict(Option<usize>),
    Syntax {
        kind: &'a str,
        name: &'a str,
    },
    /// The query compiled for each searched language.
    Query(Vec<(Language, Query)>),
    /// The pattern compiled for each searched language it parses in.
    Code(Vec<(Language, pattern::Pattern)>),
    Range(Box<Matcher<'a>>, Box<Matcher<'a>>),
}

impl<'a> Matcher<'a> {
    fn new(primary: &'a Primary, files: &[&SourceFile], parents: &[Match]) -> Result<Self, E> {
        Ok(match primary {
            Primary::Lines { start, end } => {
                let end = end.unwrap_or(*start);
                check_lines(*start, end, files, parents)?;
                Matcher::Lines { start: *start, end }
            }
            Primary::Regex(pattern) => Matcher::Regex(pattern.regex()),
            Primary::Literal(text) => match text.kind {
                TextKind::Str => Matcher::Str(&text.value),
                TextKind::Heredoc => Matcher::Heredoc {
                    lines: strip_indent(&text.value),
                    raw: false,
                },
                TextKind::RawHeredoc => Matcher::Heredoc {
                    lines: text.value.split('\n').map(String::from).collect(),
                    raw: true,
                },
            },
            Primary::File(path) => {
                if !files.iter().any(|f| same_path(&f.path, path)) {
                    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
                    return Err(E::NotInFileSet {
                        path: path.clone(),
                        files: file_list(&paths),
                        add: add_to_set(&paths, path),
                    });
                }
                Matcher::File(path)
            }
            Primary::Conflict(n) => Matcher::Conflict(*n),
            Primary::Syntax { kind, name } => {
                check_syntax(kind, name, files, parents)?;
                Matcher::Syntax { kind, name }
            }
            Primary::Query(source) => Matcher::Query(compile_query(source, files, parents)?),
            Primary::Code(code) => Matcher::Code(compile_pattern(code, files, parents)?),
            Primary::Range { from, to } => Matcher::Range(
                Box::new(Matcher::new(from, files, parents)?),
                Box::new(Matcher::new(to, files, parents)?),
            ),
        })
    }

    /// The step's matches in `parent`, with what its patterns captured.
    fn find(&self, f: &SourceFile, parent: Range<usize>) -> Vec<(Range<usize>, Captures)> {
        match self {
            Matcher::Code(patterns) => {
                let (Some(lang), Some(tree)) = (f.lang, f.tree()) else {
                    return Vec::new();
                };
                let Some((_, pattern)) = patterns.iter().find(|(l, _)| *l == lang) else {
                    return Vec::new();
                };
                pattern
                    .find(tree, &f.text, parent)
                    .into_iter()
                    .map(|m| (m.range, m.captures))
                    .collect()
            }
            Matcher::Range(from, to) => {
                let ends = to.find(f, parent.clone());
                let mut out = Vec::new();
                let mut searched_to = parent.start;
                for (start, captures) in from.find(f, parent.clone()) {
                    if start.start < searched_to {
                        continue;
                    }
                    let Some((end, end_captures)) = ends.iter().find(|(e, _)| e.start >= start.end)
                    else {
                        break;
                    };
                    let range = full_lines(&f.text, start.start..end.end);
                    searched_to = range.end;
                    out.push((range, [captures, end_captures.clone()].concat()));
                }
                out
            }
            _ => self
                .ranges(f, parent)
                .into_iter()
                .map(|r| (r, Vec::new()))
                .collect(),
        }
    }

    fn ranges(&self, f: &SourceFile, parent: Range<usize>) -> Vec<Range<usize>> {
        let within = |r: &Range<usize>| parent.start <= r.start && r.end <= parent.end;
        let scope = scope(&f.text, &parent);
        let in_scope = |r: &Range<usize>| scope.start <= r.start && r.end <= scope.end;
        match self {
            Matcher::Lines { start, end } => {
                let count = f.buffer.line_count();
                // `$` is the parent's last line, which is the file's at the top level.
                let last = f
                    .buffer
                    .byte_to_line(parent.end.saturating_sub(1).max(parent.start))
                    .expect("parent within the buffer");
                let (Some(first), Some(last)) = (
                    line_index(*start, count, last),
                    line_index(*end, count, last),
                ) else {
                    return Vec::new();
                };
                let range = line_range(&f.buffer, first).start..line_range(&f.buffer, last).end;
                if in_scope(&range) {
                    vec![range]
                } else {
                    Vec::new()
                }
            }
            Matcher::Regex(re) => re
                .find_iter(&f.text[scope.clone()])
                .map(|m| scope.start + m.start()..scope.start + m.end())
                .collect(),
            Matcher::Str(needle) => {
                let needle = match f.buffer.line_ending() {
                    LineEnding::Lf => needle.to_string(),
                    LineEnding::Crlf => needle.replace('\n', "\r\n"),
                };
                if needle.is_empty() {
                    return Vec::new();
                }
                f.text[scope.clone()]
                    .match_indices(&needle)
                    .map(|(i, _)| scope.start + i..scope.start + i + needle.len())
                    .collect()
            }
            Matcher::Heredoc { lines, raw } => {
                let whole: Vec<(Range<usize>, &str)> = (0..f.buffer.line_count())
                    .map(|i| line_range(&f.buffer, i))
                    .filter(|r| in_scope(r))
                    .map(|r| {
                        let content = f.text[r.clone()].trim_end_matches('\n');
                        let content = content.strip_suffix('\r').unwrap_or(content);
                        (r, content)
                    })
                    .collect();
                let mut out = Vec::new();
                let mut i = 0;
                while i + lines.len() <= whole.len() {
                    let window = &whole[i..i + lines.len()];
                    if heredoc_matches(lines, *raw, window.iter().map(|(_, l)| *l)) {
                        out.push(window[0].0.start..window[window.len() - 1].0.end);
                        i += lines.len();
                    } else {
                        i += 1;
                    }
                }
                out
            }
            Matcher::File(path) => {
                if same_path(&f.path, path) {
                    vec![parent]
                } else {
                    Vec::new()
                }
            }
            Matcher::Conflict(n) => f
                .conflicts()
                .iter()
                .enumerate()
                .filter(|(i, _)| n.is_none_or(|n| n == i + 1))
                .map(|(_, c)| c.span())
                .filter(within)
                .collect(),
            Matcher::Query(queries) => {
                let (Some(lang), Some(tree)) = (f.lang, f.tree()) else {
                    return Vec::new();
                };
                let (_, query) = queries
                    .iter()
                    .find(|(l, _)| *l == lang)
                    .expect("compiled for every searched language");
                let sel = query.capture_index_for_name("sel");
                let mut cursor = QueryCursor::new();
                cursor.set_byte_range(parent.clone());
                let mut matches = cursor.matches(query, tree.root_node(), f.text.as_bytes());
                let mut out = Vec::new();
                while let Some(m) = matches.next() {
                    let ranges = m.captures().iter().map(|c| c.node.byte_range());
                    match sel {
                        Some(sel) if m.captures().iter().any(|c| c.index == sel) => out.extend(
                            m.captures()
                                .iter()
                                .filter(|c| c.index == sel)
                                .map(|c| c.node.byte_range()),
                        ),
                        _ => out.extend(ranges.min_by_key(|r| (r.start, Reverse(r.end)))),
                    }
                }
                out.retain(within);
                out.sort_by_key(|r| (r.start, r.end));
                out.dedup();
                out
            }
            Matcher::Code(_) | Matcher::Range(..) => {
                self.find(f, parent).into_iter().map(|(r, _)| r).collect()
            }
            Matcher::Syntax { kind, name } => f
                .items()
                .unwrap_or_default()
                .iter()
                .filter(|i| i.kind == *kind && syntax::item_matches(name, i))
                .map(|i| i.range.clone())
                .filter(within)
                .collect(),
        }
    }
}

/// Fails if the line range is past the end of every file that still has a
/// span to search.
fn check_lines(
    start: LineNo,
    end: LineNo,
    files: &[&SourceFile],
    parents: &[Match],
) -> Result<(), E> {
    let mut searched: Vec<usize> = parents.iter().map(|m| m.file).collect();
    searched.dedup();
    let past_end = |count| {
        [start, end].into_iter().find_map(|n| match n {
            LineNo::Number(n) if n > count => Some(n.to_string()),
            LineNo::Last if count == 0 => Some("$".to_string()),
            _ => None,
        })
    };
    let mut line = None;
    for &file in &searched {
        match past_end(files[file].buffer.line_count()) {
            Some(n) => line = line.or(Some(n)),
            None => return Ok(()),
        }
    }
    let Some(line) = line else { return Ok(()) };
    let files = searched
        .iter()
        .map(|&i| {
            let count = files[i].buffer.line_count();
            let unit = if count == 1 { "line" } else { "lines" };
            format!("{} ({count} {unit})", files[i].path)
        })
        .collect::<Vec<_>>()
        .join(", ");
    Err(E::LineOutOfRange { line, files })
}

/// Fails unless some searched file's language has selector items of `kind`;
/// files whose language lacks them are skipped. The error is the first such
/// file's, or `NoLanguage` if no searched file has a language.
fn check_syntax(kind: &str, name: &str, files: &[&SourceFile], parents: &[Match]) -> Result<(), E> {
    let mut searched: Vec<usize> = parents.iter().map(|m| m.file).collect();
    searched.dedup();
    let mut first_error = None;
    for &i in &searched {
        let Some(lang) = files[i].lang else { continue };
        let kinds = syntax::kinds(lang.selectors());
        if kinds.contains(&kind) {
            return Ok(());
        }
        first_error.get_or_insert(E::UnknownKind {
            kind: kind.into(),
            lang: lang.to_string(),
            kinds: kinds.join(", "),
        });
    }
    match first_error {
        Some(error) => Err(error),
        None if searched.is_empty() => Ok(()),
        None => Err(E::NoLanguage {
            selector: syntax::selector(kind, name),
            files: file_list(
                &searched
                    .iter()
                    .map(|&i| files[i].path.as_str())
                    .collect::<Vec<_>>(),
            ),
        }),
    }
}

/// `source` compiled for each language among the searched files.
fn compile_query(
    source: &str,
    files: &[&SourceFile],
    parents: &[Match],
) -> Result<Vec<(Language, Query)>, E> {
    let mut searched: Vec<usize> = parents.iter().map(|m| m.file).collect();
    searched.dedup();
    let mut queries: Vec<(Language, Query)> = Vec::new();
    for &i in &searched {
        let Some(lang) = files[i].lang else { continue };
        if queries.iter().any(|(l, _)| *l == lang) {
            continue;
        }
        let query = Query::new(&lang.grammar(), source).map_err(|err| E::InvalidQuery {
            lang: lang.to_string(),
            message: query_error(&err, &lang.grammar()),
        })?;
        queries.push((lang, query));
    }
    if queries.is_empty() && !searched.is_empty() {
        return Err(E::NoLanguage {
            selector: format!("query{{{}}}", source.replace('}', "\\}")),
            files: searched
                .iter()
                .map(|&i| files[i].path.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        });
    }
    Ok(queries)
}

/// The pattern `code` compiled for each code language among the searched
/// files that it parses in.
fn compile_pattern(
    code: &str,
    files: &[&SourceFile],
    parents: &[Match],
) -> Result<Vec<(Language, pattern::Pattern)>, E> {
    let mut searched: Vec<usize> = parents.iter().map(|m| m.file).collect();
    searched.dedup();
    let mut patterns: Vec<(Language, pattern::Pattern)> = Vec::new();
    let mut failed: Vec<(Language, pattern::PatternError)> = Vec::new();
    for &i in &searched {
        let Some(lang) = files[i].lang.filter(|l| *l != Language::Markdown) else {
            continue;
        };
        if patterns.iter().any(|(l, _)| *l == lang) || failed.iter().any(|(l, _)| *l == lang) {
            continue;
        }
        match pattern::Pattern::compile(lang, code) {
            Ok(p) => patterns.push((lang, p)),
            Err(e) => failed.push((lang, e)),
        }
    }
    let selector = fenced(code);
    if !patterns.is_empty() || searched.is_empty() {
        return Ok(patterns);
    }
    match failed.into_iter().next() {
        Some((_, e)) => Err(E::InvalidPattern {
            selector,
            message: e.to_string(),
        }),
        None => Err(E::NoCodeLanguage {
            selector,
            files: file_list(
                &searched
                    .iter()
                    .map(|&i| files[i].path.as_str())
                    .collect::<Vec<_>>(),
            ),
        }),
    }
}

/// `code` as a pattern step: between runs of backquotes longer than any it
/// holds (§3.10).
fn fenced(code: &str) -> String {
    let longest = code.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest + 1);
    let pad = if longest > 0 { " " } else { "" };
    format!("{fence}{pad}{code}{pad}{fence}")
}

fn query_error(err: &QueryError, grammar: &tree_sitter::Language) -> String {
    let message = &err.message;
    let closest = |wanted: &str, names: Vec<&str>| {
        let limit = (wanted.chars().count() / 3).max(2);
        names
            .into_iter()
            .map(|n| (syntax::distance(wanted, n), n))
            .filter(|(d, _)| *d <= limit)
            .min()
            .map_or(String::new(), |(_, n)| format!("; did you mean `{n}`?"))
    };
    let hint = match err.kind {
        QueryErrorKind::NodeType => {
            let kinds = (0..grammar.node_kind_count() as u16)
                .filter(|&id| grammar.node_kind_is_named(id) && grammar.node_kind_is_visible(id))
                .filter_map(|id| grammar.node_kind_for_id(id))
                .collect();
            closest(message.trim_matches('"'), kinds)
        }
        QueryErrorKind::Field => {
            let fields = (1..=grammar.field_count() as u16)
                .filter_map(|id| grammar.field_name_for_id(id))
                .collect();
            closest(message.trim_matches('"'), fields)
        }
        QueryErrorKind::Predicate => "; predicates look like (#eq? @capture \"text\")".into(),
        QueryErrorKind::Language => String::new(),
        _ => "; queries look like (node field: (child) @sel)".into(),
    };
    let what = match err.kind {
        QueryErrorKind::NodeType => format!("unknown node type `{}`", message.trim_matches('"')),
        QueryErrorKind::Field => format!("unknown field `{}`", message.trim_matches('"')),
        QueryErrorKind::Capture => format!("unknown capture `@{message}`"),
        QueryErrorKind::Predicate => "bad predicate".into(),
        QueryErrorKind::Structure => "impossible pattern".into(),
        QueryErrorKind::Syntax => "syntax error".into(),
        QueryErrorKind::Language => return message.clone(),
    };
    format!("{what} at column {}{hint}", err.column + 1)
}

/// The hint for a range whose end repeats the steps before it, as in
/// `P>"a"..P>"b"`: `..` binds tighter than `>`, so that's a range from `"a"`
/// to `P` followed by the step `"b"`, and `P>"a".."b"` was meant.
fn range_precedence(selector: &Selector, src: &str) -> Option<String> {
    let steps = &selector.steps;
    let text = |range: &Range<usize>| src.get(range.clone());
    (0..steps.len()).find_map(|i| {
        let Primary::Range { to, .. } = &steps[i].primary else {
            return None;
        };
        let j = (0..i).find(|&j| steps[j].primary == **to && steps[j].parts.is_empty())?;
        let k = i + (i - j);
        let repeated = steps.get(i + 1..k)?;
        let same = |a: &Step, b: &Step| a.primary == b.primary && a.parts == b.parts;
        if k >= steps.len()
            || !steps[j + 1..i]
                .iter()
                .zip(repeated)
                .all(|(a, b)| same(a, b))
        {
            return None;
        }
        let from = text(&steps[i].span)?.strip_suffix(text(&steps[j].span)?)?;
        let fixed = format!(
            "{}{from}{}",
            text(&(selector.span.start..steps[i].span.start))?,
            text(&(steps[k].span.start..selector.span.end))?
        );
        Some(format!(
            "; `..` binds tighter than `>`: did you mean {fixed}?"
        ))
    })
}

/// The fix for a `step` that matched nothing within `parents` (§7): its name
/// under another kind, a close syntax name, a literal match ignoring case and
/// spacing, a case-insensitive regex match, the spans a nested step searched,
/// or where to look. The step starts at byte `at` of `selector`.
pub(crate) fn hint(
    step: &Step,
    files: &[&SourceFile],
    parents: &[Match],
    selector: &str,
    at: usize,
) -> String {
    if let Some(hint) = other_kind(step, files, parents, selector, at)
        .or_else(|| close_name(step, files, parents, selector, at))
    {
        return hint;
    }
    let location = |m: &Match| {
        let f = files[m.file];
        let lines = line_numbers(&f.buffer, &m.range);
        if files.len() > 1 {
            format!("{}:{lines}", f.path)
        } else {
            lines
        }
    };
    // The hints search where the step did: each parent's lines, if whole.
    let scopes: Vec<Match> = parents
        .iter()
        .map(|p| Match {
            file: p.file,
            range: scope(&files[p.file].text, &p.range),
            captures: Vec::new(),
        })
        .collect();
    match &step.primary {
        Primary::Literal(text) => {
            if text.kind == TextKind::Str
                && let Some((escaped, m)) = escaped_literal(&text.value, files, &scopes)
            {
                let quoted = escaped.replace('\\', "\\\\").replace('"', "\\\"");
                return format!(
                    "; as source text it matches at {}: \"{quoted}\"",
                    location(&m)
                );
            }
            if let Some(m) = near_literal(&text.value, files, &scopes) {
                return format!(
                    "; ignoring case and spacing, it matches at {}",
                    location(&m)
                );
            }
        }
        Primary::Regex(pattern) if !pattern.flags.case_insensitive => {
            if let Some(m) = case_insensitive_match(pattern, files, &scopes) {
                return format!(
                    "; it matches case-insensitively at {} (add the i flag)",
                    location(&m)
                );
            }
        }
        Primary::Conflict(_) => {
            let mut searched: Vec<&SourceFile> = parents.iter().map(|p| files[p.file]).collect();
            searched.dedup_by_key(|f| &f.path);
            let conflicted: Vec<String> = searched
                .iter()
                .filter(|f| !f.conflicts().is_empty())
                .take(3)
                .map(|f| {
                    let n = f.conflicts().len();
                    let names: Vec<String> = (1..=n).map(|i| format!("conflict:{i}")).collect();
                    let noun = if n == 1 { "conflict" } else { "conflicts" };
                    format!("{} has {n} {noun} ({})", f.path, names.join(", "))
                })
                .collect();
            return match searched[..] {
                _ if !conflicted.is_empty() => format!("; {}", conflicted.join("; ")),
                [f] => format!("; {} has no merge conflicts", f.path),
                _ => "; no searched file has merge conflicts".into(),
            };
        }
        _ => {}
    }
    let nested = parents
        .iter()
        .any(|p| p.range != (0..files[p.file].text.len()));
    if nested {
        let mut searched: Vec<String> = parents.iter().take(3).map(location).collect();
        if parents.len() > 3 {
            searched.push("...".into());
        }
        return format!("; it searched {}", searched.join(", "));
    }
    match step.primary {
        Primary::Syntax { .. } => "; `outline` lists the items".into(),
        _ => "; `show` prints the text to match against".into(),
    }
}

/// The first place within `parents` where `needle` occurs once case and runs
/// of whitespace are ignored.
fn near_literal(needle: &str, files: &[&SourceFile], parents: &[Match]) -> Option<Match> {
    let (needle, _) = fold(needle);
    let needle = needle.trim();
    if needle.is_empty() {
        return None;
    }
    parents.iter().find_map(|p| {
        let (haystack, offsets) = fold(&files[p.file].text[p.range.clone()]);
        let at = haystack.find(needle)?;
        let start = p.range.start + offsets[at];
        let end = p.range.start + offsets[at + needle.len() - 1] + 1;
        Some(Match {
            file: p.file,
            range: start..end,
            captures: Vec::new(),
        })
    })
}

/// `needle` as source code writes it, with its newlines, tabs, backslashes
/// and quotes escaped, and where that first occurs within `parents`, if it
/// differs from `needle`.
fn escaped_literal(
    needle: &str,
    files: &[&SourceFile],
    parents: &[Match],
) -> Option<(String, Match)> {
    let escaped = needle
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
        .replace('"', "\\\"");
    if escaped == needle {
        return None;
    }
    let found = parents.iter().find_map(|p| {
        let at = files[p.file].text[p.range.clone()].find(&escaped)?;
        let start = p.range.start + at;
        Some(Match {
            file: p.file,
            range: start..start + escaped.len(),
            captures: Vec::new(),
        })
    })?;
    Some((escaped, found))
}

/// The first match of `pattern` within `parents` when case is ignored.
fn case_insensitive_match(
    pattern: &Pattern,
    files: &[&SourceFile],
    parents: &[Match],
) -> Option<Match> {
    let flags = RegexFlags {
        case_insensitive: true,
        ..pattern.flags
    };
    let folded = Pattern::new(pattern.source.clone(), flags).ok()?;
    let re = folded.regex();
    parents.iter().find_map(|p| {
        let found = re.find(&files[p.file].text[p.range.clone()])?;
        let start = p.range.start + found.start();
        Some(Match {
            file: p.file,
            range: start..(start + found.len()).max(start + 1),
            captures: Vec::new(),
        })
    })
}

/// `text` lowercased, with each run of whitespace as one space, and the byte
/// offset in `text` of each byte of the result.
fn fold(text: &str) -> (String, Vec<usize>) {
    let mut folded = String::new();
    let mut offsets = Vec::new();
    let mut in_space = false;
    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            if !in_space {
                folded.push(' ');
                offsets.push(i);
            }
            in_space = true;
            continue;
        }
        in_space = false;
        for lower in c.to_lowercase() {
            let before = folded.len();
            folded.push(lower);
            offsets.extend(std::iter::repeat_n(i, folded.len() - before));
        }
    }
    (folded, offsets)
}

/// `; did you mean SEL (LINES)?`, naming the item closest in name to a syntax
/// `step` that matched nothing within `parents`, if one is close.
fn close_name(
    step: &Step,
    files: &[&SourceFile],
    parents: &[Match],
    selector: &str,
    at: usize,
) -> Option<String> {
    let Primary::Syntax { kind, name } = &step.primary else {
        return None;
    };
    if name.contains('*') {
        return None;
    }
    let bare = bare_name(name);
    let limit = (bare.chars().count() / 3).max(2);
    let best = parents
        .iter()
        .flat_map(|p| {
            files[p.file]
                .items()
                .unwrap_or_default()
                .iter()
                .filter(|i| i.kind == kind && p.range.start <= i.range.start)
                .filter(|i| i.range.end <= p.range.end)
                .map(|i| {
                    // A trait impl is also named by its self type.
                    let (d, closest) = [Some(&i.name), i.base_name.as_ref()]
                        .into_iter()
                        .flatten()
                        .map(|n| (syntax::distance(name, n).min(syntax::distance(&bare, n)), n))
                        .min()
                        .expect("an item has a name");
                    (d, p.file, closest, i)
                })
        })
        .filter(|(d, ..)| *d <= limit)
        .min_by_key(|(d, ..)| *d);
    let (_, file, closest, item) = best?;
    Some(did_you_mean(
        selector,
        at,
        &syntax::selector(kind, name),
        &syntax::selector(kind, closest),
        files,
        file,
        item,
    ))
}

/// `name` without its generic arguments and path prefixes, as items are
/// named: `Display for Log` for `fmt::Display for Log<'_>`.
fn bare_name(name: &str) -> String {
    let mut depth = 0usize;
    let mut outside = String::new();
    for c in name.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => outside.push(c),
            _ => {}
        }
    }
    outside
        .split_whitespace()
        .map(|word| word.rsplit("::").next().unwrap_or(word))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `; did you mean SEL (LINES)?`, naming an item of another kind with the name
/// of a syntax `step` that matched nothing within `parents`, taking kinds in
/// their order in `KINDS`.
fn other_kind(
    step: &Step,
    files: &[&SourceFile],
    parents: &[Match],
    selector: &str,
    at: usize,
) -> Option<String> {
    let Primary::Syntax { kind, name } = &step.primary else {
        return None;
    };
    let (file, item) = parents
        .iter()
        .flat_map(|p| {
            files[p.file]
                .items()
                .unwrap_or_default()
                .iter()
                .filter(|i| i.kind != kind && syntax::item_matches(name, i))
                .filter(|i| p.range.start <= i.range.start && i.range.end <= p.range.end)
                .map(|i| (p.file, i))
        })
        .min_by_key(|(_, i)| syntax::rank(i.kind))?;
    Some(did_you_mean(
        selector,
        at,
        &syntax::selector(kind, name),
        &syntax::selector(item.kind, name),
        files,
        file,
        item,
    ))
}

/// `; did you mean SEL (LINES)?`: `selector` with the first `written` from
/// byte `at`, the failing step, replaced by `fixed`, which selects `item` in
/// `files[file]`.
fn did_you_mean(
    selector: &str,
    at: usize,
    written: &str,
    fixed: &str,
    files: &[&SourceFile],
    file: usize,
    item: &Item,
) -> String {
    let suggestion = match selector[at..].find(written).map(|i| at + i) {
        Some(i) => format!(
            "{}{fixed}{}",
            &selector[..i],
            &selector[i + written.len()..]
        ),
        None => fixed.to_string(),
    };
    let f = files[file];
    let lines = line_numbers(&f.buffer, &item.range);
    let location = if files.len() > 1 {
        format!("{}:{lines}", f.path)
    } else {
        lines
    };
    format!("; did you mean {suggestion} ({location})?")
}

/// The index of line `n` in a file of `count` lines whose `$` is `last`.
fn line_index(n: LineNo, count: usize, last: usize) -> Option<usize> {
    match n {
        LineNo::Number(n) if (1..=count).contains(&n) => Some(n - 1),
        LineNo::Last if count > 0 => Some(last),
        _ => None,
    }
}

fn line_range(buffer: &Buffer, line: usize) -> Range<usize> {
    buffer
        .line_range(line)
        .expect("line index within the buffer")
}

/// Whether `window` equals the body lines, each non-blank line behind one
/// shared whitespace prefix (or exactly, if `raw`).
fn heredoc_matches<'w>(body: &[String], raw: bool, window: impl Iterator<Item = &'w str>) -> bool {
    let mut prefix = None;
    for (b, line) in body.iter().zip(window) {
        if raw {
            if b != line {
                return false;
            }
        } else if b.is_empty() {
            if !line.trim().is_empty() {
                return false;
            }
        } else {
            let Some(p) = line.strip_suffix(b.as_str()) else {
                return false;
            };
            if !p.chars().all(|c| c == ' ' || c == '\t') || *prefix.get_or_insert(p) != p {
                return false;
            }
        }
    }
    true
}

pub(crate) fn same_path(a: &str, b: &str) -> bool {
    a.strip_prefix("./").unwrap_or(a) == b.strip_prefix("./").unwrap_or(b)
}

pub(crate) fn part_name(part: Part) -> &'static str {
    match part {
        Part::Body => "body",
        Part::Sig => "sig",
        Part::Params => "params",
        Part::Name => "name",
        Part::Doc => "doc",
        Part::Attrs => "attrs",
        Part::Ret => "ret",
        Part::Type => "type",
        Part::Value => "value",
        Part::Whole => "whole",
        Part::Lines => "lines",
        Part::Refs => "refs",
        Part::Def => "def",
        Part::Ours => "ours",
        Part::Theirs => "theirs",
        Part::Base => "base",
    }
}

/// A selector for each match (§3.5): `selector`, with a wildcard name in
/// its last step replaced by the match's item name. Among the matches with
/// the same selector, it's scoped by the match's nearest enclosing item if no
/// other match shares it, otherwise by its file if no other match shares
/// that, otherwise by its lines. An item or line scope goes just before the
/// last step; a file scope goes first.
fn candidates(
    found: &[Found],
    parents: &[Match],
    files: &[&SourceFile],
    selector: &Selector,
    src: &str,
) -> Candidates {
    let last = selector
        .steps
        .last()
        .filter(|s| selector.span.start <= s.span.start && s.span.end <= selector.span.end);
    let split = last.map_or(selector.span.start, |s| s.span.start);
    let prefix = &src[selector.span.start..split];
    let in_file = matches!(
        selector.steps.first(),
        Some(Step {
            primary: Primary::File(_),
            ..
        })
    );
    // For a syntax step: the name each candidate selects its item by, the
    // item, and whether the name replaces the step's.
    let chosen: Vec<Option<(String, &Item, bool)>> = found
        .iter()
        .map(|c| last.and_then(|step| named(step, files[c.m.file], &c.core)))
        .collect();
    let named: Vec<String> = chosen
        .iter()
        .zip(found)
        .map(|(chosen, c)| {
            // The last step's filters, as written.
            let filters = last.and_then(|step| Some(step.filters.first()?.1.start..step.span.end));
            let parts = |step: &Step| -> String {
                step.parts
                    .iter()
                    .map(|&p| format!(".{}", part_name(p)))
                    .collect()
            };
            let text = match (chosen, last) {
                (Some((name, item, true)), Some(step)) => {
                    format!("{}{}", syntax::selector(item.kind, name), parts(step))
                }
                // Each of several conflicts is named by its number.
                (None, Some(step)) if step.primary == Primary::Conflict(None) => {
                    let (n, _) = files[c.m.file]
                        .conflict_at(&c.core)
                        .expect("a conflict step matches conflicts");
                    format!("conflict:{n}{}", parts(step))
                }
                _ => src[split..filters.clone().map_or(selector.span.end, |f| f.start)].to_string(),
            };
            // A line split from a span is scoped by its number, after the
            // span, instead of by `.lines` and filters.
            match c.line {
                true => text.trim_end_matches(".lines").to_string(),
                false => format!("{text}{}", filters.map_or("", |f| &src[f])),
            }
        })
        .collect();
    let enclosing: Vec<Option<String>> = found
        .iter()
        .map(|c| enclosing(files[c.m.file], &c.core, &parents[c.parent].range))
        .collect();
    let sides: Vec<Option<String>> = found
        .iter()
        .map(|c| enclosing_side(files[c.m.file], &c.core, &parents[c.parent].range))
        .collect();
    let all: Vec<(Option<String>, String)> = (0..found.len())
        .map(|i| {
            let (c, last) = (&found[i], &named[i]);
            let f = &files[c.m.file];
            let lines = line_numbers(&f.buffer, &c.core);
            let parent = &parents[c.parent].range;
            let scope = scope(&f.text, parent);
            let covered = full_lines(&f.text, c.core.clone());
            let lines_fit = scope.start <= covered.start && covered.end <= scope.end;
            // The matches this candidate's last step also selects.
            let mut peers: Vec<usize> = (0..found.len())
                .filter(|&j| match (&chosen[i], &chosen[j]) {
                    (Some((name, ..)), Some((_, item, _))) => syntax::item_matches(name, item),
                    _ => named[j] == *last,
                })
                .collect();
            // The lines of one span count once.
            peers.dedup_by_key(|&mut j| (found[j].m.file, found[j].core.clone()));
            let unique_item = enclosing[i].as_ref().filter(|&item| {
                peers
                    .iter()
                    .filter(|&&j| enclosing[j].as_ref() == Some(item))
                    .count()
                    == 1
            });
            let unique_side = sides[i].as_ref().filter(|&side| {
                peers
                    .iter()
                    .filter(|&&j| sides[j].as_ref() == Some(side))
                    .count()
                    == 1
            });
            let one_in_file = peers
                .iter()
                .filter(|&&j| found[j].m.file == c.m.file)
                .count()
                == 1;
            let candidate = match unique_item {
                _ if peers.len() == 1 => Some(format!("{prefix}{last}")),
                Some(item) => Some(format!("{prefix}{item}>{last}")),
                None if let Some(side) = unique_side => Some(format!("{prefix}{side}>{last}")),
                None if files.len() == 1 || in_file => {
                    lines_fit.then(|| format!("{prefix}{lines}>{last}"))
                }
                None if one_in_file => Some(format!("file:{}>{prefix}{last}", f.path)),
                None => lines_fit.then(|| format!("file:{}>{prefix}{lines}>{last}", f.path)),
            };
            let at = line_numbers(&f.buffer, &c.m.range);
            let candidate = match c.line {
                true => candidate.map(|candidate| format!("{candidate}>{at}")),
                false => candidate,
            };
            (candidate, format!("{}:{at}", f.path))
        })
        .collect();
    // Matches on one line get the same line-scoped selector, which picks none
    // of them alone.
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for (candidate, _) in &all {
        if let Some(candidate) = candidate {
            *counts.entry(candidate).or_default() += 1;
        }
    }
    let unique: Vec<(String, String)> = all
        .iter()
        .filter_map(|(candidate, location)| {
            let candidate = candidate.as_ref()?;
            (counts[candidate.as_str()] == 1).then(|| (candidate.clone(), location.clone()))
        })
        .collect();
    Candidates {
        shared: all.len() - unique.len(),
        listed: unique.into_iter().take(MAX_CANDIDATES).collect(),
        total: found.len(),
    }
}

/// The name that `last`, a syntax step, selects `range`'s item by in a
/// candidate, the item, and whether the name replaces the step's: it does
/// for a wildcard, and for a trait impl found by its self type.
fn named<'f>(
    last: &Step,
    f: &'f SourceFile,
    range: &Range<usize>,
) -> Option<(String, &'f Item, bool)> {
    let Primary::Syntax { kind, name } = &last.primary else {
        return None;
    };
    let item = f
        .items()?
        .iter()
        .find(|i| i.kind == kind && i.range == *range)?;
    Some(
        if name.contains('*') || !syntax::name_matches(name, &item.name) {
            (item.name.clone(), item, true)
        } else {
            (name.clone(), item, false)
        },
    )
}

/// The selector of the innermost item that strictly contains `range` (within
/// its lines, §3.4) and lies strictly inside `parent`, where a step nested in
/// the parent finds it.
fn enclosing(f: &SourceFile, range: &Range<usize>, parent: &Range<usize>) -> Option<String> {
    f.items()?
        .iter()
        .rev()
        .find(|i| {
            let lines = scope(&f.text, &i.range);
            lines.start <= range.start
                && range.end <= lines.end
                && i.range != *range
                && parent.start <= i.range.start
                && i.range.end <= parent.end
                && i.range != *parent
        })
        .map(|i| syntax::selector(i.kind, &i.name))
}

/// The selector of the conflict side, within `parent`, whose lines hold
/// `range`, as `conflict:1.ours`.
fn enclosing_side(f: &SourceFile, range: &Range<usize>, parent: &Range<usize>) -> Option<String> {
    f.conflicts().iter().enumerate().find_map(|(i, c)| {
        let side = c.sides().find(|s| {
            s.start <= range.start
                && range.end <= s.end
                && s != range
                && parent.start <= s.start
                && s.end <= parent.end
        })?;
        Some(format!("conflict:{}.{}", i + 1, c.side_at(&side)?.name()))
    })
}

/// The 1-based line or line range `range` touches, as a line selector.
pub(crate) fn line_numbers(buffer: &Buffer, range: &Range<usize>) -> String {
    let line = |offset| buffer.byte_to_line(offset).expect("match within the file") + 1;
    let first = line(range.start);
    let last = if range.is_empty() {
        first
    } else {
        line(range.end - 1)
    };
    if first == last {
        first.to_string()
    } else {
        format!("{first}-{last}")
    }
}

/// The span that a step nested in `parent` searches (§3.4): its whole lines,
/// indentation and line ending included, if it covers whole lines.
pub(crate) fn scope(text: &str, parent: &Range<usize>) -> Range<usize> {
    if is_whole_line(text, parent) {
        full_lines(text, parent.clone())
    } else {
        parent.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::ExecErrorKind;
    use crate::script::ast::CommandKind;
    use crate::script::parse;

    const TEXT: &str =
        "fn a() {\n    let x = 1;\n    let y = 2;\n}\n\nfn b() {\n    let x = 3;\n}\n";

    fn files(texts: &[(&str, &str)]) -> Vec<SourceFile> {
        texts
            .iter()
            .map(|(path, text)| {
                SourceFile::new(*path, text.to_string(), Language::detect(path, text))
            })
            .collect()
    }

    /// Resolves the target of the script's first command, `delete TARGET`.
    fn resolve_in(script: &str, files: &[SourceFile]) -> Result<Vec<Match>, ExecError> {
        let parsed = parse(script).unwrap();
        let CommandKind::Delete(target) = &parsed.commands[0].kind else {
            panic!("expected delete: {script}");
        };
        resolve(target, &files.iter().collect::<Vec<_>>(), script)
    }

    /// The text of each span `script` selects in a single file `a.rs`.
    fn select(script: &str, text: &str) -> Vec<String> {
        resolve_in(script, &files(&[("a.rs", text)]))
            .unwrap()
            .into_iter()
            .map(|m| {
                assert_eq!(m.file, 0);
                text[m.range].to_string()
            })
            .collect()
    }

    /// The text of each span `script` selects in a single file `path`.
    fn select_in(script: &str, path: &str, text: &str) -> Vec<String> {
        resolve_in(script, &files(&[(path, text)]))
            .unwrap()
            .into_iter()
            .map(|m| text[m.range].to_string())
            .collect()
    }

    fn error(script: &str, texts: &[(&str, &str)]) -> String {
        resolve_in(script, &files(texts))
            .unwrap_err()
            .render(script)
    }

    #[test]
    fn lines_cover_whole_lines_with_endings() {
        assert_eq!(select("delete 2", TEXT), ["    let x = 1;\n"]);
        assert_eq!(
            select("delete 2-3", TEXT),
            ["    let x = 1;\n    let y = 2;\n"]
        );
        assert_eq!(select("delete $", TEXT), ["}\n"]);
        assert_eq!(select("delete 7-$", TEXT), ["    let x = 3;\n}\n"]);
        assert_eq!(select("delete $", "a\nb"), ["b"]);
    }

    #[test]
    fn line_past_end_is_an_error() {
        assert_eq!(
            error("delete 9", &[("a.rs", TEXT)]),
            "error: script:1:8: line 9 is past the end of a.rs (8 lines); use `$` for the last line"
        );
        assert_eq!(
            error("delete 7-12", &[("a.rs", TEXT)]),
            "error: script:1:8: line 12 is past the end of a.rs (8 lines); use `$` for the last line"
        );
        assert_eq!(
            error("delete $", &[("a.rs", "")]),
            "error: script:1:8: line $ is past the end of a.rs (0 lines)"
        );
    }

    #[test]
    fn line_past_end_of_only_some_files_selects_in_the_others() {
        let set = files(&[("a.rs", TEXT), ("b.rs", "x\n")]);
        let matches = resolve_in("delete 5", &set).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].file, 0);
        assert_eq!(
            error("delete 9", &[("a.rs", TEXT), ("b.rs", "x\n")]),
            "error: script:1:8: line 9 is past the end of a.rs (8 lines), b.rs (1 line); \
             use `$` for the last line"
        );
    }

    #[test]
    fn regex_selects_every_match() {
        assert_eq!(
            select("delete all /let \\w/", TEXT),
            ["let x", "let y", "let x"]
        );
        assert_eq!(select("delete all /LET X/i", TEXT), ["let x", "let x"]);
        assert_eq!(select("delete all /^}$/", TEXT), ["}", "}"]);
    }

    #[test]
    fn string_selects_exact_occurrences() {
        assert_eq!(select("delete all \"let x\"", TEXT), ["let x", "let x"]);
        assert_eq!(select("delete \"1;\\n    let y\"", TEXT), ["1;\n    let y"]);
        assert_eq!(select("delete all \"aa\"", "aaaaa"), ["aa", "aa"]);
    }

    #[test]
    fn ranges_run_from_one_match_to_the_next_match_of_the_end() {
        assert_eq!(select("delete fn:a..fn:b", TEXT), [TEXT]);
        assert_eq!(
            select("delete all /let x/../let y/", TEXT),
            ["    let x = 1;\n    let y = 2;\n"]
        );
        assert_eq!(
            select("delete all /let x/../let y/.lines", TEXT),
            ["    let x = 1;\n", "    let y = 2;\n"]
        );
        assert_eq!(
            select(r#"delete fn:b>/let/.."}""#, TEXT),
            ["    let x = 3;\n}\n"]
        );
    }

    #[test]
    fn ranges_skip_starts_inside_an_earlier_range() {
        assert_eq!(select("delete all /a/../b/", "a b a b\n"), ["a b a b\n"]);
        assert_eq!(select("delete all /a/../b/", "a a b\nb\n"), ["a a b\n"]);
    }

    #[test]
    fn ambiguous_ranges_list_candidates() {
        assert_eq!(
            error("delete /a/../b/", &[("a.rs", "a b\na b\n")]),
            "error: script:1:8: /a/../b/ matches 2 items; add `all` or use one of:\n  \
             1>/a/../b/   a.rs:1\n  \
             2>/a/../b/   a.rs:2"
        );
    }

    #[test]
    fn nested_steps_resolve_within_each_span() {
        let set = files(&[("a.rs", TEXT)]);
        let m = resolve_in("delete 6-8>/let x/", &set).unwrap();
        assert_eq!(
            m,
            [Match {
                file: 0,
                range: TEXT.rfind("let x").unwrap()..TEXT.rfind(" = 3").unwrap(),
                captures: vec![],
            }]
        );
        let m = resolve_in("delete /fn b[^}]*/>\"x\"", &set).unwrap();
        let x = TEXT.rfind('x').unwrap();
        assert_eq!(
            m,
            [Match {
                file: 0,
                range: x..x + 1,
                captures: vec![],
            }]
        );
        assert_eq!(
            select("delete all /let . = \\d/>/\\d/", TEXT),
            ["1", "2", "3"]
        );
    }

    #[test]
    fn nested_lines_must_lie_inside_the_parent() {
        assert_eq!(
            error("delete 1-4>7", &[("a.rs", TEXT)]),
            "error: script:1:8: 1-4>7 matches nothing in a.rs; it searched 1-4"
        );
    }

    #[test]
    fn nested_lines_may_be_an_items_first_or_last_line() {
        assert_eq!(select("delete fn:b>8", TEXT), ["}\n"]);
        assert_eq!(
            select("delete fn:b>6-7", TEXT),
            ["fn b() {\n    let x = 3;\n"]
        );
        let text = "impl S {\n    fn new() {\n        1\n    }\n}\n";
        assert_eq!(select("delete fn:new>2", text), ["    fn new() {\n"]);
        assert_eq!(select("delete fn:new>4", text), ["    }\n"]);
        // A partial-line parent still holds none of its lines.
        let set = files(&[("a.rs", TEXT)]);
        assert!(resolve_in("delete /let x = 1/>2", &set).is_err());
    }

    #[test]
    fn nested_last_line_is_the_parents() {
        assert_eq!(select("delete fn:a>$", TEXT), ["}\n"]);
        assert_eq!(select("delete fn:a>3-$", TEXT), ["    let y = 2;\n}\n"]);
        assert_eq!(select("delete 2-3>$", TEXT), ["    let y = 2;\n"]);
        assert_eq!(select("delete $", TEXT), ["}\n"]);
        assert_eq!(select("delete fn:b>$", "fn b() {}"), ["fn b() {}"]);
    }

    #[test]
    fn nested_matches_may_start_in_an_items_indentation() {
        let text = "impl S {\n    fn new() {\n        1\n    }\n}\n";
        assert_eq!(select("delete fn:new>\"    fn new\"", text), ["    fn new"]);
        assert_eq!(
            select("delete fn:new><<END\nfn new() {\nEND\n", text),
            ["    fn new() {\n"]
        );
        assert_eq!(select("delete fn:new>/^    fn/", text), ["    fn"]);
        // `^` is a line start, not the start of the item.
        let set = files(&[("a.rs", text)]);
        assert!(resolve_in("delete fn:new>/^fn/", &set).is_err());
    }

    #[test]
    fn candidates_nest_matches_in_an_items_indentation() {
        let text = "impl S {\n    fn a() {}\n    fn b() {}\n}\n";
        assert_eq!(
            listed("delete impl:S>\"    fn\"", &[("a.rs", text)]),
            ["impl:S>fn:a>\"    fn\"", "impl:S>fn:b>\"    fn\""]
        );
    }

    #[test]
    fn lines_part_selects_each_whole_line() {
        assert_eq!(select("delete \"y = 2\".lines", TEXT), ["    let y = 2;\n"]);
        assert_eq!(
            select("delete all \"2;\\n}\".lines", TEXT),
            ["    let y = 2;\n", "}\n"]
        );
        assert_eq!(select("delete all /x/.lines", "x x\ny\n"), ["x x\n"]);
        assert_eq!(
            select("delete all fn:b.lines", TEXT),
            ["fn b() {\n", "    let x = 3;\n", "}\n"]
        );
        assert_eq!(
            select("delete all fn:a.body.lines", TEXT),
            ["    let x = 1;\n", "    let y = 2;\n"]
        );
    }

    #[test]
    fn ambiguous_lines_list_line_numbers() {
        assert_eq!(
            error("delete fn:b.lines", &[("a.rs", TEXT)]),
            "error: script:1:8: fn:b.lines matches 3 items; add `all` or use one of:\n  \
             fn:b>6   a.rs:6\n  \
             fn:b>7   a.rs:7\n  \
             fn:b>8   a.rs:8"
        );
        assert_eq!(
            error("delete fn:a.body.lines", &[("a.rs", TEXT)]),
            "error: script:1:8: fn:a.body.lines matches 2 items; add `all` or use one of:\n  \
             fn:a.body>2   a.rs:2\n  \
             fn:a.body>3   a.rs:3"
        );
    }

    #[test]
    fn bare_kinds_select_every_item_of_the_kind() {
        assert_eq!(select("delete all fn", TEXT).len(), 2);
        assert_eq!(select("delete fn:b>var", TEXT), ["let x = 3;"]);
        assert_eq!(
            error("delete fn", &[("a.rs", TEXT)]),
            "error: script:1:8: fn matches 2 items; add `all` or use one of:\n  \
             fn:a   a.rs:1-4\n  \
             fn:b   a.rs:6-8"
        );
    }

    #[test]
    fn whole_is_the_item_with_its_docs() {
        assert_eq!(
            select("delete fn:new.whole", "/// Doc.\n#[a]\nfn new() {}\n"),
            ["/// Doc.\n#[a]\nfn new() {}"]
        );
    }

    const FILTERED: &str = "/// Doc.\nfn test_a() {\n    one();\n}\n\nfn test_b() {}\n\nfn main() {\n    let long_name = 1;\n    two();\n}\n";

    #[test]
    fn filters_keep_the_spans_where_they_hold() {
        let a = "/// Doc.\nfn test_a() {\n    one();\n}";
        let main = "fn main() {\n    let long_name = 1;\n    two();\n}";
        assert_eq!(
            select("delete all fn[.name ~= /^test_/]", FILTERED),
            [a, "fn test_b() {}"]
        );
        assert_eq!(
            select("delete all fn[.doc == \"\"]", FILTERED),
            ["fn test_b() {}", main]
        );
        assert_eq!(
            select("delete all fn:main.lines[.len > 12]", FILTERED),
            ["    let long_name = 1;\n"]
        );
        assert_eq!(select("delete fn[.body.len == 2]", FILTERED), [main]);
        assert_eq!(
            select("delete fn[.name ~= /^test_/ && .doc == \"\"]", FILTERED),
            ["fn test_b() {}"]
        );
        assert_eq!(
            select("delete all fn[.name == \"main\" || .doc != \"\"]", FILTERED),
            [a, main]
        );
        assert_eq!(
            select("delete fn[.name ~= /^test_/][.doc == \"\"]", FILTERED),
            ["fn test_b() {}"]
        );
        assert_eq!(select("delete fn[.len == 4]>/one/", FILTERED), ["one"]);
    }

    #[test]
    fn filtered_candidates_keep_their_filters() {
        assert_eq!(
            error("delete fn[.name ~= /^test_/]", &[("a.rs", FILTERED)]),
            "error: script:1:8: fn[.name ~= /^test_/] matches 2 items; add `all` or use one of:\n  \
             fn:test_a[.name ~= /^test_/]   a.rs:1-4\n  \
             fn:test_b[.name ~= /^test_/]   a.rs:6"
        );
        assert_eq!(
            error("delete fn:main.lines[.len < 12]", &[("a.rs", FILTERED)]),
            "error: script:1:8: fn:main.lines[.len < 12] matches 3 items; add `all` or use one of:\n  \
             fn:main>8    a.rs:8\n  \
             fn:main>10   a.rs:10\n  \
             fn:main>11   a.rs:11"
        );
    }

    #[test]
    fn a_filter_can_reject_every_match() {
        let e = error("delete fn[.name == \"zzz\"]", &[("a.rs", FILTERED)]);
        assert!(e.contains("matches nothing"), "{e}");
        assert_eq!(
            error("delete /one/[.name == \"\"]", &[("a.rs", FILTERED)]),
            "error: script:1:8: .name needs a syntax item, e.g. fn:NAME.name"
        );
    }

    #[test]
    fn heredoc_matches_whole_lines_at_any_indentation() {
        let body = "    let x = 1;\n    let y = 2;\n";
        assert_eq!(
            select("delete <<END\nlet x = 1;\nlet y = 2;\nEND\n", TEXT),
            [body]
        );
        assert_eq!(
            select(
                "delete <<END\n        let x = 1;\n        let y = 2;\nEND\n",
                TEXT
            ),
            [body]
        );
        assert_eq!(
            select("delete <<END\nfn b() {\n    let x = 3;\n}\nEND\n", TEXT),
            ["fn b() {\n    let x = 3;\n}\n"]
        );
    }

    #[test]
    fn heredoc_keeps_relative_indentation() {
        assert_eq!(
            error(
                "delete <<END\nfn b() {\nlet x = 3;\n}\nEND\n",
                &[("a.rs", TEXT)]
            ),
            "error: script:1:8: <<END matches nothing in a.rs; \
             ignoring case and spacing, it matches at 6-8"
        );
        let text = "  let x = 1;\n    let y = 2;\n";
        assert!(
            resolve_in(
                "delete <<END\nlet x = 1;\nlet y = 2;\nEND\n",
                &files(&[("a.rs", text)])
            )
            .is_err()
        );
    }

    #[test]
    fn heredoc_blank_lines_match_blank_or_whitespace_lines() {
        assert_eq!(
            select("delete <<END\n}\n\nfn b() {\nEND\n", TEXT),
            ["}\n\nfn b() {\n"]
        );
        let text = "  a\n  \n  b\n";
        assert_eq!(select("delete <<END\na\n\nb\nEND\n", text), [text]);
    }

    #[test]
    fn heredoc_must_match_whole_lines() {
        assert_eq!(
            error("delete <<END\nlet x\nEND\n", &[("a.rs", TEXT)]),
            "error: script:1:8: <<END matches nothing in a.rs; \
             ignoring case and spacing, it matches at 2"
        );
    }

    #[test]
    fn raw_heredoc_matches_exactly() {
        assert_eq!(
            select("delete <<'END'\n    let y = 2;\nEND\n", TEXT),
            ["    let y = 2;\n"]
        );
        assert!(
            resolve_in(
                "delete <<'END'\nlet y = 2;\nEND\n",
                &files(&[("a.rs", TEXT)])
            )
            .is_err()
        );
    }

    #[test]
    fn literal_line_breaks_match_crlf() {
        let text = "a\r\nb\r\n";
        assert_eq!(select("delete \"a\\nb\"", text), ["a\r\nb"]);
        assert_eq!(select("delete <<END\na\nb\nEND\n", text), [text]);
        assert!(resolve_in("delete /a\\nb/", &files(&[("a.rs", text)])).is_err());
    }

    #[test]
    fn file_step_narrows_to_one_file() {
        let set = files(&[("a.rs", "x\n"), ("b.rs", "y\nx\n")]);
        let m = resolve_in("delete all file:b.rs>/x/", &set).unwrap();
        assert_eq!(
            m,
            [Match {
                file: 1,
                range: 2..3,
                captures: vec![],
            }]
        );
        assert_eq!(
            error("delete file:c.rs>/x/", &[("a.rs", "x\n"), ("b.rs", "x\n")]),
            "error: script:1:8: file:c.rs is not in the file set: a.rs, b.rs; \
             add it with `file a.rs b.rs c.rs`"
        );
    }

    #[test]
    fn all_selects_across_files_in_order() {
        let set = files(&[("a.rs", "x x\n"), ("b.rs", "x\n")]);
        let m = resolve_in("delete all /x/", &set).unwrap();
        assert_eq!(
            m,
            [
                Match {
                    file: 0,
                    range: 0..1,
                    captures: vec![],
                },
                Match {
                    file: 0,
                    range: 2..3,
                    captures: vec![],
                },
                Match {
                    file: 1,
                    range: 0..1,
                    captures: vec![],
                },
            ]
        );
    }

    #[test]
    fn zero_matches_is_an_error_even_with_all() {
        assert_eq!(
            error("delete all /z/", &[("a.rs", "x\n"), ("b.rs", "y\n")]),
            "error: script:1:12: /z/ matches nothing in a.rs, b.rs; `show` prints the text to match against"
        );
    }

    #[test]
    fn ambiguity_lists_line_scoped_candidates() {
        assert_eq!(
            error("delete /let x/", &[("a.rs", TEXT)]),
            "error: script:1:8: /let x/ matches 2 items; add `all` or use one of:\n  \
             2>/let x/   a.rs:2\n  \
             7>/let x/   a.rs:7"
        );
        let text = "a\nb\na\nb\n";
        assert_eq!(
            error("delete <<END\na\nb\nEND\n", &[("a.rs", text)]),
            "error: script:1:8: <<END matches 2 items; add `all` or use one of:\n  \
             1-2><<END   a.rs:1-2\n  \
             3-4><<END   a.rs:3-4"
        );
    }

    #[test]
    fn matches_sharing_a_line_are_counted_not_listed() {
        assert_eq!(
            error("delete /x/", &[("a.rs", "x x\nx\n")]),
            "error: script:1:8: /x/ matches 3 items; add `all` or use one of:\n  \
             2>/x/   a.rs:2\n  \
             2 more share a line with another match; select longer text to pick one"
        );
        assert_eq!(
            error("delete /x/", &[("a.rs", "x x\n"), ("b.rs", "x\n")]),
            "error: script:1:8: /x/ matches 3 items; add `all` or use one of:\n  \
             file:b.rs>/x/   b.rs:1\n  \
             2 more share a line with another match; select longer text to pick one"
        );
    }

    #[test]
    fn matches_that_all_share_lines_list_no_candidates() {
        assert_eq!(
            error("delete \"a\"", &[("a.rs", "a a\nb\n")]),
            "error: script:1:8: \"a\" matches 2 items; add `all`, or select longer text; \
             matches on the same line can't be picked by scope"
        );
    }

    #[test]
    fn ambiguity_candidates_are_aligned() {
        let text = "\n".repeat(8) + "x\nx\n";
        assert_eq!(
            error("delete /x/", &[("a.rs", &text)]),
            "error: script:1:8: /x/ matches 2 items; add `all` or use one of:\n  \
             9>/x/    a.rs:9\n  \
             10>/x/   a.rs:10"
        );
    }

    #[test]
    fn ambiguity_across_files_scopes_candidates_by_file() {
        assert_eq!(
            error("delete /x/", &[("a.rs", "x\n"), ("b.rs", "y\nx\n")]),
            "error: script:1:8: /x/ matches 2 items; add `all` or use one of:\n  \
             file:a.rs>/x/   a.rs:1\n  \
             file:b.rs>/x/   b.rs:2"
        );
    }

    #[test]
    fn ambiguity_lists_at_most_ten_candidates() {
        let err = error("delete /x/", &[("a.rs", &"x\n".repeat(12))]);
        assert!(
            err.starts_with("error: script:1:8: /x/ matches 12 items;"),
            "{err}"
        );
        assert!(
            err.contains("\n  10>/x/   a.rs:10\n  … and 2 more"),
            "{err}"
        );
        assert!(!err.contains("11>"), "{err}");
    }

    #[test]
    fn query_selects_sel_captures() {
        assert_eq!(
            select(
                "delete all query{(let_declaration pattern: (identifier) @sel)}",
                RUST
            ),
            ["x", "x"]
        );
        assert_eq!(
            select(
                r#"delete all query{((identifier) @sel (#eq? @sel "src"))}"#,
                RUST
            )
            .len(),
            2
        );
    }

    #[test]
    fn query_without_sel_takes_the_outermost_capture() {
        assert_eq!(
            select(
                "delete fn:main>query{(let_declaration pattern: (identifier) @p) @whole}",
                RUST
            ),
            ["let x = 1;"]
        );
    }

    #[test]
    fn query_matches_are_distinct_and_in_order() {
        assert_eq!(
            select(
                "delete all query{(identifier) @a (identifier) @b}",
                "fn a() { b; c; }\n"
            )
            .len(),
            3
        );
        let found = select("delete all impl:Parser>query{(identifier) @sel}", RUST);
        assert_eq!(found, ["new", "src", "src", "parse", "x"].map(String::from));
    }

    #[test]
    fn invalid_queries_are_errors() {
        assert_eq!(
            error("delete query{(nope) @sel}", &[("a.rs", RUST)]),
            "error: script:1:8: invalid rust query: unknown node type `nope` at column 2"
        );
        assert_eq!(
            error(
                "delete query{(identifier) @sel (#eq? @sel)}",
                &[("a.rs", RUST)]
            ),
            "error: script:1:8: invalid rust query: bad predicate at column 1; \
             predicates look like (#eq? @capture \"text\")"
        );
        let err = resolve_in("delete query{(identifier}", &files(&[("a.rs", RUST)])).unwrap_err();
        assert!(
            matches!(err.kind, ExecErrorKind::InvalidQuery { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn patterns_select_matching_code() {
        let text = "fn a() {\n    foo(1);\n}\n\nfn b() {\n    foo(2);\n    bar(3);\n}\n";
        assert_eq!(select("delete all `foo(@x)`", text), ["foo(1)", "foo(2)"]);
        assert_eq!(select("delete fn:b>`foo(@x)`", text), ["foo(2)"]);
        assert_eq!(
            select("delete all `foo(@x)`.lines", text),
            ["    foo(1);\n", "    foo(2);\n"]
        );
    }

    #[test]
    fn matches_carry_captures_from_every_pattern_step() {
        let text = "impl Parser {\n    fn new() {\n        foo(1);\n    }\n}\n";
        let found = resolve_in(
            "delete `impl @t { @_... }`>fn:new>`foo(@x)`",
            &files(&[("a.rs", text)]),
        )
        .unwrap();
        let captures: Vec<(&str, &str)> = found[0]
            .captures
            .iter()
            .map(|(n, r)| (n.as_str(), &text[r.clone()]))
            .collect();
        assert_eq!(captures, [("t", "Parser"), ("x", "1")]);
        let parts = resolve_in("delete `foo(@x)`.lines", &files(&[("a.rs", text)])).unwrap();
        assert_eq!(parts[0].captures.len(), 1);
    }

    #[test]
    fn a_capture_name_in_two_steps_is_an_error() {
        let text = "fn f() {\n    foo(bar(1));\n}\n";
        assert_eq!(
            error("delete `foo(@x)`>`bar(@x)`", &[("a.rs", text)]),
            "error: script:1:8: `@x` is captured by two pattern steps; rename one of them"
        );
    }

    #[test]
    fn ranges_of_patterns_capture_at_both_ends() {
        let text = "fn f() {\n    foo(1);\n    x();\n    bar(2);\n}\n";
        let found = resolve_in("delete `foo(@a)`..`bar(@b)`", &files(&[("a.rs", text)])).unwrap();
        let names: Vec<(&str, &str)> = found[0]
            .captures
            .iter()
            .map(|(n, r)| (n.as_str(), &text[r.clone()]))
            .collect();
        assert_eq!(names, [("a", "1"), ("b", "2")]);
        assert_eq!(
            error("delete `foo(@a)`..`bar(@a)`", &[("a.rs", text)]),
            "error: script:1:8: `@a` is captured by two pattern steps; rename one of them"
        );
    }

    #[test]
    fn ambiguous_patterns_list_candidates() {
        let text = "fn a() {\n    foo(1);\n}\n\nfn b() {\n    foo(2);\n}\n";
        assert_eq!(
            error("delete `foo(@x)`", &[("a.rs", text)]),
            "error: script:1:8: `foo(@x)` matches 2 items; add `all` or use one of:\n  \
            fn:a>`foo(@x)`   a.rs:2\n  fn:b>`foo(@x)`   a.rs:6"
        );
    }

    #[test]
    fn patterns_skip_files_whose_language_cannot_parse_them() {
        let found = resolve_in(
            "delete all `let @x = @e;`",
            &files(&[
                ("a.rs", "fn f() {\n    let x = 1;\n}\n"),
                ("b.py", "x = 1\n"),
                ("c.md", "# x\n"),
            ]),
        )
        .unwrap();
        assert_eq!(
            found,
            [Match {
                file: 0,
                range: 13..23,
                captures: vec![("x".into(), 17..18), ("e".into(), 21..22)],
            }]
        );
    }

    #[test]
    fn invalid_patterns_are_errors() {
        assert_eq!(
            error("delete `foo@bar(1)`", &[("a.rs", TEXT)]),
            "error: script:1:8: `foo@bar(1)` has `@bar` where a whole node must be; \
             write `@@` for a literal `@`"
        );
        let err = resolve_in("delete `fn (@a`", &files(&[("a.rs", TEXT)])).unwrap_err();
        assert!(
            matches!(err.kind, ExecErrorKind::InvalidPattern { .. }),
            "{err:?}"
        );
        assert!(
            err.render("delete `fn (@a`")
                .contains("doesn't parse as rust"),
            "{err:?}"
        );
    }

    #[test]
    fn patterns_need_a_programming_language() {
        assert_eq!(
            error(
                "delete `foo(@x)`",
                &[("a.md", "# foo(1)\n"), ("b.txt", "foo(1)\n")]
            ),
            "error: script:1:8: `foo(@x)` needs a programming language, but a.md, b.txt has \
             none; use a regex or literal, or --lang"
        );
    }

    #[test]
    fn errors_fence_patterns_that_hold_backquotes() {
        assert_eq!(fenced("f(@x)"), "`f(@x)`");
        assert_eq!(fenced("f(`a`)"), "`` f(`a`) ``");
        assert_eq!(fenced("``"), "``` `` ```");
    }

    #[test]
    fn query_works_in_every_detected_language() {
        let py = "def main():\n    return 1\n";
        assert_eq!(
            select_in("delete query{(return_statement) @sel}", "a.py", py),
            ["return 1"]
        );
        assert_eq!(
            error("delete query{(identifier) @sel}", &[("a.txt", RUST)]),
            "error: script:1:8: query{(identifier) @sel} needs a language, but a.txt has none; use --lang"
        );
    }

    #[test]
    fn parts_narrow_syntax_items() {
        assert_eq!(select("delete fn:main.body", RUST), ["    let x = 1;\n"]);
        assert_eq!(
            select("delete impl:Parser>fn:new.params", RUST),
            ["src: &str"]
        );
        assert_eq!(
            select("delete all fn:*.name", RUST),
            ["new", "parse", "new", "main"]
        );
        assert_eq!(select("delete fn:main.body>var:x.name", RUST), ["x"]);
        assert_eq!(
            select("delete fn:main.body.lines", RUST),
            ["    let x = 1;\n"]
        );
    }

    #[test]
    fn parts_need_syntax_items_that_have_them() {
        assert_eq!(
            error("delete fn:main.doc", &[("a.rs", RUST)]),
            "error: script:1:8: fn:main has no .doc; it has .body .sig .params .name .whole .lines"
        );
        assert_eq!(
            error("delete import:std::fmt.body", &[("a.rs", RUST)]),
            "error: script:1:8: import:std::fmt has no .body; it has .sig .name .whole .lines"
        );
        assert_eq!(
            error("delete /x/.body", &[("a.rs", RUST)]),
            "error: script:1:8: .body needs a syntax item, e.g. fn:NAME.body"
        );
        assert_eq!(
            error("delete fn:main.body.name", &[("a.rs", RUST)]),
            "error: script:1:8: .name needs a syntax item, e.g. fn:NAME.name"
        );
        assert_eq!(
            error("delete fn:main.lines.body", &[("a.rs", RUST)]),
            "error: script:1:8: .body needs a syntax item, e.g. fn:NAME.body"
        );
        assert_eq!(
            error("delete fn:main.ret", &[("a.rs", RUST)]),
            "error: script:1:8: fn:main has no .ret; it has .body .sig .params .name .whole .lines"
        );
        assert_eq!(
            error("delete /x/.type", &[("a.rs", RUST)]),
            "error: script:1:8: .type needs a syntax item, e.g. fn:NAME.type"
        );
    }

    const RUST: &str = "\
use std::fmt;

impl Parser {
    /// Makes a parser.
    pub fn new(src: &str) -> Self {
        Parser { src }
    }

    fn parse(&mut self) {
        let x = self.next();
    }
}

impl Lexer {
    fn new() -> Self {
        Lexer
    }
}

fn main() {
    let x = 1;
}
";

    #[test]
    fn syntax_selects_items_by_kind_and_name() {
        assert_eq!(
            select("delete fn:main", RUST),
            ["fn main() {\n    let x = 1;\n}"]
        );
        assert_eq!(select("delete import:std::fmt", RUST), ["use std::fmt;"]);
        assert_eq!(
            select("delete impl:Parser>fn:new", RUST),
            [
                "/// Makes a parser.\n    pub fn new(src: &str) -> Self {\n        Parser { src }\n    }"
            ]
        );
    }

    #[test]
    fn syntax_names_take_wildcards() {
        assert_eq!(select("delete all fn:*a*", RUST).len(), 2);
        assert_eq!(select("delete all impl:*er", RUST).len(), 2);
        assert_eq!(select("delete all fn:*", RUST).len(), 4);
    }

    #[test]
    fn syntax_steps_nest_with_other_primaries() {
        assert_eq!(select("delete fn:parse>/\\bx\\b/", RUST), ["x"]);
        assert_eq!(
            select("delete 14-18>fn:new", RUST),
            ["fn new() -> Self {\n        Lexer\n    }"]
        );
        assert_eq!(select("delete all fn:*>var:x", RUST).len(), 2);
        assert_eq!(select("delete all fn:main.lines", RUST).len(), 3);
    }

    #[test]
    fn ambiguous_syntax_selectors_suggest_enclosing_items() {
        assert_eq!(
            error("delete fn:new", &[("a.rs", RUST)]),
            "error: script:1:8: fn:new matches 2 items; add `all` or use one of:\n  \
             impl:Parser>fn:new   a.rs:4-7\n  \
             impl:Lexer>fn:new    a.rs:15-17"
        );
        assert_eq!(
            error("delete var:x", &[("a.rs", RUST)]),
            "error: script:1:8: var:x matches 2 items; add `all` or use one of:\n  \
             fn:parse>var:x   a.rs:10\n  \
             fn:main>var:x    a.rs:21"
        );
    }

    #[test]
    fn candidates_fall_back_to_files_then_lines() {
        let a = "fn new() {}\n";
        assert_eq!(
            error("delete fn:new", &[("a.rs", a), ("b.rs", a)]),
            "error: script:1:8: fn:new matches 2 items; add `all` or use one of:\n  \
             file:a.rs>fn:new   a.rs:1\n  \
             file:b.rs>fn:new   b.rs:1"
        );
        let twice = "impl A {\n    fn f() {}\n}\nimpl A {\n    fn f() {}\n}\n";
        assert_eq!(
            error("delete fn:f", &[("a.rs", twice)]),
            "error: script:1:8: fn:f matches 2 items; add `all` or use one of:\n  \
             2>fn:f   a.rs:2\n  \
             5>fn:f   a.rs:5"
        );
    }

    #[test]
    fn wildcard_candidates_name_their_items() {
        assert_eq!(
            error("delete fn:*", &[("a.rs", "fn a() {}\n\nfn b() {}\n")]),
            "error: script:1:8: fn:* matches 2 items; add `all` or use one of:\n  \
             fn:a   a.rs:1\n  \
             fn:b   a.rs:3"
        );
        assert_eq!(
            error(
                "delete impl:A>fn:*.body",
                &[("a.rs", "impl A {\n    fn f() {}\n    fn g() {}\n}\n")]
            ),
            "error: script:1:8: impl:A>fn:*.body matches 2 items; add `all` or use one of:\n  \
             impl:A>fn:f.body   a.rs:2\n  \
             impl:A>fn:g.body   a.rs:3"
        );
    }

    #[test]
    fn wildcard_candidates_sharing_a_name_are_scoped() {
        let text = "impl A {\n    fn f() {}\n}\nimpl B {\n    fn f() {}\n}\nfn g() {}\n";
        assert_eq!(
            error("delete fn:*", &[("a.rs", text)]),
            "error: script:1:8: fn:* matches 3 items; add `all` or use one of:\n  \
             impl:A>fn:f   a.rs:2\n  \
             impl:B>fn:f   a.rs:5\n  \
             fn:g          a.rs:7"
        );
        assert_eq!(
            error(
                "delete fn:*",
                &[
                    ("a.rs", "fn new() {}\n"),
                    ("b.rs", "fn new() {}\nfn old() {}\n")
                ]
            ),
            "error: script:1:8: fn:* matches 3 items; add `all` or use one of:\n  \
             file:a.rs>fn:new   a.rs:1\n  \
             file:b.rs>fn:new   b.rs:1\n  \
             fn:old             b.rs:2"
        );
    }

    #[test]
    fn candidates_across_files_use_enclosing_items() {
        let parser = "impl Parser {\n    fn new() {}\n}\n";
        let lexer = "impl Lexer {\n    fn new() {}\n}\n";
        assert_eq!(
            error(
                "delete fn:new",
                &[("src/parser.rs", parser), ("src/lexer.rs", lexer)]
            ),
            "error: script:1:8: fn:new matches 2 items; add `all` or use one of:\n  \
             impl:Parser>fn:new   src/parser.rs:2\n  \
             impl:Lexer>fn:new    src/lexer.rs:2"
        );
    }

    /// The candidates an ambiguous `script` lists, each checked to pick
    /// exactly one match.
    fn listed(script: &str, texts: &[(&str, &str)]) -> Vec<String> {
        let set = files(texts);
        let err = resolve_in(script, &set).unwrap_err();
        let ExecErrorKind::Ambiguous { candidates, .. } = err.kind else {
            panic!("not ambiguous: {}", err.render(script));
        };
        let listed: Vec<String> = candidates.listed.into_iter().map(|(c, _)| c).collect();
        for c in &listed {
            let picked = resolve_in(&format!("delete {c}"), &set);
            assert!(matches!(&picked, Ok(m) if m.len() == 1), "{c}: {picked:?}");
        }
        listed
    }

    #[test]
    fn nested_candidates_scope_the_last_step() {
        let m = "mod m {\n    fn a() {}\n    fn b() {}\n}\n";
        assert_eq!(
            listed("delete mod:m>/fn/", &[("a.rs", m)]),
            ["mod:m>fn:a>/fn/", "mod:m>fn:b>/fn/"]
        );
        let twice = "impl A {\n    fn f() { x }\n}\nimpl A {\n    fn f() { x }\n}\n";
        assert_eq!(
            listed("delete impl:A>fn:f>\"x\"", &[("a.rs", twice)]),
            ["impl:A>fn:f>2>\"x\"", "impl:A>fn:f>5>\"x\""]
        );
        assert_eq!(
            listed("delete 1-4>/let/", &[("a.rs", TEXT)]),
            ["1-4>var:x>/let/", "1-4>var:y>/let/"]
        );
    }

    #[test]
    fn nested_candidates_put_the_file_first() {
        let one = "mod m {\n    fn a() {}\n}\n";
        assert_eq!(
            listed("delete mod:m>fn:a", &[("a.rs", one), ("b.rs", one)]),
            ["file:a.rs>mod:m>fn:a", "file:b.rs>mod:m>fn:a"]
        );
        let two = "mod m {\n    fn a() {}\n}\nmod m {\n    fn a() {}\n}\n";
        assert_eq!(
            listed("delete mod:m>fn:a", &[("a.rs", two), ("b.rs", one)]),
            [
                "file:a.rs>mod:m>2>fn:a",
                "file:a.rs>mod:m>5>fn:a",
                "file:b.rs>mod:m>fn:a"
            ]
        );
        assert_eq!(
            listed(
                "delete file:a.rs>/x/",
                &[("a.rs", "x\nx\n"), ("b.rs", "x\n")]
            ),
            ["file:a.rs>1>/x/", "file:a.rs>2>/x/"]
        );
    }

    #[test]
    fn line_scopes_cover_the_whole_item() {
        let text = "impl A {\n    fn f() {}\n}\nimpl A {\n    fn g() {}\n}\n";
        assert_eq!(
            listed("delete impl:A.body", &[("a.rs", text)]),
            ["1-3>impl:A.body", "4-6>impl:A.body"]
        );
    }

    const IMPLS: &str = "struct S;\nimpl S {\n    fn a() {}\n}\nimpl Display for S {\n    fn fmt() {}\n}\nimpl Debug for S {}\n";

    #[test]
    fn trait_impls_match_by_full_name_or_self_type() {
        assert_eq!(
            select("delete impl:\"Display for S\"", IMPLS),
            ["impl Display for S {\n    fn fmt() {}\n}"]
        );
        assert_eq!(select("delete all impl:\"* for S\"", IMPLS).len(), 2);
        assert_eq!(select("delete all impl:S", IMPLS).len(), 3);
        assert_eq!(select("delete impl:S>fn:fmt", IMPLS), ["fn fmt() {}"]);
        assert!(
            error("delete impl:SS", &[("a.rs", IMPLS)]).contains("did you mean impl:S ("),
            "a close self type is suggested"
        );
    }

    #[test]
    fn go_methods_match_by_full_name_or_method_name() {
        let text =
            "package a\n\nfunc (p *P) Parse() {}\n\nfunc (s S) Parse() {}\n\nfunc Run() {}\n";
        let lines = |script: &str| select_in(script, "a.go", text);
        assert_eq!(lines("delete fn:\"S.Parse\""), ["func (s S) Parse() {}"]);
        assert_eq!(lines("delete all fn:Parse").len(), 2);
        let err = error("delete fn:Parse", &[("a.go", text)]);
        assert!(
            err.contains("fn:\"P.Parse\"") && err.contains("fn:\"S.Parse\""),
            "{err}"
        );
    }

    #[test]
    fn trait_impl_candidates_use_their_full_names() {
        assert_eq!(
            listed("delete impl:S", &[("a.rs", IMPLS)]),
            [
                "2-4>impl:S",
                "impl:\"Display for S\"",
                "impl:\"Debug for S\""
            ]
        );
    }

    #[test]
    fn a_name_under_another_kind_is_suggested_first() {
        let text = "enum E {\n    A,\n}\nstruct S;\n";
        assert_eq!(
            error("delete struct:E", &[("a.rs", text)]),
            "error: script:1:8: struct:E matches nothing in a.rs; did you mean enum:E (1-3)?"
        );
        assert!(
            error(
                "delete mod:m>fn:S",
                &[("a.rs", "mod m {\n    struct S;\n}\n")]
            )
            .ends_with("did you mean mod:m>struct:S (2)?")
        );
    }

    #[test]
    fn a_range_end_nested_like_its_start_suggests_the_plain_range() {
        assert!(
            error("delete fn:a>\"let x\"..fn:a>\"let y\"", &[("a.rs", TEXT)]).ends_with(
                "; `..` binds tighter than `>`: did you mean fn:a>\"let x\"..\"let y\"?"
            )
        );
        let text = "mod m {\n    fn a() {\n        x;\n        y;\n    }\n}\n";
        assert!(
            error(
                "delete mod:m>fn:a>\"x\"..mod:m>fn:a>\"y\"",
                &[("a.rs", text)]
            )
            .ends_with("did you mean mod:m>fn:a>\"x\"..\"y\"?")
        );
    }

    #[test]
    fn a_literal_matching_as_escaped_source_text_is_suggested() {
        let text = "let s = \"a\\nb\\t\";\n";
        assert_eq!(
            error("delete \"a\\nb\\t\"", &[("a.rs", text)]),
            "error: script:1:8: \"a\\nb\\t\" matches nothing in a.rs; \
             as source text it matches at 1: \"a\\\\nb\\\\t\""
        );
    }

    #[test]
    fn no_match_suggests_a_near_literal_or_regex() {
        assert_eq!(
            error(r#"delete "LET  y""#, &[("a.rs", TEXT)]),
            "error: script:1:8: \"LET  y\" matches nothing in a.rs; \
             ignoring case and spacing, it matches at 3"
        );
        assert_eq!(
            error("delete /LET Y/", &[("a.rs", TEXT), ("b.rs", "")]),
            "error: script:1:8: /LET Y/ matches nothing in a.rs, b.rs; \
             it matches case-insensitively at a.rs:3 (add the i flag)"
        );
    }

    #[test]
    fn nested_no_match_names_the_spans_searched() {
        assert_eq!(
            error("delete fn:a>/zzz/", &[("a.rs", TEXT)]),
            "error: script:1:8: fn:a>/zzz/ matches nothing in a.rs; it searched 1-4"
        );
        let many = "fn f() {}\n".repeat(5);
        assert_eq!(
            error("delete all fn:f>/zzz/", &[("a.rs", &many)]),
            "error: script:1:12: fn:f>/zzz/ matches nothing in a.rs; it searched 1, 2, 3, ..."
        );
    }

    #[test]
    fn invalid_queries_suggest_close_names() {
        assert_eq!(
            error("delete query{(call_expresion) @sel}", &[("a.rs", RUST)]),
            "error: script:1:8: invalid rust query: unknown node type `call_expresion` at column 2; \
             did you mean `call_expression`?"
        );
        assert_eq!(
            error(
                "delete query{(function_item nme: (identifier)) @sel}",
                &[("a.rs", RUST)]
            ),
            "error: script:1:8: invalid rust query: unknown field `nme` at column 16; \
             did you mean `name`?"
        );
    }

    #[test]
    fn no_match_suggests_a_close_name() {
        assert_eq!(
            error("delete fn:prase", &[("a.rs", RUST)]),
            "error: script:1:8: fn:prase matches nothing in a.rs; did you mean fn:parse (9-11)?"
        );
        assert_eq!(
            error("delete impl:Lexer>fn:nwe", &[("a.rs", RUST)]),
            "error: script:1:8: impl:Lexer>fn:nwe matches nothing in a.rs; \
             did you mean impl:Lexer>fn:new (15-17)?"
        );
        assert_eq!(
            error(
                "delete fn:new",
                &[("a.rs", "fn old() {}\n"), ("b.rs", "fn neww() {}\n")]
            ),
            "error: script:1:8: fn:new matches nothing in a.rs, b.rs; did you mean fn:neww (b.rs:1)?"
        );
        assert_eq!(
            error("delete fn:zzzzzz", &[("a.rs", RUST)]),
            "error: script:1:8: fn:zzzzzz matches nothing in a.rs; `outline` lists the items"
        );
    }

    #[test]
    fn a_nested_no_match_names_and_hints_its_first_failing_step() {
        let module = "mod tests {\n    fn exec() {}\n}\n";
        assert_eq!(
            error("delete fn:tests>fn:exec", &[("a.rs", module)]),
            "error: script:1:8: fn:tests matches nothing in a.rs; \
             did you mean mod:tests>fn:exec (1-3)?"
        );
        assert_eq!(
            error("delete impl:Lexr>fn:new>\"x\"", &[("a.rs", RUST)]),
            "error: script:1:8: impl:Lexr matches nothing in a.rs; \
             did you mean impl:Lexer>fn:new>\"x\" (14-18)?"
        );
        assert_eq!(
            error("delete impl:Lexer>fn:nwe>\"x\"", &[("a.rs", RUST)]),
            "error: script:1:8: impl:Lexer>fn:nwe matches nothing in a.rs; \
             did you mean impl:Lexer>fn:new>\"x\" (15-17)?"
        );
        assert_eq!(
            error("delete fn:zzzzzz>\"x\"", &[("a.rs", RUST)]),
            "error: script:1:8: fn:zzzzzz matches nothing in a.rs; `outline` lists the items"
        );
        // The fix replaces the failing step, not a later step written the same.
        assert_eq!(
            error(
                "delete fn:m>fn:m",
                &[("a.rs", "mod m {\n    struct s;\n}\n")]
            ),
            "error: script:1:8: fn:m matches nothing in a.rs; did you mean mod:m>fn:m (1-3)?"
        );
    }

    #[test]
    fn no_match_suggests_the_name_without_generics_or_paths() {
        let text = "struct Log<'a>(&'a str);\n\nimpl Log<'_> {\n    fn f() {}\n}\n\nimpl fmt::Display for Log<'_> {}\n";
        assert_eq!(
            error("delete impl:\"Log<'_>\"", &[("a.rs", text)]),
            "error: script:1:8: impl:\"Log<'_>\" matches nothing in a.rs; did you mean impl:Log (3-5)?"
        );
        assert_eq!(
            error(
                "delete impl:\"fmt::Display for Log<'_>\"",
                &[("a.rs", text)]
            ),
            "error: script:1:8: impl:\"fmt::Display for Log<'_>\" matches nothing in a.rs; \
             did you mean impl:\"Display for Log\" (7)?"
        );
        assert_eq!(
            error("delete impl:\"Lexer<'a>\">fn:new>\"x\"", &[("a.rs", RUST)]),
            "error: script:1:8: impl:\"Lexer<'a>\" matches nothing in a.rs; \
             did you mean impl:Lexer>fn:new>\"x\" (14-18)?"
        );
    }

    #[test]
    fn unknown_kinds_list_the_languages_kinds() {
        assert_eq!(
            error("delete class:Parser", &[("a.rs", RUST)]),
            "error: script:1:8: rust has no `class` items; use one of: \
             fn, struct, enum, variant, trait, impl, type, const, var, field, mod, import"
        );
    }

    #[test]
    fn syntax_steps_need_a_language() {
        assert_eq!(
            error("delete fn:main", &[("a.txt", RUST), ("b.txt", RUST)]),
            "error: script:1:8: fn:main needs a language, but a.txt, b.txt has none; use --lang"
        );
        let found =
            resolve_in("delete fn:main", &files(&[("a.txt", RUST), ("b.rs", RUST)])).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].file, 1);
    }
}
