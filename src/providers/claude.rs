//! Claude Code adapter, built on the official CLI.
//!
//! `claude agents --json` prints active sessions as a JSON array and does not
//! need a TTY; `--all` adds completed background sessions. Both are called, and
//! the difference between them is what tells a finished-but-alive session
//! (Idle) from one whose process is gone (Stopped) — `state: "done"` alone does
//! not say which, and it never means the user's work is done.
//!
//! Operations use the documented subcommands: `claude attach <id>`,
//! `claude stop <id>`, `claude logs <id>`, `claude --resume <sessionId>`, and
//! `claude --bg` to start. The short `id` is the operation handle; the
//! `sessionId` UUID is the identity. Observed shapes are recorded in
//! docs/p0-validation.md.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;

use super::process::{find_in_path, run, version_of};
use super::{
    Adapter, AdapterError, AttachPlan, Discovery, RuntimeUpdate, StartOutcome, StartRequest, StartResult,
    StopResult,
};
use crate::model::{
    Capabilities, Operation, Origin, Provider, Runtime, RuntimeHandle, RuntimeState, Session, SessionKey,
    Support, text,
};

pub const BINARY: &str = "claude";

/// Why `preview` is unsupported even though `claude logs <id>` exists.
///
/// `logs` prints the session's recent *terminal screen*, not its conversation.
/// After the escape sequences are stripped, the tail of that screen is the
/// status bar and spinner, so the excerpt reads as UI chrome rather than as
/// what the session is doing. Returning that would be an unsupported operation
/// dressed up as a working one. A real preview needs a transcript source; see
/// docs/p0-validation.md.
pub const PREVIEW_REASON: &str =
    "`claude logs` returns the terminal screen, not conversation content; a transcript source is needed";

const LIST_TIMEOUT: Duration = Duration::from_secs(15);
const STOP_TIMEOUT: Duration = Duration::from_secs(30);
const DELETE_TIMEOUT: Duration = Duration::from_secs(30);
const START_TIMEOUT: Duration = Duration::from_secs(120);

/// One element of `claude agents --json`. Unknown fields are ignored so a
/// newer CLI does not break the list.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentEntry {
    /// Short handle taken by `attach`, `logs`, `stop`, and `rm`.
    pub id: String,
    /// Conversation UUID. Absent on older versions, where `id` is the identity.
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub kind: Option<String>,
    /// Epoch milliseconds.
    #[serde(default)]
    pub started_at: Option<i64>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub pid: Option<u32>,
}

impl AgentEntry {
    /// The conversation's permanent identity.
    pub fn native_id(&self) -> &str {
        self.session_id.as_deref().unwrap_or(&self.id)
    }

    fn started(&self) -> Option<DateTime<Utc>> {
        self.started_at
            .and_then(|ms| Utc.timestamp_millis_opt(ms).single())
    }

    /// Map the provider's `state` onto a RuntimeState.
    ///
    /// `live` says the entry came from the default listing, which is the only
    /// evidence that the execution is still around. `blocked` becomes
    /// WaitingInput because the JSON does not separate an approval request
    /// from a question; both sort and group together, so the distinction does
    /// not change the list.
    pub fn runtime_state(&self, live: bool) -> RuntimeState {
        match self.state.as_deref() {
            Some("working") => RuntimeState::Running,
            Some("blocked") => RuntimeState::WaitingInput,
            Some("done") if live => RuntimeState::Idle,
            Some("done") => RuntimeState::Stopped,
            Some("error") | Some("failed") => RuntimeState::Error,
            _ => RuntimeState::Unknown,
        }
    }

    fn evidence(&self, live: bool) -> String {
        format!(
            "claude agents --json: state={}, status={}, {}",
            self.state.as_deref().unwrap_or("-"),
            self.status.as_deref().unwrap_or("-"),
            if live {
                "listed as active"
            } else {
                "completed only"
            },
        )
    }

