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
use ratatui::widgets::{Block, BorderType, List, ListItem, Paragraph, Wrap};

use crate::ops;
use crate::session::{self, Session, State};
use crate::status::Source;
use crate::tui::{App, Focus, RightView};

/// Width of the session-name column. Names are capped at 15 characters by the
/// gateway's sandbox-name limit, so this only ever truncates a near-maximal one.
const NAME_W: usize = 15;
/// Width of the `+12/-3 ?1` column.
const STAT_W: usize = 11;

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
fn pane(title: String, focused: bool) -> Block<'static> {
    let block = Block::bordered().title(title);
    if focused {
        block
            .border_type(BorderType::Thick)
            .border_style(Style::default().fg(Color::White))
    } else {
        block.border_style(Style::default().fg(Color::DarkGray))
    }
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [main, footer] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)]).areas(main);

    draw_list(frame, app, left);
    draw_right(frame, app, right);
    draw_footer(frame, app, footer);
}

fn draw_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let now = session::now_epoch();
    let focused = app.focus == Focus::List;

    let items: Vec<ListItem> = app
        .sessions
        .iter()
        .map(|s| {
            let age = session::humanize_age(s.created_at, now);
            let state = app.effective_state(s);
            let mut spans = vec![
                Span::raw(format!("{:<w$}", truncate(&s.name, NAME_W), w = NAME_W)),
                Span::styled(format!("{state:<9}"), state_style(state)),
                // A plain gap, so the badge's background stops at the word.
                Span::raw(" "),
            ];
            spans.extend(stat_spans(app.poll(&s.name).and_then(|p| p.stat)));
            spans.push(Span::styled(
                format!("{age:>4}"),
                Style::default().fg(Color::DarkGray),
            ));
            ListItem::new(Line::from(spans))
        })
        .collect();

    let waiting = app.waiting_count();
    let mut title = format!(" sessions ({}", app.sessions.len());
    if waiting > 0 {
        // In the title as well as the rows, so it is legible when the waiting
        // session is scrolled out of view.
        title.push_str(&format!(", {waiting} waiting"));
    }
    if app.refreshing {
        title.push_str(") - refreshing ");
    } else {
        title.push_str(") ");
    }

    if items.is_empty() {
        let hint = Paragraph::new(vec![
            Line::from(""),
            Line::from("  no sessions yet").style(Style::default().fg(Color::DarkGray)),
            Line::from(""),
            Line::from("  sbx new --repo <url> --task <what>")
                .style(Style::default().fg(Color::DarkGray)),
        ])
        .block(pane(title, focused));
        frame.render_widget(hint, area);
        return;
    }

    let list = List::new(items)
        .block(pane(title, focused))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");

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
            format!("{:<w$}", "clean", w = STAT_W),
            Style::default().fg(Color::DarkGray),
        )];
    }

    let added = format!("+{}", compact(stat.added));
    let removed = format!("-{}", compact(stat.removed));
    let untracked = if stat.untracked > 0 { " ?" } else { "" };
    let used = added.len() + 1 + removed.len() + untracked.len();

    let mut spans = vec![
        Span::styled(added, Style::default().fg(Color::Green)),
        Span::styled("/", Style::default().fg(Color::DarkGray)),
        Span::styled(removed, Style::default().fg(Color::Red)),
    ];
    if !untracked.is_empty() {
        spans.push(Span::styled(
            untracked,
            Style::default().fg(Color::DarkGray),
        ));
    }
    spans.push(Span::raw(" ".repeat(STAT_W.saturating_sub(used))));
    spans
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
        let empty = Paragraph::new("").block(pane(" preview ".to_string(), focused));
        frame.render_widget(empty, area);
        app.right_lines = 0;
        app.right_height = inner_h;
        return;
    };

    // Both produce owned lines, so no borrow of `app` outlives this call and the
    // measurements below can be written back.
    let (lines, wrap, label) = match view {
        RightView::Preview => (preview_lines(app, &session), true, "preview"),
        RightView::Diff => (diff_lines(app, &session), false, "diff"),
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
            " [{}/{}]",
            offset as usize + inner_h.min(app.right_lines),
            app.right_lines
        )
    } else {
        String::new()
    };
    let title = format!(" {label} - {}{position} ", session.name);

    let mut para = Paragraph::new(lines)
        .block(pane(title, focused))
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

fn preview_lines(app: &App, session: &Session) -> Vec<Line<'static>> {
    let mut lines = vec![
        field(
            "task",
            if session.task.is_empty() {
                "-"
            } else {
                &session.task
            },
        ),
        field("repo", &session.repo),
        field("branch", &session.work_branch),
        field("sandbox", &session.sandbox),
        field("policy", session.policy.as_deref().unwrap_or("(default)")),
        field("agent", &session.agent),
    ];
    let providers = session.providers.join(", ");
    if !providers.is_empty() {
        lines.push(field("providers", &providers));
    }
    lines.push(status_line(app, session));
    lines.push(Line::from(""));

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
    // they are rendered as headings rather than as diff content.
    if let Some(heading) = line.strip_prefix(ops::DIFF_SECTION) {
        *in_hunk = false;
        return Line::from(Span::styled(
            format!("── {heading} "),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(notice) = line.strip_prefix(ops::DIFF_NOTICE) {
        return Line::from(Span::styled(
            format!("! {notice}"),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
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

/// What the agent is doing, and which source said so.
///
/// The source is shown because the two disagree by design: the pane sees a
/// permission prompt that the hooks cannot, so `waiting (screen)` against
/// `running (hooks)` is the expected reading, not a contradiction to debug.
fn status_line(app: &App, session: &Session) -> Line<'static> {
    let Some(report) = app.agent_status(session) else {
        return Line::from(vec![
            Span::styled("agent at ", Style::default().fg(Color::DarkGray)),
            Span::styled("(not reporting)", Style::default().fg(Color::DarkGray)),
        ]);
    };

    let source = match report.source {
        Source::Hook => "hooks",
        Source::Pane => "screen",
    };
    let mut spans = vec![
        Span::styled(
            format!("{:<10}", "agent at"),
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

fn field(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<10}"), Style::default().fg(Color::DarkGray)),
        Span::raw(value.to_string()),
    ])
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    // The hints follow the focus, because that is what decides where j/k go.
    let keys = match app.focus {
        Focus::List => "j/k move  l pane  tab preview/diff  enter attach  r refresh  q quit",
        Focus::Right => {
            "j/k scroll  pgup/pgdn page  h pane  tab preview/diff  enter attach  q quit"
        }
    };
    let line = match &app.status {
        Some(msg) if app.status_is_error => Line::from(vec![
            Span::styled(" error ", Style::default().bg(Color::Red).fg(Color::Black)),
            Span::raw(format!(" {msg}")),
        ]),
        Some(msg) => Line::from(Span::styled(
            format!(" {msg}"),
            Style::default().fg(Color::Green),
        )),
        None => Line::from(Span::styled(
            format!(" {keys}"),
            Style::default().fg(Color::DarkGray),
        )),
    };
    frame.render_widget(Paragraph::new(line), area);
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
}
