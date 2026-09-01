//! A typed client for the `openshell` CLI.
//!
//! Everything the rest of `sbx` knows about OpenShell goes through the
//! [`OpenShell`] trait. OpenShell is a fast-moving v0.0.x project, so keeping
//! the CLI's surface behind one trait means version churn lands in exactly one
//! file, and lets the gRPC API replace the subprocess later without touching
//! callers.
//!
//! Behaviour verified by hand against 0.0.110 (see `docs/manual-loop.md`):
//!
//! * errors exit 1, write a message to stderr, and leave stdout empty
//! * `sandbox exec` propagates the remote exit code verbatim and keeps
//!   stdout and stderr separate
//! * `--output json` is supported by `status`, `sandbox list` and `sandbox get`

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::Command;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not run `{bin}`: {source}")]
    Spawn {
        bin: String,
        #[source]
        source: std::io::Error,
    },

    #[error("sandbox `{0}` not found")]
    NotFound(String),

    #[error("`openshell {args}` failed (exit {code}): {stderr}")]
    Cli {
        args: String,
        code: i32,
        stderr: String,
    },

    #[error("could not parse `openshell {args}` output as JSON: {source}")]
    Parse {
        args: String,
        #[source]
        source: serde_json::Error,
    },
}

/// Sandbox lifecycle phase as reported by the gateway.
///
/// Taken from the `SANDBOX_PHASE_*` enum in the gateway binary rather than
/// guessed: the JSON carries the TitleCase form of each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Provisioning,
    Starting,
    Ready,
    Stopping,
    /// Compute released, workspace retained.
    Stopped,
    Deleting,
    Error,
    /// The gateway's own "no idea" phase.
    Unknown,
    /// A phase this build does not know about, kept verbatim.
    Other(String),
}

impl From<&str> for Phase {
    fn from(s: &str) -> Self {
        match s {
            "Provisioning" => Phase::Provisioning,
            "Starting" => Phase::Starting,
            "Ready" => Phase::Ready,
            "Stopping" => Phase::Stopping,
            "Stopped" => Phase::Stopped,
            "Deleting" => Phase::Deleting,
            "Error" => Phase::Error,
            "Unknown" => Phase::Unknown,
            other => Phase::Other(other.to_string()),
        }
    }
}

impl std::fmt::Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Phase::Provisioning => f.write_str("Provisioning"),
            Phase::Starting => f.write_str("Starting"),
            Phase::Ready => f.write_str("Ready"),
            Phase::Stopping => f.write_str("Stopping"),
            Phase::Stopped => f.write_str("Stopped"),
            Phase::Deleting => f.write_str("Deleting"),
            Phase::Error => f.write_str("Error"),
            Phase::Unknown => f.write_str("Unknown"),
            Phase::Other(s) => f.write_str(s),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Sandbox {
    pub id: String,
    pub name: String,
    pub phase: Phase,
    /// Gateway-local timestamp, e.g. `2026-08-21 14:15:56`. Deliberately kept
    /// as a string: the CLI emits no timezone, so parsing it would invent one.
    pub created_at: String,
    pub labels: BTreeMap<String, String>,
    pub workspace: String,
}

/// The subset of `sandbox get` we deserialize. The full response also carries a
/// `policy`, but the policy pane reads it from `policy get --full` instead --
/// that call additionally reports which revision is *active*, which is what
/// distinguishes a submitted policy from an enforced one.
#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct RawSandbox {
    id: String,
    name: String,
    phase: String,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    labels: BTreeMap<String, String>,
    #[serde(default)]
    workspace: String,
}

