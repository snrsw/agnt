---
type: Product
title: agnt
description: A TUI that lists Claude Code and Codex sessions in one place and gets you back into them.
resource: https://github.com/snrsw/agnt
tags: [overview]
generated: { by: claude-opus-5-5, at: 2026-10-18T00:00:00Z }
sources:
  - id: readme
    resource: https://github.com/snrsw/agnt/blob/main/README.md
    title: README
  - id: adr-0001
    resource: https://github.com/snrsw/agnt/blob/main/docs/adr/0001-architecture.md
    title: "ADR-0001: agnt's basic structure"
---

# What it is

agnt is a single Rust binary that shows every coding-agent [session](/model/session.md) on the machine in one list, and moves you into the one you pick.[^readme]

**agnt is a switchboard, not an agent.** It does not run models, draw its own chat UI, or coordinate agents. The official CLIs (`claude`, `codex`) do the work; agnt observes them and hands the terminal over.[^adr-0001]

# The problem it solves

With several agents running in several repositories, the user needs to know:

* Which session needs me now (waiting for approval or input)?
* What was each session about, and which PR or issue does it belong to?
* How do I get back into it, or start a new one in the right directory?

agnt answers these from one screen. The [views](/ui/views.md) put sessions that need you first. Each row carries a title and its [links](/model/link.md). The [prompt](/ui/prompt.md) starts a new session or attaches to the selected one.

# Core loop

1. Open agnt. The list draws from the cache at once, then refreshes in the background.
2. Move to a row; the [conversation pane](/ui/conversation-pane.md) shows its recent turns.
3. Press Enter on an empty prompt to [attach](/operations/attach-and-resume.md). Leave the CLI to return to the list.
4. Or type a task and press Enter to [start](/operations/start.md) a new session.

# Out of scope

| Not done | Reason |
|---|---|
| Its own conversation UI | The official CLI already is one; agnt hands the terminal over |
| Coordinating agents | agnt manages sessions, not the work between them |
| LLM summaries of sessions | Titles and existing previews are enough, with no extra cost or delay |
| Creating worktrees | Left to the providers |

The rules behind these choices are in [Design principles](/product/design-principles.md).

[^readme]: README
[^adr-0001]: ADR-0001
