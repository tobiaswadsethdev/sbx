//! The terminal UI.

mod attach;
mod ui;
mod worker;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use openshell_client::CliClient;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::ListState;

use crate::ops;
use crate::session::Session;
use crate::tui::attach::attach;
use worker::{Request, Update, Worker};

/// How often the session list is reconciled against the gateway.
const REFRESH_EVERY: Duration = Duration::from_secs(3);
/// How long a transient footer message stays up.
const STATUS_LINGER: Duration = Duration::from_secs(4);
/// Input poll interval. Short enough to feel immediate, long enough to idle.
const TICK: Duration = Duration::from_millis(100);
/// How long right-pane content is trusted before it is fetched again.
///
/// The agent is editing the repository continuously, so a diff the user is
/// reading has to keep up. Only the *selected* session is refetched, so this is
/// one exec per interval no matter how many sessions exist.
const PANE_TTL: Duration = Duration::from_secs(4);
/// How long a diff stat is trusted. Longer than [`PANE_TTL`] because the column
/// is a rough magnitude and every session pays for it, not just the selected
/// one.
const STAT_TTL: Duration = Duration::from_secs(15);
/// Floor on the gap between stat fetches, so a long session list cannot turn
/// into a continuous stream of execs.
const STAT_MIN_GAP: Duration = Duration::from_secs(1);

/// What the right-hand pane is showing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RightView {
    #[default]
    Preview,
    Diff,
}

impl RightView {
    fn next(self) -> Self {
        match self {
            RightView::Preview => RightView::Diff,
            RightView::Diff => RightView::Preview,
        }
    }
}

/// Which pane the movement keys act on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Focus {
    #[default]
    List,
    Right,
}

/// Scroll offset per view, so switching back and forth keeps your place.
#[derive(Debug, Clone, Copy, Default)]
struct Scroll {
    preview: u16,
    diff: u16,
}

impl Scroll {
    fn get(&self, view: RightView) -> u16 {
        match view {
            RightView::Preview => self.preview,
            RightView::Diff => self.diff,
        }
    }

    fn set(&mut self, view: RightView, offset: u16) {
        match view {
            RightView::Preview => self.preview = offset,
            RightView::Diff => self.diff = offset,
        }
    }
}

/// Fetched content plus when it arrived, so it can be refetched once stale
/// without blanking the pane in the meantime.
struct Cached<T> {
    value: T,
    at: Instant,
}

impl<T> Cached<T> {
    fn new(value: T) -> Self {
        Cached {
            value,
            at: Instant::now(),
        }
    }

    fn stale_after(&self, ttl: Duration) -> bool {
        self.at.elapsed() > ttl
    }
}

pub struct App {
    sessions: Vec<Session>,
    list_state: ListState,
    previews: HashMap<String, Cached<String>>,
    diffs: HashMap<String, Cached<String>>,
    /// `None` when the sandbox could not be read; the column stays blank.
    stats: HashMap<String, Cached<Option<ops::DiffStat>>>,
    /// Sessions whose content is currently being fetched, so the same request
    /// is not queued repeatedly while the worker is busy. One per kind, so a
    /// slow diff does not stall the stat column.
    preview_in_flight: Option<String>,
    diff_in_flight: Option<String>,
    stat_in_flight: Option<String>,
    last_stat_request: Instant,
    /// Right-pane choice per session: switching sessions must not reset it.
    views: HashMap<String, RightView>,
    scroll: HashMap<String, Scroll>,
    focus: Focus,
    /// Measured by the renderer, read by the key handler so paging and clamping
    /// know the real content and viewport heights.
    right_lines: usize,
    right_height: usize,
    status: Option<String>,
    status_is_error: bool,
    status_set_at: Instant,
    refreshing: bool,
    last_refresh: Instant,
    should_quit: bool,
    /// Set by the key handler; acted on by the event loop, which is the only
    /// place with access to the terminal.
    attach_request: Option<Session>,
}

