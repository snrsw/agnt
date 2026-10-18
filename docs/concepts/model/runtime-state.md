---
type: Enumeration
title: Runtime state
description: What the provider says the session is doing now; observed, never set by the user.
resource: https://github.com/snrsw/agnt/blob/main/src/model/state.rs
tags: [model, state]
generated: { by: claude-opus-5-5, at: 2026-10-18T00:00:00Z }
sources:
  - id: state-rs
    resource: https://github.com/snrsw/agnt/blob/main/src/model/state.rs
    title: src/model/state.rs
  - id: adr-0001
    resource: https://github.com/snrsw/agnt/blob/main/docs/adr/0001-architecture.md
    title: "ADR-0001, D5"
---

# Values

Runtime state comes only from the provider. The user cannot set it.[^state-rs]

| Value | Label | Sort rank | Live |
|---|---|---|---|
| `waiting_approval` | Needs approval | 0 | yes |
| `waiting_input` | Needs input | 0 | yes |
| `error` | Error | 1 | no |
| `running` | Working | 2 | yes |
| `starting` | Starting | 3 | yes |
| `idle` | Idle | 4 | yes |
| `stopped` | Stopped | 5 | no |
| `unknown` | Unknown | 6 | no |

The Status [view](/ui/views.md) sorts by this rank, so sessions that need the user come first.

# Unknown is honest, not a failure

**A state agnt did not observe is Unknown, never a guess.**[^adr-0001] Examples:

* A row loaded from the cache before the first refresh.
* A Codex session, because Codex has no observation interface agnt can use yet (see [Codex](/providers/codex.md)).
* A value from a newer provider that this agnt does not recognise; it deserializes to Unknown instead of failing.

When an observation goes stale, the previous state is kept as `last_known_state`. The row then reads `Unknown (Needs approval)`, telling the user what it was without claiming it still is.

# What it never does

* The end of an agent's turn is not "done". Completion belongs to [workflow state](/model/workflow-state.md), which the user sets.
* A runtime change never moves the workflow state.

[^state-rs]: src/model/state.rs
[^adr-0001]: ADR-0001, D5
