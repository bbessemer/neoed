# ned-scheme design

How `ned-scheme` evaluates the plugin language specified in `docs/plugins.md`
(cited here as P§N). Today the crate only reads; everything below is planned.

## 1. Decision: a hand-rolled VM

TODO.md planned a spike comparing Steel with a hand-rolled evaluator. This
design settles it for the hand-rolled one:

- **Borrowed trees.** Plugin values must hold `tree_sitter::Node<'h>` borrowed
  from `ned`'s buffers for the length of a script, with no copying and no
  `'static` laundering. That takes a lifetime parameter on every value, which no
  embeddable Scheme offers.
- **The reader is the front end.** It already reads query syntax (`@capture`,
  `[...]`, `name:`, `.`, `#eq?`), which breaks any standard reader, and it
  carries spans for `ned`'s error rendering.
- **Small target.** Plugins run for milliseconds in a one-shot CLI. Startup and
  predictability matter more than peak speed, and R7RS-small without full
  `call/cc` is a modest language to implement.
- **Refcounting fits short lives.** Objects are freed as soon as they're
  unreachable, and the cycles refcounting can't free are broken when the isolate
  ends (§4), so no tracing collector is needed.

## 2. Pipeline

```
source ──read──▶ Syntax ──expand──▶ Core ──compile──▶ Code ──run──▶ Value<'h>
```

**Reader** (`read.rs`, exists). Additions:

- `Datum::Template(Template)` for `` #`...` ``: the fence rules of P§2.2 applied
  in the reader, and the literal split into `Text`, `Hole { name, many }`
  (`@name`, `@_`, with `...`) and `Expr { expr: Box<Syntax>, many }` (`@{...}`)
  parts. `@{` reads one datum with `}` as an extra delimiter, then expects `}`.
  `@@` becomes text.
- The placeholder lexer in `ned-core/src/template.rs` moves here, so `ned`'s
  patterns, `replace` TEXT and syntax literals share one implementation;
  `ned-core` already depends on this crate.
- Tier 2: `#\` characters and `#(` vectors.
- `Display` writes `Template` back as a literal with a long enough fence.

**Expander** (`expand.rs`). Turns `Syntax` into a small core of forms, keeping
spans: `quote`, `if`, `define`, `set!`, `lambda` (fixed and rest parameters),
`begin`, calls, `build` (a construction, §7.1) and `match-node` (one clause,
§7.2). Every other special form (P§6.1) is a derived form rewritten in Rust:
`let` family, `cond`, `case`, `and`, `or`, `when`, `unless`, `do`, `quasiquote`,
`define-record-type`, `guard`, `match`, `with-language`, `define-verb` and
`define-kind`. A brackets list in expression position, a capture or an anchor
outside a pattern, and a keyword in operator position are expansion errors with
the fixes P§2.2 gives. Tier 2 adds `syntax-rules`, expanded by renaming: a
template's introduced identifiers get fresh names bound to the definition
environment's meaning (explicit renaming, not full `syntax-case` hygiene).

**Compiler** (`compile.rs`). Core forms to bytecode, one `Code` object per
`lambda`:

- Lexical addressing: each variable is a local slot, a free-variable slot of the
  closure, or a global slot of the plugin's top level.
- Assignment conversion: a local that is `set!` and captured lives in a box;
  every other variable is copied into closures (flat closures). Most closures
  then hold no mutable state, and the only cycles are through boxes, records and
  pairs (§4).
- `Code` is plain data (instructions, constants as `Datum`s, a pc-to-span table)
  with no `'h`, so a plugin compiles once per process and every isolate shares
  it through `Rc`. The standard library procedures that call back into Scheme
  (`map`, `for-each`, `fold`, `filter`, `apply`, …) are written in a Scheme
  prelude compiled the same way, so no native procedure ever re-enters the VM.

## 3. Values

