//! `sbx` - run several coding agents in parallel, each in its own sandbox.
//!
//! The CLI and the TUI, and nothing else: everything they do is
//! [`sbx_core`]'s, so neither this nor the terminal interface is where a
//! behaviour lives. What is left here is argument parsing, printing, and
//! [`attach`] -- the one piece that is genuinely about the terminal this
//! process was started in.

mod attach;
mod remote;
mod tui;

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use openshell_client::{CliClient, OpenShell};
use sbx_core::{
    config, doctor, endpoints, events, forge, image, ops, pane, policy, publish, repos, session,
    store, toolchain, update,
};

use config::Config;
use session::{Session, State};
use store::Store;

#[derive(Parser)]
#[command(
    name = "sbx",
    version,
    about = "Parallel coding agents in OpenShell sandboxes"
)]
struct Cli {
    /// Gateway name to operate on (defaults to the active one).
    #[arg(long, global = true)]
    gateway: Option<String>,

    /// Ask a paired `sbxd` instead of this machine's own sandboxes.
    ///
    /// `--server` alone when one server is paired, `--server=<name>` otherwise;
    /// `sbx remotes` lists them. The name has to be attached with `=`, because
    /// a detached value cannot be told apart from the subcommand -- `sbx
    /// --server ls` would otherwise mean a server called `ls`.
    ///
    /// Reading commands work over a connection. The ones that need this
    /// terminal, or that change a session, do not yet.
    #[arg(
        long,
        global = true,
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "",
        value_name = "NAME"
    )]
    server: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Check that everything sbx depends on is present and working.
    Doctor,

    /// Start a session: create a sandbox, clone the repo, cut a work branch.
    New(NewArgs),

    /// List sessions, reconciled against the gateway.
    #[command(alias = "list")]
    Ls,

    /// Attach to a session's agent. Detach with Ctrl-b d.
    Attach {
        /// Session name.
        name: String,
    },

    /// Print a session's diff against the branch it started from.
    Diff {
        /// Session name.
        name: String,
    },

    /// Print the policy the gateway is enforcing for a session.
    Policy {
        /// Session name.
        name: String,
    },

    /// Print a session's recent allow/deny decisions, newest first.
    Events {
        /// Session name.
        name: String,
    },

    /// Pair with an `sbxd`, using the string `sbxd pair` printed.
    Connect {
        /// `sbx://host:port/<token>#<fingerprint>`.
        pairing: String,

        /// What to call it. Defaults to the server's host.
        #[arg(long)]
        name: Option<String>,
    },

    /// The servers this machine is paired with.
    Remotes {
        /// Forget one, by name.
        #[arg(long)]
        forget: Option<String>,
    },

    /// List the policy templates shipped with this binary.
    Policies,

    /// List the toolchains a sandbox image can be built with.
    Toolchains,

    /// Show the defaults read from the config file, and where they came from.
    Config {
        /// Write a commented starter file. Refuses to overwrite one.
        #[arg(long)]
        init: bool,

        /// Print the file's path and nothing else.
        #[arg(long)]
        path: bool,
    },

    /// Push a session's branch and open a pull request.
    Publish {
        /// Session name.
        name: String,

        /// Pull request title. Defaults to the session's task.
        #[arg(long)]
        title: Option<String>,

        /// Pull request description.
        #[arg(long)]
        body: Option<String>,

        /// Branch to merge into. Defaults to the remote's default branch.
        #[arg(long)]
        target: Option<String>,

        /// Push the branch but do not open a pull request.
        #[arg(long)]
        no_pr: bool,

        /// Open the pull request as a draft.
        #[arg(long)]
        draft: bool,
    },

    /// Manage the sandbox image.
    Image {
        #[command(subcommand)]
        action: ImageAction,
    },

    /// Update sbx itself to the newest published release.
    #[command(alias = "self-update")]
    Update {
        /// Say what an update would do, and do none of it.
        #[arg(long)]
        check: bool,

        /// Install one named release, like `v0.1.0`. Defaults to the newest.
        #[arg(long)]
        tag: Option<String>,

        /// Install even when the newest release is what is already running.
        #[arg(long)]
        force: bool,
    },

    /// Delete sessions and their sandboxes.
    #[command(alias = "delete")]
    Rm {
        /// Session names to remove.
        #[arg(required = true)]
        names: Vec<String>,
    },
}

