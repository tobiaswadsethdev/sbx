//! Rendering.
//!
//! Almost pure: it reads app state and draws. The two things it writes back are
//! the list's scroll position (owned by ratatui's `ListState`) and the measured
//! height of the right pane, which only the renderer knows and which the key
//! handler needs in order to page and clamp scrolling.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, Paragraph, Wrap};
use tui_term::widget::PseudoTerminal;

use crate::events::Verdict;
use crate::ops;
use crate::pane;
use crate::repos::{Facts, LocalRepo};
use crate::session::{self, Session, State};
use crate::status::Source;
use crate::tui::create::{Create, Field, Form, Input, Picker};
use crate::tui::{App, Focus, RightView, term};

/// Width of the session-name column. Names are capped at 15 characters by the
/// gateway's sandbox-name limit, so this only ever truncates a near-maximal one.
const NAME_W: usize = 15;
/// Width of the `+12/-3 ?1` column.
const STAT_W: usize = 11;

/// The accent: borders, the active tab, the keys in the footer.
///
/// ANSI rather than a hard-coded violet, so a light terminal theme is still
/// legible -- the shapes are what make this recognisable, not one exact hue. Not
/// magenta, which belongs to the `waiting` badge and has to stay the only thing
/// on screen wearing it.
const ACCENT: Color = Color::LightBlue;
/// Everything that is present but not the point: labels, separators, second
/// lines, inactive tabs.
const DIM: Color = Color::DarkGray;

/// One colour per state, so the list is scannable without reading it.
///
/// `Waiting` is deliberately the odd one out: a filled badge rather than
/// coloured text. Noticing that an agent is blocked on you, in a list you are
/// not currently looking at, is the entire reason to run several sessions at
/// once, so it gets to be louder than everything else.
fn state_style(state: State) -> Style {
    if state == State::Waiting {
        return Style::default()
            .bg(Color::Magenta)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD);
    }
    let colour = match state {
        State::Ready => Color::Green,
        State::Running => Color::Cyan,
        State::Waiting => unreachable!("handled above"),
        State::Creating | State::Seeding => Color::Yellow,
        State::Idle => Color::Blue,
        State::Published => Color::LightGreen,
        State::Failed => Color::Red,
        State::Dead => Color::DarkGray,
    };
    Style::default().fg(colour)
}

/// A block whose border shows whether the pane has the movement keys.
fn pane(title: impl Into<Line<'static>>, focused: bool) -> Block<'static> {
    let block = Block::bordered().title(title.into());
    if focused {
        block
            .border_type(BorderType::Thick)
            .border_style(Style::default().fg(ACCENT))
    } else {
        block.border_style(Style::default().fg(DIM))
    }
}

/// The right pane's views as tabs along its top border.
///
/// On the border rather than in a row of their own: it is where the eye already
/// is, and the pane keeps every row of its height -- which matters most for the
/// one view that is a live terminal.
fn tabs(active: RightView) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (i, view) in RightView::ORDER.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", Style::default().fg(DIM)));
        }
        let style = if *view == active {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(DIM)
        };
        spans.push(Span::styled(view.label(), style));
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}

/// The name of the session a pane is showing, for the other end of its border.
fn pane_subject(text: String) -> Line<'static> {
    Line::from(Span::styled(format!(" {text} "), Style::default().fg(DIM))).right_aligned()
}

/// Rows the session list keeps whatever else wants room. Two borders and three
/// rows: enough to see the selected session and a neighbour either side, which
/// is the least that can still be navigated.
const LIST_MIN_H: u16 = 5;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [main, footer] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)]).areas(main);

    // The session's facts sit under the list rather than at the top of the
    // preview, so they stay on screen when the right-hand pane is given over to
    // the agent's terminal -- which, now that the terminal lives in there, is
    // most of the time. The left column carries what a session *is*; the right
    // one carries whatever you are looking at.
    let session = app.selected().cloned();
    let inner_w = left.width.saturating_sub(2) as usize;
    let meta = session
        .as_ref()
        .map(|s| (s.name.clone(), meta_lines(app, s, inner_w)));

    // Sized to its content, capped so the list always has somewhere to go. An
    // empty list has no facts to show, and the list takes the whole column.
    let meta_h = match &meta {
        Some((_, lines)) => {
            (wrapped_height(lines, inner_w) as u16 + 2).min(left.height.saturating_sub(LIST_MIN_H))
        }
        None => 0,
    };
    let [list_area, meta_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(meta_h)]).areas(left);

    draw_list(frame, app, list_area);
    if let Some((name, lines)) = meta {
        draw_meta(frame, meta_area, &name, lines);
    }
    draw_right(frame, app, right);
    draw_footer(frame, app, footer);
    // Last, and over everything: the create flow is modal, and it owns the
    // keyboard while it is up.
    draw_create(frame, app, frame.area());
}

/// Columns the selection marker takes on every row, selected or not, so the
/// content does not shift sideways as the cursor moves.
const MARKER_W: usize = 2;

/// One session, over two rows and a gap.
///
/// Two rows because a session is two different questions -- *which* one is this,
/// and *where has it got to* -- and answering both on one line meant truncating
/// the name to fifteen columns and leaving nowhere for the branch. The gap is
/// what makes a list of five scannable rather than a wall.
///
/// The number is for the digit keys: `3` selects the third session, which is the
/// fastest way to a specific agent once there are a few.
fn session_item(
    app: &App,
    index: usize,
    session: &Session,
    now: u64,
    width: usize,
    selected: bool,
) -> ListItem<'static> {
    let state = app.effective_state(session);
    let stat = app.poll(&session.name).and_then(|p| p.stat);
    let age = session::humanize_age(session.created_at, now);

    // Line one: what it is, and what the agent is doing.
    let head = format!("{:>2}. ", index + 1);
    let state_text = state.to_string();
    // The dot repeats the colour the word already carries, for reading the
    // column at a glance without reading any of it.
    let dot_w = 2;
    let name_room =
        width.saturating_sub(head.chars().count() + state_text.chars().count() + dot_w + 1);
    let name = truncate(&session.name, name_room.max(4));
    let gap = width.saturating_sub(
        head.chars().count() + name.chars().count() + state_text.chars().count() + dot_w,
    );

    let first = Line::from(vec![
        Span::styled(head, Style::default().fg(DIM)),
        Span::styled(name, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" ".repeat(gap)),
        state_span(state, selected),
        Span::raw(" "),
        Span::styled("●", Style::default().fg(state_dot(state))),
    ]);

    // Line two: the branch, then what has changed and how long it has been.
    let stat_spans = stat_spans(stat);
    let stat_w: usize = stat_spans.iter().map(|s| s.content.chars().count()).sum();
    let age_w = age.chars().count() + 1;
    let branch_room = width.saturating_sub(4 + stat_w + age_w);
    let branch = truncate(&session.work_branch, branch_room.max(4));
    let pad = width.saturating_sub(4 + branch.chars().count() + stat_w + age_w);

    let mut spans = vec![
        Span::raw("    ".to_string()),
        Span::styled(branch, Style::default().fg(DIM)),
        Span::raw(" ".repeat(pad)),
    ];
    spans.extend(stat_spans);
    spans.push(Span::styled(format!(" {age}"), Style::default().fg(DIM)));

    ListItem::new(vec![first, Line::from(spans), Line::from("")])
}

/// The state, as it appears on a row.
///
/// `Waiting` is a filled badge everywhere else in the interface, but a filled
/// badge cannot sit inside the selection's own fill: a list's highlight style is
/// patched *over* the row, so the badge's background is replaced and its black
/// text lands on dark grey. On the selected row it becomes bright magenta text
/// instead -- still the only magenta on screen, still unmissable, and legible.
fn state_span(state: State, selected: bool) -> Span<'static> {
    let text = state.to_string();
    if state == State::Waiting && selected {
        return Span::styled(
            text,
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        );
    }
    Span::styled(text, state_style(state))
}

/// The same colour as the state word, as a dot.
fn state_dot(state: State) -> Color {
    if state == State::Waiting {
        return Color::Magenta;
    }
    state_style(state).fg.unwrap_or(Color::Reset)
}