impl App {
    fn new() -> Self {
        App {
            sessions: Vec::new(),
            list_state: ListState::default(),
            previews: HashMap::new(),
            diffs: HashMap::new(),
            stats: HashMap::new(),
            preview_in_flight: None,
            diff_in_flight: None,
            stat_in_flight: None,
            // Force an immediate first stat fetch.
            last_stat_request: Instant::now() - STAT_MIN_GAP,
            views: HashMap::new(),
            scroll: HashMap::new(),
            focus: Focus::default(),
            right_lines: 0,
            right_height: 0,
            status: None,
            status_is_error: false,
            status_set_at: Instant::now(),
            refreshing: false,
            // Force an immediate first refresh.
            last_refresh: Instant::now() - REFRESH_EVERY,
            should_quit: false,
            attach_request: None,
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

    /// Name of the selected session, for keying the per-session maps.
    fn selected_name(&self) -> Option<String> {
        self.selected().map(|s| s.name.clone())
    }

    /// Which view the selected session's right pane is showing.
    fn right_view(&self) -> RightView {
        self.selected()
            .and_then(|s| self.views.get(&s.name))
            .copied()
            .unwrap_or_default()
    }

    fn toggle_right_view(&mut self) {
        if let Some(name) = self.selected_name() {
            let next = self.right_view().next();
            self.views.insert(name, next);
        }
    }

    /// Current scroll offset of the selected session's right pane.
    fn right_scroll(&self) -> u16 {
        let view = self.right_view();
        self.selected()
            .and_then(|s| self.scroll.get(&s.name))
            .map(|sc| sc.get(view))
            .unwrap_or(0)
    }

    /// Highest offset that still shows content, from the last measured render.
    fn max_scroll(&self) -> u16 {
        u16::try_from(self.right_lines.saturating_sub(self.right_height)).unwrap_or(u16::MAX)
    }

    /// Scroll the right pane, clamped to the measured content.
    ///
    /// `isize` rather than `i16` so callers can pass a saturating "to the top"
    /// or "to the bottom" without knowing the content height.
    fn scroll_by(&mut self, delta: isize) {
        let Some(name) = self.selected_name() else {
            return;
        };
        let view = self.right_view();
        let current = self.right_scroll() as isize;
        let next = (current + delta).clamp(0, self.max_scroll() as isize) as u16;
        self.scroll.entry(name).or_default().set(view, next);
    }

    /// A page is a screenful less one line, so a landmark stays visible across
    /// the jump.
    fn page(&self) -> isize {
        self.right_height.saturating_sub(1).max(1) as isize
    }

    /// Forget everything fetched for a session. Called when the repository is
    /// known to have moved underneath us, e.g. after an attach.
    fn invalidate(&mut self, name: &str) {
        self.previews.remove(name);
        self.diffs.remove(name);
        self.stats.remove(name);
    }

    fn on_key(&mut self, key: KeyEvent) {
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Char('q'), _) => {
                self.should_quit = true;
            }
            // Pane movement, mirroring Ctrl-w h/l in vim. Available from either
            // pane so there is always a way back.
            (KeyCode::Char('h'), _) | (KeyCode::Left, _) => self.focus = Focus::List,
            (KeyCode::Char('l'), _) | (KeyCode::Right, _) => self.focus = Focus::Right,
            // Cycling the right pane works from either side: wanting to see the
            // diff should not first require focusing it.
            (KeyCode::Tab, _) | (KeyCode::BackTab, _) => self.toggle_right_view(),
            (KeyCode::Enter, _) | (KeyCode::Char('a'), _) => {
                self.attach_request = self.selected().cloned();
            }
            (KeyCode::Char('r'), _) => {
                // Make the next tick refresh immediately.
                self.last_refresh = Instant::now() - REFRESH_EVERY;
                self.previews.clear();
                self.diffs.clear();
                self.stats.clear();
                self.note("refreshing");
            }
            // The movement keys act on whichever pane has focus.
            (code, _) if self.focus == Focus::Right => match code {
                KeyCode::Char('j') | KeyCode::Down => self.scroll_by(1),
                KeyCode::Char('k') | KeyCode::Up => self.scroll_by(-1),
                KeyCode::PageDown => self.scroll_by(self.page()),
                KeyCode::PageUp => self.scroll_by(-self.page()),
                KeyCode::Char('g') | KeyCode::Home => self.scroll_by(isize::MIN / 2),
                KeyCode::Char('G') | KeyCode::End => self.scroll_by(isize::MAX / 2),
                _ => {}
            },
            (KeyCode::Char('j'), _) | (KeyCode::Down, _) => self.move_by(1),
            (KeyCode::Char('k'), _) | (KeyCode::Up, _) => self.move_by(-1),
            (KeyCode::PageDown, _) => self.move_by(self.page()),
            (KeyCode::PageUp, _) => self.move_by(-self.page()),
            (KeyCode::Char('g'), _) | (KeyCode::Home, _) => self.move_by(isize::MIN / 2),
            (KeyCode::Char('G'), _) | (KeyCode::End, _) => self.move_by(isize::MAX / 2),
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
                self.previews.insert(session, Cached::new(body));
            }
            Update::Diff { session, body } => {
                if self.diff_in_flight.as_deref() == Some(session.as_str()) {
                    self.diff_in_flight = None;
                }
                self.diffs.insert(session, Cached::new(body));
            }
            Update::Stat { session, stat } => {
                if self.stat_in_flight.as_deref() == Some(session.as_str()) {
                    self.stat_in_flight = None;
                }
                self.stats.insert(session, Cached::new(stat));
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

        // Drop everything keyed by a session that no longer exists, or the maps
        // grow without bound over a long-running TUI.
        let live: Vec<String> = self.sessions.iter().map(|s| s.name.clone()).collect();
        self.previews.retain(|name, _| live.contains(name));
        self.diffs.retain(|name, _| live.contains(name));
        self.stats.retain(|name, _| live.contains(name));
        self.views.retain(|name, _| live.contains(name));
        self.scroll.retain(|name, _| live.contains(name));

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
    // The worker owns its client; attaching needs a second handle because it
    // runs on this thread with the terminal handed over.
    let attach_client = client.clone();
    let worker = Worker::spawn(client);
    let mut app = App::new();

    // Installs a panic hook that restores the terminal, so a crash cannot
    // leave the user in raw mode with no echo.
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app, &worker, &attach_client);
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    worker: &Worker,
    attach_client: &CliClient,
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

        if let Some(session) = app.attach_request.take() {
            match attach(terminal, attach_client, &session) {
                Ok(()) => {
                    // The repository almost certainly moved while attached.
                    app.invalidate(&session.name);
                    app.note(format!("detached from {}", session.name));
                }
                Err(e) => app.fail(format!("attach failed: {e}")),
            }
            continue;
        }

        while let Ok(update) = worker.rx.try_recv() {
            app.on_update(update);
        }

        if !app.refreshing && app.last_refresh.elapsed() >= REFRESH_EVERY {
            app.refreshing = true;
            app.last_refresh = Instant::now();
            worker.send(Request::Refresh);
        }

        dispatch_fetches(app, worker);

        app.expire_status();

        if app.should_quit {
            return Ok(());
        }
    }
}

