//! Preparing a fresh sandbox: clone the repo, cut the work branch, and write
//! the metadata record that makes the sandbox self-describing.

use std::process::Command;

use openshell_client::OpenShell;

use crate::session::{META_PATH, REPO_PATH, Session, TASK_PATH};

/// Quote a value for safe interpolation into a `sh -c` script.
///
/// Everything interpolated below is attacker-influenced in the general case (a
/// repo URL or task string can contain anything), so nothing is pasted raw.
pub fn sh_quote(s: &str) -> String {
    // Close the quote, emit an escaped quote, reopen: the standard POSIX trick,
    // since single quotes cannot be escaped inside single quotes.
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Git identity for commits made inside the sandbox, taken from the host so
/// commits are attributed to the person running sbx rather than to a robot.
fn host_git_identity() -> (String, String) {
    let get = |key: &str| -> Option<String> {
        let out = Command::new("git")
            .args(["config", "--get", key])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!v.is_empty()).then_some(v)
    };
    (
        get("user.name").unwrap_or_else(|| "sbx".to_string()),
        get("user.email").unwrap_or_else(|| "sbx@localhost".to_string()),
    )
}

/// The script run inside the sandbox to seed it.
///
/// Written to be idempotent: re-seeding an already-seeded sandbox re-uses the
/// clone and switches to the existing branch instead of failing.
pub fn seed_script(session: &Session) -> String {
    let (name, email) = host_git_identity();

    let meta =
        serde_json::to_string_pretty(session).expect("Session is plain data and always serializes");

    let base_branch_arg = match &session.base_branch {
        Some(b) => format!("--branch {} ", sh_quote(b)),
        None => String::new(),
    };

    format!(
        r#"set -eu
mkdir -p /sandbox/.sbx
if [ ! -d {repo}/.git ]; then
  git clone --quiet {base}-- {url} {repo}
fi
cd {repo}
git config user.name {gname}
git config user.email {gemail}
git switch --quiet -c {branch} 2>/dev/null || git switch --quiet {branch}
{write_meta}
"#,
        repo = sh_quote(REPO_PATH),
        base = base_branch_arg,
        url = sh_quote(&session.repo),
        gname = sh_quote(&name),
        gemail = sh_quote(&email),
        branch = sh_quote(&session.work_branch),
        write_meta = meta_write_command(&meta),
    )
}

/// Shell command that writes the metadata record.
fn meta_write_command(meta_json: &str) -> String {
    format!(
        "mkdir -p /sandbox/.sbx && printf '%s' {meta} > {path}",
        meta = sh_quote(meta_json),
        path = sh_quote(META_PATH),
    )
}

