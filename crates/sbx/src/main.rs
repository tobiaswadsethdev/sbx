//! `sbx` - run several coding agents in parallel, each in its own sandbox.

mod doctor;
mod events;
mod forge;
mod image;
mod ops;
mod pane;
mod policy;
mod publish;
mod repos;
mod seed;
mod session;
mod status;
mod store;
mod tui;

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use openshell_client::{CliClient, OpenShell};

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
    /// Repository to clone inside the sandbox.
    #[arg(long)]
    repo: String,

    /// Session name. Derived from the task when omitted.
    #[arg(long)]
    name: Option<String>,

    /// What the agent should do. Becomes the agent's opening prompt.
    #[arg(long, default_value = "")]
    task: String,

    /// Branch to clone from. Defaults to the remote's default branch.
    #[arg(long)]
    base: Option<String>,

    /// Policy to apply: a template name, or a path to a YAML file.
    ///
    /// Defaults to `feature-work`. See `sbx policies` for the templates; a spec
    /// containing a `/` or ending in `.yaml` is always read as a path.
    #[arg(long)]
    policy: Option<String>,

    /// Credential provider to attach. Repeatable.
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

    let mut client = CliClient::new();
    if let Some(g) = cli.gateway {
        client = client.with_gateway(g);
    }

    let result = match cli.command {
        // No subcommand: the TUI is the point of the tool.
        None => tui::run(client),
        Some(Command::Doctor) => {
            let checks = doctor::run(&client);
            return match doctor::report(&checks) {
                0 => ExitCode::SUCCESS,
                _ => ExitCode::FAILURE,
            };
        }
        Some(Command::New(args)) => cmd_new(&client, args),
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

fn cmd_new(client: &dyn OpenShell, args: NewArgs) -> Fallible {
    // A name from --name, else the task, else the repo's last path segment.
    let name = match args.name {
        Some(n) => n,
        None => session::derive_name(&args.task, &args.repo)
            .ok_or("could not derive a session name; pass --name")?,
    };

    let draft = ops::Draft {
        name,
        repo: args.repo,
        task: args.task,
        base: args.base,
        policy: args
            .policy
            .unwrap_or_else(|| policy::DEFAULT_TEMPLATE.to_string()),
        providers: args.providers,
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

fn cmd_ls(client: &dyn OpenShell) -> Fallible {
    let refreshed = ops::refresh(client)?;

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
    print!(
        "{}",
        pane::to_plain(&policy::render(&rev, session.policy.as_deref()))
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

    let script = format!(
        "tmux -f /etc/tmux.conf attach -d -t {tmux} 2>/dev/null \
         || tmux -f /etc/tmux.conf new-session -s {tmux} -c {repo}",
        tmux = seed::sh_quote(&session.tmux),
        repo = seed::sh_quote(session::REPO_PATH),
    );
    println!("attaching to {name} - detach with Ctrl-b d");

    let status = client
        .interactive_exec(&session.sandbox, &["sh", "-c", &script])
        .status()?;
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
