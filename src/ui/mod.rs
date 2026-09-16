//! Ratatui rendering. Pure function of `App`; no I/O here.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::{App, ListRow, Mode};
use crate::model::{RuntimeState, SessionView, text};

/// Below this width the branch and link columns move into the detail pane.
const WIDE_MIN_COLS: u16 = 110;

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let status_lines = 2 + u16::from(app.metadata_error.is_some());
    let [header, body, detail, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(5),
        Constraint::Length(status_lines),
    ])
    .areas(area);

    draw_header(frame, header, app);
    draw_table(frame, body, app);
    draw_detail(frame, detail, app);
    draw_status(frame, status, app);
    draw_dialog(frame, area, app);
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans = vec![
        Span::styled(" agnt ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!(" grouped by {} ", app.grouping.label())),
    ];
    if !app.query.is_empty() || app.mode == Mode::Search {
        spans.push(Span::raw(format!(" search: {}_ ", app.query)));
    }
    if app.show_hidden {
        spans.push(Span::raw(format!(" showing hidden ({}) ", app.hidden_count())));
    }
    if app.refreshing {
        spans.push(Span::raw(" refreshing... "));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The state cell. A stale row names the state it last held, so the longest
/// possible cell is `Unknown (Waiting approval)` — which is what `W_STATE` is
/// sized for.
fn state_text(v: &SessionView) -> String {
    let r = &v.session.runtime;
    match (r.state, r.last_known_state) {
        (RuntimeState::Unknown, Some(prev)) => format!("{} ({})", r.state.label(), prev.label()),
        _ => r.state.label().to_string(),
    }
}

fn place_text(v: &SessionView, with_branch: bool) -> String {
    let dir = v
        .session
        .cwd
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| v.session.cwd.to_string_lossy().into_owned());
    match (with_branch, v.session.branch()) {
        (true, Some(b)) => format!("{dir} / {b}"),
        _ => dir,
    }
}

fn links_text(v: &SessionView) -> String {
    let labels: Vec<String> = v.links().iter().map(|l| l.short_label()).collect();
    if labels.is_empty() {
        "—".to_string()
    } else {
        labels.join(" · ")
    }
}

/// Column widths in terminal cells. The title column takes what is left.
const W_STATE: usize = 26;
const W_AGENT: usize = 6;
const W_PLACE: usize = 28;
const W_LINKS: usize = 20;
const W_MARKER: usize = 2;

/// One list line, laid out by hand. A `Table` would clip a group header to
/// the first column, so columns are padded here instead.
fn session_line(v: &SessionView, wide: bool, title_width: usize) -> String {
    let mut line = String::new();
    line.push_str(&text::pad_to_width(&state_text(v), W_STATE));
    line.push(' ');
    line.push_str(&text::pad_to_width(v.session.provider().label(), W_AGENT));
    line.push(' ');
    line.push_str(&text::pad_to_width(&text::sanitize(&v.title().0), title_width));
    if wide {
        line.push(' ');
        line.push_str(&text::pad_to_width(&place_text(v, true), W_PLACE));
        line.push(' ');
        line.push_str(&text::pad_to_width(&links_text(v), W_LINKS));
    }
    line
}

fn draw_table(frame: &mut Frame, area: Rect, app: &App) {
    let rows = app.rows();
    if rows.is_empty() {
        let msg = if app.session_count() == 0 {
            "No sessions. Press r to refresh, n to start one. Provider status is on the line below."
        } else {
            "No rows match. Press / to change the search, H to show hidden rows."
        };
        frame.render_widget(Paragraph::new(msg).wrap(Wrap { trim: true }), area);
        return;
    }

    let total = area.width as usize;
    let wide = area.width >= WIDE_MIN_COLS;
    let fixed = W_MARKER + W_STATE + 1 + W_AGENT + 1 + if wide { W_PLACE + 1 + W_LINKS + 1 } else { 0 };
    let title_width = total.saturating_sub(fixed).max(4);

    let mut header = String::from("  ");
    header.push_str(&text::pad_to_width("State", W_STATE));
    header.push(' ');
    header.push_str(&text::pad_to_width("Agent", W_AGENT));
    header.push(' ');
    header.push_str(&text::pad_to_width("Content", title_width));
    if wide {
        header.push(' ');
        header.push_str(&text::pad_to_width("Location / branch", W_PLACE));
        header.push(' ');
        header.push_str(&text::pad_to_width("Related", W_LINKS));
    }

    let mut selected_index = None;
    let items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| match row {
            ListRow::Group { label, count } => ListItem::new(text::truncate_to_width(
                &format!("── {label} ({count}) ──"),
                total,
            ))
            .style(Style::default().add_modifier(Modifier::BOLD | Modifier::DIM)),
            ListRow::Session(v) => {
                if app.selected.as_ref() == Some(v.key()) {
                    selected_index = Some(i);
                }
                let mut style = Style::default();
                if v.is_hidden() {
                    style = style.add_modifier(Modifier::DIM);
                }
                ListItem::new(text::truncate_to_width(
                    &session_line(v, wide, title_width),
                    total.saturating_sub(W_MARKER),
                ))
                .style(style)
            }
        })
        .collect();

    let [header_area, list_area] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    frame.render_widget(
        Paragraph::new(text::truncate_to_width(&header, total))
            .style(Style::default().add_modifier(Modifier::UNDERLINED)),
        header_area,
    );

    let list = List::new(items)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");
    let mut state = ListState::default();
    state.select(selected_index);
    frame.render_stateful_widget(list, list_area, &mut state);
}

