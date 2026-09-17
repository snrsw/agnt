use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::link::{Link, LinkSource};
use super::state::{RuntimeState, WorkflowState};
use super::text;
use super::{Provider, SessionKey};

/// How agnt came to know about the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Started by agnt.
    Managed,
    /// Found live through the provider's official discovery.
    Discovered,
    /// Found in saved history only.
    #[default]
    History,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Managed => "managed",
            Origin::Discovered => "discovered",
            Origin::History => "history",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub observed_at: DateTime<Utc>,
}

/// Where the execution lives. Separate from the key so a restarted job or a
/// different server connection does not become a "new" conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuntimeHandle {
    ClaudeJob { job_id: String },
    Pid { pid: u32 },
    CodexThread { server: String, thread_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Runtime {
    #[serde(default)]
    pub state: RuntimeState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<RuntimeHandle>,
    /// What the state was derived from (provider field, event name, ...).
    #[serde(default)]
    pub evidence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<DateTime<Utc>>,
    /// Last non-Unknown state, kept when the current observation is stale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_known_state: Option<RuntimeState>,
}

impl Runtime {
    pub fn observed(state: RuntimeState, evidence: impl Into<String>, at: DateTime<Utc>) -> Self {
        Self {
            state,
            handle: None,
            evidence: evidence.into(),
            observed_at: Some(at),
            last_known_state: (state != RuntimeState::Unknown).then_some(state),
        }
    }

    /// Mark the observation stale: state becomes Unknown, the previous state
    /// is kept as `last_known_state`, and the observation time is preserved.
    pub fn mark_stale(&mut self, reason: impl Into<String>) {
        if self.state != RuntimeState::Unknown {
            self.last_known_state = Some(self.state);
        }
        self.state = RuntimeState::Unknown;
        self.evidence = reason.into();
    }
}

/// Provider-observed session. Persisted only in the regenerable cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub key: SessionKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_prompt: Option<String>,
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_git: Option<GitInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_git: Option<GitInfo>,
    #[serde(default)]
    pub runtime: Runtime,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    #[serde(default)]
    pub origin: Origin,
    /// Most recent activity reported by the provider; drives in-group order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<DateTime<Utc>>,
    /// Fields written by a newer agnt are kept as-is.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl Session {
    pub fn new(key: SessionKey, cwd: impl Into<PathBuf>) -> Self {
        Self {
            key,
            title: None,
            preview: None,
            first_prompt: None,
            cwd: cwd.into(),
            launch_git: None,
            observed_git: None,
            runtime: Runtime::default(),
            links: Vec::new(),
            origin: Origin::History,
            last_activity_at: None,
            extra: BTreeMap::new(),
        }
    }

    pub fn provider(&self) -> Provider {
        self.key.provider
    }

    /// Branch to show: the launch-time branch wins because a later HEAD change
    /// in a shared cwd does not tell which conversation moved.
    pub fn branch(&self) -> Option<&str> {
        self.launch_git
            .as_ref()
            .and_then(|g| g.branch.as_deref())
            .or_else(|| self.observed_git.as_ref().and_then(|g| g.branch.as_deref()))
    }
}

