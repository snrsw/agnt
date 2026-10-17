//! Codex adapter.
//!
//! Codex has no scriptable session listing: `codex agents` is itself a TUI and
//! the App Server is reached over a socket protocol. So discovery uses a
//! **read-only history reader** over `~/.codex/sessions/**/rollout-*.jsonl`,
//! whose first line is a `session_meta` record. The design doc allows this as
//! long as the choice and the target format are stated, which is what this
//! comment and docs/p0-validation.md do.
//!
//! Because that reader sees files and not processes, a discovered session's
//! state is always Unknown: a rollout file says a conversation existed, never
//! that it is running now. `attach`, `stop`, and `observe` stay unsupported
//! until the App Server work in P0 (Q1/V4) is done — resuming a conversation
//! is not the same as attaching to a running one, and guessing would risk a
//! double execution.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::process::{find_in_path, run, version_of};
use super::{
    Adapter, AdapterError, AttachPlan, Discovery, RuntimeUpdate, StartOutcome, StartRequest, StopResult,
};
use crate::model::{
    Capabilities, Operation, Origin, Provider, Runtime, RuntimeState, Session, SessionKey, Support, text,
};
use crate::transcript::{self, Turn};

pub const BINARY: &str = "codex";

/// Why the operations that need a live connection are unsupported.
pub const APP_SERVER_REASON: &str =
    "needs the Codex App Server protocol; record V4/V6 in docs/p0-validation.md first";

const READ_TIMEOUT: Duration = Duration::from_secs(10);
const DELETE_TIMEOUT: Duration = Duration::from_secs(30);

/// How far back the history reader looks.
///
/// "Saved past sessions" is not "every conversation ever had". Codex keeps
/// rollout files indefinitely, and on the machine this was built against 41 of
/// 46 were more than four months old — finished work, not work anyone is going
/// back to. The list is for returning to what you left, so it stops at a month
/// and says how many it passed over. `codex resume` remains the way to reach
/// anything older.
pub const HISTORY_WINDOW_DAYS: i64 = 30;

/// The `session_meta` record that opens every rollout file.
#[derive(Debug, Clone, Deserialize)]
struct RolloutLine {
    #[serde(rename = "type")]
    kind: String,
    payload: SessionMeta,
}

#[derive(Debug, Clone, Deserialize)]
struct SessionMeta {
    /// The conversation (thread) id that `codex resume` takes.
    session_id: Option<String>,
    /// This rollout's own id. Used when `session_id` is missing.
    id: Option<String>,
    cwd: Option<PathBuf>,
    timestamp: Option<DateTime<Utc>>,
    #[serde(default)]
    originator: Option<String>,
    /// `"cli"`, `"exec"`, `"vscode"`, or `{"subagent": ...}` for a nested run.
    #[serde(default)]
    source: Option<serde_json::Value>,
}

impl SessionMeta {
    fn thread_id(&self) -> Option<&str> {
        self.session_id.as_deref().or(self.id.as_deref())
    }

    /// A subagent rollout belongs to another session's turn, not to the user's
    /// list of conversations.
    fn is_subagent(&self) -> bool {
        self.source
            .as_ref()
            .and_then(|s| s.as_object())
            .is_some_and(|o| o.contains_key("subagent"))
    }
}

/// One line of `~/.codex/session_index.jsonl`, which names threads.
#[derive(Debug, Clone, Deserialize)]
struct IndexLine {
    id: String,
    thread_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CodexAdapter {
    binary: Option<PathBuf>,
    home: Option<PathBuf>,
    window: chrono::Duration,
}

impl CodexAdapter {
    pub fn detect() -> Self {
        Self {
            binary: find_in_path(BINARY),
            home: default_codex_home(),
            window: chrono::Duration::days(HISTORY_WINDOW_DAYS),
        }
    }

    pub fn with_binary(path: impl Into<PathBuf>) -> Self {
        Self {
            binary: Some(path.into()),
            home: default_codex_home(),
            window: chrono::Duration::days(HISTORY_WINDOW_DAYS),
        }
    }

    /// Point the history reader at a different `~/.codex`, for tests.
    pub fn with_home(mut self, home: impl Into<PathBuf>) -> Self {
        self.home = Some(home.into());
        self
    }

