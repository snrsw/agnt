//! Ratatui rendering. Pure function of `App`; no I/O here.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use chrono::{DateTime, Utc};

use crate::app::{App, GroupHeader, InputMode, ListRow, Mode};
use std::collections::BTreeMap;

use crate::model::{RuntimeState, SessionView, TitleSource, View, WorkflowState, text};

/// The palette. Named ANSI colours only, so the list wears whatever the
/// terminal is themed with instead of fighting it: amber on a light theme is
/// still amber on a dark one, and a user who tuned their sixteen colours gets
/// the list in those.
///
/// Colour is additive everywhere it appears. A state always has its glyph and
/// its word as well (design.md §3), so a monochrome terminal loses nothing but
/// the emphasis.
///
/// The prompt's bubbles are the one exception and carry `bubblesextra`'s own
/// hex colours, because the point of quoting a named theme is to look like
/// it. See [`Skin`].
mod theme {
    use ratatui::style::{Color, Modifier, Style};

    use crate::model::RuntimeState;

    /// The brand mark, the cursor bar, and the caret while typing.
    pub const ACCENT: Color = Color::Blue;
    /// PR and issue references, the colour the agent view gives them.
    pub const LINK: Color = Color::Magenta;

    pub fn accent() -> Style {
        Style::default().fg(ACCENT)
    }

    pub fn link() -> Style {
        Style::default().fg(LINK)
    }

    pub fn muted() -> Style {
        Style::default().add_modifier(Modifier::DIM)
    }

    /// Rules and borders sit behind the content, never in front of it.
    pub fn rule() -> Style {
        muted()
    }

    /// The colour a state wears, on its glyph and on its group heading.
    pub fn state(s: RuntimeState) -> Style {
        match s {
            RuntimeState::WaitingApproval | RuntimeState::WaitingInput => {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            }
            RuntimeState::Error => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            RuntimeState::Running | RuntimeState::Starting => Style::default().fg(Color::Green),
            RuntimeState::Idle => Style::default(),
            RuntimeState::Stopped | RuntimeState::Unknown => muted(),
        }
    }
}

/// Left margin for the header, the prompt and the footer. Two cells, which
/// is the width of the list's cursor gutter, so their text lines up with the
/// rows' glyphs instead of sitting flush against the terminal's edge.
const MARGIN: u16 = 2;

fn with_margin(area: Rect) -> Rect {
    Rect {
        x: area.x + MARGIN.min(area.width),
        width: area.width.saturating_sub(MARGIN),
        ..area
    }
}

/// The most rows of shell the text area will grow to.
///
/// A cap, not a height: the box is drawn as tall as the shell is actually
/// using, so an idle prompt is one line. This is where it stops growing —
/// enough for one command's output, after which the shell scrolls and the
/// tail is what stays. It is a prompt, not a terminal to live in; the list
/// above it is what the screen is for.
fn shell_rows_cap(total_rows: u16) -> u16 {
    (total_rows / 4).clamp(4, 12)
}

/// The size the embedded shell should be told it has, for a screen of this
/// size. Needed before the first frame is drawn, so the shell starts at the
/// right size rather than redrawing itself once.
pub fn shell_area_size(area: Rect) -> (u16, u16) {
    let cols = area.width.saturating_sub(MARGIN * 2);
    (shell_rows_cap(area.height).max(1), cols.max(1))
}

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();

    let shell_open = app.input_mode == InputMode::Shell && app.pty.is_some();

    // One box at the bottom either way, in the same place: agnt's prompt, or
    // the shell's. Switching modes changes what is in the text area, not
    // where it is or how the screen is divided.
    let compose_h = if shell_open {
        // As tall as the shell is using, capped. An idle prompt is one line,
        // and a command's output grows the box to fit — the same way the task
        // prompt grows with the text typed into it.
        let used = app.pty.as_ref().map_or(1, |p| p.used_rows());
        1 + used.clamp(1, shell_rows_cap(area.height)) + 1
    } else {
        // The prompt grows with the task typed into it, up to a third of the
        // screen. The prefix shares the first line, so the height is what the
        // two of them wrap to together, plus one cell for the cursor.
        let width = (area.width as usize).saturating_sub(MARGIN as usize * 2).max(1);
        let grow_cap = ((area.height / 3).max(1) as usize).min(6);
        let prefix = prompt_prefix(app, width, Skin::current());
        let used = spans_width(&prefix) + app.draft.buffer.width() + 1;
        1 + used.div_ceil(width).clamp(1, grow_cap) as u16 + 1
    };

    // Detail line, then any problems, then the key hint. The detail line is
    // always there — blank when nothing is selected — so the cursor moving
    // never changes the height of anything.
    let status_h = 2
        + u16::from(app.metadata_error.is_some())
        + u16::from(app.has_provider_problem())
        + u16::from(app.notice.is_some());

    let [header, body, compose, status] = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(3),
        Constraint::Length(compose_h),
        Constraint::Length(status_h),
    ])
    .areas(area);

    draw_header(frame, header, app);
    draw_table(frame, body, app);
    if shell_open {
        draw_shell(frame, compose, app);
    } else {
        draw_compose(frame, compose, app);
    }
    draw_status(frame, status, app);
    draw_dialog(frame, area, app);
}

