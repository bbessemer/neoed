; TypeScript's (and TSX's) own selector items, after
; queries/ecma/selectors.scm.

(public_field_definition name: (_) @name) @field
(abstract_class_declaration name: (_) @name body: (class_body) @body) @class

(function_signature name: (identifier) @name parameters: (formal_parameters) @params) @fn
(abstract_method_signature name: (_) @name parameters: (formal_parameters) @params) @fn

(interface_declaration name: (type_identifier) @name body: (interface_body) @body) @interface
(method_signature name: (_) @name parameters: (formal_parameters) @params) @fn
(property_signature name: (_) @name) @field

(type_alias_declaration name: (type_identifier) @name) @type

(enum_declaration name: (identifier) @name body: (enum_body) @body) @enum
(enum_body (property_identifier) @name @variant)
(enum_body (enum_assignment name: (property_identifier) @name) @variant)

(internal_module
  name: [(identifier) (nested_identifier)] @name
  body: (statement_block) @body) @mod
(module
  name: [(identifier) @name (nested_identifier) @name (string (string_fragment) @name)]
  body: (statement_block)? @body) @mod
