# agnt — design doc

- Date: 2026-09-13
- Status: **Draft / before implementation**
- Corresponding ADR: [adr/0001-architecture.md](adr/0001-architecture.md)
- Target environment: local macOS / Linux with Ghostty. The first OS to validate on is chosen when implementation starts

## 1. Purpose and design premises

**agnt is a TUI for finding Claude Code and Codex sessions in one list and getting back to the work you need quickly.**

One screen shows a session's short content, execution state, work classification, working directory, and PR / issue. It can start from any directory, and it aims to keep running work alive after returning to the list.

The name is settled. Rust, JSON, and a preference for official integration are this document's proposals. Background execution and attaching to or leaving the standard CLI screen are separate things to validate; a setup without tmux is not treated as confirmed on a real machine.

## 2. Requirements and scope

| ID | Requirement | First-version design | Acceptance criterion |
| --- | --- | --- | --- |
| R01 | Starts from Ghostty | Run agnt as an ordinary CLI | The list opens with no terminal-specific settings |
| R02 | Lists both tools' sessions | Merge discovery results in adapters | The scope and any fetch failures are visible, and one conversation is not listed twice |
| R03 | Shows what a session is about | Title plus a short preview | Work is identifiable without opening the full conversation |
| R04 | Sorts by status | Toggle between execution state and manual work classification | Things like waiting-for-approval can be shown first |
| R05 | Shows PRs and issues | Explicit URL registration plus GitHub enrichment | The related number and kind appear in the list |
| R06 | Deletes a session | Hide from the list, restorable | It does not come back on refetch, and it is clear that history remains |
| R07 | Starts in any directory | The embedded shell, or `agnt new --cwd` | Paths outside git, with spaces, or with non-ASCII characters all work |
| R08 | Fast | Separate drawing from external I/O | The provisional performance targets in section 11 are measured |
| R09 | Work continues after returning to the list | Prefer the official runtime | A long-running task is not interrupted, and the list can reattach to it |

R06's "delete" differs from full history deletion, so it needs confirmation as an initial specification.

### Scope of session discovery

| Category | Display | Guarantee on state and operations |
| --- | --- | --- |
| Sessions started by agnt | Required | Start, attach, and stop within the chosen runtime's scope |
| Live sessions started elsewhere | Whatever the official discovery returns | If the execution location cannot be determined, state is Unknown and attach is marked unsupported |
| Saved past sessions | Recently touched ones, within a stated window | Resumable does not mean the current execution state is known; that is checked separately |
| Cloud, other machines, deleted history | Outside the first version's scope | Distinguished as outside the scope of local management |

**Saved history is bounded, and the bound is stated.** "Past sessions" is not "every conversation ever had": Codex keeps rollout files indefinitely, and on the machine this was built against 41 of 46 were more than four months old. The reader stops at 30 days and its scope note says how many it passed over, because a reader that silently drops rows cannot be told from one that failed. Anything older is reached with the provider's own tools — `codex resume` has a picker.

Claude's agent-view JSON list alone does not cover every past ordinary session. P0 investigates the official means of enumerating history and its dependencies. If a version-specific read-only history reader is needed, that decision and the target formats are stated explicitly. "Full history support" is never displayed while it is unimplemented.

### Features not added in the first version

- Automatic instruction and coordination between agents, and task decomposition.
- Custom model connections, custom authentication management, and LLM-generated summaries.
- Automatic worktree creation and deletion, and creating or updating branches, PRs, or issues.
- Web UI, cloud sync, and notification services.
- Dynamic adapter plugins and full-text history search.
- Windows support, and any guarantee that work continues after Ghostty or the managing TUI exits. The need for these is confirmed separately.

## 3. Operations and screens

The following is agnt's proposed interface, not an existing command.

```sh
agnt                                       # the whole list
agnt list --json                           # list for scripts
agnt new claude --cwd ~/src/api            # start with an initial prompt and so on
agnt new codex --cwd ~/src/api
agnt attach <id>                           # attach to a running conversation, if possible
agnt resume <id>                           # reopen a saved conversation
agnt hide <id>                             # remove from the list
agnt restore <id>                          # undo hiding
agnt stop <id>                             # stop execution
agnt link <id> <url>                       # associate a PR or issue
agnt doctor                                # supported features, versions, connection state
```

