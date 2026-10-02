//! The reader: source text to [`Syntax`].

use crate::{Datum, ReadError, ReadErrorKind, Syntax};

/// Reads every datum in `src`.
pub fn read_all(src: &str) -> Result<Vec<Syntax>, ReadError> {
    let mut reader = Reader { src, pos: 0 };
    let mut out = Vec::new();
    loop {
        match reader.next()? {
            Item::Datum(s) => out.push(s),
            Item::Close(c, at) => {
                return Err(error(ReadErrorKind::UnexpectedCloser(c), at..at + 1));
            }
            Item::End => return Ok(out),
        }
    }
}

enum Item {
    Datum(Syntax),
    /// A closing `)` or `]`, at its offset.
    Close(char, usize),
    End,
}

struct Reader<'a> {
    src: &'a str,
    pos: usize,
}

fn error(kind: ReadErrorKind, span: std::ops::Range<usize>) -> ReadError {
    ReadError { kind, span }
}

fn is_delimiter(c: char) -> bool {
    c.is_whitespace() || "()[]\";'`,".contains(c)
}

impl<'a> Reader<'a> {
    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn next(&mut self) -> Result<Item, ReadError> {
        self.skip_atmosphere()?;
        let start = self.pos;
        let rest = self.rest();
        let Some(c) = rest.chars().next() else {
            return Ok(Item::End);
        };
        let datum = |datum, end| {
            Ok(Item::Datum(Syntax {
                datum,
                span: start..end,
            }))
        };
        match c {
            '(' | '[' => {
                self.pos += 1;
                self.sequence(c, start)
            }
            ')' | ']' => {
                self.pos += 1;
                Ok(Item::Close(c, start))
            }
            '"' => {
                let s = self.string()?;
                datum(Datum::Str(s), self.pos)
            }
            '\'' | '`' | ',' => {
                let (prefix, name) = match (c, rest.starts_with(",@")) {
                    ('\'', _) => ("'", "quote"),
                    ('`', _) => ("`", "quasiquote"),
                    (_, true) => (",@", "unquote-splicing"),
                    _ => (",", "unquote"),
                };
                self.pos += prefix.len();
                let prefix_span = start..self.pos;
                let Item::Datum(quoted) = self.next()? else {
                    return Err(error(ReadErrorKind::MissingDatum(prefix), prefix_span));
                };
                let end = quoted.span.end;
                let head = Syntax {
                    datum: Datum::Symbol(name.into()),
                    span: prefix_span,
                };
                datum(Datum::List(vec![head, quoted]), end)
            }
            '#' if rest.starts_with("#;") => {
                self.pos += 2;
                match self.next()? {
                    Item::Datum(_) => self.next(),
                    _ => Err(error(ReadErrorKind::MissingDatum("#;"), start..start + 2)),
                }
            }
            '#' if rest.starts_with("#\\") || rest.starts_with("#(") => Err(error(
                ReadErrorKind::Unsupported(rest[..2].into()),
                start..start + 2,
            )),
            '@' => {
                self.pos += 1;
                let name = self.token();
                if name.is_empty() {
                    return Err(error(ReadErrorKind::EmptyCapture, start..start + 1));
                }
                datum(Datum::Capture(name.into()), self.pos)
            }
            _ => {
                let token = self.token();
                let d = atom(token, self.rest()).map_err(|kind| {
                    let len = match &kind {
                        ReadErrorKind::Unsupported(form) => form.len(),
                        _ => token.len(),
                    };
                    error(kind, start..start + len)
                })?;
                datum(d, self.pos)
            }
        }
    }

    /// The items of a list or alternation whose opener `open` is at `start`.
    fn sequence(&mut self, open: char, start: usize) -> Result<Item, ReadError> {
        let mut items = Vec::new();
        loop {
            match self.next()? {
                Item::Datum(s) => items.push(s),
                Item::Close(close, at) if close == crate::closer(open) => {
                    let datum = if open == '(' {
                        Datum::List(items)
                    } else {
                        Datum::Alternation(items)
                    };
                    return Ok(Item::Datum(Syntax {
                        datum,
                        span: start..at + 1,
                    }));
                }
                Item::Close(close, at) => {
                    return Err(error(
                        ReadErrorKind::MismatchedCloser { open, close },
                        at..at + 1,
                    ));
                }
                Item::End => {
                    return Err(error(ReadErrorKind::Unterminated(open), start..start + 1));
                }
            }
        }
    }

