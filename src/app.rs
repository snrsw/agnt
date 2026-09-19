//! In-memory state and the single event loop. Every UI state change goes
//! through `App`; provider calls run in background tasks and come back as
//! `AppEvent`s, so drawing never waits on I/O. Metadata writes are local
//! file operations and happen inline.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::model::{
    Link, LinkSource, Origin, Provider, Runtime, RuntimeState, Session, SessionKey, SessionMeta, SessionView,
    View, session::sort_views,
};
use crate::providers::{
    Adapter, AdapterError, AttachPlan, Discovery, RuntimeUpdate, StartOutcome, StartRequest, StopResult,
};
use crate::shell::{self, PtyShell};
use crate::storage::{CacheStore, MetadataStore, Paths};
use crate::terminal::{self, InputGate, TerminalGuard};
use crate::ui;

/// Results of background work, delivered to the event loop.
#[derive(Debug)]
pub enum AppEvent {
    Discovered {
        provider: Provider,
        result: Result<Discovery, AdapterError>,
    },
    Observed {
        provider: Provider,
        result: Result<Vec<RuntimeUpdate>, AdapterError>,
    },
    RefreshDone,
    Previewed {
        key: SessionKey,
        result: Result<Option<String>, AdapterError>,
    },
    Started {
        provider: Provider,
        cwd: PathBuf,
        result: Result<StartOutcome, AdapterError>,
    },
    Stopped {
        key: SessionKey,
        then_hide: bool,
        result: Result<StopResult, AdapterError>,
    },
    /// The embedded shell wrote to its screen. Carries nothing: the screen
    /// itself lives behind the shell's own lock, and this only says "redraw".
    ShellDrew,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderStatus {
    pub ok: bool,
    pub note: String,
    pub at: DateTime<Utc>,
}

/// What is taking keystrokes other than the always-live new-session prompt.
///
/// The prompt is not a mode: typing anywhere goes into it, after the agent
/// view this is modelled on. Everything here is an overlay that borrows the
/// keyboard until it is dismissed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Search,
    /// The full key list, opened with `?` on an empty prompt.
    Shortcuts,
    LinkInput {
        key: SessionKey,
        buffer: String,
    },
    /// `live` decides whether "stop first" is offered.
    HideDialog {
        key: SessionKey,
        live: bool,
    },
    /// Enter on a row whose state does not decide attach vs. resume.
    ChooseAction {
        key: SessionKey,
    },
    QuitConfirm,
    Message(String),
}

/// What the new-session bar is holding: the task text and the agent. The
/// directory it starts in is `App::shell_cwd`, moved with `cd` in shell mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSession {
    pub buffer: String,
    pub provider: Provider,
}

impl NewSession {
    pub fn new(provider: Provider) -> Self {
        Self {
            buffer: String::new(),
            provider,
        }
    }
}

/// What the one text area at the bottom is. Task mode makes it agnt's own
/// prompt, which starts sessions. Shell mode makes it the user's shell,
/// running on a pseudo-terminal — the same box, in the same place, with a
/// different thing in it.
///
/// In shell mode every key goes to the shell, which is the point: tab
/// completion, abbreviations and line editing are the shell's, not agnt's
/// imitation of them. The one key agnt keeps is Shift+Tab, to come back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputMode {
    #[default]
    Task,
    Shell,
}

/// Side effects requested by a key press; executed by the event loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Refresh,
    Attach(SessionKey),
    Resume(SessionKey),
    Stop {
        key: SessionKey,
        then_hide: bool,
    },
    Start {
        provider: Provider,
        request: StartRequest,
    },
    /// Start the embedded shell, sized to the text area it goes in.
    StartShell,
    /// Give the terminal to a command, then refresh. Used when a provider can
    /// only start interactively.
    RunPlan(Box<AttachPlan>),
    Quit,
}

/// A group heading. The tallies let the repository view say what is inside a
/// group without expanding it, which is the whole point of grouping by repo.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GroupHeader {
    pub label: String,
    pub count: usize,
    pub needs_you: usize,
    pub running: usize,
    pub idle: usize,
    pub resting: usize,
    /// True when every member is folded away, so the heading stands alone.
    pub collapsed: bool,
}

impl GroupHeader {
    fn tally(label: String, members: &[SessionView]) -> Self {
        let mut h = GroupHeader {
            label,
            count: members.len(),
            ..Default::default()
        };
        for v in members {
            match v.state() {
                RuntimeState::WaitingApproval | RuntimeState::WaitingInput => h.needs_you += 1,
                RuntimeState::Running | RuntimeState::Starting => h.running += 1,
                RuntimeState::Idle => h.idle += 1,
                _ => h.resting += 1,
            }
        }
        h
    }
}

/// A rendered list line. `SessionView` is boxed to keep the row enum small,
/// since a full list allocates one entry per session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListRow {
    Group(GroupHeader),
    Session(Box<SessionView>),
}

pub struct App {
    sessions: BTreeMap<SessionKey, Session>,
    metas: BTreeMap<SessionKey, SessionMeta>,
    /// Set when metadata.json is corrupt or newer; editing is disabled.
    pub metadata_error: Option<String>,
    pub provider_status: BTreeMap<Provider, ProviderStatus>,
    pub view: View,
    pub query: String,
    pub show_hidden: bool,
    /// Whether stopped sessions and saved history are listed. Off by default:
    /// they outnumber live work heavily, and `z` brings them back.
    pub show_history: bool,
    pub selected: Option<SessionKey>,
    pub mode: Mode,
    pub refreshing: bool,
    pub cache_saved_at: Option<DateTime<Utc>>,
    pub should_quit: bool,
    /// Keys a preview has already been requested for, so a slow or empty
    /// answer is not asked for again on every redraw.
    preview_requested: BTreeSet<SessionKey>,
    /// The new-session prompt, always live. Typing goes here in task mode.
    pub draft: NewSession,
    /// Whether the text area is taking a task or a `cd`.
    pub input_mode: InputMode,
    /// Where a new session starts. Follows the embedded shell.
    pub shell_cwd: PathBuf,
    /// When the shell's directory was last read, so a per-frame poll does
    /// not spawn `lsof` on every keystroke.
    last_cwd_poll: Option<std::time::Instant>,
    /// The embedded shell, once shell mode has been opened.
    pub pty: Option<PtyShell>,
    /// Why the shell could not be started, if it could not.
    pub notice: Option<String>,
    store: MetadataStore,
    cache: CacheStore,
}

