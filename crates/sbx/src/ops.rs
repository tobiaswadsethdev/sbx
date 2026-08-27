//! Operations shared by the CLI and the TUI.

use std::time::{Duration, Instant};

use openshell_client::{CreateOpts, Error as OsError, OpenShell, PolicyRevision, PolicyUpdate};

use crate::events;
use crate::forge;
use crate::policy;
use crate::publish;
use crate::seed;
use crate::session::{self, REPO_PATH, SELECTOR_MANAGED, STATUS_PATH, Session, State};
use crate::status;
use crate::store::{self, Store};

/// How much of the agent's pane to capture.
///
/// Was forty, when this was only feeding marker detection and every marker sits
/// in the last few lines. The agent view draws the same capture, and forty lines
/// of a fifty-row pane cut the top off the transcript -- the banner and the first
/// exchange -- so it is now more than a window's worth. Still bounded: a pane
/// left tall by an attach from a big terminal cannot turn a poll into a flood.
const PANE_LINES: usize = 120;

#[derive(Debug, Default)]
pub struct Refreshed {
    pub sessions: Vec<Session>,
    /// Sessions recovered from a sandbox the cache did not know about.
    pub adopted: Vec<String>,
    /// Sessions whose sandbox has just disappeared.
    pub dead: Vec<String>,
    /// Non-fatal problems, e.g. a sandbox that could not be adopted.
    pub warnings: Vec<String>,
}

/// Reconcile the cache against the gateway, adopt orphans, and persist.
/// [`refresh`], optionally repairing records left mid-lifecycle.
///
/// `repair` re-reads the metadata of any session whose record still says
/// `creating` or `seeding` and takes the sandbox's word for it. Two things leave a
/// record there: a create that died -- the thread is detached, so quitting the TUI
/// mid-clone is enough -- and, before the cache was locked, a refresh writing an
/// older state back over a finished one. Either way the sandbox knows what
/// happened and the record does not.
///
/// One exec per such session, so it is asked for once when a tool starts rather
/// than on every refresh: a create legitimately in flight has no metadata to read
/// yet, which is exactly how a still-cloning session is told apart from an
/// abandoned one, and asking it every second would spend an exec a second on the
/// difference.
pub fn refresh_with(
    client: &dyn OpenShell,
    repair: bool,
) -> Result<Refreshed, Box<dyn std::error::Error>> {
    // The gateway call first, outside the lock: it is the slow part, and holding
    // a lock across it would stall a create in another process for no reason.
    let live = client.list(Some(SELECTOR_MANAGED))?;

    // Reconciled against what is on disk *now*, not against a snapshot taken
    // before the call above. A create walking a session through `seeding` to
    // `ready` in another process finishes inside that window often enough that
    // the difference is a session whose record disagrees with its own sandbox.
    let rec = store::update(|store| {
        let rec = store::reconcile(store.list().into_iter().cloned().collect(), &live);
        store.merge(rec.sessions.clone());
        rec
    })?;

    let mut out = Refreshed {
        sessions: rec.sessions,
        dead: rec.dead,
        ..Default::default()
    };

    for orphan in &rec.orphans {
        let sandbox = session::sandbox_name(orphan);
        // An exec, so also outside the lock; the adopted record is written on its
        // own once it is known.
        match seed::read_meta(client, &sandbox) {
            Ok(s) => {
                out.adopted.push(s.name.clone());
                let record = s.clone();
                store::update(|store| store.upsert(record))?;
                out.sessions.push(s);
            }
            Err(e) => out.warnings.push(format!("could not adopt {sandbox}: {e}")),
        }
    }

    if repair {
        // Only where the sandbox is there to be asked: a record whose sandbox has
        // gone is already `dead`.
        let stuck: Vec<Session> = out
            .sessions
            .iter()
            .filter(|s| matches!(s.state, State::Creating | State::Seeding))
            .filter(|s| live.iter().any(|sb| sb.name == s.sandbox))
            .cloned()
            .collect();

        for s in stuck {
            // The seeder's own report, which is the only thing that knows: it runs
            // detached inside the sandbox, so "still cloning" and "gave up" look
            // identical from out here.
            let (state, note) = match seed::seed_state(client, &s) {
                seed::SeedState::Done => (State::Ready, "seeding finished".to_string()),
                seed::SeedState::Failed(why) => (State::Failed, format!("seeding failed: {why}")),
                seed::SeedState::Running { step, alive: false } => {
                    (State::Failed, format!("seeding stopped during `{step}`"))
                }
                // Genuinely still going, in the sandbox, whatever happened to the
                // tool that started it. Leave it alone.
                seed::SeedState::Running { .. } => continue,
                // Nothing to read: a sandbox seeded by an older sbx, before the
                // seeder reported anything. Fall back to the metadata, which is
                // written once seeding is done.
                seed::SeedState::Unknown => match seed::read_meta(client, &s.sandbox) {
                    Ok(m) if m.state != s.state => (m.state, "sandbox metadata".to_string()),
                    _ => continue,
                },
            };

            if state == s.state {
                continue;
            }
            out.warnings
                .push(format!("{}: {} -> {state} ({note})", s.name, s.state));
            let mut fixed = s.clone();
            fixed.state = state;
            let record = fixed.clone();
            store::update(|store| store.upsert(record))?;
            if let Some(slot) = out.sessions.iter_mut().find(|x| x.name == fixed.name) {
                *slot = fixed;
            }
        }
    }

    out.sessions.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Everything needed to start a session, however it was asked for.
///
/// The one description of a new session shared by `sbx new` and the TUI's
/// create form. Both build this and hand it to [`create`], so the two cannot
/// drift into producing subtly different sessions.
#[derive(Debug, Clone, Default)]
pub struct Draft {
    pub name: String,
    /// Clone URL. A local checkout is only ever a way of *naming* one: see
    /// [`crate::repos`].
    pub repo: String,
    pub task: String,
    /// Branch to clone from; `None` means the remote's default.
    pub base: Option<String>,
    /// Policy template name, or a path to a YAML file.
    pub policy: String,
    pub providers: Vec<String>,
    /// Whether to start the agent once the clone is done.
    pub start: bool,
}

/// A stage of creating a session, reported as it begins.
///
/// Creating takes tens of seconds and each stage can fail differently, so the
/// caller is told what is happening rather than being left with one long wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Sandbox,
    Clone,
    Agent,
}