```rust
pub enum Value<'h> {
    Null, Unspecified, Bool(bool), Int(i64), Real(f64), Char(char),
    Symbol(Sym), Keyword(Sym),
    Str(Rc<StrObj>), Pair(Rc<Pair<'h>>), Vector(Rc<VecObj<'h>>),
    Closure(Rc<Closure<'h>>), Native(&'static Native), Record(Rc<Record<'h>>),
    Box(Rc<BoxObj<'h>>), Error(Rc<ErrorObj<'h>>), Template(Rc<Template>),
    Node(Rc<NodeObj<'h>>), Span(Rc<SpanObj<'h>>),
}
```

(Shapes, not signatures; nothing is written yet.)

- Every heap variant is a thin `Rc`, so a value is 16 bytes. `Rc`, not `Arc`: an
  isolate is `!Send` and single-threaded.
- `Pair` holds its car and cdr in `RefCell`s for `set-car!`. Strings are
  immutable (P§2.3), so `StrObj` is a `Box<str>`.
- Symbols and keywords are `u32`s into the isolate's interner, so `eq?` on them
  is integer comparison.
- `Native` is a static table entry (name, arity,
  `fn(&mut Ctx<'h>, &[Value<'h>]) -> Result<Value<'h>, Error>`); natives are
  registered by the crate (core library) and by the host (§5).

## 4. Memory

Objects are reference counted and freed when the last reference drops. Cycles
are possible through mutable pairs, boxes (a `letrec` closure referring to
itself), vectors and records, and refcounting alone would leak them. Each
isolate keeps a `Vec<Weak<_>>` of the mutable containers it allocated; when the
isolate drops, it upgrades each and clears its contents, which breaks every
cycle, and the rest falls away by refcount. Within a script run, unreachable
cycles stay until then; isolates are short (§5.2) and the memory limit counts
them, so that is bounded.

Freeing a long list mustn't recurse: `Pair`'s `Drop` unlinks its `cdr` chain
iteratively.

No value outlives its isolate: `'h` ties them to it, and the host converts a
verb's results (edits, notes) to its own types before the isolate drops.

## 5. Isolates and the host

### 5.1 The `'h` lifetime

```rust
pub struct Isolate<'h> { host: &'h dyn Host<'h>, trees: &'h TreeArena, … }
```

`'h` is the lifetime of the host's borrows: the stage's buffers and trees, the
language registry, and a `TreeArena` the host creates next to the isolate.

- **Attached nodes** are `tree_sitter::Node<'h>` from the host's trees, plus the
  file id and its `&'h str` source.
- **Detached nodes** (P§3.2) come from trees the VM parses for syntax literals.
  They are allocated in the `TreeArena` (an append-only arena of
  `(Tree, String)`, e.g. `typed-arena`), which outlives the isolate, so their
  nodes are `Node<'h>` too and both kinds share one representation. The arena
  frees its trees when the host drops it after the script; detached trees are
  small and a script makes few.

`NodeObj<'h>` is then `{ node: Node<'h>, src: &'h str, file: Option<FileId> }`,
and every node accessor (P§7) is a direct `tree_sitter` call.

### 5.2 Lifecycle

The host makes one isolate per plugin per script run, when the script first uses
one of the plugin's words, and drops it when the script's commands have run,
before writing. Making one interns the prelude's and plugin's symbols, allocates
their global slots and runs the plugin's top level. Plugins never share an
isolate, so one can't see another's globals.

### 5.3 The `Host` trait

`ned-scheme` depends on the `tree-sitter` crate for its types only, never on
`ned-core` or a grammar. Everything that needs a language goes through the host,
which `ned-core` implements:

| Need                                  | Host provides                                                           |
| ------------------------------------- | ----------------------------------------------------------------------- |
| A language by name, a node's language | language ids and names                                                  |
| Read a literal (P§4.2)                | the reading for (literal, language): context, holes, separators; cached |
| Build a literal                       | the spliced text parsed in that reading, checked (P§4.2 step 3)         |
| `string->node`, `node-as`             | the same, with a required root kind                                     |
| `node->datum`, `datum->node` (§7.3)   | builders' reading and writing, and the parse that checks a datum        |
| `match` on a literal, `find-all`      | `ned-core`'s §3.10 matcher, compiled once per (literal, language)       |
| Query patterns                        | a compiled `tree_sitter::Query` for (datum, language), cached           |
| Spans, `file-root`, `span-captures`   | the file set's snapshot                                                 |
| Edits, `note`, `reject`               | recorded into the script's edit set and output                          |

