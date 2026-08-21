//! Rendering. Pure: reads app state, draws, mutates nothing but list scroll.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, Paragraph, Wrap};

use crate::session::{self, Session, State};
use crate::tui::App;

/// One colour per state, so the list is scannable without reading it.
fn state_style(state: State) -> Style {
    let colour = match state {
        State::Ready => Color::Green,
        State::Running => Color::Cyan,
        State::Waiting => Color::Magenta,
        State::Creating | State::Seeding => Color::Yellow,
        State::Idle => Color::Blue,
        State::Published => Color::LightGreen,
        State::Failed => Color::Red,
        State::Dead => Color::DarkGray,
    };
    Style::default().fg(colour)
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [main, footer] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)]).areas(main);

    draw_list(frame, app, left);
    draw_preview(frame, app, right);
    draw_footer(frame, app, footer);
}

fn draw_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let now = session::now_epoch();

    let items: Vec<ListItem> = app
        .sessions
        .iter()
        .map(|s| {
            let age = session::humanize_age(s.created_at, now);
            ListItem::new(Line::from(vec![
                Span::styled(format!("{:<16}", truncate(&s.name, 16)), Style::default()),
                Span::styled(format!("{:<9}", s.state), state_style(s.state)),
                Span::styled(format!("{age:>4}"), Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();

    let title = if app.refreshing {
        format!(" sessions ({}) - refreshing ", app.sessions.len())
    } else {
        format!(" sessions ({}) ", app.sessions.len())
    };

    if items.is_empty() {
        let hint = Paragraph::new(vec![
            Line::from(""),
            Line::from("  no sessions yet").style(Style::default().fg(Color::DarkGray)),
            Line::from(""),
            Line::from("  sbx new --repo <url> --task <what>")
                .style(Style::default().fg(Color::DarkGray)),
        ])
        .block(Block::bordered().title(title));
        frame.render_widget(hint, area);
        return;
    }

    let list = List::new(items)
        .block(Block::bordered().title(title))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, area, &mut app.list_state);
}

fn draw_preview(frame: &mut Frame, app: &App, area: Rect) {
    let Some(session) = app.selected() else {
        let empty = Paragraph::new("").block(Block::bordered().title(" preview "));
        frame.render_widget(empty, area);
        return;
    };

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
    lines.push(Line::from(""));

    match app.previews.get(&session.name) {
        Some(body) => lines.extend(body.lines().map(|l| Line::from(l.to_string()))),
        None => lines.push(
            Line::from("  reading repository ...").style(Style::default().fg(Color::DarkGray)),
        ),
    }

    let title = format!(" preview - {} ", session.name);
    let para = Paragraph::new(lines)
        .block(Block::bordered().title(title))
        .wrap(Wrap { trim: false });
    frame.render_widget(para, area);
}

fn field<'a>(label: &'a str, value: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{label:<10}"), Style::default().fg(Color::DarkGray)),
        Span::raw(value),
    ])
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let keys = "j/k move  g/G top/bottom  r refresh  q quit";
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

/// Session is re-exported for the widget signatures above.
pub type _Session = Session;

#[cfg(test)]
mod tests {
    use super::truncate;

    #[test]
    fn truncates_on_character_boundaries() {
        assert_eq!(truncate("short", 16), "short");
        assert_eq!(truncate("exactly-sixteen!", 16), "exactly-sixteen!");
        assert_eq!(truncate("a-very-long-session-name", 16), "a-very-long-ses…");
        // Multi-byte input must not panic or split a character.
        assert_eq!(truncate("ααααααα", 4), "ααα…");
    }
}
