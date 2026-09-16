# P0 technical validation checklist

- Related documents: [design.md](design.md) §13, [adr/0001-architecture.md](adr/0001-architecture.md) §6
- Status: **not started**

**This validation exists to choose the runtime, and comes before polishing the UI.** Until a check's result is recorded here, the corresponding adapter operation keeps returning `unsupported`. A feature with no recorded result is never displayed as supported.

## Environment to record

| Item | Value |
| --- | --- |
| OS / version | not recorded |
| Claude Code version | not recorded |
| Codex CLI version | not recorded |
| Terminal | not recorded |
| Date run | not recorded |

## Checks

| # | Check | Pass condition and what to record | Result |
| --- | --- | --- | --- |
| V1 | Fetch the real JSON / API | Record title, state, ID, and history scope, and map them onto `RuntimeState` | not started |
| V2 | Start a new session and get its ID | The cwd is respected and the row binds to the permanent ID with no duplicate launch | not started |
| V3 | Claude start, attach, and return | Return to agnt mid-work and reattach to the same conversation and work | not started |
| V4 | Codex start, standard-screen attach, and return | Return to agnt without interrupting the turn, and see work still running or finished | not started |
| V5 | Returning while waiting for approval or input | No unanswered request is lost; reattach and answer it | not started |
| V6 | Concurrent clients | The observation connection and the standard screen do not both process a request | not started |
| V7 | Service lifetime | Record agnt's normal exit, Ghostty's exit, and a dropped connection separately | not started |
| V8 | External sessions and past history | Confirm the scope of discovery, state, and resume. Choose how to enumerate Claude's history | not started |
| V9 | Terminal restoration | Raw mode, cursor, size, and paste all behave normally after returning | not started |

The basic procedure is to run "start new → long-running work → attach to the official screen → return to the list → reattach" on both providers.

## Mapping to the open questions

| ID | What to decide | Covering checks |
| --- | --- | --- |
| Q1 | Non-interrupting detach from the Codex standard CLI, and request delivery | V4, V5, V6 |
| Q2 | How strictly keeping the official CLI screen is required | V3, V4 |
| Q3 | Whether continuation after agnt or Ghostty exits is guaranteed | V7 |
| Q4 | How past ordinary Claude sessions are enumerated | V8 |
| Q5 | Whether hiding from the list is enough for "delete" | When the initial specification is accepted. Not a validation item |
| Q6 | The first OS, the minimum CLI versions, and library versions | The environment table above |

## Work after validating

1. Fill in the results and the measured values in the table above.
2. If the round trip to Codex's standard screen does not work, compare the cost of PTY, a custom Codex UI, and tmux, and update ADR-0001 before implementing. Adopting tmux is not treated as already decided.
3. Implement the affected adapter's `capabilities` and operations to match the confirmed behaviour. Remove `P0_REASON` for an operation only once that operation's check is done.
4. Update ADR-0001 to match the implementation, and review the conditions for moving it to Accepted (§6).
