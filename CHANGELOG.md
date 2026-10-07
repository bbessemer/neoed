# Changelog

Every tagged release of `ned`. Before 0.6.1, every PR to `main` bumped the
version, so versions merged in quick succession (0.2.0, 0.3.0) were never tagged
or released on their own; their changes are listed under the next tag.

## 0.8.1 (2026-10-07)

### Added

- Re-basing converts indent width as well as style: 2-space text goes into a
  4-space file at 4. Aligned lines keep their offset from the line they align
  to: continuations after an unclosed bracket, a block comment's `*` lines, list
  item continuations, fenced code, and lines inside a multi-line string.

### Changed

- `.refs` and `.def` show results in files outside the file set, such as a
  library's source; editing one needs `-w`, or `file PATH` in the script.
  `rename` reaching outside the set is still an error.
- The MCP server's help texts and the `ned` tool's description name tool
  arguments (`files`, `dry_run`, ...) instead of the CLI's flags, and leave out
  its usage, `-e` and stdin; `repl` and `mcp` are CLI-only help topics. The
  skill and agent guide tell agents to prefer the MCP server when it's
  connected.

### Fixed

- A heredoc replacing a partial span that ends in a line ending ends its last
  line with one, instead of joining the next line on.
- `replace` of a span that takes in some of its line's indentation and the text
  after it (`/^\s*let/`) gives the first line that indentation back, unless the
  text gives its own.
- A `case` or `default` clause inserted next to a line of another clause (Go and
  JavaScript `switch`, Python `match`) goes beside it.
- One paragraph of Markdown prose placed in a list item puts its later lines
  under the item's text; prose inserted before an item's first line stays
  outside it.
- Text placed in a Markdown block quote takes its `>` markers, on blank lines
  too, and quoted text loses one level of `>` first.

### Hints

- A parse error after a heredoc whose tag appears again later, alone on a line,
  names the heredoc and where it ended.
- A bare name with a part or a step after it suggests `*:NAME`, or the kind of
  the item with that name.
- A selector that matches nothing because its name sits in code that doesn't
  parse says where, and which construct is broken.
- A `!!:s` OLD the script holds only with escaped delimiters suggests another
  delimiter; text after a modifier says NEW ended at the delimiter. A repeat
  given no flags notes it runs `without flags` (MCP: `without arguments`).
- A missing FILE argument that the script `create`s says to drop it; a script
  run on no files says whether it reads or edits, and names the files its
  `file:` steps select.
- A comma between selectors says to separate commands, and spells them out for a
  command that takes only selectors (`delete 3; delete 7`).
- A range whose end matches only before its start ends says so.
- A formatter that changed lines away from the edit gets a note naming them.
- A single-quoted TEXT holding a `/` gets the usual error about double quotes,
  not a garbled sed-form rewrite.

## 0.8.0 (2026-10-06)

### Added

- `ned repl`, and bare `ned` on a terminal: a human REPL that runs scripts on
  in-memory buffers with the CLI's output. `:write` merges into files changed
  since, and `:undo`, `:diff`, `:reload`, `:files`, `:history` and `:commit`
  work on the buffers; any unambiguous prefix of a command works. It records
  into a session, and `:attach` follows another session's log (an agent's),
  printing each entry with its diff, merging it into unwritten buffers and
  recording corrections into that session. `ned help repl`.
- `ned mcp`: an MCP server (JSON-RPC on stdio) whose `ned`, `outline`, `show`,
  `help`, `history`, `undo` and `cd` tools run as the invocations they stand
  for, so agents call `ned` without a shell. Calls are recorded in a session
  (`-s`, `NED_SESSION` or the next free `mcp-N`); the `ned` tool's optional
  `comment` is shown to an attached REPL. `ned help mcp`.
- `ned session list [--all]` and `ned session delete NAME...`.
- Themes: on truecolor and 256-colour terminals, a theme colours each highlight
  capture and output role and tints changed lines in diffs. `theme = "NAME"` or
  a `[theme]` table (optionally `from` another theme) in the user config, or
  `NED_THEME`, picks one; built-ins are `default-dark` (the default) and
  `default-light`.
