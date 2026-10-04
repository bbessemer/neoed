# AGENTS.md - AI Coding Agent Instructions

This file is injected into every agent's context window. Keep it brief; every
addition must reduce the total tokens a new agent needs to read to get up to
speed. Keep it current after every major change and trim outdated context
aggressively. Do not change the overall structure.

## Working Rules

**Hard rules** for any AI agent on this project (and good practice for humans).
Not suggestions; exceptions are stated explicitly. Do not add or remove anything
in this section unless explicitly told to.

1. **Git is the engineer's call.** Never commit, push, merge, or delete files
   without explicit permission. Even when asked to commit: work on a feature
   branch, never the default branch (`main` / `release` / etc.); never rewrite
   history (amend, squash, reorder, rebase, force-push) — use a fixup commit to
   correct committed work.

2. **Verify before reporting done.** Tests and linters must pass after every
   change.
   - New feature work or business logic requires unit test(s); don't write
     trivial tests (e.g. asserting a constant equals its literal).
   - If the suite takes >1 minute, run only the tests relevant to the changed
     package/function.

3. **Never hardcode secrets.** Read them from the environment or other
   appropriate source at each use, even in throwaway debugging code. Avoid
   reading files or env vars containing secrets at all; ask the engineer if you
   need to verify something secret-related.

4. **Keep tasks small and focused.** Target PRs under 800 lines. Documentation
   counts at a steep discount, since it's much cheaper to review.
   - When the assigned task is complete, **stop** — even if the next step is
     obvious or a TODO file exists.
   - If a task is obviously too large, plan it, split it into sub-tasks, and
     return to the engineer before starting each one.
   - If a task grows significantly mid-implementation (bugs, architectural
     issues), **stop**, explain, and ask how to proceed.
   - Don't change existing code just to enforce stylistic rules, **including
     these rules**.

5. **Code and documentation shouldn't repeat each other.** Don't write comments
   or docs explaining what an engineer could get from the code; that's a
   maintenance burden and a source of stale-doc mistakes.
   - Default to self-explanatory code, unless there's a specific reason not to —
     performance, external constraints, or irreducibly complex business logic.
   - Docs (including doc comments) explain **what**; comments explain **why**,
     if needed; code explains **how**.

6. **No boilerplate or glue code.** Wrappers, adapters, and trivial
   transformations that express no business logic are symptoms of structural
   problems — flag them rather than working around them.
   - **Never** wrap a private function or type in a public one. Reconsider the
     use case; if external access is truly needed, make the original symbol
     public.

7. **Be mindful of your own context window.** Don't read files you don't need.
   Search for specific symbols before reading whole files. Dispatch exploration
   and parallelizable implementation work to subagents.
   - Subagents **cannot** ask the engineer questions. Resolve any ambiguity
     requiring a decision at the top level _before_ launching, and include the
     answer in the prompt.
   - Each subagent prompt must state exactly what information or file changes
     are expected back, and include the spec excerpts and file paths needed to
     finish without further lookups.
   - **Never** instruct a subagent to return the verbatim contents of a file,
     webpage, or document. Ask for a summary with key line numbers, or read it
     yourself; subagents are for summarizing and extracting.

8. **When in doubt, stop and ask.** The engineer has context you don't. On an
   unexpected constraint or a scope change, stop, explain, and ask — no
   shortcuts, workarounds, or partial solutions.
   - If blocked by missing access or permissions, **IMMEDIATELY STOP** and
     confirm you understood the task. **NEVER** work around the restriction or
     try to gain access. This includes editing files via shell commands while in
     read-only or planning mode.

## Development Workflow

**Use `ned` for all work on this project.** Read code with `ned outline` and
`ned show`, make every edit to an existing file with the installed `ned` (usage:
the project skill, `.claude/skills/ned`, and `ned help`), not the Edit tool, sed
or inline Python, and create new files with `ned`'s `create`. If `ned` can't
make an edit, makes it wrongly, or gives an unhelpful error, fall back for that
edit only and report the gap to the engineer. After changing `ned`, reinstall
it: `cargo install --path crates/ned-cli`.

Non-trivial implementation work follows a strict TDD cycle in small chunks. Each
chunk is one cohesive unit (a module, a protocol message, a service behaviour).
Finish each step before the next. Each step ends at a commit point: per rule 1,
pause and ask for approval, then commit when permitted.

1. **Skeleton** — minimum scaffolding for the next unit: module files, exported
   types, signatures with empty/`not implemented` bodies. Must build cleanly.
   Commit prefix `skeleton:` when permitted.
2. **Tests** — comprehensive tests written against the spec docs, **not** the
   skeleton. First read the relevant spec sections and neighbouring test files
   to match style and fixtures. Tests must compile; runtime failures are
   expected here. If a design decision is genuinely ambiguous and unresolvable
   from the docs, **stop and ask** — do not guess. Commit prefix `test:` when
   permitted.