IDs may be given in an abbreviated display form, but an ambiguous ID is never resolved by first-prefix match. Internally a session is identified by provider, storage scope, and native conversation ID.

### The two views

The list has two shapes, because it answers two different questions, and Ctrl+T moves between them.

**Status** — cards grouped by execution state, for "what needs me now". The task takes its own line; everything that qualifies it sits underneath in dim text.

```
── Needs you (1)
> ● claude-code-action auth subscription                        issue #880
    Claude · Needs input · reed · 34d
── Working (1)
  * rust project scaffolding                                    PR #1
    Claude · Working · agnt / scaffold · 4d
```

**Repository** — one line per session, grouped by repository, for "what was I doing in reed". The repository is on the heading, so the branch or worktree is what tells two rows apart, and each heading tallies what is inside it.

```
── reed (15)                                        ● 1  * 0  ○ 3  · 11
> ● Claude claude-code-action auth subscription                      34d
  ○ Claude reed pr 971 review comments    dr5-dedup                  23h
── agnt (1)                                         ● 0  * 1  ○ 0  · 0
  * Claude rust project scaffolding       scaffold       PR #1        4d
```

A third turn of Ctrl+T gives the same cards grouped by the classification the user set, which is how R04's manual axis is reached.

Stopped sessions and saved history are folded away by default and brought back with `Ctrl+O`. This is a scale decision rather than a taste one: on the machine this was built against, 50 of 56 rows were saved Codex conversations, some of them 200 days old, and a two-line card each buried the six live sessions under a hundred lines. A folded group keeps its heading, marked `▸`, and its heading still tallies what is inside it, so nothing is silently missing.

What counts as history is decided by state **and** origin: a session that has stopped, or one that exists only as a saved file. Unknown alone will not do, because every row loaded from the cache is Unknown until the first refresh answers — folding on the state alone empties the list on a cold start.

Repositories are ordered by what is in them — the one holding something that needs you first, then by most recent activity — because alphabetical order buries the repositories being worked in. Within a group the order is the state order above, so saved history sinks to the bottom.

These are fictional examples. State is never conveyed by colour alone: every state carries a glyph and, in the card view, its name in words.

**Colour is drawn from the terminal's own palette, and is always additive.** The list uses named ANSI colours only — amber for a session that needs you, green for one that is working, magenta for PR and issue references, blue for the brand mark, the cursor bar and the caret — so it wears whatever the terminal is themed with instead of fighting it. Every state still carries its glyph and its word, so a monochrome terminal loses only the emphasis. The cursor is a bar in the left margin rather than a reversed row: it marks every line of the selected card, and leaves the card's own colours readable.

**Spacing.** Every heading but the first has a blank line above it, and every heading with rows under it has one below, so a heading reads as a heading rather than as the first row of its group. The header, the prompt and the footer share a two-cell left margin — the width of the list's cursor gutter — so their text lines up with the rows' glyphs and nothing sits flush against the terminal's edge.

**Rows never change height. Only the cursor moves.** Every card is two lines and every compact row is one, whichever is selected. The selected session's remaining details — its shortened path, its classification when one is set, its short id, and a preview once a provider can supply one — sit on a single footer line that is always present, blank when nothing is selected:

```
> ● claude-code-action auth subscription                      issue #880
    Claude · Needs input · reed · 34d
  ○ review pr #985
    Claude · Idle · reed · 1h
──────────────────────────────────────────────────────────────────────────
> Describe a task for a new session
  Claude · ~/src/reed
──────────────────────────────────────────────────────────────────────────
…/GrassAI-Inc/reed · 5175105c
type to start a session · enter to attach · ? for shortcuts
```

Two earlier shapes were built and dropped. A separate four-line pane repeated the state, repository and links already on the card, four rows from the cursor. Expanding the selected row in place was tried next; it kept the list's total height, but every row below the cursor still moved by a line each time the cursor did, and on a screen recording the list looked as though it was shuffling. A fixed footer line costs one row and moves nothing.

