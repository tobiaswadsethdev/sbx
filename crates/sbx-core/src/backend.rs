//! Where a session's work actually happens, and the two answers to that.
//!
//! Every session up to now ran in a sandbox: the gateway created it, an exec
//! reached into it, and the policy the gateway enforced was the product. This
//! module is the seam that lets a second kind exist -- a plain `git worktree` on
//! the server, running with the server's own rights -- without either kind
//! being a special case anywhere above it. [`crate::ops`], [`crate::git`],
//! [`crate::files`] and [`crate::seed`] all talk to a [`Backend`] now, and none
//! of them ask which one they have.
//!
//! **The isolation is the product, so its absence is stated rather than
//! implied.** [`Isolation`] is on the trait for one reason: a worktree session
//! has no policy to show and no decisions to feed, and an empty policy pane
//! looks exactly like one that failed to load. Everything that would render a
//! guarantee asks for the isolation first and says which kind it is looking at.
//!
//! What the two backends differ in is small and entirely about *where*:
//!
//! | | Sandboxed | Worktree |
//! | --- | --- | --- |
//! | `exec` | `openshell sandbox exec` | a child process on the server |
//! | [`Paths::repo`] | `/sandbox/repo` | the worktree's own directory |
//! | [`Paths::sbx`] | `/sandbox/.sbx` | under the server's state directory |
//! | tmux | in the sandbox, on the image's config | on the server |
//! | policy, events | the gateway's | absent |
//! | publish | pushes from inside, credential never on the host | the server's own git credentials |
//!
//! Everything else -- the diff, the poll, the status scrape, the file tree, the
//! review, the shells -- is a script that runs somewhere, and the somewhere is
//! this trait's business rather than theirs.

use openshell_client::{
    Error as OsError, ExecOutput, OpenShell, PolicyRevision, PolicyUpdate, Provider,
};

use crate::session::{self, Session};

mod sandboxed;
mod worktree;

pub use sandboxed::Sandboxed;
pub use worktree::Worktree;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The gateway said no, or could not be reached.
    #[error(transparent)]
    Gateway(#[from] OsError),
    /// Something on the server itself: a command that would not spawn, a
    /// directory that is not there.
    #[error("{0}")]
    Local(String),
    /// Asked of a backend that has no such thing. Its own variant because the
    /// answer a caller gives for it is an explanation rather than an error --
    /// see [`Isolation`].
    #[error("{0}")]
    Unsupported(String),
}

impl Error {
    /// Whether this is "there is nothing there", which several callers treat as
    /// the state they were trying to reach rather than as a failure.
    pub fn is_missing(&self) -> bool {
        matches!(self, Error::Gateway(OsError::NotFound(_)))
    }

    fn local(e: impl std::fmt::Display) -> Self {
        Error::Local(e.to_string())
    }
}

/// What a session is isolated by, which is the one thing the two backends do
/// not have in common.
///
/// A product whose pitch is isolation cannot have a mode where the isolation is
/// quietly absent, so this is carried everywhere a session is: the list badge,
/// the policy pane, the events feed, and the sentence a publish button owes the
/// person pressing it.
// No `ts(export)`: this never crosses the wire. What a client is told is the
// session's `Kind` and, for the two requests a worktree cannot answer, a
// `no-isolation` failure carrying the sentence below -- so the wording stays
// here rather than being reimplemented beside a generated enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Isolation {
    /// A kernel-enforced sandbox with a policy the gateway applies.
    Sandboxed,
    /// None: the session runs on the server with the server's own rights.
    None,
}

impl Isolation {
    pub fn is_sandboxed(self) -> bool {
        self == Isolation::Sandboxed
    }

    /// Two or three words, for a column or a badge.
    pub fn label(self) -> &'static str {
        match self {
            Isolation::Sandboxed => "sandboxed",
            Isolation::None => "not isolated",
        }
    }

    /// The sentence a client shows where a policy pane would be.
    ///
    /// Here rather than in a front end because there are two front ends and
    /// this is a statement about a guarantee: the terminal and the window have
    /// to make the same one, and a wording kept in TypeScript would be a
    /// second answer to what a session promises.
    pub fn explain(self) -> &'static str {
        match self {
            Isolation::Sandboxed => "every outbound request goes through the gateway's policy",
            Isolation::None => {
                "this session is a git worktree on the server, running with the \
                 server's own rights. There is no policy to enforce and no \
                 decisions to report."
            }
        }
    }
}

