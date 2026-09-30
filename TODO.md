# Neoed TODO

Each milestone follows the TDD cycle in AGENTS.md. Milestones marked _(split)_
are too large for one PR; plan sub-tasks with the engineer before starting.

## 0. Bootstrap

- [x] Choose tech stack (Rust, tree-sitter, external formatters, LSP daemon later)
- [x] Cargo workspace: `ned-core` (lib), `ned-cli` (bin `ned`), core deps
- [x] Fill in AGENTS.md; write this plan

## 1. Command-language spec

- [x] `docs/command-language.md`: invocation (`-e`, stdin, file args), script
      grammar (EBNF), comments, quoting/escaping
- [x] Selectors: lines/ranges/`$`, regex, literal, syntax kinds, nesting (`>`),
      parts (`.body`, `.sig`, `.params`, `.doc`), `all`, raw query escape hatch
- [x] Verbs: `show`, `outline`, `replace`, `insert before|after`, `delete`,
      `sub`, `move`; reserved: `rename`, `check`, `refs:`
- [x] Text blocks (heredoc) and indentation re-basing rules
- [x] Output format (summary, diff hunks, `show` line numbering), errors,
      exit codes, `--dry-run`/`--quiet`/`--force`/`--no-fmt`
- [x] Worked examples comparing token cost against `sed`/Python equivalents
- [x] Engineer review and sign-off

## 2. Buffer and transactions

- [x] Rope-backed buffer with byte/line/point conversions
- [x] Edit set: collect edits against original coordinates, reject overlaps,
      apply in one pass
- [x] Atomic file write (temp file + rename, preserve permissions and line endings)
- [x] Diff summary rendering (`similar`)

## 3. Script parser

- [x] Lexer with source positions
- [x] Parser to AST (commands, selectors, text blocks)
- [x] Error messages with line/column and a caret excerpt

## 4. Line-based editing, end to end

- [x] Line/range/regex/literal selector resolution
- [x] Verbs: `show`, `replace`, `insert`, `delete`, `sub`
- [x] Indentation re-basing for inserted/replaced blocks
- [x] CLI wiring: args, stdin script, multiple files, exit codes
- [x] E2E snapshot tests in `crates/ned-cli/tests/`

## 5. Tree-sitter integration _(split)_

- [x] Language detection (extension, shebang, `--lang`) and grammar registry
- [x] `queries/rust/selectors.scm` (other languages: §12)
- [x] Syntax selectors and nesting; ambiguity errors with candidates
- [x] Parts (`.body`, `.sig`, `.params`, `.name`, `.doc`); `insert start|end`
      on an item implies `.body`