    /// Widen or narrow how far back history is read.
    pub fn with_window(mut self, window: chrono::Duration) -> Self {
        self.window = window;
        self
    }

    fn unsupported(&self, operation: Operation, reason: &str) -> AdapterError {
        AdapterError::Unsupported {
            provider: Provider::Codex,
            operation,
            reason: match &self.binary {
                None => format!("`{BINARY}` not found in PATH"),
                Some(_) => reason.to_string(),
            },
        }
    }

    fn bin(&self, operation: Operation) -> Result<&Path, AdapterError> {
        self.binary.as_deref().ok_or(AdapterError::Unsupported {
            provider: Provider::Codex,
            operation,
            reason: format!("`{BINARY}` not found in PATH"),
        })
    }
}

pub fn default_codex_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CODEX_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex"))
}

/// Read every rollout file's opening `session_meta` line and fold them into one
/// session per thread, keeping the most recent timestamp.
///
/// Only the first line of each file is read, so cost grows with the number of
/// conversations rather than their length.
pub fn read_history(home: &Path, cutoff: Option<DateTime<Utc>>) -> (Vec<Session>, usize) {
    let titles = read_titles(home);
    let mut newest: BTreeMap<String, (DateTime<Utc>, SessionMeta)> = BTreeMap::new();

    for path in rollout_files(&home.join("sessions")) {
        let Some(meta) = read_session_meta(&path) else {
            continue;
        };
        if meta.is_subagent() {
            continue;
        }
        let Some(id) = meta.thread_id().map(str::to_string) else {
            continue;
        };
        let at = meta.timestamp.unwrap_or_else(Utc::now);
        match newest.get(&id) {
            Some((seen, _)) if *seen >= at => {}
            _ => {
                newest.insert(id, (at, meta));
            }
        }
    }

    // Age out whole conversations, after folding their rollouts together, so a
    // thread is judged by when it was last touched rather than by when some
    // fork of it started.
    let mut older = 0usize;
    if let Some(cutoff) = cutoff {
        newest.retain(|_, (at, _)| {
            let keep = *at >= cutoff;
            if !keep {
                older += 1;
            }
            keep
        });
    }

    let sessions = newest
        .into_iter()
        .map(|(id, (at, meta))| {
            let key = SessionKey::new(Provider::Codex, &id);
            let cwd = meta.cwd.clone().unwrap_or_else(|| PathBuf::from("."));
            let mut s = Session::new(key, cwd);
            s.title = titles
                .get(&id)
                .map(|t| text::sanitize(t))
                .filter(|t| !t.is_empty());
            s.origin = Origin::History;
            s.last_activity_at = Some(at);
            s.runtime = Runtime {
                state: RuntimeState::Unknown,
                handle: None,
                evidence: format!(
                    "codex history file ({}); liveness needs the App Server",
                    meta.originator.as_deref().unwrap_or("unknown originator")
                ),
                observed_at: Some(Utc::now()),
                last_known_state: None,
            };
            s
        })
        .collect();
    (sessions, older)
}

/// `session_index.jsonl` maps a thread id to its name. Later lines win, since
/// a rename appends rather than rewrites.
fn read_titles(home: &Path) -> BTreeMap<String, String> {
    let mut titles = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(home.join("session_index.jsonl")) else {
        return titles;
    };
    for line in text.lines() {
        if let Ok(entry) = serde_json::from_str::<IndexLine>(line)
            && let Some(name) = entry.thread_name
        {
            titles.insert(entry.id, name);
        }
    }
    titles
}

