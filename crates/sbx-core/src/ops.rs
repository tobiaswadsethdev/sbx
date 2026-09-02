//! Operations shared by the CLI and the TUI.

use std::time::{Duration, Instant};

use openshell_client::{CreateOpts, Error as OsError, OpenShell, PolicyRevision, PolicyUpdate};

use crate::endpoints;
use crate::events;
use crate::forge;
use crate::mcp;
use crate::policy;
use crate::publish;
use crate::seed;
use crate::session::{self, REPO_PATH, SELECTOR_MANAGED, STATUS_PATH, Session, State};
use crate::skills;
use crate::status;
use crate::store::{self, Store};
use crate::toolchain::{self, Toolchain};

/// How much of the agent's pane to capture.
///
/// Was forty, when this was only feeding marker detection and every marker sits
/// in the last few lines. The agent view draws the same capture, and forty lines
/// of a fifty-row pane cut the top off the transcript -- the banner and the first
/// exchange -- so it is now more than a window's worth. Still bounded: a pane
/// left tall by an attach from a big terminal cannot turn a poll into a flood.
const PANE_LINES: usize = 120;

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
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
            // Phrased as the sandbox's state rather than as a failure of this
            // code, since the usual cause is a create in flight in another
            // process and the next refresh adopts it.
            Err(e) => out.warnings.push(format!("{sandbox} {e}")),
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

/// One entry in a create form's policy chooser.
///
/// A [`policy::Template`] flattened, plus the one case a template cannot cover:
/// a config file may name a YAML path instead, and that has to be offered or a
/// client would quietly create sessions under a different policy from
/// `sbx new`.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PolicyChoice {
    /// What goes into the draft: a template name, or a path.
    pub spec: String,
    pub summary: String,
}

/// One toolchain a session's image can carry.
///
/// Sent by name rather than as a [`Toolchain`], which holds a Dockerfile
/// fragment and a registry list that no form has any use for.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolchainChoice {
    pub name: String,
    pub summary: String,
}

/// One credential provider the gateway knows about.
///
/// Flattened for the same reason [`policy::View`] exists: `openshell_client`'s
/// own types belong to a `0.0.x` project, and putting them on the wire would
/// make their churn protocol churn.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProviderChoice {
    pub name: String,
    /// The profile type, which is what says what a provider is *for*. Names are
    /// chosen by whoever created them and several may share a type.
    pub kind: String,
}

/// Everything a create form needs that is not about a repository.
///
/// One request rather than four, because a form cannot be drawn until it has all
/// of them and four round trips to a server that may be a continent away is four
/// chances to show a half-built form.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NewOptions {
    pub policies: Vec<PolicyChoice>,
    /// Every toolchain, not only the ones a checkout points at. The list is
    /// three lines long, and one that hid `dotnet` because there is no `.csproj`
    /// yet would be a form you cannot use to start writing one.
    pub toolchains: Vec<ToolchainChoice>,
    pub providers: Vec<ProviderChoice>,
    /// Why the provider list is empty, when it is. An unreachable gateway is a
    /// fact about the server worth showing beside the field rather than a list
    /// that is simply blank.
    pub providers_error: Option<String>,
    /// The policy `sbx new` would use here, so a client can preselect what the
    /// command line would have chosen.
    pub default_policy: String,
    /// From the config file. Replaces a client's own guesswork rather than
    /// adding to it, for the reason [`Draft::providers`] gives.
    pub default_providers: Vec<String>,
    /// Only consulted for a checkout on a detached HEAD: the branch a repository
    /// is sitting on is evidence about *that* repository, and a config entry is
    /// a guess about all of them.
    pub default_base: Option<String>,
    /// Named, not chosen. Skills and MCP servers are one decision about what
    /// your agents can reach, made in the config file; a form shows them so a
    /// session's tools are not a surprise, and has nothing to offer about them.
    pub skills: Vec<String>,
    pub mcp: Vec<String>,
}

