; Selector items for Markdown. Capture conventions: see crates/ned-core/src/syntax.rs.
; Only `#` headings start sections in this grammar. An item without a name
; pattern match (a list item opening with a code block, a code block without an
; info string) has the empty name.

(section (atx_heading heading_content: (inline) @name) @head) @section

(list_item (paragraph (inline) @name)) @item
(list_item) @item

(pipe_table (pipe_table_header . (pipe_table_cell) @name)) @table

(fenced_code_block (info_string (language) @name)) @code
(fenced_code_block) @code
(indented_code_block) @code
