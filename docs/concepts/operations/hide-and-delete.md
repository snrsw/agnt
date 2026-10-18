---
type: Operation
title: Hide and delete
description: Hiding removes a row from agnt's list only; deleting removes the session for good through the provider's CLI.
tags: [operations, data]
generated: { by: claude-opus-5-5, at: 2026-10-18T00:00:00Z }
sources:
  - id: decisions
    resource: https://github.com/snrsw/agnt/blob/main/decisions.md
    title: "DR: Deleting a session from agnt"
  - id: adr-0001
    resource: https://github.com/snrsw/agnt/blob/main/docs/adr/0001-architecture.md
    title: "ADR-0001, D6"
---

# Two separate operations

| | Hide | Delete |
|---|---|---|
| Gesture | `Ctrl+X` (or `agnt hide`) | `Delete` / `Backspace` on an empty prompt, then Enter |
| Touches | agnt's `metadata.json` only | The provider's data |
| Running session | Keeps running, hidden | Stopped first, then deleted |
| Undo | `Ctrl+X` again; `Ctrl+Y` shows hidden rows | None |

# Hide

Hiding stores a `hidden_at` marker on the session's metadata. **The marker keeps the row hidden after every refresh, and it never stops a process.**[^adr-0001]

# Delete

Delete runs the provider's own CLI after agnt asks for confirmation.[^decisions]

* [Claude Code](/providers/claude-code.md): `claude stop` if the session is active, then `claude rm`.
* [Codex](/providers/codex.md): `codex delete`.

**agnt never passes flags that override the provider's own refusals.** For example, Claude refuses to remove a worktree with unpushed commits; agnt shows Claude's message and stops. agnt forgets its own record of the session only after the provider confirms the delete.

# Why agnt does not delete files itself

The provider owns the conversation data and its safety checks. Deleting files directly would skip those checks and break when the storage layout changes.[^decisions] See [Data ownership](/storage/data-ownership.md).

[^decisions]: "DR: Deleting a session from agnt"
[^adr-0001]: ADR-0001, D6
