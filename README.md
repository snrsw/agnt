# agnt

**agnt is a TUI that finds Claude Code and Codex sessions in one list and gets you back into the work you left.**

One screen shows each session's short content, runtime state, work classification, working directory, and linked PR / issue. It starts from any directory, and aims to keep running work alive when you return to the list.

Status: **early, but it runs against the real CLIs.** Claude sessions are listed, started, attached, resumed, and stopped through the official commands. Codex sessions are listed, started, and resumed; attaching to a *running* Codex session still needs the App Server work. See [Current state](#current-state).

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

To start a session, just type what it should do — the prompt at the bottom is always live:

```
──────────────────────────────────────────────────────────
> fix the flaky billing test_
  Claude · ~/src/api
──────────────────────────────────────────────────────────
enter to start · tab for agent · esc to clear
```

The task text is the only thing you have to type. Because every printable key belongs to the prompt, **the actions are Ctrl chords**, and the footer shows only the few that matter right now; `?` on an empty prompt lists the rest.

- **`Tab` switches the agent**, Claude to Codex and back. The agent is shown under the prompt, so what Enter will start is always visible.
- **`S-Tab` drops you into your shell.** Not a shell mode inside agnt — your actual `$SHELL`, given the terminal, the same way attaching to a session is. So tab completion, abbreviations, aliases, colours, your prompt, your history and every key binding you have are the real ones, because it *is* the real shell. Nothing it prints appears inside agnt. Type `exit` (or `Ctrl+D`) to come back; the task you were half-way through typing is still there, and **the directory you left the shell in is where the next session starts**.
- **`Enter` is overloaded on the prompt**: it starts the typed task, or attaches to the selected row when the prompt is empty.

This costs one thing: a process's working directory dies with it, so the shell is asked to write it down on the way out, through the exit hook its own family provides — `fish_exit` for fish, a `trap ... EXIT` sourced after your own config for bash and zsh. Your configuration is sourced first and never replaced. A shell with no hook agnt knows still starts and works normally; it just leaves the directory where it was.

Set `AGNT_SHELL` to use something other than `$SHELL`.

The two agents start differently, and the difference is visible rather than smoothed over. Claude starts a background session and the row appears immediately. Codex has no background start, so agnt hands it the terminal; when you leave it, the conversation joins the list on the next refresh.

### Colour

Named ANSI colours only, so the list takes your terminal's theme rather than bringing its own: amber for needs-you, green for working, magenta for PR and issue links, blue for the cursor bar. Colour never carries meaning alone — every state also has a glyph and a word.

Each title line starts with two marks: the state, then the provider.

```
● ◆ claude-code-action auth subscription     ← needs you, Claude
* ▲ investigate a flaky test                 ← working, Codex
```

`◆` is Claude and `▲` is Codex. Neither tool has a brand mark that survives as a single monospace cell, so the shapes are arbitrary; `?` shows the legend, and the word is on the line below.

### The two views

`Ctrl+T` switches what the list is shaped around.

**Status** answers "what needs me now" — cards grouped by state, the task on its own line:

```
── Needs you (1)
> ● claude-code-action auth subscription                        issue #880
    Claude · Needs input · reed · 34d
```

**Repository** answers "what was I doing in reed" — one line each, grouped by repository, with a tally on every heading:

```
── reed (15)                                        ● 1  * 0  ○ 3  · 11
> ● Claude claude-code-action auth subscription                      34d
  ○ Claude reed pr 971 review comments    dr5-dedup                  23h
```

Repositories are ordered by what is in them, not by name. A third turn of `Ctrl+T` gives the cards grouped by your own classification instead of by state.

**Stopped sessions and saved history are folded away**, because they outnumber live work heavily — 50 of 56 rows on the machine this was built on. Their headings stay, marked `▸` and carrying their counts, and `Ctrl+O` brings them back:

```
▸ gh-own (16)                                       ● 0  * 0  ○ 0  · 16
```

Codex history is also bounded: only conversations touched in the last 30 days are read, and the status line says how many older ones were passed over. Codex keeps rollout files forever, and almost all of them are finished work — `codex resume` is the way back to anything older.

A row is history when it has stopped, or when it exists only as a saved file. A session the provider still lists stays visible even while its state is unconfirmed — otherwise the list would be empty on every cold start, since nothing is confirmed until the first refresh answers.

### Keys in the list

| Key | Action |
| --- | --- |
| type | Write a task for a new session — the prompt is always live |
| `Enter` | Start it. On an empty prompt, attach to the selected row instead |
| `↑` `↓` | Move, wrapping at both ends |
| `Tab` | The new session's agent |
| `S-Tab` | Drop into your shell; where you leave it is where the next session starts |
| `Ctrl+T` | Cycle view: status, repository, workflow |
| `Ctrl+F` | Search |
| `Ctrl+O` | Show stopped sessions and saved history |
| `Ctrl+Y` | Show hidden rows |
| `Ctrl+R` | Refresh |
| `Ctrl+X` | Hide the selected row, or restore it |
| `Ctrl+G` | Change its classification |
| `Ctrl+L` | Attach a PR or issue URL to it |
| `Ctrl+U` / `Ctrl+W` | Clear the prompt / delete a word |
| `?` | The full list, on an empty prompt |
| `Ctrl+C` | Quit |

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
| Codex adapter | Discover, resume, and start interactively; attach, observe, and stop need the App Server |
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