/// Where a session's things are, from the point of view of its own `exec`.
///
/// Two absolute paths and everything else derived from them. A sandbox has one
/// filesystem and one obvious place to put both; a worktree session has the
/// working copy where git put it and its record deliberately somewhere else --
/// see [`Worktree`] -- so the two cannot be one root plus a suffix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// The repository's working copy.
    pub repo: String,
    /// The directory holding everything sbx itself writes about the session.
    pub sbx: String,
}

impl Paths {
    /// The sandbox's paths, which the image bakes in and this cannot change.
    ///
    /// `sbx-status` writes [`session::STATUS_PATH`] from inside the image and
    /// the seeder's own script names the rest, so these are a published
    /// interface rather than a choice. There is a test below that they still
    /// agree with the constants.
    pub fn in_sandbox() -> Self {
        Paths {
            repo: session::REPO_PATH.to_string(),
            sbx: SANDBOX_SBX_DIR.to_string(),
        }
    }

    pub fn meta(&self) -> String {
        format!("{}/meta.json", self.sbx)
    }
    pub fn task(&self) -> String {
        format!("{}/task.txt", self.sbx)
    }
    pub fn status(&self) -> String {
        format!("{}/status.json", self.sbx)
    }
    pub fn seed_state(&self) -> String {
        format!("{}/seed.state", self.sbx)
    }
    pub fn seed_log(&self) -> String {
        format!("{}/seed.log", self.sbx)
    }
    pub fn seed_script(&self) -> String {
        format!("{}/seed.sh", self.sbx)
    }
}

/// The `.sbx` directory inside a sandbox. Not public: [`Paths::in_sandbox`] is
/// the way to it, so nothing outside grows a second opinion about the layout.
const SANDBOX_SBX_DIR: &str = "/sandbox/.sbx";

/// What starting a session left behind, per backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Torn {
    /// The backend removed the thing the session ran in.
    Removed,
    /// There was nothing left to remove; only the record went.
    RecordOnly,
}

/// The place a session runs.
///
/// Deliberately small, and every method on it is a *where* rather than a what.
/// The scripts are shared: one definition of the diff, the poll, the status
/// scrape and the review lives above this and is handed the paths and the tmux
/// invocation it should use. A backend that grew its own copy of the diff
/// script would be a second answer to what a diff is.
pub trait Backend {
    fn isolation(&self) -> Isolation;

    /// Which of the two kinds this is, for a record and for a badge.
    fn kind(&self) -> session::Kind;

    fn paths(&self, session: &Session) -> Paths;

    fn exec(&self, session: &Session, argv: &[&str]) -> Result<ExecOutput>;

    /// The argv a terminal emulator spawns to attach to this session.
    fn interactive_argv(&self, session: &Session, argv: &[&str]) -> Result<Vec<String>>;

    /// How to invoke tmux where this session's agent runs.
    ///
    /// The image ships a config and a sandbox exec inherits no locale, so the
    /// sandboxed form carries both; the server's tmux has the user's own config
    /// and a locale already. `-u` is in both, because it says "this terminal is
    /// UTF-8" outright rather than inferring it from an environment.
    fn tmux(&self) -> &'static str;

    /// The prefix the shells beside the agent are named with.
    ///
    /// Per session rather than global, because a worktree session's tmux is the
    /// *server's* tmux: `shell-1` there would be one name for every session on
    /// the machine, and opening a second shell in one worktree would attach to
    /// another's.
    fn shell_prefix(&self, session: &Session) -> String;

    /// Make the thing the session runs in exist: a sandbox with its policy, or
    /// a worktree and somewhere to keep its record.
    ///
    /// Takes the session `&mut` because placing it decides facts that belong on
    /// the record: which policy revision it got, and -- for a worktree -- which
    /// directory it is, which there is nowhere else to learn afterwards.
    fn place(&self, session: &mut Session, draft: &crate::ops::Draft) -> Result<()>;

    /// Everything imposed on a session that already exists: the global endpoint
    /// lists, the MCP grants, the toolchain registries.
    ///
    /// Apart from [`Backend::place`] because the record is written between the
    /// two, and it has to be: between the sandbox existing and its record being
    /// saved it is an orphan that a refresh in another process will try to
    /// adopt. Imposing MCP endpoints is a `policy update --wait`, which made
    /// that window seconds wide.
    fn configure(
        &self,
        session: &Session,
        draft: &crate::ops::Draft,
        warnings: &mut Vec<String>,
    ) -> Result<()>;

