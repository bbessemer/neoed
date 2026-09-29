//! Resolving selectors to spans of files (command-language spec, §3).

use std::cell::OnceCell;
use std::collections::HashMap;
use std::ops::Range;

use regex::Regex;
use std::cmp::Reverse;

use tree_sitter::{Query, QueryCursor, QueryError, QueryErrorKind, StreamingIterator, Tree};

use crate::buffer::{Buffer, LineEnding};
use crate::exec::{Candidates, ExecError, ExecErrorKind as E};
use crate::lang::Language;
use crate::script::ast::{LineNo, Part, Pattern, Primary, Step, Target, TextKind};
use crate::syntax::{self, Item};
use crate::text::{full_lines, strip_indent};

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
        }
    }

    /// The syntax items of the text; `None` if the file's language has no
    /// selector query (or it has no language).
    pub fn items(&self) -> Option<&[Item]> {
        let query = self.lang?.selectors()?;
        let tree = self.tree()?;
        Some(
            self.items
                .get_or_init(|| syntax::items(query, tree, &self.text)),
        )
    }

    /// The syntax tree of the text, parsed on first use; `None` without a
    /// language.
    pub fn tree(&self) -> Option<&Tree> {
        let lang = self.lang?;
        Some(self.tree.get_or_init(|| lang.parse(&self.text)))
    }
}

/// A selected span of `files[file]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub file: usize,
    pub range: Range<usize>,
}