The table is the boundary, not an API; the trait's methods are designed with the
first milestone that needs each.

## 6. Bytecode VM

A stack machine with a value stack and a frame stack, both `Vec`s, run by one
loop: Scheme calls never use the Rust stack, so deep recursion is bounded by the
memory limit rather than crashing, and fuel applies to every step.

Instructions (sketch): `Const k`, `Local i`, `SetLocal i`, `Free i`, `Global g`,
`SetGlobal g`, `Unbox`, `SetBox`, `MakeBox`, `Closure code n`, `Call n`,
`TailCall n`, `Return`, `Jump`, `JumpIfFalse`, `Pop`, `PushHandler`,
`PopHandler`, `Build lit n`, `Match pat`, plus inline primitives (`Add`, `Lt`,
`Car`, `Cdr`, `Cons`, `Eq`, `NullP`) that fall back to the general call when a
global they name has been redefined.

- **Tail calls**: `TailCall` replaces the current frame.
- **Errors**: `raise` unwinds frames to the innermost handler pushed by `guard`.
- **Escape continuations** (tier 2): `call/cc` captures the frame and handler
  depth; invoking it unwinds to that depth while its frame is live and is an
  error after. `dynamic-wind` keeps a wind list unwound the same way.
- **Fuel**: one unit per instruction, decremented in the loop; natives charge by
  work (a `string-append` by length, a query by nodes visited, which the host
  reports). Allocation counts bytes against the memory limit.

## 7. Syntax literals and patterns

### 7.1 Construction

A literal compiles to its expressions' code, then `Build lit n`. `lit` indexes
the code's constant templates; at run time `Build` pops `n` values, checks that
each is a node or a list of nodes (P§4.1), works out the language (P§4.3), and
asks the host to build. Readings are cached per (literal, language) in the
isolate, so a literal in a loop parses its template once and its spliced text
once per evaluation.

### 7.2 `match`

A `match` expands to a chain of `match-node` tests, each compiled to
`Match pat`, which leaves the bindings on the stack and falls through, or jumps
to the next clause.

- **Literal patterns** (P§5.1) go to the host's §3.10 matcher with the node as
  the only root to try. `@{EXPR}` placeholders are evaluated before the test and
  matched with `node=?`, through a `ned-core` hook for fixed-value holes.
- **Query patterns** (P§5.2) are compiled per language to a
  `tree_sitter::Query`: the datum's `Display` text, wrapped as
  `(PATTERN) @__root`, with the leaf-text extension rewritten to `#eq?`
  predicates first. It runs with a `QueryCursor` limited to the node's range and
  `set_max_start_depth(0)`, so only matches rooted at the node count, and
  `@__root` is checked against it. Captures bind to variables by name, lists for
  quantified ones.
- Pattern variables are locals of the clause, allocated by the compiler in
  capture order, so binding is a slot store.

### 7.3 Builders: CST and AST

Builders (P§11) live in `ned-core`, which reads `builders.gen.scm` and
`builders.scm` when a language is first needed. The VM reaches them through the
host for `node->datum` and `datum->node`; fragment parsing uses their contexts
directly.

- **Compiled form.** Each template becomes a sequence of literal token text
  (spacing removed, layout kept for writing), holes (field or kind, optional,
  run, separator), `@[...]` groups and `@"..."` token choices.
- **Reading** aligns a node's children with the sequence left to right,
  backtracking over optional parts and runs as `pattern.rs`'s sibling matcher
  does, and skipping comments. A literal segment may span several anonymous
  children (`->` is one token, `()` two).
- **Writing** fills the sequence depth-first, with `Template::fill`'s
  re-indentation for holes on their own lines, then parses the result in the
  fragment's context and compares AST forms. A kind with no builder writes its
  children separated by spaces; if that doesn't read back, the innermost such
  node holding the first difference tries nothing, then one child per line, the
  parent moving on when it runs out, with the attempts capped.
