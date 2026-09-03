//! The backend this tool exists for: a session inside a kernel-enforced
//! sandbox, with the gateway applying a policy to everything that leaves it.
//!
//! Nothing here is new. It is what [`crate::ops`] did directly against
//! [`OpenShell`] before there was a second kind of session, moved behind the
//! trait so that the second kind could exist without the first growing an `if`.
//! The comments that explain *why* each step is ordered as it is came with it.

use openshell_client::{
    CreateOpts, Error as OsError, ExecOutput, OpenShell, PolicyRevision, PolicyUpdate, Provider,
};

use super::{Backend, Error, Isolation, Paths, Result, Torn};
use crate::endpoints;
use crate::mcp;
use crate::ops::Draft;
use crate::policy;
use crate::seed;
use crate::session::{self, SELECTOR_MANAGED, Session};
use crate::store;
use crate::toolchain;

pub struct Sandboxed {
    client: Box<dyn OpenShell>,
}

impl Sandboxed {
    pub fn new(client: Box<dyn OpenShell>) -> Self {
        Sandboxed { client }
    }

    /// The gateway itself, for the callers that are about the gateway rather
    /// than about a session: `sbx doctor`, the image build, the provider list.
    pub fn client(&self) -> &dyn OpenShell {
        self.client.as_ref()
    }

    /// Apply the global allow and block lists to a sandbox that has just been
    /// made.
    ///
    /// A failed *block* fails the create. The two directions are not symmetric
    /// and pretending they are would be the worst kind of bug this tool can
    /// have: an allow that did not land leaves a session that cannot reach
    /// something, which the events pane will say out loud the moment the agent
    /// tries; a block that did not land leaves a session that *can* reach
    /// something the user asked to be unreachable, and nothing will ever mention
    /// it again. So the first is a warning and the second is fatal.
    ///
    /// Costs one `policy update --wait` -- about six seconds -- and only when
    /// the lists are not empty, which is the common case for anyone who has
    /// never touched them.
    fn impose_lists(&self, sandbox: &str, warnings: &mut Vec<String>) -> Result<()> {
        let lists = match endpoints::Lists::load() {
            Ok(l) => l,
            // An unreadable list is not a reason to refuse to create a session,
            // but it is a reason to say so: the session will not have the rules
            // its owner thinks every session has.
            Err(e) => {
                warnings.push(format!(
                    "could not read the global endpoint lists, so none were applied: {e}"
                ));
                return Ok(());
            }
        };
        let updates = lists.updates();
        if updates.is_empty() {
            return Ok(());
        }

        for update in &updates {
            let Err(e) = self.client.policy_update(sandbox, update) else {
                continue;
            };
            if !update.remove_endpoints.is_empty() {
                return Err(Error::Local(format!(
                    "the global block list could not be applied, so {} would have been reachable: {e}",
                    update.remove_endpoints.join(", ")
                )));
            }
            warnings.push(format!(
                "the global allow list could not be applied, so {} is not reachable: {e}",
                update.add_endpoints.join(", ")
            ));
        }
        Ok(())
    }