/// The templates, with a configured YAML path in front of them when there is
/// one.
///
/// In front rather than instead: a config file naming a path is saying what a
/// session should get by default, and a form that then offered only the
/// built-in templates would quietly create sessions under a different policy
/// from `sbx new`.
pub fn policy_choices(configured: &str) -> Vec<PolicyChoice> {
    let mut out = Vec::new();
    if policy::find(configured).is_none() {
        out.push(PolicyChoice {
            spec: configured.to_string(),
            summary: "from your config file".to_string(),
        });
    }
    out.extend(policy::TEMPLATES.iter().map(|t| PolicyChoice {
        spec: t.name.to_string(),
        summary: t.summary.to_string(),
    }));
    out
}

/// Every toolchain there is.
pub fn toolchain_choices() -> Vec<ToolchainChoice> {
    toolchain::TOOLCHAINS
        .iter()
        .map(|t| ToolchainChoice {
            name: t.name.to_string(),
            summary: t.summary.to_string(),
        })
        .collect()
}

/// Build [`NewOptions`] from the config file and the gateway.
///
/// The provider list is the only part that can fail, and it fails softly: a
/// gateway that cannot be reached leaves the field empty with a reason attached,
/// rather than refusing to open a form whose other five fields are fine.
pub fn new_options(client: &dyn OpenShell, cfg: &crate::config::Config) -> NewOptions {
    let configured = cfg.policy();
    let (providers, providers_error) = match client.providers() {
        Ok(list) => (
            list.into_iter()
                .map(|p| ProviderChoice {
                    name: p.name,
                    kind: p.kind,
                })
                .collect(),
            None,
        ),
        Err(e) => (Vec::new(), Some(e.to_string())),
    };

    NewOptions {
        policies: policy_choices(configured),
        toolchains: toolchain_choices(),
        providers,
        providers_error,
        default_policy: configured.to_string(),
        default_providers: cfg.providers().to_vec(),
        default_base: cfg.base.clone(),
        skills: cfg.skills().iter().map(|s| s.name.clone()).collect(),
        mcp: cfg.mcp().iter().map(|m| m.name.clone()).collect(),
    }
}

/// What is known about the repository a client has picked.
///
/// The git facts, and the credentials to tick. Both are answers about *this*
/// repository and both cost something to work out -- subprocesses for one, the
/// gateway and the session cache for the other -- so they are asked for
/// together, once, about the repository actually picked.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Picked {
    pub facts: crate::repos::Facts,
    /// Provider names a new session here should start with ticked. Empty when
    /// the config file names providers, since an explicit list replaces this
    /// rather than adding to it.
    pub providers: Vec<String>,
}

/// Provider names the most recent session for this remote's host and
/// organisation was given.
///
/// A record of what worked, not a guess: it can only be wrong where the person
/// was wrong.
pub fn providers_used_for(origin: &str, sessions: &[Session]) -> Vec<String> {
    let key = |url: &str| {
        crate::forge::Remote::parse(url)
            .ok()
            .map(|r| (r.host, r.org))
    };
    let Some(wanted) = key(origin) else {
        return Vec::new();
    };
    sessions
        .iter()
        .filter(|s| key(&s.repo).is_some_and(|k| k == wanted))
        .filter(|s| !s.providers.is_empty())
        // Newest first: the last thing that worked is the best evidence.
        .max_by_key(|s| s.created_at)
        .map(|s| s.providers.clone())
        .unwrap_or_default()
}

