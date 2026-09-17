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

/// How many lines the typed task needs, so the input grows with it.
fn wrapped_lines(text: &str, width: usize) -> usize {
    if text.is_empty() || width == 0 {
        return 1;
    }
    text.width().div_ceil(width).max(1)
}

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();

    let input_w = (area.width as usize).saturating_sub(4).max(1);
    // The input grows with the task instead of truncating it, up to a third
    // of the screen. A pasted path should be readable, not clipped.
    let grow_cap = ((area.height / 3).max(1) as usize).min(6);
    let input_h = wrapped_lines(&app.draft.buffer, input_w).min(grow_cap) as u16;
    let compose_h = 1 + input_h + 1;

    let status_h = 1 + u16::from(app.metadata_error.is_some()) + u16::from(app.has_provider_problem());

    // No detail pane: a session's details belong on its own row, where the
    // cursor already is. The selected row carries them and the others do not,
    // so the list keeps its height as the cursor moves — the row being left
    // shrinks by exactly what the row being entered grows by.
    let [header, body, compose, status] = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(3),
        Constraint::Length(compose_h),
        Constraint::Length(status_h),
    ])
    .areas(area);

    draw_header(frame, header, app);
    draw_table(frame, body, app);
    draw_compose(frame, compose, app);
    draw_status(frame, status, app);
    draw_dialog(frame, area, app);
}

/// Three lines and a gap, the shape the agent view uses: what this is, where
/// it is pointed, and what is in it.
fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let width = area.width as usize;

    let counts: Vec<String> = app
        .state_summary()
        .into_iter()
        .map(|(label, n)| format!("{n} {label}"))
        .collect();
    let summary = if counts.is_empty() {
        "no sessions".to_string()
    } else {
        counts.join(" · ")
    };

    let mut second = format!("{} view · {}", app.view.label(), short_path(app.launch_cwd()));
    if !app.query.is_empty() || app.mode == Mode::Search {
        second.push_str(&format!("  ·  search: {}_", app.query));
    }
    if app.refreshing {
        second.push_str("  ·  refreshing");
    }

    frame.render_widget(
        Paragraph::new(vec![
            Line::styled("agnt", Style::default().add_modifier(Modifier::BOLD)),
            Line::styled(
                text::truncate_to_width(&second, width),
                Style::default().add_modifier(Modifier::DIM),
            ),
            Line::styled(
                text::truncate_to_width(&summary, width),
                Style::default().add_modifier(Modifier::DIM),
            ),
        ]),
        area,
    );
}

/// The new-session prompt. It is always live — typing anywhere in the list
/// lands here — so it is drawn as an active field rather than as a hint.
fn draw_compose(frame: &mut Frame, outer: Rect, app: &App) {
    let block = Block::default().borders(Borders::TOP);
    let area = block.inner(outer);
    let width = area.width as usize;

    let [input_area, target_area] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);

    let typed = &app.draft.buffer;
    let input = if typed.is_empty() {
        Paragraph::new(Line::from(vec![
            Span::styled("> ", Style::default().add_modifier(Modifier::DIM)),
            Span::styled(
                "Describe a task for a new session",
                Style::default().add_modifier(Modifier::DIM),
            ),
        ]))
    } else {
        Paragraph::new(Line::from(vec![
            Span::styled("> ", Style::default().add_modifier(Modifier::DIM)),
            Span::raw(typed.clone()),
            Span::raw("_"),
        ]))
        .wrap(Wrap { trim: false })
    };

    let cwd = app.new_session_cwd(&app.draft);
    let target = format!("  {} · {}", app.draft.provider.label(), short_path(&cwd));

    frame.render_widget(block, outer);
    frame.render_widget(input, input_area);
    frame.render_widget(
        Paragraph::new(Line::styled(
            text::truncate_to_width(&target, width),
            Style::default().add_modifier(Modifier::DIM),
        )),
        target_area,
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

/// The extra lines the selected row shows: where it is, how it is filed, and
/// its short id — the things the card's own two lines leave out. Dim, so an
/// expanded row does not shout over the list around it.
fn detail_lines(v: &SessionView, width: usize, indent: usize) -> Vec<Line<'static>> {
    let pad = " ".repeat(indent);
    let mut out = Vec::new();

    if let Some(preview) = v.session.preview.as_deref().filter(|p| !p.trim().is_empty()) {
        out.push(Line::styled(
            text::truncate_to_width(&format!("{pad}{}", text::sanitize(preview)), width),
            Style::default().add_modifier(Modifier::DIM),
        ));
    }

    let mut facts = v.session.cwd.to_string_lossy().into_owned();
    facts.push_str(&format!("  ·  {}", v.workflow().label().to_lowercase()));
    facts.push_str(&format!("  ·  {}", v.key().short_id()));
    if v.is_hidden() {
        facts.push_str("  ·  hidden");
    }
    out.push(Line::styled(
        text::truncate_to_width(&format!("{pad}{}", text::sanitize(&facts)), width),
        Style::default().add_modifier(Modifier::DIM),
    ));
    out
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
    let mut lines = vec![
        Line::from(first),
        Line::styled(text::truncate_to_width(&second, width), meta_style),
    ];
    if selected {
        lines.extend(detail_lines(v, width, W_GLYPH + 2));
    }
    lines
}