fn draw_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let now = session::now_epoch();
    let focused = app.focus == Focus::List;
    // What a row has to itself: the border either side, and the marker column.
    let width = (area.width as usize)
        .saturating_sub(2 + MARKER_W)
        .max(NAME_W);

    let items: Vec<ListItem> = app
        .sessions
        .iter()
        .enumerate()
        .map(|(i, s)| session_item(app, i, s, now, width, app.list_state.selected() == Some(i)))
        .collect();

    let title = Line::from(vec![
        Span::styled(
            " sessions ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("{} ", app.sessions.len()), Style::default().fg(DIM)),
    ]);
    // The count of blocked agents, as loud as the badge on the row it refers to,
    // and at the other end of the border so it is legible even when that row is
    // scrolled out of view.
    let waiting = app.waiting_count();
    let mut block = pane(title, focused);
    if waiting > 0 {
        block = block.title(
            Line::from(Span::styled(
                format!(" {waiting} waiting "),
                Style::default()
                    .bg(Color::Magenta)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD),
            ))
            .right_aligned(),
        );
    } else if app.refreshing {
        block = block.title(
            Line::from(Span::styled(" refreshing ", Style::default().fg(DIM))).right_aligned(),
        );
    }

    if items.is_empty() {
        let hint = Paragraph::new(vec![
            Line::from(""),
            Line::from("  no sessions yet").style(Style::default().fg(DIM)),
            Line::from(""),
            Line::from("  press n to start one").style(Style::default().fg(DIM)),
        ])
        .block(block);
        frame.render_widget(hint, area);
        return;
    }

    let list = List::new(items)
        .block(block)
        // A quiet fill for the selection and a bar in the accent, rather than
        // reversing the row: reversed video turns every coloured span inside out,
        // which makes a state word unreadable exactly when it is selected.
        .highlight_style(Style::default().bg(Color::Indexed(236)))
        .highlight_symbol("▌ ");

    frame.render_stateful_widget(list, area, &mut app.list_state);
}

/// The `+12/-3 ?` column, padded to a fixed width so the age column lines up.
///
/// Blank until the first stat arrives rather than showing a placeholder zero: a
/// session that has not been measured yet and one with no changes are different
/// things, and `+0/-0` would claim the agent has done nothing.
///
/// Untracked files are a bare `?` rather than a count. The count would push the
/// column past the width the list can afford on an 80-column terminal, and "the
/// agent created files it has not committed" is the part worth knowing at a
/// glance; the diff pane lists them.
fn stat_spans(stat: Option<ops::DiffStat>) -> Vec<Span<'static>> {
    let Some(stat) = stat else {
        return vec![Span::raw(" ".repeat(STAT_W))];
    };
    if stat.is_empty() {
        return vec![Span::styled(
            format!("{:>w$}", "clean", w = STAT_W),
            Style::default().fg(DIM),
        )];
    }

    let added = format!("+{}", compact(stat.added));
    let removed = format!("-{}", compact(stat.removed));
    let untracked = if stat.untracked > 0 { " ?" } else { "" };
    let used = added.len() + 1 + removed.len() + untracked.len();

    // Padded on the left, so the column ends where the age begins rather than
    // trailing off towards it.
    let mut spans = vec![
        Span::raw(" ".repeat(STAT_W.saturating_sub(used))),
        Span::styled(added, Style::default().fg(Color::Green)),
        Span::styled("/", Style::default().fg(DIM)),
        Span::styled(removed, Style::default().fg(Color::Red)),
    ];
    if !untracked.is_empty() {
        spans.push(Span::styled(untracked, Style::default().fg(DIM)));
    }
    spans
}

/// The agent's live screen, or an invitation to start it.
///
/// Nothing is drawn until it has been asked for: a terminal is a held process,
/// and cycling the right pane past this view should not spend one. The title
/// carries the way out, because a pane that has taken the keyboard has to say
/// how to give it back.
fn draw_agent(frame: &mut Frame, app: &mut App, area: Rect, session: &crate::session::Session) {
    let has_keyboard = app.focus == Focus::Agent;

    if !app.agent_is_open() {
        let hint = Paragraph::new(vec![
            Line::from(""),
            Line::from(format!("  {} is running in its sandbox.", session.name))
                .style(Style::default().fg(Color::DarkGray)),
            Line::from(""),
            Line::from("  press enter to open its terminal here")
                .style(Style::default().fg(Color::DarkGray)),
            Line::from("  or a to hand the whole terminal over")
                .style(Style::default().fg(Color::DarkGray)),
        ])
        .block(pane(tabs(RightView::Agent), false).title(pane_subject(session.name.clone())));
        frame.render_widget(hint, area);
        return;
    }

    // The pty is told the size of the *inner* area, so the agent draws exactly
    // what fits inside the border. Done here because the renderer is the only
    // place that knows it, and every frame, because a terminal resize has to
    // reach the agent's tmux.
    let cols = area.width.saturating_sub(2);
    let rows = area.height.saturating_sub(2);
    app.resize_agent(cols, rows);

    let subject = if has_keyboard {
        format!("{} · {}", session.name, term::ESCAPE_HINT)
    } else {
        format!("{} · enter to type", session.name)
    };

    let Some((parser, exited)) = app.agent_screen() else {
        return;
    };
    let subject = if exited {
        format!("{} · attach ended", session.name)
    } else {
        subject
    };
    // Hide our cursor when the pane does not have the keyboard: two cursors on
    // screen, one of them inert, is worse than none.
    let cursor = tui_term::widget::Cursor::default();
    let mut term = PseudoTerminal::new(parser.screen())
        .block(pane(tabs(RightView::Agent), has_keyboard).title(pane_subject(subject)))
        .cursor(cursor);
    if !has_keyboard {
        term = term.cursor({
            let mut c = tui_term::widget::Cursor::default();
            c.hide();
            c
        });
    }
    frame.render_widget(term, area);
}

/// Abbreviate a line count to at most three columns, so a huge diff cannot
/// widen the column and push the age off the pane. Precision above a few
/// thousand lines is worthless in a list anyway.
fn compact(n: u32) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=9_999 => format!("{}k", n / 1000),
        _ => "9k+".to_string(),
    }
}

fn draw_right(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Right;
    let view = app.right_view();
    // Inner area: the border takes a row top and bottom, a column either side.
    let inner_w = area.width.saturating_sub(2) as usize;
    let inner_h = area.height.saturating_sub(2) as usize;

    let Some(session) = app.selected().cloned() else {
        let empty = Paragraph::new("").block(pane(tabs(view), focused));
        frame.render_widget(empty, area);
        app.right_lines = 0;
        app.right_height = inner_h;
        return;
    };

    // The agent's terminal is not text this function lays out -- it is a screen
    // something else already laid out -- so it leaves here rather than being
    // squeezed through the paragraph path below.
    if view == RightView::Agent {
        draw_agent(frame, app, area, &session);
        app.right_lines = 0;
        app.right_height = inner_h;
        return;
    }

    // Both produce owned lines, so no borrow of `app` outlives this call and the
    // measurements below can be written back.
    let (lines, wrap) = match view {
        RightView::Preview => (preview_lines(app, &session), true),
        RightView::Diff => (diff_lines(app, &session), false),
        // Wrapped: a policy is prose as much as data, and a notice that says
        // why a section cannot be changed is worth more than the alignment.
        RightView::Policy => (policy_lines(app, &session), true),
        // Not wrapped, unlike the policy. A feed is read by scanning the time
        // and verdict columns, and a wrapped continuation starts at column
        // zero -- which puts a fragment of a URL where a verdict should be and
        // makes the whole pane unscannable. Long lines are clipped instead;
        // `sbx events` prints them in full.
        RightView::Events => (event_lines(app, &session), false),
        // Handled above, where a screen can be drawn as a screen.
        RightView::Agent => unreachable!("the agent view leaves before this"),
    };

    // With wrapping on, one logical line can occupy several rows, and
    // `Paragraph::scroll` counts rows. Measuring the wrapped height keeps the
    // end of a wrapped preview reachable.
    app.right_lines = if wrap {
        wrapped_height(&lines, inner_w)
    } else {
        lines.len()
    };
    app.right_height = inner_h;

    // Content can shrink between renders, so clamp rather than trust the stored
    // offset.
    let offset = app.right_scroll().min(app.max_scroll());
    if offset != app.right_scroll() {
        app.scroll
            .entry(session.name.clone())
            .or_default()
            .set(view, offset);
    }

    let position = if app.right_lines > inner_h {
        format!(
            "  [{}/{}]",
            offset as usize + inner_h.min(app.right_lines),
            app.right_lines
        )
    } else {
        String::new()
    };
    // The view is named by its tab; the border's other end says which session it
    // is of, and how far down it you are. Events are the one view whose contents
    // need a word of explanation, so the clock lives there rather than in a tab.
    let subject = if view == RightView::Events {
        format!("{} · UTC{position}", session.name)
    } else {
        format!("{}{position}", session.name)
    };

    let mut para = Paragraph::new(lines)
        .block(pane(tabs(view), focused).title(pane_subject(subject)))
        .scroll((offset, 0));
    if wrap {
        para = para.wrap(Wrap { trim: false });
    }
    frame.render_widget(para, area);
}