impl Step {
    pub fn label(self) -> &'static str {
        match self {
            Step::Sandbox => "creating the sandbox",
            Step::Clone => "cloning the repository",
            Step::Agent => "starting the agent",
        }
    }

    /// The step a seeder's own report corresponds to.
    ///
    /// The seeder names its steps (`clone`, `branch`, `meta`, `agent`) and this
    /// maps them onto what the interface already shows. `branch` and `meta` are
    /// part of cloning as far as anyone watching is concerned.
    pub fn for_seed(step: &str) -> Option<Step> {
        match step {
            "clone" | "branch" | "meta" => Some(Step::Clone),
            "agent" => Some(Step::Agent),
            _ => None,
        }
    }

    /// The session state this stage corresponds to, so a list showing a
    /// half-created session says the same thing as the progress message.
    pub fn state(self) -> State {
        match self {
            Step::Sandbox => State::Creating,
            Step::Clone => State::Seeding,
            Step::Agent => State::Ready,
        }
    }
}

/// A created session, plus anything that went wrong without stopping it.
#[derive(Debug, Clone)]
pub struct Created {
    pub session: Session,
    /// Non-fatal problems: an unrecognised host, an agent that did not come up.
    /// The session exists and is usable in every one of these cases.
    pub warnings: Vec<String>,
}

