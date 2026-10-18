---
type: View
title: Conversation pane
description: The selected session's recent turns, read from the files each tool already writes.
resource: https://github.com/snrsw/agnt/blob/main/src/transcript.rs
tags: [ui]
generated: { by: claude-opus-5-5, at: 2026-10-18T00:00:00Z }
sources:
  - id: transcript-rs
    resource: https://github.com/snrsw/agnt/blob/main/src/transcript.rs
    title: src/transcript.rs
  - id: adr-0001
    resource: https://github.com/snrsw/agnt/blob/main/docs/adr/0001-architecture.md
    title: "ADR-0001, D7"
---

# What it shows

Beside the list, the pane shows the last 40 user and assistant turns of the selected [session](/model/session.md). It is read-only; to reply, [attach or resume](/operations/attach-and-resume.md).

# Where the text comes from

agnt reads the files each tool already writes. It does not copy them.[^transcript-rs]

| Provider | File |
|---|---|
| [Claude Code](/providers/claude-code.md) | `~/.claude/projects/<cwd slug>/<session id>.jsonl` |
| [Codex](/providers/codex.md) | Rollout files under `~/.codex/sessions` |

A session that moved into a worktree keeps its file under another project directory, so every directory is searched for the session ID.

# Tolerant parsing

* Only user and assistant text is kept. Messages the tools inject on the user's side (command wrappers, reminders, environment blocks) are dropped.
* An unknown record is skipped, so a newer file format gives fewer turns instead of an error.
* Reading is done in the background; reading a whole conversation is limited to this pane, never the list.[^adr-0001]

[^transcript-rs]: src/transcript.rs
[^adr-0001]: ADR-0001, D7
