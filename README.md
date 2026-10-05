# neoed

`ned` is a line editor for AI coding agents. It replaces `sed`, inline Python
and whole-file rewrites with a short, word-based command language that addresses
code by its syntax (`fn:parse`, `impl:Parser>fn:new`, `fn:parse.body`) instead
of by line numbers or repeated source text.

- **Cheap in tokens.** Commands are short words, and every edit prints a diff,
  so the agent doesn't have to read the file back to check it.
- **All or nothing.** A script's edits all apply, or none do, and files are
  written atomically. An edit that introduces a syntax error is rejected.
- **Indentation is automatic.** Inserted code is written at column 0 and
  re-indented to fit where it goes.
- **Errors end with a fix**: a selector to paste, a closer name, or a missing
  flag.

## Example

```sh
ned src/parser.rs -e outline
```

```
src/parser.rs
1 import (1)
3-7 struct:Parser
9-22 impl:Parser
  10-12 fn:new
  14-17 fn:parse
  19-21 fn:debug_dump
```

Each outline line is a selector the agent can paste back. One call changes a
string inside one function, deletes a method and adds another:

```sh
ned src/parser.rs <<'EOF'
replace fn:parse>"unexpected end" with "unexpected end of input"
delete fn:debug_dump
insert end impl:Parser <<END

fn peek(&self) -> Option<char> {
    self.src[self.pos..].chars().next()
}
END
EOF
```

```diff
src/parser.rs: 3 edits, +3 -3
@@ -14,3 +14,3 @@
     pub fn parse(&mut self) -> Result<Expr, Error> {
-        let tok = self.next().ok_or(Error::new("unexpected end"))?;
+        let tok = self.next().ok_or(Error::new("unexpected end of input"))?;
         self.parse_expr(tok)
@@ -18,4 +18,4 @@

-    fn debug_dump(&self) {
-        eprintln!("{}", self.src);
+    fn peek(&self) -> Option<char> {
+        self.src[self.pos..].chars().next()
     }
```

## Install

On Linux or macOS (x86_64 or arm64), install the latest release:

```sh
curl -fsSL https://raw.githubusercontent.com/bbessemer/neoed/main/install.sh | sh
```

