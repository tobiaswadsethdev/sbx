//! `sbxd` - run several coding agents in parallel, each in its own sandbox.
//!
//! One binary for the whole Linux side: the server the desktop application
//! dials, and the commands for the machine it runs on. It absorbed `sbx` in
//! v0.4.0 -- there used to be a second binary carrying this CLI and a terminal
//! interface beside it, and keeping two front ends honest about one set of
//! sessions cost more than the terminal one was worth. The window is the
//! interactive surface now; this is everything else.
//!
//! Argument parsing, printing, and [`attach`] -- the one piece that is
//! genuinely about the terminal this process was started in. Every behaviour
//! belongs to [`sbx_core`], which is what lets the window and this agree.

mod attach;
mod auth;
mod rpc;
mod serve;
mod stream;
mod tls;

use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use openshell_client::CliClient;
use sbx_client as remote;
use sbx_core::backend::Backends;
use sbx_core::{
    config, doctor, endpoints, events, forge, image, mcp, ops, pane, policy, publish, repos,
    session, store, toolchain, tracker, update,
};
use sbx_proto::{DEFAULT_PORT, Pairing};

use auth::Tokens;
use config::Config;
use sbx_core::state;
use session::{Session, State};
use store::Store;

#[derive(Parser)]
#[command(
    name = "sbxd",
    version,
    about = "Parallel coding agents in OpenShell sandboxes, and the server for them"
)]
struct Cli {
    /// Gateway name to operate on (defaults to the active one).
    #[arg(long, global = true)]
    gateway: Option<String>,

    /// Ask a paired `sbxd` instead of this machine's own sandboxes.
    ///
    /// `--server` alone when one server is paired, `--server=<name>` otherwise;
    /// `sbxd remotes` lists them. The name has to be attached with `=`, because
    /// a detached value cannot be told apart from the subcommand -- `sbxd
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
    /// Listen. The default when no command is given.
    Serve(Serve),
    /// Print a pairing string for a new client, once.
    Pair {
        /// What the client is, so this token can be revoked without the others.
        #[arg(default_value = "client")]
        name: String,
        /// The host a client should dial, when it is not this machine's own
        /// name -- a WSL server reached from Windows, or a box behind a
        /// forwarded port.
        #[arg(long)]
        host: Option<String>,
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
    },
    /// The tokens this server accepts.
    Tokens,
    /// Stop accepting a token.
    Revoke { name: String },
    /// The MCP servers in the config file, and what each managed one is doing.
    Mcp {
        /// Start, restart or stop one of them. Omit to list.
        #[arg(long, value_name = "ACTION")]
        action: Option<McpAction>,
        /// Which one. Required with --action.
        name: Option<String>,
    },
    /// The secret names this server holds. Never the values.
    Secrets,
    /// Store a secret a managed MCP server needs.
    ///
    /// The value is read from stdin, not from an argument: an argument lands in
    /// the shell history and in `ps` output, and this is a credential that a
    /// container will hold for months.
    ///
    ///   printf %s "$TOKEN" | sbxd secret SENTRY_TOKEN
    Secret {
        name: String,
        /// Forget it instead of storing one.
        #[arg(long)]
        forget: bool,
    },
    /// The skills a client has uploaded into this server's library.
    Skills,
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

        /// Widen egress to the package registries, without restarting the
        /// agent -- for the task that turns out to need a dependency
        /// installed. This was `w` in the terminal interface.
        #[arg(long)]
        widen: bool,