/// Rows a wrapped paragraph will occupy. Approximate by design: it counts
/// characters rather than grapheme clusters, which is close enough to bound
/// scrolling and costs nothing.
fn wrapped_height(lines: &[Line<'_>], width: usize) -> usize {
    if width == 0 {
        return lines.len();
    }
    lines
        .iter()
        .map(|l| {
            let chars: usize = l.spans.iter().map(|s| s.content.chars().count()).sum();
            chars.div_ceil(width).max(1)
        })
        .sum()
}

/// How many rows of the task the facts pane will show.
///
/// A task is a whole prompt and can be a paragraph. Everything under it --
/// branch, sandbox, policy, what the agent is doing -- is short and fixed, and
/// letting a long prompt push *those* out of the pane would hide the fields that
/// are checked most often. So the prompt gives way instead, and the preview pane
/// carries it in full.
const TASK_ROWS: usize = 2;

/// The facts about a session: what it is, and what its agent is doing.
///
/// Split out from the preview it used to head, so it can be drawn in the left
/// column and stay visible whatever the right-hand pane is showing.
///
/// Every fact is exactly one row, cut to `width` rather than wrapped, so the
/// pane is precisely as tall as this is long. Wrapping was the first attempt and
/// it silently ate the last field: a URL is one unbreakable word, so a wrapped
/// paragraph is taller than any character count predicts, and the overflow falls
/// off the bottom of a pane sized from that prediction. What is cut here is
/// visible in full in the preview pane, which has the room to wrap.
fn meta_lines(app: &App, session: &Session, width: usize) -> Vec<Line<'static>> {
    let value_w = width.saturating_sub(FIELD_W).max(1);

    let mut lines = Vec::new();
    if session.task.is_empty() {
        lines.push(fact("task", "-", value_w));
    } else {
        // The prompt over as many rows as it is allowed. Continuation rows leave
        // the label column empty, so the values still line up under each other.
        let chars: Vec<char> = session.task.chars().collect();
        let rows: Vec<String> = chars
            .chunks(value_w)
            .take(TASK_ROWS)
            .map(|c| c.iter().collect())
            .collect();
        let cut_short = chars.len() > rows.len() * value_w;
        for (i, row) in rows.iter().enumerate() {
            let label = if i == 0 { "task" } else { "" };
            // Only the last row can be missing the rest of the prompt, and only
            // then is there anything to say about it.
            let text = if cut_short && i + 1 == rows.len() {
                truncate(row, value_w)
            } else {
                row.clone()
            };
            lines.push(fact(label, &text, value_w));
        }
    }

    for (label, value) in [
        ("repo", session.repo.as_str()),
        ("branch", session.work_branch.as_str()),
        ("sandbox", session.sandbox.as_str()),
        ("policy", session.policy.as_deref().unwrap_or("(default)")),
        ("agent", session.agent.as_str()),
    ] {
        lines.push(fact(label, value, value_w));
    }
    let providers = session.providers.join(", ");
    if !providers.is_empty() {
        lines.push(fact("providers", &providers, value_w));
    }
    lines.push(status_line(app, session));
    // A publish takes a push plus a REST call, so there is a visible gap where
    // nothing appears to be happening.
    if app.publishing() == Some(session.name.as_str()) {
        lines.push(progress(
            "publish",
            "pushing and opening a pull request ...",
        ));
    }
    // Deleting a sandbox is a gateway round trip, and the row stays put until
    // it comes back, which without this reads as a key that did nothing.
    if app.destroying() == Some(session.name.as_str()) {
        lines.push(progress("destroy", "deleting the sandbox ..."));
    }
    lines
}

/// One fact, on one row, cut to fit.
fn fact(label: &str, value: &str, value_w: usize) -> Line<'static> {
    field(label, &truncate(value, value_w))
}

/// Something the TUI has asked the gateway for and is waiting on.
fn progress(label: &str, message: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label:<w$}", w = FIELD_W),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(message.to_string(), Style::default().fg(Color::Yellow)),
    ])
}

/// The facts pane, under the list. Never focused: there is nothing in it to
/// move around, so it does not take a turn in the focus cycle.
fn draw_meta(frame: &mut Frame, area: Rect, name: &str, lines: Vec<Line<'static>>) {
    // No `Wrap`: every line is already cut to the pane, and wrapping is what
    // made the height unpredictable in the first place.
    let title = Line::from(vec![
        Span::styled(" session ", Style::default().fg(ACCENT)),
        Span::styled(format!("{name} "), Style::default().fg(DIM)),
    ]);
    let para = Paragraph::new(lines).block(pane(title, false));
    frame.render_widget(para, area);
}

fn preview_lines(app: &App, session: &Session) -> Vec<Line<'static>> {
    // The task in full, which the facts pane can only show the head of. The rest
    // of what used to be here now lives there.
    let mut lines = vec![
        field(
            "task",
            if session.task.is_empty() {
                "-"
            } else {
                &session.task
            },
        ),
        Line::from(""),
    ];

    match app.previews.get(&session.name) {
        Some(cached) => lines.extend(cached.value.lines().map(|l| Line::from(l.to_string()))),
        None => lines.push(
            Line::from("  reading repository ...").style(Style::default().fg(Color::DarkGray)),
        ),
    }
    lines
}

fn diff_lines(app: &App, session: &Session) -> Vec<Line<'static>> {
    let Some(cached) = app.diffs.get(&session.name) else {
        return vec![Line::from("  reading diff ...").style(Style::default().fg(Color::DarkGray))];
    };

    // Whether the previous lines put us inside a hunk. `--- x` and `+++ x` are
    // file headers only *before* the first `@@` of a file; inside a hunk they
    // are ordinary removed and added lines, which is exactly what a diff of a
    // file full of SQL or Lua comments produces. Colouring those grey would
    // hide real changes, so the state is tracked rather than guessed per line.
    let mut in_hunk = false;
    cached
        .value
        .lines()
        .map(|line| diff_line(line, &mut in_hunk))
        .collect()
}

/// Style one line of the diff body, advancing the in-hunk state.
///
/// Colour only, no syntax highlighting: `syntect` would pull in a syntax set
/// far heavier than the rest of the binary, and add/remove/hunk colouring is
/// what makes a diff readable at a glance.
fn diff_line(line: &str, in_hunk: &mut bool) -> Line<'static> {
    // The section and notice markers are ours, added by the fetch script, so
    // they are rendered as headings rather than as diff content. Shared with
    // the policy pane via `marked_line`, so the two cannot drift apart.
    if line.starts_with(ops::DIFF_SECTION) {
        // A heading ends whatever hunk was open; the next `---` after it is a
        // file header again, not a removed line.
        *in_hunk = false;
        return marked_line(line);
    }
    if line.starts_with(ops::DIFF_NOTICE) {
        return marked_line(line);
    }

    if line.starts_with("diff --git") {
        *in_hunk = false;
    }
    let header = !*in_hunk && is_file_header(line);
    if line.starts_with("@@") {
        *in_hunk = true;
    }

    let style = if line.starts_with("@@") {
        Style::default().fg(Color::Cyan)
    } else if header {
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD)
    } else if line.starts_with('+') {
        Style::default().fg(Color::Green)
    } else if line.starts_with('-') {
        Style::default().fg(Color::Red)
    } else if line.starts_with('\\') {
        // "\ No newline at end of file"
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default()
    };
    Line::from(Span::styled(line.to_string(), style))
}

