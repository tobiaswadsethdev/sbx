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
pub mod stream;
pub use pairing::Pairing;

use sbx_core::comments::{Comment, NewComment};
use sbx_core::events::Event;
use sbx_core::files::{Dir, FileText};
use sbx_core::git::{Against, FileDiff, Status as GitStatus};
use sbx_core::integrations::View as IntegrationsView;
use sbx_core::ops::{NewOptions, NewSession, Picked, Poll, Refreshed};
use sbx_core::policy::View as PolicyView;
use sbx_core::projects::{NewProject, Project};
use sbx_core::repos::Listing;
use sbx_core::session::Session;
use sbx_core::skills::Upload as SkillUpload;
use sbx_core::tracker::Inbox;

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
pub const VERSION: u32 = 2;

/// The port `sbxd` listens on unless told otherwise.
///
/// Here rather than in the server because three things need to agree about it:
/// the server that binds it, `sbxd pair` which puts it in the string, and
/// `sbx doctor`, which tells a Windows user what to dial. Next to the gateway's
/// own 17670 so the pair are memorable together, and out of the ephemeral range
/// so it can be bound reliably.
pub const DEFAULT_PORT: u16 = 17671;

/// What `GET /version` answers, to anyone, without a token.
///
/// Unauthenticated on purpose. A client that cannot even tell whether it is
/// talking to an `sbxd` has nothing useful to say to the user, and there is
/// nothing here worth withholding: the version of a thing you are already
/// connected to is not a secret, and the alternative is a pairing flow that
/// fails identically for a wrong token and an unsupported server.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
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
    /// The projects on this server: the repositories someone has said they are
    /// working on, which is what the worktrees are grouped under.
    Projects,
    /// Make one, from a checkout the picker found.
    NewProject(NewProject),
    /// Forget one. The worktrees in it are left alone -- a sandbox is a real
    /// thing with an agent in it, and removing one is `rm`'s job.
    ForgetProject { name: String },
    /// Git repositories on the server's disk, for starting a session from one.
    ///
    /// The server's and not the client's: the checkout is only a way of *naming*
    /// a remote, but which checkouts exist is a fact about the machine that will
    /// do the cloning, and `repo_roots` is configured there.
    Repos,
    /// What is known about one of them: git's account of the drift and the
    /// toolchains it points at, and the credentials a session here should
    /// start with. Costs subprocesses and a gateway call, so it is asked once,
    /// about the repository actually picked, rather than for every row.
    Inspect {
        path: String,
        /// The branch to measure against; `None` means the checkout's own.
        branch: Option<String>,
    },
    /// Everything a create form needs that is not about a repository.
    NewOptions,
    /// The working copy as git describes it: the branch, how far it has
    /// diverged, and what is staged and what is not.
    GitStatus { name: String },
    /// Both sides of one file's diff, for a side-by-side editor.
    GitDiff {
        name: String,
        path: String,
        against: Against,
    },
    /// Move one path into or out of the index, throw its changes away, or one
    /// of the three things that talk to the remote.
    ///
    /// One request with an operation on it rather than six: they answer the
    /// same way -- with git's own words and the status afterwards -- and a
    /// client that has to re-read the status after each is a client that will
    /// forget to after one of them.
    Git { name: String, action: GitOp },
    /// One directory of a worktree's working copy, relative to the repository
    /// root. One at a time, as a tree is expanded: a repository is tens of
    /// thousands of files and every listing is an exec.
    Files { name: String, path: String },
    /// One file of it, capped and read-only. The agent owns the working copy.
    File { name: String, path: String },
    /// The shells open beside a worktree's agent, by tmux session name.
    ///
    /// Asked of the sandbox rather than remembered by a client: what shells
    /// exist is a fact about the sandbox, and one that survives the window
    /// closing and a second window opening.
    Shells { name: String },
    /// Open another shell in the same sandbox. Answers with the list, including
    /// the new one, whose name the server chose.
    NewShell { name: String },
    /// Close one, killing whatever is running in it. The agent's own is not a
    /// shell and is refused.
    KillShell { name: String, tmux: String },
    /// A session's unsent review.
    Comments { name: String },
    /// Add one remark to it. Answers with the review as it now stands, so a
    /// client never has to guess what the server made of what it sent.
    Comment { name: String, comment: NewComment },
    /// Remove one remark by id.
    Uncomment {
        name: String,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        id: u64,
    },
    /// Send the review to the agent and forget it. One message, once: see
    /// `sbx_core::comments`.
    SendComments { name: String },
    /// Start a session, and answer as soon as the record exists rather than when
    /// the agent is running.
    ///
    /// Creating takes tens of seconds, and the states it passes through --
    /// `creating`, `seeding`, `ready` -- are already on the session and already
    /// polled. A request that waited would hold a connection open for a minute
    /// to tell a client something the list was about to tell it anyway.
    ///
    /// Boxed, and only this one: a `NewSession` is three times the size of
    /// every other request put together -- eight fields of `String` and an
    /// optional ticket -- and an unboxed variant would make every `Request`
    /// that big, including the `Poll` that goes out every second.
    Create(Box<NewSession>),

    /// What the server holds on a session's behalf: the MCP catalog and what
    /// each managed container is doing, the secret *names* it has, and the
    /// skills a client has uploaded.
    ///
    /// One request for all three, because they explain each other: a container
    /// that will not start is usually a secret that is not there.
    Integrations,
    /// Start, restart or stop one managed MCP server. Answers with
    /// [`Reply::Integrations`], re-read.
    Mcp { name: String, action: McpOp },
    /// Store a secret under a name, or forget it.
    ///
    /// `value` is `None` to forget. There is no request that *reads* one back:
    /// see [`sbx_core::secrets`].
    Secret { name: String, value: Option<String> },
    /// Push skills from a client's own `~/.claude/skills` into the server's
    /// library.
    ///
    /// Sent before a create as well as from the integrations screen, which is
    /// what keeps the pointer-not-copy property across two machines: editing a
    /// skill on the client still means the next session gets the edit.
    UploadSkills { skills: Vec<SkillUpload> },
    /// Drop one uploaded skill. The client's own copy is untouched -- the
    /// library is a cache of a directory on another machine.
    ForgetSkill { name: String },

    /// The task inbox: what the configured trackers say is assigned to you.
    ///
    /// Read server-side, with the credentials in the server's store, so a
    /// client shows a list rather than holding a token. Whatever could not be
    /// read comes back beside what could -- an inbox missing a tracker's rows
    /// is invisible otherwise.
    Tasks,
}

