# Using ned from an AI agent

`ned` is built for coding agents: short commands, syntax-aware selectors, edits
that apply all together or not at all, and a diff in place of a read-back. This
page covers how to give an agent access to it. The best way is the MCP server;
for agents that can't use one, there's a skill,
[`skills/ned/SKILL.md`](skills/ned/SKILL.md), and a system-prompt snippet.

## Setup

Install the binary on Linux or macOS:

```sh
curl -fsSL https://raw.githubusercontent.com/bbessemer/neoed/main/install.sh | sh
```

Or build it from source (Rust 1.90 or later):
`cargo install --locked --git https://github.com/bbessemer/neoed ned-cli`, or
`cargo install --path crates/ned-cli` from a clone.

**MCP.** `ned mcp` runs `ned` as an MCP server, and it's the preferred setup:
the agent calls `ned` as a tool instead of through a shell, so its scripts need
no quoting or heredocs, the tool descriptions teach it `ned` without a skill or
prompt snippet, and every call is recorded in a session without any setup. In
Claude Code: `claude mcp add ned -- ned mcp`; other clients take the same
command (`ned` with the argument `mcp`). Its `ned` tool takes a script, with
`files` (or `workspace` for `-w`) and the flags as arguments, and its
description is `ned help`. `outline`, `show`, `history`, `undo` and `help` are
tools too, `cd` moves the server to another directory for the rest of the
session, and each result is what `ned` would print. A `ned` call's `comment`
says what it is for: it's recorded in the session, and a REPL attached to it
(`ned repl --attach NAME`) prints it with the call's edits. The server records
into `NED_SESSION`'s session if it's set, or a new `mcp-N` otherwise
(`ned help mcp`).

**Claude Code skill.** To have Claude run `ned` from its shell instead, copy the
skill directory into a skills directory:

- all projects: `cp -r docs/skills/ned ~/.claude/skills/`
- one project: `cp -r docs/skills/ned .claude/skills/`

Claude loads it when reading, searching or editing files comes up.

**Other agents.** For an agent that can't use MCP but has a shell, add the
snippet below to the system prompt. It's shorter than the skill, and it points
the agent at `ned help` for the rest.

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
Add `--commit MSG` to commit just that edit; other changes stay uncommitted.
With a language server: `check` for diagnostics, `rename SEL to NAME`,
`SEL.refs` and `SEL.def`; after `ned daemon start`, edits that introduce
errors are rejected. With NED_SESSION set, `ned undo` reverts your last edit
and `ned -e '!!:s/old/new/'` reruns your last command corrected. Merge
conflicts don't stop parsing; `resolve conflict:N ours|theirs|both` keeps
sides of one. `ned help` and `ned help TOPIC` document the rest.

```

**Sessions.** The MCP server always records into a session. When the agent runs
`ned` from a shell, set `NED_SESSION` in its environment and `ned` records its
invocations, so the agent can undo an edit and repeat a failed command with a
fix, and you can follow its work with `ned history`. Give each conversation its
own session, so one agent's `undo` never reverts another's edit; give a subagent
its own `-s NAME`, which overrides the `NED_SESSION` it inherits. In Claude
Code, a `SessionStart` hook names the session after the conversation: add it to
`.claude/settings.json` (for one project) or `~/.claude/settings.json` (for all
of them). It needs `jq`.

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
`ned history -s NAME`, with NAME one of those `ned session list` prints (or
`ned session list --all`, for every workspace).

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
