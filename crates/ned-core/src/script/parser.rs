//! Parser from script text to [`Script`] (command-language spec, §2.2).

use std::ops::Range;

use super::ast::*;
use super::error::{COMMANDS, ParseError, ParseErrorKind as E};
use super::lexer::{Lexer, Token, TokenKind, is_ident_char, part_named};
use crate::hint::{Fix, Hint, ResultExt, verbatim};
use crate::lsp::Severity;
use crate::syntax;

pub fn parse(src: &str) -> Result<Script, ParseError> {
    let mut parser = Parser::new(src);
    script(&mut parser).map_err(|err| ended_early(src, &parser.lexer.heredocs, err))
}

/// `err`, with the fix that names the heredoc that ended early when it
/// follows one whose tag appears again later, alone on a line (§7).
fn ended_early(src: &str, heredocs: &[(String, usize, usize)], err: ParseError) -> ParseError {
    let Some(start) = err.span.as_ref().map(|s| s.start) else {
        return err;
    };
    if matches!(err.kind, E::UnterminatedHeredoc(_)) {
        return err;
    }
    // The tag alone on a later line, before any heredoc that it could end.
    let again = |tag: &str, end: usize| {
        let openers = [format!("<<{tag}"), format!("<<'{tag}'")];
        src[end..]
            .lines()
            .skip(1)
            .take_while(|l| !openers.iter().any(|o| l.contains(o.as_str())))
            .any(|l| l.trim() == tag)
    };
    let Some((tag, opener, end)) = heredocs
        .iter()
        .rev()
        .find(|(tag, _, end)| *end <= start && again(tag, *end))
    else {
        return err;
    };
    let line = |offset: usize| src[..offset].matches('\n').count() + 1;
    let heredoc = verbatim(&format!(
        "the heredoc <<{tag} at line {} ended at line {}, which holds only {tag}: pick a tag its text doesn't hold",
        line(*opener),
        line(*end),
    ));
    err.and_fix(heredoc)
}

fn script(parser: &mut Parser) -> Result<Script, ParseError> {
    let mut commands: Vec<Command> = Vec::new();
    let mut stages: Vec<usize> = Vec::new();
    let mut pipe = 0..0;
    loop {
        let token = parser.peek()?;
        match token.kind {
            TokenKind::Newline | TokenKind::Semicolon => {
                parser.bump()?;
            }
            TokenKind::Pipe => {
                if commands.len() == stages.last().copied().unwrap_or(0) {
                    return Err(E::EmptyStage.at(token.span.clone()));
                }
                pipe = parser.bump()?.span;
                stages.push(commands.len());
            }
            TokenKind::Eof => {
                if stages.last() == Some(&commands.len()) {
                    return Err(E::EmptyStage.at(pipe));
                }
                let later = stages.first().map_or(&[][..], |&first| &commands[first..]);
                if let Some((what, command)) = later
                    .iter()
                    .find_map(|c| reads_disk(&c.kind).map(|what| (what, c)))
                {
                    return Err(E::ReadsDisk(what).at(command.span.clone()));
                }
                return Ok(Script { commands, stages });
            }
            _ => {
                let command = parser.command()?;
                let next = parser.peek()?.clone();
                if !matches!(
                    next.kind,
                    TokenKind::Newline | TokenKind::Semicolon | TokenKind::Pipe | TokenKind::Eof
                ) {
                    let err = expected("end of command", &next)
                        .or_fix(|_| parser.trailing_all(&command, &next))
                        .or_fix(|_| parser.glued_step(&next.span))
                        .or_fix(|_| {
                            let split = parser.one_selector_each(&command)?;
                            Some(verbatim(&format!("one selector per command; separate commands with `;` or a new line: {split}")))
                        });
                    return Err(err);
                }
                commands.push(command);
            }
        }
    }
}

/// What in a command reads the files on disk, which a stage after a `|` can't
/// use (§2.3): `check`, `rename`, or a `.refs` or `.def` part.
fn reads_disk(kind: &CommandKind) -> Option<&'static str> {
    match kind {
        CommandKind::Check { .. } => return Some("check"),
        CommandKind::Rename { .. } => return Some("rename"),
        _ => {}
    }
    let selectors = kind.selectors();
    selectors
        .iter()
        .flat_map(|s| &s.steps)
        .flat_map(|step| &step.parts)
        .find_map(|part| match part {
            Part::Refs => Some(".refs"),
            Part::Def => Some(".def"),
            _ => None,
        })
}

struct Parser<'a> {
    src: &'a str,
    lexer: Lexer<'a>,
    peeked: Option<Token>,
    /// End of the last consumed token.
    last_end: usize,
    /// Start of the command being parsed.
    command_start: usize,
    /// Span of the last heredoc opener consumed.
    last_heredoc: Option<Range<usize>>,
    /// Span of the last regex, string or pattern that directly followed a
    /// selector, with no space between them.
    glued: Option<Range<usize>>,
}

