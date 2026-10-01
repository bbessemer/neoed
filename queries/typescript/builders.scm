; TypeScript's own builders, after queries/ecma/builders.scm.

(build interface_declaration "interface " name " {" _ "}")  ; members
(build type_alias_declaration "type " name " = " _ ";")     ; types