/// Which credentials a new session for this repository should start with.
///
/// Two rules, in order. A provider is the obvious choice when it is the only
/// one of the type that is wanted -- the agent's, and the repository host's --
/// since a session without the agent's credential comes up to a login prompt
/// and one without the host's cannot clone a private repository.
///
/// When there are several of a type, the type alone cannot say which: two Azure
/// PATs are two organisations, and the wrong one fails three steps later. That
/// used to mean nothing was ticked, which is just as wrong for the common case
/// -- the answer is almost always the one used last time. So `used_before`
/// breaks the tie.
///
/// Here rather than in either front end because both need it and neither may
/// answer it differently: a session created from the window and one created
/// from the terminal have to arrive with the same credentials.
pub fn preselect_providers(
    providers: &[ProviderChoice],
    origin: Option<&str>,
    used_before: &[String],
) -> Vec<String> {
    let agent = session::agent_provider_type("claude");
    let forge = origin
        .and_then(|url| crate::forge::Remote::parse(url).ok())
        .map(|r| r.forge.provider_profile());

    let unique = |kind: &str| providers.iter().filter(|p| p.kind == kind).count() == 1;

    providers
        .iter()
        .filter(|p| {
            [agent, forge]
                .into_iter()
                .flatten()
                .any(|kind| kind == p.kind)
                && (unique(&p.kind) || used_before.contains(&p.name))
        })
        .map(|p| p.name.clone())
        .collect()
}

/// A session a client is asking for.
///
/// The wire twin of [`Draft`], and deliberately not [`Draft`] itself: a draft
/// carries resolved `&'static Toolchain`s and the config file's skills and MCP
/// servers, and none of those are a client's to send. Skills and MCP in
/// particular are read from the *server's* config when this is turned into a
/// draft, so a client cannot attach a tool by asking for one.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NewSession {
    /// `None` to have one derived from the task, which is what `sbx new`
    /// without `--name` does. Derived here rather than in a client, because a
    /// second implementation of the slug rule is a second answer to what a
    /// session is called.
    pub name: Option<String>,
    pub repo: String,
    pub task: String,
    pub base: Option<String>,
    pub policy: String,
    pub providers: Vec<String>,
    /// Toolchain names. Resolved against the server's own list, so an unknown
    /// one fails here rather than as a docker tag nothing has ever built.
    pub toolchains: Vec<String>,
    pub start: bool,
}

