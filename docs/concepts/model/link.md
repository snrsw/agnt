---
type: Entity
title: Link
description: A PR, issue, or other URL attached to a session, ranked by where it came from.
resource: https://github.com/snrsw/agnt/blob/main/src/model/link.rs
tags: [model, github]
generated: { by: claude-opus-5-5, at: 2026-10-18T00:00:00Z }
sources:
  - id: link-rs
    resource: https://github.com/snrsw/agnt/blob/main/src/model/link.rs
    title: src/model/link.rs
  - id: session-rs
    resource: https://github.com/snrsw/agnt/blob/main/src/model/session.rs
    title: src/model/session.rs
---

# Kinds

A GitHub PR or issue URL becomes `PullRequest` or `Issue` with its repository and number; the list shows `PR #123` or `issue #98`. Any other http(s) URL becomes `Other`.[^link-rs]

# Sources and priority

| Source | Set by | Display priority |
|---|---|---|
| `manual` | The user, with `Ctrl+L` | 1 |
| `provider` | A field the provider reports | 2 |
| `inferred` | agnt, from evidence such as a branch name | 3 |

**An inferred link never overwrites a manual one, and it is shown as a guess.** When two sources give the same URL, the higher-priority source wins.[^session-rs] As of 2026-10-18 the model supports inferred links, but no code produces them yet; GitHub support is limited to detecting `gh`.

Each link keeps the evidence it was attached from. A later HEAD change in the same directory therefore does not silently re-link the session.

Manual links are stored in `metadata.json`; observed links only in the cache. See [Session](/model/session.md).

[^link-rs]: src/model/link.rs
[^session-rs]: src/model/session.rs
