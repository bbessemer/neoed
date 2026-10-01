; Selector items for Go. Capture conventions: see crates/ned-core/src/syntax.rs.
; A declaration with one spec is the item, so its span takes the keyword; in a
; grouped `( ... )` declaration, each spec is an item.

(function_declaration
  name: (identifier) @name
  parameters: (parameter_list) @params
  result: (_)? @ret
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
  result: (_)? @ret
  body: (block)? @body
  (#set! name "{receiver}.{name}")) @fn

(method_elem
  name: (field_identifier) @name
  parameters: (parameter_list) @params
  result: (_)? @ret) @fn

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
] @value) .) @type
(type_spec name: (type_identifier) @name type: [
  (type_identifier) (qualified_type) (generic_type) (pointer_type) (slice_type)
  (array_type) (map_type) (channel_type) (function_type)
] @value) @type
(type_declaration . (type_alias name: (type_identifier) @name type: (_) @value) .) @type
(type_alias name: (type_identifier) @name type: (_) @value) @type

(field_declaration name: (field_identifier) @name type: (_) @ty) @field
(field_declaration !name type: (_) @name) @field

(const_declaration . (const_spec name: (identifier) @name type: (_)? @ty value: (_)? @value) .) @const
(const_spec name: (identifier) @name type: (_)? @ty value: (_)? @value) @const
(var_declaration . (var_spec name: (identifier) @name type: (_)? @ty value: (_)? @value) .) @var
(var_spec name: (identifier) @name type: (_)? @ty value: (_)? @value) @var

(import_declaration . (import_spec
  path: (_ (interpreted_string_literal_content) @name)) .) @import
(import_spec path: (_ (interpreted_string_literal_content) @name)) @import

(comment) @doc
