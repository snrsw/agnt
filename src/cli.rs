//! Argument parsing and the non-interactive subcommands.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::app::{self, App, expand_home, new_operation_id};
use crate::model::{Operation, Provider, SessionKey, SessionView, resolve_key, text};
use crate::providers::{self, AdapterError, StartRequest};
use crate::storage::{CacheStore, MetadataStore, Paths};
use crate::{git, github, terminal};

/// Exit code for an operation the installed provider does not support.
pub const EXIT_UNSUPPORTED: u8 = 2;

const LIST_REFRESH_TIMEOUT: Duration = Duration::from_secs(10);

/// Width of the state column in `agnt list`, sized for `Unknown (Waiting approval)`.
const STATE_COL: usize = 26;

#[derive(Parser, Debug)]
#[command(name = "agnt", version, about = "Claude Code / Codex session list")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Print the session list
    List {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
        /// Include hidden sessions
        #[arg(long)]
        hidden: bool,
        /// Use the cache only; do not query providers
        #[arg(long)]
        no_refresh: bool,
    },
    /// Start a new session in a directory
    New {
        provider: Provider,
        /// Working directory (default: current directory)
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Initial prompt
        #[arg(long)]
        prompt: Option<String>,
    },
    /// Connect to a running session
    Attach { id: String },
    /// Reopen a saved session
    Resume { id: String },
    /// Hide a session from the list (history is kept)
    Hide { id: String },
    /// Show a hidden session again
    Restore { id: String },
    /// Stop a running session
    Stop { id: String },
    /// Permanently delete a session through the provider's CLI
    Delete {
        id: String,
        /// Do not ask for confirmation
        #[arg(long)]
        yes: bool,
    },
    /// Attach a PR or issue URL to a session
    Link { id: String, url: String },
    /// Show supported features, versions, and data locations
    Doctor,
}

pub async fn run() -> anyhow::Result<ExitCode> {
    let cli = Cli::parse();
    let paths = Paths::resolve().context("could not determine the data directory")?;
    match cli.command {
        None => {
            app::run_tui(paths, providers::all()).await?;
            Ok(ExitCode::SUCCESS)
        }
        Some(cmd) => run_command(paths, cmd).await,
    }
}

async fn run_command(paths: Paths, cmd: Command) -> anyhow::Result<ExitCode> {
    match cmd {
        Command::List {
            json,
            hidden,
            no_refresh,
        } => list(&paths, json, hidden, no_refresh).await,
        Command::New {
            provider,
            cwd,
            prompt,
        } => new_session(provider, cwd, prompt).await,
        Command::Attach { id } => attach_or_resume(&paths, &id, Operation::Attach).await,
        Command::Resume { id } => attach_or_resume(&paths, &id, Operation::Resume).await,
        Command::Hide { id } => {
            let key = resolve(&paths, &id)?;
            let now = chrono::Utc::now();
            MetadataStore::new(&paths).update(|m| m.entry(&key).hidden_at = Some(now))?;
            println!("hidden: {key} (history is kept by the tool)");
            Ok(ExitCode::SUCCESS)
        }
        Command::Restore { id } => {
            let key = resolve(&paths, &id)?;
            MetadataStore::new(&paths).update(|m| m.entry(&key).hidden_at = None)?;
            println!("restored: {key}");
            Ok(ExitCode::SUCCESS)
        }
        Command::Stop { id } => {
            let key = resolve(&paths, &id)?;
            let adapter = providers::for_provider(key.provider);
            match adapter.stop(&key).await {
                Ok(providers::StopResult::Confirmed) => {
                    println!("stopped: {key}");
                    Ok(ExitCode::SUCCESS)
                }
                Ok(providers::StopResult::Requested { note }) => {
                    println!("stop requested (not yet confirmed): {key}: {note}");
                    Ok(ExitCode::SUCCESS)
                }
                Err(e) => unsupported_or_fail(e),
            }
        }
        Command::Delete { id, yes } => {
            let key = resolve(&paths, &id)?;
            if !yes {
                eprint!(
                    "Permanently delete {key}? This cannot be undone. Enter to delete, anything else to cancel: "
                );
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                if !answer.trim().is_empty() {
                    println!("cancelled");
                    return Ok(ExitCode::SUCCESS);
                }
            }
            let adapter = providers::for_provider(key.provider);
            match adapter.delete(&key).await {
                Ok(()) => {
                    crate::storage::forget(&MetadataStore::new(&paths), &CacheStore::new(&paths), &key)?;
                    println!("deleted: {key}");
                    Ok(ExitCode::SUCCESS)
                }
                Err(e) => unsupported_or_fail(e),
            }
        }
        Command::Link { id, url } => {
            let key = resolve(&paths, &id)?;
            let link = crate::model::Link::from_url(&url, crate::model::LinkSource::Manual)?;
            let label = link.short_label();
            MetadataStore::new(&paths).update(move |m| m.entry(&key).upsert_link(link))?;
            println!("linked: {label}");
            Ok(ExitCode::SUCCESS)
        }
        Command::Doctor => doctor(&paths).await,
    }
}

