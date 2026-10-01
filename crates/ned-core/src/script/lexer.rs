//! Tokenizer for scripts.
//!
//! The lexer is pulled by the parser, which calls [`Lexer::path`] instead of
//! [`Lexer::next_token`] where the grammar expects `file` paths. Heredoc
//! bodies are read as soon as their `<<TAG` is lexed; the lexer then skips
//! over them when it reaches the end of the command line.

use std::ops::Range;

use super::ast::{Filter, LineNo, Part, RegexFlags};
use super::error::{ParseError, ParseErrorKind as E};

mod filter;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Range<usize>,
    /// Whether whitespace, a comment, or the start of a line precedes the
    /// token. Selectors may not contain whitespace.
    pub space_before: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    /// A verb or keyword.
    Word(String),
    /// `kind:name`; for `file:PATH`, `name` is the path.
    Syntax {
        kind: String,
        name: String,
    },
    Lines {
        start: LineNo,
        end: Option<LineNo>,
    },
    Regex {
        pattern: String,
        flags: RegexFlags,
    },
    Str(String),
    /// `<<TAG` (or `<<'TAG'` when `raw`); `body` excludes the terminator.
    Heredoc {
        tag: String,
        body: String,
        raw: bool,
    },
    Query(String),
    /// A syntax pattern, `` `CODE` `` (§3.10).
    Code(String),
    Part(Part),
    Filter(Filter),
    /// A `file` command argument, from [`Lexer::path`].
    Path(String),
    Gt,
    /// `..`, between the ends of a range.
    DotDot,
    /// `+N`: lines of context for `show`.
    Context(usize),
    Semicolon,
    /// `|`, between stages.
    Pipe,
    Newline,
    Eof,
}

