//! Ratatui rendering. Pure function of `App`; no I/O here.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use chrono::{DateTime, Utc};

use crate::app::{App, GroupHeader, ListRow, Mode};
use std::collections::BTreeMap;

use crate::model::{RuntimeState, SessionView, TitleSource, View, text};

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let status_lines = 2 + u16::from(app.metadata_error.is_some());
    // Each pane below the list is separated by its own rule. The detail pane
    // is a rule plus three lines; the compose bar a rule plus two. The list
    // takes what is left.
    let [header, body, detail, compose, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(4),
        Constraint::Length(3),
        Constraint::Length(status_lines),
    ])
    .areas(area);

    draw_header(frame, header, app);
    draw_table(frame, body, app);
    draw_detail(frame, detail, app);
    draw_compose(frame, compose, app);
    draw_status(frame, status, app);
    draw_dialog(frame, area, app);
}

/// The new-session bar, always on screen so starting work never needs a
/// remembered command. Typing is the only required step: the agent and the
/// directory carry defaults and are shown next to the prompt.
fn draw_compose(frame: &mut Frame, outer: Rect, app: &App) {
    let block = Block::default().borders(Borders::TOP);
    let area = block.inner(outer);
    let width = area.width as usize;
    let focused = matches!(app.mode, Mode::NewInput(_));

    let (typed, provider, cwd) = match &app.mode {
        Mode::NewInput(draft) => (draft.buffer.clone(), draft.provider, app.new_session_cwd(draft)),
        _ => {
            let draft = crate::app::NewSession::new(crate::model::Provider::Claude);
            (String::new(), draft.provider, app.new_session_cwd(&draft))
        }
    };

    let prompt = if focused {
        format!("> {typed}_")
    } else if typed.is_empty() {
        "> Describe a task for a new session  (n)".to_string()
    } else {
        format!("> {typed}")
    };

    let target = format!("  {} - {}", provider.label(), short_path(&cwd));
    let hint = if focused {
        "Tab agent  S-Tab directory  Enter start  Esc cancel"
    } else {
        ""
    };
    let gap = width.saturating_sub(target.width()).saturating_sub(hint.width());
    let second = format!("{target}{}{hint}", " ".repeat(gap));

    let mut prompt_style = Style::default();
    if !focused {
        prompt_style = prompt_style.add_modifier(Modifier::DIM);
    }
    frame.render_widget(block, outer);
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(text::truncate_to_width(&prompt, width), prompt_style),
            Line::styled(
                text::truncate_to_width(&second, width),
                Style::default().add_modifier(Modifier::DIM),
            ),
        ]),
        area,
    );
}