/// The shell, in the text area's own box.
///
/// Nothing here interprets the content: `vt100` has already turned the
/// shell's output into a grid of cells, colours and all, and `tui-term` draws
/// that grid. The bottom row of that grid is the shell's prompt, which is
/// what makes this a text area — the user types into their own shell, with
/// their own prompt, where agnt's prompt would otherwise be. The real cursor
/// is placed on it so it blinks where they are typing.
fn draw_shell(frame: &mut Frame, outer: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(theme::rule());
    let area = with_margin(block.inner(outer));
    frame.render_widget(block, outer);

    let Some(pty) = app.pty.as_ref() else {
        return;
    };
    let Ok(parser) = pty.parser().lock() else {
        return;
    };
    let screen = parser.screen();

    // Until the shell has drawn anything there is nothing to show, and an
    // empty box looks broken rather than busy. How long this lasts is the
    // shell's own configuration — `direnv` sourcing a nix dev shell is
    // seconds — so it says which shell is being waited for.
    if screen.contents().trim().is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(
                text::truncate_to_width(
                    &format!("starting {}…", short_path(pty.program())),
                    area.width as usize,
                ),
                Style::default().add_modifier(Modifier::DIM),
            )),
            area,
        );
        return;
    }

    frame.render_widget(tui_term::widget::PseudoTerminal::new(screen), area);

    if !screen.hide_cursor() {
        let (row, col) = screen.cursor_position();
        if row < area.height && col < area.width {
            frame.set_cursor_position((area.x + col, area.y + row));
        }
    }
}

/// Three lines and a gap, the shape the agent view uses:/// Three lines and a gap, the shape the agent view uses: what this is, where
/// it is pointed, and what is in it.
fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let area = with_margin(area);
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

    let mut second = format!("{} view · {}", app.view.label(), short_path(&app.shell_cwd));
    if !app.query.is_empty() || app.mode == Mode::Search {
        second.push_str(&format!("  ·  search: {}_", app.query));
    }
    if app.refreshing {
        second.push_str("  ·  refreshing");
    }

    frame.render_widget(
        Paragraph::new(vec![
            Line::styled("agnt", theme::accent().add_modifier(Modifier::BOLD)),
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

/// How agnt draws the things it puts in pills: the prompt, and each row's
/// second line.
///
/// `Bubbles` follows oh-my-posh's `bubblesextra`: each piece of context is a
/// rounded pill, which is what the author of this tool has in their own
/// shell, so the two prompts look like the same machine. It needs a Nerd Font
/// for the caps and the folder mark; without one they come out as blank boxes
/// or, worse, as glyphs the terminal gives a different advance width, which
/// puts every column after them out by one.
///
/// That cannot be detected from inside a terminal, so it is a choice rather
/// than a guess: `AGNT_STYLE=plain` gets the same prompt without the glyphs
/// or the colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Skin {
    #[default]
    Bubbles,
    Plain,
}

impl Skin {
    /// Read once: it cannot change while the program runs, and the prompt is
    /// rebuilt on every frame.
    pub fn current() -> Self {
        static CHOICE: std::sync::OnceLock<Skin> = std::sync::OnceLock::new();
        *CHOICE.get_or_init(|| match std::env::var("AGNT_STYLE").as_deref() {
            Ok("plain") => Self::Plain,
            _ => Self::Bubbles,
        })
    }
}

/// `bubblesextra`, as far as a prompt with two things to say needs it.
///
/// The caps are drawn in the fill colour against the terminal's own
/// background, which is what makes a rectangle of background colour read as a
/// rounded pill. The palette is the theme's: one fill for every bubble, and a
/// colour per segment on top of it.
mod bubbles {
    use ratatui::style::Color;

    /// Rounded left and right caps, and the folder mark the theme's path
    /// segment uses. All three are Nerd Font glyphs.
    pub const LEFT: &str = "\u{e0b6}";
    pub const RIGHT: &str = "\u{e0b4}";
    pub const FOLDER: &str = "\u{e5ff}";

    /// The one fill every bubble shares, `#29315A`.
    pub const FILL: Color = Color::Rgb(0x29, 0x31, 0x5a);
    /// The theme's path green, `#3EC669`.
    pub const PATH: Color = Color::Rgb(0x3e, 0xc6, 0x69);
    /// The blue it gives a language segment, `#7FD5EA`, for the agent.
    pub const AGENT: Color = Color::Rgb(0x7f, 0xd5, 0xea);

    /// Two caps and a space either side of the content.
    pub const OVERHEAD: usize = 2;
}

/// A pill with no fill: the caps alone, dim, around content the caller
/// colours.
///
/// The filled pill is right once — the prompt is a single line, and it is
/// where the user acts. A row's second line is not once: four of them, times
/// every row on screen, and a light terminal becomes a field of dark blocks.
/// So the list keeps the shape and drops the fill.
///
/// Dropping it also gives the colour rule back. A fixed fill forces a fixed
/// foreground, which is why the filled version has to carry a hardcoded
/// colour per state; against the terminal's own background a state can wear
/// the terminal's own colour again, as it does everywhere else in the list.
fn chip(content: String, style: Style) -> Vec<Span<'static>> {
    vec![
        Span::styled(bubbles::LEFT, theme::muted()),
        Span::styled(content, style),
        Span::styled(bubbles::RIGHT, theme::muted()),
    ]
}

/// One pill: a cap, the content on the fill, and a cap.
fn bubble(content: String, fg: ratatui::style::Color) -> Vec<Span<'static>> {
    vec![
        Span::styled(bubbles::LEFT, Style::default().fg(bubbles::FILL)),
        Span::styled(content, Style::default().fg(fg).bg(bubbles::FILL)),
        Span::styled(bubbles::RIGHT, Style::default().fg(bubbles::FILL)),
    ]
}