impl App {
    /// Load metadata and the cached list. Cached states are shown as the
    /// previous observation (Unknown + last known), never as confirmed.
    pub fn load(paths: &Paths) -> Self {
        let store = MetadataStore::new(paths);
        let cache = CacheStore::new(paths);
        let (metas, metadata_error) = match store.load() {
            Ok(file) => (file.by_key(), None),
            Err(e) => (BTreeMap::new(), Some(e.to_string())),
        };
        let (mut sessions, cache_saved_at): (BTreeMap<SessionKey, Session>, _) = match cache.load() {
            Some(c) => (
                c.sessions.into_iter().map(|s| (s.key.clone(), s)).collect(),
                Some(c.saved_at),
            ),
            None => (BTreeMap::new(), None),
        };
        for s in sessions.values_mut() {
            s.runtime.mark_stale("previous observation (cache)");
        }
        let mut app = Self {
            sessions,
            metas,
            metadata_error,
            provider_status: BTreeMap::new(),
            view: View::default(),
            query: String::new(),
            show_hidden: false,
            show_history: false,
            selected: None,
            mode: Mode::Normal,
            refreshing: false,
            cache_saved_at,
            should_quit: false,
            preview_requested: BTreeSet::new(),
            draft: NewSession::new(Provider::Claude),
            input_mode: InputMode::default(),
            shell_cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            pty: None,
            last_cwd_poll: None,
            notice: None,
            store,
            cache,
        };
        app.ensure_selection();
        app
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    pub fn hidden_count(&self) -> usize {
        self.views().iter().filter(|v| v.is_hidden()).count()
    }

    /// Every known session joined with its metadata (default when absent).
    pub fn views(&self) -> Vec<SessionView> {
        self.sessions
            .values()
            .map(|s| SessionView {
                session: s.clone(),
                meta: self
                    .metas
                    .get(&s.key)
                    .cloned()
                    .unwrap_or_else(|| SessionMeta::new(s.key.clone())),
            })
            .collect()
    }

    /// Keys known from either the cache or metadata (for ID resolution).
    pub fn known_keys(&self) -> Vec<SessionKey> {
        let mut keys: BTreeSet<SessionKey> = self.sessions.keys().cloned().collect();
        keys.extend(self.metas.keys().cloned());
        keys.into_iter().collect()
    }

    pub fn meta_for(&self, key: &SessionKey) -> SessionMeta {
        self.metas
            .get(key)
            .cloned()
            .unwrap_or_else(|| SessionMeta::new(key.clone()))
    }

    /// Filtered, sorted rows with group headers. The hidden filter is applied
    /// here, after discovery, so a background refresh cannot resurrect a row.
    pub fn rows(&self) -> Vec<ListRow> {
        let mut views: Vec<SessionView> = self
            .views()
            .into_iter()
            .filter(|v| self.show_hidden || !v.is_hidden())
            .filter(|v| v.matches_query(&self.query))
            .collect();
        sort_views(&mut views, self.view == View::Workflow);
        if self.view == View::Repository {
            // Order repositories by what is in them, not by name: the repo
            // holding something that needs you comes first, then the most
            // recently active. Alphabetical order buries the repos you are
            // working in under whichever ones happen to sort early.
            let mut rank: BTreeMap<String, (u8, Option<DateTime<Utc>>)> = BTreeMap::new();
            for v in &views {
                let entry = rank
                    .entry(v.repo())
                    .or_insert((u8::MAX, v.session.last_activity_at));
                entry.0 = entry.0.min(v.state().sort_rank());
                entry.1 = entry.1.max(v.session.last_activity_at);
            }
            views.sort_by(|a, b| {
                let ra = rank.get(&a.repo()).copied().unwrap_or((u8::MAX, None));
                let rb = rank.get(&b.repo()).copied().unwrap_or((u8::MAX, None));
                ra.0.cmp(&rb.0)
                    .then_with(|| rb.1.cmp(&ra.1))
                    .then_with(|| a.repo().cmp(&b.repo()))
                    .then_with(|| a.state().sort_rank().cmp(&b.state().sort_rank()))
                    .then_with(|| b.session.last_activity_at.cmp(&a.session.last_activity_at))
                    .then_with(|| a.session.key.cmp(&b.session.key))
            });
        }

        // Group first so each header can tally what is inside it.
        let mut groups: Vec<(String, Vec<SessionView>)> = Vec::new();
        for v in views {
            let label = self.group_label(&v);
            match groups.last_mut() {
                Some((current, members)) if *current == label => members.push(v),
                _ => groups.push((label, vec![v])),
            }
        }

        let mut rows = Vec::new();
        for (label, members) in groups {
            // The heading tallies every member, folded or not, so a collapsed
            // group still says how much is inside it.
            let mut header = GroupHeader::tally(label, &members);
            let shown: Vec<SessionView> = if self.show_history {
                members
            } else {
                members.into_iter().filter(|v| !v.is_history()).collect()
            };
            header.collapsed = shown.is_empty();
            rows.push(ListRow::Group(header));
            rows.extend(shown.into_iter().map(|v| ListRow::Session(Box::new(v))));
        }
        rows
    }

    /// True when a provider's last fetch failed. Only then is it worth a line
    /// at the bottom of the screen; a working provider says nothing.
    pub fn has_provider_problem(&self) -> bool {
        self.provider_status.values().any(|s| !s.ok)
    }

    /// Counts for the header, in list order: how many need you, are working,
    /// and so on, with folded history last.
    pub fn state_summary(&self) -> Vec<(&'static str, usize)> {
        let mut live: BTreeMap<u8, (&'static str, usize)> = BTreeMap::new();
        let mut folded = 0usize;
        for v in self.views() {
            if !self.show_hidden && v.is_hidden() {
                continue;
            }
            if v.is_history() && !self.show_history {
                folded += 1;
                continue;
            }
            let state = v.state();
            let entry = live.entry(state.sort_rank()).or_insert((state.short_label(), 0));
            entry.1 += 1;
        }
        let mut out: Vec<(&'static str, usize)> = live.into_values().collect();
        if folded > 0 {
            out.push(("in history", folded));
        }
        out
    }

    /// Sessions currently listed, and how many are folded away behind `z`.
    pub fn shown_and_folded(&self) -> (usize, usize) {
        let visible: Vec<SessionView> = self
            .views()
            .into_iter()
            .filter(|v| self.show_hidden || !v.is_hidden())
            .filter(|v| v.matches_query(&self.query))
            .collect();
        let folded = if self.show_history {
            0
        } else {
            visible.iter().filter(|v| v.is_history()).count()
        };
        (visible.len() - folded, folded)
    }

    fn group_label(&self, v: &SessionView) -> String {
        match self.view {
            View::Status => match v.state() {
                RuntimeState::WaitingApproval | RuntimeState::WaitingInput => "Needs you".to_string(),
                s => s.label().to_string(),
            },
            View::Workflow => v.workflow().label().to_string(),
            View::Repository => v.repo(),
        }
    }

    pub fn session_keys_in_order(&self) -> Vec<SessionKey> {
        self.rows()
            .into_iter()
            .filter_map(|r| match r {
                ListRow::Session(v) => Some(v.session.key),
                ListRow::Group(_) => None,
            })
            .collect()
    }

    pub fn selected_view(&self) -> Option<SessionView> {
        let key = self.selected.as_ref()?;
        let session = self.sessions.get(key)?;
        Some(SessionView {
            session: session.clone(),
            meta: self.meta_for(key),
        })
    }

    /// Keep the selection on the same key when rows change; fall back to the
    /// first row when it disappeared.
    fn ensure_selection(&mut self) {
        let keys = self.session_keys_in_order();
        if let Some(sel) = &self.selected
            && keys.contains(sel)
        {
            return;
        }
        self.selected = keys.first().cloned();
    }

    /// Move the cursor, wrapping at both ends: past the last row is the first.
    /// A list you scroll through repeatedly should not need a key to get back
    /// to the top.
    fn move_selection(&mut self, delta: isize) {
        let keys = self.session_keys_in_order();
        if keys.is_empty() {
            self.selected = None;
            return;
        }
        let len = keys.len() as isize;
        let current = self
            .selected
            .as_ref()
            .and_then(|k| keys.iter().position(|x| x == k))
            .unwrap_or(0) as isize;
        let next = (current + delta).rem_euclid(len) as usize;
        self.selected = Some(keys[next].clone());
    }

    // ----- metadata edits -------------------------------------------------

    fn edit_meta(&mut self, key: &SessionKey, f: impl FnOnce(&mut SessionMeta)) -> Result<(), String> {
        if let Some(err) = &self.metadata_error {
            return Err(format!("cannot edit metadata: {err}"));
        }
        let meta = self
            .store
            .update(|file| {
                let entry = file.entry(key);
                f(entry);
                entry.clone()
            })
            .map_err(|e| e.to_string())?;
        self.metas.insert(key.clone(), meta);
        Ok(())
    }

    pub fn hide(&mut self, key: &SessionKey) -> Result<(), String> {
        let now = Utc::now();
        self.edit_meta(key, |m| m.hidden_at = Some(now))?;
        self.ensure_selection();
        Ok(())
    }

    pub fn restore(&mut self, key: &SessionKey) -> Result<(), String> {
        self.edit_meta(key, |m| m.hidden_at = None)?;
        self.ensure_selection();
        Ok(())
    }

    pub fn cycle_workflow(&mut self, key: &SessionKey) -> Result<(), String> {
        self.edit_meta(key, |m| m.workflow = m.workflow.next())?;
        self.ensure_selection();
        Ok(())
    }

    pub fn add_link(&mut self, key: &SessionKey, url: &str) -> Result<Link, String> {
        let link = Link::from_url(url, LinkSource::Manual).map_err(|e| e.to_string())?;
        let stored = link.clone();
        self.edit_meta(key, move |m| m.upsert_link(stored))?;
        Ok(link)
    }

    // ----- provider results -----------------------------------------------

    pub fn apply_discovery(&mut self, provider: Provider, result: Result<Discovery, AdapterError>) {
        let now = Utc::now();
        match result {
            Ok(d) => {
                let found: BTreeSet<SessionKey> = d.sessions.iter().map(|s| s.key.clone()).collect();
                for s in d.sessions {
                    self.sessions.insert(s.key.clone(), s);
                }
                let missing: Vec<SessionKey> = self
                    .sessions
                    .keys()
                    .filter(|k| k.provider == provider && !found.contains(k))
                    .cloned()
                    .collect();
                for k in missing {
                    if d.complete {
                        self.sessions.remove(&k);
                    } else if let Some(s) = self.sessions.get_mut(&k) {
                        s.runtime.mark_stale("not in the latest discovery result");
                    }
                }
                self.provider_status.insert(
                    provider,
                    ProviderStatus {
                        ok: true,
                        note: d.scope_note,
                        at: now,
                    },
                );
            }
            Err(e) => {
                // A fetch failure is not the session's error: keep rows, mark stale.
                for s in self.sessions.values_mut().filter(|s| s.key.provider == provider) {
                    s.runtime.mark_stale(format!("discovery failed: {e}"));
                }
                self.provider_status.insert(
                    provider,
                    ProviderStatus {
                        ok: false,
                        note: e.to_string(),
                        at: now,
                    },
                );
            }
        }
        self.ensure_selection();
    }

    /// Apply state updates, ignoring any older than the current observation.
    pub fn apply_updates(&mut self, provider: Provider, result: Result<Vec<RuntimeUpdate>, AdapterError>) {
        match result {
            Ok(updates) => {
                for u in updates {
                    if let Some(s) = self.sessions.get_mut(&u.key)
                        && !is_older(&u.runtime, &s.runtime)
                    {
                        s.runtime = u.runtime;
                    }
                }
            }
            Err(e) if e.is_unsupported() => {}
            Err(e) => {
                for s in self.sessions.values_mut().filter(|s| s.key.provider == provider) {
                    s.runtime.mark_stale(format!("state update failed: {e}"));
                }
                self.provider_status.insert(
                    provider,
                    ProviderStatus {
                        ok: false,
                        note: e.to_string(),
                        at: Utc::now(),
                    },
                );
            }
        }
    }

    /// Record a background start. An interactive start has no identity yet, so
    /// it is handled by the event loop instead and never reaches this.
    pub fn apply_started(
        &mut self,
        provider: Provider,
        cwd: PathBuf,
        result: Result<StartOutcome, AdapterError>,
    ) {
        match result {
            Ok(StartOutcome::Started(res)) => {
                let mut s = Session::new(res.key.clone(), cwd);
                s.origin = Origin::Managed;
                s.runtime = Runtime::observed(RuntimeState::Starting, "start requested", Utc::now());
                s.runtime.handle = res.handle;
                s.last_activity_at = Some(Utc::now());
                self.selected = Some(res.key.clone());
                self.sessions.insert(res.key, s);
                self.mode = Mode::Normal;
            }
            // The event loop runs the plan; there is no row to add until the
            // conversation exists and discovery finds it.
            Ok(StartOutcome::Interactive(_)) => {}
            Err(e) => {
                self.mode = Mode::Message(format!("{}: cannot start: {e}", provider.label()));
            }
        }
        self.ensure_selection();
    }

    pub fn apply_stopped(
        &mut self,
        key: SessionKey,
        then_hide: bool,
        result: Result<StopResult, AdapterError>,
    ) {
        match result {
            Ok(StopResult::Confirmed) => {
                if let Some(s) = self.sessions.get_mut(&key) {
                    s.runtime = Runtime::observed(RuntimeState::Stopped, "stop confirmed", Utc::now());
                }
                if then_hide && let Err(e) = self.hide(&key) {
                    self.mode = Mode::Message(e);
                }
            }
            Ok(StopResult::Requested { note }) => {
                // Not confirmed: never hide silently.
                self.mode = Mode::Message(format!("stop requested, not yet confirmed: {note}"));
            }
            Err(e) => {
                self.mode = Mode::Message(format!("cannot stop: {e}"));
            }
        }
        self.ensure_selection();
    }

    /// The directory the next session starts in.
    pub fn new_session_cwd(&self) -> PathBuf {
        self.shell_cwd.clone()
    }

    // ----- shell mode ----------------------------------------------------

    /// Every key in shell mode belongs to the shell, except the one that
    /// leaves. Returning `None` means the key was not handled here, which
    /// only happens when there is no shell running yet.
    fn handle_shell_key(&mut self, key: KeyEvent) -> Option<Vec<Effect>> {
        let pty = self.pty.as_mut()?;
        if let Some(bytes) = shell::encode_key(key) {
            pty.send(&bytes);
        }
        Some(Vec::new())
    }

    /// Follow the shell into whatever directory it is sitting in, so a new
    /// session starts where the user navigated to. Read from the operating
    /// system, so it costs nothing at the prompt and works for any shell.
    pub fn sync_shell_cwd(&mut self) {
        if let Some(cwd) = self.pty.as_ref().and_then(PtyShell::cwd)
            && cwd.is_dir()
        {
            self.shell_cwd = cwd;
        }
    }

    /// Start the shell, or record why it could not start.
    pub fn open_shell(
        &mut self,
        program: &Path,
        rows: u16,
        cols: u16,
        on_output: impl Fn() + Send + 'static,
    ) {
        if self.pty.is_some() {
            return;
        }
        match PtyShell::spawn(program, &self.shell_cwd, rows.max(1), cols.max(1), on_output) {
            Ok(pty) => {
                self.pty = Some(pty);
                self.notice = None;
            }
            Err(e) => {
                self.notice = Some(format!("could not start {}: {e}", program.display()));
                self.input_mode = InputMode::Task;
            }
        }
    }

    /// How often the shell's directory is read while it is alive. Reading it
    /// costs a process on macOS, so it is not done on every keystroke.
    const CWD_POLL: std::time::Duration = std::time::Duration::from_millis(150);

    /// Follow the shell while it is running, not only when it is left.
    ///
    /// A shell that exits takes its directory with it — the kernel has
    /// nothing left to answer about a process that is gone — so the last
    /// directory has to have been observed before `exit`. Polling is
    /// therefore the only option, and it is not exact: `cd elsewhere && exit`
    /// on one line never exists long enough to be seen, and starts the next
    /// session where the shell was before it.
    pub fn poll_shell_cwd(&mut self) {
        if self.pty.is_none() {
            return;
        }
        let now = std::time::Instant::now();
        if self.last_cwd_poll.is_some_and(|t| now - t < Self::CWD_POLL) {
            return;
        }
        self.last_cwd_poll = Some(now);
        self.sync_shell_cwd();
    }

    /// Tell the shell how big the text area is, so a resized window reflows
    /// the shell's own output rather than leaving it wrapped for the old
    /// width.
    pub fn fit_shell(&mut self, rows: u16, cols: u16) {
        if let Some(pty) = self.pty.as_mut() {
            pty.resize(rows, cols);
        }
    }

    /// Drop the shell when it has exited, so `exit` gives the text area back
    /// to the task prompt rather than leaving a dead screen in it.
    pub fn reap_shell(&mut self) {
        if self.pty.as_mut().is_some_and(PtyShell::has_exited) {
            // No sync here: the process is already gone, so there is nothing
            // left to ask. `poll_shell_cwd` is what kept up with it.
            self.pty = None;
            self.last_cwd_poll = None;
            self.input_mode = InputMode::Task;
        }
    }

    /// The selected session's key, when its short content has not been
    /// fetched yet. Returns it once per key so drawing never queues repeats.
    pub fn take_preview_request(&mut self) -> Option<SessionKey> {
        let key = self.selected.clone()?;
        let session = self.sessions.get(&key)?;
        if session.preview.is_some() || self.preview_requested.contains(&key) {
            return None;
        }
        self.preview_requested.insert(key.clone());
        Some(key)
    }

    pub fn save_cache(&self) -> Result<(), crate::storage::StorageError> {
        let rows: Vec<Session> = self.sessions.values().cloned().collect();
        self.cache.save(&rows)
    }

    /// What quitting the list does to running sessions, for the quit dialog.
    pub fn quit_note(&self) -> String {
        let managed = self
            .sessions
            .values()
            .filter(|s| s.origin == Origin::Managed && s.runtime.state.is_live())
            .count();
        if managed == 0 {
            "No running session was started by agnt. Work inside each tool is unaffected.".to_string()
        } else {
            format!(
                "agnt started {managed} session(s). Whether the runtime keeps them alive is undecided until P0 (docs/design.md §7)."
            )
        }
    }

    // ----- input ----------------------------------------------------------

    pub fn handle_event(&mut self, event: Event) -> Vec<Effect> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.handle_key(key),
            _ => Vec::new(),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return vec![Effect::Quit];
        }
        let mode = self.mode.clone();
        match mode {
            Mode::Normal => self.handle_normal(key),
            Mode::Shortcuts => {
                self.mode = Mode::Normal;
                Vec::new()
            }
            Mode::Search => {
                match key.code {
                    KeyCode::Esc => {
                        self.query.clear();
                        self.mode = Mode::Normal;
                    }
                    KeyCode::Enter => self.mode = Mode::Normal,
                    KeyCode::Backspace => {
                        self.query.pop();
                    }
                    KeyCode::Char('u') if ctrl => self.query.clear(),
                    KeyCode::Char(c) if !ctrl => self.query.push(c),
                    _ => {}
                }
                self.ensure_selection();
                Vec::new()
            }
            Mode::LinkInput {
                key: target,
                mut buffer,
            } => match key.code {
                KeyCode::Esc => {
                    self.mode = Mode::Normal;
                    Vec::new()
                }
                KeyCode::Enter => {
                    self.mode = match self.add_link(&target, &buffer) {
                        Ok(_) => Mode::Normal,
                        Err(e) => Mode::Message(e),
                    };
                    Vec::new()
                }
                KeyCode::Backspace => {
                    buffer.pop();
                    self.mode = Mode::LinkInput { key: target, buffer };
                    Vec::new()
                }
                KeyCode::Char(c) if !ctrl => {
                    buffer.push(c);
                    self.mode = Mode::LinkInput { key: target, buffer };
                    Vec::new()
                }
                _ => Vec::new(),
            },
            Mode::HideDialog { key: target, live } => match (key.code, live) {
                (KeyCode::Esc, _) => {
                    self.mode = Mode::Normal;
                    Vec::new()
                }
                (KeyCode::Char('c'), true) | (KeyCode::Char('y'), false) | (KeyCode::Enter, false) => {
                    self.mode = match self.hide(&target) {
                        Ok(()) => Mode::Normal,
                        Err(e) => Mode::Message(e),
                    };
                    Vec::new()
                }
                (KeyCode::Char('s'), true) => {
                    self.mode = Mode::Normal;
                    vec![Effect::Stop {
                        key: target,
                        then_hide: true,
                    }]
                }
                _ => Vec::new(),
            },
            Mode::ChooseAction { key: target } => match key.code {
                KeyCode::Char('a') => {
                    self.mode = Mode::Normal;
                    vec![Effect::Attach(target)]
                }
                KeyCode::Char('r') => {
                    self.mode = Mode::Normal;
                    vec![Effect::Resume(target)]
                }
                KeyCode::Esc => {
                    self.mode = Mode::Normal;
                    Vec::new()
                }
                _ => Vec::new(),
            },
            Mode::QuitConfirm => match key.code {
                KeyCode::Enter | KeyCode::Char('y') => {
                    self.should_quit = true;
                    vec![Effect::Quit]
                }
                _ => {
                    self.mode = Mode::Normal;
                    Vec::new()
                }
            },
            Mode::Message(_) => {
                self.mode = Mode::Normal;
                Vec::new()
            }
        }
    }

    /// Keys while the list is in front.
    ///
    /// Printable characters go to the new-session prompt, so every action
    /// needs a modifier. The line-editing chords a text field owes its user —
    /// Ctrl+U and Ctrl+W — stay with the text; the rest are actions.
    fn handle_normal(&mut self, key: KeyEvent) -> Vec<Effect> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // Shift+Tab swaps agnt's prompt for the user's shell and back. The
        // task text stays where it was, so a trip through the shell to find
        // the right directory never costs a half-typed task.
        if key.code == KeyCode::BackTab {
            self.input_mode = match self.input_mode {
                InputMode::Task => InputMode::Shell,
                InputMode::Shell => InputMode::Task,
            };
            if self.input_mode == InputMode::Shell {
                // Starting a shell is expensive, so it waits until shell mode
                // is actually opened; after that the same one is kept, which
                // is what makes its history and its directory persist.
                if self.pty.is_none() {
                    return vec![Effect::StartShell];
                }
            } else {
                // Leaving: take the directory the user navigated to with them.
                self.sync_shell_cwd();
            }
            return Vec::new();
        }
        if self.input_mode == InputMode::Shell
            && let Some(effects) = self.handle_shell_key(key)
        {
            return effects;
        }

        match key.code {
            // Enter acts on the prompt when it has something, and on the
            // selected row when it does not.
            KeyCode::Enter => {
                if !self.draft.buffer.trim().is_empty() {
                    let draft = self.draft.clone();
                    let effects = self.submit_new_session(&draft);
                    if !effects.is_empty() {
                        self.draft.buffer.clear();
                    }
                    return effects;
                }
                return self.open_selected();
            }
            KeyCode::Down => self.move_selection(1),
            KeyCode::Up => self.move_selection(-1),

            // The new session's agent.
            KeyCode::Tab => {
                self.draft.provider = match self.draft.provider {
                    Provider::Claude => Provider::Codex,
                    Provider::Codex => Provider::Claude,
                };
            }

            // Editing the prompt.
            KeyCode::Backspace => {
                self.draft.buffer.pop();
            }
            KeyCode::Char('u') if ctrl => self.draft.buffer.clear(),
            KeyCode::Char('w') if ctrl => {
                let trimmed = self.draft.buffer.trim_end();
                let cut = trimmed.rfind(' ').map(|i| i + 1).unwrap_or(0);
                self.draft.buffer.truncate(cut);
            }
            KeyCode::Esc => self.draft.buffer.clear(),

            // Actions.
            KeyCode::Char('t') if ctrl => {
                self.view = self.view.next();
                self.ensure_selection();
            }
            KeyCode::Char('f') if ctrl => self.mode = Mode::Search,
            KeyCode::Char('r') if ctrl => return vec![Effect::Refresh],
            KeyCode::Char('o') if ctrl => {
                self.show_history = !self.show_history;
                self.ensure_selection();
            }
            KeyCode::Char('y') if ctrl => {
                self.show_hidden = !self.show_hidden;
                self.ensure_selection();
            }
            KeyCode::Char('x') if ctrl => {
                if let Some(v) = self.selected_view() {
                    let k = v.session.key.clone();
                    if v.is_hidden() {
                        if let Err(e) = self.restore(&k) {
                            self.mode = Mode::Message(e);
                        }
                    } else {
                        self.mode = Mode::HideDialog {
                            key: k,
                            live: v.state().is_live(),
                        };
                    }
                }
            }
            KeyCode::Char('g') if ctrl => {
                if let Some(k) = self.selected.clone()
                    && let Err(e) = self.cycle_workflow(&k)
                {
                    self.mode = Mode::Message(e);
                }
            }
            KeyCode::Char('l') if ctrl => {
                if let Some(k) = self.selected.clone() {
                    self.mode = Mode::LinkInput {
                        key: k,
                        buffer: String::new(),
                    };
                }
            }

            // `?` is a shortcut only when it would otherwise start a sentence.
            KeyCode::Char('?') if self.draft.buffer.is_empty() => self.mode = Mode::Shortcuts,
            KeyCode::Char(c) if !ctrl => self.draft.buffer.push(c),
            _ => {}
        }
        Vec::new()
    }

