# Using ned from an AI agent

`ned` is built for coding agents: short commands, syntax-aware selectors, edits
that apply all together or not at all, and a diff in place of a read-back. This
page covers how to give an agent access to it. The usage guidance itself is in
the skill, [`skills/ned/SKILL.md`](skills/ned/SKILL.md).

## Setup

Install the binary (Rust 1.90 or later):

```sh
cargo install --locked --git https://github.com/bbessemer/neoed ned-cli
```

From a clone, use `cargo install --path crates/ned-cli` instead.

**Claude Code.** Copy the skill directory into a skills directory:

- all projects: `cp -r docs/skills/ned ~/.claude/skills/`
- one project: `cp -r docs/skills/ned .claude/skills/`

Claude loads it when reading, searching or editing files comes up.

**Other agents.** Add the snippet below to the system prompt. It's shorter than
the skill, and it points the agent at `ned help` for the rest.

```text
To read, search or edit files, use `ned` rather than cat, grep, sed, inline
Python or whole-file rewrites. Pass the script on stdin with a quoted heredoc
(always when text holds a ', never shell escapes):

  ned FILE... <<'EOF'
  replace fn:parse>"old text" with "new text"
  EOF

Selectors: line ranges (12-20), /regex/, "literal", and in Rust, Python, Go,
JS/TS and Markdown, syntax items (fn:parse, impl:Parser>fn:new,
fn:parse.body). A selector must match exactly one span unless prefixed with
`all`. Run `outline` to list items and `show SEL` to read part of a file;
search with `ned -w -e 'show all /re/'`; make files with `create PATH <<END`.
Write inserted code at column 0 in a `<<END` heredoc; ned re-indents it
(`<<'END'` inside the script keeps text verbatim). ned prints a diff, so don't
re-read the file to check the edit. Errors end with a fix; apply it and rerun.
With a language server: `check` for diagnostics, `rename SEL to NAME`,
`SEL.refs` and `SEL.def`; after `ned daemon start`, edits that introduce
errors are rejected. With NED_SESSION set, `ned undo` reverts your last edit
and `ned -e '!!:s/old/new/'` reruns your last command corrected. Merge
conflicts don't stop parsing; `resolve conflict:N ours|theirs|both` keeps
sides of one. `ned help` and `ned help TOPIC` document the rest.

```

**Sessions.** With `NED_SESSION` set in the agent's environment, `ned` records
its invocations, so the agent can undo an edit and repeat a failed command with
a fix, and you can follow its work with `ned history`. Give each conversation
its own session, so one agent's `undo` never reverts another's edit; give a
subagent its own `-s NAME`, which overrides the `NED_SESSION` it inherits. In
Claude Code, a `SessionStart` hook names the session after the conversation: add
it to `.claude/settings.json` (for one project) or `~/.claude/settings.json`
(for all of them). It needs `jq`.

```json
{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "echo \"export NED_SESSION=claude-$(jq -r .session_id)\" >> \"$CLAUDE_ENV_FILE\""
          }
        ]
      }
    ]
  }
}
```

The hook writes the variable to `$CLAUDE_ENV_FILE`, which Claude Code applies to
every Bash command in the conversation. A resumed conversation keeps its
session; `/clear` starts a new one. To read a conversation's log, run
`ned history -s NAME`, with NAME a log's file name from
`ls ~/.local/state/ned/sessions/*/`, less `.log`.

## Example

A typical agent edit: read one function, then change it and add a method, in one
call.

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
