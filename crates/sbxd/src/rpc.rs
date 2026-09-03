//! One [`Request`] in, one [`Outcome`] out.
//!
//! The whole of what the server *does*, and deliberately thin: every arm is a
//! call into [`sbx_core::ops`], which is the same function the CLI calls. A
//! behaviour that exists here and not there would be one the terminal cannot
//! do, which is how two front ends start disagreeing about what a session is.
//!
//! Nothing here is async. The core talks to the gateway by running a
//! subprocess, so every one of these blocks for a few hundred milliseconds;
//! [`crate::serve`] is what keeps that off the runtime's threads.

use std::path::Path;

use openshell_client::CliClient;
use sbx_core::backend::{Backend, Backends, Isolation};
use sbx_core::session::Session;
use sbx_core::store::Store;
use sbx_core::{
    comments, config, endpoints, files, git, image, ops, policy, projects, repos, secrets, skills,
};
use sbx_proto::{Failure, GitOp, McpOp, Outcome, Reply, Request};

/// Answer one request.
///
/// Every arm that is about one session asks [`Backends::for_session`] which
/// backend it belongs to and then does exactly what it did before. The two
/// exceptions are [`Request::Policy`] and [`Request::Events`], which are about
/// the isolation itself and so are the two things a worktree session cannot
/// answer -- and they say so rather than coming back empty.
pub fn dispatch(backends: &Backends, request: Request) -> Outcome {
    match request {
        Request::Ls => ls(backends),
        Request::Poll { name } => {
            with_session(&name, |s| Ok(ops::poll(backends.for_session(s), s).into()))
        }
        Request::Diff { name } => with_session(&name, |s| {
            Ok(Reply::Diff {
                body: ops::repo_diff(backends.for_session(s), s),
            })
        }),
        Request::Policy { name } => with_session(&name, |s| policy(backends.for_session(s), s)),
        Request::Events { name } => with_session(&name, |s| events(backends.for_session(s), s)),
        Request::GitStatus { name } => with_session(&name, |s| {
            git::status(backends.for_session(s), s)
                .map(|status| Reply::Git {
                    said: String::new(),
                    status,
                })
                .map_err(Failure::failed)
        }),
        Request::GitDiff {
            name,
            path,
            against,
        } => with_session(&name, |s| {
            git::file_diff(backends.for_session(s), s, &path, against)
                .map(Reply::GitDiff)
                .map_err(Failure::failed)
        }),
        Request::Git { name, action } => with_session(&name, |s| {
            let said = match action {
                GitOp::Stage { path } => {
                    git::stage(backends.for_session(s), s, &path).map(|_| String::new())
                }
                GitOp::Unstage { path } => {
                    git::unstage(backends.for_session(s), s, &path).map(|_| String::new())
                }
                GitOp::Discard { path } => {
                    git::discard(backends.for_session(s), s, &path).map(|_| String::new())
                }
                GitOp::Commit { message } => git::commit(backends.for_session(s), s, &message),
                GitOp::Push => git::push(backends.for_session(s), s),
                GitOp::Pull => git::pull(backends.for_session(s), s),
                GitOp::Fetch => git::fetch(backends.for_session(s), s),
            }
            .map_err(Failure::failed)?;
            // Re-read rather than assume: the agent is editing while this runs,
            // so the status after a stage is not the status before it plus one
            // entry.
            let status = git::status(backends.for_session(s), s).map_err(Failure::failed)?;
            Ok(Reply::Git { said, status })
        }),
        Request::Files { name, path } => with_session(&name, |s| {
            files::list(backends.for_session(s), s, &path)
                .map(Reply::Files)
                .map_err(Failure::failed)
        }),
        Request::File { name, path } => with_session(&name, |s| {
            files::read(backends.for_session(s), s, &path)
                .map(Reply::File)
                .map_err(Failure::failed)
        }),
        Request::Shells { name } => with_session(&name, |s| {
            ops::shells(backends.for_session(s), s)
                .map(|shells| Reply::Shells { shells })
                .map_err(Failure::gateway)
        }),
        Request::NewShell { name } => with_session(&name, |s| {
            ops::new_shell(backends.for_session(s), s).map_err(Failure::failed)?;
            ops::shells(backends.for_session(s), s)
                .map(|shells| Reply::Shells { shells })
                .map_err(Failure::gateway)
        }),
        Request::KillShell { name, tmux } => with_session(&name, |s| {
            ops::kill_shell(backends.for_session(s), s, &tmux).map_err(Failure::failed)?;
            ops::shells(backends.for_session(s), s)
                .map(|shells| Reply::Shells { shells })
                .map_err(Failure::gateway)
        }),
        Request::Comments { name } => with_session(&name, |s| Ok(review(comments::list(&s.name)))),
        Request::Comment { name, comment } => with_session(&name, |s| {
            comments::add(&s.name, comment)
                .map(review)
                .map_err(Failure::failed)
        }),
        Request::Uncomment { name, id } => with_session(&name, |s| {
            comments::remove(&s.name, id)
                .map(review)
                .map_err(Failure::failed)
        }),
        Request::SendComments { name } => with_session(&name, |s| {
            ops::send_comments(backends.for_session(s), s)
                .map(|message| Reply::Told { message })
                .map_err(Failure::failed)
        }),
        Request::Projects => Reply::Projects {
            projects: projects::list(),
        }
        .into(),
        Request::NewProject(new) => match projects::add(new) {
            Ok(projects) => Reply::Projects { projects }.into(),
            Err(e) => Failure::failed(e).into(),
        },
        Request::ForgetProject { name } => match projects::remove(&name) {
            Ok(projects) => Reply::Projects { projects }.into(),
            Err(e) => Failure::failed(e).into(),
        },
        Request::Repos => repo_list(),
        Request::Inspect { path, branch } => inspect(backends, &path, branch.as_deref()),
        Request::NewOptions => match config::Config::load() {
            Ok(cfg) => Reply::NewOptions(ops::new_options(backends, &cfg)).into(),
            Err(e) => Failure::failed(format!("could not read the config file: {e}")).into(),
        },
        Request::Create(new) => create(new),

        // The integrations screen. Every one of these answers with the whole
        // view rather than an acknowledgement, for the reason the git view does
        // the same: they explain each other, and a client adjusting the list it
        // had would be inventing an answer.
        Request::Integrations => integrations(),
        Request::Mcp { name, action } => match mcp_action(&name, action) {
            Ok(()) => integrations(),
            Err(e) => Failure::failed(e).into(),
        },
        Request::Secret { name, value } => {
            let done = match value {
                Some(v) => secrets::set(&name, &v),
                None => secrets::forget(&name),
            };
            match done {
                Ok(()) => integrations(),
                Err(e) => Failure::failed(e).into(),
            }
        }
        Request::UploadSkills { skills } => upload_skills(skills),
        Request::ForgetSkill { name } => match skills::forget(&skills::library_dir(), &name) {
            Ok(()) => integrations(),
            Err(e) => Failure::failed(e).into(),
        },
    }
}

