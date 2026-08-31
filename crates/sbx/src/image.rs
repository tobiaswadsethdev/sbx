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
//!
//! **Two kinds of image.** The base, `sbx-base:latest`, is what a session with no
//! toolchain runs. A *variant* -- `sbx-base:dotnet`, `sbx-base:dotnet-rust` -- is
//! the base plus one layer per toolchain, so docker shares the base's several
//! gigabytes and only the toolchains asked for are ever built. The base is a
//! prerequisite of every variant, which is why the variant build ensures it
//! first: a variant whose `FROM` is missing fails with docker's words about a
//! manifest, several lines away from the thing to do about it. See
//! [`crate::toolchain`] for what a toolchain is beyond its install.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::session::{IMAGE, IMAGE_REPO};
use crate::toolchain::{self, Toolchain};

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
    exists_tag(IMAGE)
}

/// Whether a particular tag is built. The base image or one of the toolchain
/// variants; see [`crate::toolchain::tag`].
pub fn exists_tag(tag: &str) -> bool {
    Command::new("docker")
        .args(["image", "inspect", tag])
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

/// Build the variant image carrying `chains`, and the base image under it if it
/// is not there yet.
///
/// Streams docker's output like [`build`], and for the same reason: a toolchain
/// layer is a several-hundred-megabyte download, and a build that says nothing
/// for two minutes looks hung.
///
/// Nothing to do when the variant is already built. That is not a cache: the tag
/// is a pure function of the set of toolchains ([`crate::toolchain::tag`]), so a
/// tag that exists carries exactly what was asked for. What it does *not*
/// guarantee is that the base underneath it is still the one in front of you --
/// rebuilding the base for a newer agent leaves the variants behind it, which is
/// what [`stale_variants`] exists to say out loud.
pub fn build_variant(chains: &[&'static Toolchain]) -> Result<(), String> {
    if chains.is_empty() {
        return build();
    }
    let tag = toolchain::tag(chains);
    ensure()?;

    let dir = write_variant_context(chains)?;
    println!("building {tag} ({})", toolchain::labels(chains).join(", "));
    let result = run_variant_build(&dir, &tag);
    let _ = fs::remove_dir_all(&dir);
    result
}

/// The variant's context: one generated Dockerfile and nothing else.
///
/// A directory rather than stdin for the reason the base build uses one -- and
/// unlike the base there is nothing to `COPY`, so this is the one case where
/// piping would work. It writes a file anyway, because a build that fails is
/// worth being able to read, and because the two builds then differ in their
/// content rather than in their mechanism.
fn write_variant_context(chains: &[&'static Toolchain]) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join(format!("sbx-variant-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let path = dir.join("Dockerfile");
    fs::write(&path, toolchain::dockerfile(chains))
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    Ok(dir)
}

/// The variant `docker build` argv. Split out for the same reason as
/// [`build_argv`]: the wiring is worth a test that does not need docker.
fn variant_build_argv(dir: &Path, tag: &str) -> Vec<String> {
    vec![
        "build".to_string(),
        "-t".to_string(),
        tag.to_string(),
        dir.display().to_string(),
    ]
}

fn run_variant_build(dir: &Path, tag: &str) -> Result<(), String> {
    let status = Command::new("docker")
        .args(variant_build_argv(dir, tag))
        .status()
        .map_err(|e| format!("could not run docker: {e}"))?;
    if !status.success() {
        return Err(format!("docker build exited with {status}"));
    }
    Ok(())
}

/// The toolchains a built image records carrying, as `(name, version)`.
///
/// Read from the image rather than inferred from its tag, because a tag is a
/// claim and the manifest is what the layers actually installed. A container
/// start, so it belongs where the answer is the point -- `sbx doctor` -- and
/// never on a path a session waits on.
///
/// An empty vector for the base image, which carries no manifest: that is the
/// honest answer, not a failure.
pub fn toolchains_in(tag: &str) -> Vec<(String, String)> {
    let out = Command::new("docker")
        .args([
            "run",
            "--rm",
            "--entrypoint",
            "cat",
            tag,
            toolchain::MANIFEST_PATH,
        ])
        .output();
    let Ok(out) = out else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(name, version)| (name.to_string(), version.trim().to_string()))
        .collect()
}

/// Variant tags that exist, newest first, as docker reports them.
pub fn variants() -> Vec<String> {
    let out = Command::new("docker")
        .args([
            "image",
            "ls",
            "--filter",
            &format!("reference={IMAGE_REPO}"),
            "--format",
            "{{.Repository}}:{{.Tag}}",
        ])
        .output();
    let Ok(out) = out else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|tag| !tag.is_empty() && *tag != IMAGE && !tag.ends_with(":<none>"))
        .map(str::to_string)
        .collect()
}

/// Variants built before the base image they sit on.
///
/// A variant is `FROM sbx-base:latest`, so rebuilding the base -- which is what
/// picks up a newer Claude Code -- leaves every variant on the old one. Nothing
/// about that looks wrong: sessions start, the toolchain works, and the agent is
/// the version it was months ago. This is the check that says so.
///
/// Timestamps compare as strings because docker reports RFC 3339 in UTC, where
/// lexical and chronological order agree. Anything that cannot be read is
/// treated as not-stale, for the reason [`is_older`] stays quiet on a version
/// scheme it does not understand: a `doctor` that nags about what it cannot
/// establish is one people stop reading.
pub fn stale_variants() -> Vec<String> {
    let Some(base) = created(IMAGE) else {
        return Vec::new();
    };
    variants()
        .into_iter()
        .filter(|tag| created(tag).is_some_and(|built| built < base))
        .collect()
}

/// When an image was built, as docker reports it.
fn created(tag: &str) -> Option<String> {
    let out = Command::new("docker")
        .args(["image", "inspect", tag, "--format", "{{.Created}}"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let created = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!created.is_empty()).then_some(created)
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

/// Build the image a session with these toolchains needs, if it is missing.
///
/// The same contract as [`ensure`] and the same caller: a command line, never the
/// TUI, because the build streams docker's output. A first `--toolchain dotnet`
/// pays for the SDK once and every session after it starts as fast as any other.
pub fn ensure_for(chains: &[&'static Toolchain]) -> Result<bool, String> {
    if chains.is_empty() {
        return ensure();
    }
    let tag = toolchain::tag(chains);
    if exists_tag(&tag) {
        return Ok(false);
    }
    println!("building {tag} (first use of these toolchains, this takes a while) ...");
    build_variant(chains)?;
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

    /// A variant is built from a generated Dockerfile and no other context, and
    /// it must be tagged with the toolchains rather than over the base image --
    /// a variant built as `sbx-base:latest` would replace the thing it is
    /// layered on.
    #[test]
    fn a_variant_builds_from_its_own_context_under_its_own_tag() {
        let chains = toolchain::resolve(&["rust".to_string()]).expect("rust");
        let dir = write_variant_context(&chains).expect("write context");
        let written = fs::read_to_string(dir.join("Dockerfile")).expect("Dockerfile");
        assert_eq!(written, toolchain::dockerfile(&chains));
        assert_eq!(
            fs::read_dir(&dir).unwrap().count(),
            1,
            "a variant needs nothing to COPY"
        );
        fs::remove_dir_all(&dir).expect("cleanup");

        let argv = variant_build_argv(Path::new("/tmp/ctx"), "sbx-base:rust");
        assert_eq!(argv, ["build", "-t", "sbx-base:rust", "/tmp/ctx"]);
        assert_ne!(
            argv[2], IMAGE,
            "a variant must not overwrite the base image"
        );
    }

    /// `variants` lists the toolchain images and not the base, since the base is
    /// reported by its own check and would otherwise be named twice.
    #[test]
    fn the_variant_filter_is_the_image_repository() {
        // The filter docker is given, spelled out here so a rename of either
        // constant is caught by a test rather than by an empty list.
        assert_eq!(IMAGE_REPO, "sbx-base");
        assert!(IMAGE.starts_with(IMAGE_REPO));
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
    /// Copy-on-select is on unless something says otherwise, and the thing that
    /// says otherwise is the global config file, not `settings.json`.
    #[test]
    fn the_image_turns_copy_on_select_off() {
        assert!(
            DOCKERFILE.contains("copyOnSelect: false"),
            "the generated /sandbox/.claude.json must turn it off"
        );
        assert!(
            !CLAUDE_SETTINGS.contains("copyOnSelect"),
            "settings.json does not carry it; putting it there does nothing"
        );
    }

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

        // Empty strings, which is how Claude Code is told to write no
        // attribution at all -- the key being absent means the default trailer,
        // not silence. A sandboxed agent's commits already carry the host's git
        // identity (see `seed::host_git_identity`), so a co-author trailer would
        // credit the tool for work attributed to the person running it, and a
        // branch full of them is a branch that reads as machine-written.
        assert_eq!(v["attribution"]["commit"], "");
        assert_eq!(v["attribution"]["pr"], "");
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
