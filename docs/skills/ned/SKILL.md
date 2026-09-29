---
name: ned
description: Edit existing source files with ned, a syntax-aware line editor. Replace, insert, delete or move code by item name (fn:parse, impl:Parser>fn:new), line range, regex or literal, across several files, in one all-or-nothing call that prints a diff. Use it instead of sed, inline Python or str_replace for edits to existing files.
---

# Editing files with ned

`ned` applies a short script of edit commands to files. Every command in a
script applies, or none do. It prints a summary and diff hunks for each file,
so you don't need to read the file back to check the edit.

Syntax selectors (`fn:parse`, `impl:Parser`) work in Rust files. In other
files, use lines, regexes, literals or `query{}`. Run `ned help` for the whole
language in one screen, and `ned help TOPIC` for one verb.

## Invocation

Pass the script on stdin with a quoted heredoc, so the shell leaves it alone:

```sh
ned src/parser.rs <<'EOF'
replace fn:parse>"unexpected end" with "unexpected end of input"
EOF
```

Short scripts can use `-e`: `ned src/parser.rs -e 'delete fn:debug_dump'`.
Add `-n` to preview without writing.

## Workflow

1. `outline` to find the item. Each line is a selector you can paste back.
2. `show SEL` to read only what you need, with line numbers.
3. Edit in one script, then read the diff that ned prints.

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

Rename across files with a word-bounded regex:

```ned
file src/**/*.rs
sub /\bold_name\b/ with "new_name"
```

Insert into a Python block by line number, after checking the lines with
`show`:

```ned
insert after 3 <<END
metrics.count(req)
END
```

## Rules that trip agents up

- **One match.** A selector must match exactly one span. If it matches more,
  the error lists selectors that each pick one; paste one back. Use
  `all SEL` to act on every match.
- **The original text.** Every selector in a script sees the file as it was
  before the script. Line numbers from an earlier `show` stay valid, but a
  command can't select text that an earlier command in the same script
  inserted. Use a second `ned` call for that.
- **Indentation is automatic.** `<<END` text is re-indented to fit its
  target. Use `<<'END'` for text that must stay exactly as written.
- **Strings** are `"..."` on one line, with `\n \t \" \\`. Use a heredoc for
  multi-line text.
- **`$`** is literal in `replace`. Only `sub` expands `$1`, `${name}` and `$0`.
- **Errors end with a fix**: a selector to paste, a closer name, a missing
  flag. Apply it and rerun; nothing was written.
- **Syntax guard.** An edit that introduces a parse error is rejected. Fix the
  text; use `--force` only if the error is intended.
- **Formatting.** A configured formatter (rustfmt, gofmt, ruff or black,
  prettier) runs after the edit, and its changes are shown under
  `fmt NAME`. `--no-fmt` skips it.