#[derive(Debug, thiserror::Error)]
pub enum SeedError {
    #[error(transparent)]
    Client(#[from] openshell_client::Error),
    #[error("seeding failed (exit {code}): {stderr}")]
    Script { code: i32, stderr: String },
    #[error("sandbox metadata at {META_PATH} is not valid JSON: {0}")]
    BadMeta(#[from] serde_json::Error),
}

/// Clone, branch and write metadata inside the sandbox.
pub fn seed(client: &dyn OpenShell, session: &Session) -> Result<(), SeedError> {
    let script = seed_script(session);
    let out = client.exec(&session.sandbox, &["sh", "-c", &script])?;
    if !out.ok() {
        return Err(SeedError::Script {
            code: out.exit_code,
            stderr: out.stderr.trim().to_string(),
        });
    }
    Ok(())
}

/// Script that starts the agent inside the sandbox, under tmux.
///
/// Idempotent: if the session already exists the agent is left alone, so
/// re-running this never restarts work in progress.
///
/// The tmux session runs a plain shell and the agent is typed into it, rather
/// than being the session's command. When the agent exits the pane survives,
/// which is what makes it possible to attach afterwards and see what happened.
pub fn start_agent_script(session: &Session) -> String {
    let launch = if session.task.trim().is_empty() {
        session.agent.clone()
    } else {
        // The task is read from a file at run time so it never has to survive
        // a second round of quoting inside send-keys.
        format!("{} \"$(cat {})\"", session.agent, TASK_PATH)
    };

    format!(
        r#"set -eu
if tmux -f /etc/tmux.conf has-session -t {tmux} 2>/dev/null; then
  exit 0
fi
mkdir -p /sandbox/.sbx
printf '%s' {task} > {task_path}
tmux -f /etc/tmux.conf new-session -d -s {tmux} -c {repo}
tmux -f /etc/tmux.conf send-keys -t {tmux} {launch} Enter
"#,
        tmux = sh_quote(&session.tmux),
        task = sh_quote(&session.task),
        task_path = sh_quote(TASK_PATH),
        repo = sh_quote(REPO_PATH),
        launch = sh_quote(&launch),
    )
}

/// Start the agent. Safe to call on an already-running session.
pub fn start_agent(client: &dyn OpenShell, session: &Session) -> Result<(), SeedError> {
    let out = client.exec(
        &session.sandbox,
        &["sh", "-c", &start_agent_script(session)],
    )?;
    if !out.ok() {
        return Err(SeedError::Script {
            code: out.exit_code,
            stderr: out.stderr.trim().to_string(),
        });
    }
    Ok(())
}

/// Refresh the metadata record inside the sandbox.
///
/// The record is what adoption reads after the local cache is lost, so it has
/// to track state changes. Writing it only once during seeding leaves every
/// recovered session frozen at `seeding`.
pub fn write_meta(client: &dyn OpenShell, session: &Session) -> Result<(), SeedError> {
    let meta = serde_json::to_string_pretty(session)?;
    let out = client.exec(&session.sandbox, &["sh", "-c", &meta_write_command(&meta)])?;
    if !out.ok() {
        return Err(SeedError::Script {
            code: out.exit_code,
            stderr: out.stderr.trim().to_string(),
        });
    }
    Ok(())
}

/// Read a session back out of a sandbox, for adopting work the local cache
/// does not know about.
pub fn read_meta(client: &dyn OpenShell, sandbox: &str) -> Result<Session, SeedError> {
    let out = client.exec(sandbox, &["cat", META_PATH])?;
    if !out.ok() {
        return Err(SeedError::Script {
            code: out.exit_code,
            stderr: out.stderr.trim().to_string(),
        });
    }
    Ok(serde_json::from_str(&out.stdout)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;

    /// Ask a real shell what the quoted value expands to. Inspecting the
    /// escaped string by eye is misleading -- a correctly escaped payload still
    /// *contains* the dangerous substring -- so the only meaningful assertion
    /// is that `sh` reproduces the input exactly and runs nothing.
    fn shell_roundtrip(s: &str) -> String {
        let out = Command::new("sh")
            .arg("-c")
            .arg(format!("printf '%s' {}", sh_quote(s)))
            .output()
            .expect("sh");
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    fn quoting_survives_a_real_shell() {
        for case in [
            "plain",
            "it's",
            "'; rm -rf /; echo '",
            "$(id)",
            "`id`",
            "a b\tc",
            "back\\slash",
            "semi;colon && and || or",
            "new\nline",
        ] {
            assert_eq!(shell_roundtrip(case), case, "mangled: {case:?}");
        }
    }

    #[test]
    fn seed_script_interpolates_nothing_raw() {
        let mut s = Session::new("x".into(), "https://example.com/a'b.git".into(), "t".into());
        s.base_branch = Some("main".into());
        let script = seed_script(&s);
        // The raw, unquoted URL must never appear.
        assert!(!script.contains("https://example.com/a'b.git"));
        assert!(script.contains(r"a'\''b.git"));
        assert!(script.contains("--branch 'main'"));
        assert!(script.contains("git switch --quiet -c 'sbx/x'"));
    }

    #[test]
    fn seed_script_omits_branch_flag_when_unset() {
        let s = Session::new("x".into(), "url".into(), "t".into());
        let script = seed_script(&s);
        assert!(!script.contains("--branch"));
        assert!(script.contains("git clone --quiet -- 'url'"));
    }

    #[test]
    fn meta_write_command_is_idempotent_and_quoted() {
        let cmd = meta_write_command(r#"{"a":"it's"}"#);
        assert!(cmd.starts_with("mkdir -p /sandbox/.sbx &&"));
        assert!(cmd.contains(r"it'\''s"), "JSON must be shell-quoted: {cmd}");
    }

    #[test]
    fn agent_start_is_idempotent() {
        let s = Session::new("x".into(), "url".into(), "do the thing".into());
        let script = start_agent_script(&s);
        // Re-running must not restart an agent that is already working.
        assert!(script.contains("has-session -t 'agent'"));
        assert!(script.contains("exit 0"));
    }

    #[test]
    fn agent_command_reads_the_task_from_a_file() {
        // A task containing quotes must not need escaping twice, once for the
        // outer sh -c and again inside tmux send-keys.
        let s = Session::new(
            "x".into(),
            "url".into(),
            r#"add a "greeting" & quit"#.into(),
        );
        let script = start_agent_script(&s);
        assert!(script.contains("/sandbox/.sbx/task.txt"));
        assert!(!script.contains(r#"send-keys -t 'agent' 'claude "add a "greeting""#));
    }

    #[test]
    fn agent_starts_bare_when_there_is_no_task() {
        let s = Session::new("x".into(), "url".into(), "   ".into());
        let script = start_agent_script(&s);
        assert!(script.contains("send-keys -t 'agent' 'claude' Enter"));
    }

    #[test]
    fn seed_script_embeds_recoverable_metadata() {
        let s = Session::new("x".into(), "url".into(), "do the thing".into());
        let script = seed_script(&s);
        assert!(script.contains("/sandbox/.sbx/meta.json"));
        assert!(
            script.contains("do the thing"),
            "task must survive into the sandbox"
        );
    }
}