/// The MCP catalog, the secret names and the skill library, as the server sees
/// them now.
fn integrations() -> Outcome {
    match config::Config::load() {
        Ok(cfg) => Reply::Integrations(sbx_core::integrations::view(&cfg)).into(),
        Err(e) => Failure::failed(format!("could not read the config file: {e}")).into(),
    }
}

/// Start, restart or stop one managed server.
///
/// The catalog is the config file's, so a name a client sends is looked up
/// rather than trusted: `stop` takes a container name, and one derived from an
/// arbitrary string would let a client stop any container on the host whose name
/// begins with the prefix.
fn mcp_action(name: &str, action: McpOp) -> Result<(), String> {
    let cfg = config::Config::load().map_err(|e| format!("could not read the config file: {e}"))?;
    let entry = cfg
        .mcp()
        .iter()
        .find(|e| e.name() == name)
        .ok_or_else(|| format!("no mcp server named `{name}` in the config file"))?;
    if !entry.is_managed() {
        return Err(format!(
            "`{name}` is a url this server does not run, so there is nothing here to {}",
            match action {
                McpOp::Stop => "stop",
                _ => "start",
            }
        ));
    }
    match action {
        McpOp::Start => sbx_core::mcp::ensure(std::slice::from_ref(entry))
            .into_iter()
            .next()
            .map_or(Ok(()), Err),
        McpOp::Restart => sbx_core::mcp::start(entry),
        McpOp::Stop => sbx_core::mcp::stop(entry.name()),
    }
}

/// Take a client's skills into the library.
///
/// Every one is attempted: a client uploads its whole `~/.claude/skills` before
/// a create, and one skill that has grown a virtualenv should not stop the rest
/// arriving. What went wrong comes back as a failure only when *nothing*
/// landed, since a screen that re-reads the view can see for itself what is
/// there.
fn upload_skills(uploads: Vec<sbx_core::skills::Upload>) -> Outcome {
    let dir = skills::library_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Failure::failed(format!("could not make {}: {e}", dir.display())).into();
    }
    let mut problems = Vec::new();
    let mut installed = 0;
    for upload in &uploads {
        match skills::install(&dir, upload) {
            Ok(_) => installed += 1,
            Err(e) => problems.push(e),
        }
    }
    if installed == 0 && !problems.is_empty() {
        return Failure::failed(problems.join("; ")).into();
    }
    if !problems.is_empty() {
        eprintln!("sbxd: some skills were not stored: {}", problems.join("; "));
    }
    integrations()
}