impl Parser<'_> {
    fn new(src: &str) -> Parser<'_> {
        Parser {
            src,
            lexer: Lexer::new(src),
            peeked: None,
            last_end: 0,
            command_start: 0,
            last_heredoc: None,
            glued: None,
        }
    }

    fn peek(&mut self) -> Result<&Token, ParseError> {
        match &mut self.peeked {
            Some(token) => Ok(token),
            slot @ None => Ok(slot.insert(self.lexer.next_token()?)),
        }
    }

    fn bump(&mut self) -> Result<Token, ParseError> {
        let token = match self.peeked.take() {
            Some(token) => token,
            None => self.lexer.next_token()?,
        };
        if let TokenKind::Heredoc { .. } = token.kind {
            self.last_heredoc = Some(token.span.clone());
        }
        self.last_end = token.span.end;
        Ok(token)
    }

    fn peek_is_word(&mut self, word: &str) -> Result<bool, ParseError> {
        Ok(matches!(&self.peek()?.kind, TokenKind::Word(w) if w == word))
    }

    fn command(&mut self) -> Result<Command, ParseError> {
        let verb = self.bump()?;
        self.command_start = verb.span.start;
        let TokenKind::Word(word) = &verb.kind else {
            let fix = verbatim(&format!("commands are {COMMANDS}"));
            return Err(expected("a command", &verb).with_fix(fix));
        };
        let kind = self.command_kind(&verb, word).map_err(|err| {
            let Some(span) = err.span.clone() else {
                return err;
            };
            if let Some(glued) = self.glued_step(&span) {
                return err.with_fix(glued);
            }
            err.or_fix(|kind| {
                let usage = usage(word).filter(|_| matches!(kind, E::Expected { .. }))?;
                let fix = match (self.through_heredoc(&span), usage.rsplit_once("SEL")) {
                    (Some(written), Some((_, rest))) => format!(
                        "a heredoc's body starts on the next line, so finish the command before it: {written}{}",
                        rest.trim_start_matches(']'),
                    ),
                    _ => format!("usage: {usage}"),
                };
                Some(Fix::new(verbatim(&fix)))
            })
        })?;
        Ok(Command {
            kind,
            span: verb.span.start..self.last_end,
        })
    }

    fn command_kind(&mut self, verb: &Token, word: &str) -> Result<CommandKind, ParseError> {
        Ok(match word {
            "show" => self.show().map_err(|err| self.minus_context(verb, err))?,
            "outline" => CommandKind::Outline(self.optional_target()?),
            "replace" => {
                let target = self.target()?;
                self.expect_with()?;
                CommandKind::Replace {
                    target,
                    text: self.text()?,
                }
            }
            "insert" => {
                let position = self.position()?;
                let target = self.target()?;
                // `insert end "text"`: the text was read as the selector.
                if let (Position::Start | Position::End, [step]) =
                    (position, &target.selector.steps[..])
                    && matches!(step.primary, Primary::Literal(_))
                    && matches!(
                        self.peek()?.kind,
                        TokenKind::Newline
                            | TokenKind::Semicolon
                            | TokenKind::Pipe
                            | TokenKind::Eof
                    )
                {
                    let word = if position == Position::Start {
                        "start"
                    } else {
                        "end"
                    };
                    return Err(E::InsertNeedsSelector(word).at(verb.span.start..self.last_end));
                }
                CommandKind::Insert {
                    position,
                    target,
                    text: self.text()?,
                }
            }
            "delete" => CommandKind::Delete(self.target()?),
            "sub" => self.sub().map_err(|e| self.sed_sub(e))?,
            "move" => CommandKind::Move {
                target: self.target()?,
                position: self.position()?,
                dest: self.dest()?,
            },
            "file" => CommandKind::File(self.paths()?),
            "create" => CommandKind::Create {
                path: self.path()?,
                text: self.text()?,
            },
            "check" => self.check()?,
            "allow" => self.allow()?,
            "rename" => self.rename()?,
            "resolve" => self.resolve()?,
            _ => {
                return Err(E::UnknownCommand(word.into()).at(verb.span.clone()));
            }
        })
    }

    fn show(&mut self) -> Result<CommandKind, ParseError> {
        let raw = matches!(&self.peek()?.kind, TokenKind::Word(word) if word == "raw");
        if raw {
            self.bump()?;
        }
        let target = self.optional_target()?;
        let context = match self.peek()?.kind {
            TokenKind::Context(n) if target.is_some() => {
                self.bump()?;
                if self.peek()?.kind == TokenKind::DotDot {
                    let dots = self.bump()?;
                    let next = self.peek()?;
                    let TokenKind::Context(m) = next.kind else {
                        return Err(expected("end of command", &dots));
                    };
                    let target = target.as_ref().expect("matched a target");
                    let all = if target.all { "all " } else { "" };
                    let selector = &self.lexer.src[target.selector.span.clone()];
                    return Err(expected("end of command", &dots).with_fix(verbatim(&format!(
                        "`+N` is one count of lines around each span, not a range; write show {all}{selector} +{m}"
                    ))));
                }
                n
            }
            _ => 0,
        };
        Ok(CommandKind::Show {
            target,
            context,
            raw,
        })
    }

    /// Fixes the error at the `-` of `show SEL -N` with `show SEL +N`.
    fn minus_context(&self, verb: &Token, err: ParseError) -> ParseError {
        let Some(span) = err
            .span
            .clone()
            .filter(|_| err.kind == E::UnexpectedChar('-'))
        else {
            return err;
        };
        let src = self.lexer.src;
        let shown = src[verb.span.end..span.start].trim_end();
        let after = &src[span.end..];
        let count =
            &after[..after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len()];
        let rest = after[count.len()..].trim_start_matches([' ', '\t']);
        if shown.trim().is_empty()
            || count.is_empty()
            || !(rest.is_empty() || rest.starts_with([';', '|', '#', '\n', '\r']))
        {
            return err;
        }
        let fix = if shown.trim().bytes().all(|b| b.is_ascii_digit()) {
            format!("show{shown} +{count}, or the line range show{shown}-{count}")
        } else {
            format!("show{shown} +{count}")
        };
        err.with_fix(verbatim(&format!(
            "context is written `+N`, not `-N`; write {fix}"
        )))
    }

    /// The command up to a heredoc opener that ends its line just before
    /// `span`, which then holds the line break.
    fn through_heredoc(&self, span: &Range<usize>) -> Option<&str> {
        let heredoc = self.last_heredoc.as_ref()?;
        let between = self.src.get(heredoc.end..span.start)?;
        (heredoc.start >= self.command_start
            && between.trim().is_empty()
            && self.src[span.clone()].trim().is_empty())
        .then(|| &self.src[self.command_start..heredoc.end])
    }

    fn optional_target(&mut self) -> Result<Option<Target>, ParseError> {
        match self.peek()?.kind {
            TokenKind::Newline | TokenKind::Semicolon | TokenKind::Pipe | TokenKind::Eof => {
                Ok(None)
            }
            _ => self.target().map(Some),
        }
    }

    /// After `check`: an optional target, then an optional level.
    fn check(&mut self) -> Result<CommandKind, ParseError> {
        let target = match &self.peek()?.kind {
            TokenKind::Word(word)
                if !matches!(word.as_str(), "all" | "conflict")
                    && syntax::find_kind(word).is_none() =>
            {
                None
            }
            _ => self.optional_target()?,
        };
        let level = match &self.peek()?.kind {
            TokenKind::Word(word) => {
                let word = word.clone();
                let token = self.bump()?;
                let level = word.parse().map_err(|()| {
                    let error = E::UnknownLevel(word.clone()).at(token.span);
                    match word.strip_suffix('s').map(str::parse::<Severity>) {
                        Some(Ok(level)) => {
                            error.with_fix(verbatim(&format!("did you mean `{level}`?")))
                        }
                        _ => error,
                    }
                })?;
                Some(level)
            }
            _ => None,
        };
        Ok(CommandKind::Check { target, level })
    }

    /// After `allow`: `errors` or `warnings`.
    fn allow(&mut self) -> Result<CommandKind, ParseError> {
        let token = self.bump()?;
        match &token.kind {
            TokenKind::Word(w) if w == "errors" => Ok(CommandKind::Allow(Severity::Error)),
            TokenKind::Word(w) if w == "warnings" => Ok(CommandKind::Allow(Severity::Warning)),
            _ => Err(expected("`errors` or `warnings`", &token)),
        }
    }

    /// After `rename`: `SEL to NAME`.
    fn rename(&mut self) -> Result<CommandKind, ParseError> {
        let selector = self.dest()?;
        let to = self.bump()?;
        if !matches!(&to.kind, TokenKind::Word(w) if w == "to") {
            return Err(expected("`to`", &to));
        }
        let token = self.bump()?;
        let name = match token.kind {
            TokenKind::Word(name) | TokenKind::Str(name) => name,
            _ => return Err(expected("a name", &token)),
        };
        Ok(CommandKind::Rename { selector, name })
    }

    /// After `resolve`: `SEL ours|theirs|base|both`.
    fn resolve(&mut self) -> Result<CommandKind, ParseError> {
        let target = self.target()?;
        let token = self.bump()?;
        let keep = match &token.kind {
            TokenKind::Word(word) if word == "ours" => Keep::Ours,
            TokenKind::Word(word) if word == "theirs" => Keep::Theirs,
            TokenKind::Word(word) if word == "base" => Keep::Base,
            TokenKind::Word(word) if word == "both" => Keep::Both,
            _ => return Err(expected("ours, theirs, base, or both", &token)),
        };
        Ok(CommandKind::Resolve { target, keep })
    }

    fn target(&mut self) -> Result<Target, ParseError> {
        let all = self.peek_is_word("all")?;
        if all {
            self.bump()?;
        }
        let selector = self.selector()?;
        if !all && self.peek_is_word("all")? {
            let all = self.bump()?.span;
            return Err(self.all_after_selector(&selector, all)?);
        }
        Ok(Target { all, selector })
    }

    /// The error for `all` after `selector` instead of before it: `sub`, which
    /// takes no `all`, says to drop it.
    fn all_after_selector(
        &mut self,
        selector: &Selector,
        all: Range<usize>,
    ) -> Result<ParseError, ParseError> {
        let lead = &self.src[self.command_start..selector.span.start];
        let written = &self.src[selector.span.clone()];
        if lead.split_whitespace().next() == Some("sub") {
            let error = match &selector.steps[..] {
                [
                    Step {
                        primary: Primary::Regex(_),
                        parts,
                        ..
                    },
                ] if parts.is_empty() && self.peek_is_word("with")? => sub_all(written, all),
                _ => E::MissingSubPattern.at(all),
            };
            return Ok(error);
        }
        let more = match self.peek()?.kind {
            TokenKind::Newline | TokenKind::Semicolon | TokenKind::Pipe | TokenKind::Eof => "",
            _ => " ...",
        };
        let fix = verbatim(&format!(
            "`all` goes before the selector; write {lead}all {written}{more}"
        ));
        Ok(E::AllNotAllowed.at(all).with_fix(fix))
    }

    /// The fix for `all` at the end of `command`, after its TEXT or another
    /// argument, instead of before its selector.
    fn trailing_all(&self, command: &Command, next: &Token) -> Option<String> {
        if !matches!(&next.kind, TokenKind::Word(word) if word == "all") {
            return None;
        }
        let target = match &command.kind {
            CommandKind::Show {
                target: Some(target),
                ..
            }
            | CommandKind::Outline(Some(target))
            | CommandKind::Check {
                target: Some(target),
                ..
            }
            | CommandKind::Replace { target, .. }
            | CommandKind::Insert { target, .. }
            | CommandKind::Delete(target)
            | CommandKind::Resolve { target, .. } => target,
            _ => return None,
        };
        if target.all {
            return None;
        }
        let lead = &self.src[command.span.start..target.selector.span.start];
        let written = &self.src[target.selector.span.clone()];
        Some(verbatim(&format!(
            "`all` goes before the selector; write {lead}all {written} ..."
        )))
    }

    /// The fix for a search at `span` glued to the selector before it, which `>`
    /// nests in the selector's last step; `None` if there's none there, or if the
    /// command doesn't parse with the `>`.
    fn glued_step(&self, span: &Range<usize>) -> Option<String> {
        if self.glued.as_ref() != Some(span) {
            return None;
        }
        let fixed = format!(
            "{}>{}",
            &self.src[self.command_start..span.start],
            &self.src[span.start..]
        );
        let command = Parser::new(&fixed).command().ok()?;
        let written = fixed[command.span].lines().next()?;
        Some(verbatim(&format!(
            "a search in a step goes after `>`; write {written}"
        )))
    }

    /// `command`, a `show`, `outline` or `delete`, and the selector after it as
    /// two commands: `show fn:a; show fn:b`.
    fn one_selector_each(&mut self, command: &Command) -> Option<String> {
        {
            let (CommandKind::Show {
                target: Some(target),
                ..
            }
            | CommandKind::Outline(Some(target))
            | CommandKind::Delete(target)) = &command.kind
            else {
                return None;
            };
            let next = self.selector().ok()?;
            let verb = &self.src[command.span.start..target.selector.span.start];
            Some(verbatim(&format!(
                "{}; {verb}{}",
                &self.src[command.span.clone()],
                &self.src[next.span]
            )))
        }
    }

    fn dest(&mut self) -> Result<Selector, ParseError> {
        if self.peek_is_word("all")? {
            let token = self.bump()?;
            let fix = "drop it: a `move` destination or a `rename` is a single span";
            return Err(E::AllNotAllowed.at(token.span).with_fix(fix));
        }
        let dest = self.selector()?;
        if self.peek_is_word("all")? {
            let token = self.bump()?;
            let fix = "drop it: a `move` destination or a `rename` is a single span";
            return Err(E::AllNotAllowed.at(token.span).with_fix(fix));
        }
        Ok(dest)
    }

    fn selector(&mut self) -> Result<Selector, ParseError> {
        let first = self.bump()?;
        let start = first.span.start;
        let mut steps = vec![Step {
            primary: self.range(first)?,
            parts: Vec::new(),
            filters: Vec::new(),
            span: start..self.last_end,
        }];
        loop {
            let next = self.peek()?;
            match next.kind {
                TokenKind::Gt | TokenKind::Part(_) | TokenKind::Filter(_) if next.space_before => {
                    return Err(E::SpaceInSelector.at(next.span.clone()));
                }
                TokenKind::Part(part) => {
                    let token = self.bump()?;
                    if let Some(step) = steps.last_mut() {
                        if !step.filters.is_empty() {
                            return Err(E::PartAfterFilter.at(token.span));
                        }
                        step.parts.push(part);
                        step.span.end = self.last_end;
                    }
                }
                TokenKind::Filter(_) => {
                    let token = self.bump()?;
                    let TokenKind::Filter(filter) = token.kind else {
                        unreachable!("peeked a filter");
                    };
                    if let Some(step) = steps.last_mut() {
                        step.filters.push((filter, token.span));
                        step.span.end = self.last_end;
                    }
                }
                TokenKind::Gt => {
                    let gt = self.bump()?.span;
                    let token = self.bump()?;
                    if token.space_before {
                        return Err(E::SpaceInSelector.at(token.span));
                    }
                    let step_start = token.span.start;
                    let all = matches!(&token.kind, TokenKind::Word(word) if word == "all");
                    let primary = self.range(token).ok_or_fix(|_| {
                        all.then(|| {
                            let lead = &self.src[self.command_start..start];
                            let steps = &self.src[start..gt.start];
                            let rest = match self.selector() {
                                Ok(rest) => &self.src[rest.span],
                                Err(_) => "...",
                            };
                            verbatim(&format!(
                                "`all` goes before the selector; write {lead}all {steps}>{rest}"
                            ))
                        })
                    })?;
                    steps.push(Step {
                        primary,
                        parts: Vec::new(),
                        filters: Vec::new(),
                        span: step_start..self.last_end,
                    });
                }
                TokenKind::Regex { .. } | TokenKind::Str(_) | TokenKind::Code(_)
                    if !next.space_before =>
                {
                    self.glued = Some(next.span.clone());
                    break;
                }
                _ => break,
            }
        }
        Ok(Selector {
            steps,
            span: start..self.last_end,
        })
    }

    /// The primary `first` starts, which may be a range, `first..TO`.
    fn range(&mut self, first: Token) -> Result<Primary, ParseError> {
        let from = primary(self.src, first)?;
        let next = self.peek()?;
        if next.kind != TokenKind::DotDot {
            return Ok(from);
        }
        if next.space_before {
            return Err(E::SpaceInSelector.at(next.span.clone()));
        }
        self.bump()?;
        let token = self.bump()?;
        if token.space_before {
            return Err(E::SpaceInSelector.at(token.span));
        }
        Ok(Primary::Range {
            from: Box::new(from),
            to: Box::new(primary(self.src, token)?),
        })
    }

    fn position(&mut self) -> Result<Position, ParseError> {
        let token = self.bump()?;
        match &token.kind {
            TokenKind::Word(w) if w == "before" => Ok(Position::Before),
            TokenKind::Word(w) if w == "after" => Ok(Position::After),
            TokenKind::Word(w) if w == "start" => Ok(Position::Start),
            TokenKind::Word(w) if w == "end" => Ok(Position::End),
            _ => Err(expected("before, after, start, or end", &token)),
        }
    }

    fn expect_with(&mut self) -> Result<(), ParseError> {
        let token = self.bump()?;
        match &token.kind {
            TokenKind::Word(w) if w == "with" => Ok(()),
            _ => Err(expected("`with`", &token)),
        }
    }

    fn text(&mut self) -> Result<Text, ParseError> {
        let token = self.bump()?;
        text_from(token.kind)
            .map_err(|kind| expected("text (a string or heredoc)", &Token { kind, ..token }))
    }

    /// `sub [target] /re/ with TEXT`: a lone regex before `with` is the
    /// pattern; otherwise the selector is the scope and a regex must follow.
    fn sub(&mut self) -> Result<CommandKind, ParseError> {
        let all = self.peek()?.span.clone();
        let first = self.target()?;
        let written;
        let (scope, pattern) = if self.peek_is_word("with")? {
            let span = first.selector.span.clone();
            written = span.clone();
            let mut steps = first.selector.steps;
            match (first.all, steps.pop(), steps.is_empty()) {
                (
                    true,
                    Some(Step {
                        primary: Primary::Regex(_),
                        parts,
                        ..
                    }),
                    true,
                ) if parts.is_empty() => {
                    return Err(sub_all(&self.src[span], all));
                }
                (
                    false,
                    Some(Step {
                        primary: Primary::Regex(pattern),
                        parts,
                        ..
                    }),
                    true,
                ) if parts.is_empty() => (None, pattern),
                (
                    _,
                    Some(Step {
                        primary: Primary::Literal(_),
                        parts,
                        ..
                    }),
                    _,
                ) if parts.is_empty() => {
                    let selector = self.src[span.clone()].to_string();
                    return Err(self.literal_sub(selector, span)?);
                }
                _ => return Err(E::MissingSubPattern.at(span)),
            }
        } else {
            let token = self.bump()?;
            if let TokenKind::Str(_) = token.kind
                && self.peek_is_word("with")?
            {
                let selector = format!(
                    "{}>{}",
                    &self.src[first.selector.span.clone()],
                    &self.src[token.span.clone()]
                );
                return Err(self.literal_sub(selector, token.span)?);
            }
            let TokenKind::Regex { pattern, flags } = token.kind else {
                return Err(E::MissingSubPattern.at(token.span));
            };
            written = first.selector.span.start..token.span.end;
            if self.peek_is_word("all")? {
                let all = self.bump()?.span;
                return Err(sub_all(&self.src[written], all));
            }
            (Some(first), validate_regex(pattern, flags, token.span)?)
        };
        self.expect_with()?;
        let at = self.peek()?.span.clone();
        let text = self.text()?;
        self.validate_groups(&pattern, &text, at)?;
        if self.peek_is_word("all")? {
            let all = self.bump()?.span;
            return Err(sub_all(&self.src[written], all));
        }
        Ok(CommandKind::Sub {
            scope,
            pattern,
            text,
        })
    }

    /// The error for `selector`, a literal, before the `with` of a `sub`, with the
    /// fix that names the `replace` that, like `sub`, replaces every match.
    fn literal_sub(
        &mut self,
        selector: String,
        span: Range<usize>,
    ) -> Result<ParseError, ParseError> {
        self.expect_with()?;
        let token = self.bump()?;
        // `$` is literal in `replace`: `$$` becomes `$`, and a group reference
        // has no `replace` equivalent.
        let text = match token.kind {
            TokenKind::Str(text) if !text.replace("$$", "").contains('$') => {
                self.src[token.span].replace("$$", "$")
            }
            _ => "...".into(),
        };
        let error = E::Expected {
            expected: "a regex",
            found: "a string".into(),
        };
        Ok(error.at(span).with_fix(verbatim(&format!(
            "for a literal, write replace all {selector} with {text}"
        ))))
    }

    /// `error`, with the fix that names the `sub` sed's `/re/text/flags` means if
    /// it falls in the `text/flags` after the command's last regex.
    fn sed_sub(&self, error: ParseError) -> ParseError {
        let Some(regex) = (self.lexer.last_regex.clone()).filter(|r| r.start >= self.command_start)
        else {
            return error;
        };
        let rest = &self.src[regex.end..];
        // sed's text follows the regex's `/` directly; `with` after a space
        // is `sub`'s own form.
        let with = rest.starts_with(char::is_whitespace)
            && rest.trim_start().strip_prefix("with").is_some_and(|r| {
                r.is_empty() || r.starts_with(|c: char| c.is_whitespace() || matches!(c, '"' | '<'))
            });
        if with {
            return error;
        }
        let Some((text, flags, len)) = sed_tail(rest) else {
            return error;
        };
        let end = regex.end + len;
        if !error
            .span
            .as_ref()
            .is_some_and(|s| (regex.end..end).contains(&s.start))
        {
            return error;
        }
        let lead = self.src[self.command_start..regex.start].trim_end();
        let flags: String = ['i', 's']
            .into_iter()
            // GNU sed writes `i` as `I` too.
            .filter(|&f| flags.contains(f) || (f == 'i' && flags.contains('I')))
            .collect();
        let fix = format!(
            "{lead} {}{flags} with \"{}\"",
            &self.src[regex.clone()],
            sed_text(text)
        );
        error.with_fix(verbatim(&format!(
            "`sub` takes /re/ with TEXT, not sed's /re/text/; write {fix}"
        )))
    }

    /// Checks that each `$` reference in `text`, read from the token at `at`,
    /// names a group of `pattern`.
    fn validate_groups(
        &self,
        pattern: &Pattern,
        text: &Text,
        at: Range<usize>,
    ) -> Result<(), ParseError> {
        let regex = pattern.regex();
        let has = |name: &str| match name.parse::<usize>() {
            Ok(i) => i < regex.captures_len(),
            Err(_) => regex.capture_names().flatten().any(|n| n == name),
        };
        let Some((range, name)) = group_refs(&text.value)
            .into_iter()
            .find(|(_, name)| !has(name))
        else {
            return Ok(());
        };
        let reference = &text.value[range.clone()];
        let span = match text.kind {
            TextKind::Str => {
                str_offset(self.src, &at, range.start)..str_offset(self.src, &at, range.end)
            }
            TextKind::Heredoc | TextKind::RawHeredoc => at,
        };
        if name.is_empty() {
            return Err(E::EmptyGroup.at(span));
        }
        let split = (!reference.starts_with("${"))
            .then(|| {
                (1..name.len())
                    .rev()
                    .map(|i| name.split_at(i))
                    .find(|(group, _)| has(group))
            })
            .flatten();
        let fix = match split {
            Some((group, rest)) => format!("write `${{{group}}}{rest}`, or `$$` for a literal `$`"),
            None => {
                let groups: Vec<String> = regex
                    .capture_names()
                    .enumerate()
                    .map(|(i, name)| match name {
                        Some(name) => format!("`${{{name}}}`"),
                        None => format!("`${i}`"),
                    })
                    .collect();
                format!(
                    "its groups are {}, and `$$` is a literal `$`",
                    groups.join(" ")
                )
            }
        };
        Err(E::UnknownGroup {
            reference: reference.into(),
            name: name.into(),
        }
        .at(span)
        .with_fix(verbatim(&fix)))
    }

    fn paths(&mut self) -> Result<Vec<String>, ParseError> {
        let mut paths = Vec::new();
        while let Some(token) = self.lexer.path()? {
            self.last_end = token.span.end;
            if let TokenKind::Path(path) = token.kind {
                paths.push(path);
            }
        }
        if paths.is_empty() {
            return Err(expected("a path", self.peek()?));
        }
        Ok(paths)
    }

    fn path(&mut self) -> Result<String, ParseError> {
        match self.lexer.path()? {
            Some(Token {
                kind: TokenKind::Path(path),
                span,
                ..
            }) => {
                self.last_end = span.end;
                Ok(path)
            }
            _ => Err(expected("a path", self.peek()?)),
        }
    }
}

