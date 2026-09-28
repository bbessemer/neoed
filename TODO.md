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
- [ ] `queries/rust/selectors.scm` (other languages: §12)
- [ ] Syntax selectors, nesting, and parts; ambiguity errors with candidates
- [ ] `outline` verb (compact symbol tree with line numbers)
- [x] Parse-error guard (reject new ERROR/MISSING nodes unless `--force`)
- [x] Go tab default for re-basing
- [ ] Re-basing empty spans (enclosing item's indent)
- [ ] Raw tree-sitter query selector

## 6. Formatting

- [ ] Config: `.ned.toml` (walk up from file) merged over user config
- [ ] Per-language formatter commands with sensible defaults (rustfmt, gofmt,
      black/ruff, prettier)
- [ ] Run after edits; `--no-fmt`; skip with note when formatter is missing
- [ ] Report formatter-introduced changes separately from the agent's edits

## 7. Multi-file and advanced edits

- [ ] Globs and multiple files per script; per-file selector scoping
- [ ] `move SEL to before|after SEL` (within and across files)
- [ ] `--dry-run` diff-only mode

## 8. Agent ergonomics

- [ ] Concise `ned help` / `ned help VERB` sized for agent context windows
- [ ] Agent usage guide (skill/system-prompt snippet) in `docs/`
- [ ] Token-cost benchmark suite vs `sed`/Python on representative edits
- [ ] Error-message review: every error suggests a corrected command

## 9. LSP daemon _(split)_

- [ ] `ned-daemon` crate: per-workspace socket, lazy spawn, idle timeout
- [ ] LSP client (`lsp-types`, `tokio`): initialize, didOpen/didChange, shutdown
- [ ] Server configuration per language (rust-analyzer, gopls, pyright, tsserver)
- [ ] `check` verb: diagnostics for edited files
- [ ] `rename` verb (workspace-wide) and `refs:` / `def:` selectors
- [ ] LSP formatting fallback when no external formatter is configured

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

## Future improvements

- [ ] Smarter indent conversion in re-basing: normalize space widths (e.g.
      2-space text into a 4-space file), detect alignment (continuation lines
      aligned to a delimiter rather than indented by levels) and preserve it
