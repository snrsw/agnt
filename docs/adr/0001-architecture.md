# ADR-0001: agnt's basic structure and the division of responsibility for running sessions

- Date: 2026-09-13
- Status: **Proposed**
- Subject: a TUI that manages local Claude Code and Codex sessions
- Related document: [design.md](../design.md)

## 1. What this record is

The tool's name is settled: **agnt**. This ADR records the architecture proposed on the basis of the conversation so far. Rust, JSON persistence, and a preference for the official interfaces are the proposed choices, but that does not mean the user has approved the whole design. The runtime in particular is settled only after testing on a real machine.

| Item | Current standing |
| --- | --- |
| Name: agnt | Settled |
| A small TUI used from Ghostty | User requirement |
| Managing Claude Code and Codex together | User requirement |
| Rust, no SQLite in the first version | Proposed on the basis of the conversation |
| Work keeps running after returning to the list | Design goal |
| tmux is not a required dependency | The option to test first |
| Attaching to and leaving the official CLI screen | Awaiting testing, mainly on the Codex side |
| Work keeps running after the managing TUI or Ghostty exits | Undecided whether the first version guarantees this |

## 2. Background and decision criteria

The goal is to handle Claude Code and Codex sessions from a single list, with an experience close to Claude Code's agent view. The needed features are: seeing what a session is about, sorting by state, showing PRs and issues, deleting, and starting in an arbitrary working directory.

The decision criteria, in order:

1. Listing and switching are fast.
2. Settings, dependencies, and the things to operate stay few.
3. Execution state is never asserted wrongly.
4. The existing CLIs' authentication, permission settings, and working environment are reused.
5. The impact of each tool's specification changes stays local.

A browser UI, automatic orchestration between agents, a custom conversation engine, and automatic worktree creation are outside the first version's purpose.

## 3. The proposed structure

### D1. Ship as a single Rust executable

The proposal is clap for CLI parsing, Ratatui and crossterm for the list, Tokio for async I/O, and serde / serde_json for data conversion. Specific library versions are chosen and pinned when implementation starts.

Rust is chosen for single-executable distribution and because providers, states, and operations are easy to express in its type system. No assumption is made that Rust is necessarily faster than Go. Perceived performance depends on how often external CLIs are started, how much history is read, and network waits.

Ratatui draws and crossterm controls the terminal. Those two alone do not hold or restore a child CLI's screen. [Ratatui](https://ratatui.rs/), [crossterm](https://docs.rs/crossterm/latest/crossterm/), [Tokio](https://docs.rs/tokio/latest/tokio/)

### D2. Do not make SQLite an initial dependency

Manual titles, work classification, PR / issue associations, and hidden markers go into a small JSON file. Execution state stays in memory, and a regenerable list cache goes in a separate file if needed.

Conversation content is never copied into agnt's persistent data. Each tool holds the authoritative copy of its conversations.

A JSON update takes the dedicated lock file, reloads after acquiring the lock, writes to a temporary file in the same directory, then renames. Rename alone does not prevent a lost update under concurrent writes, so the exclusion is not skipped.

Moving to SQLite is reconsidered when one of the following appears as a measurement or a real requirement:

- Full-text search over history, or many indexes, is needed.
- Frequent updates from several processes are needed.
- Reading and writing the whole JSON file does not meet the performance targets.
- Metadata relationships or transactions become complex.

### D3. Absorb the differences between tools in adapters

The core and the TUI work with one shared Session model. An adapter handles discovery, preview retrieval, launching, attaching, stopping, and feature detection. Support for each feature is stated explicitly, and an unsupported operation is never reported as success.

For Claude Code's execution state, the official JSON list takes priority. That list is not treated as covering every past ordinary session, though. Background job IDs and conversation UUIDs are also kept distinct. [Claude agent view](https://code.claude.com/docs/en/agent-view)

For Codex, the App Server's history retrieval and state notifications take priority. The state one server returns is not used to assert whether a session running in another process is alive. The API's coverage is recorded in the Session's observation data.

