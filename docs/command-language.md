# ned command language

This is the authoritative spec for `ned`'s invocation, script syntax, selectors,
verbs, output, and exit codes. Tests are written against it; change it before
changing behaviour.

## 1. Invocation

```
ned [FLAGS] [FILE... | -w [DIR]] [-e SCRIPT]...
ned help [TOPIC]
ned daemon start|status|stop [DIR]
ned history [-s NAME] [-w DIR] [--all]
ned undo [-s NAME] [-w DIR] [--force]
```

`ned help` prints a summary of the language, sized to fit in an agent's context.
`ned help TOPIC` details one verb (`show`, `outline`, `check`, `allow`,
`replace`, `insert`, `delete`, `sub`, `move`, `rename`, `file`, `create`), or
`selectors`, `text`, `config` or `session`. An unknown topic is a usage error
that lists the topics. A subcommand (`help`, `daemon`, `history`, `undo`) must
be the first argument; write a file with one of those names as `./help`, say.

- `-e SCRIPT` may be repeated; the scripts are joined with newlines, in order.
- Without `-e`, the script is read from stdin. If stdin is a terminal, that's a
  usage error instead of a wait for input.
- A `FILE` that doesn't exist but is a command's name (`ned outline a.rs`) is a
  usage error suggesting the `-e` form (`ned a.rs -e 'outline'`).
- `FILE...` sets the initial **file set** (§2.4). A script may also name files
  itself with `file`, so `FILE` arguments are optional. `-w` starts with every
  file in the workspace instead; giving both is a usage error.
- Files must be UTF-8. Line endings are detected per file (LF or CRLF), and
  inserted text is converted to match.

| Flag                      | Effect                                                                                                                                    |
| ------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| `-n`, `--dry-run`         | Resolve and apply edits in memory, print the output, write nothing.                                                                       |
| `-q`, `--quiet`           | Print only the per-file summary lines on success (§6.3).                                                                                  |
| `--force`                 | Skip the parse-error guard (§4.3) and blocking on introduced diagnostics (§6.5).                                                          |
| `--no-check`              | Don't check edits with language servers (§6.5).                                                                                           |
| `-w`, `--workspace [DIR]` | Start with every file in the workspace, `DIR` or the one detected (§1.1), instead of `FILE` arguments (§2.4).                             |
| `--no-fmt`                | Don't run formatters (§6.4).                                                                                                              |
| `--lang LANG`             | Use this language for every file: `rust`, `python`, `typescript`, `tsx`, `javascript`, `go`, `markdown`, or `text` to parse none of them. |
| `--context N`             | Context lines around diff hunks (default 1).                                                                                              |
| `--color WHEN`            | Colour output for a terminal: `auto` (default), `always` or `never` (§6.6).                                                               |
| `-s`, `--session NAME`    | Record the invocation in session `NAME`, overriding `NED_SESSION` (§1.2).                                                                 |
| `--commit MSG`            | Commit the edits the invocation writes, and nothing else, to git with message `MSG` (§1.3).                                               |
| `-V`, `--version`         | Print the version: the package version and the build's git commit (the version alone for a release build, or one built without git).      |

Otherwise, a file's language is detected from its extension, then from its
shebang (`python…` or `node`, directly or through `env`). Extensions are
case-sensitive:

| Language     | Extensions                    |
| ------------ | ----------------------------- |
| `rust`       | `.rs`                         |
| `python`     | `.py`, `.pyi`                 |
| `typescript` | `.ts`, `.mts`, `.cts`         |
| `tsx`        | `.tsx`                        |
| `javascript` | `.js`, `.mjs`, `.cjs`, `.jsx` |
| `go`         | `.go`                         |
| `markdown`   | `.md`, `.markdown`            |
| `text`       | `.txt`                        |

A file with any other extension, and no shebang `ned` knows, is read as `text`:
it isn't parsed, so line, regex and literal selectors work on it, but syntax
selectors, the parse-error guard (§4.3), formatting (§6.4) and language servers
(§6.5) skip it. A run that reads such files prints one note on stderr naming
their extensions, sorted, e.g.
`note: read .json, .toml files as text; syntax selectors skip them`. Files
without an extension, `.txt` files and runs with `--lang` print no note.

### 1.1 Daemon

Language-server features run through a daemon, one per workspace, that keeps the
servers warm between invocations. A workspace is the nearest directory, from the
working directory up, that holds `.git`, `.hg` or `.jj`; without one, it's the
working directory. `-w DIR` makes `DIR` the workspace instead, for the file set
and the daemon alike. `check`, `rename`, `.refs` and `.def` spawn the daemon on
demand; checking edits (§6.5) and formatting through a server (§6.4) use it only
if it's already running. It exits after 10 minutes without a request (see
`idle_timeout` below). Everything else works without it.

`ned daemon start`, `status` and `stop` manage the daemon for the workspace
containing `DIR` (default: the working directory). `start` spawns it if it isn't
running and prints its status; `status` and `stop` never spawn it.

```
$ ned daemon status
daemon for /home/me/proj: pid 4121, up 3m
  rust-analyzer: ready, 12 files
$ ned daemon stop
stopped the daemon for /home/me/proj
$ ned daemon status
no daemon for /home/me/proj
```

The daemon starts a language's server the first time a feature needs it, and
keeps each server's view of the files in sync with the text `ned` sends it.
`status` lists each server with its state (`indexing`, `ready` or `exited`) and
the number of files it has open. A server that exits is started again when next
needed. Servers are shut down with the daemon.

Servers are configured per language under `[lsp]`, like formatters (§6.4): the
same files, merging and program lookup, read from the workspace root up. A value
is a command as an argv array, or `false` for none; `timeout` sets how long any
request (`check`, edit checks, `rename`, `.refs`, `.def`, formatting) waits for
a server, in seconds (default 30). `[daemon]` sets `idle_timeout`, in seconds.
`[check]` sets `show`, the lowest severity `check` prints by default (`error`,
`warning`, `info` or `hint`; default `warning`), and `block`, the lowest
severity of an introduced diagnostic that rejects an edit (§6.5; default
`error`), or `false` for none.

```toml
[lsp]
python = ["basedpyright-langserver", "--stdio"]
markdown = false

[daemon]
idle_timeout = 1800

[check]
show = "hint"

```

| Language                    | Default server                       |
| --------------------------- | ------------------------------------ |
| rust                        | `rust-analyzer`                      |
| go                          | `gopls`                              |
| python                      | `pyright-langserver --stdio`         |
| typescript, tsx, javascript | `typescript-language-server --stdio` |
| markdown                    | none                                 |

A server that isn't installed is an error when a feature needs it, naming the
`[lsp]` setting to change.

The daemon listens on a Unix socket in `$XDG_RUNTIME_DIR/ned/`, or
`$TMPDIR/ned-UID/` without it (`/tmp/ned-UID/` if `TMPDIR` is unset too); `ned`
refuses a directory that isn't owned by the user or that others can access. Its
log is next to the socket. A daemon serves only the `ned` version that started
it. The daemon is Unix-only for now; on other platforms, features that need it
are errors.

### 1.2 Sessions

A **session** is a log of `ned` invocations in one workspace, kept so an agent
can review, repeat and undo its work, and so other frontends can follow it. With
`-s NAME`, or `NED_SESSION=NAME` in the environment, an invocation is recorded
in session `NAME`. `-s` overrides `NED_SESSION`, and an empty `NED_SESSION` is
no session. A name is letters, digits, `.`, `_` and `-`, and doesn't start with
`.`; another name is a usage error. Without a session nothing is recorded, and
nothing needs a daemon.

Every invocation that runs a script is recorded, including reads, dry runs and
failures (a script syntax error too), so a failed one can be repeated with a
fix. An entry holds the script, the `FILE` arguments or `-w` directory, the
working directory, the exit code and, for one that wrote files, each file's text
before and after. A successful `ned undo` records an entry too; a failed one
isn't recorded. Usage errors from the arguments themselves are not recorded.

`ned history` prints the session's last 10 entries, or with `--all` every entry,
oldest first, one line each: the entry's number, its outcome (`ok`, `dry run`,
`exit N`, or `undo N` for an undo), the number of files it changed, `commit SHA`
(its first 7 characters) if it made a commit (§1.3), `undone` if it has been,
and the script's first line, followed by `(+N lines)` if it has more.

```
$ ned history
1 ok: show fn:parse
2 exit 1: replace fn:prase>"end" with "end of input"
3 ok, 1 file, undone: replace fn:parse>"end" with "end of input"
4 undo 3, 1 file
```

`ned undo` reverts the files of the session's last entry that changed files and
isn't undone, and prints `undo N: SCRIPT` (the script's first line) followed by
the summary and hunks of each file it changed, as for an edit (§6.3). Repeated,
it walks further back; there is no redo. A file the entry created is removed,
and summarized as `PATH: removed, -N`. Undo writes files atomically and runs no
formatter, guard or check.

If a file no longer holds the text the entry wrote, `undo` is refused (exit 1),
naming the file. `--force` instead merges the undo into the file's current text,
reverting the entry's changes and keeping later ones; a later change that
overlaps one of the entry's is an error naming its line, and nothing is written.
A file removed since can't be undone, even with `--force`. A created file that
the merge leaves empty is removed.

A script that is `!!` repeats the session's last script (a recorded `!!` is
recorded as the script it expanded to). Modifiers after it correct the repeat,
applied in order:

