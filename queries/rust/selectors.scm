; Selector items for Rust. Capture conventions: see crates/ned-core/src/syntax.rs.

(function_item name: (identifier) @name) @fn
(function_signature_item name: (identifier) @name) @fn

(struct_item name: (type_identifier) @name) @struct
(field_declaration name: (field_identifier) @name) @field

(enum_item name: (type_identifier) @name) @enum
(enum_variant name: (identifier) @name) @variant

(trait_item name: (type_identifier) @name) @trait

(impl_item
  type: [
    (type_identifier) @name
    (generic_type type: (type_identifier) @name)
    (scoped_type_identifier name: (type_identifier) @name)
  ]) @impl

(type_item name: (type_identifier) @name) @type
(associated_type name: (type_identifier) @name) @type

(const_item name: (identifier) @name) @const
(static_item name: (identifier) @name) @const

(let_declaration pattern: (identifier) @name) @var

(mod_item name: (identifier) @name) @mod

(use_declaration argument: (_) @name) @import

(line_comment outer: (outer_doc_comment_marker)) @doc
(block_comment outer: (outer_doc_comment_marker)) @doc
(attribute_item) @attr