/// Create a sandbox, clone the repository, cut the work branch, start the agent.
///
/// The order matters and is the reason this is one function rather than steps a
/// caller sequences: everything that can be checked without side effects is
/// checked first, so a bad name or an unknown policy fails while nothing exists
/// yet, and every failure afterwards leaves a record saying what happened.
///
/// The sandbox image is deliberately *not* built here. `image::build` streams
/// docker's output to the terminal, which would tear a TUI apart; the CLI calls
/// [`crate::image::ensure`] before this, and the TUI refuses to create until
/// the image is there. See the doc comment on [`crate::image::ensure`].
pub fn create(
    client: &dyn OpenShell,
    draft: &Draft,
    progress: &mut dyn FnMut(Step),
) -> Result<Created, String> {
    let mut warnings = Vec::new();

    session::validate_name(&draft.name).map_err(|e| e.to_string())?;

    // An unusable remote fails before a sandbox exists. An unknown *host* is
    // only a warning: a public repository on any host still clones, and only
    // publishing needs to know the forge.
    match forge::Remote::parse(&draft.repo) {
        Ok(_) => {}
        Err(e @ (forge::Error::Ssh(_) | forge::Error::Incomplete { .. })) => {
            return Err(e.to_string());
        }
        Err(e) => warnings.push(format!("{e}; publishing will not be available")),
    }

    // A first look for a name clash, so an obvious mistake fails before a
    // sandbox exists. Not a lock: the gateway refuses a duplicate sandbox name
    // anyway, which is the check that actually holds.
    if Store::load()
        .map_err(|e| e.to_string())?
        .contains(&draft.name)
    {
        return Err(format!("session `{}` already exists", draft.name));
    }

    // Resolved before anything is created, so a typo in the policy fails before
    // a sandbox exists rather than after. The guard owns a temp file when the
    // policy came from a template, so it has to outlive the create call below.
    let resolved = policy::resolve(&draft.policy).map_err(|e| e.to_string())?;

    let mut s = Session::new(draft.name.clone(), draft.repo.clone(), draft.task.clone());
    s.base_branch = draft.base.clone();
    s.policy = Some(resolved.label.clone());
    s.providers = draft.providers.clone();

    progress(Step::Sandbox);
    let opts = CreateOpts {
        name: s.sandbox.clone(),
        labels: s.labels(),
        policy: Some(resolved.path().to_path_buf()),
        providers: draft.providers.clone(),
        from: Some(session::IMAGE.to_string()),
        // Keep the sandbox alive after the create command exits.
        command: vec!["true".into()],
        ..Default::default()
    };

    // Each failure is recorded before being returned. A `Failed` record is the
    // only trace of a sandbox that may exist at the gateway but was never
    // seeded, and without it that sandbox is invisible to `sbx rm`.
    if let Err(e) = client.create(&opts) {
        s.state = State::Failed;
        save(s, &mut warnings);
        return Err(e.to_string());
    }

    s.state = State::Seeding;
    save(s.clone(), &mut warnings);

    progress(Step::Clone);
    if let Err(e) = seed::launch(client, &s, draft.start) {
        s.state = State::Failed;
        save(s, &mut warnings);
        return Err(e.to_string());
    }

    // From here the sandbox is doing the work and this is only watching. Quitting
    // now costs the report, not the session: the seeder finishes on its own and
    // the next `refresh_with(.., true)` catches the record up.
    match watch_seed(client, &s, progress) {
        Watched::Done => {
            s.state = State::Ready;
            save(s.clone(), &mut warnings);
        }
        Watched::Failed(why) => {
            s.state = State::Failed;
            save(s.clone(), &mut warnings);
            return Err(why);
        }
        Watched::StillGoing => {
            warnings.push(format!(
                "{} is still seeding in its sandbox; it will be picked up on the next refresh",
                s.name
            ));
        }
    }

    Ok(Created {
        session: s,
        warnings,
    })
}

/// How long to watch a seeder before leaving it to finish on its own.
///
/// Generous, because a large repository really does take minutes to clone, and
/// running out of patience here costs nothing: the seeder is detached, so the
/// only thing lost is the progress report.
const SEED_WATCH_LIMIT: Duration = Duration::from_secs(15 * 60);
/// How often to ask. One exec each, ~50ms, against a step that changes rarely --
/// often enough that the create feels attended, cheap enough not to matter.
const SEED_WATCH_EVERY: Duration = Duration::from_millis(400);

enum Watched {
    Done,
    Failed(String),
    StillGoing,
}

/// Follow a detached seeder, reporting each step as it starts.
fn watch_seed(
    client: &dyn OpenShell,
    session: &Session,
    progress: &mut dyn FnMut(Step),
) -> Watched {
    let start = Instant::now();
    let mut reported = String::new();

    while start.elapsed() < SEED_WATCH_LIMIT {
        match seed::seed_state(client, session) {
            seed::SeedState::Done => return Watched::Done,
            seed::SeedState::Failed(why) => {
                return Watched::Failed(format!("seeding failed: {why}"));
            }
            seed::SeedState::Running { step, alive } => {
                if !alive && !step.is_empty() {
                    // The seeder is gone and never said `done` or `failed`, which
                    // only happens if the sandbox itself went out from under it.
                    return Watched::Failed(format!("seeding stopped during `{step}`"));
                }
                if step != reported {
                    if let Some(s) = Step::for_seed(&step) {
                        progress(s);
                    }
                    reported = step;
                }
            }
            // Nothing written yet: the script is starting.
            seed::SeedState::Unknown => {}
        }
        std::thread::sleep(SEED_WATCH_EVERY);
    }
    Watched::StillGoing
}

/// Write one lifecycle change through to the cache.
///
/// Reloaded under the lock and saved per step, rather than held open across the
/// create: a create takes tens of seconds -- minutes on a large repository -- and
/// a TUI refreshing every second writes this file the whole time. Holding a
/// snapshot from the start meant the last write won, and the last write was
/// usually the refresh, putting a `ready` session back to `seeding`.
fn save(session: Session, warnings: &mut Vec<String>) {
    if let Err(e) = store::update(|store| store.upsert(session)) {
        warnings.push(format!("could not update the session cache: {e}"));
    }
}

/// Line cap on a fetched diff. Diffs can be arbitrarily large and the pane is
/// scrolled in memory, so the fetch is bounded rather than the render.
const DIFF_LINE_CAP: usize = 2000;

/// Section and notice markers for the diff body. Shared with the policy pane;
/// see [`crate::pane`] for why these sigils and not others.
pub use crate::pane::{NOTICE as DIFF_NOTICE, SECTION as DIFF_SECTION};