- `:s/OLD/NEW/` replaces the first occurrence of `OLD` with `NEW`, both literal
  text; `:gs/OLD/NEW/` replaces every occurrence. Any punctuation character can
  stand for `/`, and `\` before it makes it literal. The last delimiter may be
  left off.

```
$ export NED_SESSION=agent
$ ned src/parser.rs -e 'replace fn:prase>"end" with "end of input"'
error: script:1:9: fn:prase matches nothing in src/parser.rs; did you mean fn:parse (14-17)?
$ ned -e '!!:s/prase/parse/'
note: repeating 1: replace fn:parse>"end" with "end of input"
src/parser.rs: 1 edit, +1 -1
...
```

The repeat runs on the last script's file set — its `FILE` arguments, relative
to the directory they were given in, or its `-w` workspace — unless the new
invocation gives `FILE` arguments or `-w`. Flags (`-n`, `--force`, ...) are not
repeated, so a repeat of a dry run without `-n` would apply what was only
previewed: it is a usage error saying to send the script again to apply it, or
to add `-n` to preview it again. `!!` without a session, with no earlier script
in it, or with an `OLD` the script doesn't contain is a usage error; the last
says what the script is.

`history` and `undo` take the session from `-s` or `NED_SESSION` like a script
does; with neither, or a session the workspace has no log for, they're a usage
error listing the workspace's sessions. A session belongs to the workspace of
the invocations it records (§1.1), so one recorded with `-w DIR` is reached with
`-w DIR`: `ned history -w DIR`, `ned undo -w DIR` and `ned -w DIR -e '!!'`.

Sessions live in `$XDG_STATE_HOME/ned/sessions/`, or `~/.local/state/ned/...`
without it, in a directory per workspace (§1.1) named after the root's last
component and a hash of its path. `ned` creates them private to the user and
refuses ones that aren't, like the daemon's directory. Session `NAME` is
`NAME.log` there, with `NAME.lock` beside it: `ned` holds an exclusive advisory
lock on that file while it reads or appends to the log (and throughout an undo),
so several frontends can share a session.

The log is JSON Lines. Its first line is a header,
`{"ned_session":1,"workspace":"/abs/root"}`, whose number is the format version;
`ned` refuses a log with a version it doesn't know (exit 3), suggesting a new
session name. Each later line is one entry, numbered from 1 in order. A last
line cut short by an interrupted append is ignored, and the next append replaces
it:

| Field       | Value                                                                                          |
| ----------- | ---------------------------------------------------------------------------------------------- |
| `id`        | The entry's number                                                                             |
| `time`      | Seconds since the Unix epoch                                                                   |
| `cwd`       | The absolute working directory                                                                 |
| `files`     | The `FILE` arguments as given                                                                  |
| `workspace` | The `-w` workspace's absolute root, or `null`                                                  |
| `script`    | The script (after `!!` expansion), or `null` for an undo                                       |
| `undoes`    | The `id` an undo reverted, or `null`                                                           |
| `dry_run`   | Whether `-n` was given                                                                         |
| `exit`      | The exit code                                                                                  |
| `error`     | The error message, or `null`                                                                   |
| `changes`   | Per written file: `path` (absolute), `before` (`null` if created), `after` (`null` if removed) |
| `commit`    | The commit that `--commit` made (§1.3), or `null`; read as `null` if missing                   |

### 1.3 Committing

`--commit MSG` turns the edits an invocation writes into one git commit with
message `MSG`, on top of `HEAD`, and moves `HEAD` (or the branch it names) to
it. Nothing else goes into the commit: `ned` builds it from `HEAD`'s tree with
each edit applied, not from the index, so other changes, staged or not, are left
as they were, in the files and in the index.

- A file's edit is the change from its text before the script to the text
  written, formatting included. If the file has changes that aren't committed,
  the edit is applied to `HEAD`'s version for the commit (to the staged version,
  for a file `HEAD` lacks), and to the staged version in the index, so those
  changes stay uncommitted. An edit that overlaps them is an error naming the
  line (exit 1).
- A file the script creates is added. The repository is the one holding the
  first file the script edits, wherever `ned` runs; every edited file must be in
  it (not in a repository nested in its tree, or a submodule) and not ignored by
  git.
- With a session (§1.2), the commit holds every edit the session recorded since
  its last commit (its last entry with a `commit`), undos included, as well as
  the invocation's own: each file's edits are applied one after another, in
  order. A file that an undo removed is removed from the commit too, and an
  untracked file that its edits leave as they found it stays out of it,
  untracked. The invocation's entry records the commit. A script that changes no
  file is still "nothing to commit", whatever edits the session made before it.
  Each file those earlier edits wrote must still hold what the session last
  wrote to it, and be in the repository and not ignored. If one has changed
  since (by `git checkout`, `git stash` or a hand edit, say), or isn't in the
  repository or is ignored, the commit is refused (exit 1), naming the file and
  the entry that wrote it; starting a new session commits only the edits from
  then on.
- The commit is made with git's plumbing (`commit-tree`): hooks don't run, it is
  signed if `commit.gpgSign` says so, and its author and committer come from
  git's configuration and environment, as for `git commit`.
- After the summaries (§6.3), `ned` prints `commit SHA: SUBJECT`, with the
  commit's abbreviated name and its message's first line.
- `--commit` with `-n`, without git, or outside a git repository is a usage
  error (exit 2). A merge in progress, a script that changes no file or nothing
  that differs from `HEAD` ("nothing to commit"), a file outside the repository
  or ignored, a file whose version in `HEAD` or the index isn't UTF-8, and an
  overlap are rejected (exit 1). A failing git command is an I/O error (exit 3),
  and so is an index another git process has locked (`index.lock` exists): `ned`
  holds that lock from before it reads the index until it has staged the edits.
  In every case, nothing is written, `HEAD` doesn't move and the index is
  unchanged; if `HEAD` moved while the script ran, the commit is refused.

```
$ ned src/parser.rs --commit "Say what input ended" -e 'replace fn:parse>"end" with "end of input"'
src/parser.rs: 1 edit, +1 -1
@@ -14,3 +14,3 @@
...
commit 3f9c2a1: Say what input ended
```

## 2. Scripts

### 2.1 Lexical structure

- **Commands** are separated by newlines or `;`.
- **Stages** are separated by `|` (§2.3). `|` binds more loosely than `;` and
  newlines, the opposite of a shell: `a; b | c` is the stage `a; b`, then the
  stage `c`. A line may end with `|`; the next stage starts on the next line. A
  `|` needs a command on each side.
- **Comments** start with `#` at the start of a token and run to the end of the
  line. A `#` inside a string, regex, query, pattern, or heredoc body is
  literal.
- **Strings** are `"..."`, on a single line, with the escapes `\n`, `\t`, `\"`
  and `\\`. Any other backslash sequence is an error.
- **Regexes** are `/.../FLAGS`, using Rust [`regex`] syntax. Write `\/` for a
  literal slash. Multi-line mode is always on (`^` and `$` match at line
  boundaries). The flags are `i` (case-insensitive) and `s` (`.` matches
  newline).
- **Heredocs** are written `<<TAG` or `<<'TAG'`, where `TAG` is an identifier.
  The body starts on the line after the command. It ends at the first line whose
  content, with surrounding whitespace trimmed, is `TAG`. When one line holds
  several heredocs (in one command or across `;`-separated commands), their
  bodies follow in order, as in the shell. The heredoc's value is its body lines
  joined with `\n`, with no final newline. A body reads no escapes: `\x27` is
  four characters.
  - `<<TAG` bodies are **re-based** (§5) when inserted, and match
    indentation-insensitively when used as selectors (§3.2).
  - `<<'TAG'` bodies are **verbatim**: no re-basing, and exact matching.
- **Patterns** are code between backquotes (§3.10), and may span lines. As in
  Markdown, a pattern holding backquotes opens and closes with a longer run of
  them: ``` `` `${x}` `` ```. The code inside is verbatim, backslashes included.
- **Filters** are `[...]` directly after a step (§3.9). Inside the brackets,
  whitespace is allowed, digits are numbers, and `==`, `!=`, `~=`, `<`, `>`,
  `<=`, `>=`, `&&`, `||`, `(` and `)` are operators.
- **Keywords**: the verbs, `all`, `with`, `to`, `before`, `after`, `start`,
  `end`, and `file`; after `check`, its levels (`error`, `warning`, `info`,
  `hint`), and after `allow`, `errors` and `warnings`. Any other bare word is an
  error.

[`regex`]: https://docs.rs/regex/latest/regex/#syntax

### 2.2 Grammar

```ebnf
script     = { line } ;
line       = [ command { ( ";" | "|" ) command } [ "|" ] ] [ comment ] NEWLINE
             { heredoc-body } ;
command    = show | outline | replace | insert | delete | sub | move | create
           | file | check | allow | rename | resolve ;

show       = "show" [ target [ context ] ] ;
outline    = "outline" [ target ] ;
replace    = "replace" target "with" text ;
insert     = "insert" position target text ;
delete     = "delete" target ;
sub        = "sub" [ target ] regex "with" text ;
move       = "move" target position selector ;
file       = "file" path { path } ;
create     = "create" path text ;
rename     = "rename" selector "to" name ;
resolve    = "resolve" target ( "ours" | "theirs" | "base" | "both" ) ;
check      = "check" [ target ] [ level ] ;
level      = "error" | "warning" | "info" | "hint" ;
allow      = "allow" ( "errors" | "warnings" ) ;

position   = "before" | "after" | "start" | "end" ;
target     = [ "all" ] selector ;
selector   = step { ">" step } ;
step       = primary [ ".." primary ] { part } { filter } ;
context    = "+" digit { digit } ;
part       = ".body" | ".sig" | ".params" | ".name" | ".doc" | ".attrs"
           | ".ret" | ".type" | ".value" | ".whole" | ".lines" | ".refs"
           | ".def" | ".ours" | ".theirs" | ".base" ;
filter     = "[" or "]" ;   (* whitespace allowed inside *)
or         = and { "||" and } ;
and        = cond { "&&" cond } ;
cond       = property op value | "(" or ")" ;
property   = ".len" | ".text" | part [ ".len" | ".text" ] ;
op         = "==" | "!=" | "~=" | "<" | ">" | "<=" | ">=" ;
value      = string | regex | number ;
number     = digit { digit } ;
primary    = lines | regex | literal | syntax | conflict | query | pattern ;

lines      = lineno [ "-" lineno ] ;
lineno     = digit { digit } | "$" ;
literal    = string | heredoc ;
syntax     = kind [ ":" name ] ;
kind       = ident ;
name       = name-char { name-char } | string ;   (* name-char: [A-Za-z0-9_:*] *)
conflict   = "conflict" [ ":" number ] ;
query      = "query{" { any } "}" ;   (* ends at the first unescaped "}"; one line *)
pattern    = "`"+ { any } "`"+ ;   (* runs of equal length; may span lines; see 3.10 *)

text       = string | heredoc ;
path       = path-char { path-char } | string ;   (* globs allowed; see 2.4 *)
string     = '"' { char | escape } '"' ;
regex      = "/" { regex-char | "\/" } "/" { "i" | "s" } ;
heredoc    = "<<" ( ident | "'" ident "'" ) ;
comment    = "#" { any-but-newline } ;
```