- [x] `outline` verb (compact symbol tree with line numbers)
- [x] Parse-error guard (reject new ERROR/MISSING nodes unless `--force`)
- [x] Go tab default for re-basing
- [x] Re-basing empty spans (enclosing item's indent)
- [x] Raw tree-sitter query selector

## 6. Formatting

- [x] Config: `.ned.toml` (walk up from file) merged over user config
- [x] Per-language formatter commands with sensible defaults (rustfmt, gofmt,
      black/ruff, prettier)
- [x] Run after edits; `--no-fmt`; skip with note when formatter is missing
- [x] Report formatter-introduced changes separately from the agent's edits

## 7. Multi-file and advanced edits

- [x] Globs and multiple files per script; per-file selector scoping
- [x] `move SEL to before|after SEL` (within and across files)
- [x] `--dry-run` diff-only mode

## 8. Agent ergonomics

- [x] Concise `ned help` / `ned help VERB` sized for agent context windows
- [x] Agent usage guide (skill/system-prompt snippet) in `docs/`
- [x] Token-cost benchmark suite vs `sed`/Python on representative edits
- [x] Error-message review: every error suggests a corrected command
- [x] Range selectors `SEL..SEL` (`/^## 6/../^## 7/`, `fn:a..fn:c`): from the
      start of the first match to the end of the second, searched after it;
      whole lines if both ends are whole-line. The error for `/a/-/b/` suggests
      `..`
- [x] `show SEL +N`: N lines of context around each span; regions merge as now
- [x] Markdown as a language: tree-sitter-md grammar; kinds `section` (a
      heading and its content, named by the heading text), `item`, `table`,
      `code`; `outline` as the heading tree; prettier as the default formatter
      (realigns tables)
- [x] `create PATH TEXT`: creates a file (an error if it exists) as part of the
      transaction, with its language detected from the path, and adds it to
      the file set
- [x] `insert before|after` a syntax item that is blank-separated from its
      neighbours adds one separating blank line, as `move` does, unless the
      text already starts or ends with one
- [x] `insert before|after` with heredoc TEXT widens a partial-line target to
      its whole lines, as if `.lines` were given (string TEXT stays verbatim),
      so `insert after /re/ <<END` can't land mid-line
- [x] Name trait impls `TRAIT for TYPE` too: `impl:"Display for Language"`
      picks one impl, while `impl:Language` still matches every impl of the
      type
- [x] Markdown sections get a `.body`: the content after the heading line, so
      `insert start|end section:X` and `replace section:X.body` work
- [x] `replace ITEM with TEXT` keeps the item's attributes and doc comments
      (`#[test]`, `///`) unless TEXT starts with its own, so replacing a test
      function can't silently drop `#[test]`
- [x] `insert before ITEM` with TEXT that is only attributes or doc comments
      adds no separating blank line: the text attaches to the item
- [x] A syntax step that matches nothing suggests the same name under another
      kind first (`struct:LspError` → "did you mean enum:LspError?"), before
      the close-name hint
- [ ] Explicit chaining with `|`: `CMD | CMD` runs the right command against
      the text as the left one left it (selectors see its additions, renames
      and moves), while `;` and newlines keep snapshot semantics (§2.3). The
      script stays one transaction. It avoids a second ned call for, e.g.,
      `create`, `move` or `rename` followed by an edit that selects the result
- [ ] A FILE argument that doesn't exist but names a verb, as in
      `ned outline src/a.rs`, is an error suggesting
      `ned src/a.rs -e outline`, instead of waiting for a script on stdin
- [ ] Searching with `show all /re/` over a glob or `-w`: no match is a normal
      answer, so say `no matches` (exit 0) instead of an error
      listing the files searched with a hint to `show` them
- [x] A `replace` whose TEXT starts with a copy of the line just above its
      span, or ends with a copy of the line just below, prints a note naming
      the duplicated line (the range was probably off by one)
- [x] A regex, literal or heredoc lies inside a whole-line parent (a syntax
      item) if it lies within its lines, as nested line selectors do, so
      `fn:x>"    let a"` matches and `^` means a real line start (also in `sub`)
- [x] No-match hints for `P>"a"..P>"b"` (suggest `P>"a".."b"`) and for a string
      literal that matches as escaped source text (suggest `"\\n"` for `"\n"`)
- [x] Markdown list re-basing: list-item TEXT inserted, replaced or moved next
      to any line of a list item anchors to the item: re-based to its marker
      column, inserted after the whole item (children included)

## 9. LSP daemon _(split)_

- [x] `ned-daemon` crate: per-workspace socket, lazy spawn, idle timeout
- [x] LSP client (`lsp-types`, `tokio`): initialize, didOpen/didChange, shutdown
- [x] Server configuration per language (rust-analyzer, gopls, pyright, tsserver)
- [x] `check` verb: diagnostics for edited files (and automatic checking of
      edits while a daemon runs)
- [x] Diagnostics that only `cargo check` reports (rust-analyzer flycheck on
      save), e.g. borrow errors, in `check` (edits are checked unsaved)
- [x] `rename` verb (within the file set, or the workspace with `-w`)
- [x] `.refs` / `.def` parts (replacing the reserved `refs:` / `def:`)
- [x] LSP formatting fallback when no external formatter is installed

## 10. REPL

- [ ] `ned-repl` crate: interactive session with persistent buffers, undo,
      explicit write

## 11. MCP server

- [ ] `ned-mcp` crate exposing script execution, `outline`, and `show` as tools

## 12. Release

- [ ] Syntax selectors and `outline` for Python, TypeScript, TSX, JavaScript,
      Go: query files, a test per kind and part, spec notes. Decided: Python
      `.doc` is the docstring; in JS/TS, `const f = () => {}` (or
      `= function () {}`) is both `fn:f` and `const:f` (outline lists it once,
      as `fn`), `const` declarations are `const`, and `let`/`var` are `var`
- [ ] CI (build, test, clippy, fmt) on Linux and macOS
- [ ] Release binaries and install instructions
- [ ] Choose a license

## Bugs

- [x] Ambiguity candidates for nested selectors put the scope before the
      whole selector (`64>fn:items>/name/`, `var:comma>fn:items>/name/`,
      `fn:x>file:a.rs>/re/`), so they select nothing; put `file:` first and
      the line or enclosing-item scope just before the last step. The line
      scope is also wrong for items: two `impl:Workspace` candidates were
      given as `112-120>…` and `124-150>…`, the impls' bodies, but a line
      scope must cover the whole item (`111-121>impl:Workspace`) to match it
- [x] A syntax step fails when the file set includes a file in a language
      without selector queries yet (e.g. a `.py` file next to `.rs` files),
      even if other files match; skip such files as files without a language
      are skipped, and fail only if no searched file supports the kind
- [x] Deleting (or moving) the last two items of a block in one script is an
      overlap error: in `mod t { fn x() {} fn a() {} fn b() {} }`, with blank
      lines between the functions, `delete fn:a; delete fn:b` gives "edit
      overlaps command 1", because both tidy the blank line between them
- [x] A line selector nested in a syntax item can't select the item's last
      line (`fn:b>$`, `fn:b>8`): the line's newline lies outside the item's
      span, which ends at `}`
- [x] Formatting a file that `create` makes in a new directory reports
      `rustfmt not found`: the formatter runs in the file's directory, which
      doesn't exist until the write; run it in the nearest existing ancestor
- [x] Syntax selectors are slow on large files: any `fn:` selector on the
      3,500-line `crates/ned-core/src/exec.rs` takes 1.2 s (a 400-line file:
      0.01 s), even when it matches nothing, so the cost grows faster than the
      file; profile items/query resolution
- [x] `ned` panics when stdout closes early (`ned F -e '...' | head -1`):
      "failed printing to stdout: Broken pipe", before the edit was written;
      finish the script instead
- [x] `a_stale_socket_is_replaced` (ned-daemon `tests/daemon.rs`) is flaky:
      about 1 run in 5 fails `Client::connect(..).is_none()` just after
      binding and dropping a listener
- [ ] `create a.rs "fn a() {}\n"` followed by `insert after fn:a ...` in the
      same script leaves a trailing blank line (rustfmt removes it)

## Future improvements

- [ ] Smarter indent conversion in re-basing: normalize space widths (e.g.
      2-space text into a 4-space file), detect alignment (continuation lines
      aligned to a delimiter rather than indented by levels) and preserve it
- [ ] Re-basing keeps block-quote prefixes (`> `): inserted lines take the
      target line's `>` markers, not just its whitespace
