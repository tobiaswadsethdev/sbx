//! The terminal UI.

mod ansi;
mod attach;
mod create;
mod ui;
mod worker;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use openshell_client::{CliClient, PolicyRevision, PolicyUpdate, Provider};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::ListState;

use sbx_core::backend::Backends;

use crate::tui::attach::attach;
use create::{Create, Form, Picker};
use sbx_core::config::Config;
use sbx_core::endpoints::{self, Listed, Lists};
use sbx_core::events::Target;
use sbx_core::ops;
use sbx_core::policy;
use sbx_core::repos::{self, LocalRepo};
use sbx_core::session::{Session, State};
use sbx_core::status;
use worker::{Request, Update, Worker};

// What everything here costs, measured against a live gateway rather than
// assumed -- the intervals below were originally set against a guess that was
// out by an order of magnitude:
//
// | `sandbox list`               |  20ms |
// | an exec, doing nothing       |  44ms |
// | a full poll: stat + screen   |  56ms |
// | `git status` on a 10k file repo | 65ms |
// | `openshell logs`, 400 lines  |  14ms |
//
// So a read is tens of milliseconds, not the hundreds the first version of this
// was written around, and the interface can afford to feel live. The budget that
// still matters is the *sandbox's* CPU -- `git status` on a large repository is
// real work -- and the gateway's own log, which every exec writes to and the
// events pane has to read past.

/// How often the session list is reconciled against the gateway.
///
/// One call, whatever the number of sessions, so this is bounded by the 20ms
/// above and not by the list.
const REFRESH_EVERY: Duration = Duration::from_millis(1000);
/// How long a transient footer message stays up.
const STATUS_LINGER: Duration = Duration::from_secs(4);
/// Input poll interval. Short enough to feel immediate, long enough to idle.
const TICK: Duration = Duration::from_millis(100);
/// How long a fetched policy is trusted.
///
/// A policy changes only when someone changes it, and `w`/`t` hand the new
/// revision straight to the pane, so refetching it often would spend a
/// subprocess on an answer that is never different.
const POLICY_TTL: Duration = Duration::from_secs(30);
/// How long right-pane content -- a diff, an events feed -- is trusted before it
/// is fetched again.
///
/// The agent is editing the repository continuously, so a diff the user is
/// reading has to keep up. Only the *selected* session is refetched, so this is
/// one read per interval no matter how many sessions exist.
const PANE_TTL: Duration = Duration::from_millis(1500);
/// How long a poll -- diff stat, agent state and the agent's screen -- is
/// trusted, for a session that is *not* the one being looked at.
///
/// Every session pays for this, so it is the one interval that scales with the
/// list; the floor below keeps that from becoming a stream of execs.
const POLL_TTL: Duration = Duration::from_secs(2);
/// The same, for the selected session: what is on screen has to keep up with the
/// agent, and it is one session however many there are.
///
/// 500ms, which measures as under 600ms from a change in the sandbox to it being
/// on screen. Lower is affordable on the host -- the poll is 56ms -- but it is
/// the *sandbox's* `git status` that sets the floor: 65ms on a ten thousand file
/// repository, so twice a second is already a tenth of a core spent watching one
/// session. If sub-100ms ever matters, the answer is to split the poll rather
/// than to run all of it more often: the state and the screen are a file read and
/// a `capture-pane`, and only the stat needs git.
const POLL_SELECTED_TTL: Duration = Duration::from_millis(500);
/// Floor on the gap between polls, so a long session list cannot turn into a
/// continuous stream of execs.
///
/// Caps the rate at five a second across all sessions. At ~60ms each that is
/// about a third of the worker thread in the worst case, and the worker also has
/// diffs and policies to fetch. With N sessions a full round takes at worst N
/// times this, so the list stays under [`POLL_TTL`] up to ten of them.
const POLL_MIN_GAP: Duration = Duration::from_millis(200);

/// The intervals above, scaled by one number from the config file.
///
/// One knob rather than six, because the six are related: the selected session
/// polls faster than the rest, the floor is what keeps a long list from becoming
/// a stream of execs, and a diff has to keep up with the agent editing under it.
/// An absolute interval per constant would let those relationships be set to
/// nonsense; a ratio cannot. The [`TUNED`](Intervals::TUNED) values are the
/// measured ones, and `refresh` names the only one a person has an opinion
/// about -- how live the list feels -- with the rest following it.
///
/// [`TICK`] and [`STATUS_LINGER`] are deliberately not in here. The first is
/// keyboard responsiveness and the second is how long a human needs to read a
/// line; neither has anything to do with how hard the sandboxes are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Intervals {
    pub refresh: Duration,
    pub pane_ttl: Duration,
    pub poll_ttl: Duration,
    pub poll_selected_ttl: Duration,
    pub poll_min_gap: Duration,
    pub policy_ttl: Duration,
}

impl Intervals {
    /// What the measurements at the top of this module say.
    pub const TUNED: Intervals = Intervals {
        refresh: REFRESH_EVERY,
        pane_ttl: PANE_TTL,
        poll_ttl: POLL_TTL,
        poll_selected_ttl: POLL_SELECTED_TTL,
        poll_min_gap: POLL_MIN_GAP,
        policy_ttl: POLICY_TTL,
    };

    /// The tuned set, stretched or compressed so the list reconciles every
    /// `refresh`.
    pub fn scaled(refresh: Duration) -> Intervals {
        let factor = refresh.as_secs_f64() / REFRESH_EVERY.as_secs_f64();
        let scale = |d: Duration| d.mul_f64(factor);
        Intervals {
            refresh,
            pane_ttl: scale(PANE_TTL),
            poll_ttl: scale(POLL_TTL),
            poll_selected_ttl: scale(POLL_SELECTED_TTL),
            poll_min_gap: scale(POLL_MIN_GAP),
            policy_ttl: scale(POLICY_TTL),
        }
    }

    pub fn from_config(cfg: &Config) -> Intervals {
        cfg.refresh.map_or(Intervals::TUNED, Intervals::scaled)
    }
}

/// What the right-hand pane is showing.
///
/// The order is the Tab order, and it runs outward from the session: what it is
/// (preview), what it has done (diff), what it is allowed to do (policy), what
/// it has actually tried (events).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum RightView {
    /// The agent's screen, as the status poll last captured it. First and
    /// default, because it is the answer to the question the list raises: the
    /// state column says an agent is waiting, and this says what for.
    ///
    /// Read-only. Typing at an agent is `enter`, which hands the whole terminal
    /// over.
    #[default]
    Agent,
    Diff,
    Policy,
    Events,
}

impl RightView {
    pub const ORDER: [RightView; 4] = [
        RightView::Agent,
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

    /// What the tab in the pane's heading says.
    pub fn label(self) -> &'static str {
        match self {
            RightView::Agent => "agent",
            RightView::Diff => "diff",
            RightView::Policy => "policy",
            RightView::Events => "events",
        }
    }

