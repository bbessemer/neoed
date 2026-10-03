# Neoed TODO

Each work item follows the TDD cycle in AGENTS.md. Items marked _(split)_ are
too large for one PR: they carry a checklist of coarse chunks to plan with the
engineer before starting each one; a single-PR item has none, and moves to Done
when it lands.

Versions follow semver at 0.x (currently 0.6.2): a change to the command
language or any new user-visible feature bumps the minor version, and a release
that only fixes bugs or adds hints bumps the patch version. Each item below says
which it is. When to release 1.0 is TBD.

## Done

- **Bootstrap**: Rust workspace (`ned-core`, `ned-cli`), AGENTS.md, this plan.
- **Command-language spec**: `docs/command-language.md`, signed off; tests are
  written against it.
- **Buffer and transactions**: rope buffer, overlap-checked edit set, atomic
  multi-file write, diff rendering.
- **Script parser**: lexer, AST, errors with a caret excerpt and a fix.
- **Line-based editing**: line, range, regex and literal selectors; `show`,
  `replace`, `insert`, `delete`, `sub`; re-basing; CLI wiring; E2E snapshots.
- **Tree-sitter integration**: language detection,
  `queries/<lang>/selectors.scm` for Rust, Python, Go, JavaScript,
  TypeScript/TSX and Markdown, syntax selectors and parts, `outline`, the
  parse-error guard, raw `query{}`.
- **Formatting**: `.ned.toml` over user config, per-language formatters,
  formatter changes reported apart from the agent's edits.
- **Multi-file and advanced edits**: globs, per-file scoping, `move` across
  files, `--dry-run`.
- **LSP daemon**: per-workspace daemon, `check` (including save-time checks),
  edit checking, `rename`, `.refs`/`.def`, LSP formatting fallback.
- **Agent ergonomics, phase 1**: `ned help`, agent guide and skill, token
  benchmark, ranges, `show +N`, `create`, `|` chaining, and the error-message
  review (every error suggests a fix). Open items are below.
- **Release groundwork**: install instructions, MIT license.
- **CI and release binaries**: build, test, clippy and fmt on Linux and macOS;
  PRs to `main` must raise the version, and merging one tags it and publishes
  binaries.
- **Selector filters and more parts**: `.attrs`, `.ret`, `.type`, `.value` and
  `.whole`; bare kinds (`fn` = `fn:*`); `.lines` splitting into lines;
  whole-line ranges; span types; `[...]` filters with `&&`, `||` and
  parentheses.
- **Sessions** (0.6.0): `-s NAME`/`NED_SESSION` record each invocation in a
  versioned, locked per-workspace log; `ned history`, `ned undo` (`--force`
  merges into later changes) and `!!:s/OLD/NEW/` repeats.
- **Text language**: `--lang text` reads every file without parsing; files with
  unknown extensions are read as text, with one note naming the extensions.

## Phase 2

Order matters: sessions precede everything that reads a session (commit, REPL,
MCP); terminal output precedes the REPL. User-supplied grammars and plugins
close the phase and may slip.

### Syntax patterns _(split)_

An agent selects code by writing code: a backquoted pattern such as
`` fn:parse>`if name == "foo" { @body... }` `` is parsed with the file's grammar
and matched against the syntax tree, so whitespace, line breaks and comments
never have to be reproduced (spec §3.10). `@name` matches one node and
`@name...` a run of siblings; `replace` substitutes what they captured. Prior
art: ast-grep's metavariables.

- **Direct tree matching**, not compilation to queries: query anchors can't skip
  comments between siblings, and unanchored children allow gaps.
- **Fragments parse inside builders.** tree-sitter has no alternate start
  symbol, so a fragment that only parses inside other code (a method, an arm, a
  field) is tried inside each of its language's builders:
  `queries/<lang>/builders.scm`, Scheme data read by `ned-scheme` and never
  evaluated. `(build KIND PART...)` gives the text of KIND: strings are literal,
  `_` is the hole for the fragment, and other symbols are KIND's fields, filled
  with dummy names here and with real text by code generation later. What each
  hole can contain comes from the grammar's `node-types.json`, not from
  hand-written data. A test keeps only builders some fragment needs: the
  grammars accept most code at the top level (Rust items, Go statements).
- **Generation is quasi-quotation, not unparsing.** tree-sitter can't turn a
  tree back into source, and an unparser driven by `grammar.json` would be
  unreliable (external scanners, alternatives). Text comes from templates,
  captured source and re-basing, plus the formatter; trees only validate it.
  Plugins build on the same templates and builders.