No whitespace is allowed inside a selector, except within a filter's brackets or
a pattern's backquotes: `impl:Parser>fn:new.body` is one selector, while
`impl:Parser > fn:new` is a syntax error.

In `sub`, the last primary before `with` is the pattern. A preceding selector,
if present, is the scope: `sub fn:parse /x/ with "y"`.

### 2.3 Snapshot semantics

A script is one transaction. `|` splits it into **stages**, each over a snapshot
of its files as the stage before left them; a script without `|` is one stage
over the files as they are:

1. Every selector resolves against the contents at the start of its stage (the
   original contents, in the first stage). Line numbers printed by an earlier
   `show` in the same stage stay valid for the whole stage.
2. Each edit becomes one or more replacements of a byte span in those contents.
3. Two edits in one stage whose spans overlap are an error that names both
   commands. Insertions at the same point are allowed, and apply in command
   order. An insertion exactly at the boundary of a replaced or deleted span is
   also allowed. Whole-line deletions (by `delete` or `move`) that only blank
   lines separate are merged into one, so neighbouring items can be deleted
   together.
4. At the end of each stage, its edits are applied in one pass and the
   parse-error guard (§4.3) checks them. After the last stage the formatters
   (§6.4) run, and every modified file is written atomically, or none is.
5. Reads (`show`, `outline`) display the contents at the start of their stage.

So a command can't target text that another command in the same stage inserts;
put it after a `|`: `create src/lexer.rs <<END | insert after struct:Lexer ...`
or `rename fn:new to create | show fn:create`.

After the first `|`, the files on disk no longer hold the stage's contents, so
the commands that read them or ask a language server about them are a syntax
error there: `check`, `rename`, and selectors with `.refs` or `.def`. Run them
before the first `|`, or in a separate `ned` call.

### 2.4 File set

Commands apply to the **current file set**. It starts as the `FILE` arguments,
or with `-w`, every file in the workspace. `file PATH...` replaces it for the
commands that follow. `file` can't create files.

Files are read when a command first needs them, so a large set costs nothing
until a selector searches it. A `FILE` or `file` path that doesn't exist is
still an error at once; one that isn't UTF-8 is an error from the first command
that reads it.

With `-w`, the workspace's files are its regular files, except those that
`.gitignore`, `.ignore` or git's excludes leave out (inside a git repository or
not) and hidden files and directories. Files that aren't UTF-8 are skipped
silently. The set is in path order; paths are shown relative to the working
directory when they're inside it, and absolute otherwise.

`ned` expands globs in `FILE` arguments and `file` paths itself, relative to the
working directory, so a quoted glob works too:

- `*`, `?`, `[...]` and `**` (any number of directories) are supported. A
  leading `.` in a name must be matched literally, and only files match.
- Each glob's matches are sorted. A file named twice is in the set once, at its
  first position.
- A glob that matches nothing is an error (exit 3):
  ``error: glob `src/*.rx` matched nothing``. So is a plain path that doesn't
  exist.

A selector resolves against every file in the current set, and the ambiguity
rules (§3.5) count matches across all of them. Use a `file:` step to narrow a
selector to one file (§3.3).

## 3. Selectors

A selector resolves to a set of **spans** (byte ranges) in one or more files.

### 3.1 Lines

| Selector | Selects            |
| -------- | ------------------ |
| `12`     | line 12            |
| `12-20`  | lines 12 to 20     |
| `12-$`   | line 12 to the end |
| `$`      | the last line      |

Line numbers are 1-based. A line selector covers whole lines, including their
line endings. A range whose start is after its end, or a line past the end of
the file, is an error. Numbers count from the top of the file even in a nested
step, but there `$` is the last line of the enclosing span: `fn:parse>$` is the
function's closing line.

### 3.2 Regex and literal

- `/re/` selects every span that matches `re`.
- `"text"` selects every exact occurrence of `text`, which may span lines via
  `\n`.
- A `<<TAG` heredoc selects every run of **whole lines** that equals the body
  after one common indentation prefix is added to each non-blank line. The
  body's own common indentation is stripped first, so the agent doesn't have to
  reproduce indentation. A `<<'TAG'` heredoc matches its body exactly, as whole
  lines.

Matches are non-overlapping and found left to right.

In a CRLF file, a line break in a string or heredoc selector matches `\r\n`.
Regexes see the file's raw text.

### 3.3 Syntax

`kind:name` selects items of a syntax kind by name. Names are matched exactly.
`*` matches any run of characters (`fn:test_*`, `fn:*`). A name that has other
characters, such as `.` or `-`, must be quoted: `import:"os.path"`. A kind
without `:name` selects every item of the kind: `fn` is `fn:*`, so
`impl:Parser>fn` is every method of `Parser`.

Core kinds. Each language maps a subset of these through
`queries/<lang>/selectors.scm` (JavaScript and TypeScript both start with
`queries/ecma/selectors.scm`):

