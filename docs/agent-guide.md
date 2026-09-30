# Using ned from an AI agent

`ned` is built for coding agents: short commands, syntax-aware selectors, edits
that apply all together or not at all, and a diff in place of a read-back. This
page covers how to give an agent access to it. The usage guidance itself is in
the skill, [`skills/ned/SKILL.md`](skills/ned/SKILL.md).

## Setup

Install the binary:

```sh
cargo install --path crates/ned-cli
```

**Claude Code.** Copy the skill directory into a skills directory:

- all projects: `cp -r docs/skills/ned ~/.claude/skills/`
- one project: `cp -r docs/skills/ned .claude/skills/`

Claude loads it when an edit to existing files comes up.

**Other agents.** Add the snippet below to the system prompt. It's shorter than
the skill, and it points the agent at `ned help` for the rest.

```text
To edit existing files, use `ned` rather than sed, inline Python or whole-file
rewrites. Pass the script on stdin with a quoted heredoc:

  ned FILE... <<'EOF'
  replace fn:parse>"old text" with "new text"
  EOF

Selectors: line ranges (12-20), /regex/, "literal", and in Rust, Python, Go
and Markdown, syntax items (fn:parse, impl:Parser>fn:new, fn:parse.body). A
selector must match exactly one span unless prefixed with `all`. Run `outline`
to list items and `show SEL` to read part of a file. Write inserted code at
column 0; ned re-indents it. ned prints a diff, so don't re-read the file to
check the edit. Errors end with a fix; apply it and rerun. `ned help` and `ned
help TOPIC` document the rest.

```

## Example

A typical agent edit: read one function, then change it and add a method, in
one call.

```ned
show fn:parse
```

```ned
replace fn:parse>"unexpected end" with "unexpected end of input"
insert end impl:Parser <<END

fn peek(&self) -> Option<char> {
    self.src[self.pos..].chars().next()
}
END
```
