---
type: Policy
title: Data ownership
description: agnt owns only the user's metadata; the providers own the conversations, and the cache is disposable.
resource: https://github.com/snrsw/agnt/blob/main/src/storage/mod.rs
tags: [storage, data]
generated: { by: claude-opus-5-5, at: 2026-10-18T00:00:00Z }
sources:
  - id: readme
    resource: https://github.com/snrsw/agnt/blob/main/README.md
    title: README
  - id: adr-0001
    resource: https://github.com/snrsw/agnt/blob/main/docs/adr/0001-architecture.md
    title: "ADR-0001, D2"
  - id: cache-rs
    resource: https://github.com/snrsw/agnt/blob/main/src/storage/cache.rs
    title: src/storage/cache.rs
---

# Who owns what

| Data | Owner | Where |
|---|---|---|
| Conversations | The provider | The provider's own files |
| Titles, [workflow state](/model/workflow-state.md), manual [links](/model/link.md), hidden markers | The user, through agnt | `metadata.json` |
| Last observed list | Nobody; it is regenerated | `sessions-cache.json` |
| Settings and CLI paths | The user | `config.toml` (optional) |

**agnt never copies conversation text into its own files.**[^adr-0001] Files live in the OS data directory for `agnt`, or in `AGNT_DATA_DIR`.[^readme]

# metadata.json

This is the only data agnt is the source of truth for. Each update takes `metadata.lock`, reloads the file, writes a temporary file in the same directory, and renames it. Renaming alone does not stop a lost update when two processes write at once, so the lock is never skipped.[^adr-0001]

# sessions-cache.json

The cache lets the list draw at once on startup. It holds a capped number of sessions and no conversation text. **A cached state is never treated as confirmed**: rows from it show as a previous observation until the first refresh.[^cache-rs] A corrupt or outdated cache is discarded, not repaired; deleting it is safe.

# Why JSON and not SQLite

The data is small, and few processes write it. The ADR names when to revisit: full-text search, frequent writes from several processes, measured slowness, or complex relationships.[^adr-0001]

[^readme]: README
[^adr-0001]: ADR-0001, D2
[^cache-rs]: src/storage/cache.rs