/// Lines git emits to describe a file rather than its contents. Only meaningful
/// outside a hunk; see [`diff_lines`].
fn is_file_header(line: &str) -> bool {
    const PREFIXES: [&str; 11] = [
        "diff --git",
        "index ",
        "--- ",
        "+++ ",
        "new file mode",
        "deleted file mode",
        "old mode",
        "new mode",
        "similarity index",
        "rename from",
        "rename to",
    ];
    // A file with no counterpart gets a bare header with no trailing path.
    line == "--- /dev/null"
        || line == "+++ /dev/null"
        || PREFIXES.iter().any(|p| line.starts_with(p))
}

/// The effective policy, rendered from the marked-up body [`crate::policy`]
/// produces. Same split as the diff pane: fetching builds text, rendering
/// colours it.
fn policy_lines(app: &App, session: &Session) -> Vec<Line<'static>> {
    if app.repolicying() == Some(session.name.as_str()) {
        return vec![
            Line::from("  applying the policy change ...")
                .style(Style::default().fg(Color::Yellow)),
            Line::from(""),
            Line::from("  the gateway takes a few seconds to load a revision")
                .style(Style::default().fg(Color::DarkGray)),
        ];
    }
    let Some(result) = app.policy(&session.name) else {
        return vec![
            Line::from("  reading policy ...").style(Style::default().fg(Color::DarkGray)),
        ];
    };
    let rev = match result {
        Ok(rev) => rev,
        Err(e) => return vec![Line::from(e.clone()).style(Style::default().fg(Color::Red))],
    };

    let body = crate::policy::render(rev, session.policy.as_deref());
    body.lines().map(marked_line).collect()
}

/// Style a line of a marked-up pane body, stripping the sigil.
fn marked_line(line: &str) -> Line<'static> {
    if let Some(heading) = line.strip_prefix(pane::SECTION) {
        return Line::from(Span::styled(
            format!("── {heading} "),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(notice) = line.strip_prefix(pane::NOTICE) {
        return Line::from(Span::styled(
            format!("! {notice}"),
            Style::default().fg(Color::Yellow),
        ));
    }
    if let Some(field) = line.strip_prefix(pane::FIELD) {
        // The label is fixed-width, written by `pane::field`, so it can be
        // split off and dimmed without parsing the value.
        let (label, value) = field.split_at(field.len().min(FIELD_LABEL_W));
        return Line::from(vec![
            Span::styled(format!("  {label}"), Style::default().fg(Color::DarkGray)),
            Span::raw(value.to_string()),
        ]);
    }
    Line::from(line.to_string())
}

/// Width `pane::field` pads its label to.
const FIELD_LABEL_W: usize = 12;

/// The allow/deny feed, newest first.
///
/// A denial is the event worth seeing, so it is the only one that gets a filled
/// badge -- the same reasoning as `Waiting` in the list. An allow is routine and
/// stays quiet; making both loud would make neither legible.
fn event_lines(app: &App, session: &Session) -> Vec<Line<'static>> {
    let Some(result) = app.events(&session.name) else {
        return vec![Line::from("  reading log ...").style(Style::default().fg(Color::DarkGray))];
    };
    let events = match result {
        Ok(e) => e,
        Err(e) => return vec![Line::from(e.clone()).style(Style::default().fg(Color::Red))],
    };
    if events.is_empty() {
        return vec![
            Line::from("  no policy decisions in the recent log")
                .style(Style::default().fg(Color::DarkGray)),
            Line::from(""),
            Line::from("  sbx's own polling is filtered out, so this stays empty")
                .style(Style::default().fg(Color::DarkGray)),
            Line::from("  until something in the sandbox reaches for the network")
                .style(Style::default().fg(Color::DarkGray)),
        ];
    }

    let mut lines = Vec::with_capacity(events.len() * 2);
    for e in events {
        let (badge, badge_style) = match e.verdict {
            Verdict::Denied => (
                " DENY  ",
                Style::default()
                    .bg(Color::Red)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD),
            ),
            Verdict::Allowed => (" allow ", Style::default().fg(Color::Green)),
            Verdict::Neutral => (
                "   -   ",
                Style::default().fg(if e.severity.is_notable() {
                    Color::Yellow
                } else {
                    Color::DarkGray
                }),
            ),
        };
        let mut spans = vec![
            Span::styled(
                format!("{} ", e.clock_utc()),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(badge, badge_style),
            Span::raw(" "),
            Span::raw(e.subject.clone()),
        ];
        if let Some(p) = &e.policy {
            spans.push(Span::styled(
                format!("  [{p}]"),
                Style::default().fg(Color::DarkGray),
            ));
        }
        lines.push(Line::from(spans));

        // The reason only exists on a denial, and it is the whole value of the
        // pane: "endpoint pastebin.com:443 is not allowed by any policy" is the
        // sentence the user came here to read.
        if let Some(reason) = &e.reason {
            lines.push(Line::from(Span::styled(
                format!("             {reason}"),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
    lines
}

/// What the agent is doing, and which source said so.
///
/// The source is shown because the two disagree by design: the pane sees a
/// permission prompt that the hooks cannot, so `waiting (screen)` against
/// `running (hooks)` is the expected reading, not a contradiction to debug.
fn status_line(app: &App, session: &Session) -> Line<'static> {
    let Some(report) = app.agent_status(session) else {
        return Line::from(vec![
            Span::styled(
                format!("{:<w$}", "agent at", w = FIELD_W),
                Style::default().fg(DIM),
            ),
            Span::styled("(not reporting)", Style::default().fg(DIM)),
        ]);
    };

    let source = match report.source {
        Source::Hook => "hooks",
        Source::Pane => "screen",
    };
    let mut spans = vec![
        Span::styled(
            format!("{:<w$}", "agent at", w = FIELD_W),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(report.state.to_string(), state_style(report.state)),
    ];
    if let Some(detail) = &report.detail {
        spans.push(Span::raw(format!(" {detail}")));
    }
    spans.push(Span::styled(
        format!("  ({source})"),
        Style::default().fg(Color::DarkGray),
    ));
    Line::from(spans)
}

/// Width of a field's label column, so the values line up.
const FIELD_W: usize = 10;

fn field(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label:<w$}", w = FIELD_W),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(value.to_string()),
    ])
}

/// A group of key hints, rendered together and separated from the next group.
type Group = &'static [(&'static str, &'static str)];

/// What each context offers, as keys rather than as a sentence.
///
/// Structured rather than one string per context so the key and what it does are
/// styled differently -- the key is what you are looking for, the word after it
/// is only there to remind you. Grouped by what they are *for*: moving, acting,
/// leaving.
const KEYS_LIST: &[Group] = &[
    &[("j/k", "move"), ("1-9", "jump"), ("n", "new")],
    &[
        ("enter", "open"),
        ("a", "attach"),
        ("P", "publish"),
        ("D", "destroy"),
    ],
    &[("tab", "view"), ("q", "quit")],
];
const KEYS_RIGHT: &[Group] = &[
    &[("j/k", "scroll"), ("pgup/pgdn", "page"), ("h", "list")],
    &[("enter", "open")],
    &[("tab", "view"), ("q", "quit")],
];
const KEYS_POLICY: &[Group] = &[
    &[("w", "widen"), ("t", "tighten")],
    &[("h", "list"), ("tab", "view"), ("q", "quit")],
];
const KEYS_AGENT_VIEW: &[Group] = &[
    &[("enter", "type at it"), ("a", "full screen")],
    &[("j/k", "move"), ("D", "destroy")],
    &[("tab", "view"), ("q", "quit")],
];
const KEYS_AGENT_FOCUS: &[Group] = &[
    &[("every key", "goes to the agent"), ("pgup/pgdn", "scroll")],
    &[("F12", "leave")],
];
const KEYS_PICK: &[Group] = &[
    &[("type", "filter"), ("up/down", "move")],
    &[("enter", "pick"), ("esc", "cancel")],
];
const KEYS_FORM: &[Group] = &[
    &[("tab", "field"), ("</>", "policy"), ("space", "provider")],
    &[("enter", "create"), ("esc", "back")],
];

/// Lay the groups out: the key in the accent, what it does beside it in grey,
/// `·` between keys and `│` between groups.
fn hint_line(groups: &[Group]) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (g, group) in groups.iter().enumerate() {
        if g > 0 {
            spans.push(Span::styled("  │  ", Style::default().fg(DIM)));
        }
        for (i, (key, what)) in group.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" · ", Style::default().fg(DIM)));
            }
            spans.push(Span::styled(*key, Style::default().fg(ACCENT)));
            spans.push(Span::styled(format!(" {what}"), Style::default().fg(DIM)));
        }
    }
    Line::from(spans)
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    // The hints follow the focus, because that is what decides where j/k go --
    // and, in the policy pane, gain the two keys that only work there.
    let groups = match (app.create_flow(), app.focus, app.right_view()) {
        // The flow is modal, so its keys are the only ones that do anything.
        (Some(Create::Pick(_)), ..) => KEYS_PICK,
        (Some(Create::Fill(_)), ..) => KEYS_FORM,
        // The terminal has the keyboard, so there is one binding left.
        (_, Focus::Agent, _) => KEYS_AGENT_FOCUS,
        (_, _, RightView::Agent) => KEYS_AGENT_VIEW,
        (_, _, RightView::Policy) => KEYS_POLICY,
        (_, Focus::List, _) => KEYS_LIST,
        (_, Focus::Right, _) => KEYS_RIGHT,
    };

    // A pending question outranks both the hints and any status message: it is
    // the only thing the keyboard will respond to.
    if let Some(question) = app.pending_question() {
        let line = Line::from(vec![
            Span::styled(
                " confirm ",
                Style::default()
                    .bg(Color::Yellow)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(" {question}")),
        ]);
        frame.render_widget(Paragraph::new(line), area);
        return;
    }

    let line = match &app.status {
        Some(msg) if app.status_is_error => Line::from(vec![
            Span::styled(" error ", Style::default().bg(Color::Red).fg(Color::Black)),
            Span::raw(format!(" {msg}")),
        ]),
        Some(msg) => Line::from(Span::styled(
            format!(" {msg}"),
            Style::default().fg(Color::Green),
        )),
        None => hint_line(groups),
    };
    frame.render_widget(Paragraph::new(line), area);
}

