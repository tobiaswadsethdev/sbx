//! MCP servers `sbxd` runs itself.
//!
//! A configured [`super::Server`] is a URL: something is listening there, and
//! starting it was somebody's problem. That is fine for a server somebody else
//! operates and miserable for the ordinary case, which was a `docker run`
//! incantation copied out of `docs/mcp.md` -- with the credential on the command
//! line -- and re-typed after every reboot. A **managed** entry is the same
//! server described instead of started: an image, a port, arguments, plain
//! environment, and the *names* of secrets the store holds. `sbxd` starts it,
//! restarts it, and can say what it is doing.
//!
//! The URL is derived rather than configured, and that is the point: a managed
//! server is reachable at `http://sbx-mcp-<name>:<port>/mcp` because this module
//! is what named the container and what joined it to the gateway's network. The
//! two mistakes that shape used to invite -- a container on the default bridge
//! that no sandbox can resolve, and a `localhost` URL that means the sandbox
//! itself -- are unreachable from here.
//!
//! **What runs in the container is not sandboxed by anything.** It is an
//! ordinary container on the host's Docker daemon, holding the credential it was
//! given, with whatever network access that daemon allows. The isolation this
//! tool sells is around the *agent*; an MCP server is a thing the agent is
//! granted an endpoint to, and everything that server can do is something the
//! agent can now do with the host's credentials. That is a fine trade for Jira
//! and a terrible one for a filesystem server, which `docs/mcp.md` says at
//! length and the window now says at the moment a server is added.

use std::collections::BTreeMap;
use std::process::Command;

use serde::{Deserialize, Serialize};

use super::{NETWORK, Server, Transport};

/// The prefix every managed container's name carries.
///
/// A prefix rather than the bare name, for the reason the sandboxes have one:
/// `docker ps` on the host shows containers somebody started by hand beside
/// these, and a name collision would have `sbxd` restarting something that is
/// not its own.
pub const CONTAINER_PREFIX: &str = "sbx-mcp-";

/// The path a managed server serves. The one part of the URL that is a
/// convention rather than a fact -- every MCP server in the wild serves `/mcp`,
/// and an image that does not can be pointed at with a plain `url` entry.
const PATH: &str = "/mcp";

/// The container this entry runs as.
pub fn container_name(name: &str) -> String {
    format!("{CONTAINER_PREFIX}{name}")
}

/// How `sbxd` runs one MCP server.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Managed {
    /// The image, tag included. Not resolved or pulled here: `docker run` pulls
    /// what it does not have, and a digest pinned in the config file is the
    /// user's decision to make.
    pub image: String,
    /// The port the server listens on *inside* the container. Nothing is
    /// published to the host: a sandbox reaches it by container name on the
    /// gateway's network, and publishing it would put an authenticated MCP
    /// server on the host's interfaces for no one's benefit.
    pub port: u16,
    /// Arguments after the image.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment that is not secret: an organisation slug, a base URL, a log
    /// level.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Names in the server's secret store, passed as environment variables of
    /// the same name. Names only -- the values live in [`crate::secrets`] and
    /// never travel to a client.
    #[serde(default)]
    pub secrets: Vec<String>,
}

/// One catalog entry: what a session is given, and how it is run if `sbxd` runs
/// it.
///
/// The [`Server`] is what goes into a session's record and what the agent is
/// pointed at, unchanged from when every entry was a URL somebody else
/// operated. `managed` is `None` for exactly those.
// `McpEntry` on the wire: every exported type lands in one flat directory,
// and `files::Entry` and `git::Entry` are already in it. See the note in
// `scripts/gen-bindings.sh`.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, rename = "McpEntry"))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub server: Server,
    #[serde(default)]
    pub managed: Option<Managed>,
}

impl Entry {
    /// An entry for a server somebody else runs: the shape every entry had
    /// before this module existed.
    pub fn external(server: Server) -> Self {
        Entry {
            server,
            managed: None,
        }
    }