| Kind        | Items                                                              |
| ----------- | ------------------------------------------------------------------ |
| `fn`        | functions and methods                                              |
| `class`     | classes                                                            |
| `struct`    | structs                                                            |
| `enum`      | enums                                                              |
| `variant`   | enum variants                                                      |
| `trait`     | traits                                                             |
| `interface` | interfaces                                                         |
| `impl`      | impl blocks (name = self type, or `TRAIT for TYPE`; see below)     |
| `type`      | type aliases and declarations                                      |
| `const`     | constants and statics                                              |
| `var`       | module-level variables and `let`/`var` bindings                    |
| `field`     | struct and class fields                                            |
| `mod`       | modules and namespaces                                             |
| `import`    | imports (name = the path as written, e.g. `import:std::fmt`)       |
| `section`   | Markdown sections: a `#` heading and its content (name = its text) |
| `item`      | Markdown list items (name = the first line of the item's text)     |
| `table`     | Markdown tables (name = the first header cell)                     |
| `code`      | Markdown code blocks (name = the language tag, or `""` if none)    |

The kinds each language supports, and the items they cover there:

- **Rust**: `fn` (also trait method declarations), `struct`, `field`, `enum`,
  `variant`, `trait`, `impl`, `type` (also associated types), `const` (also
  `static`), `var` (`let` bindings), `mod`, `import` (`use`).
- **Markdown**: `section`, `item`, `table`, `code`.
- **Python**: `fn` (functions and methods, `async` too), `class`, `field`
  (assignments and annotations directly in a class body), `const` (module-level
  assignments to an `UPPER_SNAKE` name), `var` (other module-level assignments),
  `import` (named by the module: `import:"os.path"`; `from a.b import c` is
  `import:"a.b"`).
- **Go**: `fn` (functions, interface methods, and methods, named
  `RECEIVER.NAME`: `fn:"Parser.Parse"`; `fn:Parse` matches every `Parse`),
  `struct`, `interface` and `type` (other type declarations and aliases),
  `field` (named by its first name; an embedded field by its type), `const`,
  `var`, `import` (named by the path without quotes: `import:fmt`,
  `import:"net/http"`). In a grouped declaration (`const ( ... )`) each spec is
  an item; a declaration with one spec includes its keyword. Every comment
  directly above an item is its doc.
- **JavaScript** and **TypeScript** (TSX too): `fn` (function declarations,
  generators, class methods, and a variable declared as an arrow function or
  function expression, which is also `const` or `var`; `outline` lists it as
  `fn`), `class` (abstract too), `field` (class fields), `const` (`const`
  declarations), `var` (`let` and `var`), `import` (named by the source without
  quotes: `import:react`, `import:"./util"`). TypeScript adds `interface` (with
  `fn` and `field` members), `type`, `enum` with its `variant`s, `mod`
  (namespaces and `declare module`), and `fn` signatures without a body
  (overloads, `declare function`, abstract methods). An exported item's span and
  `.sig` include `export`; a `/** ... */` comment directly above an item is its
  doc.

- An inherent impl is named by its self type (`impl:Parser`), a trait impl by
  `TRAIT for TYPE` (`impl:"Display for Parser"`), each the last path segment
  without generic arguments (`impl<T> fmt::Display for Foo<T>` is
  `"Display for Foo"`). `impl:TYPE` also matches every trait impl of the type.
- A syntax step skips files whose language doesn't support its kind, as it skips
  text files, so `fn:parse` works in a set that also holds Markdown. If no
  searched file supports the kind, the step is an error that lists the kinds the
  first such language does support.
- A syntax item's default span is the **whole item as a reader sees it**,
  including its leading doc comments and attributes or decorators, up to the
  first blank line above it. A `,` directly after the item, on the same line, is
  part of the span too (fields, variants). When `replace` targets such an item
  and `TEXT` doesn't end with `,`, one is appended.
- Syntax steps skip text files. If every searched file is text, the selector is
  an error that suggests `--lang`.
- A file with merge conflicts still parses. The marker lines of each conflict
  that git writes are hidden from the grammar: `<<<<<<< ...`, `||||||| ...`
  (diff3 style), `=======` and `>>>>>>> ...`, each starting its line, in that
  order. The grammar sees the sides one after the other, so items on either side
  are found, and an item on both sides matches twice, which is ambiguous (§3.5)
  unless it is scoped by its side: `conflict:1.ours>fn:a`. Spans are still of
  the file's text, so an item that spans a conflict includes its marker lines.
  Marker lines that don't form a whole conflict (a lone `=======`, say) are not
  hidden. `conflict` selects the conflicts themselves (§3.11).
- `file:PATH` is a special step that selects the whole of one file in the
  current set. It exists to scope the steps after it:
  `file:src/lexer.rs>fn:new`. `PATH` may contain `/` and `.`, and ends at `>` or
  whitespace.

### 3.4 Nesting and parts

- `A>B` resolves `B` within each span of `A`. That is, `B`'s matches must lie
  inside `A`, at any depth. A match lies inside a span that covers whole lines
  (such as a syntax item) if it lies within those lines, so `fn:new>12` can
  select the item's first or last line, `fn:new>"    fn new"` can include its
  indentation, and `^` in a nested regex is a line start; `sub`'s scopes work
  the same way. Any kinds of primaries can be mixed: `impl:Parser>fn:new`,
  `fn:main>/unwrap\(\)/`, `100-200>fn:new`.
- A **part** narrows each span of its step:

| Part                        | Span                                                                                                                             |
| --------------------------- | -------------------------------------------------------------------------------------------------------------------------------- |
| `.body`                     | the item's block, between its delimiters (`{}`, or a Python indented block); for a Markdown section, the lines after its heading |
| `.sig`                      | from the start of the item (after its doc and attributes) up to its body                                                         |
| `.params`                   | the parameter list, between its parentheses                                                                                      |
| `.name`                     | the item's name identifier                                                                                                       |
| `.doc`                      | the item's leading doc comment lines; a Python docstring                                                                         |
| `.attrs`                    | the item's attributes or decorators, from the first to the last                                                                  |
| `.ret`                      | the function's return type                                                                                                       |
| `.type`                     | the declared type of a field, constant or variable                                                                               |
| `.value`                    | the value a constant, variable, field or variant is given; the type a `type` item names                                          |
| `.whole`                    | the item's default span (§3.3), doc comments, attributes and trailing `,` included, which `replace` replaces whole               |
| `.lines`                    | each whole line the span touches, as a span of its own (any selector)                                                            |
| `.refs`                     | each reference to the symbol at the span (below), without its declaration                                                        |
| `.def`                      | the symbol's definition: the item it names, or its identifier if it names no item                                                |
| `.ours`, `.theirs`, `.base` | a conflict's sides (§3.11)                                                                                                       |

- For `.body` and `.params`: if the opening delimiter ends its line and the
  closing delimiter starts its line, the part is the whole lines between them.
  Otherwise it's the text between the delimiters, with surrounding whitespace
  trimmed.
- A Markdown section's `.body` runs from the first non-blank line after its
  heading to the end of its content, subsections included, so
  `insert end section:"3. Selectors"` adds after the last subsection. It's empty
  if the heading has no content; text put there goes on the lines right after
  the heading. `ned` adds no blank lines between Markdown blocks: put them in
  the text. `.sig` is the heading line.
- `.doc` covers whole lines. On an item with no body (such as a trait method
  declaration), `.sig` is the whole item.
- `.attrs` covers whole lines when its attributes stand on lines of their own,
  as `.body` does; otherwise it's the text from the first attribute to the end
  of the last. Doc comments between attributes are inside it.
- `.ret`, `.type` and `.value` leave out the punctuation before them (`->`, `:`,
  `=`) and cover the type or expression as written; a Go `.ret` keeps its
  parentheses (`(int, error)`). An item without one, such as a Rust `fn` that
  returns `()` or a field without an initializer, doesn't have the part. Where
  each one exists:
  - **Rust**: `.ret` on `fn`; `.type` on `field`, `const` and `var`; `.value` on
    `const`, `var`, `type` and a `variant` with a discriminant; `.attrs` on any
    item.
  - **Python**: `.ret` on `fn`; `.type` and `.value` on `field`, `const` and
    `var`; `.attrs` (decorators) on `fn` and `class`.
  - **Go**: `.ret` on `fn`; `.type` on `field`, `const` and `var`; `.value` on
    `const`, `var` and `type`.
  - **JavaScript** and **TypeScript**: `.value` on `const`, `var` and `field`;
    `.attrs` (decorators) on `class`, `fn` and `field`. TypeScript adds `.ret`
    on `fn`, `.type` on `const`, `var` and `field`, and `.value` on `type` and
    `variant`.
- In Python, `.doc` is the docstring, and `.body` is the block's whole lines
  after it, so `insert start fn:f` goes after the docstring. `.sig` runs from
  `def` or `class` up to, but not including, the `:` (decorators are
  attributes).
- `.refs` and `.def` ask the language server (§1.1) about the symbol at the
  start of the step's `.name` (for a syntax item) or of its span, so they work
  on any step: `fn:parse.refs`, `fn:main>"helper(".def`. Their spans may be in
  other files: in the file set, or with `-w` in any workspace file, which then
  joins it; any other file is an error. Only `.lines` may follow them in the
  same step, and later steps search inside their spans. They spawn the daemon if
  need be.
- A part the item doesn't have (e.g. `.body` on a Rust `const`) is an error.
  Parts other than `.lines` need a syntax item, and `.ours`, `.theirs` and
  `.base` a conflict (§3.8): `/x/.body` is an error, and so is a part after
  another part, as in `.body.name`.
- `.lines` on a span within one line widens it to that line. On a multi-line
  span it selects each line separately, so `fn:parse.lines` is many spans: use
  `all`, or nest a line number (`fn:parse>12`).

### 3.5 Ambiguity and `all`

- Without `all`, the final result of a selector must be **exactly one** span.
  Zero matches is an error, and so are two or more.
- Intermediate steps of a nested chain may match many spans; only the final
  result is counted.
- `all SEL` applies the verb to every match. Zero matches is still an error.
- An ambiguous `.refs` or `.def` result lists where its matches are instead of
  candidate selectors, since no scope picks one out; add `all`.
- There is no nth-match syntax, except for conflicts (§3.11). To disambiguate,
  nest (`impl:Lexer>fn:new`), scope by lines (`40-80>fn:new`), or scope by file
  (`file:src/a.rs>fn:new`). Error messages list the candidates in exactly these
  forms (§7): nested in the match's nearest enclosing item (within the previous
  step's span) if that's unique among the matches, otherwise scoped by file if
  that's unique, otherwise by lines. A `file:` scope goes first; an item or line
  scope goes just before the selector's last step
  (`impl:Lexer>fn:new>40-44>/x/`), and a line scope covers the whole matched
  item, even when a part follows it.
- Every listed candidate picks exactly one match. Matches that share a line with
  another match can't be picked by scope, so they aren't listed; the error
  counts them and suggests selecting longer text, or `all`.
- When the last step is a syntax step with a `*` in its name, each candidate
  names its item instead (`fn:test_*` lists `fn:test_parse`), and is scoped as
  above only among the matches with the same name.
- A line that `.lines` split from a multi-line span is listed as its step
  without `.lines` and any parts or filters after it, with its line number
  nested after the step: `fn:new.lines` lists `fn:new>41`, `fn:new>42`, and so
  on.
- A candidate keeps its steps' filters (§3.9), also when it names the item:
  `fn[.doc == ""]` lists `fn:parse[.doc == ""]`.

### 3.6 Raw query

`query{...}` runs a tree-sitter query against each file's grammar, so it works
in every language `ned` detects. The span is taken from the `@sel` capture if
there is one, and otherwise from the outermost capture of each match. Text files
are skipped, as for syntax steps. A query that doesn't compile is a script error
(exit 2). Write `\}` for a literal `}` in the query. The query must fit on one
line.

```
delete all query{(call_expression function: (identifier) @f (#eq? @f "dbg")) @sel}
```

### 3.7 Ranges

`A..B` selects from the start of a match of `A` to the end of the first match of
`B` that starts after it: `/^## 6/../^## 7/`, `fn:a..fn:c`, `"BEGIN"..$`.

- Both ends are primaries, without parts. `..` binds tighter than `>`, and parts
  apply to the whole range: `impl:Parser>fn:new..fn:parse`.
- A range covers whole lines: from the start of `A`'s first line to the end of
  `B`'s last line, so `/^## 6/../^## 7/` includes the `## 7` heading line. Two
  ranges on one line are one span.
- Matches of `A` inside an earlier range are skipped. A match of `A` with no `B`
  after it ends the search.

### 3.8 Types

Every value in a script has a type, which decides the parts it has (§3.4):

- A **span** is a range of a file's text, as a selector resolves to. It is
  **single-line** if it lies within one line, not counting a line break at its
  end, and **multi-line** otherwise. Every span has `.lines`.
- An **item** is a span that a syntax step selects, with its kind (§3.3). It
  also has `.whole` and the parts its kind has in its language (§3.4). A part's
  span is a plain span, not an item.
- A **conflict** is a span that a `conflict` step selects (§3.11). It has
  `.ours`, `.theirs`, `.lines` and, in diff3 style, `.base`.
- **Strings**, **numbers** and **regexes** are values written in a script: a
  filter (§3.9) compares a span's property with one. They have no parts.

### 3.9 Filters

`[...]` after a step keeps each of the step's spans for which a condition holds:
`fn[.name ~= /^test_/]` is every test function, `fn[.doc == ""]` every
undocumented one, and `fn:parse.lines[.len > 80]` each long line of `parse`. A
filter follows the step's parts and tests the spans they select.

- A condition compares a **property** of the span with a value:
  - `.text`: the span's text, without a final line break.
  - `.len`: on a single-line span, the number of characters in `.text`; on a
    multi-line span, its number of lines (§3.8).
  - A part (§3.4), such as `.name` or `.doc`: the part's text. Add `.len` or
    `.text` after it for its length or text: `fn[.body.len > 50]`. An item
    without the part, such as a function without doc comments, has `""` there,
    with length 0. A part needs an item (or a conflict, for its sides):
    `/x/[.name == "a"]` is an error, as `/x/.name` is. `.lines`, `.refs` and
    `.def` can't be properties.
- `.len` is a number, compared with `==`, `!=`, `<`, `>`, `<=` or `>=` and a
  number. Text is compared with `==` or `!=` and a string, or with `~=` and a
  regex, which matches anywhere in it unless anchored (`~= /^test_/`). Any other
  pairing is a script error (exit 2).
- `&&` and `||` combine conditions, `&&` binding tighter, and parentheses group
  them: `fn[.name ~= /^test_/ && (.doc == "" || .body.len > 50)]`. Several
  filters must all hold: `fn[A][B]` is `fn[A && B]`.
- A span the filter rejects isn't a match: it doesn't count toward ambiguity
  (§3.5), and a filter that rejects every span is a no-match error.

### 3.10 Syntax patterns

A pattern is code between backquotes. `ned` parses it with each file's grammar
and matches it against the file's syntax tree, so `` `foo(@a, 1)` `` matches
every call of `foo` whose second argument is `1`, however the call is spaced,
wrapped or commented. Whitespace and comments matter only where the grammar
makes them matter, as Python's indentation does.

```
show all `dbg!(@x...)`
delete all fn:main>`println!(@_...);`
show `impl Display for @t { @_... }`>fn:fmt
```

**Writing.** A pattern may span lines, and its code is verbatim: `` `"\n"` `` is
the source text `"\n"`. A pattern that holds backquotes (a JavaScript template
literal, a Go raw string) opens with a run of backquotes longer than any inside
it, and closes with a run of the same length, as in Markdown:
``` `` `${name}` `` ```. One space just inside each end is dropped when both
ends have one, so a pattern can start or end with a backquote. Indentation
common to the pattern's lines is ignored.

**Placeholders** stand for the parts of the code that vary:

| Placeholder      | Matches                                         | Captures |
| ---------------- | ----------------------------------------------- | -------- |
| `@name`          | any one node: an expression, statement, name... | yes      |
| `@name...`       | a run of zero or more sibling nodes             | yes      |
| `@_` and `@_...` | the same                                        | no       |
| `@@`             | a literal `@`                                   | -        |

- A name is a letter or `_` followed by letters, digits and `_`. An `@` before
  anything else is literal: `a @ b`.
- A placeholder must stand for a whole node. One inside a string or comment is
  literal text: `` `log("user@host")` ``. Anywhere else, as part of a keyword or
  operator, it's a script error (exit 2).
- Python and TypeScript decorators start with `@`, so double it to match one:
  `` `@@app.route(@path)` ``.
- `@name...` matches as few siblings as it can while the rest of the pattern
  still matches: in `` `foo(@first, @rest...)` ``, `@first` is the first
  argument and `@rest` the others.
- A name used twice in a pattern matches only equal code, ignoring whitespace
  and comments: `` `@x == @x` ``.
- A Rust macro's arguments are tokens, not expressions, so a placeholder there
  matches one token; write a run for an argument: `` `dbg!(@x...)` ``.
- A run may stand where no name would parse, as among an impl's items:
  `` `impl Display for @t { @_... }` ``.
- A run right after a node, where no name would parse, also matches the rest of
  a longer node that starts with that node, such as the tail of a method chain:
  `` `let n = items[i] @rest...;` `` matches `let n = items[i].iter().count();`,
  and `@rest` captures `.iter().count()`.

**Parsing.** Many fragments only parse inside some other code: a method inside
an `impl` or a class, a match arm inside a `match`, a field inside a struct.
`ned` parses the pattern alone, then inside each such construct its language
defines, and keeps every reading that parses without errors. The pattern matches
any of them: `` `x: u32` `` matches a struct field or a parameter.

- A file whose language can't parse the pattern is skipped, and so are text and
  Markdown files. If no searched file's language parses the pattern, it's a
  script error (exit 2) that shows where parsing failed.
- The pattern's root is the deepest node spanning all of its code, so
  `` `foo(@a)` `` is a call and matches calls in expressions as well as in
  statements. A pattern of several statements or items matches any run of
  consecutive siblings.

**Matching.** Two nodes match when they have the same kind and their children
match in order; leaves (names, numbers, string contents) must have the same
text. Comments are skipped on both sides. Separators in the file that the
pattern leaves out (`,`, `;` and line breaks) are skipped too, so
`` `foo(@a, @b)` `` matches `foo(x, y,)`. Every other token and node must match:
`` `@a + @b` `` doesn't match `x - y`, `` `fn f(self) {}` `` doesn't match
`fn f(&self) {}`, and `` `fn @name() {}` `` doesn't match `pub fn f() {}`.

**Matches.** A match is a span (§3.8) from the start of the matched node or run
to its end, with `.lines` but no other parts. It works as any step, like
`query{}`, and its ambiguity and candidates are a literal's (§3.5). Matches are
found in source order and don't overlap: a match inside an earlier one is
skipped, so `foo(foo(1))` matches `` `foo(@a)` `` once.

**Captures.** Each named placeholder captures what it matched, with the comments
between it and its neighbours, so `replace` keeps them. A span's captures come
from every pattern step of its selector, range ends included, so each name may
appear in only one step; `` `impl @t { @_... }`>fn:new `` captures `@t` for each
`new`.

**Substitution.** When `replace`'s target has pattern steps, `@name` in `TEXT`
expands to the selected span's capture (`@name...` is the same), and `@@` to
`@`. An `@name` that nothing captured is a script error.

- A capture spanning several lines keeps the indentation of its later lines
  relative to its first, under the indentation of the `TEXT` line where `@name`
  sits.
- `TEXT` is then line-oriented or verbatim, and re-based, as usual (§5).
- Without pattern steps, `replace` expands nothing.

```
replace all `assert_eq!(@a..., true)` with "assert!(@a)"
replace fn:load>`if let Some(@x) = @e { @body... }` with <<END
let Some(@x) = @e else {
    return;
};
@body
END
```

### 3.11 Conflicts

`conflict` selects the merge conflicts that git leaves in a file, in any file
whatever its language, text files included. `conflict:N` is the file's `N`th
conflict, counting from 1 in file order, and bare `conflict` is each of them, so
it is ambiguous (§3.5) in a file with more than one unless `all` is given;
candidates are listed as `conflict:N`. A conflict's span is the whole lines from
its `<<<<<<<` line through its `>>>>>>>` line. Only whole conflicts count, as
for hiding markers from the grammar (§3.3), wherever they are: conflict-shaped
text inside a Markdown code fence or a multi-line raw string is a conflict, as
it is for git, so editing `all conflict` rewrites it too. Its parts are its
sides:

- `.ours`: the lines after `<<<<<<<`, up to `|||||||` or `=======`.
- `.base`: the lines after `|||||||`, up to `=======`. Only a conflict that git
  wrote in diff3 style, with `merge.conflictStyle` set to `diff3` or `zdiff3`,
  has one; on another, `.base` is an error that says so, and that
  `git checkout --conflict=diff3 -- FILE` (or `zdiff3`) rewrites the file's
  conflicts with bases, undoing its edits since the merge.
- `.theirs`: the lines after `=======`, up to `>>>>>>>`.

Each side is whole lines, without the marker lines; an empty side is an empty
span at the start of the marker line after it. It has no `.lines`; `show` prints
`PATH:N: conflict:2.theirs is empty` for it, and `delete` makes no edit, with a
note that it is already empty. `replace`, `insert` and `move` put their text on
lines of their own there. Conflicts nest like other spans:
`conflict:2.theirs>fn:parse`, `fn:main>conflict`. Numbers count every conflict
in the file, not only those in scope: `fn:main>conflict:2` is the file's second
conflict, which must lie in `fn:main`. `resolve conflict:2 theirs` (§4.2) keeps
one side, and `replace conflict:2 with TEXT` replaces the whole conflict,
markers included, with new text.

```ned
show conflict:1.theirs
resolve conflict:1 theirs
replace conflict:2 with <<END
let limit = config.limit.max(1);
END
```

## 4. Verbs

### 4.1 Reads

- **`show [SEL [+N]]`** prints the lines containing each selected span, numbered
  (§6.1), with `N` lines of context around each. Without a selector, it prints
  each whole file in the set.
- **`outline [SEL]`** prints the symbol tree (§6.2) of each file, or of the
  items inside `SEL`.
- **`check [SEL] [LEVEL]`** prints the language server's diagnostics for each
  file in the set, or those overlapping `SEL`'s spans, at `LEVEL` or above
  (`error`, `warning`, `info` or `hint`; default `[check] show`, §1.1). Like
  every read, it sees the original text (§2.3). It starts the daemon and the
  servers if needed and waits for them to finish indexing, up to
  `[lsp] timeout`. It also waits for the checks a server runs when a file is
  saved, such as rust-analyzer's `cargo check`, which find errors the server
  alone doesn't (unresolved names, borrow errors). If those don't finish in
  time, it prints what the server has reported, with a note on stderr:
  `note: rust-analyzer's check on save didn't finish within 30s; raise [lsp] timeout`.
  Files without a server are skipped; if no file in the set has one, it's an
  error naming the `[lsp]` setting.

  ```
  src/parser.rs:15:9: error: mismatched types [rust-analyzer E0308]
  src/parser.rs:21:5: warning: unused variable: `tok` [rust-analyzer]
  ```

  Lines are in file order, then by position; the column counts characters from
  1. Further lines of a message are indented by two spaces. With nothing to
     print, the output is `no diagnostics at warning or above`.

### 4.2 Edits

| Verb                                      | Effect                                                                                                                                                                                               |
| ----------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `replace SEL with TEXT`                   | Replaces each selected span with `TEXT`.                                                                                                                                                             |
| `insert before\|after SEL TEXT`           | Inserts `TEXT` just before or after each span.                                                                                                                                                       |
| `insert start\|end SEL TEXT`              | Inserts `TEXT` inside each span, at its start or end. On a syntax step with no part, `.body` is implied: `insert end impl:Parser`.                                                                   |
| `delete SEL`                              | Removes each span.                                                                                                                                                                                   |
| `sub [SEL] /re/ with TEXT`                | Replaces every match of `re` inside each span of `SEL` (default: each whole file in the set). `$1`, `${name}` and `$0` expand to captures; `$$` is a literal `$`. Zero matches in total is an error. |
| `move SEL before\|after\|start\|end DEST` | Deletes each span of `SEL` and inserts its text at `DEST`, which must resolve to one span. The destination may be in another file in the set. Moved text is re-based.                                |
| `create PATH TEXT`                        | Creates `PATH` holding `TEXT` (line-oriented, re-based to column 0) as if it had existed when the script started: it joins the file set and later commands can edit it. `PATH` must not exist.       |
| `rename SEL to NAME`                      | Renames the symbol at `SEL`, which must resolve to one span, wherever the language server finds it (below).                                                                                          |
| `resolve SEL ours\|theirs\|base\|both`    | Replaces each selected conflict (§3.11) with the lines of one side as they are: `both` is ours, then theirs. Each span must be a whole conflict. An empty result deletes it as `delete` does.        |

Notes:

- `replace` never expands `$`. Only `sub` does. `replace` expands `@name` only
  when its target has pattern steps (§3.10).
- `sub` inserts its replacement verbatim after `$` expansion; the whole-line
  rules of §5.1 don't apply.
- A `$` reference names the longest run of letters, digits and `_` after it, as
  in the [`regex`] crate: `$1x` is the group `1x`, and `${1}x` is group 1 then
  `x`. A reference to a group the regex doesn't have is an error.
- The `all` prefix belongs to the target: `delete all fn:test_*`. In `sub`, the
  pattern already matches everywhere; `all` applies only to the scope selector.
- `move` removes each span as `delete` does, and inserts its text as `insert`
  does: whole lines if the span is whole-line, re-based to the destination; a
  syntax destination of `start`/`end` implies `.body`. With `all`, the spans
  arrive in source order. When the destination item ends with `,` (a field or
  variant) and the moved text doesn't, one is appended, as for `replace`. A
  destination inside a moved span is an error.
- If a moved whole-line span had a blank line directly above or below it, and it
  moves `before` or `after` a whole-line destination, one blank line separates
  it from the destination.
- `insert before|after` on a syntax item other than an import or a Markdown list
  item, when the item has a blank line directly above or below it, separates the
  new text from it with one blank line, unless the text already starts (for
  `after`) or ends (for `before`) with a blank line. Text inserted before an
  item that is only doc comments and attributes (`#[inline]`, `/// ...`) gets no
  blank line: it attaches to the item. `insert before` a line, regex or literal
  target whose first line starts such an item's doc comments or attributes
  treats it as that item when the text ends with an item, so with
  `insert before 12`, where line 12 is `/// Docs.` above `fn x`, a new function
  is separated as with `insert before fn:x`.
- `replace` of a syntax item keeps the item's leading doc comments and
  attributes unless `TEXT` starts with its own, so replacing a test function
  keeps its `#[test]`. To replace them too, start `TEXT` with them, or select
  `ITEM.whole`.
- `replace` with empty `TEXT` (`""`, or a heredoc with no lines) on a whole-line
  span removes its lines, exactly as `delete` does, blank-line tidy included. To
  leave one empty line, use `"\n"`. On a partial span, empty `TEXT` removes just
  the span.
- A `replace` that looks off by one gets a note on stderr (never an error),
  ignoring lines without a letter or digit (`}`):
  - a whole-line span whose `TEXT` starts with a copy of the line just above it,
    or ends with a copy of the line just below;
  - a partial span whose `TEXT` ends with the rest of the span's last line, or
    starts with what precedes the span on its first line, ignoring whitespace
    and line breaks (`TEXT` may re-wrap them). On a single-line span, the note
    suggests selecting its line with `.lines`.
- **Blank-line tidy.** When deleting a whole-line span (§5.1) leaves two blank
  lines in a row, a blank line right after an opening delimiter (or a line
  ending in `:`, as in Python) or right before a closing one, or a blank line at
  the start or end of the file, one blank line is removed. Merged deletions
  (§2.3) are tidied as one span.
- Text that `replace`, `insert` or `move` puts into an empty `.body` is always
  line-oriented, re-based to the enclosing item's indentation plus one indent
  unit (§5.2). An empty single-line body such as `fn f() {}` is opened onto
  separate lines.

`rename SEL to NAME` asks the language server (§1.1) to rename the symbol at the
start of `SEL`'s `.name` (for a syntax item) or of its span, and applies the
edits it returns like any other edit: under snapshot semantics (§2.3), together
with the script's other edits, then formatted and checked (§6.5). Each edit the
server makes counts as one. The edits may reach only the file set, or with `-w`
any workspace file, which then joins it; an edit to any other file, or a rename
that would create, rename or delete files, rejects the script. `rename` spawns
the daemon if need be, and waits up to `[lsp] timeout` for the server to be
ready.

```
rename fn:parse to parse_all
rename impl:Parser>fn:new>"tokens" to toks
```

### 4.3 Parse-error guard

For each modified file that has a language, `ned` counts the tree-sitter `ERROR`
and `MISSING` nodes, and the matches of the language's
`queries/<lang>/errors.scm` (code the grammar accepts but the language does not,
such as an empty Python block), before and after each stage's edits (§2.3). If
the count rises, the script is rejected (exit 1) and the error shows the first
new error node the edits touch, from where the text first changes if the node
starts before that (an `ERROR` node can span the whole file). If the edits
replace a `.sig` with text ending in the character that follows it (Python's
`:`, or the `{` of a body), the error adds that `.sig` stops before it. If the
edited text holds an escape such as `\x27`, the error adds that heredocs read no
escapes, and to pass a script that holds a `'` on stdin (§1). Conflict markers
are hidden from the grammar (§3.3), so a file with merge conflicts can be
edited. `--force` skips this check.

### 4.4 Directives

`allow errors` lets the script's edits apply even if they introduce errors
(§6.5), as for an unfinished change with missing symbols; the errors are still
shown. `allow warnings` lets introduced warnings (and less severe diagnostics)
through but still blocks errors, which matters only when `[check] block` is
`warning` or lower. A directive applies to the whole script, wherever it
appears; with several, the most permissive wins.

## 5. Text and indentation

### 5.1 Line-oriented and verbatim text

A span is **whole-line** if only whitespace precedes it on its first line and
only whitespace follows it on its last line. Line selectors, syntax items, most
`.body`s, heredoc literals and `.lines` are normally whole-line. Most strings
and regex matches are not.

- **Whole-line target:** the edit operates on the full lines, including the
  leading indentation and the line ending. `TEXT` is **line-oriented**: a final
  newline is added (a string that already ends in one gets no other), and its
  lines are re-based (§5.2). For example, `insert after` puts new lines after
  the target's last line.
- **Partial-line target:** `TEXT` is inserted **verbatim** at the span.
  - If `TEXT` has several lines, the first is inserted as-is. The rest are
    re-based relative to the line the span starts on.
  - Exception: `insert before|after` with heredoc `TEXT` widens a partial-line
    target to its whole lines, so `insert after /re/ <<END` adds lines after the
    match's line. A target that ends in an item part (any part but `.lines`,
    `.refs` and `.def`) isn't widened, and string `TEXT` stays verbatim. `move`
    to such a destination widens the same way when it moves whole lines.

