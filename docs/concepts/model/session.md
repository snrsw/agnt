---
type: Entity
title: Session
description: One conversation with a coding agent, as agnt sees it - provider observation joined with user metadata.
resource: https://github.com/snrsw/agnt/blob/main/src/model/session.rs
tags: [model]
generated: { by: claude-opus-5-5, at: 2026-10-18T00:00:00Z }
sources:
  - id: session-rs
    resource: https://github.com/snrsw/agnt/blob/main/src/model/session.rs
    title: src/model/session.rs
---

# Definition

A session is one conversation in Claude Code or Codex. It is identified by its [session key](/model/session-key.md) and appears as one row in the list.

**Every session is built from two halves with different owners.**[^session-rs]

| Half | Type | Owner | Persisted in |
|---|---|---|---|
| Observation | `Session` | The provider | `sessions-cache.json` (regenerable) |
| Metadata | `SessionMeta` | The user | `metadata.json` (authoritative) |

`SessionView` joins the two for display and for `agnt list --json`. See [Data ownership](/storage/data-ownership.md) for the files.

# Observation fields

| Field | Meaning |
|---|---|
| `title`, `first_prompt`, `preview` | Text the provider reports about the task |
| `cwd` | Working directory the session runs in |
| `launch_git`, `observed_git` | Branch at launch and branch seen later |
| `runtime` | [Runtime state](/model/runtime-state.md), its evidence, and the time it was observed |
| `links` | [Links](/model/link.md) the provider reported or agnt inferred |
| `origin` | [Origin](/model/origin.md) |
| `last_activity_at` | Orders rows within a group |

# Metadata fields

| Field | Meaning |
|---|---|
| `title_override` | Title the user typed |
| `workflow` | [Workflow state](/model/workflow-state.md) |
| `hidden_at` | Set when the user [hides](/operations/hide-and-delete.md) the row |
| `links` | Links the user added; they win over observed ones |

# Derived values

* **Title**: the first non-empty of user override, provider title, an excerpt of the first prompt (60 characters), and the short ID.
* **Branch**: the launch-time branch wins, because a later HEAD change in a shared directory does not say which conversation moved.
* **Repository and worktree**: derived from the path, not from git, so grouping needs no command per row. `<repo>/.claude/worktrees/<name>` and `<repo>/.worktrees/<name>` are recognised.
* **History row**: a Stopped session, or an Unknown one whose origin is History. These fold away by default (`Ctrl+O` shows them).

Both halves keep unknown JSON fields as they are, so a file written by a newer agnt survives an older one.

[^session-rs]: src/model/session.rs