- [x] Spec: placeholder grammar, capture rules, substitution in `replace`
- [x] `ned-scheme`: a datum reader (no evaluation) for a Scheme dialect that
      reads tree-sitter query syntax, producing syntax objects with spans
- [x] Fragment parsing with per-language builders
      (`queries/<lang>/builders.scm`) and containment from `node-types.json`
- [x] Pattern matching
- [x] Capture substitution in `replace` TEXT

Version: minor once matching works; a further minor if capture substitution
ships separately. The spec, reader and builder chunks alone release nothing.

### Commit from ned

An agent turns its edits into one git commit without touching anything else in
the working tree: `--commit MSG` commits exactly the invocation's edits, and
with `-s` the session's edits so far. `ned` patches the index directly rather
than staging paths, so other staged or unstaged changes are never swept into the
commit. Depends on Sessions for the session case.

Version: minor; a new flag.

### Merge conflicts

A file with `<<<<<<<`/`=======`/`>>>>>>>` markers still parses: markers are
hidden from the grammar, so syntax selectors find items inside either side and
the parse-error guard doesn't block edits to a conflicted file. Then an agent
resolves conflicts structurally: a `conflict` kind (numbered in file order) with
`.ours`, `.theirs` and `.base` parts, and `resolve conflict:2 ours` (or
`replace conflict:2 with ...`) replaces the whole conflict with one side or new
text. The marker tolerance is independent; the resolution verb depends on the
parts work above.

Version: patch for marker tolerance (existing scripts start working on
conflicted files); minor for the `conflict` kind, its parts and `resolve`.

### Terminal output

When stdout is a terminal, `ned` formats for a human: syntax-highlighted code
(from the grammars' highlight queries), right-aligned line numbers delimited
from code by colour instead of `:`, coloured diffs. `--color auto|always|never`
and `NO_COLOR` control it. Output to a pipe or file is unchanged, so agents keep
the terse form that §6 of the spec defines. Prerequisite to the REPL.

Version: minor; `--color` is new and terminal output changes, though piped
output doesn't.

### REPL

`ned-repl`: a human edits interactively with persistent buffers, undo and an
explicit write, using the same command language, and can attach to an agent's
session to watch and correct its work. It records into a session automatically.
Depends on Sessions and Terminal output.

Version: minor; a new binary.

### MCP server

`ned-mcp`: an MCP server exposes script execution, `outline` and `show` as
tools, so agent frameworks call `ned` without a shell. It is a thin client of
`ned-core` and the session store, with no logic of its own, and records into a
session automatically. Depends on Sessions.

Version: minor; a new binary.

### User-supplied grammars _(split)_

A user adds a language without rebuilding `ned`: `[languages.NAME]` in
`.ned.toml` names a compiled tree-sitter grammar library, its file extensions
and a directory of query files (`selectors.scm`, `highlights.scm`). The closed
`Language` enum becomes an open registry that syntax selectors, `outline`,
formatting, LSP config and highlighting all key on. This needs no plugin system:
a grammar plus queries is data, as the built-in languages are.

- [ ] Language registry replacing the enum
- [ ] Dynamic grammar loading and query-file lookup
- [ ] Guide to writing selector and highlight queries

Version: minor when loading works; the registry refactor alone is a patch if
released on its own, since nothing visible changes.

### Plugins

Later. A Scheme extending tree-sitter's query syntax, embedded with Steel or a
hand-rolled R5RS, for new languages whose items need logic, custom commands,
complex tree manipulations and procedural code generation: a language-generic
but syntax-aware macro system. Not for external processes, I/O outside the
editing core, or Emacs-style scope creep; `ned` stays a focused tool. First step
is a spike comparing Steel with a hand-rolled evaluator on one real use case;
`ned-scheme`'s reader (from syntax patterns) is the front end either way. It
reads tree-sitter query syntax natively (`@capture`, `[...]` alternation, `#eq?`
predicates, `.` anchors, so no dotted pairs), which counts toward a hand-rolled
evaluator unless Steel's reader can be made to read it too (to check in the
spike).

Version: minor when a first plugin can load; the spike releases nothing.

## Agent ergonomics

Hints and relaxed errors: each is a patch, and they batch into the next release
of either kind.

- [x] A `sub` replacement that names a group its regex doesn't have is an error,
      not an empty expansion: `$1deletions` is the group `1deletions`, so
      suggest `${1}deletions` (or `$$` for a literal `$`)
- [x] An invalid escape in a string suggests doubling the backslash, for text
      copied from source: "invalid escape `\r`; write `\\r` for a backslash and
      r"
- [ ] `show` with a line range past the end of the file shows up to the last
      line, with a note, instead of an error (`show 1-60` on a 57-line file);
      edits keep the error
