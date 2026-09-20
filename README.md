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
  ◆ Claude    ~/src/api  fix the flaky billing test_
──────────────────────────────────────────────────────────
enter to start · tab for agent · esc to clear
```

The agent and the directory are bubbles, in the style of oh-my-posh's [`bubblesextra`](https://github.com/JanDeDobbeleer/oh-my-posh/blob/main/themes/bubblesextra.omp.json), and so is each row's second line — so if you run that in your shell, both prompts look like the same machine. **It needs a Nerd Font**: without one the rounded caps come out as blank boxes or, worse, as glyphs your terminal gives a different width, which knocks every column after them sideways. That cannot be detected from inside a terminal, so `AGNT_STYLE=plain` gives the same prompt with the same words, in named ANSI colours and no special glyphs.

The prompt says where the task is going before it says anything else: the agent that will take it, then the directory it will start in, then what you type. Those two used to sit on a line underneath, which meant reading down to answer "where is this going?" while typing across — and they are the subject of the sentence being written, so they belong in front of it, the way a shell puts the directory in front of the command. `Tab` changes the agent and the prompt changes with it. On a narrow terminal the path gives way first, keeping the components that say which checkout it is.

The task text is the only thing you have to type. Because every printable key belongs to the prompt, **the actions are Ctrl chords**, and the footer shows only the few that matter right now; `?` on an empty prompt lists the rest.

- **`Tab` switches the agent**, Claude to Codex and back. The agent is shown under the prompt, so what Enter will start is always visible.
- **`S-Tab` puts your shell in the text area** — the real one, running on a pseudo-terminal. Same box, same place; what changes is what is in it. It is `$SHELL` started as a login shell, so **tab completion, abbreviations, aliases, autosuggestions, colours, your key bindings, your prompt and your history all work**, because your shell is doing them, not agnt's imitation of them. Anything that wants a terminal — an editor, a pager, `git commit` — works too. Every key in that mode belongs to the shell; `S-Tab` is the one agnt keeps, and it comes back to the task, still there half-typed. Wherever the shell ends up is where the next session starts — read from the operating system rather than asked of the shell, so it works whatever your shell is. (It is polled while the shell runs, since a shell that has exited can no longer be asked; a `cd` on the same line as `exit` is too brief to be seen.) `exit` gives the box back to the task prompt.

  The box is as tall as the shell is using: one or two rows at an idle prompt, growing to fit a command's output and stopping at four to twelve rows by screen height. It is a prompt, not a terminal to live in; more output than that scrolls, and the tail is what stays.

This replaced an earlier version that handed single lines to `$SHELL -c` and rebuilt the missing pieces — expanding abbreviations itself, appending a `$PWD` trailer to follow `cd`, probing the shell to find out whether it had understood. Each piece worked; together they were a worse terminal than the one already installed.

**It starts when agnt starts**, not when you first press `S-Tab`, so whatever your shell config costs is paid while you are reading the list. Until the shell has drawn anything the box says which shell it is starting.

agnt answers the terminal queries a shell sends — device attributes, cursor position, and the rest. That sounds like a detail and was worth ten seconds: fish asks what the terminal is and waits ten seconds for a reply before drawing its prompt, and nothing was replying. It now reaches a prompt in about 0.3 seconds here, of which 0.15 is `direnv` sourcing a nix dev shell — your own config, which agnt cannot speed up. To see where yours goes:

```sh
nix develop --command cargo run --example shell_timing            # your shell
nix develop --command cargo run --example shell_timing -- /bin/sh # for contrast
```

It prints each step the shell takes, so a pause has the line responsible right above it.

**Which shell you get is the one the OS has on file for you** — what `chsh` set — not `$SHELL`. `$SHELL` is the conventional answer and it is wrong inside a development shell: `nix develop` exports its own bash as `$SHELL`, so agnt run that way would hand you bash however your own shell is set. `agnt doctor` prints the shell it will open, and says so when it is ignoring a `$SHELL` that disagrees. Set `AGNT_SHELL` to override both.

- **`Enter` is overloaded on the prompt**: it starts the typed task, or attaches to the selected row when the prompt is empty.

The two agents start differently, and the difference is visible rather than smoothed over. Claude starts a background session and the row appears immediately. Codex has no background start, so agnt hands it the terminal; when you leave it, the conversation joins the list on the next refresh.

### Colour

Named ANSI colours only — the prompt's bubbles excepted, which carry `bubblesextra`'s own — so the list takes your terminal's theme rather than bringing its own: amber for needs-you, green for working, magenta for PR and issue links, blue for the cursor bar. Colour never carries meaning alone — every state also has a glyph and a word.

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
     ◆ Claude   Needs input   󰉋 reed   34d
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
| `S-Tab` | Put `$SHELL` in the text area; where it ends up is where the next session starts |
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
