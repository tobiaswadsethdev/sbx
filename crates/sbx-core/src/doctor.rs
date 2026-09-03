//! Preflight checks.
//!
//! Every failure mode here is one actually hit while bringing this project up
//! on Arch/WSL2, so each check carries the fix rather than just a verdict.

use std::net::{TcpStream, ToSocketAddrs};
use std::process::Command;
use std::time::Duration;

use openshell_client::OpenShell;

use crate::config::{self, Config};
use crate::mcp;
use crate::secrets;
use crate::skills;
use crate::tracker;

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
    pub fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Check {
            name,
            level: Level::Ok,
            detail: detail.into(),
            fix: None,
        }
    }

    pub fn warn(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Check {
            name,
            level: Level::Warn,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }

    pub fn fail(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
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
             OpenShell's own install.sh supports dpkg/rpm only",
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

/// Whether the tool running this is the newest one published.
///
/// Reported rather than acted on: nothing updates itself here, and a machine
/// that cannot reach github is a machine this stays quiet about -- "could not
/// ask" is not "up to date", but it is also not a problem with the setup, which
/// is what every other check is about.
fn check_version() -> Check {
    match crate::update::check() {
        crate::update::Status::Newer { running, latest } => Check::warn(
            "version",
            format!("sbx {running}; {latest} is out"),
            "sbx update",
        ),
        crate::update::Status::Current(v) => Check::ok("version", format!("sbx {v}, newest")),
        crate::update::Status::Ahead(v) => {
            Check::ok("version", format!("sbx {v}, ahead of the newest release"))
        }
        crate::update::Status::Unknown => Check::ok(
            "version",
            format!(
                "sbx {} (no release list to compare against)",
                crate::update::current()
            ),
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

/// The toolchain variants that are built, and whether they are still current.
///
/// Only when there are some: a variant is a per-session choice, so its absence is
/// not a problem to report -- unlike the base image, which every session needs.
///
/// The staleness half is the part worth a check. A variant is `FROM
/// sbx-base:latest`, so rebuilding the base for a newer agent leaves every
/// variant behind it, and nothing about that looks wrong from outside: sessions
/// start, the toolchain works, and the agent is whatever version it was when the
/// variant was built.
fn rebuild_commands(tags: &[String]) -> Vec<String> {
    tags.iter()
        .filter_map(|tag| tag.split_once(':'))
        .map(|(_, toolchains)| {
            format!(
                "sbx image build --toolchain {}",
                toolchains.replace('-', ",")
            )
        })
        .collect()
}

fn check_toolchains() -> Option<Check> {
    let variants = crate::image::variants();
    if variants.is_empty() {
        return None;
    }

    let stale = crate::image::stale_variants();
    if !stale.is_empty() {
        return Some(Check::warn(
            "toolchains",
            format!(
                "{} older than {}, so still on its previous agent",
                stale.join(", "),
                crate::session::IMAGE
            ),
            // One command per variant, because each is its own build. The tag's
            // toolchains are joined with `-` and `--toolchain` takes them
            // comma-separated, so `sbx-base:dotnet-rust` turns back into
            // `--toolchain dotnet,rust`.
            rebuild_commands(&stale).join("; "),
        ));
    }

    // What each one carries, read from the image's own manifest rather than
    // inferred from its tag: the tag is a claim about what was asked for, the
    // manifest is what the layers installed.
    let built: Vec<String> = variants
        .iter()
        .map(|tag| {
            let carried = crate::image::toolchains_in(tag)
                .iter()
                .map(|(name, version)| format!("{name} {version}"))
                .collect::<Vec<_>>()
                .join(", ");
            if carried.is_empty() {
                tag.clone()
            } else {
                format!("{tag} ({carried})")
            }
        })
        .collect();

    Some(Check::ok("toolchains", built.join("; ")))
}

/// How long to wait for a published MCP port to answer.
///
/// It is on the loopback bridge, so a server that is up answers in microseconds
/// and anything slower is a firewall or a wrong address. Short enough that a
/// misconfigured entry does not make `doctor` feel broken.
const MCP_CONNECT_TIMEOUT: Duration = Duration::from_millis(500);

/// Whether the MCP servers the config names are actually there.
///
/// The quietest failure this feature has: a container that is not running, or
/// one running but not attached to the gateway's network, produces a session
/// whose agent comes up with a tool it cannot reach -- and the agent reports
/// that as "needs authentication", which sends anyone looking in the wrong
/// direction entirely.
///
/// Three shapes, checked differently because they fail differently. A
/// **managed** entry is asked of [`mcp::statuses`], which is the same answer the
/// window's integrations screen shows -- one implementation, so a check that
/// passes here cannot disagree with a screen that says something is wrong. An
/// external **container name** is asked about through Docker, since the host
/// cannot reach it by name at all -- only sandboxes on that network can. An
/// external **published port** is connected to, on the bridge gateway address
/// the sandbox will use rather than on `localhost`, because a container
/// published to `127.0.0.1` only is exactly the mistake that looks fine from the
/// host and is unreachable from a sandbox.
fn check_mcp(entries: &[mcp::Entry]) -> Check {
    // Also the answer to "is Docker there at all": without the network there is
    // no address to connect to and no point asking about containers, and the
    // docker check above has already said why. Saying it a second time here
    // would be two failures for one cause.
    let Some(bridge) = bridge_gateway() else {
        return Check::ok(
            "mcp",
            "not checked: the openshell docker network is not there",
        );
    };
    let mut problems: Vec<String> = Vec::new();
    let live = mcp::statuses(entries);

    for (entry, status) in entries.iter().zip(&live) {
        let s = &entry.server;
        let problem = if entry.is_managed() {
            status.problem.clone()
        } else if s.via_host() {
            (!port_open(&bridge, port_of(s))).then(|| {
                format!(
                    "nothing is listening on {bridge}:{}, which is where `{}` points from inside a sandbox",
                    port_of(s),
                    s.host()
                )
            })
        } else {
            container_problem(s.host())
        };
        if let Some(p) = problem {
            problems.push(format!("{}: {p}", s.name));
        }
    }

    let named = entries
        .iter()
        .map(|e| e.name())
        .collect::<Vec<_>>()
        .join(", ");
    if problems.is_empty() {
        return Check::ok("mcp", named);
    }
    Check::warn(
        "mcp",
        problems.join("; "),
        format!(
            "a managed one starts from the window's integrations screen; \
             one of your own can be attached with \
             `docker network connect {} <container>`, or its url fixed in the \
             config file",
            mcp::NETWORK
        ),
    )
}

/// What is wrong with the container an MCP url names, if anything.
///
/// Only ever called once Docker is known to be answering, so `inspect` failing
/// means the container does not exist -- which is the most likely thing to be
/// wrong, and the one a sandbox reports as an MCP server that needs
/// authentication.
fn container_problem(name: &str) -> Option<String> {
    let Some(out) = probe(&[
        "docker",
        "inspect",
        name,
        "--format",
        "{{.State.Running}} {{range $k, $v := .NetworkSettings.Networks}}{{$k}} {{end}}",
    ]) else {
        return Some(format!(
            "there is no container named `{name}`, so no sandbox can resolve that url"
        ));
    };
    let mut parts = out.split_whitespace();
    let running = parts.next() == Some("true");
    let networks: Vec<&str> = parts.collect();
    if !running {
        return Some(format!("container `{name}` is not running"));
    }
    if !networks.contains(&mcp::NETWORK) {
        return Some(format!(
            "container `{name}` is not on the `{}` network, so no sandbox can resolve it",
            mcp::NETWORK
        ));
    }
    None
}

/// The address `host.openshell.internal` resolves to inside a sandbox.
fn bridge_gateway() -> Option<String> {
    probe(&[
        "docker",
        "network",
        "inspect",
        mcp::NETWORK,
        "--format",
        "{{(index .IPAM.Config 0).Gateway}}",
    ])
    .filter(|ip| !ip.is_empty())
}

fn port_of(s: &mcp::Server) -> &str {
    s.endpoint.rsplit_once(':').map_or("", |(_, p)| p)
}

fn port_open(host: &str, port: &str) -> bool {
    let Ok(mut addrs) = format!("{host}:{port}").to_socket_addrs() else {
        return false;
    };
    addrs.any(|a| TcpStream::connect_timeout(&a, MCP_CONNECT_TIMEOUT).is_ok())
}

/// Whether the skills the config names are where it says they are.
///
/// A skill is a directory the agent loads on sight, so the failure is quiet in
/// the same way a stale provider name is: nothing errors, the session simply
/// comes up without it and the agent no longer knows how to do the thing you
/// wrote down. Here is the only place that can be said before it happens.
fn check_skills(configured: &[skills::Skill]) -> Check {
    let problems: Vec<String> = configured
        .iter()
        .filter_map(|s| s.problem().map(|p| format!("{}: {p}", s.name)))
        .collect();
    let named = configured
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if problems.is_empty() {
        return Check::ok("skills", named);
    }
    Check::warn(
        "skills",
        problems.join("; "),
        format!(
            "a skill is a directory with a SKILL.md in it; `skills` takes a name              under {} or a path to one",
            skills::host_skills_dir().display()
        ),
    )
}

/// Whether the trackers the inbox reads can be read.
///
/// A tracker whose credential is not in the store produces an inbox that is
/// silently *missing rows* -- which looks exactly like having nothing assigned
/// to you, and is the same class of quiet failure as a stale provider name or
/// an MCP container that is not running.
///
/// Checked against the store rather than by making a request: a name with no
/// value is the failure worth catching, and asking three trackers over the
/// network would make `doctor` take seconds and fail on a train.
fn check_trackers(sources: &[tracker::Source]) -> Check {
    let stored = secrets::names();
    let problems: Vec<String> = sources
        .iter()
        .filter_map(|s| {
            if let Some(problem) = s.problem() {
                return Some(problem);
            }
            (!stored.iter().any(|n| n == &s.secret)).then(|| {
                format!(
                    "{}: no value stored for `{}`, so its tasks will not appear",
                    s.name, s.secret
                )
            })
        })
        .collect();
    let named = sources
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if problems.is_empty() {
        return Check::ok("trackers", named);
    }
    Check::warn(
        "trackers",
        problems.join("; "),
        "store it with `printf %s \"$TOKEN\" | sbxd secret <NAME>`, or from the \
         window's integrations screen"
            .to_string(),
    )
}

/// `config` is the load result rather than a [`Config`], because a file that
/// cannot be read is exactly the kind of thing this command exists to say out
/// loud -- every other command refuses to run until it is fixed, so this is the
/// only place the error can be shown next to its fix.
/// Which way WSL2 is wired, when this is WSL2 at all.
///
/// Two ways of being reachable, and the difference is invisible from inside:
/// with mirrored networking the Windows side reaches this at `localhost`, and
/// with the default NAT it reaches it at an address that changes whenever WSL
/// restarts. Getting it wrong looks exactly like a firewall problem and is not
/// one, which is why this is a check rather than a paragraph in a document.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum WslNetworking {
    Mirrored,
    Nat,
}

/// `None` when this is not WSL, so an ordinary Linux box gets no line about it.
pub fn wsl_networking() -> Option<WslNetworking> {
    if !is_wsl() {
        return None;
    }
    Some(if mirrored() {
        WslNetworking::Mirrored
    } else {
        WslNetworking::Nat
    })
}

fn is_wsl() -> bool {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| {
            let s = s.to_ascii_lowercase();
            s.contains("microsoft") || s.contains("wsl")
        })
        .unwrap_or(false)
}

/// Mirrored mode, from what it *does* rather than from what was configured.
///
/// The `loopback0` interface is mirrored networking's own, and unlike
/// `.wslconfig` it does not depend on `/mnt/c` being mounted or on guessing
/// which Windows user's file is in force. The config file is still consulted,
/// because a machine where the interface naming changes should still get the
/// right answer from the thing that decided it.
fn mirrored() -> bool {
    if std::path::Path::new("/sys/class/net/loopback0").exists() {
        return true;
    }
    let Ok(users) = std::fs::read_dir("/mnt/c/Users") else {
        return false;
    };
    users
        .flatten()
        .filter_map(|u| std::fs::read_to_string(u.path().join(".wslconfig")).ok())
        .any(|text| wslconfig_is_mirrored(&text))
}

/// Whether a `.wslconfig` selects mirrored networking.
///
/// Windows is not fussy about the case of either half, and the file is edited
/// by hand often enough to have stray spaces in it.
fn wslconfig_is_mirrored(text: &str) -> bool {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .any(|(k, v)| {
            k.trim().eq_ignore_ascii_case("networkingMode")
                && v.trim().eq_ignore_ascii_case("mirrored")
        })
}

/// What a client on Windows should dial to reach a server running here.
///
/// The port is passed in rather than known here: it belongs to the protocol,
/// and the protocol crate is built *on* this one, so importing it would be a
/// cycle. One caller with both is cheaper than a second copy of the number.
pub fn check_wsl(port: u16) -> Option<Check> {
    match wsl_networking()? {
        WslNetworking::Mirrored => Some(Check::ok(
            "wsl",
            format!("mirrored networking: a client on Windows uses localhost:{port}"),
        )),
        WslNetworking::Nat => {
            let addrs = own_addresses();
            let address = addrs.first().cloned().unwrap_or_else(|| "<this vm>".into());
            Some(Check::warn(
                "wsl",
                format!("NAT networking: a client on Windows uses {address}:{port}, not localhost"),
                "that address changes when WSL restarts, and every restart then needs \
                 pairing again with the new one. `networkingMode=mirrored` in \
                 %USERPROFILE%\\.wslconfig makes it localhost and keeps it there",
            ))
        }
    }
}

/// This machine's non-loopback addresses, best effort.
fn own_addresses() -> Vec<String> {
    let Ok(text) = std::fs::read_to_string("/proc/net/fib_trie") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let Some(addr) = line.trim().strip_prefix("|-- ") else {
            continue;
        };
        if !lines.peek().is_some_and(|n| n.contains("LOCAL")) {
            continue;
        }
        if let Ok(ip) = addr.trim().parse::<std::net::IpAddr>()
            && !ip.is_loopback()
        {
            let s = ip.to_string();
            if !out.contains(&s) {
                out.push(s);
            }
        }
    }
    out
}

pub fn run(client: &dyn OpenShell, config: &Result<Config, config::Error>) -> Vec<Check> {
    let mut checks = vec![
        check_version(),
        check_config(config),
        check_openshell(),
        check_gateway(client),
        check_docker(),
        check_tmux(),
        check_linger(),
        check_image(),
    ];
    checks.extend(check_toolchains());
    // Only when the file names some, since the check is about the file being
    // right rather than about providers existing.
    if let Ok(cfg) = config
        && !cfg.providers().is_empty()
    {
        checks.push(check_config_providers(client, cfg.providers()));
    }
    // Same reasoning: the check is about the file being right, so it only runs
    // when the file says something.
    if let Ok(cfg) = config
        && !cfg.mcp().is_empty()
    {
        checks.push(check_mcp(cfg.mcp()));
    }
    if let Ok(cfg) = config
        && !cfg.skills().is_empty()
    {
        checks.push(check_skills(cfg.skills()));
    }
    if let Ok(cfg) = config
        && !cfg.trackers().is_empty()
    {
        checks.push(check_trackers(cfg.trackers()));
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

/// The fix a stale variant is given has to be the command that rebuilds *that*
/// variant, not the base image: rebuilding the base is what made it stale.
#[cfg(test)]
mod toolchain_tests {
    use super::rebuild_commands;

    #[test]
    fn a_stale_variant_is_told_how_to_rebuild_itself() {
        assert_eq!(
            rebuild_commands(&["sbx-base:dotnet".to_string()]),
            ["sbx image build --toolchain dotnet"]
        );
        // The tag joins with `-`, the flag takes `,`.
        assert_eq!(
            rebuild_commands(&["sbx-base:dotnet-rust".to_string()]),
            ["sbx image build --toolchain dotnet,rust"]
        );
        // One command each, since each is its own build.
        assert_eq!(
            rebuild_commands(&["sbx-base:dotnet".into(), "sbx-base:rust".into()]).len(),
            2
        );
        // Nothing invented from something that is not a tag.
        assert!(rebuild_commands(&["sbx-base".to_string()]).is_empty());
    }
}

#[cfg(test)]
mod wsl_tests {
    use super::*;

    /// Windows is not fussy about case and the file is hand-edited, so neither
    /// half can be matched exactly.
    #[test]
    fn a_wslconfig_selecting_mirrored_is_recognised_however_it_is_written() {
        for yes in [
            "[wsl2]\nnetworkingMode=mirrored",
            "[wsl2]\nnetworkingMode = Mirrored\n",
            "[wsl2]\nNETWORKINGMODE=MIRRORED",
            "[wsl2]\nmemory=8GB\nnetworkingMode=mirrored\nswap=0",
        ] {
            assert!(wslconfig_is_mirrored(yes), "{yes:?}");
        }
        for no in [
            "",
            "[wsl2]\nmemory=8GB",
            "[wsl2]\nnetworkingMode=nat",
            // A mode named in a comment is not the mode in force.
            "[wsl2]\n# networkingMode is mirrored on the other machine",
        ] {
            assert!(!wslconfig_is_mirrored(no), "{no:?}");
        }
    }

    /// The check has to be silent on an ordinary Linux box: a line about WSL
    /// where there is no WSL is noise in the one command people read closely.
    #[test]
    fn there_is_no_wsl_line_unless_this_is_wsl() {
        assert_eq!(check_wsl(17671).is_some(), wsl_networking().is_some());
        if !is_wsl() {
            assert!(check_wsl(17671).is_none());
        }
    }

    /// Whichever way it is wired, the advice names the port a client dials --
    /// which is the whole content of it.
    #[test]
    fn the_wsl_advice_carries_the_port_it_was_given() {
        if let Some(check) = check_wsl(12345) {
            assert!(check.detail.contains("12345"), "{}", check.detail);
        }
    }
}
