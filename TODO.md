# Neoed TODO

Each work item follows the TDD cycle in AGENTS.md. Items marked _(split)_ are
too large for one PR: they carry a checklist of coarse chunks to plan with the
engineer before starting each one; a single-PR item has none, and moves to Done
when it lands.

Versions follow semver at 0.x (currently 0.4.0): a change to the command
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
  hand-written data.
- **Generation is quasi-quotation, not unparsing.** tree-sitter can't turn a
  tree back into source, and an unparser driven by `grammar.json` would be
  unreliable (external scanners, alternatives). Text comes from templates,
  captured source and re-basing, plus the formatter; trees only validate it.
  Plugins build on the same templates and builders.

- [x] Spec: placeholder grammar, capture rules, substitution in `replace`
- [x] `ned-scheme`: a datum reader (no evaluation) for a Scheme dialect that
      reads tree-sitter query syntax, producing syntax objects with spans
- [ ] Fragment parsing with per-language builders
      (`queries/<lang>/builders.scm`) and containment from `node-types.json`
- [ ] Pattern matching
- [ ] Capture substitution in `replace` TEXT

Version: minor once matching works; a further minor if capture substitution
ships separately. The spec, reader and builder chunks alone release nothing.

### Sessions _(split)_

With `-s, --session [NAME]` or `NED_SESSION`, `ned` records each invocation
(script, file set, outcome and per-file edits) in an append-only log under a
per-workspace state directory, guarded by an advisory lock so several frontends
can share it. An agent repeats its last command with a correction using a
shell-style `!!` shorthand instead of resending the script, and undoes the last
invocation's edits. The REPL and MCP server use a session automatically, and the
same session is visible from every mode, so a human can follow an agent's
progress from the REPL. The store is a `ned-core` module and needs no daemon;
the CLI keeps working without one.

- [ ] Store and log format, lock, per-workspace location
- [ ] `-s`/`NED_SESSION`, `history` and `undo`
- [ ] Repeat-with-correction shorthand

Version: minor for the flag and store; the log format is versioned, and a format
change before 1.0 is another minor. The shorthand is a minor if it ships after.

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
session to watch and correct its work. Depends on Sessions and Terminal output.

Version: minor; a new binary.

### MCP server

`ned-mcp`: an MCP server exposes script execution, `outline` and `show` as
tools, so agent frameworks call `ned` without a shell. It is a thin client of
`ned-core` and the session store, with no logic of its own. Depends on Sessions.

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

- [ ] A `sub` replacement that names a group its regex doesn't have is an error,
      not an empty expansion: `$1deletions` is the group `1deletions`, so
      suggest `${1}deletions` (or `$$` for a literal `$`)
- [ ] An invalid escape in a string suggests doubling the backslash, for text
      copied from source: "invalid escape `\r`; write `\\r` for a backslash and
      r"
- [ ] `show` with a line range past the end of the file shows up to the last
      line, with a note, instead of an error (`show 1-60` on a 57-line file);
      edits keep the error
- [ ] `sub all /re/ with "x"` says "`sub` needs a regex before `with`", since
      `all /re/` parses as the scope; say instead that `sub` already replaces
      every match, so `all` goes: `sub /re/ with "x"`

## Bugs

Each fix is a patch; a fix that changes documented behaviour is a minor.

- [ ] `create a.rs "fn a() {}\n"` followed by `insert after fn:a ...` in the
      same script leaves a trailing blank line (rustfmt removes it)
- [ ] The did-you-mean-another-kind hint only fires for a selector's last step:
      `show fn:tests>fn:exec` says "`outline` lists the items" where
      `show fn:tests` suggests `mod:tests`
- [ ] `show`, `outline` or `check` without a selector can't come before a `|`:
      `outline | show 1` is a parse error ("expected a selector, found '|'"),
      because `optional_target` (`script/parser.rs`) doesn't treat `|` as the
      end of the command
- [ ] Re-basing a `<<END` heredoc into `crates/ned-core/src/syntax.rs` indented
      its nested lines with tabs, though the file's Rust code is indented with
      spaces (rustfmt fixed it). The indent unit seems to come from tab-indented
      lines elsewhere in the file (the Go test fixtures in raw strings), not
      from the lines around the target
- [ ] `insert end mod:tests <<END` in a Rust file put the text at column 0, not
      at the module body's indentation (`insert end fn:...` re-bases correctly)
- [ ] `insert after "LINE1\n...LASTLINE" <<END` re-based the text to the
      literal's first line's indentation, not its last line's, though the text
      goes after the last line

## Future improvements

Re-basing changes are patches, since the spec leaves their details open; the `|`
change is a minor, because it lifts a documented error.

- [ ] Smarter indent conversion in re-basing: normalize space widths (e.g.
      2-space text into a 4-space file), detect alignment (continuation lines
      aligned to a delimiter rather than indented by levels) and preserve it
- [ ] Re-basing keeps block-quote prefixes (`> `): inserted lines take the
      target line's `>` markers, not just its whitespace
- [ ] `check`, `rename`, `.refs` and `.def` after a `|`: send the daemon each
      changed file's stage text instead of relying on the files on disk, and
      lift the syntax error
