//! Preparing a fresh sandbox: clone the repo, cut the work branch, write the
//! metadata record that makes the sandbox self-describing, and start the agent.
//!
//! **All of it runs inside the sandbox, detached from the command that asks for
//! it.** It used to run as one long `exec`, which meant the clone was a child of
//! the host process: quitting the TUI mid-clone -- and the create thread is
//! detached, so quitting is enough -- killed it and left a sandbox holding 69MB
//! of a 238MB repository, no `HEAD`, and a record that still said `seeding`.
//! Nothing in the gateway log, because nothing failed; the client simply went
//! away.
//!
//! Now the host writes a script into the sandbox, starts it with `setsid`, and
//! watches [`SEED_STATE_PATH`]. The seeder finishes whatever happens to the tool
//! that started it, which is the same principle as the agent's own tmux session,
//! and the record is caught up either by the watcher or by the repair pass in
//! [`crate::ops::refresh_with`] the next time anything runs.

use std::process::Command;

use openshell_client::OpenShell;

use crate::forge;
use crate::session::{
    META_PATH, REPO_PATH, SEED_LOG_PATH, SEED_SCRIPT_PATH, SEED_STATE_PATH, Session, TASK_PATH,
};

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

/// The clone-and-branch half of seeding, without a shebang or a `set`.
///
/// Idempotent: re-seeding an already-seeded sandbox re-uses the clone and
/// switches to the existing branch instead of failing. Kept separate from
/// [`detached_script`] so the tricky parts -- the credential prelude, the
/// quoting -- have one home.
fn clone_and_branch(session: &Session) -> String {
    let (name, email) = host_git_identity();

    let base_branch_arg = match &session.base_branch {
        Some(b) => format!("--branch {} ", sh_quote(b)),
        None => String::new(),
    };

    // A recognised forge contributes two things: the URL with any userinfo
    // stripped, and a credential header. An unrecognised host is not an error
    // -- a public repository on any host still clones -- so both fall back to
    // the URL exactly as given and no header at all.
    let remote = forge::Remote::parse(&session.repo).ok();
    let url = remote
        .as_ref()
        .map_or(session.repo.clone(), |r| r.clone_url.clone());
    let prelude = remote.as_ref().map_or_else(
        // Still has to define everything the script below references, or
        // `set -eu` aborts on the first unset variable.
        || String::from("git_auth=''\nauth_header=''\ngitc() { git \"$@\"; }\n"),
        |r| forge::git_auth_prelude(r.forge),
    );

    format!(
        r#"{prelude}if [ ! -d {repo}/.git ]; then
  gitc clone --quiet {base}-- {url} {repo}
fi
cd {repo}
git config user.name {gname}
git config user.email {gemail}
# Persist the credential header in the clone, so a later push or fetch needs no
# special casing. Safe: the value is the gateway's placeholder, not the secret,
# and it is meaningless outside this sandbox.
if [ -n "$git_auth" ]; then
  git config "http.extraHeader" "$auth_header"
fi
git switch --quiet -c {branch} 2>/dev/null || git switch --quiet {branch}
"#,
        prelude = prelude,
        repo = sh_quote(REPO_PATH),
        base = base_branch_arg,
        url = sh_quote(&url),
        gname = sh_quote(&name),
        gemail = sh_quote(&email),
        branch = sh_quote(&session.work_branch),
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

/// How far the seeder has got, as it reports itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedState {
    /// Working, and the named step is what it is doing. `alive` is whether the
    /// process is still there: a seeder that died mid-step -- the sandbox itself
    /// restarting is the only way -- looks identical otherwise.
    Running {
        step: String,
        alive: bool,
    },
    /// Everything done: cloned, branched, metadata written, agent started.
    Done,
    Failed(String),
    /// Nothing has reported yet. The launcher has run but the script has not got
    /// as far as its first write, or this sandbox was seeded by an older sbx.
    Unknown,
}

/// The whole of seeding, as a script the sandbox runs on its own.
///
/// Every step announces itself into [`SEED_STATE_PATH`] before doing anything, so
/// a watcher -- this run's, or a later one after the tool was closed -- can say
/// what is happening. The trap turns any failure into `failed` with the last
/// lines of the log attached, because the alternative is a state file that simply
/// stops and a session that looks like it is still working.
///
/// The agent is started from in here rather than by the host for the same reason
/// as everything else: if the host has gone, the session should still come up
/// ready to work.
///
/// The failure handler takes no argument, deliberately. `/bin/sh` in the sandbox
/// is dash, which has no `$LINENO`, and reaching for it under `set -u` makes the
/// handler itself fail -- which is how a clone that could not authenticate came
/// to write no reason at all into the state file, leaving the host to infer
/// "stopped" from a missing process. The last lines of the log say more than a
/// line number would.
pub fn detached_script(session: &Session, start_agent: bool) -> String {
    let meta =
        serde_json::to_string_pretty(session).expect("Session is plain data and always serializes");

    let agent = if start_agent {
        // In a subshell: the agent script exits early when a session is already
        // running, and that must not end the seeder before it reports `done`.
        format!(
            "step agent
(
{}
)
",
            start_agent_script(session).trim_end()
        )
    } else {
        String::new()
    };

    format!(
        r#"set -eu
mkdir -p /sandbox/.sbx
state={state}
say() {{ printf '%s
' "$*" >> "$state"; }}
step() {{ say "step $1"; }}
fail() {{
  say "failed $(tail -n 3 {log} 2>/dev/null | tr '\n' ' ')"
  exit 1
}}
trap fail EXIT INT TERM
: > "$state"
say "pid $$"

step clone
{clone}
step branch
step meta
{write_meta}
{agent}trap - EXIT INT TERM
say done
"#,
        state = sh_quote(SEED_STATE_PATH),
        log = sh_quote(SEED_LOG_PATH),
        clone = clone_and_branch(session).trim_end(),
        write_meta = meta_write_command(&meta),
        agent = agent,
    )
}

/// Write the seeder into the sandbox and start it, detached.
///
/// Returns as soon as it is running. `setsid` is what makes it outlive this exec:
/// without a session of its own the seeder is torn down with the exec's process
/// group, which is exactly the failure this whole arrangement exists to remove.
/// Its output goes to a file because it has no terminal to write to and because a
/// failure needs something to quote.
pub fn launch(
    client: &dyn OpenShell,
    session: &Session,
    start_agent: bool,
) -> Result<(), SeedError> {
    let script = detached_script(session, start_agent);
    let launcher = format!(
        "mkdir -p /sandbox/.sbx && printf '%s' {script} > {path} &&          setsid sh {path} > {log} 2>&1 < /dev/null &          sleep 0.1",
        script = sh_quote(&script),
        path = sh_quote(SEED_SCRIPT_PATH),
        log = sh_quote(SEED_LOG_PATH),
    );

    let out = client.exec(&session.sandbox, &["sh", "-c", &launcher])?;
    if !out.ok() {
        return Err(SeedError::Script {
            code: out.exit_code,
            stderr: out.stderr.trim().to_string(),
        });
    }
    Ok(())
}

/// Ask the sandbox how the seeding is going.
///
/// One exec, cheap enough to do twice a second while watching and once per stuck
/// session when repairing records. The liveness check is a directory test in
/// `/proc`, which costs nothing next to the round trip.
pub fn seed_state(client: &dyn OpenShell, session: &Session) -> SeedState {
    let script = format!(
        "cat {state} 2>/dev/null || true;          pid=$(sed -n 's/^pid //p' {state} 2>/dev/null | tail -1);          if [ -n \"$pid\" ] && [ -d /proc/\"$pid\" ]; then echo 'alive'; fi",
        state = sh_quote(SEED_STATE_PATH),
    );
    match client.exec(&session.sandbox, &["sh", "-c", &script]) {
        Ok(out) if out.ok() => parse_seed_state(&out.stdout),
        // An unreachable sandbox says nothing about the seeding; the caller
        // treats that as "no news" and asks again.
        _ => SeedState::Unknown,
    }
}

/// Read the state file. Pure, so the state machine is testable without a
/// sandbox.
pub fn parse_seed_state(text: &str) -> SeedState {
    let mut step = None;
    let mut alive = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("failed ") {
            return SeedState::Failed(rest.trim().to_string());
        }
        if line == "done" {
            return SeedState::Done;
        }
        if let Some(rest) = line.strip_prefix("step ") {
            step = Some(rest.trim().to_string());
        }
        if line == "alive" {
            alive = true;
        }
    }
    match step {
        Some(step) => SeedState::Running { step, alive },
        None => SeedState::Unknown,
    }
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

    // The locale is exported here as well as in the image, because this runs as
    // an exec and the gateway does not pass the image's environment through --
    // and this exec is the one that starts the *tmux server*, whose environment
    // every pane inherits. An agent with no UTF-8 locale draws its own box rules
    // and glyphs as something tmux cannot map. See `ops::attach_script`.
    format!(
        r#"set -eu
export LANG=C.UTF-8 LC_ALL=C.UTF-8 COLORTERM=truecolor
if tmux -u -f /etc/tmux.conf has-session -t {tmux} 2>/dev/null; then
  exit 0
fi
mkdir -p /sandbox/.sbx
printf '%s' {task} > {task_path}
tmux -u -f /etc/tmux.conf new-session -d -s {tmux} -c {repo}
tmux -u -f /etc/tmux.conf send-keys -t {tmux} {launch} Enter
"#,
        tmux = sh_quote(&session.tmux),
        task = sh_quote(&session.task),
        task_path = sh_quote(TASK_PATH),
        repo = sh_quote(REPO_PATH),
        launch = sh_quote(&launch),
    )
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

    /// The seeder runs detached, so the state file is the only thing that knows
    /// what happened. Every shape it can be in has to read back correctly.
    #[test]
    fn the_state_file_reads_back() {
        assert_eq!(parse_seed_state(""), SeedState::Unknown);
        assert_eq!(parse_seed_state("pid 41\n"), SeedState::Unknown);

        assert_eq!(
            parse_seed_state("pid 41\nstep clone\nalive\n"),
            SeedState::Running {
                step: "clone".into(),
                alive: true
            }
        );
        // Later steps win: the file is appended to, not rewritten.
        assert_eq!(
            parse_seed_state("pid 41\nstep clone\nstep branch\nstep meta\nalive\n"),
            SeedState::Running {
                step: "meta".into(),
                alive: true
            }
        );
        // No `alive` line means the process is gone -- which, mid-step, is the
        // one case that cannot be told from "still working" any other way.
        assert_eq!(
            parse_seed_state("pid 41\nstep clone\n"),
            SeedState::Running {
                step: "clone".into(),
                alive: false
            }
        );

        assert_eq!(
            parse_seed_state("pid 41\nstep agent\ndone\n"),
            SeedState::Done
        );
        assert_eq!(
            parse_seed_state("pid 41\nstep clone\nfailed 12: fatal: repository not found\n"),
            SeedState::Failed("12: fatal: repository not found".into())
        );
    }

    /// What the detached script has to do, in the order it has to do it. Each of
    /// these is invisible until a seeder dies halfway and the record has to say
    /// something true about it.
    #[test]
    fn the_detached_script_reports_every_step_and_traps_failure() {
        let s = Session::new("x".into(), "https://github.com/o/r.git".into(), "t".into());
        let script = detached_script(&s, true);

        // Announced before the work, or a watcher shows the wrong step.
        let clone_at = script.find("step clone").expect("a clone step");
        let cloning_at = script.find("gitc clone").expect("the clone itself");
        assert!(clone_at < cloning_at, "{script}");

        for step in ["step clone", "step branch", "step meta", "step agent"] {
            assert!(script.contains(step), "missing `{step}`: {script}");
        }
        // The end, and the only thing that says the session is usable.
        assert!(script.trim_end().ends_with("say done"), "{script}");
        // Anything unexpected has to become `failed`, or the state file simply
        // stops and the session looks like it is still working.
        assert!(script.contains("trap fail EXIT INT TERM"), "{script}");
        // dash has no `$LINENO`, and reaching for it under `set -u` turns the
        // failure handler into a second failure that reports nothing at all.
        assert!(!script.contains("LINENO"), "{script}");
        assert!(
            script.contains("trap - EXIT INT TERM"),
            "the trap is cleared before `done`"
        );
        // The metadata is written inside the sandbox now, because the host may
        // not be there when seeding ends.
        assert!(script.contains(crate::session::META_PATH), "{script}");
    }

    /// Without `--start`, nothing about the agent is in the script at all: the
    /// session is prepared and left alone.
    #[test]
    fn the_agent_is_only_started_when_asked_for() {
        let s = Session::new("x".into(), "https://github.com/o/r.git".into(), "t".into());
        assert!(!detached_script(&s, false).contains("step agent"));
        assert!(!detached_script(&s, false).contains("new-session"));
        assert!(detached_script(&s, true).contains("new-session"));
    }

    #[test]
    fn seed_script_interpolates_nothing_raw() {
        let mut s = Session::new("x".into(), "https://example.com/a'b.git".into(), "t".into());
        s.base_branch = Some("main".into());
        let script = detached_script(&s, true);
        // The raw, unquoted URL must never appear.
        assert!(!script.contains("https://example.com/a'b.git"));
        assert!(script.contains(r"a'\''b.git"));
        assert!(script.contains("--branch 'main'"));
        assert!(script.contains("git switch --quiet -c 'sbx/x'"));
    }

    #[test]
    fn seed_script_omits_branch_flag_when_unset() {
        let s = Session::new("x".into(), "url".into(), "t".into());
        let script = detached_script(&s, true);
        assert!(!script.contains("--branch"));
        assert!(script.contains("gitc clone --quiet -- 'url'"));
    }

    /// A host sbx does not recognise is not an error: a public repository on
    /// any host still has to clone. But everything the script references must
    /// be defined anyway, or `set -eu` aborts on the first unset variable.
    #[test]
    fn an_unrecognised_host_still_seeds() {
        let s = Session::new("x".into(), "https://gitlab.com/o/r.git".into(), "t".into());
        let script = detached_script(&s, true);
        assert!(script.contains("gitc clone"), "{script}");
        assert!(script.contains("git_auth=''"), "must be defined: {script}");
        assert!(
            script.contains("auth_header=''"),
            "must be defined: {script}"
        );
        assert!(
            !script.contains("extraHeader=Authorization"),
            "no credential to send: {script}"
        );
        // The URL is passed through untouched, since nothing is known about it.
        assert!(script.contains("'https://gitlab.com/o/r.git'"), "{script}");
    }

    /// The whole point of the forge work: a private Azure DevOps repo needs a
    /// credential header on the *clone*, not just on the push.
    #[test]
    fn seeding_an_azure_repo_sends_the_credential() {
        let s = Session::new(
            "x".into(),
            "https://inetse@dev.azure.com/inetse/proj/_git/repo".into(),
            "t".into(),
        );
        let script = detached_script(&s, true);
        assert!(script.contains("AZURE_DEVOPS_PAT"), "{script}");
        assert!(
            script.contains("Basic"),
            "PAT is basic, not bearer: {script}"
        );
        // Userinfo stripped from the *clone*, or git demands a password before
        // it sends anything. The metadata record below keeps the URL exactly as
        // the user gave it, userinfo and all, so this is checked on the one
        // line that matters rather than on the whole script.
        let clone_line = script
            .lines()
            .find(|l| l.contains("gitc clone"))
            .expect("a clone");
        assert_eq!(
            clone_line.trim(),
            "gitc clone --quiet -- 'https://dev.azure.com/inetse/proj/_git/repo' '/sandbox/repo'"
        );
        assert!(!clone_line.contains('@'), "{clone_line}");
        // And the header is persisted so a later push needs no special casing.
        assert!(
            script.contains(r#"git config "http.extraHeader""#),
            "{script}"
        );
    }

    #[test]
    fn seeding_a_github_repo_sends_a_bearer_token() {
        let s = Session::new(
            "x".into(),
            "https://github.com/octocat/Hello-World.git".into(),
            "t".into(),
        );
        let script = detached_script(&s, true);
        assert!(script.contains("Bearer $GITHUB_TOKEN"), "{script}");
        assert!(
            !script.contains("base64"),
            "bearer needs no encoding: {script}"
        );
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
        let script = detached_script(&s, true);
        assert!(script.contains("/sandbox/.sbx/meta.json"));
        assert!(
            script.contains("do the thing"),
            "task must survive into the sandbox"
        );
    }
}