/// What the prompt says before the cursor: which agent will take the task,
/// and where it will run.
///
/// Both used to live on a second line under the prompt, which meant reading
/// down to answer "where is this going?" while typing across. They are the
/// subject of the sentence the user is writing, so they belong in front of
/// it, the way a shell puts the directory in front of the command.
fn prompt_prefix(app: &App, width: usize, style: Skin) -> Vec<Span<'static>> {
    let provider = app.draft.provider;

    // At most a third of the line, so there is always room to type. The
    // agent is short and fixed; the path is the part that has to give.
    let room = (width / 3).min(40);

    match style {
        Skin::Plain => vec![
            Span::styled(
                format!("{} {}", provider.icon(), provider.label()),
                theme::accent(),
            ),
            Span::styled(
                format!("  {}  ", short_path_within(&app.new_session_cwd(), room)),
                theme::muted(),
            ),
        ],
        Skin::Bubbles => {
            let path = short_path_within(&app.new_session_cwd(), room.saturating_sub(bubbles::OVERHEAD + 2));
            let mut spans = bubble(
                format!(" {} {} ", provider.icon(), provider.label()),
                bubbles::AGENT,
            );
            spans.push(Span::raw(" "));
            spans.extend(bubble(format!(" {} {} ", bubbles::FOLDER, path), bubbles::PATH));
            // One space, and then whatever is being typed. The pill's own
            // rounded edge is the hard edge the eye needs to find where the
            // typing starts, so there is nothing left for a caret to do.
            spans.push(Span::raw(" "));
            spans
        }
    }
}

fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(|s| s.content.as_ref().width()).sum()
}

/// The new-session prompt. It is always live — typing anywhere in the list
/// lands here — so it is drawn as an active field rather than as a hint.
fn draw_compose(frame: &mut Frame, outer: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(theme::rule());
    let area = with_margin(block.inner(outer));

    let mut spans = prompt_prefix(app, area.width as usize, Skin::current());
    if app.draft.buffer.is_empty() {
        spans.push(Span::styled("Describe a task for a new session", theme::muted()));
    } else {
        spans.push(Span::raw(app.draft.buffer.clone()));
        spans.push(Span::styled("_", theme::accent()));
    }

    frame.render_widget(block, outer);
    // Wrapping puts continuation lines at the left margin rather than under
    // the caret: a task long enough to wrap is easier to read on full-width
    // lines than in a column the width of what is left after the prefix.
    frame.render_widget(Paragraph::new(Line::from(spans)).wrap(Wrap { trim: false }), area);
}

/// A directory short enough to sit beside the prompt: `$HOME` becomes `~`,
/// and a still-long path keeps its last components, which are the ones that
/// identify the checkout. Worktree paths are long enough that the head is the
/// part worth dropping.
pub fn short_path(path: &std::path::Path) -> String {
    short_path_within(path, 40)
}

