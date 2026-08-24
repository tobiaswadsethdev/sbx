//! The terminal UI.

mod attach;
mod ui;
mod worker;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use openshell_client::{CliClient, PolicyRevision, PolicyUpdate};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::ListState;

use crate::ops;
use crate::policy;
use crate::session::{Session, State};
use crate::status;
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
/// How long a poll -- diff stat plus agent state -- is trusted.
///
/// Shorter than a stat alone would need, because the same exec now carries the
/// "this agent needs you" signal and that is worth being prompt about. Every
/// session pays for it, not just the selected one, so it is bounded below by
/// [`POLL_MIN_GAP`].
const POLL_TTL: Duration = Duration::from_secs(6);
/// Floor on the gap between polls, so a long session list cannot turn into a
/// continuous stream of execs. With N sessions a full round trip takes at worst
/// N times this, and the exec rate never exceeds one per interval.
const POLL_MIN_GAP: Duration = Duration::from_secs(1);

/// What the right-hand pane is showing.
///
/// The order is the Tab order, and it runs outward from the session: what it is
/// (preview), what it has done (diff), what it is allowed to do (policy), what
/// it has actually tried (events).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum RightView {
    #[default]
    Preview,
    Diff,
    Policy,
    Events,
}

impl RightView {
    const ORDER: [RightView; 4] = [
        RightView::Preview,
        RightView::Diff,
        RightView::Policy,
        RightView::Events,
    ];

    fn next(self) -> Self {
        let i = Self::ORDER.iter().position(|v| *v == self).unwrap_or(0);
        Self::ORDER[(i + 1) % Self::ORDER.len()]
    }

    fn prev(self) -> Self {
        let i = Self::ORDER.iter().position(|v| *v == self).unwrap_or(0);
        Self::ORDER[(i + Self::ORDER.len() - 1) % Self::ORDER.len()]
    }