The first version builds both adapters into the same binary rather than creating a dynamic plugin mechanism. [Codex App Server](https://learn.chatgpt.com/docs/app-server)

### D4. Prefer official integration without tmux; decide the runtime by testing

The preferred option leaves execution to Claude's background service and a local Codex App Server, and makes agnt a client that operates and observes. No custom daemon or PTY implementation is introduced at this stage.

Three things are treated as separate properties, however:

1. Work on the server continues when the custom TUI's display switches away.
2. After attaching to the official CLI screen, that screen can be left to return to the custom list.
3. Work continues after agnt or Ghostty exits.

Claude's background sessions have attach and detach. What happens to a running turn or an unanswered approval request when a standard CLI attached to the Codex App Server exits is untested as of this ADR. Resuming a conversation's history does not mean a running process was preserved.

The first technical test runs "start new → long-running work → attach to the official screen → return to the list → reattach" on both tools. For Codex it keeps agnt's observation connection alive and checks the standard CLI's disconnect, the pending requests, and the server's lifetime separately.

If official integration does not meet the requirements, the alternatives below are compared. None is adopted implicitly; this ADR is updated instead.

| Alternative | Benefit | Cost and constraints |
| --- | --- | --- |
| agnt owns a PTY | Keeps the official CLI screen and switches with a shared key | Needs screen retention, continuous output reading, resize, and input handling |
| Build a conversation UI for the Codex App Server | Separates execution from display and handles state directly | Widens the scope to conversation display, approvals, and answering questions |
| Use tmux as the runtime | Leaves PTYs and reattachment to an existing implementation | Adds an external dependency. Execution state still has to come from elsewhere |

To keep work running after the managing TUI exits under the PTY approach, the PTY's owner has to be a separate process. "Single binary" and "no resident process" are not the same thing.

**Amended: the prompt's shell does run on a pty.** What is deferred above is agnt owning the pty a *provider's CLI* runs on, and the costs listed are the costs of that: retaining a screen the user cannot see, reading continuously so a full buffer cannot stall the child, and keeping work alive past agnt's own exit. The shell behind Shift+Tab has none of those properties — it is visible whenever it exists, it is the user's own shell, and it is meant to die with agnt. Reimplementing an interactive shell's line editor on top of `$SHELL -c` was tried first and was the worse trade. Attaching to a session still hands the whole terminal over rather than running the CLI in a pane, so this decision stands for everything it was written about.

### D5. Separate execution state from work classification

Execution state is observed from the tool; work classification is set by the user. The end of a response is not converted into the completion of work. A state that cannot be observed is Unknown, shown with the time it was last observed.

A merged PR or a normally exited process never changes the user's work classification on its own.

### D6. Make removal from the list, stopping, and history deletion separate operations

In the first version, "delete" means hiding from agnt's list. A tombstone is stored so the row stays hidden when data is fetched again. Restore is provided.

For a running session, the choice between stopping before deleting and continuing to run while hidden is shown and left to the user. Hiding alone never stops a process.

Full history deletion is not offered in the first version. Whether this satisfies the user requirement to "delete" is an acceptance question. If history deletion is needed, a separate ADR covers each provider's deletion scope.

Claude's `rm` affects worktrees, and Codex's `delete` deletes conversation history. A single shared delete key does not call both uniformly. [Claude's management operations](https://code.claude.com/docs/en/agent-view), [Codex CLI](https://learn.chatgpt.com/docs/developer-commands?surface=cli)

### D7. Separate external I/O from drawing the list

The list draws from memory or the cache first, then updates provider information and GitHub information asynchronously. A GitHub outage or a slow CLI never blocks a keystroke.

No custom LLM summarization is introduced. Titles and short existing previews are used, and reading a whole conversation is limited to the detail view.

## 4. Other basic structures considered

| Option | Why it is not adopted here |
| --- | --- |
| Go + Bubble Tea | A strong option, but Rust is the proposal this time. Not for performance reasons |
| SQLite from the start | Not required at this point for persisting a small amount of metadata |
| Scan every history file each time | Startup time would depend on history volume, and it is fragile against internal format changes |
| Custom daemon + custom RPC + PTY from the start | It does solve keeping connections alive, but the initial maintenance scope is too wide |
| Custom conversation UIs for both tools | Far beyond the scope of a listing and switching tool |
| Depend directly on Ghostty's tab operations | Would need terminal-specific integration. Ordinary terminal I/O comes first |

## 5. Consequences

### What this gains

- A small distributable and little persistent data to start with.
- List rendering that does not wait on the network or on reading full history.
- Reduced dependence on each tool's internal file formats.
- Clear ownership between the user's classifications and links and the observed data.

### What this accepts

- Attach operations and observable scope differ between Claude and Codex.
- Sessions started outside agnt may have limits on state and attachment.
- JSON also needs exclusion, corruption handling, and schema management.
- The final runtime cannot be decided until the round trip to the official CLI screen works.

## 6. Conditions for moving to Accepted

1. Run the design doc's P0 validation and record CLI versions, OS, and results.
2. Settle the runtime and the behaviour when the managing TUI exits.
3. Settle the discovery scope, including past ordinary sessions.
4. Confirm the initial specification that "delete means hide from the list".
5. Update this ADR to match the implementation that was adopted.

This document is a design record. It does not indicate that agnt has been implemented or validated on a real machine.
