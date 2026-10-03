# ned plugins

This is the spec for `ned`'s plugin language and plugin API. The command
language (`docs/command-language.md`, cited here as §N) is unchanged by it until
the plugin milestone lands (§9.4). Nothing here is implemented yet; the VM that
runs it is designed in `crates/ned-scheme/DESIGN.md`. Change this file before
changing behaviour.

## 1. Overview

A plugin is a Scheme file that adds verbs and selector kinds to `ned`. Plugins
exist for edits that need logic: generating code from a struct's fields,
rewriting a call only when its arguments meet a condition, or anything a
`replace` with a pattern (§3.10) can't express. They are not for running
processes, reaching the network or editing outside the script's file set: a
plugin sees only what `ned` lends it (§10).

The language is a small Scheme. It targets R7RS-small with extensions, but
compatibility gives way where tree-sitter's query syntax or `ned`'s needs differ
(§2.3). Its data model includes syntax tree **nodes** (§3), so plugin code takes
code apart with the query syntax every tree-sitter grammar already documents
(§5), and puts it together by writing it: a **syntax literal** such as
`` #`fn new() -> Self { @body }` `` is code, parsed and checked like a §3.10
pattern, with Scheme values spliced into its placeholders (§4). In a `match` the
same literal is a pattern (§5.1).

```scheme
(define-verb (getter sel field)
  "Add a getter for FIELD to the selected impl."
  (let ((name (string->node field 'identifier)))
    (insert-end! sel #`pub fn @name(&self) -> &str { &self.@name }`)))
```

```
ned src/user.rs -e 'getter impl:User "email"'
```

## 2. Lexical syntax

Plugins are read by `ned-scheme`'s reader, the one that reads
`queries/<lang>/*.scm`, so every query file is valid plugin data.

### 2.1 Tokens

- **Delimiters** are whitespace and ``( ) [ ] " ; ' ` ,``. Any other run of
  characters is one token, so `#eq?`, `name:`, `@x.y` and `a->b` are single
  tokens.
- **Booleans** are `#t`, `#true`, `#f` and `#false`.
- **Numbers** are `[+-]?DIGITS` (an integer) or `[+-]?DIGITS.DIGITS` (a real).
  Any other token is a symbol: `1.`, `1e3` and `+` among them.
- **Strings** are `"..."` and may span lines. Escapes are tree-sitter's: `\n`,
  `\r`, `\t`, `\0`, and `\X` for any other character X.
- **Keywords** are tokens ending in `:`, such as `name:`. A lone `:` is a
  symbol.
- **Captures** are tokens starting with `@`, such as `@name` or `@fn.body`.
- **Anchors** are a lone `.`.
- **Comments** are `;` to the end of the line, `#| ... |#` (nesting), and `#;`
  before a datum, which comments out that datum.
- **Quotes**: `'x`, `` `x ``, `,x` and `,@x` read as `(quote x)`,
  `(quasiquote x)`, `(unquote x)` and `(unquote-splicing x)`.

### 2.2 Compound data

- `(...)` is a list and `[...]` an **alternation**, as in queries. Brackets are
  not parentheses: `(let ([x 1]) x)` is an error whose fix is `(let ((x 1)) x)`.
- `` #`...` `` is a syntax literal (§4). It ends at the next run of as many
  backquotes as opened it, so a literal holding backquotes opens with more:
  ``` #`` `${name}` `` ```. One space just inside each end is dropped when both
  ends have one, and indentation common to its lines is ignored, as for §3.10
  patterns. Inside it, `@` starts a placeholder (§4.1).
- Not yet read: characters (`#\a`), vectors (`#(...)`) and bytevectors
  (`#u8(...)`) are errors until tier 2 (§6.2) adds the first two.

### 2.3 Differences from R7RS

| R7RS                               | Here                                                                |
| ---------------------------------- | ------------------------------------------------------------------- |
| `(a . b)` is a pair                | `.` is an anchor; pairs exist but have no read syntax (§3.1)        |
| `[...]` is a list (in most Scheme) | `[...]` is an alternation, valid only in patterns                   |
| `name:` is a symbol                | `name:` is a keyword and evaluates to itself (SRFI 88)              |
| `@x` is a symbol                   | `@x` is a capture, valid only in patterns                           |
| `#eq?` is not an identifier        | `#eq?` is a symbol, so query predicates read as lists               |
| `\x41;` and `\a` string escapes    | tree-sitter escapes (§2.1)                                          |
| `\|sym\|` symbols                  | `\|` is an ordinary token character                                 |
| exact rationals, bignums, complex  | 64-bit integers and reals only; integer overflow is an error        |
| full `call/cc`                     | escape-only continuations (§6.2)                                    |
| ports, `load`, `eval`, files       | none (§10)                                                          |
| mutable strings                    | strings are immutable; `string-set!` and `string-fill!` don't exist |

