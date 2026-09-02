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

use openshell_client::{CliClient, OpenShell};
use sbx_core::session::Session;
use sbx_core::store::Store;
use sbx_core::{config, endpoints, image, ops, policy, repos};
use sbx_proto::{Failure, Outcome, Reply, Request};

/// Answer one request.
pub fn dispatch(client: &dyn OpenShell, request: Request) -> Outcome {
    match request {
        Request::Ls => ls(client),
        Request::Poll { name } => with_session(&name, |s| Ok(ops::poll(client, s).into())),
        Request::Diff { name } => with_session(&name, |s| {
            Ok(Reply::Diff {
                body: ops::repo_diff(client, s),
            })
        }),
        Request::Policy { name } => with_session(&name, |s| policy(client, s)),
        Request::Events { name } => with_session(&name, |s| events(client, s)),
        Request::Repos => repo_list(),
        Request::Inspect { path, branch } => inspect(client, &path, branch.as_deref()),
        Request::NewOptions => match config::Config::load() {
            Ok(cfg) => Reply::NewOptions(ops::new_options(client, &cfg)).into(),
            Err(e) => Failure::failed(format!("could not read the config file: {e}")).into(),
        },
        Request::Create(new) => create(new),
    }
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
fn inspect(client: &dyn OpenShell, path: &str, branch: Option<&str>) -> Outcome {
    let path = Path::new(path);
    let facts = repos::inspect(path, branch);
    let cfg = config::Config::load().unwrap_or_default();

    // An explicit list in the config file replaces this rather than adding to
    // it, so there is nothing to work out when there is one.
    let providers = if !cfg.providers().is_empty() {
        Vec::new()
    } else {
        let origin = repos::read(path).origin;
        let choices: Vec<ops::ProviderChoice> = client
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

    Reply::Inspect(ops::Picked { facts, providers }).into()
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
        let client = CliClient::default();
        if let Err(e) = image::ensure_for(&draft.toolchains) {
            eprintln!("sbxd: {}: could not build the image: {e}", draft.name);
            return;
        }
        // Progress is dropped rather than reported: the steps map onto states
        // the record already carries, and a channel per create would be a second
        // way to learn the same thing.
        if let Err(e) = ops::create(&client, &draft, &mut |_| {}) {
            eprintln!("sbxd: {}: could not create the session: {e}", draft.name);
        }
    });

    Reply::Created { name }.into()
}

fn ls(client: &dyn OpenShell) -> Outcome {
    // `repair` is false: it costs one exec per session left mid-lifecycle, and
    // is the right thing for a tool that starts, prints and exits. A server
    // answering this every second for every connected client would spend an
    // exec a second on a question that only changes when a create dies. The
    // repair happens once, at startup, in `serve`.
    match ops::refresh_with(client, false) {
        Ok(refreshed) => Reply::from(refreshed).into(),
        Err(e) => Failure::gateway(e.to_string()).into(),
    }
}

fn policy(client: &dyn OpenShell, session: &Session) -> Result<Reply, Failure> {
    let revision = ops::policy(client, session).map_err(Failure::gateway)?;
    Ok(Reply::Policy(policy::View::of(
        &revision,
        session.policy.as_deref(),
        &lists(),
    )))
}

fn events(client: &dyn OpenShell, session: &Session) -> Result<Reply, Failure> {
    let events = ops::events(client, session).map_err(Failure::gateway)?;
    Ok(Reply::Events { events })
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