/// A centred box, clamped to what the frame can hold.
///
/// Both dimensions are a *maximum*: on a small terminal the box shrinks rather
/// than being drawn outside the frame, which ratatui would clip into nonsense.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

/// Width the modal boxes aim for. Wide enough for a path plus a branch, narrow
/// enough to leave the list visible around it on a normal terminal.
const MODAL_W: u16 = 78;
/// Rows the picker's list gets at most, so a long scan does not fill the screen.
const PICKER_ROWS: usize = 12;
/// Width of the create form's label column.
const LABEL_W: usize = 11;

fn draw_create(frame: &mut Frame, app: &App, area: Rect) {
    match app.create_flow() {
        None => {}
        Some(Create::Pick(picker)) => draw_picker(frame, picker, area),
        Some(Create::Fill(form)) => draw_form(frame, form, area),
    }
}

/// Render a field's value with the cursor drawn in it.
///
/// The cursor is a reversed cell rather than a terminal cursor: the frame is
/// redrawn on a timer and positioning the real cursor would mean threading a
/// coordinate out of here, for a caret that blinks in a place the layout already
/// knows.
fn with_cursor(input: &Input, focused: bool) -> Vec<Span<'static>> {
    let text = input.text().to_string();
    if !focused {
        return vec![Span::raw(text)];
    }
    let chars: Vec<char> = text.chars().collect();
    let at = input.cursor().min(chars.len());
    let before: String = chars[..at].iter().collect();
    // At the end of the line there is no character to reverse, so a space
    // stands in for one.
    let (under, after) = match chars.get(at) {
        Some(c) => (c.to_string(), chars[at + 1..].iter().collect::<String>()),
        None => (" ".to_string(), String::new()),
    };
    vec![
        Span::raw(before),
        Span::styled(under, Style::default().add_modifier(Modifier::REVERSED)),
        Span::raw(after),
    ]
}

fn draw_picker(frame: &mut Frame, picker: &Picker, area: Rect) {
    let rows = picker.rows();
    let shown = rows.len().min(PICKER_ROWS);
    // Query line, the rows, and a line for a complaint when there is one.
    let height = 2 + 1 + shown.max(1) + usize::from(picker.error().is_some());
    let box_area = centered(area, MODAL_W, u16::try_from(height).unwrap_or(u16::MAX));
    let inner_w = box_area.width.saturating_sub(2) as usize;

    let mut lines = vec![Line::from(
        [
            vec![Span::styled("> ", Style::default().fg(Color::DarkGray))],
            with_cursor(picker.query(), true),
        ]
        .concat(),
    )];

    if rows.is_empty() {
        lines.push(Line::from(Span::styled(
            if picker.scanning() {
                "  scanning ..."
            } else {
                "  nothing matches"
            },
            Style::default().fg(Color::DarkGray),
        )));
    }

    // A window around the cursor, so a long list stays navigable in twelve rows.
    let first = picker.cursor().saturating_sub(PICKER_ROWS - 1);
    for (i, repo) in rows.iter().enumerate().skip(first).take(shown) {
        lines.push(repo_row(repo, i == picker.cursor(), inner_w));
    }

    if let Some(error) = picker.error() {
        lines.push(Line::from(Span::styled(
            format!("  {error}"),
            Style::default().fg(Color::Red),
        )));
    }

    let mut title = format!(" pick a repo ({}", picker.total());
    if picker.scanning() {
        title.push_str(", scanning ...) ");
    } else {
        title.push_str(") ");
    }

    frame.render_widget(Clear, box_area);
    frame.render_widget(Paragraph::new(lines).block(pane(title, true)), box_area);
}

/// One repository row: where it is, what branch it is on, and whether it can
/// start a session at all.
fn repo_row(repo: &LocalRepo, selected: bool, width: usize) -> Line<'static> {
    // Branch and marker are fixed-width on the right, so the path gets whatever
    // is left rather than pushing them off the box.
    const BRANCH_W: usize = 22;
    /// Width of the "no origin" marker, reserved whether or not it is shown so
    /// the branch column lands in the same place on every row.
    const MARKER_W: usize = 10;
    let path_w = width.saturating_sub(2 + BRANCH_W + MARKER_W).max(8);

    let mut spans = vec![
        Span::raw(if selected { "> " } else { "  " }),
        Span::raw(format!(
            "{:<w$}",
            truncate(&repo.display, path_w),
            w = path_w
        )),
    ];
    match &repo.branch {
        Some(b) => spans.push(Span::styled(
            format!("{:<BRANCH_W$}", truncate(b, BRANCH_W)),
            Style::default().fg(Color::Cyan),
        )),
        None => spans.push(Span::styled(
            format!("{:<BRANCH_W$}", "(detached)"),
            Style::default().fg(Color::DarkGray),
        )),
    }
    // Named on the row rather than hidden, because the picker refuses these and
    // saying why in advance beats an error on enter.
    if repo.origin.is_none() {
        spans.push(Span::styled(
            "no origin",
            Style::default().fg(Color::DarkGray),
        ));
    }

    let line = Line::from(spans);
    if selected {
        line.style(Style::default().add_modifier(Modifier::REVERSED))
    } else {
        line
    }
}

