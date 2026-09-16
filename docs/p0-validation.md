# P0 technical validation checklist

- Related documents: [design.md](design.md) §13, [adr/0001-architecture.md](adr/0001-architecture.md) §6
- Status: **partly done.** Claude's listing and operations are recorded; Codex's live path is not

**This validation exists to choose the runtime, and comes before polishing the UI.** Until a check's result is recorded here, the corresponding adapter operation keeps returning `unsupported`. A feature with no recorded result is never displayed as supported.

## Environment recorded

| Item | Value |
| --- | --- |
| OS / version | macOS, Darwin 25.3.0 (arm64) |
| Claude Code version | 2.1.270 |
| Codex CLI version | codex-cli 0.154.0 |
| Terminal | not recorded; the interactive checks below are still open |
| Date run | 2026-09-16 |

## What each CLI offers

| Need | Claude | Codex |
| --- | --- | --- |
| Scriptable listing | `claude agents --json`, `--all` adds completed. No TTY needed | none: `codex agents` is itself a TUI |
| Attach to a running session | `claude attach <id>` | only through the App Server protocol |
| Reopen a saved conversation | `claude --resume <sessionId>` | `codex resume <SESSION_ID>` |
| Stop | `claude stop <id>` | only through the App Server protocol |
| Recent output | `claude logs <id>`, a terminal screen dump | none found |
| Start in the background | `claude --bg <prompt>`, prints the id | none; `codex` starts interactively |

## Checks

| # | Check | Result |
| --- | --- | --- |
| V1 | Fetch the real JSON / API | **Done for Claude.** Shape and state mapping below. **Codex has no JSON listing**; discovery reads its session files instead |
| V2 | Start a new session and get its ID | **Not run.** `claude --bg` is implemented from its documented output but has not been executed; it would create a real session |
| V3 | Claude start, attach, and return | **Partly.** `claude attach <id>` is wired and its documented behaviour recorded; the interactive round trip has not been run in a terminal |
| V4 | Codex start, standard-screen attach, and return | **Not run.** Blocked on the App Server protocol |
| V5 | Returning while waiting for approval or input | **Not run.** Needs V3 and V4 |
| V6 | Concurrent clients | **Not run.** Needs V4 |
| V7 | Service lifetime | **Not run.** Would require stopping real sessions |
| V8 | External sessions and past history | **Done.** Claude's `--all` covers completed background sessions; Codex's past conversations come from the rollout reader |
| V9 | Terminal restoration | **Not run.** Needs an interactive terminal |

### V1: the Claude listing

`claude agents --json` returns an array. Fields observed on 2.1.270:

| Field | Meaning |
| --- | --- |
| `id` | Short handle, 8 hex characters. What `attach`, `logs`, `stop`, and `rm` take |
| `sessionId` | Conversation UUID. agnt's `native_session_id` |
| `cwd` | Working directory |
| `kind` | `"background"` observed |
| `startedAt` | Epoch milliseconds |
| `name` | Title |
| `state` | `"working"`, `"blocked"`, `"done"` observed |
| `status` | `"busy"`, `"idle"`. Not always present |
| `pid` | Not always present, including on some live sessions |

State mapping, and why:

| Observed | RuntimeState | Reason |
| --- | --- | --- |
| `state: "working"` | Running | |
| `state: "blocked"` | WaitingInput | The JSON does not separate an approval request from a question. Both sort and group together, so the list is unaffected |
| `state: "done"`, listed without `--all` | Idle | The turn finished and the execution is still listed as active. Never WorkflowState's Done |
| `state: "done"`, only under `--all` | Stopped | Claude calls these completed |
| anything else | Unknown | |

`pid` is not a liveness signal: a session with `state: "blocked"` and no `pid` was listed as active. Membership of the default listing is the signal, so discovery calls the listing twice and takes the difference.

### V8: the Codex history reader

Codex has no scriptable listing, so discovery reads `~/.codex/sessions/<year>/<month>/<day>/rollout-*.jsonl`. Only the first line of each file is read; it is a `session_meta` record carrying `session_id`, `id`, `cwd`, `timestamp`, `originator`, and `source`.

This is the version-specific read-only history reader that design.md §2 permits, and it is declared here as that section requires. Consequences:

- A file proves a conversation existed, never that it is running. Every discovered Codex session is **Unknown**.
- `source: {"subagent": …}` rollouts belong to another session's turn and are skipped. 3 of 49 files on the test machine.
- Several rollout files share one thread; they collapse to the newest timestamp.
- Titles come from `~/.codex/session_index.jsonl` (`thread_name`), where later lines win.

Codex session IDs are UUIDv7, so conversations started close together **share 8-character prefixes**. Two such collisions existed on the test machine, and `agnt attach 019c3d7f` correctly refuses as ambiguous instead of picking one.

### Preview is unsupported on both providers

`claude logs <id>` prints the session's recent terminal *screen*, not its conversation: about 100 KB of escape sequences, box drawing, and spinner output. Stripping the sequences makes it readable, but the tail of a screen is the status bar, so the excerpt reads as UI chrome rather than as what the session is doing. Reporting that as a working preview would be an unsupported operation dressed up as a success, so `preview` stays unsupported with that reason. A real preview needs a transcript source.

The sequence stripping is kept regardless: design.md §9 requires that text from a provider can never act as terminal control.

## Current support, by operation

| Operation | Claude | Codex |
| --- | --- | --- |
| discover | supported | supported, from the history reader |
| observe | supported | unsupported: needs the App Server |
| preview | unsupported: `logs` is a screen dump | unsupported: only `session_meta` is read |
| start | supported, needs an initial prompt. **V2 not run** | unsupported: no background start |
| attach | supported. **V3 round trip not run** | unsupported: needs the App Server |
| resume | supported | supported |
| stop | supported. **V7 not run** | unsupported: needs the App Server |

## Mapping to the open questions

| ID | What to decide | Covering checks | Status |
| --- | --- | --- | --- |
| Q1 | Non-interrupting detach from the Codex standard CLI, and request delivery | V4, V5, V6 | Open. Needs the App Server protocol |
| Q2 | How strictly keeping the official CLI screen is required | V3, V4 | Open for Codex. Claude documents attach and detach |
| Q3 | Whether continuation after agnt or Ghostty exits is guaranteed | V7 | Open |
| Q4 | How past ordinary Claude sessions are enumerated | V8 | **Answered:** `claude agents --json --all` |
| Q5 | Whether hiding from the list is enough for "delete" | acceptance | Open |
| Q6 | The first OS, the minimum CLI versions, and library versions | environment table | Answered for the test machine |

## Work still to do

1. Run the interactive checks (V3, V5, V9) in a terminal: attach a running Claude session from the list, come back, and confirm raw mode, the cursor, size, and paste all behave.
2. Run V2 and V7 deliberately, on a session created for the purpose, and record what `claude --bg` prints and what `claude stop` confirms.
3. For Codex, decide between the App Server protocol and the alternatives in ADR-0001 §D4 before implementing attach, observe, or stop. Adopting tmux is not treated as already decided.
4. Update ADR-0001 to match, and review the conditions for moving it to Accepted (§6).
