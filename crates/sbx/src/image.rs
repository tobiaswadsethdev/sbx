//! The sandbox image.
//!
//! The Dockerfile is embedded in the binary rather than read from the repo, so
//! `sbx` can build its image from anywhere once installed.

use std::io::Write;
use std::process::{Command, Stdio};

use crate::session::IMAGE;

const DOCKERFILE: &str = include_str!("../../../images/sbx-base/Dockerfile");

pub fn exists() -> bool {
    Command::new("docker")
        .args(["image", "inspect", IMAGE])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Build the image, streaming docker's output so a slow first build shows
/// progress instead of looking hung.
pub fn build() -> Result<(), String> {
    let mut child = Command::new("docker")
        .args(["build", "-t", IMAGE, "-"])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run docker: {e}"))?;

    child
        .stdin
        .take()
        .ok_or("docker stdin unavailable")?
        .write_all(DOCKERFILE.as_bytes())
        .map_err(|e| format!("could not send the Dockerfile to docker: {e}"))?;

    let status = child
        .wait()
        .map_err(|e| format!("docker build failed: {e}"))?;
    if !status.success() {
        return Err(format!("docker build exited with {status}"));
    }
    Ok(())
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
    use super::DOCKERFILE;

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
}
