# Changelog

Every tagged release of `ned`. Before 0.6.1, every PR to `main` bumped the
version, so versions merged in quick succession (0.2.0, 0.3.0) were never tagged
or released on their own; their changes are listed under the next tag.

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
