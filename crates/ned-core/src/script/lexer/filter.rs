//! Filters, `[...]` after a step (command-language spec, §3.9), lexed as one
//! token so that their operators and numbers stay out of the main grammar.

use super::{Lexer, TokenKind, is_ident_char};
use crate::script::ast::{Filter, Op, Part, Property, Value};
use crate::script::error::{ParseError, ParseErrorKind as E};
use crate::script::parser::validate_regex;

const PROPERTY: &str = "a property (.text, .len or a part)";
const OPERATOR: &str = "an operator (== != ~= < > <= >=)";
const VALUE: &str = "a value (a number, \"string\" or /regex/)";

impl Lexer<'_> {
    /// The filter at `[`, through its `]`.
    pub(super) fn filter(&mut self) -> Result<Filter, ParseError> {
        let open = self.pos;
        self.pos += 1;
        let filter = self.or(open)?;
        self.blank();
        if !self.eat("]") {
            return Err(self.unexpected("&&, || or ]", open));
        }
        Ok(filter)
    }

    fn or(&mut self, open: usize) -> Result<Filter, ParseError> {
        let mut any = vec![self.and(open)?];
        while self.eat_op("||") {
            any.push(self.and(open)?);
        }
        Ok(match any.len() {
            1 => any.remove(0),
            _ => Filter::Or(any),
        })
    }

    fn and(&mut self, open: usize) -> Result<Filter, ParseError> {
        let mut all = vec![self.cond(open)?];
        while self.eat_op("&&") {
            all.push(self.cond(open)?);
        }
        Ok(match all.len() {
            1 => all.remove(0),
            _ => Filter::And(all),
        })
    }

    fn cond(&mut self, open: usize) -> Result<Filter, ParseError> {
        if self.eat_op("(") {
            let inner = self.or(open)?;
            if !self.eat_op(")") {
                return Err(self.unexpected("&&, || or )", open));
            }
            return Ok(inner);
        }
        self.blank();
        let start = self.pos;
        let property = self.property(open)?;
        let shown = self.src[start..self.pos].to_string();
        self.blank();
        let at = self.pos;
        let op = self.op().ok_or_else(|| self.unexpected(OPERATOR, open))?;
        let op_span = at..self.pos;
        self.blank();
        let value = self.value(open)?;
        let fits = match (property.len, op, &value) {
            (true, Op::Match, _) => false,
            (true, _, value) => matches!(value, Value::Number(_)),
            (false, Op::Eq | Op::Ne, value) => matches!(value, Value::Str(_)),
            (false, Op::Match, value) => matches!(value, Value::Regex(_)),
            (false, _, _) => false,
        };
        if !fits {
            let kind = match property.len {
                true => E::CompareNumber { property: shown },
                false => E::CompareText { property: shown },
            };
            return Err(ParseError::new(kind, op_span));
        }
        Ok(Filter::Cond {
            property,
            op,
            value,
        })
    }

    /// `.text`, `.len`, or a part and then optionally one of them.
    fn property(&mut self, open: usize) -> Result<Property, ParseError> {
        if self.peek() != Some('.') {
            return Err(self.unexpected(PROPERTY, open));
        }
        if let Some(len) = self.measure() {
            return Ok(Property { part: None, len });
        }
        let start = self.pos;
        let part = self.part()?;
        if matches!(part, Part::Lines | Part::Refs | Part::Def) {
            let name = self.src[start + 1..self.pos].to_string();
            return Err(ParseError::new(E::NotAProperty(name), start..self.pos));
        }
        let len = match self.peek() {
            Some('.') => self.measure().unwrap_or(false),
            _ => false,
        };
        Ok(Property {
            part: Some(part),
            len,
        })
    }

    /// At `.len` or `.text`: consumes it and returns whether it's `.len`.
    fn measure(&mut self) -> Option<bool> {
        let name = self.src[self.pos + 1..]
            .split(|c| !is_ident_char(c))
            .next()?;
        let len = match name {
            "len" => true,
            "text" => false,
            _ => return None,
        };
        self.pos += 1 + name.len();
        Some(len)
    }

    fn op(&mut self) -> Option<Op> {
        // Two-character operators first, so `<=` isn't read as `<`.
        let ops = [
            ("==", Op::Eq),
            ("!=", Op::Ne),
            ("~=", Op::Match),
            ("<=", Op::Le),
            (">=", Op::Ge),
            ("<", Op::Lt),
            (">", Op::Gt),
        ];
        let (text, op) = ops
            .into_iter()
            .find(|(text, _)| self.src[self.pos..].starts_with(text))?;
        self.pos += text.len();
        Some(op)
    }

    fn value(&mut self, open: usize) -> Result<Value, ParseError> {
        let start = self.pos;
        match self.peek() {
            Some('"') => Ok(Value::Str(self.string()?)),
            Some('/') => {
                let TokenKind::Regex { pattern, flags } = self.regex()? else {
                    unreachable!("regex() lexes a regex");
                };
                Ok(Value::Regex(validate_regex(
                    pattern,
                    flags,
                    start..self.pos,
                )?))
            }
            Some(c) if c.is_ascii_digit() => {
                let digits = self.take_while(|c| c.is_ascii_digit());
                match digits.parse() {
                    Ok(n) => Ok(Value::Number(n)),
                    Err(_) => {
                        self.pos = start;
                        Err(self.unexpected(VALUE, open))
                    }
                }
            }
            _ => Err(self.unexpected(VALUE, open)),
        }
    }

    fn blank(&mut self) {
        self.take_while(|c| c == ' ' || c == '\t');
    }

    fn eat(&mut self, text: &str) -> bool {
        let found = self.src[self.pos..].starts_with(text);
        if found {
            self.pos += text.len();
        }
        found
    }

    /// `text` after any blanks.
    fn eat_op(&mut self, text: &str) -> bool {
        self.blank();
        self.eat(text)
    }

    /// The error for what's at the cursor, where the filter at `open` needs
    /// `expected`.
    fn unexpected(&mut self, expected: &'static str, open: usize) -> ParseError {
        self.blank();
        let rest = &self.src[self.pos..];
        let len = match rest.chars().next() {
            None | Some('\n' | '\r') => {
                return ParseError::new(E::UnterminatedFilter, open..self.pos);
            }
            Some(c) if is_ident_char(c) => rest.find(|c| !is_ident_char(c)).unwrap_or(rest.len()),
            Some(c) if "=!~<>&|".contains(c) => {
                rest.find(|c| !"=!~<>&|".contains(c)).unwrap_or(rest.len())
            }
            Some(c) => c.len_utf8(),
        };
        ParseError::new(
            E::InFilter {
                expected,
                found: format!("`{}`", &rest[..len]),
            },
            self.pos..self.pos + len,
        )
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
