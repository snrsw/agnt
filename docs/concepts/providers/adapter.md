---
type: Interface
title: Adapter
description: The one interface every provider implements, so the core and TUI never see tool-specific behaviour.
resource: https://github.com/snrsw/agnt/blob/main/src/providers/mod.rs
tags: [providers, architecture]
generated: { by: claude-opus-5-5, at: 2026-10-18T00:00:00Z }
sources:
  - id: providers-mod
    resource: https://github.com/snrsw/agnt/blob/main/src/providers/mod.rs
    title: src/providers/mod.rs
  - id: adr-0001
    resource: https://github.com/snrsw/agnt/blob/main/docs/adr/0001-architecture.md
    title: "ADR-0001, D3"
---

# Role

The core and the TUI work only with the shared [Session](/model/session.md) model and the `Adapter` trait. **Every tool-specific quirk stays inside one adapter module**, so a change in Claude Code or Codex touches one file.[^adr-0001] Both adapters are compiled into the binary; there is no plugin mechanism.

# Operations

| Method | Purpose |
|---|---|
| `capabilities` | Which operations work on this machine, and why not; see [Capabilities](/providers/capabilities.md) |
| `discover` | List sessions, with a note on what the listing covers |
| `observe` | Refresh [runtime state](/model/runtime-state.md) for given keys |
| `preview` | A short excerpt for the row |
| `conversation` | Recent turns for the [conversation pane](/ui/conversation-pane.md) |
| `start` | Begin a new session; see [Start](/operations/start.md) |
| `attach`, `resume` | Return a command to hand the terminal to; see [Attach and resume](/operations/attach-and-resume.md) |
| `stop` | End the execution; returns Confirmed or Requested |
| `delete` | Remove the session through the provider's CLI; see [Hide and delete](/operations/hide-and-delete.md) |

# Differences are typed, not hidden

Where the tools differ, the return type says so instead of pretending they are alike.[^providers-mod]

* `start` returns either a started session with its identity (Claude) or a terminal hand-off after which the new session is discovered (Codex). Inventing an identity for a session that does not exist yet would be wrong.
* `stop` returns `Confirmed` when the provider confirms the end, or `Requested` when the end must be observed later.
* A hand-off is an argument array plus a working directory and a note, never a shell string.

[^providers-mod]: src/providers/mod.rs
[^adr-0001]: ADR-0001, D3
