---
name: ned
description:
  Read, search, create and edit source files with ned, a syntax-aware line
  editor. Outline a file, show an item (fn:parse, impl:Parser>fn:new), a line
  range or every match of a regex or literal across a glob or the whole
  workspace (-w); select code by writing it, with @ placeholders, and rewrite it
  from what they captured; create files; replace, insert, delete or move code in
  one all-or-nothing call that prints a diff; find a symbol's references or
  definition, rename it across the workspace, and check language-server
  diagnostics without a build. Use it instead of grep, sed, cat, inline Python
  or str_replace, through its MCP tools when they're connected.
---

# Reading, searching and editing files with ned

`ned` applies a short script of edit commands to files. Every command in a
script applies, or none do. It prints a summary and diff hunks for each file, so
you don't need to read the file back to check the edit.

Syntax selectors work in Rust (`fn:parse`, `impl:Parser`, also for
`impl Parser<'a>`, `impl:"Display for Parser"`), Python (`class:App>fn:start`;
decorators come with the item, and `.doc` is the docstring), Go
(`fn:"Server.Run"` for a method), JavaScript and TypeScript
(`class:App>fn:render`, `interface:Shape`; an item includes its `export`) and
Markdown (`section:"Install"`, `item:`, `table:`, `code:`);
`insert end section:X` appends to a section; `*:NAME` is an item of any kind
(`*:LIMIT`, whether `const` or `var`). Other files are read as text, with a note
naming their extensions: use lines, regexes and literals, or give the file a
language with `--lang LANG` (its formatter then runs on the file too);
`--lang text` turns parsing off. Run `ned help` for the whole language in one
screen, and `ned help TOPIC` for one verb.

## Invocation

When ned's MCP server is connected (`ned mcp`; its tools are named `ned`,
`show`, `outline` and so on), use it rather than the shell. Its `ned` tool runs
a script: `script` is the script, `files` the files (or `workspace: true` for
`-w`), and `dry_run`, `commit` and the other flags are arguments; `comment` says
what the call is for, for a human following the session. Its `outline`, `show`,
`history`, `undo` and `help` tools do what those commands do, and `cd` moves the
server to another directory (a git worktree, say) for the rest of the session,
so `files` can be relative to it. The examples below are shell commands; their
files and script carry over to the tools unchanged.

Without the MCP server, pass the script on stdin with a quoted heredoc, so the
shell leaves it alone:

```sh
ned src/parser.rs <<'EOF'
replace fn:parse>"unexpected end" with "unexpected end of input"
EOF
```

Short scripts with no `'` in them can use `-e`:
`ned src/parser.rs -e 'delete fn:debug_dump'`. Don't escape a `'` into an `-e`
script: `ned`'s heredocs read no escapes, so a `\x27` in one goes in as written.
Add `-n` to preview without writing.

Search with `show all` instead of grep. It prints each match's line with its
number, under the file's name, across a glob or the whole workspace (`-w`, which
skips ignored files), or `no matches for ...` if there are none; add `+N` for
context:

```sh
ned 'crates/**/*.rs' -e 'show all /fn with_published/'
ned -w -e 'show all "SAVE_GRACE" +2'
```

ned is for files of code and docs. To search or filter a command's output (test
results, logs), pipe it to grep as usual.

Read with `outline` and `show SEL` instead of cat (`show all 1-$` for several
whole files; `show raw SEL` drops the line numbers, to copy text verbatim), and
make new files with `create` (see `ned help create`).

If `NED_SESSION` is set (or with `-s NAME`), each call is recorded. Fix a failed
call by repeating it with a correction instead of resending the script: `!!` is
the last script that edited or failed (skipping any `show` or `outline` since),
`:s/OLD/NEW/` replaces the first `OLD` in it and `:gs/OLD/NEW/` every one, and
the repeat runs on the same files, but without the flags (after `-n`, resend the
script to apply it). `ned undo` reverts the last edit (`--force` if the file
changed since), and `ned history` lists the calls:

```sh
ned src/parser.rs -e 'replace fn:prase>"end" with "end of input"'
ned -e '!!:s/prase/parse/'
ned undo
```

A subagent inherits `NED_SESSION`, so its `undo` or `!!` could act on its
parent's calls: give each subagent its own `-s NAME`, which overrides it.

Commit just your edit with `--commit MSG`: nothing else in the working tree or
index goes in, staged or not, and the commit line follows the diff:

```sh
ned src/parser.rs --commit "Say what input ended" -e 'replace fn:parse>"end" with "end of input"'
```

In a session, the commit holds every edit since the session's last commit.

## Workflow

1. `outline` to find the item. Each line is a selector you can paste back.
2. `show SEL` to read only what you need, with line numbers.
3. Edit in one script, then read the diff that ned prints.
4. `check` (or `check SEL`, `check SEL hint`) to see the language server's
   errors and warnings, instead of running the build. Edits are checked as they
   apply while a daemon runs, and once written, the checks run on save
   (`cargo check`: unresolved names, borrow errors) report what they introduced.

```ned
outline
show impl:Parser>fn:new
```

## Patterns

Change a string inside one function:

```ned
replace fn:parse>"unexpected end" with "unexpected end of input"
```

Add a method at the end of an impl. Write the code at column 0; ned indents it.
The leading blank line separates it from the previous method.

```ned
insert end impl:Parser <<END