### Keys

| Key in the list | Operation |
| --- | --- |
| type | Write a task for a new session. The prompt is always live |
| Enter | Start it; on an empty prompt, attach or resume the selected row, asking when the state does not decide |
| Up / Down | Move between rows, wrapping at both ends |
| Tab | The new session's agent |
| Shift+Tab | Put `$SHELL` in the text area; where it ends up is where the next session starts |
| Ctrl+T | Cycle the view: status cards, repository rows, workflow cards |
| Ctrl+F | Local search over title, path, and related number |
| Ctrl+O | Show stopped sessions and saved history, folded away by default |
| Ctrl+Y | Show rows hidden from the list |
| Ctrl+R | Request an asynchronous refresh |
| Ctrl+X | Hide the selected row, or restore a hidden one. When running, choose between continuing and hiding, or stopping then hiding |
| Ctrl+G | Change work classification |
| Ctrl+L | Register or edit a PR / issue URL |
| Ctrl+U / Ctrl+W | Clear the prompt / delete a word |
| ? | The full key list, on an empty prompt |
| Ctrl+C | Leave the list. The effect on sessions follows the chosen runtime's behaviour |

**Typing goes to the prompt, so every action is a chord.** Starting work is the most frequent thing the list is for, and making it cost a key to open a field made it the one action with a toll. The cost is that no plain letter can be an action: they are all text. Ctrl+U and Ctrl+W stay with the text, since a field holding prose owes its user those. `Enter` is deliberately overloaded on whether the prompt holds anything — start the task, or open the row — one key doing the obvious thing in both states rather than two keys to remember.

**Where a session starts is chosen with the shell — the real one.** Shift+Tab puts `$SHELL` in the text area. The same box in the same place, a few rows tall; what changes is what is in it. It runs on a pseudo-terminal as a login shell, so tab completion, abbreviations, aliases, autosuggestions, colours, key bindings, the prompt and the history are all the shell's own. Every key in that mode goes to the shell; the only one agnt keeps is Shift+Tab, which comes back to the task, still holding its half-typed text.

**The box is as tall as the shell is using.** An idle prompt is one or two rows; running a command grows it to fit the output, the same way the task prompt grows with the text typed into it. It stops at a cap of four to twelve rows by screen height, after which the shell scrolls and the tail is what stays — which is the part a prompt is usually consulted for. The list above it is what the screen is for; anything deserving a full screen deserves a session, which is what Enter on the task prompt is.

A fixed-height box was tried first and was wrong on a tall window: at twelve rows it was mostly blank, and pinning the prompt to the bottom of that blank space made it worse rather than better. Growing from the top needs no pinning — the prompt is simply the last row with anything on it.

**Which shell is `$SHELL`'s job only as a last resort.** `AGNT_SHELL` wins if it is set; otherwise the shell comes from the operating system's record for the user — what `chsh` wrote, read with `getpwuid`, which on macOS is the directory service rather than `/etc/passwd`. `$SHELL` is the conventional answer and the wrong one here: a development shell rewrites it, and inside `nix develop` it is the bash that nix supplies, so agnt run that way would hand the user bash however their own shell is set. `agnt doctor` prints the shell it will open and says when it is ignoring a `$SHELL` that disagrees, because otherwise the difference is invisible until the wrong prompt appears.

**Which directory the shell is in is read from the operating system, not asked of the shell.** Asking means writing a line of the shell's own syntax into the user's prompt, where they would see it, and spelling it differently for every shell — `$PWD` against nushell against PowerShell. The kernel already knows: `/proc/<pid>/cwd` on Linux, `lsof -a -d cwd -p <pid>` on macOS. That works for any shell and costs nothing at the prompt.

It has to be polled rather than read once, because a shell that exits takes its directory with it: there is no process left to ask, so the last directory must already have been observed. It is read at most every 150 ms while the shell is alive — reading it costs a process on macOS, so once per keystroke would be felt. The consequence is that it is approximate at the edges: `cd elsewhere && exit` on one line never exists long enough to be seen, and the next session starts where the shell was before it.

