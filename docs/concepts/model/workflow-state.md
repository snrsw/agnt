---
type: Enumeration
title: Workflow state
description: The user's own classification of a session; only the user changes it.
resource: https://github.com/snrsw/agnt/blob/main/src/model/state.rs
tags: [model, state]
generated: { by: claude-opus-5-5, at: 2026-10-18T00:00:00Z }
sources:
  - id: state-rs
    resource: https://github.com/snrsw/agnt/blob/main/src/model/state.rs
    title: src/model/state.rs
---

# Values

| Value | Label | Sort rank |
|---|---|---|
| `in_progress` | In progress | 0 |
| `review` | Review | 1 |
| `unclassified` | Unclassified (default) | 2 |
| `done` | Done | 3 |

`Ctrl+G` cycles a session through Unclassified → In progress → Review → Done → Unclassified. The Workflow [view](/ui/views.md) groups by this value.

# Only the user moves it

**A finished turn, a merged PR, or a stopped process never changes the workflow state.**[^state-rs] It records what the user thinks of the work. [Runtime state](/model/runtime-state.md) records what the process is doing. Keeping them apart means agnt never claims work is finished because an agent stopped talking.

The value is stored in `metadata.json`; see [Data ownership](/storage/data-ownership.md).

[^state-rs]: src/model/state.rs