/// The same, but told how much room it has.
///
/// The prompt puts the directory in front of what is being typed, so on a
/// narrow terminal a long path would leave a few columns for the task. The
/// last components are the ones that identify the checkout, so the head is
/// what gets dropped.
pub fn short_path_within(path: &std::path::Path, max: usize) -> String {
    let max = max.max(8);
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
    if tilde.width() <= max {
        return tilde;
    }
    let parts: Vec<&str> = tilde.split('/').filter(|p| !p.is_empty()).collect();
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 1; // the leading ellipsis
    for part in parts.iter().rev() {
        let next = used + part.width() + 1;
        if next > max && !kept.is_empty() {
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
const W_AGENT: usize = 8;
const W_ICON: usize = 2;
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
    theme::state(state)
}

/// What the footer says about the selected session: where it is, how it is
/// filed, and its short id. One line, always present, so moving the cursor
/// changes what it says and never where anything sits.
fn detail_text(v: &SessionView) -> String {
    let mut facts = short_path(&v.session.cwd);
    if v.workflow() != WorkflowState::Unclassified {
        facts.push_str(&format!("  ·  {}", v.workflow().label().to_lowercase()));
    }
    facts.push_str(&format!("  ·  {}", v.key().short_id()));
    if v.is_hidden() {
        facts.push_str("  ·  hidden");
    }
    if let Some(preview) = v.session.preview.as_deref().filter(|p| !p.trim().is_empty()) {
        facts.push_str(&format!("  ·  {}", text::sanitize(preview)));
    }
    facts
}

/// A session as two lines: the task, then everything that qualifies it.
/// Used by the status and workflow views.
fn card_lines(
    v: &SessionView,
    width: usize,
    now: DateTime<Utc>,
    selected: bool,
    suffix: &str,
    skin: Skin,
) -> Vec<Line<'static>> {
    let links = links_text(v);
    let links_w = if links.is_empty() { 0 } else { links.width() + 2 };
    let title_w = width.saturating_sub(W_GLYPH + W_ICON + links_w).max(4);

    // A session with no name falls back to its id. An id is not a title, so it
    // is not styled like one — bolding it draws the eye to the least
    // informative rows in the list.
    let (title, source) = v.title();
    let title_style = match (source, selected) {
        (TitleSource::ShortId, _) => theme::muted(),
        (_, true) => theme::accent().add_modifier(Modifier::BOLD),
        (_, false) => Style::default().add_modifier(Modifier::BOLD),
    };

    let mut first = vec![
        Span::styled(
            text::pad_to_width(state_glyph(v.state()), W_GLYPH),
            state_style(v.state()),
        ),
        // The provider's mark, dim so it identifies without competing with
        // the state glyph beside it or the title after it.
        Span::styled(
            text::pad_to_width(v.session.provider().icon(), W_ICON),
            theme::muted(),
        ),
        Span::styled(
            text::pad_to_width(&format!("{}{suffix}", text::sanitize(&title)), title_w),
            title_style,
        ),
    ];
    if links_w > 0 {
        first.push(Span::raw("  "));
        first.push(Span::styled(links, theme::link()));
    }

    // Always two lines. The selected row changes colour, never height: a row
    // that grew under the cursor pushed every row below it as the cursor
    // moved, and the list looked as though it was shuffling.
    vec![Line::from(first), meta_line(v, width, now, selected, skin)]
}

/// A card's second line: agent, state in words, where, and how long ago.
///
/// In the bubbles skin these are pills, the same four-piece sentence the
/// prompt makes of the agent and the directory, so a row and the prompt read
/// as the same design rather than two.
fn meta_line(v: &SessionView, width: usize, now: DateTime<Utc>, selected: bool, skin: Skin) -> Line<'static> {
    let mut place = v.repo();
    if let Some(b) = v.branch_or_worktree() {
        place.push_str(&format!(" / {b}"));
    }
    let age = v.age(now);

    if skin == Skin::Plain {
        let mut meta = vec![v.session.provider().label().to_string(), v.state_label()];
        meta.push(place);
        meta.extend(age);
        let second = format!("{}{}", " ".repeat(W_GLYPH), meta.join(" · "));
        // The selected card's second line keeps full contrast so it reads as
        // part of the row the cursor is on.
        let style = if selected {
            Style::default()
        } else {
            theme::muted()
        };
        return Line::styled(text::truncate_to_width(&second, width), style);
    }

    // The fixed pieces first, so what is left is what the place may have.
    let agent = format!(
        " {} {} ",
        v.session.provider().icon(),
        v.session.provider().label()
    );
    let state = format!(" {} ", v.state_label());
    let age_pill = age.map(|a| format!(" {a} "));
    let fixed = W_GLYPH
        + agent.width()
        + state.width()
        + age_pill
            .as_deref()
            .map_or(0, |a| a.width() + bubbles::OVERHEAD + 1)
        + bubbles::OVERHEAD * 3
        + 2;
    let room = width.saturating_sub(fixed).saturating_sub(3).max(4);

    // The selected card keeps full contrast so it reads as part of the row
    // the cursor is on; the rest recede.
    let quiet = if selected {
        Style::default()
    } else {
        theme::muted()
    };

    let mut spans = vec![Span::raw(" ".repeat(W_GLYPH))];
    spans.extend(chip(agent, quiet));
    spans.push(Span::raw(" "));
    spans.extend(chip(state, state_style(v.state())));
    spans.push(Span::raw(" "));
    spans.extend(chip(
        format!(" {} {} ", bubbles::FOLDER, text::truncate_to_width(&place, room)),
        quiet,
    ));
    if let Some(a) = age_pill {
        spans.push(Span::raw(" "));
        spans.extend(chip(a, quiet));
    }
    Line::from(spans)
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
        Span::raw(text::pad_to_width(
            &format!("{} {}", v.session.provider().icon(), v.session.provider().label()),
            W_AGENT,
        )),
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
        Span::styled(text::pad_to_width(&links_text(v), W_LINKS), theme::link()),
        Span::styled(text::pad_to_width(&age, 4), theme::muted()),
    ])
}

/// A group heading. In the repository view it carries the tally, which is what
/// lets a repo say what is inside it without being expanded.
/// A heading wears the colour of what it holds. In the status view every
/// member shares a state, so the heading takes that state's colour; a mixed
/// group — a repository — stays neutral and lets its tally carry the colour.
fn group_style(h: &GroupHeader) -> Style {
    let all = |n: usize| n > 0 && n == h.count;
    let base = if all(h.needs_you) {
        theme::state(RuntimeState::WaitingInput)
    } else if all(h.running) {
        theme::state(RuntimeState::Running)
    } else if all(h.resting) {
        theme::muted()
    } else {
        Style::default()
    };
    base.add_modifier(Modifier::BOLD)
}

