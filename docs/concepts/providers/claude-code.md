---
type: Integration
title: Claude Code
description: The provider with full support, driven through `claude agents --json`, `--bg`, `attach`, `stop`, and `rm`.
resource: https://github.com/snrsw/agnt/blob/main/src/providers/claude.rs
tags: [providers, claude]
generated: { by: claude-opus-5-5, at: 2026-10-18T00:00:00Z }
sources:
  - id: claude-rs
    resource: https://github.com/snrsw/agnt/blob/main/src/providers/claude.rs
    title: src/providers/claude.rs
  - id: adr-0001
    resource: https://github.com/snrsw/agnt/blob/main/docs/adr/0001-architecture.md
    title: "ADR-0001, D3 and D4"
---

# How agnt drives it

All operations go through the official `claude` CLI.[^claude-rs]

| Operation | Command |
|---|---|
| discover, observe | `claude agents --json` (and `--all` for finished ones) |
| start | `claude --bg <prompt>` in the chosen directory |
| attach | `claude attach <job id>` |
| resume | `claude --resume <conversation uuid>` |
| stop | `claude stop <job id>` |
| delete | `claude stop` if still listed as active, then `claude rm` |

# Two identifiers

**A background job ID and a conversation UUID are different things, and agnt keeps them apart.**[^adr-0001] The job ID is a runtime handle used by `attach` and `stop`. The UUID is the [session key](/model/session-key.md) used by `--resume`. Resuming restores a conversation; it does not mean the old process was kept.

# Execution model

Claude runs sessions in its own background service. agnt is a client: it starts, observes, and attaches, and work keeps running when you return to the list. The [conversation pane](/ui/conversation-pane.md) reads `~/.claude/projects/<cwd slug>/<session id>.jsonl`.

[^claude-rs]: src/providers/claude.rs
[^adr-0001]: ADR-0001, D3 and D4