/// Queue the gateway reads the current view needs, and nothing more.
///
/// Exec on a sandbox is serialised gateway-side, so a wasted read does not just
/// cost time -- it delays the next one for that session. Two budgets are kept
/// deliberately separate:
///
/// * the **right pane**, for the selected session only, refetched every
///   [`PANE_TTL`] so a diff under the user's eyes stays current;
/// * the **stat column**, which every row needs, round-robined over the whole
///   list at no more than one request per [`STAT_MIN_GAP`].
///
/// The total is therefore bounded by the refresh intervals rather than by the
/// number of sessions.
fn dispatch_fetches(app: &mut App, worker: &Worker) {
    // Cloned out first so the immutable borrow of `app` ends before the
    // in-flight markers are written.
    let selected = app.selected().cloned();
    if let Some(session) = selected {
        match app.right_view() {
            RightView::Preview => {
                let due = app
                    .previews
                    .get(&session.name)
                    .is_none_or(|c| c.stale_after(PANE_TTL));
                if due && app.preview_in_flight.is_none() {
                    app.preview_in_flight = Some(session.name.clone());
                    worker.send(Request::Preview(Box::new(session.clone())));
                }
            }
            RightView::Diff => {
                let due = app
                    .diffs
                    .get(&session.name)
                    .is_none_or(|c| c.stale_after(PANE_TTL));
                if due && app.diff_in_flight.is_none() {
                    app.diff_in_flight = Some(session.name.clone());
                    worker.send(Request::Diff(Box::new(session.clone())));
                }
            }
        }
    }

    if app.stat_in_flight.is_some() || app.last_stat_request.elapsed() < STAT_MIN_GAP {
        return;
    }
    if let Some(session) = next_stat_target(app) {
        app.last_stat_request = Instant::now();
        app.stat_in_flight = Some(session.name.clone());
        worker.send(Request::Stat(Box::new(session)));
    }
}