impl From<RawSandbox> for Sandbox {
    fn from(r: RawSandbox) -> Self {
        Sandbox {
            id: r.id,
            phase: Phase::from(r.phase.as_str()),
            name: r.name,
            created_at: r.created_at,
            labels: r.labels,
            workspace: r.workspace,
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct GatewayStatus {
    pub gateway: String,
    pub server: String,
    pub status: String,
    pub version: String,
    #[serde(default)]
    pub authentication: Authentication,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct Authentication {
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub status: String,
}

impl GatewayStatus {
    pub fn is_connected(&self) -> bool {
        self.status == "connected"
    }
}

#[derive(Debug, Clone)]
pub struct ExecOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

impl ExecOutput {
    pub fn ok(&self) -> bool {
        self.exit_code == 0
    }

    /// Trailing-newline-trimmed stdout, which is what callers almost always want.
    pub fn trimmed(&self) -> &str {
        self.stdout.trim_end_matches('\n')
    }
}

/// Options for creating a sandbox.
#[derive(Debug, Clone, Default)]
pub struct CreateOpts {
    pub name: String,
    pub labels: BTreeMap<String, String>,
    pub policy: Option<PathBuf>,
    pub providers: Vec<String>,
    /// Image, community sandbox name, or Dockerfile path for `--from`.
    pub from: Option<String>,
    pub cpu: Option<String>,
    pub memory: Option<String>,
    /// Command run after `--`. Empty means the CLI's default shell.
    pub command: Vec<String>,
}

/// One revision of a sandbox's policy, as `policy get --full` reports it.
///
/// `policy get` without `--full` returns only this metadata; the payload needs
/// the flag. `sandbox get` carries a `policy` too, but this is the call that
/// also says which revision is *active*, which is the only way to tell a
/// submitted policy from a loaded one.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct PolicyRevision {
    #[serde(default)]
    pub version: u32,
    /// The revision the sandbox has actually loaded. Lags `version` for the few
    /// seconds between submitting a policy and the supervisor applying it.
    #[serde(default)]
    pub active_version: u32,
    #[serde(default)]
    pub hash: String,
    /// `sandbox` or `global`: a gateway-global policy lock outranks the
    /// sandbox's own, and the pane has to be able to say so.
    #[serde(default)]
    pub policy_source: String,
    #[serde(default)]
    pub status: String,
    /// Absent unless `--full` was passed.
    #[serde(default)]
    pub policy: Option<Policy>,
}

impl PolicyRevision {
    /// Whether the loaded revision is the latest submitted one.
    pub fn is_settled(&self) -> bool {
        self.active_version == self.version
    }
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct Policy {
    #[serde(default)]
    pub filesystem_policy: FilesystemPolicy,
    #[serde(default)]
    pub process: ProcessPolicy,
    /// Keyed by rule key, which is not the same as the rule's `name`.
    #[serde(default)]
    pub network_policies: BTreeMap<String, NetworkPolicy>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct FilesystemPolicy {
    #[serde(default)]
    pub include_workdir: bool,
    #[serde(default)]
    pub read_only: Vec<String>,
    #[serde(default)]
    pub read_write: Vec<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct ProcessPolicy {
    #[serde(default)]
    pub run_as_user: Option<String>,
    #[serde(default)]
    pub run_as_group: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct NetworkPolicy {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub endpoints: Vec<Endpoint>,
    #[serde(default)]
    pub binaries: Vec<Binary>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct Endpoint {
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub enforcement: Option<String>,
    /// A method class: `read-only`, `read-write`, `full`. Absent when the
    /// endpoint is governed only by [`Endpoint::rules`].
    #[serde(default)]
    pub access: Option<String>,
    /// `terminate` (deprecated, now the default) or `skip`.
    #[serde(default)]
    pub tls: Option<String>,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

impl Endpoint {
    pub fn host_port(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// A method/path rule. Exactly one of the two is set.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct Rule {
    #[serde(default)]
    pub allow: Option<MethodPath>,
    #[serde(default)]
    pub deny: Option<MethodPath>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct MethodPath {
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub path: String,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct Binary {
    #[serde(default)]
    pub path: String,
}

/// An incremental policy change, mapping one-to-one onto `policy update`.
///
/// One struct rather than a list of operations because the CLI's flags are not
/// independent: `--binary` applies to *every* `--add-endpoint` in the same
/// invocation, and `--rule-name` is only accepted when there is exactly one.
/// Modelling it as separate ops would invite building a command the CLI rejects.
/// A credential provider the gateway can attach to a sandbox.
///
/// Only identity and shape: the credential itself lives in gateway state and is
/// never returned by the CLI, which is the whole point of a provider.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Provider {
    pub name: String,
    /// Profile type, e.g. `claude-code-oauth` or `azure-devops-pat`. This, not
    /// the name, is what says what a provider is *for*: names are chosen by
    /// whoever created them and several may share a type.
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub credential_keys: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PolicyUpdate {
    /// `host:port[:access[:protocol[:enforcement[:options]]]]`.
    pub add_endpoints: Vec<String>,
    /// `host:port`.
    pub remove_endpoints: Vec<String>,
    /// Applied to each added endpoint.
    pub binaries: Vec<String>,
    pub rule_name: Option<String>,
    /// Block until the sandbox reports the new revision loaded. Without this
    /// the call returns while the old policy is still being enforced.
    pub wait: bool,
}

impl PolicyUpdate {
    pub fn is_empty(&self) -> bool {
        self.add_endpoints.is_empty() && self.remove_endpoints.is_empty()
    }
}

pub trait OpenShell {
    fn status(&self) -> Result<GatewayStatus>;
    fn create(&self, opts: &CreateOpts) -> Result<Sandbox>;
    /// `selector` is a `key=value` label filter, as accepted by `--selector`.
    fn list(&self, selector: Option<&str>) -> Result<Vec<Sandbox>>;
    fn get(&self, name: &str) -> Result<Sandbox>;
    fn exec(&self, name: &str, argv: &[&str]) -> Result<ExecOutput>;
    fn delete(&self, name: &str) -> Result<()>;
    /// The policy the gateway is actually enforcing, provider-composed entries
    /// included.
    fn policy(&self, name: &str) -> Result<PolicyRevision>;
    /// Merge an incremental change into a live sandbox's policy.
    fn policy_update(&self, name: &str, update: &PolicyUpdate) -> Result<()>;
    /// Recent gateway and sandbox log lines, newest last.
    fn logs(&self, name: &str, lines: usize) -> Result<String>;
    /// Providers defined at the gateway, for offering a choice of
    /// credentials rather than requiring their names to be known.
    fn providers(&self) -> Result<Vec<Provider>>;
}

/// [`OpenShell`] backed by the `openshell` CLI.
#[derive(Debug, Clone)]
pub struct CliClient {
    bin: PathBuf,
    gateway: Option<String>,
    workspace: Option<String>,
}

impl Default for CliClient {
    fn default() -> Self {
        Self {
            bin: PathBuf::from("openshell"),
            gateway: None,
            workspace: None,
        }
    }
}

impl CliClient {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_bin(mut self, bin: impl Into<PathBuf>) -> Self {
        self.bin = bin.into();
        self
    }

    pub fn with_gateway(mut self, gateway: impl Into<String>) -> Self {
        self.gateway = Some(gateway.into());
        self
    }

    pub fn with_workspace(mut self, workspace: impl Into<String>) -> Self {
        self.workspace = Some(workspace.into());
        self
    }

    /// Run the CLI and capture both streams. Returns the raw outcome without
    /// judging the exit code, because `exec` legitimately returns non-zero.
    fn run<I, S>(&self, args: I) -> Result<ExecOutput>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut cmd = Command::new(&self.bin);
        if let Some(g) = &self.gateway {
            cmd.arg("--gateway").arg(g);
        }
        if let Some(w) = &self.workspace {
            cmd.arg("--workspace").arg(w);
        }
        cmd.args(args);

        let out = cmd.output().map_err(|source| Error::Spawn {
            bin: self.bin.display().to_string(),
            source,
        })?;

        Ok(ExecOutput {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            // A signalled process reports no code; -1 keeps this infallible
            // and is never a real CLI exit status.
            exit_code: out.status.code().unwrap_or(-1),
        })
    }

    /// Run the CLI and require success, mapping failures onto [`Error`].
    fn run_checked<I, S>(&self, args: I, display: &str) -> Result<ExecOutput>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let out = self.run(args)?;
        if out.ok() {
            return Ok(out);
        }
        // The gateway reports a missing sandbox as a generic exit-1 error, so
        // the message is the only thing distinguishing it.
        if out.stderr.contains("not found")
            && let Some(name) = display.split_whitespace().last()
        {
            return Err(Error::NotFound(name.to_string()));
        }
        Err(Error::Cli {
            args: display.to_string(),
            code: out.exit_code,
            stderr: out.stderr.trim().to_string(),
        })
    }

    fn parse_json<T: serde::de::DeserializeOwned>(stdout: &str, display: &str) -> Result<T> {
        serde_json::from_str(stdout).map_err(|source| Error::Parse {
            args: display.to_string(),
            source,
        })
    }
}

impl CliClient {
    /// Build a command for an interactive exec that inherits this process's
    /// terminal.
    ///
    /// Unlike [`OpenShell::exec`], nothing is captured: the child owns the tty
    /// until it exits. Returned rather than run so the caller can tear down its
    /// own terminal handling first.
    pub fn interactive_exec(&self, sandbox: &str, argv: &[&str]) -> Command {
        let argv = self.interactive_exec_argv(sandbox, argv);
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..]);
        cmd
    }

    /// The same invocation as [`Self::interactive_exec`], as an argv.
    ///
    /// For callers that cannot use a [`Command`] -- spawning under a pty needs
    /// the program and its arguments separately. Kept as the one definition of
    /// what an interactive exec *is*, so the embedded terminal and `sbx attach`
    /// cannot end up talking to the gateway differently.
    pub fn interactive_exec_argv(&self, sandbox: &str, argv: &[&str]) -> Vec<String> {
        let mut out = vec![self.bin.display().to_string()];
        if let Some(g) = &self.gateway {
            out.push("--gateway".into());
            out.push(g.clone());
        }
        if let Some(w) = &self.workspace {
            out.push("--workspace".into());
            out.push(w.clone());
        }
        out.extend(["sandbox", "exec", "-n", sandbox, "--tty", "--"].map(String::from));
        out.extend(argv.iter().map(|a| (*a).to_string()));
        out
    }
}

impl OpenShell for CliClient {
    fn status(&self) -> Result<GatewayStatus> {
        let display = "status --output json";
        let out = self.run_checked(["status", "--output", "json"], display)?;
        Self::parse_json(&out.stdout, display)
    }

    fn create(&self, opts: &CreateOpts) -> Result<Sandbox> {
        let mut args: Vec<String> = vec![
            "sandbox".into(),
            "create".into(),
            "--name".into(),
            opts.name.clone(),
            // Session creation must never block on a prompt: the TUI has no
            // stdin to offer the CLI.
            "--no-auto-providers".into(),
            "--no-tty".into(),
        ];
        for (k, v) in &opts.labels {
            args.push("--label".into());
            args.push(format!("{k}={v}"));
        }
        if let Some(p) = &opts.policy {
            args.push("--policy".into());
            args.push(p.display().to_string());
        }
        for p in &opts.providers {
            args.push("--provider".into());
            args.push(p.clone());
        }
        if let Some(f) = &opts.from {
            args.push("--from".into());
            args.push(f.clone());
        }
        if let Some(c) = &opts.cpu {
            args.push("--cpu".into());
            args.push(c.clone());
        }
        if let Some(m) = &opts.memory {
            args.push("--memory".into());
            args.push(m.clone());
        }
        if !opts.command.is_empty() {
            args.push("--".into());
            args.extend(opts.command.iter().cloned());
        }

        let display = format!("sandbox create --name {}", opts.name);
        self.run_checked(&args, &display)?;
        // `create` streams human-readable progress rather than JSON, so read
        // the authoritative record back afterwards.
        self.get(&opts.name)
    }

    fn list(&self, selector: Option<&str>) -> Result<Vec<Sandbox>> {
        let mut args = vec!["sandbox", "list", "--output", "json"];
        if let Some(s) = selector {
            args.push("--selector");
            args.push(s);
        }
        let display = args.join(" ");
        let out = self.run_checked(&args, &display)?;
        let raw: Vec<RawSandbox> = Self::parse_json(&out.stdout, &display)?;
        Ok(raw.into_iter().map(Sandbox::from).collect())
    }

    fn get(&self, name: &str) -> Result<Sandbox> {
        let display = format!("sandbox get {name}");
        let out = self.run_checked(["sandbox", "get", name, "--output", "json"], &display)?;
        let raw: RawSandbox = Self::parse_json(&out.stdout, &display)?;
        Ok(raw.into())
    }

    fn exec(&self, name: &str, argv: &[&str]) -> Result<ExecOutput> {
        let mut args = vec!["sandbox", "exec", "-n", name, "--no-tty", "--"];
        args.extend_from_slice(argv);
        // Deliberately unchecked: a non-zero remote exit is data, not an error.
        self.run(&args)
    }

    fn delete(&self, name: &str) -> Result<()> {
        let display = format!("sandbox delete {name}");
        self.run_checked(["sandbox", "delete", name], &display)?;
        Ok(())
    }

    fn policy(&self, name: &str) -> Result<PolicyRevision> {
        let display = format!("policy get {name} --full");
        let out = self.run_checked(
            ["policy", "get", name, "--output", "json", "--full"],
            &display,
        )?;
        Self::parse_json(&out.stdout, &display)
    }

    fn policy_update(&self, name: &str, update: &PolicyUpdate) -> Result<()> {
        let mut args: Vec<String> = vec!["policy".into(), "update".into(), name.into()];
        for e in &update.add_endpoints {
            args.push("--add-endpoint".into());
            args.push(e.clone());
        }
        for e in &update.remove_endpoints {
            args.push("--remove-endpoint".into());
            args.push(e.clone());
        }
        for b in &update.binaries {
            args.push("--binary".into());
            args.push(b.clone());
        }
        if let Some(n) = &update.rule_name {
            args.push("--rule-name".into());
            args.push(n.clone());
        }
        if update.wait {
            args.push("--wait".into());
        }

        let display = format!("policy update {name}");
        self.run_checked(&args, &display)?;
        Ok(())
    }

    fn logs(&self, name: &str, lines: usize) -> Result<String> {
        let n = lines.to_string();
        let display = format!("logs {name} -n {n}");
        // Deliberately not `--tail`: streaming would need a thread of its own
        // and a way to stop it. Refetching a bounded window on a timer is what
        // every other pane already does.
        let out = self.run_checked(["logs", name, "-n", &n], &display)?;
        Ok(out.stdout)
    }

    fn providers(&self) -> Result<Vec<Provider>> {
        let display = "provider list --output json";
        let out = self.run_checked(["provider", "list", "--output", "json"], display)?;
        Self::parse_json(&out.stdout, display)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured verbatim from `openshell sandbox list --output json` on 0.0.110.
    const LIST_JSON: &str = r#"[
      {
        "annotations": {},
        "created_at": "2026-08-21 14:15:56",
        "current_policy_version": 1,
        "id": "edfedc2d-9184-42ae-b969-846f9dea410a",
        "labels": { "sbx.repo": "demo", "sbx.session": "probe" },
        "name": "sbx-probe",
        "phase": "Ready",
        "resource_version": 7,
        "workspace": "default"
      }
    ]"#;

    /// Captured verbatim from `openshell provider list --output json` on 0.0.110,
    /// with the credential values redacted -- the CLI never prints them.
    const PROVIDERS_JSON: &str = r#"[
      {
        "credential_keys": ["CLAUDE_CODE_OAUTH_TOKEN"],
        "id": "e469673c-4f1e-4fa3-912b-d4e10a6cc633",
        "name": "claude-oauth",
        "resource_version": 1,
        "type": "claude-code-oauth",
        "workspace": "default"
      },
      {
        "credential_keys": ["AZURE_DEVOPS_PAT"],
        "id": "123dea58-a262-44e8-abd4-573b934888dc",
        "name": "azure-pat",
        "resource_version": 1,
        "type": "azure-devops-pat",
        "workspace": "default"
      }
    ]"#;

    /// Captured verbatim from `openshell status --output json` on 0.0.110.
    const STATUS_JSON: &str = r#"{
      "authentication": { "provider": "mTLS transport", "status": "authenticated" },
      "gateway": "openshell",
      "server": "https://127.0.0.1:17670",
      "status": "connected",
      "version": "0.0.110"
    }"#;

    #[test]
    fn parses_sandbox_list() {
        let raw: Vec<RawSandbox> = serde_json::from_str(LIST_JSON).unwrap();
        let boxes: Vec<Sandbox> = raw.into_iter().map(Sandbox::from).collect();
        assert_eq!(boxes.len(), 1);
        let s = &boxes[0];
        assert_eq!(s.name, "sbx-probe");
        assert_eq!(s.phase, Phase::Ready);
        assert_eq!(s.workspace, "default");
        assert_eq!(
            s.labels.get("sbx.session").map(String::as_str),
            Some("probe")
        );
    }

    #[test]
    fn parses_every_known_phase() {
        for (text, want) in [
            ("Provisioning", Phase::Provisioning),
            ("Starting", Phase::Starting),
            ("Ready", Phase::Ready),
            ("Stopping", Phase::Stopping),
            ("Stopped", Phase::Stopped),
            ("Deleting", Phase::Deleting),
            ("Error", Phase::Error),
            ("Unknown", Phase::Unknown),
        ] {
            assert_eq!(Phase::from(text), want);
            assert_eq!(want.to_string(), text, "Display must round-trip");
        }
    }

    /// Captured verbatim from `openshell policy get <name> --output json --full`
    /// on 0.0.110, against a sandbox created from `policies/feature-work.yaml`.
    /// Trimmed to one rule of each shape: an `access`-governed endpoint, a
    /// `rules`-governed one, and one with both absent.
    const POLICY_JSON: &str = r#"{
      "active_version": 1,
      "config_revision": 3957685892634909647,
      "hash": "90715775a1ec73bed8bcf1b289d245562fe2dfec88ba9dee0c26fc6ebe02eab6",
      "policy": {
        "filesystem_policy": {
          "include_workdir": true,
          "read_only": ["/usr", "/lib", "/proc"],
          "read_write": ["/sandbox", "/tmp", "/dev/null", "/dev/pts"]
        },
        "landlock": { "compatibility": "best_effort" },
        "network_policies": {
          "claude_code": {
            "binaries": [{ "path": "/usr/local/bin/claude" }, { "path": "/usr/bin/node" }],
            "endpoints": [
              {
                "access": "full",
                "enforcement": "enforce",
                "host": "api.anthropic.com",
                "port": 443,
                "protocol": "rest"
              }
            ],
            "name": "claude-code"
          },
          "github_git": {
            "binaries": [{ "path": "/usr/bin/git" }],
            "endpoints": [
              {
                "enforcement": "enforce",
                "host": "github.com",
                "port": 443,
                "protocol": "rest",
                "rules": [
                  { "allow": { "method": "GET", "path": "/**/info/refs*" } },
                  { "allow": { "method": "POST", "path": "/**/git-receive-pack" } }
                ],
                "tls": "terminate"
              }
            ],
            "name": "github-git"
          }
        },
        "process": { "run_as_group": "sandbox", "run_as_user": "sandbox" },
        "version": 1
      },
      "policy_source": "sandbox",
      "sandbox": "sbx-probe",
      "scope": "sandbox",
      "status": "effective",
      "version": 1
    }"#;

    #[test]
    fn parses_the_effective_policy() {
        let rev: PolicyRevision = serde_json::from_str(POLICY_JSON).unwrap();
        assert_eq!(rev.version, 1);
        assert!(rev.is_settled());
        assert_eq!(rev.policy_source, "sandbox");

        let p = rev.policy.expect("--full carries a payload");
        assert!(p.filesystem_policy.include_workdir);
        assert_eq!(p.filesystem_policy.read_write.len(), 4);
        assert_eq!(p.process.run_as_user.as_deref(), Some("sandbox"));

        // Keyed by rule key, and the display name differs from it.
        let claude = &p.network_policies["claude_code"];
        assert_eq!(claude.name.as_deref(), Some("claude-code"));
        assert_eq!(claude.binaries.len(), 2);
        assert_eq!(claude.endpoints[0].host_port(), "api.anthropic.com:443");
        assert_eq!(claude.endpoints[0].access.as_deref(), Some("full"));
        assert!(claude.endpoints[0].rules.is_empty());

        // A rules-governed endpoint has no `access` at all, which is what makes
        // it default-deny. Conflating the two would misreport the policy.
        let git = &p.network_policies["github_git"];
        assert_eq!(git.endpoints[0].access, None);
        assert_eq!(git.endpoints[0].rules.len(), 2);
        let first = git.endpoints[0].rules[0].allow.as_ref().unwrap();
        assert_eq!(
            (first.method.as_str(), first.path.as_str()),
            ("GET", "/**/info/refs*")
        );
        assert!(git.endpoints[0].rules[0].deny.is_none());
    }

    /// A policy submitted but not yet loaded is the normal state for a few
    /// seconds after a mid-run widen, and the pane has to be able to say so
    /// rather than claiming the new rules are in force.
    #[test]
    fn an_unsettled_revision_is_distinguishable() {
        let json = r#"{"version": 4, "active_version": 3, "hash": "abc"}"#;
        let rev: PolicyRevision = serde_json::from_str(json).unwrap();
        assert!(!rev.is_settled());
        assert!(rev.policy.is_none(), "no --full, no payload");
    }

    /// Without `--full` the payload is absent, not empty. Treating that as an
    /// empty policy would render a sandbox as having no rules at all.
    #[test]
    fn a_metadata_only_revision_has_no_payload() {
        let json = r#"{
          "active_version": 1, "hash": "9071", "policy_source": "sandbox",
          "status": "effective", "version": 1, "future_field": true
        }"#;
        let rev: PolicyRevision = serde_json::from_str(json).unwrap();
        assert!(rev.policy.is_none());
        assert!(rev.is_settled());
    }

    /// The flag order is a contract with the CLI: `--binary` applies to every
    /// `--add-endpoint` in the invocation, so a widen that needs per-rule
    /// binaries has to be split into separate calls rather than merged.
    #[test]
    fn a_policy_update_is_empty_until_it_changes_something() {
        let mut u = PolicyUpdate {
            binaries: vec!["/usr/bin/node".into()],
            rule_name: Some("registries".into()),
            wait: true,
            ..Default::default()
        };
        assert!(u.is_empty(), "binaries alone change nothing");
        u.add_endpoints
            .push("registry.npmjs.org:443:read-only".into());
        assert!(!u.is_empty());
    }

    #[test]
    fn parses_gateway_status() {
        let st: GatewayStatus = serde_json::from_str(STATUS_JSON).unwrap();
        assert!(st.is_connected());
        assert_eq!(st.version, "0.0.110");
        assert_eq!(st.authentication.status, "authenticated");
    }

    #[test]
    fn parses_providers() {
        let ps: Vec<Provider> = serde_json::from_str(PROVIDERS_JSON).unwrap();
        assert_eq!(ps.len(), 2);
        assert_eq!(ps[0].name, "claude-oauth");
        // `type` is a keyword, so the field is renamed; if that mapping breaks,
        // every provider reads as having no type and the create form can no
        // longer tell an agent credential from a git one.
        assert_eq!(ps[0].kind, "claude-code-oauth");
        assert_eq!(ps[1].credential_keys, vec!["AZURE_DEVOPS_PAT"]);
    }

    /// Unknown fields and phases must not break parsing: the gateway adds them
    /// between patch releases and a hard failure would take the whole TUI down.
    #[test]
    fn tolerates_unknown_fields_and_phases() {
        let json = r#"{"id":"x","name":"y","phase":"Rebooting","future_field":true}"#;
        let raw: RawSandbox = serde_json::from_str(json).unwrap();
        let s = Sandbox::from(raw);
        assert_eq!(s.phase, Phase::Other("Rebooting".into()));
        assert!(s.labels.is_empty());
    }

    #[test]
    fn exec_output_helpers() {
        let out = ExecOutput {
            stdout: "hi\n".into(),
            stderr: String::new(),
            exit_code: 0,
        };
        assert!(out.ok());
        assert_eq!(out.trimmed(), "hi");
    }
}