## 3. Data model

### 3.1 Types

Values are booleans, integers (`i64`), reals (`f64`), strings, symbols,
keywords, pairs and the empty list, procedures, records, error objects, spans
(§8.1), nodes and templates. Tier 2 adds characters and vectors. A template is a
syntax literal as data, `` '#`foo(@a)` ``: the value of quoting one, usable as a
pattern (§5.3).

- `eqv?` compares numbers, characters, symbols, keywords and booleans by value
  and everything else by identity; `eq?` is `eqv?`.
- `equal?` compares pairs, strings and records by contents, and nodes as
  `node=?` does (§7).
- `write` shows a pair whose `cdr` isn't a list as `(a . b)`, which the reader
  can't read back; build pairs with `cons`.

### 3.2 Nodes

A node is an immutable reference to a node of a tree-sitter tree, with the
source text it spans. It is **attached** when it comes from a file in the
script's file set, as the tree was at the start of the script's current stage
(§2.3), and **detached** when a syntax literal or `string->node` built it.

- A node's kind is a symbol for a named node (`function_item`) and a string for
  an anonymous one (`"fn"`), as queries write them.
- A node is written as its kind and its text:
  ``#<function_item `fn new() {}`>``. The text is cut to its first line and 60
  characters, with `...`.
- `node->datum` gives the node's **AST form**: query syntax, with each named
  child as a list under its field name and each named leaf (an identifier, a
  comment) with its text as a string. Of the anonymous tokens, it keeps only
  those that carry meaning: an operator, or a keyword chosen from several
  (`async`). The tokens a kind always has (`fn`, `(`, the `->` before a return
  type) and separators are left out. The language's builders say which are which
  (§11).

  ```scheme
  (node->datum #`async fn f(x: u32) -> u8 {}`)
  ; => (function_item
  ;      (function_modifiers "async")
  ;      name: (identifier "f")
  ;      parameters: (parameters
  ;                    (parameter pattern: (identifier "x")
  ;                               type: (primitive_type "u32")))
  ;      return_type: (primitive_type "u8")
  ;      body: (block))
  ```

  Read as a pattern (§5.2), the datum matches the node it came from. A kind with
  no builder keeps all its tokens, so nothing is ever left out unless a builder
  accounts for it.

- `datum->node` inverts it: `(datum->node DATUM)` is a detached node whose AST
  form is `DATUM`, in the current language (§4.3). Builders write its text
  (§11.3), which is parsed as a fragment (§11.4) and must read back as `DATUM`,
  with field names compared only where `DATUM` gives them. A datum that doesn't
  read back is an error showing the first node that came out different, and what
  it came out as. `(datum->node (node->datum N))` is `node=?` to `N`. The text
  has the builders' layout; the formatter lays it out when it is edited into a
  file (§6.4).

- Nodes are never mutated. An edit (§8) records a change to a file; the node
  keeps the text it had.

## 4. Syntax literals

A syntax literal builds code. `` #`CODE` `` evaluates to the node `CODE` parses
to, in the current language (§4.3), with each placeholder replaced by the text
of the node its expression yields.

### 4.1 Placeholders

| Placeholder  | Splices                                         |
| ------------ | ----------------------------------------------- |
| `@name`      | the node bound to the variable `name`           |
| `@name...`   | the nodes in the list bound to `name`, in order |
| `@{EXPR}`    | the node `EXPR` evaluates to                    |
| `@{EXPR}...` | the nodes in the list `EXPR` evaluates to       |
| `@@`         | a literal `@`                                   |

- A name follows §3.10's rule: a letter or `_`, then letters, digits and `_`.
  Use `@{...}` for any other variable: `@{field-name}`. Inside `@{`, `}` is a
  delimiter, so `@{x}` is the variable `x`.
- `@_` and `@_...` stand for code in patterns only; in a construction they are
  an error whose fix is `@{...}`.