fn peek(&self) -> Option<char> {
    self.src[self.pos..].chars().next()
}
END
```

Replace a function body, its parameters, or its return type:

```ned
replace fn:parse.body with <<END
let tok = self.next()?;
self.parse_expr(tok)
END
replace fn:new.params with "src: impl Into<String>"
replace fn:new.ret with "Result<Self, Error>"
```

Add an import, delete a function, and move one:

```ned
insert after import:std::fmt "use std::io;"
delete fn:debug_dump
move fn:new after fn:parse
```

Create a file, then keep editing it in the same script:

```ned
create src/lexer.rs <<END
pub struct Lexer;
END
insert after struct:Lexer "impl Lexer {}"
```

Edit what an earlier command moved or inserted: a `|` starts a stage that sees
the edits before it:

```ned
move fn:new after fn:parse | insert before fn:new "#[inline]"
```

Rename a symbol everywhere it's used with `rename`, which asks the language
server. Pass `-w` so it can reach every file in the workspace:

```ned
rename impl:Parser>fn:new to create
```

Find a symbol's uses, or read its definition, with the `.refs` and `.def` parts.
They work on any step and show any file they find, but edit only files in the
set, or the workspace with `-w`:

```ned
show all fn:parse.refs
show fn:main>"helper(".def
```

Without a language server, rename with a word-bounded regex. `sub` takes an
optional selector to limit it:

```ned
file src/**/*.rs
sub /\bold_name\b/ with "new_name"
sub fn:parse /\bpos\b/ with "offset"
```

Select a run of whole lines with `A..B`, from the start of the first's line to
the end of the second:

```ned
delete fn:helper_a..fn:helper_c
show /^## Usage/../^## License/
```

Select code by writing it: a backquoted pattern matches whatever the spacing,
line breaks and comments. `@name` stands for one node (an expression, a
statement, a name), `@name...` for a run of one or more (`@name...?` zero or
more), `@_` for any of them without a name:

```ned
show all `dbg!(@x...)`
delete all fn:main>`println!(@_...?);`
show `impl Display for @t { @_... }`>fn:fmt
```

`replace` puts what a pattern captured where TEXT names it; a capture's later
lines are re-indented to fit:

```ned
replace all `assert_eq!(@a..., true)` with "assert!(@a)"
replace fn:load>`if let Some(@x) = @e { @body... }` with <<END
let Some(@x) = @e else {
    return;
};
@body
END
```

Select by a property instead of a name with a filter: `.text`, `.len`
(characters on one line, lines on several) or a part, compared with `==`, `!=`,
`<`, `>`, `<=`, `>=` or `~=` (regex), and combined with `&&` and `||`:

```ned
show all fn[.doc == ""]
delete all impl:Parser>fn[.name ~= /^old_/ || .body.len == 0]
show all fn:parse.lines[.len > 100]
show all fn[.lines:1 ~= /async/]
```

Add a statement at the start of a Python method, after its docstring; ned
indents it to the block:

```ned
insert start class:App>fn:handle "metrics.count(req)"
```

Merge conflicts don't stop parsing: items on either side of a conflict are
found. `conflict:N` is a file's Nth conflict (`outline` lists them), with
`.ours`, `.theirs` and, in diff3 style, `.base`. `resolve` keeps one side, or
`both` (ours, then theirs); `replace` resolves it with new text:

```ned
show conflict:1.theirs
resolve conflict:1 theirs
replace conflict:2 with <<END
let limit = config.limit.max(1);
END
```

Conflict-shaped text inside a Markdown code fence or a multi-line raw string
counts as a conflict, as it does for git, so an edit to `all conflict` rewrites
it too.

## Rules that trip agents up

- **One match.** A selector must match exactly one span. If it matches more, the
  error lists selectors that each pick one; paste one back. They narrow by
  enclosing item (`impl:Lexer>fn:new`), line range (`40-80>fn:new`) or file
  (`file:src/a.rs>fn:new`). Use `all SEL` to act on every match.
- **The original text, per stage.** Every selector sees the file as it was
  before the script, or at the last `|`. Line numbers from an earlier `show`
  stay valid until the next `|`. To select what an earlier command inserted,
  moved or renamed, put a `|` between them. `|` binds more loosely than `;` and
  newlines, unlike a shell's: `a; b | c` is `a; b`, then `c`. After a `|`,
  `check`, `rename`, `.refs` and `.def` are errors, because they read the files
  on disk; run them before the first `|` or in another `ned` call. The syntax
  guard checks every stage, so each must leave the file parseable: to change
  both ends of a construct, replace it whole in one stage.
- **Indentation is automatic.** `<<END` text is re-indented to fit its target,
  and so is a one-line string inserted or replaced on lines of its own: its
  leading spaces don't survive. Use `<<'END'` for text that must stay exactly as
  written, such as an indented line in a test's expected output.
- **Strings** are `"..."` on one line, with `\n \t \" \\`. Use a heredoc for
  multi-line text.
