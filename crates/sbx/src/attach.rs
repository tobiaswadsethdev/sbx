//! Handing the local terminal to the agent, and taking it back.
//!
//! Shared by `sbx attach` and the TUI rather than living in the core, because
//! it is the one part of attaching that is about *this* terminal: a client
//! driving a session from somewhere else has its own pty to manage and no use
//! for raw mode here. What the core keeps is
//! [`sbx_core::ops::attach_argv`] -- the command that attaches, which the
//! session's backend decides and which is the same wherever it is run from.

use std::process::Command;

use sbx_core::backend::Backends;
use sbx_core::ops;
use sbx_core::session::Session;

/// Hand the terminal to the agent, and take it back afterwards.
///
/// **The terminal has to be put in raw mode here**, because nothing else does
/// it. `openshell sandbox exec --tty` allocates a pty at the *sandbox* end and
/// leaves the local one exactly as it found it -- measured against 0.0.110:
/// `ICANON`, `ECHO`, `ISIG` and `ICRNL` are all still set while the exec runs.
/// A cooked terminal cannot drive a full-screen program:
///
/// * input is line-buffered, so arrow keys reach the agent in a batch when
///   Enter is pressed, if at all -- a question with options cannot be answered;
/// * `ICRNL` turns Enter into `\n` where the agent's input box submits on
///   `\r`, so a typed line sits in the box and nothing happens;
/// * `ISIG` catches Ctrl-C locally instead of passing `0x03` through, and
///   Ctrl-B never reaches tmux, so there is no way to detach either.
///
/// The symptom is an agent that echoes what you type and ignores every key that
/// matters, which reads as the agent being stuck rather than as the terminal
/// being wrong. `sbx attach` and the TUI's attach share this for that reason:
/// two copies would be one fixed and one not.
///
/// The guard restores the terminal on every path out, including a panic, and a
/// terminal that cannot be put into raw mode -- output redirected, no tty --
/// attaches anyway rather than refusing, since that is still useful for reading.
pub fn interactively(
    backends: &Backends,
    session: &Session,
) -> std::io::Result<std::process::ExitStatus> {
    let _raw = RawMode::enter();
    let argv = ops::attach_argv(backends.for_session(session), session, &session.tmux)
        .map_err(std::io::Error::other)?;
    // Not `.output()` and never killed: the child must exit on its own, because
    // killing an `exec --tty` wedges the exec path for that sandbox until it is
    // recreated.
    Command::new(&argv[0]).args(&argv[1..]).status()
}

/// Raw mode for as long as it is alive.
struct RawMode(());

impl RawMode {
    fn enter() -> Option<Self> {
        ratatui::crossterm::terminal::enable_raw_mode()
            .ok()
            .map(RawMode)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = ratatui::crossterm::terminal::disable_raw_mode();
    }
}