/// A directory short enough to sit beside the prompt: `$HOME` becomes `~`,
/// and a still-long path keeps its last components, which are the ones that
/// identify the checkout. Worktree paths are long enough that the head is the
/// part worth dropping.
pub fn short_path(path: &std::path::Path) -> String {
    const MAX: usize = 40;
    let text = path.to_string_lossy();
    let tilde = match std::env::var_os("HOME") {
        Some(home) => {
            let home = home.to_string_lossy().into_owned();
            match text.strip_prefix(&home) {
                Some(rest) => format!("~{rest}"),
                None => text.clone().into_owned(),
            }
        }
        None => text.clone().into_owned(),
    };
    if tilde.width() <= MAX {
        return tilde;
    }
    let parts: Vec<&str> = tilde.split('/').filter(|p| !p.is_empty()).collect();
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 1; // the leading ellipsis
    for part in parts.iter().rev() {
        let next = used + part.width() + 1;
        if next > MAX && !kept.is_empty() {
            break;
        }
        used = next;
        kept.push(part);
    }
    kept.reverse();
    format!("\u{2026}/{}", kept.join("/"))
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let (shown, folded) = app.shown_and_folded();
    let mut spans = vec![
        Span::styled(" agnt ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!(" view: {} (Tab) ", app.view.label())),
        Span::styled(
            format!(" {shown} listed "),
            Style::default().add_modifier(Modifier::DIM),
        ),
    ];
    if folded > 0 {
        spans.push(Span::styled(
            format!(" {folded} in history (z) "),
            Style::default().add_modifier(Modifier::DIM),
        ));
    }
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

/// Links as text, empty when there are none. A placeholder dash in a column
/// that is usually empty reads as a typo rather than as "nothing here".
fn links_text(v: &SessionView) -> String {
    v.links()
        .iter()
        .map(|l| l.short_label())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Column widths in terminal cells.
const W_AGENT: usize = 6;
const W_BRANCH: usize = 22;
const W_LINKS: usize = 20;
const W_MARKER: usize = 2;
const W_GLYPH: usize = 2;

/// A state's glyph. Paired with a word or a tally everywhere it appears —
/// design.md §3 forbids conveying state by colour alone, and a glyph alone is
/// the same mistake in a different alphabet.
fn state_glyph(state: RuntimeState) -> &'static str {
    match state {
        RuntimeState::WaitingApproval | RuntimeState::WaitingInput => "●",
        // `◐` is rare enough that terminals fall back to another font for it,
        // which changes its advance width and shifts the whole row. `*` is in
        // every font at exactly one cell, and reads as "busy".
        RuntimeState::Running | RuntimeState::Starting => "*",
        RuntimeState::Idle => "○",
        RuntimeState::Stopped => "·",
        RuntimeState::Error => "!",
        RuntimeState::Unknown => "?",
    }
}

fn state_style(state: RuntimeState) -> Style {
    match state {
        RuntimeState::WaitingApproval | RuntimeState::WaitingInput | RuntimeState::Error => {
            Style::default().add_modifier(Modifier::BOLD)
        }
        RuntimeState::Stopped | RuntimeState::Unknown => Style::default().add_modifier(Modifier::DIM),
        _ => Style::default(),
    }
}

/// A session as two lines: the task, then everything that qualifies it.
/// Used by the status and workflow views.
fn card_lines(
    v: &SessionView,
    width: usize,
    now: DateTime<Utc>,
    selected: bool,
    suffix: &str,
) -> Vec<Line<'static>> {
    let links = links_text(v);
    let links_w = if links.is_empty() { 0 } else { links.width() + 2 };
    let title_w = width.saturating_sub(W_GLYPH + links_w).max(4);

    // A session with no name falls back to its id. An id is not a title, so it
    // is not styled like one — bolding it draws the eye to the least
    // informative rows in the list.
    let (title, source) = v.title();
    let title_style = match source {
        TitleSource::ShortId => Style::default().add_modifier(Modifier::DIM),
        _ => Style::default().add_modifier(Modifier::BOLD),
    };

    let mut first = vec![
        Span::styled(
            text::pad_to_width(state_glyph(v.state()), W_GLYPH),
            state_style(v.state()),
        ),
        Span::styled(
            text::pad_to_width(&format!("{}{suffix}", text::sanitize(&title)), title_w),
            title_style,
        ),
    ];
    if links_w > 0 {
        first.push(Span::raw("  "));
        first.push(Span::raw(links));
    }

    // Second line: agent, state in words, where, and how long ago.
    let mut meta = vec![v.session.provider().label().to_string(), v.state_label()];
    let mut place = v.repo();
    if let Some(b) = v.branch_or_worktree() {
        place.push_str(&format!(" / {b}"));
    }
    meta.push(place);
    if let Some(age) = v.age(now) {
        meta.push(age);
    }
    let second = format!("{}{}", " ".repeat(W_GLYPH), meta.join(" · "));

    // Dim on top of the selection's reversed colours is close to unreadable,
    // so the selected card's second line keeps full contrast.
    let meta_style = if selected {
        Style::default()
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };
    vec![
        Line::from(first),
        Line::styled(text::truncate_to_width(&second, width), meta_style),
    ]
}

/// A session as one line under a repository heading. The repository is already
/// on the group row, so the branch is what tells two rows apart.
fn compact_line(v: &SessionView, width: usize, now: DateTime<Utc>, suffix: &str) -> Line<'static> {
    let age = v.age(now).unwrap_or_default();
    let fixed = W_GLYPH + W_AGENT + 1 + W_BRANCH + 1 + W_LINKS + 1 + 4;
    let title_w = width.saturating_sub(fixed).max(4);
    let (title, source) = v.title();
    let title_style = match source {
        TitleSource::ShortId => Style::default().add_modifier(Modifier::DIM),
        _ => Style::default(),
    };

    Line::from(vec![
        Span::styled(
            text::pad_to_width(state_glyph(v.state()), W_GLYPH),
            state_style(v.state()),
        ),
        Span::raw(text::pad_to_width(v.session.provider().label(), W_AGENT)),
        Span::raw(" "),
        Span::styled(
            text::pad_to_width(&format!("{}{suffix}", text::sanitize(&title)), title_w),
            title_style,
        ),
        Span::raw(" "),
        Span::styled(
            text::pad_to_width(&v.branch_or_worktree().unwrap_or_default(), W_BRANCH),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::raw(" "),
        Span::raw(text::pad_to_width(&links_text(v), W_LINKS)),
        Span::styled(
            text::pad_to_width(&age, 4),
            Style::default().add_modifier(Modifier::DIM),
        ),
    ])
}

/// A group heading. In the repository view it carries the tally, which is what
/// lets a repo say what is inside it without being expanded.
fn group_line(h: &GroupHeader, view: View, width: usize) -> Line<'static> {
    // A folded group announces itself, so nothing is silently missing.
    let marker = if h.collapsed { "▸" } else { "──" };
    let left = format!("{marker} {} ({}) ", h.label, h.count);
    if view != View::Repository {
        return Line::styled(
            text::truncate_to_width(&left, width),
            Style::default().add_modifier(Modifier::BOLD | Modifier::DIM),
        );
    }
    let tally = format!(
        "● {}  ◐ {}  ○ {}  · {}",
        h.needs_you, h.running, h.idle, h.resting
    );
    let gap = width.saturating_sub(left.width()).saturating_sub(tally.width());
    Line::from(vec![
        Span::styled(left, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" ".repeat(gap)),
        Span::styled(tally, Style::default().add_modifier(Modifier::DIM)),
    ])
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

    let total = (area.width as usize).saturating_sub(W_MARKER);
    let now = Utc::now();
    let mut selected_index = None;

    // Two conversations can honestly share a name — a fork, or a rename that
    // kept both rollouts. Showing the id only on the ones that collide keeps
    // them apart without putting an id on every row.
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for row in &rows {
        if let ListRow::Session(v) = row {
            *seen.entry(v.title().0).or_default() += 1;
        }
    }

    let items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| match row {
            // A blank line above each heading but the first, so groups read
            // as separate sections rather than as one run of rows.
            ListRow::Group(h) => {
                let line = group_line(h, app.view, total);
                if i == 0 {
                    ListItem::new(line)
                } else {
                    ListItem::new(vec![Line::raw(""), line])
                }
            }
            ListRow::Session(v) => {
                let selected = app.selected.as_ref() == Some(v.key());
                if selected {
                    selected_index = Some(i);
                }
                let (title, source) = v.title();
                let suffix = if source != TitleSource::ShortId && seen.get(&title).copied().unwrap_or(0) > 1 {
                    format!("  {}", v.key().short_id())
                } else {
                    String::new()
                };
                let mut style = Style::default();
                if v.is_hidden() {
                    style = style.add_modifier(Modifier::DIM);
                }
                let item = if app.view.is_cards() {
                    ListItem::new(card_lines(v, total, now, selected, &suffix))
                } else {
                    ListItem::new(compact_line(v, total, now, &suffix))
                };
                item.style(style)
            }
        })
        .collect();

    let list = List::new(items)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");
    let mut state = ListState::default();
    state.select(selected_index);
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_detail(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::TOP);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(v) = app.selected_view() else {
        return;
    };
    let width = inner.width as usize;
    // The top line is the most prominent in the pane, so it carries the
    // preview when there is one and the evidence when there is not, rather
    // than spending itself on the words "no content".
    let has_preview = v.session.preview.is_some();
    let lead = if has_preview {
        v.preview()
    } else {
        text::sanitize(&v.session.runtime.evidence)
    };
    let mut place = v.session.cwd.to_string_lossy().into_owned();
    if let Some(b) = v.branch_or_worktree() {
        place.push_str(&format!("  [{b}]"));
    }
    let links = links_text(&v);
    if !links.is_empty() {
        place.push_str(&format!("  {links}"));
    }
    if v.is_hidden() {
        place.push_str("  (hidden)");
    }
    // The evidence is only repeated here when the top line did not take it.
    let evidence = if has_preview {
        format!("  evidence: {}", text::sanitize(&v.session.runtime.evidence))
    } else {
        String::new()
    };
    let observed = match v.session.runtime.observed_at {
        Some(t) => format!(
            "last observed {}{evidence}  class: {}  origin: {}  id: {}",
            t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S"),
            v.workflow().label(),
            v.session.origin.as_str(),
            v.key().qualified(),
        ),
        None => format!(
            "last observed: never{evidence}  class: {}  origin: {}  id: {}",
            v.workflow().label(),
            v.session.origin.as_str(),
            v.key().qualified()
        ),
    };
    let lines = vec![
        Line::from(text::truncate_to_width(&lead, width)),
        Line::from(text::truncate_to_width(&text::sanitize(&place), width)),
        Line::styled(
            text::truncate_to_width(&observed, width),
            Style::default().add_modifier(Modifier::DIM),
        ),
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
        "j/k move  Enter attach/resume  n new  / search  Tab view  z history  m class  l link  d delete  r refresh  H hidden  q quit",
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
        // Normal, Search, and NewInput are drawn inline, not as a dialog.
        Mode::Normal | Mode::Search | Mode::NewInput(_) => return,
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
    use crate::model::{Link, LinkSource, Provider, Runtime, RuntimeState, Session, SessionKey};
    use crate::providers::Discovery;
    use crate::storage::Paths;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn session(id: &str, title: &str, state: RuntimeState) -> Session {
        let mut s = Session::new(SessionKey::new(Provider::Claude, id), "/Users/me/src/api");
        s.title = Some(title.to_string());
        s.runtime = Runtime::observed(state, "test", chrono::Utc::now());
        // Discovered, not history: the list folds history away by default.
        s.origin = crate::model::Origin::Discovered;
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
    fn the_status_view_draws_a_card_per_session() {
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
        assert!(has(&out, "PR #123"), "{out}");
        // The second line qualifies the first: agent, state in words, place.
        assert!(has(&out, "Claude · Waiting approval · api"), "{out}");
        assert!(has(&out, "Idle (1)"), "{out}");
    }

    #[test]
    fn the_repository_view_draws_one_line_and_a_tally() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(
            &dir,
            vec![
                session("a", "Fix the auth handler", RuntimeState::WaitingApproval),
                session("b", "Tidy up the README", RuntimeState::Idle),
            ],
        );
        app.view = crate::model::View::Repository;
        let out = render(&app, 120, 24);
        assert!(has(&out, "api (2)"), "{out}");
        assert!(has(&out, "● 1  ◐ 0  ○ 1  · 0"), "{out}");
        assert!(has(&out, "Fix the auth handler"), "{out}");
        // One line each, so the card's second line is gone.
        assert!(!has(&out, "Claude · Waiting approval · api"), "{out}");
    }

    /// Groups have to read as separate sections, so every heading but the
    /// first carries a blank line above it.
    #[test]
    fn groups_are_separated_by_a_blank_line() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with(
            &dir,
            vec![
                session("a", "Fix the auth handler", RuntimeState::WaitingApproval),
                session("b", "Tidy up the README", RuntimeState::Idle),
            ],
        );
        let out = render(&app, 120, 24);
        let lines: Vec<&str> = out.lines().collect();

        let heading = |needle: &str| {
            lines
                .iter()
                .position(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("no heading {needle} in\n{out}"))
        };
        let first = heading("Waiting approval or input");
        let second = heading("Idle (1)");

        // The first heading sits directly under the view header.
        assert!(!lines[first - 1].trim().is_empty(), "{out}");
        // The second is preceded by a blank line.
        assert!(lines[second - 1].trim().is_empty(), "{out}");
    }

    /// The panes below the list are each fenced off by a rule, so the detail
    /// and the new-session prompt do not read as one block.
    #[test]
    fn the_compose_bar_is_ruled_off_from_the_detail_pane() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with(
            &dir,
            vec![session("a", "Fix the auth handler", RuntimeState::Idle)],
        );
        let out = render(&app, 120, 24);
        let prompt = out
            .lines()
            .position(|l| l.contains("Describe a task"))
            .unwrap_or_else(|| panic!("no prompt in\n{out}"));
        let above: &str = out.lines().nth(prompt - 1).unwrap();
        assert!(
            above.chars().all(|c| c == '─'),
            "expected a rule above the prompt, got {above:?}\n{out}"
        );
    }

    #[test]
    fn a_narrow_terminal_still_shows_state_and_title() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with(
            &dir,
            vec![session("a", "Fix the auth handler", RuntimeState::Running)],
        );
        let out = render(&app, 60, 20);
        assert!(has(&out, "Running"), "{out}");
        assert!(has(&out, "Fix the auth handler"), "{out}");
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

#[cfg(test)]
mod path_tests {
    use super::short_path;
    use std::path::Path;

    #[test]
    fn home_becomes_tilde_and_short_paths_are_untouched() {
        let home = std::env::var("HOME").unwrap_or_default();
        if !home.is_empty() {
            assert_eq!(short_path(&Path::new(&home).join("src/api")), "~/src/api");
        }
        assert_eq!(short_path(Path::new("/tmp/x")), "/tmp/x");
    }

    #[test]
    fn a_long_path_keeps_its_tail() {
        let p = Path::new("/Users/me/src/github.com/owner/agnt/.claude/worktrees/scaffold");
        let out = short_path(p);
        assert!(out.starts_with('\u{2026}'), "{out}");
        assert!(out.ends_with("scaffold"), "{out}");
        assert!(out.chars().count() <= 41, "{out}");
    }
}