    /// The seeder's first step: put the repository where [`Paths::repo`] says
    /// it is, on the branch the session works on.
    ///
    /// A script rather than an action, because it runs inside the detached
    /// seeder along with everything else -- which is what lets a clone survive
    /// the tool that asked for it going away.
    fn fetch_script(&self, session: &Session) -> String;

    /// Whether this backend wants the skills and MCP steps.
    ///
    /// A sandbox is a fresh machine and has to be given both. A worktree
    /// session's agent is the server's own, reading the server user's
    /// `~/.claude`, and copying skills into the worktree would put them in
    /// every `git status` the agent runs.
    fn seeds_tooling(&self) -> bool {
        self.isolation().is_sandboxed()
    }

    /// Remove what this session ran in. The record is the caller's to drop.
    fn tear_down(&self, name: &str, session: Option<&Session>) -> Result<Torn>;

    /// Reconcile this backend's share of the cache against what it can see.
    ///
    /// A backend knows what "still there" means for its own kind and nothing
    /// else does: a sandbox is a phase the gateway reports, a worktree is a
    /// directory that either exists or has been deleted from under it. Both
    /// answer with the same [`crate::store::Reconciliation`], so
    /// [`crate::ops::refresh_with`] merges them without a match on the kind.
    fn live(&self, cached: Vec<Session>) -> Result<crate::store::Reconciliation>;

    /// Read a session's own record, from wherever this backend keeps it.
    fn read_meta(&self, name: &str) -> Result<Session>;

    /// The effective policy, for a backend that enforces one.
    fn policy(&self, session: &Session) -> Result<PolicyRevision> {
        Err(self.no_isolation(session))
    }

    fn policy_update(&self, session: &Session, _update: &PolicyUpdate) -> Result<()> {
        Err(self.no_isolation(session))
    }

    /// The decision log, for a backend that decides anything.
    fn logs(&self, session: &Session, _lines: usize) -> Result<String> {
        Err(self.no_isolation(session))
    }

    /// Credential providers a new session of this kind may be given.
    ///
    /// Empty rather than an error for a worktree: a provider is a secret the
    /// *gateway* swaps into a request, and there is no gateway in that path --
    /// the server's own credentials are what a worktree session pushes with.
    fn providers(&self) -> Result<Vec<Provider>> {
        Ok(Vec::new())
    }

    /// The refusal a backend with no isolation gives, in the same words
    /// everywhere.
    fn no_isolation(&self, session: &Session) -> Error {
        // Not "has no policy": the same refusal answers the events feed, and a
        // feed that said "no policy" would be answering a question nobody
        // asked. `explain` covers both, in one wording.
        Error::Unsupported(format!(
            "`{}` is {}: {}",
            session.name,
            self.isolation().label(),
            self.isolation().explain()
        ))
    }
}

/// Both backends, and which of them a session belongs to.
///
/// The one thing above this that still knows there are two. Everything that
/// works on a session takes a `&dyn Backend` and is handed the right one; the
/// two operations that span both kinds -- listing what exists and creating
/// something new -- take this.
pub struct Backends {
    sandboxed: Sandboxed,
    worktree: Worktree,
}

impl Backends {
    pub fn new(sandboxed: Sandboxed, worktree: Worktree) -> Self {
        Backends {
            sandboxed,
            worktree,
        }
    }

    /// The pair as configured: the gateway client for one, the server's
    /// worktree root for the other.
    pub fn from_config(client: Box<dyn OpenShell>, cfg: &crate::config::Config) -> Self {
        Backends::new(Sandboxed::new(client), Worktree::from_config(cfg))
    }

    pub fn for_session(&self, session: &Session) -> &dyn Backend {
        self.of_kind(session.backend)
    }

    pub fn of_kind(&self, kind: session::Kind) -> &dyn Backend {
        match kind {
            session::Kind::Sandbox => &self.sandboxed,
            session::Kind::Worktree => &self.worktree,
        }
    }

    /// The sandboxed backend, for the few callers that are about the gateway
    /// itself rather than about a session: `sbx doctor`, the provider list, the
    /// image build.
    pub fn gateway(&self) -> &dyn OpenShell {
        self.sandboxed.client()
    }