- A placeholder inside a string or comment in `CODE` is literal text, as in
  §3.10.
- Every value must be a node, or for `...` a list of nodes. A string is an error
  whose fix is `(string->node STRING 'KIND)` (§7); this keeps every splice a
  whole node, so the result's shape is the literal's.
- The nodes of a `...` splice are joined by the first of `, `, a line break and
  a space under which each of them comes out as a whole sibling (§4.2, step 3):
  `, ` in an argument list, a line break between statements or items.

### 4.2 Parsing and checking

A literal is parsed once per language, the first time it is evaluated there,
with each placeholder as a hole: alone, then inside each of the language's
contexts (§11.4), keeping the first reading that parses without errors (§3.10's
"Parsing", but construction needs one reading). A hole that isn't a whole node,
or code that parses in no context, is an error at the literal, whatever the
values.

Each evaluation then builds text and checks it:

1. Each value's text replaces its placeholder. A value spanning several lines
   keeps its later lines' indentation relative to its first, under the
   indentation of the line the placeholder is on (§3.10, "Substitution").
2. The text is parsed again in the same reading.
3. Each spliced node must come out as one whole node at its place (a run of
   whole siblings, for `...`), of the same kind. A named leaf may change kind
   with its context: an `identifier` spliced where a type goes becomes a
   `type_identifier`. Anything else is an error naming the placeholder, the kind
   it held and what the text parsed as. This catches precedence: splicing
   `a + b` into `` #`@x * 2` `` gives `a + b * 2`, where `@x` is no longer one
   node, and the fix is the language's grouping, `` #`(@x) * 2` ``.

The value is the reading's root node, detached, or a list of nodes when the code
is several statements or items. The text is the literal's, not reformatted: the
formatter runs on the file after the edit, as for any edit (§6.4).

`(node-as KIND NODE)` re-reads a detached node's text in the first reading whose
root is `KIND`, for code that parses more than one way (`` #`x: u32` `` is a
field and a parameter). Splicing never needs it: a spliced node's text is parsed
again where it lands.

### 4.3 Language

The language of a literal is that of the first node spliced into it, and of all
of them: nodes of different languages in one literal are an error. A literal
with no node spliced in uses the current language, the parameter
`current-language`: `ned` binds it to the language of the file a verb runs on
(§9.2). `(with-language 'rust BODY...)` binds it for its body. A literal
evaluated where `current-language` is `#f` is an error whose fix is
`with-language`.

## 5. Patterns and `match`

A syntax literal builds code and a §3.10 pattern selects code by writing it: the
two are the same construct used in opposite directions, so in a `match` clause a
syntax literal is a pattern. Query syntax is the other pattern form, for
matching by structure instead of by writing.

### 5.1 `match`

```scheme
(match EXPR
  (PATTERN BODY...)
  ...
  (else BODY...))
```

`EXPR` must evaluate to a node. The first clause whose pattern matches it runs,
with each capture bound to a variable in `BODY`, and its last value is the
`match`'s value. No matching clause and no `else` is an error showing the node.
A clause's pattern is written as is, not evaluated:

- `` #`CODE` ``: a §3.10 pattern, matched with the node as its root. `@name`
  binds `name` to a node, `@name...` to a list of nodes, and `@_` binds nothing.
  A name used twice matches equal code. `@{EXPR}` matches code equal to the node
  `EXPR` evaluates to (`node=?`), evaluated before matching, in the enclosing
  scope.
- A query (§5.2) whose outermost pattern must match the node itself.
- `_`, which matches any node.

```scheme
(match call
  (#`@recv.unwrap()` #`@recv?`)
  (#`@recv.expect(@_)` #`@recv?`)
  (else call))
```

### 5.2 Query patterns

A query pattern is tree-sitter query syntax, as in `selectors.scm` and §3.6:
`(kind field: (kind) @capture)`, `_`, anonymous nodes as strings, `[...]`
alternations, `*`, `+` and `?` quantifiers, `.` anchors, `!field` negations and
the `#eq?`, `#not-eq?`, `#match?`, `#not-match?`, `#any-of?` predicates. One
extension: a string as the only child of a named node that has no named children
matches its text, so `(identifier "new")` matches the identifier `new`, and
`node->datum`'s output is a pattern.