/// What to do to a managed MCP container.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum McpOp {
    /// Start it if it is not running. Leaves a running one alone, because
    /// restarting it would drop the agent connections of every live session
    /// using it.
    Start,
    /// Recreate it from the catalog entry, running or not. What to press after
    /// changing a secret.
    Restart,
    Stop,
}

impl Request {
    /// The session this is about, when it is about one. What the server logs
    /// and what an authorisation check would key on.
    pub fn session(&self) -> Option<&str> {
        match self {
            Request::Ls
            | Request::Repos
            | Request::Inspect { .. }
            | Request::NewOptions
            | Request::Projects
            | Request::NewProject(_)
            | Request::ForgetProject { .. }
            | Request::Integrations
            | Request::Mcp { .. }
            | Request::Secret { .. }
            | Request::UploadSkills { .. }
            | Request::ForgetSkill { .. }
            | Request::Tasks => None,
            Request::Poll { name }
            | Request::Diff { name }
            | Request::Policy { name }
            | Request::Events { name }
            | Request::GitStatus { name }
            | Request::GitDiff { name, .. }
            | Request::Git { name, .. }
            | Request::Files { name, .. }
            | Request::File { name, .. }
            | Request::Shells { name }
            | Request::NewShell { name }
            | Request::KillShell { name, .. }
            | Request::Comments { name }
            | Request::Comment { name, .. }
            | Request::Uncomment { name, .. }
            | Request::SendComments { name } => Some(name),
            Request::Create(new) => new.name.as_deref(),
        }
    }
}

/// One thing to do to the working copy or the remote.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "kebab-case")]
pub enum GitOp {
    Stage {
        path: String,
    },
    Unstage {
        path: String,
    },
    /// Destructive, and it races the agent by definition -- it may be part-way
    /// through writing the file this restores. The client asks first.
    Discard {
        path: String,
    },
    Commit {
        message: String,
    },
    Push,
    Pull,
    Fetch,
}

