# Providers

* [Adapter](/providers/adapter.md) - The one interface every provider implements, so the core and TUI never see tool-specific behaviour.
* [Capabilities](/providers/capabilities.md) - Per-operation support with a reason, so an unsupported operation is never reported as success.
* [Claude Code](/providers/claude-code.md) - The provider with full support, driven through `claude agents --json`, `--bg`, `attach`, `stop`, and `rm`.
* [Codex](/providers/codex.md) - The provider read from saved history; start, resume, and delete work, live observation does not yet.