    /// Enter on a row: attach when it is running, resume when it has stopped,
    /// and ask when the state does not decide.
    fn open_selected(&mut self) -> Vec<Effect> {
        let Some(v) = self.selected_view() else {
            return Vec::new();
        };
        let k = v.session.key.clone();
        match v.state() {
            s if s.is_live() => vec![Effect::Attach(k)],
            RuntimeState::Stopped => vec![Effect::Resume(k)],
            _ => {
                self.mode = Mode::ChooseAction { key: k };
                Vec::new()
            }
        }
    }

    /// Turn the bar's contents into a start request. An empty task is refused
    /// here rather than sent to a provider that would reject it.
    fn submit_new_session(&mut self, draft: &NewSession) -> Vec<Effect> {
        let prompt = draft.buffer.trim().to_string();
        if prompt.is_empty() {
            self.mode = Mode::Message("Type what the session should do, then press Enter.".into());
            return Vec::new();
        }
        let cwd = self.new_session_cwd();
        if !cwd.is_dir() {
            self.mode = Mode::Message(format!("cwd does not exist: {}", cwd.display()));
            return Vec::new();
        }
        self.mode = Mode::Normal;
        vec![Effect::Start {
            provider: draft.provider,
            request: StartRequest {
                operation_id: new_operation_id(draft.provider),
                cwd,
                prompt: Some(prompt),
            },
        }]
    }