/// What the server sends back when it worked.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
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
    /// The policy as facts, not as a rendering.
    ///
    /// A `PolicyView` rather than the gateway's own `PolicyRevision`, which
    /// keeps `openshell-client` off the wire entirely. That matters more than
    /// it sounds: those types belong to a `0.0.x` project, and putting them in
    /// the protocol would make their churn protocol churn. The view also
    /// carries the two things the revision does not -- the template the session
    /// was created from, and the global lists -- so a client has everything the
    /// pane needs from one request.
    Policy(PolicyView),
    Events {
        events: Vec<Event>,
    },
    Comments {
        comments: Vec<Comment>,
    },
    Shells {
        shells: Vec<String>,
    },
    Files(Dir),
    File(FileText),
    /// The status after whatever was asked for, and what git said while doing
    /// it. Both, because a push that succeeded still has output worth reading
    /// and a status alone would not say the push had happened at all.
    Git {
        said: String,
        status: GitStatus,
    },
    GitDiff(FileDiff),
    /// What was actually said to the agent, so a client can show the message it
    /// sent rather than a claim that it sent one.
    Told {
        message: String,
    },
    Projects {
        projects: Vec<Project>,
    },
    Repos(Listing),
    Inspect(Picked),
    NewOptions(NewOptions),
    /// The session's name, which the client already sent -- echoed because it is
    /// what every later request about the session is keyed on, and because a
    /// server that derived one would otherwise leave the client guessing.
    Created {
        name: String,
    },
    /// The whole integrations view, which is what every action on it answers
    /// with rather than an acknowledgement: starting a container or storing a
    /// secret changes what the rest of the screen says.
    Integrations(IntegrationsView),
    /// The inbox, and whatever could not be read.
    Tasks(Inbox),
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub kind: FailureKind,
    /// Written for a person. The client shows it rather than composing its own.
    pub message: String,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
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
    /// The session has no isolation, so the thing asked for does not exist for
    /// it: a worktree session has no policy and no decision feed.
    ///
    /// Its own kind rather than a [`Self::Failed`], because it is not a failure
    /// and a client must not draw it as one. The `message` is the server's
    /// explanation and the client shows it where the pane would have been --
    /// which is the difference between a stated absence and a pane that looks
    /// like it could not load.
    NoIsolation,
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

    pub fn no_isolation(message: impl Into<String>) -> Self {
        Self {
            kind: FailureKind::NoIsolation,
            message: message.into(),
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
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
            (Request::Integrations, "integrations"),
            (
                Request::Mcp {
                    name: "jira".into(),
                    action: McpOp::Restart,
                },
                "mcp",
            ),
            (
                Request::UploadSkills { skills: Vec::new() },
                "upload-skills",
            ),
            (Request::Tasks, "tasks"),
        ] {
            let v: serde_json::Value = serde_json::to_value(&req).unwrap();
            assert_eq!(v["op"], op, "{req:?}");
        }
    }

    /// **A secret goes one way.** There is no request that reads one back and
    /// no reply that carries a value, and this is the test that says so: the
    /// integrations view is the whole of what a client learns about them, and it
    /// carries names and whether each is set.
    #[test]
    fn nothing_on_the_wire_carries_a_secret_value_back() {
        let set = Request::Secret {
            name: "SENTRY_TOKEN".into(),
            value: Some("sntrys_abc".into()),
        };
        let json = serde_json::to_string(&set).unwrap();
        assert!(json.contains("sntrys_abc"), "it goes in: {json}");

        // And the reply to it is the view, whose secret rows are a name and a
        // boolean. Asserted on the type's own shape rather than on a string, so
        // a field added to `secrets::Named` has to come past this test.
        let view = IntegrationsView {
            mcp: Vec::new(),
            secrets: vec![sbx_core::secrets::Named {
                name: "SENTRY_TOKEN".into(),
                set: true,
                used_by: vec!["sentry".into()],
            }],
            skills: Vec::new(),
            configured_skills: Vec::new(),
        };
        let json = serde_json::to_string(&Reply::Integrations(view)).unwrap();
        assert!(json.contains("SENTRY_TOKEN"), "{json}");
        assert!(
            !json.contains("sntrys"),
            "a value must never be in a reply: {json}"
        );
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
            usage: None,
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

    /// Three things agree about this: the server that binds it, the pairing
    /// string, and the advice `sbx doctor` gives a Windows client.
    #[test]
    fn the_default_port_is_not_the_gateways() {
        assert_eq!(DEFAULT_PORT, 17671);
        assert_ne!(DEFAULT_PORT, 17670, "that is the openshell gateway");
    }

    /// The policy reply carries a view, and the view carries the two things the
    /// gateway's own revision does not: the template, and the global lists.
    #[test]
    fn a_policy_reply_carries_a_view_and_not_a_revision() {
        let view = PolicyView {
            template: Some("feature-work".into()),
            revision: sbx_core::policy::Revision {
                version: 1,
                active_version: 1,
                settled: true,
                source: None,
                hash: None,
            },
            network: Some(Vec::new()),
            lists: None,
            locked: None,
        };
        let out: Outcome = Reply::Policy(view.clone()).into();

        let json = serde_json::to_string(&out).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["ok"]["reply"], "policy");
        assert_eq!(v["ok"]["template"], "feature-work");

        match serde_json::from_str::<Outcome>(&json)
            .unwrap()
            .into_result()
            .unwrap()
        {
            Reply::Policy(back) => assert_eq!(back, view),
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