/// User-owned metadata. This is the only part agnt is the source of truth for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub key: SessionKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_override: Option<String>,
    #[serde(default)]
    pub workflow: WorkflowState,
    #[serde(default)]
    pub hidden_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl SessionMeta {
    pub fn new(key: SessionKey) -> Self {
        Self {
            key,
            title_override: None,
            workflow: WorkflowState::default(),
            hidden_at: None,
            links: Vec::new(),
            extra: BTreeMap::new(),
        }
    }

    pub fn is_hidden(&self) -> bool {
        self.hidden_at.is_some()
    }

    /// Add a manual link, replacing an existing entry with the same URL.
    pub fn upsert_link(&mut self, link: Link) {
        self.links.retain(|l| l.url != link.url);
        self.links.push(link);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleSource {
    Override,
    Provider,
    FirstPrompt,
    ShortId,
}

/// Join of provider observation and user metadata, for display and JSON output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionView {
    pub session: Session,
    pub meta: SessionMeta,
}

impl SessionView {
    pub const EXCERPT_CHARS: usize = 60;

    pub fn key(&self) -> &SessionKey {
        &self.session.key
    }

    pub fn is_hidden(&self) -> bool {
        self.meta.is_hidden()
    }

    /// Title priority from the design doc (section 9):
    /// manual override, provider title, opening prompt excerpt, short ID.
    pub fn title(&self) -> (String, TitleSource) {
        if let Some(t) = self
            .meta
            .title_override
            .as_deref()
            .filter(|t| !t.trim().is_empty())
        {
            return (text::sanitize(t), TitleSource::Override);
        }
        if let Some(t) = self.session.title.as_deref().filter(|t| !t.trim().is_empty()) {
            return (text::sanitize(t), TitleSource::Provider);
        }
        if let Some(p) = self
            .session
            .first_prompt
            .as_deref()
            .filter(|p| !p.trim().is_empty())
        {
            return (text::excerpt(p, Self::EXCERPT_CHARS), TitleSource::FirstPrompt);
        }
        (self.session.key.short_id(), TitleSource::ShortId)
    }

    /// State as shown in a list. A stale row names the state it last held, so
    /// the longest possible value is `Unknown (Waiting approval)`. Both the
    /// TUI and `agnt list` use this, so the two agree.
    pub fn state_label(&self) -> String {
        let r = &self.session.runtime;
        match (r.state, r.last_known_state) {
            (RuntimeState::Unknown, Some(prev)) => format!("{} ({})", r.state.label(), prev.label()),
            _ => r.state.label().to_string(),
        }
    }

    /// Sanitized preview, or the fixed "not fetched" marker.
    pub fn preview(&self) -> String {
        match self.session.preview.as_deref().filter(|p| !p.trim().is_empty()) {
            Some(p) => text::sanitize(p),
            None => "no content".to_string(),
        }
    }

    /// Manual links first, then provider, then inferred; duplicates by URL
    /// keep the higher-priority source.
    pub fn links(&self) -> Vec<Link> {
        let mut all: Vec<Link> = self
            .meta
            .links
            .iter()
            .cloned()
            .map(|mut l| {
                l.source = LinkSource::Manual;
                l
            })
            .collect();
        for l in &self.session.links {
            if !all.iter().any(|m| m.url == l.url) {
                all.push(l.clone());
            }
        }
        all.sort_by_key(|l| l.source);
        all
    }

    pub fn state(&self) -> RuntimeState {
        self.session.runtime.state
    }

    /// Repository this session's directory belongs to.
    pub fn repo(&self) -> String {
        repo_and_worktree(&self.session.cwd).0
    }

    /// The branch when git reported one, otherwise the worktree directory,
    /// which is what actually distinguishes two rows in the same repository.
    pub fn branch_or_worktree(&self) -> Option<String> {
        self.session
            .branch()
            .map(str::to_string)
            .or_else(|| repo_and_worktree(&self.session.cwd).1)
    }

    /// Age of the last activity the provider reported.
    pub fn age(&self, now: DateTime<Utc>) -> Option<String> {
        self.session.last_activity_at.map(|t| relative_age(t, now))
    }

    pub fn workflow(&self) -> WorkflowState {
        self.meta.workflow
    }

    /// True when the search text appears in the title, path, or a link.
    pub fn matches_query(&self, query: &str) -> bool {
        let q = query.to_lowercase();
        if q.is_empty() {
            return true;
        }
        self.title().0.to_lowercase().contains(&q)
            || self.session.cwd.to_string_lossy().to_lowercase().contains(&q)
            || self.session.key.native_session_id.to_lowercase().starts_with(&q)
            || self.links().iter().any(|l| {
                l.short_label().to_lowercase().contains(&q)
                    || l.number
                        .is_some_and(|n| n.to_string() == q.trim_start_matches('#'))
            })
    }
}

/// Split a working directory into the repository it belongs to and, when the
/// path is a worktree, the worktree's name.
///
/// Derived from the path rather than from git, so the list can group without
/// running a command per row. It recognises the two worktree layouts that
/// actually appear: `<repo>/.claude/worktrees/<name>` and
/// `<repo>/.worktrees/<name>`.
pub fn repo_and_worktree(cwd: &std::path::Path) -> (String, Option<String>) {
    let parts: Vec<String> = cwd
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();

    for (i, part) in parts.iter().enumerate() {
        let is_marker =
            (part == ".claude" && parts.get(i + 1).is_some_and(|w| w == "worktrees")) || part == ".worktrees";
        if !is_marker {
            continue;
        }
        let name_at = if part == ".worktrees" { i + 1 } else { i + 2 };
        if let (Some(repo), Some(name)) = (i.checked_sub(1).and_then(|j| parts.get(j)), parts.get(name_at)) {
            return (repo.clone(), Some(name.clone()));
        }
    }

    let repo = parts
        .last()
        .cloned()
        .unwrap_or_else(|| cwd.to_string_lossy().into_owned());
    (repo, None)
}

/// A short age such as `3m`, `2h`, `5d`. Sub-minute reads as `now`, and a time
/// in the future reads as `now` too rather than as a negative age.
pub fn relative_age(then: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - then).num_seconds();
    if secs < 60 {
        return "now".to_string();
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    let days = hours / 24;
    if days < 365 {
        return format!("{days}d");
    }
    format!("{}y", days / 365)
}

/// Default list order: state rank, then most recent activity first,
/// then key for a stable tiebreak.
pub fn sort_views(views: &mut [SessionView], by_workflow: bool) {
    views.sort_by(|a, b| {
        let rank_a = if by_workflow {
            a.workflow().sort_rank()
        } else {
            a.state().sort_rank()
        };
        let rank_b = if by_workflow {
            b.workflow().sort_rank()
        } else {
            b.state().sort_rank()
        };
        rank_a
            .cmp(&rank_b)
            .then_with(|| b.session.last_activity_at.cmp(&a.session.last_activity_at))
            .then_with(|| a.session.key.cmp(&b.session.key))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::LinkKind;

    fn view(id: &str) -> SessionView {
        let key = SessionKey::new(Provider::Claude, id);
        SessionView {
            session: Session::new(key.clone(), "/tmp/x"),
            meta: SessionMeta::new(key),
        }
    }

    #[test]
    fn title_priority_override_provider_prompt_id() {
        let mut v = view("abcdef123456");
        assert_eq!(v.title(), ("abcdef12".to_string(), TitleSource::ShortId));
        v.session.first_prompt = Some("fix the\tauth handler".into());
        assert_eq!(
            v.title(),
            ("fix the auth handler".to_string(), TitleSource::FirstPrompt)
        );
        v.session.title = Some("Auth fix".into());
        assert_eq!(v.title(), ("Auth fix".to_string(), TitleSource::Provider));
        v.meta.title_override = Some("Fix the auth handler".into());
        assert_eq!(
            v.title(),
            ("Fix the auth handler".to_string(), TitleSource::Override)
        );
        v.meta.title_override = Some("   ".into());
        assert_eq!(v.title().1, TitleSource::Provider);
    }

    #[test]
    fn preview_falls_back_to_marker() {
        let mut v = view("a");
        assert_eq!(v.preview(), "no content");
        v.session.preview = Some("x\x1b[0m y".into());
        assert_eq!(v.preview(), "x[0m y");
    }

    #[test]
    fn manual_links_win_over_provider_duplicates() {
        let mut v = view("a");
        let url = "https://github.com/o/r/pull/1";
        v.session
            .links
            .push(Link::from_url(url, LinkSource::Inferred).unwrap());
        v.session
            .links
            .push(Link::from_url("https://github.com/o/r/issues/2", LinkSource::Provider).unwrap());
        v.meta
            .upsert_link(Link::from_url(url, LinkSource::Manual).unwrap());
        let links = v.links();
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].source, LinkSource::Manual);
        assert_eq!(links[0].kind, LinkKind::PullRequest);
        assert_eq!(links[1].kind, LinkKind::Issue);
    }

    #[test]
    fn sort_puts_waiting_first_then_recent() {
        let now = Utc::now();
        let mut a = view("a");
        a.session.runtime.state = RuntimeState::Idle;
        a.session.last_activity_at = Some(now);
        let mut b = view("b");
        b.session.runtime.state = RuntimeState::WaitingApproval;
        let mut c = view("c");
        c.session.runtime.state = RuntimeState::Idle;
        c.session.last_activity_at = Some(now - chrono::Duration::minutes(5));
        let mut views = vec![c, a, b];
        sort_views(&mut views, false);
        let ids: Vec<_> = views.iter().map(|v| v.key().native_session_id.as_str()).collect();
        assert_eq!(ids, ["b", "a", "c"]);
    }

    #[test]
    fn query_matches_title_path_and_link_number() {
        let mut v = view("a");
        v.session.title = Some("Tidy up the README".into());
        v.session.cwd = "/Users/me/src/docs".into();
        v.meta
            .upsert_link(Link::from_url("https://github.com/o/r/pull/123", LinkSource::Manual).unwrap());
        assert!(v.matches_query("readme"));
        assert!(v.matches_query("src/docs"));
        assert!(v.matches_query("#123"));
        assert!(v.matches_query("pr #1"));
        assert!(!v.matches_query("codex"));
    }

    #[test]
    fn a_worktree_path_resolves_to_its_repository() {
        use std::path::Path;
        assert_eq!(
            repo_and_worktree(Path::new(
                "/Users/me/src/github.com/o/reed/.claude/worktrees/slo-dash"
            )),
            ("reed".to_string(), Some("slo-dash".to_string()))
        );
        assert_eq!(
            repo_and_worktree(Path::new("/Users/me/src/gh-own/.worktrees/sandbox")),
            ("gh-own".to_string(), Some("sandbox".to_string()))
        );
        assert_eq!(
            repo_and_worktree(Path::new("/Users/me/src/memo")),
            ("memo".to_string(), None)
        );
        // A .worktrees directory with nothing under it is not a worktree, and
        // must not panic or claim its parent as the repository.
        assert_eq!(
            repo_and_worktree(Path::new("/Users/me/src/thing/.worktrees")),
            (".worktrees".to_string(), None)
        );
    }

    #[test]
    fn relative_age_reads_in_the_largest_useful_unit() {
        let now = Utc::now();
        let ago = |d: chrono::Duration| relative_age(now - d, now);
        assert_eq!(ago(chrono::Duration::seconds(5)), "now");
        assert_eq!(ago(chrono::Duration::minutes(3)), "3m");
        assert_eq!(ago(chrono::Duration::hours(2)), "2h");
        assert_eq!(ago(chrono::Duration::days(33)), "33d");
        assert_eq!(ago(chrono::Duration::days(800)), "2y");
        // A clock skewed into the future reads as now, never a negative age.
        assert_eq!(relative_age(now + chrono::Duration::hours(1), now), "now");
    }

    #[test]
    fn stale_keeps_last_known_state() {
        let mut r = Runtime::observed(RuntimeState::Running, "job list", Utc::now());
        r.mark_stale("server disconnected");
        assert_eq!(r.state, RuntimeState::Unknown);
        assert_eq!(r.last_known_state, Some(RuntimeState::Running));
        assert!(r.observed_at.is_some());
    }

    #[test]
    fn unknown_fields_survive_round_trip() {
        let json = r#"{"key":{"provider":"codex","scope":"default","native_session_id":"x"},
            "workflow":"review","hidden_at":null,"future_field":{"a":1}}"#;
        let meta: SessionMeta = serde_json::from_str(json).unwrap();
        assert_eq!(meta.workflow, WorkflowState::Review);
        let back = serde_json::to_value(&meta).unwrap();
        assert_eq!(back["future_field"]["a"], 1);
    }
}