/// Resolve user text to one key. A fully qualified key is accepted even when
/// agnt has not seen the session, so metadata can be attached ahead of
/// discovery; short IDs must match exactly one known session.
fn resolve(paths: &Paths, id: &str) -> anyhow::Result<SessionKey> {
    let app = App::load(paths);
    let known = app.known_keys();
    match resolve_key(&known, id) {
        Ok(k) => Ok(k),
        Err(e) => match SessionKey::parse_qualified(id) {
            Some(k) => Ok(k),
            None => Err(e.into()),
        },
    }
}

fn unsupported_or_fail(e: AdapterError) -> anyhow::Result<ExitCode> {
    if e.is_unsupported() {
        eprintln!("{e}");
        Ok(ExitCode::from(EXIT_UNSUPPORTED))
    } else {
        Err(e.into())
    }
}

#[derive(Serialize)]
struct ListEntry {
    key: SessionKey,
    id: String,
    provider: Provider,
    title: String,
    state: &'static str,
    last_known_state: Option<&'static str>,
    workflow: &'static str,
    cwd: PathBuf,
    branch: Option<String>,
    links: Vec<crate::model::Link>,
    hidden: bool,
    origin: &'static str,
    observed_at: Option<chrono::DateTime<chrono::Utc>>,
    evidence: String,
}

impl From<&SessionView> for ListEntry {
    fn from(v: &SessionView) -> Self {
        Self {
            key: v.key().clone(),
            id: v.key().qualified(),
            provider: v.session.provider(),
            title: v.title().0,
            state: v.state().as_str(),
            last_known_state: v.session.runtime.last_known_state.map(|s| s.as_str()),
            workflow: v.workflow().as_str(),
            cwd: v.session.cwd.clone(),
            branch: v.session.branch().map(str::to_string),
            links: v.links(),
            hidden: v.is_hidden(),
            origin: v.session.origin.as_str(),
            observed_at: v.session.runtime.observed_at,
            evidence: v.session.runtime.evidence.clone(),
        }
    }
}

async fn list(paths: &Paths, json: bool, hidden: bool, no_refresh: bool) -> anyhow::Result<ExitCode> {
    let mut app = App::load(paths);
    app.show_hidden = hidden;

    if !no_refresh {
        for adapter in providers::all() {
            let provider = adapter.provider();
            let result = match tokio::time::timeout(LIST_REFRESH_TIMEOUT, adapter.discover()).await {
                Ok(r) => r,
                Err(_) => Err(AdapterError::Timeout {
                    command: format!("{provider} discover"),
                    timeout_ms: LIST_REFRESH_TIMEOUT.as_millis() as u64,
                }),
            };
            app.apply_discovery(provider, result);
        }
        app.save_cache()?;
    }

    let views: Vec<SessionView> = app
        .rows()
        .into_iter()
        .filter_map(|r| match r {
            app::ListRow::Session(v) => Some(*v),
            app::ListRow::Group(_) => None,
        })
        .collect();

    if json {
        #[derive(Serialize)]
        struct Output {
            sessions: Vec<ListEntry>,
            providers: Vec<ProviderNote>,
            metadata_error: Option<String>,
        }
        #[derive(Serialize)]
        struct ProviderNote {
            provider: Provider,
            ok: bool,
            note: String,
        }
        let out = Output {
            sessions: views.iter().map(ListEntry::from).collect(),
            providers: app
                .provider_status
                .iter()
                .map(|(p, s)| ProviderNote {
                    provider: *p,
                    ok: s.ok,
                    note: s.note.clone(),
                })
                .collect(),
            metadata_error: app.metadata_error.clone(),
        };
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(ExitCode::SUCCESS);
    }

    for (p, s) in &app.provider_status {
        eprintln!("{}: {}{}", p.label(), if s.ok { "" } else { "✗ " }, s.note);
    }
    if let Some(e) = &app.metadata_error {
        eprintln!("metadata: {e}");
    }
    if views.is_empty() {
        println!("(no sessions)");
        return Ok(ExitCode::SUCCESS);
    }
    println!(
        "{} {} {} {} Location",
        text::pad_to_width("State", STATE_COL),
        text::pad_to_width("Agent", 6),
        text::pad_to_width("ID", 10),
        text::pad_to_width("Content", 40),
    );
    for v in &views {
        println!(
            "{} {} {} {} {}",
            text::pad_to_width(&v.state_label(), STATE_COL),
            text::pad_to_width(v.session.provider().label(), 6),
            text::pad_to_width(&v.key().short_id(), 10),
            text::pad_to_width(&v.title().0, 40),
            v.session.cwd.display()
        );
    }
    Ok(ExitCode::SUCCESS)
}

