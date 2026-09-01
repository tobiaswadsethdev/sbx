//! The wire between a client and `sbxd`.
//!
//! One definition of every message, so that the server, the CLI talking to a
//! remote one, and -- once there is a UI -- the TypeScript generated from these
//! types cannot drift apart. Two hand-maintained copies of a message is the
//! failure that makes a self-hosted client and server miserable, and it is worth
//! a crate to avoid.
//!
//! **The types on the wire are the core's own.** [`sbx_core::session::Session`]
//! is what a client is sent, rather than a `SessionDto` beside it that has to be
//! kept in step by hand. That couples the protocol to the core's structs, which
//! is a real cost -- renaming a field is a protocol break -- and [`VERSION`] is
//! what makes the break loud instead of silent. A second definition would only
//! have moved the coupling somewhere a compiler cannot see it.
//!
//! Requests are named for what a *client* wants, not for the function that
//! serves them, which is why there is no `Refresh`: reconciling the cache
//! against the gateway is how the server answers [`Request::Ls`], and a client
//! has no way to want one without the other.

use serde::{Deserialize, Serialize};

pub mod pairing;
pub use pairing::Pairing;

use openshell_client::PolicyRevision;
use sbx_core::endpoints::Lists;
use sbx_core::events::Event;
use sbx_core::ops::{Poll, Refreshed};
use sbx_core::session::Session;

/// The protocol this build speaks.
///
/// A shipped desktop application and a server somebody updated separately
/// *will* disagree eventually. [`Hello`] carries this from an endpoint that
/// needs no token, so a client can say "this server speaks 2, I speak 1" rather
/// than failing in the middle of a request with something unhelpful.
///
/// Bump it whenever an existing message changes shape. Adding a [`Request`]
/// variant does not need a bump: an older server answers an unknown request
/// with [`Failure::unsupported`], which is a better error than a version check
/// would have produced anyway.
pub const VERSION: u32 = 1;

/// What `GET /version` answers, to anyone, without a token.
///
/// Unauthenticated on purpose. A client that cannot even tell whether it is
/// talking to an `sbxd` has nothing useful to say to the user, and there is
/// nothing here worth withholding: the version of a thing you are already
/// connected to is not a secret, and the alternative is a pairing flow that
/// fails identically for a wrong token and an unsupported server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// Always `"sbxd"`. What distinguishes this from any other server that
    /// happens to answer on the port.
    pub server: String,
    /// [`VERSION`], as the server understands it.
    pub protocol: u32,
    /// The `sbxd` release, for a human reading an error message.
    pub version: String,
}

impl Hello {
    pub fn current() -> Self {
        Self {
            server: "sbxd".into(),
            protocol: VERSION,
            version: env!("CARGO_PKG_VERSION").into(),
        }
    }

    /// Whether a client speaking [`VERSION`] can talk to this server.
    pub fn is_sbxd(&self) -> bool {
        self.server == "sbxd"
    }

    pub fn speaks(&self, version: u32) -> bool {
        self.protocol == version
    }
}

/// Something a client asks the server to do.
///
/// Tagged by `op` rather than positionally, so a message stays readable in a log
/// and an unknown variant is a name rather than an index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Request {
    /// Every session, reconciled against the gateway first.
    Ls,
    /// What the agent is doing, and how far the working copy has moved.
    Poll { name: String },
    /// The three diff sections, as a marked-up body.
    Diff { name: String },
    /// The policy the gateway is enforcing for a session.
    Policy { name: String },
    /// The allow/deny feed, newest first.
    Events { name: String },
}

impl Request {
    /// The session this is about, when it is about one. What the server logs
    /// and what an authorisation check would key on.
    pub fn session(&self) -> Option<&str> {
        match self {
            Request::Ls => None,
            Request::Poll { name }
            | Request::Diff { name }
            | Request::Policy { name }
            | Request::Events { name } => Some(name),
        }
    }
}

/// What the server sends back when it worked.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "kebab-case")]
pub enum Reply {
    Ls {
        sessions: Vec<Session>,
        /// Sessions recovered from a sandbox the cache did not know about, and
        /// ones whose sandbox has just gone. Carried because reconciliation is
        /// the server's to do and the client would otherwise see sessions
        /// appear and vanish with no account of why.
        adopted: Vec<String>,
        dead: Vec<String>,
        warnings: Vec<String>,
    },
    /// Carried whole rather than restated field by field, which is the
    /// point of putting the core's types on the wire: `Poll` gaining a field
    /// is then one change, not two that have to be noticed.
    Poll(Poll),
    Diff {
        body: String,
    },
    Policy {
        revision: PolicyRevision,
        /// The template the session was created from, which is recorded rather
        /// than derived and is not in the revision.
        template: Option<String>,
        /// The global allow and block lists, which apply to every *new* session
        /// and are therefore not in the revision either. Sent because the pane
        /// that answers "what may this reach?" is wrong without them: a standing
        /// decision applied to every session and visible in none of them is the
        /// kind of state that becomes a bug report about the gateway.
        lists: Lists,
    },
    Events {
        events: Vec<Event>,
    },
}

impl From<Refreshed> for Reply {
    fn from(r: Refreshed) -> Self {
        Reply::Ls {
            sessions: r.sessions,
            adopted: r.adopted,
            dead: r.dead,
            warnings: r.warnings,
        }
    }
}

impl From<Poll> for Reply {
    fn from(p: Poll) -> Self {
        Reply::Poll(p)
    }
}

