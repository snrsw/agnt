---
type: Principle
title: Design principles
description: The rules that decide how every agnt feature behaves.
tags: [overview, architecture]
generated: { by: claude-opus-5-5, at: 2026-10-18T00:00:00Z }
sources:
  - id: adr-0001
    resource: https://github.com/snrsw/agnt/blob/main/docs/adr/0001-architecture.md
    title: "ADR-0001: agnt's basic structure"
  - id: decisions
    resource: https://github.com/snrsw/agnt/blob/main/decisions.md
    title: Decisions log
---

# Decision criteria

The ADR ranks five criteria; an earlier one wins a conflict.[^adr-0001]

| # | Criterion | What it means in practice |
|---|---|---|
| 1 | Listing and switching are fast | The list draws from memory or the cache first. Provider and GitHub calls run in the background and never block a keystroke |
| 2 | Few settings, dependencies, and moving parts | One binary and a few JSON files. No SQLite, no tmux, no daemon |
| 3 | Execution state is never asserted wrongly | A state that was not observed shows as Unknown, with the last known value. See [Runtime state](/model/runtime-state.md) |
| 4 | The official CLIs are reused | Authentication, permissions, and environment come from `claude` and `codex` unchanged |
| 5 | A provider's change stays local | Tool-specific behaviour lives inside one [adapter](/providers/adapter.md) |

# Rules that follow

* **An unsupported operation is never reported as success.** It returns a reason, which `agnt doctor` and the UI show. See [Capabilities](/providers/capabilities.md).
* **Observed data and user-owned data never change each other.** The provider owns [runtime state](/model/runtime-state.md); the user owns [workflow state](/model/workflow-state.md).
* **The provider owns the conversation.** agnt never copies conversation text, and it deletes only through the provider's CLI.[^decisions] See [Data ownership](/storage/data-ownership.md).
* Commands run as argument arrays, never as shell strings, so user text never reaches a shell parser.

[^adr-0001]: ADR-0001
[^decisions]: Decisions log
