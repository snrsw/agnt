# Decisions

## DR: Deleting a session from agnt
- **Date**: 2026-10-17
- **Context**: agnt could only hide a row (`Ctrl+X`, `agnt hide`), which keeps the provider's data. A real delete was requested.
- **Decision**: Delete through each provider's official CLI: `claude rm <job-id>` and `codex delete --force <uuid>`. The Delete key opens a confirmation dialog in the TUI; Enter confirms and Esc cancels. `agnt delete <id>` asks for Enter unless `--yes` is passed. Running sessions must be stopped first. agnt never passes `claude rm --discard-unpushed` or `--force-remove-worktree`; when Claude refuses, its message is shown. agnt's own metadata for the session is removed after the provider confirms.
- **Rationale**: The provider owns the conversation data and its safety checks (for example, Claude refuses to drop a worktree with unpushed commits). agnt deleting files itself would bypass those checks and break when the storage layout changes.