/// The session whose stat is most worth fetching: the selected one first, since
/// that is the number being read, then whichever has been stale longest.
fn next_stat_target(app: &App) -> Option<Session> {
    let due = |s: &Session| {
        app.stats
            .get(&s.name)
            .is_none_or(|c| c.stale_after(STAT_TTL))
    };

    if let Some(s) = app.selected().filter(|s| due(s)) {
        return Some(s.clone());
    }
    app.sessions
        .iter()
        .filter(|s| due(s))
        // Never fetched sorts before any fetched one, so no session starves.
        .max_by_key(|s| {
            app.stats
                .get(&s.name)
                .map_or(Duration::MAX, |c| c.at.elapsed())
        })
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::State;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

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
        app.previews.insert("a".into(), Cached::new("old".into()));
        app.previews.insert("b".into(), Cached::new("old".into()));

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
    fn enter_requests_an_attach_to_the_selected_session() {
        let mut app = app_with(&["a", "b"]);
        app.move_by(1);
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            app.attach_request.as_ref().map(|s| s.name.as_str()),
            Some("b")
        );
    }

    #[test]
    fn enter_on_an_empty_list_requests_nothing() {
        let mut app = app_with(&[]);
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.attach_request.is_none());
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

    /// The plan calls for the choice to be remembered per session, so that
    /// glancing at another session's preview does not lose the diff you were
    /// reading.
    #[test]
    fn the_right_pane_choice_is_remembered_per_session() {
        let mut app = app_with(&["a", "b"]);
        assert_eq!(app.right_view(), RightView::Preview, "preview by default");

        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.right_view(), RightView::Diff);

        // Move to "b": it has its own, untouched choice.
        app.move_by(1);
        assert_eq!(app.right_view(), RightView::Preview);

        // Back to "a": the diff is still selected.
        app.move_by(-1);
        assert_eq!(app.right_view(), RightView::Diff);

        // And cycling returns.
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.right_view(), RightView::Preview);
    }

    #[test]
    fn tab_on_an_empty_list_is_a_no_op() {
        let mut app = app_with(&[]);
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.right_view(), RightView::Preview);
    }

    #[test]
    fn focus_decides_which_pane_the_movement_keys_move() {
        let mut app = app_with(&["a", "b", "c"]);
        app.right_lines = 100;
        app.right_height = 10;

        // Focus starts on the list.
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.list_state.selected(), Some(1));
        assert_eq!(app.right_scroll(), 0);

        app.on_key(key(KeyCode::Char('l')));
        assert_eq!(app.focus, Focus::Right);

        // Now j scrolls and leaves the selection alone.
        app.on_key(key(KeyCode::Char('j')));
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(
            app.list_state.selected(),
            Some(1),
            "selection must not move"
        );
        assert_eq!(app.right_scroll(), 2);

        app.on_key(key(KeyCode::Char('h')));
        assert_eq!(app.focus, Focus::List);
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.list_state.selected(), Some(2));
    }

    #[test]
    fn scrolling_clamps_to_the_measured_content() {
        let mut app = app_with(&["a"]);
        app.focus = Focus::Right;
        app.right_lines = 30;
        app.right_height = 10;
        assert_eq!(app.max_scroll(), 20);

        app.on_key(key(KeyCode::Char('G')));
        assert_eq!(app.right_scroll(), 20, "G goes to the last screenful");
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.right_scroll(), 20, "must not scroll past the end");

        app.on_key(key(KeyCode::Char('g')));
        assert_eq!(app.right_scroll(), 0);
        app.on_key(key(KeyCode::Char('k')));
        assert_eq!(app.right_scroll(), 0, "must not scroll above the top");

        // A page is a screenful less one line, so a landmark stays visible.
        app.on_key(key(KeyCode::PageDown));
        assert_eq!(app.right_scroll(), 9);
        app.on_key(key(KeyCode::PageUp));
        assert_eq!(app.right_scroll(), 0);
    }

    /// Content shorter than the pane must not be scrollable at all, or the user
    /// can scroll a three-line diff off the screen.
    #[test]
    fn content_shorter_than_the_pane_does_not_scroll() {
        let mut app = app_with(&["a"]);
        app.focus = Focus::Right;
        app.right_lines = 4;
        app.right_height = 20;
        assert_eq!(app.max_scroll(), 0);
        app.on_key(key(KeyCode::Char('G')));
        assert_eq!(app.right_scroll(), 0);
    }

    #[test]
    fn scroll_offsets_are_kept_per_view_and_per_session() {
        let mut app = app_with(&["a", "b"]);
        app.focus = Focus::Right;
        app.right_lines = 100;
        app.right_height = 10;

        // Scroll "a"'s preview, then switch "a" to the diff: a fresh view.
        app.scroll_by(5);
        assert_eq!(app.right_scroll(), 5);
        app.toggle_right_view();
        assert_eq!(app.right_scroll(), 0, "the diff has its own offset");
        app.scroll_by(7);

        // "b" is untouched.
        app.move_by(1);
        assert_eq!(app.right_scroll(), 0);

        // Both of "a"'s offsets survived.
        app.move_by(-1);
        assert_eq!(app.right_scroll(), 7);
        app.toggle_right_view();
        assert_eq!(app.right_scroll(), 5);
    }

    /// The stat column is the one read that scales with the number of sessions,
    /// so the selected session is served first and no other session starves.
    #[test]
    fn stats_prefer_the_selected_session_then_the_stalest() {
        let mut app = app_with(&["a", "b", "c"]);
        app.move_by(1); // on "b"

        assert_eq!(
            next_stat_target(&app).map(|s| s.name),
            Some("b".to_string()),
            "the number being looked at comes first"
        );

        // With "b" measured, the others are picked up.
        app.stats.insert("b".into(), Cached::new(None));
        let next = next_stat_target(&app).map(|s| s.name).unwrap();
        assert!(next == "a" || next == "c", "got {next}");

        // Once everything is fresh there is nothing to do.
        app.stats.insert("a".into(), Cached::new(None));
        app.stats.insert("c".into(), Cached::new(None));
        assert!(next_stat_target(&app).is_none());

        // A stale entry becomes a candidate again.
        app.stats.insert(
            "c".into(),
            Cached {
                value: None,
                at: Instant::now() - STAT_TTL - Duration::from_secs(1),
            },
        );
        assert_eq!(
            next_stat_target(&app).map(|s| s.name),
            Some("c".to_string())
        );
    }

    #[test]
    fn stats_pick_the_never_fetched_session_over_a_merely_stale_one() {
        let mut app = app_with(&["a", "b"]);
        app.list_state.select(None); // nothing selected, so no preference
        app.stats.insert(
            "a".into(),
            Cached {
                value: None,
                at: Instant::now() - STAT_TTL - Duration::from_secs(1),
            },
        );
        // "b" has never been fetched, which must outrank "a" being stale.
        assert_eq!(
            next_stat_target(&app).map(|s| s.name),
            Some("b".to_string())
        );
    }

    #[test]
    fn refresh_drops_diffs_stats_views_and_scroll_for_vanished_sessions() {
        let mut app = app_with(&["a", "b"]);
        for name in ["a", "b"] {
            app.diffs.insert(name.into(), Cached::new("d".into()));
            app.stats.insert(name.into(), Cached::new(None));
            app.views.insert(name.into(), RightView::Diff);
            app.scroll.insert(name.into(), Scroll::default());
        }

        let refreshed = ops::Refreshed {
            sessions: vec![Session::new("a".into(), "r".into(), "t".into())],
            ..Default::default()
        };
        app.apply_refresh(refreshed);

        assert!(app.diffs.contains_key("a"));
        for map_is_empty in [
            !app.diffs.contains_key("b"),
            !app.stats.contains_key("b"),
            !app.views.contains_key("b"),
            !app.scroll.contains_key("b"),
        ] {
            assert!(map_is_empty, "every per-session map must be pruned");
        }
    }

    /// Attaching hands the terminal to the agent, which then edits the
    /// repository, so everything read from it beforehand is stale.
    #[test]
    fn invalidate_clears_every_cached_read() {
        let mut app = app_with(&["a"]);
        app.previews.insert("a".into(), Cached::new("p".into()));
        app.diffs.insert("a".into(), Cached::new("d".into()));
        app.stats.insert("a".into(), Cached::new(None));

        app.invalidate("a");

        assert!(app.previews.is_empty());
        assert!(app.diffs.is_empty());
        assert!(app.stats.is_empty());
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