This is the third design for this, and the first two were reimplementations of a terminal. A `cd`-only picker could not reach a directory with no sessions in it. Handing whole lines to `$SHELL -c` ran commands, but everything an interactive shell gives you had to be rebuilt on top: abbreviation expansion (they are a line-editor feature, so `-c` never expands them, and a generated config usually defines them only behind an `is-interactive` guard), a `$PWD` trailer to read the directory back, an exit-status save so `cd nope` did not report the trailer's success, a probe to find out whether the shell understood any of it, suppressed history, and path completion. Each piece worked; together they were a worse terminal than the one already installed. A terminal is a thing you can simply have.

**So ADR-0001's deferral of the PTY is deliberately overridden, for the prompt only.** What that ADR rules out is agnt owning the PTY that a *provider's CLI* runs on — that needs screen retention while hidden, continuous reading so the buffer cannot stall the child, resize, and work that outlives agnt. None of it applies here: this shell is visible whenever it exists, it is the user's own process, and it dies with agnt because nothing is meant to survive it. Attaching to a session still hands over the whole terminal (`terminal::handoff`) rather than running inside a pane.

`portable-pty` opens the pty, `vt100` turns the bytes into a screen, and `tui-term` draws that screen as a ratatui widget. A reader thread does the blocking read and signals the event loop to redraw; the send is non-blocking, because a full channel must never stall the thread draining the pty. The shell is told the box's size before its first prompt, so it never has to redraw, and told again whenever the window changes. `exit` gives the box back to the task prompt. `AGNT_SHELL` overrides `$SHELL`.

Two things follow from the shell being real that were not true before. Anything that wants a terminal — an editor, a pager, `git commit` — now works. And the user's history is the user's again: agnt used to suppress it, which was the polite choice for a prompt they could not scroll; a shell they can scroll should keep what they typed.

The footer carries only what is useful in the current state — a few keys when the prompt is empty, the start keys when it is not, and in shell mode only the key that leaves, since the rest are the shell's — and the full list is behind `?`. These keys apply only while the list owns the terminal. While an official CLI screen is open, that CLI's keys apply.

## 4. Architecture

```mermaid
flowchart TD
    UI["CLI / Ratatui"] --> Core["Session model / actions"]
    Core --> Store["JSON metadata / cache"]
    Core --> Claude["Claude adapter"]
    Core --> Codex["Codex adapter"]
    Core --> Links["Git / GitHub enrichment"]
    Claude --> CC["Claude CLI / background service"]
    Codex --> CS["Local Codex App Server"]
    Links --> GH["git / gh"]
```

The diagram shows the division of responsibility in the official-integration option. Handing over the terminal when attaching to a CLI screen is covered in section 7.

### Proposed Rust structure

Start from a single Cargo package, and do not split into several crates until there is a reason to.

| Module | Responsibility |
| --- | --- |
| cli | clap argument parsing and non-interactive output |
| app | Event loop, running operations, updating in-memory state |
| model | Session, RuntimeState, WorkflowState, Link, Capability |
| ui | List, input, detail, confirmation screens |
| providers/claude | Claude discovery, launching, attaching, observation |
| providers/codex | App Server connection, history, notifications, feature detection |
| terminal | Terminal ownership handover, restoring raw mode and the like |
| storage | Metadata, cache, exclusion, schema migration |
| git / github | Git information from a cwd, and link enrichment |

UI state updates are consolidated into a single event loop. CLI processes, App Server notifications, and GitHub queries send events from background tasks. Slow work is never called from a drawing function. Updates to visible rows are batched, but approval requests and errors are never dropped silently.

## 5. The session and state model

### Session identity

A session's permanent key is `provider + scope + native_session_id`. The scope is a stable identifier for a local profile or storage area, not a PID or a temporary socket name. The standard profile is `default`.

Claude's background job ID and execution PID, and Codex's connection target and load state, are separated into a RuntimeHandle. A client attached to the same conversation is never listed as a separate conversation. When several executions turn out to share one conversation, the execution unit is shown in the detail pane.