    /// Skips whitespace, `;` comments and nested `#|...|#` comments.
    fn skip_atmosphere(&mut self) -> Result<(), ReadError> {
        loop {
            let rest = self.rest();
            let trimmed = rest.trim_start();
            self.pos += rest.len() - trimmed.len();
            if trimmed.starts_with(';') {
                self.pos += trimmed.find('\n').unwrap_or(trimmed.len());
            } else if trimmed.starts_with("#|") {
                self.block_comment()?;
            } else {
                return Ok(());
            }
        }
    }

    fn block_comment(&mut self) -> Result<(), ReadError> {
        let start = self.pos;
        let mut depth = 0;
        while self.pos < self.src.len() {
            let rest = self.rest();
            if rest.starts_with("#|") {
                depth += 1;
                self.pos += 2;
            } else if rest.starts_with("|#") {
                depth -= 1;
                self.pos += 2;
                if depth == 0 {
                    return Ok(());
                }
            } else {
                self.pos += rest.chars().next().map_or(1, char::len_utf8);
            }
        }
        Err(error(ReadErrorKind::UnterminatedComment, start..start + 2))
    }

    /// The string starting at the current `"`, with tree-sitter's query escapes:
    /// `\n`, `\r`, `\t` and `\0`, and any other escaped character for itself.
    fn string(&mut self) -> Result<String, ReadError> {
        let start = self.pos;
        let mut out = String::new();
        let mut chars = self.src[start + 1..].char_indices();
        while let Some((i, c)) = chars.next() {
            match c {
                '"' => {
                    self.pos = start + 1 + i + 1;
                    return Ok(out);
                }
                '\\' => match chars.next() {
                    Some((_, e)) => out.push(match e {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        '0' => '\0',
                        e => e,
                    }),
                    None => break,
                },
                c => out.push(c),
            }
        }
        Err(error(ReadErrorKind::UnterminatedString, start..start + 1))
    }

    /// The run of non-delimiter characters at the current position.
    fn token(&mut self) -> &'a str {
        let rest = self.rest();
        let len = rest.find(is_delimiter).unwrap_or(rest.len());
        self.pos += len;
        &rest[..len]
    }
}

/// The datum a token stands for; `after` is the source after it.
fn atom(token: &str, after: &str) -> Result<Datum, ReadErrorKind> {
    Ok(match token {
        "." => Datum::Anchor,
        "#" => return Err(ReadErrorKind::Unsupported("#".into())),
        "#u8" if after.starts_with('(') => return Err(ReadErrorKind::Unsupported("#u8(".into())),
        "#t" | "#true" => Datum::Bool(true),
        "#f" | "#false" => Datum::Bool(false),
        _ if is_number(token) => match token.parse() {
            Ok(n) => Datum::Int(n),
            Err(_) if token.contains('.') => Datum::Real(token.parse().expect("digits.digits")),
            Err(_) => return Err(ReadErrorKind::IntOutOfRange(token.into())),
        },
        _ => match token.strip_suffix(':') {
            Some(name) if !name.is_empty() => Datum::Keyword(name.into()),
            _ => Datum::Symbol(token.into()),
        },
    })
}