- `*:NAME` selects an item of any kind (`show *:LIMIT`); when the name matches
  several kinds, each candidate names its kind.
- `.lines:N` and `.lines:$` pick a span's Nth or last line, skipping spans too
  short with a note, and work in filters (`all fn[.lines:1 ~= /async/]`).
- `show raw` prints the selected text without line numbers, for copying it.
- `-e -` reads a script from stdin in that place among the `-e` scripts, so
  `ned -e 'file a.rs' -e - <<'EOF'` runs both.
- Written edits report what checks run on save (`cargo check`, with a daemon
  running) find they introduced, without blocking.

### Changed

- `!!` passes over read-only scripts that succeeded, repeating the last one that
  edited or failed.
- Syntax patterns are matched as abstract syntax trees, so separators and
  comments are skipped on both sides, while children still match in the same
  role (`[0; 4]` differs from `[0, 4]`). A pattern is parsed in place of the
  code its previous step selected, or alone for a whole file, replacing the
  per-language builders (`queries/<lang>/builders.scm`); a pattern for code that
  parses only inside a construct no step selects alone (a match arm, a `case`
  clause, a dict entry) no longer matches.
- A run (`@name...`) needs at least one sibling; `@name...?` and `@_...?` match
  zero or more.
- An edit that makes a formatter fail where it passed before is rejected unless
  `--force`, instead of being written with a note.

### Fixed

- The parse-error guard rejects a Rust `(…)` or `[…]` macro statement with no
  `;` before another statement, which tree-sitter accepts but rustc doesn't.
- Deleting and moving items keep the file's own blank-line spacing; moving the
  first top-level Python function no longer leaves a blank line at the top.
- Syntax patterns:
  - A placeholder alone in a block or list stands for its element, not the list.
  - A name repeated in a declaration and a use matches, through nodes that only
    wrap it.
  - A run left out of a pattern's text leaves out the separator after it.
  - A pattern ending in a comment takes its statement terminator before the
    comment.

## 0.7.2 (2026-10-05)

### Fixed

- A Rust `use` that spans lines is named in the one-line form rustfmt would
  print, without its comments, so `use c::{` then `d,` and `e,` on lines of
  their own is `import:"c::{d, e}"`.
- A regex, literal or pattern glued to a selector (`show fn:a/re/`) is an error
  suggesting `>` (`show fn:a>/re/`), in any command.
- A sed-style line range (`show 10,20`) is an error suggesting `10-20`.
- A subcommand after a flag (`ned -s x undo`) is an error suggesting it first,
  with the flags it takes (`ned undo -s x`), instead of reading it as a file.
- A match ending in a multibyte character (`show all "ï"`) ends on its own line:
  `show` no longer prints through the end of the file, and listing ambiguous
  candidates or `$` inside such a match no longer panics.

## 0.7.1 (2026-10-05)

### Added

- `file:GLOB` steps: `file:src/*.rs>fn:new` narrows a selector to the files in
  the set that the glob matches, with the `file` command's glob syntax. With
  `-w`, only those files are read.
- `.mdx` files are read as Markdown.

### Changed

- `show` with a line range alone (`show 1-200`) shows up to each file's last
  line when the range ends past it, with a note, instead of failing.

### Fixed

- An item that starts or ends inside a merge conflict, but isn't wholly on one
  of its sides, takes in the whole conflict, so `delete fn:f` leaves no marker
  line behind and `fn:f>conflict` finds it.
- `move` to the `start` or `end` of a body whose items are separated by blank
  lines separates the moved span from its neighbour with one too.
- A daemon socket path longer than the platform allows (103 bytes on macOS, 107
  on Linux) is an error naming its length, with a fix (a shorter
  `XDG_RUNTIME_DIR`), instead of "the daemon didn't start".

### Hints

- Quoting for a name with a `-` (`import:"react-router"`) or braces
  (`import:"a::b::{A, B}"`).
- A file path where a selector belongs (`src/a.rs>fn:x`) suggests a `file:`
  step.
- `show SEL -N` suggests `show SEL +N` (or, after a line number, the line range
  `show N-M`).