#[derive(Debug)]
pub struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    /// Where the next heredoc body on the current line starts.
    heredoc_cursor: Option<usize>,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Lexer {
            src,
            pos: 0,
            heredoc_cursor: None,
        }
    }

    pub fn next_token(&mut self) -> Result<Token, ParseError> {
        let space_before = self.skip_space();
        let start = self.pos;
        let Some(c) = self.peek() else {
            return Ok(Token {
                kind: TokenKind::Eof,
                span: start..start,
                space_before,
            });
        };
        let kind = match c {
            '\n' | '\r' => {
                let end = start + if c == '\r' { 2 } else { 1 };
                self.pos = self.heredoc_cursor.take().unwrap_or(end);
                return Ok(Token {
                    kind: TokenKind::Newline,
                    span: start..end,
                    space_before,
                });
            }
            ';' => {
                self.pos += 1;
                TokenKind::Semicolon
            }
            '|' => {
                self.pos += 1;
                TokenKind::Pipe
            }
            '>' => {
                self.pos += 1;
                TokenKind::Gt
            }
            '"' => TokenKind::Str(self.string()?),
            '/' => self.regex()?,
            '<' => self.heredoc()?,
            '$' | '0'..='9' => self.lines()?,
            '+' => TokenKind::Context(self.context()?),
            '.' if self.src[self.pos..].starts_with("..") => {
                self.pos += 2;
                TokenKind::DotDot
            }
            '.' => TokenKind::Part(self.part()?),
            '[' => TokenKind::Filter(self.filter()?),
            '`' => self.code(start)?,
            c if c.is_ascii_alphabetic() || c == '_' => self.word()?,
            c => {
                return Err(ParseError::new(
                    E::UnexpectedChar(c),
                    start..start + c.len_utf8(),
                ));
            }
        };
        Ok(Token {
            kind,
            span: start..self.pos,
            space_before,
        })
    }

    /// Lexes one `file` argument: a quoted string, or a run of characters up
    /// to whitespace, `;` or `|`. Returns `None`, consuming nothing, at the end
    /// of the command.
    pub fn path(&mut self) -> Result<Option<Token>, ParseError> {
        let space_before = self.skip_space();
        let start = self.pos;
        let path = match self.peek() {
            None | Some(';' | '\n' | '\r' | '|') => return Ok(None),
            Some('"') => self.string()?,
            Some(_) => self
                .take_while(|c| !matches!(c, ' ' | '\t' | '\r' | '\n' | ';' | '|'))
                .to_string(),
        };
        Ok(Some(Token {
            kind: TokenKind::Path(path),
            span: start..self.pos,
            space_before,
        }))
    }

    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn peek_second(&self) -> Option<char> {
        self.src[self.pos..].chars().nth(1)
    }

    fn take_while(&mut self, pred: impl Fn(char) -> bool) -> &'a str {
        let start = self.pos;
        while let Some(c) = self.peek().filter(|&c| pred(c)) {
            self.pos += c.len_utf8();
        }
        &self.src[start..self.pos]
    }

    /// Skips spaces, tabs, stray `\r`s, and comments, stopping at a newline.
    /// Returns whether the next token is preceded by whitespace or starts a
    /// line.
    fn skip_space(&mut self) -> bool {
        let start = self.pos;
        loop {
            match self.peek() {
                Some(' ' | '\t') => self.pos += 1,
                Some('\r') if self.peek_second() != Some('\n') => self.pos += 1,
                Some('#') => {
                    self.take_while(|c| c != '\n');
                }
                _ => break,
            }
        }
        self.pos > start || self.pos == 0 || self.src[..self.pos].ends_with('\n')
    }

    fn string(&mut self) -> Result<String, ParseError> {
        let start = self.pos;
        self.pos += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek().filter(|&c| c != '\n') else {
                return Err(ParseError::new(E::UnterminatedString, start..self.pos));
            };
            self.pos += c.len_utf8();
            match c {
                '"' => return Ok(out),
                '\\' => {
                    let escape = self.peek().filter(|&c| c != '\n');
                    let Some(e) = escape else {
                        return Err(ParseError::new(E::UnterminatedString, start..self.pos));
                    };
                    out.push(match e {
                        'n' => '\n',
                        't' => '\t',
                        '"' => '"',
                        '\\' => '\\',
                        _ => {
                            let span = self.pos - 1..self.pos + e.len_utf8();
                            return Err(ParseError::new(E::InvalidEscape(e), span));
                        }
                    });
                    self.pos += e.len_utf8();
                }
                c => out.push(c),
            }
        }
    }

    fn regex(&mut self) -> Result<TokenKind, ParseError> {
        let start = self.pos;
        self.pos += 1;
        let mut pattern = String::new();
        loop {
            let Some(c) = self.peek().filter(|&c| c != '\n') else {
                return Err(ParseError::new(E::UnterminatedRegex, start..self.pos));
            };
            self.pos += c.len_utf8();
            match c {
                '/' => break,
                '\\' => match self.peek().filter(|&c| c != '\n') {
                    None => return Err(ParseError::new(E::UnterminatedRegex, start..self.pos)),
                    Some('/') => {
                        pattern.push('/');
                        self.pos += 1;
                    }
                    Some(e) => {
                        pattern.push('\\');
                        pattern.push(e);
                        self.pos += e.len_utf8();
                    }
                },
                c => pattern.push(c),
            }
        }
        let mut flags = RegexFlags::default();
        while let Some(c) = self.peek().filter(char::is_ascii_alphabetic) {
            match c {
                'i' => flags.case_insensitive = true,
                's' => flags.dot_all = true,
                _ => {
                    return Err(ParseError::new(
                        E::UnknownRegexFlag(c),
                        self.pos..self.pos + 1,
                    ));
                }
            }
            self.pos += 1;
        }
        Ok(TokenKind::Regex { pattern, flags })
    }

    fn lines(&mut self) -> Result<TokenKind, ParseError> {
        let start = self.line_no()?;
        if self.peek() != Some('-') {
            return Ok(TokenKind::Lines { start, end: None });
        }
        self.pos += 1;
        if !matches!(self.peek(), Some('$' | '0'..='9')) {
            return Err(ParseError::new(E::MissingRangeEnd, self.pos - 1..self.pos));
        }
        let end_start = self.pos;
        let end = self.line_no()?;
        if let (LineNo::Number(a), LineNo::Number(b)) = (start, end)
            && a > b
        {
            let span = end_start..self.pos;
            return Err(ParseError::new(E::ReversedLines { start: a, end: b }, span));
        }
        Ok(TokenKind::Lines {
            start,
            end: Some(end),
        })
    }

    fn context(&mut self) -> Result<usize, ParseError> {
        let start = self.pos;
        self.pos += 1;
        let digits = self.take_while(|c| c.is_ascii_digit());
        match digits {
            "" => Err(ParseError::new(E::MissingContext, start..self.pos)),
            _ => digits
                .parse()
                .map_err(|_| ParseError::new(E::LineOverflow, start..self.pos)),
        }
    }

    fn line_no(&mut self) -> Result<LineNo, ParseError> {
        let start = self.pos;
        if self.peek() == Some('$') {
            self.pos += 1;
            return Ok(LineNo::Last);
        }
        let digits = self.take_while(|c| c.is_ascii_digit());
        match digits.parse::<usize>() {
            Ok(0) => Err(ParseError::new(E::ZeroLine, start..self.pos)),
            Ok(n) => Ok(LineNo::Number(n)),
            Err(_) => Err(ParseError::new(E::LineOverflow, start..self.pos)),
        }
    }

    fn part(&mut self) -> Result<Part, ParseError> {
        let start = self.pos;
        self.pos += 1;
        let name = self.take_while(is_ident_char);
        Ok(match name {
            "" => return Err(ParseError::new(E::UnexpectedChar('.'), start..start + 1)),
            "body" => Part::Body,
            "sig" => Part::Sig,
            "params" => Part::Params,
            "name" => Part::Name,
            "doc" => Part::Doc,
            "attrs" => Part::Attrs,
            "ret" => Part::Ret,
            "type" => Part::Type,
            "value" => Part::Value,
            "whole" => Part::Whole,
            "lines" => Part::Lines,
            "refs" => Part::Refs,
            "def" => Part::Def,
            _ => {
                return Err(ParseError::new(
                    E::UnknownPart(name.into()),
                    start..self.pos,
                ));
            }
        })
    }

    fn word(&mut self) -> Result<TokenKind, ParseError> {
        let start = self.pos;
        let word = self.take_while(is_ident_char);
        match self.peek() {
            Some(':') => {
                self.pos += 1;
                let name = match self.peek() {
                    Some('"') => self.string()?,
                    _ if word == "file" => self
                        .take_while(|c| !matches!(c, ' ' | '\t' | '\r' | '\n' | '>' | ';' | '|'))
                        .to_string(),
                    _ => self
                        .take_while(|c| is_ident_char(c) || c == ':' || c == '*')
                        .to_string(),
                };
                if name.is_empty() {
                    return Err(ParseError::new(
                        E::MissingName(word.into()),
                        start..self.pos,
                    ));
                }
                Ok(TokenKind::Syntax {
                    kind: word.into(),
                    name,
                })
            }
            Some('{') if word == "query" => self.query(start),
            _ => Ok(TokenKind::Word(word.into())),
        }
    }

    fn query(&mut self, start: usize) -> Result<TokenKind, ParseError> {
        self.pos += 1;
        let mut query = String::new();
        loop {
            let Some(c) = self.peek().filter(|&c| c != '\n') else {
                return Err(ParseError::new(E::UnterminatedQuery, start..self.pos));
            };
            self.pos += c.len_utf8();
            match c {
                '}' => return Ok(TokenKind::Query(query)),
                '\\' if self.peek() == Some('}') => {
                    query.push('}');
                    self.pos += 1;
                }
                c => query.push(c),
            }
        }
    }

    /// Lexes a pattern from its opening backquote: it may span lines, and
    /// `` \` `` is its only escape.
    fn code(&mut self, start: usize) -> Result<TokenKind, ParseError> {
        self.pos += 1;
        let mut code = String::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(ParseError::new(E::UnterminatedPattern, start..self.pos));
            };
            self.pos += c.len_utf8();
            match c {
                '`' => return Ok(TokenKind::Code(code)),
                '\\' if self.peek() == Some('`') => {
                    code.push('`');
                    self.pos += 1;
                }
                c => code.push(c),
            }
        }
    }

    /// Lexes `<<TAG` and reads its body from the lines after the command line
    /// (or after the previous heredoc body on this line).
    fn heredoc(&mut self) -> Result<TokenKind, ParseError> {
        let start = self.pos;
        if self.peek_second() != Some('<') {
            return Err(ParseError::new(E::UnexpectedChar('<'), start..start + 1));
        }
        self.pos += 2;
        let raw = self.peek() == Some('\'');
        if raw {
            self.pos += 1;
        }
        let tag = self.take_while(is_ident_char);
        if tag.is_empty() || (raw && self.peek() != Some('\'')) {
            return Err(ParseError::new(E::MissingHeredocTag, start..self.pos));
        }
        if raw {
            self.pos += 1;
        }

        let src = self.src;
        let body_start = self.heredoc_cursor.unwrap_or_else(|| {
            src[self.pos..]
                .find('\n')
                .map_or(src.len(), |i| self.pos + i + 1)
        });
        let mut body = Vec::new();
        let mut line_start = body_start;
        while line_start < src.len() {
            let (line_end, next) = match src[line_start..].find('\n') {
                Some(i) => (line_start + i, line_start + i + 1),
                None => (src.len(), src.len()),
            };
            let line = &src[line_start..line_end];
            let line = line.strip_suffix('\r').unwrap_or(line);
            if line.trim() == tag {
                self.heredoc_cursor = Some(next);
                return Ok(TokenKind::Heredoc {
                    tag: tag.into(),
                    body: body.join("\n"),
                    raw,
                });
            }
            body.push(line);
            line_start = next;
        }
        Err(ParseError::new(
            E::UnterminatedHeredoc(tag.into()),
            start..self.pos,
        ))
    }
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::ParseErrorKind as E;
    use TokenKind as T;

    fn tokens(src: &str) -> Vec<Token> {
        let mut lexer = Lexer::new(src);
        let mut out = Vec::new();
        loop {
            let token = lexer
                .next_token()
                .unwrap_or_else(|e| panic!("{src:?}: {e}"));
            if token.kind == T::Eof {
                return out;
            }
            out.push(token);
        }
    }

    fn kinds(src: &str) -> Vec<TokenKind> {
        tokens(src).into_iter().map(|t| t.kind).collect()
    }

    fn error(src: &str) -> ParseError {
        let mut lexer = Lexer::new(src);
        loop {
            match lexer.next_token() {
                Ok(t) if t.kind == T::Eof => panic!("{src:?} lexed without error"),
                Ok(_) => {}
                Err(e) => return e,
            }
        }
    }

    fn word(s: &str) -> TokenKind {
        T::Word(s.into())
    }

    fn syntax(kind: &str, name: &str) -> TokenKind {
        T::Syntax {
            kind: kind.into(),
            name: name.into(),
        }
    }

    fn lines(start: LineNo, end: Option<LineNo>) -> TokenKind {
        T::Lines { start, end }
    }

    fn heredoc(tag: &str, body: &str, raw: bool) -> TokenKind {
        T::Heredoc {
            tag: tag.into(),
            body: body.into(),
            raw,
        }
    }

    fn regex(pattern: &str, case_insensitive: bool, dot_all: bool) -> TokenKind {
        T::Regex {
            pattern: pattern.into(),
            flags: RegexFlags {
                case_insensitive,
                dot_all,
            },
        }
    }

    use LineNo::{Last, Number as N};

    #[test]
    fn empty_script_is_eof() {
        let mut lexer = Lexer::new("");
        let eof = lexer.next_token().unwrap();
        assert_eq!(eof.kind, T::Eof);
        assert_eq!(eof.span, 0..0);
        assert_eq!(lexer.next_token().unwrap().kind, T::Eof);
    }

    #[test]
    fn words_and_separators() {
        assert_eq!(
            kinds("show; delete\n"),
            [word("show"), T::Semicolon, word("delete"), T::Newline]
        );
    }

    #[test]
    fn spans_and_space_before() {
        let got: Vec<_> = tokens(r#"replace fn:parse>"x" with "y""#)
            .into_iter()
            .map(|t| (t.span, t.space_before))
            .collect();
        assert_eq!(
            got,
            [
                (0..7, true),
                (8..16, true),
                (16..17, false),
                (17..20, false),
                (21..25, true),
                (26..29, true),
            ]
        );
    }

    #[test]
    fn syntax_selectors() {
        assert_eq!(kinds("import:std::fmt"), [syntax("import", "std::fmt")]);
        assert_eq!(kinds("fn:test_*"), [syntax("fn", "test_*")]);
        assert_eq!(kinds(r#"import:"os.path""#), [syntax("import", "os.path")]);
        assert_eq!(
            kinds("impl:Parser>fn:new.body"),
            [
                syntax("impl", "Parser"),
                T::Gt,
                syntax("fn", "new"),
                T::Part(Part::Body)
            ]
        );
    }

    #[test]
    fn file_selector_names_are_paths() {
        assert_eq!(
            kinds("file:src/a.rs>fn:new"),
            [syntax("file", "src/a.rs"), T::Gt, syntax("fn", "new")]
        );
        assert_eq!(
            kinds(r#"file:"my dir/a.rs""#),
            [syntax("file", "my dir/a.rs")]
        );
    }

    #[test]
    fn missing_syntax_name() {
        let e = error("delete fn: x");
        assert_eq!(e.kind, E::MissingName("fn".into()));
        assert_eq!(e.span.start, 7);
    }

    #[test]
    fn line_selectors() {
        assert_eq!(kinds("12"), [lines(N(12), None)]);
        assert_eq!(kinds("12-20"), [lines(N(12), Some(N(20)))]);
        assert_eq!(kinds("12-$"), [lines(N(12), Some(Last))]);
        assert_eq!(kinds("$"), [lines(Last, None)]);
        assert_eq!(
            kinds("100-200>fn:new"),
            [lines(N(100), Some(N(200))), T::Gt, syntax("fn", "new")]
        );
    }

    #[test]
    fn bad_line_selectors() {
        assert_eq!(error("0").kind, E::ZeroLine);
        assert_eq!(error("3-0").kind, E::ZeroLine);
        assert_eq!(error("20-12").kind, E::ReversedLines { start: 20, end: 12 });
        assert_eq!(error("12- ").kind, E::MissingRangeEnd);
        assert_eq!(error("99999999999999999999999").kind, E::LineOverflow);
    }

    #[test]
    fn regexes() {
        assert_eq!(kinds(r"/a\/b\d/is"), [regex(r"a/b\d", true, true)]);
        assert_eq!(kinds("/x/i"), [regex("x", true, false)]);
        assert_eq!(
            kinds("/dbg!/.lines"),
            [regex("dbg!", false, false), T::Part(Part::Lines)]
        );
        assert_eq!(
            kinds(r"fn:main>/unwrap\(\)/"),
            [
                syntax("fn", "main"),
                T::Gt,
                regex(r"unwrap\(\)", false, false)
            ]
        );
    }

    #[test]
    fn bad_regexes() {
        let e = error("/x/g");
        assert_eq!(e.kind, E::UnknownRegexFlag('g'));
        assert_eq!(e.span, 3..4);
        assert_eq!(error("/abc\n/").kind, E::UnterminatedRegex);
        assert_eq!(error(r"/abc\/").kind, E::UnterminatedRegex);
    }

    #[test]
    fn strings() {
        assert_eq!(kinds(r#""a\n\t\"\\b#""#), [T::Str("a\n\t\"\\b#".into())]);
        assert_eq!(kinds(r#""""#), [T::Str(String::new())]);
    }

    #[test]
    fn bad_strings() {
        let e = error(r#"show "a\qb""#);
        assert_eq!(e.kind, E::InvalidEscape('q'));
        assert_eq!(e.span, 7..9);
        assert_eq!(error(r#""abc"#).kind, E::UnterminatedString);
        assert_eq!(error("\"ab\ncd\"").kind, E::UnterminatedString);
    }

    #[test]
    fn queries() {
        assert_eq!(
            kinds("query{(call_expression) @sel}.lines"),
            [
                T::Query("(call_expression) @sel".into()),
                T::Part(Part::Lines)
            ]
        );
        assert_eq!(kinds(r"query{a\}b}"), [T::Query("a}b".into())]);
        assert_eq!(error("query{(a)\n}").kind, E::UnterminatedQuery);
    }

    #[test]
    fn patterns() {
        assert_eq!(
            kinds("`foo(@a)`>fn:x"),
            [T::Code("foo(@a)".into()), T::Gt, syntax("fn", "x")]
        );
        assert_eq!(kinds("`if c:\n    x`"), [T::Code("if c:\n    x".into())]);
        assert_eq!(kinds(r#"`"\n" \`"#), [T::Code(r#""\n" \"#.into())]);
        assert_eq!(kinds("`a # b`"), [T::Code("a # b".into())]);
        let e = error("show `foo(");
        assert_eq!(e.kind, E::UnterminatedPattern);
        assert_eq!(e.span, 5..10);
    }

    #[test]
    fn patterns_with_backquotes() {
        assert_eq!(kinds("``a`b``"), [T::Code("a`b".into())]);
        assert_eq!(kinds("``a```b``"), [T::Code("a```b".into())]);
        // One space just inside each end is dropped, if both ends have one.
        assert_eq!(kinds("`` `${x}` ``"), [T::Code("`${x}`".into())]);
        assert_eq!(kinds("`` a ``"), [T::Code("a".into())]);
        assert_eq!(kinds("` a`"), [T::Code(" a".into())]);
        assert_eq!(kinds("`  `"), [T::Code("  ".into())]);
        let e = error("show ``foo`");
        assert_eq!(e.kind, E::UnterminatedPattern);
        assert_eq!(e.span, 5..11);
    }

    #[test]
    fn parts() {
        assert_eq!(
            kinds("fn:f.body.sig.params.name.doc.attrs.ret.type.value.lines"),
            [
                syntax("fn", "f"),
                T::Part(Part::Body),
                T::Part(Part::Sig),
                T::Part(Part::Params),
                T::Part(Part::Name),
                T::Part(Part::Doc),
                T::Part(Part::Attrs),
                T::Part(Part::Ret),
                T::Part(Part::Type),
                T::Part(Part::Value),
                T::Part(Part::Lines),
            ]
        );
        let e = error("fn:f.bodyy");
        assert_eq!(e.kind, E::UnknownPart("bodyy".into()));
        assert_eq!(e.span, 4..10);
    }

    #[test]
    fn comments() {
        let got = tokens("show # a \"comment\n# whole line\n  delete 3");
        let got: Vec<_> = got.into_iter().map(|t| (t.kind, t.space_before)).collect();
        assert_eq!(
            got,
            [
                (word("show"), true),
                (T::Newline, true),
                (T::Newline, true),
                (word("delete"), true),
                (lines(N(3), None), true),
            ]
        );
    }

    #[test]
    fn heredoc_body_is_skipped_after_the_command_line() {
        let src = "replace fn:parse.body <<END\n  let x = 1;\n\n  y\n  END  \nshow";
        let got = tokens(src);
        assert_eq!(
            got.iter().map(|t| t.kind.clone()).collect::<Vec<_>>(),
            [
                word("replace"),
                syntax("fn", "parse"),
                T::Part(Part::Body),
                heredoc("END", "  let x = 1;\n\n  y", false),
                T::Newline,
                word("show"),
            ]
        );
        assert_eq!(got[3].span, 22..27);
        assert_eq!(got[4].span, 27..28);
    }

    #[test]
    fn raw_and_empty_heredocs() {
        assert_eq!(
            kinds("<<'EOF'\n  x\nEOF"),
            [heredoc("EOF", "  x", true), T::Newline]
        );
        assert_eq!(kinds("<<E\nE\n"), [heredoc("E", "", false), T::Newline]);
    }

    #[test]
    fn terminator_must_be_the_whole_trimmed_line() {
        assert_eq!(
            kinds("<<END\nEND;\nENDS\n END\n"),
            [heredoc("END", "END;\nENDS", false), T::Newline]
        );
    }

    #[test]
    fn several_heredocs_on_one_line_read_in_order() {
        assert_eq!(
            kinds("replace <<A with <<B\nold\nA\nnew\nB\ndelete 1"),
            [
                word("replace"),
                heredoc("A", "old", false),
                word("with"),
                heredoc("B", "new", false),
                T::Newline,
                word("delete"),
                lines(N(1), None),
            ]
        );
        assert_eq!(
            kinds("insert after 1 <<A; insert after 2 <<B\na\nA\nb\nB\n"),
            [
                word("insert"),
                word("after"),
                lines(N(1), None),
                heredoc("A", "a", false),
                T::Semicolon,
                word("insert"),
                word("after"),
                lines(N(2), None),
                heredoc("B", "b", false),
                T::Newline,
            ]
        );
    }

    #[test]
    fn crlf_scripts() {
        assert_eq!(
            kinds("replace 1 <<E\r\n  a\r\nE\r\nshow\r\n"),
            [
                word("replace"),
                lines(N(1), None),
                heredoc("E", "  a", false),
                T::Newline,
                word("show"),
                T::Newline,
            ]
        );
    }

    #[test]
    fn bad_heredocs() {
        assert_eq!(error("replace 1 << \n").kind, E::MissingHeredocTag);
        assert_eq!(error("replace 1 <<'A\nx\nA\n").kind, E::MissingHeredocTag);
        let e = error("show\nreplace fn:parse.body <<END\nfoo\n");
        assert_eq!(e.kind, E::UnterminatedHeredoc("END".into()));
        assert_eq!(e.span.start, 27);
        assert_eq!(
            error("replace 1 <<END").kind,
            E::UnterminatedHeredoc("END".into())
        );
    }

    #[test]
    fn unexpected_characters() {
        assert_eq!(error("show @").kind, E::UnexpectedChar('@'));
        assert_eq!(error("show < 3").kind, E::UnexpectedChar('<'));
        assert_eq!(error("show é").kind, E::UnexpectedChar('é'));
        assert_eq!(error("show .").kind, E::UnexpectedChar('.'));
    }

    #[test]
    fn file_paths() {
        let mut lexer = Lexer::new(r#"file src/*.rs "my file.rs" b#1.rs; show"#);
        assert_eq!(lexer.next_token().unwrap().kind, word("file"));
        let path = lexer.path().unwrap().unwrap();
        assert_eq!((path.kind, path.span), (T::Path("src/*.rs".into()), 5..13));
        assert_eq!(
            lexer.path().unwrap().unwrap().kind,
            T::Path("my file.rs".into())
        );
        assert_eq!(
            lexer.path().unwrap().unwrap().kind,
            T::Path("b#1.rs".into())
        );
        assert_eq!(lexer.path().unwrap(), None);
        assert_eq!(lexer.next_token().unwrap().kind, T::Semicolon);
        assert_eq!(lexer.next_token().unwrap().kind, word("show"));
    }

    #[test]
    fn file_paths_end_at_comments_and_newlines() {
        let mut lexer = Lexer::new("file a.rs # c\nshow");
        lexer.next_token().unwrap();
        assert_eq!(lexer.path().unwrap().unwrap().kind, T::Path("a.rs".into()));
        assert_eq!(lexer.path().unwrap(), None);
        assert_eq!(lexer.next_token().unwrap().kind, T::Newline);
        assert_eq!(lexer.next_token().unwrap().kind, word("show"));
    }
}