fn draw_form(frame: &mut Frame, form: &Form, area: Rect) {
    let focused = form.field();
    let label = |field: Field| {
        let style = if field == focused {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        Span::styled(format!("{:<LABEL_W$}", field.label()), style)
    };

    let mut lines = Vec::new();

    // Everything past the label column, so a long path or clone URL is
    // truncated rather than clipped by the border.
    let value_w = usize::from(MODAL_W).saturating_sub(2 + LABEL_W);

    // What the sandbox will actually clone, spelled out: the local checkout is
    // only how the remote was named, and conflating the two is the one
    // misunderstanding this screen has to prevent.
    lines.push(Line::from(vec![
        Span::styled(
            format!("{:<LABEL_W$}", "repo"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(truncate(&form.repo.display, value_w)),
    ]));
    lines.push(Line::from(vec![
        Span::styled(
            format!("{:<LABEL_W$}", "clones"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            truncate(form.repo.origin.as_deref().unwrap_or("-"), value_w),
            Style::default().fg(Color::Cyan),
        ),
    ]));
    lines.push(Line::from(""));

    for field in [Field::Task, Field::Name, Field::Base] {
        let Some(input) = form.input(field) else {
            continue;
        };
        let mut spans = vec![label(field)];
        spans.extend(with_cursor(input, field == focused));
        // An empty base is not a missing answer, it is "the remote's default",
        // and saying so stops it reading as something left unfilled.
        if field == Field::Base && input.text().trim().is_empty() {
            spans.push(Span::styled(
                " (the remote's default branch)",
                Style::default().fg(Color::DarkGray),
            ));
        }
        lines.push(Line::from(spans));
    }

    let template = form.policy();
    lines.push(Line::from(vec![
        label(Field::Policy),
        Span::styled(
            format!("< {} >", template.name),
            Style::default().fg(Color::Yellow),
        ),
        Span::styled(
            format!("  {}", template.summary),
            Style::default().fg(Color::DarkGray),
        ),
    ]));

    let providers = form.providers();
    if providers.is_empty() {
        lines.push(Line::from(vec![
            label(Field::Providers),
            Span::styled(
                form.providers_error().unwrap_or("none defined").to_string(),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }
    for (i, choice) in providers.iter().enumerate() {
        let cursor = form.field() == Field::Providers && i == form.provider_cursor();
        let spans = vec![
            // The label sits on the first row only; the rest are indented under
            // it, which is what makes the group read as one field.
            if i == 0 {
                label(Field::Providers)
            } else {
                Span::raw(" ".repeat(LABEL_W))
            },
            Span::raw(if cursor { "> " } else { "  " }),
            Span::styled(
                if choice.selected { "[x] " } else { "[ ] " },
                if choice.selected {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ),
            Span::raw(format!("{:<22}", truncate(&choice.name, 22))),
            Span::styled(choice.kind.clone(), Style::default().fg(Color::DarkGray)),
        ];
        lines.push(Line::from(spans));
    }

    if let Some(note) = drift_note(form.facts()) {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            truncate(&format!(" {note}"), usize::from(MODAL_W) - 2),
            Style::default().fg(Color::Yellow),
        )));
    }
    if let Some(error) = form.error() {
        lines.push(Line::from(Span::styled(
            format!(" {error}"),
            Style::default().fg(Color::Red),
        )));
    }

    let height = u16::try_from(lines.len() + 2).unwrap_or(u16::MAX);
    let box_area = centered(area, MODAL_W, height);
    frame.render_widget(Clear, box_area);
    frame.render_widget(
        Paragraph::new(lines).block(pane(" new session ".to_string(), true)),
        box_area,
    );
}

/// What the sandbox will not be getting, in words.
///
/// The sandbox clones `origin`, so anything not pushed stays on the host. That
/// is the design, but it is a surprise the first time, and a count is the only
/// honest way to say it.
fn drift_note(facts: Option<&Facts>) -> Option<String> {
    let facts = facts?;
    let mut parts = Vec::new();
    if facts.uncommitted > 0 {
        parts.push(format!("{} uncommitted file(s)", facts.uncommitted));
    }
    match facts.unpushed {
        Some(n) if n > 0 => parts.push(format!("{n} unpushed commit(s)")),
        // No upstream at all: nothing has been pushed, so nothing about the
        // local branch is in the clone.
        None => parts.push("no upstream for this branch".to_string()),
        Some(_) => {}
    }
    if parts.is_empty() {
        return None;
    }
    // Short on purpose: it has to fit the box on one line, and the sentence it
    // replaces ("... stay on the host because the sandbox clones from the
    // remote rather than from this checkout") is the doc comment above.
    Some(format!("staying on the host: {}", parts.join(", ")))
}

fn truncate(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    // Reserve one column for the ellipsis so the column never overflows.
    let keep = width.saturating_sub(1);
    s.chars().take(keep).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::{KeyCode, KeyEvent};

    use super::*;

    #[test]
    fn truncates_on_character_boundaries() {
        assert_eq!(truncate("short", 16), "short");
        assert_eq!(truncate("exactly-sixteen!", 16), "exactly-sixteen!");
        assert_eq!(truncate("a-very-long-session-name", 16), "a-very-long-ses…");
        // Multi-byte input must not panic or split a character.
        assert_eq!(truncate("ααααααα", 4), "ααα…");
    }

    /// Colour is the whole readability story for a diff, and the prefix that is
    /// easy to get wrong is `---`: styled as a removed line it makes every file
    /// header look like a change, and styled as a header it hides real removals
    /// from any file whose comments start with a dash.
    #[test]
    fn file_headers_are_distinguished_from_added_and_removed_lines() {
        // A whole file block, so the in-hunk state is exercised as it really
        // arrives rather than line by line.
        let body = "\
diff --git a/q.sql b/q.sql
index 1111111..2222222 100644
--- a/q.sql
+++ b/q.sql
@@ -1,3 +1,3 @@
 select 1
--- removed sql comment
+++ added sql comment
";
        let mut in_hunk = false;
        let colours: Vec<Option<Color>> = body
            .lines()
            .map(|l| diff_line(l, &mut in_hunk).spans[0].style.fg)
            .collect();

        assert_eq!(
            colours,
            vec![
                Some(Color::DarkGray), // diff --git
                Some(Color::DarkGray), // index
                Some(Color::DarkGray), // --- a/q.sql
                Some(Color::DarkGray), // +++ b/q.sql
                Some(Color::Cyan),     // @@
                None,                  // context
                Some(Color::Red),      // "-" + "-- removed sql comment"
                Some(Color::Green),    // "+" + "++ added sql comment"
            ]
        );
    }

    /// A second file in the same diff must be recognised as a new header block,
    /// or every header after the first hunk is coloured as content.
    #[test]
    fn a_following_file_header_is_recognised_again() {
        let body = "\
@@ -1 +1 @@
-old
diff --git a/b b/b
--- a/b
+++ b/b
";
        let mut in_hunk = false;
        let colours: Vec<Option<Color>> = body
            .lines()
            .map(|l| diff_line(l, &mut in_hunk).spans[0].style.fg)
            .collect();

        assert_eq!(
            colours,
            vec![
                Some(Color::Cyan),
                Some(Color::Red),
                Some(Color::DarkGray),
                Some(Color::DarkGray),
                Some(Color::DarkGray),
            ]
        );
    }

    #[test]
    fn section_and_notice_markers_render_as_headings() {
        let mut in_hunk = true;
        let section = diff_line("### committed, vs origin/main", &mut in_hunk);
        assert!(
            section.spans[0]
                .content
                .contains("committed, vs origin/main")
        );
        assert!(
            !section.spans[0].content.starts_with("###"),
            "the sigil is ours, not something to show the user"
        );
        assert!(!in_hunk, "a new section starts outside any hunk");

        let notice = diff_line("!!! showing 2000 of 3018 lines", &mut in_hunk);
        assert!(
            notice.spans[0]
                .content
                .contains("showing 2000 of 3018 lines")
        );
        assert!(!notice.spans[0].content.starts_with("!!!"));
    }

    /// The badge style must not bleed into the gap or the stat column, which is
    /// what a background colour applied to a padded span would do.
    #[test]
    fn only_waiting_gets_a_filled_badge() {
        assert_eq!(state_style(State::Waiting).bg, Some(Color::Magenta));
        for state in [
            State::Ready,
            State::Running,
            State::Idle,
            State::Creating,
            State::Seeding,
            State::Published,
            State::Failed,
            State::Dead,
        ] {
            assert_eq!(state_style(state).bg, None, "{state} must not be filled");
        }
    }

    #[test]
    fn stat_column_is_fixed_width() {
        let width =
            |spans: Vec<Span>| -> usize { spans.iter().map(|s| s.content.chars().count()).sum() };
        assert_eq!(width(stat_spans(None)), STAT_W, "unmeasured");
        assert_eq!(
            width(stat_spans(Some(ops::DiffStat::default()))),
            STAT_W,
            "clean"
        );
        for stat in [
            ops::DiffStat {
                added: 1,
                removed: 2,
                untracked: 0,
            },
            ops::DiffStat {
                added: 12,
                removed: 3,
                untracked: 1,
            },
            ops::DiffStat {
                added: 999,
                removed: 999,
                untracked: 9,
            },
            // Must not widen the column and push the age off the pane.
            ops::DiffStat {
                added: 4_000_000,
                removed: 120_000,
                untracked: 1234,
            },
            ops::DiffStat {
                added: 9_999,
                removed: 1_000,
                untracked: 1,
            },
        ] {
            assert_eq!(width(stat_spans(Some(stat))), STAT_W, "{stat:?}");
        }
    }

    #[test]
    fn compacts_large_counts_to_three_columns() {
        assert_eq!(compact(0), "0");
        assert_eq!(compact(999), "999");
        assert_eq!(compact(1_500), "1k");
        assert_eq!(compact(9_999), "9k");
        assert_eq!(compact(10_000), "9k+");
        assert_eq!(compact(u32::MAX), "9k+");
        for n in [0, 1, 999, 1_000, 9_999, 10_000, u32::MAX] {
            assert!(compact(n).len() <= 3, "{n} rendered as {}", compact(n));
        }
    }

    #[test]
    fn wrapped_height_counts_rows_not_lines() {
        let lines = vec![Line::from("abcdefghij"), Line::from("")];
        // Ten characters over a four-column pane is three rows; the empty line
        // still occupies one.
        assert_eq!(wrapped_height(&lines, 4), 4);
        assert_eq!(wrapped_height(&lines, 40), 2);
        // A zero-width pane must not divide by zero.
        assert_eq!(wrapped_height(&lines, 0), 2);
    }

    #[test]
    fn a_modal_box_is_clamped_to_the_frame() {
        let frame = Rect::new(0, 0, 100, 40);
        let box_area = centered(frame, MODAL_W, 20);
        assert_eq!(box_area.width, MODAL_W);
        assert_eq!(box_area.x, (100 - MODAL_W) / 2);

        // A terminal smaller than the box shrinks it rather than drawing off
        // the frame, which ratatui would clip into nonsense.
        let tiny = Rect::new(0, 0, 30, 6);
        let box_area = centered(tiny, MODAL_W, 20);
        assert_eq!((box_area.width, box_area.height), (30, 6));
        assert_eq!((box_area.x, box_area.y), (0, 0));
    }

    #[test]
    fn the_cursor_is_drawn_in_the_focused_field_only() {
        let input = Input::new("abc");
        let spans = with_cursor(&input, false);
        assert_eq!(spans.len(), 1, "an unfocused field is plain text");

        // At the end of the line there is no character to reverse, so a space
        // stands in for one.
        let spans = with_cursor(&input, true);
        let reversed: Vec<&str> = spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::REVERSED))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(reversed, vec![" "]);
    }

    /// The whole point of the note: the sandbox clones `origin`, so anything not
    /// pushed is not in it. Silence when there is nothing to say.
    #[test]
    fn the_drift_note_only_appears_when_something_would_be_left_behind() {
        assert_eq!(drift_note(None), None, "nothing is known yet");
        assert_eq!(
            drift_note(Some(&Facts {
                uncommitted: 0,
                unpushed: Some(0),
                base_on_remote: true,
            })),
            None,
            "in sync: no warning to give"
        );

        let note = drift_note(Some(&Facts {
            uncommitted: 3,
            unpushed: Some(2),
            base_on_remote: true,
        }))
        .unwrap();
        assert!(note.contains("3 uncommitted"), "{note}");
        assert!(note.contains("2 unpushed"), "{note}");

        // No upstream is not "in sync with zero commits ahead".
        let note = drift_note(Some(&Facts {
            uncommitted: 0,
            unpushed: None,
            base_on_remote: false,
        }))
        .unwrap();
        assert!(note.contains("no upstream"), "{note}");
    }

    /// A long path must not push the branch column out of the box, and a
    /// repository that cannot start a session has to say so on its own row.
    #[test]
    fn a_repository_row_fits_the_box_and_names_a_missing_origin() {
        let repo = LocalRepo {
            path: "/x".into(),
            display: "~/dev/some/quite/deeply/nested/checkout-with-a-long-name".into(),
            name: "checkout-with-a-long-name".into(),
            origin: None,
            branch: Some("feature/a-long-branch-name-too".into()),
        };
        let line = repo_row(&repo, true, 76);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.chars().count() <= 76,
            "{} columns: {text}",
            text.chars().count()
        );
        assert!(text.contains("no origin"), "{text}");
        assert!(
            text.contains('…'),
            "the path is truncated, not wrapped: {text}"
        );
    }

    /// Both modals, rendered into a real buffer.
    ///
    /// The pure helpers cover what each line says; this covers what only a
    /// buffer knows -- that nothing overflows the box. A line one column too
    /// long is not a wrapped line, it is a sentence with its end cut off, and
    /// the one that says what stays on the host is exactly the line that must
    /// not lose its ending.
    fn render(app: &mut App, width: u16, height: u16) -> Vec<String> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// An app with one session selected, for the layout tests.
    fn app_with_session() -> App {
        let mut app = App::new();
        let mut session = Session::new(
            "readme-fix".into(),
            "https://github.com/octocat/Hello-World.git".into(),
            "fix the typo in the readme".into(),
        );
        session.policy = Some("feature-work".into());
        session.providers = vec!["claude-oauth".into()];
        app.sessions = vec![session];
        app.list_state.select(Some(0));
        app
    }

    /// Cell styles from a rendered buffer, for the one thing text cannot show.
    fn styles_at(app: &mut App, width: u16, height: u16, needle: &str) -> Vec<Style> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();

        for y in 0..height {
            let row: String = (0..width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect();
            // Byte offsets are not cell offsets: a border row is mostly
            // multi-byte box drawing, so the needle's position has to be counted
            // in characters to index the buffer with it.
            if let Some(byte_at) = row.find(needle) {
                let at = row[..byte_at].chars().count();
                return (at..at + needle.chars().count())
                    .map(|x| buffer[(x as u16, y)].style())
                    .collect();
            }
        }
        panic!("`{needle}` is not on screen");
    }

    /// A waiting agent is the one thing the whole tool exists to surface, and it
    /// used to become invisible at the worst possible moment: a list's highlight
    /// style is patched over the row, so the badge's magenta fill was replaced by
    /// the selection's grey and its black text went with it. Selected, the badge
    /// becomes bright text instead of a fill.
    #[test]
    fn the_waiting_badge_stays_legible_on_the_selected_row() {
        let mut app = app_with_session();
        app.sessions[0].state = State::Waiting;

        // The badge in the list's title is the first `waiting` on screen and is
        // a fill wherever it appears; the row's is what this is about, so it is
        // found by the marker that only the selected row carries.
        let selected = styles_at(&mut app, 100, 24, "readme-fix               waiting");
        let word = &selected[selected.len() - "waiting".len()..];
        for style in word {
            assert_eq!(style.fg, Some(Color::Magenta), "bright, not black-on-grey");
            assert_ne!(style.bg, Some(Color::Magenta), "and not a fill");
        }

        // Unselected, it is the filled badge it is everywhere else.
        app.sessions.push(Session::new(
            "other".into(),
            "https://github.com/o/r.git".into(),
            "t".into(),
        ));
        app.list_state.select(Some(1));
        let unselected = styles_at(&mut app, 100, 24, "readme-fix               waiting");
        let word = &unselected[unselected.len() - "waiting".len()..];
        for style in word {
            assert_eq!(style.bg, Some(Color::Magenta), "filled when not selected");
            assert_eq!(style.fg, Some(Color::Black));
        }
    }

    /// The point of the split: the facts stay on screen while the right-hand
    /// pane is the agent's terminal. They used to head the preview, so opening a
    /// terminal hid them.
    #[test]
    fn the_facts_stay_visible_when_the_right_pane_is_the_agent() {
        let mut app = app_with_session();
        app.views
            .insert("readme-fix".into(), crate::tui::RightView::Agent);

        let rows = render(&mut app, 120, 30);
        let body = rows.join("\n");

        assert!(body.contains("session readme-fix"), "{body}");
        for expected in [
            "fix the typo",
            "sbx/readme-fix",
            "sbx-readme-fix",
            "feature-work",
            "claude-oauth",
        ] {
            assert!(body.contains(expected), "missing `{expected}`:\n{body}");
        }
        // And the list is still there, above it.
        assert!(body.contains("sessions"), "{body}");
        assert!(body.contains("1. readme-fix"), "the numbered row: {body}");
        // The right-hand pane is the agent's, named by its tab.
        assert!(
            body.contains("preview · diff · policy · events · agent"),
            "the tabs: {body}"
        );
        assert!(body.contains("press enter to open its terminal"), "{body}");
        for row in &rows {
            assert!(row.chars().count() <= 120, "overflowed: {row}");
        }
    }

    /// The list must survive a short terminal: the facts pane is sized to its
    /// content, and content is the one thing that can grow.
    #[test]
    fn the_list_keeps_room_on_a_short_terminal() {
        let mut app = app_with_session();
        let rows = render(&mut app, 100, 12);
        let body = rows.join("\n");
        assert!(body.contains("sessions"), "the list survives:\n{body}");
        assert!(body.contains("readme-fix"), "{body}");
        for row in &rows {
            assert!(row.chars().count() <= 100, "overflowed: {row}");
        }
    }

    /// A long task must not push the short fields out of the pane: they are the
    /// ones that get checked, and the preview carries the task in full.
    #[test]
    fn a_long_task_gives_way_to_the_fields_under_it() {
        let mut app = app_with_session();
        let long = "refactor the seeding path so the clone, the branch and the \
                    identity are three steps that can each fail on their own, and \
                    then write the tests for every one of them"
            .to_string();
        app.sessions[0].task = long.clone();

        let lines = meta_lines(&app, &app.sessions[0], 40);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(text.contains('…'), "the task is cut: {text}");
        assert!(text.contains("sbx-readme-fix"), "and the fields survive");
        assert!(text.contains("agent at"), "{text}");

        // The whole task is still reachable, in the pane that has room for it.
        let preview: String = preview_lines(&app, &app.sessions[0])
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(preview.contains("write the tests for every one of them"));
    }

    /// The bug this pane's sizing was rewritten for: a repository URL is one
    /// unbreakable word, so a *wrapped* facts pane is taller than any character
    /// count predicts, and the field at the bottom -- what the agent is doing --
    /// quietly fell off the end of it.
    #[test]
    fn every_fact_survives_a_narrow_pane() {
        let mut app = app_with_session();
        app.sessions[0].repo = "https://inetse@dev.azure.com/inetse/inet/_git/Inet.Server".into();
        app.sessions[0].task = "refactor the seeding path so each step can fail on \
                                its own, then test every one of them"
            .into();

        let rows = render(&mut app, 100, 24);
        let body = rows.join("\n");
        for expected in ["task", "repo", "branch", "sandbox", "policy", "agent at"] {
            assert!(body.contains(expected), "lost `{expected}`:\n{body}");
        }
        // And the long ones are cut rather than wrapped onto a row of their own.
        assert!(body.contains('…'), "{body}");
        for row in &rows {
            assert!(row.chars().count() <= 100, "overflowed: {row}");
        }
    }

    /// An empty list has no facts to show, so the pane must not appear at all --
    /// an empty bordered box under the list is just noise.
    #[test]
    fn no_facts_pane_without_a_session() {
        let mut app = App::new();
        let rows = render(&mut app, 100, 24);
        let body = rows.join("\n");
        assert!(body.contains("no sessions yet"), "{body}");
        // No facts pane at all: its first field is the tell.
        assert!(!body.contains("task "), "{body}");
    }

    fn probe_repo(name: &str, origin: Option<&str>, branch: &str) -> LocalRepo {
        LocalRepo {
            path: format!("/home/u/dev/{name}").into(),
            display: format!("~/dev/{name}"),
            name: name.to_string(),
            origin: origin.map(String::from),
            branch: Some(branch.to_string()),
        }
    }

    /// An app with the create flow open on the picker, populated.
    fn app_picking() -> App {
        let mut app = App::new();
        app.repos = Some(vec![
            probe_repo("sbx", Some("https://github.com/o/sbx.git"), "main"),
            probe_repo(
                "Inet.Server",
                Some("https://inetse@dev.azure.com/inetse/inet/_git/Inet.Server"),
                "tobias/CODE-18757-qty-shipped-non-nullable",
            ),
            probe_repo("notes", None, "main"),
        ]);
        app.providers = Some(Ok(vec![
            openshell_client::Provider {
                name: "claude-oauth".into(),
                kind: "claude-code-oauth".into(),
                credential_keys: vec![],
            },
            openshell_client::Provider {
                name: "azure-pat".into(),
                kind: "azure-devops-pat".into(),
                credential_keys: vec![],
            },
        ]));
        app.open_create();
        app
    }

    #[test]
    fn the_picker_renders_inside_its_box() {
        let mut app = app_picking();
        let rows = render(&mut app, 100, 26);
        let picker = rows
            .iter()
            .find(|r| r.contains("pick a repo"))
            .expect("the picker box");
        assert!(
            picker.contains("(3)"),
            "the count is in the title: {picker}"
        );

        // Every repository is offered, including the one that cannot be used --
        // with the reason on its row.
        let body = rows.join("\n");
        assert!(body.contains("~/dev/sbx"), "{body}");
        assert!(body.contains("no origin"), "{body}");
        // The long branch is truncated rather than pushing the box apart.
        assert!(body.contains("tobias/CODE-18757"), "{body}");
        for row in &rows {
            assert!(row.chars().count() <= 100, "overflowed the frame: {row}");
        }
    }

    #[test]
    fn the_form_renders_inside_its_box() {
        let mut app = app_picking();
        // Pick the second repository, so the long Azure URL is the one shown.
        app.on_key(KeyEvent::from(KeyCode::Down));
        app.on_key(KeyEvent::from(KeyCode::Enter));
        app.on_update(crate::tui::worker::Update::Inspected {
            path: "/home/u/dev/Inet.Server".into(),
            facts: Box::new(Facts {
                uncommitted: 9,
                unpushed: Some(2),
                base_on_remote: true,
            }),
        });

        let rows = render(&mut app, 100, 26);
        let body = rows.join("\n");
        assert!(body.contains("new session"), "{body}");
        assert!(body.contains("feature-work"), "the default policy: {body}");
        // The agent's credential, and the one for this repository's host: an
        // Azure repo with exactly one Azure PAT defined leaves no ambiguity.
        assert!(body.contains("[x] claude-oauth"), "preselected: {body}");
        assert!(body.contains("[x] azure-pat"), "preselected: {body}");
        // And the URL the sandbox will clone, which is the point of the screen.
        assert!(body.contains("dev.azure.com/inetse/inet/_git"), "{body}");

        // The note has to survive intact: it is the one thing on this screen
        // that corrects a wrong assumption about what the sandbox will contain.
        let note = rows
            .iter()
            .find(|r| r.contains("staying on the host"))
            .expect("the drift note");
        assert!(
            note.contains("staying on the host: 9 uncommitted file(s), 2 unpushed commit(s)"),
            "the note was cut off by the border: {note}"
        );
    }

    /// A terminal too small for the box must still render something rather than
    /// panicking on an out-of-frame rect.
    #[test]
    fn the_modals_survive_a_tiny_terminal() {
        let mut app = app_picking();
        for (w, h) in [(20u16, 6u16), (40, 10), (78, 4)] {
            let rows = render(&mut app, w, h);
            assert_eq!(rows.len(), usize::from(h));
        }
    }
}