/// Whether `token` is `[+-]?digits(.digits)?`.
fn is_number(token: &str) -> bool {
    let digits = token.strip_prefix(['+', '-']).unwrap_or(token);
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    match digits.split_once('.') {
        Some((int, frac)) => all_digits(int) && all_digits(frac),
        None => all_digits(digits),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Datum, ReadErrorKind};

    /// The datums read from `src`, with every span zeroed.
    fn read(src: &str) -> Vec<Datum> {
        read_all(src).unwrap().into_iter().map(strip).collect()
    }

    fn strip(s: Syntax) -> Datum {
        let items = |v: Vec<Syntax>| v.into_iter().map(|s| zero(strip(s))).collect();
        match s.datum {
            Datum::List(v) => Datum::List(items(v)),
            Datum::Alternation(v) => Datum::Alternation(items(v)),
            d => d,
        }
    }

    fn zero(datum: Datum) -> Syntax {
        Syntax { datum, span: 0..0 }
    }

    fn list(items: Vec<Datum>) -> Datum {
        Datum::List(items.into_iter().map(zero).collect())
    }

    fn sym(s: &str) -> Datum {
        Datum::Symbol(s.into())
    }

    fn string(s: &str) -> Datum {
        Datum::Str(s.into())
    }

    fn err(src: &str) -> ReadError {
        read_all(src).unwrap_err()
    }

    #[test]
    fn atoms() {
        assert_eq!(
            read("#t #f #true #false 42 -7 +3 1.5 -0.25 \"hi\""),
            vec![
                Datum::Bool(true),
                Datum::Bool(false),
                Datum::Bool(true),
                Datum::Bool(false),
                Datum::Int(42),
                Datum::Int(-7),
                Datum::Int(3),
                Datum::Real(1.5),
                Datum::Real(-0.25),
                string("hi"),
            ]
        );
    }

    #[test]
    fn symbols() {
        let names = "foo _ * + - ? : !name #eq? #not-match? #set! a.b .x 1+ 1. kebab-case";
        let expected: Vec<_> = names.split(' ').map(sym).collect();
        assert_eq!(read(names), expected);
    }

    #[test]
    fn spans() {
        let read = read_all("  foo (a \"b\")\n@c").unwrap();
        assert_eq!(read[0].span, 2..5);
        assert_eq!(read[1].span, 6..13);
        let Datum::List(items) = &read[1].datum else {
            panic!("not a list: {:?}", read[1]);
        };
        assert_eq!(items[0].span, 7..8);
        assert_eq!(items[1].span, 9..12);
        assert_eq!(read[2].span, 14..16);
    }

    #[test]
    fn keywords() {
        let expected = vec![Datum::Keyword("name".into()), list(vec![sym("x")])];
        assert_eq!(read("name: (x)"), expected);
        assert_eq!(read("name:(x)"), expected);
    }

    #[test]
    fn captures() {
        assert_eq!(
            read("@name @function.method @_x (x)? @p"),
            vec![
                Datum::Capture("name".into()),
                Datum::Capture("function.method".into()),
                Datum::Capture("_x".into()),
                list(vec![sym("x")]),
                sym("?"),
                Datum::Capture("p".into()),
            ]
        );
    }

    #[test]
    fn anchors() {
        assert_eq!(
            read("(a . (b) .)"),
            vec![list(vec![
                sym("a"),
                Datum::Anchor,
                list(vec![sym("b")]),
                Datum::Anchor,
            ])]
        );
        assert_eq!(
            read("(.(b))"),
            vec![list(vec![Datum::Anchor, list(vec![sym("b")])])]
        );
    }

    #[test]
    fn alternations() {
        assert_eq!(
            read("[ (a) \"b\" ]"),
            vec![Datum::Alternation(vec![
                zero(list(vec![sym("a")])),
                zero(string("b"))
            ])]
        );
    }

    #[test]
    fn string_escapes() {
        assert_eq!(
            read(r#""a\nb\tc\rd\0e\\f\"g\.h""#),
            vec![string("a\nb\tc\rd\0e\\f\"g.h")]
        );
        assert_eq!(read(r#""^/\\*\\*""#), vec![string(r"^/\*\*")]);
        assert_eq!(read("\"two\nlines\""), vec![string("two\nlines")]);
    }

    #[test]
    fn comments() {
        assert_eq!(read("a ; c (\n b"), vec![sym("a"), sym("b")]);
        assert_eq!(read("a #| x #| y |# ) |# b"), vec![sym("a"), sym("b")]);
        assert_eq!(read("a #; (skip [me]) b"), vec![sym("a"), sym("b")]);
        assert_eq!(read("(a #;b)"), vec![list(vec![sym("a")])]);
    }

    #[test]
    fn quote_forms() {
        let quoted = |name: &str, d: Datum| list(vec![sym(name), d]);
        assert_eq!(
            read("'x `(a ,b ,@c) , @d"),
            vec![
                quoted("quote", sym("x")),
                quoted(
                    "quasiquote",
                    list(vec![
                        sym("a"),
                        quoted("unquote", sym("b")),
                        quoted("unquote-splicing", sym("c")),
                    ])
                ),
                quoted("unquote", Datum::Capture("d".into())),
            ]
        );
        let read = read_all("  'x ,@ (y)").unwrap();
        assert_eq!(read[0].span, 2..4);
        assert_eq!(read[1].span, 5..11);
    }

    #[test]
    fn errors() {
        let cases: &[(&str, ReadErrorKind, std::ops::Range<usize>)] = &[
            ("x \"abc", ReadErrorKind::UnterminatedString, 2..3),
            ("x (a b", ReadErrorKind::Unterminated('('), 2..3),
            ("[a", ReadErrorKind::Unterminated('['), 0..1),
            ("#| x #| y |#", ReadErrorKind::UnterminatedComment, 0..2),
            ("a )", ReadErrorKind::UnexpectedCloser(')'), 2..3),
            ("]", ReadErrorKind::UnexpectedCloser(']'), 0..1),
            (
                "(a]",
                ReadErrorKind::MismatchedCloser {
                    open: '(',
                    close: ']',
                },
                2..3,
            ),
            ("'", ReadErrorKind::MissingDatum("'"), 0..1),
            ("(a ')", ReadErrorKind::MissingDatum("'"), 3..4),
            ("a ,@", ReadErrorKind::MissingDatum(",@"), 2..4),
            ("#;", ReadErrorKind::MissingDatum("#;"), 0..2),
            ("@ x", ReadErrorKind::EmptyCapture, 0..1),
            ("#\\a", ReadErrorKind::Unsupported("#\\".into()), 0..2),
            ("#(1)", ReadErrorKind::Unsupported("#(".into()), 0..2),
            ("#u8(1)", ReadErrorKind::Unsupported("#u8(".into()), 0..4),
            ("# x", ReadErrorKind::Unsupported("#".into()), 0..1),
            (
                "99999999999999999999",
                ReadErrorKind::IntOutOfRange("99999999999999999999".into()),
                0..20,
            ),
        ];
        for (src, kind, span) in cases {
            assert_eq!(
                err(src),
                ReadError {
                    kind: kind.clone(),
                    span: span.clone()
                },
                "{src}"
            );
        }
    }

    #[test]
    fn errors_end_with_a_fix() {
        assert_eq!(
            err("(a]").to_string(),
            "`(` closed by `]`; close it with `)`"
        );
        assert_eq!(err("[a").to_string(), "unterminated `[`; close it with `]`");
    }

    #[test]
    fn display_round_trips() {
        let src = r#"(a "b\n\"c\"\\" [x @y.z] name: 1 -2.5 1.0 #t #f . 'q `(,r ,@s))"#;
        let written: Vec<_> = read_all(src)
            .unwrap()
            .iter()
            .map(|s| s.datum.to_string())
            .collect();
        assert_eq!(read(&written.join(" ")), read(src));
    }

    #[test]
    fn queries_read() {
        let queries = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../queries");
        let mut files = 0;
        for dir in std::fs::read_dir(&queries).unwrap() {
            for file in std::fs::read_dir(dir.unwrap().path()).unwrap() {
                let path = file.unwrap().path();
                if path.extension().is_some_and(|e| e == "scm") {
                    let src = std::fs::read_to_string(&path).unwrap();
                    if let Err(e) = read_all(&src) {
                        panic!("{}: {e} at {:?}", path.display(), e.span);
                    }
                    files += 1;
                }
            }
        }
        assert!(files >= 7, "read only {files} query files");
    }

    #[test]
    fn query_forms() {
        assert_eq!(
            read(r#"(f !name type: (_) @name (#set! name "{a}.{b}"))"#),
            vec![list(vec![
                sym("f"),
                sym("!name"),
                Datum::Keyword("type".into()),
                list(vec![sym("_")]),
                Datum::Capture("name".into()),
                list(vec![sym("#set!"), sym("name"), string("{a}.{b}")]),
            ])]
        );
    }
}
