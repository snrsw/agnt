// Prints a sample list so the English layout can be eyeballed.
use agnt::app::App;
use agnt::model::{Link, LinkSource, Provider, Runtime, RuntimeState, Session, SessionKey};
use agnt::providers::Discovery;
use agnt::storage::Paths;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn s(id: &str, title: &str, cwd: &str, state: RuntimeState, link: Option<&str>) -> Session {
    let provider = if id.starts_with('c') {
        Provider::Codex
    } else {
        Provider::Claude
    };
    let mut s = Session::new(SessionKey::new(provider, id), cwd);
    s.title = Some(title.to_string());
    s.runtime = Runtime::observed(state, "job list", chrono::Utc::now());
    if let Some(u) = link {
        s.links.push(Link::from_url(u, LinkSource::Provider).unwrap());
    }
    s
}

fn main() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::load(&Paths::at(dir.path()));
    let mut stale = s(
        "a9",
        "Refactor the cache layer",
        "/Users/me/src/api",
        RuntimeState::Unknown,
        None,
    );
    stale.runtime = Runtime::observed(RuntimeState::WaitingApproval, "x", chrono::Utc::now());
    stale.runtime.mark_stale("server disconnected");

    app.apply_discovery(
        Provider::Claude,
        Ok(Discovery {
            sessions: vec![
                s(
                    "a1",
                    "Fix the auth handler",
                    "/Users/me/src/api",
                    RuntimeState::WaitingApproval,
                    Some("https://github.com/o/api/pull/123"),
                ),
                s(
                    "a3",
                    "Tidy up the README",
                    "/Users/me/src/docs",
                    RuntimeState::Idle,
                    None,
                ),
                stale,
            ],
            scope_note: "live background jobs".into(),
            complete: true,
        }),
    );
    app.apply_discovery(
        Provider::Codex,
        Ok(Discovery {
            sessions: vec![s(
                "c2",
                "Investigate a flaky test",
                "/Users/me/src/api",
                RuntimeState::Running,
                Some("https://github.com/o/api/issues/105"),
            )],
            scope_note: "App Server threads".into(),
            complete: true,
        }),
    );

    // Draw the list in each view, then with the new-session bar focused.
    for label in ["status view", "repository view", "composing"] {
        match label {
            "repository view" => app.view = agnt::model::View::Repository,
            "composing" => {
                app.view = agnt::model::View::Status;
                app.draft.buffer = "fix the flaky billing test".into();
            }
            _ => {}
        }
        let width = 120u16;
        let mut t = Terminal::new(TestBackend::new(width, 22)).unwrap();
        t.draw(|f| agnt::ui::draw(f, &app)).unwrap();
        println!("=== {label} ===");
        for row in t.backend().buffer().content().chunks(width as usize) {
            println!("|{}|", row.iter().map(|c| c.symbol()).collect::<String>());
        }
    }
}