3. **Red** — run the tests and record every failure. Classify each as a **logic
   bug** in the skeleton or a **spec gap** (ask the engineer before proceeding).
   Fix nothing yet; the only thing to commit here is a spec clarification, if
   one was obtained. If everything already passes, skip to step 5.
4. **Fix** — fix bugs one logical group at a time, rerunning the suite after
   each group to confirm the targeted tests pass with no regressions. Commit
   prefix `fix:` (bug fixes) or `feat:` (initial implementations), when
   permitted.
5. **Green** — run the full test suite, linter, and static analysis. All must
   pass cleanly. Commit any remaining changes when permitted.

Other commit prefixes: `docs:` (documentation), `todo:` (TODO.md only), and
`format:` (formatting only).

**At the end of every session**, review the traps and bugs you hit while using
`ned`. Add every bug to TODO.md (Bugs). For a trap a new feature would prevent,
propose the feature to the engineer and add it to TODO.md once approved;
otherwise add guidance to `docs/skills/ned/SKILL.md`. Commit these to the
current feature branch (prefix `todo:`), not a new branch.

**Feedback from other agents** (reports on `ned` from sessions in other
projects) goes in the same places. This repo is public: keep those projects'
names, paths, identifiers and code out of every file and commit message, and
rewrite examples with generic names.

## Project Overview

Neoed (`ned`) is a line editor for AI coding agents, replacing `sed`/ad-hoc
Python. LLMs, like teletypes, work over an append-only text stream where every
token costs, so `ned` offers a concise, word-based command language,
syntax-aware addressing (tree-sitter, plus LSP through a per-workspace daemon),
and automatic formatting. MVP is a one-shot CLI; a human REPL and an MCP server
come later.

## Status

Spec signed off. `ned-core` has the buffer (`buffer`), edit set (`edit`), atomic
multi-file write (`fs`), diff rendering (`diff`), the script lexer, parser, and
error rendering (`script::parse`), selector resolution (`select`), spans, their
parts and filters (`span`), whole-line/re-basing text helpers (`text`), the
executor (`exec::run`), language detection and parsing (`lang`), syntax items
and parts from `queries/<lang>/selectors.scm` (`syntax`), `outline`, and
external formatters with `.ned.toml` config (`format`). The CLI supports every
selector (including `A..B` ranges, bare kinds and `[...]` filters) and verb
(including `create`), `show +N`, globbed file sets, the parse-error guard,
formatting, `-w` (every workspace file; files are read lazily), and `ned help`.
Every error ends with a fix. `ned-daemon` has the per-workspace daemon (socket,
lock, lazy spawn, idle exit) behind `ned daemon`, and starts language servers
(`lsp`, `servers`; `[lsp]` config in `ned-core`) for `open` requests and
diagnostics, through the `Lsp` trait in `ned-core`: `check [SEL] [LEVEL]`, and
edits checked while a daemon runs (introduced errors block unless
`allow errors`), `rename`, and the `.refs`/`.def` parts (within the file set or
`-w` workspace); the version names the build commit, and a daemon serves only
its own build. With a daemon running, language servers format files whose
formatters aren't installed, and `check` also waits for save-time checks
(`cargo check`). §9 is done. Sessions (spec §1.2): `session` in `ned-core` keeps
the per-workspace log, plans `undo` (`--force` merges with `diff::merge`) and
expands `!!`; the CLI records every script run under `-s`/`NED_SESSION` and has
`ned history` and `ned undo`. `--commit MSG` (`git` in `ned-core`) commits
exactly an invocation's edits (in a session, every edit since its last commit)
with git's plumbing, leaving other staged and unstaged changes alone. Terminal
output (spec §6.6): `style` paints output under `--color`/`NO_COLOR`, and
`highlight` colours `show` and diff hunks from the grammars' highlight queries;
piped output is unchanged. Syntax selectors cover Rust, Python, Go, JavaScript,
TypeScript/TSX and Markdown; `conflict` hides merge-conflict markers from every
parse and finds the conflicts (`conflict:N`, `.ours`/`.theirs`/`.base`), which
`resolve` resolves. Syntax patterns (spec §3.10) select code by writing it:
`ned-scheme` reads the Scheme dialect (tree-sitter query syntax plus Scheme
data) that builders and, later, plugins are written in; `template` lexes `@`
placeholders, `fragment` parses a pattern alone or inside
`queries/<lang>/builders.scm`, and `pattern` matches it against the tree;
`replace` substitutes its captures. MIT-licensed; README has install and usage;
CI (`.github/workflows/`) gates PRs.

## Key Documentation

- `TODO.md` — milestone plan; check items off as they land.
- `docs/command-language.md` — authoritative spec for syntax, selectors, verbs,
  output, and exit codes. Tests are written against it; update it _before_
  changing behaviour.