#[derive(clap::Args)]
struct NewArgs {
    /// Repository to clone inside the sandbox. Defaults to `repo` in the
    /// config file.
    #[arg(long)]
    repo: Option<String>,

    /// Session name. Derived from the task when omitted.
    #[arg(long)]
    name: Option<String>,

    /// What the agent should do. Becomes the agent's opening prompt.
    #[arg(long, default_value = "")]
    task: String,

    /// Branch to clone from. Defaults to `base` in the config file, else the
    /// remote's default branch.
    #[arg(long)]
    base: Option<String>,

    /// Policy to apply: a template name, or a path to a YAML file.
    ///
    /// Defaults to `policy` in the config file, else `feature-work`. See
    /// `sbx policies` for the templates; a spec containing a `/` or ending in
    /// `.yaml` is always read as a path.
    #[arg(long)]
    policy: Option<String>,

    /// Credential provider to attach. Repeatable. Any given here replace
    /// `providers` in the config file rather than adding to them.
    #[arg(long = "provider")]
    providers: Vec<String>,

    /// Toolchain the sandbox should carry. Repeatable, or comma-separated.
    ///
    /// Each set of toolchains is its own image variant, layered onto the base
    /// image and built on first use; `sbx toolchains` lists them. A toolchain
    /// also opens its package registry for the binary that fetches from it, and
    /// for nothing else in the sandbox.
    #[arg(long = "toolchain", value_delimiter = ',')]
    toolchains: Vec<String>,

    /// Create the sandbox and clone, but do not start the agent.
    #[arg(long)]
    no_start: bool,
}