async fn new_session(
    provider: Provider,
    cwd: Option<PathBuf>,
    prompt: Option<String>,
) -> anyhow::Result<ExitCode> {
    let cwd = match cwd {
        Some(c) => expand_home(&c.to_string_lossy()),
        None => std::env::current_dir()?,
    };
    if !cwd.is_dir() {
        anyhow::bail!("cwd does not exist: {}", cwd.display());
    }
    let adapter = providers::for_provider(provider);
    let req = StartRequest {
        operation_id: new_operation_id(provider),
        cwd,
        prompt,
    };
    match adapter.start(req).await {
        Ok(providers::StartOutcome::Started(res)) => {
            println!("started: {}", res.key);
            Ok(ExitCode::SUCCESS)
        }
        // Codex only starts interactively: run it here with the terminal, the
        // same as `agnt attach` does.
        Ok(providers::StartOutcome::Interactive(plan)) => {
            if !plan.note.is_empty() {
                eprintln!("{}", plan.note);
            }
            let status = terminal::run_plan(&plan).await?;
            Ok(if status.success() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Err(e) => unsupported_or_fail(e),
    }
}

async fn attach_or_resume(paths: &Paths, id: &str, op: Operation) -> anyhow::Result<ExitCode> {
    let key = resolve(paths, id)?;
    let adapter = providers::for_provider(key.provider);
    let plan = match op {
        Operation::Attach => adapter.attach(&key).await,
        _ => adapter.resume(&key).await,
    };
    match plan {
        Ok(plan) => {
            if !plan.note.is_empty() {
                eprintln!("{}", plan.note);
            }
            let status = terminal::run_plan(&plan).await?;
            Ok(if status.success() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Err(e) => unsupported_or_fail(e),
    }
}

async fn doctor(paths: &Paths) -> anyhow::Result<ExitCode> {
    println!("agnt {}", env!("CARGO_PKG_VERSION"));
    println!("data dir: {}", paths.base.display());

    let store = MetadataStore::new(paths);
    match store.load() {
        Ok(m) => println!(
            "metadata: ok (schema {}, revision {}, {} entries)",
            m.schema_version,
            m.revision,
            m.sessions.len()
        ),
        Err(e) => println!("metadata: ✗ {e}"),
    }
    // Which shell Shift+Tab will open, and why. `$SHELL` is shown when it
    // disagrees, because a development shell rewrites it and the difference
    // is otherwise invisible until the wrong prompt appears.
    let shell = crate::shell::program();
    print!("shell: {}", shell.display());
    match std::env::var("SHELL") {
        Ok(env) if std::path::Path::new(&env) != shell => {
            println!(" (ignoring $SHELL={env}; set AGNT_SHELL to override)")
        }
        _ => println!(),
    }

    match crate::ui::Skin::current() {
        crate::ui::Skin::Bubbles => {
            println!("style: bubbles (needs a Nerd Font; AGNT_STYLE=plain for none)")
        }
        crate::ui::Skin::Plain => println!("style: plain"),
    }

    match CacheStore::new(paths).load() {
        Some(c) => println!(
            "cache: {} rows, saved {}",
            c.sessions.len(),
            c.saved_at.to_rfc3339()
        ),
        None => println!("cache: none"),
    }

    for adapter in providers::all() {
        let p = adapter.provider();
        println!();
        match adapter.binary() {
            Some(bin) => println!("{}: {}", p.label(), bin.display()),
            None => println!("{}: not found in PATH", p.label()),
        }
        match adapter.version().await {
            Ok(v) => println!("  version: {v}"),
            Err(e) => println!("  version: ✗ {e}"),
        }
        let caps = adapter.capabilities().await;
        for op in Operation::ALL {
            match caps.get(op) {
                crate::model::Support::Supported => println!("  {:<9} supported", op.as_str()),
                crate::model::Support::Unsupported { reason } => {
                    println!("  {:<9} unsupported: {reason}", op.as_str())
                }
            }
        }
    }

    println!();
    match github::gh_version().await {
        Ok(v) => println!("gh: {v}"),
        Err(e) => println!("gh: x {e} (registering and showing URLs still works)"),
    }
    match git::git_binary() {
        Some(g) => println!("git: {}", g.display()),
        None => println!("git: not found (branch column stays empty)"),
    }
    Ok(ExitCode::SUCCESS)
}
