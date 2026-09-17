//! In-memory state and the single event loop. Every UI state change goes
//! through `App`; provider calls run in background tasks and come back as
//! `AppEvent`s, so drawing never waits on I/O. Metadata writes are local
//! file operations and happen inline.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderStatus {
    pub ok: bool,
    pub note: String,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Search,
    /// The new-session prompt has focus. The bar itself is always drawn.
    NewInput(NewSession),
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

/// What the new-session bar is holding: the task text plus the two things
/// that would otherwise have to be typed every time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSession {
    pub buffer: String,
    pub provider: Provider,
    /// Index into the directories offered by `cwd_choices`.
    pub cwd_index: usize,
}

impl NewSession {
    pub fn new(provider: Provider) -> Self {
        Self {
            buffer: String::new(),
            provider,
            cwd_index: 0,
        }
    }

    pub fn cwd<'a>(&self, choices: &'a [PathBuf]) -> Option<&'a PathBuf> {
        choices.get(self.cwd_index % choices.len().max(1))
    }
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
    pub selected: Option<SessionKey>,
    pub mode: Mode,
    pub refreshing: bool,
    pub cache_saved_at: Option<DateTime<Utc>>,
    pub should_quit: bool,
    /// Keys a preview has already been requested for, so a slow or empty
    /// answer is not asked for again on every redraw.
    preview_requested: BTreeSet<SessionKey>,
    /// Where agnt was started. The first directory a new session is offered.
    launch_cwd: PathBuf,
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
            selected: None,
            mode: Mode::Normal,
            refreshing: false,
            cache_saved_at,
            should_quit: false,
            preview_requested: BTreeSet::new(),
            launch_cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
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
            rows.push(ListRow::Group(GroupHeader::tally(label, &members)));
            rows.extend(members.into_iter().map(|v| ListRow::Session(Box::new(v))));
        }
        rows
    }

    fn group_label(&self, v: &SessionView) -> String {
        match self.view {
            View::Status => match v.state() {
                RuntimeState::WaitingApproval | RuntimeState::WaitingInput => {
                    "Waiting approval or input".to_string()
                }
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

    fn move_selection(&mut self, delta: isize) {
        let keys = self.session_keys_in_order();
        if keys.is_empty() {
            self.selected = None;
            return;
        }
        let current = self
            .selected
            .as_ref()
            .and_then(|k| keys.iter().position(|x| x == k))
            .unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, keys.len() as isize - 1) as usize;
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

    /// Directories a new session can start in: where agnt was started, then
    /// the selected row's directory, then every other directory in the list.
    /// Offering these removes the most tedious part of typing a new session.
    pub fn cwd_choices(&self) -> Vec<PathBuf> {
        let mut out = vec![self.launch_cwd.clone()];
        let push = |p: &PathBuf, out: &mut Vec<PathBuf>| {
            if !out.contains(p) {
                out.push(p.clone());
            }
        };
        if let Some(v) = self.selected_view() {
            push(&v.session.cwd, &mut out);
        }
        for row in self.rows() {
            if let ListRow::Session(v) = row {
                push(&v.session.cwd, &mut out);
            }
        }
        out
    }

    /// The directory the new-session bar is currently pointing at.
    pub fn new_session_cwd(&self, draft: &NewSession) -> PathBuf {
        let choices = self.cwd_choices();
        draft
            .cwd(&choices)
            .cloned()
            .unwrap_or_else(|| self.launch_cwd.clone())
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
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return vec![Effect::Quit];
        }
        let mode = self.mode.clone();
        match mode {
            Mode::Normal => self.handle_normal(key),
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
                    KeyCode::Char(c) => self.query.push(c),
                    _ => {}
                }
                self.ensure_selection();
                Vec::new()
            }
            Mode::NewInput(mut draft) => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match key.code {
                    KeyCode::Esc => {
                        self.mode = Mode::Normal;
                        return Vec::new();
                    }
                    KeyCode::Enter => {
                        return self.submit_new_session(&draft);
                    }
                    // Tab cycles the agent and Shift+Tab the directory, so
                    // neither is typed to start somewhere other than the
                    // default. Tab is used rather than a Ctrl chord because
                    // Ctrl+A and Ctrl+D mean line-start and delete in a text
                    // field, and this field holds ordinary prose.
                    KeyCode::Tab => {
                        draft.provider = match draft.provider {
                            Provider::Claude => Provider::Codex,
                            Provider::Codex => Provider::Claude,
                        };
                    }
                    KeyCode::BackTab => {
                        let count = self.cwd_choices().len().max(1);
                        draft.cwd_index = (draft.cwd_index + 1) % count;
                    }
                    KeyCode::Backspace => {
                        draft.buffer.pop();
                    }
                    KeyCode::Char(c) if !ctrl => draft.buffer.push(c),
                    _ => {}
                }
                self.mode = Mode::NewInput(draft);
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
                KeyCode::Char(c) => {
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
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('q') => {
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

    fn handle_normal(&mut self, key: KeyEvent) -> Vec<Effect> {
        match key.code {
            KeyCode::Char('q') => self.mode = Mode::QuitConfirm,
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Char('n') => self.mode = Mode::NewInput(NewSession::new(Provider::Claude)),
            KeyCode::Char('/') => self.mode = Mode::Search,
            KeyCode::Tab => {
                self.view = self.view.next();
                self.ensure_selection();
            }
            KeyCode::Char('r') => return vec![Effect::Refresh],
            KeyCode::Char('H') => {
                self.show_hidden = !self.show_hidden;
                self.ensure_selection();
            }
            KeyCode::Enter => {
                if let Some(v) = self.selected_view() {
                    let k = v.session.key.clone();
                    return match v.state() {
                        s if s.is_live() => vec![Effect::Attach(k)],
                        RuntimeState::Stopped => vec![Effect::Resume(k)],
                        _ => {
                            self.mode = Mode::ChooseAction { key: k };
                            Vec::new()
                        }
                    };
                }
            }
            KeyCode::Char('m') => {
                if let Some(k) = self.selected.clone()
                    && let Err(e) = self.cycle_workflow(&k)
                {
                    self.mode = Mode::Message(e);
                }
            }
            KeyCode::Char('l') => {
                if let Some(k) = self.selected.clone() {
                    self.mode = Mode::LinkInput {
                        key: k,
                        buffer: String::new(),
                    };
                }
            }
            KeyCode::Char('d') => {
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
            _ => {}
        }
        Vec::new()
    }

    /// Turn the bar's contents into a start request. An empty task is refused
    /// here rather than sent to a provider that would reject it.
    fn submit_new_session(&mut self, draft: &NewSession) -> Vec<Effect> {
        let prompt = draft.buffer.trim().to_string();
        if prompt.is_empty() {
            self.mode = Mode::Message("Type what the session should do, then press Enter.".into());
            return Vec::new();
        }
        let cwd = self.new_session_cwd(draft);
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

    fn session(provider: Provider, id: &str, state: RuntimeState) -> Session {
        let mut s = Session::new(SessionKey::new(provider, id), "/tmp/proj");
        s.runtime = Runtime::observed(state, "test", Utc::now());
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
        app.handle_key(key('m'));
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
        app.handle_key(key('m'));
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
        app.handle_key(key('d'));
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
        app.handle_key(key('/'));
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
        assert!(
            matches!(&rows[0], ListRow::Group(h) if h.label == "Waiting approval or input" && h.count == 2)
        );
        assert!(matches!(&rows[3], ListRow::Group(h) if h.label == "Idle" && h.count == 1));
    }

    fn at(cwd: &str, id: &str, state: RuntimeState) -> Session {
        let mut s = Session::new(SessionKey::new(Provider::Claude, id), cwd);
        s.runtime = Runtime::observed(state, "test", Utc::now());
        s
    }

    /// Tab moves between the two views the list is built around, plus the
    /// workflow grouping the design doc asks for, and comes back round.
    #[test]
    fn tab_cycles_the_views() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        assert_eq!(app.view, View::Status);
        app.handle_key(code(KeyCode::Tab));
        assert_eq!(app.view, View::Repository);
        app.handle_key(code(KeyCode::Tab));
        assert_eq!(app.view, View::Workflow);
        app.handle_key(code(KeyCode::Tab));
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
        assert_eq!(labels, vec!["Waiting approval or input", "Running"]);
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
        app.handle_key(code(KeyCode::Tab));
        assert_eq!(app.selected, Some(SessionKey::new(Provider::Claude, "d")));
        app.handle_key(code(KeyCode::Tab));
        assert_eq!(app.selected, Some(SessionKey::new(Provider::Claude, "d")));
    }

    /// Typing a task and pressing Enter is the whole flow: no agent name and
    /// no path, because both carry defaults.
    #[test]
    fn typing_a_task_and_enter_starts_a_session() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        app.launch_cwd = dir.path().to_path_buf();

        app.handle_key(key('n'));
        assert!(matches!(app.mode, Mode::NewInput(_)));
        for c in "fix the flaky test".chars() {
            app.handle_key(key(c));
        }
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
    }

    #[test]
    fn an_empty_task_is_refused_rather_than_sent() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        app.launch_cwd = dir.path().to_path_buf();
        app.handle_key(key('n'));
        app.handle_key(key(' '));
        assert!(app.handle_key(code(KeyCode::Enter)).is_empty());
        assert!(matches!(app.mode, Mode::Message(ref m) if m.contains("Type what the session")));
    }

    #[test]
    fn tab_switches_agent_and_shift_tab_walks_the_directories() {
        let dir = tempfile::tempdir().unwrap();
        let other = dir.path().join("other-repo");
        std::fs::create_dir(&other).unwrap();
        let mut app = app_with(&dir, vec![]);
        app.launch_cwd = dir.path().to_path_buf();
        app.apply_discovery(
            Provider::Codex,
            Ok(Discovery {
                sessions: vec![Session::new(SessionKey::new(Provider::Codex, "c1"), &other)],
                scope_note: "test".into(),
                complete: true,
            }),
        );

        app.handle_key(key('n'));
        app.handle_key(code(KeyCode::Tab));
        match &app.mode {
            Mode::NewInput(d) => assert_eq!(d.provider, Provider::Codex),
            other => panic!("unexpected {other:?}"),
        }
        // Tab is a toggle, so a second press comes back to Claude.
        app.handle_key(code(KeyCode::Tab));
        match &app.mode {
            Mode::NewInput(d) => assert_eq!(d.provider, Provider::Claude),
            other => panic!("unexpected {other:?}"),
        }
        app.handle_key(code(KeyCode::Tab));

        // The launch directory comes first; Shift+Tab moves to the next one.
        app.handle_key(code(KeyCode::BackTab));
        let draft = match &app.mode {
            Mode::NewInput(d) => d.clone(),
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(app.new_session_cwd(&draft), other);

        // Neither key leaves anything in the task text.
        assert!(draft.buffer.is_empty());
        assert_eq!(draft.provider, Provider::Codex);
    }

    /// Ctrl chords in the bar are left to the text field, not stolen as
    /// shortcuts: Ctrl+A means line-start to anyone typing prose.
    #[test]
    fn ctrl_chords_do_not_switch_the_agent() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        app.handle_key(key('n'));
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        match &app.mode {
            Mode::NewInput(d) => {
                assert_eq!(d.provider, Provider::Claude);
                assert!(d.buffer.is_empty());
            }
            other => panic!("unexpected {other:?}"),
        }
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
    fn directory_choices_start_at_the_launch_dir_and_have_no_repeats() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let mut app = app_with(&dir, vec![]);
        app.launch_cwd = dir.path().to_path_buf();
        app.apply_discovery(
            Provider::Claude,
            Ok(Discovery {
                sessions: vec![
                    Session::new(SessionKey::new(Provider::Claude, "a"), &repo),
                    Session::new(SessionKey::new(Provider::Claude, "b"), &repo),
                ],
                scope_note: "test".into(),
                complete: true,
            }),
        );
        let choices = app.cwd_choices();
        assert_eq!(choices[0], dir.path());
        assert_eq!(choices.iter().filter(|c| **c == repo).count(), 1);
    }

    #[test]
    fn esc_leaves_the_bar_without_starting_anything() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        app.handle_key(key('n'));
        for c in "half typed".chars() {
            app.handle_key(key(c));
        }
        assert!(app.handle_key(code(KeyCode::Esc)).is_empty());
        assert_eq!(app.mode, Mode::Normal);
    }
}
