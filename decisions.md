# Decisions

## DR: Deleting a session from agnt
- **Date**: 2026-10-17
- **Context**: agnt could only hide a row (`Ctrl+X`, `agnt hide`), which keeps the provider's data. A real delete was requested.
- **Decision**: Delete through each provider's official CLI (`claude rm`, `codex delete`), after a confirmation in agnt. Never pass the flags that override the provider's own refusals. Forget agnt's record of the session only after the provider confirms.
- **Rationale**: The provider owns the conversation data and its safety checks (for example, Claude refuses to drop a worktree with unpushed commits). agnt deleting files itself would bypass those checks and break when the storage layout changes.