    /// How long fetched content stays fresh.
    ///
    /// Not one constant, because the panes want different things. A diff under
    /// the user's eyes has to keep up with the agent editing underneath it; a
    /// policy only changes when someone changes it, and refetching it every few
    /// seconds would spend a subprocess on an answer that is never different.
    /// The events feed is the fastest, because it is a feed.
    fn ttl(self, iv: &Intervals) -> Duration {
        match self {
            RightView::Diff => iv.pane_ttl,
            RightView::Policy => iv.policy_ttl,
            // A gateway call rather than an exec, so it contends with nothing;
            // see `sbx_core::events`.
            RightView::Events => iv.pane_ttl,
            // Drawn from the poll, which has its own schedule; see
            // [`next_poll_target`]. The value is never read.
            RightView::Agent => Duration::MAX,
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
    diffs: HashMap<String, Cached<String>>,
    /// Diff stat and agent state per session, from one exec each.
    polls: HashMap<String, Cached<ops::Poll>>,
    /// The effective policy, and the reason if it could not be read. Both are
    /// worth caching: an unreachable gateway should not blank the pane on every
    /// tick, it should keep saying why.
    policies: HashMap<String, Cached<Result<PolicyRevision, String>>>,
    events: HashMap<String, Cached<Result<Vec<sbx_core::events::Event>, String>>>,
    /// Which event the feed's cursor is on, per session. An index rather than a
    /// key because that is what the renderer needs; kept pointing at the same
    /// *event* across a refetch by [`App::on_update`], since the feed grows at
    /// the top.
    event_cursor: HashMap<String, usize>,
    /// Sessions whose content is currently being fetched, so the same request
    /// is not queued repeatedly while the worker is busy. One per kind, so a
    /// slow diff does not stall the stat column.
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
    /// Whether the one repairing refresh has been asked for yet.
    repaired: bool,
    last_refresh: Instant,
    should_quit: bool,
    /// Defaults from the config file: what the create form starts with, and how
    /// often the sandboxes are read.
    cfg: Config,
    intervals: Intervals,
    /// Set by the key handler; acted on by the event loop, which is the only
    /// place with access to the terminal.
    attach_request: Option<Session>,
    /// Set by the key handler; sent by the event loop, which owns the worker.
    repolicy_request: Option<(Session, Box<PolicyUpdate>, String)>,
    /// An action waiting on a y/n answer, and the question to show.
    ///
    /// Publishing pushes a branch and opens a pull request -- it is visible to
    /// other people and not undone by pressing something else. A single
    /// keystroke is the wrong interface for that, so it asks first. Widening a
    /// policy does not ask: it is reversible with `t`, and its effect is
    /// confined to the sandbox.
    confirm: Option<(String, Confirm)>,
    /// A decision about one endpoint, waiting on a keystroke.
    choice: Option<Choice>,
    /// The global allow and block lists, as they are on disk. Held rather than
    /// re-read, because the policy pane draws them on every frame and they only
    /// change when this process changes them -- see [`App::decide`], which
    /// writes the file and this copy together.
    lists: Lists,
    /// Where those lists live. A field rather than a call to
    /// [`Lists::default_path`] at the point of writing, so a test that exercises
    /// a global decision writes to a temporary file instead of to the
    /// developer's own configuration -- which is what the first version of that
    /// test did.
    lists_path: PathBuf,
    publish_request: Option<Session>,
    publishing: Option<String>,
    /// Set by the key handler; sent by the event loop, which owns the worker.
    destroy_request: Option<String>,
    /// The session a destroy is running for. One at a time, like a publish: the
    /// gateway call takes a moment and the row has to keep saying why it is
    /// still there.
    destroying: Option<String>,
    /// The create flow, while it is open. It owns the keyboard, like a pending
    /// question does.
    create: Option<Create>,
    /// The picker as it was when a repository was chosen, so escaping the form
    /// comes back to the same query rather than to a blank filter.
    stashed_picker: Option<Picker>,
    /// Repositories found on the host, kept for the TUI's lifetime so reopening
    /// the picker is instant. Refreshed in the background each time it opens, so
    /// a checkout made since the last one still shows up.
    repos: Option<Vec<LocalRepo>>,
    scan_in_flight: bool,
    /// The gateway's providers, or why they could not be read. Fetched once:
    /// providers are created by hand and do not change under a running TUI.
    providers: Option<Result<Vec<Provider>, String>>,
    providers_in_flight: bool,
    /// A session being created, or just created, that the store may not know
    /// about yet. Merged into the list so the row appears the moment the create
    /// starts rather than after the next refresh.
    pending: Option<Session>,
    /// Set by the key handler; sent by the event loop, which owns the worker.
    scan_request: bool,
    providers_request: bool,
    inspect_request: Option<(PathBuf, Option<String>)>,
    create_request: Option<Box<ops::Draft>>,
}

/// A decision about one endpoint, waiting on a keystroke.
///
/// Not a [`Confirm`]. That is a yes-or-no about an action already chosen; this
/// is the choice itself, and it has four answers -- allow or block, here or
/// everywhere. Asking it as two questions in a row would be worse than asking it
/// as one, because the first answer would have no visible consequence.
#[derive(Debug, Clone)]
struct Choice {
    session: Session,
    target: Target,
    /// Whether the endpoint is in this sandbox's policy at all, which is what a
    /// block acts on. `None` when the policy has not been read, which is what
    /// keeps "it was not reachable anyway" an observation rather than a guess.
    present: Option<bool>,
    /// Whether it is reachable *by the binary this event named*, which is what
    /// an allow acts on. Not the same question: `github.com:443` is reachable by
    /// git under `feature-work` and denied to curl, and the whole reason this
    /// tool exists is that those are different facts.
    reachable: Option<bool>,
    /// Which global list already names it, if either.
    listed: Option<Listed>,
}

/// An action held pending confirmation.
#[derive(Debug, Clone)]
enum Confirm {
    Publish(Box<Session>),
    /// Destroying a session. Carries the name only: the sandbox is resolved
    /// again when the destroy runs, so a record the cache has lost is still
    /// removable.
    Destroy(String),
    /// Quitting while a create is running. The create thread dies with the
    /// process, so this is worth asking about; see `worker::spawn_create`.
    Quit,
}

impl App {
    fn new(cfg: Config) -> Self {
        let intervals = Intervals::from_config(&cfg);
        // Read once, here, rather than on every frame that draws them. A file
        // that will not parse is reported and treated as empty: the lists are a
        // convenience, and refusing to open the TUI over them would be refusing
        // to show the sessions too.
        let (lists, unreadable) = match Lists::load() {
            Ok(l) => (l, None),
            Err(e) => (
                Lists::default(),
                Some(format!(
                    "could not read {}: {e}",
                    Lists::default_path().display()
                )),
            ),
        };
        let mut app = App {
            sessions: Vec::new(),
            list_state: ListState::default(),
            diffs: HashMap::new(),
            polls: HashMap::new(),
            policies: HashMap::new(),
            events: HashMap::new(),
            event_cursor: HashMap::new(),
            diff_in_flight: None,
            poll_in_flight: None,
            policy_in_flight: None,
            events_in_flight: None,
            repolicy_in_flight: None,
            // Force an immediate first poll.
            last_poll_request: Instant::now() - intervals.poll_min_gap,
            views: HashMap::new(),
            scroll: HashMap::new(),
            focus: Focus::default(),
            right_lines: 0,
            right_height: 0,
            status: None,
            status_is_error: false,
            status_set_at: Instant::now(),
            refreshing: false,
            repaired: false,
            // Force an immediate first refresh.
            last_refresh: Instant::now() - intervals.refresh,
            cfg,
            intervals,
            should_quit: false,
            attach_request: None,
            repolicy_request: None,
            confirm: None,
            choice: None,
            lists,
            lists_path: Lists::default_path(),
            publish_request: None,
            publishing: None,
            destroy_request: None,
            destroying: None,
            create: None,
            stashed_picker: None,
            repos: None,
            scan_in_flight: false,
            providers: None,
            providers_in_flight: false,
            pending: None,
            scan_request: false,
            providers_request: false,
            inspect_request: None,
            create_request: None,
        };
        if let Some(why) = unreadable {
            app.fail(why);
        }
        app
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

    /// Move within the right pane: the cursor in the events feed, the scroll
    /// offset in every other view.
    ///
    /// One rule rather than a second set of keys. The feed is the one pane whose
    /// rows are *acted on* rather than only read, so what `j` moves there is the
    /// selection; the renderer scrolls to keep it in sight, which makes the two
    /// indistinguishable until the feed is longer than the pane.
    ///
    /// `isize` rather than `i16` so callers can pass a saturating "to the top"
    /// or "to the bottom" without knowing the content height.
    fn scroll_by(&mut self, delta: isize) {
        if self.right_view() == RightView::Events {
            self.move_event_cursor(delta);
            return;
        }
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

    /// Whether the gateway can be asked about a session yet.
    ///
    /// False only for the row standing in for a create that has not got as far
    /// as a sandbox: every exec against it would fail for the seconds that
    /// takes, at the cost of a subprocess and a blanked pane each. Once the
    /// sandbox exists the row is polled like any other, half-cloned repository
    /// and all -- the poll script is written to tolerate that.
    ///
    /// Deliberately not keyed off `State::Creating` in general. A cached session
    /// left in that state by a crashed create *does* have a sandbox, usually,
    /// and refusing to poll it would leave it saying `creating` for ever.
    fn is_live(&self, session: &Session) -> bool {
        !self
            .pending
            .as_ref()
            .is_some_and(|p| p.name == session.name && p.state == State::Creating)
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
    pub fn events(&self, name: &str) -> Option<&Result<Vec<sbx_core::events::Event>, String>> {
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

    /// The global allow and block lists, for the pane that draws them.
    pub fn lists(&self) -> &Lists {
        &self.lists
    }

    /// Which row of the feed the cursor is on.
    ///
    /// Clamped on read rather than on write, because the feed can shrink
    /// underneath a stored index -- a session destroyed and recreated under the
    /// same name starts its history again -- and an index past the end would
    /// draw a highlight on nothing.
    pub fn event_cursor(&self, name: &str) -> usize {
        let stored = self.event_cursor.get(name).copied().unwrap_or(0);
        match self.feed(name) {
            Some(events) if !events.is_empty() => stored.min(events.len() - 1),
            _ => 0,
        }
    }

    /// A session's feed, if it has come back and was readable.
    fn feed(&self, name: &str) -> Option<&[sbx_core::events::Event]> {
        match self.events(name)? {
            Ok(events) => Some(events),
            Err(_) => None,
        }
    }

    /// The event the cursor is on.
    fn selected_event(&self, name: &str) -> Option<&sbx_core::events::Event> {
        self.feed(name)?.get(self.event_cursor(name))
    }

    fn move_event_cursor(&mut self, delta: isize) {
        let Some(name) = self.selected_name() else {
            return;
        };
        let Some(len) = self.feed(&name).map(<[_]>::len).filter(|n| *n > 0) else {
            return;
        };
        let at = (self.event_cursor(&name) as isize + delta).clamp(0, len as isize - 1) as usize;
        self.event_cursor.insert(name, at);
    }

    /// Whether a sandbox's policy names an endpoint at all.
    ///
    /// What a block acts on: `--remove-endpoint` takes it away from every binary
    /// at once, because `host:port` is the only granularity it has.
    fn endpoint_present(&self, name: &str, endpoint: &str) -> Option<bool> {
        match self.policy(name)? {
            Ok(rev) => rev.policy.as_ref().map(|p| {
                p.network_policies
                    .values()
                    .any(|r| r.endpoints.iter().any(|e| e.host_port() == endpoint))
            }),
            Err(_) => None,
        }
    }

    /// Whether an endpoint is reachable by the binary an event named.
    ///
    /// What an allow acts on, and a different question from
    /// [`App::endpoint_present`]: a rule grants its endpoints to *its* binaries,
    /// so `github.com:443` being in the policy says nothing about whether curl
    /// may reach it. Reporting the endpoint as already allowed on the strength
    /// of git's rule is exactly the bug this pane exists to make visible.
    fn reachable_by(&self, name: &str, target: &Target) -> Option<bool> {
        match self.policy(name)? {
            Ok(rev) => rev.policy.as_ref().map(|p| {
                p.network_policies.values().any(|r| {
                    r.endpoints.iter().any(|e| e.host_port() == target.endpoint)
                        && match &target.binary {
                            Some(b) => r.binaries.iter().any(|rb| rb.path == *b),
                            // Nothing to check against; the endpoint being there
                            // is all that can be said.
                            None => true,
                        }
                })
            }),
            Err(_) => None,
        }
    }

    /// What the endpoint chooser is asking, at two widths.
    ///
    /// `(long, short)`. The footer has one line and three things competing for
    /// it -- the endpoint, the binary and what the four keys mean -- so it needs
    /// to know what may be dropped rather than being handed a string it can only
    /// clip. The binary goes first: it is already on screen in the row the
    /// cursor is on, and the key descriptions are on screen nowhere else.
    pub fn pending_choice(&self) -> Option<(String, String)> {
        let c = self.choice.as_ref()?;
        let short = c.target.endpoint.clone();

        let mut long = short.clone();
        if let Some(b) = &c.target.binary {
            long.push_str(&format!(" for {b}"));
        }
        // What is already true, so the four keys are a choice between outcomes
        // rather than a guess. Silent when the policy has not come back: saying
        // nothing is better than saying something unfounded about egress.
        match (c.reachable, c.present) {
            (Some(true), _) => long.push_str("  -- reachable now"),
            (Some(false), Some(true)) => long.push_str("  -- endpoint present, this binary denied"),
            (Some(false), _) => long.push_str("  -- denied now"),
            (None, _) => {}
        }
        if let Some(l) = c.listed {
            long.push_str(&format!("  -- {}", l.label()));
        }
        Some((long, short))
    }

    /// Offer the four decisions about the event under the cursor.
    fn open_choice(&mut self) {
        let Some(session) = self.selected().cloned() else {
            return;
        };
        if self.repolicy_in_flight.is_some() {
            self.fail("a policy change is already in flight");
            return;
        }
        let Some(event) = self.selected_event(&session.name) else {
            self.fail("nothing to act on: the feed is empty");
            return;
        };
        // A `CONFIG:VALIDATED` warning is a sentence, not a decision about an
        // endpoint, and there is one keystroke between a wrong answer here and a
        // rule at the gateway. See `Event::target`.
        let Some(target) = event.target() else {
            self.fail("that event is not about an endpoint");
            return;
        };
        self.choice = Some(Choice {
            present: self.endpoint_present(&session.name, &target.endpoint),
            reachable: self.reachable_by(&session.name, &target),
            listed: self.lists.verdict(&target.endpoint),
            target,
            session,
        });
    }

    fn on_choice_key(&mut self, key: KeyEvent) {
        let Some(choice) = self.choice.take() else {
            return;
        };
        // Lowercase is this session, uppercase is every session -- the same
        // shape as `P` and `D`, where the capital is the one that reaches
        // further than the keystroke.
        let (allow, global) = match key.code {
            KeyCode::Char('a') => (true, false),
            KeyCode::Char('b') => (false, false),
            KeyCode::Char('A') => (true, true),
            KeyCode::Char('B') => (false, true),
            _ => {
                self.note("cancelled");
                return;
            }
        };
        self.repolicy_request = self.decide(choice, allow, global);
    }

    /// Act on a decision: write the global list if it was a global one, and
    /// return the live change to make, if there is one to make.
    fn decide(
        &mut self,
        choice: Choice,
        allow: bool,
        global: bool,
    ) -> Option<(Session, Box<PolicyUpdate>, String)> {
        let Choice {
            session,
            target,
            present,
            reachable,
            ..
        } = choice;
        let endpoint = target.endpoint;

        // An endpoint rule with no binaries grants nothing, so an allow needs
        // one. An L7 decision names none -- and does not need to: the proxy
        // could only inspect that request because the endpoint was already
        // reachable, so what it denied was the *path*, which is not something
        // this key changes. Saying that is better than issuing a rule that
        // quietly does nothing.
        let binaries: Vec<String> = match (&target.binary, allow) {
            (Some(b), true) => vec![b.clone()],
            (None, true) => {
                self.fail(format!(
                    "{endpoint} was decided by a rule that names no binary, so there is nothing to bind an allow to"
                ));
                return None;
            }
            (_, false) => Vec::new(),
        };

        // The global list is written first, and a failure to write it stops
        // everything. "Always" that silently turned out to be "here, once" is
        // the worse outcome: only the refusal is visible.
        if global {
            let (ep, bins) = (endpoint.clone(), binaries.clone());
            let written = endpoints::update_at(
                &self.lists_path,
                self.lists_path.with_extension("lock"),
                move |l| {
                    if allow {
                        l.allow(&ep, bins);
                    } else {
                        l.block(&ep);
                    }
                },
            );
            if let Err(e) = written {
                self.fail(format!("could not write the global endpoint lists: {e}"));
                return None;
            }
            // The copy the pane draws, kept in step with the file rather than
            // re-read from it.
            if allow {
                self.lists.allow(&endpoint, binaries.clone());
            } else {
                self.lists.block(&endpoint);
            }
        }

        let also = if global {
            match allow {
                true => "; on the global allow list from now on",
                false => "; on the global block list from now on",
            }
        } else {
            ""
        };

        // Two round trips that would change nothing, named rather than made.
        // Both come *after* the write, because "already true here" is no reason
        // not to record it for every session after this one.
        if allow && reachable == Some(true) {
            self.note(format!(
                "{endpoint} is already reachable from {}{also}",
                session.name
            ));
            return None;
        }
        if !allow && present == Some(false) {
            self.note(format!(
                "{endpoint} was not in {}'s policy anyway{also}",
                session.name
            ));
            return None;
        }

        let (update, label) = if allow {
            (
                endpoints::allow_update(&endpoint, &binaries),
                format!("allowed: {endpoint} for {}{also}", binaries.join(", ")),
            )
        } else {
            (
                endpoints::block_update(&endpoint),
                format!("blocked: {endpoint}{also}"),
            )
        };

        self.repolicy_in_flight = Some(session.name.clone());
        // Switching to the policy pane makes the consequence visible, which is
        // the only reason a change to egress is safe to bind to one key -- the
        // same bargain `w` and `t` make.
        self.views.insert(session.name.clone(), RightView::Policy);
        self.note(format!(
            "{} ... the gateway takes a few seconds to load a revision",
            if allow { "allowing" } else { "blocking" }
        ));
        Some((session, Box::new(update), label))
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
        self.diffs.remove(name);
        self.polls.remove(name);
        self.policies.remove(name);
        self.events.remove(name);
        self.event_cursor.remove(name);
    }

    /// Drop a session from the list, along with everything cached for it.
    ///
    /// The refresh does this too, from the store, but only once the gateway
    /// agrees the sandbox has gone. Doing it locally is what makes the row
    /// disappear on the keystroke rather than several seconds later.
    fn forget(&mut self, name: &str) {
        self.invalidate(name);
        self.views.remove(name);
        self.scroll.remove(name);
        self.sessions.retain(|s| s.name != name);
        if self.pending.as_ref().is_some_and(|p| p.name == name) {
            self.pending = None;
        }
        // Clamp rather than clear: the cursor should land on the neighbour of
        // the row that just went, not jump back to the top of the list.
        let index = match self.sessions.len() {
            0 => None,
            len => Some(self.list_state.selected().unwrap_or(0).min(len - 1)),
        };
        self.list_state.select(index);
    }

    /// Whether a question is on screen, and what it says.
    pub fn pending_question(&self) -> Option<&str> {
        self.confirm.as_ref().map(|(q, _)| q.as_str())
    }

    /// The session a publish is running for, so the pane can say so.
    pub fn publishing(&self) -> Option<&str> {
        self.publishing.as_deref()
    }

    /// Ask before publishing. Returns the question to show.
    fn ask_publish(&mut self) {
        let Some(session) = self.selected().cloned() else {
            return;
        };
        if self.publishing.is_some() {
            self.fail("a publish is already running");
            return;
        }
        // Parsed here rather than at confirm time so an unpublishable remote is
        // refused before the user is asked a pointless question.
        let target = match sbx_core::forge::Remote::parse(&session.repo) {
            Ok(r) => r.slug(),
            Err(e) => {
                self.fail(format!("cannot publish: {e}"));
                return;
            }
        };
        self.confirm = Some((
            format!(
                "push {} to {} and open a pull request?  y/n",
                session.work_branch, target
            ),
            Confirm::Publish(Box::new(session)),
        ));
    }

    /// The session a destroy is running for, so the row can say so.
    pub fn destroying(&self) -> Option<&str> {
        self.destroying.as_deref()
    }

    /// Ask before destroying the selected session.
    ///
    /// Always asks, and says what is at stake: a sandbox holds the only copy of
    /// whatever the agent has not published, so this is the one key in the TUI
    /// that can throw away work that exists nowhere else.
    fn ask_destroy(&mut self) {
        let Some(session) = self.selected().cloned() else {
            return;
        };
        if self.destroying.is_some() {
            self.fail("a destroy is already running");
            return;
        }
        // A create still running would write its record back after the destroy
        // removed it, leaving a record for a sandbox that no longer exists --
        // and the create's own clone would keep going against a dead sandbox.
        // Waiting is a few tens of seconds; the alternative is a mess.
        if self
            .pending
            .as_ref()
            .is_some_and(|p| p.name == session.name)
        {
            self.fail(format!("{} is still being created", session.name));
            return;
        }
        self.confirm = Some((
            format!(
                "destroy {}?  {}  y/n",
                session.name,
                self.at_stake(&session)
            ),
            Confirm::Destroy(session.name),
        ));
    }

    /// What destroying a session would lose, for the question.
    ///
    /// Read from the last poll rather than fetched: the question has to go up on
    /// the keystroke, and the stat is already on screen in the list column the
    /// cursor is sitting on. An unpolled session says the honest thing rather
    /// than claiming there is nothing to lose.
    fn at_stake(&self, session: &Session) -> String {
        let published = session.state == State::Published;
        match self.poll(&session.name).and_then(|p| p.stat) {
            Some(stat) if stat.is_empty() && published => "published, nothing uncommitted".into(),
            Some(stat) if stat.is_empty() => "no changes to lose".into(),
            Some(stat) => {
                let untracked = if stat.untracked > 0 { " ?" } else { "" };
                let published = if published { ", published" } else { "" };
                format!(
                    "+{}/-{}{untracked} goes with the sandbox{published}",
                    stat.added, stat.removed
                )
            }
            None => "the sandbox and everything in it goes".into(),
        }
    }

    /// Resolve a pending question. Anything but `y` cancels, so a stray key
    /// cannot publish.
    fn answer(&mut self, yes: bool) {
        let Some((_, action)) = self.confirm.take() else {
            return;
        };
        if !yes {
            self.note("cancelled");
            return;
        }
        match action {
            Confirm::Publish(session) => {
                self.publishing = Some(session.name.clone());
                self.note(format!("publishing {} ...", session.work_branch));
                self.publish_request = Some(*session);
            }
            Confirm::Destroy(name) => {
                self.destroying = Some(name.clone());
                self.note(format!("destroying {name} ..."));
                self.destroy_request = Some(name);
            }
            Confirm::Quit => self.should_quit = true,
        }
    }

    /// The create flow, for the renderer.
    pub fn create_flow(&self) -> Option<&Create> {
        self.create.as_ref()
    }

    /// Open the create flow on the repository picker.
    ///
    /// The cached scan is shown immediately and a fresh one requested behind it,
    /// so the picker is never empty on a second opening and never stale on a
    /// long-running TUI.
    fn open_create(&mut self) {
        if self.pending.is_some() {
            self.fail("a session is already being created");
            return;
        }
        let mut picker = Picker::preferring(self.cfg.repo.clone());
        if let Some(repos) = &self.repos {
            picker.scanned(repos.clone());
        }
        self.create = Some(Create::Pick(picker));
        self.stashed_picker = None;
        self.scan_request = true;
        // The form needs these, and asking now means they are usually there by
        // the time a repository has been chosen. Asked again after a failure,
        // so a gateway hiccup does not leave the form unable to offer
        // credentials for the rest of the session.
        self.providers_request = !matches!(self.providers, Some(Ok(_)));
    }

    /// The config file's answers, in the shape the create form wants them.
    fn defaults(&self) -> create::Defaults {
        create::Defaults {
            base: self.cfg.base.clone(),
            policy: self.cfg.policy.clone(),
            providers: self.cfg.providers.clone(),
            mcp: self.cfg.mcp_servers(),
            skills: self.cfg.skills().to_vec(),
        }
    }

    /// What the existing sessions have to say about a new one in this repository.
    ///
    /// The names in use, so a derived one can step around them, and the
    /// credentials the most recent session for the same *host and organisation*
    /// was given. Host and organisation rather than the exact URL, because an
    /// Azure PAT is scoped to an organisation and covers every repository in it --
    /// which is what makes the answer useful for a repository never opened
    /// before.
    fn history_for(&self, repo: &LocalRepo) -> create::History {
        let taken = self.sessions.iter().map(|s| s.name.clone()).collect();

        let key = |url: &str| {
            sbx_core::forge::Remote::parse(url)
                .ok()
                .map(|r| (r.host, r.org))
        };
        let wanted = repo.origin.as_deref().and_then(key);

        // Newest first: the last thing that worked is the best evidence.
        let providers = wanted
            .and_then(|w| {
                self.sessions
                    .iter()
                    .filter(|s| key(&s.repo).is_some_and(|k| k == w))
                    .filter(|s| !s.providers.is_empty())
                    .max_by_key(|s| s.created_at)
                    .map(|s| s.providers.clone())
            })
            .unwrap_or_default();

        create::History { taken, providers }
    }

    /// Route a key to the create flow and act on what it decided.
    fn on_create_key(&mut self, key: KeyEvent) {
        let Some(flow) = self.create.as_mut() else {
            return;
        };
        let action = match flow {
            Create::Pick(picker) => picker.on_key(key),
            Create::Fill(form) => form.on_key(key),
        };
        match action {
            create::Action::None => {}
            create::Action::Cancel => {
                self.create = None;
                self.stashed_picker = None;
            }
            create::Action::Picked(repo) => {
                if let Some(Create::Pick(picker)) = self.create.take() {
                    self.stashed_picker = Some(picker);
                }
                self.inspect_request = Some((repo.path.clone(), repo.branch.clone()));
                let history = self.history_for(&repo);
                self.create = Some(Create::Fill(Box::new(Form::new(
                    *repo,
                    self.providers.as_ref(),
                    history,
                    &self.defaults(),
                ))));
            }
            create::Action::Back => {
                // The stashed picker keeps the query; the cache keeps it
                // current, in case a scan landed while the form was up.
                let mut picker = self.stashed_picker.take().unwrap_or_else(Picker::new);
                if let Some(repos) = &self.repos {
                    picker.scanned(repos.clone());
                }
                self.create = Some(Create::Pick(picker));
            }
            create::Action::Submit(draft) => self.submit_create(*draft),
        }
    }

    /// Accept a filled-in form, or push the reason back into it.
    ///
    /// The duplicate-name check lives here rather than in the form because only
    /// the app knows what sessions exist -- and it has to be made again inside
    /// [`ops::create`] anyway, against the store, which is the authority.
    fn submit_create(&mut self, draft: ops::Draft) {
        let taken = self.sessions.iter().any(|s| s.name == draft.name)
            || self.pending.as_ref().is_some_and(|s| s.name == draft.name);
        if taken {
            if let Some(Create::Fill(form)) = self.create.as_mut() {
                form.set_error(format!("session `{}` already exists", draft.name));
            }
            return;
        }

        // A row for the session before it exists, so the list accounts for it
        // while the gateway works. Replaced by the real record on the first
        // refresh that sees it.
        let mut row = Session::new(draft.name.clone(), draft.repo.clone(), draft.task.clone());
        row.base_branch = draft.base.clone();
        row.policy = Some(draft.policy.clone());
        row.providers = draft.providers.clone();
        row.state = State::Creating;

        self.note(format!("creating {} ...", draft.name));
        self.set_pending(row);
        self.select(&draft.name);
        self.create = None;
        self.stashed_picker = None;
        self.create_request = Some(Box::new(draft));
    }

    /// Record the session being created, in the list as well as in `pending`.
    ///
    /// Both, so a stage change shows up immediately rather than at the next
    /// refresh: the list is rebuilt from the store every few seconds, and until
    /// then it holds this copy.
    fn set_pending(&mut self, session: Session) {
        match self.sessions.iter_mut().find(|s| s.name == session.name) {
            Some(row) => *row = session.clone(),
            None => {
                self.sessions.push(session.clone());
                self.sessions.sort_by(|a, b| a.name.cmp(&b.name));
            }
        }
        self.pending = Some(session);
    }

    /// Put the cursor on a session by name, if the list has it.
    fn select(&mut self, name: &str) {
        if let Some(i) = self.sessions.iter().position(|s| s.name == name) {
            self.list_state.select(Some(i));
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        // A pending question owns the keyboard: nothing else may act while it
        // is up, or the answer could be consumed as a movement key.
        if self.confirm.is_some() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => self.answer(true),
                _ => self.answer(false),
            }
            return;
        }
        // Then the endpoint chooser, for the same reason: `a` is attach
        // everywhere else in the TUI, and it must not both answer this and open
        // a terminal.
        if self.choice.is_some() {
            self.on_choice_key(key);
            return;
        }
        // Then the create flow, for the same reason: a character typed into the
        // task must not also move the session list.
        if self.create.is_some() {
            self.on_create_key(key);
            return;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Char('q'), _) => {
                // Quitting kills the create thread with the process, which can
                // leave a sandbox half-seeded, so it asks first.
                match self.pending.as_ref().map(|s| s.name.clone()) {
                    Some(name) => {
                        self.confirm = Some((
                            format!("{name} is still being created; quit anyway?  y/n"),
                            Confirm::Quit,
                        ));
                    }
                    None => self.should_quit = true,
                }
            }
            // New session: the picker, then the form. Lowercase, unlike publish,
            // because nothing has left the machine until the form is submitted.
            (KeyCode::Char('n'), _) => self.open_create(),
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
            // Act on the endpoint the feed's cursor is on. Bound only while the
            // events pane is showing, for the reason `w` and `t` are bound only
            // in the policy pane: the thing being decided about is on screen
            // when the key is pressed.
            //
            // Not `enter`, which hands the whole terminal to the agent from
            // every pane and is worth keeping unambiguous.
            (KeyCode::Char('e'), _) if self.right_view() == RightView::Events => {
                self.open_choice();
            }
            // Entering a session hands the whole terminal over to its agent,
            // full width and with no key routing in between -- the pane shows
            // what the agent is doing; this is for doing something about it.
            (KeyCode::Enter, _) | (KeyCode::Char('a'), _) => {
                self.attach_request = self.selected().cloned();
            }
            // Shift-P, not p: publishing is outward-facing, so it should not
            // share a neighbourhood with the movement keys.
            (KeyCode::Char('P'), _) => self.ask_publish(),
            // Shift-D, for the same reason as Shift-P, and more so: this is the
            // one key that can destroy work no other copy of exists.
            (KeyCode::Char('D'), _) => self.ask_destroy(),
            // The numbers on the rows, made good: `3` goes to the third session.
            // Placed before the focus-dependent movement keys, because a jump is
            // not a movement -- it means the same thing from either pane.
            (KeyCode::Char(c), _) if c.is_ascii_digit() && c != '0' => {
                let index = c as usize - '1' as usize;
                if index < self.sessions.len() {
                    self.list_state.select(Some(index));
                }
            }
            (KeyCode::Char('r'), _) => {
                // Make the next tick refresh immediately.
                self.last_refresh = Instant::now() - self.intervals.refresh;
                self.diffs.clear();
                self.polls.clear();
                self.note("refreshing");
            }
            // Scrolling the right-hand pane, from either side. The focus-based
            // keys below need `l` first, and a diff you cannot scroll without
            // knowing that is a diff that looks broken -- which is exactly how it
            // was reported. Paging goes here too: a list of a handful of sessions
            // has nothing to page through, and the content pane always does.
            (KeyCode::Down, KeyModifiers::SHIFT) => self.scroll_by(1),
            (KeyCode::Up, KeyModifiers::SHIFT) => self.scroll_by(-1),
            (KeyCode::PageDown, _) => self.scroll_by(self.page()),
            (KeyCode::PageUp, _) => self.scroll_by(-self.page()),
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
            (KeyCode::Char('g'), _) | (KeyCode::Home, _) => self.move_by(isize::MIN / 2),
            (KeyCode::Char('G'), _) | (KeyCode::End, _) => self.move_by(isize::MAX / 2),
            _ => {}
        }
    }

    /// The agent's screen as last captured, for the pane that shows it.
    pub fn agent_screen(&self, session: &Session) -> Option<&str> {
        self.poll(&session.name).and_then(|p| p.pane.as_deref())
    }

    fn on_update(&mut self, update: Update) {
        match update {
            Update::Sessions(r) => {
                self.refreshing = false;
                self.apply_refresh(*r);
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
                // The feed grows at the top, so a row index is not a handle on
                // an event: a couple of arrivals between two keystrokes and the
                // cursor is on something else, which is intolerable for a pane
                // whose keys act on whatever it is pointing at. Re-anchored by
                // identity -- the same notion of sameness the kept file dedupes
                // on, so the cursor cannot follow an event the merge discarded.
                if let Some(was) = self
                    .selected_event(&session)
                    .map(sbx_core::events::Event::key)
                    && let Ok(now) = &*result
                {
                    let at = now.iter().position(|e| e.key() == was).unwrap_or(0);
                    self.event_cursor.insert(session.clone(), at);
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
            Update::Published { session, result } => {
                if self.publishing.as_deref() == Some(session.as_str()) {
                    self.publishing = None;
                }
                match &*result {
                    Ok(o) => {
                        let mut msg = match &o.pull_request {
                            Some(url) => format!("published: {url}"),
                            None => "pushed (no pull request)".to_string(),
                        };
                        if let Some(w) = o.warnings.first() {
                            msg.push_str(&format!("  -- {w}"));
                        }
                        self.note(msg);
                        // The state comes back on the next refresh, which reads
                        // it from the store the worker just wrote.
                        self.last_refresh = Instant::now() - self.intervals.refresh;
                    }
                    Err(e) => self.fail(e.clone()),
                }
            }
            Update::Destroyed { session, result } => {
                if self.destroying.as_deref() == Some(session.as_str()) {
                    self.destroying = None;
                }
                match *result {
                    Ok(outcome) => {
                        self.note(match outcome {
                            ops::Destroyed::Sandbox => format!("destroyed {session}"),
                            ops::Destroyed::RecordOnly => {
                                format!("{session} was already gone; forgot it")
                            }
                        });
                        // Dropped here rather than waiting for the refresh: the
                        // gateway lists a deleted sandbox as `Deleting` for a
                        // while, so a refresh landing in that window would put
                        // the row back as `dead` and make it look as though the
                        // destroy had half worked.
                        self.forget(&session);
                    }
                    Err(e) => self.fail(e),
                }
                self.last_refresh = Instant::now() - self.intervals.refresh;
            }
            Update::Repos(repos) => {
                self.scan_in_flight = false;
                // Into the open picker as well as the cache: a scan that lands
                // while the picker is up should fill it in, not wait for the
                // next opening.
                if let Some(Create::Pick(picker)) = self.create.as_mut() {
                    picker.scanned(repos.clone());
                }
                self.repos = Some(repos);
            }
            Update::Inspected { path, facts } => {
                // Only if the form is still about that repository: going back
                // and picking another one must not be told about the first.
                if let Some(Create::Fill(form)) = self.create.as_mut()
                    && form.repo.path == path
                {
                    form.inspected(*facts);
                }
            }
            Update::Providers(result) => {
                self.providers_in_flight = false;
                if let Some(Create::Fill(form)) = self.create.as_mut() {
                    form.providers_arrived(&result);
                }
                self.providers = Some(*result);
            }
            Update::Creating { session, step } => {
                if let Some(mut row) = self.pending.clone().filter(|p| p.name == session) {
                    row.state = step.state();
                    self.set_pending(row);
                }
                // The terminal only ever creates sandboxed sessions; the
                // worktree backend is the window's, which is where the choice
                // is offered. See `docs/desktop.md`.
                self.note(format!(
                    "{session}: {}",
                    step.label(sbx_core::session::Kind::Sandbox)
                ));
            }
            Update::Created { session, result } => {
                match *result {
                    Ok(created) => {
                        let mut msg = format!("created {session}");
                        if let Some(w) = created.warnings.first() {
                            msg.push_str(&format!("  -- {w}"));
                        }
                        self.note(msg);
                        // Kept as the pending row until a refresh reads it back
                        // from the store, so the row does not blink out of the
                        // list in between.
                        self.set_pending(created.session);
                    }
                    Err(e) => {
                        self.fail(format!("could not create {session}: {e}"));
                        // Dropped rather than left showing: whether a record
                        // survived is up to how far the create got, and the
                        // refresh below is what knows.
                        self.pending = None;
                    }
                }
                self.last_refresh = Instant::now() - self.intervals.refresh;
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

        // A create in flight is not in the store yet -- and a create that has
        // just finished may not be either, since the worker's refresh could have
        // read the file before the create thread wrote it. Either way the row
        // belongs in the list; once the store has it, the store's copy wins.
        match &self.pending {
            Some(row) if !self.sessions.iter().any(|s| s.name == row.name) => {
                self.sessions.push(row.clone());
                self.sessions.sort_by(|a, b| a.name.cmp(&b.name));
            }
            Some(_) => self.pending = None,
            None => {}
        }

        let index = previously
            .and_then(|name| self.sessions.iter().position(|s| s.name == name))
            .or_else(|| (!self.sessions.is_empty()).then_some(0));
        self.list_state.select(index);

        // Drop everything keyed by a session that no longer exists, or the maps
        // grow without bound over a long-running TUI.
        let live: Vec<String> = self.sessions.iter().map(|s| s.name.clone()).collect();
        self.diffs.retain(|name, _| live.contains(name));
        self.polls.retain(|name, _| live.contains(name));
        self.policies.retain(|name, _| live.contains(name));
        self.events.retain(|name, _| live.contains(name));
        self.event_cursor.retain(|name, _| live.contains(name));
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

pub fn run(client: CliClient, cfg: Config) -> Result<(), Box<dyn std::error::Error>> {
    // The worker owns its client; attaching needs its own handle because it
    // runs on this thread with the terminal handed over.
    let attach_backends = sbx_core::backend::Backends::from_config(Box::new(client.clone()), &cfg);
    // Resolved once, on this thread: the roots depend on the working directory,
    // which is fixed for the life of the process, and the worker should not be
    // reading config files between requests.
    let worker = Worker::spawn(client, cfg.clone(), repos::roots(cfg.repo_roots.as_deref()));
    let mut app = App::new(cfg);

    // Installs a panic hook that restores the terminal, so a crash cannot
    // leave the user in raw mode with no echo.
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app, &worker, &attach_backends);
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    worker: &Worker,
    attach_backends: &Backends,
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

        if let Some(session) = app.publish_request.take() {
            worker.send(Request::Publish(Box::new(session)));
        }

        if let Some(name) = app.destroy_request.take() {
            worker.send(Request::Destroy(name));
        }

        // A scan already running answers the pending request too, so a second
        // one is dropped rather than queued behind it.
        if std::mem::take(&mut app.scan_request) && !app.scan_in_flight {
            app.scan_in_flight = true;
            worker.send(Request::ScanRepos);
        }

        if std::mem::take(&mut app.providers_request) && !app.providers_in_flight {
            app.providers_in_flight = true;
            worker.send(Request::Providers);
        }

        if let Some((path, branch)) = app.inspect_request.take() {
            worker.send(Request::Inspect { path, branch });
        }

        if let Some(draft) = app.create_request.take() {
            worker.send(Request::Create(draft));
        }

        if let Some((session, update, label)) = app.repolicy_request.take() {
            worker.send(Request::Repolicy {
                session: Box::new(session),
                update,
                label,
            });
        }

        if let Some(session) = app.attach_request.take() {
            match attach(terminal, attach_backends, &session) {
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

        if !app.refreshing && app.last_refresh.elapsed() >= app.intervals.refresh {
            app.refreshing = true;
            app.last_refresh = Instant::now();
            // The first one repairs records left mid-create -- by a create that
            // died with its TUI, or by a write that lost a race before the cache
            // was locked. One exec per stuck session, once, rather than every
            // second; see `ops::refresh_with`.
            let repair = !std::mem::replace(&mut app.repaired, true);
            worker.send(Request::Refresh { repair });
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
///   [`Intervals::pane_ttl`] so a diff under the user's eyes stays current;
/// * the **stat column**, which every row needs, round-robined over the whole
///   list at no more than one request per [`STAT_MIN_GAP`].
///
/// The total is therefore bounded by the refresh intervals rather than by the
/// number of sessions.
fn dispatch_fetches(app: &mut App, worker: &Worker) {
    // Cloned out first so the immutable borrow of `app` ends before the
    // in-flight markers are written.
    let selected = app.selected().cloned().filter(|s| app.is_live(s));
    if let Some(session) = selected {
        let view = app.right_view();
        let ttl = view.ttl(&app.intervals);
        let name = session.name.clone();
        // Each arm asks the same three questions -- is it stale, is one already
        // in flight, and if not, fetch -- of a different map, so the shapes are
        // spelled out rather than abstracted over. Four near-identical closures
        // over four differently-typed maps costs more than it saves.
        let due = match view {
            RightView::Diff => app.diffs.get(&name).is_none_or(|c| c.stale_after(ttl)),
            RightView::Policy => app.policies.get(&name).is_none_or(|c| c.stale_after(ttl)),
            RightView::Events => app.events.get(&name).is_none_or(|c| c.stale_after(ttl)),
            // Nothing to fetch: the terminal pushes its own updates through the
            // pty, so a session being watched live costs no execs at all.
            RightView::Agent => false,
        };
        if due {
            match view {
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

    if app.poll_in_flight.is_some() || app.last_poll_request.elapsed() < app.intervals.poll_min_gap
    {
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
    let due = |s: &Session, ttl: Duration| {
        app.is_live(s) && app.polls.get(&s.name).is_none_or(|c| c.stale_after(ttl))
    };

    // The selected session first and on a shorter interval, whatever view is
    // showing: its state column, its stat and -- when the agent view is up -- its
    // screen are all drawn from this one capture, and all three are things the
    // user is looking at. It is one session however many there are, so the floor
    // between polls is what bounds the cost, not this.
    if let Some(s) = app
        .selected()
        .filter(|s| due(s, app.intervals.poll_selected_ttl))
    {
        return Some(s.clone());
    }
    app.sessions
        .iter()
        .filter(|s| due(s, app.intervals.poll_ttl))
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
        let mut app = App::new(Config::default());
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

    /// Everything keyed by a session has to go when the session does, or the
    /// maps grow for as long as the TUI runs.
    #[test]
    fn refresh_drops_content_for_vanished_sessions() {
        let mut app = app_with(&["a", "b"]);
        for name in ["a", "b"] {
            app.polls
                .insert(name.into(), Cached::new(ops::Poll::default()));
            app.diffs.insert(name.into(), Cached::new("old".into()));
        }

        let refreshed = ops::Refreshed {
            sessions: vec![Session::new("a".into(), "r".into(), "t".into())],
            ..Default::default()
        };
        app.apply_refresh(refreshed);

        assert!(app.polls.contains_key("a"));
        assert!(!app.polls.contains_key("b"), "stale poll must be dropped");
        assert!(!app.diffs.contains_key("b"), "and the diff with it");
    }

    /// Entering a session hands the whole terminal over to its agent. `a` is
    /// the same thing under a second key, kept because it is what `sbx attach`
    /// is called on the command line.
    #[test]
    fn entering_a_session_attaches_to_it() {
        for pressed in [KeyCode::Enter, KeyCode::Char('a')] {
            let mut app = app_with(&["a", "b"]);
            app.move_by(1);
            app.on_key(key(pressed));
            assert_eq!(
                app.attach_request.as_ref().map(|s| s.name.as_str()),
                Some("b"),
                "{pressed:?}"
            );
        }
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

    /// The choice is remembered per session, so glancing at what another agent
    /// is doing does not lose the diff you were reading.
    #[test]
    fn the_right_pane_choice_is_remembered_per_session() {
        let mut app = app_with(&["a", "b"]);
        assert_eq!(
            app.right_view(),
            RightView::Agent,
            "the agent's screen by default: it answers what the list asks"
        );

        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.right_view(), RightView::Diff);

        // Move to "b": it has its own, untouched choice.
        app.move_by(1);
        assert_eq!(app.right_view(), RightView::Agent);

        // Back to "a": the diff is still selected.
        app.move_by(-1);
        assert_eq!(app.right_view(), RightView::Diff);

        // And cycling all the way round returns.
        for _ in 1..RightView::ORDER.len() {
            app.on_key(key(KeyCode::Tab));
        }
        assert_eq!(app.right_view(), RightView::Agent);

        // Shift-Tab walks back, which is the only sane way to reach the last
        // view.
        app.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
        assert_eq!(app.right_view(), RightView::Events);
    }

    #[test]
    fn tab_on_an_empty_list_is_a_no_op() {
        let mut app = app_with(&[]);
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.right_view(), RightView::Agent);
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

    /// Every scrolling view keeps its own offset, not just the first two. A
    /// shared one would drop the user halfway down a policy after reading a long
    /// diff.
    ///
    /// The events feed is not one of them: there the keys move a cursor and the
    /// renderer derives the offset from it, which the test below covers.
    #[test]
    fn every_scrolling_view_scrolls_independently() {
        let mut app = app_with(&["a"]);
        app.focus = Focus::Right;
        app.right_lines = 500;
        app.right_height = 10;

        let scrolling: Vec<RightView> = RightView::ORDER
            .into_iter()
            .filter(|v| *v != RightView::Events)
            .collect();
        assert_eq!(scrolling.len(), 3);

        for (i, view) in scrolling.iter().enumerate() {
            app.views.insert("a".into(), *view);
            app.scroll_by(i as isize + 1);
        }
        for (i, view) in scrolling.iter().enumerate() {
            app.views.insert("a".into(), *view);
            assert_eq!(app.right_scroll(), i as u16 + 1, "{view:?}");
        }
    }

    // ---- the events feed's cursor, and the four decisions it offers ----

    fn event(at: u64, subject: &str, reason: Option<&str>) -> sbx_core::events::Event {
        sbx_core::events::Event {
            at,
            class: "NET:OPEN".into(),
            severity: sbx_core::events::Severity::Medium,
            verdict: sbx_core::events::Verdict::Denied,
            subject: subject.into(),
            policy: None,
            reason: reason.map(str::to_string),
        }
    }

    /// An app on the events pane of session "a", with a feed and a policy.
    ///
    /// The global lists are pointed at a temporary file, so a test that presses
    /// `A` or `B` cannot write to the developer's own configuration. Named by
    /// thread as well as process, because the suite runs these in parallel.
    fn app_on_feed(feed: Vec<sbx_core::events::Event>, rev: Option<PolicyRevision>) -> App {
        let mut app = app_with(&["a"]);
        app.lists_path = std::env::temp_dir().join(format!(
            "sbx-test-endpoints-{}-{:?}.json",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&app.lists_path);
        app.focus = Focus::Right;
        app.views.insert("a".into(), RightView::Events);
        app.events.insert("a".into(), Cached::new(Ok(feed)));
        if let Some(rev) = rev {
            app.policies.insert("a".into(), Cached::new(Ok(rev)));
        }
        app
    }

    /// A revision granting `host:port` to `binary`, which is the shape every
    /// question about reachability is asked against.
    fn policy_granting(host: &str, port: u16, binary: &str) -> PolicyRevision {
        let mut rev: PolicyRevision = serde_json::from_value(serde_json::json!({
            "version": 1, "active_version": 1, "hash": "abc",
        }))
        .unwrap();
        let mut p = openshell_client::Policy::default();
        p.network_policies.insert(
            "r".into(),
            openshell_client::NetworkPolicy {
                name: Some("r".into()),
                endpoints: vec![openshell_client::Endpoint {
                    host: host.into(),
                    port,
                    ..Default::default()
                }],
                binaries: vec![openshell_client::Binary {
                    path: binary.into(),
                }],
            },
        );
        rev.policy = Some(p);
        rev
    }

    /// In the feed the movement keys move a selection, not the viewport, and it
    /// clamps at both ends like the session list does.
    #[test]
    fn the_feed_moves_a_cursor_rather_than_a_scroll_offset() {
        let mut app = app_on_feed(
            vec![
                event(3, "/usr/bin/curl(1) -> c.com:443", None),
                event(2, "/usr/bin/curl(1) -> b.com:443", None),
                event(1, "/usr/bin/curl(1) -> a.com:443", None),
            ],
            None,
        );
        app.right_lines = 3;
        app.right_height = 10;

        assert_eq!(app.event_cursor("a"), 0, "the newest, which is the top");
        app.on_key(key(KeyCode::Char('k')));
        assert_eq!(app.event_cursor("a"), 0, "must not wrap past the top");

        app.on_key(key(KeyCode::Char('j')));
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.event_cursor("a"), 2);
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.event_cursor("a"), 2, "must not wrap past the end");

        assert_eq!(
            app.right_scroll(),
            0,
            "the offset is the renderer's, derived from the cursor"
        );

        // `G` and `g` are the same movement writ large.
        app.on_key(key(KeyCode::Char('g')));
        assert_eq!(app.event_cursor("a"), 0);
        app.on_key(key(KeyCode::Char('G')));
        assert_eq!(app.event_cursor("a"), 2);
    }

    /// The feed grows at the top, so a row index is not a handle on an event.
    /// Without re-anchoring, pressing `e` after a refetch acts on whatever
    /// happened to land under the cursor in between.
    #[test]
    fn the_cursor_follows_its_event_when_the_feed_grows() {
        let older = event(2, "/usr/bin/curl(1) -> b.com:443", None);
        let oldest = event(1, "/usr/bin/curl(1) -> a.com:443", None);
        let mut app = app_on_feed(vec![older.clone(), oldest.clone()], None);

        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.selected_event("a").unwrap().subject, oldest.subject);

        // Two arrivals, newest first, pushing everything down two rows.
        app.on_update(Update::Events {
            session: "a".into(),
            result: Box::new(Ok(vec![
                event(4, "/usr/bin/curl(1) -> d.com:443", None),
                event(3, "/usr/bin/curl(1) -> c.com:443", None),
                older,
                oldest.clone(),
            ])),
        });
        assert_eq!(app.event_cursor("a"), 3, "moved with its event");
        assert_eq!(app.selected_event("a").unwrap().subject, oldest.subject);

        // And an event that has fallen out of the history entirely puts the
        // cursor back on the newest rather than on an arbitrary neighbour.
        app.on_update(Update::Events {
            session: "a".into(),
            result: Box::new(Ok(vec![event(9, "/usr/bin/curl(1) -> z.com:443", None)])),
        });
        assert_eq!(app.event_cursor("a"), 0);
    }

    /// `e` is bound only in the feed, for the reason `w` and `t` are bound only
    /// in the policy pane: the thing being decided about has to be on screen.
    #[test]
    fn the_chooser_opens_only_from_the_feed_and_only_on_an_endpoint() {
        let denial = event(1, "/usr/bin/curl(9) -> pastebin.com:443", None);

        // Not the feed: `e` does nothing at all.
        let mut app = app_on_feed(vec![denial.clone()], None);
        app.views.insert("a".into(), RightView::Diff);
        app.on_key(key(KeyCode::Char('e')));
        assert!(app.pending_choice().is_none());

        // The feed, on a decision: it opens, and says what it is about.
        let mut app = app_on_feed(vec![denial], None);
        app.on_key(key(KeyCode::Char('e')));
        let (q, short) = app.pending_choice().expect("the chooser");
        assert!(q.contains("pastebin.com:443"), "{q}");
        assert!(q.contains("/usr/bin/curl"), "{q}");
        assert_eq!(
            short, "pastebin.com:443",
            "the form a narrow footer falls back to"
        );

        // Anything else takes it down without acting.
        app.on_key(key(KeyCode::Char('x')));
        assert!(app.pending_choice().is_none());
        assert!(app.repolicy_request.is_none());

        // The feed, on a `CONFIG:VALIDATED` warning, which is a sentence rather
        // than a decision about an endpoint.
        let mut warning = event(1, "'tls: terminate' is deprecated; use 'tls: skip'", None);
        warning.class = "CONFIG:VALIDATED".into();
        warning.verdict = sbx_core::events::Verdict::Neutral;
        let mut app = app_on_feed(vec![warning], None);
        app.on_key(key(KeyCode::Char('e')));
        assert!(app.pending_choice().is_none());
        assert!(app.status_is_error, "and it says why");
    }

    /// The chooser owns the keyboard while it is up. `a` is attach everywhere
    /// else in the TUI, and answering a question about egress must not also
    /// hand the terminal to the agent.
    #[test]
    fn the_chooser_owns_the_keyboard() {
        let mut app = app_on_feed(
            vec![event(1, "/usr/bin/curl(9) -> pastebin.com:443", None)],
            None,
        );
        app.on_key(key(KeyCode::Char('e')));
        app.on_key(key(KeyCode::Char('a')));
        assert!(app.attach_request.is_none(), "no terminal was handed over");
        let (session, update, label) = app.repolicy_request.take().expect("a policy change");
        assert_eq!(session.name, "a");
        assert_eq!(update.add_endpoints, ["pastebin.com:443:full:rest:enforce"]);
        assert_eq!(update.binaries, ["/usr/bin/curl"]);
        assert!(update.remove_endpoints.is_empty());
        assert!(label.contains("pastebin.com:443"), "{label}");
        assert!(
            !label.contains("global"),
            "`a` is this session only: {label}"
        );
        // And the consequence is put on screen, which is what makes one key
        // safe here.
        assert_eq!(app.right_view(), RightView::Policy);
    }

    /// `b` removes the endpoint. There is no deny that outranks an allow at L4,
    /// so a removal is the whole of what blocking can mean.
    #[test]
    fn blocking_removes_the_endpoint() {
        let mut app = app_on_feed(
            vec![event(1, "/usr/bin/git(9) -> github.com:443", None)],
            Some(policy_granting("github.com", 443, "/usr/bin/git")),
        );
        app.on_key(key(KeyCode::Char('e')));
        app.on_key(key(KeyCode::Char('b')));
        let (_, update, label) = app.repolicy_request.take().expect("a policy change");
        assert_eq!(update.remove_endpoints, ["github.com:443"]);
        assert!(update.add_endpoints.is_empty());
        assert!(update.binaries.is_empty(), "a removal is not per-binary");
        assert!(label.contains("blocked"), "{label}");
    }

    /// An endpoint being in the policy says nothing about whether *this* binary
    /// may reach it -- that difference is the whole premise of the tool, and
    /// reporting "already reachable" on the strength of another rule's binaries
    /// would refuse to fix the exact case the feed exists to show.
    #[test]
    fn an_allow_is_judged_against_the_binary_not_just_the_host() {
        // github.com:443 is granted, but to git. curl was denied.
        let mut app = app_on_feed(
            vec![event(1, "/usr/bin/curl(9) -> github.com:443", None)],
            Some(policy_granting("github.com", 443, "/usr/bin/git")),
        );
        app.on_key(key(KeyCode::Char('e')));
        let (q, _) = app.pending_choice().unwrap();
        assert!(q.contains("this binary denied"), "{q}");
        app.on_key(key(KeyCode::Char('a')));
        let (_, update, _) = app.repolicy_request.take().expect("a real change");
        assert_eq!(update.binaries, ["/usr/bin/curl"]);

        // Whereas the binary that already has it is told so, and nothing is
        // sent.
        let mut app = app_on_feed(
            vec![event(1, "/usr/bin/git(9) -> github.com:443", None)],
            Some(policy_granting("github.com", 443, "/usr/bin/git")),
        );
        app.on_key(key(KeyCode::Char('e')));
        app.on_key(key(KeyCode::Char('a')));
        assert!(app.repolicy_request.is_none());
        assert!(
            app.status.as_deref().unwrap().contains("already reachable"),
            "{:?}",
            app.status
        );
    }

    /// An L7 decision names a method and a path, never a binary -- and an
    /// endpoint rule with no binaries grants nothing. Issuing one anyway would
    /// report a change that did nothing, which is the failure mode this whole
    /// pane exists to prevent.
    #[test]
    fn an_allow_with_no_binary_to_bind_to_is_refused() {
        let mut app = app_on_feed(vec![event(1, "GET httpbin.org:443/ip", None)], None);
        app.on_key(key(KeyCode::Char('e')));
        app.on_key(key(KeyCode::Char('a')));
        assert!(app.repolicy_request.is_none());
        assert!(app.status_is_error);
        assert!(
            app.status.as_deref().unwrap().contains("no binary"),
            "{:?}",
            app.status
        );

        // Blocking it is still perfectly meaningful: the endpoint goes.
        app.on_key(key(KeyCode::Char('e')));
        app.on_key(key(KeyCode::Char('b')));
        let (_, update, _) = app.repolicy_request.take().expect("a removal");
        assert_eq!(update.remove_endpoints, ["httpbin.org:443"]);
    }

    /// A round trip that would change nothing is named rather than made -- but
    /// only after the global list has been written, because "already true here"
    /// is no reason not to record it for every session after this one.
    #[test]
    fn a_change_that_would_do_nothing_here_is_still_recorded_globally() {
        let mut app = app_on_feed(
            vec![event(1, "/usr/bin/curl(9) -> pastebin.com:443", None)],
            // pastebin is in no rule, so blocking it here is a no-op.
            Some(policy_granting("github.com", 443, "/usr/bin/git")),
        );
        app.on_key(key(KeyCode::Char('e')));
        app.on_key(key(KeyCode::Char('B')));

        assert!(app.repolicy_request.is_none(), "nothing to send");
        let note = app.status.clone().unwrap();
        assert!(note.contains("not in a's policy anyway"), "{note}");
        assert!(note.contains("global block list"), "{note}");
        assert_eq!(
            app.lists().verdict("pastebin.com:443"),
            Some(Listed::Blocked),
            "and it is on the list for every session after this one"
        );
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

        for view in [RightView::Agent, RightView::Diff, RightView::Events] {
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

    fn app_with_repo(repo: &str) -> App {
        let mut app = App::new(Config::default());
        app.sessions = vec![Session::new("a".into(), repo.into(), "t".into())];
        app.list_state.select(Some(0));
        app
    }

    const ADO: &str = "https://dev.azure.com/org/proj/_git/repo";

    /// Publishing pushes a branch and opens a pull request: other people see
    /// it, and pressing something else does not undo it. So it asks, and only
    /// `y` proceeds.
    #[test]
    fn publishing_asks_before_doing_anything() {
        let mut app = app_with_repo(ADO);
        app.on_key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::NONE));

        let q = app.pending_question().expect("a question");
        assert!(q.contains("sbx/a"), "{q}");
        assert!(q.contains("org/proj/repo"), "{q}");
        assert!(app.publish_request.is_none(), "nothing sent yet");
    }

    #[test]
    fn only_y_confirms_and_anything_else_cancels() {
        for (key, expected) in [('y', true), ('Y', true), ('n', false), ('x', false)] {
            let mut app = app_with_repo(ADO);
            app.on_key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::NONE));
            app.on_key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE));
            assert_eq!(
                app.publish_request.is_some(),
                expected,
                "{key} should {} publish",
                if expected { "" } else { "not" }
            );
            assert!(app.pending_question().is_none(), "the question must clear");
        }
        // Enter is not a confirmation either -- it is the attach key, and
        // treating it as yes would publish on a keystroke people press often.
        let mut app = app_with_repo(ADO);
        app.on_key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.publish_request.is_none());
        assert!(app.attach_request.is_none(), "and must not attach either");
    }

    /// While a question is up it owns the keyboard, or the answer gets consumed
    /// as a movement key and the question stays on screen.
    #[test]
    fn a_pending_question_swallows_every_other_key() {
        let mut app = App::new(Config::default());
        app.sessions = ["a", "b"]
            .iter()
            .map(|n| Session::new((*n).to_string(), ADO.into(), "t".into()))
            .collect();
        app.list_state.select(Some(0));

        app.on_key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::NONE));
        // `j` would normally move the cursor; here it cancels and moves nothing.
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.list_state.selected(), Some(0), "must not have moved");
        assert!(app.pending_question().is_none());
        assert!(app.publish_request.is_none());
    }

    /// A remote that cannot be published to is refused before the user is asked
    /// a question whose answer could not be honoured.
    #[test]
    fn an_unpublishable_remote_is_refused_without_asking() {
        for repo in ["git@github.com:o/r.git", "https://gitlab.com/o/r"] {
            let mut app = app_with_repo(repo);
            app.on_key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::NONE));
            assert!(app.pending_question().is_none(), "{repo}");
            assert!(app.publish_request.is_none(), "{repo}");
            assert!(app.status_is_error, "{repo}");
        }
    }

    #[test]
    fn a_second_publish_is_refused_while_one_runs() {
        let mut app = app_with_repo(ADO);
        app.publishing = Some("a".into());
        app.on_key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::NONE));
        assert!(app.pending_question().is_none());
        assert!(app.status_is_error);
    }

    #[test]
    fn a_completed_publish_reports_the_pull_request() {
        let mut app = app_with_repo(ADO);
        app.publishing = Some("a".into());
        app.on_update(Update::Published {
            session: "a".into(),
            result: Box::new(Ok(sbx_core::publish::Outcome {
                pushed: true,
                pull_request: Some("https://dev.azure.com/o/p/_git/r/pullrequest/7".into()),
                warnings: vec![],
            })),
        });
        assert!(app.publishing().is_none());
        assert!(!app.status_is_error);
        assert!(app.status.as_deref().unwrap().contains("pullrequest/7"));
    }

    #[test]
    fn a_failed_publish_surfaces_the_reason() {
        let mut app = app_with_repo(ADO);
        app.publishing = Some("a".into());
        app.on_update(Update::Published {
            session: "a".into(),
            result: Box::new(Err("the push was refused with 403".into())),
        });
        assert!(app.publishing().is_none());
        assert!(app.status_is_error);
        assert!(app.status.as_deref().unwrap().contains("403"));
    }

    /// Destroying deletes the sandbox, and a sandbox holds the only copy of
    /// whatever the agent has not published. So it asks, and the question says
    /// what would be lost.
    #[test]
    fn destroying_asks_and_names_what_goes() {
        let mut app = app_with(&["a"]);
        app.polls.insert(
            "a".into(),
            Cached::new(ops::Poll {
                stat: Some(ops::DiffStat {
                    added: 12,
                    removed: 3,
                    untracked: 1,
                }),
                status: None,
                pane: None,
            }),
        );

        app.on_key(key(KeyCode::Char('D')));

        let q = app.pending_question().expect("a question");
        assert!(q.contains("destroy a"), "{q}");
        assert!(q.contains("+12/-3"), "the stat is what is at stake: {q}");
        assert!(q.contains('?'), "untracked files count too: {q}");
        assert!(app.destroy_request.is_none(), "nothing sent yet");
    }

    /// An unpolled session must not claim there is nothing to lose -- absence of
    /// a stat is absence of knowledge.
    #[test]
    fn an_unpolled_session_says_the_sandbox_goes() {
        let mut app = app_with(&["a"]);
        app.on_key(key(KeyCode::Char('D')));
        let q = app.pending_question().expect("a question");
        assert!(q.contains("everything in it"), "{q}");
        assert!(!q.contains("nothing"), "must not claim a clean tree: {q}");
    }

    #[test]
    fn a_clean_session_says_there_is_nothing_to_lose() {
        let mut app = app_with(&["a"]);
        app.polls.insert(
            "a".into(),
            Cached::new(ops::Poll {
                stat: Some(ops::DiffStat::default()),
                status: None,
                pane: None,
            }),
        );
        app.on_key(key(KeyCode::Char('D')));
        let q = app.pending_question().expect("a question");
        assert!(q.contains("no changes"), "{q}");
    }

    #[test]
    fn only_y_destroys_and_anything_else_cancels() {
        for (pressed, expected) in [('y', true), ('Y', true), ('n', false), ('d', false)] {
            let mut app = app_with(&["a"]);
            app.on_key(key(KeyCode::Char('D')));
            app.on_key(key(KeyCode::Char(pressed)));
            assert_eq!(
                app.destroy_request.is_some(),
                expected,
                "{pressed} should {} destroy",
                if expected { "" } else { "not" }
            );
            assert!(app.pending_question().is_none());
        }
        // Enter is the attach key. Treating it as yes would destroy a session on
        // a keystroke people press constantly.
        let mut app = app_with(&["a"]);
        app.on_key(key(KeyCode::Char('D')));
        app.on_key(key(KeyCode::Enter));
        assert!(app.destroy_request.is_none());
        assert!(app.attach_request.is_none(), "and must not attach either");
    }

    /// Lowercase `d` is not a destroy key. It is next to `j`/`k`, and a session
    /// deleted by a typo is not recoverable.
    #[test]
    fn lowercase_d_does_nothing() {
        let mut app = app_with(&["a"]);
        app.on_key(key(KeyCode::Char('d')));
        assert!(app.pending_question().is_none());
        assert!(app.destroy_request.is_none());
    }

    /// A create still running would write its record back after the destroy
    /// dropped it, and go on cloning into a sandbox that no longer exists.
    #[test]
    fn a_session_still_being_created_is_refused() {
        let mut app = app_with(&["a"]);
        app.pending = Some(app.sessions[0].clone());
        app.on_key(key(KeyCode::Char('D')));
        assert!(app.pending_question().is_none());
        assert!(app.status_is_error);
    }

    #[test]
    fn a_second_destroy_is_refused_while_one_runs() {
        let mut app = app_with(&["a", "b"]);
        app.destroying = Some("b".into());
        app.on_key(key(KeyCode::Char('D')));
        assert!(app.pending_question().is_none());
        assert!(app.status_is_error);
    }

    /// The row goes on the answer coming back, not on the next refresh: the
    /// gateway reports a deleted sandbox as `Deleting` for a while, so a refresh
    /// landing in that window would put the row back as `dead`.
    #[test]
    fn a_completed_destroy_drops_the_row_and_its_caches() {
        let mut app = app_with(&["a", "b", "c"]);
        app.move_by(1); // on "b"
        for name in ["a", "b"] {
            app.polls
                .insert(name.into(), Cached::new(ops::Poll::default()));
            app.views.insert(name.into(), RightView::Diff);
        }
        app.destroying = Some("b".into());

        app.on_update(Update::Destroyed {
            session: "b".into(),
            result: Box::new(Ok(ops::Destroyed::Sandbox)),
        });

        assert!(app.destroying().is_none());
        assert!(!app.status_is_error);
        assert_eq!(
            app.sessions
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            ["a", "c"]
        );
        assert!(!app.polls.contains_key("b"), "cached poll must go with it");
        assert!(!app.views.contains_key("b"), "and the pane choice");
        assert!(app.polls.contains_key("a"), "other sessions are untouched");
        // The cursor lands on the neighbour rather than jumping to the top.
        assert_eq!(app.selected().map(|s| s.name.as_str()), Some("c"));
    }

    /// Destroying the last session must leave nothing selected, or every
    /// accessor keyed on the selection indexes past the end of the list.
    #[test]
    fn destroying_the_last_session_clears_the_selection() {
        let mut app = app_with(&["a"]);
        app.on_update(Update::Destroyed {
            session: "a".into(),
            result: Box::new(Ok(ops::Destroyed::Sandbox)),
        });
        assert!(app.sessions.is_empty());
        assert_eq!(app.list_state.selected(), None);
        assert!(app.selected().is_none());
    }

    /// A sandbox that was already gone is the desired end state, so the record
    /// goes too -- that is the only way to clear a session left behind by a
    /// create that died before it provisioned anything.
    #[test]
    fn an_already_gone_sandbox_still_clears_the_row() {
        let mut app = app_with(&["a"]);
        app.destroying = Some("a".into());
        app.on_update(Update::Destroyed {
            session: "a".into(),
            result: Box::new(Ok(ops::Destroyed::RecordOnly)),
        });
        assert!(app.sessions.is_empty());
        assert!(!app.status_is_error);
        assert!(app.status.as_deref().unwrap().contains("already gone"));
    }

    /// A failed destroy must leave the row alone: the sandbox is still there,
    /// and a row that vanished would hide a session that is still running.
    #[test]
    fn a_failed_destroy_keeps_the_row_and_reports_why() {
        let mut app = app_with(&["a"]);
        app.destroying = Some("a".into());
        app.on_update(Update::Destroyed {
            session: "a".into(),
            result: Box::new(Err("could not delete sbx-a: gateway unreachable".into())),
        });
        assert!(app.destroying().is_none());
        assert!(app.status_is_error);
        assert!(
            app.status
                .as_deref()
                .unwrap()
                .contains("gateway unreachable")
        );
        assert_eq!(app.sessions.len(), 1, "the session is still there");
    }

    /// The rows are numbered, so the numbers have to do something: `3` selects
    /// the third session from either pane, and a number with no session behind
    /// it does nothing rather than clearing the selection.
    #[test]
    fn digits_jump_to_a_session() {
        let mut app = app_with(&["a", "b", "c"]);
        app.on_key(key(KeyCode::Char('3')));
        assert_eq!(app.selected().map(|s| s.name.as_str()), Some("c"));

        app.on_key(key(KeyCode::Char('1')));
        assert_eq!(app.selected().map(|s| s.name.as_str()), Some("a"));

        // From the right pane too, where j/k mean scrolling.
        app.focus = Focus::Right;
        app.on_key(key(KeyCode::Char('2')));
        assert_eq!(app.selected().map(|s| s.name.as_str()), Some("b"));

        // Past the end, and zero, are no-ops.
        app.on_key(key(KeyCode::Char('9')));
        app.on_key(key(KeyCode::Char('0')));
        assert_eq!(app.selected().map(|s| s.name.as_str()), Some("b"));
    }

    fn poll_with(state: Option<State>) -> ops::Poll {
        ops::Poll {
            stat: None,
            status: state.map(|state| status::Report {
                state,
                detail: None,
                source: status::Source::Hook,
            }),
            pane: None,
        }
    }

    /// Scrolling the content pane must not require focusing it first: the diff
    /// was reported as unscrollable by someone pressing `j`, which moved the
    /// list exactly as the footer said it would.
    #[test]
    fn shift_arrows_and_paging_scroll_the_pane_from_the_list() {
        let mut app = app_with(&["a", "b"]);
        app.right_lines = 100;
        app.right_height = 10;
        assert_eq!(app.focus, Focus::List);

        app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
        assert_eq!(app.right_scroll(), 1, "shift-down scrolls the pane");
        assert_eq!(app.list_state.selected(), Some(0), "and not the list");

        app.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
        assert_eq!(app.right_scroll(), 0);

        // Paging a list of a handful of sessions is worth nothing; paging the
        // content is worth having from either side.
        app.on_key(key(KeyCode::PageDown));
        // A page keeps a line of context, so it is one less than the height.
        assert_eq!(app.right_scroll(), app.page() as u16);
        assert_eq!(app.list_state.selected(), Some(0));
        app.on_key(key(KeyCode::PageUp));
        assert_eq!(app.right_scroll(), 0);

        // And j/k still walk the list, which is what the footer promises.
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.list_state.selected(), Some(1));
    }

    /// The intervals are a budget, and the parts have to add up: the selected
    /// session must be served sooner than the rest, the floor between polls must
    /// leave a list of a useful size inside its own TTL, and nothing may be
    /// slower than the refresh that reconciles the list.
    ///
    /// Asserted for every `refresh` the config file will accept, not just the
    /// tuned set. That is the argument for scaling one number instead of
    /// exposing six: the relationships are the design, and a ratio cannot break
    /// them. `TICK` is the one thing that does not scale, which is exactly why
    /// `config::REFRESH_MIN` is where it is.
    fn assert_coherent(iv: Intervals) {
        assert!(
            iv.poll_selected_ttl < iv.poll_ttl,
            "what is on screen comes first"
        );
        assert!(
            iv.poll_min_gap < iv.poll_selected_ttl,
            "the floor must not be the thing that decides the interval"
        );
        // Up to this many sessions, a full round still lands inside poll_ttl, so
        // no session waits longer than its own interval to be looked at.
        let round = iv.poll_ttl.as_millis() / iv.poll_min_gap.as_millis();
        assert!(round >= 10, "only {round} sessions fit the round trip");
        // And the redraw has to be quicker than anything it draws, or fresh data
        // waits for the next frame.
        assert!(TICK < iv.poll_selected_ttl, "{iv:?}");
    }

    #[test]
    fn the_poll_budget_is_coherent() {
        assert_coherent(Intervals::TUNED);
    }

    #[test]
    fn the_poll_budget_survives_every_refresh_the_config_accepts() {
        for ms in [250, 500, 1000, 1500, 3000, 10_000, 60_000] {
            assert_coherent(Intervals::scaled(Duration::from_millis(ms)));
        }
    }

    #[test]
    fn refresh_scales_the_rest_and_nothing_else() {
        let iv = Intervals::scaled(Duration::from_secs(2));
        assert_eq!(iv.refresh, Duration::from_secs(2));
        assert_eq!(
            iv.poll_ttl,
            POLL_TTL * 2,
            "twice the default is twice as slow"
        );
        assert_eq!(iv.pane_ttl, PANE_TTL * 2);
        assert_eq!(iv.poll_min_gap, POLL_MIN_GAP * 2);

        // An unset `refresh` leaves the measured numbers exactly as measured.
        assert_eq!(Intervals::from_config(&Config::default()), Intervals::TUNED);
        assert_eq!(
            Intervals::scaled(REFRESH_EVERY),
            Intervals::TUNED,
            "the default value is the identity"
        );
    }

    /// The pane TTLs come from the same set, so a slower config slows the diff
    /// and the events feed too rather than leaving them at the tuned rate.
    #[test]
    fn the_pane_ttls_follow_the_config() {
        let app = App::new(Config {
            refresh: Some(Duration::from_secs(3)),
            ..Config::default()
        });
        assert_eq!(RightView::Diff.ttl(&app.intervals), PANE_TTL * 3);
        assert_eq!(RightView::Events.ttl(&app.intervals), PANE_TTL * 3);
        assert_eq!(RightView::Policy.ttl(&app.intervals), POLICY_TTL * 3);
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
        app.diffs.insert("a".into(), Cached::new("d".into()));
        app.polls
            .insert("a".into(), Cached::new(ops::Poll::default()));

        app.invalidate("a");

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

    // --- the create flow, driven through `App` exactly as keys reach it ------

    fn local_repo(name: &str, origin: Option<&str>) -> LocalRepo {
        LocalRepo {
            path: format!("/home/u/dev/{name}").into(),
            display: format!("~/dev/{name}"),
            name: name.to_string(),
            origin: origin.map(String::from),
            branch: Some("main".into()),
        }
    }

    /// Walk the flow to a queued create, returning the app it left behind.
    fn app_after_submit(names: &[&str], task: &str) -> App {
        let mut app = app_with(names);
        app.on_key(key(KeyCode::Char('n')));
        app.on_update(Update::Repos(vec![local_repo(
            "api",
            Some("https://github.com/o/api.git"),
        )]));
        app.on_key(key(KeyCode::Enter));
        for c in task.chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));
        app
    }

    #[test]
    fn n_opens_the_picker_and_asks_for_a_scan() {
        let mut app = app_with(&["a"]);
        app.on_key(key(KeyCode::Char('n')));
        assert!(matches!(app.create, Some(Create::Pick(_))));
        assert!(app.scan_request, "the loop sends this");
        assert!(app.providers_request);
    }

    #[test]
    fn a_failed_provider_read_is_retried_when_the_picker_opens_again() {
        let mut app = app_with(&[]);
        app.providers = Some(Err("gateway unreachable".into()));
        app.open_create();
        assert!(
            app.providers_request,
            "a failure is worth asking about again"
        );

        app.providers = Some(Ok(vec![]));
        app.providers_request = false;
        app.create = None;
        app.open_create();
        assert!(
            !app.providers_request,
            "but a good answer is only read once"
        );
    }

    /// A cached scan is shown at once. Reopening the picker and waiting seconds
    /// for a walk of a home directory already done would be the difference
    /// between this being usable and not.
    #[test]
    fn a_cached_scan_fills_the_picker_immediately() {
        let mut app = app_with(&[]);
        app.repos = Some(vec![local_repo("api", Some("u"))]);
        app.on_key(key(KeyCode::Char('n')));
        match &app.create {
            Some(Create::Pick(p)) => {
                assert!(!p.scanning());
                assert_eq!(p.rows().len(), 1);
            }
            _ => panic!("expected the picker"),
        }
    }

    /// The flow is modal: while it is open, keys go to it and nothing else.
    #[test]
    fn the_flow_owns_the_keyboard() {
        let mut app = app_with(&["a", "b"]);
        app.on_key(key(KeyCode::Char('n')));
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(
            app.list_state.selected(),
            Some(0),
            "j is a character in the filter, not a movement"
        );
        app.on_key(key(KeyCode::Esc));
        assert!(app.create.is_none());
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.list_state.selected(), Some(1), "and now it moves again");
    }

