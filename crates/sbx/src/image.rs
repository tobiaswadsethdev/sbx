//! The sandbox image.
//!
//! Every file the image needs is embedded in the binary, so `sbx` can build it
//! from anywhere once installed rather than only from a checkout.
//!
//! They are written to a temporary directory and handed to `docker build` as a
//! context, rather than being piped in on stdin. A context is needed because
//! `COPY` has nothing to copy from without one, and the alternative -- heredocs
//! in the Dockerfile -- silently requires BuildKit: the legacy builder ignores
//! the `# syntax=` directive and fails with "no source files were specified".

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::session::IMAGE;

const DOCKERFILE: &str = include_str!("../../../images/sbx-base/Dockerfile");
/// Writes `/sandbox/.sbx/status.json` from Claude Code's hooks.
const SBX_STATUS: &str = include_str!("../../../images/sbx-base/sbx-status");
/// Hook wiring, baked in so a session needs no per-session setup.
const CLAUDE_SETTINGS: &str = include_str!("../../../images/sbx-base/claude-settings.json");

/// Files making up the build context, as (name in the context, content).
const CONTEXT: [(&str, &str); 3] = [
    ("Dockerfile", DOCKERFILE),
    ("sbx-status", SBX_STATUS),
    ("claude-settings.json", CLAUDE_SETTINGS),
];

