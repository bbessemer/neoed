//! Tokenizer for scripts.
//!
//! The lexer is pulled by the parser, which calls [`Lexer::path`] instead of
//! [`Lexer::next_token`] where the grammar expects `file` paths. Heredoc
//! bodies are read as soon as their `<<TAG` is lexed; the lexer then skips
//! over them when it reaches the end of the command line.

use std::ops::Range;

use super::ast::{LineNo, Part, RegexFlags};
use super::error::ParseError;

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
    Part(Part),
    /// A `file` command argument, from [`Lexer::path`].
    Path(String),
    Gt,
    Semicolon,
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
        todo!()
    }

    pub fn next_token(&mut self) -> Result<Token, ParseError> {
        todo!()
    }

    /// Lexes one `file` argument: a quoted string, or a run of characters up
    /// to whitespace or `;`. Returns `None`, consuming nothing, at the end of
    /// the command.
    pub fn path(&mut self) -> Result<Option<Token>, ParseError> {
        todo!()
    }
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
    fn parts() {
        assert_eq!(
            kinds("fn:f.body.sig.params.name.doc.lines"),
            [
                syntax("fn", "f"),
                T::Part(Part::Body),
                T::Part(Part::Sig),
                T::Part(Part::Params),
                T::Part(Part::Name),
                T::Part(Part::Doc),
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
        assert_eq!(kinds("<<'EOF'\n  x\nEOF"), [heredoc("EOF", "  x", true)]);
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