- **A `<<TAG` stands in for its text**, so the command stays on one line:
  `replace <<OLD with <<NEW`, then OLD's body, then NEW's.
- **`$`** is literal in `replace`. Only `sub` expands `$1`, `${name}` and `$0`.
- **A literal's quotes aren't in what it selects.** To edit a string in code,
  select it with its quotes escaped (`"\"old\""`) and give `TEXT` its quotes
  too; select only `"old"` and `TEXT` replaces just the contents, so quotes
  written into `TEXT` double up.
- **Errors end with a fix**: a selector to paste, a closer name, a missing flag.
  Apply it and rerun; nothing was written.
- **Syntax guard.** An edit that introduces a parse error is rejected. Fix the
  text; use `--force` only if the error is intended.
- **`.sig` stops before the body**: `show` prints its whole lines, but leave out
  the `{` (or Python's `:`): replace it with `fn f(x: u8) -> u8`, not
  `fn f(x: u8) -> u8 {`.
- **`.body` is inside the braces**: TEXT that replaces it leaves out the `{` and
  `}`; with them, the new block nests inside the old braces.
- **Introduced errors block edits** while a daemon runs (`ned daemon start`;
  `status` and `stop` too; Unix only): the error lists what the edit broke. Fix
  the text, or add `allow errors` to the script when the code is knowingly
  unfinished. `--no-check` skips checking; `--force` applies anyway.
- **Formatting.** A configured formatter (rustfmt, gofmt, ruff or black,
  prettier) runs after the edit, and its changes are shown under `fmt NAME`; if
  none is installed and a daemon runs, the language server formats instead
  (`fmt rust-analyzer`). An edit that makes the formatter fail is rejected
  (`--force` applies it anyway). `--no-fmt` skips it.
- **Replacing an item keeps its doc comments and attributes** (`///`, `#[test]`,
  decorators) unless TEXT starts with its own; select `ITEM.whole` to replace
  them too. A field or variant keeps its trailing `,`: TEXT without one gets it
  back.
- **Heredoc tags nest like the shell's.** A script heredoc ends at the first
  line holding only its tag, so when the text contains an `END` line (a ned
  script inside a script, say), use another tag: `<<'MD'`. The same goes for the
  shell's `<<'EOF'`: if the text has an `EOF` line, pick another shell tag.
- **Line numbers are absolute**, even in a nested step: `fn:parse>15-16` names
  lines 15 and 16 of the file, which must lie inside `fn:parse`. But `$` in a
  nested step is the parent's last line: `fn:parse>$`. Line numbers from an
  earlier call are stale once its edits are written: take them from a new
  `show`, not from the diff, or use an item or regex selector.
- **Keep output small** on big edits with `-q` (summaries only) or
  `--context 0`.
- **`@` in a pattern is a placeholder.** Double it for a real one, as in a
  decorator, which matches with what it decorates:
  `` `@@Component(@o) class @c { @_... }` ``. A match arm, `case` or dict entry
  isn't a pattern alone yet: write its `match` or `switch` too. A pattern
  holding backquotes opens and closes with a longer run of them, as in Markdown:
  ``` `` `${x}` `` ```. A Rust macro's arguments are tokens, so match one with a
  run: `` `dbg!(@x...)` ``.
- **Patterns match strictly.** Only separators (`,`, `;`, line breaks) and
  comments are skipped, in the pattern and the file; every other token and node
  must match, so `` `fn f(self) {}` `` doesn't match `fn f(&self) {}`, nor
  `` `fn @f() {}` `` `pub fn f() {}`. Put `@_` or `@_...` where code may vary,
  `@_...?` where it may be absent. Captures keep the comments at their ends.
- **A pattern parses where it searches**: in the body of the item before it, in
  a part, or alone at the top of a file. Give code that only parses inside
  something its context: `` struct:Foo>`x: u32` ``, `` fn:f.params>`x: u32` ``.
- **Partial matches get verbatim text.** `insert after /re/ "x"` inserts right
  after the match, even mid-line, and `insert start|end /re/` does the same at
  the span's start or end. A heredoc `insert before|after` goes on lines of its
  own beside the match's lines instead (but in place beside `.body` and other
  item parts). `replace /re/` replaces only the match, so add `.lines` to
  replace its line; a note says so when TEXT repeats the rest of the line. On a
  span of several lines, `.lines` selects each line: use `all`, or `.lines:N`
  for its Nth line (`.lines:$` the last, `fn:f.body.lines:1` the first of the
  body; a shorter span is skipped, with a note). TEXT for a partial span keeps
  its first line as written and indents the rest by the first line's
  indentation, so leave that indentation off.