fn review(comments: Vec<comments::Comment>) -> Reply {
    Reply::Comments { comments }
}

/// The repositories on this machine, and where it looked for them.
fn repo_list() -> Outcome {
    let cfg = config::Config::load().unwrap_or_default();
    let roots = repos::roots(cfg.repo_roots.as_deref());
    let repos = repos::discover_in(&roots);
    Reply::Repos(repos::Listing {
        roots: roots.iter().map(|r| r.path.display().to_string()).collect(),
        repos,
    })
    .into()
}

/// What is known about the repository a client has picked.
///
/// The provider half fails softly, like the list in [`ops::new_options`]: a
/// gateway that cannot be reached leaves nothing ticked, which is a form you
/// can still fill in, rather than an error against a question that was mostly
/// about git.
fn inspect(backends: &Backends, path: &str, branch: Option<&str>) -> Outcome {
    let path = Path::new(path);
    let checkout = repos::read(path);
    // `None` means the checkout's own branch, which is what the request says it
    // means -- and `inspect` cannot work that out for itself, because it is
    // handed a path rather than the record `read` produces. Left unresolved it
    // reports every branch as missing from the remote, and a form built on that
    // silently falls back to the remote's default.
    let branch = branch
        .map(str::to_string)
        .or_else(|| checkout.branch.clone());
    let facts = repos::inspect(path, branch.as_deref());
    let cfg = config::Config::load().unwrap_or_default();

    // An explicit list in the config file replaces this rather than adding to
    // it, so there is nothing to work out when there is one.
    let providers = if !cfg.providers().is_empty() {
        Vec::new()
    } else {
        let origin = checkout.origin.clone();
        let choices: Vec<ops::ProviderChoice> = backends
            .gateway()
            .providers()
            .unwrap_or_default()
            .into_iter()
            .map(|p| ops::ProviderChoice {
                name: p.name,
                kind: p.kind,
            })
            .collect();
        let sessions: Result<Vec<Session>, _> =
            Store::load().map(|s| s.list().into_iter().cloned().collect());
        let used = match (&origin, &sessions) {
            (Some(url), Ok(list)) => ops::providers_used_for(url, list),
            _ => Vec::new(),
        };
        ops::preselect_providers(&choices, origin.as_deref(), &used)
    };

    Reply::Inspect(ops::Picked {
        facts,
        branch: checkout.branch,
        providers,
    })
    .into()
}

/// Start a session, and answer before it has finished starting.
///
/// Everything that can be judged from the request is judged here, so a name
/// with a slash in it or a toolchain nobody has heard of comes back as an error
/// against the request that caused it. What is left is tens of seconds of
/// gateway and network, and that runs on a thread: the states it passes through
/// are on the session, and the session is already polled.
fn create(new: sbx_core::ops::NewSession) -> Outcome {
    let cfg = match config::Config::load() {
        Ok(c) => c,
        Err(e) => return Failure::failed(format!("could not read the config file: {e}")).into(),
    };

    // Built here and not on the thread: naming, validating and resolving the
    // toolchains are everything that can fail on the client's account, and they
    // belong to the request that caused them.
    let draft = match new.into_draft(&cfg) {
        Ok(d) => d,
        Err(e) => return Failure::failed(e).into(),
    };
    let name = draft.name.clone();

    std::thread::spawn(move || {
        // The CLI builds the image before creating, because the build streams
        // docker's output to a terminal. There is no terminal here, so it
        // happens on this thread with the session sitting in `creating` -- which
        // is what a client watching the list sees either way, only for longer
        // the first time a set of toolchains is used.
        let backends = backends();
        // Only a sandbox has an image. A worktree session runs on the server
        // with the server's toolchains, which is both its point and its
        // limitation.
        if draft.backend == sbx_core::session::Kind::Sandbox
            && let Err(e) = image::ensure_for(&draft.toolchains)
        {
            eprintln!("sbxd: {}: could not build the image: {e}", draft.name);
            return;
        }
        // The managed MCP containers, before the seeder registers them with the
        // agent. Here rather than in `ops::create` for the same reason the image
        // build is here: it is a side effect on the host with output of its own,
        // and `ops` is what both front ends share. A server that will not start
        // is a warning -- the session is still worth having, and the agent will
        // report the tool as unreachable, which the events pane explains.
        for warning in sbx_core::mcp::ensure(cfg.mcp()) {
            eprintln!("sbxd: {}: {warning}", draft.name);
        }
        // Progress is dropped rather than reported: the steps map onto states
        // the record already carries, and a channel per create would be a second
        // way to learn the same thing.
        if let Err(e) = ops::create(&backends, &draft, &mut |_| {}) {
            eprintln!("sbxd: {}: could not create the session: {e}", draft.name);
        }
    });

    Reply::Created { name }.into()
}