fn primary(src: &str, token: Token) -> Result<Primary, ParseError> {
    Ok(match token.kind {
        TokenKind::Lines { start, end } => Primary::Lines { start, end },
        TokenKind::Regex { pattern, flags } => {
            Primary::Regex(validate_regex(pattern, flags, token.span)?)
        }
        TokenKind::Syntax { kind, name } => match kind.as_str() {
            "file" => Primary::File(name),
            "conflict" => match name.parse() {
                Ok(n) if n > 0 && name.bytes().all(|b| b.is_ascii_digit()) => {
                    Primary::Conflict(Some(n))
                }
                _ => return Err(E::ConflictNumber(name).at(token.span)),
            },
            "refs" | "def" => return Err(E::PartAsKind(kind).at(token.span)),
            _ => Primary::Syntax { kind, name },
        },
        TokenKind::Word(word) if word == "conflict" => Primary::Conflict(None),
        TokenKind::Word(word) if syntax::find_kind(&word).is_some() => Primary::Syntax {
            kind: word,
            name: "*".into(),
        },
        TokenKind::Query(query) => Primary::Query(query),
        TokenKind::Code(code) => Primary::Code(code),
        kind => match text_from(kind) {
            Ok(text) => Primary::Literal(text),
            Err(kind) => {
                let token = Token { kind, ..token };
                return Err(bare_path(src, &token)
                    .or_else(|| bare_name(src, &token))
                    .unwrap_or_else(|| expected("a selector", &token)));
            }
        },
    })
}

/// The error for `all` at `all` in a `sub` of `regex`, which replaces every
/// match without it.
fn sub_all(regex: &str, all: Range<usize>) -> ParseError {
    let fix = verbatim(&format!(
        "`sub` already replaces every match; drop `all`: sub {regex} with ..."
    ));
    E::AllNotAllowed.at(all).with_fix(fix)
}

/// For a bare word that starts a file path, such as `src/a.rs>fn:x`, an error
/// suggesting the `file:` step that scopes to it.
fn bare_path(src: &str, token: &Token) -> Option<ParseError> {
    let TokenKind::Word(word) = &token.kind else {
        return None;
    };
    let rest = &src[token.span.end..];
    let selector_end = rest.find(|c: char| c.is_whitespace() || matches!(c, ';' | '|'));
    let selector = &rest[..selector_end.unwrap_or(rest.len())];
    let tail = &selector[..selector.find('>').unwrap_or(selector.len())];
    let is_path = match tail.chars().next() {
        Some('/') => tail.len() > 1,
        // `x.body` is more likely a part after unquoted text than a file.
        Some('.') => {
            let ext = tail[1..].split(|c| !is_ident_char(c)).next().unwrap_or("");
            !ext.is_empty() && part_named(ext).is_none()
        }
        _ => false,
    };
    is_path.then(|| {
        let rest = &selector[tail.len()..];
        // A quoted step or filter may hold the whitespace `selector` ended at.
        let rest = if rest.contains(['"', '`', '/', '[', '<']) {
            ">…"
        } else {
            rest
        };
        let file = format!("select the file with a `file:` step: file:{word}{tail}{rest}");
        // Without a `/` or a step after it, `self.x` may be unquoted text.
        let fix = if tail.contains('/') || !rest.is_empty() {
            file
        } else {
            format!("quote literal text: \"{word}{tail}\", or {file}")
        };
        E::Expected {
            expected: "a selector",
            found: format!("`{word}{tail}`"),
        }
        .at(token.span.start..token.span.end + tail.len())
        .with_fix(verbatim(&fix))
    })
}

/// For a bare name where a selector is expected, such as `GitError.body>…`, an
/// error suggesting the item it may name.
fn bare_name(src: &str, token: &Token) -> Option<ParseError> {
    let TokenKind::Word(word) = &token.kind else {
        return None;
    };
    if !word.chars().all(is_ident_char)
        || KEYWORDS.contains(&word.as_str())
        || usage(word).is_some()
    {
        return None;
    }
    let rest = &src[token.span.end..];
    let rest = &rest[..rest
        .find(|c: char| c.is_whitespace() || matches!(c, ';' | '|'))
        .unwrap_or(rest.len())];
    if !(rest.is_empty() || rest.starts_with(['.', '>'])) {
        return None;
    }
    // A quoted step or filter may hold the whitespace `rest` ended at.
    let rest = match rest.find(['"', '`', '/', '[', '<']) {
        Some(i) => format!("{}…", &rest[..i]),
        None => rest.to_string(),
    };
    let kind = E::BareName {
        word: word.clone(),
        rest,
    };
    Some(kind.at(token.span.clone()))
}

/// The text of a string or heredoc token, or the token kind back if it is
/// neither.
fn text_from(kind: TokenKind) -> Result<Text, TokenKind> {
    match kind {
        TokenKind::Str(value) => Ok(Text {
            value,
            kind: TextKind::Str,
        }),
        TokenKind::Heredoc { body, raw, .. } => Ok(Text {
            value: body,
            kind: if raw {
                TextKind::RawHeredoc
            } else {
                TextKind::Heredoc
            },
        }),
        kind => Err(kind),
    }
}

