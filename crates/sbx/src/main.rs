//! `sbx` - run several coding agents in parallel, each in its own sandbox.

mod doctor;
mod image;
mod ops;
mod seed;
mod session;
mod store;
mod tui;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use openshell_client::{CliClient, CreateOpts, Error as OsError, OpenShell};

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

    /// Policy YAML applied to the sandbox.
    #[arg(long)]
    policy: Option<PathBuf>,

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
        None => session::slugify(&args.task)
            .or_else(|| {
                args.repo
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .map(|s| s.trim_end_matches(".git"))
                    .and_then(session::slugify)
            })
            .ok_or("could not derive a session name; pass --name")?,
    };
    session::validate_name(&name)?;

    let mut store = Store::load()?;
    if store.contains(&name) {
        return Err(format!("session `{name}` already exists").into());
    }

    let mut s = Session::new(name, args.repo, args.task);
    s.base_branch = args.base;
    s.policy = args.policy.as_ref().map(|p| p.display().to_string());
    s.providers = args.providers.clone();

    image::ensure()?;

    println!("creating sandbox {} ...", s.sandbox);
    let opts = CreateOpts {
        name: s.sandbox.clone(),
        labels: s.labels(),
        policy: args.policy,
        providers: args.providers,
        from: Some(session::IMAGE.to_string()),
        // Keep the sandbox alive after the create command exits.
        command: vec!["true".into()],
        ..Default::default()
    };

    if let Err(e) = client.create(&opts) {
        s.state = State::Failed;
        store.upsert(s);
        store.save()?;
        return Err(e.into());
    }

    s.state = State::Seeding;
    store.upsert(s.clone());
    store.save()?;

    println!("cloning {} ...", s.repo);
    if let Err(e) = seed::seed(client, &s) {
        s.state = State::Failed;
        store.upsert(s);
        store.save()?;
        return Err(e.into());
    }

    s.state = State::Ready;
    store.upsert(s.clone());
    store.save()?;
    // Keep the in-sandbox record current: it is what adoption reads back, and
    // a stale one leaves recovered sessions frozen mid-lifecycle.
    if let Err(e) = seed::write_meta(client, &s) {
        eprintln!("sbx: warning: could not refresh sandbox metadata: {e}");
    }

    if args.no_start {
        println!(
            "agent not started (--no-start); attach with: sbx attach {}",
            s.name
        );
    } else {
        println!("starting {} ...", s.agent);
        if let Err(e) = seed::start_agent(client, &s) {
            // The session is usable even if the agent did not come up, so this
            // is reported rather than treated as a failed create.
            eprintln!("sbx: warning: could not start the agent: {e}");
        }
    }

    println!();
    println!("session  {}", s.name);
    println!("sandbox  {}", s.sandbox);
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
        println!(
            "{:<20} {:<10} {:>5}  {:<24} {}",
            s.name,
            s.state.to_string(),
            session::humanize_age(s.created_at, now),
            s.work_branch,
            s.repo,
        );
    }
    Ok(())
}

fn cmd_attach(client: &CliClient, name: &str) -> Fallible {
    let store = Store::load()?;
    let session = store
        .get(name)
        .cloned()
        .ok_or_else(|| format!("no session `{name}`; see sbx ls"))?;

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
    let mut store = Store::load()?;
    let mut failures = 0;

    for name in names {
        // Fall back to the naming convention so a sandbox can still be removed
        // when the cache has lost the session.
        let sandbox = store
            .get(&name)
            .map(|s| s.sandbox.clone())
            .unwrap_or_else(|| format!("sbx-{name}"));

        match client.delete(&sandbox) {
            Ok(()) => println!("deleted {sandbox}"),
            // Already gone is the desired end state, not a failure.
            Err(OsError::NotFound(_)) => println!("{sandbox} was already gone"),
            Err(e) => {
                eprintln!("sbx: could not delete {sandbox}: {e}");
                failures += 1;
                continue;
            }
        }
        store.remove(&name);
    }

    store.save()?;
    if failures > 0 {
        return Err(format!("{failures} session(s) could not be removed").into());
    }
    Ok(())
}
