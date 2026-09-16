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
    Grouping, Link, LinkSource, Origin, Provider, Runtime, RuntimeState, Session, SessionKey, SessionMeta,
    SessionView, session::sort_views,
};
use crate::providers::{
    Adapter, AdapterError, Discovery, RuntimeUpdate, StartRequest, StartResult, StopResult,
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
    Started {
        provider: Provider,
        cwd: PathBuf,
        result: Result<StartResult, AdapterError>,
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
    NewInput {
        buffer: String,
    },
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

/// Side effects requested by a key press; executed by the event loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Refresh,
    Attach(SessionKey),
    Resume(SessionKey),
    Stop { key: SessionKey, then_hide: bool },
    Start(StartRequest),
    Quit,
}

/// A rendered list line. `SessionView` is boxed to keep the row enum small,
/// since a full list allocates one entry per session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListRow {
    Group { label: String, count: usize },
    Session(Box<SessionView>),
}

pub struct App {
    sessions: BTreeMap<SessionKey, Session>,
    metas: BTreeMap<SessionKey, SessionMeta>,
    /// Set when metadata.json is corrupt or newer; editing is disabled.
    pub metadata_error: Option<String>,
    pub provider_status: BTreeMap<Provider, ProviderStatus>,
    pub grouping: Grouping,
    pub query: String,
    pub show_hidden: bool,
    pub selected: Option<SessionKey>,
    pub mode: Mode,
    pub refreshing: bool,
    pub cache_saved_at: Option<DateTime<Utc>>,
    pub should_quit: bool,
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
            grouping: Grouping::default(),
            query: String::new(),
            show_hidden: false,
            selected: None,
            mode: Mode::Normal,
            refreshing: false,
            cache_saved_at,
            should_quit: false,
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
        sort_views(&mut views, self.grouping == Grouping::Workflow);