/// How much a session's working copy has diverged from its base branch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiffStat {
    pub added: u32,
    pub removed: u32,
    /// Untracked entries. Counted as entries rather than lines: whole untracked
    /// directories collapse to one, so the count stays bounded no matter what
    /// the agent left lying around.
    pub untracked: u32,
}

impl DiffStat {
    pub fn is_empty(&self) -> bool {
        *self == DiffStat::default()
    }

    /// Parse the `<added> <removed> <untracked>` line the stat script prints.
    fn parse(s: &str) -> Option<Self> {
        let mut it = s.split_whitespace();
        let mut next = || it.next()?.parse::<u32>().ok();
        Some(DiffStat {
            added: next()?,
            removed: next()?,
            untracked: next()?,
        })
    }
}

/// Shell that resolves the base ref to diff against, leaving it in `$base`.
///
/// `git clone` sets `refs/remotes/origin/HEAD`, so the remote's default branch
/// is recoverable even when the session did not pin one. `$base` is left empty
/// if it cannot be resolved, which callers must handle: a fresh clone of a
/// repository with an unusual remote layout has no usable base.
fn resolve_base(session: &Session) -> String {
    // A stored base branch names a local branch; the remote-tracking ref is the
    // one that still points at the base after the agent commits.
    let base = match &session.base_branch {
        Some(b) => format!("origin/{b}"),
        None => String::new(),
    };
    format!(
        r#"base={base}
if [ -z "$base" ]; then
  base=$(git symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null)
fi
if [ -n "$base" ]; then
  git rev-parse --verify --quiet "$base" >/dev/null 2>&1 || base=''
fi
"#,
        base = seed::sh_quote(&base),
    )
}

/// The diff between a session's work and the branch it started from.
///
/// One exec, because exec on a sandbox is serialised: a second concurrent call
/// waits behind the first, so each pane costs exactly one round trip.
///
/// Three sections, because none of them alone is the answer. `diff base...HEAD`
/// is committed work measured from the merge-base, so commits landing on the
/// base branch afterwards never show up as the agent's. `diff HEAD` is staged
/// and unstaged work together. Untracked files appear in neither, and a new
/// file is the most common thing an agent produces.
pub fn repo_diff(client: &dyn OpenShell, session: &Session) -> String {
    let script = format!(
        r#"cd {repo} 2>/dev/null || {{ printf 'no repository at %s\n' {repo}; exit 0; }}
{resolve_base}
emit() {{
  if [ -z "$2" ]; then return 0; fi
  printf '{section}%s\n' "$1"
  total=$(printf '%s\n' "$2" | wc -l)
  if [ "$total" -gt {cap} ]; then
    printf '%s\n' "$2" | head -n {cap}
    printf '{notice}showing {cap} of %s lines; attach to read the rest\n' "$total"
  else
    printf '%s\n' "$2"
  fi
}}

any=''
if [ -n "$base" ]; then
  committed=$(git --no-pager diff --no-color "$base...HEAD" 2>/dev/null)
  if [ -n "$committed" ]; then any=y; fi
  emit "committed, vs $base" "$committed"
else
  printf '{notice}base branch could not be resolved; committed work is not shown\n'
fi

working=$(git --no-pager diff --no-color HEAD 2>/dev/null)
if [ -n "$working" ]; then any=y; fi
emit 'uncommitted' "$working"

untracked=$(git ls-files --others --exclude-standard --directory 2>/dev/null)
if [ -n "$untracked" ]; then any=y; fi
emit 'untracked' "$untracked"

if [ -z "$any" ]; then printf 'no changes yet\n'; fi
"#,
        repo = seed::sh_quote(REPO_PATH),
        resolve_base = resolve_base(session),
        section = DIFF_SECTION,
        notice = DIFF_NOTICE,
        cap = DIFF_LINE_CAP,
    );

    match client.exec(&session.sandbox, &["sh", "-c", &script]) {
        Ok(out) if out.ok() => out.trimmed().to_string(),
        Ok(out) => format!("(could not read the diff: {})", out.stderr.trim()),
        Err(e) => format!("(sandbox unreachable: {e})"),
    }
}

/// The effective policy of a session's sandbox.
///
/// A gateway call, not an exec, so unlike the diff and the poll this does not
/// queue behind whatever else is running against the sandbox.
pub fn policy(client: &dyn OpenShell, session: &Session) -> Result<PolicyRevision, String> {
    client
        .policy(&session.sandbox)
        .map_err(|e| format!("could not read the policy: {e}"))
}

/// How many log lines to ask for. The gateway returns the newest, so this is a
/// window on the end of the log rather than a limit on what is kept.
///
/// Raised when the poll interval came down: every exec sbx makes writes three
/// events of its own, `events::parse` drops them, and the window has to be big
/// enough that what is left still covers a useful stretch of time. The read
/// itself is 14ms for 400 lines, so this is close to free.
const LOG_LINES: usize = 1500;