/// Resolves `target` against every file in `files`, enforcing the ambiguity
/// rules of §3.5. `src` is the script, for error messages.
pub fn resolve(target: &Target, files: &[&SourceFile], src: &str) -> Result<Vec<Match>, ExecError> {
    let whole = files
        .iter()
        .enumerate()
        .map(|(file, f)| Match {
            file,
            range: 0..f.text.len(),
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
    let mut matches = start;
    let mut parents = Vec::new();
    for step in &target.selector.steps {
        let next = resolve_step(step, files, &matches).map_err(error)?;
        parents = std::mem::replace(&mut matches, next);
    }
    let selector = &src[span.clone()];
    match matches.len() {
        0 => Err(error(E::NoMatch {
            selector: selector.into(),
            files: file_list(&files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>()),
            hint: target
                .selector
                .steps
                .last()
                .map(|step| hint(step, files, &parents, selector))
                .unwrap_or_default(),
        })),
        1 => Ok(matches),
        _ if target.all => Ok(matches),
        _ => Err(error(E::Ambiguous {
            selector: selector.into(),
            candidates: candidates(&matches, files, selector, target.selector.steps.last()),
        })),
    }
}

fn resolve_step(step: &Step, files: &[&SourceFile], parents: &[Match]) -> Result<Vec<Match>, E> {
    // Only a syntax item has parts other than `.lines`, and a part's span is
    // no longer an item.
    let mut item = matches!(step.primary, Primary::Syntax { .. });
    for part in &step.parts {
        if *part != Part::Lines && !item {
            return Err(E::PartNeedsItem {
                part: part_name(*part).into(),
            });
        }
        item = false;
    }
    let matcher = Matcher::new(&step.primary, files, parents)?;
    let mut out: Vec<Match> = Vec::new();
    for parent in parents {
        let f = &files[parent.file];
        for mut range in matcher.find(f, parent.range.clone()) {
            let mut item = match &step.primary {
                Primary::Syntax { kind, .. } => f
                    .items()
                    .unwrap_or_default()
                    .iter()
                    .find(|i| i.kind == kind && i.range == range),
                _ => None,
            };
            for part in &step.parts {
                range = match (part, item.take()) {
                    (Part::Lines, _) => full_lines(&f.text, range),
                    (part, Some(item)) => {
                        syntax::part(item, *part, &f.text).ok_or_else(|| E::MissingPart {
                            item: syntax::selector(item.kind, &item.name),
                            part: part_name(*part).into(),
                            has: parts_of(item),
                        })?
                    }
                    (Part::Refs | Part::Def, _) => unreachable!("resolved by the executor"),
                    (_, None) => unreachable!("checked above"),
                };
            }
            let m = Match {
                file: parent.file,
                range,
            };
            if out.last() != Some(&m) {
                out.push(m);
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
    Regex(Regex),
    Str(&'a str),
    Heredoc {
        lines: Vec<String>,
        raw: bool,
    },
    File(&'a str),
    Syntax {
        kind: &'a str,
        name: &'a str,
    },
    /// The query compiled for each searched language.
    Query(Vec<(Language, Query)>),
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
            Primary::Regex(pattern) => Matcher::Regex(
                pattern
                    .regex()
                    .expect("regexes are validated when the script is parsed"),
            ),
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
            Primary::Syntax { kind, name } => {
                check_syntax(kind, name, files, parents)?;
                Matcher::Syntax { kind, name }
            }
            Primary::Query(source) => Matcher::Query(compile_query(source, files, parents)?),
            Primary::Range { from, to } => Matcher::Range(
                Box::new(Matcher::new(from, files, parents)?),
                Box::new(Matcher::new(to, files, parents)?),
            ),
        })
    }

    fn find(&self, f: &SourceFile, parent: Range<usize>) -> Vec<Range<usize>> {
        let within = |r: &Range<usize>| parent.start <= r.start && r.end <= parent.end;
        match self {
            Matcher::Lines { start, end } => {
                let count = f.buffer.line_count();
                let (Some(first), Some(last)) =
                    (line_index(*start, count), line_index(*end, count))
                else {
                    return Vec::new();
                };
                let range = line_range(&f.buffer, first).start..line_range(&f.buffer, last).end;
                if within(&range) {
                    vec![range]
                } else {
                    Vec::new()
                }
            }
            Matcher::Regex(re) => re
                .find_iter(&f.text[parent.clone()])
                .map(|m| parent.start + m.start()..parent.start + m.end())
                .collect(),
            Matcher::Str(needle) => {
                let needle = match f.buffer.line_ending() {
                    LineEnding::Lf => needle.to_string(),
                    LineEnding::Crlf => needle.replace('\n', "\r\n"),
                };
                if needle.is_empty() {
                    return Vec::new();
                }
                f.text[parent.clone()]
                    .match_indices(&needle)
                    .map(|(i, _)| parent.start + i..parent.start + i + needle.len())
                    .collect()
            }
            Matcher::Heredoc { lines, raw } => {
                let whole: Vec<(Range<usize>, &str)> = (0..f.buffer.line_count())
                    .map(|i| line_range(&f.buffer, i))
                    .filter(|r| within(r))
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
            Matcher::Range(from, to) => {
                let ends = to.find(f, parent.clone());
                let mut ranges = Vec::new();
                let mut searched_to = parent.start;
                for start in from.find(f, parent.clone()) {
                    if start.start < searched_to {
                        continue;
                    }
                    let Some(end) = ends.iter().find(|e| e.start >= start.end) else {
                        break;
                    };
                    searched_to = end.end;
                    ranges.push(start.start..end.end);
                }
                ranges
            }
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
            Matcher::Syntax { kind, name } => f
                .items()
                .unwrap_or_default()
                .iter()
                .filter(|i| i.kind == *kind && syntax::name_matches(name, &i.name))
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
        let error = match lang.selectors() {
            None => E::Unsupported {
                what: format!("`{}` in {lang} files", syntax::selector(kind, name)),
                instead: r#"use a line, /regex/, "literal" or query{} selector"#,
            },
            Some(query) => {
                let kinds = syntax::kinds(query);
                if kinds.contains(&kind) {
                    return Ok(());
                }
                E::UnknownKind {
                    kind: kind.into(),
                    lang: lang.to_string(),
                    kinds: kinds.join(", "),
                }
            }
        };
        first_error.get_or_insert(error);
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

/// The parts `item` has, as `.body .sig ...`.
fn parts_of(item: &Item) -> String {
    [
        item.body.is_some().then_some(".body"),
        Some(".sig"),
        item.params.is_some().then_some(".params"),
        Some(".name"),
        item.doc.is_some().then_some(".doc"),
        Some(".lines"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ")
}

/// The fix for a `step` that matched nothing within `parents` (§7): a close
/// syntax name, a literal match ignoring case and spacing, a case-insensitive
/// regex match, the spans a nested step searched, or where to look.
pub(crate) fn hint(
    step: &Step,
    files: &[&SourceFile],
    parents: &[Match],
    selector: &str,
) -> String {
    if let Some(hint) = close_name(step, files, parents, selector) {
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
    match &step.primary {
        Primary::Literal(text) => {
            if let Some(m) = near_literal(&text.value, files, parents) {
                return format!(
                    "; ignoring case and spacing, it matches at {}",
                    location(&m)
                );
            }
        }
        Primary::Regex(pattern) if !pattern.flags.case_insensitive => {
            if let Some(m) = case_insensitive_match(pattern, files, parents) {
                return format!(
                    "; it matches case-insensitively at {} (add the i flag)",
                    location(&m)
                );
            }
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
        })
    })
}

/// The first match of `pattern` within `parents` when case is ignored.
fn case_insensitive_match(
    pattern: &Pattern,
    files: &[&SourceFile],
    parents: &[Match],
) -> Option<Match> {
    let mut folded = pattern.clone();
    folded.flags.case_insensitive = true;
    let re = folded.regex().ok()?;
    parents.iter().find_map(|p| {
        let found = re.find(&files[p.file].text[p.range.clone()])?;
        let start = p.range.start + found.start();
        Some(Match {
            file: p.file,
            range: start..(start + found.len()).max(start + 1),
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
) -> Option<String> {
    let Primary::Syntax { kind, name } = &step.primary else {
        return None;
    };
    if name.contains('*') {
        return None;
    }
    let limit = (name.chars().count() / 3).max(2);
    let best = parents
        .iter()
        .flat_map(|p| {
            files[p.file]
                .items()
                .unwrap_or_default()
                .iter()
                .filter(|i| i.kind == kind && p.range.start <= i.range.start)
                .filter(|i| i.range.end <= p.range.end)
                .map(|i| (syntax::distance(name, &i.name), p.file, i))
        })
        .filter(|(d, ..)| *d <= limit)
        .min_by_key(|(d, ..)| *d);
    let (_, file, item) = best?;
    let written = syntax::selector(kind, name);
    let fixed = syntax::selector(kind, &item.name);
    let suggestion = match selector.rfind(&written) {
        Some(i) => format!(
            "{}{fixed}{}",
            &selector[..i],
            &selector[i + written.len()..]
        ),
        None => fixed,
    };
    let f = files[file];
    let lines = line_numbers(&f.buffer, &item.range);
    let location = if files.len() > 1 {
        format!("{}:{lines}", f.path)
    } else {
        lines
    };
    Some(format!("; did you mean {suggestion} ({location})?"))
}

fn line_index(n: LineNo, count: usize) -> Option<usize> {
    match n {
        LineNo::Number(n) if (1..=count).contains(&n) => Some(n - 1),
        LineNo::Last if count > 0 => Some(count - 1),
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

fn part_name(part: Part) -> &'static str {
    match part {
        Part::Body => "body",
        Part::Sig => "sig",
        Part::Params => "params",
        Part::Name => "name",
        Part::Doc => "doc",
        Part::Lines => "lines",
        Part::Refs => "refs",
        Part::Def => "def",
    }
}

/// A selector for each match (§3.5): `selector`, with a wildcard name in
/// its `last` step replaced by the match's item name. Among the matches with
/// the same selector, it's nested in the match's nearest enclosing item if no
/// other match shares it, otherwise scoped to its file if no other match
/// shares that, otherwise to its lines.
fn candidates(
    matches: &[Match],
    files: &[&SourceFile],
    selector: &str,
    last: Option<&Step>,
) -> Candidates {
    let named: Vec<String> = matches
        .iter()
        .map(|m| {
            last.and_then(|step| named(selector, step, files[m.file], &m.range))
                .unwrap_or_else(|| selector.to_string())
        })
        .collect();
    let enclosing: Vec<Option<String>> = matches
        .iter()
        .map(|m| enclosing(files[m.file], &m.range))
        .collect();
    let all: Vec<(String, String)> = (0..matches.len())
        .map(|i| {
            let (m, selector) = (&matches[i], &named[i]);
            let f = &files[m.file];
            let lines = line_numbers(&f.buffer, &m.range);
            let peers: Vec<usize> = (0..matches.len())
                .filter(|&j| named[j] == *selector)
                .collect();
            let unique_item = enclosing[i].as_ref().filter(|&item| {
                peers
                    .iter()
                    .filter(|&&j| enclosing[j].as_ref() == Some(item))
                    .count()
                    == 1
            });
            let scope = match unique_item {
                _ if peers.len() == 1 => String::new(),
                Some(item) => format!("{item}>"),
                None if files.len() == 1 => format!("{lines}>"),
                None if peers.iter().filter(|&&j| matches[j].file == m.file).count() == 1 => {
                    format!("file:{}>", f.path)
                }
                None => format!("file:{}>{lines}>", f.path),
            };
            (format!("{scope}{selector}"), format!("{}:{lines}", f.path))
        })
        .collect();
    // Matches on one line get the same line-scoped selector, which picks none
    // of them alone.
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for (selector, _) in &all {
        *counts.entry(selector).or_default() += 1;
    }
    let unique: Vec<(String, String)> = all
        .iter()
        .filter(|(selector, _)| counts[selector.as_str()] == 1)
        .cloned()
        .collect();
    Candidates {
        shared: all.len() - unique.len(),
        listed: unique.into_iter().take(MAX_CANDIDATES).collect(),
        total: matches.len(),
    }
}

/// `selector` with the wildcard name of its `last` step replaced by the name
/// of the innermost item of that kind holding `range`, if it has one.
fn named(selector: &str, last: &Step, f: &SourceFile, range: &Range<usize>) -> Option<String> {
    let Primary::Syntax { kind, name } = &last.primary else {
        return None;
    };
    if !name.contains('*') {
        return None;
    }
    let item = f
        .items()?
        .iter()
        .rev()
        .find(|i| i.kind == kind && i.range.start <= range.start && range.end <= i.range.end)?;
    let prefix = &selector[..selector.rfind(&format!("{kind}:"))?];
    let parts: String = last
        .parts
        .iter()
        .map(|&p| format!(".{}", part_name(p)))
        .collect();
    Some(format!(
        "{prefix}{}{parts}",
        syntax::selector(kind, &item.name)
    ))
}

/// The selector of the innermost item that strictly contains `range`.
fn enclosing(f: &SourceFile, range: &Range<usize>) -> Option<String> {
    f.items()?
        .iter()
        .rev()
        .find(|i| i.range.start <= range.start && range.end <= i.range.end && i.range != *range)
        .map(|i| syntax::selector(i.kind, &i.name))
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
        assert_eq!(select("delete fn:a..fn:b", TEXT), [TEXT.trim_end()]);
        assert_eq!(
            select("delete all /let x/../let y/", TEXT),
            ["let x = 1;\n    let y"]
        );
        assert_eq!(
            select("delete /let x/../let y/.lines", TEXT),
            ["    let x = 1;\n    let y = 2;\n"]
        );
        assert_eq!(select(r#"delete fn:b>/let/.."}""#, TEXT), ["let x = 3;\n}"]);
    }

    #[test]
    fn ranges_skip_starts_inside_an_earlier_range() {
        assert_eq!(select("delete all /a/../b/", "a b a b\n"), ["a b", "a b"]);
        assert_eq!(select("delete all /a/../b/", "a a b\n"), ["a a b"]);
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
                range: TEXT.rfind("let x").unwrap()..TEXT.rfind(" = 3").unwrap()
            }]
        );
        let m = resolve_in("delete /fn b[^}]*/>\"x\"", &set).unwrap();
        let x = TEXT.rfind('x').unwrap();
        assert_eq!(
            m,
            [Match {
                file: 0,
                range: x..x + 1
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
    fn lines_part_widens_to_whole_lines() {
        assert_eq!(select("delete \"y = 2\".lines", TEXT), ["    let y = 2;\n"]);
        assert_eq!(
            select("delete \"2;\\n}\".lines", TEXT),
            ["    let y = 2;\n}\n"]
        );
        assert_eq!(select("delete all /x/.lines", "x x\ny\n"), ["x x\n"]);
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
                range: 2..3
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
                    range: 0..1
                },
                Match {
                    file: 0,
                    range: 2..3
                },
                Match {
                    file: 1,
                    range: 0..1
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
            "error: script:1:8: fn:main has no .doc; it has .body .sig .params .name .lines"
        );
        assert_eq!(
            error("delete import:std::fmt.body", &[("a.rs", RUST)]),
            "error: script:1:8: import:std::fmt has no .body; it has .sig .name .lines"
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
        assert_eq!(select("delete fn:main.lines", RUST).len(), 1);
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

    #[test]
    fn syntax_steps_are_not_yet_supported_for_other_languages() {
        assert_eq!(
            error("delete fn:main", &[("a.py", "def main():\n    pass\n")]),
            "error: script:1:8: `fn:main` in python files is not yet supported; \
             use a line, /regex/, \"literal\" or query{} selector"
        );
    }
}
