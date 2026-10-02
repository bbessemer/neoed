; Builders for Python fragments: see crates/ned-core/src/fragment.rs.

(build function_definition "def " name "(" _ "):\n    pass")   ; parameters
(build match_statement "match " subject ":\n    " _)           ; case clauses
(build dictionary "{" _ "}")                                   ; pairs
(build decorated_definition "" _ "\ndef f():\n    pass")         ; decorators