Captures bind as in tree-sitter: `@x` binds `x` to a node, a quantified capture
(`(parameter)* @ps`) to a list of nodes, and a capture under `?` that matched
nothing to `#f`. A capture name with dots binds that symbol: `@fn.name` binds
`fn.name`.

```scheme
(match item
  ((function_item name: (identifier) @name
                  body: (block . (expression_statement)* @stmts))
   (length stmts))
  (else 0))
```

### 5.3 Searching

| Procedure                 | Returns                                                             |
| ------------------------- | ------------------------------------------------------------------- |
| `(find-all NODE PATTERN)` | each match inside `NODE`, in source order, without overlaps (§3.10) |
| `(find NODE PATTERN)`     | the first match, or `#f`                                            |

`PATTERN` here is a value: a template (§3.1), such as `` '#`foo(@a)` ``, or a
query datum, such as `'((call_expression) @c)`, so patterns can be built with
`quasiquote`. Each match is a pair of the matched node and an association list
from capture symbols to what they bound:
`(#<call_expression ...> (a . #<identifier ...>))`.

## 6. Core language

Forms and procedures come in tiers: tier 1 ships with the first plugin release,
tier 2 later (§6.2). R7RS names and meanings are kept where they exist.

### 6.1 Tier 1

**Special forms**: `define` (variables and procedures), `lambda`, `let`, `let*`,
`letrec`, `letrec*`, named `let`, `if`, `cond` (with `=>`), `case`, `and`, `or`,
`when`, `unless`, `do`, `begin`, `set!`, `quote`, `quasiquote`, `unquote`,
`unquote-splicing`, `define-record-type`, `match` (§5.1), `guard`,
`with-language` (§4.3), `define-verb` and `define-kind` (§9). Calls in tail
position are proper tail calls.

In a parameter list, `.` before the last parameter makes it a rest parameter, as
in R7RS: `(define (f a . rest) ...)`. The reader reads that `.` as an anchor;
`define` and `lambda` give it this meaning.

**Procedures** (R7RS unless marked):

- Equivalence: `eq?`, `eqv?`, `equal?`.
- Numbers: `number?`, `integer?`, `real?`, `+`, `-`, `*`, `/`, `quotient`,
  `remainder`, `modulo`, `abs`, `min`, `max`, `=`, `<`, `>`, `<=`, `>=`,
  `zero?`, `positive?`, `negative?`, `even?`, `odd?`, `floor`, `ceiling`,
  `round`, `truncate`, `exact`, `inexact`, `number->string`, `string->number`.
  `/` of two integers that don't divide is a real.
- Booleans: `not`, `boolean?`.
- Lists: `cons`, `car`, `cdr`, `caar`, `cadr`, `cdar`, `cddr`, `set-car!`,
  `set-cdr!`, `pair?`, `null?`, `list?`, `list`, `length`, `append`, `reverse`,
  `list-tail`, `list-ref`, `memq`, `memv`, `member`, `assq`, `assv`, `assoc`,
  `map`, `for-each`, `apply`; from SRFI 1: `filter`, `remove`, `fold`,
  `fold-right`, `reduce`, `find-tail`, `any`, `every`, `delete`, `iota`, `last`,
  `append-map`, `filter-map`, `partition`.
- Symbols and keywords: `symbol?`, `symbol->string`, `string->symbol`,
  `keyword?`, `keyword->string`, `string->keyword`.
