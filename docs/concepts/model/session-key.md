---
type: Identifier
title: Session key
description: The permanent identity of a conversation, separate from the process running it.
resource: https://github.com/snrsw/agnt/blob/main/src/model/key.rs
tags: [model, identity]
generated: { by: claude-opus-5-5, at: 2026-10-18T00:00:00Z }
sources:
  - id: key-rs
    resource: https://github.com/snrsw/agnt/blob/main/src/model/key.rs
    title: src/model/key.rs
  - id: session-rs
    resource: https://github.com/snrsw/agnt/blob/main/src/model/session.rs
    title: src/model/session.rs
---

# Shape

A session key is `provider + scope + native_session_id`, written `provider:scope:native_session_id`.[^key-rs]

| Part | Meaning | Example |
|---|---|---|
| `provider` | The tool that owns the conversation | `claude`, `codex` |
| `scope` | A stable local profile or storage area | the default profile |
| `native_session_id` | The provider's own conversation ID | a UUID |

# Identity is not the running process

**The key never contains a PID, job ID, or server connection.** Those live in a separate runtime handle (`ClaudeJob`, `Pid`, `CodexThread`).[^session-rs] A restarted job or a new server connection therefore stays the same conversation, so the user's [metadata](/model/session.md) stays attached to it.

# Resolving what the user types

The CLI accepts a qualified key, an exact native ID, or a unique prefix of a native ID (optionally `provider:prefix`). A prefix shared by several sessions is an error, not a guess. The 8-character short ID is for display only and never used for lookup on its own.

[^key-rs]: src/model/key.rs
[^session-rs]: src/model/session.rs
