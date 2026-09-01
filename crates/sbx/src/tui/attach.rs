//! Handing the terminal over to the agent, and taking it back.

use std::io;

use openshell_client::CliClient;

use sbx_core::session::Session;

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
    ratatui::restore();
    println!("attaching to {} - detach with Ctrl-b d", session.name);

    // Raw mode is [`crate::attach::interactively`]'s to manage, and it has to
    // outlive `ratatui::restore` above: restoring the TUI's terminal turns raw
    // mode *off*, which is right for a shell prompt and wrong for the agent.
    let status = crate::attach::interactively(client, session);

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
