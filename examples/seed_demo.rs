//! Write sample sessions into the list cache so the TUI can be driven by hand
//! while the provider adapters are still stubs.
//!
//! ```sh
//! export AGNT_DATA_DIR=/tmp/agnt-demo
//! cargo run --example seed_demo
//! cargo run
//! ```
//!
//! The rows come from the cache, so agnt shows them as a previous observation:
//! the state reads `Unknown (Running)` and so on. That is the intended
//! behaviour, not a bug — a cached state is never presented as confirmed.

use std::path::PathBuf;

use agnt::model::{Link, LinkSource, Provider, Runtime, RuntimeState, Session, SessionKey};
use agnt::storage::{CacheStore, Paths};
use chrono::{Duration, Utc};

fn main() -> anyhow::Result<()> {
    let paths = Paths::resolve().ok_or_else(|| anyhow::anyhow!("no data directory"))?;
    paths.ensure_base()?;

    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let src = PathBuf::from(&home).join("src");
    let now = Utc::now();

    let rows = vec![
        session(
            Provider::Claude,
            "a1b2c3d4-1111-2222-3333-444455556666",
            "Fix the auth handler",
            "The token refresh path still double-writes the session cookie.",
            src.join("api"),
            RuntimeState::WaitingApproval,
            now - Duration::seconds(30),
            Some("https://github.com/example/api/pull/123"),
        ),
        session(
            Provider::Codex,
            "c0ffee00-5555-6666-7777-888899990000",
            "Investigate a flaky test",
            "Reproduced twice out of fifty runs; suspect the shared fixture.",
            src.join("api"),
            RuntimeState::Running,
            now - Duration::minutes(3),
            Some("https://github.com/example/api/issues/105"),
        ),
        session(
            Provider::Claude,
            "deadbeef-9999-aaaa-bbbb-ccccddddeeee",
            "Tidy up the README",
            "Moved the install steps above the feature list.",
            src.join("docs"),
            RuntimeState::Idle,
            now - Duration::hours(2),
            None,
        ),
        session(
            Provider::Codex,
            "facade11-0000-1111-2222-333344445555",
            "Refactor the cache layer",
            "",
            src.join("api"),
            RuntimeState::Stopped,
            now - Duration::days(1),
            None,
        ),
    ];

    let cache = CacheStore::new(&paths);
    cache.save(&rows)?;
    println!("wrote {} sessions to {}", rows.len(), cache.path().display());
    println!("now run: cargo run");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn session(
    provider: Provider,
    id: &str,
    title: &str,
    preview: &str,
    cwd: PathBuf,
    state: RuntimeState,
    last_activity: chrono::DateTime<Utc>,
    link: Option<&str>,
) -> Session {
    let mut s = Session::new(SessionKey::new(provider, id), cwd);
    s.title = Some(title.to_string());
    if !preview.is_empty() {
        s.preview = Some(preview.to_string());
    }
    s.runtime = Runtime::observed(state, "seed_demo example", last_activity);
    s.last_activity_at = Some(last_activity);
    if let Some(url) = link {
        s.links.push(Link::from_url(url, LinkSource::Provider).unwrap());
    }
    s
}
