# Neoed TODO

Each work item follows the TDD cycle in AGENTS.md. Items marked _(split)_ are
too large for one PR: they carry a checklist of coarse chunks to plan with the
engineer before starting each one; a single-PR item has none, and moves to Done
when it lands.

Versions follow semver at 0.x (currently 0.7.0): a change to the command
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
- **Syntax patterns** (0.5.0): select code by writing it, with `@` placeholders
  matched against the syntax tree; fragments parse inside per-language builders
  (`queries/<lang>/builders.scm`, read by `ned-scheme`); `replace` substitutes
  captures.
- **Sessions** (0.6.0): `-s NAME`/`NED_SESSION` record each invocation in a
  versioned, locked per-workspace log; `ned history`, `ned undo` (`--force`
  merges into later changes) and `!!:s/OLD/NEW/` repeats.
- **Text language** (0.6.2): `--lang text` reads every file without parsing;
  files with unknown extensions are read as text, with one note naming the
  extensions.
- **Terminal output** (0.7.0): `--color auto|always|never` and `NO_COLOR`; on a
  terminal, `show` and diff hunks are highlighted from the grammars' highlight
  queries, line numbers are right-aligned, and `outline`, `check` and messages
  are coloured. Piped output is unchanged.
- **Commit from ned** (0.7.0): `--commit MSG` commits exactly the invocation's
  edits (with `-s`, the session's since its last commit) through git's plumbing,
  leaving other staged and unstaged changes alone.
- **Merge conflicts** (0.7.0): conflict markers are hidden from every parse;
  `conflict:N` with `.ours`, `.theirs` and `.base`; `resolve` keeps a side.

## Phase 2

The REPL and the MCP server read sessions, and the REPL builds on terminal
output, both done. User-supplied grammars and plugins close the phase and may
slip.

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

- [ ] `show` with a line range past the end of the file shows up to the last
      line, with a note, instead of an error (`show 1-60` on a 57-line file);
      edits keep the error
- [ ] `-e` plus a script on stdin runs both: the `-e` scripts first, then stdin,
      joined with newlines as several `-e`s are (today stdin is ignored
      silently, so `ned -e 'file X' <<'EOF' ... EOF` drops the heredoc). A
      minor, since §1 changes. Decide how not to wait on an open pipe that never
      closes, which `-e` alone doesn't read today
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

## Bugs

Each fix is a patch; a fix that changes documented behaviour is a minor.

- [x] `.lines` on a multi-line literal that matches once
      (`insert after "- a b\n  c d".lines "x"`) says "matches 2 items" and lists
      identical candidates
- [ ] An item whose last line falls inside a conflict's last side stops before
      the conflict's `>>>>>>>` line, so `fn:f>conflict` matches nothing; the
      error lists the file's conflicts without saying they lie outside `fn:f`
- [ ] An unquoted name with a `-` (`import:react-router`) is reported as a
      malformed range (`ranges between selectors are written SEL..SEL`) instead
      of suggesting quotes (`import:"react-router"`)
- [ ] `item:"[ ] text*"` matches nothing without suggesting the name without its
      task-list checkbox (`item:"text*"`), which is how items are named

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