    /// Apply an event and return any work it asks the event loop to do.
    /// A provider that starts interactively needs the terminal, which only the
    /// loop owns, so that comes back as an effect rather than being applied.
    pub fn absorb(&mut self, event: AppEvent) -> Vec<Effect> {
        if let AppEvent::Started {
            result: Ok(StartOutcome::Interactive(plan)),
            ..
        } = &event
        {
            let plan = plan.clone();
            self.mode = Mode::Normal;
            return vec![Effect::RunPlan(Box::new(plan))];
        }
        self.handle_app_event(event);
        Vec::new()
    }

    pub fn handle_app_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Discovered { provider, result } => {
                self.apply_discovery(provider, result);
                let _ = self.save_cache();
            }
            AppEvent::Observed { provider, result } => self.apply_updates(provider, result),
            AppEvent::RefreshDone => self.refreshing = false,
            AppEvent::Previewed { key, result } => {
                // A failed or unsupported preview leaves the row alone; the
                // detail pane keeps saying the content was not fetched.
                if let Ok(Some(text)) = result
                    && let Some(s) = self.sessions.get_mut(&key)
                {
                    s.preview = Some(text);
                }
            }
            AppEvent::Started {
                provider,
                cwd,
                result,
            } => self.apply_started(provider, cwd, result),
            AppEvent::Stopped {
                key,
                then_hide,
                result,
            } => self.apply_stopped(key, then_hide, result),
            // The shell drew; the next frame picks the screen up.
            AppEvent::ShellDrew => {}
        }
    }
}

fn is_older(candidate: &Runtime, current: &Runtime) -> bool {
    match (candidate.observed_at, current.observed_at) {
        (Some(new), Some(cur)) => new < cur,
        _ => false,
    }
}

pub fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    if path == "~"
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home);
    }
    PathBuf::from(path)
}