    pub fn to_session(&self, live: bool) -> Session {
        let key = SessionKey::new(Provider::Claude, self.native_id());
        let cwd = self.cwd.clone().unwrap_or_else(|| PathBuf::from("."));
        let mut s = Session::new(key, cwd);
        s.title = self
            .name
            .as_ref()
            .map(|n| text::sanitize(n))
            .filter(|n| !n.is_empty());
        s.origin = Origin::Discovered;
        s.last_activity_at = self.started();
        s.runtime = Runtime {
            state: self.runtime_state(live),
            handle: Some(RuntimeHandle::ClaudeJob {
                job_id: self.id.clone(),
            }),
            evidence: self.evidence(live),
            observed_at: Some(Utc::now()),
            last_known_state: None,
        };
        if s.runtime.state != RuntimeState::Unknown {
            s.runtime.last_known_state = Some(s.runtime.state);
        }
        s
    }
}

#[derive(Debug, Clone)]
pub struct ClaudeAdapter {
    binary: Option<PathBuf>,
}

impl ClaudeAdapter {
    pub fn detect() -> Self {
        Self {
            binary: find_in_path(BINARY),
        }
    }

    pub fn with_binary(path: impl Into<PathBuf>) -> Self {
        Self {
            binary: Some(path.into()),
        }
    }

    fn bin(&self, operation: Operation) -> Result<&Path, AdapterError> {
        self.binary.as_deref().ok_or(AdapterError::Unsupported {
            provider: Provider::Claude,
            operation,
            reason: format!("`{BINARY}` not found in PATH"),
        })
    }

    async fn list(&self, all: bool) -> Result<Vec<AgentEntry>, AdapterError> {
        let bin = self.bin(Operation::Discover)?;
        let args: &[&str] = if all {
            &["agents", "--json", "--all"]
        } else {
            &["agents", "--json"]
        };
        let out = run(bin, args, None, LIST_TIMEOUT).await?;
        parse_entries(&out.stdout)
    }

    /// The short handle `attach`, `logs`, and `stop` take. Looked up rather
    /// than derived from the UUID, so a change in how Claude shortens IDs
    /// cannot send an operation to the wrong session.
    async fn job_id_for(&self, key: &SessionKey, operation: Operation) -> Result<String, AdapterError> {
        Ok(self.entry_for(key, operation).await?.id)
    }

    async fn entry_for(&self, key: &SessionKey, operation: Operation) -> Result<AgentEntry, AdapterError> {
        let entries = self.list(true).await?;
        entries
            .into_iter()
            .find(|e| e.native_id() == key.native_session_id)
            .ok_or(AdapterError::Unsupported {
                provider: Provider::Claude,
                operation,
                reason: format!(
                    "`claude agents --json --all` does not list {}; it may have been removed",
                    key.native_session_id
                ),
            })
    }
}