Right after launching, a row is created under an agnt-side temporary operation_id, then rebound to the permanent key once the native ID is known. When a fork changes the ID, the result is registered as a new conversation that keeps its relationship to the parent.

### Main fields

| Field | Meaning and authoritative source |
| --- | --- |
| key | Provider, scope, native conversation ID |
| title / preview | From the provider. A manual title wins at display time |
| cwd | The session's working directory. Distinct from where agnt was started |
| launch_git / observed_git | Git information at launch, and the currently observed information |
| runtime | RuntimeHandle, execution state, the evidence for it, and the observation time |
| workflow | The classification the user set |
| links | Kind, URL, repo, number, source, and the evidence for the association |
| capabilities | Whether attach, resume, stop, history retrieval and so on are supported |
| origin | managed / discovered / history |
| hidden_at | When the session was hidden from agnt's list |

### Execution state

| State | Display | Basis |
| --- | --- | --- |
| Starting | Starting | A start request was accepted and the result is pending |
| Running | Working | Trustworthy provider information says it is running |
| WaitingApproval | Needs approval | An explicit approval request |
| WaitingInput | Needs input | An explicit question or input request |
| Idle | Idle | The execution environment is alive and no turn is currently running |
| Stopped | Stopped | A managed execution's stop or exit was confirmed |
| Error | Error | The tool reported an execution failure |
| Unknown | Unknown | Not observed, an old observation, an unknown location, or undecidable by specification |

The ordinary end of a response is a candidate for moving to Idle, not for WorkflowState's Done. A provider's "done" is interpreted together with information about whether the execution environment is alive. Codex's `notLoaded` alone is not taken to mean the session is stopped in other execution environments too.

A failed fetch is not turned into the session's own Error. An old observation is kept as last_known_state, and the list shows Unknown or "not confirmed". After reconnecting, state is not overwritten with old events until a fresh snapshot is taken.

Work classification is `Unclassified / InProgress / Review / Done`. The user changes it; rerunning or merging a PR never changes it automatically.

The default display order is waiting-for-approval and waiting-for-input, then error, running, starting, idle, stopped, unknown, with the most recent activity first inside a group. The selected row is tracked by ID.

## 6. Data retrieval and adapters

Conceptually, an adapter provides the following operations. The exact Rust trait is settled at implementation time.

| Operation | Contract |
| --- | --- |
| capabilities | Returns the features supported by the running version |
| discover | Returns a page of sessions and the scope that was searched |
| observe | Returns update events, or state differences by polling |
| preview | Returns short content with a size limit |
| start | Takes a cwd and input, returns a native ID and a RuntimeHandle |
| attach | Returns a plan for attaching to a running conversation |
| resume | Reopens a saved conversation. Not to be confused with attach |
| stop | Requests that the execution stop; confirmation of termination is returned separately |

### Claude

