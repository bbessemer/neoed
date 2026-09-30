; Selector items for Go. Capture conventions: see crates/ned-core/src/syntax.rs.
; A declaration with one spec is the item, so its span takes the keyword; in a
; grouped `( ... )` declaration, each spec is an item.

(function_declaration
  name: (identifier) @name
  parameters: (parameter_list) @params
  body: (block)? @body) @fn

; A method is named `RECEIVER.NAME`.
(method_declaration
  receiver: (parameter_list (parameter_declaration type: [
    (type_identifier) @receiver
    (generic_type type: (type_identifier) @receiver)
    (pointer_type [
      (type_identifier) @receiver
      (generic_type type: (type_identifier) @receiver)
    ])
  ]))
  name: (field_identifier) @name
  parameters: (parameter_list) @params
  body: (block)? @body
  (#set! name "{receiver}.{name}")) @fn

(method_elem
  name: (field_identifier) @name
  parameters: (parameter_list) @params) @fn

(type_declaration . (type_spec
  name: (type_identifier) @name
  type: (struct_type (field_declaration_list) @body)) .) @struct
(type_spec
  name: (type_identifier) @name
  type: (struct_type (field_declaration_list) @body)) @struct

(type_declaration . (type_spec
  name: (type_identifier) @name
  type: (interface_type)) .) @interface
(type_spec name: (type_identifier) @name type: (interface_type)) @interface

(type_declaration . (type_spec name: (type_identifier) @name type: [
  (type_identifier) (qualified_type) (generic_type) (pointer_type) (slice_type)
  (array_type) (map_type) (channel_type) (function_type)
]) .) @type
(type_spec name: (type_identifier) @name type: [
  (type_identifier) (qualified_type) (generic_type) (pointer_type) (slice_type)
  (array_type) (map_type) (channel_type) (function_type)
]) @type
(type_declaration . (type_alias name: (type_identifier) @name) .) @type
(type_alias name: (type_identifier) @name) @type

(field_declaration name: (field_identifier) @name) @field
(field_declaration !name type: (_) @name) @field

(const_declaration . (const_spec name: (identifier) @name) .) @const
(const_spec name: (identifier) @name) @const
(var_declaration . (var_spec name: (identifier) @name) .) @var
(var_spec name: (identifier) @name) @var

(import_declaration . (import_spec
  path: (_ (interpreted_string_literal_content) @name)) .) @import
(import_spec path: (_ (interpreted_string_literal_content) @name)) @import

(comment) @doc