/// A session as one line under a repository heading. The repository is already
/// on the group row, so the branch is what tells two rows apart.
fn compact_lines(
    v: &SessionView,
    width: usize,
    now: DateTime<Utc>,
    suffix: &str,
    selected: bool,
) -> Vec<Line<'static>> {
    let mut lines = vec![compact_line(v, width, now, suffix)];
    if selected {
        lines.extend(detail_lines(v, width, W_GLYPH + 2));
    }
    lines
}

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
                    ListItem::new(compact_lines(v, total, now, &suffix, selected))
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

/// One line of keys for what you are doing now, and nothing else unless
/// something is wrong. A working provider does not need to announce itself,
/// and the full key list is behind `?` rather than across the screen.
fn draw_status(frame: &mut Frame, area: Rect, app: &App) {
    let width = area.width as usize;
    let mut lines = Vec::new();

    if let Some(err) = &app.metadata_error {
        lines.push(Line::from(text::truncate_to_width(
            &format!("metadata editing disabled: {}", text::sanitize(err)),
            width,
        )));
    }
    for (provider, status) in app.provider_status.iter().filter(|(_, s)| !s.ok) {
        lines.push(Line::from(text::truncate_to_width(
            &format!("{}: {}", provider.label(), text::sanitize(&status.note)),
            width,
        )));
    }

    let hint = if !app.draft.buffer.trim().is_empty() {
        "enter to start · tab for agent · esc to clear"
    } else {
        "type to start a session · enter to attach · ? for shortcuts"
    };
    lines.push(Line::from(text::truncate_to_width(hint, width)));

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
        // The list, the search line and the prompt are drawn inline.
        Mode::Normal | Mode::Search => return,
        Mode::Shortcuts => (
            "Shortcuts",
            vec![
                "type            write a task for a new session".into(),
                "enter           start it, or attach to the selected row when the prompt is empty".into(),
                "up / down       move".into(),
                "tab / s-tab     the new session's agent / directory".into(),
                "".into(),
                "ctrl+t          cycle view: status, repository, workflow".into(),
                "ctrl+f          search".into(),
                "ctrl+o          show stopped sessions and saved history".into(),
                "ctrl+y          show hidden rows".into(),
                "ctrl+r          refresh".into(),
                "".into(),
                "ctrl+x          hide the selected row, or restore it".into(),
                "ctrl+g          change its classification".into(),
                "ctrl+l          attach a PR or issue URL to it".into(),
                "".into(),
                "ctrl+u / ctrl+w clear the prompt / delete a word".into(),
                "ctrl+c          quit. Sessions keep running; see docs/design.md §7".into(),
                "".into(),
                "any key closes this".into(),
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
        assert!(has(&out, "Needs you (1)"), "{out}");
        assert!(has(&out, "Fix the auth handler"), "{out}");
        assert!(has(&out, "PR #123"), "{out}");
        // The second line qualifies the first: agent, state in words, place.
        assert!(has(&out, "Claude · Needs approval · api"), "{out}");
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
        assert!(!has(&out, "Claude · Needs approval · api"), "{out}");
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
        let first = heading("Needs you");
        let second = heading("Idle (1)");

        // A heading is followed straight away by its own rows.
        assert!(!lines[first + 1].trim().is_empty(), "{out}");
        // The next section is preceded by a blank line.
        assert!(lines[second - 1].trim().is_empty(), "{out}");
        assert!(second > first + 1, "{out}");
    }

    /// The prompt is fenced off by a rule, so it does not read as another
    /// row of the list.
    #[test]
    fn the_compose_bar_is_ruled_off_from_the_list() {
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
        assert!(has(&out, "Working"), "{out}");
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
        assert!(has(&out, "Unknown (Working)"), "{out}");
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

    /// Details belong to the row the cursor is on, not to a pane four rows
    /// away. Only the selected row carries them.
    #[test]
    fn only_the_selected_row_shows_its_details() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(
            &dir,
            vec![
                session("a", "Fix the auth handler", RuntimeState::WaitingApproval),
                session("b", "Tidy up the README", RuntimeState::Idle),
            ],
        );
        app.selected = Some(SessionKey::new(Provider::Claude, "a"));
        let out = render(&app, 120, 24);

        // The full path is on the selected row and nowhere else.
        assert_eq!(
            out.lines().filter(|l| l.contains("/Users/me/src/api")).count(),
            1,
            "{out}"
        );
        // It sits directly under the selected card, not at the bottom.
        let title = out
            .lines()
            .position(|l| l.contains("Fix the auth handler"))
            .unwrap();
        let detail = out.lines().position(|l| l.contains("/Users/me/src/api")).unwrap();
        assert_eq!(detail, title + 2, "{out}");
    }

    /// Moving the cursor must not shuffle the list: the row being left shrinks
    /// by exactly what the row being entered grows by.
    #[test]
    fn the_list_keeps_its_height_as_the_cursor_moves() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(
            &dir,
            vec![
                session("a", "First", RuntimeState::Idle),
                session("b", "Second", RuntimeState::Idle),
                session("c", "Third", RuntimeState::Idle),
            ],
        );

        let filled = |app: &App| {
            render(app, 120, 30)
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count()
        };

        app.selected = Some(SessionKey::new(Provider::Claude, "a"));
        let with_first = filled(&app);
        app.selected = Some(SessionKey::new(Provider::Claude, "b"));
        let with_second = filled(&app);
        app.selected = Some(SessionKey::new(Provider::Claude, "c"));
        let with_third = filled(&app);

        assert_eq!(with_first, with_second);
        assert_eq!(with_second, with_third);
    }

    /// A long task must stay readable rather than being clipped to one line.
    #[test]
    fn the_input_grows_with_the_task() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![session("a", "x", RuntimeState::Idle)]);

        let empty = render(&app, 80, 24);
        let rows_when_empty = empty.lines().filter(|l| l.contains("Describe a task")).count();
        assert_eq!(rows_when_empty, 1);

        app.draft.buffer = "s".repeat(300);
        let full = render(&app, 80, 24);
        let typed_rows = full.lines().filter(|l| l.contains("ssss")).count();
        assert!(typed_rows > 1, "the task should wrap, got one line:\n{full}");
        // It stops growing before it eats the list.
        assert!(typed_rows <= 6, "{full}");
    }

    /// A working provider says nothing; only a failing one is worth a line.
    #[test]
    fn the_footer_is_one_line_until_something_is_wrong() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![session("a", "x", RuntimeState::Idle)]);
        let out = render(&app, 120, 24);
        assert!(has(&out, "? for shortcuts"), "{out}");
        assert!(!has(&out, "live jobs"), "{out}");

        app.apply_discovery(
            Provider::Claude,
            Err(crate::providers::AdapterError::Timeout {
                command: "claude".into(),
                timeout_ms: 1,
            }),
        );
        let out = render(&app, 120, 24);
        assert!(has(&out, "timed out"), "{out}");
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