- [x] `sub all /re/ with "x"` says "`sub` needs a regex before `with`", since
      `all /re/` parses as the scope; say instead that `sub` already replaces
      every match, so `all` goes: `sub /re/ with "x"`
- [ ] `-e` plus a script on stdin runs both: the `-e` scripts first, then stdin,
      joined with newlines as several `-e`s are (today stdin is ignored
      silently, so `ned -e 'file X' <<'EOF' ... EOF` drops the heredoc). A
      minor, since §1 changes. Decide how not to wait on an open pipe that never
      closes, which `-e` alone doesn't read today
- [x] A dotted name whose tail isn't a part suggests quoting it:
      `import:app.models.user` says "unknown part `.models`" and should suggest
      `import:"app.models.user"`
- [x] `insert end` or `insert start` with no selector says it needs one, and
      that `insert after $` appends to the file, instead of "expected text (a
      string or heredoc), found end of line"
- [x] A `..` followed by `+N` (`show /re/+0..+70`) suggests `show /re/ +70`
      rather than "expected end of command, found '..'"
- [x] A Python `.sig` replacement ending in `:` that the guard rejects
      (`-> None::`) says `.sig` stops before the `:`; spec §3.4,
      `ned help selectors` and the skill say so too
- [x] `ned help selectors` says `..` binds tighter than `>`, with the example
      `class:Server>fn:start..fn:run` (not `...fn:start..class:Server>fn:run`)
- [x] The skill and agent guide say to use a heredoc whenever TEXT holds a
      quote, rather than `-e` with shell escapes such as `'"'"'`
- [ ] An any-kind selector matches a name whatever its kind, when that is
      unique, so a long script needn't guess `const:` versus `var:` (syntax to
      decide: `item:NAME`, `*:NAME` or a bare name). A minor
- [ ] `show --raw` prints the selected lines without `N:` prefixes, for copying
      text verbatim. A minor
- [ ] A sed-style `sub SEL /a/b/` says "`sub` takes `/re/ with TEXT`", instead
      of "unknown regex flag" or a hint about `SEL..SEL` ranges
- [ ] `sub` with a literal before `with` (`sub 3 "- [ ]" with "- [x]"`) suggests
      `replace 3>"- [ ]" with "- [x]"`, since `sub` takes only a regex
- [ ] Context written `-N` (`show all "x" -3`) says context is `+N`, not a hint
      about `SEL..SEL` ranges
- [ ] A file path as a selector step (`a.rs>fn:x`) suggests `file:a.rs>fn:x`
      rather than quoting `a` as a literal
- [ ] A name with `{` (`import:a::b::{A, B}`) suggests the quoted name `outline`
      prints (`import:"a::b::{A, B}"`), not "unexpected character `{`"
- [ ] A part or filter picks the Nth line of a multi-line match, since `.lines`
      splits a match into every line and there is no `.lines.first` (syntax to
      decide). A minor
- [ ] `!!` repeats the last script that edited or failed, not a read-only call
      in between: after a failed edit, an `outline` to look around makes `!!`
      refer to the `outline`. A minor, since §1.2 changes
- [ ] Several selectors after one `show` (`show fn:a fn:b`) suggest a `;` or a
      new line between `show`s, instead of only "expected end of command"
- [ ] A `-n` dry run's output says `!!` applies it: an agent that runs `!!`
      after `-n` and then sends the script again applies the edit twice
- [ ] A selector that matches nothing, where an earlier command in the script
      inserts matching text, says selectors resolve against the stage's input
      and suggests a `|` before the command
- [x] The off-by-one note fires when TEXT repeats the rest of the span's last
      line wrapped across a line break: compare ignoring whitespace and line
      breaks, not just whether TEXT ends with that rest
- [x] The skill says a subagent inherits its parent's `NED_SESSION`, so its
      `ned undo` can revert the parent's edits; give a subagent its own `-s`

## Bugs

Each fix is a patch; a fix that changes documented behaviour is a minor.

- [x] `create a.rs "fn a() {}\n"` followed by `insert after fn:a ...` in the
      same script leaves a trailing blank line (rustfmt removes it)
- [x] The did-you-mean-another-kind hint only fires for a selector's last step:
      `show fn:tests>fn:exec` says "`outline` lists the items" where
      `show fn:tests` suggests `mod:tests`
- [x] `show`, `outline` or `check` without a selector can't come before a `|`:
      `outline | show 1` is a parse error ("expected a selector, found '|'"),
      because `optional_target` (`script/parser.rs`) doesn't treat `|` as the
      end of the command