    #[test]
    fn submitting_queues_the_create_and_shows_a_row() {
        let app = app_after_submit(&["other"], "fix the readme");

        let draft = app.create_request.as_ref().expect("a queued create");
        assert_eq!(draft.name, "fix-readme");
        assert_eq!(draft.repo, "https://github.com/o/api.git");
        assert!(app.create.is_none(), "the flow closes on submit");

        // The row is in the list before the gateway has done anything, and it is
        // what the cursor is on.
        let row = app
            .sessions
            .iter()
            .find(|s| s.name == "fix-readme")
            .expect("a row");
        assert_eq!(row.state, State::Creating);
        assert_eq!(app.selected().map(|s| s.name.as_str()), Some("fix-readme"));
    }

    /// A derived name steps around the ones in use, so starting a second session
    /// in a repository that already has one needs no correcting: with no task
    /// typed, both derive the repository's name, which is the normal case.
    #[test]
    fn a_derived_name_avoids_a_collision_instead_of_refusing() {
        let app = app_after_submit(&["api"], "");
        let draft = app.create_request.as_ref().expect("a queued create");
        assert_eq!(draft.name, "api-2");
        assert!(
            app.create.is_none(),
            "the flow closes rather than complaining"
        );

        // Same for a task whose slug is taken. Names have room now, so the
        // counter is appended rather than eating the stem -- but a name already
        // at the cap still gives way to it, since the cap is the hard limit.
        let app = app_after_submit(&["fix-readme"], "fix the readme");
        let draft = app.create_request.as_ref().expect("a queued create");
        assert_eq!(draft.name, "fix-readme-2");
        assert!(sbx_core::session::validate_name(&draft.name).is_ok());
    }

