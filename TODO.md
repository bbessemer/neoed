# Neoed TODO

Each work item follows the TDD cycle in AGENTS.md. Items marked _(split)_ are
too large for one PR: they carry a checklist of coarse chunks to plan with the
engineer before starting each one; a single-PR item has none, and moves to Done
when it lands.

Versions follow semver at 0.x (currently 0.8.0): a change to the command
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
- **Syntax patterns** (0.5.0; reworked in 0.8.0): select code by writing it,
  with `@` placeholders; a pattern parses in place of the code its previous step
  selected and matches as an abstract syntax tree, skipping separators and
  comments; `replace` substitutes captures.
- **Sessions** (0.6.0): `-s NAME`/`NED_SESSION` record each invocation in a
  versioned, locked per-workspace log; `ned history`, `ned undo` (`--force`
  merges into later changes) and `!!:s/OLD/NEW/` repeats; `ned session list` and
  `delete` (0.8.0).
- **Text language** (0.6.2): `--lang text` reads every file without parsing;
  files with unknown extensions are read as text, with one note naming the
  extensions.
- **Terminal output** (0.7.0): `--color auto|always|never` and `NO_COLOR`; on a
  terminal, `show` and diff hunks are highlighted from the grammars' highlight
  queries, line numbers are right-aligned, and `outline`, `check` and messages
  are coloured. Piped output is unchanged. Themes (0.8.0) colour captures and
  tint changed lines on truecolor and 256-colour terminals.
- **Commit from ned** (0.7.0): `--commit MSG` commits exactly the invocation's
  edits (with `-s`, the session's since its last commit) through git's plumbing,
  leaving other staged and unstaged changes alone.
- **Merge conflicts** (0.7.0): conflict markers are hidden from every parse;
  `conflict:N` with `.ours`, `.theirs` and `.base`; `resolve` keeps a side.
- **REPL** (0.8.0): `ned repl` (and bare `ned` on a terminal) runs scripts on
  in-memory buffers with `:write` (merging into files changed since), `:undo`,
  `:diff` and `:commit`; records into a session; `:attach` follows an agent's
  session and records corrections into it. Written edits, in the CLI too, report
  what checks run on save (`cargo check`) find they introduced.
- **MCP server** (0.8.0): `ned mcp` serves the `ned`, `outline`, `show`, `help`,
  `history`, `undo` and `cd` tools over JSON-RPC on stdio, recording into a
  session; its protocol lives in `ned-mcp`, on `ned-core`'s shared `invoke`
  pipeline.
- **Agent ergonomics, phase 2** (0.7.x–0.8.0): `*:NAME`, `.lines:N`, `show raw`,
  `-e -`, `!!` passing over reads, and hints for sed-style and misplaced syntax.

## Phase 2

User-supplied grammars and plugins close the phase and may slip.

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

- [x] Replacing a braced `.body` (which leaves out the braces) with `TEXT` that
      starts with `{` and ends with the matching `}` nests a second pair: note
      it, and suggest dropping the braces from `TEXT` or replacing the whole
      item
- [ ] `impl:NAME` that names a trait matches nothing, and the hint suggests an
      item of the trait (`trait:NAME>fn:f`); suggest the trait's impls instead
      (`impl:"NAME for TYPE"`)
- [x] Replacing an item keeps its attributes, so `TEXT` that brings its own
      `#[derive(...)]` (or starts with another item, then the attributes) leaves
      them twice, which the guard misses until a later edit breaks the build:
      note it, and suggest `ITEM.whole`
- [ ] Two commands of a script that edit the same span (a `replace` of a match,
      then a `replace all` of the same regex) fail with a plain ambiguity or
      overlap error: say that the commands overlap, and suggest merging them or
      putting a `|` between them
- [ ] A `<<OLD` block selector whose last line is only part of a source line
      matches nothing, and the error says only that, ignoring case and spacing,
      it matches elsewhere: say that a block matches whole lines, and suggest
      the whole last line or a literal

## Bugs

Each fix is a patch; a fix that changes documented behaviour is a minor.

- [x] In Python, a syntax pattern of several statements matches at top level but
      not as a nested step: ``show fn:f>`a = 0\nb = 1` `` matches nothing where
      the statements are in `f`'s body (the Rust equivalent matches). It breaks
      the tutorial's pattern step (`tutorial.mdx`, the `fn:total>` `replace`),
      and the output after it.
- [ ] A part after a range's end applies to the whole range:
      `show /^const X/..fn:f.doc` fails with ".doc needs a syntax item, e.g.
      fn:NAME.doc", though `fn:f.doc` alone matches. Either bind the part to the
      end step or say that a part can't follow a range, with the fix.
- [x] The MCP server's `!!` error for a dry-run entry says "add -n to preview it
      again", the CLI's flag; it should name `dry_run`. Fix it as part of the
      move onto `hint`.
- [ ] The parse-error guard can locate an introduced error far from the edit
      that caused it: in a script whose later edit dropped a function's closing
      `}` (a nested range ending at `$`), the error named line 7, an unrelated
      `use` inserted by the script's first edit. Locate it within the edit whose
      change the error spans, or name that command.
- [x] A sed-style `sub SEL /re/text$1/`'s parse error suggests `with "…$$1"`,
      which inserts a literal `$1`, not group 1: the fix should keep `$1` (or
      write `${1}`), as `sub`'s TEXT expands it.
- [x] `sub` over whole lines matches an empty string after the span's last
      newline, at the start of the next line: `sub 1 /$/ with ";"` on `a\nb`
      gives `a;\n;b`, and `sub SEL /.*/ with "X"` writes a second `X` there.
      Matches should end at or before the span's last newline.

### Won't fix

- Conflict-shaped text in a Markdown code fence is a merge conflict (§3.11, as
  git counts it), so formatting skips the file: `tutorial.mdx` shows conflict
  markers in its conflicts section, so it is never formatted.

## Future improvements

Re-basing changes are patches, since the spec leaves their details open; the `|`
change is a minor, because it lifts a documented error.

- [ ] `check`, `rename`, `.refs` and `.def` after a `|`: send the daemon each
      changed file's stage text instead of relying on the files on disk, and
      lift the syntax error
- [ ] Compact session logs, which keep each written file's whole text before and
      after: diffs against the previous entry, or pruning old entries (a log
      format change, so a minor)
- [ ] TOML syntax selectors (`table`, `key`), for `pyproject.toml`, `Cargo.toml`
      and lock files (a new language, so a minor)
- [ ] Relative range ends: `/re/..+70` is the match and the 70 lines after it (a
      minor)
- [ ] Patterns for code that parses only inside a construct no step selects
      alone: match arms, `case` clauses, dict and object entries, a decorator
      without its definition (lost with `builders.scm`). A minor, since §3.10
      changes
- [ ] A `FILE` argument (MCP `files`) that the script `create`s joins the set
      when it's made, instead of being an error at once (a minor: lifts a
      documented error; 0.8.1 only names the fix)
