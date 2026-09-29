//! The `outline` symbol tree (command-language spec, §6.2).

use std::ops::Range;

use crate::select::{SourceFile, line_numbers};
use crate::syntax::{self, Item, KINDS};

/// The outline entries of the items in `f`, one per line, or of the items
/// strictly inside `within`. Empty if `f` has no items.
pub fn render(f: &SourceFile, within: Option<&Range<usize>>) -> String {
    let Some(items) = f.items() else {
        return String::new();
    };
    let rank = |kind: &str| KINDS.iter().position(|k| *k == kind);
    let listed = items.iter().filter(|i| {
        within.is_none_or(|w| w.start <= i.range.start && i.range.end <= w.end && i.range != *w)
            // One entry per node, preferring the earliest kind (`fn`).
            && !items.iter().any(|o| o.node == i.node && rank(o.kind) < rank(i.kind))
    });
    let mut out = String::new();
    let mut stack: Vec<&Item> = Vec::new();
    // Consecutive imports at one depth: (depth, span, count).
    let mut imports: Option<(usize, Range<usize>, usize)> = None;
    let flush = |out: &mut String, imports: &mut Option<(usize, Range<usize>, usize)>| {
        if let Some((depth, range, count)) = imports.take() {
            let lines = line_numbers(&f.buffer, &range);
            out.push_str(&format!("{}{lines} import ({count})\n", "  ".repeat(depth)));
        }
    };
    for item in listed {
        while stack
            .last()
            .is_some_and(|p| !(p.range.start <= item.range.start && item.range.end <= p.range.end))
        {
            stack.pop();
        }
        if stack.iter().any(|p| p.kind == "fn") {
            continue;
        }
        let depth = stack.len();
        let member = matches!(item.kind, "field" | "variant" | "item" | "table" | "code");
        if member && !(within.is_some() && depth == 0) {
            continue;
        }
        stack.push(item);
        if item.kind == "import" {
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
        out.push_str(&format!("{}{lines} {selector}\n", "  ".repeat(depth)));
    }
    flush(&mut out, &mut imports);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::Language;

    fn outline(text: &str) -> String {
        render(
            &SourceFile::new("a.rs", text.into(), Some(Language::Rust)),
            None,
        )
    }

    #[test]
    fn nests_items_by_containment() {
        let text = "mod a {\n    mod b {\n        fn c() {}\n    }\n    trait T {\n        fn d(&self);\n    }\n}\nconst E: u8 = 1;\n";
        assert_eq!(
            outline(text),
            "1-8 mod:a\n  2-4 mod:b\n    3 fn:c\n  5-7 trait:T\n    6 fn:d\n9 const:E\n"
        );
    }

    #[test]
    fn collapses_consecutive_imports() {
        let text = "use a;\nuse b::{c, d};\nuse e;\n\nfn f() {}\nuse g;\nmod h {\n    use i;\n}\n";
        assert_eq!(
            outline(text),
            "1-3 import (3)\n5 fn:f\n6 import (1)\n7-9 mod:h\n  8 import (1)\n"
        );
    }

    #[test]
    fn omits_items_inside_function_bodies() {
        let text = "fn f() {\n    let x = 1;\n    fn g() {}\n    struct S;\n}\n";
        assert_eq!(outline(text), "1-5 fn:f\n");
    }

    #[test]
    fn lists_fields_and_variants_only_inside_their_parent() {
        let text = "/// Doc.\nstruct S {\n    a: u8,\n}\nenum E {\n    A,\n    B(u8),\n}\n";
        assert_eq!(outline(text), "1-4 struct:S\n5-8 enum:E\n");
        let f = SourceFile::new("a.rs", text.into(), Some(Language::Rust));
        let s = f
            .items()
            .unwrap()
            .iter()
            .find(|i| i.kind == "struct")
            .unwrap();
        assert_eq!(render(&f, Some(&s.range)), "3 field:a\n");
        let e = f
            .items()
            .unwrap()
            .iter()
            .find(|i| i.kind == "enum")
            .unwrap();
        assert_eq!(render(&f, Some(&e.range)), "6 variant:A\n7 variant:B\n");
    }

    #[test]
    fn inside_a_span_starts_at_the_margin() {
        let text = "impl A {\n    fn b() {}\n    fn c() {}\n}\n";
        let f = SourceFile::new("a.rs", text.into(), Some(Language::Rust));
        assert_eq!(
            render(&f, Some(&(0..text.len()))),
            "1-4 impl:A\n  2 fn:b\n  3 fn:c\n"
        );
        assert_eq!(render(&f, Some(&(9..text.len()))), "2 fn:b\n3 fn:c\n");
    }

    #[test]
    fn names_impls_by_self_type() {
        let text = "impl<T> std::fmt::Display for W<T> {}\n";
        assert_eq!(outline(text), "1 impl:W\n");
    }

    #[test]
    fn outlines_markdown_as_a_tree_of_sections() {
        let text = "# Title\n\n## Two\n\n- a\n- b\n\n| X | Y |\n| - | - |\n\n```rust\nx\n```\n\n### Deep\n\ntext\n";
        let f = SourceFile::new("a.md", text.into(), Some(Language::Markdown));
        assert_eq!(
            render(&f, None),
            "1-17 section:Title\n  3-17 section:Two\n    15-17 section:Deep\n"
        );
        let two = f.items().unwrap().iter().find(|i| i.name == "Two").unwrap();
        assert_eq!(
            render(&f, Some(&two.range)),
            "5 item:a\n6 item:b\n8-9 table:X\n11-13 code:rust\n15-17 section:Deep\n"
        );
    }
}