    /// The guard is still needed for a name typed by hand: editing the name pins
    /// it, and a pinned name is the user's, not the form's to change.
    #[test]
    fn a_hand_typed_name_that_is_taken_is_refused_in_the_form() {
        let mut app = app_with(&["taken-name"]);
        app.on_key(key(KeyCode::Char('n')));
        app.on_update(Update::Repos(vec![local_repo(
            "api",
            Some("https://github.com/o/api.git"),
        )]));
        app.on_key(key(KeyCode::Enter));
        // Into the name field, cleared, and typed over.
        app.on_key(key(KeyCode::Tab));
        for _ in 0..10 {
            app.on_key(key(KeyCode::Backspace));
        }
        for c in "taken-name".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));

        assert!(app.create_request.is_none(), "nothing may be queued");
        match &app.create {
            Some(Create::Fill(form)) => assert!(
                form.error().unwrap().contains("already exists"),
                "got {:?}",
                form.error()
            ),
            _ => panic!("the form must stay open with the complaint on it"),
        }

        // Editing it to something free lets it through -- the cursor is still in
        // the name field, so this appends.
        app.on_key(key(KeyCode::Char('2')));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(
            app.create_request.as_ref().map(|d| d.name.as_str()),
            Some("taken-name2")
        );
    }

    #[test]
    fn progress_moves_the_row_through_the_states() {
        let mut app = app_after_submit(&[], "fix the readme");
        let name = "fix-readme";
        let state = |app: &App| app.sessions.iter().find(|s| s.name == name).unwrap().state;

        app.on_update(Update::Creating {
            session: name.to_string(),
            step: ops::Step::Clone,
        });
        assert_eq!(state(&app), State::Seeding);

        app.on_update(Update::Creating {
            session: name.to_string(),
            step: ops::Step::Agent,
        });
        assert_eq!(state(&app), State::Ready);
    }

    /// A create that has not reached a sandbox yet must not be polled: every
    /// exec would fail, at the cost of a subprocess and a blanked pane each.
    #[test]
    fn the_pending_row_is_not_polled_until_it_has_a_sandbox() {
        let mut app = app_after_submit(&[], "fix the readme");
        let name = "fix-readme";
        assert!(
            next_poll_target(&app).is_none(),
            "nothing else exists, and the pending row is not askable"
        );

        // Once the sandbox is up, it is polled like anything else.
        app.on_update(Update::Creating {
            session: name.to_string(),
            step: ops::Step::Clone,
        });
        assert_eq!(
            next_poll_target(&app).map(|s| s.name),
            Some(name.to_string())
        );
    }

    #[test]
    fn a_created_session_keeps_its_row_until_the_store_has_it() {
        let mut app = app_after_submit(&[], "fix the readme");
        let name = "fix-readme";
        let mut created = Session::new(name.into(), "r".into(), "t".into());
        created.state = State::Ready;

        app.on_update(Update::Created {
            session: name.to_string(),
            result: Box::new(Ok(ops::Created {
                session: created.clone(),
                warnings: vec![],
            })),
        });
        assert!(app.pending.is_some());

        // A refresh that has not caught up yet keeps the row rather than
        // blinking it out of the list.
        app.apply_refresh(ops::Refreshed::default());
        assert_eq!(app.sessions.len(), 1);

        // And once the store knows about it, the store's copy takes over.
        app.apply_refresh(ops::Refreshed {
            sessions: vec![created],
            ..Default::default()
        });
        assert!(app.pending.is_none());
        assert_eq!(app.sessions.len(), 1);
    }

    #[test]
    fn a_failed_create_drops_the_row_and_says_why() {
        let mut app = app_after_submit(&[], "fix the readme");
        app.on_update(Update::Created {
            session: "fix-readme".to_string(),
            result: Box::new(Err("the gateway said no".into())),
        });
        assert!(app.pending.is_none());
        assert!(app.status_is_error);
        assert!(
            app.status
                .as_deref()
                .unwrap()
                .contains("the gateway said no")
        );
        // The refresh that follows is what decides whether a record survived.
        assert!(app.last_refresh.elapsed() >= REFRESH_EVERY);
    }

    /// The create runs on a thread that dies with the process, so quitting
    /// mid-create can leave a half-seeded sandbox behind. Worth one question.
    #[test]
    fn quitting_mid_create_asks_first() {
        let mut app = app_after_submit(&[], "fix the readme");
        app.on_key(key(KeyCode::Char('q')));
        assert!(!app.should_quit);
        assert!(
            app.pending_question()
                .unwrap()
                .contains("still being created"),
            "got {:?}",
            app.pending_question()
        );
        app.on_key(key(KeyCode::Char('y')));
        assert!(app.should_quit);
    }

    #[test]
    fn quitting_with_nothing_in_flight_does_not_ask() {
        let mut app = app_with(&["a"]);
        app.on_key(key(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    /// Going back from the form re-opens the picker with the scan intact, so a
    /// mis-pick costs one keystroke rather than another walk of the disk.
    #[test]
    fn escaping_the_form_returns_to_a_populated_picker() {
        let mut app = app_with(&[]);
        app.on_key(key(KeyCode::Char('n')));
        app.on_update(Update::Repos(vec![local_repo("api", Some("u"))]));
        app.on_key(key(KeyCode::Char('a')));
        app.on_key(key(KeyCode::Char('p')));
        app.on_key(key(KeyCode::Enter));
        assert!(matches!(app.create, Some(Create::Fill(_))));

        app.on_key(key(KeyCode::Esc));
        match &app.create {
            Some(Create::Pick(p)) => {
                assert_eq!(p.rows().len(), 1, "the scan is reused");
                assert_eq!(p.query().text(), "ap", "and so is what was typed");
            }
            _ => panic!("expected the picker"),
        }
        // And escaping again leaves the flow entirely.
        app.on_key(key(KeyCode::Esc));
        assert!(app.create.is_none());
    }

    /// An inspection is asked for on the pick and matched to the repository it
    /// was asked about, so an answer arriving after a re-pick is discarded.
    #[test]
    fn an_inspection_for_another_repository_is_ignored() {
        let mut app = app_with(&[]);
        app.on_key(key(KeyCode::Char('n')));
        app.on_update(Update::Repos(vec![local_repo("api", Some("u"))]));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(
            app.inspect_request.as_ref().map(|(p, _)| p.clone()),
            Some("/home/u/dev/api".into())
        );

        app.on_update(Update::Inspected {
            path: "/home/u/dev/somewhere-else".into(),
            facts: Box::new(sbx_core::repos::Facts {
                uncommitted: 9,
                unpushed: None,
                base_on_remote: false,
                toolchains: Vec::new(),
            }),
        });
        match &app.create {
            Some(Create::Fill(form)) => assert!(
                form.facts().is_none(),
                "an answer about another repository must not be shown"
            ),
            _ => panic!("expected the form"),
        }
    }
}