/// Unique per launch attempt so a timeout can be reconciled rather than retried.
pub fn new_operation_id(provider: Provider) -> String {
    format!(
        "{}-{}-{}",
        provider,
        Utc::now().format("%Y%m%dT%H%M%S%.6f"),
        std::process::id()
    )
}

// ----- event loop ---------------------------------------------------------

fn adapter_for(adapters: &[Arc<dyn Adapter>], provider: Provider) -> Option<Arc<dyn Adapter>> {
    adapters.iter().find(|a| a.provider() == provider).cloned()
}

fn spawn_refresh(app: &mut App, adapters: &[Arc<dyn Adapter>], tx: mpsc::Sender<AppEvent>) {
    if app.refreshing {
        return;
    }
    app.refreshing = true;
    let keys_by_provider: BTreeMap<Provider, Vec<SessionKey>> =
        app.sessions.keys().fold(BTreeMap::new(), |mut m, k| {
            m.entry(k.provider).or_default().push(k.clone());
            m
        });
    let adapters: Vec<Arc<dyn Adapter>> = adapters.to_vec();
    tokio::spawn(async move {
        let mut set = JoinSet::new();
        for adapter in adapters {
            let tx = tx.clone();
            let keys = keys_by_provider
                .get(&adapter.provider())
                .cloned()
                .unwrap_or_default();
            set.spawn(async move {
                let provider = adapter.provider();
                let result = adapter.discover().await;
                let _ = tx.send(AppEvent::Discovered { provider, result }).await;
                if !keys.is_empty() {
                    let result = adapter.observe(&keys).await;
                    let _ = tx.send(AppEvent::Observed { provider, result }).await;
                }
            });
        }
        while set.join_next().await.is_some() {}
        let _ = tx.send(AppEvent::RefreshDone).await;
    });
}