/// What the server sends back when it did not work.
///
/// A message and a kind, rather than only a message: a client showing a
/// stale-session error wants to drop it from the list, and one showing a
/// gateway error wants to keep it and say the gateway is unreachable. Matching
/// on rendered English to tell those apart is how a client ends up wrong.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub kind: FailureKind,
    /// Written for a person. The client shows it rather than composing its own.
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailureKind {
    /// No session by that name, on the server or at the gateway.
    NoSuchSession,
    /// The gateway refused, or could not be reached at all.
    Gateway,
    /// A request this server does not have, which is what an older server says
    /// to a newer client rather than failing to parse it.
    Unsupported,
    /// Anything else the server could not do.
    Failed,
}

impl Failure {
    pub fn no_such_session(name: &str) -> Self {
        Self {
            kind: FailureKind::NoSuchSession,
            message: format!("no session named `{name}`"),
        }
    }

    pub fn gateway(message: impl Into<String>) -> Self {
        Self {
            kind: FailureKind::Gateway,
            message: message.into(),
        }
    }

    pub fn unsupported(op: &str) -> Self {
        Self {
            kind: FailureKind::Unsupported,
            message: format!("this sbxd does not support `{op}`; it speaks protocol {VERSION}"),
        }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            kind: FailureKind::Failed,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Failure {}

/// One reply, either way.
///
/// An envelope rather than an HTTP status, because a request that failed for a
/// reason the client should act on is not a transport failure: the round trip
/// worked. Statuses stay for the things that really are transport -- no token,
/// a body that is not a request at all.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    /// Boxed only to keep the two variants a similar size. `Box<T>` is
    /// transparent to serde, so the bytes on the wire are the same either way.
    Ok(Box<Reply>),
    Error(Failure),
}

impl From<Reply> for Outcome {
    fn from(r: Reply) -> Self {
        Outcome::Ok(Box::new(r))
    }
}

impl From<Failure> for Outcome {
    fn from(f: Failure) -> Self {
        Outcome::Error(f)
    }
}

impl Outcome {
    pub fn into_result(self) -> Result<Reply, Failure> {
        match self {
            Outcome::Ok(r) => Ok(*r),
            Outcome::Error(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_round_trips_by_name() {
        let r = Request::Diff {
            name: "readme-fix".into(),
        };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#"{"op":"diff","name":"readme-fix"}"#);
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), r);
    }

    /// The tag is what an older server matches on to answer `unsupported`
    /// rather than failing to parse, so it has to be readable and stable.
    #[test]
    fn requests_are_tagged_with_a_name_not_a_number() {
        for (req, op) in [
            (Request::Ls, "ls"),
            (Request::Poll { name: "a".into() }, "poll"),
            (Request::Diff { name: "a".into() }, "diff"),
            (Request::Policy { name: "a".into() }, "policy"),
            (Request::Events { name: "a".into() }, "events"),
        ] {
            let v: serde_json::Value = serde_json::to_value(&req).unwrap();
            assert_eq!(v["op"], op, "{req:?}");
        }
    }

    #[test]
    fn every_request_but_ls_names_a_session() {
        assert_eq!(Request::Ls.session(), None);
        assert_eq!(
            Request::Events {
                name: "readme-fix".into()
            }
            .session(),
            Some("readme-fix")
        );
    }

    /// A failure has to survive the round trip as a *kind*, because that is what
    /// the client branches on. Losing it to a string would be silent.
    #[test]
    fn a_failure_keeps_its_kind_across_the_wire() {
        let out: Outcome = Failure::no_such_session("gone").into();
        let json = serde_json::to_string(&out).unwrap();
        let back: Outcome = serde_json::from_str(&json).unwrap();
        let err = back.into_result().unwrap_err();
        assert_eq!(err.kind, FailureKind::NoSuchSession);
        assert_eq!(err.message, "no session named `gone`");
    }

    #[test]
    fn an_ok_outcome_is_a_reply() {
        let out: Outcome = Reply::Diff {
            body: "### committed\n".into(),
        }
        .into();
        let json = serde_json::to_string(&out).unwrap();
        let back: Outcome = serde_json::from_str(&json).unwrap();
        match back.into_result().unwrap() {
            Reply::Diff { body } => assert_eq!(body, "### committed\n"),
            other => panic!("{other:?}"),
        }
    }

    /// An internally tagged enum can only hold a variant that serialises as a
    /// map, and serde only says so at runtime. `Poll` is the one variant carried
    /// whole, so this is where that would break.
    #[test]
    fn a_poll_carried_whole_round_trips_under_the_tag() {
        let out: Outcome = Reply::from(Poll {
            stat: Some(sbx_core::ops::DiffStat {
                added: 12,
                removed: 3,
                untracked: 1,
            }),
            status: None,
            pane: Some("? for shortcuts".into()),
        })
        .into();

        let json = serde_json::to_string(&out).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["ok"]["reply"], "poll", "the tag sits beside the fields");

        match serde_json::from_str::<Outcome>(&json)
            .unwrap()
            .into_result()
            .unwrap()
        {
            Reply::Poll(p) => {
                assert_eq!(p.stat.unwrap().added, 12);
                assert_eq!(p.pane.as_deref(), Some("? for shortcuts"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn hello_says_what_it_is_and_what_it_speaks() {
        let h = Hello::current();
        assert!(h.is_sbxd());
        assert!(h.speaks(VERSION));
        assert!(!h.speaks(VERSION + 1));

        // Something else answering on the port is the case this exists for.
        let nginx = Hello {
            server: "nginx".into(),
            protocol: 1,
            version: "1.29".into(),
        };
        assert!(!nginx.is_sbxd());
    }

    /// The reason `Hello` is its own message and not a bare integer: it has to
    /// survive being parsed by a *newer* client than the server that sent it.
    #[test]
    fn hello_parses_without_the_fields_a_later_version_might_add() {
        let h: Hello =
            serde_json::from_str(r#"{"server":"sbxd","protocol":1,"version":"0.2.0"}"#).unwrap();
        assert_eq!(h.protocol, 1);
    }
}