Blank or whitespace-only lines in `TEXT` are written as empty lines. Leading and
trailing blank lines in `TEXT` are kept. This is how an agent adds a separating
blank line. A string's final `\n` ends its last line, so `"x\n"` adds one line
and `"x\n\n"` adds it and a blank line.

### 5.2 Re-basing

Every line-oriented `TEXT` is re-based, except a `<<'TAG'` heredoc.

1. **Strip** the common leading whitespace of the text's non-blank lines.
2. **Convert** the indent style if the text and the file differ (spaces vs
   tabs). One indent level in the text is its smallest non-zero indentation.
   Each level becomes one level of the file's indent unit.
3. **Prefix** every non-blank line with the target indentation:

| Edit                                        | Target indentation                                                                                                                                                                                                                 |
| ------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `replace`, `insert before`, `move` (before) | indentation of the target span's first line                                                                                                                                                                                        |
| `insert after`, `move` (after)              | indentation of the target span's first line, or of its last non-blank line when the selector's last step is a literal or regex with no parts                                                                                       |
| `insert start\|end`, `move` (start/end)     | indentation of the first non-blank line inside the span. If the span is empty or blank, the indentation of the enclosing item's first line plus one indent unit                                                                    |
| `replace` of a conflict or its empty side   | indentation of the first non-blank line of the conflict's sides (§3.11); if all are blank, of the lines of the block enclosing the conflict, or one indent unit deeper than the line before it when that line opens an empty block |