/// A session's recent policy decisions, newest first.
///
/// Newest first because the pane is a feed: the event you want is the one that
/// just happened, and it should be at the top without scrolling.
pub fn events(client: &dyn OpenShell, session: &Session) -> Result<Vec<events::Event>, String> {
    let raw = client
        .logs(&session.sandbox, LOG_LINES)
        .map_err(|e| format!("could not read the log: {e}"))?;
    // Merged into what this session has already shown rather than replacing it:
    // the gateway's window is a couple of minutes wide at these poll intervals,
    // and the feed is meant to be a record. Newest first comes back from the
    // merge, so the pane still reads as a feed.
    Ok(events::merge_kept(&session.name, events::parse(&raw)))
}

/// Apply an incremental policy change and report what the sandbox ended up
/// with, so the caller never has to assume the change landed.
pub fn repolicy(
    client: &dyn OpenShell,
    session: &Session,
    update: &PolicyUpdate,
) -> Result<PolicyRevision, String> {
    client
        .policy_update(&session.sandbox, update)
        .map_err(|e| format!("policy update failed: {e}"))?;
    policy(client, session)
}

/// Publish a session and record that it happened.
///
/// The store update lives here rather than in [`crate::publish`] so the CLI and
/// the TUI cannot disagree about it -- the TUI reads the state back on its next
/// refresh, and a publish that updated only one of the two paths would show as
/// unpublished in whichever was missed.
pub fn publish(
    client: &dyn OpenShell,
    session: &Session,
    opts: &publish::Options,
) -> Result<publish::Outcome, String> {
    let outcome = publish::publish(client, session, opts).map_err(|e| e.to_string())?;
    if outcome.pushed {
        // Published is a fact about the branch, not the sandbox, so it is
        // recorded even when the pull request could not be opened: the work has
        // left the sandbox either way, which is what the state means.
        store::update(|store| {
            if let Some(mut s) = store.get(&session.name).cloned() {
                s.state = session::State::Published;
                store.upsert(s);
            }
        })
        .map_err(|e| e.to_string())?;
    }
    Ok(outcome)
}

/// The shell that attaches to a session's agent, for both `sbx attach` and the
/// TUI.
///
/// One definition, because the two paths have to leave the sandbox in the same
/// state. `attach -d` evicts a client left behind by an earlier crash; without it
/// a stale client makes the new attach share a resized, confusing view. Falling
/// through to `new-session` means attaching always lands somewhere useful even if
/// the agent was never started or has been killed.
///
/// The resize afterwards is not cosmetic. tmux sizes a window to its latest
/// client and *keeps* that size after the client leaves, and
/// [`crate::status::scrape_pane`] reads that window -- so attaching from an
/// 80-column terminal would otherwise leave the agent's pane 80 columns wide for
/// the rest of its life, narrow enough to truncate the footer the running marker
/// lives in. `window-size latest` goes straight back, so the next client resizes
/// the window as usual; with nothing attached, tmux has no client size to apply
/// and the wide one stands. Every part of it is best-effort: a session that has
/// just been created by the fallback above is not worth failing an attach over.
pub fn attach_script(session: &Session) -> String {
    let (cols, rows) = session::SCRAPE_SIZE;
    format!(
        "{UTF8_ENV} \
         tmux -u -f /etc/tmux.conf attach -d -t {tmux} 2>/dev/null \
         || {UTF8_ENV} tmux -u -f /etc/tmux.conf new-session -s {tmux} -c {repo}; \
         tmux -u resize-window -t {tmux} -x {cols} -y {rows} 2>/dev/null; \
         tmux -u set -w -t {tmux} window-size latest 2>/dev/null; \
         true",
        tmux = seed::sh_quote(&session.tmux),
        repo = seed::sh_quote(REPO_PATH),
    )
}

/// The locale a tmux client needs, and the `-u` that does not depend on it.
///
/// The gateway does not pass the image's environment through to an exec, so a
/// client started this way inherits no locale: tmux then assumes a terminal that
/// is not UTF-8, draws box rules with the DEC line-drawing set and replaces every
/// character it cannot map with `_`. That is what turned Claude Code's banner and
/// its `⏸` and `❯` glyphs into underscores. `-u` says "this terminal is UTF-8"
/// outright; the locale is exported as well because everything else in the
/// sandbox reads it -- git for one -- and `COLORTERM` is how the agent decides it
/// may use 24-bit colour.
const UTF8_ENV: &str = "LANG=C.UTF-8 LC_ALL=C.UTF-8 COLORTERM=truecolor";

/// What destroying a session did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destroyed {
    /// The gateway deleted the sandbox.
    Sandbox,
    /// There was no sandbox left to delete; only the record went.
    RecordOnly,
}

