---
name: ned
description: Read, search, create and edit source files with ned, a syntax-aware line editor. Outline a file, show an item (fn:parse, impl:Parser>fn:new), a line range or every match of a regex or literal across a glob or the whole workspace (-w); create files; replace, insert, delete or move code in one all-or-nothing call that prints a diff; find a symbol's references or definition, rename it across the workspace, and check language-server diagnostics without a build. Use it instead of grep, sed, cat, inline Python or str_replace.
---

# Reading, searching and editing files with ned

`ned` applies a short script of edit commands to files. Every command in a
script applies, or none do. It prints a summary and diff hunks for each file,
so you don't need to read the file back to check the edit.

Syntax selectors work in Rust (`fn:parse`, `impl:Parser`,
`impl:"Display for Parser"`), Python (`class:App>fn:start`; decorators come
with the item, and `.doc` is the docstring), Go (`fn:"Server.Run"` for a
method), JavaScript and TypeScript (`class:App>fn:render`,
`interface:Shape`; an item includes its `export`) and Markdown
(`section:"Install"`, `item:`, `table:`, `code:`); `insert end section:X`
appends to a section. In other files, use lines, regexes and literals, or give
the file a language with `--lang LANG`. Run `ned help` for the whole language
in one screen, and `ned help TOPIC` for one verb.

## Invocation

Pass the script on stdin with a quoted heredoc, so the shell leaves it alone:

```sh
ned src/parser.rs <<'EOF'
replace fn:parse>"unexpected end" with "unexpected end of input"
EOF
```

Short scripts can use `-e`: `ned src/parser.rs -e 'delete fn:debug_dump'`.
Add `-n` to preview without writing.

Search with `show all` instead of grep. It prints each match's line with its
number, under the file's name, across a glob or the whole workspace (`-w`,
which skips ignored files), or `no matches for ...` if there are none; add
`+N` for context:

```sh
ned 'crates/**/*.rs' -e 'show all /fn with_published/'
ned -w -e 'show all "SAVE_GRACE" +2'
```

Read with `outline` and `show SEL` instead of cat, and make new files with
`create` (see `ned help create`).

## Workflow

1. `outline` to find the item. Each line is a selector you can paste back.
2. `show SEL` to read only what you need, with line numbers.
3. Edit in one script, then read the diff that ned prints.
4. `check` (or `check SEL`, `check SEL hint`) to see the language server's
   errors and warnings, instead of running the build. Edits are checked as
   they apply while a daemon runs, but only `check` includes `cargo check`'s
   errors (unresolved names, borrow errors), so run it after a Rust edit.

```ned
outline
show impl:Parser>fn:new
```

## Patterns

Change a string inside one function:

```ned
replace fn:parse>"unexpected end" with "unexpected end of input"
```

Add a method at the end of an impl. Write the code at column 0; ned indents
it. The leading blank line separates it from the previous method.

```ned
insert end impl:Parser <<END

fn peek(&self) -> Option<char> {
    self.src[self.pos..].chars().next()
}
END
```

Replace a function body, or its parameters:

```ned
replace fn:parse.body with <<END
let tok = self.next()?;
self.parse_expr(tok)
END
replace fn:new.params with "src: impl Into<String>"
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

Find a symbol's uses, or read its definition, with the `.refs` and `.def`
parts. They work on any step, and reach every workspace file with `-w`:

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

Select a run of items or lines with `A..B`, from the start of the first to the
end of the second:

```ned
delete fn:helper_a..fn:helper_c
show /^## Usage/../^## License/
```

Add a statement at the start of a Python method, after its docstring; ned
indents it to the block:

```ned
insert start class:App>fn:handle "metrics.count(req)"
```

## Rules that trip agents up

- **One match.** A selector must match exactly one span. If it matches more,
  the error lists selectors that each pick one; paste one back. They narrow by
  enclosing item (`impl:Lexer>fn:new`), line range (`40-80>fn:new`) or file
  (`file:src/a.rs>fn:new`). Use `all SEL` to act on every match.
- **The original text, per stage.** Every selector sees the file as it was
  before the script, or at the last `|`. Line numbers from an earlier `show`
  stay valid until the next `|`. To select what an earlier command inserted,
  moved or renamed, put a `|` between them. `|` binds more loosely than `;`
  and newlines, unlike a shell's: `a; b | c` is `a; b`, then `c`. After a `|`,
  `check`, `rename`, `.refs` and `.def` are errors, because they read the
  files on disk; run them before the first `|` or in another `ned` call.
- **Indentation is automatic.** `<<END` text is re-indented to fit its
  target, and so is a one-line string inserted or replaced on lines of its
  own: its leading spaces don't survive. Use `<<'END'` for text that must stay
  exactly as written, such as an indented line in a test's expected output.
- **Strings** are `"..."` on one line, with `\n \t \" \\`. Use a heredoc for
  multi-line text.