Live and background sessions use the official JSON list. The short job ID is kept for operations and the conversation UUID for identity. The JSON list is not assumed to carry title, summary, and PR in full; missing items are filled in from explicit metadata or another read path. [Claude agent view](https://code.claude.com/docs/en/agent-view)

Background launching with an initial prompt takes priority. Starting an empty session, the behaviour of cwd and worktrees, and inheritance of existing settings are checked in P0. If launching causes worktree creation that agnt did not ask for, whether that default can be controlled is checked; if it cannot, the behaviour is stated in the UI.

### Codex

Saved conversations and execution notifications come through the App Server. The proposal is to consolidate conversations started from agnt into the chosen local server. Protocol initialization and supported features are checked, and the connection is held independently of drawing. [Codex App Server](https://learn.chatgpt.com/docs/app-server)

A Codex session started elsewhere is Unknown if its execution location is not known, even when it is found in the same history area. A conversation is never resumed into a double execution while its liveness is unknown. When the user resumes it, this uncertainty is shown in the operation screen.

### Shared constraints

- If one provider is not installed, the other provider and agnt's metadata still work.
- Unknown JSON fields are tolerated; missing required fields and unknown states are diagnosable.
- CLI calls use an argument array and a cwd. User input is never assembled into a shell string.
- When a version difference means something is unsupported, `doctor` shows the reason, and no other operation is silently substituted.
- A failed read never deletes existing rows or metadata.

## 7. Execution, attachment, and returning to the list

### The official-integration option

agnt handles the list and observation, and the official service holds the execution. Before a new launch, the operation is recorded uniquely, so a timeout can be reconciled as an unknown result. Automatic retries of a launch never create duplicate sessions.

When attaching, the list's input and drawing stop, raw mode and the alternate screen are properly undone, and the terminal is handed to the official CLI. agnt itself and its background observation tasks stay alive. When the child CLI returns, the terminal state is restored and the list is redrawn. A restore path exists so the terminal stays usable even after a panic or an abnormal child exit.

Claude's background sessions have an official detach operation. The behaviour where Ctrl+Z from a shell attach returns to the caller is also validated with a custom TUI as the parent. [Claude attach and detach](https://code.claude.com/docs/en/agent-view)

For Codex, attaching to the standard screen and exiting it are validated in P0. It is not assumed that keeping agnt's observation connection alive is enough to handle every request. Where approval and input requests are delivered, what happens when a connection is closed, and whether reconnecting restores pending requests are all checked. Since the official protocol treats approval as a request to the client, a live server alone does not guarantee the requirement is met. [Codex approval protocol](https://learn.chatgpt.com/docs/app-server)

When the managing side and the standard CLI can both receive a request, which one is responsible for answering is decided. A design in which an unanswered request is lost, denied without asking, or answered twice is not adopted. Approvals and questions that need an answer are handled by opening the session, answering, and then returning to the list.

### Service lifetime

If an independent local service is used, it is not assumed to outlive a child stdio process of the TUI. The launch method, the connection target, the owner, and reconnection are all stated. When agnt attaches to an existing user service, it does not stop that service when it exits.

The first version's guarantee target is: "while agnt's list is open, work in another session keeps running." Continuation after agnt exits, after Ghostty exits, or across a machine restart is a separate requirement. The actual results for a normal exit, SIGHUP, and an abnormal exit are recorded in P0, and the guaranteed scope is finalized then.

### If the design changes to PTY

This is about a provider's CLI, not the shell in the prompt, which already runs on a pty (section 3). The execution part of an adapter is replaced by a PTY owner. Output keeps being read even while hidden, and a bounded scrollback and the terminal screen state are held. Leaving output unread can fill the PTY buffer and stall the child process. A raw output log alone does not guarantee that the screen can be reconstructed.

What becomes necessary: terminal emulation, resizing, CJK and combining characters, input, mouse and paste handling, and responses to queries from the terminal. Ratatui does not take this role. Returning to the list with a plain SIGSTOP or shell job control does not satisfy the requirement that work continues.

## 8. Persistence

Paths come from a resolver that follows OS conventions such as XDG. The following is a logical layout; no absolute path for a specific OS is fixed.

| File | Contents | Authoritative |
| --- | --- | --- |
| config.toml | Optional settings, CLI paths, connection targets | Yes, for settings |
| metadata.json | Manual titles, classification, links, hidden markers | Yes, for agnt metadata |
| metadata.lock | Exclusion during an update | Not data |
| sessions-cache.json | A bounded snapshot of the list | Regenerable. Can be discarded when broken |

agnt starts without a configuration file. The cache's initial proposed limit is about 1,000 rows, and it never contains conversation content. At startup the cache's states are shown as "the previous observation", never as a confirmed running state.

An example `metadata.json`:

```json
{
  "schema_version": 1,
  "revision": 1,
  "sessions": [
    {
      "key": {
        "provider": "codex",
        "scope": "default",
        "native_session_id": "example-session-id"
      },
      "title_override": "Fix the auth handler",
      "workflow": "review",
      "hidden_at": null,
      "links": [
        {
          "kind": "pull_request",
          "url": "https://github.com/example/api/pull/123",
          "source": "manual"
        }
      ]
    }
  ]
}
```

The IDs and URLs are examples.

An update takes the lock, reloads the current JSON, applies the change, writes to a temporary file with a flush and any needed sync, renames, and releases the lock. The lock is taken on a dedicated file that rename does not replace, not on the data file itself. The lock is not held during network queries.

When the schema is unknown or the JSON is corrupt, the original is not overwritten. Metadata editing stops, a diagnostic is shown, and listing from providers continues as far as it can. A corrupt cache is discarded and refetched. Frequently updated observation data is kept separate from manual metadata to avoid unnecessary whole-file writes.

## 9. Showing content, PRs, and issues

A title is chosen in this order: manual title, the provider's name, an excerpt of the first retrievable request text, then the short ID. A preview prefers existing short content; when none can be retrieved, it shows "no content". No custom model calls are made.

Link priority is: manual registration, an association the provider states explicitly, then candidates derived from repository and branch. A candidate never overwrites manual information, and is marked as an inference. Registering several PRs or issues is allowed.

GitHub integration uses an authenticated `gh`. Registering and displaying URLs still works without `gh`. GitHub access is read-only, and opening a URL is limited to an explicit user action.

Matching from a branch collects candidates on the basis of the repository, the head-side repository, and the branch. When there is a same-named branch, a fork, or several PRs, one is not picked arbitrarily. A PR's closingIssuesReferences can be used as part of the issue candidates, but it is not a list of every related issue. [GitHub CLI](https://cli.github.com/manual/gh_pr_view)

The cwd and git information at launch, and the evidence for each link, are stored. When a working directory's HEAD changes later, past associations are not reassigned automatically. When several sessions share one cwd, the current HEAD alone does not determine a specific conversation's branch.

External text shown in the session list has control characters removed and is truncated to the display width. A stored preview must never be executed as terminal control sequences.

## 10. Stopping, deleting, and restoring

`hide` records `hidden_at` in `metadata.json`. The hidden filter is applied after discovery, so a background refresh never brings a row back. `restore` clears that record. Hidden rows can be inspected through a dedicated filter.

The delete dialog for a running session explains that history remains, and offers the choice between continuing while hidden and stopping before hiding. When "stop first" is chosen, the confirmation of the stop is awaited, and a failure or timeout never hides the row silently.

`stop` prefers the provider's official operation, and process operations are considered only for executions agnt definitely owns. Nothing is killed on the basis of an old PID alone. When the execution location is unknown, stop is shown as unsupported. Approvals are never granted automatically to clear a waiting-for-input state.

Full history deletion, worktree deletion, and branch deletion are not performed in the first version. In particular, Claude's `rm` and Codex's `delete` are not mechanically mapped onto `hide`. [Claude management operations](https://code.claude.com/docs/en/agent-view), [Codex CLI](https://learn.chatgpt.com/docs/developer-commands?surface=cli)

## 11. Performance design

The following are unmeasured targets, not performance guarantees. The initial conditions are a release build, a local SSD, a 1,000-row cache, 10 live sessions, and 100 measurements, recording OS, CPU, CLI version, and whether the cache was present.

| Metric | Provisional target |
| --- | --- |
| Process start to the first draw of the cached list | p95 within 100 ms |
| First UI draw with no cache | p95 within 200 ms. A loading indicator is acceptable |
| Keystroke to a local display update | p95 within 50 ms |
| A retrieved event appearing in the UI | p95 within 100 ms |
| CPU for a list with no updates | Under 1% on average, per core, on the test machine |

The time for the first fresh data fetch and the time to attach to the official CLI are measured separately, not mixed into the drawing times above. Provider startup time and GitHub round-trip time are reported too.

For Claude, the initial proposal is batched polling at a 2-second interval, avoiding overlapping runs. The interval grows while hidden, and failures back off. Codex uses notifications plus a snapshot on reconnect. On versions where notifications are unavailable, the feature degrades.

GitHub queries are batched per repository / branch or per URL, with an initial TTL proposal of 60 seconds and a limit on concurrent queries. When the network is unavailable, the last retrieved value and its timestamp remain. Drawing is event-driven; nothing redraws at a high rate while nothing changes.

## 12. Behaviour on failure

| Situation | Behaviour |
| --- | --- |
| A CLI is not installed or unsupported | Show that provider's state and the reason; the other one still works |
| A CLI's list retrieval is slow | Time out, back off, keep showing the cache |
| The App Server connection drops | Mark the affected state as not confirmed, reconcile after reconnecting |
| The connection drops before a launch result arrives | Treat the result as unknown and reconcile. Do not retry the launch automatically |
| The cwd disappeared | Fail the start; changing to a different cwd is an explicit action |
| The cwd is not under git | Leave the branch blank. The session still starts |
| GitHub is unavailable | Show stored links, with a note about the failed query |
| Metadata lock contention | Retry after a short wait. The UI does not stall |
| An unknown state value | Show Unknown with diagnostic information. Never convert it to done or stopped |
| The official screen exits abnormally | Restore the terminal and re-observe the execution state |

## 13. Implementation and validation order

### P0: technical validation to choose the runtime

Done before polishing the UI's appearance. The purpose of this stage is to remove specification uncertainty, not to finish the product.

| Validation | Pass condition and what to record |
| --- | --- |
| Fetch the real JSON / API | Record title, state, ID, and history scope, and build a mapping table |
| Start a new session and get its ID | The cwd is respected and the row binds to the permanent ID with no duplicate launch |
| Claude start, attach, and return | Return to agnt mid-work and reattach to the same conversation and work |
| Codex start, standard-screen attach, and return | Return to agnt without interrupting the turn, and see work still running or finished |
| Returning while waiting for approval or input | No unanswered request is lost; reattach and answer it |
| Concurrent clients | The observation connection and the standard screen do not both process a request |
| Service lifetime | Record agnt's normal exit, Ghostty's exit, and a dropped connection separately |
| External sessions and past history | Confirm the scope of discovery, state, and resume. Choose how to enumerate Claude's history |
| Terminal restoration | Raw mode, cursor, size, and paste all behave normally after returning |

If the round trip to Codex's standard screen does not work, compare the cost of PTY, a custom Codex UI, and tmux, and update ADR-0001 before moving on. Adopting tmux is not treated as already decided.

### P1: the minimum usable path

Implement list merging, state and content display, starting in any cwd, attach and resume, returning to the list, and hide / restore in JSON. Switching is not treated as complete until R09 holds for both tools.

### P2: management information

Finish manual classification, titles, PR / issue registration, GitHub enrichment, and history display with a clearly defined scope.

### P3: checking the completion conditions

Validate the serious failure paths and performance. Tests focus on these specific risks:

- Two updating processes do not lose a classification change or an added link.
- A failure partway through saving JSON does not corrupt the existing metadata.
- A hidden session does not come back when it is rediscovered.
- `notLoaded`, an unknown value, or an old notification does not cause a wrong state.
- Retrying a launch timeout does not create a duplicate session.
- A branch change in the same cwd does not reassign a past PR.
- A round trip from running or waiting-for-approval preserves the work and the request.
- A GitHub outage or one broken CLI still leaves keystrokes and the list working.
- The performance targets above are measured, with reasons and improvements recorded for any miss.

## 14. Open questions

| ID | What to decide | When to decide |
| --- | --- | --- |
| Q1 | Non-interrupting detach from the Codex standard CLI, and request delivery | P0. Before choosing the runtime |
| Q2 | How strictly keeping the official CLI screen is required | P0, if official integration does not work |
| Q3 | Whether continuation after agnt or Ghostty exits is guaranteed | Before finalizing the service lifetime |
| Q4 | How past ordinary Claude sessions are enumerated | P0. Before settling R02's scope |
| Q5 | Whether hiding from the list is enough for "delete" | When the initial specification is accepted |
| Q6 | The first OS, the minimum CLI versions, and library versions | At implementation start and P0 |

This document's statements about APIs and libraries are based on the official material referenced. Measurement in the user's local environment, installing and starting each CLI, and implementing agnt have not been done yet.
