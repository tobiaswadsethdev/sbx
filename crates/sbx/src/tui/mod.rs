//! The terminal UI.

mod ui;
mod worker;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use openshell_client::CliClient;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::ListState;

use crate::ops;
use crate::session::Session;
use worker::{Request, Update, Worker};

/// How often the session list is reconciled against the gateway.
const REFRESH_EVERY: Duration = Duration::from_secs(3);
/// How long a transient footer message stays up.
const STATUS_LINGER: Duration = Duration::from_secs(4);
/// Input poll interval. Short enough to feel immediate, long enough to idle.
const TICK: Duration = Duration::from_millis(100);

pub struct App {
    sessions: Vec<Session>,
    list_state: ListState,
    previews: HashMap<String, String>,
    /// Session whose preview is currently being fetched, so the same request
    /// is not queued repeatedly while the worker is busy.
    preview_in_flight: Option<String>,
    status: Option<String>,
    status_is_error: bool,
    status_set_at: Instant,
    refreshing: bool,
    last_refresh: Instant,
    should_quit: bool,
}

impl App {
    fn new() -> Self {
        App {
            sessions: Vec::new(),
            list_state: ListState::default(),
            previews: HashMap::new(),
            preview_in_flight: None,
            status: None,
            status_is_error: false,
            status_set_at: Instant::now(),
            refreshing: false,
            // Force an immediate first refresh.
            last_refresh: Instant::now() - REFRESH_EVERY,
            should_quit: false,
        }
    }

    fn selected(&self) -> Option<&Session> {
        self.list_state
            .selected()
            .and_then(|i| self.sessions.get(i))
    }

    fn note(&mut self, msg: impl Into<String>) {
        self.status = Some(msg.into());
        self.status_is_error = false;
        self.status_set_at = Instant::now();
    }

    fn fail(&mut self, msg: impl Into<String>) {
        self.status = Some(msg.into());
        self.status_is_error = true;
        self.status_set_at = Instant::now();
    }

    fn move_by(&mut self, delta: isize) {
        if self.sessions.is_empty() {
            return;
        }
        let last = self.sessions.len() - 1;
        let current = self.list_state.selected().unwrap_or(0) as isize;
        // Clamp rather than wrap: wrapping past the end is disorienting when
        // the list is long enough to scroll.
        let next = (current + delta).clamp(0, last as isize) as usize;
        self.list_state.select(Some(next));
    }

    fn on_key(&mut self, key: KeyEvent) {
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Char('q'), _) => {
                self.should_quit = true;
            }
            (KeyCode::Char('j'), _) | (KeyCode::Down, _) => self.move_by(1),
            (KeyCode::Char('k'), _) | (KeyCode::Up, _) => self.move_by(-1),
            (KeyCode::Char('g'), _) | (KeyCode::Home, _) => self.move_by(isize::MIN / 2),
            (KeyCode::Char('G'), _) | (KeyCode::End, _) => self.move_by(isize::MAX / 2),
            (KeyCode::Char('r'), _) => {
                // Make the next tick refresh immediately.
                self.last_refresh = Instant::now() - REFRESH_EVERY;
                self.previews.clear();
                self.note("refreshing");
            }
            _ => {}
        }
    }

    fn on_update(&mut self, update: Update) {
        match update {
            Update::Sessions(r) => {
                self.refreshing = false;
                self.apply_refresh(*r);
            }
            Update::Preview { session, body } => {
                if self.preview_in_flight.as_deref() == Some(session.as_str()) {
                    self.preview_in_flight = None;
                }
                self.previews.insert(session, body);
            }
            Update::Failed(e) => {
                self.refreshing = false;
                self.fail(e);
            }
        }
    }

    fn apply_refresh(&mut self, r: ops::Refreshed) {
        // Keep the cursor on the same session across refreshes; index alone
        // would jump when a session is added or removed above the cursor.
        let previously = self.selected().map(|s| s.name.clone());
        self.sessions = r.sessions;

        let index = previously
            .and_then(|name| self.sessions.iter().position(|s| s.name == name))
            .or_else(|| (!self.sessions.is_empty()).then_some(0));
        self.list_state.select(index);

        // Drop previews for sessions that no longer exist.
        let live: Vec<String> = self.sessions.iter().map(|s| s.name.clone()).collect();
        self.previews.retain(|name, _| live.contains(name));

        if !r.adopted.is_empty() {
            self.note(format!("adopted {}", r.adopted.join(", ")));
        } else if !r.dead.is_empty() {
            self.fail(format!("sandbox gone: {}", r.dead.join(", ")));
        } else if let Some(w) = r.warnings.first() {
            self.fail(w.clone());
        }
    }

    fn expire_status(&mut self) {
        if self.status.is_some() && self.status_set_at.elapsed() > STATUS_LINGER {
            self.status = None;
        }
    }
}

