---
type: Enumeration
title: Origin
description: How agnt came to know about a session - started by agnt, discovered live, or found in history.
resource: https://github.com/snrsw/agnt/blob/main/src/model/session.rs
tags: [model]
generated: { by: claude-opus-5-5, at: 2026-10-18T00:00:00Z }
sources:
  - id: session-rs
    resource: https://github.com/snrsw/agnt/blob/main/src/model/session.rs
    title: src/model/session.rs
---

| Value | Meaning |
|---|---|
| `managed` | agnt [started](/operations/start.md) it |
| `discovered` | The provider's official listing reports it live |
| `history` | It exists only in saved history files (default) |

# Why it matters

Origin decides whether an Unknown row is folded away as history.[^session-rs] [Runtime state](/model/runtime-state.md) Unknown means two different things:

* A conversation that exists only as a saved file.
* A live session not observed yet, such as every row loaded from the cache before the first refresh.

Folding on the state alone would empty the list on a cold start. So an Unknown row folds only when its origin is `history`; a session the provider still lists stays visible.

[^session-rs]: src/model/session.rs
