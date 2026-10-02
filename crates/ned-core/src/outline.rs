//! The `outline` symbol tree (command-language spec, §6.2).

use std::ops::Range;

use crate::select::{SourceFile, line_numbers};
use crate::style::{Role, Style};
use crate::syntax::{self, Item, Kind, rank};

/// The outline entries of the items in `f`, one per line, or of the items
/// strictly inside `within`. Empty if `f` has no items.
pub fn render(f: &SourceFile, within: Option<&Range<usize>>, style: Style) -> String {
    let Some(items) = f.items() else {
        return String::new();
    };
    let listed = items.iter().filter(|i| {
        within.is_none_or(|w| w.start <= i.range.start && i.range.end <= w.end && i.range != *w)
            // One entry per node, preferring the earliest kind (`fn`).
            && !items.iter().any(|o| o.node == i.node && rank(o.kind) < rank(i.kind))
    });
    let mut out = String::new();
    let mut stack: Vec<&Item> = Vec::new();
    // Consecutive grouped items at one depth: (depth, span, count).

    let mut imports: Option<(usize, Range<usize>, usize)> = None;
    let flush = |out: &mut String, imports: &mut Option<(usize, Range<usize>, usize)>| {
        if let Some((depth, range, count)) = imports.take() {
            let lines = line_numbers(&f.buffer, &range);
            let (lines, kind) = (
                style.paint(Role::Dim, &lines),
                style.paint(Role::Kind, "import"),
            );
            out.push_str(&format!("{}{lines} {kind} ({count})\n", "  ".repeat(depth)));
        }
    };
    for item in listed {
        while stack
            .last()
            .is_some_and(|p| !(p.range.start <= item.range.start && item.range.end <= p.range.end))
        {
            stack.pop();
        }
        if stack.iter().any(|p| is(p, |k| k.opaque)) {
            continue;
        }
        let depth = stack.len();
        let member = is(item, |k| k.member);
        if member && !(within.is_some() && depth == 0) {
            continue;
        }
        stack.push(item);
        if is(item, |k| k.grouped) {
            match &mut imports {
                Some((d, range, count)) if *d == depth => {
                    range.end = item.range.end;
                    *count += 1;
                }
                _ => {
                    flush(&mut out, &mut imports);
                    imports = Some((depth, item.range.clone(), 1));
                }
            }
            continue;
        }
        flush(&mut out, &mut imports);
        let lines = line_numbers(&f.buffer, &item.range);
        let selector = syntax::selector(item.kind, &item.name);
        // `selector` is the kind, then `:` and the name.
        let name = &selector[item.kind.len()..];
        let (lines, kind) = (
            style.paint(Role::Dim, &lines),
            style.paint(Role::Kind, item.kind),
        );
        out.push_str(&format!("{}{lines} {kind}{name}\n", "  ".repeat(depth)));
    }
    flush(&mut out, &mut imports);
    out
}

/// Whether `item`'s kind has the property `has`.
fn is(item: &Item, has: fn(&Kind) -> bool) -> bool {
    syntax::find_kind(item.kind).is_some_and(has)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::Language;
    use crate::style::shown;

    fn file(path: &str, text: &str) -> SourceFile {
        SourceFile::new(path, text.into(), Language::detect(path, text))
    }

    fn outline(path: &str, text: &str) -> String {
        render(&file(path, text), None, Style::Plain)
    }

    #[test]
    fn nests_items_by_containment() {
        let text = "mod a {\n    mod b {\n        fn c() {}\n    }\n    trait T {\n        fn d(&self);\n    }\n}\nconst E: u8 = 1;\n";
        assert_eq!(
            outline("a.rs", text),
            "1-8 mod:a\n  2-4 mod:b\n    3 fn:c\n  5-7 trait:T\n    6 fn:d\n9 const:E\n"
        );
    }

    #[test]
    fn color_dims_lines_and_paints_kinds() {
        let text = "use a;\nuse b;\n\nimpl S {\n    fn new() {}\n}\n";
        let expected = [
            r"\e[2m1-2\e[0m \e[36mimport\e[0m (2)",
            r"\e[2m4-6\e[0m \e[36mimpl\e[0m:S",
            r"  \e[2m5\e[0m \e[36mfn\e[0m:new",
            "",
        ];
        let f = file("a.rs", text);
        assert_eq!(shown(&render(&f, None, Style::Color)), expected.join("\n"));
    }

    #[test]
    fn collapses_consecutive_imports() {
        let text = "use a;\nuse b::{c, d};\nuse e;\n\nfn f() {}\nuse g;\nmod h {\n    use i;\n}\n";
        assert_eq!(
            outline("a.rs", text),
            "1-3 import (3)\n5 fn:f\n6 import (1)\n7-9 mod:h\n  8 import (1)\n"
        );
    }

    #[test]
    fn omits_items_inside_function_bodies() {
        let text = "fn f() {\n    let x = 1;\n    fn g() {}\n    struct S;\n}\n";
        assert_eq!(outline("a.rs", text), "1-5 fn:f\n");
    }

    #[test]
    fn lists_fields_and_variants_only_inside_their_parent() {
        let text = "/// Doc.\nstruct S {\n    a: u8,\n}\nenum E {\n    A,\n    B(u8),\n}\n";
        assert_eq!(outline("a.rs", text), "1-4 struct:S\n5-8 enum:E\n");
        let f = SourceFile::new("a.rs", text.into(), Some(Language::Rust));
        let s = f
            .items()
            .unwrap()
            .iter()
            .find(|i| i.kind == "struct")
            .unwrap();
        assert_eq!(render(&f, Some(&s.range), Style::Plain), "3 field:a\n");
        let e = f
            .items()
            .unwrap()
            .iter()
            .find(|i| i.kind == "enum")
            .unwrap();
        assert_eq!(
            render(&f, Some(&e.range), Style::Plain),
            "6 variant:A\n7 variant:B\n"
        );
    }

    #[test]
    fn inside_a_span_starts_at_the_margin() {
        let text = "impl A {\n    fn b() {}\n    fn c() {}\n}\n";
        let f = SourceFile::new("a.rs", text.into(), Some(Language::Rust));
        assert_eq!(
            render(&f, Some(&(0..text.len())), Style::Plain),
            "1-4 impl:A\n  2 fn:b\n  3 fn:c\n"
        );
        assert_eq!(
            render(&f, Some(&(9..text.len())), Style::Plain),
            "2 fn:b\n3 fn:c\n"
        );
    }

    #[test]
    fn names_trait_impls_by_trait_and_self_type() {
        let text = "impl<T> std::fmt::Display for W<T> {}\n";
        assert_eq!(outline("a.rs", text), "1 impl:\"Display for W\"\n");
    }

    #[test]
    fn outlines_markdown_as_a_tree_of_sections() {
        let text = "# Title\n\n## Two\n\n- a\n- b\n\n| X | Y |\n| - | - |\n\n```rust\nx\n```\n\n### Deep\n\ntext\n";
        let f = file("a.md", text);
        assert_eq!(
            render(&f, None, Style::Plain),
            "1-17 section:Title\n  3-17 section:Two\n    15-17 section:Deep\n"
        );
        let two = f.items().unwrap().iter().find(|i| i.name == "Two").unwrap();
        assert_eq!(
            render(&f, Some(&two.range), Style::Plain),
            "5 item:a\n6 item:b\n8-9 table:X\n11-13 code:rust\n15-17 section:Deep\n"
        );
    }
}
