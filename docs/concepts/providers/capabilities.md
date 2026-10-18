---
type: Policy
title: Capabilities
description: Per-operation support with a reason, so an unsupported operation is never reported as success.
resource: https://github.com/snrsw/agnt/blob/main/src/model/capability.rs
tags: [providers]
generated: { by: claude-opus-5-5, at: 2026-10-18T00:00:00Z }
sources:
  - id: capability-rs
    resource: https://github.com/snrsw/agnt/blob/main/src/model/capability.rs
    title: src/model/capability.rs
  - id: readme
    resource: https://github.com/snrsw/agnt/blob/main/README.md
    title: README
---

# Rule

Each [adapter](/providers/adapter.md) reports, for each of eight operations, either `Supported` or `Unsupported { reason }`.[^capability-rs] **An unsupported operation fails with its reason; it is never reported as success.**

Operations: discover, observe, preview, start, attach, resume, stop, delete.

# Where the reasons show

* `agnt doctor` lists every operation per provider with the reason it does not work, such as a missing CLI or a protocol not yet validated.[^readme]
* The TUI shows the reason when the user tries an unsupported action.
* When a CLI is missing entirely, every operation is unsupported with the same reason.

# Current support

| Operation | [Claude Code](/providers/claude-code.md) | [Codex](/providers/codex.md) |
|---|---|---|
| discover | Supported | Supported |
| observe | Supported | Unsupported |
| preview | Supported | Unsupported |
| start | Supported | Supported |
| attach | Supported | Unsupported |
| resume | Supported | Supported |
| stop | Supported | Unsupported |
| delete | Supported | Supported |

[^capability-rs]: src/model/capability.rs
[^readme]: README
