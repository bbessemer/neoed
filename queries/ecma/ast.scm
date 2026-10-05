; Tokens an abstract syntax tree leaves out (spec §3.10): separators, which
; the node they sit in already implies. One capture per pattern: tree-sitter
; allows three on a node in one pattern, so each rule stays its own pattern.

"," @skip
";" @skip