For `insert after` (and `move ... after`) in a file with a syntax tree, when
that line ends a construct begun on an earlier line, such as the last line of a
statement split across lines (`        .collect();`), the target indentation is
that of the construct's first line. The construct is the largest syntax node
that ends on the line and starts a line, holding the line's first token and
inside the innermost node that lays out its children on lines of their own (a
block's statements, a list's items). A line in the middle of a construct, or one
that starts or closes an item of such a layout, keeps its own indentation.

`replace` puts `TEXT`'s first non-blank line in place of the target's first
line, so when that line is indented more than another of `TEXT`'s lines (the
text closes blocks it didn't open, such as `x;`, then `}`, then a new item), the
first line takes the target indentation instead: every line is shifted by the
same amount, so the lines `TEXT` dedents step out of the target's enclosing
blocks, and the least indented never goes left of column 0. Insertions keep the
rules above, since text indented below its first line there continues a block
the target line opens.

In Markdown, when `TEXT` starts with a list item (`-`, `*`, `+`, `1.` or `1)`)
and the target line lies in a list item (not in a code block inside it), the
edit anchors to that innermost list item: the target indentation is its marker's
column, `insert after` (and `move ... after`) goes after the whole item, its
wrapped lines and nested items included, and `insert before` goes before its
first line. So a new item next to a wrapped item's continuation line becomes its
sibling. `<<'TAG'` text is placed the same way but not re-based.

