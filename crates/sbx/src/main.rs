//! `sbx` - run several coding agents in parallel, each in its own sandbox.

mod ansi;
mod config;
mod doctor;
mod endpoints;
mod events;
mod forge;
mod image;
mod mcp;
mod ops;
mod pane;
mod policy;
mod publish;
mod repos;
mod seed;
mod session;
mod skills;
mod status;
mod store;
mod tui;

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use openshell_client::{CliClient, OpenShell};

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

    /// List the policy templates shipped with this binary.
    Policies,

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

    /// Create the sandbox and clone, but do not start the agent.
    #[arg(long)]
    no_start: bool,
}

#[derive(Subcommand)]
enum ImageAction {
    /// Build the sandbox image (community base plus tmux).
    Build,
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

    let result = match cli.command {
        // No subcommand: the TUI is the point of the tool.
        None => tui::run(client, cfg),
        Some(Command::Doctor) => {
            let checks = doctor::run(&client, &loaded);
            return match doctor::report(&checks) {
                0 => ExitCode::SUCCESS,
                _ => ExitCode::FAILURE,
            };
        }
        Some(Command::Config { init, path }) => cmd_config(&cfg, init, path),
        Some(Command::New(args)) => cmd_new(&client, args, &cfg),
        Some(Command::Ls) => cmd_ls(&client),
        Some(Command::Attach { name }) => cmd_attach(&client, &name),
        Some(Command::Diff { name }) => cmd_diff(&client, &name),
        Some(Command::Policy { name }) => cmd_policy(&client, &name),
        Some(Command::Events { name }) => cmd_events(&client, &name),
        Some(Command::Policies) => {
            println!("{}", policy::help());
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
            ImageAction::Build => image::build().map_err(Into::into),
        },
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
        start: !args.no_start,
    };

    // Here rather than inside ops::create, which never builds the image: the
    // build streams docker's output to the terminal, which only a command-line
    // caller can afford.
    image::ensure()?;

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
    println!("branch   {}", s.work_branch);
    println!("workdir  {}", session::REPO_PATH);
    Ok(())
}

/// Show, or create, the config file.
///
/// The effective value and where it came from, rather than the file's contents:
/// what a person wants to know is what the next `sbx new` will do, and the
/// answer is a mix of the file and the built-in defaults.
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

    if refreshed.sessions.is_empty() {
        println!("no sessions. create one with: sbx new --repo <url> --task <what to do>");
        return Ok(());
    }

    let now = session::now_epoch();
    println!(
        "{:<20} {:<10} {:>5}  {:<24} REPO",
        "NAME", "STATE", "AGE", "BRANCH"
    );
    for s in &refreshed.sessions {
        // One poll per session. Fine for a command that prints once and exits,
        // unlike the TUI, which has to keep this bounded as sessions are added.
        let state = match s.state {
            State::Ready => ops::poll(client, s)
                .status
                .map_or(s.state, |report| report.state),
            // Anything else is a fact about the sandbox, which outranks
            // anything the agent inside it has to say.
            other => other,
        };
        println!(
            "{:<20} {:<10} {:>5}  {:<24} {}",
            s.name,
            state.to_string(),
            session::humanize_age(s.created_at, now),
            s.work_branch,
            s.repo,
        );
    }
    Ok(())
}

fn cmd_diff(client: &dyn OpenShell, name: &str) -> Fallible {
    let session = require_session(name)?;

    if let Some(stat) = ops::poll(client, &session).stat {
        println!(
            "+{} -{}  {} untracked",
            stat.added, stat.removed, stat.untracked
        );
    }
    println!("{}", ops::repo_diff(client, &session));
    Ok(())
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
    print!(
        "{}",
        pane::to_plain(&policy::render(&rev, session.policy.as_deref(), &lists))
    );
    Ok(())
}

fn cmd_events(client: &dyn OpenShell, name: &str) -> Fallible {
    let session = require_session(name)?;
    let events = ops::events(client, &session)?;
    if events.is_empty() {
        println!("no policy decisions in the recent log");
        return Ok(());
    }
    for e in &events {
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
    Ok(())
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

    let status = ops::attach_interactively(client, &session)?;
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
