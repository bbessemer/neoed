; Builders for Go fragments: see crates/ned-core/src/fragment.rs.

(build type_spec "type " name " struct {\n" _ "\n}\n")          ; fields
(build type_spec "type " name " interface {\n" _ "\n}\n")       ; methods
(build expression_switch_statement "func f() {\nswitch " value " {\n" _ "\n}\n}\n")  ; cases
(build function_declaration "func " name "(" _ ") {}\n")          ; parameters