- [ ] With no files given, a script whose selectors all start with `file:PATH`
      steps runs on those files (a minor: `file:` never adds to the set today;
      0.8.1 names the `file` command)
- [ ] Several selectors in one `show`: `show 31-35, 118-124` (a minor; 0.8.1
      suggests `show 31-35; show 118-124`)
- [ ] `undo FILE...` reverts only those files of the last edit, so one bad file
      of a multi-file call needn't undo the rest: undo entries record the paths
      they revert, and `history` marks an entry partly undone (a minor, with a
      session log change)
- [ ] Go formatting runs goimports when it's installed, then gofmt, adding the
      imports an edit needs; goimports also drops unused ones, so an import
      added before its first use would go (a minor: the documented default)
- [ ] Keep only the formatter's changes that touch the edit, for formatters that
      rewrite the whole file (prettier rewriting an unrelated YAML example);
      opt-in per formatter or flag. 0.8.1 notes the lines outside the edit (a
      minor)
- [ ] The session log records an invocation's flags, so `!!` can keep the
      output-affecting ones (`--no-fmt`, MCP `no_fmt`) or name those it drops (a
      minor: a log field; 0.8.1 notes a repeat runs without flags)
- [ ] `A..B` where `B`'s span holds `A`'s end, as an item whose doc comment `A`
      matched: the range runs from `A` to `B`'s end, rather than matching
      nothing (a minor: §3.7 says `B` starts after `A`; 0.8.1 explains it)
- [ ] Single-quoted strings, SQL-style: `'text'` reads no escapes, and `''`
      stands for one `'`, so text full of `"` or `\` needs no escaping (a minor:
      new syntax; today `'` is an error saying strings use double quotes)
