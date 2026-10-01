; Selector items shared by JavaScript and TypeScript, which each add their own
; file. Capture conventions: see crates/ned-core/src/syntax.rs.

(function_declaration
  name: (identifier) @name
  parameters: (formal_parameters) @params
  body: (statement_block) @body) @fn
(generator_function_declaration
  name: (identifier) @name
  parameters: (formal_parameters) @params
  body: (statement_block) @body) @fn
(method_definition
  name: (_) @name
  parameters: (formal_parameters) @params
  body: (statement_block) @body) @fn

; A variable declared as a function is also a `const` or `var`.
(lexical_declaration (variable_declarator
  name: (identifier) @name
  value: [
    (arrow_function parameters: (formal_parameters)? @params body: (statement_block)? @body)
    (function_expression parameters: (formal_parameters) @params body: (statement_block) @body)
  ])) @fn
(variable_declaration (variable_declarator
  name: (identifier) @name
  value: [
    (arrow_function parameters: (formal_parameters)? @params body: (statement_block)? @body)
    (function_expression parameters: (formal_parameters) @params body: (statement_block) @body)
  ])) @fn

(class_declaration name: (_) @name body: (class_body) @body) @class

(lexical_declaration kind: "const" (variable_declarator name: (identifier) @name value: (_)? @value)) @const
(lexical_declaration kind: "let" (variable_declarator name: (identifier) @name value: (_)? @value)) @var
(variable_declaration (variable_declarator name: (identifier) @name value: (_)? @value)) @var

(import_statement source: (string (string_fragment) @name)) @import

; An exported item includes its `export`.
(export_statement declaration: (_)) @wrap

((comment) @doc (#match? @doc "^/\\*\\*"))
(decorator) @attr