    pub fn each(&self) -> [&dyn Backend; 2] {
        [&self.sandboxed, &self.worktree]
    }
}

/// Backends for the tests that assert on the *shape* of a script.
///
/// Almost every test in this crate about a backend is about the script it
/// produces -- the diff, the poll, the seeder, the publish -- and a script is
/// pure. What was missing was a way to get a [`Backend`] without a gateway to
/// talk to, which is why this exists and why its `OpenShell` panics: a test
/// that reaches the network through one of these is a test that meant to be a
/// live test.
#[cfg(test)]
pub(crate) mod testing {
    use openshell_client::{
        CreateOpts, ExecOutput, GatewayStatus, OpenShell, PolicyRevision, PolicyUpdate, Provider,
        Result as OsResult, Sandbox,
    };

    use super::Sandboxed;

    pub(crate) fn sandboxed() -> Sandboxed {
        Sandboxed::new(Box::new(NoGateway))
    }

    struct NoGateway;

    /// Every method unreachable, on purpose. See the module comment.
    impl OpenShell for NoGateway {
        fn status(&self) -> OsResult<GatewayStatus> {
            unreachable!("no gateway in a script test")
        }
        fn create(&self, _: &CreateOpts) -> OsResult<Sandbox> {
            unreachable!("no gateway in a script test")
        }
        fn list(&self, _: Option<&str>) -> OsResult<Vec<Sandbox>> {
            unreachable!("no gateway in a script test")
        }
        fn get(&self, _: &str) -> OsResult<Sandbox> {
            unreachable!("no gateway in a script test")
        }
        fn exec(&self, _: &str, _: &[&str]) -> OsResult<ExecOutput> {
            unreachable!("no gateway in a script test")
        }
        fn delete(&self, _: &str) -> OsResult<()> {
            unreachable!("no gateway in a script test")
        }
        fn policy(&self, _: &str) -> OsResult<PolicyRevision> {
            unreachable!("no gateway in a script test")
        }
        fn policy_update(&self, _: &str, _: &PolicyUpdate) -> OsResult<()> {
            unreachable!("no gateway in a script test")
        }
        fn logs(&self, _: &str, _: usize) -> OsResult<String> {
            unreachable!("no gateway in a script test")
        }
        fn providers(&self) -> OsResult<Vec<Provider>> {
            unreachable!("no gateway in a script test")
        }
        fn interactive_argv(&self, name: &str, argv: &[&str]) -> Vec<String> {
            // The one method that is pure: it builds a command line and talks
            // to nothing, and a test about attaching wants to read it.
            let mut out = vec!["openshell".to_string()];
            out.extend(["sandbox", "exec", "-n", name, "--tty", "--"].map(String::from));
            out.extend(argv.iter().map(|a| (*a).to_string()));
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The image bakes these paths in: `sbx-status` writes `status.json` from a
    /// hook inside the sandbox, and the seeder's script is written to a fixed
    /// place so a second `sbx` can find it. If [`Paths`] drifted from the
    /// constants, the host would be reading files nothing writes.
    #[test]
    fn the_sandbox_paths_are_the_ones_the_image_uses() {
        let p = Paths::in_sandbox();
        assert_eq!(p.repo, session::REPO_PATH);
        assert_eq!(p.meta(), session::META_PATH);
        assert_eq!(p.task(), session::TASK_PATH);
        assert_eq!(p.status(), session::STATUS_PATH);
        assert_eq!(p.seed_state(), session::SEED_STATE_PATH);
        assert_eq!(p.seed_log(), session::SEED_LOG_PATH);
        assert_eq!(p.seed_script(), session::SEED_SCRIPT_PATH);
    }

    /// The sentence a client shows where the policy pane would be. It is the
    /// difference between a stated absence and a pane that looks broken, so it
    /// says which kind of session it is talking about.
    #[test]
    fn the_absent_isolation_explains_itself() {
        assert_eq!(Isolation::None.label(), "not isolated");
        let said = Isolation::None.explain();
        assert!(said.contains("worktree on the server"), "{said}");
        assert!(said.contains("server's own rights"), "{said}");
        assert!(!Isolation::None.is_sandboxed());
        assert!(Isolation::Sandboxed.is_sandboxed());
    }
}