- **Contexts** come from a breadth-first search over builders from the start
  kind, through placeholders whose kinds (`node-types.json`, supertypes
  expanded) lead toward each target kind. The smallest code of each kind is a
  fixpoint over its builders: the one with the fewest required holes, each
  filled with the smallest code of its kind, names written `__ned`, token
  choices taking their first option. Contexts are cached per language and
  replace the `fragment.rs` builder list.

**Generator.** A workspace binary (`tools/gen-builders`, not published) reads
each grammar's `src/grammar.json` from its crate's source (`cargo metadata`) and
writes `queries/<lang>/builders.gen.scm`:

| grammar.json                        | Template                                   |
| ----------------------------------- | ------------------------------------------ |
| `STRING`                            | literal text                               |
| `FIELD`                             | `@field`                                   |
| named `SYMBOL` outside a field      | `@kind`                                    |
| hidden rule (`_expression`)         | inlined, or `@_` when it is a supertype    |
| `CHOICE` with `BLANK`               | `?`, or `@[...]` around a sequence         |
| `CHOICE` of `STRING`s               | `@"..."`                                   |
| `REPEAT` of `SEQ(SEP, X)` after `X` | `@x SEP ...`                               |
| other `REPEAT`                      | `@x...`                                    |
| `CHOICE` of sequences, other tokens | one builder per alternative                |
| `PREC*`, `ALIAS`                    | unwrapped; an alias builds its new name    |
| `PATTERN`, `TOKEN`, external tokens | a leaf, or reported for a hand-written one |

It lists the rules it couldn't translate, which become the hand-written
`builders.scm`, with layout for whitespace-sensitive grammars.

## 8. Errors

A Scheme error object holds a message, irritants, an optional fix and, when
raised, the call chain: one (code, pc) per frame, which the pc-to-span tables
turn into `PLUGIN.scm:LINE:COL` lines. Reader, expander and compiler errors
share the reader's `ReadError` style: a kind, a span and a message ending with a
fix. The host renders all of them as P§10.3 says.

## 9. Testing

- **Reader**: unit tests as today, plus `Template` parts, fences and `@{}`.
- **Language**: table-driven `.scm` files under `tests/scheme/`, each a list of
  `(test EXPR EXPECTED)` and `(test-error EXPR)` forms, run by one Rust test
  against a host with no languages. They cover every P§6 form and procedure,
  tail calls in a million-iteration loop, fuel and memory limits, and cycle
  teardown (a counter of live objects returns to zero).
- **Nodes, literals and patterns**: tests in `ned-core` against real grammars,
  one per P§4.2 rule and P§5 pattern form. A corpus per language checks that
  every builder fits some node, and that `datum->node` of `node->datum` gives
  back every node.
- **Plugins**: CLI tests with `assert_cmd` and `insta`, plugins in fixtures.

## 10. Milestones

Each is one PR or planned into several with the engineer, following the TDD
cycle in AGENTS.md.

1. **Generator**: the reader's `Template` literals and the builder placeholders
   (P§11.1), the shared placeholder lexer, the generator, `builders.gen.scm` for
   every language, and reading (P§11.2) with the corpus test that every builder
   fits some node. Patch release; lands before the VM.
2. **Builders in use**: the hand-written `builders.scm` fixes, contexts derived
   from builders (P§11.4) replacing the `fragment.rs` builder list, writing
   (P§11.3), and `node->datum`/`datum->node` in `ned-core` with the round-trip
   corpus. Patch release.
3. **Core VM**: expander, compiler, VM, tier-1 library without nodes, the
   conformance suite. Likely two PRs (expander and compiler; VM and library).
   Patch release; nothing user-visible.
4. **Nodes and the host**: the `Host` trait, `TreeArena`, node and span
   procedures, syntax literals, `match`, `find-all`. Patch release.
5. **Plugins**: P§9 and P§10, with the command-language spec changes (P§9.4).
   Minor release.
6. **Tier 2**. Minor release.