- Strings: `string?`, `string-length`, `substring`, `string-append`, `string=?`,
  `string<?`, `string>?`, `string-upcase`, `string-downcase`, `string-copy`;
  `ned`: `string-index` (of a substring, or `#f`), `string-prefix?`,
  `string-suffix?`, `string-split` (on a literal separator), `string-join`,
  `string-trim`, `regex-match` (a list of the match and its groups, or `#f`),
  `regex-replace` (every match, `$1` and `${name}` expanded, as `sub`'s §4.2).
- Control and errors: `procedure?`, `error`, `raise`, `error-object?`,
  `error-object-message`, `error-object-irritants`; `ned`: `reject` (§10.3),
  `note` (§10.3).
- Nodes, spans and edits: §7 and §8.

### 6.2 Tier 2

`define-syntax` with `syntax-rules` (hygienic for the identifiers a template
introduces), characters and their procedures, vectors and their procedures,
`string->list`, `list->string`, `parameterize`, `make-parameter`,
`dynamic-wind`, `call/cc` and `call-with-current-continuation` as escape-only
continuations (calling one after its extent ends is an error),
`values`/`call-with-values`, `let-values`, `case-lambda`, and association-list
helpers. `define-library` and `import` come with plugins that share code.

## 7. Nodes

| Procedure                        | Returns                                                                 |
| -------------------------------- | ----------------------------------------------------------------------- |
| `(node? X)`                      | whether X is a node                                                     |
| `(node-kind N)`                  | its kind: a symbol, or a string for an anonymous node                   |
| `(node-text N)`                  | its source text                                                         |
| `(node-field N KEY)`             | the child in field `KEY` (`name:`), or `#f`                             |
| `(node-fields N KEY)`            | every child in field `KEY`, as a list                                   |
| `(node-children N)`              | its children, anonymous ones included                                   |
| `(node-named-children N)`        | its named children                                                      |
| `(node-parent N)`                | its parent, or `#f` at the root or for a detached root                  |
| `(node-next N)`, `(node-prev N)` | its next or previous named sibling, or `#f`                             |
| `(node-span N)`                  | the span (§8.1) it covers in its file; an error for a detached node     |
| `(node-file N)`                  | its file's path relative to the workspace, or `#f` when detached        |
| `(node-language N)`              | its language as a symbol (`rust`)                                       |
| `(node-error? N)`                | whether it is or contains a parse error                                 |
| `(node=? A B)`                   | whether they are equal code: same kinds and leaf text, comments ignored |
| `(node->datum N)`                | its query-syntax datum (§3.2)                                           |
| `(datum->node D)`                | the detached node `D` describes (§3.2)                                  |
| `(string->node S KIND)`          | `S` parsed as one node of `KIND` in the current language (§4.3)         |
| `(node-as KIND N)`               | a detached node's text re-read with a `KIND` root (§4.2)                |
| `(file-root PATH)`               | the root node of a file in the file set, or an error naming the set     |

- `string->node` makes leaves for splicing:
  `(string->node "email" 'identifier)`. It parses `S` as a syntax literal with
  no placeholders and checks that the whole text is one node of `KIND`, allowing
  a named leaf's change of kind as §4.2 does; a string that isn't is an error
  showing what it parsed as.
- Comments are children like any other node; `node-named-children` includes them
  when the grammar names them, as most do.

## 8. Edits

### 8.1 Spans

A span is a range of a file in the file set: what a selector selects (§3.8) and
what an edit targets. Attached nodes are spans too: every procedure below that
takes a span takes an attached node.

| Procedure           | Returns                                                                 |
| ------------------- | ----------------------------------------------------------------------- |
| `(span? X)`         | whether X is a span                                                     |
| `(span-file S)`     | its file's path relative to the workspace                               |
| `(span-start S)`    | its start as `(LINE . COLUMN)`, both from 1                             |
| `(span-end S)`      | its end, likewise                                                       |
| `(span-text S)`     | its text                                                                |
| `(span-node S)`     | the node it covers exactly, or `#f` (a line range, a regex match)       |
| `(span-captures S)` | its pattern captures (§3.10), as an association list of nodes and lists |

### 8.2 Edit procedures

| Procedure                 | As the verb            |
| ------------------------- | ---------------------- |
| `(replace! S TEXT)`       | `replace S with TEXT`  |
| `(insert-before! S TEXT)` | `insert before S TEXT` |
| `(insert-after! S TEXT)`  | `insert after S TEXT`  |
| `(insert-start! S TEXT)`  | `insert start S TEXT`  |
| `(insert-end! S TEXT)`    | `insert end S TEXT`    |
| `(delete! S)`             | `delete S`             |

- `TEXT` is a node, a list of nodes (joined by line breaks) or a string. Its
  text is placed and re-based exactly as the verb's `<<END` text would be (§5):
  a node is several-line text unless it is on one line.
- An edit procedure returns no value; it records the edit in the script's edit
  set. Nothing changes until the script ends, so trees, nodes and `span-text`
  keep showing the stage's snapshot, and later edits in the same plugin call
  target the original text, as later commands in a stage do (§2.3).
- Edits overlapping each other or another command's edit reject the script (exit
  1), as for verbs. The parse-error guard, edit checking and formatting apply to
  the result as usual (§4.3, §6.4, §6.5), and the script is applied all or
  nothing.
- A span outside the file set, or a detached node, is an error.