- sed's `sub /re/text/flags` suggests `sub /re/flags with "text"`.
- A literal in `sub` suggests the `replace all` it means.
- After a heredoc selector, the error shows the rest of the command on the
  selector's line: `replace <<END with TEXT`.
- A Markdown item's name with its checkbox (`item:"[x] done"`) suggests the name
  without it.
- A nested search that matches only across spans of the step before it suggests
  the selector without that step, or an `A..B` or line range in its place.
- A `conflict` step that matches nothing names a conflict that overlaps the
  searched span without lying inside it.
- A range that skips a start of `A` inside an earlier range notes that line,
  since that range likely spans more than meant.
- `file:GLOB` that matches no file in the set suggests the `file` command that
  adds its files, or, if none exist, correcting the glob.

## 0.7.0 (2026-10-04)

### Added

- Terminal output: when stdout is a terminal, `show` and diff hunks are
  syntax-highlighted from the grammars' highlight queries, `show` prints
  right-aligned line numbers, and `outline`, `check`, summaries and messages are
  coloured. `--color auto|always|never` and a non-empty `NO_COLOR` control it;
  piped output is unchanged.
- `--commit MSG` commits exactly the invocation's edits through git's plumbing:
  other staged and unstaged changes, even in the same file, stay uncommitted. In
  a session, the commit holds every edit since the session's last commit, undos
  included, and `ned history` shows it.
- Merge conflicts:
  - Conflict markers are hidden from every parse, so syntax selectors find items
    on both sides and the parse-error guard doesn't block edits to a conflicted
    file.
  - `conflict:N` selects a file's Nth conflict, in any file, with `.ours`,
    `.theirs` and, in diff3 style, `.base`; `outline` lists them. Text replacing
    a conflict or an empty side is re-based to its sides.
  - `resolve [all] SEL ours|theirs|base|both` keeps a side of each conflict.
  - `ned help conflicts` and `ned help resolve`.

### Changed

- `!!` after a `-n` dry run is an error unless it has `-n` too, since it would
  apply what was only previewed.

### Hints

- `all` after a selector or at the end of a command (`show /re/ all`,
  `replace /x/ with "y" all`) says it goes before the selector: `show all /re/`.
  In `sub`, which takes no `all`, it says to drop it; after a `move`
  destination, that the destination must be one span.

## 0.6.3 (2026-10-03)

### Changed

- `replace` with empty TEXT on whole lines removes them, as `delete` does,
  instead of leaving an empty line. `"\n"` still leaves one empty line.
- Syntax patterns: a run right after a node, where no name would parse, also
  matches the rest of a longer node that starts with that node, such as the tail
  of a method chain (`` `let n = items[i] @_...;` ``) or a match arm whose value
  is a chain.

### Fixed

- Formatting reads a `.ned.toml` that the same script creates or edits.
- A lone `\r` no longer ends a line in diff hunks, so their line numbers match
  `ned`'s `\n`-only ones.
- Re-basing:
  - A whole-line `replace` whose text dedents below its first line (`x;`, `}`,
    `fn g() {`) keeps that first line at the target's indentation, so the lines
    it dedents step out of the enclosing blocks.
  - `insert after` the last line of a statement split across lines takes the
    statement's indentation, not the continuation line's.
  - Inserting an item before another item's doc comment or attribute lines by
    line, regex or literal adds the blank line `insert before fn:x` adds.
- The parse-error guard points at the edit, not at the start of an `ERROR` node
  that can span the whole file.

### Hints

- The guard says `.sig` stops before the `{` (Rust, Go, JS/TS) as well as
  Python's `:`, when a `.sig` replacement ends with it.
- The guard says heredocs read no escapes when the rejected text holds one such
  as `\x27`, and to pass a script that holds a `'` on stdin.
- A second selector after `show`, `outline` or `delete` suggests one command
  each: `show fn:a; show fn:b`.
- A selector that matches only after the stage's earlier edits suggests a `|`
  before the command.
- The off-by-one note ignores whitespace and line breaks, so it also catches
  TEXT that repeats the rest of a line re-wrapped.
- The skill and agent guide say to give a subagent its own `-s NAME`, since it
  inherits `NED_SESSION`.

