; Builders for Rust fragments: see crates/ned-core/src/fragment.rs.
;
; (build KIND PART...) builds a KIND around a fragment that only parses inside
; one. Strings are literal, `_` is where the fragment goes, and any other
; symbol is one of KIND's fields, filled with a dummy name. Each builder needs
; a fragment in fragment.rs's BUILDER_SAMPLES that only it parses.

(build function_item "fn " name "() {" _ "}")         ; statements, tail expressions
(build match_expression "match " value " {" _ "}")    ; arms
(build struct_item "struct " name " {" _ "}")         ; fields
(build enum_item "enum " name " {" _ "}")             ; variants
(build function_item "fn " name "(" _ ") {}")         ; parameters, and types (a bare type is a parameter)
(build function_item "fn " name "<" _ ">() {}")       ; type parameters
