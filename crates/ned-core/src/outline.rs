//! The `outline` symbol tree (command-language spec, §6.2).

use std::ops::Range;

use crate::select::SourceFile;

/// The outline entries of the items in `f`, one per line, or of the items
/// strictly inside `within`. Empty if `f` has no items.
pub fn render(f: &SourceFile, within: Option<&Range<usize>>) -> String {
    let _ = (f, within);
    String::new()
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
}
