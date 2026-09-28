//! Parser from script text to [`Script`] (command-language spec, §2.2).

use super::ast::Script;
use super::error::ParseError;

pub fn parse(src: &str) -> Result<Script, ParseError> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::ParseErrorKind as E;
    use crate::script::ast::*;
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
            span: 0..0,
            ..selector
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
            Show(t) => Show(t.map(unspan_target)),
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
        assert_eq!(one("show"), CommandKind::Show(None));
        assert_eq!(one("outline"), CommandKind::Outline(None));
        assert_eq!(
            one("show 12-20"),
            CommandKind::Show(Some(target(vec![lines(N(12), Some(N(20)))])))
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
            one("replace fn:parse.body <<END\n  let a;\nEND"),
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
    fn nested_selectors_and_parts() {
        let show = |steps| CommandKind::Show(Some(target(steps)));
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

    #[test]
    fn spans() {
        let script = parse("show 1\n  delete all fn:x.body # c").unwrap();
        assert_eq!(script.commands[0].span, 0..6);
        assert_eq!(script.commands[1].span, 9..29);
        let CommandKind::Delete(t) = &script.commands[1].kind else {
            panic!()
        };
        assert_eq!(t.selector.span, 20..29);
        let heredoc = parse("replace 1 <<E\nx\nE").unwrap();
        assert_eq!(heredoc.commands[0].span, 0..13);
    }

    #[test]
    fn spec_examples_parse() {
        let examples = [
            r#"replace fn:parse>"unexpected end" with "unexpected end of input""#,
            "insert end impl:Parser <<END\n\nfn peek(&self) -> Option<char> {\n    self.src[self.pos..].chars().next()\n}\nEND\n",
            "delete fn:debug_dump",
            r#"replace fn:new.params with "src: impl Into<String>""#,
            "replace fn:parse.body <<END\nlet tok = self.next().ok_or(Error::Eof)?;\nself.parse_expr(tok)\nEND\n",
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
        assert_eq!(error("rename fn:x to y").kind, E::Reserved("rename".into()));
        assert_eq!(error("check").kind, E::Reserved("check".into()));
        assert_eq!(error("show refs:foo").kind, E::Reserved("refs:".into()));
        assert_eq!(error("show def:foo").kind, E::Reserved("def:".into()));
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
}
