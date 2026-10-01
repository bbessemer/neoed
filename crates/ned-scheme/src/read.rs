//! The reader: source text to [`Syntax`].

use crate::{ReadError, Syntax};

/// Reads every datum in `src`.
pub fn read_all(_src: &str) -> Result<Vec<Syntax>, ReadError> {
    unimplemented!()
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
