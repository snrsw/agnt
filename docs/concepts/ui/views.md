---
type: View
title: Views
description: Three ways to group the list - by runtime state, by repository, or by workflow state.
resource: https://github.com/snrsw/agnt/blob/main/src/model/state.rs
tags: [ui]
generated: { by: claude-opus-5-5, at: 2026-10-18T00:00:00Z }
sources:
  - id: state-rs
    resource: https://github.com/snrsw/agnt/blob/main/src/model/state.rs
    title: src/model/state.rs
---

`Ctrl+T` cycles the three views. Each view answers a different question.[^state-rs]

| View | Groups by | Row shape | Question it answers |
|---|---|---|---|
| Status (default) | [Runtime state](/model/runtime-state.md) | Two-line card | What needs me now? |
| Repository | Repository and worktree | One-line row | What was I doing in this repo? |
| Workflow | [Workflow state](/model/workflow-state.md) | Two-line card | Where is each piece of work in my process? |

# Row shapes

* A card puts the task on its own line and the details under it: state, provider, directory, [links](/model/link.md), age.
* A compact row sits under a repository heading. The repository is already known, so the branch or worktree name tells rows apart.

# Order inside a group

Rows sort by the group's rank, then by most recent activity, then by [session key](/model/session-key.md) for a stable order.

# What is folded away

History rows (stopped, or unknown and only in saved history; see [Origin](/model/origin.md)) are folded away; `Ctrl+O` shows them. Hidden rows are left out; `Ctrl+Y` shows them. `Ctrl+F` searches titles, paths, IDs, and links.

[^state-rs]: src/model/state.rs
