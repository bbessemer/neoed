; Selector items for Python. Capture conventions: see crates/ned-core/src/syntax.rs.
; A @block body is the block's whole lines; a docstring is the item's @doc.

(function_definition
  name: (identifier) @name
  parameters: (parameters) @params
  body: (block . (expression_statement (string) @doc)?) @block) @fn

(class_definition
  name: (identifier) @name
  body: (block . (expression_statement (string) @doc)?) @block) @class

(class_definition
  body: (block
    (expression_statement (assignment left: (identifier) @name)) @field))

(module
  (expression_statement (assignment left: (identifier) @name)) @const
  (#match? @name "^[A-Z][A-Z0-9_]*$"))
(module
  (expression_statement (assignment left: (identifier) @name)) @var
  (#not-match? @name "^[A-Z][A-Z0-9_]*$"))

(import_statement name: [
  (dotted_name) @name
  (aliased_import name: (dotted_name) @name)
]) @import
(import_from_statement module_name: (_) @name) @import
(future_import_statement (#set! name "__future__")) @import

(decorator) @attr
