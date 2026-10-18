---
type: Operation
title: Attach and resume
description: Handing the terminal to the official CLI for a live session (attach) or a saved conversation (resume), then taking it back.
resource: https://github.com/snrsw/agnt/blob/main/src/terminal.rs
tags: [operations, terminal]
generated: { by: claude-opus-5-5, at: 2026-10-18T00:00:00Z }
sources:
  - id: terminal-rs
    resource: https://github.com/snrsw/agnt/blob/main/src/terminal.rs
    title: src/terminal.rs
  - id: claude-rs
    resource: https://github.com/snrsw/agnt/blob/main/src/providers/claude.rs
    title: src/providers/claude.rs
  - id: adr-0001
    resource: https://github.com/snrsw/agnt/blob/main/docs/adr/0001-architecture.md
    title: "ADR-0001, D4"
---

# Two different operations

| Operation | Target | Effect |
|---|---|---|
| Attach | A session that is running now | Joins the live execution |
| Resume | A saved conversation | Opens the conversation again in a new process |

**Resuming a conversation does not mean the old process was kept.**[^adr-0001] agnt says so in the note shown before hand-off.[^claude-rs]

Enter on an empty [prompt](/ui/prompt.md) picks the operation from the [runtime state](/model/runtime-state.md):

| State | Action |
|---|---|
| Live (starting, running, waiting, idle) | Attach |
| Stopped | Resume |
| Error or Unknown | agnt asks which one |

# Terminal hand-off

agnt does not draw the agent's screen itself. It hands the whole terminal to the official CLI:[^terminal-rs]

1. Leave the alternate screen and raw mode.
2. Pause agnt's keyboard reader so it does not steal the child's keystrokes.
3. Run the CLI's command as an argument array in the session's directory.
4. When the CLI exits, take the terminal back and redraw the list.

The terminal is restored even when the child fails to start or agnt panics.

With [Claude Code](/providers/claude-code.md), leaving the attached screen returns to the list, and the session keeps running. [Codex](/providers/codex.md) supports resume only.

[^terminal-rs]: src/terminal.rs
[^claude-rs]: src/providers/claude.rs
[^adr-0001]: ADR-0001, D4
