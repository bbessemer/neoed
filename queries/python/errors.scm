; Code the grammar accepts without an ERROR node but Python rejects; the
; parse-error guard counts each match as an error (spec §4.3).

; A compound statement with no body, as `class A:` once its only method is
; deleted: tree-sitter-python parses it with an empty `block`.
((block) @error
  (#eq? @error ""))
