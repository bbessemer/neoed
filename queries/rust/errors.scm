; Code the grammar accepts without an ERROR node but rustc rejects; the
; parse-error guard counts each match as an error (spec §4.3).

; A `(…)` or `[…]` macro call with no `;`, followed by another statement:
; tree-sitter-rust accepts a bare macro call as a statement, but only a `{…}`
; one may go without `;` unless it is the block's tail expression. A comment
; (`//`, `/*`) after it doesn't count.
((block
  (macro_invocation (token_tree . ["(" "["])) @error
  (_) @next)
  (#not-match? @next "^/[/*]")
  (#set! message "macro statement needs a `;` before the next statement")
  (#set! fix "add one after its closing bracket"))