    /// An entry `sbxd` runs, with the URL derived from the container it will
    /// start.
    ///
    /// Validated through [`Server::parse`] like any other, so a name this
    /// module would turn into an unusable container name fails against the
    /// config file rather than against Docker.
    pub fn managed(
        name: &str,
        transport: Transport,
        managed: Managed,
    ) -> Result<Self, super::Error> {
        if managed.image.trim().is_empty() {
            return Err(super::Error::NoImage);
        }
        if managed.port == 0 {
            return Err(super::Error::BadPort("0".into()));
        }
        let url = format!(
            "http://{}:{}{PATH}",
            container_name(name.trim()),
            managed.port
        );
        let server = Server::parse(name, &url, transport)?;
        Ok(Entry {
            server,
            managed: Some(managed),
        })
    }

    pub fn name(&self) -> &str {
        &self.server.name
    }

    pub fn is_managed(&self) -> bool {
        self.managed.is_some()
    }
}

/// What a managed container is doing.
// `McpState`, not `State`: `session::State` has that name.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, rename = "McpState"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Running, and on the gateway's network -- which is the only sense in
    /// which a sandbox can reach it.
    Running,
    /// Running, but not attached to the gateway's network. Its own state
    /// because it is the failure that looks fine in `docker ps` and reports
    /// itself as an authentication problem in the agent.
    Detached,
    /// Started, exited, and started again -- an image that cannot stay up.
    ///
    /// Its own state because `--restart unless-stopped` makes this the *worst*
    /// one to read from `.State.Running` alone: Docker reports a container it
    /// is in the middle of restarting as running, so a container crash-looping
    /// on a bad argument every two seconds looked healthy. Measured: seven
    /// restarts in, `Running` was `true` and `Restarting` was `true` on one
    /// call and `false` on the next.
    Crashing,
    /// It exists and is not running.
    Stopped,
    /// No container of that name. The normal state before the first start.
    Absent,
    /// Docker itself could not be asked.
    Unknown,
}

/// One entry as a screen shows it.
// `McpStatus`, not `Status`: `git::Status` has that name.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, rename = "McpStatus"))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub name: String,
    /// The URL the agent is given, which for a managed entry is derived.
    pub url: String,
    /// Whether `sbxd` runs it. An external entry has a state of `Unknown` and
    /// nothing to press.
    pub managed: bool,
    pub image: Option<String>,
    pub state: State,
    /// The container's name, for the person who is about to run `docker logs`.
    pub container: Option<String>,
    /// Secret names this entry asks for, and whether the store has each.
    pub secrets: Vec<crate::secrets::Named>,
    /// What is wrong, if anything: a missing secret, a container that exited, a
    /// network it is not on. Written for a person and shown as it is.
    pub problem: Option<String>,
    /// The last few lines of the container's own output, when it is not
    /// running. The only thing that ever says *why* an image exited, and
    /// otherwise a `docker logs` away on a machine the person may not be on.
    pub log: Option<String>,
}

/// How many lines of a stopped container's log to carry.
///
/// Enough for a stack trace's first frames or an "invalid token" line, bounded
/// because this rides in an RPC reply that a screen re-reads after every action.
const LOG_LINES: usize = 20;

/// Ask Docker about every entry.
///
/// One `docker inspect` per managed entry, which is a few milliseconds each and
/// only happens when a screen is open or `doctor` runs. External entries are
/// reported without asking anything: the host cannot reach a container by name,
/// so there is nothing to ask that would not be a lie.
pub fn statuses(entries: &[Entry]) -> Vec<Status> {
    let stored = crate::secrets::names();
    entries.iter().map(|e| status(e, &stored)).collect()
}

/// What `docker inspect` said, in the shape this module reasons about.
struct Inspected {
    state: State,
    /// How many times Docker has restarted it. Non-zero with a *running*
    /// container is the crash loop above.
    restarts: u32,
}

