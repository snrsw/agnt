# agnt

**agnt is a TUI that finds Claude Code and Codex sessions in one list and gets you back into the work you left.**

One screen shows each session's short content, runtime state, work classification, working directory, and linked PR / issue. It starts from any directory, and aims to keep running work alive when you return to the list.

Status: **early, but it runs against the real CLIs.** Claude sessions are listed, attached, resumed, and stopped through the official commands. Codex is listed and resumed; attaching to a running Codex session still needs the App Server work. See [Current state](#current-state).

## Documents

| Document | Contents |
| --- | --- |
| [docs/design.md](docs/design.md) | Requirements, screens, architecture, data model, performance targets |
| [docs/adr/0001-architecture.md](docs/adr/0001-architecture.md) | Why Rust, why JSON instead of SQLite, how the runtime is chosen |
| [docs/p0-validation.md](docs/p0-validation.md) | The technical checks that must be recorded before adapters do real work |

## Build and run

The development environment is a Nix flake (`nix develop`, or `direnv allow` with the included `.envrc`).

```sh
nix develop --command cargo build
nix develop --command cargo test
nix develop --command cargo run -- doctor
```

Without Nix, a stable Rust toolchain (1.93 or newer, edition 2024) works with plain `cargo`.

All text — documents, code comments, and the strings the UI shows — is in English.

## Trying it locally

Point `AGNT_DATA_DIR` at a scratch directory so your real data is untouched, then run it:

```sh
export AGNT_DATA_DIR=/tmp/agnt-demo
nix develop --command cargo run -- list        # your real Claude and Codex sessions
nix develop --command cargo run                # the TUI
```

To start a session, press `n` and type what it should do:

```
> fix the flaky billing test_
  Claude - ~/src/api                    ^a agent  ^d directory  Enter start  Esc cancel
```

The task text is the only thing you have to type. The agent defaults to Claude and the directory to where agnt was started; `^a` and `^d` change them, and `^d` walks the directories already in the list, so starting work in a repo you can see takes two keystrokes instead of a typed path. Codex has no background start, so choosing it there says so rather than pretending.

### Keys in the list

| Key | Action |
| --- | --- |
| `j` / `k`, arrows | Move |
| `Enter` | Attach when running, resume when stopped, otherwise ask |
| `n` | Focus the new-session prompt: type the task, Enter starts it |
| `/` | Search title, path, and link number |
| `Tab` | Group by runtime state or by work classification |
| `m` | Change work classification |
| `l` | Register a PR / issue URL |
| `d` | Hide dialog; hide a running session or stop it first. On a hidden row, restore it |
| `r` | Request a refresh |
| `H` | Show hidden sessions |
| `q` | Leave the list |

These apply while agnt owns the terminal. Inside an official CLI screen, that CLI's keys apply.

If neither CLI has any sessions, seed the cache with samples instead:

```sh
nix develop --command cargo run --example seed_demo
```

Two ways to look at it without a terminal session:

```sh
nix develop --command cargo run --example render_dump  # the list drawn at 120 and 70 columns
nix develop --command cargo run -- list --json
```

Delete `$AGNT_DATA_DIR` to start over.

## Current state

| Area | State |
| --- | --- |
| Session model, ID resolution, state and classification | Implemented, tested |
| Metadata JSON with lock + atomic rename, cache | Implemented, tested |
| List, grouping, search, dialogs, terminal hand-off | Implemented |
| CLI subcommands | Implemented |
| Claude adapter | Real: discover, observe, attach, resume, stop, start |
| Codex adapter | Discover and resume; attach, observe, and stop need the App Server |
| GitHub enrichment | `gh` detection only; branch to PR candidates are P2 |

Claude runs on the official CLI: `claude agents --json` for the list, `claude attach <id>` to attach, plus `stop`, `--resume`, and `--bg`. Codex has no scriptable listing — `codex agents` is itself a TUI — so its past conversations come from a declared read-only reader over `~/.codex/sessions`. A file proves a conversation existed, never that it is running, so every Codex row is Unknown until the App Server work lands.

Anything unsupported says why, per operation, in `agnt doctor`. Preview is unsupported on both: `claude logs` returns the terminal screen rather than conversation content.

[docs/p0-validation.md](docs/p0-validation.md) records what was validated and what was not — notably that starting, stopping, and the interactive attach round trip have not been run yet.

## Data

Files live in the OS data directory for `agnt` (override with `AGNT_DATA_DIR`).

| File | Contents | Source of truth |
| --- | --- | --- |
| `config.toml` | Optional settings, CLI paths, endpoints | Yes, for settings |
| `metadata.json` | Manual titles, classification, links, hidden markers | Yes, for agnt metadata |
| `metadata.lock` | Exclusion during updates | Not data |
| `sessions-cache.json` | Capped list snapshot | No; discard and refetch |

Conversation text is never copied into agnt's data. Each tool keeps its own history, and `hide` removes a row from the list without deleting anything.
