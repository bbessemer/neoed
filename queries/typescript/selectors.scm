; TypeScript's (and TSX's) own selector items, after
; queries/ecma/selectors.scm.

(public_field_definition name: (_) @name type: (_ (_) @ty)? value: (_)? @value) @field
(abstract_class_declaration name: (_) @name body: (class_body) @body) @class

(function_signature name: (identifier) @name parameters: (formal_parameters) @params return_type: (_ (_) @ret)?) @fn
(abstract_method_signature name: (_) @name parameters: (formal_parameters) @params return_type: (_ (_) @ret)?) @fn

; Return and declared types, which JavaScript lacks, join the item
; queries/ecma/selectors.scm finds for the same node. A type annotation's
; type is its child, after the `:`.
(function_declaration name: (identifier) @name return_type: (_ (_) @ret)) @fn
(generator_function_declaration name: (identifier) @name return_type: (_ (_) @ret)) @fn
(method_definition name: (_) @name return_type: (_ (_) @ret)) @fn
(lexical_declaration (variable_declarator
  name: (identifier) @name
  value: [(arrow_function return_type: (_ (_) @ret)) (function_expression return_type: (_ (_) @ret))])) @fn
(variable_declaration (variable_declarator
  name: (identifier) @name
  value: [(arrow_function return_type: (_ (_) @ret)) (function_expression return_type: (_ (_) @ret))])) @fn
(lexical_declaration kind: "const" (variable_declarator name: (identifier) @name type: (_ (_) @ty))) @const
(lexical_declaration kind: "let" (variable_declarator name: (identifier) @name type: (_ (_) @ty))) @var
(variable_declaration (variable_declarator name: (identifier) @name type: (_ (_) @ty))) @var

(interface_declaration name: (type_identifier) @name body: (interface_body) @body) @interface
(method_signature name: (_) @name parameters: (formal_parameters) @params return_type: (_ (_) @ret)?) @fn
(property_signature name: (_) @name type: (_ (_) @ty)?) @field

(type_alias_declaration name: (type_identifier) @name value: (_) @value) @type

(enum_declaration name: (identifier) @name body: (enum_body) @body) @enum
(enum_body (property_identifier) @name @variant)
(enum_body (enum_assignment name: (property_identifier) @name value: (_) @value) @variant)

(internal_module
  name: [(identifier) (nested_identifier)] @name
  body: (statement_block) @body) @mod
(module
  name: [(identifier) @name (nested_identifier) @name (string (string_fragment) @name)]
  body: (statement_block)? @body) @mod