fn draw_detail(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::TOP);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(v) = app.selected_view() else {
        return;
    };
    let width = inner.width as usize;
    let preview = text::truncate_to_width(&v.preview(), width);
    let mut place = v.session.cwd.to_string_lossy().into_owned();
    if let Some(b) = v.session.branch() {
        place.push_str(&format!("  [{b}]"));
    }
    place.push_str(&format!("  {}", links_text(&v)));
    if v.is_hidden() {
        place.push_str("  (hidden)");
    }
    let observed = match v.session.runtime.observed_at {
        Some(t) => format!(
            "last observed {}  evidence: {}  class: {}  origin: {}  id: {}",
            t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S"),
            text::sanitize(&v.session.runtime.evidence),
            v.workflow().label(),
            v.session.origin.as_str(),
            v.key().qualified(),
        ),
        None => format!(
            "last observed: never  class: {}  origin: {}  id: {}",
            v.workflow().label(),
            v.session.origin.as_str(),
            v.key().qualified()
        ),
    };
    let lines = vec![
        Line::from(preview),
        Line::from(text::truncate_to_width(&text::sanitize(&place), width)),
        Line::from(text::truncate_to_width(&observed, width)),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_status(frame: &mut Frame, area: Rect, app: &App) {
    let width = area.width as usize;
    let mut lines = Vec::new();
    let providers: Vec<String> = crate::model::Provider::ALL
        .iter()
        .map(|p| match app.provider_status.get(p) {
            Some(s) => format!(
                "{}: {}{}",
                p.label(),
                if s.ok { "" } else { "x " },
                text::sanitize(&s.note)
            ),
            None => format!("{}: not fetched", p.label()),
        })
        .collect();
    lines.push(Line::from(text::truncate_to_width(
        &providers.join("  |  "),
        width,
    )));
    if let Some(err) = &app.metadata_error {
        lines.push(Line::from(text::truncate_to_width(
            &format!("metadata editing disabled: {}", text::sanitize(err)),
            width,
        )));
    }
    lines.push(Line::from(text::truncate_to_width(
        "j/k move  Enter attach/resume  n new  / search  Tab group  m class  l link  d delete  r refresh  H hidden  q quit",
        width,
    )));
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().add_modifier(Modifier::DIM)),
        area,
    );
}

fn centered(area: Rect, width_pct: u16, height: u16) -> Rect {
    let [_, mid, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height),
        Constraint::Fill(1),
    ])
    .areas(area);
    let [_, rect, _] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Percentage(width_pct),
        Constraint::Fill(1),
    ])
    .areas(mid);
    rect
}

