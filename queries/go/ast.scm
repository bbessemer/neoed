; Tokens an abstract syntax tree leaves out (spec §3.10): separators, which
; the node they sit in already implies. One capture per pattern: tree-sitter
; allows three on a node in one pattern, so each rule stays its own pattern.

"," @skip
";" @skip

; Lists: a placeholder that is a list's only element stands for the element,
; so `if @c { @s }` matches a block of one statement and `return @x` returns
; one value.
(statement_list) @list
(expression_list) @list