        // Group first so each header can carry its own count.
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
            rows.push(ListRow::Group {
                label,
                count: members.len(),
            });
            rows.extend(members.into_iter().map(|v| ListRow::Session(Box::new(v))));
        }
        rows
    }

    fn group_label(&self, v: &SessionView) -> String {
        match self.grouping {
            Grouping::Runtime => match v.state() {
                RuntimeState::WaitingApproval | RuntimeState::WaitingInput => {
                    "Waiting approval or input".to_string()
                }
                s => s.label().to_string(),
            },
            Grouping::Workflow => v.workflow().label().to_string(),
        }
    }

    pub fn session_keys_in_order(&self) -> Vec<SessionKey> {
        self.rows()
            .into_iter()
            .filter_map(|r| match r {
                ListRow::Session(v) => Some(v.session.key),
                ListRow::Group { .. } => None,
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

    pub fn apply_started(
        &mut self,
        provider: Provider,
        cwd: PathBuf,
        result: Result<StartResult, AdapterError>,
    ) {
        match result {
            Ok(res) => {
                let mut s = Session::new(res.key.clone(), cwd);
                s.origin = Origin::Managed;
                s.runtime = Runtime::observed(RuntimeState::Starting, "start requested", Utc::now());
                s.runtime.handle = res.handle;
                s.last_activity_at = Some(Utc::now());
                self.selected = Some(res.key.clone());
                self.sessions.insert(res.key, s);
                self.mode = Mode::Normal;
            }
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
            Mode::NewInput { mut buffer } => match key.code {
                KeyCode::Esc => {
                    self.mode = Mode::Normal;
                    Vec::new()
                }
                KeyCode::Enter => match parse_new_input(&buffer) {
                    Ok(req) => vec![Effect::Start(req)],
                    Err(e) => {
                        self.mode = Mode::Message(e);
                        Vec::new()
                    }
                },
                KeyCode::Backspace => {
                    buffer.pop();
                    self.mode = Mode::NewInput { buffer };
                    Vec::new()
                }
                KeyCode::Char(c) => {
                    buffer.push(c);
                    self.mode = Mode::NewInput { buffer };
                    Vec::new()
                }
                _ => Vec::new(),
            },
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
            KeyCode::Char('n') => {
                self.mode = Mode::NewInput {
                    buffer: String::new(),
                }
            }
            KeyCode::Char('/') => self.mode = Mode::Search,
            KeyCode::Tab => {
                self.grouping = self.grouping.toggle();
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

    pub fn handle_app_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Discovered { provider, result } => {
                self.apply_discovery(provider, result);
                let _ = self.save_cache();
            }
            AppEvent::Observed { provider, result } => self.apply_updates(provider, result),
            AppEvent::RefreshDone => self.refreshing = false,
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

/// `n` input: `<claude|codex> <cwd> [-- <prompt>]`. The cwd may contain
/// spaces; `--` separates it from the prompt. `~` expands to $HOME.
pub fn parse_new_input(text: &str) -> Result<StartRequest, String> {
    let text = text.trim();
    let (provider_text, rest) = text
        .split_once(char::is_whitespace)
        .ok_or_else(|| "format: <claude|codex> <cwd> [-- <initial prompt>]".to_string())?;
    let provider: Provider = provider_text.parse()?;
    let (cwd_text, prompt) = match rest.split_once(" -- ") {
        Some((c, p)) => (c.trim(), Some(p.trim().to_string()).filter(|p| !p.is_empty())),
        None => (rest.trim(), None),
    };
    if cwd_text.is_empty() {
        return Err("give a working directory".to_string());
    }
    let cwd = expand_home(cwd_text);
    if !cwd.is_dir() {
        return Err(format!("cwd does not exist: {}", cwd.display()));
    }
    Ok(StartRequest {
        operation_id: new_operation_id(provider),
        cwd,
        prompt,
    })
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

        tokio::select! {
            Some(ev) = in_rx.recv() => {
                effects.extend(app.handle_event(ev));
                while let Ok(ev) = in_rx.try_recv() {
                    effects.extend(app.handle_event(ev));
                }
            }
            Some(ev) = ev_rx.recv() => {
                app.handle_app_event(ev);
                while let Ok(ev) = ev_rx.try_recv() {
                    app.handle_app_event(ev);
                }
            }
            else => break,
        }

        for effect in effects.drain(..) {
            match effect {
                Effect::Quit => app.should_quit = true,
                Effect::Refresh => spawn_refresh(&mut app, &adapters, ev_tx.clone()),
                Effect::Attach(key) | Effect::Resume(key) => {
                    let Some(adapter) = adapter_for(&adapters, key.provider) else {
                        app.mode = Mode::Message(format!("no adapter for {}", key.provider.label()));
                        continue;
                    };
                    let is_attach = matches!(app.mode, Mode::Normal)
                        && app.sessions.get(&key).is_some_and(|s| s.runtime.state.is_live());
                    let plan = if is_attach {
                        adapter.attach(&key).await
                    } else {
                        adapter.resume(&key).await
                    };
                    match plan {
                        Ok(plan) => {
                            let status = terminal::handoff(&mut guard, &gate, &plan).await;
                            app.mode = match status {
                                Ok(s) => Mode::Message(format!(
                                    "{} exited ({s}). Refreshing state.",
                                    plan.program.display()
                                )),
                                Err(e) => Mode::Message(format!("could not start it: {e}")),
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
                Effect::Start(req) => {
                    let provider: Provider = req
                        .operation_id
                        .split('-')
                        .next()
                        .and_then(|p| p.parse().ok())
                        .unwrap_or(Provider::Claude);
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
    use crate::model::{Operation, WorkflowState};

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
            matches!(&rows[0], ListRow::Group { label, count: 2 } if label == "Waiting approval or input")
        );
        assert!(matches!(&rows[3], ListRow::Group { label, count: 1 } if label == "Idle"));
    }

    #[test]
    fn new_input_parses_provider_cwd_and_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let spaced = dir.path().join("my api");
        std::fs::create_dir(&spaced).unwrap();
        let req = parse_new_input(&format!("codex {} -- fix tests", spaced.display())).unwrap();
        assert_eq!(req.cwd, spaced);
        assert_eq!(req.prompt.as_deref(), Some("fix tests"));
        assert!(req.operation_id.starts_with("codex-"));
        assert!(parse_new_input("claude /definitely/missing/dir").is_err());
        assert!(parse_new_input("gemini /tmp").is_err());
    }
}
