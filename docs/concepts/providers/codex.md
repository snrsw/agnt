---
type: Integration
title: Codex
description: The provider read from saved history; start, resume, and delete work, live observation does not yet.
resource: https://github.com/snrsw/agnt/blob/main/src/providers/codex.rs
tags: [providers, codex]
generated: { by: claude-opus-5-5, at: 2026-10-18T00:00:00Z }
sources:
  - id: codex-rs
    resource: https://github.com/snrsw/agnt/blob/main/src/providers/codex.rs
    title: src/providers/codex.rs
  - id: readme
    resource: https://github.com/snrsw/agnt/blob/main/README.md
    title: README
---

# How agnt drives it

Codex has no scriptable session listing, so **agnt reads `~/.codex/sessions` read-only** to discover sessions.[^readme] History older than 30 days is not read.[^codex-rs]

| Operation | How |
|---|---|
| discover | Read rollout files under `~/.codex/sessions` |
| start | Hand the terminal to `codex` with the prompt and directory; the new session is discovered afterwards |
| resume | Hand the terminal to `codex resume <id>` |
| delete | `codex delete --force <id>`, after agnt's own confirmation |
| observe, attach, stop | Unsupported |

`--force` only skips Codex's interactive prompt, which has no terminal to appear on; agnt has already asked the user.

# Why some operations are missing

Observing, attaching, and stopping a live Codex session need the Codex App Server protocol. That protocol has not been validated on a real machine yet, so these operations report Unsupported rather than guess (see [Capabilities](/providers/capabilities.md)). Every Codex row therefore shows [runtime state](/model/runtime-state.md) Unknown.

[^codex-rs]: src/providers/codex.rs
[^readme]: README