- [x] Re-basing a `<<END` heredoc into `crates/ned-core/src/syntax.rs` indented
      its nested lines with tabs, though the file's Rust code is indented with
      spaces (rustfmt fixed it). The indent unit seems to come from tab-indented
      lines elsewhere in the file (the Go test fixtures in raw strings), not
      from the lines around the target
- [x] `insert end mod:tests <<END` in a Rust file put the text at column 0, not
      at the module body's indentation (`insert end fn:...` re-bases correctly)
- [x] `insert after "LINE1\n...LASTLINE" <<END` re-based the text to the
      literal's first line's indentation, not its last line's, though the text
      goes after the last line
- [x] rustfmt formats a `.rs` file with edition 2015 when the script also
      `create`s its crate's `Cargo.toml`: `rust_edition` (`format.rs`) reads
      manifests from disk, not the script's created files, so 2024-style code is
      reformatted (`if c { a } else { b }` split over five lines)
- [x] A nested selector whose earlier step matches nothing reports the whole
      selector with the last step's hint: `impl:"Lexer<'a>">fn:next>"x"` says
      "matches nothing; `show` prints the text to match against" instead of
      naming the failing step and suggesting `impl:Lexer`
- [x] The parse-error guard misses a Python class left with no body (`delete` of
      its only methods): tree-sitter-python parses `class A:` at the end of a
      file without an error node; only ruff reports it
- [x] `impl:"Log<'_>"` matches nothing in a file with `impl Log<'_>`, and the
      hint is only "`outline` lists the items": suggest `impl:Log`, the name
      without its generic arguments
- [x] A `.ned.toml` the script creates or edits is ignored: `Config::layers`
      (`config.rs`) loads layers from disk, though `{edition}` now reads
      manifests the script writes
- [x] `replace "LINE\n" with ""` leaves an empty line where the whole line was
      selected; empty TEXT for whole lines should remove them, as `delete` does
- [ ] `.lines` on a multi-line literal that matches once
      (`insert after "- a b\n  c d".lines "x"`) says "matches 2 items" and lists
      identical candidates
- [x] Rust `show fn:f.sig` prints the whole first line, `{` included, though
      `.sig` ends before the `{`: `replace fn:f.sig with "fn f(b: u8) {"`
      doubles the brace, and the guard's error points at 1:1, not at the edit
- [x] An escape such as `\x27` in a heredoc inside a single-quoted `-e` script
      goes in literally (heredocs don't read escapes): the skill should say to
      pass a script with `'` on stdin, or `ned` could hint at it when the guard
      rejects text holding `\x27`
- [x] With a lone `\r` line ending, diff hunk headers count it as a line break
      (`similar` splits lines there), so their line numbers disagree with ned's
      `\n`-only ones
- [x] A syntax pattern can't match the tail of a method chain after a receiver
      (`` `let n = files[i] @_...;` ``), nor a match arm whose value is a chain
      (`` `Primary::Conflict(_) => f @_...,` ``)

## Future improvements

Re-basing changes are patches, since the spec leaves their details open; the `|`
change is a minor, because it lifts a documented error.

- [ ] Smarter indent conversion in re-basing: normalize space widths (e.g.
      2-space text into a 4-space file), detect alignment (continuation lines
      aligned to a delimiter rather than indented by levels) and preserve it
- [ ] Re-basing keeps block-quote prefixes (`> `): inserted lines take the
      target line's `>` markers, not just its whitespace
- [ ] Re-basing follows the text's own dedent: `replace LINE with` text that
      closes the enclosing block and starts a top-level item (`}` then `fn g()`)
      keeps the item at column 0, not the replaced line's indentation
- [ ] `insert after N` where line N continues a statement (`.collect();`)
      indents to the statement's first line, not the continuation's deeper
      indent
- [ ] `insert before` an item's first line (its doc comment or attributes) keeps
      a blank line between the inserted item and the next, as
      `insert before fn:x` does
- [ ] `check`, `rename`, `.refs` and `.def` after a `|`: send the daemon each
      changed file's stage text instead of relying on the files on disk, and
      lift the syntax error
- [ ] Re-basing keeps a Markdown list item's hanging indent for verbatim text: a
      multi-line string replacing part of an item gives its later lines the
      item's continuation indent, as line-oriented text gets (§5.2), not the
      indentation of the line the span starts on (column 0 for a top-level item)
- [ ] Compact session logs, which keep each written file's whole text before and
      after: diffs against the previous entry, or pruning old entries (a log
      format change, so a minor)
- [ ] TOML syntax selectors (`table`, `key`), for `pyproject.toml`, `Cargo.toml`
      and lock files (a new language, so a minor)
- [ ] Relative range ends: `/re/..+70` is the match and the 70 lines after it (a
      minor)