- **`$`** is literal in `replace`. Only `sub` expands `$1`, `${name}` and `$0`.
- **Errors end with a fix**: a selector to paste, a closer name, a missing
  flag. Apply it and rerun; nothing was written.
- **Syntax guard.** An edit that introduces a parse error is rejected. Fix the
  text; use `--force` only if the error is intended.
- **Introduced errors block edits** while a daemon runs (`ned daemon start`;
  `status` and `stop` too; Unix only): the error lists what the edit broke.
  Fix the text, or add `allow errors` to the script when the code is knowingly
  unfinished. `--no-check` skips checking; `--force` applies anyway.
- **Formatting.** A configured formatter (rustfmt, gofmt, ruff or black,
  prettier) runs after the edit, and its changes are shown under `fmt NAME`;
  if none is installed and a daemon runs, the language server formats instead
  (`fmt rust-analyzer`). `--no-fmt` skips it.
- **Replacing an item keeps its doc comments and attributes** (`///`,
  `#[test]`, decorators) unless TEXT starts with its own; select `ITEM.lines`
  to replace them too. A field or variant keeps its trailing `,`: TEXT without
  one gets it back.
- **Heredoc tags nest like the shell's.** A script heredoc ends at the first
  line holding only its tag, so when the text contains an `END` line (a ned
  script inside a script, say), use another tag: `<<'MD'`. The same goes for
  the shell's `<<'EOF'`: if the text has an `EOF` line, pick another shell tag.
- **Line numbers are absolute**, even in a nested step: `fn:parse>15-16`
  names lines 15 and 16 of the file, which must lie inside `fn:parse`. But `$`
  in a nested step is the parent's last line: `fn:parse>$`. Line numbers from
  an earlier call are stale once its edits are written: take them from a new
  `show`, not from the diff, or use an item or regex selector.
- **Keep output small** on big edits with `-q` (summaries only) or
  `--context 0`.
- **Use `delete` to remove lines.** `replace 12 with ""` leaves an empty line,
  because line-oriented text always ends with a newline.
- **Partial matches get verbatim text.** `insert after /re/ "x"` inserts right
  after the match, even mid-line, and `insert start|end /re/` does the same at
  the span's start or end. A heredoc `insert before|after` goes on lines of
  its own beside the match's lines instead (but in place beside `.body` and
  other item parts). `replace /re/` replaces only the match, so add `.lines`
  to replace whole lines; a note says so when TEXT repeats the rest of the
  line.
- **Re-basing follows the target line.** `<<END` text takes the indentation of
  the line it's inserted next to, or of the first line it replaces. In
  Markdown, a new list item (`- ...`) next to any line of a list item goes
  beside the whole item, at its marker's column; other text next to a wrapped
  item's continuation line gets the hanging indent, continuing its paragraph.
  For code, use `<<END`: a quoted `<<'END'` inside the script, a shell habit,
  leaves code at column 0.
- **Whole-line string TEXT gets its own newline.** A literal that runs from a
  line's indentation to its end (`"    x,\n"`, or `"    s\n}"`) is a whole-line
  target: string TEXT for it is re-based like a heredoc, and a final newline
  is added, so a trailing `\n` in the string adds a blank line. Leave the
  `\n` off, or use a line number or a heredoc.
- **Always give a script.** Without `-e` or a heredoc, `ned` reads the script
  from stdin: on a terminal that's an error, but an open pipe that never
  closes makes it wait.
