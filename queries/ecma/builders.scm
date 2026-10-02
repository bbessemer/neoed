; Builders for JavaScript and TypeScript fragments: see
; crates/ned-core/src/fragment.rs.

(build class_declaration "class " name " {" _ "}")          ; members
(build object "({" _ "})")                                  ; properties
(build function_declaration "function " name "(" _ ") {}")  ; parameters
(build switch_statement "switch (" value ") {" _ "}")       ; cases
(build class_declaration "" _ "\nclass C {}")                 ; decorators