/// Each `$` reference in a `sub` replacement, read as the regex crate's
/// `Captures::expand` reads it: its byte range, and the group it names.
fn group_refs(text: &str) -> Vec<(Range<usize>, &str)> {
    let mut refs = Vec::new();
    let mut at = 0;
    while let Some(start) = text[at..].find('$').map(|i| at + i) {
        let rest = &text[start + 1..];
        let named = if rest.starts_with('$') {
            at = start + 2;
            continue;
        } else if let Some(braced) = rest.strip_prefix('{') {
            braced.find('}').map(|end| (&braced[..end], end + 2))
        } else {
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            (end > 0).then(|| (&rest[..end], end))
        };
        at = start + 1;
        if let Some((name, len)) = named {
            at += len;
            refs.push((start..at, name));
        }
    }
    refs
}

/// Splits `rest`, what follows a regex, into sed's `text/flags`, with the
/// length they span; `None` unless they end the command.
fn sed_tail(rest: &str) -> Option<(&str, &str, usize)> {
    let mut chars = rest.char_indices();
    let slash = loop {
        match chars.next()? {
            (_, '\n') => return None,
            (i, '/') => break i,
            (_, '\\') if chars.next()?.1 == '\n' => return None,
            _ => {}
        }
    };
    let flags = &rest[slash + 1..];
    let flags = &flags[..flags
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(flags.len())];
    let end = slash + 1 + flags.len();
    (end == rest.len() || rest[end..].starts_with([' ', '\t', '\r', '\n', ';', '|'])).then_some((
        &rest[..slash],
        flags,
        end,
    ))
}

/// sed's replacement `text` as the inside of a string for `sub`.
fn sed_text(text: &str) -> String {
    fn escaped(out: &mut String, c: char) {
        match c {
            '$' => out.push_str("$$"),
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '&' => out.push_str("${0}"),
            '\\' => match chars.next() {
                Some(d @ '0'..='9') => out.extend(['$', '{', d, '}']),
                Some('n') => out.push_str("\\n"),
                Some('t') => out.push_str("\\t"),
                Some(c) => escaped(&mut out, c),
                None => escaped(&mut out, '\\'),
            },
            c => escaped(&mut out, c),
        }
    }
    out
}

/// The script offset of byte `offset` of the value of the string token at
/// `token`; each escape is two bytes of script for one of value.
fn str_offset(src: &str, token: &Range<usize>, offset: usize) -> usize {
    let body = token.start + 1;
    let mut value = 0;
    let mut chars = src[body..token.end].char_indices();
    while let Some((i, c)) = chars.next() {
        if value >= offset {
            return body + i;
        }
        if c == '\\' {
            chars.next();
        }
        value += if c == '\\' { 1 } else { c.len_utf8() };
    }
    token.end
}

pub(super) fn validate_regex(
    source: String,
    flags: RegexFlags,
    span: Range<usize>,
) -> Result<Pattern, ParseError> {
    Pattern::new(source, flags).map_err(|err| {
        // The regex crate's message is a multi-line excerpt ending in
        // `error: <reason>`; keep only the reason.
        let message = err.to_string();
        let reason = message
            .lines()
            .find_map(|l| l.strip_prefix("error: "))
            .unwrap_or(&message);
        E::InvalidRegex(reason.into()).at(span)
    })
}

const KEYWORDS: [&str; 8] = [
    "all", "with", "to", "before", "after", "start", "end", "file",
];

/// The syntax of the command `verb`, as `ned help VERB` starts.
pub fn usage(verb: &str) -> Option<&'static str> {
    Some(match verb {
        "show" => "show [raw] [SEL [+N]]",
        "outline" => "outline [SEL]",
        "replace" => "replace [all] SEL with TEXT",
        "insert" => "insert before|after|start|end [all] SEL TEXT",
        "delete" => "delete [all] SEL",
        "sub" => "sub [[all] SEL] /re/ with TEXT",
        "move" => "move [all] SEL before|after|start|end DEST",
        "file" => "file PATH...",
        "create" => "create PATH TEXT",
        "check" => "check [SEL] [LEVEL]",
        "allow" => "allow errors|warnings",
        "rename" => "rename SEL to NAME",
        "resolve" => "resolve [all] SEL ours|theirs|base|both",
        _ => return None,
    })
}