## 9. The plugin API

### 9.1 Plugin files

A plugin is one `.scm` file. Its first form declares it:

```scheme
(plugin rust-getters
  (ned "0.8")
  (verbs getter getters)
  (kinds (rust test)))
```

- `(ned "VERSION")` is the oldest `ned` it needs; an older `ned` refuses it.
- `(verbs NAME...)` and `(kinds (LANG KIND...)...)` list what the file defines,
  so `ned` reads only this form to learn what a plugin provides, and evaluates a
  plugin only when a script uses one of its words. A file defining a word it
  didn't declare, or declaring one it doesn't define, is an error when it loads.

`ned` finds plugins in `.ned/plugins/` at the workspace root and in
`$XDG_CONFIG_HOME/ned/plugins/` (else `~/.config/ned/plugins/`), and loads those
`.ned.toml` names, by file stem:

```toml
[plugins]
enable = ["rust-getters", "go-errors"]
```

The workspace's list replaces the user config's. A name found in both
directories is the workspace's. Two enabled plugins providing one word is a
config error (exit 2) naming both. A plugin's words can't shadow `ned`'s own
verbs or a language's built-in kinds.

### 9.2 Verbs

```scheme
(define-verb (NAME SEL ARG...)
  "DOC"
  BODY...)
```

A script calls it as `NAME [all] SEL [TEXT]...`: a selector, then a `TEXT` (a
string or heredoc, §2.1) for each `ARG`. The procedure is called once per
selected span, in source order, with `SEL` bound to the span and each `ARG` to a
string; with `all`, every match is called, and without it the selector must
match once (§3.5), as for any verb. `current-language` is the span's file's
language, or `#f` for a text file. `DOC` is shown by `ned help NAME`, under a
usage line built from the parameters.

- `NAME` is lowercase letters, digits and `-`, starting with a letter.
- A verb call is a command like any other: it runs in its stage (§2.3), and its
  edits join the script's.
- Calling it with the wrong number of `TEXT`s is a script error (exit 2) giving
  the usage line.

### 9.3 Selector kinds

```scheme
(define-kind LANG KIND QUERY [PREDICATE])
```

`KIND:NAME` then selects in `LANG`'s files what `QUERY` captures, as a pattern
in `queries/LANG/selectors.scm` would (§3.3): the item is captured as `@KIND`
and its name as `@name`, with the same optional parts (`@body`, `@params`, ...).
`PREDICATE`, if given, is called with each item's node and keeps it when it
returns true.

```scheme
(define-kind 'rust 'test
  '((function_item name: (identifier) @name body: (block) @body) @test)
  (lambda (f)
    (let ((attr (node-prev f)))
      (and attr
           (match attr
             ((attribute_item (attribute (identifier "test"))) #t)
             (else #f))))))
```

`outline` lists plugin kinds with the built-in ones.

### 9.4 Command-language changes

When plugins ship, the command language gains plugin verbs (§9.2) in §2.2's
grammar and §4, plugin kinds in §3.3, the errors of §10.3 in §7, and the
`[plugins]` config section. `docs/command-language.md` changes first, in the
same milestone.

## 10. Sandbox and errors

### 10.1 Capabilities

A plugin can read the files of the script's file set (`-w`: the workspace's)
through spans and `file-root`, parse code in any language `ned` knows, record
edits, and add notes to the output. It has no other access: no files outside the
set, no writes but edits, no processes, network, environment, clock or
randomness. Given the same files and script, a plugin does the same thing, so
sessions (§1.2) record and undo its edits like any other.

### 10.2 Limits

Each script run gives each plugin a fuel limit (VM steps) and a memory limit,
set in `.ned.toml`:

```toml
[plugins]
fuel = 100_000_000   # steps per script
memory = "256MiB"
```

Running out is an error naming the plugin, the verb and the limit, whose fix is
raising it.

### 10.3 Errors and output

- `(reject MESSAGE [fix: FIX])` rejects the edit: the script fails with exit 1,
  as a selector matching nothing does, showing `MESSAGE` and `FIX`.
- Any other uncaught error (`error`, `raise`, a wrong type, a failed `match`, an
  exhausted limit) is a plugin error (exit 2):
  `error: PLUGIN.scm:LINE:COL: message`, a caret excerpt of the plugin source,
  the call chain as `PLUGIN.scm:LINE:COL` lines (innermost first), and the fix.
  `(error MESSAGE IRRITANT... fix: FIX)` sets the fix; without one, it is
  `check the plugin's call at script:LINE:COL`.