- `docs/agent-guide.md`, `docs/skills/ned/SKILL.md` — how agents use `ned`; keep
  in step with behaviour (every ```ned block is parsed by a test).

## Tech Stack / Dependencies

| Technology                        | Role                                       |
| --------------------------------- | ------------------------------------------ |
| Rust (edition 2024)               | Language; single fast-starting binary      |
| `tree-sitter` + grammar crates    | Parsing: Rust, Python, TS/JS, Go, Markdown |
| `ropey`                           | Rope text buffer                           |
| `regex`                           | Regex selectors and `sub`                  |
| `glob`, `ignore`                  | File-set globs; `-w` workspace walk        |
| `similar`                         | Diff output                                |
| `serde` + `toml`                  | Config (`.ned.toml`)                       |
| `thiserror` / `anyhow`            | Errors in core / CLI                       |
| `clap` (derive)                   | CLI arguments                              |
| `tokio`, `serde_json`, `libc`     | Daemon: event loop, protocol, `getuid`     |
| `lsp-types`, `url`                | LSP messages and file URIs                 |
| `insta`, `assert_cmd`, `tempfile` | Snapshot, CLI, and fs tests                |

## Repository Structure

```
Cargo.toml         workspace; shared version, edition, lints
crates/ned-core/   library: buffer, script parser, selectors, languages, exec, formatting
crates/ned-cli/    `ned` binary: args, I/O, output rendering, help texts, daemon client glue
crates/ned-daemon/ per-workspace daemon (Unix socket), its sync client, language servers
crates/ned-scheme/ reader for the Scheme dialect of query files, builders and plugins
queries/<lang>/    tree-sitter selector queries (.scm), one dir per language
docs/              specs, agent guide, Claude Code skill
bench/             token-cost (cases/ back spec §8's table) and --commit git-process benchmarks (uv project)
```

Planned crates: `ned-repl`, `ned-mcp`. All logic lives in `ned-core` so
frontends stay thin.

## Design Decisions

- **One-shot, stateless CLI.** `ned FILE... -e SCRIPT` (or script on stdin).
  Startup latency matters, so grammars are statically linked. LSP features go
  through a lazily-spawned per-workspace daemon that keeps servers warm; the CLI
  must work fully without it.
- **Tokens, not characters.** Verbs are short words (`show`, `replace`,
  `insert after`, `delete`, `sub`, `outline`), not sigils. Output is terse:
  per-file summary plus minimal diff hunks.
- **Selectors over line numbers.** Lines/ranges and regex/literal matches, plus
  syntax selectors (`fn:parse`, `impl:Parser>fn:new`, `fn:parse.body`).
  Ambiguous matches are errors listing candidates unless `all` is given.
- **Data-driven languages.** Selector kinds map to per-language `.scm` queries;
  adding a language should need a grammar crate and query files, not code.
- **Transactional scripts.** All commands in a script apply or none do; files
  are written atomically. Edits that introduce new tree-sitter parse errors are
  rejected unless `--force`.
- **Indentation re-basing.** Inserted text blocks are re-indented to the target
  site; agents need not reproduce indentation.
- **Formatting** runs configured external formatters (per language, via
  `.ned.toml`/user config) after edits; LSP formatting is a fallback when the
  daemon is available. Missing formatters are skipped with a note, not an error.

## Coding Conventions

- `cargo fmt` defaults; `cargo clippy --all-targets -- -D warnings` clean.
- `thiserror` error enums in `ned-core`; `anyhow` only in binaries.
- Unit tests beside code; CLI end-to-end tests in `crates/ned-cli/tests/` using
  `assert_cmd` + `insta` snapshots (review snapshots with `cargo insta review`).
- Query files under `queries/` are code: every selector kind needs a test per
  language.
- Keep `.ned.toml` organized by what a setting configures: a setting several
  features use belongs in the section they share (`[lsp] timeout` covers every
  server request), not the section of the feature that first needed it. Move it
  when its use widens; no aliases before 1.0.

## Local Development

| Target / Script                             | Description                            |
| ------------------------------------------- | -------------------------------------- |
| `cargo build`                               | Build workspace                        |
| `cargo test`                                | Run all tests                          |
| `cargo clippy --all-targets -- -D warnings` | Lint                                   |
| `cargo fmt --check`                         | Format check                           |
| `prettier --check '**/*.md'`                | Docs format check (`.prettierrc`)      |
| `cargo run -q -p ned-cli -- ARGS`           | Run `ned` from source                  |
| `cargo test -p ned-daemon -- --ignored`     | Smoke test real LSP servers            |
| `cd bench && uv run bench.py --check`       | Token benchmark vs spec §8             |
| `cd bench && uv run git_procs.py --check`   | `--commit`'s git processes vs baseline |
| `cd bench && uv run pytest`                 | Benchmark unit tests                   |

## Deployment Notes

Every PR to `main` must raise `[workspace.package] version` (CI checks; which
part per TODO.md). Merging tags `vX.Y.Z` and publishes Linux (musl) and macOS
binaries (`release.yml`, built with `NED_RELEASE` set for a metadata-free
version). Install from source:
`cargo install --locked --git https://github.com/bbessemer/neoed ned-cli`; from
a checkout, `cargo install --path crates/ned-cli`.