fn status(entry: &Entry, stored: &[String]) -> Status {
    let secrets: Vec<crate::secrets::Named> = entry
        .managed
        .iter()
        .flat_map(|m| m.secrets.iter())
        .map(|name| crate::secrets::Named {
            name: name.clone(),
            set: stored.iter().any(|s| s == name),
            used_by: vec![entry.name().to_string()],
        })
        .collect();
    let missing: Vec<&str> = secrets
        .iter()
        .filter(|s| !s.set)
        .map(|s| s.name.as_str())
        .collect();

    let Some(m) = &entry.managed else {
        return Status {
            name: entry.name().to_string(),
            url: entry.server.url.clone(),
            managed: false,
            image: None,
            state: State::Unknown,
            container: None,
            secrets,
            problem: None,
            log: None,
        };
    };

    let container = container_name(entry.name());
    let Inspected { state, restarts } = inspect(&container);
    // A missing secret first, because it is the cause of most of the states
    // below: a container that exits immediately usually exited because the
    // credential it needed was not there.
    let problem = if !missing.is_empty() {
        Some(format!(
            "no value stored for {}; set it and restart",
            missing.join(", ")
        ))
    } else {
        match state {
            State::Running => None,
            State::Crashing => Some(format!(
                "it has restarted {restarts} times: the image starts and exits. \
                 The last of its own output is below."
            )),
            State::Detached => Some(format!(
                "running, but not on the `{NETWORK}` network, so no sandbox can resolve it"
            )),
            State::Stopped => Some("the container is not running".into()),
            State::Absent => Some("not started yet".into()),
            State::Unknown => Some("docker could not be asked".into()),
        }
    };
    // Whenever the container has something to answer for. Its own output is the
    // only thing that ever says *why* an image will not stay up, and this is
    // the one place a person reading a screen on another machine can see it.
    let log = matches!(state, State::Stopped | State::Crashing)
        .then(|| logs(&container))
        .flatten();

    Status {
        name: entry.name().to_string(),
        url: entry.server.url.clone(),
        managed: true,
        image: Some(m.image.clone()),
        state,
        container: Some(container),
        secrets,
        problem,
        log,
    }
}