The file's **indent unit** is measured on its non-blank lines, skipping those
that start inside a string or comment. Its style is tabs or spaces, whichever
indents more of those lines (the language default's on a tie), and it is the
smallest non-zero increase in that style between consecutive lines. If the file
has none, it's the language default: four spaces, two for Markdown, or a tab for
Go.

Formatters (§6.4) run after re-basing, so small indentation differences in brace
languages don't matter. Python relies on re-basing alone.

## 6. Output

Output goes to stdout. Reads print in command order. Edit results print after
all commands, once per modified file, in the order the files first appear.

### 6.1 `show`

Each selected region is headed `PATH:START-END`, or `PATH:N` for a single line.
Its lines follow as `N:text`, with no padding. `show SEL +N` adds up to `N`
lines of context before and after each span. If regions are within one line of
each other, they merge.

`show all` is a search: when its last step (a regex, literal, heredoc or
`.refs`) matches nothing where the earlier steps matched, it prints
`no matches for SEL in N files` and the script goes on, exiting 0 if nothing
else fails. `show` without `all`, and every edit, still fail on no match.

```
src/parser.rs:14-17
14:    pub fn parse(&mut self) -> Result<Ast, Error> {
15:        let tok = self.next().expect("unexpected end");
16:        self.parse_expr(tok)
17:    }
```

### 6.2 `outline`

Each file is headed by its path. Every item is printed as `START-END kind:name`,
indented two spaces per nesting level, so the agent can paste the selector
straight back. The line range covers the item's default span.

- Imports collapse into one line: `1-3 import (3)`.
- Items inside function bodies are omitted.
- `field` and `variant` items, and Markdown `item`, `table` and `code` items,
  are listed only when `outline SEL` targets their parent. In Markdown, the
  outline is the tree of sections.
- `outline SEL` lists the items strictly inside each span of `SEL`, starting at
  the left margin, under one header per file.
- Like syntax steps, `outline` skips text files, and is an error if every file
  in the set is text.
- A file with conflicts (§3.11) ends its outline with one line per conflict,
  `START-END conflict:N`, at the left margin.

```
src/parser.rs
1 import (1)
3-7 struct:Parser
9-22 impl:Parser
  10-12 fn:new
  14-17 fn:parse
  19-21 fn:debug_dump
```

### 6.3 Edits

Each modified file gets one summary line, followed by its diff hunks. The hunks
use the standard `@@ -a,b +c,d @@` header, with no `---`/`+++` file headers, and
`--context` lines of context.

```
src/parser.rs: 1 edit, +1 -1
@@ -14,3 +14,3 @@
     pub fn parse(&mut self) -> Result<Ast, Error> {
-        let tok = self.next().expect("unexpected end");
+        let tok = self.next().expect("unexpected end of input");
         self.parse_expr(tok)
```

- The edit count is the number of spans edited, summed over stages; the hunks
  show the change from the original contents to the last stage's.
- `--dry-run` prefixes each summary line with `(dry run) `.
- `--quiet` prints only the summary lines.
- A file made by `create` is summarized as `PATH: created, +N`, and its hunks
  show its whole contents. Missing parent directories are created.

### 6.4 Formatting

After the guard passes, each modified file with a configured formatter is
formatted.

- Changes the formatter makes are reported after the file's edit hunks, under
  their own header: `fmt rustfmt: +0 -1`, followed by hunks against the
  post-edit text. `--quiet` keeps the header and drops its hunks.
- A formatter that isn't installed, or that exits non-zero or prints non-UTF-8
  output, produces a note on stderr:
  `note: rustfmt not found; skipped formatting src/parser.rs`, or
  `note: rustfmt failed: <first stderr line>; skipped formatting src/parser.rs`.
  The file is written unformatted and the exit code stays 0.
- A file that still has merge conflicts (§3.3) isn't formatted, by its formatter
  or a language server, since formatting may rewrite its marker lines:
  `note: skipped formatting src/a.rs: it has merge conflicts`.
- `--dry-run` still runs formatters, on in-memory copies.

When no formatter for the language is installed and a daemon is running (§6.5),
the file's language server formats it instead (`textDocument/formatting`), if it
supports that: the header names the server, `fmt rust-analyzer: +0 -1`. A server
that fails keeps the note, with its reason added. The daemon isn't started for
this, and `false` disables the fallback too.

A formatter gets the file's text on stdin and prints the formatted text on
stdout. It runs in the file's directory (or, for a file `create` makes in a new
directory, its nearest existing ancestor), so its own configuration
(`rustfmt.toml`, `.prettierrc`, ...) is found. Its name in the output is the
basename of its program.

Formatters are configured per language under `[format]`, keyed by the `--lang`
names. A value is a command as an argv array, or `false` for none:

```toml
[format]
rust = ["rustfmt", "--edition", "{edition}", "--config", "max_width=80"]
python = false
```

- Settings are merged per language, with later sources winning: the defaults,
  then the user config (`$XDG_CONFIG_HOME/ned/config.toml`, or
  `~/.config/ned/config.toml`), then every `.ned.toml` from the filesystem root
  down to the file's directory, as the script leaves them.
- In a command, `{path}` is the file's absolute path, and `{edition}` is the
  Rust edition from the nearest `Cargo.toml` as the script leaves it (following
  `edition.workspace = true`), or `2015` if there is none.
- A bare program name is looked up in `node_modules/.bin` in the file's
  directory and each one above it, then on `PATH`. A program path containing `/`
  is relative to the config file that sets it.
- An unknown key or a value of the wrong type is an error at its location:
  `error: .ned.toml:2:1: invalid config: ...`. Formatting reads `[format]` only
  when it runs, so `--no-fmt` skips it; `check`, `rename` and edit checks read
  `[check]` and `[lsp]` through the daemon.

| Language                    | Default formatter                                                                                         |
| --------------------------- | --------------------------------------------------------------------------------------------------------- |
| rust                        | `rustfmt --edition {edition}`                                                                             |
| go                          | `gofmt`                                                                                                   |
| python                      | `ruff format --stdin-filename {path} -`, or if ruff isn't installed, `black -q --stdin-filename {path} -` |
| typescript, tsx, javascript | `prettier --stdin-filepath {path}`                                                                        |
| markdown                    | `prettier --stdin-filepath {path}`                                                                        |

### 6.5 Checking

After formatting, if a daemon is running for the workspace (§1.1), each modified
file whose language has a server is checked: the server diagnoses the original
text (empty for a created file) and the final text. Edits never start a daemon;
`ned daemon start` or a `check` does. `--no-check` skips checking.

A diagnostic in the final text is **introduced** unless the original has an
identical one left to match it: the same severity, source, code and message.
Positions don't count, since edits move them.

- Introduced diagnostics at `[check] show` or above are printed after the file's
  hunks and `fmt` lines, in `check`'s format (§4.1), with positions in the final
  text. `--quiet` keeps them.
- Introduced diagnostics at `[check] block` or above (default `error`) reject
  the script (exit 1), and nothing is written, even with `--dry-run`. `allow`
  (§4.4) and `--force` let them through.

  ```
  error: edit introduces 1 error; fix it, or add `allow errors` to the script to apply it anyway
  src/parser.rs:15:9: error: mismatched types [rust-analyzer E0308]
  ```

- A server that fails or doesn't report in time (`[lsp] timeout`) skips checking
  with a note, and the edit applies:
  `note: rust-analyzer didn't answer diagnostics within 30s; it may still be indexing, so rerun in a few seconds; skipped checking src/parser.rs`.