        /// Tighten it back again. This was `t`.
        #[arg(long, conflicts_with = "widen")]
        tighten: bool,
    },

    /// The task inbox: what the configured trackers say is assigned to you
    Tasks,
    /// Print a session's recent allow/deny decisions, newest first.
    Events {
        /// Session name.
        name: String,
    },

    /// Follow a session's events and agent state as they happen.
    ///
    /// Needs `--server`: the stream is the server's to push, and a session on
    /// this machine is already being watched by the TUI.
    Watch {
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

    /// Update sbxd itself to the newest published release.
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

    /// The global endpoint allow and block lists, applied to every new sandbox.
    ///
    /// Prints them with no flags. `--allow` and `--block` are what the terminal
    /// interface's `A` and `B` did against a denial in the events feed, and are
    /// the only way to edit these lists since that went: nothing else writes
    /// them, and a list you can only read is a list that rots.
    ///
    /// This records the rule for sandboxes started from now on. It does not
    /// touch a session that is already running -- that is `policy update`
    /// through the gateway, and it needs a live sandbox to update.
    Endpoints {
        /// Put an endpoint on the allow list, e.g. `crates.io:443`.
        #[arg(long, value_name = "ENDPOINT")]
        allow: Option<String>,

        /// Put an endpoint on the block list.
        #[arg(long, value_name = "ENDPOINT", conflicts_with = "allow")]
        block: Option<String>,

        /// Restrict an `--allow` to one binary. Repeatable; every binary when
        /// omitted, which is the widest the gateway can express.
        #[arg(long = "binary", value_name = "PATH", requires = "allow")]
        binaries: Vec<String>,
    },

    /// Delete sessions and their sandboxes.
    #[command(alias = "delete")]
    Rm {
        /// Session names to remove.
        #[arg(required = true)]
        names: Vec<String>,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum McpAction {
    Start,
    Restart,
    Stop,
}

#[derive(clap::Args)]
struct Serve {
    /// Address to listen on. Loopback by default: anything else is reachable
    /// from the network, and an authenticated client can create containers here.
    #[arg(long, default_value = "127.0.0.1")]
    bind: String,
    #[arg(long, default_value_t = DEFAULT_PORT)]
    port: u16,
    /// An extra name or address to put in the certificate. Repeatable.
    #[arg(long = "san")]
    sans: Vec<String>,
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
    /// `sbxd policies` for the templates; a spec containing a `/` or ending in
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
    /// image and built on first use; `sbxd toolchains` lists them. A toolchain
    /// also opens its package registry for the binary that fetches from it, and
    /// for nothing else in the sandbox.
    #[arg(long = "toolchain", value_delimiter = ',')]
    toolchains: Vec<String>,

    /// Create the sandbox and clone, but do not start the agent.
    #[arg(long)]
    no_start: bool,

    /// Run the session in a `git worktree` on this machine instead of in a
    /// sandbox.
    ///
    /// **There is no isolation.** The agent runs as you, with your files, your
    /// git credentials and whatever the network allows; there is no policy to
    /// apply and nothing to allow or deny, so `sbxd policy` and `sbxd events`
    /// have nothing to show for it. What it buys is a session in seconds
    /// sharing an existing checkout's object store, which is the case a clone
    /// into a fresh sandbox is slowest at.
    ///
    /// `--repo` must be a checkout on this machine, since a worktree is added
    /// to one. `--policy`, `--provider` and `--toolchain` do not apply and are
    /// refused rather than ignored.
    #[arg(long)]
    worktree: bool,
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
    // Before the arguments are even parsed. A release downloaded by an earlier
    // run is put in place here and nowhere else, so the swap can never land
    // halfway through a command -- and `exec` rather than carrying on, because
    // this process is still the old binary and a caller who restarted to pick
    // up an update should not have to restart twice.
    //
    // `exec` replaces this process image, so nothing below runs on that path.
    // If it returns at all it failed, and carrying on as the version we already
    // are is the right answer: the update is not worth refusing to start over.
    if let Some(at) = update::apply_staged() {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(&at)
            .args(std::env::args_os().skip(1))
            .exec();
        eprintln!(
            "sbxd: updated to {}, but could not restart: {err}",
            at.display()
        );
    }

    let cli = Cli::parse();

    // Before anything else, because a file that cannot be read has to stop the
    // command rather than be quietly replaced by the built-in defaults -- the
    // one exception being `doctor`, whose job is to say what is wrong.
    let loaded = Config::load();
    let cfg = match (&loaded, &cli.command) {
        (Ok(c), _) => c.clone(),
        (Err(_), Some(Command::Doctor)) => Config::default(),
        (Err(e), _) => {
            eprintln!("sbxd: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut client = CliClient::new();
    // The flag first, the file second: a gateway named on the command line is
    // about this command, and the file is about every other one.
    if let Some(g) = cli.gateway.clone().or_else(|| cfg.gateway.clone()) {
        client = client.with_gateway(g);
    }
    // Both backends. Every command that works on a session goes through this
    // rather than the client, because which one a session belongs to is a fact
    // about the session and not about the command.
    let backends = Backends::from_config(Box::new(client.clone()), &cfg);

    // Read out before the match, which moves `cli.command`.
    let chosen = cli.server.clone();

    let result = match cli.command {
        // No subcommand: listen. This used to open a terminal interface, and
        // before that `sbxd serve` was a subcommand you had to name. Serving is
        // what the machine this runs on is for.
        None => serve(Serve {
            bind: "127.0.0.1".into(),
            port: DEFAULT_PORT,
            sans: Vec::new(),
        }),
        Some(Command::Serve(s)) => serve(s),
        Some(Command::Pair { name, host, port }) => pair(&name, host, port),
        Some(Command::Tokens) => list_tokens(),
        Some(Command::Revoke { name }) => revoke(&name),
        Some(Command::Mcp { action, name }) => mcp(action, name),
        Some(Command::Secrets) => list_secrets(),
        Some(Command::Secret { name, forget }) => secret(&name, forget),
        Some(Command::Skills) => list_skills(),
        Some(Command::Doctor) => {
            let mut checks = doctor::run(&client, &loaded);
            // Appended here rather than inside `doctor::run`, because both need
            // things the core does not have: the protocol's port, and a client
            // for it.
            checks.extend(doctor::check_wsl(DEFAULT_PORT));
            checks.extend(remote::checks());
            return match doctor::report(&checks) {
                0 => ExitCode::SUCCESS,
                _ => ExitCode::FAILURE,
            };
        }
        Some(Command::Config { init, path }) => cmd_config(&cfg, init, path),
        Some(Command::New(args)) => cmd_new(&backends, args, &cfg),
        Some(Command::Ls) => match server(chosen.as_deref()) {
            Ok(Some(r)) => remote_ls(&r),
            Ok(None) => cmd_ls(&backends),
            Err(e) => Err(e),
        },
        Some(Command::Attach { name }) => cmd_attach(&backends, &name),
        Some(Command::Diff { name }) => match server(chosen.as_deref()) {
            Ok(Some(r)) => remote_diff(&r, &name),
            Ok(None) => cmd_diff(&backends, &name),
            Err(e) => Err(e),
        },
        Some(Command::Policy {
            name,
            widen,
            tighten,
        }) => match server(chosen.as_deref()) {
            // Reading works over a connection; changing a policy does not yet,
            // and saying so beats printing the remote policy as though the
            // flag had been honoured.
            Ok(Some(_)) if widen || tighten => {
                Err("--widen and --tighten need the machine the sandbox is on".into())
            }
            Ok(Some(r)) => remote_policy(&r, &name),
            Ok(None) => cmd_policy(&backends, &name, widen, tighten),
            Err(e) => Err(e),
        },
        Some(Command::Events { name }) => match server(chosen.as_deref()) {
            Ok(Some(r)) => remote_events(&r, &name),
            Ok(None) => cmd_events(&backends, &name),
            Err(e) => Err(e),
        },
        Some(Command::Tasks) => match server(chosen.as_deref()) {
            Ok(Some(r)) => remote_tasks(&r),
            Ok(None) => cmd_tasks(&cfg),
            Err(e) => Err(e),
        },
        Some(Command::Watch { name }) => match server(chosen.as_deref()) {
            Ok(Some(r)) => remote_watch(&r, &name),
            Ok(None) => Err("watch needs a server: try `sbxd --server watch <name>`".into()),
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
            &backends,
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
        Some(Command::Endpoints {
            allow,
            block,
            binaries,
        }) => cmd_endpoints(allow.as_deref(), block.as_deref(), binaries),
        Some(Command::Rm { names }) => cmd_rm(&backends, names),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sbxd: {e}");
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
    // The checking and the saving are `sbx_client::pair`, because the desktop
    // application's connect dialog does the same thing and two implementations
    // of "is this a server I can talk to" would be one implementation and one
    // place a mistake is silent. What is left here is what a terminal adds.
    let (remote, hello) = remote::pair(pairing, name)?;
    println!(
        "paired with `{}` at {} (sbxd {})",
        remote.name,
        remote.address(),
        hello.version
    );
    println!("try: sbxd --server {} ls", remote.name);
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
        println!("no servers paired. run `sbxd pair` on one, then `sbxd connect <string>`");
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
        eprintln!("sbxd: {warning}");
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
    let sbx_proto::Reply::Policy(view) =
        remote.call(sbx_proto::Request::Policy { name: name.into() })?
    else {
        return Err("the server answered something other than a policy".into());
    };
    // The server's view, not one assembled here: the template and the global
    // lists in it are the ones that machine is enforcing with.
    print_policy(&view);
    Ok(())
}

/// Follow a session until interrupted.
///
/// The third thing to speak the protocol, after the server and the desktop
/// application, and the cheapest place to notice that a frame does not say what
/// it needs to: a terminal shows you the feed with nothing between you and it.
fn remote_watch(remote: &remote::Remote, name: &str) -> Fallible {
    use sbx_proto::stream::{Channel, ClientFrame, ServerFrame};

    let stream = remote.stream()?;
    const EVENTS: u32 = 1;
    const STATUS: u32 = 2;

    stream.send(ClientFrame::Open {
        id: EVENTS,
        channel: Channel::Events {
            session: name.to_string(),
        },
    });
    stream.send(ClientFrame::Open {
        id: STATUS,
        channel: Channel::Status {
            session: name.to_string(),
        },
    });

    println!("watching {name}; Ctrl-C to stop");

    for message in stream.frames() {
        match message {
            remote::Incoming::Frame(frame) => match *frame {
                ServerFrame::Events { events, .. } => print_events(&events),
                ServerFrame::Status { poll, .. } => {
                    if let Some(report) = poll.status {
                        let detail = report
                            .detail
                            .as_deref()
                            .map(|d| format!("  {d}"))
                            .unwrap_or_default();
                        println!("-- {}{detail}", report.state);
                    }
                }
                ServerFrame::Closed {
                    reason: Some(reason),
                    ..
                } => return Err(reason.into()),
                _ => {}
            },
            remote::Incoming::Ended(reason) => {
                return match reason {
                    Some(reason) => Err(reason.into()),
                    None => Ok(()),
                };
            }
        }
    }
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

fn remote_tasks(remote: &remote::Remote) -> Fallible {
    let sbx_proto::Reply::Tasks(inbox) = remote.call(sbx_proto::Request::Tasks)? else {
        return Err("the server answered something other than a task inbox".into());
    };
    print_tasks(&inbox);
    Ok(())
}

fn cmd_tasks(cfg: &Config) -> Fallible {
    if cfg.trackers().is_empty() {
        return Err(format!(
            "no trackers configured; add a `[[tracker]]` table to {} (see docs/inbox.md)",
            cfg.path.display()
        )
        .into());
    }
    print_tasks(&tracker::inbox(cfg.trackers(), cfg.branch_prefix()));
    Ok(())
}

/// The inbox, for whichever client fetched it.
///
/// Shared for the reason the session table is: two clients printing the same
/// thing differently is the drift the protocol exists to prevent.
fn print_tasks(inbox: &tracker::Inbox) {
    for warning in &inbox.warnings {
        eprintln!("sbxd: {warning}");
    }
    if inbox.tasks.is_empty() {
        println!("nothing assigned to you");
        return;
    }
    println!(
        "{:<12} {:<12} {:<14} {:<40} BRANCH",
        "KEY", "TRACKER", "STATE", "TITLE"
    );
    for t in &inbox.tasks {
        println!(
            "{:<12} {:<12} {:<14} {:<40} {}",
            t.key,
            t.tracker,
            truncate(&t.status, 14),
            truncate(&t.title, 40),
            t.branch,
        );
    }
}

/// Cut to a column width, so one long title does not fold the whole table.
fn truncate(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    s.chars().take(width.saturating_sub(1)).collect::<String>() + "…"
}

fn cmd_new(backends: &Backends, args: NewArgs, cfg: &Config) -> Fallible {
    // Refused rather than ignored: each of these is an instruction to a gateway
    // that will not be involved, and a session created with a policy flag that
    // did nothing would be one whose owner believes it is isolated.
    if args.worktree {
        for (flag, given) in [
            ("--policy", args.policy.is_some()),
            ("--provider", !args.providers.is_empty()),
            ("--toolchain", !args.toolchains.is_empty()),
        ] {
            if given {
                return Err(format!(
                    "{flag} does not apply to --worktree: there is no sandbox and no policy. \
                     See `sbxd new --help`."
                )
                .into());
            }
        }
    }

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
        backend: if args.worktree {
            session::Kind::Worktree
        } else {
            session::Kind::Sandbox
        },
        // The convention, from the config file, because this builds a `Draft`
        // directly rather than going through `NewSession::into_draft` -- which
        // is the server's one place for this and is where the window's answer
        // comes from. Left `None` it would be `sbx/<name>` whatever the file
        // says, which is two answers on one machine.
        branch: Some(format!("{}/{name}", cfg.branch_prefix())),
        // No inbox here: a session started from a ticket is the window's.
        ticket: None,
        // The terminal has no projects; see `Session::project`.
        project: None,
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
        mcp: cfg.mcp_servers(),
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
    // Only a sandbox has an image; a worktree session uses this machine's
    // toolchains, which is both its point and its limitation.
    if draft.backend == session::Kind::Sandbox {
        image::ensure_for(&draft.toolchains)?;
    }
    // The managed MCP containers, before the seeder points the agent at them.
    // Here rather than in `ops::create` for the reason the image build is here:
    // it is a side effect on this machine, with output of its own, and `ops` is
    // what both front ends share.
    for warning in mcp::ensure(cfg.mcp()) {
        eprintln!("sbxd: warning: {warning}");
    }

    let repo = draft.repo.clone();
    let kind = draft.backend;
    let created = ops::create(backends, &draft, &mut |step| match (step, kind) {
        // The URL is worth naming, since this is the slow step and the one that
        // fails when a credential or a policy is wrong.
        (ops::Step::Clone, session::Kind::Sandbox) => println!("cloning {repo} ..."),
        (other, kind) => println!("{} ...", other.label(kind)),
    })?;

    for warning in &created.warnings {
        eprintln!("sbxd: warning: {warning}");
    }
    let s = created.session;
    if !draft.start {
        println!(
            "agent not started (--no-start); attach with: sbxd attach {}",
            s.name
        );
    }

    println!();
    println!("session  {}", s.name);
    let backend = backends.for_session(&s);
    match s.backend {
        session::Kind::Sandbox => {
            println!("sandbox  {}", s.sandbox);
            println!("policy   {}", s.policy.as_deref().unwrap_or("-"));
        }
        // What a sandboxed session says here is the isolation it got. This one
        // has none, and the line that would have named a policy says so
        // instead of being left blank.
        session::Kind::Worktree => {
            println!("isolation {}", backend.isolation().label());
            println!("         {}", backend.isolation().explain());
        }
    }
    if !s.toolchains.is_empty() {
        println!("tools    {}", s.toolchains.join(", "));
    }
    println!("branch   {}", s.work_branch);
    // The backend's, not a constant: `/sandbox/repo` is where a sandboxed
    // session's working copy is and a worktree's is wherever it was put.
    println!("workdir  {}", backend.paths(&s).repo);
    Ok(())
}

/// Show, or create, the config file.
///
/// The effective value and where it came from, rather than the file's contents:
/// what a person wants to know is what the next `sbxd new` will do, and the
/// answer is a mix of the file and the built-in defaults.
/// `sbxd update`.
///
/// Prints what it is about to do before doing it, because replacing the binary
/// the caller is running is not a thing to do quietly -- and `--check` is the
/// same walk with the last step left out.
fn cmd_update(check: bool, tag: Option<&str>, force: bool) -> Fallible {
    if check {
        match update::check() {
            update::Status::Newer { running, latest } => {
                println!("sbxd {running} is running; {latest} is out");
                println!("  update with: sbxd update");
            }
            update::Status::Current(v) => println!("sbxd {v} is the newest release"),
            update::Status::Ahead(v) => {
                println!("sbxd {v} is ahead of the newest release (a build from a checkout)")
            }
            // Never "up to date": the question was not answered.
            update::Status::Unknown => {
                println!(
                    "sbxd {}; could not read the release list",
                    update::current()
                )
            }
        }
        return Ok(());
    }

    match update::install(tag, force)? {
        update::Outcome::NoChange { version } => {
            println!("sbxd {version} is the newest release; nothing to do");
            println!("  reinstall it anyway with: sbxd update --force");
        }
        update::Outcome::Updated { from, to, at } => {
            println!("sbxd {from} -> {to}  ({})", at.display());
            // Replacing the file does not replace the process: Linux keeps a
            // running server on the old inode until something restarts it, and
            // one still answering the previous version is exactly the confusion
            // this line exists to prevent.
            println!("  restart it to pick this up: systemctl --user restart sbxd");
            // The sandbox image is versioned separately and an update is the
            // moment its recipe most likely changed underneath it.
            println!("  `sbxd image build` picks up any change to the sandbox image");
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
        println!("  (no file; create one with: sbxd config --init)");
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
    // Only the terminal interface ever read this, and that went in v0.4.0. The
    // key is still parsed, so a config file written before then still loads --
    // but nothing acts on it, and saying so beats printing a number that looks
    // like it does something.
    row(
        "refresh",
        cfg.refresh.is_some(),
        cfg.refresh
            .map(|d| format!("{}ms (unused)", d.as_millis()))
            .unwrap_or_else(|| "(unused)".into()),
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
                .map(|e| {
                    // A managed entry's url is derived from the container this
                    // server starts, so what is worth printing is the image it
                    // runs; an external one is a url somebody else operates.
                    match &e.managed {
                        Some(m) => format!("{} -> {} (managed)", e.name(), m.image),
                        None => format!("{} -> {}", e.name(), e.server.url),
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        },
    );
    println!();
    println!("* set here, - built-in default");
    Ok(())
}

fn cmd_ls(backends: &Backends) -> Fallible {
    let refreshed = ops::refresh_with(backends, true)?;

    for name in &refreshed.adopted {
        println!("adopted `{name}`");
    }
    for warning in &refreshed.warnings {
        eprintln!("sbxd: {warning}");
    }

    let rows: Vec<(Session, State)> = refreshed
        .sessions
        .into_iter()
        .map(|s| {
            // One poll per session. Fine for a command that prints once and
            // exits, unlike the TUI, which has to keep this bounded as sessions
            // are added.
            let state = match s.state {
                State::Ready => ops::poll(backends.for_session(&s), &s)
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
        println!("no sessions. create one with: sbxd new --repo <url> --task <what to do>");
        return;
    }

    let now = session::now_epoch();
    // The kind is a column rather than a suffix on the state, because it is not
    // a state: it says what the session *is*, and a product whose pitch is
    // isolation cannot have a mode where the isolation is invisible in the list.
    println!(
        "{:<20} {:<9} {:<10} {:>5}  {:<24} REPO",
        "NAME", "KIND", "STATE", "AGE", "BRANCH"
    );
    for (s, state) in rows {
        println!(
            "{:<20} {:<9} {:<10} {:>5}  {:<24} {}",
            s.name,
            s.backend.to_string(),
            state.to_string(),
            session::humanize_age(s.created_at, now),
            s.work_branch,
            s.repo,
        );
    }
}

fn cmd_diff(backends: &Backends, name: &str) -> Fallible {
    let session = require_session(name)?;
    print_diff(
        ops::poll(backends.for_session(&session), &session).stat,
        &ops::repo_diff(backends.for_session(&session), &session),
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
        .ok_or_else(|| format!("no session `{name}`; see sbxd ls").into())
}

fn cmd_policy(backends: &Backends, name: &str, widen: bool, tighten: bool) -> Fallible {
    let session = require_session(name)?;
    let backend = backends.for_session(&session);

    // The change first, so what is printed below is the policy that came back
    // from applying it rather than the one from before. `repolicy` waits for
    // the gateway to load the rules, so this is not a race.
    let rev = if widen || tighten {
        let preset = &policy::REGISTRIES;
        let update = match widen {
            true => preset.widen(),
            false => preset.tighten(),
        };
        let rev = ops::repolicy(backend, &session, &update)?;
        println!(
            "{} {}",
            if widen {
                "widened to"
            } else {
                "tightened from"
            },
            preset.label
        );
        rev
    } else {
        ops::policy(backend, &session)?
    };

    // Empty on a read failure rather than fatal: the reason to run this command
    // is the sandbox's own rules, and losing them to an unreadable convenience
    // file would be the wrong trade. The section is omitted when the lists are
    // empty, so a failure reads the same as never having used the feature --
    // which is why `sbxd endpoints`, where the lists are edited, reports it
    // instead.
    let lists = endpoints::Lists::load().unwrap_or_default();
    print_policy(&policy::View::of(&rev, session.policy.as_deref(), &lists));
    Ok(())
}

fn print_policy(view: &policy::View) {
    print!("{}", pane::to_plain(&policy::render(view)));
}

fn cmd_events(backends: &Backends, name: &str) -> Fallible {
    let session = require_session(name)?;
    print_events(&ops::events(backends.for_session(&session), &session)?);
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

fn cmd_publish(backends: &Backends, name: &str, opts: publish::Options) -> Fallible {
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
    let outcome = ops::publish(backends.for_session(&session), &session, &opts)?;
    for w in &outcome.warnings {
        eprintln!("sbxd: {w}");
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

fn cmd_attach(backends: &Backends, name: &str) -> Fallible {
    let session = require_session(name)?;

    println!("attaching to {name} - detach with Ctrl-b d");

    let status = attach::interactively(backends, &session)?;
    if !status.success() {
        return Err(format!("attach exited with {status}").into());
    }
    Ok(())
}

/// Read or edit the global endpoint lists.
///
/// Through [`endpoints::update_at`] rather than a load/modify/save here,
/// because that takes the lock file the window's own writes take: two processes
/// editing this at once is not exotic when one of them is a server.
fn cmd_endpoints(allow: Option<&str>, block: Option<&str>, binaries: Vec<String>) -> Fallible {
    let path = endpoints::Lists::default_path();
    if let Some(endpoint) = allow {
        endpoints::update_at(&path, path.with_extension("lock"), |l| {
            l.allow(endpoint, binaries)
        })?;
        println!("{endpoint} is on the allow list");
    } else if let Some(endpoint) = block {
        endpoints::update_at(&path, path.with_extension("lock"), |l| l.block(endpoint))?;
        println!("{endpoint} is on the block list");
    }

    let lists = endpoints::Lists::load().unwrap_or_default();
    if lists.is_empty() {
        println!("no global endpoint rules ({})", path.display());
        return Ok(());
    }
    for a in &lists.allow {
        let who = match a.binaries.is_empty() {
            true => "any binary".to_string(),
            false => a.binaries.join(" "),
        };
        println!("{:<7} {:<32} {who}", "allow", a.endpoint);
    }
    for e in &lists.block {
        println!("{:<7} {e}", "block");
    }
    Ok(())
}

fn cmd_rm(backends: &Backends, names: Vec<String>) -> Fallible {
    let mut failures = 0;

    for name in names {
        // One name at a time, each persisting as it goes: `sbxd rm a b` that
        // fails on `b` must still have forgotten `a`, or a retry would try to
        // delete a sandbox that is already gone and call that the failure.
        match ops::destroy(backends, &name) {
            Ok(ops::Destroyed::Sandbox) => println!("deleted {name}"),
            Ok(ops::Destroyed::RecordOnly) => println!("{name} was already gone"),
            Err(e) => {
                eprintln!("sbxd: {e}");
                failures += 1;
            }
        }
    }

    if failures > 0 {
        return Err(format!("{failures} session(s) could not be removed").into());
    }
    Ok(())
}

/// The catalog and what Docker says about it, or one action on one entry.
///
/// The same [`sbx_core::mcp`] calls the window's integrations screen makes, so a
/// headless server is not a second-class one -- and so a state this prints
/// cannot disagree with a state that screen shows.
fn mcp(action: Option<McpAction>, name: Option<String>) -> Fallible {
    let cfg = sbx_core::config::Config::load()?;
    if let Some(action) = action {
        let name = name.ok_or("which mcp server? give a name with --action")?;
        let entry = cfg
            .mcp()
            .iter()
            .find(|e| e.name() == name)
            .ok_or_else(|| format!("no mcp server named `{name}` in {}", cfg.path.display()))?;
        if !entry.is_managed() {
            return Err(format!("`{name}` is a url this server does not run").into());
        }
        match action {
            McpAction::Start => {
                if let Some(e) = sbx_core::mcp::ensure(std::slice::from_ref(entry)).first() {
                    return Err(e.clone().into());
                }
            }
            McpAction::Restart => sbx_core::mcp::start(entry)?,
            McpAction::Stop => sbx_core::mcp::stop(entry.name())?,
        }
    }

    let live = sbx_core::mcp::statuses(cfg.mcp());
    if live.is_empty() {
        println!("no mcp servers in {}", cfg.path.display());
        return Ok(());
    }
    println!("{:<16} {:<10} WHAT", "NAME", "STATE");
    for s in &live {
        println!(
            "{:<16} {:<10} {}",
            s.name,
            if s.managed {
                format!("{:?}", s.state).to_lowercase()
            } else {
                "external".into()
            },
            s.image.clone().unwrap_or_else(|| s.url.clone()),
        );
        if let Some(problem) = &s.problem {
            println!("{:<16} {problem}", "");
        }
    }
    Ok(())
}

fn list_secrets() -> Fallible {
    let names = sbx_core::secrets::names();
    if names.is_empty() {
        println!("no secrets. `printf %s \"$TOKEN\" | sbxd secret <NAME>` stores one.");
        return Ok(());
    }
    // Names only, and this is the only thing that ever prints them: there is no
    // command that reads a value back, on purpose.
    for name in names {
        println!("{name}");
    }
    Ok(())
}

fn secret(name: &str, forget: bool) -> Fallible {
    if forget {
        sbx_core::secrets::forget(name)?;
        println!("forgot `{name}`");
        return Ok(());
    }
    use std::io::Read as _;
    let mut value = String::new();
    std::io::stdin().read_to_string(&mut value)?;
    // Trailing newline trimmed and nothing else: a token with a space in it is
    // not a thing, and `printf` without `\n` is not what anybody types.
    sbx_core::secrets::set(name, value.trim_end_matches(['\n', '\r']))?;
    println!("stored `{name}`; restart the servers that use it: sbxd mcp --action restart <name>");
    Ok(())
}

fn list_skills() -> Fallible {
    let library = sbx_core::skills::library();
    if library.is_empty() {
        println!(
            "no uploaded skills in {}. A client pushes its own when it creates a session.",
            sbx_core::skills::library_dir().display()
        );
        return Ok(());
    }
    println!("{:<24} FROM", "NAME");
    for s in &library {
        println!("{:<24} {}", s.name, s.origin);
    }
    Ok(())
}

fn serve(opts: Serve) -> Fallible {
    let dir = state::dir();
    state::private_dir(&dir)?;

    let identity = tls::ensure(&dir, &tls::default_sans(&opts.sans))?;
    let tokens = Tokens::load()?;

    if tokens.is_empty() {
        // Starting with nothing that can connect is legal and almost never
        // meant. Said at start rather than left for the first client to
        // discover as an unexplained 401.
        eprintln!("sbxd: no tokens yet, so nothing can connect. Run `sbxd pair` to make one.");
    }

    let addr: SocketAddr = format!("{}:{}", opts.bind, opts.port).parse()?;
    println!("{}", serve::describe(addr));
    println!("certificate {}", identity.fingerprint);

    // Once, at startup rather than per request: repairing a record left mid
    // lifecycle costs an exec per session, and what it fixes is a create that
    // died, which cannot happen again while the server is down.
    match sbx_core::ops::refresh_with(&rpc::backends(), true) {
        Ok(r) => {
            for name in &r.adopted {
                println!("adopted `{name}`");
            }
            for warning in &r.warnings {
                eprintln!("sbxd: {warning}");
            }
        }
        // Not fatal. The gateway can come back, and a server that refuses to
        // start without it is one you cannot reach to find out why.
        Err(e) => eprintln!("sbxd: the gateway did not answer at startup: {e}"),
    }

    // The managed MCP containers, brought up with the server that owns them.
    //
    // Here rather than left to the first create, because a session is not the
    // only thing that uses them: an agent already running reconnects to one
    // that comes back, and a person opening the integrations screen after a
    // reboot should see them running rather than have to press start. Anything
    // already running is left exactly alone -- restarting it would drop the
    // connections of every live session using it.
    if let Ok(cfg) = sbx_core::config::Config::load() {
        let managed = cfg.mcp().iter().filter(|e| e.is_managed()).count();
        if managed > 0 {
            let warnings = sbx_core::mcp::ensure(cfg.mcp());
            println!(
                "{managed} managed mcp server{} ({} could not start)",
                if managed == 1 { "" } else { "s" },
                warnings.len()
            );
            for warning in &warnings {
                eprintln!("sbxd: {warning}");
            }
        }
    }

    let server = Arc::new(serve::Server {
        tokens: std::sync::RwLock::new(tokens),
    });

    // Whether to look for new releases while running. On by default: a server
    // that sits behind a window for months is exactly the thing that goes stale
    // without anyone noticing.
    let auto_update = sbx_core::config::Config::load()
        .ok()
        .and_then(|c| c.auto_update)
        .unwrap_or(true);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async move {
        if auto_update {
            tokio::spawn(watch_for_releases());
        }

        let config = axum_server::tls_rustls::RustlsConfig::from_pem(
            identity.cert_pem.into_bytes(),
            identity.key_pem.into_bytes(),
        )
        .await?;

        axum_server::bind_rustls(addr, config)
            .serve(serve::app(server).into_make_service())
            .await
    })?;

    Ok(())
}

/// Ask github for a newer release, and download one if there is.
///
/// Downloads only. The staged file sits beside the binary until something
/// starts `sbxd` again, at which point `main` swaps it in -- so a session
/// running right now is never interrupted by this, and the server keeps
/// answering as the version it started as. That is the whole reason this is
/// two halves rather than one.
///
/// Every failure is a log line and nothing more. A server that could not reach
/// github is a server that is working; the only thing that has not happened is
/// an update, and `sbxd doctor` says so on demand anyway.
///
/// `spawn_blocking` because the work underneath is `curl`, `sha256sum` and
/// `tar` -- subprocesses, which would otherwise hold a runtime thread that has
/// requests to serve.
async fn watch_for_releases() {
    // Not immediately at startup. A restart is the most likely moment for
    // several things to want the network at once, and nothing here is urgent
    // enough to join that.
    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    loop {
        match tokio::task::spawn_blocking(update::stage).await {
            Ok(Ok(Some(v))) => {
                println!("sbxd {v} downloaded and waiting; it is applied the next time sbxd starts")
            }
            Ok(Ok(None)) => {}
            Ok(Err(e)) => eprintln!("sbxd: could not check for a new release: {e}"),
            // The blocking pool cancelled or panicked. Not a reason to stop
            // checking for the lifetime of the process.
            Err(e) => eprintln!("sbxd: the update check did not run: {e}"),
        }
        tokio::time::sleep(update::CHECK_EVERY).await;
    }
}

fn pair(name: &str, host: Option<String>, port: u16) -> Fallible {
    let dir = state::dir();
    state::private_dir(&dir)?;

    // Generated now if the server has not run yet, so a pairing string can be
    // made before the first start -- and so the fingerprint in it is the one
    // that server will actually present.
    let identity = tls::ensure(&dir, &tls::default_sans(&[]))?;

    let mut tokens = Tokens::load()?;
    let token = tokens.create(name)?;

    let host = host.unwrap_or_else(default_host);
    let pairing = Pairing {
        host,
        port,
        token,
        fingerprint: identity.fingerprint,
    };

    println!("{pairing}");
    println!();
    println!("Paste that into the client once. It is shown here and nowhere else --");
    println!("the server keeps only a hash, so a lost token is replaced, not recovered.");
    if let Some(hint) = wsl_hint() {
        println!();
        println!("{hint}");
    }
    Ok(())
}

fn default_host() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "localhost".to_string())
}

/// Said at pairing time, because this is the one place the address in the
/// string is chosen, and the WSL answer is not the machine's own hostname.
fn wsl_hint() -> Option<String> {
    if !is_wsl() {
        return None;
    }
    Some(
        "This is WSL. If the client is on Windows, the host above is probably not\n\
         what it should dial: with mirrored networking use `localhost`, and with the\n\
         default NAT use the address `hostname -I` prints. `sbxd doctor` says which."
            .to_string(),
    )
}

fn is_wsl() -> bool {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| {
            let s = s.to_ascii_lowercase();
            s.contains("microsoft") || s.contains("wsl")
        })
        .unwrap_or(false)
}

fn list_tokens() -> Fallible {
    let tokens = Tokens::load()?;
    if tokens.is_empty() {
        println!("no tokens. `sbxd pair` makes one.");
        return Ok(());
    }
    println!("{:<24} {:<12} CREATED", "NAME", "HASH");
    for entry in tokens.list() {
        println!(
            "{:<24} {:<12} {}",
            entry.name,
            &entry.hash[..12],
            entry.created_at
        );
    }
    Ok(())
}

fn revoke(name: &str) -> Fallible {
    let mut tokens = Tokens::load()?;
    match tokens.revoke(name)? {
        0 => Err(format!("no token named `{name}`; see `sbxd tokens`").into()),
        1 => {
            println!("revoked `{name}`");
            Ok(())
        }
        n => {
            println!("revoked {n} tokens named `{name}`");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_host_is_a_name_and_never_empty() {
        let host = default_host();
        assert!(!host.is_empty());
        assert!(!host.contains(char::is_whitespace), "{host}");
    }

    /// The hint is only useful where it applies, and misleading everywhere
    /// else -- it tells you to dial something other than what was printed.
    #[test]
    fn the_wsl_hint_appears_only_under_wsl() {
        assert_eq!(wsl_hint().is_some(), is_wsl());
    }
}