fn group_line(h: &GroupHeader, view: View, width: usize) -> Line<'static> {
    // A folded group announces itself, so nothing is silently missing.
    let label = if h.collapsed {
        format!("▸ {}", h.label)
    } else {
        h.label.clone()
    };
    let left = vec![
        Span::styled(label, group_style(h)),
        Span::styled(format!(" {}", h.count), theme::muted()),
    ];
    if view != View::Repository {
        return Line::from(left);
    }

    // The tally's glyphs each wear their state's colour, so a repository row
    // says at a glance whether anything in it needs you.
    let tally: Vec<Span<'static>> = vec![
        Span::styled("●", theme::state(RuntimeState::WaitingInput)),
        Span::styled(format!(" {}  ", h.needs_you), theme::muted()),
        Span::styled("*", theme::state(RuntimeState::Running)),
        Span::styled(format!(" {}  ", h.running), theme::muted()),
        Span::styled("○", Style::default()),
        Span::styled(format!(" {}  ", h.idle), theme::muted()),
        Span::styled("·", theme::muted()),
        Span::styled(format!(" {}", h.resting), theme::muted()),
    ];
    let left_w: usize = left.iter().map(|s| s.content.width()).sum();
    let tally_w: usize = tally.iter().map(|s| s.content.width()).sum();
    let gap = width.saturating_sub(left_w).saturating_sub(tally_w);

    let mut spans = left;
    spans.push(Span::raw(" ".repeat(gap)));
    spans.extend(tally);
    Line::from(spans)
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

    // The cursor is a bar in the margin, not a reversed row. Every line of the
    // selected item carries it, so a three-line card reads as one thing.
    let gutter = |lines: Vec<Line<'static>>, selected: bool| -> Vec<Line<'static>> {
        lines
            .into_iter()
            .map(|mut line| {
                let mark = if selected {
                    Span::styled("▌ ", theme::accent())
                } else {
                    Span::raw("  ")
                };
                line.spans.insert(0, mark);
                line
            })
            .collect()
    };

    let items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| match row {
            // A blank line above each heading but the first, so groups read
            // as separate sections, and one below it, so the heading stands
            // off from its rows. A folded heading has no rows and takes none.
            ListRow::Group(h) => {
                let line = group_line(h, app.view, total);
                let mut lines = Vec::with_capacity(3);
                if i != 0 {
                    lines.push(Line::raw(""));
                }
                lines.push(line);
                if !h.collapsed {
                    lines.push(Line::raw(""));
                }
                ListItem::new(gutter(lines, false))
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
                let lines = if app.view.is_cards() {
                    card_lines(v, total, now, selected, &suffix, Skin::current())
                } else {
                    vec![compact_line(v, total, now, &suffix)]
                };
                ListItem::new(gutter(lines, selected)).style(style)
            }
        })
        .collect();

    // Selection is drawn by the gutter above; the widget only scrolls to it.
    let list = List::new(items).highlight_symbol("");
    let mut state = ListState::default();
    state.select(selected_index);
    frame.render_stateful_widget(list, area, &mut state);
}