## 0.6.2 (2026-10-02)

### Added

- A `text` language: `--lang text` reads every file without parsing it. Line,
  regex and literal selectors work; syntax selectors, the parse-error guard,
  formatting and language servers skip the file.
- A run that reads files with unrecognised extensions prints one note naming
  them: `note: read .json, .toml files as text; syntax selectors skip them`.

## 0.6.1 (2026-10-02)

### Changed

- A string TEXT's final `\n` ends its last line rather than adding a blank one
  (heredocs are unchanged).
- A `$name` in `sub` TEXT for a group the regex lacks is an error, not an empty
  expansion.

### Fixed

- A `|` ends `show`, `outline` or `check` without a selector, so
  `outline | show 1` parses.
- The parse-error guard catches an emptied Python body (`class A:` with no
  methods left).
- rustfmt's `{edition}` reads a `Cargo.toml` that the same script creates or
  edits.
- Re-basing: the indent unit skips strings and comments and follows the file's
  dominant style (and is cached, making `all` edits on large files much faster);
  `insert start|end` into a blank body indents one unit; `insert after` a
  literal or regex match takes its last line's indentation.

### Hints

- `sub all /re/` says to drop `all`; `${1}x` for a group followed by text;
  doubling the backslash for an invalid escape; quoting or nesting for a dotted
  name; `insert after $` for `insert end` with no selector; `show SEL +M` for
  `+N..+M`.
- A nested selector that matches nothing names its first failing step, and a
  close name ignores generic arguments and paths (`impl:Log` for
  `impl:"Log<'_>"`).
- A rejected Python `.sig` replacement ending in `:` says `.sig` stops before
  the colon.

## 0.6.0 (2026-10-02)

### Added

- Sessions (`-s NAME` or `NED_SESSION`): every run is recorded in a
  per-workspace log.
  - `ned history` lists the session's recent entries.
  - `ned undo` reverts the last entry that wrote files; `--force` merges the
    undo into files changed since.
  - `!!` repeats the last script, with `:s/OLD/NEW/` and `:gs/OLD/NEW/` to
    correct it.

## 0.5.0 (2026-10-02)

### Added

- Syntax patterns: select code by writing it, with `@` placeholders (`@name`,
  `@name...`, `@_`), matched against the syntax tree so whitespace and comments
  needn't be reproduced. `replace` substitutes captures into TEXT. Fragments
  that only parse inside other code are tried inside per-language builders
  (`queries/<lang>/builders.scm`).
- `ned-scheme`, a reader for the Scheme dialect builders (and, later, plugins)
  are written in.

## 0.4.0 (2026-10-01)

Includes the untagged 0.2.0 and 0.3.0.

### Added

- Filters: `[...]` after a step keeps the spans for which a condition holds
  (`fn[.doc == ""]`, `fn:parse.lines[.len > 80]`), with `.text`, `.len` and
  parts as properties, comparisons, `~=` regex matches, `&&`, `||` and
  parentheses.
- Bare kinds: a kind alone selects every item of that kind (`show all fn`).
- `.whole`: an item with its doc comments, attributes and trailing `,`.
- Parts `.ret`, `.type`, `.value` and `.attrs`.

### Changed

- **Breaking:** `.lines` on a multi-line span selects each line separately; use
  `.whole` to replace an item with its docs, or a range for one multi-line span.
- **Breaking:** `A..B` ranges always cover whole lines.

## 0.1.1 (2026-10-01)

The first release, with Linux (musl) and macOS binaries.

- One-shot CLI: `ned FILE... -e SCRIPT` with `show`, `outline`, `replace`,
  `insert`, `delete`, `move`, `sub` and `create`; line, range, regex and literal
  selectors; and transactional, atomic writes printing minimal diffs.
- Syntax selectors and parts for Rust, Python, Go, JavaScript, TypeScript/TSX
  and Markdown.
- Indentation re-basing, a parse-error guard, and external formatters configured
  in `.ned.toml`.
- A per-workspace daemon that keeps language servers warm for `check`, `rename`,
  `.refs`/`.def` and checks on edits.
- Globbed file sets, `-w` for the whole workspace, and `ned help`.
