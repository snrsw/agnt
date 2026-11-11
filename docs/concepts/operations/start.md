---
type: Operation
title: Start
description: Typing a task and pressing Enter begins a new session in the current directory, in whichever way the provider supports.
tags: [operations]
generated: { by: claude-opus-5-5, at: 2026-10-18T00:00:00Z }
sources:
  - id: providers-mod
    resource: https://github.com/snrsw/agnt/blob/main/src/providers/mod.rs
    title: src/providers/mod.rs
  - id: readme
    resource: https://github.com/snrsw/agnt/blob/main/README.md
    title: README
  - id: app
    resource: https://github.com/snrsw/agnt/blob/main/src/app.rs
    title: src/app.rs
---

# Gesture

Type a task in the [prompt](/ui/prompt.md) and press Enter. `Tab` picks the agent (Claude or Codex) first. The session starts in the directory the shell in [shell mode](/ui/prompt.md) last ended in.[^readme]

# What happens per provider

The two providers start work differently, and agnt does not hide the difference.[^providers-mod]

| Provider | What agnt does | What the user sees |
|---|---|---|
| [Claude Code](/providers/claude-code.md) | Runs `claude --bg <task>` and gets the new session's identity back | A new row appears; the list stays on screen |
| [Codex](/providers/codex.md) | Hands the terminal to `codex` with the task | Codex's own screen; the new session is discovered on return |

A session started by agnt has [origin](/model/origin.md) `managed`.

# Confirming a background start

A Claude Code row starts in [runtime state](/model/runtime-state.md) `Starting`, which only says agnt asked for the session. agnt refreshes immediately after the start and again every 2 seconds while any row is still `Starting`. The provider's state replaces `Starting` as soon as the session is listed.[^app]

A refresh that was already running when the session started used a listing taken before the session existed. That result does not remove the row, and another refresh runs when it finishes.[^app]

[^app]: src/app.rs

[^providers-mod]: src/providers/mod.rs
[^readme]: README