impl NewSession {
    /// Turn a request into something [`create`] will accept.
    ///
    /// The only place a client's message becomes a draft, which is why the
    /// fields it may not set are filled from `cfg` here rather than trusted.
    pub fn into_draft(self, cfg: &crate::config::Config) -> Result<Draft, String> {
        let name = match self
            .name
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
        {
            Some(n) => n,
            None => session::derive_name(&self.task, &self.repo)
                .ok_or("could not work out a name for this session; give it one")?,
        };
        session::validate_name(&name).map_err(|e| e.to_string())?;
        Ok(Draft {
            name,
            repo: self.repo,
            task: self.task,
            base: self.base.or_else(|| cfg.base.clone()),
            policy: self.policy,
            providers: self.providers,
            mcp: cfg.mcp().to_vec(),
            skills: cfg.skills().to_vec(),
            toolchains: toolchain::resolve(&self.toolchains).map_err(|e| e.to_string())?,
            start: self.start,
        })
    }
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
    /// Skills copied into the sandbox, resolved from the config file. Same
    /// reasoning as `mcp`: global, and on the draft so both front ends describe
    /// a session the same way.
    pub skills: Vec<skills::Skill>,
    /// MCP servers the agent is given, from the config file rather than from a
    /// per-session choice. Carried on the draft anyway, so both front ends hand
    /// [`create`] one complete description of the session and neither can
    /// create one this module then quietly changes.
    pub mcp: Vec<mcp::Server>,
    /// Toolchains the sandbox image carries, already resolved.
    ///
    /// Per-session, unlike `mcp` and `skills`: which toolchain a task needs is a
    /// fact about the repository, and a session that does not need the .NET SDK
    /// should not be carrying it. Resolved before it reaches here, so an unknown
    /// name fails against a command line rather than against docker.
    pub toolchains: Vec<&'static Toolchain>,
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
    /// The seeder names its steps (`clone`, `branch`, `meta`, `skills`, `mcp`,
    /// `agent`) and this maps them onto what the interface already shows.
    /// Everything between the clone and the agent is part of cloning as far as
    /// anyone watching is
    /// concerned: each is a fraction of a second, and a stage that flashes past
    /// is noise rather than progress.
    pub fn for_seed(step: &str) -> Option<Step> {
        match step {
            "clone" | "branch" | "meta" | "skills" | "mcp" => Some(Step::Clone),
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

/// Apply the global allow and block lists to a sandbox that has just been made.
///
/// A failed *block* fails the create. The two directions are not symmetric and
/// pretending they are would be the worst kind of bug this tool can have: an
/// allow that did not land leaves a session that cannot reach something, which
/// the events pane will say out loud the moment the agent tries; a block that
/// did not land leaves a session that *can* reach something the user asked to be
/// unreachable, and nothing will ever mention it again. So the first is a
/// warning and the second is fatal.
///
/// Costs one `policy update --wait` -- about six seconds -- and only when the
/// lists are not empty, which is the common case for anyone who has never
/// touched them.
fn impose_lists(
    client: &dyn OpenShell,
    sandbox: &str,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    let lists = match endpoints::Lists::load() {
        Ok(l) => l,
        // An unreadable list is not a reason to refuse to create a session, but
        // it is a reason to say so: the session will not have the rules its
        // owner thinks every session has.
        Err(e) => {
            warnings.push(format!(
                "could not read the global endpoint lists, so none were applied: {e}"
            ));
            return Ok(());
        }
    };
    let updates = lists.updates();
    if updates.is_empty() {
        return Ok(());
    }

    for update in &updates {
        let Err(e) = client.policy_update(sandbox, update) else {
            continue;
        };
        if !update.remove_endpoints.is_empty() {
            return Err(format!(
                "the global block list could not be applied, so {} would have been reachable: {e}",
                update.remove_endpoints.join(", ")
            ));
        }
        warnings.push(format!(
            "the global allow list could not be applied, so {} is not reachable: {e}",
            update.add_endpoints.join(", ")
        ));
    }
    Ok(())
}

/// Open the endpoints of the session's MCP servers.
///
/// Separate from [`impose_lists`] and after it, because the two answer different
/// questions -- that one is "what have I decided every session may reach", this
/// one is "what tools does the agent have" -- and because a failure here means
/// something different: an MCP server that could not be opened leaves a session
/// whose agent starts, works, and reports a dead tool. Worth a warning, not
/// worth refusing to create the session over, which is the same reading as a
/// failed allow.
fn impose_mcp(
    client: &dyn OpenShell,
    sandbox: &str,
    servers: &[mcp::Server],
    warnings: &mut Vec<String>,
) {
    let Some(update) = mcp::widen(servers) else {
        return;
    };
    if let Err(e) = client.policy_update(sandbox, &update) {
        warnings.push(format!(
            "the mcp endpoints could not be opened, so the agent will report {} unreachable: {e}",
            servers
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
}

/// Open the package registries the session's toolchains need.
///
/// A warning rather than a failure, on the same reading as [`impose_mcp`]: a
/// registry that could not be opened leaves a session whose agent builds against
/// what is already vendored and reports a denial the moment it restores. The
/// events pane says so out loud, which is the test for whether a failure is worth
/// refusing to create a session over -- a *block* that does not land is silent,
/// and that is the one that is fatal.
///
/// Costs one `policy update --wait` per distinct binary list -- about six seconds
/// each, and at most one per toolchain. Only for a session that asked for one.
fn impose_toolchains(
    client: &dyn OpenShell,
    sandbox: &str,
    chains: &[&'static Toolchain],
    warnings: &mut Vec<String>,
) {
    for update in toolchain::updates(chains) {
        if let Err(e) = client.policy_update(sandbox, &update) {
            warnings.push(format!(
                "the toolchain registries could not be opened, so {} is not \
                 reachable and a restore will be denied: {e}",
                update.add_endpoints.join(", ")
            ));
        }
    }
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
/// [`crate::image::ensure_for`] before this, and the TUI refuses to create until
/// the image the draft's toolchains name is there. See the doc comment on
/// [`crate::image::ensure`].
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
    s.mcp = draft.mcp.clone();
    s.skills = draft.skills.clone();
    s.toolchains = toolchain::labels(&draft.toolchains);

    progress(Step::Sandbox);
    let opts = CreateOpts {
        name: s.sandbox.clone(),
        labels: s.labels(),
        policy: Some(resolved.path().to_path_buf()),
        providers: draft.providers.clone(),
        // The base image for a session with no toolchain, and the variant
        // carrying exactly the ones asked for otherwise. Not built here, for the
        // reason the base image is not: see the note above about docker's output.
        from: Some(toolchain::tag(&draft.toolchains)),
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

    // The record is written the moment the sandbox exists, and before the policy
    // calls below, because between those two points the sandbox is an *orphan*:
    // labelled `sbx.managed`, with no record and no `meta.json` yet. A refresh
    // landing in that window -- the TUI runs one a second, and a `sbx new` in
    // another terminal is the normal case -- tries to adopt it, and reports
    // `could not adopt sbx-x: cat: /sandbox/.sbx/meta.json: No such file or
    // directory` about a session that is being created perfectly well.
    //
    // The window was always here and used to be microseconds; imposing MCP
    // endpoints made it a `policy update --wait`, which is seconds. Saving first
    // closes it: a record in `creating` is one the repair pass knows to leave
    // alone until the seeder has something to say.
    save(s.clone(), &mut warnings);

    // The global lists, imposed before anything runs inside the sandbox.
    //
    // Here rather than by editing the policy before `sandbox create`, because
    // `--policy` may be the user's own YAML file and this has to work whatever
    // shape it is in. The window between the sandbox existing and the rules
    // landing is real, and it is empty: nothing is launched in it until the
    // seeder below.
    if let Err(e) = impose_lists(client, &s.sandbox, &mut warnings) {
        s.state = State::Failed;
        save(s, &mut warnings);
        return Err(e);
    }
    impose_mcp(client, &s.sandbox, &s.mcp, &mut warnings);
    impose_toolchains(client, &s.sandbox, &draft.toolchains, &mut warnings);

    // The seeder packs the skills itself; this is the same pack, thrown away,
    // for its warnings. A skill that cannot be read is worth saying out loud
    // here -- the seeder runs detached and has nowhere to say it, and a session
    // silently missing a skill looks like the agent forgetting how to do
    // something it used to know.
    warnings.extend(skills::pack(&s.skills).1);

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
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

/// Say something to the agent, as if it had been pasted into its terminal.
///
/// Through tmux's paste buffer rather than `send-keys`, and the difference
/// matters for anything with a newline in it. `send-keys` types a message a key
/// at a time, so a review of six comments arrives as six submissions -- the
/// agent starts on the first line while the rest is still being typed at it.
/// `load-buffer` then `paste-buffer -p` sends the whole thing as one bracketed
/// paste, which is what a terminal agent is built to receive: it treats the
/// bracketed run as text rather than as input, however many newlines are in it.
/// The `Enter` afterwards is the submission, once, on purpose.
///
/// `-d` deletes the buffer after pasting, so a review does not sit in the
/// sandbox's tmux buffer stack after it has been delivered.
pub fn tell(client: &dyn OpenShell, session: &Session, message: &str) -> Result<(), String> {
    if message.trim().is_empty() {
        return Err("nothing to say".into());
    }
    let script = tell_script(&session.tmux, message);
    match client.exec(&session.sandbox, &["sh", "-c", &script]) {
        Ok(out) if out.ok() => Ok(()),
        Ok(out) => Err(format!(
            "the agent could not be told: {}",
            out.stderr.trim()
        )),
        Err(e) => Err(format!("sandbox unreachable: {e}")),
    }
}

/// The shell that delivers one message. Separated so its shape can be asserted
/// without a sandbox: what makes this correct is invisible at the call site.
fn tell_script(tmux: &str, message: &str) -> String {
    format!(
        "printf '%s' {message} | tmux -u load-buffer -b sbx-tell - \
         && tmux -u paste-buffer -b sbx-tell -t {tmux} -d -p \
         && tmux -u send-keys -t {tmux} Enter",
        message = seed::sh_quote(message),
        tmux = seed::sh_quote(tmux),
    )
}

/// Send a session's unsent review to its agent, and forget it.
///
/// Cleared only once the paste has landed: a review that failed to arrive is
/// still a review, and losing it to a sandbox that was briefly unreachable
/// would be losing the work rather than the delivery.
pub fn send_comments(client: &dyn OpenShell, session: &Session) -> Result<String, String> {
    let review = crate::comments::list(&session.name);
    if review.is_empty() {
        return Err("there are no comments to send".into());
    }
    let message = crate::comments::message(&review);
    tell(client, session, &message)?;
    crate::comments::clear(&session.name)?;
    Ok(message)
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
// `PartialEq` so a stream can tell a poll that changed from one that did not,
// which is the difference between a frame and silence.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
        pane: (!plain.trim().is_empty()).then_some(pane_part.trim_end().to_string()),
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

    /// A blank name is the normal case from a form, and the rule that fills it
    /// has to be the same one `sbx new` uses -- which is why it lives here and
    /// not in a client.
    #[test]
    fn a_new_session_with_no_name_is_named_from_its_task() {
        let cfg = crate::config::Config::default();
        let draft = NewSession {
            name: None,
            repo: "https://github.com/o/thing.git".into(),
            task: "Fix the flaky login test".into(),
            ..Default::default()
        }
        .into_draft(&cfg)
        .expect("a name should have been derived");
        assert_eq!(
            draft.name,
            session::derive_name("Fix the flaky login test", "").unwrap()
        );
    }

    /// A name of nothing but spaces is not a name. Trimmed to empty and then
    /// derived, rather than accepted and rejected later by `validate_name`.
    #[test]
    fn a_whitespace_name_falls_back_to_the_task() {
        let cfg = crate::config::Config::default();
        let draft = NewSession {
            name: Some("   ".into()),
            repo: "https://github.com/o/thing.git".into(),
            task: "tidy the docs".into(),
            ..Default::default()
        }
        .into_draft(&cfg)
        .expect("a name should have been derived");
        // Compared against the rule rather than against a slug spelled out
        // here: `derive_name` drops stop words, and a literal would be a second
        // copy of that decision, wrong the next time it changes.
        assert_eq!(
            draft.name,
            session::derive_name("tidy the docs", "").unwrap()
        );
    }

    /// With no task to slug, the repository names the session -- and with
    /// neither, the client is told rather than handed a session called nothing.
    #[test]
    fn with_no_task_the_repository_names_it_and_with_neither_it_refuses() {
        let cfg = crate::config::Config::default();
        let draft = NewSession {
            name: None,
            repo: "https://github.com/o/thing.git".into(),
            task: String::new(),
            ..Default::default()
        }
        .into_draft(&cfg)
        .expect("the repository should have named it");
        assert_eq!(draft.name, "thing");

        let err = NewSession::default().into_draft(&cfg).unwrap_err();
        assert!(err.contains("name"), "{err}");
    }

    /// The name is validated where it is made, so a client cannot get a session
    /// called `../..` past the form by sending one.
    #[test]
    fn a_name_that_is_not_a_name_is_refused() {
        let cfg = crate::config::Config::default();
        let err = NewSession {
            name: Some("../../etc".into()),
            repo: "https://github.com/o/thing.git".into(),
            ..Default::default()
        }
        .into_draft(&cfg)
        .unwrap_err();
        assert!(!err.is_empty());
    }

    /// An unknown toolchain fails against the request that named it, rather
    /// than later as a docker tag nothing has ever built.
    #[test]
    fn an_unknown_toolchain_is_refused_by_name() {
        let cfg = crate::config::Config::default();
        let err = NewSession {
            name: Some("x".into()),
            repo: "https://github.com/o/thing.git".into(),
            toolchains: vec!["cobol".into()],
            ..Default::default()
        }
        .into_draft(&cfg)
        .unwrap_err();
        assert!(err.contains("cobol"), "{err}");
    }

    /// Skills and MCP servers are read from the server's config, never from the
    /// request. A client that could send them could attach a tool -- and an
    /// endpoint the policy then opens -- that nobody configured.
    #[test]
    fn skills_and_mcp_come_from_the_config_and_not_from_the_request() {
        let cfg = crate::config::Config {
            mcp: vec![mcp::Server {
                name: "jira".into(),
                url: "http://mcp:9000/mcp".into(),
                transport: mcp::Transport::default(),
                endpoint: "mcp:9000".into(),
            }],
            ..Default::default()
        };
        let draft = NewSession {
            name: Some("x".into()),
            repo: "https://github.com/o/thing.git".into(),
            ..Default::default()
        }
        .into_draft(&cfg)
        .unwrap();
        assert_eq!(draft.mcp.len(), 1);
        assert_eq!(draft.mcp[0].name, "jira");
    }

    /// A YAML path from the config file is offered as well as the templates,
    /// and first, because it is what `sbx new` would have used.
    #[test]
    fn a_configured_policy_path_is_offered_ahead_of_the_templates() {
        let choices = policy_choices("/etc/sbx/mine.yaml");
        assert_eq!(choices[0].spec, "/etc/sbx/mine.yaml");
        assert_eq!(choices.len(), policy::TEMPLATES.len() + 1);

        // A configured *template* is already in the list, so it is not repeated.
        let named = policy_choices(policy::TEMPLATES[0].name);
        assert_eq!(named.len(), policy::TEMPLATES.len());
    }

    /// Every toolchain is offered, not only the ones a checkout points at: a
    /// form that hid `dotnet` because there is no `.csproj` yet would be one you
    /// cannot use to start writing one.
    #[test]
    fn every_toolchain_is_offered() {
        assert_eq!(toolchain_choices().len(), toolchain::TOOLCHAINS.len());
    }

    /// A review is one paste, not one submission per line. `paste-buffer -p`
    /// brackets it, which is what stops an agent acting on the first comment
    /// while the rest is still arriving; the single `Enter` is the submission.
    #[test]
    fn a_multi_line_message_is_one_bracketed_paste_and_one_enter() {
        let script = tell_script("agent", "first line\nsecond line");
        assert!(script.contains("load-buffer -b sbx-tell -"), "{script}");
        assert!(
            script.contains("paste-buffer -b sbx-tell -t 'agent' -d -p"),
            "{script}"
        );
        assert_eq!(script.matches("send-keys").count(), 1, "{script}");
        assert!(script.contains("Enter"), "{script}");
    }

    /// A comment is free text and will contain quotes. It has to reach the
    /// agent as text rather than as shell.
    #[test]
    fn a_message_with_quotes_in_it_cannot_break_out_of_the_script() {
        let script = tell_script("agent", "it's `wrong`; rm -rf / #");
        // The dangerous run is inside a quoted literal, not sitting in the
        // command position where the shell would act on it.
        assert!(!script.contains("; rm -rf / #'\n"), "{script}");
        assert!(
            script.contains(r"'\''"),
            "the apostrophe was not escaped: {script}"
        );
    }
}
