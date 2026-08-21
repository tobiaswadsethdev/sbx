//! Handing the terminal over to the agent, and taking it back.

use std::io;

use openshell_client::CliClient;

use crate::seed::sh_quote;
use crate::session::{REPO_PATH, Session};

/// Attach to the agent's tmux session inside the sandbox.
///
/// Blocks until the user detaches. The child must be allowed to exit on its
/// own: killing an `exec --tty` abruptly wedges the exec path for that sandbox
/// until it is recreated, so nothing here terminates it.
pub fn attach(
    terminal: &mut ratatui::DefaultTerminal,
    client: &CliClient,
    session: &Session,
) -> io::Result<()> {
    // `attach -d` evicts any client left behind by an earlier crash; without
    // it a stale client makes the new attach share a resized, confusing view.
    // Falling through to new-session means Enter always lands somewhere useful
    // even if the agent was never started or has been killed.
    let script = format!(
        "tmux -f /etc/tmux.conf attach -d -t {tmux} 2>/dev/null \
         || tmux -f /etc/tmux.conf new-session -s {tmux} -c {repo}",
        tmux = sh_quote(&session.tmux),
        repo = sh_quote(REPO_PATH),
    );

    ratatui::restore();
    println!("attaching to {} - detach with Ctrl-b d", session.name);

    let status = client
        .interactive_exec(&session.sandbox, &["sh", "-c", &script])
        .status();

    // Restore the TUI before reporting anything, so an error is drawn inside
    // the interface rather than scrolling past on a bare terminal.
    *terminal = ratatui::init();
    terminal.clear()?;

    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(io::Error::other(format!("attach exited with {s}"))),
        Err(e) => Err(e),
    }
}