/// One line of keys for what you are doing now, and nothing else unless
/// something is wrong. A working provider does not need to announce itself,
/// and the full key list is behind `?` rather than across the screen.
fn draw_status(frame: &mut Frame, area: Rect, app: &App) {
    let area = with_margin(area);
    let width = area.width as usize;
    let mut lines = Vec::new();

    // The selected session's details live here, on a line that is always
    // present, rather than on the row itself.
    let detail = app.selected_view().map(|v| detail_text(&v)).unwrap_or_default();
    lines.push(Line::from(text::truncate_to_width(&detail, width)));

    if let Some(notice) = &app.notice {
        lines.push(Line::from(text::truncate_to_width(notice, width)));
    }

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

    let hint = match app.input_mode {
        // Every other key belongs to the shell, so the only one worth naming
        // is the one that comes back.
        InputMode::Shell => "shift-tab back to the list · the shell has every other key",
        InputMode::Task if !app.draft.buffer.trim().is_empty() => {
            "enter to start · tab for agent · esc to clear"
        }
        InputMode::Task => {
            "type to start a session · enter to attach · shift-tab for a shell · ? for shortcuts"
        }
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
                "tab             the new session's agent".into(),
                "shift-tab       put your shell in the text area, and come back from it".into(),
                "                it is a real terminal: completion, abbreviations and history".into(),
                "                are your shell's. Where it ends up is where a session starts.".into(),
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
                "◆ Claude   ▲ Codex      ● needs you   * working   ○ idle   · stopped   ? unconfirmed".into(),
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
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::accent())
        .title(Span::styled(
            format!(" {title} "),
            theme::accent().add_modifier(Modifier::BOLD),
        ));
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

    /// A shell that has not drawn anything yet says so. Starting one costs
    /// whatever the user's configuration costs — seconds, where `direnv`
    /// sources a nix dev shell — and an empty box in the meantime reads as
    /// broken rather than busy.
    #[tokio::test]
    async fn a_shell_that_has_not_drawn_yet_says_it_is_starting() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        app.shell_cwd = dir.path().to_path_buf();
        app.input_mode = crate::app::InputMode::Shell;
        app.open_shell(std::path::Path::new("/bin/sh"), 8, 96, || {});
        if app.pty.is_none() {
            eprintln!("no pty available here; skipping");
            return;
        }

        // Drawn before the shell has had time to write: spawning a process
        // takes milliseconds and this render takes microseconds.
        let drawn = render(&app, 100, 30);
        assert!(drawn.contains("starting /bin/sh"), "{drawn}");

        // And it is replaced by the shell itself once there is a prompt.
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let now = render(&app, 100, 30);
            if !now.contains("starting /bin/sh") {
                return;
            }
        }
        panic!("the starting line never gave way to the shell");
    }

    /// The text area holds the shell's own screen: the command's output,
    /// and on the bottom row the shell's prompt, where agnt's would be.
    #[tokio::test]
    async fn the_text_area_holds_the_shell_and_what_it_printed() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![session("a", "fix a flaky test", RuntimeState::Idle)]);
        app.shell_cwd = dir.path().to_path_buf();
        app.input_mode = crate::app::InputMode::Shell;
        // Sized the way the loop sizes it, so the shell wraps its own output
        // to the box it is actually drawn in.
        let (rows, cols) = shell_area_size(Rect::new(0, 0, 100, 30));
        // A plain, fast shell: this is about the box, not about whose shell
        // ends up in it.
        app.open_shell(std::path::Path::new("/bin/sh"), rows, cols, || {});
        if app.pty.is_none() {
            eprintln!("no pty available here; skipping");
            return;
        }

        // What the shell itself has drawn, which is what the box shows.
        let shell_screen = |app: &App| -> String {
            app.pty
                .as_ref()
                .and_then(|p| p.parser().lock().ok().map(|g| g.screen().contents()))
                .unwrap_or_default()
        };

        // Wait for the shell's own prompt before typing, as a user would:
        // keys sent to a shell that has not started yet are echoed by the tty
        // rather than by the shell, and land somewhere else.
        for _ in 0..200 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if !shell_screen(&app).trim().is_empty() {
                break;
            }
        }
        app.pty.as_mut().unwrap().send(b"echo shell-canary\r");

        // Wait for the output, not merely the echo of what was typed.
        for _ in 0..200 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if shell_screen(&app).lines().any(|l| l.trim() == "shell-canary") {
                break;
            }
        }
        let drawn = render(&app, 100, 30);
        assert!(
            drawn.lines().any(|l| l.trim() == "shell-canary"),
            "the shell's output never reached the text area\n{drawn}"
        );
        // The list is still there above it, and agnt's own prompt is not.
        assert!(has(&drawn, "fix a flaky test"), "{drawn}");
        assert!(!drawn.contains("Describe a task for a new session"), "{drawn}");
        assert!(drawn.contains("shift-tab back to the list"), "{drawn}");

        // The shell sits in one box where the task prompt's box was, not in a
        // pane of its own: two rules, the same as task mode.
        let lines: Vec<&str> = drawn.lines().collect();
        let rules: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.starts_with("───"))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(rules.len(), 2, "expected one box, got {rules:?}\n{drawn}");

        // The box is as tall as the shell is using: the prompt is its last
        // row and there is no blank space padding it out.
        let (top, bottom) = (rules[0], rules[1]);
        assert!(
            !lines[bottom - 1].trim().is_empty(),
            "the prompt is not on the box's last row\n{drawn}"
        );
        for line in &lines[top + 1..bottom] {
            assert!(!line.trim().is_empty(), "the box has blank rows\n{drawn}");
        }
        // The height follows the content rather than being a fixed block of
        // mostly-blank rows. How many rows a shell uses for a prompt is the
        // shell's business, so the bound is the cap, not an exact number.
        let content_rows = bottom - top - 1;
        assert!(
            (1..=shell_rows_cap(30)).contains(&(content_rows as u16)),
            "box is {content_rows} rows, outside 1..={}\n{drawn}",
            shell_rows_cap(30)
        );
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
        assert!(has(&out, "Needs you 1"), "{out}");
        assert!(has(&out, "Fix the auth handler"), "{out}");
        assert!(has(&out, "PR #123"), "{out}");
        // The second line qualifies the first: agent, state in words, place.
        for needle in ["Claude", "Needs approval", "api"] {
            assert!(has(&out, needle), "{needle} missing in\n{out}");
        }
        assert!(has(&out, "Idle 1"), "{out}");
    }

    /// A row's second line is the same four-piece sentence the prompt makes
    /// of the agent and the directory, drawn the same way, so a row and the
    /// prompt read as one design rather than two.
    #[test]
    fn a_rows_second_line_is_bubbles_too() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with(
            &dir,
            vec![session(
                "a",
                "Fix the auth handler",
                RuntimeState::WaitingApproval,
            )],
        );
        let v = app.rows().iter().find_map(|r| match r {
            ListRow::Session(v) => Some(v.clone()),
            _ => None,
        });
        let v = v.expect("a session row");

        let line = meta_line(&v, 120, chrono::Utc::now(), false, Skin::Bubbles);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text.matches(bubbles::LEFT).count(), 3, "{text:?}");
        assert!(text.contains(bubbles::FOLDER), "{text:?}");
        assert!(text.contains("Claude") && text.contains("Needs approval"));

        // Nothing in a row is filled. Four filled pills times every row on
        // screen is a field of dark blocks on a light terminal; the prompt is
        // one line and can afford it, the list cannot.
        assert!(
            line.spans.iter().all(|s| s.style.bg.is_none()),
            "a row must not be filled: {:?}",
            line.spans
        );
        // And because it is not filled, a state wears the terminal's own
        // colour here, the same as its glyph on the line above.
        let waiting = line
            .spans
            .iter()
            .find(|s| s.content.contains("Needs approval"))
            .expect("a state chip");
        assert_eq!(waiting.style, state_style(v.state()), "{waiting:?}");

        // The prompt, by contrast, keeps its fill.
        let prompt = prompt_prefix(&app, 100, Skin::Bubbles);
        assert!(
            prompt.iter().any(|s| s.style.bg == Some(bubbles::FILL)),
            "{prompt:?}"
        );

        // The plain skin keeps the separated form, and no glyphs.
        let plain = meta_line(&v, 120, chrono::Utc::now(), false, Skin::Plain);
        let text: String = plain.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("Claude · Needs approval · api"), "{text:?}");
        assert!(
            !text.chars().any(|c| ('\u{e000}'..='\u{f8ff}').contains(&c)),
            "{text:?}"
        );
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
        assert!(has(&out, "api 2"), "{out}");
        assert!(has(&out, "● 1  * 0  ○ 1  · 0"), "{out}");
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
        let second = heading("Idle 1");

        // A heading stands off from its rows by one blank line.
        assert!(lines[first + 1].trim().is_empty(), "{out}");
        assert!(!lines[first + 2].trim().is_empty(), "{out}");
        // The next section is preceded by a blank line.
        assert!(lines[second - 1].trim().is_empty(), "{out}");
        assert!(second > first + 2, "{out}");
    }

    /// The header, the prompt and the footer share a left margin equal to the
    /// list's gutter, so nothing sits flush against the terminal's edge.
    #[test]
    fn the_chrome_shares_the_gutter_margin() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_with(&dir, vec![session("a", "x", RuntimeState::Idle)]);
        let out = render(&app, 120, 24);
        for needle in ["agnt", "Describe a task", "? for shortcuts"] {
            let line = out.lines().find(|l| l.contains(needle)).unwrap();
            assert!(line.starts_with("  "), "{needle:?} is flush left:\n{out}");
        }
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
        let rule = |offset: isize| {
            let i = (prompt as isize + offset) as usize;
            let line: &str = out.lines().nth(i).unwrap();
            assert!(
                line.chars().all(|c| c == '\u{2500}'),
                "expected a rule at {offset:+}, got {line:?}\n{out}"
            );
        };
        // The prompt is a box: a rule above the text and another below it, so
        // it never reads as one more row of the list.
        rule(-1);
        rule(1);
    }

    /// The prompt says where the task is going, in front of the task: which
    /// agent will take it and which directory it starts in. Both used to sit
    /// on a line of their own underneath.
    #[test]
    fn the_prompt_says_which_agent_and_where_before_the_task() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        app.shell_cwd = std::path::PathBuf::from("/tmp/some/checkout");

        let line_with = |app: &App, needle: &str| -> String {
            render(app, 100, 24)
                .lines()
                .find(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("no {needle:?} in\n{}", render(app, 100, 24)))
                .to_string()
        };

        let empty = line_with(&app, "Describe a task");
        assert!(empty.contains("Claude"), "{empty}");
        assert!(empty.contains("/tmp/some/checkout"), "{empty}");
        // Both come before the text, not after it.
        let text_at = empty.find("Describe a task");
        assert!(
            empty.find("Claude") < text_at && empty.find("/tmp/some/checkout") < text_at,
            "{empty}"
        );

        // The agent follows Tab, since that is what Enter will start.
        app.draft.provider = Provider::Codex;
        app.draft.buffer = "fix the billing test".into();
        let typed = line_with(&app, "fix the billing test");
        assert!(typed.contains("Codex") && !typed.contains("Claude"), "{typed}");
        assert!(
            typed.find("/tmp/some/checkout") < typed.find("fix the billing test"),
            "{typed}"
        );

        // And it is one line: the box is a rule, the prompt, a rule.
        let out = render(&app, 100, 24);
        let at = out
            .lines()
            .position(|l| l.contains("fix the billing test"))
            .unwrap();
        for offset in [at - 1, at + 1] {
            let rule: &str = out.lines().nth(offset).unwrap();
            assert!(rule.chars().all(|c| c == '\u{2500}'), "{rule:?}\n{out}");
        }
    }

    /// The bubbles are `bubblesextra`'s: a rounded cap, the content on the
    /// fill, a rounded cap, one pill per thing the prompt has to say.
    #[test]
    fn the_prompt_is_drawn_as_bubbles() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        app.shell_cwd = std::path::PathBuf::from("/tmp/some/checkout");

        let spans = prompt_prefix(&app, 100, Skin::Bubbles);
        let line: String = spans.iter().map(|s| s.content.as_ref()).collect();

        // Two pills, so two of each cap, and the folder mark in the second.
        assert_eq!(line.matches(bubbles::LEFT).count(), 2, "{line:?}");
        assert_eq!(line.matches(bubbles::RIGHT).count(), 2, "{line:?}");
        assert!(line.contains(bubbles::FOLDER), "{line:?}");
        assert!(line.contains("Claude") && line.contains("/tmp/some/checkout"));

        // The caps are the fill colour against the terminal's own background,
        // which is what makes the fill read as a rounded pill rather than a
        // rectangle. Getting this wrong is invisible in a text dump.
        let caps: Vec<&Span> = spans
            .iter()
            .filter(|s| s.content == bubbles::LEFT || s.content == bubbles::RIGHT)
            .collect();
        assert_eq!(caps.len(), 4);
        for cap in caps {
            assert_eq!(cap.style.fg, Some(bubbles::FILL), "{cap:?}");
            assert_eq!(cap.style.bg, None, "a cap must not be filled: {cap:?}");
        }
        // And the content between them is.
        let filled = spans.iter().filter(|s| s.style.bg == Some(bubbles::FILL));
        assert_eq!(filled.count(), 2);
    }

    /// Every glyph in the prompt is one cell wide, or every column after it
    /// is out by one. This caught a bad glyph choice once already.
    #[test]
    fn every_prompt_glyph_is_one_cell() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        app.draft.provider = Provider::Codex;
        for style in [Skin::Bubbles, Skin::Plain] {
            let spans = prompt_prefix(&app, 100, style);
            for span in &spans {
                for c in span.content.chars() {
                    assert_eq!(
                        unicode_width::UnicodeWidthChar::width(c),
                        Some(1),
                        "U+{:04X} in {style:?} is not one cell",
                        c as u32
                    );
                }
            }
        }
    }

    /// A terminal without a Nerd Font would draw the caps as blank boxes, and
    /// that cannot be detected from in here — so it is a choice, and the
    /// plain prompt has to keep working.
    #[test]
    fn the_plain_prompt_has_no_nerd_font_glyphs() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        app.shell_cwd = std::path::PathBuf::from("/tmp/some/checkout");

        let spans = prompt_prefix(&app, 100, Skin::Plain);
        let line: String = spans.iter().map(|s| s.content.as_ref()).collect();
        for glyph in [bubbles::LEFT, bubbles::RIGHT, bubbles::FOLDER] {
            assert!(!line.contains(glyph), "{line:?}");
        }
        // No private-use characters of any kind, and nothing filled.
        assert!(
            !line.chars().any(|c| ('\u{e000}'..='\u{f8ff}').contains(&c)),
            "{line:?}"
        );
        assert!(spans.iter().all(|s| s.style.bg.is_none()));
        // It still says the same things.
        assert!(line.contains("Claude") && line.contains("/tmp/some/checkout"));
        // And nothing stands between the directory and the task but a space.
        assert!(line.ends_with("  "), "{line:?}");
    }

    /// A directory long enough to crowd out the task is shortened, keeping
    /// the components that say which checkout it is.
    #[test]
    fn a_long_directory_gives_way_to_the_task() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(&dir, vec![]);
        app.shell_cwd =
            std::path::PathBuf::from("/Users/someone/src/github.com/an-org/a-long-repository-name");

        let wide = render(&app, 120, 24);
        let wide_line = wide.lines().find(|l| l.contains("Claude")).unwrap();
        assert!(wide_line.contains("a-long-repository-name"), "{wide_line}");

        let narrow = render(&app, 60, 24);
        let narrow_line = narrow.lines().find(|l| l.contains("Claude")).unwrap();
        assert!(narrow_line.contains('\u{2026}'), "{narrow_line}");
        // The tail is kept: it is what says which checkout this is.
        assert!(narrow_line.contains("a-long-repository-name"), "{narrow_line}");
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

    /// Each provider has a mark on the title line, so a Claude session and a
    /// Codex session can be told apart without reading the line below.
    #[test]
    fn the_title_line_carries_the_provider_mark() {
        let dir = tempfile::tempdir().unwrap();
        let mut codex = session("c", "Investigate a flaky test", RuntimeState::Running);
        codex.key = SessionKey::new(Provider::Codex, "c");
        let app = app_with(
            &dir,
            vec![session("a", "Fix the auth handler", RuntimeState::Idle), codex],
        );
        let out = render(&app, 120, 24);
        let claude = out.lines().find(|l| l.contains("Fix the auth handler")).unwrap();
        let codex = out
            .lines()
            .find(|l| l.contains("Investigate a flaky test"))
            .unwrap();
        assert!(claude.contains("◆"), "{claude:?}");
        assert!(!claude.contains("▲"), "{claude:?}");
        assert!(codex.contains("▲"), "{codex:?}");
        assert!(!codex.contains("◆"), "{codex:?}");

        // The compact view carries it in the agent column.
        let mut app = app;
        app.view = crate::model::View::Repository;
        let out = render(&app, 120, 24);
        assert!(has(&out, "◆ Claude"), "{out}");
        assert!(has(&out, "▲ Codex"), "{out}");
    }

    /// Every row keeps its place as the cursor moves. Only the cursor moves.
    #[test]
    fn rows_do_not_move_when_the_cursor_does() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(
            &dir,
            vec![
                session("a", "First", RuntimeState::Idle),
                session("b", "Second", RuntimeState::Idle),
                session("c", "Third", RuntimeState::Idle),
            ],
        );
        let positions = |app: &App| -> Vec<usize> {
            let out = render(app, 120, 30);
            ["First", "Second", "Third"]
                .iter()
                .map(|t| out.lines().position(|l| l.contains(t)).unwrap())
                .collect()
        };
        app.selected = Some(SessionKey::new(Provider::Claude, "a"));
        let on_first = positions(&app);
        app.selected = Some(SessionKey::new(Provider::Claude, "c"));
        let on_third = positions(&app);
        assert_eq!(on_first, on_third);
    }

    /// The selected session's details sit on one fixed footer line, below
    /// the prompt, not on the row.
    #[test]
    fn the_selected_details_are_on_a_fixed_footer_line() {
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
        let lines: Vec<&str> = out.lines().collect();

        let detail = lines
            .iter()
            .position(|l| l.contains("/Users/me/src/api"))
            .unwrap();
        let prompt = lines.iter().position(|l| l.contains("Describe a task")).unwrap();
        let title = lines
            .iter()
            .position(|l| l.contains("Fix the auth handler"))
            .unwrap();
        assert!(detail > prompt, "detail should be below the prompt\n{out}");
        assert!(detail > title, "{out}");
        assert_eq!(
            lines.iter().filter(|l| l.contains("/Users/me/src/api")).count(),
            1,
            "{out}"
        );
        assert!(!has(&out, "unclassified"), "{out}");
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
