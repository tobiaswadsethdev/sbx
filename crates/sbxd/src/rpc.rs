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

use openshell_client::OpenShell;
use sbx_core::store::Store;
use sbx_core::{endpoints, ops, session::Session};
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
    }
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
    Ok(Reply::Policy {
        revision,
        template: session.policy.clone(),
        lists: lists(),
    })
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