The script downloads the binary for your platform from
[GitHub Releases](https://github.com/bbessemer/neoed/releases/latest), checks
its SHA-256, and puts `ned` in `~/.local/bin`. Set `NED_INSTALL_DIR` to install
elsewhere, or `NED_VERSION` (e.g. `v0.8.0`) to pin a release.

To build from source instead, you need Rust 1.90 or later and a C compiler,
because the tree-sitter grammars are compiled in:

```sh
cargo install --locked --git https://github.com/bbessemer/neoed ned-cli
```

From a clone, run `cargo install --path crates/ned-cli` instead.

Formatters and language servers are optional, and ned uses them when they're
installed:

| Language                         | Formatter          | Language server              |
| -------------------------------- | ------------------ | ---------------------------- |
| Rust                             | `rustfmt`          | `rust-analyzer`              |
| Python                           | `ruff`, or `black` | `pyright-langserver`         |
| Go                               | `gofmt`            | `gopls`                      |
| JavaScript, TypeScript (and TSX) | `prettier`         | `typescript-language-server` |
| Markdown                         | `prettier`         | none                         |

You can change either per language in `.ned.toml` (see `ned help config`).

## Use it from an agent

**Claude Code:** copy the skill into your skills directory:

```sh
git clone https://github.com/bbessemer/neoed
cp -r neoed/docs/skills/ned ~/.claude/skills/
```

Claude then uses ned when it reads, searches or edits files.

**MCP:** `ned mcp` serves the same scripts as tools (`ned`, `outline`, `show`,
`history`, `undo`, `help`, and `cd` to move it) over the Model Context Protocol,
for agents without a shell. It records every call in a session, so `undo` and
`!!` work there too:

```sh
claude mcp add ned -- ned mcp
```

To let each conversation undo its own edits and repeat a failed command with a
fix, give it a session with a `SessionStart` hook; the
[agent guide](docs/agent-guide.md#setup) has the snippet.

**Other agents:** add the system-prompt snippet from
[`docs/agent-guide.md`](docs/agent-guide.md). It points the agent at `ned help`
for the rest.

## Usage

```
ned [FLAGS] [FILE... | -w [DIR]] -e SCRIPT   (or on stdin; -e - too)
ned help [TOPIC]
ned daemon start|status|stop [DIR]
ned history|undo [-s NAME] [-w DIR]
ned session list [--all] | delete NAME... [-w DIR]
```

A script is a list of commands, one per line or separated by `;`:

| Command                                     | What it does                                        |
| ------------------------------------------- | --------------------------------------------------- |
| `show [raw] [SEL [+N]]`                     | print lines, numbered; `show all /re/` searches     |
| `outline [SEL]`                             | list syntax items as selectors you can paste back   |
| `replace SEL with TEXT`                     | replace a span                                      |
| `insert before\|after\|start\|end SEL TEXT` | insert beside or inside a span                      |
| `delete SEL`                                | delete a span                                       |
| `sub [SEL] /re/ with TEXT`                  | regex replace, with `$1` groups                     |
| `move SEL before\|after\|start\|end DEST`   | move a span, within or across files                 |
| `create PATH TEXT`                          | create a file that later commands can edit          |
| `file PATH...`                              | set the files later commands act on (globs work)    |
| `check [SEL] [LEVEL]`                       | language-server diagnostics, without a build        |
| `rename SEL to NAME`                        | rename a symbol everywhere, via the language server |
| `resolve SEL ours\|theirs\|base\|both`      | resolve a merge conflict by keeping its sides       |

A selector can be:

- a line number or range (`12-20`, `$`);
- a regex (`/re/i`) or a literal (`"text"`);
- a syntax item (`fn:parse`, `fn:test_*`, or `fn` for every function);
- a nested step (`impl:Parser>fn:new>"x"`);
- a part: `.body`, `.sig`, `.params`, `.name`, `.doc`, `.attrs`, `.ret`,
  `.type`, `.value`, `.whole`, `.lines`, and `.refs` and `.def` through the
  language server;
- a range (`fn:a..fn:c`);
- a filter (`fn[.doc == ""]`, `fn:parse.lines[.len > 80]`);
- scoped to one file or a glob's files (`file:src/a.rs>fn:new`,
  `file:src/*.rs>fn:new`);
- a merge conflict (`conflict:2`), with `.ours`, `.theirs` and `.base`;
- a syntax pattern: code with placeholders, matched whatever its spacing and
  comments (`` `foo(@x, @rest...)` ``);
- a raw tree-sitter query (`query{...}`).

A syntax pattern's `@name` placeholders capture what they match, and `replace`
puts the captures into TEXT:
``replace all `assert_eq!(@a..., true)` with "assert!(@a)"``. A pattern may
leave out separators (`,`, `;`), but every other token must match, so
`` `fn f(self) {}` `` doesn't match `fn f(&self) {}`. `ned help patterns` has
the rules.

A selector must match exactly one span unless it starts with `all`. A `|`
between commands starts a stage that sees the edits before it.

`-n` previews without writing, and `-w` works on every file in the workspace,
respecting `.gitignore`. `ned help` prints the whole language on one screen;
[`docs/command-language.md`](docs/command-language.md) is the full spec.

On a terminal, output is coloured: shown code and diffs are syntax-highlighted,
with aligned line numbers. A non-empty `NO_COLOR` turns it off, and
`--color always` or `--color never` overrides both. On a truecolor or 256-colour
terminal, a theme colours each kind of token its own way, and tints changed
lines red or green. It's `default-dark` unless the user config sets another
(`theme = "default-light"`, or your own; see `ned help config`) or `NED_THEME`
names one.

With `-s NAME`, or `NED_SESSION=NAME` in the environment, `ned` records each
invocation in a session: `ned history` lists them, `ned undo` reverts the last
edit, and `ned -e '!!:s/prase/parse/'` repeats the last script with a fix.

`--commit MSG` turns the edits an invocation writes into one git commit, and
nothing else: other changes in the working tree and index, staged or not, stay
uncommitted. In a session, it commits every edit since the session's last
commit.

`ned repl` (or plain `ned` on a terminal) is an editor for humans: each line is
a script, but its edits stay in memory until `:write`, so they can be read and
undone first (`:undo`, `:diff`). `:commit MSG` writes and commits.
`:attach NAME` follows an agent's session, printing each edit it makes, and
records your corrections into that session. `ned help repl` lists the commands.

## Languages

| Language        | Syntax items                                                                     |
| --------------- | -------------------------------------------------------------------------------- |
| Rust            | `fn struct field enum variant trait impl type const var mod import`              |
| Python          | `fn class field const var import` (decorators and docstrings belong to the item) |
| Go              | `fn struct interface type field const var import` (methods are `fn:"Recv.Name"`) |
| JavaScript      | `fn class field const var import`                                                |
| TypeScript, TSX | the JavaScript items, plus `interface type enum variant mod`                     |
| Markdown        | `section item table code`                                                        |

Syntax patterns work in every language above but Markdown. Line, regex, literal
and `conflict` selectors work in any UTF-8 file.

## Language servers

`check`, `rename`, `.refs` and `.def` go through a per-workspace daemon. `ned`
starts it on demand, it keeps the servers warm between calls, and it exits after
10 minutes idle. While a daemon runs (`ned daemon start`), every edit is checked
too, and an edit that introduces errors is rejected unless the script says
`allow errors`. The daemon is Unix-only for now, and everything else works
without it.

## Token cost

Tokens are counted with tiktoken's `o200k_base` over the full command text. The
table comes from spec §8, and `bench/` reproduces it:

| Task                            | ned | sed | Python | str_replace |
| ------------------------------- | --: | --: | -----: | ----------: |
| Change a string in one function |  22 |  20 |     61 |          32 |
| Add a method to an impl         |  41 |   — |     94 |          74 |
| Delete a function               |  13 |  18 |     70 |          44 |
| Replace a function body         |  38 |   — |     93 |          62 |

`sed` can't do some of these at all. Both `sed` and Python also need a read-back
to verify the edit, and ned's diff replaces it.

## Status

`ned` is pre-1.0. The command language is specified, and ned is used day to day
to develop ned itself, but the language may still change before 1.0.

## Roadmap

In order, with details in [`TODO.md`](TODO.md):

- User-supplied tree-sitter grammars and query files, without rebuilding `ned`
- Later, plugins: a Scheme over tree-sitter queries for new languages, custom
  commands and syntax-aware code generation

## Development

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
prettier --check '**/*.md'
cargo run -q -p ned-cli -- FILE -e SCRIPT
```

See [`AGENTS.md`](AGENTS.md) for the project's conventions and layout.

CI runs these on Linux and macOS for pull requests to `main` and `release/*`
branches. Every pull request to `main` that changes Rust code (or `queries/`)
must update `CHANGELOG.md` and raise `[workspace.package] version` in
`Cargo.toml` (see `TODO.md` for which part to bump); merging it tags that
version and publishes its release binaries.

## License

MIT; see [`LICENSE`](LICENSE).