/// Run the interactive list until the user quits.
pub async fn run_tui(paths: Paths, adapters: Vec<Arc<dyn Adapter>>) -> anyhow::Result<()> {
    let mut app = App::load(&paths);
    let (ev_tx, mut ev_rx) = mpsc::channel::<AppEvent>(64);
    let (in_tx, mut in_rx) = mpsc::channel::<Event>(64);
    let gate = Arc::new(InputGate::default());

    let mut guard = TerminalGuard::enter()?;
    let _reader = terminal::spawn_input_reader(gate.clone(), in_tx);
    spawn_refresh(&mut app, &adapters, ev_tx.clone());

    let mut effects: Vec<Effect> = Vec::new();
    loop {
        // Both of these are properties of the frame about to be drawn: the
        // text area is sized by the window, and a shell the user typed `exit`
        // into must not be drawn as if it were still there.
        app.poll_shell_cwd();
        app.reap_shell();
        let (rows, cols) = ui::shell_area_size(guard.terminal().size()?.into());
        app.fit_shell(rows, cols);

        guard.terminal().draw(|f| ui::draw(f, &app))?;

        if let Some(key) = app.take_preview_request()
            && let Some(adapter) = adapter_for(&adapters, key.provider)
        {
            let tx = ev_tx.clone();
            tokio::spawn(async move {
                let result = adapter.preview(&key, SessionView::EXCERPT_CHARS * 4).await;
                let _ = tx.send(AppEvent::Previewed { key, result }).await;
            });
        }

        tokio::select! {
            Some(ev) = in_rx.recv() => {
                effects.extend(app.handle_event(ev));
                while let Ok(ev) = in_rx.try_recv() {
                    effects.extend(app.handle_event(ev));
                }
            }
            Some(ev) = ev_rx.recv() => {
                effects.extend(app.absorb(ev));
                while let Ok(ev) = ev_rx.try_recv() {
                    effects.extend(app.absorb(ev));
                }
            }
            else => break,
        }

        for effect in effects.drain(..) {
            match effect {
                Effect::Quit => app.should_quit = true,
                Effect::Refresh => spawn_refresh(&mut app, &adapters, ev_tx.clone()),
                // The effect decides between attach and resume. Re-deriving it
                // here from the row's state would ignore what the user picked
                // in the ChooseAction dialog.
                effect @ (Effect::Attach(_) | Effect::Resume(_)) => {
                    let (key, attaching) = match effect {
                        Effect::Attach(key) => (key, true),
                        Effect::Resume(key) => (key, false),
                        _ => unreachable!("outer pattern admits only these two"),
                    };
                    let Some(adapter) = adapter_for(&adapters, key.provider) else {
                        app.mode = Mode::Message(format!("no adapter for {}", key.provider.label()));
                        continue;
                    };
                    let plan = if attaching {
                        adapter.attach(&key).await
                    } else {
                        adapter.resume(&key).await
                    };
                    match plan {
                        Ok(plan) => {
                            if !plan.note.is_empty() {
                                app.mode = Mode::Message(plan.note.clone());
                            }
                            let status = terminal::handoff(&mut guard, &gate, &plan).await;
                            app.mode = match status {
                                Ok(s) if s.success() => {
                                    Mode::Message("Back in the list. Refreshing state.".into())
                                }
                                Ok(s) => Mode::Message(format!(
                                    "{} exited with {s}. Refreshing state.",
                                    plan.program.display()
                                )),
                                Err(e) => Mode::Message(format!("could not run it: {e}")),
                            };
                            spawn_refresh(&mut app, &adapters, ev_tx.clone());
                        }
                        Err(e) => app.mode = Mode::Message(e.to_string()),
                    }
                }
                Effect::Stop { key, then_hide } => {
                    let Some(adapter) = adapter_for(&adapters, key.provider) else {
                        continue;
                    };
                    let tx = ev_tx.clone();
                    tokio::spawn(async move {
                        let result = adapter.stop(&key).await;
                        let _ = tx
                            .send(AppEvent::Stopped {
                                key,
                                then_hide,
                                result,
                            })
                            .await;
                    });
                }
                Effect::RunPlan(plan) => {
                    app.mode = Mode::Message(plan.note.clone());
                    let status = terminal::handoff(&mut guard, &gate, &plan).await;
                    app.mode = match status {
                        Ok(s) if s.success() => Mode::Message("Back in the list. Refreshing.".into()),
                        Ok(s) => Mode::Message(format!("{} exited with {s}.", plan.program.display())),
                        Err(e) => Mode::Message(format!("could not run it: {e}")),
                    };
                    spawn_refresh(&mut app, &adapters, ev_tx.clone());
                }
                Effect::StartShell => {
                    let tx = ev_tx.clone();
                    let (rows, cols) = ui::shell_area_size(guard.terminal().size()?.into());
                    app.open_shell(&shell::program(), rows, cols, move || {
                        // A blocking send would deadlock the reader thread
                        // against a full channel; a dropped redraw is
                        // harmless, because the next one carries the screen.
                        let _ = tx.try_send(AppEvent::ShellDrew);
                    });
                }
                Effect::Start {
                    provider,
                    request: req,
                } => {
                    let Some(adapter) = adapter_for(&adapters, provider) else {
                        continue;
                    };
                    let tx = ev_tx.clone();
                    let cwd = req.cwd.clone();
                    tokio::spawn(async move {
                        let result = adapter.start(req).await;
                        let _ = tx
                            .send(AppEvent::Started {
                                provider,
                                cwd,
                                result,
                            })
                            .await;
                    });
                }
            }
        }
        if app.should_quit {
            break;
        }
    }

    let saved = app.save_cache();
    drop(guard);
    saved?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Operation, View, WorkflowState};

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn code(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    /// A session as discovery produces one. `Origin::Discovered` matters:
    /// a session the provider still lists is not history even when its state
    /// is Unknown, and the list folds on that distinction.
    fn session(provider: Provider, id: &str, state: RuntimeState) -> Session {
        let mut s = Session::new(SessionKey::new(provider, id), "/tmp/proj");
        s.runtime = Runtime::observed(state, "test", Utc::now());
        s.origin = Origin::Discovered;
        s
    }

    fn app_with(dir: &tempfile::TempDir, sessions: Vec<Session>) -> App {
        let paths = Paths::at(dir.path());
        CacheStore::new(&paths).save(&sessions).unwrap();
        App::load(&paths)
    }

    fn discovery(sessions: Vec<Session>, complete: bool) -> Discovery {
        Discovery {
            sessions,
            scope_note: "test".into(),
            complete,
        }
    }

    #[test]
    fn cached_rows_load_as_previous_observation() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with(&dir, vec![session(Provider::Claude, "a", RuntimeState::Running)]);
        let v = app.selected_view().unwrap();
        assert_eq!(v.state(), RuntimeState::Unknown);
        assert_eq!(v.session.runtime.last_known_state, Some(RuntimeState::Running));
    }

    #[test]
    fn hidden_row_stays_hidden_after_rediscovery() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![session(Provider::Codex, "a", RuntimeState::Idle)]);
        let k = SessionKey::new(Provider::Codex, "a");
        app.hide(&k).unwrap();
        assert!(app.session_keys_in_order().is_empty());
        app.apply_discovery(
            Provider::Codex,
            Ok(discovery(
                vec![session(Provider::Codex, "a", RuntimeState::Running)],
                true,
            )),
        );
        assert!(app.session_keys_in_order().is_empty());
        app.show_hidden = true;
        assert_eq!(app.session_keys_in_order(), vec![k.clone()]);
        app.restore(&k).unwrap();
        app.show_hidden = false;
        assert_eq!(app.session_keys_in_order(), vec![k]);
    }

    #[test]
    fn discovery_failure_keeps_rows_and_marks_stale() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        app.apply_discovery(
            Provider::Claude,
            Ok(discovery(
                vec![session(Provider::Claude, "a", RuntimeState::Running)],
                true,
            )),
        );
        assert_eq!(app.selected_view().unwrap().state(), RuntimeState::Running);
        app.apply_discovery(
            Provider::Claude,
            Err(AdapterError::Timeout {
                command: "claude".into(),
                timeout_ms: 1,
            }),
        );
        let v = app.selected_view().unwrap();
        assert_eq!(v.state(), RuntimeState::Unknown);
        assert_eq!(v.session.runtime.last_known_state, Some(RuntimeState::Running));
        assert!(!app.provider_status[&Provider::Claude].ok);
    }

    #[test]
    fn incomplete_discovery_keeps_unseen_rows_complete_drops_them() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(
            &dir,
            vec![
                session(Provider::Claude, "a", RuntimeState::Idle),
                session(Provider::Codex, "c", RuntimeState::Idle),
            ],
        );
        app.apply_discovery(
            Provider::Claude,
            Ok(discovery(
                vec![session(Provider::Claude, "b", RuntimeState::Idle)],
                false,
            )),
        );
        assert_eq!(app.session_count(), 3);
        app.apply_discovery(
            Provider::Claude,
            Ok(discovery(
                vec![session(Provider::Claude, "b", RuntimeState::Idle)],
                true,
            )),
        );
        assert_eq!(app.session_count(), 2);
        assert!(app.sessions.contains_key(&SessionKey::new(Provider::Codex, "c")));
    }

    #[test]
    fn older_observation_does_not_overwrite_newer() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        let now = Utc::now();
        let mut s = session(Provider::Codex, "a", RuntimeState::WaitingApproval);
        s.runtime.observed_at = Some(now);
        app.apply_discovery(Provider::Codex, Ok(discovery(vec![s], true)));
        let old = RuntimeUpdate {
            key: SessionKey::new(Provider::Codex, "a"),
            runtime: Runtime::observed(
                RuntimeState::Idle,
                "stale event",
                now - chrono::Duration::seconds(30),
            ),
        };
        app.apply_updates(Provider::Codex, Ok(vec![old]));
        assert_eq!(
            app.selected_view().unwrap().state(),
            RuntimeState::WaitingApproval
        );
    }

    #[test]
    fn workflow_cycles_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![session(Provider::Claude, "a", RuntimeState::Idle)]);
        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
        assert_eq!(app.selected_view().unwrap().workflow(), WorkflowState::InProgress);
        let reloaded = App::load(&Paths::at(dir.path()));
        assert_eq!(
            reloaded.selected_view().unwrap().workflow(),
            WorkflowState::InProgress
        );
    }

    #[test]
    fn corrupt_metadata_disables_editing_but_keeps_list() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(Paths::at(dir.path()).metadata_json(), b"{oops").unwrap();
        let mut app = app_with(&dir, vec![session(Provider::Claude, "a", RuntimeState::Idle)]);
        assert!(app.metadata_error.is_some());
        assert_eq!(app.session_keys_in_order().len(), 1);
        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
        assert!(matches!(app.mode, Mode::Message(ref m) if m.contains("cannot edit metadata")));
        assert_eq!(
            std::fs::read(Paths::at(dir.path()).metadata_json()).unwrap(),
            b"{oops"
        );
    }

    #[test]
    fn hide_dialog_offers_stop_only_for_live_rows() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        app.apply_discovery(
            Provider::Claude,
            Ok(discovery(
                vec![session(Provider::Claude, "live", RuntimeState::Running)],
                true,
            )),
        );
        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
        assert!(matches!(app.mode, Mode::HideDialog { live: true, .. }));
        // 'y' is not an option for live rows; 's' requests stop-then-hide.
        assert!(app.handle_key(key('y')).is_empty());
        let effects = app.handle_key(key('s'));
        assert!(matches!(
            effects.as_slice(),
            [Effect::Stop { then_hide: true, .. }]
        ));
        assert!(!app.selected_view().unwrap().is_hidden());
    }

    #[test]
    fn unconfirmed_stop_does_not_hide() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![session(Provider::Claude, "a", RuntimeState::Running)]);
        let k = SessionKey::new(Provider::Claude, "a");
        app.apply_stopped(
            k.clone(),
            true,
            Ok(StopResult::Requested { note: "async".into() }),
        );
        assert!(!app.meta_for(&k).is_hidden());
        app.apply_stopped(
            k.clone(),
            true,
            Err(AdapterError::Unsupported {
                provider: Provider::Claude,
                operation: Operation::Stop,
                reason: "p0".into(),
            }),
        );
        assert!(!app.meta_for(&k).is_hidden());
        app.apply_stopped(k.clone(), true, Ok(StopResult::Confirmed));
        assert!(app.meta_for(&k).is_hidden());
    }

    #[test]
    fn enter_picks_attach_resume_or_asks() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        app.apply_discovery(
            Provider::Codex,
            Ok(discovery(
                vec![
                    session(Provider::Codex, "run", RuntimeState::Running),
                    session(Provider::Codex, "stop", RuntimeState::Stopped),
                    session(Provider::Codex, "unk", RuntimeState::Unknown),
                ],
                true,
            )),
        );
        app.selected = Some(SessionKey::new(Provider::Codex, "run"));
        assert!(matches!(
            app.handle_key(code(KeyCode::Enter)).as_slice(),
            [Effect::Attach(_)]
        ));
        app.selected = Some(SessionKey::new(Provider::Codex, "stop"));
        assert!(matches!(
            app.handle_key(code(KeyCode::Enter)).as_slice(),
            [Effect::Resume(_)]
        ));
        app.selected = Some(SessionKey::new(Provider::Codex, "unk"));
        assert!(app.handle_key(code(KeyCode::Enter)).is_empty());
        assert!(matches!(app.mode, Mode::ChooseAction { .. }));
    }

    #[test]
    fn search_filters_rows_and_esc_clears() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = session(Provider::Claude, "a", RuntimeState::Idle);
        a.title = Some("Fix the auth handler".into());
        let mut b = session(Provider::Codex, "b", RuntimeState::Idle);
        b.title = Some("flaky test".into());
        let mut app = app_with(&dir, vec![a, b]);
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        for c in "flaky".chars() {
            app.handle_key(key(c));
        }
        assert_eq!(
            app.session_keys_in_order(),
            vec![SessionKey::new(Provider::Codex, "b")]
        );
        app.handle_key(code(KeyCode::Esc));
        assert_eq!(app.session_keys_in_order().len(), 2);
    }

    #[test]
    fn rows_group_by_state_with_counts() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        app.apply_discovery(
            Provider::Claude,
            Ok(discovery(
                vec![
                    session(Provider::Claude, "1", RuntimeState::WaitingInput),
                    session(Provider::Claude, "2", RuntimeState::WaitingApproval),
                    session(Provider::Claude, "3", RuntimeState::Idle),
                ],
                true,
            )),
        );
        let rows = app.rows();
        assert!(matches!(&rows[0], ListRow::Group(h) if h.label == "Needs you" && h.count == 2));
        assert!(matches!(&rows[3], ListRow::Group(h) if h.label == "Idle" && h.count == 1));
    }

    fn at(cwd: &str, id: &str, state: RuntimeState) -> Session {
        let mut s = Session::new(SessionKey::new(Provider::Claude, id), cwd);
        s.runtime = Runtime::observed(state, "test", Utc::now());
        s.origin = Origin::Discovered;
        s
    }

    fn history_row(cwd: &str, id: &str) -> Session {
        let mut s = Session::new(SessionKey::new(Provider::Codex, id), cwd);
        s.runtime = Runtime::observed(RuntimeState::Unknown, "rollout file", Utc::now());
        s.origin = Origin::History;
        s
    }

    /// Saved history outnumbers live work heavily, so it is folded away and
    /// its heading still says how much is behind it.
    #[test]
    fn history_is_folded_until_z() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        app.apply_discovery(
            Provider::Claude,
            Ok(Discovery {
                sessions: vec![
                    at("/Users/me/src/reed", "live", RuntimeState::WaitingApproval),
                    at("/Users/me/src/reed", "done", RuntimeState::Stopped),
                ],
                scope_note: "test".into(),
                complete: true,
            }),
        );
        app.apply_discovery(
            Provider::Codex,
            Ok(Discovery {
                sessions: vec![
                    history_row("/Users/me/src/reed", "old1"),
                    history_row("/Users/me/src/reed", "old2"),
                ],
                scope_note: "test".into(),
                complete: true,
            }),
        );

        assert_eq!(app.shown_and_folded(), (1, 3));
        assert_eq!(app.session_keys_in_order().len(), 1);

        // The folded groups still appear, marked and counted.
        let folded: Vec<GroupHeader> = app
            .rows()
            .into_iter()
            .filter_map(|r| match r {
                ListRow::Group(h) if h.collapsed => Some(h),
                _ => None,
            })
            .collect();
        assert_eq!(folded.len(), 2);
        assert_eq!(folded.iter().map(|h| h.count).sum::<usize>(), 3);

        app.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert_eq!(app.shown_and_folded(), (4, 0));
        assert_eq!(app.session_keys_in_order().len(), 4);
    }

    /// The bug this nearly shipped with: every row loaded from the cache is
    /// Unknown until the first refresh answers, so folding on the state alone
    /// empties the list on a cold start.
    #[test]
    fn cached_rows_are_not_mistaken_for_history() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with(
            &dir,
            vec![
                session(Provider::Claude, "a", RuntimeState::Running),
                session(Provider::Claude, "b", RuntimeState::Idle),
            ],
        );
        // Loading marks them stale, so both read Unknown.
        assert!(app.views().iter().all(|v| v.state() == RuntimeState::Unknown));
        // They are still listed: the provider had them, so they are not history.
        assert_eq!(app.shown_and_folded(), (2, 0));
        assert_eq!(app.session_keys_in_order().len(), 2);
    }

    /// The cursor wraps at both ends, so a long list never needs a key to get
    /// back to the top.
    #[test]
    fn the_cursor_is_cyclic() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        app.apply_discovery(
            Provider::Claude,
            Ok(Discovery {
                sessions: vec![
                    at("/Users/me/src/reed", "a", RuntimeState::WaitingApproval),
                    at("/Users/me/src/reed", "b", RuntimeState::Running),
                    at("/Users/me/src/reed", "c", RuntimeState::Idle),
                ],
                scope_note: "test".into(),
                complete: true,
            }),
        );
        let order = app.session_keys_in_order();
        assert_eq!(order.len(), 3);

        app.selected = Some(order[0].clone());
        // Up from the first row lands on the last.
        app.handle_key(code(KeyCode::Up));
        assert_eq!(app.selected.as_ref(), Some(&order[2]));
        // Down from the last comes back to the first.
        app.handle_key(code(KeyCode::Down));
        assert_eq!(app.selected.as_ref(), Some(&order[0]));

        // And a full lap returns where it started.
        for _ in 0..order.len() {
            app.handle_key(code(KeyCode::Down));
        }
        assert_eq!(app.selected.as_ref(), Some(&order[0]));
    }

    /// The header counts what is on screen, folded history included as its
    /// own tally rather than silently missing from the total.
    #[test]
    fn the_header_summarises_what_is_listed() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        app.apply_discovery(
            Provider::Claude,
            Ok(Discovery {
                sessions: vec![
                    at("/Users/me/src/reed", "a", RuntimeState::WaitingApproval),
                    at("/Users/me/src/reed", "b", RuntimeState::Idle),
                    at("/Users/me/src/reed", "c", RuntimeState::Stopped),
                ],
                scope_note: "test".into(),
                complete: true,
            }),
        );
        assert_eq!(
            app.state_summary(),
            vec![("needs you", 1), ("idle", 1), ("in history", 1)]
        );

        app.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert_eq!(
            app.state_summary(),
            vec![("needs you", 1), ("idle", 1), ("stopped", 1)]
        );
    }

    /// Tab moves between the two views the list is built around, plus the
    /// workflow grouping the design doc asks for, and comes back round.
    #[test]
    fn tab_cycles_the_views() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        assert_eq!(app.view, View::Status);
        app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert_eq!(app.view, View::Repository);
        app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert_eq!(app.view, View::Workflow);
        app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert_eq!(app.view, View::Status);
    }

    #[test]
    fn the_repository_view_groups_by_repo_and_tallies_each_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        app.apply_discovery(
            Provider::Claude,
            Ok(Discovery {
                sessions: vec![
                    at("/Users/me/src/reed", "a", RuntimeState::WaitingApproval),
                    at(
                        "/Users/me/src/reed/.claude/worktrees/slo",
                        "b",
                        RuntimeState::Idle,
                    ),
                    at("/Users/me/src/reed", "c", RuntimeState::Stopped),
                    at("/Users/me/src/agnt", "d", RuntimeState::Running),
                ],
                scope_note: "test".into(),
                complete: true,
            }),
        );
        app.view = View::Repository;
        let rows = app.rows();

        // A worktree belongs to its repository, not to a group of its own.
        let groups: Vec<&GroupHeader> = rows
            .iter()
            .filter_map(|r| match r {
                ListRow::Group(h) => Some(h),
                _ => None,
            })
            .collect();
        assert_eq!(groups.len(), 2);
        let reed = groups.iter().find(|h| h.label == "reed").unwrap();
        assert_eq!(reed.count, 3);
        assert_eq!((reed.needs_you, reed.idle, reed.resting), (1, 1, 1));
        let agnt = groups.iter().find(|h| h.label == "agnt").unwrap();
        assert_eq!((agnt.count, agnt.running), (1, 1));

        // Inside a repository the row that needs you comes first.
        let first = rows
            .iter()
            .skip_while(|r| !matches!(r, ListRow::Group(h) if h.label == "reed"))
            .nth(1)
            .unwrap();
        assert!(matches!(first, ListRow::Session(v) if v.state() == RuntimeState::WaitingApproval));
    }

    /// Alphabetical repository order buries the work; the repo holding
    /// something that needs you has to come first.
    #[test]
    fn repositories_are_ordered_by_what_is_in_them() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        let old = Utc::now() - chrono::Duration::days(200);
        let recent = Utc::now() - chrono::Duration::minutes(5);

        let mut history = at("/Users/me/src/aaa-archive", "h", RuntimeState::Unknown);
        history.last_activity_at = Some(old);
        let mut busy = at("/Users/me/src/zzz-reed", "w", RuntimeState::WaitingApproval);
        busy.last_activity_at = Some(recent);
        let mut middling = at("/Users/me/src/mmm-notes", "i", RuntimeState::Idle);
        middling.last_activity_at = Some(recent);

        app.apply_discovery(
            Provider::Claude,
            Ok(Discovery {
                sessions: vec![history, busy, middling],
                scope_note: "test".into(),
                complete: true,
            }),
        );
        app.view = View::Repository;
        let labels: Vec<String> = app
            .rows()
            .iter()
            .filter_map(|r| match r {
                ListRow::Group(h) => Some(h.label.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(labels, vec!["zzz-reed", "mmm-notes", "aaa-archive"]);
    }

    #[test]
    fn the_status_view_still_groups_by_state() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        app.apply_discovery(
            Provider::Claude,
            Ok(Discovery {
                sessions: vec![
                    at("/Users/me/src/reed", "a", RuntimeState::WaitingApproval),
                    at("/Users/me/src/agnt", "d", RuntimeState::Running),
                ],
                scope_note: "test".into(),
                complete: true,
            }),
        );
        assert_eq!(app.view, View::Status);
        let labels: Vec<String> = app
            .rows()
            .iter()
            .filter_map(|r| match r {
                ListRow::Group(h) => Some(h.label.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(labels, vec!["Needs you", "Working"]);
    }

    /// Switching views must not lose the row you were on.
    #[test]
    fn the_selection_survives_a_view_change() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        app.apply_discovery(
            Provider::Claude,
            Ok(Discovery {
                sessions: vec![
                    at("/Users/me/src/reed", "a", RuntimeState::WaitingApproval),
                    at("/Users/me/src/agnt", "d", RuntimeState::Running),
                ],
                scope_note: "test".into(),
                complete: true,
            }),
        );
        app.selected = Some(SessionKey::new(Provider::Claude, "d"));
        app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert_eq!(app.selected, Some(SessionKey::new(Provider::Claude, "d")));
        app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert_eq!(app.selected, Some(SessionKey::new(Provider::Claude, "d")));
    }

    /// Typing a task and pressing Enter is the whole flow: no key to open the
    /// prompt, no agent name, no path.
    #[test]
    fn typing_a_task_and_enter_starts_a_session() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        app.shell_cwd = dir.path().to_path_buf();

        for c in "fix the flaky test".chars() {
            app.handle_key(key(c));
        }
        assert_eq!(app.draft.buffer, "fix the flaky test");
        let effects = app.handle_key(code(KeyCode::Enter));
        match effects.as_slice() {
            [Effect::Start { provider, request }] => {
                assert_eq!(*provider, Provider::Claude);
                assert_eq!(request.prompt.as_deref(), Some("fix the flaky test"));
                assert_eq!(request.cwd, dir.path());
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(app.mode, Mode::Normal);
        // The prompt clears once the task is on its way.
        assert!(app.draft.buffer.is_empty());
    }

    /// Enter is overloaded on whether the prompt has anything in it, so a
    /// prompt holding only spaces must not swallow the row action.
    #[test]
    fn enter_on_a_blank_prompt_acts_on_the_row() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![session(Provider::Claude, "a", RuntimeState::Running)]);
        app.apply_discovery(
            Provider::Claude,
            Ok(Discovery {
                sessions: vec![at("/Users/me/src/reed", "a", RuntimeState::Running)],
                scope_note: "test".into(),
                complete: true,
            }),
        );
        app.handle_key(key(' '));
        assert!(matches!(
            app.handle_key(code(KeyCode::Enter)).as_slice(),
            [Effect::Attach(_)]
        ));
    }

    #[test]
    fn tab_switches_agent_and_shift_tab_opens_the_shell() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        app.shell_cwd = dir.path().to_path_buf();

        app.handle_key(code(KeyCode::Tab));
        assert_eq!(app.draft.provider, Provider::Codex);
        app.handle_key(code(KeyCode::Tab));
        assert_eq!(app.draft.provider, Provider::Claude);

        for c in "fix the".chars() {
            app.handle_key(key(c));
        }
        // Opening shell mode asks for a shell, because there is none yet.
        assert_eq!(app.handle_key(code(KeyCode::BackTab)), vec![Effect::StartShell]);
        assert_eq!(app.input_mode, InputMode::Shell);

        // Coming back leaves the half-typed task where it was.
        app.handle_key(code(KeyCode::BackTab));
        assert_eq!(app.input_mode, InputMode::Task);
        assert_eq!(app.draft.buffer, "fix the");
    }

    /// A shell that failed to start says so and does not leave the user in a
    /// mode with nothing in it.
    #[test]
    fn a_shell_that_will_not_start_says_so_and_hands_the_mode_back() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        app.input_mode = InputMode::Shell;
        app.open_shell(Path::new("/nonexistent/definitely-not-a-shell"), 24, 80, || {});

        assert!(app.pty.is_none());
        assert_eq!(app.input_mode, InputMode::Task);
        let notice = app.notice.clone().unwrap_or_default();
        assert!(notice.contains("definitely-not-a-shell"), "{notice}");
    }

    /// The real thing, through the app: the shell runs, every key reaches it
    /// rather than the list, and where it ends up is where a session starts.
    #[tokio::test]
    async fn keys_reach_the_embedded_shell_and_the_directory_follows_it() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let mut app = app_with(&dir, vec![session(Provider::Claude, "a", RuntimeState::Idle)]);
        app.shell_cwd = dir.path().to_path_buf();

        assert_eq!(app.handle_key(code(KeyCode::BackTab)), vec![Effect::StartShell]);
        app.open_shell(Path::new("/bin/sh"), 24, 80, || {});
        if app.pty.is_none() {
            eprintln!("no pty available here; skipping");
            return;
        }

        for _ in 0..100 {
            if app.pty.as_ref().and_then(PtyShell::cwd).is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        // Ctrl+T is a list action in task mode; in shell mode it is the
        // shell's, so the view must not move.
        let view = app.view;
        app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert_eq!(app.view, view);

        for c in "cd sub".chars() {
            app.handle_key(key(c));
        }
        app.handle_key(code(KeyCode::Enter));

        let mut moved = false;
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            app.sync_shell_cwd();
            if app.shell_cwd.canonicalize().ok() == sub.canonicalize().ok() {
                moved = true;
                break;
            }
        }
        assert!(
            moved,
            "the shell did not move; screen was:\n{}",
            app.pty.unwrap().parser().lock().unwrap().screen().contents()
        );
        assert_eq!(
            app.new_session_cwd().canonicalize().unwrap(),
            sub.canonicalize().unwrap()
        );
    }

    /// `exit` gives the text area back to the task prompt instead of leaving
    /// a dead screen in it, and hands over the directory the shell reached.
    #[tokio::test]
    async fn exiting_the_shell_gives_the_text_area_back() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let mut app = app_with(&dir, vec![]);
        app.shell_cwd = dir.path().to_path_buf();
        app.input_mode = InputMode::Shell;
        app.open_shell(Path::new("/bin/sh"), 24, 80, || {});
        if app.pty.is_none() {
            eprintln!("no pty available here; skipping");
            return;
        }

        // Resizing a live shell is accepted; the box follows the window.
        app.fit_shell(10, 40);

        app.pty.as_mut().unwrap().send(b"cd sub\r");
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            app.sync_shell_cwd();
            if app.shell_cwd.canonicalize().ok() == sub.canonicalize().ok() {
                break;
            }
        }

        app.pty.as_mut().unwrap().send(b"exit\r");
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            app.poll_shell_cwd();
            app.reap_shell();
            if app.pty.is_none() {
                break;
            }
        }
        assert!(app.pty.is_none(), "the shell was never reaped");
        assert_eq!(app.input_mode, InputMode::Task);
        // The directory it reached before exiting is where the next
        // session starts — read while it was alive, since a dead process
        // cannot be asked.
        assert_eq!(
            app.new_session_cwd().canonicalize().unwrap(),
            sub.canonicalize().unwrap()
        );
    }

    /// Every printable key belongs to the prompt, so an action key that is
    /// not a chord would eat a letter out of the task text.
    #[test]
    fn plain_letters_never_act_they_only_type() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![session(Provider::Claude, "a", RuntimeState::Idle)]);
        let before = app.view;
        for c in "trofxgly".chars() {
            app.handle_key(key(c));
        }
        assert_eq!(app.draft.buffer, "trofxgly");
        assert_eq!(app.view, before);
        assert_eq!(app.mode, Mode::Normal);
        assert!(!app.show_history);
        assert!(!app.show_hidden);
    }

    /// The chords a text field owes its user stay with the text.
    #[test]
    fn ctrl_u_and_ctrl_w_edit_the_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        for c in "fix the flaky test".chars() {
            app.handle_key(key(c));
        }
        app.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(app.draft.buffer, "fix the flaky ");
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(app.draft.buffer.is_empty());
    }

    #[test]
    fn ctrl_chords_reach_the_actions() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![session(Provider::Claude, "a", RuntimeState::Idle)]);

        app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert_eq!(app.view, View::Repository);

        app.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert!(app.show_history);

        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));
        assert!(app.show_hidden);

        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert_eq!(app.mode, Mode::Search);
        app.handle_key(code(KeyCode::Esc));

        assert!(matches!(
            app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL))
                .as_slice(),
            [Effect::Refresh]
        ));

        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
        assert!(matches!(app.mode, Mode::HideDialog { .. }));
        app.handle_key(code(KeyCode::Esc));

        // The prompt was never touched by any of it.
        assert!(app.draft.buffer.is_empty());
    }

    /// `?` opens the key list, but only when it is not the start of a task.
    #[test]
    fn question_mark_opens_shortcuts_only_on_an_empty_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        app.handle_key(key('?'));
        assert_eq!(app.mode, Mode::Shortcuts);
        app.handle_key(key(' '));
        assert_eq!(app.mode, Mode::Normal);

        for c in "why".chars() {
            app.handle_key(key(c));
        }
        app.handle_key(key('?'));
        assert_eq!(app.draft.buffer, "why?");
        assert_eq!(app.mode, Mode::Normal);
    }

    /// A provider that can only start interactively hands a plan back to the
    /// loop instead of inventing a row for a session that has no id yet.
    #[test]
    fn an_interactive_start_becomes_a_run_plan_effect() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        let plan = AttachPlan {
            program: "/usr/bin/codex".into(),
            args: vec!["fix the test".into()],
            cwd: Some("/tmp".into()),
            note: "Codex starts interactively.".into(),
        };
        let effects = app.absorb(AppEvent::Started {
            provider: Provider::Codex,
            cwd: "/tmp".into(),
            result: Ok(StartOutcome::Interactive(plan.clone())),
        });
        match effects.as_slice() {
            [Effect::RunPlan(p)] => assert_eq!(**p, plan),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(app.session_count(), 0);
    }

    #[test]
    fn a_background_start_adds_the_row_without_a_plan() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        let key = SessionKey::new(Provider::Claude, "new-1");
        let effects = app.absorb(AppEvent::Started {
            provider: Provider::Claude,
            cwd: "/tmp".into(),
            result: Ok(StartOutcome::Started(crate::providers::StartResult {
                key: key.clone(),
                handle: None,
            })),
        });
        assert!(effects.is_empty());
        assert_eq!(app.session_count(), 1);
        assert_eq!(app.selected.as_ref(), Some(&key));
    }

    #[test]
    fn esc_clears_the_prompt_without_starting_anything() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        for c in "half typed".chars() {
            app.handle_key(key(c));
        }
        assert!(app.handle_key(code(KeyCode::Esc)).is_empty());
        assert!(app.draft.buffer.is_empty());
        assert_eq!(app.mode, Mode::Normal);
    }
}