/// Delete a session's sandbox and drop its record.
///
/// Shared by `sbx rm` and the TUI, so destroying means one thing wherever it is
/// asked for -- and so the TUI cannot leave behind a record the CLI would then
/// report as a session whose sandbox has died.
///
/// A sandbox the gateway has never heard of is the desired end state rather than
/// a failure: that is the case for a session left behind by a create that died
/// before provisioning, and refusing to remove the record would make it
/// permanent. The name is resolved through the cache with a fall back to the
/// naming convention, so a session the cache has lost is still removable.
///
/// Deletion itself is asynchronous. The sandbox stays listed as `Deleting` for
/// a while afterwards, which `store::reconcile` already reads as dead, so a
/// caller that refreshes immediately sees the row go rather than come back.
pub fn destroy(client: &dyn OpenShell, name: &str) -> Result<Destroyed, String> {
    let sandbox = Store::load()
        .map_err(|e| format!("could not read the session cache: {e}"))?
        .get(name)
        .map(|s| s.sandbox.clone())
        .unwrap_or_else(|| session::sandbox_name(name));

    let outcome = match client.delete(&sandbox) {
        Ok(()) => Destroyed::Sandbox,
        Err(OsError::NotFound(_)) => Destroyed::RecordOnly,
        Err(e) => return Err(format!("could not delete {sandbox}: {e}")),
    };

    // Only after the gateway has accepted the deletion: dropping the record
    // first would lose the sandbox name on a failure, leaving a sandbox running
    // that nothing knows how to name.
    store::update(|store| store.remove(name))
        .map_err(|e| format!("deleted {sandbox}, but could not update the cache: {e}"))?;
    // The kept events go with it: they are about a sandbox that no longer exists.
    events::forget_kept(name);
    Ok(outcome)
}

/// Everything one round trip per session is worth spending an exec on.
///
/// Kept together deliberately. Exec on a sandbox is serialised gateway-side, so
/// two separate polls would not just double the traffic -- they would queue
/// behind each other. One script, one round trip, both answers.
#[derive(Debug, Clone, Default)]
pub struct Poll {
    pub stat: Option<DiffStat>,
    pub status: Option<status::Report>,
    /// The agent's screen as captured, for the pane that shows it.
    ///
    /// Kept rather than dropped after the markers have been read: the capture is
    /// already paid for -- it is what decides the state column -- so showing it
    /// costs nothing beyond the memory. Empty for a sandbox with no agent pane.
    pub pane: Option<String>,
}

/// Read a session's diff stat and agent state in a single exec.
///
/// Output is three sections, separated by markers rather than parsed
/// positionally: a pane capture is arbitrary text and can contain anything,
/// including something that looks like a stat line.
pub fn poll(client: &dyn OpenShell, session: &Session) -> Poll {
    let script = poll_script(session);

    // A poll is decoration on a column: an unreachable sandbox or a
    // half-seeded repository leaves it blank rather than shouting.
    let Ok(out) = client.exec(&session.sandbox, &["sh", "-c", &script]) else {
        return Poll::default();
    };
    if !out.ok() {
        return Poll::default();
    }
    parse_poll(&out.stdout, session::now_epoch())
}

/// The script [`poll`] runs. Separate so its shape can be asserted on.
///
/// The repository work is confined to a subshell: a session whose clone has not
/// finished, or failed, still has an agent worth asking about, and a bare `cd`
/// failure would otherwise take the rest of the script with it.
fn poll_script(session: &Session) -> String {
    format!(
        r#"( cd {repo} 2>/dev/null || exit 0
{resolve_base}
mb=''
if [ -n "$base" ]; then mb=$(git merge-base "$base" HEAD 2>/dev/null); fi
if [ -z "$mb" ]; then mb=HEAD; fi
tracked=$(git --no-pager diff --numstat "$mb" 2>/dev/null |
  awk '{{a+=$1; d+=$2}} END {{printf "%d %d", a+0, d+0}}')
untracked=$(git ls-files --others --exclude-standard --directory 2>/dev/null | wc -l)
printf '%s %s
' "$tracked" "$untracked" )
printf '%s
' {status_marker}
cat {status_path} 2>/dev/null
printf '
%s
' {pane_marker}
tmux -u -f /etc/tmux.conf capture-pane -pe -t {tmux} 2>/dev/null | tail -n {pane_lines}
"#,
        repo = seed::sh_quote(REPO_PATH),
        resolve_base = resolve_base(session),
        status_marker = seed::sh_quote(status::STATUS_MARKER),
        status_path = seed::sh_quote(STATUS_PATH),
        pane_marker = seed::sh_quote(status::PANE_MARKER),
        tmux = seed::sh_quote(&session.tmux),
        pane_lines = PANE_LINES,
    )
}