    /// How long fetched content stays fresh.
    ///
    /// Not one constant, because the panes want different things. A diff under
    /// the user's eyes has to keep up with the agent editing underneath it; a
    /// policy only changes when someone changes it, and refetching it every few
    /// seconds would spend a subprocess on an answer that is never different.
    /// The events feed is the fastest, because it is a feed.
    fn ttl(self) -> Duration {
        match self {
            RightView::Preview | RightView::Diff => PANE_TTL,
            RightView::Policy => Duration::from_secs(30),
            RightView::Events => Duration::from_secs(3),
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
#[derive(Debug, Clone, Default)]
struct Scroll(HashMap<RightView, u16>);

impl Scroll {
    fn get(&self, view: RightView) -> u16 {
        self.0.get(&view).copied().unwrap_or(0)
    }

    fn set(&mut self, view: RightView, offset: u16) {
        self.0.insert(view, offset);
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
    /// Diff stat and agent state per session, from one exec each.
    polls: HashMap<String, Cached<ops::Poll>>,
    /// The effective policy, and the reason if it could not be read. Both are
    /// worth caching: an unreachable gateway should not blank the pane on every
    /// tick, it should keep saying why.
    policies: HashMap<String, Cached<Result<PolicyRevision, String>>>,
    events: HashMap<String, Cached<Result<Vec<crate::events::Event>, String>>>,
    /// Sessions whose content is currently being fetched, so the same request
    /// is not queued repeatedly while the worker is busy. One per kind, so a
    /// slow diff does not stall the stat column.
    preview_in_flight: Option<String>,
    diff_in_flight: Option<String>,
    poll_in_flight: Option<String>,
    policy_in_flight: Option<String>,
    events_in_flight: Option<String>,
    /// A policy change in progress. Blocks a second one for the same session:
    /// two overlapping updates would race on the revision and the loser's
    /// endpoints would silently not be there.
    repolicy_in_flight: Option<String>,
    last_poll_request: Instant,
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
    /// Set by the key handler; sent by the event loop, which owns the worker.
    repolicy_request: Option<(Session, Box<PolicyUpdate>, String)>,
}

impl App {
    fn new() -> Self {
        App {
            sessions: Vec::new(),
            list_state: ListState::default(),
            previews: HashMap::new(),
            diffs: HashMap::new(),
            polls: HashMap::new(),
            policies: HashMap::new(),
            events: HashMap::new(),
            preview_in_flight: None,
            diff_in_flight: None,
            poll_in_flight: None,
            policy_in_flight: None,
            events_in_flight: None,
            repolicy_in_flight: None,
            // Force an immediate first poll.
            last_poll_request: Instant::now() - POLL_MIN_GAP,
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
            repolicy_request: None,
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

    fn cycle_right_view(&mut self, step: fn(RightView) -> RightView) {
        if let Some(name) = self.selected_name() {
            let next = step(self.right_view());
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

    /// The last poll for a session, if one has come back.
    pub fn poll(&self, name: &str) -> Option<&ops::Poll> {
        self.polls.get(name).map(|c| &c.value)
    }

    /// What to show in the state column.
    ///
    /// The gateway only knows whether the sandbox is up; what the *agent* is
    /// doing comes from polling it. The agent's answer wins when there is one,
    /// but only over `Ready` -- `Dead`, `Failed` and the in-flight states are
    /// facts about the sandbox that a stale poll must not paper over.
    pub fn effective_state(&self, session: &Session) -> State {
        if session.state != State::Ready {
            return session.state;
        }
        self.poll(&session.name)
            .and_then(|p| p.status.as_ref())
            .map_or(session.state, |r| r.state)
    }

    /// What the agent is doing, for the preview pane.
    pub fn agent_status(&self, session: &Session) -> Option<&status::Report> {
        self.poll(&session.name).and_then(|p| p.status.as_ref())
    }

    /// How many sessions need attention. Drives the count in the list title, so
    /// a waiting session is visible even when it is scrolled out of view.
    pub fn waiting_count(&self) -> usize {
        self.sessions
            .iter()
            .filter(|s| self.effective_state(s) == State::Waiting)
            .count()
    }

    /// The last policy read for a session, if one has come back.
    pub fn policy(&self, name: &str) -> Option<&Result<PolicyRevision, String>> {
        self.policies.get(name).map(|c| &c.value)
    }

    /// The last events read for a session, if any have come back.
    pub fn events(&self, name: &str) -> Option<&Result<Vec<crate::events::Event>, String>> {
        self.events.get(name).map(|c| &c.value)
    }

    /// Whether a policy change is in flight for the selected session, so the
    /// pane can say "widening ..." rather than looking like nothing happened
    /// for the six seconds the gateway takes to load a revision.
    pub fn repolicying(&self) -> Option<&str> {
        self.repolicy_in_flight.as_deref()
    }

    /// Whether the registries preset is currently in force, which decides
    /// whether `w` or `t` is the useful key.
    pub fn widened(&self, name: &str) -> Option<bool> {
        match self.policy(name)? {
            Ok(rev) => rev
                .policy
                .as_ref()
                .map(|p| !policy::preset_rule_names(p, &policy::REGISTRIES).is_empty()),
            Err(_) => None,
        }
    }

    /// Widen or tighten the selected session's egress.
    ///
    /// Only the network section, and deliberately so: the filesystem and
    /// process sections are fixed when the sandbox is created, and the gateway
    /// will happily accept a change to them, report it as effective, and never
    /// enforce it. Offering that would be worse than not offering it.
    fn request_repolicy(&mut self, widen: bool) -> Option<(Session, Box<PolicyUpdate>, String)> {
        let session = self.selected().cloned()?;
        if self.repolicy_in_flight.is_some() {
            self.fail("a policy change is already in flight");
            return None;
        }
        // Refusing rather than guessing: without a policy read there is no way
        // to know whether the preset is already applied, and a widen issued
        // blind would report a change it did not make.
        let Some(applied) = self.widened(&session.name) else {
            self.fail("the policy has not been read yet");
            return None;
        };
        if applied == widen {
            self.note(if widen {
                "the registries are already reachable"
            } else {
                "the registries are already denied"
            });
            return None;
        }

        let preset = &policy::REGISTRIES;
        let (update, label) = if widen {
            (
                preset.widen(),
                format!("widened: {} now reachable", preset.label),
            )
        } else {
            (
                preset.tighten(),
                format!("tightened: {} denied again", preset.label),
            )
        };
        self.repolicy_in_flight = Some(session.name.clone());
        // Switching to the pane makes the consequence visible, which is the
        // only reason it is safe to bind this to one key.
        self.views.insert(session.name.clone(), RightView::Policy);
        self.note(format!(
            "{} ... the gateway takes a few seconds to load a revision",
            if widen { "widening" } else { "tightening" }
        ));
        Some((session, Box::new(update), label))
    }

    /// Forget everything fetched for a session. Called when the repository is
    /// known to have moved underneath us, e.g. after an attach.
    fn invalidate(&mut self, name: &str) {
        self.previews.remove(name);
        self.diffs.remove(name);
        self.polls.remove(name);
        self.policies.remove(name);
        self.events.remove(name);
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
            // diff should not first require focusing it. Shift-Tab goes back,
            // which matters now there are four views rather than two.
            (KeyCode::Tab, _) => self.cycle_right_view(RightView::next),
            (KeyCode::BackTab, _) => self.cycle_right_view(RightView::prev),
            // Widen and tighten egress. Only bound while the policy pane is
            // showing, so the rules being changed are on screen when the key is
            // pressed and the result is visible without doing anything else.
            (KeyCode::Char('w'), _) if self.right_view() == RightView::Policy => {
                self.repolicy_request = self.request_repolicy(true);
            }
            (KeyCode::Char('t'), _) if self.right_view() == RightView::Policy => {
                self.repolicy_request = self.request_repolicy(false);
            }
            (KeyCode::Enter, _) | (KeyCode::Char('a'), _) => {
                self.attach_request = self.selected().cloned();
            }
            (KeyCode::Char('r'), _) => {
                // Make the next tick refresh immediately.
                self.last_refresh = Instant::now() - REFRESH_EVERY;
                self.previews.clear();
                self.diffs.clear();
                self.polls.clear();
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
            Update::Polled { session, poll } => {
                if self.poll_in_flight.as_deref() == Some(session.as_str()) {
                    self.poll_in_flight = None;
                }
                self.polls.insert(session, Cached::new(*poll));
            }
            Update::Policy { session, result } => {
                if self.policy_in_flight.as_deref() == Some(session.as_str()) {
                    self.policy_in_flight = None;
                }
                self.policies.insert(session, Cached::new(*result));
            }
            Update::Events { session, result } => {
                if self.events_in_flight.as_deref() == Some(session.as_str()) {
                    self.events_in_flight = None;
                }
                self.events.insert(session, Cached::new(*result));
            }
            Update::Repolicied {
                session,
                label,
                result,
            } => {
                if self.repolicy_in_flight.as_deref() == Some(session.as_str()) {
                    self.repolicy_in_flight = None;
                }
                match &*result {
                    Ok(_) => self.note(label),
                    Err(e) => self.fail(e.clone()),
                }
                // The revision that came back is the authority, so it replaces
                // the cached one instead of merely invalidating it -- otherwise
                // the pane shows the pre-change rules until the next TTL.
                self.policies.insert(session, Cached::new(*result));
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
        self.polls.retain(|name, _| live.contains(name));
        self.policies.retain(|name, _| live.contains(name));
        self.events.retain(|name, _| live.contains(name));
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

        if let Some((session, update, label)) = app.repolicy_request.take() {
            worker.send(Request::Repolicy {
                session: Box::new(session),
                update,
                label,
            });
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
        let view = app.right_view();
        let ttl = view.ttl();
        let name = session.name.clone();
        // Each arm asks the same three questions -- is it stale, is one already
        // in flight, and if not, fetch -- of a different map, so the shapes are
        // spelled out rather than abstracted over. Four near-identical closures
        // over four differently-typed maps costs more than it saves.
        let due = match view {
            RightView::Preview => app.previews.get(&name).is_none_or(|c| c.stale_after(ttl)),
            RightView::Diff => app.diffs.get(&name).is_none_or(|c| c.stale_after(ttl)),
            RightView::Policy => app.policies.get(&name).is_none_or(|c| c.stale_after(ttl)),
            RightView::Events => app.events.get(&name).is_none_or(|c| c.stale_after(ttl)),
        };
        if due {
            match view {
                RightView::Preview if app.preview_in_flight.is_none() => {
                    app.preview_in_flight = Some(name);
                    worker.send(Request::Preview(Box::new(session.clone())));
                }
                RightView::Diff if app.diff_in_flight.is_none() => {
                    app.diff_in_flight = Some(name);
                    worker.send(Request::Diff(Box::new(session.clone())));
                }
                // Not while a change is in flight: the refetch would land
                // before the update finished and put the pre-change rules back
                // on screen, which reads as the widen having failed.
                RightView::Policy
                    if app.policy_in_flight.is_none() && app.repolicy_in_flight.is_none() =>
                {
                    app.policy_in_flight = Some(name);
                    worker.send(Request::Policy(Box::new(session.clone())));
                }
                RightView::Events if app.events_in_flight.is_none() => {
                    app.events_in_flight = Some(name);
                    worker.send(Request::Events(Box::new(session.clone())));
                }
                _ => {}
            }
        }
    }

    if app.poll_in_flight.is_some() || app.last_poll_request.elapsed() < POLL_MIN_GAP {
        return;
    }
    if let Some(session) = next_poll_target(app) {
        app.last_poll_request = Instant::now();
        app.poll_in_flight = Some(session.name.clone());
        worker.send(Request::Poll(Box::new(session)));
    }
}

/// The session most worth polling: the selected one first, since that is what
/// is being read, then whichever has been stale longest.
fn next_poll_target(app: &App) -> Option<Session> {
    let due = |s: &Session| {
        app.polls
            .get(&s.name)
            .is_none_or(|c| c.stale_after(POLL_TTL))
    };

    if let Some(s) = app.selected().filter(|s| due(s)) {
        return Some(s.clone());
    }
    app.sessions
        .iter()
        .filter(|s| due(s))
        // Never polled sorts before any polled one, so no session starves.
        .max_by_key(|s| {
            app.polls
                .get(&s.name)
                .map_or(Duration::MAX, |c| c.at.elapsed())
        })
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

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

        // And cycling all the way round returns.
        for _ in 1..RightView::ORDER.len() {
            app.on_key(key(KeyCode::Tab));
        }
        assert_eq!(app.right_view(), RightView::Preview);

        // Shift-Tab walks back, which is the only sane way to reach the last
        // view once there are four of them.
        app.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
        assert_eq!(app.right_view(), RightView::Events);
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
        app.cycle_right_view(RightView::next);
        assert_eq!(app.right_scroll(), 0, "the diff has its own offset");
        app.scroll_by(7);

        // "b" is untouched.
        app.move_by(1);
        assert_eq!(app.right_scroll(), 0);

        // Both of "a"'s offsets survived.
        app.move_by(-1);
        assert_eq!(app.right_scroll(), 7);
        app.cycle_right_view(RightView::prev);
        assert_eq!(app.right_scroll(), 5);
    }

    /// Every view keeps its own offset, not just the first two. A shared one
    /// would drop the user halfway down a policy after reading a long diff.
    #[test]
    fn all_four_views_scroll_independently() {
        let mut app = app_with(&["a"]);
        app.focus = Focus::Right;
        app.right_lines = 500;
        app.right_height = 10;

        for (i, _) in RightView::ORDER.iter().enumerate() {
            app.scroll_by(i as isize + 1);
            app.cycle_right_view(RightView::next);
        }
        // Back at the start after a full cycle, with each offset intact.
        for (i, view) in RightView::ORDER.iter().enumerate() {
            assert_eq!(app.right_view(), *view);
            assert_eq!(app.right_scroll(), i as u16 + 1, "{view:?}");
            app.cycle_right_view(RightView::next);
        }
    }

    fn policy_with_registries(applied: bool) -> PolicyRevision {
        let mut rev: PolicyRevision = serde_json::from_value(serde_json::json!({
            "version": 1, "active_version": 1, "hash": "abc",
        }))
        .unwrap();
        let mut p = openshell_client::Policy::default();
        if applied {
            p.network_policies.insert(
                // A name the gateway chose, not one we asked for.
                "registry-npmjs-org".into(),
                openshell_client::NetworkPolicy {
                    name: None,
                    endpoints: vec![openshell_client::Endpoint {
                        host: "registry.npmjs.org".into(),
                        port: 443,
                        ..Default::default()
                    }],
                    binaries: vec![],
                },
            );
        }
        rev.policy = Some(p);
        rev
    }

    /// Widening is a change to a security boundary on one keypress, so every
    /// path that could make it happen by accident is worth a test.
    #[test]
    fn widening_needs_a_policy_read_first() {
        let mut app = app_with(&["a"]);
        // Nothing read yet: refuse rather than issue a change blind, which
        // would report a widen whether or not it altered anything.
        assert!(app.request_repolicy(true).is_none());
        assert!(app.status_is_error);
        assert!(app.repolicy_in_flight.is_none());
    }

    #[test]
    fn widening_is_a_no_op_when_already_applied() {
        let mut app = app_with(&["a"]);
        app.policies
            .insert("a".into(), Cached::new(Ok(policy_with_registries(true))));
        assert_eq!(app.widened("a"), Some(true));

        assert!(app.request_repolicy(true).is_none(), "already reachable");
        assert!(!app.status_is_error, "a no-op is not an error");
        assert!(app.repolicy_in_flight.is_none());

        // Tightening from the same state is the one that should go through.
        let (session, update, label) = app.request_repolicy(false).expect("a tighten");
        assert_eq!(session.name, "a");
        assert!(!update.remove_endpoints.is_empty());
        assert!(update.add_endpoints.is_empty());
        assert!(label.contains("tightened"));
        assert_eq!(app.repolicy_in_flight.as_deref(), Some("a"));
    }

    #[test]
    fn widening_switches_to_the_pane_that_shows_the_result() {
        let mut app = app_with(&["a"]);
        app.policies
            .insert("a".into(), Cached::new(Ok(policy_with_registries(false))));
        assert_eq!(app.widened("a"), Some(false));

        assert!(app.request_repolicy(true).is_some());
        assert_eq!(
            app.right_view(),
            RightView::Policy,
            "the consequence has to be on screen"
        );
    }

    /// Two overlapping updates would race on the revision, and the loser's
    /// endpoints would silently not be there.
    #[test]
    fn a_second_change_is_refused_while_one_is_in_flight() {
        let mut app = app_with(&["a"]);
        app.policies
            .insert("a".into(), Cached::new(Ok(policy_with_registries(false))));
        assert!(app.request_repolicy(true).is_some());
        assert!(app.request_repolicy(false).is_none());
        assert!(app.status_is_error);
    }

    /// The keys only exist in the policy pane, so that the rules being changed
    /// are on screen when the key is pressed. In any other view `w` and `t`
    /// must fall through to the movement handling rather than widening egress.
    #[test]
    fn the_widen_keys_are_inert_outside_the_policy_pane() {
        let mut app = app_with(&["a"]);
        app.policies
            .insert("a".into(), Cached::new(Ok(policy_with_registries(false))));

        for view in [RightView::Preview, RightView::Diff, RightView::Events] {
            app.views.insert("a".into(), view);
            app.on_key(key(KeyCode::Char('w')));
            app.on_key(key(KeyCode::Char('t')));
            assert!(app.repolicy_request.is_none(), "{view:?}");
            assert!(app.repolicy_in_flight.is_none(), "{view:?}");
        }

        app.views.insert("a".into(), RightView::Policy);
        app.on_key(key(KeyCode::Char('w')));
        assert!(app.repolicy_request.is_some(), "bound in the policy pane");
    }

    /// A failed read must not read as "the registries are denied", or the widen
    /// key would report success against a sandbox it never reached.
    #[test]
    fn an_unreadable_policy_is_not_a_narrow_one() {
        let mut app = app_with(&["a"]);
        app.policies
            .insert("a".into(), Cached::new(Err("gateway down".into())));
        assert_eq!(app.widened("a"), None);
        assert!(app.request_repolicy(true).is_none());
        assert!(app.status_is_error);
    }

    /// The revision that comes back from a change is the authority. Merely
    /// invalidating the cache would leave the pane showing the pre-change rules
    /// until the next fetch, which reads as the widen having failed.
    #[test]
    fn a_completed_change_replaces_the_cached_policy() {
        let mut app = app_with(&["a"]);
        app.policies
            .insert("a".into(), Cached::new(Ok(policy_with_registries(false))));
        app.repolicy_in_flight = Some("a".into());

        app.on_update(Update::Repolicied {
            session: "a".into(),
            label: "widened: registries now reachable".into(),
            result: Box::new(Ok(policy_with_registries(true))),
        });

        assert!(app.repolicy_in_flight.is_none());
        assert_eq!(app.widened("a"), Some(true));
        assert!(!app.status_is_error);
        assert_eq!(
            app.status.as_deref(),
            Some("widened: registries now reachable")
        );
    }

    #[test]
    fn a_failed_change_is_reported_as_an_error() {
        let mut app = app_with(&["a"]);
        app.repolicy_in_flight = Some("a".into());
        app.on_update(Update::Repolicied {
            session: "a".into(),
            label: "widened".into(),
            result: Box::new(Err("policy update failed: exit 1".into())),
        });
        assert!(app.status_is_error);
        assert!(app.status.as_deref().unwrap().contains("exit 1"));
        assert!(app.repolicy_in_flight.is_none());
    }

    fn poll_with(state: Option<State>) -> ops::Poll {
        ops::Poll {
            stat: None,
            status: state.map(|state| status::Report {
                state,
                detail: None,
                source: status::Source::Hook,
            }),
        }
    }

    /// The poll is the one read that scales with the number of sessions, so the
    /// selected session is served first and no other session starves.
    #[test]
    fn polls_prefer_the_selected_session_then_the_stalest() {
        let mut app = app_with(&["a", "b", "c"]);
        app.move_by(1); // on "b"

        assert_eq!(
            next_poll_target(&app).map(|s| s.name),
            Some("b".to_string()),
            "what is being looked at comes first"
        );

        // With "b" polled, the others are picked up.
        app.polls
            .insert("b".into(), Cached::new(ops::Poll::default()));
        let next = next_poll_target(&app).map(|s| s.name).unwrap();
        assert!(next == "a" || next == "c", "got {next}");

        // Once everything is fresh there is nothing to do.
        app.polls
            .insert("a".into(), Cached::new(ops::Poll::default()));
        app.polls
            .insert("c".into(), Cached::new(ops::Poll::default()));
        assert!(next_poll_target(&app).is_none());

        // A stale entry becomes a candidate again.
        app.polls.insert(
            "c".into(),
            Cached {
                value: ops::Poll::default(),
                at: Instant::now() - POLL_TTL - Duration::from_secs(1),
            },
        );
        assert_eq!(
            next_poll_target(&app).map(|s| s.name),
            Some("c".to_string())
        );
    }

    #[test]
    fn polls_pick_the_never_polled_session_over_a_merely_stale_one() {
        let mut app = app_with(&["a", "b"]);
        app.list_state.select(None); // nothing selected, so no preference
        app.polls.insert(
            "a".into(),
            Cached {
                value: ops::Poll::default(),
                at: Instant::now() - POLL_TTL - Duration::from_secs(1),
            },
        );
        // "b" has never been polled, which must outrank "a" being stale.
        assert_eq!(
            next_poll_target(&app).map(|s| s.name),
            Some("b".to_string())
        );
    }

    /// The gateway says whether the sandbox is up; polling says what the agent
    /// is doing. The agent's answer is the one worth showing.
    #[test]
    fn the_agent_state_replaces_ready_in_the_column() {
        let mut app = app_with(&["a"]);
        app.sessions[0].state = State::Ready;
        assert_eq!(app.effective_state(&app.sessions[0]), State::Ready);

        app.polls
            .insert("a".into(), Cached::new(poll_with(Some(State::Waiting))));
        assert_eq!(app.effective_state(&app.sessions[0]), State::Waiting);
    }

    /// A poll that arrived before the sandbox died must not keep claiming the
    /// agent is busy, and an in-flight session must not be overwritten by a
    /// poll of the previous sandbox.
    #[test]
    fn sandbox_facts_outrank_a_poll() {
        let mut app = app_with(&["a"]);
        app.polls
            .insert("a".into(), Cached::new(poll_with(Some(State::Running))));

        for fact in [
            State::Dead,
            State::Failed,
            State::Creating,
            State::Seeding,
            State::Published,
        ] {
            app.sessions[0].state = fact;
            assert_eq!(
                app.effective_state(&app.sessions[0]),
                fact,
                "a poll must not paper over {fact}"
            );
        }
    }

    #[test]
    fn waiting_sessions_are_counted_for_the_title() {
        let mut app = app_with(&["a", "b", "c"]);
        for s in &mut app.sessions {
            s.state = State::Ready;
        }
        assert_eq!(app.waiting_count(), 0);

        app.polls
            .insert("a".into(), Cached::new(poll_with(Some(State::Waiting))));
        app.polls
            .insert("b".into(), Cached::new(poll_with(Some(State::Running))));
        app.polls
            .insert("c".into(), Cached::new(poll_with(Some(State::Waiting))));
        assert_eq!(app.waiting_count(), 2);

        // A dead sandbox is not waiting on anyone, whatever its last poll said.
        app.sessions[0].state = State::Dead;
        assert_eq!(app.waiting_count(), 1);
    }

    #[test]
    fn refresh_drops_diffs_polls_views_and_scroll_for_vanished_sessions() {
        let mut app = app_with(&["a", "b"]);
        for name in ["a", "b"] {
            app.diffs.insert(name.into(), Cached::new("d".into()));
            app.polls
                .insert(name.into(), Cached::new(ops::Poll::default()));
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
            !app.polls.contains_key("b"),
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
        app.polls
            .insert("a".into(), Cached::new(ops::Poll::default()));

        app.invalidate("a");

        assert!(app.previews.is_empty());
        assert!(app.diffs.is_empty());
        assert!(app.polls.is_empty());
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