/// Parse `claude agents --json`. An empty body is an empty list, not an error.
pub fn parse_entries(stdout: &[u8]) -> Result<Vec<AgentEntry>, AdapterError> {
    let text = String::from_utf8_lossy(stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(trimmed).map_err(|e| AdapterError::CliFailed {
        command: "claude agents --json".into(),
        status: "unparsable output".into(),
        stderr: e.to_string(),
    })
}

/// Merge the active listing and the full listing into one set of sessions.
/// An entry present in `active` is live; the rest are completed.
pub fn merge_listings(active: &[AgentEntry], all: &[AgentEntry]) -> Vec<Session> {
    let live: Vec<&str> = active.iter().map(AgentEntry::native_id).collect();
    let mut sessions: Vec<Session> = Vec::new();
    // `all` should be a superset, but a session that raced into completion
    // between the two calls would only be in `active`; keep both sides.
    for entry in all.iter().chain(active.iter()) {
        let id = entry.native_id();
        if sessions.iter().any(|s| s.key.native_session_id == id) {
            continue;
        }
        sessions.push(entry.to_session(live.contains(&id)));
    }
    sessions
}

#[async_trait]
impl Adapter for ClaudeAdapter {
    fn provider(&self) -> Provider {
        Provider::Claude
    }

    fn binary(&self) -> Option<&Path> {
        self.binary.as_deref()
    }

    async fn version(&self) -> Result<String, AdapterError> {
        version_of(self.binary.as_deref(), BINARY).await
    }

    async fn capabilities(&self) -> Capabilities {
        match &self.binary {
            None => Capabilities::none(format!("`{BINARY}` not found in PATH")),
            Some(_) => Capabilities {
                discover: Support::Supported,
                observe: Support::Supported,
                preview: Support::unsupported(PREVIEW_REASON),
                start: Support::Supported,
                attach: Support::Supported,
                resume: Support::Supported,
                stop: Support::Supported,
                delete: Support::Supported,
            },
        }
    }

    async fn discover(&self) -> Result<Discovery, AdapterError> {
        let (active, all) = tokio::try_join!(self.list(false), self.list(true))?;
        let note = format!(
            "claude agents --json: {} active, {} including completed",
            active.len(),
            all.len()
        );
        Ok(Discovery {
            sessions: merge_listings(&active, &all),
            scope_note: note,
            complete: true,
        })
    }

    async fn observe(&self, keys: &[SessionKey]) -> Result<Vec<RuntimeUpdate>, AdapterError> {
        let discovery = self.discover().await?;
        Ok(discovery
            .sessions
            .into_iter()
            .filter(|s| keys.contains(&s.key))
            .map(|s| RuntimeUpdate {
                key: s.key,
                runtime: s.runtime,
            })
            .collect())
    }

    async fn preview(&self, _key: &SessionKey, _max_chars: usize) -> Result<Option<String>, AdapterError> {
        Err(AdapterError::Unsupported {
            provider: Provider::Claude,
            operation: Operation::Preview,
            reason: PREVIEW_REASON.to_string(),
        })
    }

    async fn start(&self, req: StartRequest) -> Result<StartOutcome, AdapterError> {
        let bin = self.bin(Operation::Start)?;
        let prompt = req.prompt.as_deref().unwrap_or("").trim();
        if prompt.is_empty() {
            return Err(AdapterError::Unsupported {
                provider: Provider::Claude,
                operation: Operation::Start,
                reason: "a background Claude session needs an initial prompt".into(),
            });
        }
        let out = run(bin, &["--bg", prompt], Some(&req.cwd), START_TIMEOUT).await?;
        let printed = String::from_utf8_lossy(&out.stdout);
        let job_id = printed
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .ok_or_else(|| AdapterError::CliFailed {
                command: "claude --bg".into(),
                status: "no id printed".into(),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            })?
            .to_string();

        // `--bg` prints the short id; look the conversation UUID up so the row
        // gets its permanent key instead of the handle.
        let native = self
            .list(true)
            .await
            .ok()
            .and_then(|es| {
                es.into_iter()
                    .find(|e| e.id == job_id)
                    .map(|e| e.native_id().to_string())
            })
            .unwrap_or_else(|| job_id.clone());

        Ok(StartOutcome::Started(StartResult {
            key: SessionKey::new(Provider::Claude, native),
            handle: Some(RuntimeHandle::ClaudeJob { job_id }),
        }))
    }

    async fn attach(&self, key: &SessionKey) -> Result<AttachPlan, AdapterError> {
        let bin = self.bin(Operation::Attach)?;
        let job_id = self.job_id_for(key, Operation::Attach).await?;
        Ok(AttachPlan {
            program: bin.to_path_buf(),
            args: vec!["attach".into(), job_id],
            cwd: None,
            note: "Claude owns the terminal now. Left arrow returns to its agent view; Ctrl+Z drops back here. The session keeps running either way.".into(),
        })
    }

    async fn resume(&self, key: &SessionKey) -> Result<AttachPlan, AdapterError> {
        let bin = self.bin(Operation::Resume)?;
        Ok(AttachPlan {
            program: bin.to_path_buf(),
            args: vec!["--resume".into(), key.native_session_id.clone()],
            cwd: None,
            note: "Reopening the saved conversation. This does not reattach to a run that was already going."
                .into(),
        })
    }

    async fn stop(&self, key: &SessionKey) -> Result<StopResult, AdapterError> {
        let bin = self.bin(Operation::Stop)?;
        let job_id = self.job_id_for(key, Operation::Stop).await?;
        run(bin, &["stop", &job_id], None, STOP_TIMEOUT).await?;
        Ok(StopResult::Confirmed)
    }

    /// `claude rm` also removes the session's worktree, and refuses when that
    /// would lose unpushed commits. agnt never passes the flags that override
    /// the refusal; Claude's message is shown instead.
    async fn delete(&self, key: &SessionKey) -> Result<(), AdapterError> {
        let bin = self.bin(Operation::Delete)?;
        let entry = self.entry_for(key, Operation::Delete).await?;
        if entry.runtime_state(false).is_live() {
            return Err(AdapterError::Unsupported {
                provider: Provider::Claude,
                operation: Operation::Delete,
                reason: "it is running; stop it first".into(),
            });
        }
        run(bin, &["rm", &entry.id], None, DELETE_TIMEOUT).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from `claude agents --json` on 2026-09-16, CLI 2.1.270.
    const SAMPLE: &str = r#"[
      {"id":"5175105c","cwd":"/Users/me/src/reed","kind":"background","startedAt":1786708446815,
       "sessionId":"5175105c-ec25-4624-a2f9-9addbe8d6ec3","name":"auth subscription","state":"blocked"},
      {"pid":3676,"id":"3f6a6553","cwd":"/Users/me/src/agnt","kind":"background","startedAt":1789285723075,
       "sessionId":"3f6a6553-7021-4f78-b4fc-5803a9340fe9","name":"rust scaffolding","status":"busy","state":"working"},
      {"pid":30982,"id":"450b4952","cwd":"/Users/me/src/memo","kind":"background","startedAt":1789561225410,
       "sessionId":"450b4952-286f-4761-a89b-33d40a4fad2e","name":"clock in","status":"idle","state":"done"}
    ]"#;

    const COMPLETED: &str = r#"[
      {"id":"98f57e7a","cwd":"/Users/me/src/reed","kind":"background","startedAt":1786106639560,
       "sessionId":"98f57e7a-80ed-4212-b681-88cd88424c6c","name":"admin views","state":"done"}
    ]"#;

    fn entries(s: &str) -> Vec<AgentEntry> {
        parse_entries(s.as_bytes()).unwrap()
    }

    #[test]
    fn parses_the_real_json_shape() {
        let es = entries(SAMPLE);
        assert_eq!(es.len(), 3);
        assert_eq!(es[0].id, "5175105c");
        assert_eq!(es[0].native_id(), "5175105c-ec25-4624-a2f9-9addbe8d6ec3");
        assert_eq!(es[1].pid, Some(3676));
        assert_eq!(es[2].name.as_deref(), Some("clock in"));
    }

    #[test]
    fn empty_output_is_an_empty_list() {
        assert!(parse_entries(b"").unwrap().is_empty());
        assert!(parse_entries(b"  \n").unwrap().is_empty());
        assert!(parse_entries(b"[]").unwrap().is_empty());
    }

    #[test]
    fn unknown_fields_and_states_do_not_break_parsing() {
        let json = r#"[{"id":"x1","sessionId":"x1-uuid","state":"hibernating","brandNewField":{"a":1}}]"#;
        let es = entries(json);
        assert_eq!(es[0].runtime_state(true), RuntimeState::Unknown);
    }

    #[test]
    fn done_is_idle_while_listed_active_and_stopped_once_completed() {
        let active = entries(SAMPLE);
        let all = entries(COMPLETED);
        let sessions = merge_listings(&active, &all);
        let by_id = |uuid: &str| {
            sessions
                .iter()
                .find(|s| s.key.native_session_id == uuid)
                .unwrap()
                .clone()
        };
        // Listed as active, turn finished -> Idle, never WorkflowState::Done.
        assert_eq!(
            by_id("450b4952-286f-4761-a89b-33d40a4fad2e").runtime.state,
            RuntimeState::Idle
        );
        // Only in the --all listing -> the process is gone.
        assert_eq!(
            by_id("98f57e7a-80ed-4212-b681-88cd88424c6c").runtime.state,
            RuntimeState::Stopped
        );
        assert_eq!(
            by_id("3f6a6553-7021-4f78-b4fc-5803a9340fe9").runtime.state,
            RuntimeState::Running
        );
        // `blocked` does not say approval or input; it groups with both.
        assert_eq!(
            by_id("5175105c-ec25-4624-a2f9-9addbe8d6ec3").runtime.state,
            RuntimeState::WaitingInput
        );
    }

    #[test]
    fn merge_lists_each_session_once_and_keeps_the_job_handle() {
        let active = entries(SAMPLE);
        let all = entries(COMPLETED);
        let sessions = merge_listings(&active, &all);
        assert_eq!(sessions.len(), 4);
        let running = sessions
            .iter()
            .find(|s| s.key.native_session_id.starts_with("3f6a6553"))
            .unwrap();
        assert_eq!(
            running.runtime.handle,
            Some(RuntimeHandle::ClaudeJob {
                job_id: "3f6a6553".into()
            })
        );
        assert_eq!(running.title.as_deref(), Some("rust scaffolding"));
        assert_eq!(running.cwd, PathBuf::from("/Users/me/src/agnt"));
    }

    #[test]
    fn a_session_in_both_listings_is_not_duplicated() {
        let active = entries(SAMPLE);
        let sessions = merge_listings(&active, &active);
        assert_eq!(sessions.len(), 3);
    }

    /// A fake `claude` that lists `SAMPLE` and records what `rm` was given.
    fn fake_claude(dir: &Path) -> (ClaudeAdapter, PathBuf) {
        let log = dir.join("rm-args");
        let bin = dir.join("claude");
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = agents ]; then cat <<'EOF'\n{SAMPLE}\nEOF\nelse echo \"$@\" > {}; fi\n",
            log.display()
        );
        std::fs::write(&bin, script).unwrap();
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&bin, perms).unwrap();
        (ClaudeAdapter { binary: Some(bin) }, log)
    }

    #[tokio::test]
    async fn delete_refuses_a_running_session_and_removes_a_finished_one() {
        let dir = tempfile::tempdir().unwrap();
        let (a, log) = fake_claude(dir.path());

        let blocked = SessionKey::new(Provider::Claude, "5175105c-ec25-4624-a2f9-9addbe8d6ec3");
        assert!(a.delete(&blocked).await.unwrap_err().is_unsupported());
        assert!(!log.exists());

        let done = SessionKey::new(Provider::Claude, "450b4952-286f-4761-a89b-33d40a4fad2e");
        a.delete(&done).await.unwrap();
        assert_eq!(std::fs::read_to_string(&log).unwrap().trim(), "rm 450b4952");
    }

    #[tokio::test]
    async fn missing_binary_is_unsupported_everywhere() {
        let a = ClaudeAdapter { binary: None };
        let caps = a.capabilities().await;
        for op in Operation::ALL {
            assert!(!caps.get(op).is_supported(), "{op:?}");
        }
        assert!(a.discover().await.unwrap_err().is_unsupported());
        assert!(
            a.attach(&SessionKey::new(Provider::Claude, "x"))
                .await
                .unwrap_err()
                .is_unsupported()
        );
    }

    #[tokio::test]
    async fn resume_uses_the_conversation_uuid_not_the_short_handle() {
        let a = ClaudeAdapter::with_binary("/usr/bin/true");
        let key = SessionKey::new(Provider::Claude, "5175105c-ec25-4624-a2f9-9addbe8d6ec3");
        let plan = a.resume(&key).await.unwrap();
        assert_eq!(
            plan.args,
            vec!["--resume", "5175105c-ec25-4624-a2f9-9addbe8d6ec3"]
        );
    }

    #[tokio::test]
    async fn start_without_a_prompt_is_refused() {
        let a = ClaudeAdapter::with_binary("/usr/bin/true");
        let err = a
            .start(StartRequest {
                operation_id: "op".into(),
                cwd: "/tmp".into(),
                prompt: None,
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("needs an initial prompt"));
    }
}