    /// Open the endpoints of the session's MCP servers.
    ///
    /// Separate from [`Self::impose_lists`] and after it, because the two answer
    /// different questions -- that one is "what have I decided every session may
    /// reach", this one is "what tools does the agent have" -- and because a
    /// failure here means something different: an MCP server that could not be
    /// opened leaves a session whose agent starts, works, and reports a dead
    /// tool. Worth a warning, not worth refusing to create the session over,
    /// which is the same reading as a failed allow.
    fn impose_mcp(&self, sandbox: &str, servers: &[mcp::Server], warnings: &mut Vec<String>) {
        let Some(update) = mcp::widen(servers) else {
            return;
        };
        if let Err(e) = self.client.policy_update(sandbox, &update) {
            warnings.push(format!(
                "the mcp endpoints could not be opened, so the agent will report {} unreachable: {e}",
                servers
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    /// Open the package registries the session's toolchains need.
    ///
    /// A warning rather than a failure, on the same reading as
    /// [`Self::impose_mcp`]: a registry that could not be opened leaves a
    /// session whose agent builds against what is already vendored and reports a
    /// denial the moment it restores. The events pane says so out loud, which is
    /// the test for whether a failure is worth refusing to create a session
    /// over -- a *block* that does not land is silent, and that is the one that
    /// is fatal.
    ///
    /// Costs one `policy update --wait` per distinct binary list -- about six
    /// seconds each, and at most one per toolchain. Only for a session that
    /// asked for one.
    fn impose_toolchains(
        &self,
        sandbox: &str,
        chains: &[&'static toolchain::Toolchain],
        warnings: &mut Vec<String>,
    ) {
        for update in toolchain::updates(chains) {
            if let Err(e) = self.client.policy_update(sandbox, &update) {
                warnings.push(format!(
                    "the toolchain registries could not be opened, so {} is not \
                     reachable and a restore will be denied: {e}",
                    update.add_endpoints.join(", ")
                ));
            }
        }
    }
}

impl Backend for Sandboxed {
    fn isolation(&self) -> Isolation {
        Isolation::Sandboxed
    }

    fn kind(&self) -> session::Kind {
        session::Kind::Sandbox
    }

    fn paths(&self, _session: &Session) -> Paths {
        Paths::in_sandbox()
    }

    fn exec(&self, session: &Session, argv: &[&str]) -> Result<ExecOutput> {
        Ok(self.client.exec(&session.sandbox, argv)?)
    }

    fn interactive_argv(&self, session: &Session, argv: &[&str]) -> Result<Vec<String>> {
        Ok(self.client.interactive_argv(&session.sandbox, argv))
    }

    /// The gateway does not pass the image's environment through to an exec, so
    /// a tmux client started this way inherits no locale: tmux then assumes a
    /// terminal that is not UTF-8, draws box rules with the DEC line-drawing set
    /// and replaces every character it cannot map with `_`. That is what turned
    /// Claude Code's banner and its `⏸` and `❯` glyphs into underscores. `-u`
    /// says "this terminal is UTF-8" outright; the locale is exported as well
    /// because everything else in the sandbox reads it -- git for one -- and
    /// `COLORTERM` is how the agent decides it may use 24-bit colour.
    fn tmux(&self) -> &'static str {
        "LANG=C.UTF-8 LC_ALL=C.UTF-8 COLORTERM=truecolor tmux -u -f /etc/tmux.conf"
    }

    /// One sandbox, one namespace: `shell-1` here cannot collide with anything,
    /// because nothing else runs in this filesystem.
    fn shell_prefix(&self, _session: &Session) -> String {
        "shell-".to_string()
    }

    fn place(&self, session: &mut Session, draft: &Draft) -> Result<()> {
        // Resolved before anything is created, so a typo in the policy fails
        // before a sandbox exists rather than after. The guard owns a temp file
        // when the policy came from a template, so it has to outlive the create
        // call below -- which is why it is resolved here and not by the caller.
        let resolved = policy::resolve(&draft.policy).map_err(Error::local)?;
        session.policy = Some(resolved.label.clone());

        let opts = CreateOpts {
            name: session.sandbox.clone(),
            labels: session.labels(),
            policy: Some(resolved.path().to_path_buf()),
            providers: draft.providers.clone(),
            // The base image for a session with no toolchain, and the variant
            // carrying exactly the ones asked for otherwise. Not built here, for
            // the reason the base image is not: see `ops::create` on docker's
            // output.
            from: Some(toolchain::tag(&draft.toolchains)),
            // Keep the sandbox alive after the create command exits.
            command: vec!["true".into()],
            ..Default::default()
        };
        self.client.create(&opts)?;
        Ok(())
    }

    fn configure(
        &self,
        session: &Session,
        draft: &Draft,
        warnings: &mut Vec<String>,
    ) -> Result<()> {
        // The global lists, imposed before anything runs inside the sandbox.
        //
        // Here rather than by editing the policy before `sandbox create`,
        // because `--policy` may be the user's own YAML file and this has to
        // work whatever shape it is in. The window between the sandbox existing
        // and the rules landing is real, and it is empty: nothing is launched in
        // it until the seeder.
        self.impose_lists(&session.sandbox, warnings)?;
        self.impose_mcp(&session.sandbox, &session.mcp, warnings);
        self.impose_toolchains(&session.sandbox, &draft.toolchains, warnings);
        Ok(())
    }

    fn fetch_script(&self, session: &Session) -> String {
        seed::clone_and_branch(session, &self.paths(session))
    }

    fn tear_down(&self, name: &str, session: Option<&Session>) -> Result<Torn> {
        // Through the cache with a fall back to the naming convention, so a
        // session the cache has lost is still removable -- that is what the
        // convention is for.
        let sandbox = session
            .map(|s| s.sandbox.clone())
            .unwrap_or_else(|| session::sandbox_name(name));

        match self.client.delete(&sandbox) {
            Ok(()) => Ok(Torn::Removed),
            // The desired end state rather than a failure: that is the case for
            // a session left behind by a create that died before provisioning,
            // and refusing to remove the record would make it permanent.
            Err(OsError::NotFound(_)) => Ok(Torn::RecordOnly),
            Err(e) => Err(e.into()),
        }
    }

    fn live(&self, cached: Vec<Session>) -> Result<store::Reconciliation> {
        let live = self.client.list(Some(SELECTOR_MANAGED))?;
        Ok(store::reconcile(cached, &live))
    }

    fn read_meta(&self, name: &str) -> Result<Session> {
        seed::read_meta(self.client.as_ref(), &session::sandbox_name(name))
            .map_err(|e| Error::Local(e.to_string()))
    }

    fn policy(&self, session: &Session) -> Result<PolicyRevision> {
        Ok(self.client.policy(&session.sandbox)?)
    }

    fn policy_update(&self, session: &Session, update: &PolicyUpdate) -> Result<()> {
        Ok(self.client.policy_update(&session.sandbox, update)?)
    }

    fn logs(&self, session: &Session, lines: usize) -> Result<String> {
        Ok(self.client.logs(&session.sandbox, lines)?)
    }

    fn providers(&self) -> Result<Vec<Provider>> {
        Ok(self.client.providers()?)
    }
}