pub fn run(client: CliClient) -> Result<(), Box<dyn std::error::Error>> {
    let worker = Worker::spawn(client);
    let mut app = App::new();

    // Installs a panic hook that restores the terminal, so a crash cannot
    // leave the user in raw mode with no echo.
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app, &worker);
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    worker: &Worker,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;

        if event::poll(TICK)?
            && let Event::Key(key) = event::read()?
            // Windows reports press and release; acting on both double-steps.
            && key.kind == KeyEventKind::Press
        {
            app.on_key(key);
        }

        while let Ok(update) = worker.rx.try_recv() {
            app.on_update(update);
        }

        if !app.refreshing && app.last_refresh.elapsed() >= REFRESH_EVERY {
            app.refreshing = true;
            app.last_refresh = Instant::now();
            worker.send(Request::Refresh);
        }

        // Fetch the selected session's preview once, lazily. Cloned out first
        // so the immutable borrow of `app` ends before the in-flight marker is
        // written.
        let wanted = app
            .preview_in_flight
            .is_none()
            .then(|| {
                app.selected()
                    .filter(|s| !app.previews.contains_key(&s.name))
                    .cloned()
            })
            .flatten();
        if let Some(session) = wanted {
            app.preview_in_flight = Some(session.name.clone());
            worker.send(Request::Preview(Box::new(session)));
        }

        app.expire_status();

        if app.should_quit {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::State;

    fn app_with(names: &[&str]) -> App {
        let mut app = App::new();
        app.sessions = names
            .iter()
            .map(|n| Session::new((*n).to_string(), "r".into(), "t".into()))
            .collect();
        app.list_state.select((!names.is_empty()).then_some(0));
        app
    }

    #[test]
    fn movement_clamps_at_both_ends() {
        let mut app = app_with(&["a", "b", "c"]);
        app.move_by(-1);
        assert_eq!(
            app.list_state.selected(),
            Some(0),
            "must not wrap past the top"
        );
        app.move_by(1);
        app.move_by(1);
        app.move_by(1);
        assert_eq!(
            app.list_state.selected(),
            Some(2),
            "must not wrap past the end"
        );
    }

    #[test]
    fn movement_on_empty_list_is_a_no_op() {
        let mut app = app_with(&[]);
        app.move_by(1);
        assert_eq!(app.list_state.selected(), None);
    }

    #[test]
    fn g_and_shift_g_jump_to_the_ends() {
        let mut app = app_with(&["a", "b", "c"]);
        app.on_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE));
        assert_eq!(app.list_state.selected(), Some(2));
        app.on_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        assert_eq!(app.list_state.selected(), Some(0));
    }

    #[test]
    fn refresh_keeps_the_cursor_on_the_same_session() {
        let mut app = app_with(&["a", "b", "c"]);
        app.list_state.select(Some(2)); // on "c"

        // "a" disappears: index 2 would now point past the end, or at the
        // wrong session. The cursor must follow "c".
        let refreshed = ops::Refreshed {
            sessions: ["b", "c"]
                .iter()
                .map(|n| Session::new((*n).to_string(), "r".into(), "t".into()))
                .collect(),
            ..Default::default()
        };
        app.apply_refresh(refreshed);

        assert_eq!(app.selected().map(|s| s.name.as_str()), Some("c"));
    }

    #[test]
    fn refresh_falls_back_to_the_first_row_when_the_session_is_gone() {
        let mut app = app_with(&["a", "b"]);
        app.list_state.select(Some(1)); // on "b"

        let refreshed = ops::Refreshed {
            sessions: vec![Session::new("a".into(), "r".into(), "t".into())],
            ..Default::default()
        };
        app.apply_refresh(refreshed);

        assert_eq!(app.selected().map(|s| s.name.as_str()), Some("a"));
    }

    #[test]
    fn refresh_drops_previews_for_vanished_sessions() {
        let mut app = app_with(&["a", "b"]);
        app.previews.insert("a".into(), "old".into());
        app.previews.insert("b".into(), "old".into());

        let refreshed = ops::Refreshed {
            sessions: vec![Session::new("a".into(), "r".into(), "t".into())],
            ..Default::default()
        };
        app.apply_refresh(refreshed);

        assert!(app.previews.contains_key("a"));
        assert!(
            !app.previews.contains_key("b"),
            "stale preview must be dropped"
        );
    }

    #[test]
    fn quit_keys() {
        let mut app = app_with(&["a"]);
        app.on_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(app.should_quit);

        let mut app = app_with(&["a"]);
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.should_quit);
    }

    #[test]
    fn status_expires() {
        let mut app = app_with(&[]);
        app.note("hello");
        assert!(app.status.is_some());
        app.status_set_at = Instant::now() - STATUS_LINGER - Duration::from_secs(1);
        app.expire_status();
        assert!(app.status.is_none());
    }

    #[test]
    fn dead_sessions_are_reported_as_an_error() {
        let mut app = app_with(&[]);
        let refreshed = ops::Refreshed {
            dead: vec!["gone".into()],
            ..Default::default()
        };
        app.apply_refresh(refreshed);
        assert!(app.status_is_error);
        assert_eq!(app.selected().map(|s| s.state), None::<State>);
    }
}