fn draw_dialog(frame: &mut Frame, area: Rect, app: &App) {
    let (title, body): (&str, Vec<String>) = match &app.mode {
        Mode::Normal | Mode::Search => return,
        Mode::NewInput { buffer } => (
            "Start a session",
            vec![
                "<claude|codex> <cwd> [-- <initial prompt>]   Enter start / Esc back".into(),
                format!("> {buffer}▏"),
            ],
        ),
        Mode::LinkInput { key, buffer } => (
            "Register a PR / issue URL",
            vec![
                format!("target: {}", key.qualified()),
                "Enter register / Esc back".into(),
                format!("> {buffer}▏"),
            ],
        ),
        Mode::HideDialog { key, live } => {
            let mut lines = vec![
                format!("target: {}", key.qualified()),
                "Hides the row from this list. History stays in the tool; nothing is deleted.".into(),
            ];
            if *live {
                lines.push(
                    "It is running.  c: keep running and hide   s: stop first, then hide   Esc: back".into(),
                );
            } else {
                lines.push("y / Enter: hide   Esc: back".into());
            }
            ("Delete (hide)", lines)
        }
        Mode::ChooseAction { key } => (
            "Choose an operation",
            vec![
                format!("target: {}", key.qualified()),
                "The execution state is undecidable.  a: attach to a running conversation   r: resume a saved one   Esc: back"
                    .into(),
            ],
        ),
        Mode::QuitConfirm => (
            "Leave the list",
            vec![app.quit_note(), "Enter / y: quit   Esc: back".into()],
        ),
        Mode::Message(msg) => ("Message", vec![msg.clone(), "Press any key to close".into()]),
    };

    let height = (body.len() as u16) + 2;
    let rect = centered(area, 80, height.max(3));
    frame.render_widget(Clear, rect);
    let block = Block::default().borders(Borders::ALL).title(format!(" {title} "));
    let lines: Vec<Line> = body.into_iter().map(Line::from).collect();
    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: false }),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::model::{Link, LinkSource, Provider, Runtime, Session, SessionKey};
    use crate::providers::Discovery;
    use crate::storage::Paths;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn session(id: &str, title: &str, state: RuntimeState) -> Session {
        let mut s = Session::new(SessionKey::new(Provider::Claude, id), "/Users/me/src/api");
        s.title = Some(title.to_string());
        s.runtime = Runtime::observed(state, "test", chrono::Utc::now());
        s.links
            .push(Link::from_url("https://github.com/o/api/pull/123", LinkSource::Provider).unwrap());
        s
    }

    fn app_with(dir: &tempfile::TempDir, sessions: Vec<Session>) -> App {
        let mut app = App::load(&Paths::at(dir.path()));
        app.apply_discovery(
            Provider::Claude,
            Ok(Discovery {
                sessions,
                scope_note: "live jobs".into(),
                complete: true,
            }),
        );
        app
    }

    fn render(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(width as usize)
            .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Wide characters occupy two cells and column padding varies with the
    /// terminal size, so compare on the text with all spaces removed.
    fn has(rendered: &str, needle: &str) -> bool {
        fn squeeze(s: &str) -> String {
            s.chars().filter(|c| !c.is_whitespace()).collect()
        }
        squeeze(rendered).contains(&squeeze(needle))
    }

    #[test]
    fn wide_list_shows_group_state_title_branch_and_links() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with(
            &dir,
            vec![
                session("a", "Fix the auth handler", RuntimeState::WaitingApproval),
                session("b", "Tidy up the README", RuntimeState::Idle),
            ],
        );
        let out = render(&app, 120, 24);
        assert!(has(&out, "Waiting approval or input (1)"), "{out}");
        assert!(has(&out, "Fix the auth handler"), "{out}");
        assert!(has(&out, "Idle (1)"), "{out}");
        assert!(has(&out, "PR #123"), "{out}");
        assert!(has(&out, "api"), "{out}");
    }

    #[test]
    fn narrow_terminal_keeps_state_and_title_only() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with(
            &dir,
            vec![session("a", "Fix the auth handler", RuntimeState::Running)],
        );
        let out = render(&app, 60, 20);
        assert!(has(&out, "Running"), "{out}");
        assert!(has(&out, "Fix the auth handler"), "{out}");
        // Branch and link columns move to the detail pane, which is still shown.
        assert!(!has(&out, "Location / branch"), "{out}");
        assert!(has(&out, "/Users/me/src/api"), "{out}");
    }

    #[test]
    fn stale_row_shows_last_known_state() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![session("a", "x", RuntimeState::Running)]);
        app.apply_discovery(
            Provider::Claude,
            Err(crate::providers::AdapterError::Timeout {
                command: "claude".into(),
                timeout_ms: 1,
            }),
        );
        let out = render(&app, 120, 24);
        assert!(has(&out, "Unknown (Running)"), "{out}");
    }

    #[test]
    fn hide_dialog_offers_stop_for_a_running_session() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![session("a", "x", RuntimeState::Running)]);
        app.mode = Mode::HideDialog {
            key: SessionKey::new(Provider::Claude, "a"),
            live: true,
        };
        let out = render(&app, 100, 24);
        assert!(has(&out, "Delete (hide)"), "{out}");
        assert!(has(&out, "History stays in the tool"), "{out}");
        assert!(has(&out, "stop first, then hide"), "{out}");
    }

    #[test]
    fn empty_and_tiny_terminals_do_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let empty = App::load(&Paths::at(dir.path()));
        assert!(has(&render(&empty, 80, 24), "No sessions"));
        for (w, h) in [(1u16, 1u16), (4, 2), (20, 3), (200, 60)] {
            let app = app_with(&dir, vec![session("a", "x", RuntimeState::Idle)]);
            render(&app, w, h);
        }
    }
}