/// Split the poll script's output and interpret each part.
///
/// Separate from [`poll`] so it can be tested against captured output without a
/// gateway.
fn parse_poll(stdout: &str, now: u64) -> Poll {
    let (stat_part, rest) = match stdout.split_once(status::STATUS_MARKER) {
        Some(split) => split,
        // An older sandbox, or a script that failed before the first marker.
        None => (stdout, ""),
    };
    let (hook_part, pane_part) = rest.split_once(status::PANE_MARKER).unwrap_or((rest, ""));

    // The capture carries the colour it was drawn in, which the pane that shows
    // it wants and the marker search must not see: a phrase with a colour change
    // inside it is not findable. One strip, used for the search only.
    let plain = crate::ansi::strip(pane_part);

    Poll {
        stat: DiffStat::parse(stat_part.trim()),
        status: status::combine(
            status::parse_hook(hook_part).as_ref(),
            status::scrape_pane(&plain),
            now,
        ),
        // Kept as captured, escapes and all: the pane redraws it with the colour
        // the agent chose. Emptiness is judged on the stripped copy, because a
        // screen of nothing but colour changes is a blank screen.
        pane: Some(pane_part.trim_end().to_string()).filter(|_| !plain.trim().is_empty()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_stat_line() {
        assert_eq!(
            DiffStat::parse("12 3 1"),
            Some(DiffStat {
                added: 12,
                removed: 3,
                untracked: 1
            })
        );
        assert_eq!(DiffStat::parse("0 0 0"), Some(DiffStat::default()));
        // The script prints the awk result and the count with a single space,
        // but a repository with no changes at all makes awk emit "0 0" and the
        // shell add the third field, so tolerate any run of whitespace.
        assert_eq!(
            DiffStat::parse("  6   0  \n"),
            None,
            "a missing field is not a zero"
        );
        assert_eq!(DiffStat::parse(""), None);
        assert_eq!(DiffStat::parse("a b c"), None);
        assert_eq!(DiffStat::parse("-1 0 0"), None, "counts are never negative");
    }

    #[test]
    fn empty_stat_is_distinguishable_from_a_measured_one() {
        assert!(DiffStat::default().is_empty());
        assert!(
            !DiffStat {
                added: 0,
                removed: 0,
                untracked: 1
            }
            .is_empty(),
            "an untracked file is a change even with no line edits"
        );
    }

    /// Both attach paths run this, and each clause is there for a reason that is
    /// invisible until it is missing.
    #[test]
    fn the_attach_script_attaches_falls_back_and_puts_the_size_back() {
        let script = attach_script(&session());
        assert!(script.contains("attach -d"), "{script}");
        // Without these the client draws Claude Code's glyphs as underscores.
        assert!(
            script.contains("tmux -u "),
            "the client must be UTF-8: {script}"
        );
        assert!(script.contains("LANG=C.UTF-8"), "{script}");
        assert!(script.contains("new-session"), "a killed agent still opens");
        let (cols, rows) = session::SCRAPE_SIZE;
        assert!(
            script.contains(&format!("resize-window -t 'agent' -x {cols} -y {rows}")),
            "the window has to go back to a scrapeable width: {script}"
        );
        assert!(
            script.contains("window-size latest"),
            "or the next attach cannot resize it: {script}"
        );
        assert!(
            script.trim_end().ends_with("true"),
            "the tidy-up must not fail the attach: {script}"
        );
    }

    /// The capture is kept, because the pane that shows the agent is drawn from
    /// it -- and because it is already paid for by the status column.
    #[test]
    fn the_captured_screen_is_kept_for_the_pane() {
        let out = format!(
            "12 3 1\n{}\n{{\"state\":\"running\",\"at\":100,\"detail\":\"Edit\"}}\n{}\n\
             ● Read README.md\n❯ fix the typo\n  esc to interrupt\n\n\n",
            status::STATUS_MARKER,
            status::PANE_MARKER
        );
        let poll = parse_poll(&out, 100);
        let pane = poll.pane.expect("the screen");
        assert!(pane.contains("Read README.md"), "{pane}");
        assert!(
            !pane.ends_with('\n'),
            "the blank tail is trimmed, or it pushes the screen out of view"
        );
        assert_eq!(poll.status.map(|r| r.state), Some(session::State::Running));
    }

    /// A sandbox with no agent pane has no screen to show, which is different
    /// from an empty one.
    #[test]
    fn no_pane_means_none() {
        let out = format!(
            "0 0 0\n{}\n\n{}\n   \n\n",
            status::STATUS_MARKER,
            status::PANE_MARKER
        );
        assert!(parse_poll(&out, 100).pane.is_none());
    }

    fn session() -> Session {
        Session::new(
            "t".into(),
            "https://example.com/r.git".into(),
            "task".into(),
        )
    }

    /// The base ref has to be the *remote-tracking* branch. `origin/main` still
    /// points at the base after the agent commits to the work branch; a local
    /// `main` would be left behind by a `git switch -c`, and diffing against it
    /// would credit the agent with everything on the base branch.
    #[test]
    fn base_resolution_prefers_the_remote_tracking_ref() {
        let mut s = session();
        s.base_branch = Some("develop".into());
        let script = resolve_base(&s);
        assert!(script.contains("base='origin/develop'"), "{script}");

        // With no pinned base, the clone's origin/HEAD is the fallback.
        s.base_branch = None;
        let script = resolve_base(&s);
        assert!(script.contains("base=''"), "{script}");
        assert!(script.contains("refs/remotes/origin/HEAD"), "{script}");
    }

    #[test]
    fn base_resolution_quotes_a_hostile_branch_name() {
        let mut s = session();
        s.base_branch = Some("a'; rm -rf /; echo '".into());
        let script = resolve_base(&s);
        assert!(
            !script.contains("rm -rf /;\n") && script.contains(r"'\''"),
            "the branch name must stay inside one quoted word: {script}"
        );
    }

    /// The pane half of the poll output is arbitrary terminal text. If the
    /// sections were split positionally instead of by marker, a transcript
    /// containing something stat-shaped would be read as the stat.
    #[test]
    fn poll_output_is_split_by_marker_not_by_position() {
        let stdout = format!(
            "12 3 1\n{}\n{{\"state\":\"running\",\"at\":1000,\"detail\":\"Bash\"}}\n{}\n             99 99 99\n  esc to interrupt\n",
            status::STATUS_MARKER,
            status::PANE_MARKER,
        );
        let p = parse_poll(&stdout, 1010);

        assert_eq!(
            p.stat,
            Some(DiffStat {
                added: 12,
                removed: 3,
                untracked: 1
            }),
            "the stat-shaped line inside the pane must not win"
        );
        let status = p.status.expect("a status");
        assert_eq!(status.state, crate::session::State::Running);
        assert_eq!(status.detail.as_deref(), Some("Bash"));
    }

    #[test]
    fn a_poll_survives_every_part_being_missing() {
        // A sandbox with no repository, no status file and no agent.
        let empty = format!("{}\n\n{}\n", status::STATUS_MARKER, status::PANE_MARKER);
        let p = parse_poll(&empty, 1000);
        assert_eq!(p.stat, None);
        assert!(p.status.is_none());

        // Output that stopped before the first marker, e.g. an older sandbox.
        let p = parse_poll("5 0 0\n", 1000);
        assert_eq!(p.stat.map(|s| s.added), Some(5));
        assert!(p.status.is_none());

        assert_eq!(parse_poll("", 1000).stat, None);
    }

    /// The whole point of increment 6: a permission prompt on screen reports
    /// waiting even though the hook file says the agent is running.
    #[test]
    fn a_prompt_on_screen_reports_waiting() {
        let stdout = format!(
            "1 0 0\n{}\n{{\"state\":\"running\",\"at\":1000,\"detail\":\"Bash\"}}\n{}\n             Do you want to proceed?\n ❯ 1. Yes\n   2. No\n\n Esc to cancel\n",
            status::STATUS_MARKER,
            status::PANE_MARKER,
        );
        let status = parse_poll(&stdout, 1005).status.expect("a status");
        assert_eq!(status.state, crate::session::State::Waiting);
        assert_eq!(status.source, status::Source::Pane);
    }

    /// The poll script has to read the status file and the pane even when the
    /// repository is missing, or a half-seeded session never reports anything.
    #[test]
    fn the_poll_script_reads_status_outside_the_repository_subshell() {
        let script_has = |needle: &str| {
            let s = Session::new("t".into(), "url".into(), "task".into());
            // Rebuilt here rather than exposed, since only its shape matters.
            let script = poll_script(&s);
            assert!(script.contains(needle), "missing {needle} in:\n{script}");
        };
        script_has(STATUS_PATH);
        // With the escapes, because the pane that shows the screen wants the
        // colour, and `-u` so the capture is UTF-8 rather than mangled into
        // underscores.
        script_has("capture-pane -pe");
        script_has("tmux -u ");
        script_has(status::STATUS_MARKER);
        script_has(status::PANE_MARKER);
        // `cd` failing must not skip the status read, so it is confined to a
        // subshell rather than exiting the script.
        let s = Session::new("t".into(), "url".into(), "task".into());
        let script = poll_script(&s);
        let cd_line = script.lines().find(|l| l.contains("cd ")).unwrap();
        assert!(
            cd_line.trim_start().starts_with('('),
            "the cd must be inside a subshell: {cd_line}"
        );
    }

    /// The section and notice sigils are a contract between the fetch script
    /// and the renderer, which strips them. If they drift the pane shows raw
    /// markers.
    #[test]
    fn the_diff_script_emits_the_markers_the_renderer_strips() {
        assert_eq!(DIFF_SECTION, "### ");
        assert_eq!(DIFF_NOTICE, "!!! ");
    }
}
