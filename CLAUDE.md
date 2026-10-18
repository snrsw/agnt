# agnt

A TUI that lists Claude Code and Codex sessions in one place. Usage is in [README.md](README.md), architecture in [docs/adr/0001-architecture.md](docs/adr/0001-architecture.md), decisions in [decisions.md](decisions.md).

## Build and test

Run cargo inside the Nix dev shell: `nix develop --command cargo test`. Outside it, linking fails with `library not found for -liconv`.

## Keep the concept docs in sync

`docs/concepts/` is an OKF v0.2 bundle that explains what agnt's concepts mean: sessions, runtime and workflow state, providers, operations, views, data ownership. Load the `okf` skill before editing it.

**When a change alters what a concept means or how it behaves, update its concept doc in the same PR.** Examples that count as concept drift:

- A new or changed `RuntimeState`, `WorkflowState`, `Origin`, `View`, or `LinkSource` value.
- A provider gains or loses an operation (`Capabilities` changes).
- A key, gesture, or operation changes meaning (for example, what Enter or Delete does).
- A file agnt writes, or what it stores there, changes.

Each concept change takes three edits:

1. The concept file. Update the facts, `sources`, and footnotes.
2. The `index.md` entry in its folder, when the title or description changes.
3. A dated entry at the top of `docs/concepts/log.md`.

A refactor that leaves behaviour unchanged needs no doc update.

Never name a concept file `claude.md`. On a case-insensitive filesystem Claude Code loads it as `CLAUDE.md`.