/// Start one, replacing a container of the same name if there is one.
///
/// `docker rm -f` first rather than `docker start`, because the reason to press
/// start is usually that something changed -- a secret, an argument, the image
/// tag -- and starting the old container would silently keep the old
/// definition. The container is the deployment of a catalog entry, so it is
/// recreated from it every time.
///
/// The secrets are passed as `--env NAME` with the value in *this process's*
/// environment rather than as `--env NAME=value`, so no credential appears in
/// the argument list -- `ps` on the host, and the error message Docker prints
/// when a run fails, would otherwise both carry it.
pub fn start(entry: &Entry) -> Result<(), String> {
    let Some(m) = &entry.managed else {
        return Err(format!(
            "`{}` is a url this server does not run; start it wherever it lives",
            entry.name()
        ));
    };
    let container = container_name(entry.name());

    let mut missing = Vec::new();
    let mut passed: Vec<(String, String)> = Vec::new();
    for name in &m.secrets {
        match crate::secrets::get(name) {
            Some(v) => passed.push((name.clone(), v)),
            None => missing.push(name.clone()),
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "no value stored for {}; set it first",
            missing.join(", ")
        ));
    }

    let _ = docker(&["rm", "-f", &container], &[]);

    let mut argv: Vec<String> = vec![
        "run".into(),
        "-d".into(),
        "--name".into(),
        container.clone(),
        "--network".into(),
        NETWORK.to_string(),
        // Survives a reboot of the host without `sbxd` having to notice, which
        // is what "managed" has to mean for something an agent depends on.
        "--restart".into(),
        "unless-stopped".into(),
        // Nothing is published: a sandbox reaches it by name on the network
        // above, and the host has no business reaching it at all.
        "--label".into(),
        "sbx.mcp=true".into(),
    ];
    for (k, v) in &m.env {
        argv.push("--env".into());
        argv.push(format!("{k}={v}"));
    }
    for (k, _) in &passed {
        // Name only: `docker run --env NAME` takes the value from this
        // process's environment, which is where the value is put below.
        argv.push("--env".into());
        argv.push(k.clone());
    }
    argv.push(m.image.clone());
    argv.extend(m.args.iter().cloned());

    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let out = docker(&refs, &passed)?;
    if out.status.success() {
        return Ok(());
    }
    Err(format!(
        "could not start `{container}`: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

/// Stop one and take the container away.
///
/// Removed rather than stopped, so `start` above is the only thing that ever
/// creates one and there is no half-state where a container exists with a
/// definition nothing in the config file describes.
pub fn stop(name: &str) -> Result<(), String> {
    let container = container_name(name);
    let out = docker(&["rm", "-f", &container], &[])?;
    if out.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&out.stderr);
    // Already gone is the desired end state. Case-insensitive for the reason
    // `inspect` above is: Docker changed the capital between versions.
    let lowered = said.to_ascii_lowercase();
    if lowered.contains("no such container") || lowered.contains("no such object") {
        return Ok(());
    }
    Err(format!("could not stop `{container}`: {}", said.trim()))
}

/// Start every managed entry that is not already running.
///
/// Called when `sbxd` starts and before a session is seeded. Idempotent by
/// design: an entry that is already running is left exactly alone, because
/// restarting it would drop the agent connections of every live session that is
/// using it.
///
/// Returns what could not be started, as warnings. A dead MCP server is a
/// session whose agent reports a tool that needs authentication, which is worth
/// saying out loud -- and is not worth refusing to create a session over.
pub fn ensure(entries: &[Entry]) -> Vec<String> {
    let mut warnings = Vec::new();
    for entry in entries.iter().filter(|e| e.is_managed()) {
        let container = container_name(entry.name());
        // Only a healthy one is left alone. A crash-looping container is one
        // Docker is already restarting, so pressing start on it should recreate
        // it from the catalog -- which is usually what has changed.
        if inspect(&container).state == State::Running {
            continue;
        }
        if let Err(e) = start(entry) {
            warnings.push(format!("mcp `{}`: {e}", entry.name()));
        }
    }
    warnings
}

/// Ask Docker what a container is doing, in one call.
///
/// The network is part of the answer rather than a second question: "running"
/// and "running where a sandbox can resolve it" are different states, and the
/// difference is the whole failure mode this feature has.
fn inspect(container: &str) -> Inspected {
    let plain = |state| Inspected { state, restarts: 0 };
    let Ok(out) = Command::new("docker")
        .args([
            "inspect",
            container,
            "--format",
            "{{.State.Running}} {{.RestartCount}} \
             {{range $k, $v := .NetworkSettings.Networks}}{{$k}} {{end}}",
        ])
        .output()
    else {
        return plain(State::Unknown);
    };
    if !out.status.success() {
        // Docker distinguishes the two, and so must this: a daemon that is not
        // answering is not the same as a container that is not there.
        //
        // Lowercased before matching, and that is not defensive: 29.7.2 says
        // `error: no such object: sbx-mcp-probe` where older versions said
        // `Error response from daemon: No such object`. Matching the capital
        // reported every container that had never been started as "docker could
        // not be asked", which sends someone to look at their daemon.
        let said = String::from_utf8_lossy(&out.stderr).to_ascii_lowercase();
        return plain(
            if said.contains("no such object") || said.contains("no such container") {
                State::Absent
            } else {
                State::Unknown
            },
        );
    }
    let said = String::from_utf8_lossy(&out.stdout);
    let mut parts = said.split_whitespace();
    let running = parts.next() == Some("true");
    let restarts: u32 = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
    let networks: Vec<&str> = parts.collect();

    let state = if !running {
        State::Stopped
    } else if restarts > 0 {
        // Deliberately ahead of the network check: an image that will not stay
        // up is the thing to fix first, and a restarting container has no
        // networks listed at all on some of the calls.
        State::Crashing
    } else if networks.contains(&NETWORK) {
        State::Running
    } else {
        State::Detached
    };
    Inspected { state, restarts }
}

fn logs(container: &str) -> Option<String> {
    let out = Command::new("docker")
        .args(["logs", "--tail", &LOG_LINES.to_string(), container])
        .output()
        .ok()?;
    // Images write to both, and which one is not this module's business.
    let mut said = String::from_utf8_lossy(&out.stdout).into_owned();
    said.push_str(&String::from_utf8_lossy(&out.stderr));
    let said = said.trim().to_string();
    (!said.is_empty()).then_some(said)
}

/// Run `docker`, with any secret values placed in the child's environment
/// rather than in its arguments.
fn docker(argv: &[&str], env: &[(String, String)]) -> Result<std::process::Output, String> {
    let mut cmd = Command::new("docker");
    cmd.args(argv);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output()
        .map_err(|e| format!("could not run docker: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn managed() -> Managed {
        Managed {
            image: "ghcr.io/example/mcp-jira:1.2".into(),
            port: 9000,
            args: vec!["--verbose".into()],
            env: BTreeMap::from([("JIRA_SITE".to_string(), "example".to_string())]),
            secrets: vec!["JIRA_TOKEN".into()],
        }
    }

    /// The URL is derived, which is what removes the two mistakes a
    /// hand-written one invites: a container name nothing resolves, and a
    /// `localhost` that means the sandbox itself.
    #[test]
    fn a_managed_entry_derives_its_url_from_the_container_it_will_start() {
        let e = Entry::managed("jira", Transport::Http, managed()).unwrap();
        assert_eq!(e.server.url, "http://sbx-mcp-jira:9000/mcp");
        assert_eq!(e.server.endpoint, "sbx-mcp-jira:9000");
        assert_eq!(container_name("jira"), "sbx-mcp-jira");
        assert!(e.is_managed());
    }

    /// Refused against the config file, not against Docker: a name that is not
    /// a name would become a container name Docker rejects, three steps from
    /// the line that caused it.
    #[test]
    fn an_entry_that_could_not_be_run_is_refused_where_it_is_written() {
        let bad_name = Entry::managed("jira issues", Transport::Http, managed());
        assert!(bad_name.is_err(), "a space is not a container name");

        let mut no_image = managed();
        no_image.image = "  ".into();
        assert!(Entry::managed("jira", Transport::Http, no_image).is_err());

        let mut no_port = managed();
        no_port.port = 0;
        assert!(Entry::managed("jira", Transport::Http, no_port).is_err());
    }

    /// A secret the store does not have is the first thing said about an entry,
    /// because it is the cause of most of the other states: a container that
    /// exits on startup usually exited for want of the credential.
    #[test]
    fn a_missing_secret_outranks_whatever_the_container_is_doing() {
        let e = Entry::managed("jira", Transport::Http, managed()).unwrap();
        // No store, so nothing is set. Docker is not consulted for the verdict.
        let s = status(&e, &[]);
        assert_eq!(s.secrets.len(), 1);
        assert!(!s.secrets[0].set);
        let problem = s.problem.expect("a problem");
        assert!(problem.contains("JIRA_TOKEN"), "{problem}");
        assert!(problem.contains("set it"), "{problem}");

        let ok = status(&e, &["JIRA_TOKEN".to_string()]);
        assert!(ok.secrets[0].set);
        // Whatever docker said, it is no longer about the secret.
        assert!(
            ok.problem.as_deref() != Some(problem.as_str()),
            "{:?}",
            ok.problem
        );
    }

    /// An external entry is a URL somebody else operates: there is nothing to
    /// start and nothing honest to say about its state from here.
    #[test]
    fn an_external_entry_has_nothing_to_press() {
        let e = Entry::external(
            Server::parse("jira", "http://mcp-jira:9000/mcp", Transport::Http).unwrap(),
        );
        let s = status(&e, &[]);
        assert!(!s.managed);
        assert_eq!(s.state, State::Unknown);
        assert_eq!(s.container, None);
        assert_eq!(s.problem, None);
        assert!(start(&e).is_err(), "there is nothing to start");
    }
}
