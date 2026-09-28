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

4. **Keep tasks small and focused.** Target PRs under 800 lines.
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

## Project Overview

<!-- 2-4 sentences: what this project does and why. -->

## Status

<!-- What's complete, what's in progress, which build/test commands pass cleanly.
Implementation must stay consistent with the design doc. -->

## Key Documentation

<!-- Docs agents should reference, and when each must be updated. E.g.:
  - Background and requirements: `docs/BRIEF.md`
  - Architecture and design decisions: `docs/DESIGN.md`
  - Prioritized task list: `docs/TODO.md`
-->

## Tech Stack / Dependencies

| Technology | Role |
| ---------- | ---- |
| ...        | ...  |

## Repository Structure

<!-- Annotated directory tree: purpose of each top-level directory and any key files. -->

## Design Decisions

<!-- Non-obvious decisions that constrain implementation, each with its rationale.
Agents must not violate these without explicit approval. -->

## Coding Conventions

<!-- Linting/type-checking requirements, test command, logging approach, comment policy,
generated-code directories that must not be hand-edited, naming conventions. -->

## Local Development

<!-- Starting/stopping the local stack: build images, start services, view logs, check
status, and the URLs services are reachable at. -->

| Target / Script | Description |
| --------------- | ----------- |
| `build`         | ...         |
| `test`          | ...         |
| `lint`          | ...         |

<!-- Stateful services (databases, brokers): how state persists and what's needed for a
clean-slate restart (e.g. volume wipes). -->

<!-- Per-service configuration: where configs live and the key fields agents need. -->

## Deployment Notes

<!-- Where and how the project runs in production. -->
