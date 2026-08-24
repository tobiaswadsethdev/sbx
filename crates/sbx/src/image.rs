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
    let result = run_build(&dir);
    // Clean up whether or not the build worked; a failed build's context is not
    // worth keeping, since it is regenerated from constants every time.
    let _ = fs::remove_dir_all(&dir);
    result
}

fn run_build(dir: &Path) -> Result<(), String> {
    let status = Command::new("docker")
        .arg("build")
        .args(["-t", IMAGE])
        .arg(dir)
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
