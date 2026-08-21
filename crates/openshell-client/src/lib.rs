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

/// The subset of `sandbox get` we deserialize. The full response also carries
/// the effective policy, which the policy pane will want later.
#[derive(Debug, serde::Deserialize)]
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

#[derive(Debug, Clone, serde::Deserialize)]
pub struct GatewayStatus {
    pub gateway: String,
    pub server: String,
    pub status: String,
    pub version: String,
    #[serde(default)]
    pub authentication: Authentication,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
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

pub trait OpenShell {
    fn status(&self) -> Result<GatewayStatus>;
    fn create(&self, opts: &CreateOpts) -> Result<Sandbox>;
    /// `selector` is a `key=value` label filter, as accepted by `--selector`.
    fn list(&self, selector: Option<&str>) -> Result<Vec<Sandbox>>;
    fn get(&self, name: &str) -> Result<Sandbox>;
    fn exec(&self, name: &str, argv: &[&str]) -> Result<ExecOutput>;
    fn delete(&self, name: &str) -> Result<()>;
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
        let mut cmd = Command::new(&self.bin);
        if let Some(g) = &self.gateway {
            cmd.arg("--gateway").arg(g);
        }
        if let Some(w) = &self.workspace {
            cmd.arg("--workspace").arg(w);
        }
        cmd.args(["sandbox", "exec", "-n", sandbox, "--tty", "--"]);
        cmd.args(argv);
        cmd
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

    #[test]
    fn parses_gateway_status() {
        let st: GatewayStatus = serde_json::from_str(STATUS_JSON).unwrap();
        assert!(st.is_connected());
        assert_eq!(st.version, "0.0.110");
        assert_eq!(st.authentication.status, "authenticated");
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