- If the edit isn't written, the servers are sent the original text again.
- Checks a server runs on save (rust-analyzer's `cargo check`) don't run, since
  the edit isn't written yet; run `check` after the edit for those.

### 6.6 Terminal output

With `--color auto`, `ned` colours stdout when it is a terminal and stderr when
that is one, unless `NO_COLOR` is set to a non-empty value. `--color always`
colours both, and `--color never` neither, whatever `NO_COLOR` says. A
subcommand takes `--color` after its name: `ned undo --color always`. Uncoloured
output is exactly as §6.1–6.5 and §7 give it, so output read by a program never
changes.

Colour changes only how output looks, not what it says:

- `show` prints each line's number right-aligned to the widest in its region,
  dimmed and followed by a space instead of `:`, then the line's code,
  highlighted with its language's highlight query.
- `outline` dims each item's lines and colours its kind.
- The headers naming a file, a `show` region's or `outline`'s, are bold.
- An edit's summary line and `fmt` header are bold; in its hunks, `@@` headers
  are cyan, removed lines red and added lines green, and each line's code is
  highlighted from the text it belongs to.
- `check` colours each diagnostic's severity: errors red, warnings yellow, info
  and hints blue.
- On stderr, only the `error:` and `note:` that start a message are coloured,
  except in an error in the command line's options, which also colours the
  arguments it quotes and its usage.

## 7. Errors and exit codes

Errors go to stderr, in the form `error: LOC: message`.

- `LOC` is `script:LINE:COL` for script and selector errors, or `PATH:LINE:COL`
  for errors inside a file.
- A caret excerpt follows where it helps.
- Every error ends with a concrete fix: candidate selectors, a nearby name, or
  the flag to use.
- A selector that matches nothing is named up to its first step that matched
  nothing, and the fix is for that step: `impl:Lexr>fn:next` gives
  `impl:Lexr matches nothing in src/lexer.rs; did you mean impl:Lexer>fn:next (12-30)?`.

| Error                                                                           | Fix it suggests                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| ------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Script syntax                                                                   | Quoting, for a bare word where text or a selector belongs or a dotted `import` name; nesting for another dotted name (`KIND:App>fn:handle`, or a Go method's `fn:"App.handle"`); `\\` for a backslash in a string; a selector for `insert end`, or `insert after $`; `show SEL +M` for `+N..+M`; `;` or a new line between commands for a second selector, e.g. `show fn:a; show fn:b`; otherwise the command's usage, e.g. `usage: replace [all] SEL with TEXT`        |
| `check`, `rename`, `.refs` or `.def` after a `\|`                               | Running it before the first `\|`, or in a separate `ned` call                                                                                                                                                                                                                                                                                                                                                                                                           |
| `sub all /re/ with TEXT`, with no scope                                         | Dropping `all`, since `sub` replaces every match                                                                                                                                                                                                                                                                                                                                                                                                                        |
| Selector matches nothing                                                        | The same name under another kind; a close syntax name, or the name without its generic arguments and paths (`impl:Log` for `impl:"Log<'_>"`); for `P>"a"..P>"b"`, `P>"a".."b"`; a string literal that matches as escaped source text (`"\\n"` for `"\n"`); a literal match that differs only in case or spacing; a regex that matches with `i`; the spans a nested step searched; a `\|` before the command, when the stage's earlier edits make it match; or `outline` |
| A `conflict` step that matches nothing                                          | The conflicts each searched file has (`a.rs has 2 conflicts (conflict:1, conflict:2)`), or that it has none                                                                                                                                                                                                                                                                                                                                                             |
| Command's name given as a `FILE`                                                | The `-e` form of the arguments                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| Ambiguous selector                                                              | Candidate selectors (§3.5), or longer text for matches that share a line                                                                                                                                                                                                                                                                                                                                                                                                |
| Missing part, part on a non-syntax step                                         | The parts the item has, or an example                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| `resolve` of a span that isn't a whole conflict                                 | Selecting one with `conflict:N`, or `replace`                                                                                                                                                                                                                                                                                                                                                                                                                           |
| Invalid query                                                                   | The closest node type or field name in the grammar                                                                                                                                                                                                                                                                                                                                                                                                                      |
| Unterminated pattern                                                            | Ending it with as many backquotes as opened it                                                                                                                                                                                                                                                                                                                                                                                                                          |
| Pattern that no searched file's language parses                                 | The first syntax error in it; adding the code around it, or `query{}`                                                                                                                                                                                                                                                                                                                                                                                                   |
| Pattern with no code                                                            | Writing the code to match between the backquotes                                                                                                                                                                                                                                                                                                                                                                                                                        |
| Placeholder that isn't a whole node                                             | `@@` for a literal `@`                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| Capture name in two pattern steps                                               | Renaming one of them                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| `@name` in `replace` TEXT that nothing captured                                 | The names captured, or `@@name` for a literal                                                                                                                                                                                                                                                                                                                                                                                                                           |
| `@_` in `replace` TEXT                                                          | `@@_` for a literal `@`                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| `$name` in `sub` TEXT that the regex doesn't have                               | `${1}rest` for a name starting with a group, else the groups it has; `$$` for a literal `$`                                                                                                                                                                                                                                                                                                                                                                             |
| `${}` in `sub` TEXT                                                             | `$$` for a literal `$`                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| Pattern searching only text and Markdown files                                  | A regex or literal, or `--lang`                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| Syntax step or `outline` in a text file                                         | `--lang`                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| Syntax step, pattern or `outline` with `--lang text`                            | Dropping `--lang text`, or a regex or literal                                                                                                                                                                                                                                                                                                                                                                                                                           |
| Kind the language doesn't have                                                  | The kinds it has                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| Line past the end                                                               | `$` for the last line                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| No language server for the files                                                | The `[lsp]` setting for their language                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| Language server failure                                                         | Installing the server or fixing its `[lsp]` setting, or rerunning once it has indexed                                                                                                                                                                                                                                                                                                                                                                                   |
| Server can't rename there                                                       | Selecting the name itself                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| Rename, `.refs` or `.def` reaching a file outside the set                       | `-w`; outside the workspace, a regex (`sub`, for a rename)                                                                                                                                                                                                                                                                                                                                                                                                              |
| Ambiguous `.refs` or `.def` result                                              | `all`, with the matches' locations                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| File not in the set                                                             | The `file` command that adds it                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| Overlapping edits                                                               | Merging them, or a `\|` between them                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| Missing file or empty glob                                                      | The working directory paths are relative to                                                                                                                                                                                                                                                                                                                                                                                                                             |
| No files to edit                                                                | `FILE` arguments or `file PATH`                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| `create` of a file that exists                                                  | `file PATH` to edit it                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| Session name with other characters                                              | Letters, digits, `.`, `_` and `-`                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| `!!`, `history` or `undo` without a session                                     | `-s NAME` or `NED_SESSION`, with the workspace's sessions                                                                                                                                                                                                                                                                                                                                                                                                               |
| `!!` with no earlier script                                                     | Writing the script out                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| `!!` without `-n` after a dry run                                               | Sending the script again to apply it, or `-n` to preview it again                                                                                                                                                                                                                                                                                                                                                                                                       |
| `!!:s/OLD/NEW/` whose `OLD` the script doesn't contain                          | The script                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| Malformed `!!` modifier                                                         | `usage: !![:s/OLD/NEW/][:gs/OLD/NEW/]...`                                                                                                                                                                                                                                                                                                                                                                                                                               |
| Nothing to undo                                                                 | `ned history`                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| Undo of a file changed since                                                    | `--force`, to merge the undo into it                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| Undo of a file removed since, or a `--force` merge that overlaps a later change | Editing the file by hand; `ned history`                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| Unsafe session directory                                                        | Removing it, or setting `XDG_STATE_HOME` to a private directory                                                                                                                                                                                                                                                                                                                                                                                                         |
| Session log of an unknown format version                                        | Another session name                                                                                                                                                                                                                                                                                                                                                                                                                                                    |

```
error: script:1:8: fn:new matches 2 items; add `all` or use one of:
  impl:Parser>fn:new   src/parser.rs:10-12
  impl:Lexer>fn:new    src/lexer.rs:8-10

error: script:1:8: fn:prase matches nothing in src/parser.rs; did you mean fn:parse (14-17)?

error: script:3:1: edit overlaps command 1 at src/parser.rs:14-17

error: src/parser.rs:15:31: edit introduces a syntax error (use --force to apply anyway)
15:        let tok = (self.next();
                                 ^

error: script:2:28: unterminated heredoc <<END (started here); end it with a line holding only END
2:replace fn:parse.body with <<END
                             ^
```

| Code | Meaning                                                                                                                                                                                                                                                                                                                                                                                                                        |
| ---- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 0    | Success, including dry runs and skipped formatters                                                                                                                                                                                                                                                                                                                                                                             |
| 1    | Edit rejected: no match, ambiguous match, overlap, missing part, unknown kind, text file, line past the end, file not in the set, `create` of an existing file, parse-error guard, introduced diagnostics, move into its own source, rename refused, reaching outside the file set, ambiguous `.refs`/`.def` result, nothing to undo, undo of a file changed or removed since, undo merge overlap, a refused `--commit` (§1.3) |
| 2    | Usage error (bad flags or arguments, a command's name given as a `FILE`, no script on a terminal, no files to edit, a bad session name, no session, a bad `!!`, `--commit` with `-n` or outside a git repository), script syntax error, invalid query, pattern or config, or no language server                                                                                                                                |
| 3    | I/O error: unreadable or non-UTF-8 file, glob matched nothing, write failure, language server failure, an unsafe or unknown-version session store, or a failing git command                                                                                                                                                                                                                                                    |

On any non-zero exit, no file is modified. Reads that ran before the failure
still print their output.

## 8. Worked examples

These compare ned with the alternatives an agent uses today:

- `sed` (BSD syntax, as on macOS)
- an inline Python script
- a str_replace-style edit tool call (JSON arguments only)

Token counts are for the full command text, measured with tiktoken's
`o200k_base` encoding as a proxy for LLM tokenizers. They count input only. ned
also prints a diff, which saves the read-back that `sed` and Python usually need
for verification.

`bench/` reproduces the table: `uv run bench.py --check`, run there, applies
every variant to a copy of the files, checks the result, and counts its tokens.

The examples use this file, `src/parser.rs`:

```rust
use std::fmt;

/// A recursive-descent parser.
pub struct Parser {
    src: String,
    pos: usize,
}

impl Parser {
    pub fn new(src: &str) -> Self {
        Parser { src: src.to_string(), pos: 0 }
    }

    pub fn parse(&mut self) -> Result<Ast, Error> {
        let tok = self.next().expect("unexpected end");
        self.parse_expr(tok)
    }

    fn debug_dump(&self) {
        eprintln!("{}", self.src);
    }
}
```

| #   | Task                            | ned | sed | Python |           str_replace |
| --- | ------------------------------- | --: | --: | -----: | --------------------: |
| 1   | Change a string in one function |  22 | 20† |     61 |                    32 |
| 2   | Add a method to an impl         |  41 |   — |     94 |                    74 |
| 3   | Delete a function               |  13 | 18† |     70 |                    44 |
| 4   | Change a function's params      |  20 |  27 |     63 |                    34 |
| 5   | Replace a function body         |  38 |   — |     93 |                    62 |
| 6   | Add an import                   |  21 |  23 |     63 |                    35 |
| 7   | Insert into a Python block      |  30 |   — |     67 |                    38 |
| 8   | Rename an identifier in 5 files |  18 |  25 |     48 | 1 call per occurrence |

† = not scoped or not reliable. — = not practical.

**1. Change a string inside one function.**

```sh
ned src/parser.rs -e 'replace fn:parse>"unexpected end" with "unexpected end of input"'
```

`sed -i '' 's/"unexpected end"/"unexpected end of input"/' src/parser.rs` is
slightly cheaper, but it's unscoped: it edits every function with that string,
and fails silently if there are none.

**2. Add a method to an impl.** The method is written at column 0; re-basing
indents it. The leading blank line separates it from the previous method.

```sh
ned src/parser.rs <<'EOF'
insert end impl:Parser <<END

fn peek(&self) -> Option<char> {
    self.src[self.pos..].chars().next()
}
END
EOF
```

The Python and str_replace versions both need a unique anchor (the end of
`debug_dump`) and hand-indented code. str_replace also JSON-escapes it.

**3. Delete a function.**

```sh
ned src/parser.rs -e 'delete fn:debug_dump'
```

The sed range `/fn debug_dump/,/^    }$/d` depends on the indentation of the
closing brace, and leaves a stray blank line behind.

**4. Change a function's parameters.**

```sh
ned src/parser.rs -e 'replace fn:new.params with "src: impl Into<String>"'
```

**5. Replace a function body.**

```sh
ned src/parser.rs <<'EOF'
replace fn:parse.body with <<END
let tok = self.next().ok_or(Error::Eof)?;
self.parse_expr(tok)
END
EOF
```

The alternatives must repeat the old body verbatim, including its indentation.

**6. Add an import.**

```sh
ned src/parser.rs -e 'insert after import:std::fmt "use std::io;"'
```

**7. Insert into a Python block.** This uses `app.py`:

```python
def handle(req):
    if req.ok:
        log(req)
        return 200
    return 500
```

```sh
ned app.py <<'EOF'
insert after "log(req)".lines <<END
if req.slow:
    warn(req)
END
EOF
```

The block lands at 8 spaces of indentation, with `warn` at 12. Every alternative
has to spell out that indentation.

**8. Rename an identifier across files.**

```sh
ned src/*.rs -e 'sub /\bold_name\b/ with "new_name"'
```

sed is competitive here, but needs BSD `[[:<:]]` word boundaries on macOS and
reports nothing. ned prints a per-file diff, and exits 1 if nothing matched.
With a language server, `rename fn:old_name to new_name` does it semantically,
skipping comments, strings and unrelated names.
