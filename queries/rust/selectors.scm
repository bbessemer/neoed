; Selector items for Rust. Capture conventions: see crates/ned-core/src/syntax.rs.
; @body and @params nodes include their delimiters.

(function_item
  name: (identifier) @name
  parameters: (parameters) @params
  body: (block) @body) @fn
(function_signature_item
  name: (identifier) @name
  parameters: (parameters) @params) @fn

(struct_item name: (type_identifier) @name body: (_)? @body) @struct
(field_declaration name: (field_identifier) @name) @field

(enum_item name: (type_identifier) @name body: (enum_variant_list) @body) @enum
(enum_variant name: (identifier) @name body: (_)? @body) @variant

(trait_item name: (type_identifier) @name body: (declaration_list) @body) @trait

; A trait impl is named `TRAIT for TYPE` (the kind `trait` can't be a capture
; name).
(impl_item
  trait: [
    (type_identifier) @trait_name
    (scoped_type_identifier name: (type_identifier) @trait_name)
    (generic_type type: [
      (type_identifier) @trait_name
      (scoped_type_identifier name: (type_identifier) @trait_name)
    ])
  ]?
  type: [
    (type_identifier) @name
    (generic_type type: (type_identifier) @name)
    (scoped_type_identifier name: (type_identifier) @name)
    (generic_type type: (scoped_type_identifier name: (type_identifier) @name))
    (reference_type type: [
      (type_identifier) @name
      (generic_type type: (type_identifier) @name)
    ])
  ]
  body: (declaration_list)? @body
  (#set! name "{trait_name} for {name}")) @impl

(type_item name: (type_identifier) @name) @type
(associated_type name: (type_identifier) @name) @type

(const_item name: (identifier) @name) @const
(static_item name: (identifier) @name) @const

(let_declaration pattern: (identifier) @name) @var

(mod_item name: (identifier) @name body: (declaration_list)? @body) @mod

(use_declaration argument: (_) @name) @import

(line_comment outer: (outer_doc_comment_marker)) @doc
(block_comment outer: (outer_doc_comment_marker)) @doc
(attribute_item) @attr