fn expected(what: &'static str, token: &Token) -> ParseError {
    let found = match &token.kind {
        TokenKind::Word(w) => format!("`{w}`"),
        TokenKind::Syntax { kind, name } => format!("`{kind}:{name}`"),
        TokenKind::Lines { .. } => "a line selector".into(),
        TokenKind::Regex { .. } => "a regex".into(),
        TokenKind::Str(_) => "a string".into(),
        TokenKind::Heredoc { .. } => "a heredoc".into(),
        TokenKind::Query(_) => "a query".into(),
        TokenKind::Code(_) => "a pattern".into(),
        TokenKind::Part(_) => "a part".into(),
        TokenKind::Filter(_) => "a filter".into(),
        TokenKind::Path(_) => "a path".into(),
        TokenKind::Gt => "`>`".into(),
        TokenKind::DotDot => "`..`".into(),
        TokenKind::Context(_) => "a context count".into(),
        TokenKind::Semicolon => "`;`".into(),
        TokenKind::Pipe => "`|`".into(),
        TokenKind::Newline => "end of line".into(),
        TokenKind::Eof => "end of script".into(),
    };
    let error = E::Expected {
        expected: what,
        found,
    }
    .at(token.span.clone());
    match &token.kind {
        // A keyword out of place is a usage mistake, not unquoted text.
        TokenKind::Word(w)
            if (what == "a selector" || what.starts_with("text"))
                && !KEYWORDS.contains(&w.as_str())
                && usage(w).is_none() =>
        {
            error.with_fix(verbatim(&format!("quote literal text: \"{w}\"")))
        }
        _ => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hint::Frontend;
    use crate::script::ParseErrorKind as E;
    use LineNo::{Last, Number as N};

    fn commands(src: &str) -> Vec<CommandKind> {
        parse(src)
            .unwrap_or_else(|e| panic!("{}", e.render(Frontend::Cli, Some(src))))
            .commands
            .into_iter()
            .map(|c| unspan(c.kind))
            .collect()
    }

    fn one(src: &str) -> CommandKind {
        let mut all = commands(src);
        assert_eq!(all.len(), 1, "{src:?}");
        all.remove(0)
    }

    fn error(src: &str) -> ParseError {
        match parse(src) {
            Ok(script) => panic!("{src:?} parsed: {script:?}"),
            Err(e) => e,
        }
    }

    fn unspan_selector(selector: Selector) -> Selector {
        Selector {
            steps: selector
                .steps
                .into_iter()
                .map(|step| Step {
                    filters: step
                        .filters
                        .into_iter()
                        .map(|(filter, _)| (filter, 0..0))
                        .collect(),
                    span: 0..0,
                    ..step
                })
                .collect(),
            span: 0..0,
        }
    }

    fn unspan_target(target: Target) -> Target {
        Target {
            selector: unspan_selector(target.selector),
            ..target
        }
    }

    fn unspan(kind: CommandKind) -> CommandKind {
        use CommandKind::*;
        match kind {
            Show {
                target,
                context,
                raw,
            } => Show {
                target: target.map(unspan_target),
                context,
                raw,
            },
            Outline(t) => Outline(t.map(unspan_target)),
            Replace { target, text } => Replace {
                target: unspan_target(target),
                text,
            },
            Insert {
                position,
                target,
                text,
            } => Insert {
                position,
                target: unspan_target(target),
                text,
            },
            Delete(t) => Delete(unspan_target(t)),
            Resolve { target, keep } => Resolve {
                target: unspan_target(target),
                keep,
            },
            Sub {
                scope,
                pattern,
                text,
            } => Sub {
                scope: scope.map(unspan_target),
                pattern,
                text,
            },
            Move {
                target,
                position,
                dest,
            } => Move {
                target: unspan_target(target),
                position,
                dest: unspan_selector(dest),
            },
            File(paths) => File(paths),
            Create { path, text } => Create { path, text },
            Allow(level) => Allow(level),
            Check { target, level } => Check {
                target: target.map(unspan_target),
                level,
            },
            Rename { selector, name } => Rename {
                selector: unspan_selector(selector),
                name,
            },
        }
    }

    fn selector(steps: Vec<Step>) -> Selector {
        Selector { steps, span: 0..0 }
    }

    fn target(steps: Vec<Step>) -> Target {
        Target {
            all: false,
            selector: selector(steps),
        }
    }

    fn all(steps: Vec<Step>) -> Target {
        Target {
            all: true,
            selector: selector(steps),
        }
    }

    fn step(primary: Primary) -> Step {
        Step {
            primary,
            parts: Vec::new(),
            filters: Vec::new(),
            span: 0..0,
        }
    }

    fn parts(step: Step, parts: &[Part]) -> Step {
        Step {
            parts: parts.to_vec(),
            ..step
        }
    }

    fn syntax(kind: &str, name: &str) -> Step {
        step(Primary::Syntax {
            kind: kind.into(),
            name: name.into(),
        })
    }

    fn lines(start: LineNo, end: Option<LineNo>) -> Step {
        step(Primary::Lines { start, end })
    }

    fn pattern(source: &str) -> Pattern {
        Pattern::new(source.into(), RegexFlags::default()).unwrap()
    }

    fn text(value: &str, kind: TextKind) -> Text {
        Text {
            value: value.into(),
            kind,
        }
    }

    fn string(value: &str) -> Text {
        text(value, TextKind::Str)
    }

    fn literal(value: &str) -> Step {
        step(Primary::Literal(string(value)))
    }

    #[test]
    fn empty_scripts() {
        assert_eq!(commands(""), []);
        assert_eq!(commands("\n\n; ;\n  # comment\n"), []);
    }

    #[test]
    fn show_and_outline() {
        assert_eq!(
            one("show"),
            CommandKind::Show {
                target: None,
                context: 0,
                raw: false,
            }
        );
        assert_eq!(one("outline"), CommandKind::Outline(None));
        assert_eq!(
            one("show 12-20"),
            CommandKind::Show {
                target: Some(target(vec![lines(N(12), Some(N(20)))])),
                context: 0,
                raw: false,
            }
        );
        assert_eq!(
            one("outline impl:Parser"),
            CommandKind::Outline(Some(target(vec![syntax("impl", "Parser")])))
        );
    }

    #[test]
    fn replace_with_string() {
        assert_eq!(
            one(r#"replace fn:parse>"unexpected end" with "unexpected end of input""#),
            CommandKind::Replace {
                target: target(vec![syntax("fn", "parse"), literal("unexpected end")]),
                text: string("unexpected end of input"),
            }
        );
    }

    #[test]
    fn replace_with_heredoc() {
        assert_eq!(
            one("replace fn:parse.body with <<END\n  let a;\nEND"),
            CommandKind::Replace {
                target: target(vec![parts(syntax("fn", "parse"), &[Part::Body])]),
                text: text("  let a;", TextKind::Heredoc),
            }
        );
    }

    #[test]
    fn heredoc_selector_and_raw_heredoc_text() {
        assert_eq!(
            one("replace <<A with <<'B'\nold\nA\nnew\nB"),
            CommandKind::Replace {
                target: target(vec![step(Primary::Literal(text("old", TextKind::Heredoc)))]),
                text: text("new", TextKind::RawHeredoc),
            }
        );
    }

    #[test]
    fn insert_positions() {
        for (word, position) in [
            ("before", Position::Before),
            ("after", Position::After),
            ("start", Position::Start),
            ("end", Position::End),
        ] {
            assert_eq!(
                one(&format!(r#"insert {word} 3 "x""#)),
                CommandKind::Insert {
                    position,
                    target: target(vec![lines(N(3), None)]),
                    text: string("x"),
                }
            );
        }
    }

    #[test]
    fn delete_all() {
        assert_eq!(
            one("delete all fn:test_*"),
            CommandKind::Delete(all(vec![syntax("fn", "test_*")]))
        );
        assert_eq!(
            one("delete all /dbg!/.lines"),
            CommandKind::Delete(all(vec![parts(
                step(Primary::Regex(pattern("dbg!"))),
                &[Part::Lines]
            )]))
        );
    }

    #[test]
    fn line_of_a_span() {
        let show = |steps| CommandKind::Show {
            target: Some(target(steps)),
            context: 0,
            raw: false,
        };
        assert_eq!(
            one("show fn:a.lines:2"),
            show(vec![parts(
                syntax("fn", "a"),
                &[Part::Line(LineNo::Number(2))]
            )])
        );
        assert_eq!(
            one("show fn:f.body.lines:$>/x/"),
            show(vec![
                parts(syntax("fn", "f"), &[Part::Body, Part::Line(LineNo::Last)]),
                step(Primary::Regex(pattern("x")))
            ])
        );
        let CommandKind::Show {
            target: Some(target),
            ..
        } = one("show all fn.body.lines:1[.text ~= /x/]")
        else {
            panic!("not a show");
        };
        let step = &target.selector.steps[0];
        assert!(target.all);
        assert_eq!(step.parts, [Part::Body, Part::Line(LineNo::Number(1))]);
        assert_eq!(step.filters.len(), 1);
    }

    #[test]
    fn sub_without_scope() {
        assert_eq!(
            one(r#"sub /\bold_name\b/ with "new_name""#),
            CommandKind::Sub {
                scope: None,
                pattern: pattern(r"\bold_name\b"),
                text: string("new_name"),
            }
        );
    }

    #[test]
    fn sub_with_scope() {
        assert_eq!(
            one(r#"sub fn:parse /x/i with "y""#),
            CommandKind::Sub {
                scope: Some(target(vec![syntax("fn", "parse")])),
                pattern: Pattern::new(
                    "x".into(),
                    RegexFlags {
                        case_insensitive: true,
                        dot_all: false,
                    },
                )
                .unwrap(),
                text: string("y"),
            }
        );
        assert_eq!(
            one(r#"sub all fn:test_* /a/ with "b""#),
            CommandKind::Sub {
                scope: Some(all(vec![syntax("fn", "test_*")])),
                pattern: pattern("a"),
                text: string("b"),
            }
        );
        assert_eq!(
            one(r#"sub /foo/.lines /o/ with "0""#),
            CommandKind::Sub {
                scope: Some(target(vec![parts(
                    step(Primary::Regex(pattern("foo"))),
                    &[Part::Lines]
                )])),
                pattern: pattern("o"),
                text: string("0"),
            }
        );
    }

    #[test]
    fn move_command() {
        assert_eq!(
            one("move all fn:helper_* end mod:utils"),
            CommandKind::Move {
                target: all(vec![syntax("fn", "helper_*")]),
                position: Position::End,
                dest: selector(vec![syntax("mod", "utils")]),
            }
        );
    }

    #[test]
    fn rename_command() {
        assert_eq!(
            one("rename impl:P>fn:new to create"),
            CommandKind::Rename {
                selector: selector(vec![syntax("impl", "P"), syntax("fn", "new")]),
                name: "create".into(),
            }
        );
        assert_eq!(
            one(r#"rename "old" to "r#type""#),
            CommandKind::Rename {
                selector: selector(vec![literal("old")]),
                name: "r#type".into(),
            }
        );
        assert_eq!(
            message("rename fn:x y"),
            "expected `to`, found `y`; usage: rename SEL to NAME"
        );
        assert_eq!(
            message("rename fn:x to"),
            "expected a name, found end of script; usage: rename SEL to NAME"
        );
        assert_eq!(error("rename all fn:x to y").kind, E::AllNotAllowed);
    }

    #[test]
    fn file_command() {
        assert_eq!(
            commands(r#"file src/*.rs "my file.rs"; delete 1"#),
            [
                CommandKind::File(vec!["src/*.rs".into(), "my file.rs".into()]),
                CommandKind::Delete(target(vec![lines(N(1), None)])),
            ]
        );
    }

    #[test]
    fn create_command() {
        assert_eq!(
            one(r#"create src/a.rs "x""#),
            CommandKind::Create {
                path: "src/a.rs".into(),
                text: string("x"),
            }
        );
        assert_eq!(
            one("create \"my file.rs\" <<END\nx\nEND\n"),
            CommandKind::Create {
                path: "my file.rs".into(),
                text: text("x", TextKind::Heredoc),
            }
        );
        assert_eq!(
            message("create"),
            "expected a path, found end of script; usage: create PATH TEXT"
        );
    }

    #[test]
    fn nested_selectors_and_parts() {
        let show = |steps| CommandKind::Show {
            target: Some(target(steps)),
            context: 0,
            raw: false,
        };
        assert_eq!(
            one("show impl:Parser.body>fn:new.sig"),
            show(vec![
                parts(syntax("impl", "Parser"), &[Part::Body]),
                parts(syntax("fn", "new"), &[Part::Sig]),
            ])
        );
        assert_eq!(
            one("show file:src/a.rs>fn:new"),
            show(vec![
                step(Primary::File("src/a.rs".into())),
                syntax("fn", "new")
            ])
        );
        assert_eq!(
            one("show 100-$>fn:new"),
            show(vec![lines(N(100), Some(Last)), syntax("fn", "new")])
        );
        assert_eq!(
            one(r"show fn:main>/unwrap\(\)/"),
            show(vec![
                syntax("fn", "main"),
                step(Primary::Regex(pattern(r"unwrap\(\)")))
            ])
        );
        assert_eq!(
            one("show query{(x) @sel}"),
            show(vec![step(Primary::Query("(x) @sel".into()))])
        );
        assert_eq!(
            one("show fn:main>`foo(@a, 1)`"),
            show(vec![
                syntax("fn", "main"),
                step(Primary::Code("foo(@a, 1)".into()))
            ])
        );
    }

    #[test]
    fn conflicts_and_their_sides() {
        let show = |steps| CommandKind::Show {
            target: Some(target(steps)),
            context: 0,
            raw: false,
        };
        let conflict = |n| step(Primary::Conflict(n));
        assert_eq!(
            one("show conflict:2.theirs"),
            show(vec![parts(conflict(Some(2)), &[Part::Theirs])])
        );
        assert_eq!(
            one("show conflict.ours>fn:a"),
            show(vec![
                parts(conflict(None), &[Part::Ours]),
                syntax("fn", "a")
            ])
        );
        assert_eq!(
            one("show fn:a>conflict.base"),
            show(vec![
                syntax("fn", "a"),
                parts(conflict(None), &[Part::Base])
            ])
        );
        assert_eq!(
            one("check conflict"),
            CommandKind::Check {
                target: Some(target(vec![conflict(None)])),
                level: None,
            }
        );
        for n in ["0", "x", "*", "1a", "+1", "-1", ""] {
            assert_eq!(
                message(&format!("show conflict:{n}")),
                format!(
                    "`conflict:{n}` isn't a conflict's number; conflicts count from 1 in file order, as in conflict:1, and `conflict` is each of them"
                )
            );
        }
    }

    #[test]
    fn resolve_keeps_a_side() {
        let conflict = |n| step(Primary::Conflict(n));
        assert_eq!(
            one("resolve conflict:2 theirs"),
            CommandKind::Resolve {
                target: target(vec![conflict(Some(2))]),
                keep: Keep::Theirs,
            }
        );
        assert_eq!(
            one("resolve all conflict both"),
            CommandKind::Resolve {
                target: all(vec![conflict(None)]),
                keep: Keep::Both,
            }
        );
        for (word, keep) in [("ours", Keep::Ours), ("base", Keep::Base)] {
            assert!(
                matches!(one(&format!("resolve conflict:1 {word}")), CommandKind::Resolve { keep: k, .. } if k == keep)
            );
        }
        for src in ["resolve conflict:1 mine", "resolve conflict:1"] {
            assert!(
                message(src).starts_with("expected ours, theirs, base, or both, found"),
                "{src}: {}",
                message(src)
            );
        }
    }

    #[test]
    fn bare_kinds_are_wildcard_syntax_steps() {
        let show = |steps| CommandKind::Show {
            target: Some(target(steps)),
            context: 0,
            raw: false,
        };
        assert_eq!(one("show fn"), show(vec![syntax("fn", "*")]));
        assert_eq!(
            one("show impl:Parser>fn.body"),
            show(vec![
                syntax("impl", "Parser"),
                parts(syntax("fn", "*"), &[Part::Body]),
            ])
        );
        let any = |kind: &str| Primary::Syntax {
            kind: kind.into(),
            name: "*".into(),
        };
        assert_eq!(
            one("show fn..struct"),
            show(vec![step(Primary::Range {
                from: Box::new(any("fn")),
                to: Box::new(any("struct")),
            })])
        );
        assert_eq!(
            one("check fn error"),
            CommandKind::Check {
                target: Some(target(vec![syntax("fn", "*")])),
                level: Some(Severity::Error),
            }
        );
        assert_eq!(
            message("show nope"),
            "expected a selector, found `nope`; quote literal text: \"nope\""
        );
    }

    #[test]
    fn any_kind_is_a_star_syntax_step() {
        let show = |steps| CommandKind::Show {
            target: Some(target(steps)),
            context: 0,
            raw: false,
        };
        assert_eq!(one("show *:LIMIT"), show(vec![syntax("*", "LIMIT")]));
        assert_eq!(
            one("show *:Parser>fn:new"),
            show(vec![syntax("*", "Parser"), syntax("fn", "new")])
        );
        assert_eq!(
            one("show *:parse.body"),
            show(vec![parts(syntax("*", "parse"), &[Part::Body])])
        );
        assert_eq!(
            message("show *"),
            "`*` alone selects nothing; *:NAME is the item NAME of any kind, e.g. *:parse, and *:* is every item"
        );
    }

    #[test]
    fn filters_follow_a_step_and_its_parts() {
        let show = |steps| CommandKind::Show {
            target: Some(target(steps)),
            context: 0,
            raw: false,
        };
        let long = Filter::Cond {
            property: Property {
                part: None,
                len: true,
            },
            op: Op::Gt,
            value: Value::Number(1),
        };
        let filtered = |step: Step, filters: Vec<Filter>| Step {
            filters: filters.into_iter().map(|f| (f, 0..0)).collect(),
            ..step
        };
        assert_eq!(
            one("show fn.body[.len > 1]"),
            show(vec![filtered(
                parts(syntax("fn", "*"), &[Part::Body]),
                vec![long.clone()]
            )])
        );
        assert_eq!(
            one("show impl:P>fn[.len > 1][.len > 1]"),
            show(vec![
                syntax("impl", "P"),
                filtered(syntax("fn", "*"), vec![long.clone(), long.clone()]),
            ])
        );
        let any = |kind: &str| Primary::Syntax {
            kind: kind.into(),
            name: "*".into(),
        };
        assert_eq!(
            one("show fn..struct[.len > 1]"),
            show(vec![filtered(
                step(Primary::Range {
                    from: Box::new(any("fn")),
                    to: Box::new(any("struct")),
                }),
                vec![long]
            )])
        );
        let Ok(script) = parse("show fn[.len > 1]>/x/") else {
            panic!("filter before a nested step");
        };
        let CommandKind::Show {
            target: Some(target),
            ..
        } = &script.commands[0].kind
        else {
            panic!("{script:?}");
        };
        assert_eq!(target.selector.steps[0].span, 5..17);
        assert_eq!(target.selector.steps[0].filters[0].1, 7..17);
        assert_eq!(
            message("show fn [.len > 1]"),
            "selectors can't contain spaces; write e.g. `impl:Parser>fn:new`"
        );
        assert_eq!(
            message("show fn[.len > 1].body"),
            "a part can't follow a filter; put it first: fn.body[...]"
        );
    }

    #[test]
    fn ranges() {
        let show = |steps| CommandKind::Show {
            target: Some(target(steps)),
            context: 0,
            raw: false,
        };
        let range = |from: Step, to: Step| {
            step(Primary::Range {
                from: Box::new(from.primary),
                to: Box::new(to.primary),
            })
        };
        assert_eq!(
            one("show /^## 6/../^## 7/"),
            show(vec![range(
                step(Primary::Regex(pattern("^## 6"))),
                step(Primary::Regex(pattern("^## 7")))
            )])
        );
        assert_eq!(
            one("show impl:P>fn:a..fn:c.lines"),
            show(vec![
                syntax("impl", "P"),
                parts(range(syntax("fn", "a"), syntax("fn", "c")), &[Part::Lines]),
            ])
        );
        assert_eq!(
            one(r#"show "BEGIN"..$"#),
            show(vec![range(literal("BEGIN"), lines(Last, None))])
        );
        assert_eq!(error("show /a/ ../b/").kind, E::SpaceInSelector);
        assert_eq!(error("show /a/.. /b/").kind, E::SpaceInSelector);
        assert_eq!(
            message("show /a/-/b/"),
            "unexpected character `-`; ranges between selectors are written SEL..SEL, e.g. /a/../b/"
        );
        assert_eq!(
            message("show import:react-router"),
            r#"unexpected character `-`; quote the name: import:"react-router""#
        );
    }

    #[test]
    fn show_context() {
        let show = |context| CommandKind::Show {
            target: Some(target(vec![syntax("fn", "x")])),
            context,
            raw: false,
        };
        assert_eq!(one("show fn:x +3"), show(3));
        assert_eq!(one("show fn:x+3"), show(3));
        assert_eq!(
            message("show fn:x +"),
            "expected a line count after `+`; write one, e.g. show fn:parse +3"
        );
        assert_eq!(
            message("show +3"),
            "expected a selector, found a context count; usage: show [raw] [SEL [+N]]"
        );
        assert_eq!(
            message("show /re/+0..+70"),
            "expected end of command, found `..`; `+N` is one count of lines around each span, not a range; write show /re/ +70"
        );
        let e = error(r#"show all "x" +2..+5"#);
        assert_eq!(
            rendered(&e),
            r#"expected end of command, found `..`; `+N` is one count of lines around each span, not a range; write show all "x" +5"#
        );
        assert_eq!(e.span, Some(15..17));
        let e = error(r#"show all "x" -3"#);
        assert_eq!(
            rendered(&e),
            r#"unexpected character `-`; context is written `+N`, not `-N`; write show all "x" +3"#
        );
        assert_eq!(e.span, Some(13..14));
        assert_eq!(
            message("show /re/-12 | show 1"),
            "unexpected character `-`; context is written `+N`, not `-N`; write show /re/ +12"
        );
        assert_eq!(
            message("show /re/ -2 # c"),
            "unexpected character `-`; context is written `+N`, not `-N`; write show /re/ +2"
        );
        assert_eq!(
            message("show 10 -20"),
            "unexpected character `-`; context is written `+N`, not `-N`; write show 10 +20, or the line range show 10-20"
        );
        assert_eq!(
            message("show /re/ -3x"),
            "unexpected character `-`; ranges between selectors are written SEL..SEL, e.g. /a/../b/"
        );
        assert_eq!(
            message("show -3"),
            "unexpected character `-`; ranges between selectors are written SEL..SEL, e.g. /a/../b/"
        );
    }

    #[test]
    fn show_raw() {
        let show = |target, context, raw| CommandKind::Show {
            target,
            context,
            raw,
        };
        assert_eq!(one("show raw"), show(None, 0, true));
        assert_eq!(
            one("show raw fn:x +2"),
            show(Some(target(vec![syntax("fn", "x")])), 2, true)
        );
        assert_eq!(
            one("show raw all fn:x"),
            show(Some(all(vec![syntax("fn", "x")])), 0, true)
        );
        assert_eq!(
            one("show fn:raw"),
            show(Some(target(vec![syntax("fn", "raw")])), 0, false)
        );
        assert_eq!(
            commands("show raw; show raw | show raw"),
            [
                show(None, 0, true),
                show(None, 0, true),
                show(None, 0, true)
            ]
        );
    }

    #[test]
    fn several_commands() {
        assert_eq!(commands("show 1; delete 2\n\n# c\nshow").len(), 3);
        assert_eq!(
            commands("insert after 1 <<A\nx\nA\ndelete 2"),
            [
                CommandKind::Insert {
                    position: Position::After,
                    target: target(vec![lines(N(1), None)]),
                    text: text("x", TextKind::Heredoc),
                },
                CommandKind::Delete(target(vec![lines(N(2), None)])),
            ]
        );
    }

    fn stages(src: &str) -> Vec<usize> {
        parse(src)
            .unwrap_or_else(|e| panic!("{}", e.render(Frontend::Cli, Some(src))))
            .stages
    }

    #[test]
    fn pipes_separate_stages_more_loosely_than_semicolons() {
        assert_eq!(stages("show 1; show 2 | show 3"), [2]);
        assert_eq!(stages("show 1 |\nshow 2\nshow 3 | show 4"), [1, 3]);
        assert_eq!(stages("show 1 | # c\nshow 2"), [1]);
        assert_eq!(stages("show /a|b/ | show \"|\""), [1]);
        assert_eq!(
            commands("file a.rs| show 1"),
            [
                CommandKind::File(vec!["a.rs".into()]),
                CommandKind::Show {
                    target: Some(target(vec![lines(N(1), None)])),
                    context: 0,
                    raw: false,
                }
            ]
        );
        assert_eq!(stages("show 1"), Vec::<usize>::new());
    }

    #[test]
    fn commands_without_a_selector_can_end_a_stage() {
        assert_eq!(stages("outline | show 1"), [1]);
        assert_eq!(stages("show | show 1"), [1]);
        assert_eq!(stages("check | show 1"), [1]);
        assert_eq!(stages("check error | show 1"), [1]);
    }

    #[test]
    fn a_pipe_needs_a_command_on_each_side() {
        for src in [
            "| show 1",
            "show 1 || show 2",
            "show 1 | | show 2",
            "show 1 |",
            "show 1 |\n",
        ] {
            assert_eq!(message(src), "`|` needs a command on each side", "{src:?}");
        }
    }

    #[test]
    fn commands_that_read_the_disk_are_errors_after_a_pipe() {
        for (src, what) in [
            ("show 1 | check", "check"),
            ("show 1 | rename fn:a to b", "rename"),
            ("show 1 | show all fn:a.refs", ".refs"),
            ("show 1 | show fn:a>\"f(\".def", ".def"),
        ] {
            assert_eq!(
                message(src),
                format!(
                    "{what} reads the files on disk, which don't hold the edits before a `|`; \
                     run it before the first `|`, or in a separate ned call"
                ),
                "{src:?}"
            );
        }
        assert_eq!(
            stages("check; rename fn:a to b; show fn:a.refs | show 1"),
            [3]
        );
    }

    #[test]
    fn spans() {
        let script = parse("show 1\n  delete all fn:x.body # c").unwrap();
        assert_eq!(script.commands[0].span, 0..6);
        assert_eq!(script.commands[1].span, 9..29);
        let CommandKind::Delete(t) = &script.commands[1].kind else {
            panic!()
        };
        assert_eq!(t.selector.span, 20..29);
        let heredoc = parse("replace 1 with <<E\nx\nE").unwrap();
        assert_eq!(heredoc.commands[0].span, 0..18);
    }

    #[test]
    fn spec_examples_parse() {
        let examples = [
            r#"replace fn:parse>"unexpected end" with "unexpected end of input""#,
            "insert end impl:Parser <<END\n\nfn peek(&self) -> Option<char> {\n    self.src[self.pos..].chars().next()\n}\nEND\n",
            "delete fn:debug_dump",
            r#"replace fn:new.params with "src: impl Into<String>""#,
            "replace fn:parse.body with <<END\nlet tok = self.next().ok_or(Error::Eof)?;\nself.parse_expr(tok)\nEND\n",
            r#"insert after import:std::fmt "use std::io;""#,
            "insert after \"log(req)\".lines <<END\nif req.slow:\n    warn(req)\nEND\n",
            r#"sub /\bold_name\b/ with "new_name""#,
            r#"delete all query{(call_expression function: (identifier) @f (#eq? @f "dbg")) @sel}"#,
        ];
        for src in examples {
            one(src);
        }
        let CommandKind::Insert { text, .. } = one(examples[1]) else {
            panic!()
        };
        assert!(text.value.starts_with("\nfn peek"));
    }

    #[test]
    fn spaces_in_selectors() {
        let src = r#"replace impl:Parser > fn:new with "x""#;
        let e = error(src);
        assert_eq!(
            (e.kind.clone(), e.span.clone()),
            (E::SpaceInSelector, Some(20..21))
        );
        assert_eq!(
            e.render(Frontend::Cli, Some(src)),
            format!(
                "error: script:1:21: selectors can't contain spaces; write e.g. `impl:Parser>fn:new`\n1:{src}\n{}^",
                " ".repeat(22)
            )
        );
        assert_eq!(error("show impl:Parser> fn:new").kind, E::SpaceInSelector);
        assert_eq!(error("show fn:x .body").kind, E::SpaceInSelector);
    }

    #[test]
    fn unknown_and_reserved_commands() {
        let e = error("frobnicate 3");
        assert_eq!(
            (e.kind, e.span),
            (E::UnknownCommand("frobnicate".into()), Some(0..10))
        );
        assert!(matches!(
            error("12").kind,
            E::Expected {
                expected: "a command",
                ..
            }
        ));
    }

    fn expected(src: &str) -> &'static str {
        match error(src).kind {
            E::Expected { expected, .. } => expected,
            other => panic!("{src:?}: {other:?}"),
        }
    }

    #[test]
    fn missing_pieces() {
        assert_eq!(expected(r#"replace 3 "x""#), "`with`");
        assert_eq!(
            expected("replace 3 with fn:x"),
            "text (a string or heredoc)"
        );
        assert_eq!(expected("replace 3 with"), "text (a string or heredoc)");
        assert_eq!(
            expected(r#"insert inside 3 "x""#),
            "before, after, start, or end"
        );
        assert_eq!(expected("delete"), "a selector");
        assert_eq!(expected("delete with"), "a selector");
        assert_eq!(expected("delete all"), "a selector");
        assert_eq!(expected("delete 3 4"), "end of command");
        assert_eq!(expected("show 1 with"), "end of command");
        assert_eq!(expected("file"), "a path");
        assert_eq!(expected("file ; show"), "a path");
    }

    #[test]
    fn move_destination_is_a_single_span() {
        assert_eq!(error("move fn:a after all fn:b").kind, E::AllNotAllowed);
    }

    #[test]
    fn sub_needs_a_pattern() {
        for src in [r#"sub fn:x with "y""#, r#"sub /x/.lines with "y""#] {
            assert_eq!(error(src).kind, E::MissingSubPattern, "{src:?}");
        }
    }

    #[test]
    fn sub_all_with_a_lone_regex_suggests_dropping_all() {
        let src = r#"sub all /x\/y/i with "z""#;
        let e = error(src);
        assert_eq!(
            rendered(&e),
            r"`all` can't be used here; `sub` already replaces every match; drop `all`: sub /x\/y/i with ..."
        );
        assert_eq!(e.span, Some(4..7));
    }

    #[test]
    fn literal_sub_suggests_replace_all() {
        for (src, fix) in [
            (
                r#"sub 1 "- [ ]" with "- [x]""#,
                r#"replace all 1>"- [ ]" with "- [x]""#,
            ),
            (r#"sub "a" with "b""#, r#"replace all "a" with "b""#),
            (
                r#"sub fn:x>"a" with "b""#,
                r#"replace all fn:x>"a" with "b""#,
            ),
            (
                r#"sub all fn:x.body "a" with "b""#,
                r#"replace all fn:x.body>"a" with "b""#,
            ),
            (r#"sub 1 "a" with "$$b""#, r#"replace all 1>"a" with "$b""#),
            (r#"sub 1 "a" with "${0}b""#, r#"replace all 1>"a" with ..."#),
            (r#"sub all "a" with "b""#, r#"replace all "a" with "b""#),
            (
                "sub 1 \"a\" with <<EOF\nb\nEOF",
                r#"replace all 1>"a" with ..."#,
            ),
        ] {
            assert_eq!(
                fix_of(src),
                format!("for a literal, write {fix}"),
                "{src:?}"
            );
        }
        let e = error(r#"show 1 ; sub 3-5 "a" with "b""#);
        assert_eq!(
            rendered(&e),
            r#"expected a regex, found a string; for a literal, write replace all 3-5>"a" with "b""#
        );
        assert_eq!(e.span, Some(17..20));
        assert_eq!(error(r#"sub 1 "a" "b""#).kind, E::MissingSubPattern);
    }

    #[test]
    fn sed_style_sub_suggests_with() {
        for (src, fix) in [
            ("sub 1 /a/b/", r#"sub 1 /a/ with "b""#),
            ("sub /a/b/", r#"sub /a/ with "b""#),
            ("sub /a/b/g", r#"sub /a/ with "b""#),
            ("sub 3-5 /a/b c/gi", r#"sub 3-5 /a/i with "b c""#),
            ("sub 1 /a/b/I", r#"sub 1 /a/i with "b""#),
            (r"sub 1 /a/b\tc/", r#"sub 1 /a/ with "b\tc""#),
            ("sub 1 /a/-b/", r#"sub 1 /a/ with "-b""#),
            (r"sub /x\/y/i/", r#"sub /x\/y/ with "i""#),
            ("sub /a/i/", r#"sub /a/ with "i""#),
            ("sub 1 /a// ; show 1", r#"sub 1 /a/ with """#),
            (
                r#"sub /(a)b/\1&$\/"\n/"#,
                r#"sub /(a)b/ with "${1}${0}$$/\"\n""#,
            ),
        ] {
            let fix = format!("`sub` takes /re/ with TEXT, not sed's /re/text/; write {fix}");
            assert_eq!(fix_of(src), fix, "{src:?}");
        }
        let e = error("show /x/ ; sub 1 /a/b/");
        assert_eq!(
            rendered(&e),
            r#"unknown regex flag `b`; `sub` takes /re/ with TEXT, not sed's /re/text/; write sub 1 /a/ with "b""#
        );
        assert_eq!(e.span, Some(20..21));
    }

    #[test]
    fn sed_style_regexes_elsewhere_keep_their_errors() {
        assert_eq!(error("show /a/b/").kind, E::UnknownRegexFlag('b'));
        assert_eq!(error("sub 1 /a/b c").kind, E::UnknownRegexFlag('b'));
        assert_eq!(error("sub 1 /a/b/c/").kind, E::UnknownRegexFlag('b'));
        // `with` after the regex is `sub`'s own form, whatever TEXT holds.
        assert_eq!(
            error(r#"sub 1 /a/ with 'x = 1/ with "y"'"#).kind,
            E::UnexpectedChar('\'')
        );
    }

    #[test]
    fn sed_style_line_ranges_suggest_a_dash() {
        for (src, fix) in [
            ("show 10,20", "10-20"),
            (r#"sub 1,2 /a/ with "b""#, "1-2"),
            ("show 3,$", "3-$"),
        ] {
            let fix = format!("line ranges are written N-M, not sed's N,M; write {fix}");
            assert_eq!(fix_of(src), fix, "{src:?}");
        }
        let e = error("show 10,20");
        assert_eq!(
            rendered(&e),
            "unexpected character `,`; line ranges are written N-M, not sed's N,M; write 10-20"
        );
    }

    #[test]
    fn sub_references_name_groups_the_regex_has() {
        for src in [
            r#"sub /(a)/ with "$0 $1 ${1}x ${0}""#,
            r#"sub /(?<year>\d+)/ with "$year ${year}s $1""#,
            r#"sub /a/ with "$$1 $$x $ $- ${ x $$""#,
            "sub /a/ with <<E\n$0\nE",
        ] {
            one(src);
        }
    }

    #[test]
    fn sub_references_to_missing_groups_are_errors() {
        assert_eq!(
            message(r#"sub /(\d+) lines/ with "$1deletions""#),
            "`$1deletions` names group `1deletions`, which the regex doesn't have; \
             write `${1}deletions`, or `$$` for a literal `$`"
        );
        assert_eq!(
            message(r#"sub /(?<year>\d+)/ with "$yearly""#),
            "`$yearly` names group `yearly`, which the regex doesn't have; \
             write `${year}ly`, or `$$` for a literal `$`"
        );
        assert_eq!(
            message(r#"sub /(a)(?<b>b)/ with "${c}""#),
            "`${c}` names group `c`, which the regex doesn't have; \
             its groups are `$0` `$1` `${b}`, and `$$` is a literal `$`"
        );
        assert_eq!(
            message(r#"sub /a/ with "$1""#),
            "`$1` names group `1`, which the regex doesn't have; \
             its groups are `$0`, and `$$` is a literal `$`"
        );
        assert_eq!(
            message(r#"sub /a/ with "${10}""#),
            "`${10}` names group `10`, which the regex doesn't have; \
             its groups are `$0`, and `$$` is a literal `$`"
        );
        assert_eq!(
            message(r#"sub /(a)/ with "${}x""#),
            "`${}` names no group; write `$$` for a literal `$`"
        );
    }

    #[test]
    fn sub_reference_errors_point_at_the_reference() {
        let src = r#"sub /(a)/ with "\"\t$1 $2x""#;
        assert_eq!(
            error(src).span,
            Some(src.find("$2").unwrap()..src.len() - 1)
        );
        let src = "sub /a/ with <<E\n$1\nE";
        assert_eq!(error(src).span.unwrap().start, src.find("<<").unwrap());
    }

    #[test]
    fn regexes_are_validated() {
        let e = error("delete /(/");
        assert!(matches!(e.kind, E::InvalidRegex(_)), "{e:?}");
        assert_eq!(e.span, Some(7..10));
        assert!(matches!(
            error(r#"sub /x/ /[/ with "y""#).kind,
            E::InvalidRegex(_)
        ));
    }

    fn message(src: &str) -> String {
        rendered(&error(src))
    }

    /// The error's problem and fix, as the CLI prints them after `error: `.
    fn rendered(error: &ParseError) -> String {
        let rendered = error.render(Frontend::Cli, None);
        rendered.strip_prefix("error: ").unwrap().to_string()
    }

    /// The fix for `src`'s error, as the CLI prints it.
    fn fix_of(src: &str) -> String {
        Frontend::Cli.render(&error(src).fix.expect("a fix").text)
    }

    #[test]
    fn bare_words_suggest_quoting() {
        assert_eq!(
            message(r#"replace x with "y""#),
            r#"expected a selector, found `x`; quote literal text: "x""#
        );
        assert_eq!(
            message("replace 3 with foo"),
            r#"expected text (a string or heredoc), found `foo`; quote literal text: "foo""#
        );
    }

    #[test]
    fn bare_paths_suggest_a_file_step() {
        for (src, path, hint) in [
            ("show a.rs>fn:a", "a.rs", "file:a.rs>fn:a"),
            (
                "show src/a.rs>fn:a; show 1",
                "src/a.rs",
                "file:src/a.rs>fn:a",
            ),
            (
                "delete lib/mod.test.ts",
                "lib/mod.test.ts",
                "file:lib/mod.test.ts",
            ),
            (r#"show src/a.rs>"x y""#, "src/a.rs", "file:src/a.rs>…"),
            ("show a.rs>fn[.len > 3]", "a.rs", "file:a.rs>…"),
        ] {
            assert_eq!(
                message(src),
                format!(
                    "expected a selector, found `{path}`; select the file with a `file:` step: {hint}"
                ),
                "{src:?}"
            );
        }
        assert_eq!(
            message("show self.x"),
            "expected a selector, found `self.x`; quote literal text: \"self.x\", \
             or select the file with a `file:` step: file:self.x"
        );
        assert_eq!(
            message("show x.body"),
            r#"expected a selector, found `x`; quote literal text: "x", or select an item by name: *:x.body"#
        );
    }

    #[test]
    fn bare_names_suggest_selecting_an_item() {
        assert_eq!(
            message("show GitError.body>fn:x"),
            "expected a selector, found `GitError`; quote literal text: \"GitError\", or select an item by name: *:GitError.body>fn:x"
        );
        assert_eq!(
            message("delete GitError; show 1"),
            "expected a selector, found `GitError`; quote literal text: \"GitError\""
        );
    }

    #[test]
    fn an_error_after_a_heredoc_that_ended_early_names_it() {
        let src = "insert end fn:x <<EOF\na\nEOF\nb `c\nEOF\n";
        assert!(
            message(src).ends_with("; the heredoc <<EOF at line 1 ended at line 3, which holds only EOF: pick a tag its text doesn't hold"),
            "{}",
            message(src)
        );
        // Without the tag again later, the heredoc ended where it was meant to.
        assert!(!message("insert end fn:x <<EOF\na\nEOF\nb `c\n").contains("heredoc"));
        // An error before the heredoc's end is not about it.
        assert!(!message("insert end fn:x x <<EOF\na\nEOF\nb\nEOF\n").contains("ended at"));
    }

    #[test]
    fn other_syntax_errors_show_the_usage() {
        assert_eq!(
            message(r#"replace 3 "x""#),
            "expected `with`, found a string; usage: replace [all] SEL with TEXT"
        );
        assert_eq!(
            message(r#"insert inside 3 "a""#),
            "expected before, after, start, or end, found `inside`; \
             usage: insert before|after|start|end [all] SEL TEXT"
        );
        assert_eq!(
            message("delete"),
            "expected a selector, found end of script; usage: delete [all] SEL"
        );
        assert_eq!(
            message("move 3 after"),
            "expected a selector, found end of script; \
             usage: move [all] SEL before|after|start|end DEST"
        );
        assert_eq!(
            message(r#""x""#),
            "expected a command, found a string; \
             commands are show outline check replace insert delete sub move rename resolve file create allow"
        );
    }

    #[test]
    fn a_heredoc_selector_keeps_the_command_on_its_line() {
        let hint = "a heredoc's body starts on the next line, so finish the command before it:";
        assert_eq!(
            message("replace <<END\nx\nEND\nwith \"y\""),
            format!("expected `with`, found end of line; {hint} replace <<END with TEXT")
        );
        assert_eq!(
            message("replace all <<'END'  \nx\nEND\nwith <<B\ny\nB"),
            format!("expected `with`, found end of line; {hint} replace all <<'END' with TEXT")
        );
        assert_eq!(
            message("move <<END\nx\nEND\nafter 3"),
            format!(
                "expected before, after, start, or end, found end of line; \
                 {hint} move <<END before|after|start|end DEST"
            )
        );
        assert_eq!(
            message("insert before <<END\nx\nEND\n\"y\""),
            format!(
                "expected text (a string or heredoc), found end of line; \
                 {hint} insert before <<END TEXT"
            )
        );
        // A heredoc that isn't the last thing before the line break gets the usage.
        assert_eq!(
            message("replace <<END.lines\nx\nEND\nwith \"y\""),
            "expected `with`, found end of line; usage: replace [all] SEL with TEXT"
        );
    }

    #[test]
    fn a_second_selector_suggests_a_command_each() {
        for (src, split) in [
            ("show fn:a fn:b", "show fn:a; show fn:b"),
            (
                "show all /x/ impl:P>fn:new.body",
                "show all /x/; show all impl:P>fn:new.body",
            ),
            ("show fn:a +3 10..12", "show fn:a +3; show 10..12"),
            ("outline impl:P \"x\"", "outline impl:P; outline \"x\""),
            ("delete 3 4", "delete 3; delete 4"),
        ] {
            let message = message(src);
            assert!(
                message.starts_with("expected end of command, found ")
                    && message.ends_with(&format!(
                        "; one selector per command; \
                         separate commands with `;` or a new line: {split}"
                    )),
                "{src:?}: {message}"
            );
        }
        assert_eq!(error("show fn:a fn:b").span, Some(10..14));
        assert_eq!(
            message("show 1 with"),
            "expected end of command, found `with`"
        );
        assert_eq!(
            message(r#"replace 3 with "x" fn:a"#),
            "expected end of command, found `fn:a`"
        );
    }

    #[test]
    fn a_search_glued_to_a_step_suggests_nesting_it() {
        for (src, nested) in [
            ("show impl:X>fn:y/z/", "show impl:X>fn:y>/z/"),
            (r#"delete fn:y"x""#, r#"delete fn:y>"x""#),
            ("outline impl:P`fn @f() {}`", "outline impl:P>`fn @f() {}`"),
            ("show all fn:y.body/z/i", "show all fn:y.body>/z/i"),
            ("show fn:y/z/.body +2", "show fn:y>/z/.body +2"),
            (
                r#"replace fn:y/z/ with "a""#,
                r#"replace fn:y>/z/ with "a""#,
            ),
            (
                "replace fn:y`f()` with <<END\nx\nEND\n",
                "replace fn:y>`f()` with <<END",
            ),
            (
                r#"insert after fn:y/z/ "x""#,
                r#"insert after fn:y>/z/ "x""#,
            ),
            ("move fn:y/z/ after fn:a", "move fn:y>/z/ after fn:a"),
            ("move fn:a after fn:y/z/", "move fn:a after fn:y>/z/"),
            (
                r#"sub fn:y"x" /a/ with "b""#,
                r#"sub fn:y>"x" /a/ with "b""#,
            ),
        ] {
            assert_eq!(
                fix_of(src),
                format!("a search in a step goes after `>`; write {nested}"),
                "{src:?}"
            );
        }
        assert_eq!(error("replace fn:y/z/ with \"a\"").span, Some(12..15));
        for src in ["show fn:y /z/", r#"show fn:y +3"x""#] {
            assert!(
                message(src).contains("one selector per command"),
                "{src:?}: {}",
                message(src)
            );
        }
    }

    #[test]
    fn text_or_a_regex_glued_to_a_selector_may_be_the_command_s() {
        for src in [r#"insert after fn:y"x""#, r#"sub fn:y/re/ with "x""#] {
            assert!(parse(src).is_ok(), "{src:?}");
        }
    }

    #[test]
    fn all_after_the_selector_goes_before_it() {
        for (src, fix) in [
            ("show /x/ all", "show all /x/"),
            ("show fn:a.body all +2", "show all fn:a.body ..."),
            ("outline impl:P all", "outline all impl:P"),
            ("delete \"x\" all", "delete all \"x\""),
            ("check /x/ all error", "check all /x/ ..."),
            (r#"replace /x/ all with "y""#, "replace all /x/ ..."),
            (r#"insert after /x/ all "y""#, "insert after all /x/ ..."),
            ("move fn:a all after fn:b", "move all fn:a ..."),
            ("resolve conflict all ours", "resolve all conflict ..."),
        ] {
            assert_eq!(
                fix_of(src),
                format!("`all` goes before the selector; write {fix}"),
                "{src:?}"
            );
        }
        assert_eq!(error("show /x/ all").span, Some(9..12));
    }

    #[test]
    fn all_after_a_step_goes_before_the_selector() {
        for (src, fixed) in [
            ("show file:a.rs>all /x/", "show all file:a.rs>/x/"),
            (r#"delete fn:f>all "x""#, r#"delete all fn:f>"x""#),
            ("show raw fn:f>all", "show raw all fn:f>..."),
        ] {
            let e = error(src);
            let found = E::Expected {
                expected: "a selector",
                found: "`all`".into(),
            };
            assert_eq!(e.kind, found, "{src:?}");
            assert_eq!(
                fix_of(src),
                format!("`all` goes before the selector; write {fixed}"),
                "{src:?}"
            );
        }
    }

    #[test]
    fn sub_with_all_after_its_regex_suggests_dropping_all() {
        let e = error(r#"sub /x/i all with "y""#);
        assert_eq!(
            rendered(&e),
            "`all` can't be used here; `sub` already replaces every match; drop `all`: sub /x/i with ..."
        );
        assert_eq!(e.span, Some(9..12));
        assert_eq!(
            error(r#"sub fn:a all /x/ with "y""#).kind,
            E::MissingSubPattern
        );
    }

    #[test]
    fn all_at_the_end_of_a_command_goes_before_its_selector() {
        for (src, fix) in [
            (r#"replace /x/ with "y" all"#, "replace all /x/ ..."),
            (r#"insert after fn:a "y" all"#, "insert after all fn:a ..."),
            ("resolve conflict ours all", "resolve all conflict ..."),
            ("check /x/ error all", "check all /x/ ..."),
        ] {
            assert_eq!(
                fix_of(src),
                format!("`all` goes before the selector; write {fix}"),
                "{src:?}"
            );
        }
        assert_eq!(error(r#"replace 3 with "y" all"#).span, Some(19..22));
        for src in [r#"sub 1 /e/ with "x" all"#, r#"sub 1 /e/ all with "x""#] {
            assert_eq!(
                message(src),
                "`all` can't be used here; `sub` already replaces every match; drop `all`: sub 1 /e/ with ...",
                "{src:?}"
            );
        }
        assert_eq!(error("move fn:b before fn:a all").kind, E::AllNotAllowed);
    }

    #[test]
    fn insert_start_or_end_needs_a_selector() {
        assert_eq!(
            message(r#"insert end "x""#),
            "`insert end` needs a selector before the text, e.g. insert end fn:NAME TEXT; \
         insert after $ TEXT appends to the file"
        );
        assert_eq!(
            message("insert start <<END\nx\nEND\n"),
            "`insert start` needs a selector before the text, e.g. insert start fn:NAME TEXT; \
         insert before 1 TEXT adds to the top of the file"
        );
        assert_eq!(error(r#"insert end "x"; show"#).span, Some(0..14));
        // A selector that isn't text is still missing its text.
        assert_eq!(expected("insert end fn:x"), "text (a string or heredoc)");
        assert_eq!(
            expected(r#"insert after "x""#),
            "text (a string or heredoc)"
        );
    }

    #[test]
    fn every_command_has_a_usage_starting_with_it() {
        for verb in [
            "show", "outline", "replace", "insert", "delete", "sub", "move", "file",
        ] {
            let usage = usage(verb).unwrap_or_else(|| panic!("no usage for {verb}"));
            assert!(usage.starts_with(verb), "{usage}");
        }
        assert_eq!(usage("frobnicate"), None);
        assert_eq!(usage("show"), Some("show [raw] [SEL [+N]]"));
    }

    #[test]
    fn lexical_errors_suggest_fixes() {
        assert_eq!(
            message("show @"),
            r#"unexpected character `@`; quote literal text: "...""#
        );
        assert_eq!(
            message("show 'x'"),
            r#"unexpected character `'`; strings use double quotes: "...""#
        );
        assert_eq!(
            message("show 0"),
            "line numbers start at 1; use 1 for the first line"
        );
        assert_eq!(
            message("show 99999999999999999999999"),
            "line number is too large; use `$` for the last line"
        );
        assert_eq!(
            message("replace 1 with <<END\nx\n"),
            "unterminated heredoc <<END (started here); end it with a line holding only END"
        );
        assert_eq!(
            message("show /(/"),
            r#"invalid regex: unclosed group; escape literal characters such as ( [ . * with \, or select a "string""#
        );
        assert_eq!(
            message(r#"show "\r""#),
            r#"invalid escape `\r`; write `\\r` for a backslash and r (strings support \n \t \" \\)"#
        );
        assert_eq!(
            message("show import:app.models.user"),
            r#"unknown part `.models`; quote a name that has dots: import:"app.models.user""#
        );
        assert_eq!(
            message("show fn:App.handle"),
            r#"unknown part `.handle`; to name a member, nest it: KIND:App>fn:handle (a Go method: fn:"App.handle")"#
        );
        assert_eq!(
            message("show class:A.B"),
            "unknown part `.B`; to name a member, nest it: KIND:A>class:B"
        );
    }

    #[test]
    fn refs_and_def_are_parts_not_kinds() {
        assert_eq!(
            one("show all fn:parse.refs"),
            CommandKind::Show {
                target: Some(all(vec![parts(syntax("fn", "parse"), &[Part::Refs])])),
                context: 0,
                raw: false,
            }
        );
        assert_eq!(
            one(r#"show "f(".def.lines"#),
            CommandKind::Show {
                target: Some(target(vec![parts(
                    literal("f("),
                    &[Part::Def, Part::Lines]
                )])),
                context: 0,
                raw: false,
            }
        );
        assert_eq!(
            message("show refs:foo"),
            "`refs:` is a part, not a kind; select the symbol and add .refs, e.g. fn:NAME.refs"
        );
        assert_eq!(
            message("show def:foo"),
            "`def:` is a part, not a kind; select the symbol and add .def, e.g. fn:NAME.def"
        );
    }

    #[test]
    fn check_takes_an_optional_target_and_level() {
        assert_eq!(
            one("check"),
            CommandKind::Check {
                target: None,
                level: None
            }
        );
        assert_eq!(
            one("check hint"),
            CommandKind::Check {
                target: None,
                level: Some(Severity::Hint)
            }
        );
        assert_eq!(
            one("check fn:x"),
            CommandKind::Check {
                target: Some(target(vec![syntax("fn", "x")])),
                level: None
            }
        );
        assert_eq!(
            commands("check all fn:x error; check")[0],
            CommandKind::Check {
                target: Some(all(vec![syntax("fn", "x")])),
                level: Some(Severity::Error)
            }
        );
    }

    #[test]
    fn unknown_check_levels_list_the_levels() {
        assert_eq!(
            message("check fn:x bogus"),
            "unknown level `bogus`; levels are error warning info hint"
        );
        assert_eq!(
            message("check warnings"),
            "unknown level `warnings`; did you mean `warning`?"
        );
        assert!(matches!(
            error("check fn:x hint more").kind,
            E::Expected { .. }
        ));
    }

    #[test]
    fn allow_takes_errors_or_warnings() {
        assert_eq!(one("allow errors"), CommandKind::Allow(Severity::Error));
        assert_eq!(one("allow warnings"), CommandKind::Allow(Severity::Warning));
        assert_eq!(
            message("allow maybe"),
            "expected `errors` or `warnings`, found `maybe`; usage: allow errors|warnings"
        );
        assert!(matches!(error("allow").kind, E::Expected { .. }));
    }

    #[test]
    fn script_text_in_a_fix_prints_as_written() {
        for (src, written) in [
            (r#"show "{-w} {{x}}" -3"#, r#"write show "{-w} {{x}}" +3"#),
            (r#"show 1, "{mcp:y}""#, r#"show 1; show "{mcp:y}""#),
            (r#"show fn:a"{-w}""#, r#"write show fn:a>"{-w}""#),
        ] {
            let err = parse(src).unwrap_err();
            for frontend in [Frontend::Cli, Frontend::Mcp, Frontend::Repl] {
                let rendered = err.render(frontend, Some(src));
                assert!(rendered.contains(written), "{frontend:?}: {rendered}");
            }
        }
    }
}
