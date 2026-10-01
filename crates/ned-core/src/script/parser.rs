//! Parser from script text to [`Script`] (command-language spec, §2.2).

use std::ops::Range;

use super::ast::*;
use super::error::{COMMANDS, ParseError, ParseErrorKind as E};
use super::lexer::{Lexer, Token, TokenKind};
use crate::lsp::Severity;

pub fn parse(src: &str) -> Result<Script, ParseError> {
    let mut parser = Parser {
        lexer: Lexer::new(src),
        peeked: None,
        last_end: 0,
    };
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
                    return Err(ParseError::new(E::EmptyStage, token.span.clone()));
                }
                pipe = parser.bump()?.span;
                stages.push(commands.len());
            }
            TokenKind::Eof => {
                if stages.last() == Some(&commands.len()) {
                    return Err(ParseError::new(E::EmptyStage, pipe));
                }
                let later = stages.first().map_or(&[][..], |&first| &commands[first..]);
                if let Some((what, command)) = later
                    .iter()
                    .find_map(|c| reads_disk(&c.kind).map(|what| (what, c)))
                {
                    return Err(ParseError::new(E::ReadsDisk(what), command.span.clone()));
                }
                return Ok(Script { commands, stages });
            }
            _ => {
                commands.push(parser.command()?);
                let next = parser.peek()?;
                if !matches!(
                    next.kind,
                    TokenKind::Newline | TokenKind::Semicolon | TokenKind::Pipe | TokenKind::Eof
                ) {
                    return Err(expected("end of command", next));
                }
            }
        }
    }
}

/// What in a command reads the files on disk, which a stage after a `|` can't
/// use (§2.3): `check`, `rename`, or a `.refs` or `.def` part.
fn reads_disk(kind: &CommandKind) -> Option<&'static str> {
    let selectors: Vec<&Selector> = match kind {
        CommandKind::Check { .. } => return Some("check"),
        CommandKind::Rename { .. } => return Some("rename"),
        CommandKind::Show { target, .. } | CommandKind::Outline(target) => {
            target.iter().map(|t| &t.selector).collect()
        }
        CommandKind::Sub { scope, .. } => scope.iter().map(|t| &t.selector).collect(),
        CommandKind::Replace { target, .. }
        | CommandKind::Insert { target, .. }
        | CommandKind::Delete(target) => vec![&target.selector],
        CommandKind::Move { target, dest, .. } => vec![&target.selector, dest],
        CommandKind::File(_) | CommandKind::Create { .. } | CommandKind::Allow(_) => vec![],
    };
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
    lexer: Lexer<'a>,
    peeked: Option<Token>,
    /// End of the last consumed token.
    last_end: usize,
}