- **Re-basing follows the target line.** `<<END` text takes the indentation of
  the target's first line (after a literal or regex, its last line), or, for
  `insert start|end`, of the first line inside it; `insert after` the last line
  of a statement split across lines takes the statement's indentation. In
  Markdown, a new list item (`- ...`) next to any line of a list item goes
  beside the whole item, at its marker's column; other text next to a wrapped
  item's continuation line gets the hanging indent, continuing its paragraph.
  For code, use `<<END`: a quoted `<<'END'` inside the script, a shell habit,
  leaves code at column 0. A `replace` whose text dedents below its first line
  (`x;`, `}`, `fn g() {`) keeps that first line at the target's indentation, so
  write it as indented as the line it replaces.
- **Whole-line string TEXT gets its own newline.** A literal that runs from a
  line's indentation to its end (`"    x,\n"`, or `"    s\n}"`) is a whole-line
  target: string TEXT for it is re-based like a heredoc, and it ends in a
  newline, whether or not the string does.
- **Always give a script.** Without `-e` or a heredoc, `ned` reads the script
  from stdin: on a terminal that's an error, but an open pipe that never closes
  makes it wait. With `-e`, `-e -` adds the heredoc's script in its place.
- **Ranges inside a scope.** `..` binds tighter than `>`, so scope a range once:
  `fn:f>/start/../end/`, not `fn:f>/start/..fn:f>/end/`. A `+N` after a selector
  is `show`'s context, not a line offset.
- **Several lines of text by their lines.** A `"..."` literal must match the
  file's indentation exactly; a `<<END` block selector matches whole lines
  whatever their indentation, and `/first/../last/` spans from one line to
  another.
- **`insert end` goes at the very end of the span**, after a closing `]` or `}`
  the span includes, as in a constant's `.value`. To add a last element to an
  array or a match, `insert after` its current last element.
- **A Markdown list item is named by its whole first line**: select it with a
  prefix and `*`, as in `item:"Syntax steps skip*"`.
