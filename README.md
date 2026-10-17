# agnt

**agnt is a TUI that lists your Claude Code and Codex sessions in one place and gets you back into them.**

Each row shows the session's task, runtime state, classification, working directory, and linked PR or issue. Type a task to start a new session from the same screen.

Status: **early, but it runs against the real CLIs.** Claude sessions can be listed, started, attached, resumed, and stopped. Codex sessions can be listed, started, and resumed; attaching to a running Codex session is not supported yet.

## Build and run

The development environment is a Nix flake (`nix develop`, or `direnv allow`). Without Nix, use stable Rust 1.93 or newer.

```sh
cargo build
cargo test
cargo run -- doctor   # what works on this machine, and why not
cargo run -- list     # your sessions, as text (--json for JSON)
cargo run             # the TUI
```

To try it without touching your real data, set `AGNT_DATA_DIR` to a scratch directory. If you have no sessions yet, `cargo run --example seed_demo` fills the list with samples.

The prompt uses Nerd Font glyphs. Set `AGNT_STYLE=plain` if your font lacks them.

## Usage

The prompt at the bottom is always live: type a task and press `Enter` to start a session. Actions are Ctrl chords; `?` on an empty prompt lists them all.

| Key | Action |
| --- | --- |
| type, `Enter` | Start a session with the typed task |
| `Enter` on an empty prompt | Attach to the selected session |
| `↑` `↓` | Move |
| `Tab` | Switch the agent (Claude / Codex) |
| `S-Tab` | Switch to your shell and back |
| `Ctrl+T` | Cycle view: status, repository, workflow |
| `Ctrl+F` | Search |
| `Ctrl+O` | Show stopped sessions and saved history |
| `Ctrl+Y` | Show hidden rows |
| `Ctrl+R` | Refresh |
| `Ctrl+X` | Hide or restore the selected row |
| `Delete` | Delete the selected session for good (Enter confirms) |
| `Ctrl+G` | Change its classification |
| `Ctrl+L` | Link a PR or issue URL |
| `Ctrl+C` | Quit |

### Shell mode

`S-Tab` puts your login shell, running on a pseudo-terminal, in the prompt box. Completion, aliases, history, and full-screen programs all work. The directory the shell ends in is where the next session starts. `S-Tab` returns to the task prompt with your text intact.

agnt opens the shell set with `chsh`, not `$SHELL`, because `nix develop` overrides `$SHELL`. Set `AGNT_SHELL` to choose another. `cargo run --example shell_timing` shows where your shell's startup time goes.

### Views

- **Status** groups sessions by state, so what needs you is at the top.
- **Repository** groups sessions by repository, one line each.
- **Workflow** groups sessions by your own classification.

Stopped sessions and saved history are folded away by default (`Ctrl+O` shows them). Codex history older than 30 days is not read.

## Current state

| Area | State |
| --- | --- |
| Claude | Discover, observe, start, attach, resume, stop, delete |
| Codex | Discover, start, resume, delete; attach, observe, and stop need the Codex App Server |
| GitHub | `gh` detection only |

Claude support uses the official CLI (`claude agents --json`, `claude attach`, `--resume`, `--bg`). Codex has no scriptable listing, so agnt reads `~/.codex/sessions` read-only; Codex rows show state Unknown. `agnt doctor` lists each unsupported operation and the reason.

Architecture decisions are in [docs/adr/0001-architecture.md](docs/adr/0001-architecture.md).

## Data

Files live in the OS data directory for `agnt`, or in `AGNT_DATA_DIR`.

| File | Contents |
| --- | --- |
| `config.toml` | Optional settings and CLI paths |
| `metadata.json` | Your titles, classifications, links, hidden markers |
| `metadata.lock` | Lock held during updates |
| `sessions-cache.json` | List snapshot; safe to delete |

agnt never copies conversation text. `hide` removes a row from the list without deleting anything. `delete` (or `agnt delete <id>`) permanently deletes the session through `claude rm` or `codex delete`; a running session must be stopped first.
