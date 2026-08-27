//! Preflight checks.
//!
//! Every failure mode here is one actually hit while bringing this project up
//! on Arch/WSL2, so each check carries the fix rather than just a verdict.

use std::process::Command;

use openshell_client::OpenShell;

use crate::config::{self, Config};

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

impl Level {
    fn tag(self) -> &'static str {
        match self {
            Level::Ok => "  ok  ",
            Level::Warn => " warn ",
            Level::Fail => " FAIL ",
        }
    }
}

pub struct Check {
    pub name: &'static str,
    pub level: Level,
    pub detail: String,
    /// Shown only when the check did not pass.
    pub fix: Option<String>,
}

impl Check {
    fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Check {
            name,
            level: Level::Ok,
            detail: detail.into(),
            fix: None,
        }
    }

    fn warn(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Check {
            name,
            level: Level::Warn,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }

    fn fail(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Check {
            name,
            level: Level::Fail,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }
}

/// Run `argv` and return trimmed stdout if it exits zero.
fn probe(argv: &[&str]) -> Option<String> {
    let out = Command::new(argv[0]).args(&argv[1..]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn check_openshell() -> Check {
    match probe(&["openshell", "--version"]) {
        Some(v) => Check::ok("openshell", v),
        None => Check::fail(
            "openshell",
            "not on PATH",
            "install from the release tarballs into ~/.local/bin (see docs/manual-loop.md); \
             install.sh only supports dpkg/rpm",
        ),
    }
}

fn check_gateway(client: &dyn OpenShell) -> Check {
    match client.status() {
        Ok(st) if st.is_connected() => Check::ok(
            "gateway",
            format!(
                "{} {} ({})",
                st.server, st.version, st.authentication.status
            ),
        ),
        Ok(st) => Check::fail(
            "gateway",
            format!("reachable but status is `{}`", st.status),
            "systemctl --user status openshell-gateway",
        ),
        Err(e) => Check::fail(
            "gateway",
            e.to_string(),
            "systemctl --user enable --now openshell-gateway && \
             openshell gateway add https://127.0.0.1:17670 --local --name openshell",
        ),
    }
}

fn check_docker() -> Check {
    match probe(&["docker", "version", "--format", "{{.Server.Version}}"]) {
        Some(v) if !v.is_empty() => Check::ok("docker", format!("server {v}")),
        _ => Check::fail(
            "docker",
            "daemon not reachable",
            "start Docker; the gateway auto-selects kubernetes > podman > docker",
        ),
    }
}

fn check_tmux() -> Check {
    match probe(&["tmux", "-V"]) {
        Some(v) => Check::ok("tmux", v),
        None => Check::fail(
            "tmux",
            "not on PATH",
            "install tmux; sessions attach through a host tmux pane",
        ),
    }
}

/// WSL in particular: without lingering the user manager exits with the last
/// shell, taking the gateway and every running sandbox with it.
fn check_linger() -> Check {
    let user = std::env::var("USER").unwrap_or_default();
    match probe(&["loginctl", "show-user", &user]) {
        Some(out) if out.contains("Linger=yes") => Check::ok("linger", "enabled"),
        Some(_) => Check::warn(
            "linger",
            "disabled: the gateway dies when your last shell exits",
            format!("sudo loginctl enable-linger {user}"),
        ),
        None => Check::warn(
            "linger",
            "could not query loginctl",
            "check systemd is running",
        ),
    }
}

/// A built image is the difference between a ~1s and a ~minute session.
fn check_image() -> Check {
    if crate::image::exists() {
        // An image from an older sbx works, but reports no agent status, and
        // nothing else about it looks wrong.
        if !crate::image::reports_status() {
            return Check::warn(
                "image",
                format!(
                    "{} predates status reporting: the state column will stay `ready`",
                    crate::session::IMAGE
                ),
                "sbx image build",
            );
        }
        // The agent's own version. The base image freezes whatever Claude Code
        // was current when it was published, and the agent cannot upgrade itself
        // from inside a sandbox -- so without this an image built months ago
        // looks perfectly healthy while running an agent that is months behind.
        match (
            crate::image::claude_version(),
            crate::image::latest_claude_version(),
        ) {
            (Some(built), Some(latest)) if crate::image::is_older(&built, &latest) => Check::warn(
                "image",
                format!(
                    "{} carries claude {built}; {latest} is out",
                    crate::session::IMAGE
                ),
                "sbx image build",
            ),
            (Some(built), _) => Check::ok(
                "image",
                format!("{} built, claude {built}", crate::session::IMAGE),
            ),
            // Nothing to compare: an image whose `claude --version` cannot be
            // read is odd, but it is not what doctor is here to diagnose.
            (None, _) => Check::ok("image", format!("{} built", crate::session::IMAGE)),
        }
    } else {
        Check::warn(
            "image",
            format!(
                "{} missing: it will be built on first use",
                crate::session::IMAGE
            ),
            "sbx image build",
        )
    }
}

/// `config` is the load result rather than a [`Config`], because a file that
/// cannot be read is exactly the kind of thing this command exists to say out
/// loud -- every other command refuses to run until it is fixed, so this is the
/// only place the error can be shown next to its fix.
pub fn run(client: &dyn OpenShell, config: &Result<Config, config::Error>) -> Vec<Check> {
    let mut checks = vec![
        check_config(config),
        check_openshell(),
        check_gateway(client),
        check_docker(),
        check_tmux(),
        check_linger(),
        check_image(),
    ];
    // Only when the file names some, since the check is about the file being
    // right rather than about providers existing.
    if let Ok(cfg) = config
        && !cfg.providers().is_empty()
    {
        checks.push(check_config_providers(client, cfg.providers()));
    }
    checks
}

fn check_config(config: &Result<Config, config::Error>) -> Check {
    match config {
        Err(e) => Check::fail(
            "config",
            e.to_string(),
            "fix the file, or delete it to go back to the defaults",
        ),
        Ok(c) if c.present => Check::ok("config", c.path.display().to_string()),
        Ok(c) => Check::ok("config", format!("{} (defaults)", c.path.display())),
    }
}

/// Whether the providers the config file names still exist at the gateway.
///
/// A name that does not is the quietest failure sbx has: the create form simply
/// does not tick it, the sandbox comes up without the credential, and the clone
/// fails for what looks like an authentication problem several steps later.
/// Here is the only place that can be said before it happens, because it is the
/// one command that both reads the file and asks the gateway.
fn check_config_providers(client: &dyn OpenShell, named: &[String]) -> Check {
    let existing = match client.providers() {
        Ok(list) => list,
        // The gateway check above already says so; repeating it here would be
        // two failures for one cause.
        Err(_) => return Check::ok("providers", "not checked: the gateway is unreachable"),
    };
    let missing: Vec<&str> = named
        .iter()
        .map(String::as_str)
        .filter(|n| !existing.iter().any(|p| p.name == *n))
        .collect();
    if missing.is_empty() {
        return Check::ok("providers", named.join(", "));
    }
    Check::warn(
        "providers",
        format!("no provider named {}", missing.join(", ")),
        "openshell provider list; fix `providers` in the config file",
    )
}

/// Print the report. Returns the process exit code.
pub fn report(checks: &[Check]) -> i32 {
    for c in checks {
        println!("[{}] {:<12} {}", c.level.tag(), c.name, c.detail);
        if c.level != Level::Ok
            && let Some(fix) = &c.fix
        {
            println!("{:>8} fix: {fix}", "");
        }
    }

    let failed = checks.iter().filter(|c| c.level == Level::Fail).count();
    let warned = checks.iter().filter(|c| c.level == Level::Warn).count();
    let warnings = if warned == 1 { "warning" } else { "warnings" };
    println!();
    if failed > 0 {
        println!("{failed} failed, {warned} {warnings}");
        1
    } else if warned > 0 {
        println!("all required checks passed, {warned} {warnings}");
        0
    } else {
        println!("all checks passed");
        0
    }
}
