# agnt

**agnt is a TUI that finds Claude Code and Codex sessions in one list and gets you back into the work you left.**

One screen shows each session's short content, runtime state, work classification, working directory, and linked PR / issue. It starts from any directory, and aims to keep running work alive when you return to the list.

Status: **skeleton**. The model, storage, terminal handling, and UI are implemented and tested. Both provider adapters report every operation as unsupported until the P0 checks in [docs/p0-validation.md](docs/p0-validation.md) are recorded — see [Current state](#current-state).

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

**The adapters are stubs, so a plain `agnt` finds nothing.** To drive the real TUI, seed the list cache with sample sessions and point `AGNT_DATA_DIR` at a scratch directory so your real data is untouched.

```sh
export AGNT_DATA_DIR=/tmp/agnt-demo
nix develop --command cargo run --example seed_demo   # four sample sessions
nix develop --command cargo run                       # the TUI, with rows
```

Everything that does not need a provider works on that data: moving, grouping with `Tab`, searching with `/`, classifying with `m`, linking with `l`, hiding and restoring with `d`, and the dialogs. `Enter` and `n` reach the adapters and report why the operation is unsupported.

Seeded rows show as `Unknown (Running)` and so on. That is deliberate: a state read from the cache is a previous observation, never a confirmed one.

Two more ways to look at it without a terminal session:

```sh
nix develop --command cargo run --example render_dump  # the list drawn at 120 and 70 columns
nix develop --command cargo run -- list --no-refresh   # the same rows as text
```

Delete `$AGNT_DATA_DIR` to start over.

## Commands

```sh
agnt                                  # the list
agnt list --json                      # machine-readable list
agnt new claude --cwd ~/src/api       # start with a directory and optional --prompt
agnt attach <id>                      # connect to a running conversation
agnt resume <id>                      # reopen a saved conversation
agnt hide <id> / agnt restore <id>    # hide from the list / undo
agnt stop <id>                        # stop a running session
agnt link <id> <url>                  # attach a PR or issue
agnt doctor                           # supported features, versions, data locations
```

IDs accept a unique prefix or the fully qualified `provider:scope:native_id`. An ambiguous prefix is an error, never a first match.

### Keys in the list

| Key | Action |
| --- | --- |
| `j` / `k`, arrows | Move |
| `Enter` | Attach when running, resume when stopped, otherwise ask |
| `n` | Start a session: agent, cwd, initial prompt |
| `/` | Search title, path, and link number |
| `Tab` | Group by runtime state or by work classification |
| `m` | Change work classification |
| `l` | Register a PR / issue URL |
| `d` | Hide dialog; hide a running session or stop it first |
| `r` | Request a refresh |
| `H` | Show hidden sessions |
| `q` | Leave the list |

These apply while agnt owns the terminal. Inside an official CLI screen, that CLI's keys apply.

## Current state

| Area | State |
| --- | --- |
| Session model, ID resolution, state and classification | Implemented, tested |
| Metadata JSON with lock + atomic rename, cache | Implemented, tested |
| List, grouping, search, dialogs, terminal hand-off | Implemented |
| CLI subcommands | Implemented |
| Claude adapter | Every operation reports `unsupported`, reason: P0 not validated |
| Codex adapter | Every operation reports `unsupported`, reason: P0 not validated |
| GitHub enrichment | `gh` detection only; branch → PR candidates are P2 |

Adapters stay unsupported on purpose. The design doc requires that an unsupported operation never be reported as success, and the two runtime questions — whether the Codex standard CLI can detach without dropping a pending approval, and how past Claude sessions are enumerated — are unanswered. `agnt doctor` prints the reason per operation.

## Data

Files live in the OS data directory for `agnt` (override with `AGNT_DATA_DIR`).

| File | Contents | Source of truth |
| --- | --- | --- |
| `config.toml` | Optional settings, CLI paths, endpoints | Yes, for settings |
| `metadata.json` | Manual titles, classification, links, hidden markers | Yes, for agnt metadata |
| `metadata.lock` | Exclusion during updates | Not data |
| `sessions-cache.json` | Capped list snapshot | No; discard and refetch |

Conversation text is never copied into agnt's data. Each tool keeps its own history, and `hide` removes a row from the list without deleting anything.
