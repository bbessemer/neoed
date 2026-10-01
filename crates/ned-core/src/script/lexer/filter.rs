//! Filters, `[...]` after a step (command-language spec, §3.9), lexed as one
//! token.

use super::Lexer;
use crate::script::ast::Filter;
use crate::script::error::ParseError;

impl Lexer<'_> {
    /// The filter at `[`, through its `]`.
    pub(super) fn filter(&mut self) -> Result<Filter, ParseError> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::super::TokenKind;
    use super::*;
    use crate::script::ast::{Op, Part, Pattern, Property, RegexFlags, Value};
    use crate::script::error::ParseErrorKind as E;

    fn filter(src: &str) -> Filter {
        match Lexer::new(src).next_token().unwrap().kind {
            TokenKind::Filter(f) => f,
            kind => panic!("{src:?} lexed as {kind:?}"),
        }
    }

    fn error(src: &str) -> ParseError {
        match Lexer::new(src).next_token() {
            Ok(token) => panic!("{src:?} lexed as {token:?}"),
            Err(e) => e,
        }
    }

    fn cond(part: Option<Part>, len: bool, op: Op, value: Value) -> Filter {
        Filter::Cond {
            property: Property { part, len },
            op,
            value,
        }
    }

    fn len(op: Op, n: usize) -> Filter {
        cond(None, true, op, Value::Number(n))
    }

    fn text(op: Op, s: &str) -> Filter {
        cond(None, false, op, Value::Str(s.into()))
    }

    fn regex(source: &str, flags: RegexFlags) -> Value {
        Value::Regex(Pattern {
            source: source.into(),
            flags,
        })
    }

    #[test]
    fn conditions_compare_a_property_with_a_value() {
        assert_eq!(filter("[.len > 80]"), len(Op::Gt, 80));
        assert_eq!(filter("[.text == \"x\"]"), text(Op::Eq, "x"));
        assert_eq!(
            filter("[.name ~= /^test_/i]"),
            cond(
                Some(Part::Name),
                false,
                Op::Match,
                regex(
                    "^test_",
                    RegexFlags {
                        case_insensitive: true,
                        dot_all: false
                    }
                )
            )
        );
        assert_eq!(
            filter("[.body.len >= 3]"),
            cond(Some(Part::Body), true, Op::Ge, Value::Number(3))
        );
        assert_eq!(
            filter("[.name.text != \"a\"]"),
            cond(Some(Part::Name), false, Op::Ne, Value::Str("a".into()))
        );
        assert_eq!(
            filter("[.doc == \"\"]"),
            cond(Some(Part::Doc), false, Op::Eq, Value::Str(String::new()))
        );
    }

    #[test]
    fn every_number_operator() {
        for (op, src) in [
            (Op::Eq, "=="),
            (Op::Ne, "!="),
            (Op::Lt, "<"),
            (Op::Gt, ">"),
            (Op::Le, "<="),
            (Op::Ge, ">="),
        ] {
            assert_eq!(filter(&format!("[.len {src} 2]")), len(op, 2), "{src}");
            assert_eq!(filter(&format!("[.len{src}2]")), len(op, 2), "{src}");
        }
        assert_eq!(filter("[  .len > 1  ]"), len(Op::Gt, 1));
    }

    #[test]
    fn and_binds_tighter_than_or() {
        assert_eq!(
            filter("[.len > 1 || .len < 5 && .text == \"a\"]"),
            Filter::Or(vec![
                len(Op::Gt, 1),
                Filter::And(vec![len(Op::Lt, 5), text(Op::Eq, "a")]),
            ])
        );
        assert_eq!(
            filter("[(.len > 1 || .len < 5) && .text == \"a\"]"),
            Filter::And(vec![
                Filter::Or(vec![len(Op::Gt, 1), len(Op::Lt, 5)]),
                text(Op::Eq, "a"),
            ])
        );
        assert_eq!(
            filter("[.len > 1 && .len < 5 && .len != 3]"),
            Filter::And(vec![len(Op::Gt, 1), len(Op::Lt, 5), len(Op::Ne, 3)])
        );
    }

    #[test]
    fn a_filter_ends_at_its_bracket() {
        let mut lexer = Lexer::new("[.len > 1].body");
        assert_eq!(
            lexer.next_token().unwrap().kind,
            TokenKind::Filter(len(Op::Gt, 1))
        );
        assert_eq!(
            lexer.next_token().unwrap().kind,
            TokenKind::Part(Part::Body)
        );
    }

    #[test]
    fn errors_suggest_fixes() {
        let e = error("[.len > 80");
        assert_eq!(e.kind, E::UnterminatedFilter);
        assert_eq!(e.span.start, 0);
        assert_eq!(
            error("[.lines == \"\"]").kind,
            E::NotAProperty("lines".into())
        );
        assert_eq!(
            error("[.refs.len > 1]").kind,
            E::NotAProperty("refs".into())
        );
        assert_eq!(error("[.bogus > 1]").kind, E::UnknownPart("bogus".into()));
        let number = E::CompareNumber {
            property: ".len".into(),
        };
        assert_eq!(error("[.len ~= /x/]").kind, number);
        assert_eq!(error("[.len == \"a\"]").kind, number);
        let e = error("[.body.len ~= /x/]");
        assert_eq!(
            e.kind,
            E::CompareNumber {
                property: ".body.len".into()
            }
        );
        assert_eq!(e.span, 11..13);
        let name = E::CompareText {
            property: ".name".into(),
        };
        assert_eq!(error("[.name > 3]").kind, name);
        assert_eq!(error("[.name == /x/]").kind, name);
        assert_eq!(
            error("[.len >]").kind,
            E::InFilter {
                expected: "a value (a number, \"string\" or /regex/)",
                found: "`]`".into()
            }
        );
        assert_eq!(
            error("[len > 1]").kind,
            E::InFilter {
                expected: "a property (.text, .len or a part)",
                found: "`len`".into()
            }
        );
        assert_eq!(
            error("[.len 80]").kind,
            E::InFilter {
                expected: "an operator (== != ~= < > <= >=)",
                found: "`80`".into()
            }
        );
        assert_eq!(
            error("[.len > 1 &&]").kind,
            E::InFilter {
                expected: "a property (.text, .len or a part)",
                found: "`]`".into()
            }
        );
        assert_eq!(
            error("[.len > 1 & .len < 2]").kind,
            E::InFilter {
                expected: "&&, || or ]",
                found: "`&`".into()
            }
        );
        assert_eq!(
            error("[(.len > 1]").kind,
            E::InFilter {
                expected: "&&, || or )",
                found: "`]`".into()
            }
        );
        assert!(matches!(error("[.name ~= /(/]").kind, E::InvalidRegex(_)));
    }
}