/// `sessions/<year>/<month>/<day>/rollout-*.jsonl`, walked without following
/// symlinks out of the tree.
fn rollout_files(sessions: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > 4 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(t) if t.is_dir() => walk(&path, depth + 1, out),
                Ok(t) if t.is_file() => {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with("rollout-") && name.ends_with(".jsonl") {
                        out.push(path);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(sessions, 0, &mut out);
    out
}

/// The newest non-subagent rollout of a thread. A thread forked or resumed
/// has several; the newest holds the turns most recently added.
fn rollout_for(home: &Path, thread_id: &str) -> Option<PathBuf> {
    rollout_files(&home.join("sessions"))
        .into_iter()
        .filter_map(|p| read_session_meta(&p).map(|m| (p, m)))
        .filter(|(_, m)| !m.is_subagent() && m.thread_id() == Some(thread_id))
        .max_by_key(|(_, m)| m.timestamp)
        .map(|(p, _)| p)
}

fn read_session_meta(path: &Path) -> Option<SessionMeta> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(path).ok()?;
    let mut first = String::new();
    BufReader::new(file).read_line(&mut first).ok()?;
    let line: RolloutLine = serde_json::from_str(first.trim()).ok()?;
    (line.kind == "session_meta").then_some(line.payload)
}

#[async_trait]
impl Adapter for CodexAdapter {
    fn provider(&self) -> Provider {
        Provider::Codex
    }

    fn binary(&self) -> Option<&Path> {
        self.binary.as_deref()
    }

    async fn version(&self) -> Result<String, AdapterError> {
        version_of(self.binary.as_deref(), BINARY).await
    }

    async fn capabilities(&self) -> Capabilities {
        if self.binary.is_none() {
            return Capabilities::none(format!("`{BINARY}` not found in PATH"));
        }
        Capabilities {
            discover: Support::Supported,
            observe: Support::unsupported(APP_SERVER_REASON),
            preview: Support::unsupported(
                "the rollout reader only reads the session_meta line; transcript reading is P2",
            ),
            start: Support::Supported,
            attach: Support::unsupported(APP_SERVER_REASON),
            resume: Support::Supported,
            stop: Support::unsupported(APP_SERVER_REASON),
            delete: Support::Supported,
        }
    }

    async fn discover(&self) -> Result<Discovery, AdapterError> {
        let Some(home) = self.home.clone() else {
            return Err(self.unsupported(Operation::Discover, "no CODEX_HOME or HOME to read"));
        };
        let cutoff = Some(Utc::now() - self.window);
        // Blocking file walk, kept off the runtime's async workers.
        let (sessions, older) = tokio::time::timeout(
            READ_TIMEOUT,
            tokio::task::spawn_blocking(move || read_history(&home, cutoff)),
        )
        .await
        .map_err(|_| AdapterError::Timeout {
            command: "codex history read".into(),
            timeout_ms: READ_TIMEOUT.as_millis() as u64,
        })?
        .map_err(|e| AdapterError::CliFailed {
            command: "codex history read".into(),
            status: "join error".into(),
            stderr: e.to_string(),
        })?;

        // The scope has to be visible: a reader that silently drops rows is
        // indistinguishable from one that failed.
        let days = self.window.num_days();
        let note = if older > 0 {
            format!(
                "{} conversations from the last {days}d ({older} older not read); running state needs the App Server",
                sessions.len()
            )
        } else {
            format!(
                "{} conversations from the last {days}d; running state needs the App Server",
                sessions.len()
            )
        };
        Ok(Discovery {
            sessions,
            scope_note: note,
            complete: true,
        })
    }

    async fn observe(&self, _keys: &[SessionKey]) -> Result<Vec<RuntimeUpdate>, AdapterError> {
        Err(self.unsupported(Operation::Observe, APP_SERVER_REASON))
    }

    async fn preview(&self, _key: &SessionKey, _max_chars: usize) -> Result<Option<String>, AdapterError> {
        Err(self.unsupported(
            Operation::Preview,
            "the rollout reader only reads the session_meta line; transcript reading is P2",
        ))
    }

    /// Codex only starts interactively, so the terminal is handed to it and
    /// the new conversation is picked up by the next discovery. No identity is
    /// invented in the meantime.
    async fn conversation(&self, key: &SessionKey, max_turns: usize) -> Result<Vec<Turn>, AdapterError> {
        let Some(home) = self.home.clone() else {
            return Ok(Vec::new());
        };
        let id = key.native_session_id.clone();
        let read = tokio::task::spawn_blocking(move || match rollout_for(&home, &id) {
            Some(path) => transcript::read_tail(&path, transcript::parse_codex, max_turns),
            None => Ok(Vec::new()),
        });
        Ok(read.await.map_err(std::io::Error::other)??)
    }

    async fn start(&self, req: StartRequest) -> Result<StartOutcome, AdapterError> {
        let bin = self.bin(Operation::Start)?;
        let mut args = Vec::new();
        if let Some(prompt) = req.prompt.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
            args.push(prompt.to_string());
        }
        Ok(StartOutcome::Interactive(AttachPlan {
            program: bin.to_path_buf(),
            args,
            cwd: Some(req.cwd),
            note: "Codex starts interactively. Leave it when you are done and the session joins the list."
                .into(),
        }))
    }

    async fn attach(&self, _key: &SessionKey) -> Result<AttachPlan, AdapterError> {
        Err(self.unsupported(Operation::Attach, APP_SERVER_REASON))
    }

    async fn resume(&self, key: &SessionKey) -> Result<AttachPlan, AdapterError> {
        let bin = self.bin(Operation::Resume)?;
        Ok(AttachPlan {
            program: bin.to_path_buf(),
            args: vec!["resume".into(), key.native_session_id.clone()],
            cwd: None,
            note: "Reopening the saved conversation. Codex cannot say whether this thread is still running elsewhere, so check before you send a turn.".into(),
        })
    }

    async fn stop(&self, _key: &SessionKey) -> Result<StopResult, AdapterError> {
        Err(self.unsupported(Operation::Stop, APP_SERVER_REASON))
    }

    /// `--force` skips Codex's own prompt: agnt has already asked, and the
    /// CLI runs with no terminal to ask on.
    async fn delete(&self, key: &SessionKey) -> Result<(), AdapterError> {
        let bin = self.bin(Operation::Delete)?;
        run(
            bin,
            &["delete", "--force", &key.native_session_id],
            None,
            DELETE_TIMEOUT,
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_rollout(dir: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), body).unwrap();
    }

    /// A rollout timestamp `d` days before now.
    fn days_ago(d: i64) -> String {
        (Utc::now() - chrono::Duration::days(d))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }

    /// Shape captured from a real rollout file on 2026-09-16, codex-cli 0.154.0.
    fn meta_line(session_id: &str, cwd: &str, ts: &str, source: &str) -> String {
        format!(
            r#"{{"timestamp":"{ts}","type":"session_meta","payload":{{"session_id":"{session_id}","id":"{session_id}","cwd":"{cwd}","timestamp":"{ts}","originator":"codex-tui","cli_version":"0.154.0","source":{source}}}}}
{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"task_started"}}}}
"#
        )
    }

    fn home_with(files: &[(&str, String)], index: Option<&str>) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let day = home.path().join("sessions/2026/09/16");
        for (name, body) in files {
            write_rollout(&day, name, body);
        }
        if let Some(index) = index {
            std::fs::write(home.path().join("session_index.jsonl"), index).unwrap();
        }
        home
    }

    #[test]
    fn reads_thread_id_cwd_and_time_from_the_first_line() {
        let home = home_with(
            &[(
                "rollout-2026-09-16T10-00-00-aaa.jsonl",
                meta_line(
                    "01a07697-25db-7b01-ad8b-1aa4ce86c953",
                    "/Users/me/src/api",
                    "2026-09-16T10:00:00Z",
                    "\"cli\"",
                ),
            )],
            Some(r#"{"id":"01a07697-25db-7b01-ad8b-1aa4ce86c953","thread_name":"Update the PRV version"}"#),
        );
        let (sessions, _) = read_history(home.path(), None);
        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s.key.native_session_id, "01a07697-25db-7b01-ad8b-1aa4ce86c953");
        assert_eq!(s.cwd, PathBuf::from("/Users/me/src/api"));
        assert_eq!(s.title.as_deref(), Some("Update the PRV version"));
        // A file proves the conversation existed, never that it is running.
        assert_eq!(s.runtime.state, RuntimeState::Unknown);
    }

    #[test]
    fn several_rollouts_of_one_thread_collapse_to_the_newest() {
        let id = "01a07697-25db-7b01-ad8b-1aa4ce86c953";
        let home = home_with(
            &[
                (
                    "rollout-a.jsonl",
                    meta_line(id, "/old/path", "2026-09-16T10:00:00Z", "\"cli\""),
                ),
                (
                    "rollout-b.jsonl",
                    meta_line(id, "/new/path", "2026-09-16T12:00:00Z", "\"cli\""),
                ),
                (
                    "rollout-c.jsonl",
                    meta_line(id, "/mid/path", "2026-09-16T11:00:00Z", "\"cli\""),
                ),
            ],
            None,
        );
        let (sessions, _) = read_history(home.path(), None);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].cwd, PathBuf::from("/new/path"));
    }

    #[test]
    fn subagent_rollouts_are_not_listed_as_conversations() {
        let home = home_with(
            &[
                (
                    "rollout-user.jsonl",
                    meta_line(
                        "11111111-1111-1111-1111-111111111111",
                        "/a",
                        "2026-09-16T10:00:00Z",
                        "\"cli\"",
                    ),
                ),
                (
                    "rollout-guardian.jsonl",
                    meta_line(
                        "22222222-2222-2222-2222-222222222222",
                        "/b",
                        "2026-09-16T10:00:00Z",
                        r#"{"subagent":{"other":"guardian"}}"#,
                    ),
                ),
            ],
            None,
        );
        let (sessions, _) = read_history(home.path(), None);
        assert_eq!(sessions.len(), 1);
        assert!(sessions[0].key.native_session_id.starts_with("1111"));
    }

    #[test]
    fn a_broken_or_foreign_file_is_skipped_not_fatal() {
        let home = home_with(
            &[
                (
                    "rollout-ok.jsonl",
                    meta_line(
                        "33333333-3333-3333-3333-333333333333",
                        "/a",
                        "2026-09-16T10:00:00Z",
                        "\"cli\"",
                    ),
                ),
                ("rollout-truncated.jsonl", "{\"type\":\"session_m".to_string()),
                ("rollout-empty.jsonl", String::new()),
                (
                    "rollout-other.jsonl",
                    "{\"type\":\"event_msg\",\"payload\":{}}\n".to_string(),
                ),
                (
                    "not-a-rollout.jsonl",
                    meta_line(
                        "44444444-4444-4444-4444-444444444444",
                        "/c",
                        "2026-09-16T10:00:00Z",
                        "\"cli\"",
                    ),
                ),
            ],
            None,
        );
        let (sessions, _) = read_history(home.path(), None);
        assert_eq!(sessions.len(), 1);
        assert!(sessions[0].key.native_session_id.starts_with("3333"));
    }

    /// Codex keeps rollout files forever. The list is for work you might go
    /// back to, so old conversations are left out — and counted, because a
    /// reader that silently drops rows cannot be told from one that failed.
    #[test]
    fn conversations_older_than_the_window_are_left_out_and_counted() {
        let recent = days_ago(3);
        let stale = days_ago(200);

        let home = home_with(
            &[
                (
                    "rollout-recent.jsonl",
                    meta_line("11111111-1111-1111-1111-111111111111", "/a", &recent, "\"cli\""),
                ),
                (
                    "rollout-old-a.jsonl",
                    meta_line("22222222-2222-2222-2222-222222222222", "/b", &stale, "\"cli\""),
                ),
                (
                    "rollout-old-b.jsonl",
                    meta_line("33333333-3333-3333-3333-333333333333", "/c", &stale, "\"cli\""),
                ),
            ],
            None,
        );

        let cutoff = Some(Utc::now() - chrono::Duration::days(30));
        let (sessions, older) = read_history(home.path(), cutoff);
        assert_eq!(sessions.len(), 1);
        assert!(sessions[0].key.native_session_id.starts_with("1111"));
        assert_eq!(older, 2);

        // Without a cutoff nothing is dropped, which is what tests and a
        // widened window rely on.
        let (all, older) = read_history(home.path(), None);
        assert_eq!(all.len(), 3);
        assert_eq!(older, 0);
    }

    /// A conversation is judged by when it was last touched, not by when some
    /// earlier fork of it started.
    #[test]
    fn a_thread_touched_recently_survives_an_old_first_rollout() {
        let id = "44444444-4444-4444-4444-444444444444";
        let home = home_with(
            &[
                (
                    "rollout-first.jsonl",
                    meta_line(id, "/old", &days_ago(200), "\"cli\""),
                ),
                (
                    "rollout-latest.jsonl",
                    meta_line(id, "/new", &days_ago(2), "\"cli\""),
                ),
            ],
            None,
        );
        let (sessions, older) = read_history(home.path(), Some(Utc::now() - chrono::Duration::days(30)));
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].cwd, PathBuf::from("/new"));
        assert_eq!(older, 0);
    }

    #[tokio::test]
    async fn the_scope_note_says_what_was_left_out() {
        let home = home_with(
            &[
                (
                    "rollout-a.jsonl",
                    meta_line(
                        "55555555-5555-5555-5555-555555555555",
                        "/a",
                        &days_ago(1),
                        "\"cli\"",
                    ),
                ),
                (
                    "rollout-b.jsonl",
                    meta_line(
                        "66666666-6666-6666-6666-666666666666",
                        "/b",
                        &days_ago(300),
                        "\"cli\"",
                    ),
                ),
            ],
            None,
        );
        let a = CodexAdapter::with_binary("/usr/bin/true").with_home(home.path());
        let d = a.discover().await.unwrap();
        assert!(d.scope_note.contains("1 older not read"), "{}", d.scope_note);
        assert!(d.scope_note.contains("last 30d"), "{}", d.scope_note);
    }

    #[test]
    fn a_missing_codex_home_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_history(&dir.path().join("nope"), None).0.is_empty());
    }

    #[tokio::test]
    async fn start_hands_over_the_terminal_with_the_prompt_and_cwd() {
        let a = CodexAdapter::with_binary("/usr/bin/true");
        let outcome = a
            .start(StartRequest {
                operation_id: "op".into(),
                cwd: "/Users/me/src/api".into(),
                prompt: Some("  fix the flaky test  ".into()),
            })
            .await
            .unwrap();
        match outcome {
            StartOutcome::Interactive(plan) => {
                assert_eq!(plan.args, vec!["fix the flaky test"]);
                assert_eq!(plan.cwd.as_deref(), Some(Path::new("/Users/me/src/api")));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn start_without_a_prompt_opens_an_empty_session() {
        let a = CodexAdapter::with_binary("/usr/bin/true");
        let outcome = a
            .start(StartRequest {
                operation_id: "op".into(),
                cwd: "/tmp".into(),
                prompt: None,
            })
            .await
            .unwrap();
        match outcome {
            StartOutcome::Interactive(plan) => assert!(plan.args.is_empty()),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn resume_is_supported_but_attach_and_stop_are_not() {
        let a = CodexAdapter::with_binary("/usr/bin/true");
        let key = SessionKey::new(Provider::Codex, "01a07697-25db-7b01-ad8b-1aa4ce86c953");

        let plan = a.resume(&key).await.unwrap();
        assert_eq!(plan.args, vec!["resume", "01a07697-25db-7b01-ad8b-1aa4ce86c953"]);

        let caps = a.capabilities().await;
        assert!(caps.resume.is_supported());
        assert!(caps.discover.is_supported());
        assert!(!caps.attach.is_supported());
        assert!(!caps.stop.is_supported());

        // Never a silent success: both report why.
        assert!(a.attach(&key).await.unwrap_err().is_unsupported());
        assert!(a.stop(&key).await.unwrap_err().is_unsupported());
    }

    #[tokio::test]
    async fn delete_passes_the_uuid_and_skips_the_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("args");
        let fake = dir.path().join("codex");
        std::fs::write(&fake, format!("#!/bin/sh\necho \"$@\" > {}\n", log.display())).unwrap();
        let mut perms = std::fs::metadata(&fake).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&fake, perms).unwrap();

        let a = CodexAdapter::with_binary(&fake);
        assert!(a.capabilities().await.delete.is_supported());
        let id = "01a07697-25db-7b01-ad8b-1aa4ce86c953";
        a.delete(&SessionKey::new(Provider::Codex, id)).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(&log).unwrap().trim(),
            format!("delete --force {id}")
        );
    }

    #[tokio::test]
    async fn discovery_reads_the_configured_home() {
        let home = home_with(
            &[(
                "rollout-x.jsonl",
                meta_line(
                    "55555555-5555-5555-5555-555555555555",
                    "/w",
                    &days_ago(1),
                    "\"cli\"",
                ),
            )],
            None,
        );
        let a = CodexAdapter::with_binary("/usr/bin/true").with_home(home.path());
        let d = a.discover().await.unwrap();
        assert_eq!(d.sessions.len(), 1);
        assert!(d.scope_note.contains("App Server"));
    }
}