#[derive(Subcommand)]
enum ImageAction {
    /// Build the sandbox image (community base plus tmux).
    Build {
        /// Toolchain to layer in. Repeatable, or comma-separated.
        ///
        /// Builds `sbx-base:<toolchains>` on top of the base image, building the
        /// base first if it is missing. Without this, the base image itself is
        /// built, which is what a session with no toolchain runs.
        #[arg(long = "toolchain", value_delimiter = ',')]
        toolchains: Vec<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    // Before anything else, because a file that cannot be read has to stop the
    // command rather than be quietly replaced by the built-in defaults -- the
    // one exception being `doctor`, whose job is to say what is wrong.
    let loaded = Config::load();
    let cfg = match (&loaded, &cli.command) {
        (Ok(c), _) => c.clone(),
        (Err(_), Some(Command::Doctor)) => Config::default(),
        (Err(e), _) => {
            eprintln!("sbx: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut client = CliClient::new();
    // The flag first, the file second: a gateway named on the command line is
    // about this command, and the file is about every other one.
    if let Some(g) = cli.gateway.clone().or_else(|| cfg.gateway.clone()) {
        client = client.with_gateway(g);
    }

    // Read out before the match, which moves `cli.command`.
    let chosen = cli.server.clone();

    let result = match cli.command {
        // No subcommand: the TUI is the point of the tool.
        None => tui::run(client, cfg),
        Some(Command::Doctor) => {
            let mut checks = doctor::run(&client, &loaded);
            // Appended here rather than inside `doctor::run`, because both need
            // things the core does not have: the protocol's port, and a client
            // for it.
            checks.extend(doctor::check_wsl(sbx_proto::DEFAULT_PORT));
            checks.extend(remote::checks());
            return match doctor::report(&checks) {
                0 => ExitCode::SUCCESS,
                _ => ExitCode::FAILURE,
            };
        }
        Some(Command::Config { init, path }) => cmd_config(&cfg, init, path),
        Some(Command::New(args)) => cmd_new(&client, args, &cfg),
        Some(Command::Ls) => match server(chosen.as_deref()) {
            Ok(Some(r)) => remote_ls(&r),
            Ok(None) => cmd_ls(&client),
            Err(e) => Err(e),
        },
        Some(Command::Attach { name }) => cmd_attach(&client, &name),
        Some(Command::Diff { name }) => match server(chosen.as_deref()) {
            Ok(Some(r)) => remote_diff(&r, &name),
            Ok(None) => cmd_diff(&client, &name),
            Err(e) => Err(e),
        },
        Some(Command::Policy { name }) => match server(chosen.as_deref()) {
            Ok(Some(r)) => remote_policy(&r, &name),
            Ok(None) => cmd_policy(&client, &name),
            Err(e) => Err(e),
        },
        Some(Command::Events { name }) => match server(chosen.as_deref()) {
            Ok(Some(r)) => remote_events(&r, &name),
            Ok(None) => cmd_events(&client, &name),
            Err(e) => Err(e),
        },
        Some(Command::Connect { pairing, name }) => cmd_connect(&pairing, name.as_deref()),
        Some(Command::Remotes { forget }) => cmd_remotes(forget.as_deref()),
        Some(Command::Policies) => {
            println!("{}", policy::help());
            return ExitCode::SUCCESS;
        }
        Some(Command::Toolchains) => {
            println!("{}", toolchain::help());
            return ExitCode::SUCCESS;
        }
        Some(Command::Publish {
            name,
            title,
            body,
            target,
            no_pr,
            draft,
        }) => cmd_publish(
            &client,
            &name,
            publish::Options {
                title,
                body,
                target,
                no_pr,
                draft,
            },
        ),
        Some(Command::Image { action }) => match action {
            ImageAction::Build { toolchains } => match toolchain::resolve(&toolchains) {
                Ok(chains) => image::build_variant(&chains).map_err(Into::into),
                Err(e) => Err(e.into()),
            },
        },
        Some(Command::Update { check, tag, force }) => cmd_update(check, tag.as_deref(), force),
        Some(Command::Rm { names }) => cmd_rm(&client, names),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sbx: {e}");
            ExitCode::FAILURE
        }
    }
}

type Fallible = Result<(), Box<dyn std::error::Error>>;

/// The server a command should talk to, if any.
///
/// `None` is the ordinary case: this machine's own sandboxes, through the
/// gateway, exactly as before. The flag is what turns `sbx` into a client of
/// something else -- which is also the second implementation of the protocol,
/// and the reason it exists before there is a user interface.
fn server(flag: Option<&str>) -> Result<Option<remote::Remote>, Box<dyn std::error::Error>> {
    let Some(name) = flag else {
        return Ok(None);
    };
    // `--server` with nothing after it means "the paired one", which is what a
    // machine with a single server almost always wants to say.
    let wanted = (!name.is_empty()).then_some(name);
    let remotes = remote::Remotes::load()?;
    Ok(Some(remotes.select(wanted)?.clone()))
}

fn cmd_connect(pairing: &str, name: Option<&str>) -> Fallible {
    let pairing: sbx_proto::Pairing = pairing.parse()?;
    let name = name.unwrap_or(&pairing.host).to_string();
    let candidate = remote::Remote::from_pairing(&name, pairing);

    // Tried before it is saved, so a mistyped address or a fingerprint from the
    // wrong server is an error now rather than on every command afterwards.
    let hello = candidate.hello()?;
    if !hello.is_sbxd() {
        return Err(format!(
            "`{}` is a {}, not an sbxd",
            candidate.address(),
            hello.server
        )
        .into());
    }
    if !hello.speaks(sbx_proto::VERSION) {
        return Err(format!(
            "`{}` speaks protocol {} and this sbx speaks {}. Update whichever is older.",
            candidate.address(),
            hello.protocol,
            sbx_proto::VERSION
        )
        .into());
    }

    let mut remotes = remote::Remotes::load()?;
    remotes.insert(candidate.clone());
    remotes.save()?;

    println!(
        "paired with `{}` at {} (sbxd {})",
        name,
        candidate.address(),
        hello.version
    );
    println!("try: sbx --server {name} ls");
    Ok(())
}

fn cmd_remotes(forget: Option<&str>) -> Fallible {
    let mut remotes = remote::Remotes::load()?;

    if let Some(name) = forget {
        if !remotes.remove(name) {
            return Err(format!("no server named `{name}`").into());
        }
        remotes.save()?;
        println!("forgot `{name}`");
        return Ok(());
    }

    if remotes.is_empty() {
        println!("no servers paired. run `sbxd pair` on one, then `sbx connect <string>`");
        return Ok(());
    }
    println!("{:<20} {:<28} CERTIFICATE", "NAME", "ADDRESS");
    for r in remotes.list() {
        println!(
            "{:<20} {:<28} {}",
            r.name,
            r.address(),
            &r.fingerprint[..16]
        );
    }
    Ok(())
}

/// The same four commands, asked of a server instead of the gateway.
///
/// Each one prints through the same helper the local path does, so the two
/// cannot drift into showing the same session differently.
fn remote_ls(remote: &remote::Remote) -> Fallible {
    let sbx_proto::Reply::Ls {
        sessions,
        adopted,
        dead,
        warnings,
    } = remote.call(sbx_proto::Request::Ls)?
    else {
        return Err("the server answered something other than a session list".into());
    };

    for name in &adopted {
        println!("adopted `{name}`");
    }
    for name in &dead {
        println!("`{name}` is gone");
    }
    for warning in &warnings {
        eprintln!("sbx: {warning}");
    }

    // One `Poll` per ready session, the same as the local path -- and the same
    // reasoning: a command that prints once and exits can afford it. It is a
    // round trip each rather than an exec each, which is the one place this is
    // meaningfully slower than being on the machine itself.
    let rows = sessions
        .into_iter()
        .map(|s| {
            let state = match s.state {
                State::Ready => poll_state(remote, &s.name).unwrap_or(s.state),
                other => other,
            };
            (s, state)
        })
        .collect::<Vec<_>>();

    print_sessions(&rows);
    Ok(())
}

/// The agent's state, or `None` if the server could not say.
///
/// A session that cannot be polled keeps its recorded state rather than failing
/// the whole listing: one wedged sandbox should not stop the other nine being
/// printed.
fn poll_state(remote: &remote::Remote, name: &str) -> Option<State> {
    match remote.call(sbx_proto::Request::Poll { name: name.into() }) {
        Ok(sbx_proto::Reply::Poll(poll)) => poll.status.map(|r| r.state),
        _ => None,
    }
}

fn remote_diff(remote: &remote::Remote, name: &str) -> Fallible {
    let stat = match remote.call(sbx_proto::Request::Poll { name: name.into() })? {
        sbx_proto::Reply::Poll(poll) => poll.stat,
        _ => None,
    };
    let sbx_proto::Reply::Diff { body } =
        remote.call(sbx_proto::Request::Diff { name: name.into() })?
    else {
        return Err("the server answered something other than a diff".into());
    };
    print_diff(stat, &body);
    Ok(())
}

fn remote_policy(remote: &remote::Remote, name: &str) -> Fallible {
    let sbx_proto::Reply::Policy {
        revision,
        template,
        lists,
    } = remote.call(sbx_proto::Request::Policy { name: name.into() })?
    else {
        return Err("the server answered something other than a policy".into());
    };
    // The server's lists, not this machine's: the rules being described are the
    // ones it is enforcing, and the global lists are part of that picture.
    print_policy(&revision, template.as_deref(), &lists);
    Ok(())
}

fn remote_events(remote: &remote::Remote, name: &str) -> Fallible {
    let sbx_proto::Reply::Events { events } =
        remote.call(sbx_proto::Request::Events { name: name.into() })?
    else {
        return Err("the server answered something other than an event feed".into());
    };
    print_events(&events);
    Ok(())
}

fn cmd_new(client: &dyn OpenShell, args: NewArgs, cfg: &Config) -> Fallible {
    let repo = args.repo.or_else(|| cfg.repo.clone()).ok_or_else(|| {
        format!(
            "no repository: pass --repo, or set `repo` in {}",
            cfg.path.display()
        )
    })?;

    // A name from --name, else the task, else the repo's last path segment.
    let name = match args.name {
        Some(n) => n,
        None => session::derive_name(&args.task, &repo)
            .ok_or("could not derive a session name; pass --name")?,
    };

    let draft = ops::Draft {
        name,
        repo,
        task: args.task,
        base: args.base.or_else(|| cfg.base.clone()),
        policy: args.policy.unwrap_or_else(|| cfg.policy().to_string()),
        // Replace rather than merge: `--provider` on the command line is the
        // whole answer for this session, and a config entry silently added to it
        // would attach a credential nobody asked for.
        providers: if args.providers.is_empty() {
            cfg.providers().to_vec()
        } else {
            args.providers
        },
        // No flag to override this: an MCP server is a tool the agent has, set
        // up once in the config file next to the container that serves it, and
        // a URL typed on a command line would be a policy hole opened by hand.
        mcp: cfg.mcp().to_vec(),
        skills: cfg.skills().to_vec(),
        // Resolved before anything is created, so an unknown name fails here
        // rather than as a docker tag nothing has ever built.
        toolchains: toolchain::resolve(&args.toolchains)?,
        start: !args.no_start,
    };

    // Here rather than inside ops::create, which never builds the image: the
    // build streams docker's output to the terminal, which only a command-line
    // caller can afford. The first session wanting a toolchain pays for the
    // variant; every one after it starts as fast as any other.
    image::ensure_for(&draft.toolchains)?;

    let repo = draft.repo.clone();
    let created = ops::create(client, &draft, &mut |step| match step {
        // The URL is worth naming, since this is the slow step and the one that
        // fails when a credential or a policy is wrong.
        ops::Step::Clone => println!("cloning {repo} ..."),
        other => println!("{} ...", other.label()),
    })?;

    for warning in &created.warnings {
        eprintln!("sbx: warning: {warning}");
    }
    let s = created.session;
    if !draft.start {
        println!(
            "agent not started (--no-start); attach with: sbx attach {}",
            s.name
        );
    }

    println!();
    println!("session  {}", s.name);
    println!("sandbox  {}", s.sandbox);
    println!("policy   {}", s.policy.as_deref().unwrap_or("-"));
    if !s.toolchains.is_empty() {
        println!("tools    {}", s.toolchains.join(", "));
    }
    println!("branch   {}", s.work_branch);
    println!("workdir  {}", session::REPO_PATH);
    Ok(())
}

/// Show, or create, the config file.
///
/// The effective value and where it came from, rather than the file's contents:
/// what a person wants to know is what the next `sbx new` will do, and the
/// answer is a mix of the file and the built-in defaults.
/// `sbx update`.
///
/// Prints what it is about to do before doing it, because replacing the binary
/// the caller is running is not a thing to do quietly -- and `--check` is the
/// same walk with the last step left out.
fn cmd_update(check: bool, tag: Option<&str>, force: bool) -> Fallible {
    if check {
        match update::check() {
            update::Status::Newer { running, latest } => {
                println!("sbx {running} is running; {latest} is out");
                println!("  update with: sbx update");
            }
            update::Status::Current(v) => println!("sbx {v} is the newest release"),
            update::Status::Ahead(v) => {
                println!("sbx {v} is ahead of the newest release (a build from a checkout)")
            }
            // Never "up to date": the question was not answered.
            update::Status::Unknown => {
                println!("sbx {}; could not read the release list", update::current())
            }
        }
        return Ok(());
    }

    match update::install(tag, force)? {
        update::Outcome::NoChange { version } => {
            println!("sbx {version} is the newest release; nothing to do");
            println!("  reinstall it anyway with: sbx update --force");
        }
        update::Outcome::Updated { from, to, at } => {
            println!("sbx {from} -> {to}  ({})", at.display());
            // The sandbox image is versioned separately and an update is the
            // moment its recipe most likely changed underneath it.
            println!("  `sbx image build` picks up any change to the sandbox image");
        }
    }
    Ok(())
}

fn cmd_config(cfg: &Config, init: bool, path_only: bool) -> Fallible {
    if path_only {
        println!("{}", cfg.path.display());
        return Ok(());
    }

    if init {
        if cfg.path.exists() {
            return Err(format!(
                "{} already exists; edit it, or delete it first",
                cfg.path.display()
            )
            .into());
        }
        if let Some(dir) = cfg.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&cfg.path, config::EXAMPLE)?;
        println!("wrote {}", cfg.path.display());
        println!("every key is commented out, so nothing has changed yet");
        return Ok(());
    }

    println!("{}", cfg.path.display());
    if !cfg.present {
        println!("  (no file; create one with: sbx config --init)");
    }
    println!();

    // `-` for a default and `*` for something the file says, so the two are
    // distinguishable at a glance -- which is the whole question this command
    // answers.
    let row = |key: &str, set: bool, value: String| {
        println!("{} {:<12} {}", if set { "*" } else { "-" }, key, value);
    };
    row(
        "gateway",
        cfg.gateway.is_some(),
        cfg.gateway
            .clone()
            .unwrap_or_else(|| "(the active one)".into()),
    );
    row(
        "repo",
        cfg.repo.is_some(),
        cfg.repo
            .clone()
            .unwrap_or_else(|| "(--repo each time)".into()),
    );
    row(
        "base",
        cfg.base.is_some(),
        cfg.base
            .clone()
            .unwrap_or_else(|| "(the remote's default branch)".into()),
    );
    row("policy", cfg.policy.is_some(), cfg.policy().to_string());
    row(
        "providers",
        cfg.providers.is_some(),
        if cfg.providers().is_empty() {
            "(none; the create form guesses)".into()
        } else {
            cfg.providers().join(", ")
        },
    );
    row(
        "repo_roots",
        cfg.repo_roots.is_some(),
        repos::roots(cfg.repo_roots.as_deref())
            .iter()
            .map(|r| r.path.display().to_string())
            .collect::<Vec<_>>()
            .join(" "),
    );
    row(
        "refresh",
        cfg.refresh.is_some(),
        format!("{}ms", tui::Intervals::from_config(cfg).refresh.as_millis()),
    );
    // Name and url both, because the url is the half that is wrong when an
    // agent reports a tool it cannot reach.
    row(
        "mcp",
        !cfg.mcp().is_empty(),
        if cfg.mcp().is_empty() {
            "(none)".into()
        } else {
            cfg.mcp()
                .iter()
                .map(|m| format!("{} -> {}", m.name, m.url))
                .collect::<Vec<_>>()
                .join(", ")
        },
    );
    println!();
    println!("* set here, - built-in default");
    Ok(())
}

fn cmd_ls(client: &dyn OpenShell) -> Fallible {
    let refreshed = ops::refresh_with(client, true)?;

    for name in &refreshed.adopted {
        println!("adopted `{name}`");
    }
    for warning in &refreshed.warnings {
        eprintln!("sbx: {warning}");
    }

    let rows: Vec<(Session, State)> = refreshed
        .sessions
        .into_iter()
        .map(|s| {
            // One poll per session. Fine for a command that prints once and
            // exits, unlike the TUI, which has to keep this bounded as sessions
            // are added.
            let state = match s.state {
                State::Ready => ops::poll(client, &s)
                    .status
                    .map_or(s.state, |report| report.state),
                // Anything else is a fact about the sandbox, which outranks
                // anything the agent inside it has to say.
                other => other,
            };
            (s, state)
        })
        .collect();

    print_sessions(&rows);
    Ok(())
}

/// The session table, for whichever client fetched the rows.
///
/// Shared rather than written twice, because two clients printing the same
/// thing differently is precisely the drift the protocol exists to prevent --
/// and a difference in this table is the one a person would notice first.
fn print_sessions(rows: &[(Session, State)]) {
    if rows.is_empty() {
        println!("no sessions. create one with: sbx new --repo <url> --task <what to do>");
        return;
    }

    let now = session::now_epoch();
    println!(
        "{:<20} {:<10} {:>5}  {:<24} REPO",
        "NAME", "STATE", "AGE", "BRANCH"
    );
    for (s, state) in rows {
        println!(
            "{:<20} {:<10} {:>5}  {:<24} {}",
            s.name,
            state.to_string(),
            session::humanize_age(s.created_at, now),
            s.work_branch,
            s.repo,
        );
    }
}

fn cmd_diff(client: &dyn OpenShell, name: &str) -> Fallible {
    let session = require_session(name)?;
    print_diff(
        ops::poll(client, &session).stat,
        &ops::repo_diff(client, &session),
    );
    Ok(())
}

fn print_diff(stat: Option<ops::DiffStat>, body: &str) {
    if let Some(stat) = stat {
        println!(
            "+{} -{}  {} untracked",
            stat.added, stat.removed, stat.untracked
        );
    }
    println!("{body}");
}

/// A session by name, or an error naming what to do about it.
fn require_session(name: &str) -> Result<Session, Box<dyn std::error::Error>> {
    Store::load()?
        .get(name)
        .cloned()
        .ok_or_else(|| format!("no session `{name}`; see sbx ls").into())
}

fn cmd_policy(client: &dyn OpenShell, name: &str) -> Fallible {
    let session = require_session(name)?;
    let rev = ops::policy(client, &session)?;
    // Empty on a read failure rather than fatal: the reason to run this command
    // is the sandbox's own rules, and losing them to an unreadable convenience
    // file would be the wrong trade. The section is omitted when the lists are
    // empty, so a failure reads the same as never having used the feature --
    // which is why the TUI, where the lists are edited, reports it instead.
    let lists = endpoints::Lists::load().unwrap_or_default();
    print_policy(&rev, session.policy.as_deref(), &lists);
    Ok(())
}

fn print_policy(
    rev: &openshell_client::PolicyRevision,
    template: Option<&str>,
    lists: &endpoints::Lists,
) {
    print!("{}", pane::to_plain(&policy::render(rev, template, lists)));
}

fn cmd_events(client: &dyn OpenShell, name: &str) -> Fallible {
    let session = require_session(name)?;
    print_events(&ops::events(client, &session)?);
    Ok(())
}

fn print_events(events: &[events::Event]) {
    if events.is_empty() {
        println!("no policy decisions in the recent log");
        return;
    }
    for e in events {
        let verdict = match e.verdict {
            events::Verdict::Allowed => "allow",
            events::Verdict::Denied => "DENY",
            events::Verdict::Neutral => "-",
        };
        println!(
            "{}  {:<5}  {:<16} {}{}",
            e.clock_utc(),
            verdict,
            e.class,
            e.subject,
            e.policy
                .as_deref()
                .map(|p| format!("  [{p}]"))
                .unwrap_or_default(),
        );
        if let Some(reason) = &e.reason {
            println!("                                   {reason}");
        }
    }
}

fn cmd_publish(client: &dyn OpenShell, name: &str, opts: publish::Options) -> Fallible {
    let session = require_session(name)?;
    let remote = forge::Remote::parse(&session.repo)?;
    println!(
        "publishing {} to {} ...",
        session.work_branch,
        remote.slug()
    );

    // ops::publish, not publish::publish: the state change belongs with the
    // action, so the CLI and the TUI cannot disagree about whether a session
    // has been published.
    let outcome = ops::publish(client, &session, &opts)?;
    for w in &outcome.warnings {
        eprintln!("sbx: {w}");
    }
    if outcome.pushed {
        println!("pushed   {}", session.work_branch);
    }
    match &outcome.pull_request {
        Some(url) => println!("pr       {url}"),
        None if opts.no_pr => {}
        None => println!("pr       (not opened)"),
    }

    Ok(())
}

fn cmd_attach(client: &CliClient, name: &str) -> Fallible {
    let session = require_session(name)?;

    println!("attaching to {name} - detach with Ctrl-b d");

    let status = attach::interactively(client, &session)?;
    if !status.success() {
        return Err(format!("attach exited with {status}").into());
    }
    Ok(())
}

fn cmd_rm(client: &dyn OpenShell, names: Vec<String>) -> Fallible {
    let mut failures = 0;

    for name in names {
        // One name at a time, each persisting as it goes: `sbx rm a b` that
        // fails on `b` must still have forgotten `a`, or a retry would try to
        // delete a sandbox that is already gone and call that the failure.
        match ops::destroy(client, &name) {
            Ok(ops::Destroyed::Sandbox) => println!("deleted {name}"),
            Ok(ops::Destroyed::RecordOnly) => println!("{name} was already gone"),
            Err(e) => {
                eprintln!("sbx: {e}");
                failures += 1;
            }
        }
    }

    if failures > 0 {
        return Err(format!("{failures} session(s) could not be removed").into());
    }
    Ok(())
}