fn ls(backends: &Backends) -> Outcome {
    // `repair` is false: it costs one exec per session left mid-lifecycle, and
    // is the right thing for a tool that starts, prints and exits. A server
    // answering this every second for every connected client would spend an
    // exec a second on a question that only changes when a create dies. The
    // repair happens once, at startup, in `serve`.
    match ops::refresh_with(backends, false) {
        Ok(refreshed) => Reply::from(refreshed).into(),
        Err(e) => Failure::gateway(e.to_string()).into(),
    }
}

/// The policy pane's contents, or the sentence that says why there is no pane.
///
/// Not a `Failed`: a session with no isolation has no policy in the same way it
/// has no sandbox, and drawing that as an error would make an ordinary worktree
/// session look broken every time its dock is opened.
fn policy(backend: &dyn Backend, session: &Session) -> Result<Reply, Failure> {
    let revision = ops::policy(backend, session).map_err(|e| no_isolation(backend, e))?;
    Ok(Reply::Policy(policy::View::of(
        &revision,
        session.policy.as_deref(),
        &lists(),
    )))
}

fn events(backend: &dyn Backend, session: &Session) -> Result<Reply, Failure> {
    let events = ops::events(backend, session).map_err(|e| no_isolation(backend, e))?;
    Ok(Reply::Events { events })
}

/// Which failure the two panes above report.
///
/// One function, because the pane and the feed are the same question asked
/// twice and a client that got two different kinds for it would draw one of
/// them wrong.
fn no_isolation(backend: &dyn Backend, message: String) -> Failure {
    match backend.isolation() {
        Isolation::Sandboxed => Failure::gateway(message),
        Isolation::None => Failure::no_isolation(message),
    }
}

/// Both backends, as this server holds them.
///
/// Built per use rather than kept in a `static`: a `CliClient` is a path and two
/// options, the worktree backend is two paths, and building them costs a config
/// read. What that buys is an `sbxd` that picks up an edited `config.toml`
/// without a restart, which is the same promise every other read here makes.
pub fn backends() -> Backends {
    let cfg = config::Config::load().unwrap_or_default();
    let mut client = CliClient::default();
    if let Some(g) = &cfg.gateway {
        client = client.with_gateway(g.clone());
    }
    Backends::from_config(Box::new(client), &cfg)
}

/// Look a session up by name, and answer for it.
///
/// The lookup is against the cache rather than the gateway, which is what
/// `require_session` in the CLI does too: the cache is reconciled by `Ls`, and
/// a name that is not in it is a client asking about something that has gone.
fn with_session(name: &str, f: impl FnOnce(&Session) -> Result<Reply, Failure>) -> Outcome {
    let store = match Store::load() {
        Ok(s) => s,
        Err(e) => return Failure::failed(format!("could not read the session cache: {e}")).into(),
    };
    let Some(session) = store.get(name) else {
        return Failure::no_such_session(name).into();
    };
    match f(session) {
        Ok(reply) => reply.into(),
        Err(failure) => failure.into(),
    }
}

/// The global allow and block lists, for the policy reply.
///
/// Empty on a read failure rather than fatal, for the reason `sbx policy` does
/// the same: the point of asking is the sandbox's own rules, and losing them to
/// an unreadable convenience file is the wrong trade.
fn lists() -> endpoints::Lists {
    endpoints::Lists::load().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cache is a real file in a real home directory, so the only arm that
    /// can be exercised without one is the miss -- which is also the one a
    /// client hits most, since a session it knew about can go at any time.
    #[test]
    fn a_request_for_a_session_that_is_not_there_says_so_by_kind() {
        let out = with_session("definitely-not-a-session-4f2a", |_| {
            panic!("should not have been called")
        });
        let err = out.into_result().unwrap_err();
        assert_eq!(err.kind, sbx_proto::FailureKind::NoSuchSession);
        assert!(err.message.contains("definitely-not-a-session-4f2a"));
    }

    /// Every request names the op it failed on, so a client can tell an
    /// unsupported request from a broken one.
    #[test]
    fn an_unsupported_op_names_itself_and_the_protocol() {
        let f = Failure::unsupported("attach");
        assert_eq!(f.kind, sbx_proto::FailureKind::Unsupported);
        assert!(f.message.contains("attach"), "{}", f.message);
        assert!(
            f.message.contains(&sbx_proto::VERSION.to_string()),
            "{}",
            f.message
        );
    }
}
