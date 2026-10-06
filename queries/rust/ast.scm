; Tokens an abstract syntax tree leaves out (spec §3.10): separators, which
; the node they sit in already implies. One capture per pattern: tree-sitter
; allows three on a node in one pattern, so each rule stays its own pattern.
; A token captured `@keep` stays, even if a rule skips it.

"," @skip
";" @skip

; A macro's tokens imply nothing: `[0; 4]` isn't `[0, 4]`.
(token_tree ";" @keep)

; A one-element tuple's comma is what makes it a tuple: `(u8,)` isn't `(u8)`.
(tuple_type . (_) . "," @keep)
(tuple_pattern . (_) . "," @keep)
