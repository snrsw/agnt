---
type: View
title: Prompt
description: The always-live input at the bottom - type a task to start, or switch to your real shell with Shift+Tab.
resource: https://github.com/snrsw/agnt/blob/main/src/shell.rs
tags: [ui, shell]
generated: { by: claude-opus-5-5, at: 2026-10-18T00:00:00Z }
sources:
  - id: readme
    resource: https://github.com/snrsw/agnt/blob/main/README.md
    title: README
  - id: shell-rs
    resource: https://github.com/snrsw/agnt/blob/main/src/shell.rs
    title: src/shell.rs
  - id: adr-0001
    resource: https://github.com/snrsw/agnt/blob/main/docs/adr/0001-architecture.md
    title: "ADR-0001, D4 amendment"
---

# Task mode

**The prompt is always live: there is no mode to enter before typing.**[^readme] Actions on the list use Ctrl chords, so typing never triggers one.

| Input | Result |
|---|---|
| Text, then Enter | [Start](/operations/start.md) a session with that task |
| Enter on an empty prompt | [Attach or resume](/operations/attach-and-resume.md) the selected session |
| `Tab` | Switch the agent between Claude and Codex |
| `?` on an empty prompt | List every key |

# Shell mode

`Shift+Tab` puts the user's login shell, running on a pseudo-terminal, in the same box.[^shell-rs] Completion, aliases, history, and full-screen programs work, because the real shell (fish, zsh, bash) is doing them. `Shift+Tab` returns to task mode with the typed text kept.

**The directory the shell ends in is where the next session starts.** Shell mode exists to pick that directory without leaving agnt.

agnt opens the shell set with `chsh`, not `$SHELL`, because `nix develop` overrides `$SHELL`. `AGNT_SHELL` overrides both.[^readme]

# Why the shell is on a pty but agents are not

The ADR defers agnt owning the pty a provider's CLI runs on.[^adr-0001] That would mean keeping a hidden screen, reading output continuously, and keeping work alive after agnt exits. The shell has none of these needs: it is always visible, it is the user's own, and it should end with agnt. An earlier version that sent single lines to `$SHELL -c` was replaced because it had to reimplement what an interactive shell already does.

[^readme]: README
[^shell-rs]: src/shell.rs
[^adr-0001]: ADR-0001, D4 amendment