impl Parser<'_> {
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
        self.last_end = token.span.end;
        Ok(token)
    }

    fn peek_is_word(&mut self, word: &str) -> Result<bool, ParseError> {
        Ok(matches!(&self.peek()?.kind, TokenKind::Word(w) if w == word))
    }

    fn command(&mut self) -> Result<Command, ParseError> {
        let verb = self.bump()?;
        let TokenKind::Word(word) = &verb.kind else {
            let mut err = expected("a command", &verb);
            if let E::Expected { hint, .. } = &mut err.kind {
                *hint = format!("; commands are {COMMANDS}");
            }
            return Err(err);
        };
        let kind = self.command_kind(&verb, word).map_err(|mut err| {
            if let E::Expected { hint, .. } = &mut err.kind
                && hint.is_empty()
                && let Some(usage) = usage(word)
            {
                *hint = format!("; usage: {usage}");
            }
            err
        })?;
        Ok(Command {
            kind,
            span: verb.span.start..self.last_end,
        })
    }

    fn command_kind(&mut self, verb: &Token, word: &str) -> Result<CommandKind, ParseError> {
        Ok(match word {
            "show" => {
                let target = self.optional_target()?;
                let context = match self.peek()?.kind {
                    TokenKind::Context(n) if target.is_some() => {
                        self.bump()?;
                        n
                    }
                    _ => 0,
                };
                CommandKind::Show { target, context }
            }
            "outline" => CommandKind::Outline(self.optional_target()?),
            "replace" => {
                let target = self.target()?;
                self.expect_with()?;
                CommandKind::Replace {
                    target,
                    text: self.text()?,
                }
            }
            "insert" => CommandKind::Insert {
                position: self.position()?,
                target: self.target()?,
                text: self.text()?,
            },
            "delete" => CommandKind::Delete(self.target()?),
            "sub" => self.sub()?,
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
            _ => {
                return Err(ParseError::new(
                    E::UnknownCommand(word.into()),
                    verb.span.clone(),
                ));
            }
        })
    }

    fn optional_target(&mut self) -> Result<Option<Target>, ParseError> {
        match self.peek()?.kind {
            TokenKind::Newline | TokenKind::Semicolon | TokenKind::Eof => Ok(None),
            _ => self.target().map(Some),
        }
    }

    /// After `check`: an optional target, then an optional level.
    fn check(&mut self) -> Result<CommandKind, ParseError> {
        let target = match &self.peek()?.kind {
            TokenKind::Word(word) if word != "all" => None,
            _ => self.optional_target()?,
        };
        let level = match &self.peek()?.kind {
            TokenKind::Word(word) => {
                let word = word.clone();
                let token = self.bump()?;
                let level = word.parse().map_err(|()| {
                    let fix = match word.strip_suffix('s').map(str::parse::<Severity>) {
                        Some(Ok(level)) => format!("did you mean `{level}`?"),
                        _ => "levels are error warning info hint".into(),
                    };
                    ParseError::new(E::UnknownLevel { word, fix }, token.span)
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

    fn target(&mut self) -> Result<Target, ParseError> {
        let all = self.peek_is_word("all")?;
        if all {
            self.bump()?;
        }
        Ok(Target {
            all,
            selector: self.selector()?,
        })
    }

    fn dest(&mut self) -> Result<Selector, ParseError> {
        if self.peek_is_word("all")? {
            let token = self.bump()?;
            return Err(ParseError::new(E::AllNotAllowed, token.span));
        }
        self.selector()
    }

    fn selector(&mut self) -> Result<Selector, ParseError> {
        let first = self.bump()?;
        let start = first.span.start;
        let mut steps = vec![Step {
            primary: self.range(first)?,
            parts: Vec::new(),
            span: start..self.last_end,
        }];
        loop {
            let next = self.peek()?;
            match next.kind {
                TokenKind::Gt | TokenKind::Part(_) if next.space_before => {
                    return Err(ParseError::new(E::SpaceInSelector, next.span.clone()));
                }
                TokenKind::Part(part) => {
                    self.bump()?;
                    if let Some(step) = steps.last_mut() {
                        step.parts.push(part);
                        step.span.end = self.last_end;
                    }
                }
                TokenKind::Gt => {
                    self.bump()?;
                    let token = self.bump()?;
                    if token.space_before {
                        return Err(ParseError::new(E::SpaceInSelector, token.span));
                    }
                    let step_start = token.span.start;
                    steps.push(Step {
                        primary: self.range(token)?,
                        parts: Vec::new(),
                        span: step_start..self.last_end,
                    });
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
        let from = primary(first)?;
        let next = self.peek()?;
        if next.kind != TokenKind::DotDot {
            return Ok(from);
        }
        if next.space_before {
            return Err(ParseError::new(E::SpaceInSelector, next.span.clone()));
        }
        self.bump()?;
        let token = self.bump()?;
        if token.space_before {
            return Err(ParseError::new(E::SpaceInSelector, token.span));
        }
        Ok(Primary::Range {
            from: Box::new(from),
            to: Box::new(primary(token)?),
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
        let first = self.target()?;
        let (scope, pattern) = if self.peek_is_word("with")? {
            let span = first.selector.span.clone();
            let mut steps = first.selector.steps;
            match (first.all, steps.pop(), steps.is_empty()) {
                (
                    false,
                    Some(Step {
                        primary: Primary::Regex(pattern),
                        parts,
                        ..
                    }),
                    true,
                ) if parts.is_empty() => (None, pattern),
                _ => return Err(ParseError::new(E::MissingSubPattern, span)),
            }
        } else {
            let token = self.bump()?;
            let TokenKind::Regex { pattern, flags } = token.kind else {
                return Err(ParseError::new(E::MissingSubPattern, token.span));
            };
            (Some(first), validate_regex(pattern, flags, token.span)?)
        };
        self.expect_with()?;
        Ok(CommandKind::Sub {
            scope,
            pattern,
            text: self.text()?,
        })
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

fn primary(token: Token) -> Result<Primary, ParseError> {
    Ok(match token.kind {
        TokenKind::Lines { start, end } => Primary::Lines { start, end },
        TokenKind::Regex { pattern, flags } => {
            Primary::Regex(validate_regex(pattern, flags, token.span)?)
        }
        TokenKind::Syntax { kind, name } => match kind.as_str() {
            "file" => Primary::File(name),
            "refs" | "def" => return Err(ParseError::new(E::PartAsKind(kind), token.span)),
            _ => Primary::Syntax { kind, name },
        },
        TokenKind::Query(query) => Primary::Query(query),
        kind => match text_from(kind) {
            Ok(text) => Primary::Literal(text),
            Err(kind) => return Err(expected("a selector", &Token { kind, ..token })),
        },
    })
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

fn validate_regex(
    source: String,
    flags: RegexFlags,
    span: Range<usize>,
) -> Result<Pattern, ParseError> {
    let pattern = Pattern { source, flags };
    if let Err(err) = pattern.regex() {
        // The regex crate's message is a multi-line excerpt ending in
        // `error: <reason>`; keep only the reason.
        let message = err.to_string();
        let reason = message
            .lines()
            .find_map(|l| l.strip_prefix("error: "))
            .unwrap_or(&message);
        return Err(ParseError::new(E::InvalidRegex(reason.into()), span));
    }
    Ok(pattern)
}

const KEYWORDS: [&str; 8] = [
    "all", "with", "to", "before", "after", "start", "end", "file",
];

/// The syntax of the command `verb`, as `ned help VERB` starts.
pub fn usage(verb: &str) -> Option<&'static str> {
    Some(match verb {
        "show" => "show [SEL [+N]]",
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
        TokenKind::Part(_) => "a part".into(),
        TokenKind::Path(_) => "a path".into(),
        TokenKind::Gt => "`>`".into(),
        TokenKind::DotDot => "`..`".into(),
        TokenKind::Context(_) => "a context count".into(),
        TokenKind::Semicolon => "`;`".into(),
        TokenKind::Pipe => "`|`".into(),
        TokenKind::Newline => "end of line".into(),
        TokenKind::Eof => "end of script".into(),
    };
    let hint = match &token.kind {
        // A keyword out of place is a usage mistake, not unquoted text.
        TokenKind::Word(w)
            if (what == "a selector" || what.starts_with("text"))
                && !KEYWORDS.contains(&w.as_str())
                && usage(w).is_none() =>
        {
            format!("; quote literal text: \"{w}\"")
        }
        _ => String::new(),
    };
    ParseError::new(
        E::Expected {
            expected: what,
            found,
            hint,
        },
        token.span.clone(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::ParseErrorKind as E;
    use LineNo::{Last, Number as N};

    fn commands(src: &str) -> Vec<CommandKind> {
        parse(src)
            .unwrap_or_else(|e| panic!("{}", e.render(src)))
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
                .map(|step| Step { span: 0..0, ..step })
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
            Show { target, context } => Show {
                target: target.map(unspan_target),
                context,
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
        Pattern {
            source: source.into(),
            flags: RegexFlags::default(),
        }
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
                context: 0
            }
        );
        assert_eq!(one("outline"), CommandKind::Outline(None));
        assert_eq!(
            one("show 12-20"),
            CommandKind::Show {
                target: Some(target(vec![lines(N(12), Some(N(20)))])),
                context: 0,
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
                pattern: Pattern {
                    source: "x".into(),
                    flags: RegexFlags {
                        case_insensitive: true,
                        dot_all: false,
                    },
                },
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
    }

    #[test]
    fn bare_kinds_are_wildcard_syntax_steps() {
        let show = |steps| CommandKind::Show {
            target: Some(target(steps)),
            context: 0,
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
    fn ranges() {
        let show = |steps| CommandKind::Show {
            target: Some(target(steps)),
            context: 0,
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
    }

    #[test]
    fn show_context() {
        let show = |context| CommandKind::Show {
            target: Some(target(vec![syntax("fn", "x")])),
            context,
        };
        assert_eq!(one("show fn:x +3"), show(3));
        assert_eq!(one("show fn:x+3"), show(3));
        assert_eq!(
            message("show fn:x +"),
            "expected a line count after `+`, e.g. show fn:parse +3"
        );
        assert_eq!(
            message("show +3"),
            "expected a selector, found a context count; usage: show [SEL [+N]]"
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
            .unwrap_or_else(|e| panic!("{}", e.render(src)))
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
                    context: 0
                }
            ]
        );
        assert_eq!(stages("show 1"), Vec::<usize>::new());
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
            (E::SpaceInSelector, 20..21)
        );
        assert_eq!(
            e.render(src),
            format!(
                "error: script:1:21: {}\n1:{src}\n{}^",
                E::SpaceInSelector,
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
            (E::UnknownCommand("frobnicate".into()), 0..10)
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
        for src in [
            r#"sub fn:x with "y""#,
            r#"sub all /x/ with "y""#,
            r#"sub /x/.lines with "y""#,
            r#"sub fn:x "y" with "z""#,
        ] {
            assert_eq!(error(src).kind, E::MissingSubPattern, "{src:?}");
        }
    }

    #[test]
    fn regexes_are_validated() {
        let e = error("delete /(/");
        assert!(matches!(e.kind, E::InvalidRegex(_)), "{e:?}");
        assert_eq!(e.span, 7..10);
        assert!(matches!(
            error(r#"sub /x/ /[/ with "y""#).kind,
            E::InvalidRegex(_)
        ));
    }

    fn message(src: &str) -> String {
        error(src).kind.to_string()
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
             commands are show outline check replace insert delete sub move rename file create allow"
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
        assert_eq!(usage("show"), Some("show [SEL [+N]]"));
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
    }

    #[test]
    fn refs_and_def_are_parts_not_kinds() {
        assert_eq!(
            one("show all fn:parse.refs"),
            CommandKind::Show {
                target: Some(all(vec![parts(syntax("fn", "parse"), &[Part::Refs])])),
                context: 0,
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
}