pub fn exists() -> bool {
    Command::new("docker")
        .args(["image", "inspect", IMAGE])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Write the embedded context to a fresh directory and return its path.
fn write_context() -> Result<PathBuf, String> {
    // The pid keeps concurrent builds from sharing a directory. Not a security
    // boundary: everything written here is a compile-time constant.
    let dir = std::env::temp_dir().join(format!("sbx-image-{}", std::process::id()));
    // Remove any leftovers from a previous run that died before cleaning up, so
    // a stale file can never end up in the image.
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;

    for (name, content) in CONTEXT {
        let path = dir.join(name);
        fs::write(&path, content)
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    }
    Ok(dir)
}

/// Build the image, streaming docker's output so a slow first build shows
/// progress instead of looking hung.
pub fn build() -> Result<(), String> {
    let dir = write_context()?;
    // The agent's version is resolved here rather than left to the Dockerfile's
    // own `latest` branch, because docker would answer a rebuild from the cached
    // layer: `latest` inside the build means "whatever was newest the first time
    // this layer was built", which is the staleness the step exists to fix.
    // Passing the concrete version changes the ARG, which invalidates that layer
    // and everything after it exactly when there is something new to install.
    let claude = latest_claude_version();
    match &claude {
        Some(v) => println!("claude {v} (latest release)"),
        // Not fatal: a cached layer can still satisfy the build, and the
        // Dockerfile resolves `latest` itself when nothing was passed in. What
        // must not happen is silently building an old agent while claiming to
        // have fetched the newest.
        None => eprintln!(
            "sbx: could not ask {CLAUDE_RELEASES} what the newest claude is; \
             building with whatever docker has cached"
        ),
    }
    let result = run_build(&dir, claude.as_deref());
    // Clean up whether or not the build worked; a failed build's context is not
    // worth keeping, since it is regenerated from constants every time.
    let _ = fs::remove_dir_all(&dir);
    result
}

/// The `docker build` argv. Split out so the build-arg wiring is testable
/// without running docker.
fn build_argv(dir: &Path, claude: Option<&str>) -> Vec<String> {
    let mut argv = vec!["build".to_string(), "-t".to_string(), IMAGE.to_string()];
    if let Some(version) = claude {
        argv.push("--build-arg".to_string());
        argv.push(format!("CLAUDE_VERSION={version}"));
    }
    argv.push(dir.display().to_string());
    argv
}

fn run_build(dir: &Path, claude: Option<&str>) -> Result<(), String> {
    let status = Command::new("docker")
        .args(build_argv(dir, claude))
        .status()
        .map_err(|e| format!("could not run docker: {e}"))?;
    if !status.success() {
        return Err(format!("docker build exited with {status}"));
    }
    Ok(())
}

/// Whether the built image carries the status reporter.
///
/// An image built by an older `sbx` is perfectly usable, and nothing about it
/// looks wrong -- sessions start and the diff pane works -- but the state column
/// silently never leaves `ready`. Checking for the script turns that into
/// something `sbx doctor` can say out loud.
pub fn reports_status() -> bool {
    Command::new("docker")
        .args([
            "run",
            "--rm",
            "--entrypoint",
            "test",
            IMAGE,
            "-x",
            STATUS_SCRIPT_PATH,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Where the Dockerfile installs the reporter.
const STATUS_SCRIPT_PATH: &str = "/usr/local/bin/sbx-status";

/// Where Claude Code releases are published. The Dockerfile downloads from the
/// same service; a test keeps the two in step.
const CLAUDE_RELEASES: &str = "https://downloads.claude.ai/claude-code-releases";

/// The newest Claude Code release, as the download service reports it.
///
/// Through `curl` rather than an HTTP client: the whole project is built on
/// subprocesses, and a TLS stack for one line of text would outweigh everything
/// it is used for. Short timeouts, because every caller has something better to
/// do than wait -- `None` means "could not ask", and no caller may read that as
/// "up to date".
pub fn latest_claude_version() -> Option<String> {
    let out = Command::new("curl")
        .args(["-fsSL", "--connect-timeout", "3", "--max-time", "10"])
        .arg(format!("{CLAUDE_RELEASES}/latest"))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // The service answers an unavailable region with an HTML page rather than an
    // error status, so the shape is checked before the string is believed.
    let looks_like_a_version = version
        .split('.')
        .take(2)
        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    (version.contains('.') && looks_like_a_version).then_some(version)
}

/// The Claude Code version inside the built image.
///
/// A container start, so it is only worth doing where the answer is the point --
/// `sbx doctor` -- and never on a path a session waits on.
pub fn claude_version() -> Option<String> {
    let out = Command::new("docker")
        .args(["run", "--rm", "--entrypoint", "claude", IMAGE, "--version"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // `2.1.246 (Claude Code)`
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .map(str::to_string)
}

/// Whether `built` is an older release than `available`.
///
/// Compared component-wise rather than as strings, because `2.1.9` sorts after
/// `2.1.246` lexically. Only *older* counts: an image built from a `--build-arg`
/// ahead of the published release is a deliberate act, and warning about it
/// would be telling the user off for being early. Anything unparseable is
/// treated as not-older, so a version scheme this does not understand stays
/// quiet rather than nagging on every `doctor` run.
pub fn is_older(built: &str, available: &str) -> bool {
    fn parts(v: &str) -> Option<Vec<u32>> {
        // Drop any pre-release suffix: `2.1.246-rc1` compares as `2.1.246`.
        let core = v.split(['-', '+']).next()?;
        core.split('.').map(|p| p.parse::<u32>().ok()).collect()
    }
    match (parts(built), parts(available)) {
        (Some(a), Some(b)) => a < b,
        _ => false,
    }
}

/// Build the image if it is missing. Returns whether a build happened.
pub fn ensure() -> Result<bool, String> {
    if exists() {
        return Ok(false);
    }
    println!("building {IMAGE} (first run, this takes a minute) ...");
    build()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The embedded copy must stay in step with what the image needs; a
    /// Dockerfile without tmux would silently produce sandboxes that cannot
    /// run an agent.
    #[test]
    fn embedded_dockerfile_installs_tmux() {
        assert!(DOCKERFILE.contains("tmux"));
        assert!(DOCKERFILE.contains("openshell-community/sandboxes/base"));
        assert!(
            DOCKERFILE.contains("USER sandbox"),
            "must drop back to the sandbox user"
        );
    }

    /// Every file `COPY`d must actually be in the context. A missing one fails
    /// the build with "no such file or directory" only when someone next builds
    /// the image, which may be long after the Dockerfile was edited.
    #[test]
    fn every_copied_file_is_in_the_build_context() {
        let copied: Vec<&str> = DOCKERFILE
            .lines()
            .filter_map(|l| l.trim().strip_prefix("COPY "))
            .filter_map(|rest| rest.split_whitespace().next())
            .collect();
        assert!(!copied.is_empty(), "expected the Dockerfile to COPY files");

        for source in copied {
            assert!(
                CONTEXT.iter().any(|(name, _)| *name == source),
                "Dockerfile copies `{source}`, which the build context does not provide"
            );
        }
    }

    /// The heredoc form of `COPY` needs BuildKit, and the legacy builder fails
    /// on it with a message that does not mention the builder at all. Keeping
    /// the context explicit is what makes the build work on both.
    #[test]
    fn the_dockerfile_does_not_rely_on_heredocs() {
        assert!(
            !DOCKERFILE.contains("COPY <<"),
            "heredoc COPY silently requires BuildKit; use a context file"
        );
    }

    #[test]
    fn context_is_written_and_cleaned_up() {
        let dir = write_context().expect("write context");
        for (name, content) in CONTEXT {
            let written = fs::read_to_string(dir.join(name)).expect(name);
            assert_eq!(written, content, "{name} written verbatim");
        }
        fs::remove_dir_all(&dir).expect("cleanup");
        assert!(!dir.exists());
    }

    /// `reports_status` probes for this path, so the Dockerfile has to keep
    /// installing it there.
    #[test]
    fn the_dockerfile_installs_the_reporter_where_doctor_looks_for_it() {
        assert!(
            DOCKERFILE.contains(STATUS_SCRIPT_PATH),
            "the reporter must land at {STATUS_SCRIPT_PATH}"
        );
    }

    /// The version story has three halves that have to agree: the Dockerfile
    /// takes a build arg, defaults it to `latest`, and verifies what it
    /// installed. Any one of them missing makes the image quietly ship an
    /// unexpected agent.
    #[test]
    fn the_dockerfile_installs_a_claude_version_it_was_given() {
        assert!(
            DOCKERFILE.contains("ARG CLAUDE_VERSION=latest"),
            "a plain `docker build` must default to the newest release"
        );
        assert!(
            DOCKERFILE.contains("$version/$platform/claude"),
            "the resolved version must be what is downloaded"
        );
        assert!(
            DOCKERFILE.contains("sha256sum -c -"),
            "the download must be checksummed"
        );
        assert!(
            DOCKERFILE.contains("test \"$installed\" = \"$version\""),
            "the build must verify the binary it ended up with"
        );
        // Both sides fetch from the same service; a change to one is a change to
        // the other.
        assert!(
            DOCKERFILE.contains(CLAUDE_RELEASES),
            "the Dockerfile must download from {CLAUDE_RELEASES}"
        );
    }

    /// Both of these exist to keep the agent's screen readable to `status`:
    /// the width the markers have to fit in, and the update attempt that
    /// otherwise writes a failure line over them. Neither is visible in any
    /// test that does not run a sandbox, so they are asserted here.
    #[test]
    fn the_image_keeps_the_agents_screen_scrapeable() {
        // Wide enough that Claude Code's footer -- where `status.rs` finds
        // `esc to interrupt` -- is not truncated away, and the same size an
        // attach puts the window back to when it detaches.
        let (cols, rows) = crate::session::SCRAPE_SIZE;
        assert!(
            DOCKERFILE.contains(&format!("default-size {cols}x{rows}")),
            "an unattached agent pane must be wide enough for its footer"
        );
        assert!(
            DOCKERFILE.contains("ENV DISABLE_AUTOUPDATER=1"),
            "the agent must not try to update itself inside the sandbox"
        );
        // The embedded terminal detaches by sending the tmux prefix plus `d`,
        // hard-coded as Ctrl-b. If the image ever set its own prefix, that would
        // become two characters typed at the agent instead.
        assert!(
            !DOCKERFILE.contains("set -g prefix"),
            "tui::term sends Ctrl-b to detach; the image must not rebind the prefix"
        );
        // The one that reaches the agent: the gateway does not pass the image's
        // environment through, so the `ENV` above covers only what a person
        // starts by hand.
        let settings: serde_json::Value =
            serde_json::from_str(CLAUDE_SETTINGS).expect("valid settings");
        assert_eq!(
            settings["env"]["DISABLE_AUTOUPDATER"], "1",
            "settings.json is what the agent actually reads"
        );
    }

    /// A resolved version has to reach docker as a build arg. Without it the
    /// Dockerfile falls back to its own `latest`, which docker answers from the
    /// cached layer -- an upgrade that silently does nothing.
    #[test]
    fn a_resolved_claude_version_is_passed_to_docker_as_a_build_arg() {
        let dir = Path::new("/tmp/ctx");
        let argv = build_argv(dir, Some("2.1.246"));
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--build-arg" && w[1] == "CLAUDE_VERSION=2.1.246"),
            "{argv:?}"
        );
        assert_eq!(argv.last().unwrap(), "/tmp/ctx", "the context comes last");

        // And nothing invented when the release service could not be reached.
        let argv = build_argv(dir, None);
        assert!(!argv.iter().any(|a| a == "--build-arg"), "{argv:?}");
        assert_eq!(argv.last().unwrap(), "/tmp/ctx");
    }

    /// Reaches the network, so it is not part of the default run. Kept because
    /// the shape of what the service answers is a contract this relies on.
    #[test]
    #[ignore = "requires network"]
    fn the_latest_release_can_be_resolved() {
        let v = latest_claude_version().expect("a version");
        assert!(v.split('.').count() >= 2, "`{v}` does not look like one");
    }

    #[test]
    fn versions_compare_by_component_not_as_strings() {
        assert!(is_older("2.1.143", "2.1.246"));
        // The case string comparison gets wrong.
        assert!(is_older("2.1.9", "2.1.246"));
        assert!(is_older("1.9.0", "2.0.0"));
        assert!(!is_older("2.1.246", "2.1.246"));
        // Ahead of the pin is deliberate, not a problem to report.
        assert!(!is_older("2.2.0", "2.1.246"));
        assert!(!is_older("2.1.246-rc1", "2.1.246"));
        // Nothing understandable to compare: stay quiet rather than nag.
        assert!(!is_older("nightly", "2.1.246"));
        assert!(!is_older("2.1.246", ""));
    }

    /// The script and the Rust parser have to agree on the field names, or
    /// status silently never resolves.
    #[test]
    fn the_reporter_writes_the_fields_the_parser_reads() {
        for field in ["state", "at", "detail"] {
            assert!(
                SBX_STATUS.contains(&format!("\"{field}\"")),
                "sbx-status must write a `{field}` field"
            );
        }
        // Every path has to exit 0: a hook that fails is fed back to the model.
        assert!(
            SBX_STATUS.contains("exit 0"),
            "a failing hook must never break the agent"
        );
        assert!(
            SBX_STATUS.contains("mv \"$tmp\""),
            "the file must be renamed into place, not written in place"
        );
    }

    /// The defaults a session starts with, which are the whole point of baking a
    /// settings file rather than leaving the agent on its own.
    #[test]
    fn the_baked_settings_choose_a_model_and_a_permission_mode() {
        let v: serde_json::Value =
            serde_json::from_str(CLAUDE_SETTINGS).expect("settings.json must be valid JSON");

        // An alias, not a pinned id: `opus[1m]` follows the newest Opus and keeps
        // the million-token context, where `claude-opus-5[1m]` would go stale the
        // way the image's own Claude Code version did before increment 10.
        assert_eq!(v["model"], "opus[1m]");

        // `auto`, which is its own mode -- not `acceptEdits`, which still stops
        // for anything that is not an edit, and not `bypassPermissions`, which
        // stops asking altogether. Claude Code's own words for auto mode are
        // that it is "only for use in isolated environments", which is the one
        // thing sbx can actually promise.
        assert_eq!(v["permissions"]["defaultMode"], "auto");

        // Every one of these exists because the sandbox denies the traffic behind
        // it, and a denial with nothing worth investigating behind it is noise in
        // the events pane.
        for quiet in [
            "DISABLE_AUTOUPDATER",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
            "CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL",
        ] {
            assert_eq!(v["env"][quiet], "1", "{quiet}");
        }
    }

    #[test]
    fn hook_settings_are_valid_json_covering_the_events_that_matter() {
        let v: serde_json::Value =
            serde_json::from_str(CLAUDE_SETTINGS).expect("settings.json must be valid JSON");
        let hooks = v
            .get("hooks")
            .and_then(|h| h.as_object())
            .expect("a hooks object");

        // Notification is the one that makes a session loud; without Stop a
        // finished turn would stay `running` until the file went stale.
        for event in ["Notification", "Stop", "UserPromptSubmit", "PreToolUse"] {
            assert!(hooks.contains_key(event), "missing the {event} hook");
        }

        // Every hook has to invoke the reporter, or it reports nothing.
        let text = CLAUDE_SETTINGS;
        for state in [
            "sbx-status idle",
            "sbx-status running",
            "sbx-status waiting",
        ] {
            assert!(text.contains(state), "no hook writes `{state}`");
        }
    }
}