- `(note STRING)` adds a note to the output, as `ned`'s own notes are shown.
- Errors at load time (a reader error, an undeclared word, an unknown form) are
  plugin errors reported when a script first needs the plugin, and by
  `ned help NAME`.

## 11. Builders

tree-sitter produces a concrete syntax tree (CST), with every token; plugins
work on the AST form (§3.2), without the tokens a kind always has. A language's
builders convert between the two, one kind at a time, and give fragments the
code to parse inside (§11.4). They live in `queries/<lang>/builders.scm`, as
data, and most are generated from the grammar (§11.5).

```scheme
(build 'function_item
  #`@visibility_modifier? @function_modifiers? fn @name @type_parameters?
    @parameters @[-> @return_type] @where_clause? @body`)
(build 'parameters #`(@_ , ...)`)
(build 'binary_expression #`@left @operator @right`)
(build 'unary_expression #`@"- * !" @_`)
```

### 11.1 Templates

A builder's template is a syntax literal (§2.2) whose placeholders stand for the
node's children:

| Placeholder  | Stands for                                                                                             |
| ------------ | ------------------------------------------------------------------------------------------------------ |
| `@name`      | the child in field `name`, or, when the kind has no such field, its one unfielded child of kind `name` |
| `@_`         | its one unfielded named child, whatever its kind                                                       |
| `@x?`        | as `@x`, but may be absent                                                                             |
| `@x...`      | zero or more such children, on one line, or one per line when the placeholder is alone on its line     |
| `@x SEP ...` | zero or more, separated by the token `SEP`; a trailing `SEP` is read but not written                   |
| `@[TEXT]`    | `TEXT`, present exactly when the placeholders in it are: `@[-> @return_type]`                          |
| `@"A B"`     | one anonymous token, `A` or `B`, kept in the AST form as its string                                    |
| `@@`         | a literal `@`                                                                                          |

The rest of the template is literal: the tokens the kind always has, which the
AST form leaves out. Spacing between tokens doesn't matter, but line breaks and
indentation are the layout `datum->node` writes, as in Python:

```scheme
(build 'function_definition
  #`def @name@parameters@[ -> @return_type]:
        @body`)
```

A kind may have several builders, for a rule whose alternatives have different
tokens; the first that fits is used.

### 11.2 Reading: CST to AST

`node->datum` lines the node's children up with the first of its kind's builders
that fits: each literal, spacing removed, must equal the text of the anonymous
children at its place, and each placeholder takes the children it stands for.
The AST form is the kind followed by what the placeholders took, in order.
Comments are skipped while lining up and kept before the child that follows
them. A node no builder fits keeps all its children, tokens included.

### 11.3 Writing: AST to CST

`datum->node` writes a datum with the first of its kind's builders whose
placeholders the datum satisfies: every required one present, and each `@"..."`
string among its choices. Each placeholder gets its children's text, written the
same way and re-indented to the template line it sits on; an absent optional one
writes nothing, nor does the `@[...]` around it. A kind with no builder writes
its children in order, its tokens included.

### 11.4 Contexts

A fragment that only parses inside other code (a match arm, a method, a field)
is parsed inside a context: code, built from builders, with a hole where the
fragment goes. For each placeholder that may hold a kind the grammar's start
rule can't hold directly (from `node-types.json`), `ned` writes the shortest
chain of builders from the start rule down to that placeholder, and fills every
other required placeholder with the smallest code of its kind (a name is
`__ned`). Placeholders that may hold the same kinds share a context. Contexts
are derived when a language is first needed, and are tried shortest first.

### 11.5 Generation

`builders.gen.scm` is generated from the grammar's `grammar.json` by a dev tool
and never edited. `builders.scm` is written by hand, and its builders replace
the generated ones for their kinds, where generation falls short: external
scanners (Python's indentation), layout, and rules the tool can't translate,
which it lists. Tests check that every builder fits some node of the language's
test corpus, and that `datum->node` of `node->datum` gives back every node of
it.

## 12. Not in this spec

Language-defining plugins (grammars whose items need logic), plugin filters in
`[...]` (§3.9), running Scheme from a script, a REPL, and libraries shared
between plugins come later.
